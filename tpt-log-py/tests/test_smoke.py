"""Smoke tests for the compiled extension (run after `maturin develop`)."""

import json

import pytest

import tpt_microlog
from tpt_microlog import MicroLog


def read_entries(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def make_log(tmp_path, **kwargs):
    out = tmp_path / "out.jsonl"
    log = MicroLog(
        backend="file",
        file_path=str(out),
        wal_enabled=False,
        batch_size=1,
        flush_interval=0.05,
        **kwargs,
    )
    return log, out


def test_logs_reach_the_file_backend(tmp_path):
    log, out = make_log(tmp_path)
    log.info("hello", user_id=123)
    log.error("boom")
    log.metric("cpu", 0.5, tags={"host": "a"})
    assert log.flush()
    log.shutdown()
    entries = read_entries(out)
    messages = {e["message"] for e in entries}
    assert {"hello", "boom", "cpu"} <= messages
    hello = next(e for e in entries if e["message"] == "hello")
    assert hello["metadata"]["user_id"] == 123


def test_context_manager_flushes_on_exit(tmp_path):
    out = tmp_path / "ctx.jsonl"
    with MicroLog(backend="file", file_path=str(out), wal_enabled=False) as log:
        log.warn("from context")
    assert any(e["message"] == "from context" for e in read_entries(out))


def test_redaction_masks_before_delivery(tmp_path):
    log, out = make_log(tmp_path, redact_keys=["token"], redact_builtin=True)
    log.info("mail alice@example.com", token="s3cret")
    log.flush()
    log.shutdown()
    entry = read_entries(out)[0]
    assert entry["message"] == "mail [REDACTED]"
    assert entry["metadata"]["token"] == "[REDACTED]"


def test_unknown_option_is_rejected():
    with pytest.raises(ValueError):
        MicroLog(backend="none", definitely_not_an_option=1)


def test_on_error_receives_config_warnings():
    seen = []
    tpt_microlog.set_on_error(lambda kind, msg: seen.append((kind, msg)))
    try:
        # An invalid filter regex is reported through the diagnostics channel.
        MicroLog(backend="none", wal_enabled=False, filter_regex="(unclosed").shutdown()
    finally:
        tpt_microlog.set_on_error(None)
    assert any(kind == "config" and "filter_regex" in msg for kind, msg in seen)


def test_logging_handler_forwards_records(tmp_path):
    import logging

    from tpt_microlog.handler import MicroLogHandler

    out = tmp_path / "handler.jsonl"
    handler = MicroLogHandler(
        backend="file", file_path=str(out), wal_enabled=False, batch_size=1,
        redact_keys=["password"],
    )
    logger = logging.getLogger("tpt-test-handler")
    logger.setLevel(1)
    logger.propagate = False
    logger.addHandler(handler)
    try:
        logger.info("user %s logged in", "bob", extra={"user_id": 7, "password": "x"})
        logger.critical("meltdown")
        logger.log(5, "very fine detail")
        try:
            1 / 0
        except ZeroDivisionError:
            logger.exception("calc failed")
        logger.info("odd value", extra={"obj": object()})
    finally:
        logger.removeHandler(handler)
        handler.close()

    entries = {e["message"]: e for e in read_entries(out)}
    login = entries["user bob logged in"]
    assert login["level"] == "INFO"
    assert login["metadata"]["user_id"] == 7
    assert login["metadata"]["password"] == "[REDACTED]"
    assert login["metadata"]["logger"] == "tpt-test-handler"
    assert entries["meltdown"]["level"] == "FATAL"
    assert entries["very fine detail"]["level"] == "TRACE"
    assert "ZeroDivisionError" in entries["calc failed"]["metadata"]["exception"]
    assert entries["calc failed"]["level"] == "ERROR"
    assert isinstance(entries["odd value"]["metadata"]["obj"], str)


def test_handler_borrowed_logger_is_not_shut_down(tmp_path):
    import logging

    from tpt_microlog.handler import MicroLogHandler

    log, out = make_log(tmp_path)
    handler = MicroLogHandler(log=log)
    rec = logging.LogRecord("x", logging.WARNING, __file__, 1, "careful", (), None)
    handler.handle(rec)
    handler.close()
    log.info("still alive")  # a borrowed logger keeps working after close()
    log.flush()
    log.shutdown()
    assert {"careful", "still alive"} <= {e["message"] for e in read_entries(out)}
    with pytest.raises(ValueError):
        MicroLogHandler(log=log, backend="none")


def test_governors_and_health(tmp_path):
    out = tmp_path / "gov.jsonl"
    log = MicroLog(
        backend="file",
        file_path=str(out),
        wal_enabled=False,
        batch_size=1000,
        dedup_window_ms=60_000,
        flight_recorder_size=20,
        health_bind="127.0.0.1:0",
    )
    for _ in range(4):
        log.warn("flaky upstream")
    log.debug("context before the failure")
    log.error("request failed")
    assert log.flush()

    stats = log.stats()
    assert stats["deduplicated_entries"] == 3
    assert stats["recorded_entries"] == 1
    health = log.health()
    assert health["status"] == "ok"
    assert health["delivered_entries"] >= 3
    log.shutdown()
    assert log.health()["status"] == "stopped"

    entries = read_entries(out)
    summary = [e for e in entries if e["metadata"].get("repeat_summary")]
    assert summary and summary[0]["metadata"]["repeat_count"] == 3
    ctx = [e for e in entries if e["metadata"].get("flight_recorder")]
    assert [e["message"] for e in ctx] == ["context before the failure"]


SAMPLE_TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"


def test_traceparent_parsing():
    from tpt_microlog import TraceContext

    ctx = TraceContext.parse(SAMPLE_TRACEPARENT)
    assert ctx.trace_id == "4bf92f3577b34da6a3ce929d0e0e4736"
    assert ctx.traceparent == SAMPLE_TRACEPARENT
    assert not TraceContext.parse(SAMPLE_TRACEPARENT.replace("-01", "-00")).sampled
    for bad in (
        None,
        "",
        "garbage",
        "00-" + "0" * 32 + "-00f067aa0ba902b7-01",
        "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        SAMPLE_TRACEPARENT + "-extra",
        SAMPLE_TRACEPARENT.upper(),
    ):
        assert TraceContext.parse(bad) is None, bad
    root = TraceContext.new_root()
    assert len(root.trace_id) == 32 and len(root.span_id) == 16
    assert root.child().trace_id == root.trace_id


def test_trace_span_correlates_logs_across_asyncio_tasks(tmp_path):
    import asyncio

    from tpt_microlog import current_trace

    log, out = make_log(tmp_path)
    log.info("outside")

    async def request(n):
        with log.trace_span(f"req{n}") as ctx:
            await asyncio.sleep(0.01 * (n % 2))
            log.info(f"working {n}")
            return ctx

    async def main():
        return await asyncio.gather(*(request(n) for n in range(4)))

    ctxs = asyncio.run(main())
    assert current_trace() is None
    with log.trace_span("incoming", traceparent=SAMPLE_TRACEPARENT) as parent:
        with log.trace_span("nested") as nested:
            log.warn("deep")
    log.flush()
    log.shutdown()

    entries = {e["message"]: e for e in read_entries(out)}
    assert "trace_id" not in entries["outside"]["metadata"]
    for n, ctx in enumerate(ctxs):
        meta = entries[f"working {n}"]["metadata"]
        assert meta["trace_id"] == ctx.trace_id and meta["span_id"] == ctx.span_id
    assert len({c.trace_id for c in ctxs}) == 4, "independent tasks get independent traces"
    assert parent.trace_id == "4bf92f3577b34da6a3ce929d0e0e4736"
    assert nested.trace_id == parent.trace_id and nested.span_id != parent.span_id
    assert entries["deep"]["metadata"]["span_id"] == nested.span_id
    span_entry = entries["nested"]["metadata"]["span"]
    assert span_entry["parent_span_id"] == parent.span_id
    assert entries["incoming"]["metadata"]["span"]["parent_span_id"] == "00f067aa0ba902b7"


def test_typed_events(tmp_path):
    import dataclasses
    import datetime

    @dataclasses.dataclass
    class OrderPlaced:
        __event_version__ = 3
        order_id: int
        placed_at: datetime.datetime

    @dataclasses.dataclass
    class PaymentFailed:
        __event_name__ = "payment_failed"
        __event_level__ = "error"
        reason: str

    log, out = make_log(tmp_path)
    log.event(OrderPlaced(7, datetime.datetime(2026, 1, 2, 3, 4, 5)), channel="web")
    log.event(PaymentFailed("declined"))
    log.event("heartbeat", beat=1)
    with pytest.raises(TypeError):
        log.event(object())
    log.flush()
    log.shutdown()

    entries = {e["message"]: e for e in read_entries(out)}
    order = entries["OrderPlaced"]
    assert order["metadata"]["order_id"] == 7
    assert order["metadata"]["channel"] == "web"
    assert order["metadata"]["event"] == "OrderPlaced"
    assert order["metadata"]["event_version"] == 3
    assert order["metadata"]["placed_at"].startswith("2026-01-02")
    assert entries["payment_failed"]["level"] == "ERROR"
    assert entries["payment_failed"]["metadata"]["event_version"] == 1
    assert entries["heartbeat"]["metadata"]["beat"] == 1

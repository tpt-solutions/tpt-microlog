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

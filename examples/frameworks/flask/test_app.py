import json
import os
import tempfile

from tpt_microlog import MicroLog

from app import create_app

TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"


def make():
    out = os.path.join(tempfile.mkdtemp(), "flask.jsonl")
    log = MicroLog(backend="file", file_path=out, wal_enabled=False, flush_interval=0.05)
    return create_app(log), log, out


def read(log, out):
    log.flush()
    with open(out, encoding="utf-8") as fh:
        return [json.loads(line) for line in fh if line.strip()]


def test_request_logging_ids_and_trace_correlation():
    app, log, out = make()
    client = app.test_client()
    r = client.get("/hello/ada", headers={"traceparent": TRACEPARENT, "x-request-id": "req-9"})
    assert r.status_code == 200 and r.get_json() == {"hello": "ada"}
    assert r.headers["x-request-id"] == "req-9"
    by_message = {e["message"]: e for e in read(log, out)}
    greeting, access = by_message["greeting"]["metadata"], by_message["request"]["metadata"]
    assert greeting["trace_id"] == "4bf92f3577b34da6a3ce929d0e0e4736"
    assert greeting["span_id"] == access["span_id"]
    assert access["request_id"] == "req-9" and access["status"] == 200
    log.shutdown()


def test_unhandled_exception_is_logged_and_the_span_still_closes():
    app, log, out = make()
    app.testing = False  # let Flask turn the exception into a 500
    r = app.test_client().get("/boom")
    assert r.status_code == 500
    entries = read(log, out)
    errors = [e for e in entries if e["message"] == "unhandled exception"]
    assert errors and "something broke" in errors[0]["metadata"]["error"]
    # The request span was recorded even though the view raised.
    assert any(e["type"] == "trace" and e["message"] == "GET /boom" for e in entries)
    log.shutdown()

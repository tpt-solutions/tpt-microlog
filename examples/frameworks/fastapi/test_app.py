import json
import os
import tempfile

# Configure the logger before the app module creates it at import time.
OUT = os.path.join(tempfile.mkdtemp(), "fastapi.jsonl")
os.environ.update(
    TPT_LOG_BACKEND="file",
    TPT_LOG_FILE_PATH=OUT,
    TPT_LOG_WAL_ENABLED="false",
    TPT_LOG_FLUSH_INTERVAL_MS="50",
)

from fastapi.testclient import TestClient  # noqa: E402

from app import app, log  # noqa: E402

TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"


def entries():
    log.flush()
    with open(OUT, encoding="utf-8") as fh:
        return [json.loads(line) for line in fh if line.strip()]


def test_requests_are_logged_with_ids_and_trace_correlation():
    with TestClient(app, raise_server_exceptions=False) as client:
        r = client.get("/hello/ada", headers={"traceparent": TRACEPARENT, "x-request-id": "req-1"})
        assert r.status_code == 200 and r.json() == {"hello": "ada"}
        assert r.headers["x-request-id"] == "req-1"
        assert r.headers["traceparent"].startswith("00-4bf92f3577b34da6a3ce929d0e0e4736-")

        by_message = {e["message"]: e for e in entries()}
        greeting = by_message["greeting"]["metadata"]
        access = by_message["request"]["metadata"]
        assert greeting["trace_id"] == "4bf92f3577b34da6a3ce929d0e0e4736"
        assert greeting["span_id"] == access["span_id"], "same span for the whole request"
        assert access["request_id"] == "req-1" and access["status"] == 200
        assert access["path"] == "/hello/ada"

        r = client.get("/boom")
        assert r.status_code == 500
        errors = [e for e in entries() if e["message"] == "unhandled exception"]
        assert errors and "something broke" in errors[0]["metadata"]["error"]
        assert errors[0]["level"] == "ERROR"
        assert "trace_id" in errors[0]["metadata"]

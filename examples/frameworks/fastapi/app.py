"""tpt-microlog with FastAPI: access logs, request ids and trace correlation.

    pip install fastapi uvicorn tpt-microlog
    TPT_LOG_BACKEND=console uvicorn app:app

* one logger for the process; backend and options come from `tpt-log.toml` /
  `TPT_LOG_*` (set `TPT_LOG_BACKEND=datadog TPT_LOG_API_KEY=...` for real use);
* middleware gives every request an id (honoring an incoming `x-request-id`) and
  a trace span (continuing an incoming W3C `traceparent`), so every log line
  written while handling the request carries `trace_id`/`span_id`;
* the lifespan hook flushes and drains on shutdown.

Tested with the framework's TestClient (see test_app.py).
"""

import time
import uuid
from contextlib import asynccontextmanager

from fastapi import FastAPI, Request
from tpt_microlog import MicroLog

log = MicroLog()


@asynccontextmanager
async def lifespan(app: FastAPI):
    log.info("service starting", framework="fastapi")
    yield
    log.info("service stopping")
    log.shutdown()  # flush + drain; bounded by shutdown_timeout_ms


app = FastAPI(lifespan=lifespan)


@app.middleware("http")
async def access_log(request: Request, call_next):
    request_id = request.headers.get("x-request-id") or uuid.uuid4().hex[:12]
    started = time.perf_counter()
    route = f"{request.method} {request.url.path}"
    with log.trace_span(route, traceparent=request.headers.get("traceparent")) as span:
        try:
            response = await call_next(request)
        except Exception as exc:  # unhandled: log it, let FastAPI answer 500
            log.error(
                "unhandled exception",
                request_id=request_id,
                path=request.url.path,
                error=repr(exc),
            )
            raise
        log.info(
            "request",
            request_id=request_id,
            method=request.method,
            path=request.url.path,
            status=response.status_code,
            ms=round((time.perf_counter() - started) * 1000, 2),
        )
    response.headers["x-request-id"] = request_id
    response.headers["traceparent"] = span.traceparent  # for downstream calls
    return response


@app.get("/hello/{name}")
async def hello(name: str):
    log.info("greeting", name=name)  # carries the request's trace_id / span_id
    return {"hello": name}


@app.get("/boom")
async def boom():
    raise RuntimeError("something broke")

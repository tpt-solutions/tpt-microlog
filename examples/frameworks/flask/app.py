"""tpt-microlog with Flask: access logs, request ids and trace correlation.

    pip install flask tpt-microlog
    TPT_LOG_BACKEND=console flask --app app run

Under a pre-forking server (gunicorn, uWSGI) create the logger *after* the fork,
once per worker: keep this module free of an import-time `MicroLog()` or use
`gunicorn --preload` only if you construct the logger lazily. Here the logger is
created in `create_app()`, which each worker calls itself.

Tested with Flask's test client (see test_app.py).
"""

import time
import uuid

from flask import Flask, g, jsonify, request
from tpt_microlog import MicroLog


def create_app(log: MicroLog | None = None) -> Flask:
    app = Flask(__name__)
    app.log = log or MicroLog()

    @app.before_request
    def start_request():
        g.request_id = request.headers.get("x-request-id") or uuid.uuid4().hex[:12]
        g.started = time.perf_counter()
        # Open a span for the request; it is closed in teardown_request.
        g.span = app.log.trace_span(
            f"{request.method} {request.path}",
            traceparent=request.headers.get("traceparent"),
        )
        g.ctx = g.span.__enter__()

    @app.after_request
    def finish_request(response):
        app.log.info(
            "request",
            request_id=g.request_id,
            method=request.method,
            path=request.path,
            status=response.status_code,
            ms=round((time.perf_counter() - g.started) * 1000, 2),
        )
        response.headers["x-request-id"] = g.request_id
        response.headers["traceparent"] = g.ctx.traceparent
        return response

    @app.teardown_request
    def end_span(exc):
        span = g.pop("span", None)
        if exc is not None:
            app.log.error("unhandled exception", request_id=g.get("request_id"), error=repr(exc))
        if span is not None:
            span.__exit__(None, None, None)

    @app.get("/hello/<name>")
    def hello(name):
        app.log.info("greeting", name=name)
        return jsonify(hello=name)

    @app.get("/boom")
    def boom():
        raise RuntimeError("something broke")

    return app


if __name__ == "__main__":
    create_app().run()

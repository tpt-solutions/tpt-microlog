"""tpt-microlog — the microscopic telemetry & log router.

Guaranteed delivery, zero infrastructure. Embeds in your process, batches
and compresses logs, and retries until they are delivered.

Example:
    from tpt_microlog import MicroLog

    log = MicroLog(backend="datadog", api_key="your-api-key")
    log.info("User logged in", user_id=123, ip="1.2.3.4")
    log.metric("cpu_usage", 42.5, tags={"host": "server-01"})
    log.shutdown()
"""

import atexit
import contextvars
import dataclasses
import json
import os
import re
import secrets
import time
import weakref
from contextlib import contextmanager
from typing import NamedTuple, Optional

from tpt_microlog._native import MicroLog as _NativeMicroLog
from tpt_microlog._native import load_config, set_on_error

__all__ = [
    "MicroLog",
    "TraceContext",
    "current_trace",
    "load_config",
    "set_on_error",
]
__version__ = "0.1.0"

# Live loggers (weakly held) so they can be flushed on interpreter exit and
# detached in forked children, where the native worker threads do not exist.
_live_loggers: "weakref.WeakSet" = weakref.WeakSet()


_TRACEPARENT = re.compile(
    r"^([0-9a-f]{2})-([0-9a-f]{32})-([0-9a-f]{16})-([0-9a-f]{2})(-.*)?$"
)


class TraceContext(NamedTuple):
    """A W3C trace-context identity (see https://www.w3.org/TR/trace-context/)."""

    trace_id: str  # 32 lower-case hex characters
    span_id: str  # 16 lower-case hex characters
    sampled: bool = True

    @property
    def traceparent(self) -> str:
        """The `traceparent` header to send downstream."""
        return f"00-{self.trace_id}-{self.span_id}-{'01' if self.sampled else '00'}"

    @classmethod
    def new_root(cls) -> "TraceContext":
        return cls(secrets.token_hex(16), secrets.token_hex(8), True)

    def child(self) -> "TraceContext":
        """A new span in the same trace."""
        return TraceContext(self.trace_id, secrets.token_hex(8), self.sampled)

    @classmethod
    def parse(cls, header: Optional[str]) -> Optional["TraceContext"]:
        """Parse a `traceparent` header; None if it is missing or malformed."""
        match = _TRACEPARENT.match((header or "").strip())
        if not match:
            return None
        version, trace_id, parent_id, flags, extra = match.groups()
        if version == "ff" or (version == "00" and extra):
            return None
        if set(trace_id) == {"0"} or set(parent_id) == {"0"}:
            return None
        return cls(trace_id, parent_id, bool(int(flags, 16) & 1))


_current_trace: "contextvars.ContextVar[Optional[TraceContext]]" = contextvars.ContextVar(
    "tpt_microlog_trace", default=None
)


def current_trace() -> Optional[TraceContext]:
    """The trace context active in this thread / asyncio task, if any."""
    return _current_trace.get()


class MicroLog(_NativeMicroLog):
    """Native MicroLog with interpreter-exit and fork safety.

    Usage:
        log = MicroLog(backend="file", file_path="app.jsonl")
        with log:               # shutdown on exit
            log.info("hello", request_id="abc")
    """

    def __init__(self, *args, **kwargs):
        # The native constructor runs via __new__ (which already received
        # *args/**kwargs); __init__ must not forward them again.
        super().__init__()
        _live_loggers.add(self)

    @contextmanager
    def trace_span(self, name: str, traceparent: Optional[str] = None):
        """Open a span: log calls inside it carry `trace_id` / `span_id`.

        `traceparent` continues an incoming W3C header; otherwise the span
        continues the current one or starts a new trace. The span is recorded
        (with its duration) when the block exits. Yields the span's
        :class:`TraceContext`; send ``ctx.traceparent`` to downstream services.
        Works across threads and asyncio tasks (it is `contextvars` based).
        """
        parent = TraceContext.parse(traceparent) or _current_trace.get()
        ctx = parent.child() if parent else TraceContext.new_root()
        token = _current_trace.set(ctx)
        started = time.perf_counter()
        try:
            yield ctx
        finally:
            _current_trace.reset(token)
            self.span(
                name,
                ctx.trace_id,
                ctx.span_id,
                parent.span_id if parent else None,
                int((time.perf_counter() - started) * 1_000_000),
            )

    def event(self, event, /, **fields):
        """Log a typed event.

        ``event`` is a dataclass instance, a pydantic v2 model, or a plain name
        (``log.event("order_placed", order_id=7)``). The entry message is the
        event name; fields become metadata alongside ``event`` and
        ``event_version``. Customize with class attributes::

            @dataclasses.dataclass
            class PaymentFailed:
                __event_name__ = "payment_failed"   # default: the class name
                __event_version__ = 2               # default: 1
                __event_level__ = "error"           # default: "info"
                reason: str
        """
        if isinstance(event, str):
            name, version, level, data = event, 1, "info", {}
        else:
            cls = type(event)
            if dataclasses.is_dataclass(event) and not isinstance(event, type):
                data = dataclasses.asdict(event)
            elif hasattr(event, "model_dump"):
                data = event.model_dump(mode="json")
            else:
                raise TypeError(
                    "event must be a name, a dataclass instance or a pydantic model, "
                    f"not {cls.__name__}"
                )
            name = getattr(cls, "__event_name__", cls.__name__)
            version = getattr(cls, "__event_version__", 1)
            level = getattr(cls, "__event_level__", "info")
        data.update(fields)
        # Non-JSON values (datetimes, Decimals, ...) fall back to str().
        data = json.loads(json.dumps(data, default=str))
        data["event"] = name
        data["event_version"] = version
        if level not in ("trace", "debug", "info", "warn", "error", "fatal"):
            raise ValueError(f"unknown event level {level!r}")
        return getattr(self, level)(name, **data)

    @staticmethod
    def _correlate(kwargs: dict) -> dict:
        ctx = _current_trace.get()
        if ctx is not None:
            kwargs.setdefault("trace_id", ctx.trace_id)
            kwargs.setdefault("span_id", ctx.span_id)
        return kwargs

    def trace(self, message, **kwargs):
        return super().trace(message, **self._correlate(kwargs))

    def debug(self, message, **kwargs):
        return super().debug(message, **self._correlate(kwargs))

    def info(self, message, **kwargs):
        return super().info(message, **self._correlate(kwargs))

    def warn(self, message, **kwargs):
        return super().warn(message, **self._correlate(kwargs))

    def error(self, message, **kwargs):
        return super().error(message, **self._correlate(kwargs))

    def fatal(self, message, **kwargs):
        return super().fatal(message, **self._correlate(kwargs))

    def shutdown(self, *args, **kwargs):
        result = super().shutdown(*args, **kwargs)
        _live_loggers.discard(self)
        return result


def _shutdown_all_at_exit() -> None:
    for log in list(_live_loggers):
        try:
            log.shutdown()
        except Exception:
            pass  # never let teardown masking a crash


def _detach_all_after_fork() -> None:
    for log in list(_live_loggers):
        try:
            log._detach_after_fork()
        except Exception:
            pass


atexit.register(_shutdown_all_at_exit)
if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_detach_all_after_fork)

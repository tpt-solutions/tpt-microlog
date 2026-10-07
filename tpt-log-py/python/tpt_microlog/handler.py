"""Standard-library ``logging`` integration.

    import logging
    from tpt_microlog.handler import MicroLogHandler

    logging.getLogger().addHandler(MicroLogHandler(backend="datadog", api_key="..."))
    logging.getLogger("checkout").warning("slow query", extra={"ms": 840})
"""

from __future__ import annotations

import logging
import traceback
from typing import Any, Optional

from tpt_microlog import MicroLog

__all__ = ["MicroLogHandler"]

# Attributes every LogRecord has; anything else came from ``extra=``.
_STANDARD_ATTRS = frozenset(
    logging.LogRecord("", 0, "", 0, "", (), None).__dict__.keys()
) | {"message", "asctime", "taskName"}

_JSON_SCALARS = (str, int, float, bool, type(None))


def _jsonable(value: Any, _depth: int = 0) -> Any:
    """Coerce arbitrary ``extra`` values into JSON-safe data."""
    if isinstance(value, _JSON_SCALARS):
        return value
    if _depth < 4:
        if isinstance(value, dict):
            return {str(k): _jsonable(v, _depth + 1) for k, v in value.items()}
        if isinstance(value, (list, tuple, set, frozenset)):
            return [_jsonable(v, _depth + 1) for v in value]
    return repr(value)


class MicroLogHandler(logging.Handler):
    """A ``logging.Handler`` that forwards records to a :class:`MicroLog`.

    Pass an existing logger with ``log=``, or keyword options to build (and
    own) one: ``MicroLogHandler(backend="file", file_path="app.jsonl")``. An
    owned logger is shut down by :meth:`close`; a borrowed one is only flushed.

    Records become entries with ``logger``, ``module``, ``function``, ``line``
    and ``thread`` metadata, plus every ``extra=`` field and, when present, the
    formatted traceback under ``exception``.
    """

    def __init__(
        self,
        log: Optional[MicroLog] = None,
        level: int = logging.NOTSET,
        **options: Any,
    ) -> None:
        super().__init__(level)
        if log is not None and options:
            raise ValueError("pass either `log` or MicroLog options, not both")
        self._owned = log is None
        self.log = log if log is not None else MicroLog(**options)

    def emit(self, record: logging.LogRecord) -> None:
        # The binding's own diagnostics must never feed back into itself.
        if record.name.startswith("tpt_microlog"):
            return
        try:
            meta = {
                "logger": record.name,
                "module": record.module,
                "function": record.funcName,
                "line": record.lineno,
                "thread": record.threadName,
            }
            for key, value in record.__dict__.items():
                if key not in _STANDARD_ATTRS and not key.startswith("_"):
                    meta[key] = _jsonable(value)
            if record.exc_info and record.exc_info[0] is not None:
                meta["exception"] = "".join(
                    traceback.format_exception(*record.exc_info)
                ).rstrip()
            elif record.exc_text:
                meta["exception"] = record.exc_text
            # Logging's own text (``%`` args applied), not the formatter's
            # decoration: structure lives in the metadata.
            self._dispatch(record.levelno, record.getMessage(), meta)
        except Exception:
            self.handleError(record)

    def _dispatch(self, levelno: int, message: str, meta: dict) -> None:
        log = self.log
        if levelno >= logging.CRITICAL:
            log.fatal(message, **meta)
        elif levelno >= logging.ERROR:
            log.error(message, **meta)
        elif levelno >= logging.WARNING:
            log.warn(message, **meta)
        elif levelno >= logging.INFO:
            log.info(message, **meta)
        elif levelno >= logging.DEBUG:
            log.debug(message, **meta)
        else:
            log.trace(message, **meta)

    def flush(self) -> None:
        try:
            self.log.flush()
        except Exception:
            pass

    def close(self) -> None:
        try:
            if self._owned:
                self.log.shutdown()
            else:
                self.log.flush()
        finally:
            super().close()

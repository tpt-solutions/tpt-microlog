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
import os
import weakref

from tpt_microlog._native import MicroLog as _NativeMicroLog
from tpt_microlog._native import load_config, set_on_error

__all__ = ["MicroLog", "load_config", "set_on_error"]
__version__ = "0.1.0"

# Live loggers (weakly held) so they can be flushed on interpreter exit and
# detached in forked children, where the native worker threads do not exist.
_live_loggers: "weakref.WeakSet" = weakref.WeakSet()


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

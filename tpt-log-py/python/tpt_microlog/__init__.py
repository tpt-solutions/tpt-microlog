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

from tpt_microlog._native import MicroLog, load_config

__all__ = ["MicroLog", "load_config"]
__version__ = "0.1.0"

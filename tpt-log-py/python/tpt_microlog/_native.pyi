"""Type stubs for the compiled `tpt_microlog._native` extension module."""

from typing import Any, Mapping

_Metadata = Mapping[str, Any] | None

class MicroLog:
    """Ultra-lightweight telemetry & log router with guaranteed delivery.

    Keyword options (all optional — defaults come from `tpt-log.toml` and
    `TPT_LOG_*` environment variables):

    - backend: one of "none", "custom"/"http", "datadog", "elasticsearch",
      "cloudwatch", "prometheus", "grpc", "udp", "tcp", "file"
    - endpoint: base URL for HTTP-style backends
    - api_key: secret token (Datadog DD-API-KEY, etc.)
    - service: logical service name attached to every entry
    - batch_size: flush at this many entries (default 100)
    - flush_interval: flush at least this often, in seconds (default 1.0)
    - memory_limit_mb: flush once buffered bytes cross this (default 10)
    - compression_level: zstd level 1-21 (default 3)
    - fast_mode: disable compression for extreme throughput
    - max_retries: delivery attempts with exponential backoff (default 10)
    - wal_enabled / wal_dir / wal_max_size_mb: disk persistence on failure
    - min_level: drop entries below this ("TRACE".."FATAL")
    - sample_rate: fraction of entries to deliver, 0.0-1.0 (default 1.0)
    - enrichment: attach hostname / PID / service to every entry
    - extra_headers: list of "Name=Value" HTTP headers
    """

    def __init__(self, **kwargs: Any) -> None: ...
    def trace(self, message: str, /, **metadata: Any) -> None: ...
    def debug(self, message: str, /, **metadata: Any) -> None: ...
    def info(self, message: str, /, **metadata: Any) -> None: ...
    def warn(self, message: str, /, **metadata: Any) -> None: ...
    def error(self, message: str, /, **metadata: Any) -> None: ...
    def fatal(self, message: str, /, **metadata: Any) -> None: ...
    def metric(
        self, name: str, value: float, tags: Mapping[str, Any] | None = None
    ) -> None: ...
    def span(
        self,
        name: str,
        trace_id: str,
        span_id: str,
        parent_span_id: str | None = None,
        duration_us: int = 0,
    ) -> None: ...
    def queued(self) -> int: ...
    @property
    def backend(self) -> str: ...
    def shutdown(self) -> None: ...

def load_config(path: str) -> MicroLog: ...

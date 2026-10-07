"""Type stubs for the compiled `tpt_microlog._native` extension module."""

from typing import Any, Callable, Mapping

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
    - redact_keys: metadata keys (case-insensitive) whose values are masked
    - redact_patterns: regexes masked in messages and string metadata
    - redact_builtin: mask emails, cards, SSNs, JWTs, bearer tokens, AWS keys
    - filter_tags: drop entries carrying any "tag=value" pair
    - filter_regex: drop entries whose message matches
    - sample_rate_by_level: {"DEBUG": 0.1} per-level sampling (WARN+ is never sampled)
    - queue_full_policy: "block" | "drop_newest" | "drop_oldest"
    - durability: "best-effort" | "wal-first" (write the WAL before the first send)
    - shutdown_timeout_ms / wal_retry_interval_ms / wal_dead_letter_replays /
      retry_after_max_ms: delivery and WAL timing
    - enrich_env_vars: environment variable names captured into entries
    - backends / failover: fan-out (or ordered failover) routing, with backend="fanout"
    - ca_cert / proxy: TLS bundle and HTTP(S) proxy for network backends
    - preset: "dev", "prod", "grafana-loki", ... applied before other options
    - dedup_window_ms: fold identical entries within the window into one with `repeat_count`
    - rate_limit_per_sec: cap routine (below WARN) entries per second
    - adaptive_sampling / adaptive_sampling_floor: sample harder as the queue fills
    - flight_recorder_size / flight_recorder_max_level: keep recent DEBUG context, ship it only on ERROR+
    - daily_budget_entries: auto-tightening daily volume budget (WARN+ never limited)
    - health_bind: "host:port" serving /healthz and /stats
    - self_metrics_interval_ms: emit the logger's own health as metrics
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
    def stats(self) -> dict[str, int]: ...
    def health(self) -> dict[str, Any]: ...
    @property
    def backend(self) -> str: ...
    def shutdown(self) -> None: ...

def load_config(path: str) -> MicroLog: ...
def set_on_error(callback: Callable[[str, str], None] | None = None) -> None:
    """Receive internal failures as (kind, message); kind is one of
    "delivery", "wal", "worker", "config". Pass None to restore stderr."""


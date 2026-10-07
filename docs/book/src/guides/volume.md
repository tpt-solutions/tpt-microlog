# Controlling volume

Logging cost scales with volume, and volume is dominated by routine entries.
These controls thin out *routine* traffic and leave errors alone. **WARN and
above are never sampled, rate limited or budget-limited**, and metrics and
trace spans are exempt too.

Entries pass the controls in this order: drop filters, flight recorder,
`min_level`, sampling (static x adaptive x budget), rate limit, de-duplication.
Each control that drops an entry has its own counter in `stats()`, so you can
see what is being thinned and by what.

| Control | Setting | Counter |
| --- | --- | --- |
| Level floor | `min_level` | (none) |
| Drop by message / tag | `filter_regex`, `filter_tags` | (none) |
| Static sampling | `sample_rate`, `[sample_rate_by_level]` | `sampled_entries` |
| Adaptive sampling | `adaptive_sampling`, `adaptive_sampling_floor` | `sampled_entries` |
| Daily budget | `daily_budget_entries` | `sampled_entries` |
| Rate limit | `rate_limit_per_sec` | `rate_limited_entries` |
| Burst de-duplication | `dedup_window_ms` | `deduplicated_entries` |
| Flight recorder | `flight_recorder_size` | `recorded_entries` |

## Sampling

```toml
[sample_rate_by_level]
DEBUG = 0.05
INFO = 0.5
```

keeps 5% of DEBUG and half of INFO. `sample_rate` sets the default for levels
without an override.

## Adaptive sampling

```toml
adaptive_sampling = true
adaptive_sampling_floor = 0.05
```

While the ingestion queue is less than half full nothing changes. Past that, the
sampling rate for routine entries falls linearly from 1.0 to the floor as the
queue fills, so a backend slowdown sheds routine load instead of blocking your
threads or dropping at random. Errors always pass.

## Daily budget

```toml
daily_budget_entries = 5000000
```

The logger watches how fast routine entries arrive during the (UTC) day and
chooses a sampling rate so the projected total lands on the budget; it leaves
traffic untouched while you are on track (and for the first minute of the day,
when the rate is too noisy to project from). Once the budget is spent, only
WARN and above get through until the next UTC day. Errors still count against
the spend; they are just never blocked.

## Rate limit

```toml
rate_limit_per_sec = 2000
```

A token bucket (one second of burst) over routine entries: a hard ceiling that
holds even when sampling alone would not.

## De-duplication

```toml
dedup_window_ms = 5000
```

Identical entries (same level and message) inside the window are folded into the
first one. When the window closes, a summary entry with `repeat_count = N` and
`repeat_summary = true` records how many were folded, so a tight error loop
becomes two entries instead of a million. Open windows are flushed on `flush()`
and `shutdown()`, and the number of distinct messages tracked is bounded.

## Flight recorder

```toml
min_level = "INFO"
flight_recorder_size = 200
flight_recorder_max_level = "DEBUG"
```

Entries at or below `flight_recorder_max_level` are held in a ring of
`flight_recorder_size` entries (already redacted) instead of being shipped.
When an ERROR or FATAL is logged, the ring is emitted first, oldest to newest,
each marked `flight_recorder = true`, then the error itself. You get the DEBUG
context that explains a failure, and pay for none of it on the days nothing
fails. The ring is emptied each time, so a second error does not repeat stale
context. Note that these entries are never shipped on their own, even if
`min_level` would have allowed them.

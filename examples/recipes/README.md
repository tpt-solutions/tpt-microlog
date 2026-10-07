# Recipes

Small, runnable answers to common questions. Every script asserts its own
result, so they are also exercised by CI. Install the Python binding first
(`pip install tpt-microlog`, or `maturin develop` in `tpt-log-py`).

| Recipe | Shows |
| --- | --- |
| [`request_id_correlation.py`](request_id_correlation.py) | One id per request on every log line (`contextvars` + a `logging.Filter`), correct under asyncio interleaving. |
| [`pii_redaction.py`](pii_redaction.py) | `redact_keys`, custom `redact_patterns` and `redact_builtin`: secrets never reach the WAL or the network. |
| [`sampling_noisy_debug.py`](sampling_noisy_debug.py) | `sample_rate_by_level` thins DEBUG/INFO while WARN+ is never sampled; `filter_regex` drops health checks. |
| [`offline_first_wal.py`](offline_first_wal.py) | A collector outage parks batches in the WAL; a restarted logger replays them. Nothing is lost. |
| [`migrate_from_logging.py`](migrate_from_logging.py) | Adopt tpt-microlog from the stdlib `logging` module by adding one handler. |

```sh
python examples/recipes/offline_first_wal.py
```

The same features are available from every binding; see each package's README
for the option names in that language.

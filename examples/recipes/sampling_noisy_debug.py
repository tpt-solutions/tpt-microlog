"""Recipe: keep the signal, sample the noise.

`sample_rate_by_level` thins out chatty levels while WARN and above are
*never* sampled, so you cannot lose an error to sampling. Metrics and trace
spans are exempt as well. `filter_regex` / `filter_tags` drop known-useless
lines outright (health checks, for example).

Run: python examples/recipes/sampling_noisy_debug.py
"""

import json
import os
import tempfile
from collections import Counter

from tpt_microlog import MicroLog

out = os.path.join(tempfile.mkdtemp(), "sampled.jsonl")
log = MicroLog(
    backend="file",
    file_path=out,
    wal_enabled=False,
    queue_full_policy="block",       # never drop at the queue; sampling decides
    sample_rate_by_level={"DEBUG": 0.1, "INFO": 0.5},
    filter_regex=r"^GET /healthz",   # drop health-check noise entirely
)

for i in range(2000):
    log.debug("cache lookup", i=i)
for i in range(1000):
    log.info("request handled", i=i)
for i in range(50):
    log.warn("slow upstream", i=i)
    log.error("upstream failed", i=i)
log.info("GET /healthz 200")        # filtered
log.metric("queue_depth", 3)        # exempt from sampling
log.shutdown()

with open(out, encoding="utf-8") as fh:
    entries = [json.loads(line) for line in fh]
counts = Counter(e["level"] for e in entries if e["type"] == "log")
print("kept:", dict(counts))

assert counts["WARN"] == 50 and counts["ERROR"] == 50, "WARN and above are never sampled"
assert 120 <= counts["DEBUG"] <= 290, f"DEBUG ~10% of 2000, got {counts['DEBUG']}"
assert 400 <= counts["INFO"] <= 600, f"INFO ~50% of 1000, got {counts['INFO']}"
assert not any("healthz" in e["message"] for e in entries)
assert any(e["type"] == "metric" for e in entries)
print("ok")

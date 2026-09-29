"""tpt-microlog Python example — zero-config local JSONL file backend.

Writes to `logs.jsonl` with rotation; handy for local debugging or forwarding
with another tool. Run: `python examples/python/local_file.py`
"""

import json
import os
import tempfile

from tpt_microlog import MicroLog

out = os.path.join(tempfile.mkdtemp(), "logs.jsonl")

log = MicroLog(
    backend="file",
    file_path=out,
    file_rotation_bytes=1_000_000,
    file_rotations=3,
    enrichment=True,          # attach hostname / PID / service
    service="local-dev",
)

log.info("service started", env="dev")
log.metric("heartbeat", 1, tags={"interval_s": "30"})
log.shutdown()

print(f"wrote {out}:")
with open(out, encoding="utf-8") as fh:
    for line in fh:
        print(" ", json.dumps(json.loads(line)))

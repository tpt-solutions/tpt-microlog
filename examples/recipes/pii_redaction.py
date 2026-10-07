"""Recipe: keep secrets and PII out of logs, the WAL and the network.

Redaction runs inside the engine *before* an entry is queued, so neither the
on-disk write-ahead log nor any backend ever sees the original value.

Three layers, use any combination:
  * redact_keys     - metadata keys whose value is always masked
  * redact_patterns - your own regexes (matched in the message and in strings)
  * redact_builtin  - emails, card numbers, SSNs, JWTs, bearer tokens, AWS keys

Run: python examples/recipes/pii_redaction.py
"""

import json
import os
import tempfile

from tpt_microlog import MicroLog

out = os.path.join(tempfile.mkdtemp(), "redacted.jsonl")
log = MicroLog(
    backend="file",
    file_path=out,
    wal_enabled=False,
    redact_keys=["password", "authorization", "api_key"],
    redact_patterns=[r"ORD-\d{6}"],  # an internal identifier format
    redact_builtin=True,
)

log.info(
    "signup from alice@example.com for ORD-123456",
    password="hunter2",
    headers={"Authorization": "Bearer abc.def.ghi", "accept": "*/*"},
    card="4111 1111 1111 1111",
    note="call me, SSN 123-45-6789",
    user_id=42,  # untouched
)
log.shutdown()

with open(out, encoding="utf-8") as fh:
    entry = json.loads(fh.readline())

blob = json.dumps(entry)
for secret in ("alice@example.com", "ORD-123456", "hunter2", "abc.def.ghi",
               "4111 1111 1111 1111", "123-45-6789"):
    assert secret not in blob, f"leaked {secret!r}: {blob}"
assert entry["metadata"]["user_id"] == 42
assert entry["metadata"]["headers"]["accept"] == "*/*"
print("ok:", entry["message"])
print("    ", json.dumps(entry["metadata"]))

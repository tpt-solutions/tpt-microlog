"""Recipe: logs survive an outage - offline-first delivery with the WAL.

1. The collector is down: the logger cannot deliver, so batches land in the
   on-disk write-ahead log instead of being dropped.
2. The collector comes back and the process restarts (or just keeps
   running): the WAL is replayed in order and the files are removed.

The WAL is namespaced by service + backend + endpoint, so the second logger
must be configured for the same destination to pick the batches up.

Run: python examples/recipes/offline_first_wal.py
"""

import json
import os
import socket
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

from tpt_microlog import MicroLog

wal_dir = tempfile.mkdtemp(prefix="wal-demo-")

# Reserve a port that nothing listens on yet.
with socket.socket() as probe:
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
endpoint = f"http://127.0.0.1:{port}/logs"


def make_logger() -> MicroLog:
    return MicroLog(
        backend="custom",
        endpoint=endpoint,
        service="wal-demo",
        wal_enabled=True,
        wal_dir=wal_dir,
        batch_size=1,
        max_retries=1,                 # give up on the network quickly...
        retry_base_ms=10,              # ...and let the WAL take over
        wal_retry_interval_ms=200,     # replay pass every 200 ms
        durability="best-effort",
    )


def wal_files() -> list:
    return [
        os.path.join(root, name)
        for root, _dirs, names in os.walk(wal_dir)
        for name in names
        if name.endswith(".wal") and os.sep + "dead" + os.sep not in os.path.join(root, name)
    ]


# --- 1. outage ---------------------------------------------------------------
log = make_logger()
for n in range(3):
    log.info(f"written while offline #{n}")
log.flush()
log.shutdown()
parked = wal_files()
print(f"collector down: {len(parked)} batch(es) parked in the WAL")
assert parked, "undelivered batches must be persisted, not dropped"

# --- 2. recovery -------------------------------------------------------------
received: list = []


class Collector(BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802 (stdlib naming)
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        received.extend(e["message"] for e in json.loads(body)["entries"])
        self.send_response(200)
        self.end_headers()

    def log_message(self, *args):  # keep the demo output clean
        pass


server = HTTPServer(("127.0.0.1", port), Collector)
threading.Thread(target=server.serve_forever, daemon=True).start()
print("collector back up; starting a fresh logger on the same WAL...")

log = make_logger()
deadline = time.time() + 15
while time.time() < deadline and len(received) < 3:
    time.sleep(0.1)
log.shutdown()
server.shutdown()

print("collector received:", sorted(received))
assert sorted(received) == [f"written while offline #{n}" for n in range(3)]
assert not wal_files(), "replayed batches are removed from the WAL"
print("ok: nothing lost, WAL drained")

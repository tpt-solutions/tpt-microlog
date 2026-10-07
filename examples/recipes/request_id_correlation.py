"""Recipe: correlate every log line of a request with a request id.

A `contextvars.ContextVar` holds the id for the current request (it follows
asyncio tasks and threads correctly), and a `logging.Filter` copies it onto
every record. `MicroLogHandler` ships all `extra=` style attributes as
metadata, so the id ends up on every entry without touching call sites.

Run: python examples/recipes/request_id_correlation.py
"""

import asyncio
import contextvars
import json
import logging
import os
import tempfile
import uuid

from tpt_microlog.handler import MicroLogHandler

request_id = contextvars.ContextVar("request_id", default="-")


class RequestIdFilter(logging.Filter):
    def filter(self, record: logging.LogRecord) -> bool:
        record.request_id = request_id.get()
        return True


out = os.path.join(tempfile.mkdtemp(), "requests.jsonl")
handler = MicroLogHandler(backend="file", file_path=out, wal_enabled=False)
handler.addFilter(RequestIdFilter())
logging.basicConfig(level=logging.INFO, handlers=[handler])
log = logging.getLogger("shop")


async def handle(order: int) -> None:
    request_id.set(uuid.uuid4().hex[:8])  # one id per request/task
    log.info("order %d received", order)
    await asyncio.sleep(0.01 * (order % 3))  # interleave with other requests
    log.info("order %d charged", order)
    await asyncio.sleep(0.01)
    log.info("order %d shipped", order)


async def main() -> None:
    await asyncio.gather(*(handle(n) for n in range(5)))


asyncio.run(main())
handler.close()

# Every entry of one order shares exactly one request id, and ids differ
# between orders even though the lines were interleaved.
ids_by_order: dict[str, set] = {}
with open(out, encoding="utf-8") as fh:
    for line in fh:
        entry = json.loads(line)
        order = entry["message"].split()[1]
        ids_by_order.setdefault(order, set()).add(entry["metadata"]["request_id"])

assert len(ids_by_order) == 5, ids_by_order
assert all(len(ids) == 1 for ids in ids_by_order.values()), ids_by_order
assert len({next(iter(ids)) for ids in ids_by_order.values()}) == 5, "ids must be distinct"
print("ok: 5 interleaved requests, each log line carries its own request_id")

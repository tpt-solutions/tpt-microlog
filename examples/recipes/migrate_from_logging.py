"""Recipe: migrate an app from the standard `logging` module - zero call-site edits.

Existing code keeps calling `logging.getLogger(...).info(...)`. You add one
handler and (optionally) keep your console output. `%`-formatting, `extra=`
and `exception()` all keep working, and `extra=` fields become structured
metadata instead of being lost in a format string.

Run: python examples/recipes/migrate_from_logging.py
"""

import json
import logging
import os
import tempfile

# --- the application code: unchanged -----------------------------------------
log = logging.getLogger("billing.invoices")


def create_invoice(customer: str, cents: int) -> None:
    log.info("invoice created for %s", customer, extra={"cents": cents})
    try:
        1 / 0
    except ZeroDivisionError:
        log.exception("tax calculation failed")


# --- the migration: configure once at startup --------------------------------
from tpt_microlog.handler import MicroLogHandler  # noqa: E402

out = os.path.join(tempfile.mkdtemp(), "migrated.jsonl")
logging.basicConfig(
    level=logging.INFO,
    handlers=[
        logging.StreamHandler(),  # keep the console output you already have
        MicroLogHandler(          # ...and also ship structured entries
            backend="file",       # swap for backend="datadog", api_key=... etc.
            file_path=out,
            wal_enabled=False,
            service="billing",
        ),
    ],
)

create_invoice("acme", 1999)
logging.shutdown()  # flushes and closes every handler (including ours)

with open(out, encoding="utf-8") as fh:
    entries = {e["message"]: e for e in map(json.loads, fh)}

created = entries["invoice created for acme"]
assert created["metadata"]["cents"] == 1999, "extra= became metadata"
assert created["metadata"]["logger"] == "billing.invoices"
assert "ZeroDivisionError" in entries["tax calculation failed"]["metadata"]["exception"]
assert entries["tax calculation failed"]["level"] == "ERROR"
print("ok: existing logging calls now produce structured entries")

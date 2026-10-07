"""Emit a little demo traffic through whatever `tpt-log.toml` is in effect.

    TPT_LOG_CONFIG=examples/stacks/elk/tpt-log.toml python examples/stacks/demo.py
    python examples/stacks/demo.py --seconds 120   # keep gauges alive for scraping

Logs, warnings, errors and a few gauges are produced every second, so every
stack has something to show within moments.
"""

import argparse
import random
import time

from tpt_microlog import MicroLog

parser = argparse.ArgumentParser()
parser.add_argument("--seconds", type=int, default=10, help="how long to keep emitting")
args = parser.parse_args()

log = MicroLog()  # picks up TPT_LOG_CONFIG / ./tpt-log.toml / TPT_LOG_* env
print(f"backend={log.backend}; emitting for {args.seconds}s (Ctrl+C to stop)")

end = time.time() + args.seconds
tick = 0
try:
    while time.time() < end:
        tick += 1
        log.info("request handled", route="/api/items", status=200, ms=random.randint(5, 80))
        if tick % 5 == 0:
            log.warn("slow upstream", upstream="payments", ms=random.randint(400, 900))
        if tick % 9 == 0:
            log.error("payment failed", order_id=f"ORD-{tick:06d}", reason="card_declined")
        log.metric("demo_queue_depth", random.randint(0, 20), tags={"queue": "emails"})
        log.metric("demo_queue_depth", random.randint(0, 5), tags={"queue": "billing"})
        time.sleep(1)
finally:
    log.shutdown()
    print("done")

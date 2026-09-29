"""tpt-microlog Python example — structured logs + metrics to Datadog.

Build and install the wrapper first:

    cd tpt-log-py && maturin develop   # inside a virtualenv

Then run:

    python examples/python/datadog.py
"""

import os
import time

from tpt_microlog import MicroLog

log = MicroLog(
    backend="datadog",
    api_key=os.environ.get("DD_API_KEY", "your-api-key"),
    service="checkout-api",
    batch_size=100,
    flush_interval=1.0,
)

# Structured message with keyword-argument metadata (any JSON-able value).
log.info("User logged in", user_id=123, ip="1.2.3.4")
log.debug("Cart inspected", items=3, cart_value=59.97)
log.warn("Slow query", query="SELECT ...", duration_ms=950)

# Numeric metric with tags.
log.metric("cpu_usage", 42.5, tags={"host": "server-01"})
log.metric("requests", 1, tags={"route": "/checkout", "method": "POST"})

# Distributed-trace span.
log.span("checkout", trace_id="4bf92f35", span_id="00f067aa", duration_us=1520)

# Give the background batcher a moment, then flush and drain before exit.
time.sleep(1.5)
log.shutdown()
print("delivered", log.queued(), "== 0 pending entries")

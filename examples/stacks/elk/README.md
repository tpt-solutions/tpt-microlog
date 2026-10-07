# ELK (Elasticsearch + Kibana)

```sh
docker compose up -d            # wait for Kibana at http://localhost:5601
TPT_LOG_CONFIG=tpt-log.toml python ../demo.py --seconds 20
```

Then in Kibana: **Stack Management -> Data Views -> Create data view**, index
pattern `tpt-microlog*`, timestamp field `@timestamp`, and open **Discover**.

The adapter uses the Bulk API (`/_bulk`) and reports per-item bulk failures, so
mapping conflicts surface as delivery errors instead of silent data loss.

> **Status: untested.** `tpt-log.toml` and the compose file are checked for
> syntax only; the stack has not been started. Adjust image tags as needed.

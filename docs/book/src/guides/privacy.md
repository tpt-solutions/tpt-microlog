# Privacy and redaction

Redaction runs inside the engine, before an entry is queued. That ordering is
the point: the on-disk WAL, the network, `tail`, the support bundle and the
flight recorder only ever see redacted data.

Three mechanisms, usable together:

```toml
redact_keys = ["password", "authorization", "api_key", "token"]  # by metadata key
redact_patterns = ["ORD-\\d{6}"]                                  # your own regexes
redact_builtin = true                                             # common PII/secrets
```

* **`redact_keys`**: any metadata key with that name (case-insensitive, at any
  nesting depth) has its value replaced by `[REDACTED]`.
* **`redact_patterns`**: regexes matched in the message and in every string
  value of the metadata (including nested objects and arrays).
* **`redact_builtin`**: ready-made detectors for email addresses, card numbers,
  US social security numbers, JWTs, `Bearer` tokens and AWS access key ids.

```python
log.info("signup from alice@example.com", password="hunter2")
# -> message "signup from [REDACTED]", metadata {"password": "[REDACTED]"}
```

## What it does not do

* It cannot know that a free-text message contains a name or an address. The
  detectors cover well-known shapes; add `redact_patterns` for your own
  identifiers, and keep sensitive data out of messages in the first place.
* Card-number detection is pattern based (13-19 digits); it can over-match long
  numbers.
* It does not rewrite data already sent before redaction was configured.

## Secrets in the configuration

`api_key`, `aws_secret_key`, `aws_session_token` and `es_basic_auth` are held in
a type that prints as `[redacted]` in debug output, JSON and the support bundle.
Prefer the environment (`TPT_LOG_API_KEY`) or your secret manager to committing
keys to `tpt-log.toml`. The `tpt-microlog support-bundle` command additionally
masks header values, URL credentials and query strings, environment values whose
names look secret, and reduces entry metadata to key names.

## Browsers

Never ship an API key in a browser bundle; point the WebAssembly build at your
own endpoint and let the server forward. `redactKeys`/`redactPatterns`/
`redactBuiltin` are available there too and run before buffering.

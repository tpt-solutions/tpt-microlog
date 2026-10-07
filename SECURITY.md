# Security policy

## Reporting a vulnerability

Please report suspected vulnerabilities **privately** using GitHub's private
vulnerability reporting: open the repository's **Security** tab and choose
**Report a vulnerability**
(<https://github.com/tpt-solutions/tpt-microlog/security/advisories/new>).

Please do not open a public issue or discussion for a security problem, and do
not include working exploits in public places.

Helpful details: affected binding and version, a minimal reproduction, the
impact you believe it has, and whether it is already public.

You can expect an acknowledgement, a fix or a clear explanation of why not, and
credit in the advisory if you want it. This is a small project maintained by
one person, so please allow reasonable time.

## Supported versions

Only the latest released version of each package receives security fixes. The
project is pre-1.0; fixes ship as new patch releases.

## What is in scope

- The Rust crates (`tpt-log-core`, `tpt-log-transport`, `tpt-log-ffi`,
  `tpt-log-cli`) and the Python, WebAssembly and Go bindings.
- Memory-safety problems at the C ABI boundary.
- Credential handling: API keys and AWS secrets are held in a redacting
  `Secret` type and must never appear in `Debug`/serde output or diagnostics.
- Redaction bypasses: data matching `redact_keys`, `redact_patterns` or the
  built-in detectors that still reaches the WAL, a transport or a diagnostic
  message.
- WAL file handling (path traversal, unsafe permissions, lock bypass).

## Out of scope

- Vulnerabilities in the third-party services logs are sent to.
- Secrets placed in `tpt-log.toml` or the environment by the operator, and
  data an application chooses to log without configuring redaction.
- `examples/` and the docker-compose stacks, which are demonstrations
  (security is disabled in some on purpose).
- Denial of service by an application that logs faster than a backend can take
  it; this is governed by `queue_full_policy`.

## Hardening notes for operators

- Prefer `TPT_LOG_API_KEY` (or your secret manager) over committing keys to
  `tpt-log.toml`.
- Turn on `redact_builtin` and list your sensitive metadata keys in
  `redact_keys`; redaction runs before an entry is written to the WAL.
- The WAL directory holds batched log data: give it the same permissions as
  the logs themselves.
- Never ship API keys in browser bundles; use a proxy endpoint in front of the
  vendor for the WebAssembly build.

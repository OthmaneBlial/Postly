# Security policy

Postly handles API workflows that may contain sensitive data. Please read the
[security model](docs/security.md) before enabling scripts, insecure TLS or
sharing a workspace artifact.

Do not publish credentials, private certificates, customer payloads or complete
request exports in an issue or discussion. Remove sensitive values and provide
the smallest reproducible example possible. The repository's local checks and
compatibility reports are designed to run without real secrets or public API
endpoints.

Postly is an early technical preview. The script bridge is opt-in and is not a
security boundary for hostile JavaScript; do not use it to run untrusted code.

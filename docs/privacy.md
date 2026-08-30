# Privacy and security boundaries

Postly's core local workflow does not require an account, a cloud workspace or telemetry. Collections, variables and responses remain in the local process and filesystem unless the user explicitly sends a request to its target server.

Important boundaries:

- Local-first does not mean sandboxed. A future scripting runtime must document filesystem, network, process and environment-variable permissions.
- Authorization headers, cookies and body contents must not appear in normal logs, error reports or benchmark output.
- Imported environments may contain secrets. Store them in ignored local files until OS keychain integration is available.
- --insecure disables TLS certificate verification for the current CLI request and should only be used intentionally.
- Request history, crash recovery and clipboard integrations are future surfaces that require their own threat model.

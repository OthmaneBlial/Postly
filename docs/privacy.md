# Privacy and security boundaries

Postly's core local workflow does not require an account, a cloud workspace or telemetry. Collections, variables and responses remain in the local process and filesystem unless the user explicitly sends a request to its target server.

Important boundaries:

- Local-first does not mean sandboxed. A future scripting runtime must document filesystem, network, process and environment-variable permissions.
- Authorization headers, cookies and body contents must not appear in normal logs, error reports or benchmark output.
- The HTTP engine keeps session cookies in memory for its lifetime so a request sequence can behave like a browser session; cookie values are not persisted by the jar. Response cookie attributes are shown only in the local response view, while explicitly saved request cookies remain the user's project data.
- postly env set --secret KEY=VALUE stores the value in the operating-system credential store (macOS Keychain, Windows Credential Manager or Linux keyutils where supported) and writes only an opaque workspace-scoped reference to the environment file. The assignment itself can still be visible in shell history/process arguments, so prefer a short-lived shell or a future stdin/file workflow for high-risk credentials.
- Imported or legacy environment files can still contain plaintext secret values. They remain supported for migration, but should be moved to keychain-backed entries with postly env set --secret; the secret marker alone does not encrypt an existing file.
- If the platform credential store is unavailable, Postly fails the --secret operation instead of silently writing a new plaintext secret. Use an explicitly ignored local environment file only as a deliberate fallback.
- --insecure disables TLS certificate verification for the current CLI request and should only be used intentionally.
- Saved-request history is now available as a local, bounded metadata-only JSONL file under `.postly/history.jsonl`; it is ignored by Git and excludes query values, headers, cookies, bodies, auth and response content. `postly history --clear` truncates it explicitly. Crash recovery and clipboard integrations remain future surfaces that require their own threat model.

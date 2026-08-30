# Privacy and security boundaries

Postly's core local workflow does not require an account, a cloud workspace or telemetry. Collections, variables and responses remain in the local process and filesystem unless the user explicitly sends a request to its target server.

Important boundaries:

- Local-first does not mean sandboxed. A future scripting runtime must document filesystem, network, process and environment-variable permissions.
- Authorization headers, cookies and body contents must not appear in normal logs, error reports or benchmark output.
- The HTTP engine keeps session cookies in memory for its lifetime so a request sequence can behave like a browser session; cookie values are not persisted by the jar. Response cookie attributes are shown only in the local response view, while explicitly saved request cookies remain the user's project data.
- postly env set --secret KEY=VALUE stores the value in the operating-system credential store (macOS Keychain, Windows Credential Manager or Linux keyutils where supported) and writes only an opaque workspace-scoped reference to the environment file. For high-risk credentials, `--secret-stdin KEY` accepts one value per key without putting the value in shell history or process arguments.
- Imported or legacy environment files can still contain plaintext secret values. They remain supported for migration; use `postly env migrate --key KEY` for an explicit value or `postly env migrate --all` for imported variables marked as secret. The secret marker alone does not encrypt an existing file.
- If the platform credential store is unavailable, Postly fails the --secret operation instead of silently writing a new plaintext secret. Use an explicitly ignored local environment file only as a deliberate fallback.
- --insecure disables TLS certificate verification for the current CLI request and should only be used intentionally.
- Saved-request history is now available as a local, bounded metadata-only JSONL file under `.postly/history.jsonl`; it is ignored by Git and excludes query values, headers, cookies, bodies, auth and response content. `postly history --clear` truncates it explicitly.
- The native GUI writes a bounded `.postly/recovery.json` snapshot while an unsaved request is being edited. It can contain the draft body, headers, authentication and scripts because recovery must restore the editor state; it is never included in history, logs or network requests. The file is written through a temporary-file replacement and is `0600` on Unix-like systems. On the next open it is restored as a new unsaved draft, and Save or “Discard recovery” removes it. Treat `.postly/` as private local data on a shared machine.

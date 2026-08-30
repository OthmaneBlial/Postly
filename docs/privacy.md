# Privacy and security boundaries

Postly's core local workflow does not require an account, a cloud workspace or telemetry. Collections, variables and responses remain in the local process and filesystem unless the user explicitly sends a request to its target server.

Important boundaries:

- Local-first does not mean sandboxed. A future scripting runtime must document filesystem, network, process and environment-variable permissions.
- Authorization headers, cookies and body contents must not appear in normal logs, error reports or benchmark output.
- The HTTP engine keeps session cookies in memory for its lifetime so a request sequence can behave like a browser session; cookie values are not persisted by the jar. Response cookie attributes are shown only in the local response view, while explicitly saved request cookies remain the user's project data.
- Imported environments may contain secrets. Store them in ignored local files until OS keychain integration is available.
- --insecure disables TLS certificate verification for the current CLI request and should only be used intentionally.
- Saved-request history is now available as a local, bounded metadata-only JSONL file under `.postly/history.jsonl`; it is ignored by Git and excludes query values, headers, cookies, bodies, auth and response content. `postly history --clear` truncates it explicitly. Crash recovery and clipboard integrations remain future surfaces that require their own threat model.

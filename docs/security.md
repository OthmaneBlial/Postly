# Security model and boundaries

Postly is designed for local API work that may contain production URLs,
credentials, cookies and customer data. The security model is intentionally
explicit: local-first is a data-flow choice, not a claim that every component
is a sandbox.

## Data boundaries

- Core use does not require an account, hosted workspace or telemetry.
- Requests, collections and environments stay in the local process/filesystem
  unless the user sends a request to its configured target.
- Normal CLI output, history, benchmarks and the developer console avoid
  printing request bodies, header values, cookies or authentication material.
- History is bounded metadata and is stored under ignored `.postly/` state.
- GUI recovery is private local state and can contain unsaved editor content;
  it is bounded, atomically replaced and permission-restricted on Unix systems.
- `postly env set --secret` stores the value in the platform credential store
  and writes only an opaque workspace-scoped reference to the project file.

Moving or copying a workspace requires setting its credential-backed secrets
again. A reference from another workspace is rejected rather than resolved
silently.

## Transport safety

TLS verification is enabled by default. Custom CA bundles, client identities,
PKCS#12 identities and proxies are explicit transport settings. `--insecure`
disables certificate verification only for the current request and should be
treated as a deliberate debugging exception. Proxy credentials and certificate
passphrases should be supplied through the supported local configuration path,
not committed to a request file.

## Script boundary

Postman scripts are opt-in and run through a short-lived Node bridge. The
bridge has bounded source/input/output sizes, execution time, logs and test
results. Before launch, Postly rejects explicit filesystem/process/module
capabilities; on Node versions that expose the permission model, network access
is retained only for bounded `pm.sendRequest` callbacks while filesystem,
child-process, worker and addon access remains disabled by default.

This is defense in depth, not a hostile-code sandbox. The Node VM is not a
security boundary for malicious JavaScript. Run only scripts you intentionally
trust, keep the feature disabled when it is not needed, and do not put secrets
in `console.log` calls. The script compatibility matrix records the supported
surface without claiming complete Postman parity.

## Import and shell boundaries

Postman, OpenAPI, dotenv and cURL imports parse files/data into the native model.
The cURL parser does not execute a shell command. Unsupported or ambiguous
fields are reported for review rather than silently presented as equivalent.
Imported plaintext environments remain plaintext until the user explicitly
migrates selected values to the credential store.

## Local review checklist

- Run `postly validate ./project` before sending after a merge or import.
- Keep real environment files, private certificates and `.postly/` ignored.
- Prefer `--secret-stdin` for values that must not enter shell history.
- Review generated cURL/code snippets before running them elsewhere.
- Treat saved response examples and GUI recovery files as sensitive local data.
- Never enable `--insecure` or scripts as a blanket default.
- Redact URLs, headers, cookies, bodies and tokens from bug reports.

For the current implementation evidence and known limitations, see
[privacy](privacy.md), [scripting](scripting.md), [progress](progress.md) and
[debugging](debugging.md).

# Debugging Postly workflows

Postly keeps diagnostics local and makes the failure boundary visible. Start
with the smallest command that reproduces the problem, then add structured
output or the native GUI only when it helps inspect state.

## First checks

Validate a workspace without sending a request:

```bash
postly validate ./my-api
postly validate ./my-api --output-json
```

The validator scans canonical collection, request and environment files. It
does not require or inspect ignored runtime state. For a repeatable local
baseline, run the repository checks:

```bash
cargo xtask check
cargo xtask compat
cargo xtask fuzz
```

## Request failures

Use JSON output when a failure needs to be captured by another local tool:

```bash
postly send ./my-api/collections/api/requests/health.postly.toml \
  --environment Local --output-json
```

Typical boundaries are reported as actionable errors:

| Symptom | Check |
| --- | --- |
| `{{name}}` remains unresolved | Confirm the selected environment, enabled flag and variable scope precedence. Run `postly validate` first. |
| Connection refused or DNS failure | Confirm the URL and local service, then reproduce against a deterministic local mock with `postly mock`. |
| TLS or certificate failure | Check the CA/client-identity path and passphrase; use `--insecure` only as a temporary, explicit diagnostic. |
| Proxy failure | Inspect the proxy URL, bypass list and `HTTP_PROXY`/`HTTPS_PROXY` settings. Retry the target through a local forwarding test. |
| Body rejected before sending | Check the body mode, content type and file path. Buffered HTTP responses are bounded; SSE remains progressive. |
| Request assertion failure | Read the named native assertion in text/JSON output. The received response remains available for inspection. |

## Scripts and tests

Scripts are disabled by default. Enable them deliberately:

```bash
postly send ./my-api/collections/api/requests/health.postly.toml --scripts
postly run ./my-api --scripts --reporter json
```

The runner reports each test name, pass/fail state, duration and bounded error
detail. A failed script assertion is different from a transport failure. A
script that references an unsupported host capability is rejected before Node
starts; a script that times out or is cancelled terminates its child process.
See [scripting](scripting.md) for the exact compatibility boundary.

## Streaming and protocols

- SSE: use JSON-lines output and inspect event type, ID, retry and reconnect
  behavior. `Last-Event-ID` is part of the reconnect path.
- WebSocket: use the interactive CLI or GUI message history to separate
  handshake, send, receive and close events.
- gRPC: confirm the proto/include paths or reflection host and method shape;
  reflection and dynamic calls produce more useful errors than a generic HTTP
  status.
- GraphQL: inspect the structured `data`/`errors` envelope and validate that
  variables are a JSON object.

For deterministic reproduction, prefer the checked-in local protocol tests and
the HTTP mock server over public endpoints. Do not include credentials or real
customer payloads in a fixture.

## GUI diagnostics

The native workspace exposes response metadata, bounded search, the local
developer console, script results and cancellation state. Clear the console
before a fresh reproduction and save a response as an example only after
checking it does not contain sensitive data. If an unsaved editor disappears,
reopen the workspace: the private recovery snapshot restores dirty tabs as new
drafts; it is not canonical project data.

When reporting a local issue, include the Postly version, operating system,
reproduction command, sanitized error text and whether `cargo xtask check`
passes. Exclude URLs with credentials, tokens, cookies, bodies and private
certificate material.

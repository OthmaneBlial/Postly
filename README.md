<div align="center">

# Postly

### The Postman alternative without an account.

Fast, native and local-first API development powered by Rust.

<p>
  <a href="https://github.com/OthmaneBlial/Postly"><img src="https://img.shields.io/badge/status-active%20development-5b8def?style=flat-square" alt="Active development"></a>
  <a href="https://github.com/OthmaneBlial/Postly/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-8bc34a?style=flat-square" alt="MIT License"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/built%20with-Rust-dea584?style=flat-square&logo=rust&logoColor=white" alt="Built with Rust"></a>
  <a href="https://github.com/OthmaneBlial/Postly/stargazers"><img src="https://img.shields.io/github/stars/OthmaneBlial/Postly?style=flat-square&color=f5b942" alt="GitHub stars"></a>
</p>

<p>
  <a href="#quick-start">Try it in 60 seconds</a> ·
  <a href="docs/migration-from-postman.md">Migrate from Postman</a> ·
  <a href="docs/progress.md">See what is real</a>
</p>

</div>

Postly is an open-source <strong>API client</strong>, <strong>REST client</strong> and API testing
workspace for developers who want their requests, collections and environments
to stay on their machine. It is designed as a credible <strong>Postman alternative</strong>
for local work: no signup, no mandatory cloud workspace, no Electron app and
no telemetry dependency for the core workflow.

> <strong>The promise:</strong> open a project, send a request, inspect the response, save
> the work and keep moving — even when you are offline.

## Why Postly?

Postman made API development approachable. Postly keeps the familiar mental
model — requests, collections, environments, scripts and runners — while
changing the default relationship with your data.

| Postly principle | What it means in practice |
| --- | --- |
| <strong>No account</strong> | Core local requests work without signup or login. |
| <strong>Local-first</strong> | Request data, environments and history stay in the local workspace. |
| <strong>Git-friendly</strong> | Collections are readable project files, with one request per file. |
| <strong>Rust-powered</strong> | The core, HTTP engine, protocols, storage and CLI share native Rust code. |
| <strong>Migration-minded</strong> | Postman Collection v2.1 and environment import include review diagnostics. |
| <strong>Automation-ready</strong> | The same core powers the desktop workspace and headless CLI. |

Postly is not claiming perfect Postman parity today. Compatibility is measured
by executable fixtures and documented honestly as the product grows.

## What works today

This is a working vertical slice, not a static UI mockup.

- <strong>HTTP:</strong> GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS and custom methods;
  query parameters, duplicate headers, cookies, raw/JSON/form/multipart/file
  bodies, redirects, compression, timeouts, cancellation and an ignored local
  cookie jar for saved workspaces.
- <strong>Authentication:</strong> Bearer, Basic, API-key and OAuth 2.0 Client Credentials auth with variable resolution.
- <strong>TLS and routing:</strong> explicit insecure-TLS opt-in, custom PEM CA bundles,
  combined PEM client identities, HTTP(S) proxy routing and actionable file
  diagnostics. HTTPS and mutual TLS are covered by local integration tests.
- <strong>Response inspection:</strong> status, headers, cookies, protocol, duration,
  Pretty/Raw views, virtualized line rendering, search, wrapping, copy and
  local response snapshots.
- <strong>Collections:</strong> local TOML project model, nested request folders,
  deterministic discovery, stable request identity, duplicate/delete/rename
  flows, global metadata search and bounded metadata-only history.
- <strong>Migration:</strong> Postman Collection v2.1 import/export, environment import/
  export, OpenAPI 3.0/3.1 JSON/YAML file or URL import and common cURL import.
- <strong>Testing and automation:</strong> explicit response assertions, opt-in Node.js
  script bridge, basic <code>pm.*</code>, collection runner, iteration data, pretty/JSON/
  JUnit reporters and deliberate exit behavior.
- <strong>Modern APIs:</strong> structured GraphQL requests, progressive SSE subscriptions,
  interactive WebSockets and dynamic <code>.proto</code> gRPC calls for unary and all
  three streaming modes.
- <strong>Native desktop workspace:</strong> a Rust/egui application using the same core,
  with local history, request editing, a Scripts tab, cancellation and persisted
  local Transport settings for HTTP/SSE.

For the exact boundary of each feature, read the
<a href="docs/compatibility.md">compatibility matrix</a> and the
<a href="docs/progress.md">living progress log</a>.

## Quick start

### 1. Create a local API workspace

```bash
git clone https://github.com/OthmaneBlial/Postly.git
cd Postly

cargo run -- init ./my-api --name "My API"
cargo run -- new request \
  --workspace ./my-api \
  --collection "My API" \
  --name health \
  https://example.com/health \
  --query "probe=1"
```

### 2. Send a real request

```bash
cargo run -- request https://httpbin.org/get \
  --query "source=postly" \
  --header "Accept: application/json"
```

Use <code>--output-json</code> when another local tool should consume the response:

```bash
cargo run -- request https://httpbin.org/get --output-json
```

### 3. Open the native workspace

```bash
cargo run -p postly-app -- ./my-api
```

The desktop app and CLI read the same local project. There is no account
creation step and no hosted workspace required.

## Migrate from Postman

The shortest path from Postman is deliberately explicit:

```bash
# Export a collection and environment from Postman first.
cargo run -- import collection ./collection.json --output ./my-api
cargo run -- import environment ./environment.json --output ./my-api

# Inspect the imported workspace.
cargo run -- list ./my-api
cargo run -- run ./my-api --environment Local --reporter pretty
```

Postly preserves supported collection metadata, folders, URLs, parameters,
headers, bodies, auth, variables, examples and script source. Unsupported or
ambiguous fields are reported for review instead of being silently presented as
fully compatible.

Read the complete <a href="docs/migration-from-postman.md">Postman migration guide</a>,
the <a href="docs/compatibility.md">compatibility matrix</a> and the checked-in import
fixtures in <code>compat/postman-import/</code>.

## A Git-native API project

Your canonical API work can live beside the code that consumes it:

```text
my-api/
├── postly.toml
├── collections/
│   └── my-api/
│       ├── postly.collection.toml
│       └── requests/
│           ├── health.postly.toml
│           └── users/
│               └── list-users.postly.toml
└── environments/
    └── local.postly-env.toml
```

That makes the workflow simple:

```bash
git clone <your-api-project>
postly list .
git diff
git status
```

Keep secrets out of Git with local environment files and templates. Postly's
metadata-only history and response snapshots live under ignored <code>.postly/</code>
artifacts; canonical request files remain ordinary project data.

## CLI for developers and automation

The CLI is useful without the desktop application:

```bash
# Immediate REST request
postly request https://api.example.com/users --bearer "$API_TOKEN"

# Saved request with an environment
postly send ./my-api/collections/my-api/requests/health.postly.toml \
  --environment Local

# Collection runner with machine-readable output
postly run ./my-api --environment Local --reporter json
postly run ./my-api --environment Local --reporter junit > postly-results.xml
postly run ./my-api --folder auth --environment Local --reporter pretty

# Search every collection without exposing request secrets
postly search payments --workspace ./my-api --output-json

# Protocol workflows
postly graphql https://api.example.com/graphql --query 'query { health }'
postly sse https://api.example.com/events --reconnect 3
postly websocket wss://api.example.com/socket --send '{"type":"ping"}'
postly grpc describe ./api.proto
```

During development, <code>cargo run --</code> is equivalent to <code>postly</code>. A published
binary/package is not claimed until the release and packaging gates are
validated.

## Environments, proxies and certificates

```bash
# Environment variables are resolved with Postman-style {{variables}} syntax.
cargo run -- env set \
  --workspace ./my-api \
  --name Local \
  --set baseUrl=https://api.example.com

# Route CLI HTTP workflows through a trusted explicit proxy.
cargo run -- request 'https://api.example.com/health' \
  --proxy http://127.0.0.1:8080

# Trust a private CA without disabling TLS verification.
cargo run -- request 'https://internal.example.com/health' \
  --ca-cert ./certs/company-ca.pem \
  --client-identity ./certs/client-identity.pem
```

The GUI exposes the same HTTP/SSE controls in its <strong>Transport</strong> tab and stores
only connection flags and file paths in ignored local settings. It never needs
the private-key contents in the project. See <a href="docs/certificates.md">certificates</a>
and <a href="docs/proxy.md">proxy behavior</a> for security boundaries and current limits.
The command palette is available with <code>⌘K</code> (or <code>Ctrl+K</code>), with quick
actions for creating, saving, sending and managing the current request. See
<a href="docs/shortcuts.md">keyboard shortcuts</a> for the complete list.

## Protocol coverage

Postly is intentionally growing from a strong HTTP foundation:

| Protocol / format | Current capability |
| --- | --- |
| REST / HTTP | Native async requests, bodies, auth, cookies, response views, proxy and TLS slices |
| GraphQL | Structured query, variables, operation name, error-aware response parsing, schema introspection and GUI explorer |
| Server-Sent Events | Chunk-safe progressive events, metadata, cancellation and bounded <code>Last-Event-ID</code> reconnects |
| WebSocket | <code>ws://</code>/<code>wss://</code>, headers/auth, text and binary frames, ping/pong, console and bounded history |
| gRPC | Dynamic <code>.proto</code> discovery plus unary, server-streaming, client-streaming and bidi CLI calls with HTTPS custom-CA/client-identity support |
| OpenAPI | 3.0/3.1 JSON/YAML request generation with guarded local reference resolution |

gRPC reflection, richer protocol-specific GUI surfaces and deeper TLS/proxy parity
remain active roadmap work. The matrix is the source of truth.

## Privacy by default

Postly is built for developers who do not want an API client to become another
cloud dependency.

- No account wall for the core local workflow.
- No mandatory cloud workspace or synchronization service.
- No request payload upload to a Postly service.
- No telemetry dependency in the current milestone.
- Local history is bounded and metadata-only; it excludes query values,
  headers, cookies, bodies, auth and response content.
- Imported scripts are not magically sandboxed. Script permissions and runtime
  boundaries are documented as they are implemented.

Local-first is not the same thing as a security sandbox. Treat collections as
sensitive code and data: review files, proxy settings, scripts, clipboard use
and filesystem permissions before using production credentials.

## Built in the open, measured honestly

Postly deliberately does not publish invented numbers:

- no “20× faster” claim without reproducible benchmarks;
- no “100% Postman compatible” badge without semantic fixture coverage;
- no fake testimonials, user counts or screenshots;
- no GitHub Actions dependency — important checks run locally.

Run the local validation pipeline:

```bash
cargo xtask fmt
cargo xtask lint
cargo xtask test
cargo xtask check
```

On a constrained disk, these settings reduce generated debug artifacts:

```bash
CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_PROFILE_TEST_DEBUG=0 \
CARGO_INCREMENTAL=0 \
cargo xtask check
```

The test suite includes importer regressions, filesystem round trips, variable
diagnostics, local HTTP/proxy/TLS/mTLS servers, GraphQL, SSE, WebSocket, gRPC,
cancellation, runner assertions and GUI worker state. See
<a href="docs/development.md">development</a> for the contributor workflow.

## Product direction

The long-term goal is straightforward: become the API client a developer can
genuinely consider replacing Postman with, especially when local ownership,
Git workflows, privacy and automation matter.

Highest-value roadmap areas include:

- broader Postman script and <code>pm.*</code> compatibility with a hardened runtime;
- OS keychain integration and complete secret-handling workflows;
- richer Postman fixtures and executable compatibility scoring;
- gRPC reflection and a first-class gRPC GUI;
- tabs, accessibility and crash recovery;
- deterministic protocol test servers, fuzzing, benchmarks and packaging.

See <a href="docs/progress.md">docs/progress.md</a> for current evidence instead of a
marketing-only roadmap.

## Documentation

- <a href="docs/architecture.md">Architecture</a>
- <a href="docs/development.md">Development and local validation</a>
- <a href="docs/benchmarks.md">Benchmarks</a>
- <a href="docs/cli.md">CLI reference</a>
- <a href="docs/collections.md">Collections and environments</a>
- <a href="docs/authentication.md">Authentication</a>
- <a href="docs/scripting.md">Scripting and pm.*</a>
- <a href="docs/history.md">History</a>
- <a href="docs/shortcuts.md">Keyboard shortcuts</a>
- <a href="docs/cookies.md">Cookies</a>
- <a href="docs/certificates.md">Certificates</a>
- <a href="docs/proxy.md">Proxy</a>
- <a href="docs/graphql.md">GraphQL</a>
- <a href="docs/sse.md">SSE</a>
- <a href="docs/websocket.md">WebSockets</a>
- <a href="docs/grpc.md">gRPC</a>
- <a href="docs/openapi.md">OpenAPI</a>
- <a href="docs/migration-from-postman.md">Postman migration</a>
- <a href="docs/compatibility.md">Compatibility status</a>
- <a href="docs/progress.md">Project progress</a>

## Contributing

Postly is early, ambitious and deliberately evidence-driven. Good
contributions include:

1. a minimal regression fixture for a real migration edge case;
2. a deterministic local protocol test;
3. a UX improvement backed by a functioning core path;
4. documentation that makes a limitation clearer;
5. a benchmark with hardware, revision and methodology recorded.

Please keep <code>base/</code> local: it is an ignored research corpus and must never be
committed. Before opening a change, run the relevant local checks and state
which product boundary the change improves.

## License

Postly is released under the <a href="LICENSE">MIT License</a>.

<div align="center">

### Build APIs with less friction — and keep your work yours.

<a href="https://github.com/OthmaneBlial/Postly">Explore the repository</a> ·
<a href="https://github.com/OthmaneBlial/Postly/issues">Share a migration edge case</a> ·
<a href="https://github.com/OthmaneBlial/Postly/stargazers">Star Postly</a>

</div>

# Postly

**The Postman alternative without an account.**

Postly is an open-source, Rust-first API development workspace designed around local files, Git workflows and privacy. The core request engine and CLI run without signup, a cloud workspace or mandatory telemetry.

> This repository is under active construction. The checked-in milestone is a working local CLI/core slice, not a claim of complete Postman parity.

## What works today

- Rust workspace with a reusable postly-core library.
- Real HTTP requests using GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS and custom methods.
- Query parameters, duplicate headers, cookies, raw/JSON/form/multipart/file bodies.
- Bearer, Basic and API-key authentication.
- Timeouts, redirects, compressed responses and explicit insecure-TLS opt-in.
- Local, human-readable TOML projects with one request per file.
- Environment variables and Postman-style {{variable}} interpolation with precedence diagnostics.
- Postman Collection v2.1 and environment import with a migration report.
- OpenAPI 3 JSON/YAML import with generated requests and explicit warnings.
- Opt-in Postman script execution through a local Node.js bridge with basic `pm.*` tests.
- Headless commands for immediate requests, saved requests and sequential collection runs.
- Searchable, filterable and bounded metadata-only local history for saved request executions (`postly history`).
- A native Rust desktop request workspace (`postly-gui`) using the same core.
- No GitHub Actions; validation is designed to run locally with cargo xtask check.

## Quick start

~~~bash
cargo run -- init ./my-api --name "My API"
cargo run -- new request --workspace ./my-api --collection "My API" --name health https://example.com/health --query "probe=1"
cargo run -- env set --workspace ./my-api --name Local --set baseUrl=https://example.com
cargo run -- request https://httpbin.org/get
cargo run -- list ./my-api
cargo run -- history ./my-api --limit 10
cargo run -- history ./my-api --search users --method GET
cargo run -- history ./my-api --errors-only
cargo run -p postly-app -- ./my-api
~~~

Import an existing Postman export:

~~~bash
cargo run -- import collection ./collection.json --output ./my-api
cargo run -- import environment ./environment.json --output ./my-api
cargo run -- import openapi ./openapi.yaml --output ./my-api
cargo run -- import curl "curl -H 'Accept: application/json' https://example.com/health" --output ./my-api
cargo run -- list ./my-api
~~~

Send a saved request:

~~~bash
cargo run -- send ./my-api/collections/my-api/requests/health.postly.toml
cargo run -- send ./my-api/collections/my-api/requests/health.postly.toml --environment Local
cargo run -- send ./my-api/collections/my-api/requests/health.postly.toml --scripts --environment Local
~~~

The CLI can emit machine-readable response data with --output-json. `postly run` executes saved requests in deterministic path order and returns a failing exit code when a request returns a 4xx/5xx, cannot be sent, or an enabled script assertion fails. Pass `--scripts` to opt into the local Node.js bridge.

## Local validation

~~~bash
cargo xtask fmt
cargo xtask lint
cargo xtask test
cargo xtask check
~~~

Use CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 on constrained disks.

## Product direction

The long-term target is a credible local-first Postman replacement: a professional request workspace, Git-native collections, environments, scripting and tests, runner/CLI parity, Postman migration, OpenAPI, GraphQL, WebSockets, SSE and gRPC. Features are only documented as supported once executable behavior and tests exist.

See docs/progress.md, docs/architecture.md, docs/migration-from-postman.md, docs/openapi.md, docs/scripting.md, docs/history.md and docs/compatibility.md.

## Privacy

Postly has no account wall and no cloud dependency in this milestone. Requests, collections and imported environments are stored locally. Secrets are not sent to a Postly service, and the CLI does not log request bodies or authorization values. Saved-request history is local, ignored by Git, bounded, and stores only request metadata plus status and duration; it excludes query values, headers, cookies, bodies, auth and response content. Use `postly history --clear` to truncate it. Local execution is not a sandbox; imported scripts and future extensions will have an explicitly documented permission model.

## License

Postly is released under the MIT License. Reference projects under base/ are ignored and are used only for research; their licenses remain their own.

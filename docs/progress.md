# Postly progress

Updated: 2026-08-30

## Current milestone

Foundation plus a first native desktop/CLI/core vertical slice.

Implemented:

- Rust workspace and local validation entry point.
- Local project/collection/request/environment TOML model.
- Deterministic recursive request discovery.
- Variable scopes, precedence and undefined-variable diagnostics.
- Native async HTTP execution with common body/auth/header/query behavior.
- Response metadata and JSON pretty formatting.
- Postman Collection v2.1 and environment import reports.
- collection/folder/request script source preservation and a truthful pm.* compatibility matrix.
- collection and folder authentication inheritance is materialized into imported request files.
- init, request, send, import, list and sequential run CLI commands.
- new request creates and persists a saved request without editing files by hand.
- env set creates local environments and saved requests resolve enabled environment variables.
- common cURL commands can be parsed and imported without shell execution.
- saved-request executions can be recorded, searched, filtered, cleared and retained as bounded ignored metadata-only local history.
- native `postly-gui` request workspace with async send, editor tabs and response views.
- OpenAPI 3 JSON/YAML import for common operations, parameters, JSON bodies and auth placeholders.
- Opt-in Node.js script bridge with basic `pm.*`, `pm.test` and runner assertion results.
- Stateful in-memory cookie jar, response `Set-Cookie` metadata and explicit request-cookie editing.
- reusable runner results with pass/fail status, deterministic order, fail-fast and cooperative cancellation.
- runner iteration data from JSON objects/arrays plus pretty, JSON and JUnit reporters.
- Ignored shallow research corpus for Bruno, Yaak and Posting.

Not yet implemented:

- Desktop GUI polish, large-response virtualization and richer response editor.
- Embedded/hardened script runtime and broader pm.* compatibility.
- Postman tests/assertions beyond the current basic runner slice.
- OS keychain storage, persistent/manual cookie management and crash recovery.
- OpenAPI reference resolution, GraphQL, WebSockets, SSE and gRPC.
- Local deterministic protocol test servers, fuzzing, benchmarks and packaging.

## Verification

cargo xtask check is the required validation command for this milestone. The CLI was exercised against deterministic local HTTP servers: a real JSON response was received and formatted, then a saved request created with new request was sent and run with structured JSON output. Iteration data was exercised twice through JSON and JUnit reporters, an environment created with env set resolved two variables into a saved request, a cURL command was imported and sent with HTTP 201, OpenAPI YAML generated two requests, an imported Postman test executed through the opt-in Node bridge, a two-request cookie exchange verified jar reuse plus response metadata, and history filters, clear and the 1,000-entry retention bound were tested. The native app compiles with the same core and has state tests for JSON/auth editing plus an async local-response test; native window interaction still needs manual desktop QA. The machine initially reached zero free space during a debug build; the generated Postly target directory was cleaned and subsequent validation used low-debug-info artifacts.

## Next highest-value work

1. Add importer fixtures for more Postman body/auth/URL variants.
2. Add script isolation research and an ADR before broadening runtime compatibility.
3. Add a safe, explicit test/assertion model beyond the current basic bridge.
4. Add large-response handling, search and desktop accessibility QA.

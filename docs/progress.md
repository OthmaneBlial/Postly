# Postly progress

Updated: 2026-08-30

## Current milestone

Foundation plus a first real CLI/core vertical slice.

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
- reusable runner results with pass/fail status, deterministic order, fail-fast and cooperative cancellation.
- runner iteration data from JSON objects/arrays plus pretty, JSON and JUnit reporters.
- Ignored shallow research corpus for Bruno, Yaak and Posting.

Not yet implemented:

- Desktop GUI and polished response editor.
- Script runtime and pm.* compatibility.
- Postman tests/assertions and richer collection-runner semantics.
- OS keychain storage, history and crash recovery.
- OpenAPI, GraphQL, WebSockets, SSE and gRPC.
- Local deterministic protocol test servers, fuzzing, benchmarks and packaging.

## Verification

cargo xtask check passed during this milestone with format, strict Clippy and 12 tests. The CLI was exercised against a deterministic local HTTP server: a real JSON response was received and formatted, then a saved request created with new request was sent and run with structured JSON output. Iteration data was exercised twice through JSON and JUnit reporters, an environment created with env set resolved two variables into a saved request, and a cURL command was imported and sent with HTTP 201. The machine initially reached zero free space during a debug build; the generated Postly target directory was cleaned and subsequent validation used low-debug-info artifacts.

## Next highest-value work

1. Add iteration data, assertions and JSON/JUnit runner reporters.
2. Add importer fixtures for more Postman body/auth/URL variants.
3. Prototype a Rust-first desktop UI against the same core.
4. Add script runtime research and an ADR before claiming any script compatibility.

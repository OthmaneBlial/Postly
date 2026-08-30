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
- init, request, send, import, list and sequential run CLI commands.
- Ignored shallow research corpus for Bruno, Yaak and Posting.

Not yet implemented:

- Desktop GUI and polished response editor.
- Script runtime and pm.* compatibility.
- Postman tests/assertions and collection-runner iteration data.
- OS keychain storage, history, cancellation and crash recovery.
- OpenAPI, GraphQL, WebSockets, SSE and gRPC.
- Local deterministic protocol test servers, fuzzing, benchmarks and packaging.

## Verification

cargo xtask check passed during this milestone with format, strict Clippy and 7 tests. The CLI was also exercised against a deterministic local HTTP server: a real JSON response was received and formatted, then init/import/list were run against a temporary workspace. The machine initially reached zero free space during a debug build; the generated Postly target directory was cleaned and subsequent validation used low-debug-info artifacts.

## Next highest-value work

1. Add importer fixtures and integration tests for HTTP requests against a deterministic local server.
2. Add an explicit runner/reporting core and request execution cancellation.
3. Prototype a Rust-first desktop UI against the same core.
4. Add script runtime research and an ADR before claiming any script compatibility.
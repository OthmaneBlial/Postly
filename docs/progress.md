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
- Custom PEM CA bundles and combined PEM client identities in the shared HTTP engine and CLI workflows, with actionable file/format diagnostics and local HTTPS/mTLS integration tests.
- Explicit HTTP(S) proxy routing in the shared HTTP engine and CLI request/stream/runner workflows, with invalid-URL diagnostics and a local proxy integration test.
- Response metadata and JSON pretty formatting.
- Postman Collection v2.1 and environment import reports.
- Postman Collection v2.1 and environment export with a tested native round-trip.
- Postman importer regression fixture for structured URLs, disabled/non-text values, form bodies, file parts, API-key query auth and GraphQL review warnings.
- Postman import now preserves scalar header values, marks unsupported auth types
  as manual-review requests (including inherited auth), and exercises JSON/HTML
  raw bodies in the expanded variant fixture.
- collection/folder/request script source preservation and a truthful pm.* compatibility matrix.
- collection and folder authentication inheritance is materialized into imported request files.
- init, request, send, import, list and sequential run CLI commands.
- new request creates and persists a saved request without editing files by hand.
- env set creates local environments and saved requests resolve enabled environment variables.
- common cURL commands can be parsed and imported without shell execution.
- saved-request executions can be recorded, searched, filtered, cleared and retained as bounded ignored metadata-only local history.
- native `postly-gui` request workspace with async send, editor tabs and response views.
- native saved-request duplication and guarded deletion with storage/UI regression tests.
- saved request rename/folder changes relocate the canonical file while preserving request identity.
- response Pretty/Raw views now provide case-insensitive local search with occurrence counts and line snippets.
- response Pretty/Raw views now use virtualized line rows with optional wrapping, clipboard copy and workspace-local response snapshots.
- OpenAPI 3.0/3.1 JSON/YAML import for common operations, local `$ref` components, parameters, JSON bodies and auth placeholders.
- Structured GraphQL core/CLI/GUI request model with variables, operation names, partial-data/error parsing, validated GUI editing and local HTTP integration coverage.
- SSE parser plus progressive CLI/native GUI subscriptions with chunk-safe event decoding, event metadata, bounded GUI history, JSON-lines output and local streaming coverage.
- WebSocket CLI and native GUI client for `ws://` and `wss://` with headers/auth, interactive text sends, text/binary/pong output, ping replies, bounded reconnects/history and local integration coverage.
- native GUI HTTP, SSE and WebSocket workers support explicit cancellation, with cancellation-aware body/stream reads and local worker tests.
- native GUI Transport tab with persisted local timeout, HTTP(S) proxy, custom CA, client identity and explicit insecure-TLS settings for HTTP/SSE workflows.
- Dynamic gRPC `.proto` compilation with service/method discovery plus unary, server-streaming, client-streaming and bidirectional CLI calls using protobuf JSON, metadata and HTTPS webpki roots.
- Persistable response assertions for status, headers, body text and JSON Pointer values, evaluated by the runner without Node.js.
- Opt-in Node.js script bridge with basic `pm.*`, `pm.test` and runner assertion results.
- Script compatibility boundary now carries explicit variable unsets, globals,
  read-only iteration data, request header mutations and bounded source size;
  the child environment removes Node module injection variables, and the
  worker bounds process duration plus captured logs and test results.
- Common response assertions now cover headers, cookies, status health, numeric/type/regex and negated expectations.
- Stateful in-memory cookie jar, response `Set-Cookie` metadata and explicit request-cookie editing.
- reusable runner results with pass/fail status, deterministic order, fail-fast and cooperative cancellation.
- runner iteration data from JSON objects/arrays plus pretty, JSON and JUnit reporters.
- Ignored shallow research corpus for Bruno, Yaak and Posting.

Not yet implemented:

- Desktop GUI polish, richer response preview/syntax features and manual responsive/accessibility QA.
- Embedded/hardened script runtime and broader pm.* compatibility beyond the
  tested scoped-variable/request-header/response subset.
- Broader Postman tests/assertions and GUI assertion editing beyond the current explicit runner slice.
- OS keychain storage, persistent/manual cookie management and crash recovery.
- encrypted/PKCS#12 identities, passphrase handling, per-domain certificate association and certificate settings for WebSocket/gRPC workflows.
- OpenAPI external/cyclic references, GraphQL schema explorer, gRPC reflection and gRPC GUI.
- Local deterministic protocol test servers, fuzzing, benchmarks and packaging.

## Verification

cargo xtask check is the required validation command for this milestone. The CLI was exercised against deterministic local HTTP servers: a real JSON response was received and formatted, then a saved request created with new request was sent and run with structured JSON output. Iteration data was exercised twice through JSON and JUnit reporters, an environment created with env set resolved two variables into a saved request, a cURL command was imported and sent with HTTP 201, OpenAPI YAML generated two requests, an imported Postman test executed through the opt-in Node bridge, a two-request cookie exchange verified jar reuse plus response metadata, a GraphQL query with variables was sent through the structured CLI command, an SSE stream was decoded progressively from a local endpoint and reconnected with `Last-Event-ID`, WebSocket echo and bounded reconnect tests completed real handshake/send/receive/close cycles, gRPC unary/server/client/bidirectional calls completed against a local tonic HTTP/2 server, history filters/clear/retention were tested, and a native collection was exported and imported back with its request semantics. The HTTP core now also completes local encrypted HTTPS with a custom CA and mutual TLS with a client identity, while invalid certificate paths/material fail before network I/O. The native app has state tests for JSON/auth/GraphQL editing, async local HTTP/SSE/WebSocket workers, bounded event/message history, history reopen, cancellation during body/stream reads and SSE reconnection; native window interaction still needs manual desktop QA. The machine initially reached zero free space during a debug build; the generated Postly target directory was cleaned and subsequent validation used low-debug-info artifacts. A complete workspace check was retried but remains constrained by the available disk space; core/CLI tests and the native app check/clippy pass independently.

## Next highest-value work

1. Add importer fixtures for more Postman body/auth/URL variants.
2. Prototype an embedded or isolated script runtime before enabling broader
   compatibility by default; keep the opt-in Node boundary explicit.
3. Extend the explicit assertion model with broader Postman-compatible cases and GUI coverage.
4. Extend response preview features and complete desktop accessibility/responsive QA.

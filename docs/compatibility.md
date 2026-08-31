# Compatibility status

Compatibility numbers are not published until they come from executable fixtures. The current evidence is the fixtures in compat/postman-import/ and the importer unit coverage, including structured URL/body/auth variants and explicit review warnings.

Run the measured fixture report locally:

~~~bash
cargo xtask compat
cargo xtask compat --json > compat-generated/local.json
~~~

The report has two deliberately separate scores. `fixture_execution` tells us
whether every checked-in fixture still imports successfully. `request_mapping`
counts imported requests that require no manual review; it is a fixture-backed
mapping signal, not a behavioral parity percentage for all of Postman. Warnings
and manual-review cases remain visible in the report.

| Area | Status | Evidence |
| --- | --- | --- |
| Postman Collection v2.1 JSON parsing | working slice | importer tests and fixture |
| folders and request files | working slice | filesystem round-trip and importer tests |
| variables and environments | working slice; keychain-backed `postly env set --secret` and explicit `.env --secret KEY` references resolve without persisting the value | variable precedence tests, secure-store round-trip tests, environment import and dotenv parser tests |
| common headers, bodies and auth | working slice; GUI edits raw/JSON/GraphQL, URL-encoded, multipart and binary-file bodies; OAuth 2.0 Client Credentials, Authorization Code + PKCE explicit exchange plus loopback browser callback, Refresh Token exchange, Device Authorization Grant polling and buffered AWS Signature V4 signing supported | model/import/export coverage, GUI round-trip tests and local token-exchange/cache/browser-callback/SigV4 integration tests |
| HTTP(S)/SOCKS proxy routing | working HTTP/SSE slice plus CLI/GUI WebSocket SOCKS5 and gRPC HTTP CONNECT routing; explicit CLI/GUI proxy, bypass list and env/system proxy support; SOCKS gRPC remains pending | local forwarding, direct bypass, WebSocket CONNECT/SOCKS5 relays, gRPC HTTP/2 CONNECT relay, SOCKS URL construction and invalid-proxy tests |
| HTTPS certificates | core and CLI PEM CA/client-identity slice; native GUI HTTP/SSE Transport settings; PKCS#12/passphrases and domain association pending | local HTTPS CA and mutual-TLS tests, GUI settings persistence and file/format diagnostics |
| Postman Collection v2.1 export | working slice | export/import round-trip fixture |
| Postman environment export | working slice; secure references require explicit secret-resolving export | export serialization fixture and secure export boundary |
| Postman scripts | opt-in basic execution in CLI and GUI send flow, native source editing and explicit GUI preview | Node bridge tests, GUI persistence/preview/send tests and migration docs |
| Postman pm.* runtime | partial tested subset, including scoped variables, iteration data, request headers, common response matchers and bounded `pm.sendRequest` callbacks | compatibility matrix, bridge tests and runner integration test |
| Explicit response assertions | working core/runner and native GUI editor slice | status/header/body/JSON Pointer runner integration test and GUI editor round-trip |
| Native response viewer | working GUI slice with JSON and well-formed XML pretty formatting | virtualized line rows, search, copy, local save and wrapping |
| collection runner | sequential HTTP slice with iteration data, reporters and folder selection | CLI run |
| Local API documentation | deterministic Markdown generator with default redaction | core documentation tests and CLI command |
| cURL interoperability | common import plus native GUI paste and shell-quoted export | parser/exporter tests, GUI draft test and CLI round-trip |
| OpenAPI 3.0/3.1 import | JSON/YAML operations plus same-source-directory local `$ref` resolution | OpenAPI fixtures and importer tests |
| OpenAPI 3.0 export | Native HTTP collections export paths, parameters, bodies, common auth and response examples to JSON/YAML; custom methods and gRPC remain explicit extensions | OpenAPI exporter JSON/YAML tests |
| GraphQL | structured core/CLI/GUI request editing and HTTP slice; CLI and GUI schema introspection explorer | GraphQL model, schema parser, GUI validation and local HTTP integration tests |
| Server-Sent Events | CLI and native GUI streaming slice with cancellation and bounded reconnects using `Last-Event-ID` | chunked parser tests, local streaming/reconnect CLI integration tests and GUI worker/cancellation/reconnect tests |
| WebSocket | CLI and native GUI bidirectional `ws://`/`wss://` slice with interactive text console, header/query auth, bounded reconnects and cancellation | local echo/reconnect integration tests, request-builder coverage and GUI worker/cancellation tests |
| gRPC | local `.proto` discovery plus CLI/native GUI server reflection (v1 with v1alpha fallback), dynamic unary/server/client/bidirectional streaming, HTTPS webpki roots, custom PEM CA/client identity and explicit HTTP CONNECT proxy routing | protox descriptor tests, local tonic HTTP/2 reflection/call integration tests for all four CLI call modes, CLI/GUI CONNECT relay tests, GUI editor round-trip and native GUI reflection worker coverage |
| Postman behavioral parity | not measured; fixture execution and request-mapping signals are available via `cargo xtask compat` | no full behavioral percentage claimed |

Any future score must count semantic cases, exclude placeholders and retain failing fixtures as regressions.

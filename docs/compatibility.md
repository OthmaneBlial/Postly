# Compatibility status

Compatibility numbers are not published until they come from executable fixtures. The current evidence is the fixtures in compat/postman-import/ and the importer unit coverage, including structured URL/body/auth variants and explicit review warnings.

| Area | Status | Evidence |
| --- | --- | --- |
| Postman Collection v2.1 JSON parsing | working slice | importer tests and fixture |
| folders and request files | working slice | filesystem round-trip and importer tests |
| variables and environments | working slice; keychain-backed postly env set --secret references resolve without persisting the value | variable precedence tests, secure-store round-trip tests and environment import |
| common headers, bodies and auth | working slice; GUI edits raw/JSON/GraphQL, URL-encoded, multipart and binary-file bodies; OAuth 2.0 Client Credentials supported, other OAuth grants require review | model/import/export coverage, GUI round-trip tests and local token-exchange/cache integration test |
| HTTP(S) proxy routing | core and CLI slice; GUI setting and SOCKS pending | local proxy forwarding and invalid-proxy tests |
| HTTPS certificates | core and CLI PEM CA/client-identity slice; native GUI HTTP/SSE Transport settings; PKCS#12/passphrases and domain association pending | local HTTPS CA and mutual-TLS tests, GUI settings persistence and file/format diagnostics |
| Postman Collection v2.1 export | working slice | export/import round-trip fixture |
| Postman environment export | working slice; secure references require explicit secret-resolving export | export serialization fixture and secure export boundary |
| Postman scripts | opt-in basic execution, native source editing and explicit GUI preview | Node bridge tests, GUI persistence/preview tests and migration docs |
| Postman pm.* runtime | partial tested subset, including scoped variables, iteration data, request headers and common response matchers | compatibility matrix and script tests |
| Explicit response assertions | working core/runner slice | status/header/body/JSON Pointer runner integration test |
| Native response viewer | working GUI slice | virtualized line rows, search, copy, local save and wrapping |
| collection runner | sequential HTTP slice with iteration data, reporters and folder selection | CLI run |
| cURL interoperability | common import plus native GUI paste and shell-quoted export | parser/exporter tests, GUI draft test and CLI round-trip |
| OpenAPI 3.0/3.1 import | JSON/YAML operations plus same-source-directory local `$ref` resolution | OpenAPI fixtures and importer tests |
| GraphQL | structured core/CLI/GUI request editing and HTTP slice; CLI and GUI schema introspection explorer | GraphQL model, schema parser, GUI validation and local HTTP integration tests |
| Server-Sent Events | CLI and native GUI streaming slice with cancellation and bounded reconnects using `Last-Event-ID` | chunked parser tests, local streaming/reconnect CLI integration tests and GUI worker/cancellation/reconnect tests |
| WebSocket | CLI and native GUI bidirectional `ws://`/`wss://` slice with interactive text console, header/query auth and cancellation; reconnect policy remains CLI-only | local echo/reconnect integration tests, request-builder coverage and GUI worker/cancellation tests |
| gRPC | local `.proto` discovery plus CLI server reflection (v1 with v1alpha fallback), and dynamic unary/server/client/bidirectional streaming CLI and native GUI slice with persisted method metadata, HTTPS webpki roots, custom PEM CA and combined PEM client identity | protox descriptor tests, local tonic HTTP/2 reflection/call integration tests for all four CLI call modes and native GUI unary worker coverage |
| Postman behavioral parity | not measured | no percentage claimed |

Any future score must count semantic cases, exclude placeholders and retain failing fixtures as regressions.

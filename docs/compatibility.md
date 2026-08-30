# Compatibility status

Compatibility numbers are not published until they come from executable fixtures. The current evidence is the fixtures in compat/postman-import/ and the importer unit coverage, including structured URL/body/auth variants and explicit review warnings.

| Area | Status | Evidence |
| --- | --- | --- |
| Postman Collection v2.1 JSON parsing | working slice | importer tests and fixture |
| folders and request files | working slice | filesystem round-trip and importer tests |
| variables and environments | working slice | variable precedence tests and environment import |
| common headers, bodies and auth | working slice | model/import coverage |
| Postman Collection v2.1 export | working slice | export/import round-trip fixture |
| Postman environment export | working slice | export serialization fixture |
| Postman scripts | opt-in basic execution | Node bridge tests and migration docs |
| Postman pm.* runtime | partial tested subset, including common response matchers | compatibility matrix and script tests |
| Explicit response assertions | working core/runner slice | status/header/body/JSON Pointer runner integration test |
| collection runner | sequential HTTP slice | CLI run |
| cURL import | common request slice | curl parser tests and CLI round-trip |
| OpenAPI 3.0/3.1 import | JSON/YAML operation and local `$ref` slice | OpenAPI fixtures and importer tests |
| GraphQL | structured core/CLI/GUI request editing and HTTP slice; schema explorer pending | GraphQL model, parser, GUI validation and local HTTP integration tests |
| Server-Sent Events | CLI streaming slice; reconnecting GUI console pending | chunked parser tests and local streaming CLI integration test |
| WebSocket | CLI bidirectional `ws://`/`wss://` slice; GUI workspace pending | local echo integration test and tungstenite client |
| Postman behavioral parity | not measured | no percentage claimed |

Any future score must count semantic cases, exclude placeholders and retain failing fixtures as regressions.

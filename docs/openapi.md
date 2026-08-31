# OpenAPI import

Postly can import OpenAPI 3.0 and 3.1 JSON or YAML documents into the local
TOML workspace:

The importer writes through a local rollback journal. If a later request or
collection write fails, files already written by that import are restored.

~~~bash
postly import openapi ./openapi.yaml --output ./project
postly import openapi https://api.example.com/openapi.yaml --output ./project
postly list ./project
~~~

HTTP(S) imports are explicit network reads performed by the CLI, capped at
16 MiB for the root document and labeled with the source URL in the JSON
report. URL documents may be JSON or YAML; the importer detects YAML when JSON
parsing is not applicable. `$ref` documents reached from a local or URL import
are also resolved over HTTP(S), with a 15-second timeout, at most five
redirects, 16 MiB per document and 32 remote documents per import. Downloaded
documents are imported as local project files and are not kept as a remote
dependency.

The importer creates one request per local HTTP operation, using the first
server, operation IDs, tags as folders, path/query/header/cookie parameters,
JSON, URL-encoded form, multipart, binary-file and text request-body examples,
response examples (including `Set-Cookie` metadata), local `$ref` components, and common HTTP/API-key security
schemes. JSON Schema examples/defaults, composed schemas
(`allOf`/`oneOf`/`anyOf`), format-aware scalar values and array item samples
are used to create useful sample bodies. Server defaults become
collection variables. Parameters without an
example/default remain explicit `{{variable}}` placeholders and are reported
as warnings rather than silently invented. Multipart binary fields and binary
media types preserve an example path as a local file selection; if no example
path exists, Postly keeps an empty file selection and reports that the user
must choose a file before sending.

The JSON report is part of the migration artifact. Cyclic graphs are detected
and left unresolved with a warning. Local relative references cannot escape
the source directory, and remote references accept only HTTP(S) URLs within
the bounded fetch policy. OAuth coordination and OpenAPI 2/Swagger documents
still require manual review or a future milestone.

## Export a native collection

Native collections can be exported as an OpenAPI 3.0 document in either JSON
or YAML, selected by the output extension:

~~~bash
postly export openapi ./my-api --collection "My API" --output ./openapi.json
postly export openapi ./my-api --collection "My API" --output ./openapi.yaml
~~~

The exporter maps HTTP operations, path/query/header/cookie parameters,
request bodies, common authentication schemes, response examples (including
saved response cookies as `Set-Cookie` header examples) and collection server variables. JSON request and response examples now produce
schemas with nested examples, nullable values, deterministic `oneOf` item
variants for heterogeneous arrays and conservative string formats such as
`uuid`, `date`, `date-time`, `email` and `uri`. Auth-field secrets and
file contents are not embedded, while request and response examples remain
user data and should be reviewed before sharing. GraphQL bodies are represented
as JSON envelopes, while refresh-token-only OAuth, binary file paths and custom
HTTP methods receive explicit x-postly-* metadata and warnings. gRPC requests
are preserved in x-postly-unmapped-requests because they are not standard
OpenAPI operations.

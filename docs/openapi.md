# OpenAPI import

Postly can import OpenAPI 3.0 and 3.1 JSON or YAML documents into the local
TOML workspace:

~~~bash
postly import openapi ./openapi.yaml --output ./project
postly import openapi https://api.example.com/openapi.yaml --output ./project
postly list ./project
~~~

HTTP(S) imports are explicit network reads performed by the CLI, capped at
16 MiB and labeled with the source URL in the JSON report. URL documents may
be JSON or YAML; the importer detects YAML when JSON parsing is not applicable.
Downloaded documents are imported as local project files and are not kept as a
remote dependency.

The importer creates one request per local HTTP operation, using the first
server, operation IDs, tags as folders, path/query/header/cookie parameters,
JSON or text request-body examples, local `$ref` components, and common
HTTP/API-key security schemes. JSON Schema examples/defaults and simple object
properties are used to create useful sample bodies. Server defaults become
collection variables. Parameters without an
example/default remain explicit `{{variable}}` placeholders and are reported
as warnings rather than silently invented.

The JSON report is part of the migration artifact. Cyclic graphs, remote or
out-of-source `$ref` targets, OAuth coordination, binary/multipart body
generation, OpenAPI 2/Swagger documents and response examples still require
manual review or a future milestone.

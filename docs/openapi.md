# OpenAPI import

Postly can import OpenAPI 3.0 and 3.1 JSON or YAML documents into the local
TOML workspace:

~~~bash
postly import openapi ./openapi.yaml --output ./project
postly list ./project
~~~

The importer creates one request per local HTTP operation, using the first
server, operation IDs, tags as folders, path/query/header/cookie parameters,
JSON or text request-body examples, local `$ref` components, and common
HTTP/API-key security schemes. JSON Schema examples/defaults and simple object
properties are used to create useful sample bodies. Server defaults become
collection variables. Parameters without an
example/default remain explicit `{{variable}}` placeholders and are reported
as warnings rather than silently invented.

The JSON report is part of the migration artifact. External or cyclic `$ref`
graphs, OAuth coordination, binary/multipart body generation, OpenAPI
2/Swagger documents and response examples still require manual review or a
future milestone.

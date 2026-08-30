# OpenAPI import

Postly can import an OpenAPI 3 JSON or YAML document into the local TOML
workspace:

~~~bash
postly import openapi ./openapi.yaml --output ./project
postly list ./project
~~~

The importer creates one request per local HTTP operation, using the first
server, operation IDs, tags as folders, path/query/header/cookie parameters,
JSON or text request-body examples, and common HTTP/API-key security schemes.
Server defaults become collection variables. Parameters without an
example/default remain explicit `{{variable}}` placeholders and are reported
as warnings rather than silently invented.

The JSON report is part of the migration artifact. Local `$ref` resolution,
OAuth coordination, binary/multipart body generation, OpenAPI 2/Swagger
documents and response examples still require manual review or a future
milestone.

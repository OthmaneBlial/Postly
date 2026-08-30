# GraphQL

Postly has a first-class GraphQL request model in the Rust core. It keeps the
query, JSON variables and optional operation name separate from a generic raw
body, then sends the standard JSON envelope over HTTP.

The CLI vertical slice can execute a query without an account or workspace:

~~~bash
postly graphql https://api.example.test/graphql \
  --query 'query User($id: ID!) { user(id: $id) { id name } }' \
  --variable id=42
~~~

For larger queries, use `--query-file query.graphql`; use `--variables-json`
for a JSON object instead of repeated `--variable key=value` flags. Bearer,
Basic, custom headers, timeouts, and explicit insecure-TLS mode follow the same
CLI rules as regular HTTP requests.

The response parser preserves partial `data` and structured `errors`, and the
CLI exits non-zero when the GraphQL envelope contains errors even if HTTP
returned status 200. The native GUI exposes the query, variables JSON and
optional operation name in the Body tab, validating them before save or send.
## Inspect a schema

Fetch the endpoint's standard introspection document and print the root fields:

~~~bash
postly graphql https://api.example.test/graphql --introspect
~~~

Add --output-json to receive the complete parsed schema, including named types,
fields, arguments, enum values, input fields and possible types. Headers,
Bearer/Basic auth, certificates and proxy settings use the same flags as a
regular GraphQL request. Introspection errors are reported without treating a
partial schema as complete.

In the native GUI, choose **Inspect schema** in the GraphQL Body tab. The
response pane opens a searchable local explorer with root operations, field
arguments, return types, descriptions and deprecated markers. The schema fetch
is not written to request history.

Imported Postman GraphQL bodies are preserved as structured GraphQL data and
explicitly marked for review.

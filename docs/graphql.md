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
returned status 200. Schema introspection and a dedicated GUI query editor
remain planned; imported Postman GraphQL bodies are preserved as structured
GraphQL data and explicitly marked for review.

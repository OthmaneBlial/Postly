# Postly CLI

The `postly` binary is the headless interface to the same local project model
used by the native workspace. It does not require an account, a hosted
workspace or the desktop application.

## Start a project

~~~bash
postly init ./my-api --name "My API"
postly new request \
  --workspace ./my-api \
  --collection "My API" \
  --name health \
  --folder smoke \
  https://example.com/health
postly list ./my-api
~~~

During development, use `cargo run --` in place of `postly`.

## Send requests

~~~bash
postly request https://api.example.com/users \
  --query "limit=20" \
  --header "Accept: application/json" \
  --bearer "$API_TOKEN" \
  --output-json

postly send ./my-api/collections/my-api/requests/smoke/health.postly.toml \
  --environment Local \
  --output-json
~~~

`request` is for an unsaved URL. `send` executes a saved request file and can
run its preserved scripts with the explicit `--scripts` flag.

## Run collections and folders

~~~bash
postly run ./my-api --environment Local --reporter pretty
postly run ./my-api --folder smoke --environment Local --reporter json
postly run ./my-api --folder auth --scripts --reporter junit > postly-results.xml
~~~

`--folder` selects the named folder and all nested folders. Folder matching is
case-sensitive and accepts either slash separator. A run with no matching
request fails instead of silently reporting success. The runner is sequential
and deterministic; `--fail-fast` stops after the first failed request.

Iteration data is a JSON object or an array of objects:

~~~bash
postly run ./my-api --data-file compat/runner/iterations.json --reporter json
~~~

Exit status is non-zero when a request, explicit assertion, script test or
runner operation fails. JSON and JUnit reporters are intended for local
automation and self-hosted CI systems.

## Environments and transport

~~~bash
postly env set --workspace ./my-api --name Local \
  --set baseUrl=https://api.example.com \
  --secret token="$API_TOKEN"

postly run ./my-api --environment Local \
  --proxy http://127.0.0.1:8080 \
  --ca-cert ./certs/company-ca.pem \
  --client-identity ./certs/client-identity.pem
~~~

Environment values are stored in local ignored files. Certificate options
read PEM files from disk; private-key contents are never command-line output
or history data. `request`, `graphql`, `sse`, `send` and `run` accept the same
HTTP proxy and certificate flags where the transport applies.

## Protocol commands

~~~bash
postly graphql https://api.example.com/graphql --query 'query { health }'
postly sse https://api.example.com/events --reconnect 3
postly websocket wss://api.example.com/socket --send '{"type":"ping"}'
postly grpc describe ./api.proto
postly grpc call https://api.example.com --proto ./api.proto \
  --method /demo.Echo/Echo --message '{"message":"hello"}'
~~~

See the protocol pages for streaming behavior, TLS boundaries and current
limitations:

- [GraphQL](graphql.md)
- [SSE](sse.md)
- [WebSockets](websocket.md)
- [gRPC](grpc.md)

## Inspect local history

~~~bash
postly history ./my-api --limit 20
postly history ./my-api --errors-only --output-json
postly history ./my-api --clear
~~~

History is bounded metadata-only local data. Request bodies, authorization
values and cookie values are not written to the history file.

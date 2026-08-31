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

## Validate a workspace

Run a read-only integrity scan over the canonical manifest, collection files,
saved requests and environments:

~~~bash
postly validate ./my-api
postly validate ./my-api --output-json
~~~

The scan reports every malformed canonical file it can find, uses paths
relative to the workspace root and exits non-zero when issues are present.
Ignored `.postly/` runtime state is intentionally not part of the result.

## Export collections

Export a native collection to Postman Collection v2.1 or OpenAPI 3.0:

~~~bash
postly export collection ./my-api --collection "My API" --output ./my-api.postman.json
postly export openapi ./my-api --collection "My API" --output ./my-api.openapi.yaml
~~~

OpenAPI output is selected by the .json, .yaml or .yml extension. The command
prints a JSON report with the number of exported operations and any
manual-review warnings; unsupported custom methods and gRPC requests are
retained in x-postly-* extensions instead of being silently rewritten.

## Search the workspace

Search covers collection names, request names, folders, methods, URLs,
descriptions, header/cookie names and script source across every collection.
Header/cookie values, bodies and authentication material are intentionally
excluded from both the index and output:

~~~bash
postly search payments --workspace ./my-api
postly search "admin / read" --workspace ./my-api --output-json
~~~

The result paths are relative to the workspace root and deterministic. An
empty query is rejected; a valid query with no matches exits successfully and
prints an explicit no-match message. Matching can use the saved URL, but output
URLs are sanitized to remove credentials, query strings and fragments; header,
cookie, body and auth values, and script contents, are never included in search
results.

## Send requests

~~~bash
postly request https://api.example.com/users \
  --query "limit=20" \
  --header "Accept: application/json" \
  --bearer "$API_TOKEN" \
  --output-json

postly request https://api.example.com/private/users \
  --oauth-token-url https://auth.example.com/oauth/token \
  --oauth-client-id postly-local \
  --oauth-client-secret "$OAUTH_CLIENT_SECRET" \
  --oauth-scope read:users \
  --output-json

postly request https://api.example.com/device-users \
  --oauth-token-url https://auth.example.com/oauth/token \
  --oauth-device-authorization-url https://auth.example.com/oauth/device \
  --oauth-client-id postly-local \
  --oauth-scope read:users \
  --output-json

postly send ./my-api/collections/my-api/requests/smoke/health.postly.toml \
  --environment Local \
  --output-json
~~~

`request` is for an unsaved URL. `send` executes a saved request file and can
run its preserved scripts with the explicit `--scripts` flag. It also evaluates
the request's native response assertions, includes their pass/fail details in
text or JSON output, and exits non-zero when one fails.

## Generate code snippets

Generate source from the canonical saved request model:

~~~bash
postly snippet ./my-api/collections/my-api/requests/health.postly.toml \
  --language javascript
postly snippet ./my-api/collections/my-api/requests/health.postly.toml \
  --language python --output-json
~~~

Supported languages are curl, javascript, python, rust, go, java, csharp and
php. Variables remain visible as {{placeholders}} and OAuth client-credential
values are not fetched. Warnings are printed to stderr (or included in JSON
output), so generated source stays reviewable before it is copied into another
project. See the code-generation guide.

## Run collections and folders

~~~bash
postly run ./my-api --environment Local --reporter pretty
postly run ./my-api --folder smoke --environment Local --reporter json
postly run ./my-api --folder auth --scripts --reporter junit > postly-results.xml
postly run ./my-api --concurrency 4 --reporter json
~~~

`--folder` selects the named folder and all nested folders. Folder matching is
case-sensitive and accepts either slash separator. A run with no matching
request fails instead of silently reporting success. The runner is sequential
by default and deterministic. `--concurrency N` enables bounded batches of up
to 64 script-free requests when no delay is configured; result output remains
in request order, while `--fail-fast` stops after the first completed failed
batch. Scripts and delays keep the safer sequential behavior. Pretty output
includes a status distribution, while JSON exposes it as
`status_distribution` for automation.

Iteration data is a JSON object or an array of objects:

~~~bash
postly run ./my-api --data-file compat/runner/iterations.json --reporter json
~~~

Exit status is non-zero when a request, explicit assertion, script test or
runner operation fails. JSON and JUnit reporters are intended for local
automation and self-hosted CI systems.

The pretty reporter prints each request first, then its native assertion failures
and individual script tests with pass/fail state, duration and bounded errors.
JUnit keeps request-level counts and includes the same script-test detail in
`system-out`, with failure messages and assertion details XML-escaped.

Press `Ctrl-C` during `postly run` to cancel the active HTTP request or script.
The cancellation token is propagated to the runner, no later collection is
started, and the command exits with an explicit cancellation error.

## Environments and transport

~~~bash
postly env set --workspace ./my-api --name Local \
  --set baseUrl=https://api.example.com \
  --secret token="$API_TOKEN"

# Keep the secret out of shell history and process arguments.
printf '%s\n' "$API_TOKEN" | postly env set --workspace ./my-api --name Local \
  --secret-stdin token

# Migrate an imported legacy secret explicitly.
postly env migrate --workspace ./my-api --name Local --key token
postly env migrate --workspace ./my-api --name Local --all

postly run ./my-api --environment Local \
  --proxy http://127.0.0.1:8080 \
  --max-redirects 5 \
  --ca-cert ./certs/company-ca.pem \
  --client-identity ./certs/client-identity.pem
~~~

Plain --set values are stored in the ignored environment file. --secret values
are written to the OS credential store and the file stores only an opaque
workspace-scoped reference; Postly fails rather than silently falling back to a
new plaintext secret when the store is unavailable. The value can still be
exposed by shell history or process arguments, so use a controlled shell for
high-risk credentials; prefer `--secret-stdin KEY` for non-interactive safe
entry. `env migrate --key KEY` moves an existing plaintext value to the OS
credential store without printing it, while `--all` migrates only imported
variables marked as secrets. Certificate options read PEM or `.p12`/`.pfx` files
from disk; private-key contents and PKCS#12 passphrases are never command-line
output or history data. For encrypted PKCS#12 identities, set
`POSTLY_CLIENT_IDENTITY_PASSPHRASE` for the current process. Request, GraphQL,
SSE, send and run accept the same
HTTP(S)/SOCKS proxy, `--no-proxy` bypass and certificate flags where the
transport applies. WebSocket and gRPC support HTTP CONNECT and SOCKS5/SOCKS5H
routing with the same bypass rules. When `--proxy` is omitted, platform
and standard proxy environment variables are handled by the HTTP client.
These HTTP-based commands also accept `--max-redirects N`; the default is 10,
and `--max-redirects 0` keeps the initial 3xx response without following it.
The limit is applied per request, including saved sends and collection runs.
The `postly websocket` command accepts the same `--ca-cert` and
`--client-identity` file formats for `wss://`, plus the explicit `--insecure`
exception; those flags are rejected for `ws://`.

Import a conventional `.env` file explicitly. Values stay literal—there is no
variable expansion or command execution—and only keys named with `--secret`
use the OS credential store:

~~~bash
postly import dotenv .env --output ./my-api --name Local
postly import dotenv .env --output ./my-api --name Local --secret API_TOKEN
~~~

Malformed assignments fail the import. Duplicate keys use the last value and
are reported in the JSON migration report. Keys not listed with `--secret`
remain plaintext in the ignored local environment file so the security choice
is explicit and reviewable.

## Generate local API documentation

Generate deterministic Markdown from saved collections. The output includes
descriptions, navigational request metadata, parameter/header names and
response-example status metadata:

~~~bash
postly docs ./my-api --output ./my-api/API.md
postly docs ./my-api --collection Payments
postly docs ./my-api --include-example-bodies --output /tmp/payments-api.md
~~~

Header values and authentication material are never copied. Response-example
bodies are omitted by default; including them is an explicit choice and the
generated file should be reviewed before sharing.

Saved requests can use OAuth 2.0 Client Credentials without an account or
cloud service. `postly request` and `postly new request` also accept
`--oauth-token-url`, `--oauth-client-id`, `--oauth-client-secret` and
`--oauth-scope` directly. For an explicit Authorization Code + PKCE exchange,
add `--oauth-authorization-url`, `--oauth-redirect-uri`, `--oauth-code` and
`--oauth-code-verifier`; complete the provider login in the browser first.
For an already-issued refresh token, use `--oauth-refresh-token` instead of
the PKCE options. For Device Authorization Grant, use
`--oauth-device-authorization-url` with the token URL and client ID; Postly
prints the verification URL/code and polls until the user approves or the
provider's expiry is reached. Saved requests can then be run normally:

~~~bash
postly send ./my-api/collections/my-api/requests/private/users.postly.toml \
  --environment Local --output-json
~~~

The token exchange is local to the current process and its access token is not
written to history or the workspace. See [authentication](authentication.md)
for the model and supported grant boundaries.

## Protocol commands

~~~bash
postly graphql https://api.example.com/graphql --query 'query { health }'
postly sse https://api.example.com/events --reconnect 3
postly websocket wss://api.example.com/socket --send '{"type":"ping"}'
postly grpc describe ./api.proto
postly grpc reflect https://api.example.com:443 --output-json
postly grpc call https://api.example.com --proto ./api.proto \
  --method /demo.Echo/Echo --message '{"message":"hello"}'
~~~

Serve saved response examples as a local HTTP fixture server:

~~~bash
postly mock ./my-api --port 3000
postly mock ./my-api/collections/users --port 3001 --once
~~~

See [Local mock server](mock-server.md) for route matching, response examples,
delays and the explicit fixture-server boundary.

`grpc reflect` connects to a server reflection endpoint, tries protocol v1
then v1alpha, and prints the discovered services and methods. `--host` sets the
reflection host field; `--ca-cert` and `--client-identity` configure verified
HTTPS just like `grpc call`. Reflected descriptors stay in memory and are not
written to the workspace.

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

## Inspect local cookies

Inspect the active, unexpired cookies held by a workspace session. Values are
always masked in both text and JSON output:

~~~bash
postly cookies ./my-api
postly cookies ./my-api --output-json
postly cookies ./my-api --clear
~~~

The command only reads or explicitly clears `.postly/cookies.json`; it does not
make a network request. Use the native Cookies tab for the same local view.

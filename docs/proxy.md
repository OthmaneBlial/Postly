# Proxy routing

The shared Rust HTTP engine accepts explicit HTTP, HTTPS and SOCKS proxy URLs.
The CLI exposes the setting for immediate requests, GraphQL, SSE, saved
requests and collection runs:

```bash
postly request https://api.example.test/health --proxy http://127.0.0.1:8080
postly graphql https://api.example.test/graphql --query '{ health }' --proxy http://127.0.0.1:8080
postly sse https://api.example.test/events --proxy http://127.0.0.1:8080
postly run ./my-api --proxy http://127.0.0.1:8080
postly websocket ws://api.example.test/socket --proxy http://127.0.0.1:8080
postly grpc reflect https://api.example.test:443 --proxy http://127.0.0.1:8080
postly grpc call https://api.example.test:443 --proto ./api.proto \
  --method /demo.Echo/Echo --proxy http://127.0.0.1:8080

# SOCKS5 with proxy-side DNS resolution
postly request https://api.example.test/health --proxy socks5h://127.0.0.1:1080

# Bypass localhost and an internal domain for an explicit proxy
postly request https://api.example.test/health \
  --proxy http://127.0.0.1:8080 \
  --no-proxy localhost,127.0.0.1,.internal.example
```

The proxy URL and bypass list are validated while constructing the client, and
HTTP forwarding plus direct bypass are covered by deterministic local tests.
The setting is shared by normal HTTP bodies and streaming responses, so
collection-run requests use the same route. `--no-proxy` is an explicit bypass
list for `--proxy`; use `NO_PROXY`/`no_proxy` with environment proxy variables.

When no explicit `--proxy` is supplied, the HTTP engine uses the platform/env
proxy configuration provided by `reqwest`: `HTTP_PROXY`, `HTTPS_PROXY`,
`ALL_PROXY` and `NO_PROXY` (including lowercase variants where supported).
An explicit proxy disables automatic proxy selection for that client and can
use `--no-proxy` to retain selected direct destinations.

The native GUI exposes the proxy URL in the request workspace's `Transport`
tab, plus a comma-separated bypass list. Both are persisted under the ignored
`.postly/gui-settings.json` file and apply to HTTP requests, SSE streams,
WebSocket connections and gRPC channels. The CLI and GUI WebSocket clients and
the CLI/GUI gRPC clients support explicit `http://` proxy CONNECT routing and
the same bypass matching. SOCKS WebSocket and gRPC routing remain unsupported.
A proxy can observe traffic and credentials; use one you trust and keep TLS
verification enabled unless an explicit, documented local exception is required.

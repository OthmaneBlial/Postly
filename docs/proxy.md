# HTTP proxy

The shared Rust HTTP engine accepts an explicit HTTP or HTTPS proxy URL. The
CLI exposes it for immediate requests, GraphQL, SSE, saved requests and
collection runs:

```bash
postly request https://api.example.test/health --proxy http://127.0.0.1:8080
postly graphql https://api.example.test/graphql --query '{ health }' --proxy http://127.0.0.1:8080
postly sse https://api.example.test/events --proxy http://127.0.0.1:8080
postly run ./my-api --proxy http://127.0.0.1:8080
```

The proxy URL is validated while constructing the client, and forwarding is
covered by a deterministic local proxy test. The setting is shared by normal
HTTP bodies and streaming responses, so collection-run requests use the same
route.

The native GUI exposes the proxy URL in the request workspace's `Transport`
tab. It is persisted under the ignored `.postly/gui-settings.json` file and
applies to HTTP requests and SSE streams. Per-request proxy files, SOCKS
configuration and WebSocket proxy routing remain future slices. A proxy can
observe traffic and credentials; use one you trust and keep TLS verification
enabled unless an explicit, documented local exception is required.

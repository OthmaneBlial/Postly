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

The current slice does not yet expose a GUI proxy editor, per-request proxy
files, SOCKS configuration or WebSocket proxy routing. A proxy can observe
traffic and credentials; use one you trust and keep TLS verification enabled
unless an explicit, documented local exception is required.

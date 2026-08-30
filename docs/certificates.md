# HTTPS certificates

Postly's shared HTTP engine supports an additional PEM trust bundle and a
combined PEM client identity. The CLI exposes both options for `request`,
`graphql`, `sse`, `send` and `run`. The native GUI exposes the same HTTP/SSE
settings from the request workspace's `Transport` tab:

```bash
postly request https://api.example.test/health --ca-cert ./certs/company-ca.pem
postly request https://api.example.test/health \
  --ca-cert ./certs/company-ca.pem \
  --client-identity ./certs/client-identity.pem
postly send ./my-api/collections/my-api/requests/health.postly.toml \
  --ca-cert ./certs/company-ca.pem
postly run ./my-api --ca-cert ./certs/company-ca.pem
```

GUI transport settings are stored locally in the ignored
`.postly/gui-settings.json` file. They contain paths and connection flags,
never certificate or private-key contents. `Validate & apply` checks the
configured files before the next request.

`--ca-cert` accepts a PEM bundle containing one or more trusted CA
certificates. It is added to the normal trust roots; it does not disable
certificate verification.

`--client-identity` accepts an unencrypted PEM bundle containing the client
certificate chain followed by its private key, for example:

```text
-----BEGIN CERTIFICATE-----
...
-----END CERTIFICATE-----
-----BEGIN PRIVATE KEY-----
...
-----END PRIVATE KEY-----
```

Postly validates the file path and PEM structure before sending a request.
Diagnostics include the file path but never print certificate or private-key
contents. The repository contains disposable local-only certificate fixtures
under `crates/postly-core/testdata/tls/`; they are used to exercise both
ordinary HTTPS with a custom CA and mutual TLS.

This slice deliberately does not claim support for encrypted private keys,
PKCS#12 containers, per-domain certificate association, WebSocket certificate
routing or gRPC certificate routing. Those are separate follow-up
capabilities. `--insecure` remains an explicit escape hatch and should only
be used for a controlled local exception.

# HTTPS certificates

Postly's shared HTTP engine supports an additional PEM trust bundle and a
combined PEM or encrypted PKCS#12 client identity. The CLI exposes both options for `request`,
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

For a password-protected `.p12` or `.pfx` identity, provide the passphrase
through the process environment rather than a command-line argument:

```bash
POSTLY_CLIENT_IDENTITY_PASSPHRASE='use-a-local-secret-manager-value' \
  postly request https://api.example.test/health \
  --client-identity ./certs/client-identity.p12
```

The passphrase is read only for the current process and is not printed,
persisted in the workspace, or included in request history. The GUI offers a
masked session-only passphrase field in Transport settings.

GUI transport settings are stored locally in the ignored
`.postly/gui-settings.json` file. They contain paths and connection flags,
never certificate or private-key contents. `Validate & apply` checks the
configured files before the next request.

`--ca-cert` accepts a PEM bundle containing one or more trusted CA
certificates. It is added to the normal trust roots; it does not disable
certificate verification.

`--client-identity` accepts either an unencrypted PEM bundle containing the
client certificate chain followed by its private key, or a password-protected
DER PKCS#12 container. `.p12` and `.pfx` extensions select the PKCS#12 path;
other extensions are treated as PEM. A PEM identity looks like:

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

Per-domain certificate association remains open. CLI and native GUI WebSocket
connections now route the Transport CA, combined PEM or PKCS#12 client identity
and explicit insecure-TLS settings for `wss://`; gRPC CLI calls have their own
HTTPS PEM CA/client-identity options; see
[gRPC](grpc.md). `--insecure` remains an explicit escape hatch and should only
be used for a controlled local exception.

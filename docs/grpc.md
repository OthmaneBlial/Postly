# gRPC

Postly supports a descriptor-driven gRPC slice. It can compile a local root
`.proto` file or discover descriptors through the standard server reflection
protocol, then convert protobuf JSON into dynamic messages and execute unary,
server-streaming, client-streaming and bidirectional calls through Tonic.

The native workspace can persist the same configuration. Open the command palette,
choose **New gRPC request**, set the endpoint, proto file, method path and metadata
in the gRPC tab, then put a protobuf-JSON object (or an array for client-streaming)
in the Body tab and press **Send**. Relative proto/include paths are resolved from
the workspace root; the response viewer presents the resulting messages as local
JSON while retaining the request in the normal Git-friendly collection model.

## Discover services and methods

```bash
cargo run -- grpc describe ./echo.proto
cargo run -- grpc describe ./echo.proto --include ./proto --output-json
```

The output includes canonical gRPC paths such as `/demo.Echo/Echo`, input/output message types and streaming flags. The root file's directory is included automatically; `--include` adds import roots.

## Discover a live server through reflection

When a gRPC server exposes server reflection, Postly can discover its services
without a local `.proto` checkout:

```bash
cargo run -- grpc reflect http://127.0.0.1:50051
cargo run -- grpc reflect https://api.example.com:443 --output-json
```

Postly tries reflection protocol v1 first and falls back to v1alpha for older
servers. The command asks for the service list, fetches the descriptors for
each service, and builds the dynamic schema in memory; it does not write a
generated `.proto` file. Use `--host` when the server routes reflection by a
virtual host, and use the same `--ca-cert` and `--client-identity` flags as a
gRPC call for a private HTTPS endpoint. TLS verification stays enabled.

```bash
cargo run -- grpc reflect https://grpc.internal.example:443 \
  --host api.internal.example \
  --ca-cert ./certs/company-ca.pem \
  --client-identity ./certs/client-identity.pem \
  --output-json
```

Reflection is discovery only in the current CLI slice. Use a discovered method
with `grpc call` once its local descriptor or generated request configuration is
available; a future GUI slice will make the reflected schema directly
selectable in the native workspace.

## Call a unary or server-streaming method

```bash
cargo run -- grpc call http://127.0.0.1:50051 \
  --proto ./echo.proto \
  --method demo.Echo/Echo \
  --message '{"message":"hello"}' \
  --metadata x-request-id=local \
  --output-json
```

Use `--message-file request.json` for a larger request. Metadata is supplied as repeated `--metadata key=value`; bearer and Basic credentials are sent as the standard `authorization` metadata entry. HTTPS endpoints use the bundled webpki roots by default.

For a private HTTPS service, add a PEM-encoded CA certificate. Mutual TLS accepts a combined PEM file containing the client certificate and private key:

```bash
cargo run -- grpc call https://localhost:50051 \
  --proto ./echo.proto \
  --method demo.Echo/Echo \
  --message '{"message":"secure"}' \
  --ca-cert ./certs/company-ca.pem \
  --client-identity ./certs/client-identity.pem
```

The certificate flags are validated before the network call and only apply to `https://` endpoints. TLS verification remains enabled; an explicit insecure-TLS mode, encrypted/PKCS#12 identities and passphrase handling are not supported yet.

For a server-streaming method, use its canonical path and the same request options:

```bash
cargo run -- grpc call http://127.0.0.1:50051 \
  --proto ./echo.proto \
  --method demo.Echo/EchoStream \
  --message '{"message":"hello"}' \
  --output-json
```

Server responses are consumed progressively. With `--output-json`, Postly emits one JSON object per line with `method`, `stream_index` and `response`; human-readable output labels each message. The GUI collects the finite response stream into the local response viewer.

## Client and bidirectional streaming

Client-streaming and bidirectional methods take a JSON array of protobuf request
objects. The array is encoded as a finite client stream. The native GUI uses the
same JSON shape in its Body tab:

```bash
cargo run -- grpc call http://127.0.0.1:50051 \
  --proto ./echo.proto \
  --method demo.Echo/EchoClient \
  --message '[{"message":"one"},{"message":"two"}]' \
  --output-json

cargo run -- grpc call http://127.0.0.1:50051 \
  --proto ./echo.proto \
  --method demo.Echo/EchoBidi \
  --message-file ./messages.json \
  --output-json
```

Client-streaming methods return one response and bidirectional methods emit
one JSON object per response with `stream_index` and `input_count`. Custom CA
certificates and combined PEM client identities work for HTTPS calls. Native GUI gRPC calls
currently require verified TLS for HTTPS endpoints and reject proxy/insecure-TLS
configuration explicitly rather than silently ignoring it.

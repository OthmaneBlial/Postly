# gRPC

Postly currently supports a local, descriptor-driven gRPC slice. It compiles a root `.proto` file in Rust, discovers services and methods, converts protobuf JSON into dynamic messages, and executes unary, server-streaming, client-streaming and bidirectional calls through Tonic.

## Discover services and methods

```bash
cargo run -- grpc describe ./echo.proto
cargo run -- grpc describe ./echo.proto --include ./proto --output-json
```

The output includes canonical gRPC paths such as `/demo.Echo/Echo`, input/output message types and streaming flags. The root file's directory is included automatically; `--include` adds import roots.

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

Server responses are consumed progressively. With `--output-json`, Postly emits one JSON object per line with `method`, `stream_index` and `response`; human-readable output labels each message. Reflection and GUI gRPC editing remain future slices.

## Client and bidirectional streaming

Client-streaming and bidirectional methods take a JSON array of protobuf request
objects. The array is encoded as a finite client stream:

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
certificates and combined PEM client identities work for HTTPS calls; reflection
and GUI gRPC editing remain future slices.

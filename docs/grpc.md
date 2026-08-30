# gRPC

Postly currently supports a local, descriptor-driven gRPC slice. It compiles a root `.proto` file in Rust, discovers services and methods, converts protobuf JSON into dynamic messages, and executes unary and server-streaming calls through Tonic.

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

Use `--message-file request.json` for a larger request. Metadata is supplied as repeated `--metadata key=value`; bearer and Basic credentials are sent as the standard `authorization` metadata entry. HTTPS endpoints use the bundled webpki roots.

For a server-streaming method, use its canonical path and the same request options:

```bash
cargo run -- grpc call http://127.0.0.1:50051 \
  --proto ./echo.proto \
  --method demo.Echo/EchoStream \
  --message '{"message":"hello"}' \
  --output-json
```

Server responses are consumed progressively. With `--output-json`, Postly emits one JSON object per line with `method`, `stream_index` and `response`; human-readable output labels each message. Reflection, client/bidirectional streaming, custom CA files, client certificates, insecure TLS and GUI gRPC editing remain future slices.

# Postly v0.1.0

A locally validated macOS Apple Silicon technical preview of Postly, the
open-source Postman alternative without an account.

## Included

- Native Rust desktop workspace and CLI.
- Git-friendly collections and local environments.
- REST/HTTP, GraphQL, SSE, WebSocket and dynamic gRPC workflows.
- Postman v2.1 and environment migration with explicit review diagnostics.
- Assertions, opt-in Node scripts, collection runs, JSON/JUnit reports and
  local mock responses.
- A SHA-256 manifest inside the archive.

## Install

1. Download and extract `postly-v0.1.0-macos-aarch64.tar.gz`.
2. Run `shasum -a 256 -c SHA256SUMS`.
3. Run `./postly --help` or launch `./postly-gui`.

## Scope

This release is a technical preview. Encrypted/PKCS#12 identities,
passphrases, broader Postman `pm.*` parity, richer GUI polish, cross-platform
installers, notarization and external end-user validation remain open. TLS
certificate verification stays enabled by default.

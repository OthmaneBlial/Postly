# Feature reference

This repository contains working vertical slices, not a static interface mockup.

- **HTTP and REST:** GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS and custom
  methods; query parameters, duplicate headers, cookies, redirects, compression,
  timeouts, cancellation, raw/JSON/form/multipart/file bodies and response metadata.
- **Authentication:** Basic, Digest, Bearer, API key, OAuth 2.0 Client Credentials,
  Authorization Code + PKCE, Refresh Token and Device Authorization Grant,
  plus buffered AWS Signature V4 signing; including variable resolution,
  opt-in loopback browser login, bounded approval polling and local
  expiry-aware token caching.
- **Privacy-aware environments:** plain values stay in ignored local files;
  `postly env set --secret` stores new values in the OS credential store and keeps
  only an opaque workspace-scoped reference in the project; `--secret-stdin` and
  explicit legacy-secret migration avoid putting values in shell arguments. The
  native GUI can create, edit, disable and rename environments; existing secret
  references stay masked and new secret values go through the OS credential store.
- **Transport controls:** explicit insecure-TLS opt-in for supported HTTP and
  WebSocket flows,
  verified HTTPS, custom PEM CAs, combined PEM or password-protected PKCS#12
  client identities, HTTP(S)/SOCKS
  proxy routing, CLI/GUI WebSocket and gRPC HTTP CONNECT routing, WebSocket
  custom CA/client-identity support, native GUI exact-host/wildcard
  certificate associations, environment proxy support
  and bypass rules, per-request redirect/cookie overrides, with actionable
  diagnostics.
- **Response inspection:** Pretty/Raw views with JSON, YAML and well-formed XML
  formatting, detected JSON/YAML/XML/HTML/JavaScript/Text previews with lightweight
  syntax coloring, status/headers/cookies/protocol/duration, local search,
  TTFB/download timing where measurable, wrapping, clipboard copy, virtualized rendering, an in-app developer console,
  response snapshots and save-as-example fixtures for local mocks. Buffered
  HTTP responses are bounded to 100 MiB by default and can be tuned in the GUI
  Transport settings; streaming endpoints remain progressive and bounded by
  their live history views.
- **Collections:** local TOML projects, nested folders, deterministic discovery,
  stable request identity, duplicate/delete/rename flows, metadata-only history
  and workspace-wide request search. The native GUI supports multiple saved
  request tabs, dirty indicators, close-others, reordering and local tab
  restoration.
- **Migration:** Postman Collection v2.1 and environment import/export, explicit
  `.env` import with opt-in keychain storage, OpenAPI 3.0/3.1 JSON/YAML import
  with guarded local references, and cURL paste/copy.
- **API documentation:** generate deterministic local Markdown from collections,
  request descriptions, parameters, headers and response-example metadata.
- **OpenAPI export:** turn a native collection into OpenAPI 3.0 JSON or YAML,
  with explicit warnings and x-postly extensions for lossy cases.
- **Project site:** a responsive, dependency-free static showcase with SEO
  metadata, reduced-motion support and source-backed navigation.
- **Code snippets:** generate reviewable cURL, JavaScript fetch, Python
  requests, Rust reqwest, Go, Java, C# and PHP from the same saved request
  model.
- **Testing and automation:** response assertions, an opt-in Node.js script
  bridge, tested `pm.*` behavior including request/body facades and bounded
  `pm.sendRequest` callbacks, collection runs, folder selection, iteration
  data from JSON or CSV files, bounded script-free concurrency, fail-fast
  execution, pretty/JSON/JUnit reporters and a deterministic local HTTP mock
  server backed by saved response examples.
- **Modern API protocols:** structured GraphQL with schema introspection, SSE
  subscriptions, WebSocket text/binary flows with saved message presets, and
  dynamic gRPC calls with local `.proto` discovery or CLI server reflection (v1
  with v1alpha fallback).
- **Native desktop workspace:** request editing, dedicated raw text/JSON/XML/
  HTML/JavaScript body modes, Scripts and Body tabs, command palette,
  cancellation, local history, transport settings, dark/light/system themes and
  the same core semantics as the CLI.

The [living progress log](progress.md) records what is implemented, what was
verified locally and which release gates still require external validation.

The current source preview also includes guided desktop import, a saved-request
collection runner, structural JSON comparison and an adaptive, theme-aware
response inspector. See [desktop workflows](desktop-workflows.md) for the
supported steps and [the implementation log](roadmap-implementation.md) for
local evidence. Source capabilities are not a claim about every older release.

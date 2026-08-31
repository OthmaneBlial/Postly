# Postly progress

Updated: 2026-08-31

## Current milestone

Foundation plus a first native desktop/CLI/core vertical slice.

Implemented:

- Rust workspace and local validation entry point.
- Local project/collection/request/environment TOML model.
- Deterministic recursive request discovery.
- Variable scopes, precedence and undefined-variable diagnostics.
- Native async HTTP execution with common body/auth/header/query behavior.
- Custom PEM CA bundles and combined PEM client identities in the shared HTTP engine and CLI workflows, with actionable file/format diagnostics and local HTTPS/mTLS integration tests.
- Basic, Bearer, API-key, OAuth 2.0 Client Credentials, Authorization Code + PKCE (explicit exchange and loopback browser callback), Refresh Token, Device Authorization Grant and buffered AWS Signature V4 signing in the native model, GUI, CLI, Postman import/export and shared HTTP engine, with bounded approval polling, in-memory expiry-aware token caching and local token-exchange/browser-callback/SigV4 integration tests.
- Explicit HTTP(S)/SOCKS proxy routing in the shared HTTP engine and CLI request/stream/runner workflows, plus CLI/GUI WebSocket and gRPC SOCKS5/HTTP CONNECT routing, with `--no-proxy` bypass lists, platform/env proxy support, invalid-URL diagnostics and local forwarding/bypass/relay tests.
- Response metadata and JSON pretty formatting.
- Postman Collection v2.1 and environment import reports.
- Postman Collection v2.1 and environment export with a tested native round-trip.
- Postman importer regression fixture for structured URLs, disabled/non-text values, form bodies, file parts, API-key query auth and GraphQL review warnings.
- Postman import now preserves scalar header values, marks unsupported auth types
  as manual-review requests (including inherited auth), and exercises JSON/HTML
  raw bodies in the expanded variant fixture.
- collection/folder/request script source preservation and a truthful pm.* compatibility matrix.
- collection and folder authentication inheritance is materialized into imported request files.
- init, request, send, import, list and sequential run CLI commands.
- new request creates and persists a saved request without editing files by hand.
- env set creates local environments and saved requests resolve enabled environment variables.
- postly env set --secret stores new environment secrets in the OS credential
  store and persists only workspace-scoped opaque references; CLI and GUI
  resolution share the same backend, while secure Postman export is explicit.
- `postly env set --secret-stdin KEY` accepts secret values without command-line
  arguments, and `postly env migrate --key/--all` migrates legacy plaintext
  values into the OS credential store while preserving enabled flags.
- `postly import dotenv` imports literal `KEY=VALUE` files with strict parsing;
  only explicitly named `--secret KEY` values are stored in the OS credential
  store, and duplicate/malformed entries are reported deterministically.
- common cURL commands can be parsed and imported without shell execution.
- Native GUI cURL paste import creates an unsaved draft, and the current request can be copied as a shell-quoted cURL command with explicit warnings for non-materialized auth.
- saved-request executions can be recorded, searched, filtered, cleared and retained as bounded ignored metadata-only local history.
- global request metadata search across collections is available in the native workspace and CLI; secrets are excluded from the index.
- native `postly-gui` request workspace with async send, editor tabs and response views.
- native saved-request duplication and guarded deletion with storage/UI regression tests.
- saved request rename/folder changes relocate the canonical file while preserving request identity.
- response Pretty/Raw views now provide JSON and well-formed XML formatting,
  detected JSON/XML/HTML/JavaScript/Text previews with lightweight syntax
  coloring, case-insensitive local search with occurrence counts and line
  snippets; HTTP and gRPC responses expose received body size as metadata. The
  buffered HTTP engine rejects bodies over its configurable 100 MiB default
  before unbounded accumulation, while SSE remains progressive.
- response Pretty/Raw views now use virtualized line rows with optional wrapping, clipboard copy and workspace-local response snapshots.
- OpenAPI 3.0/3.1 JSON/YAML import for common operations, local `$ref` components and same-source-directory external refs, parameters, JSON bodies and auth placeholders.
- OpenAPI import accepts local files or explicit HTTP(S) URLs with a bounded download and source-preserving report.
- Native collections export to OpenAPI 3.0 JSON/YAML with operation paths,
  parameters, request bodies, common security schemes, response examples and
  explicit x-postly-* warnings/extensions for lossy or non-standard cases.
- Structured GraphQL core/CLI/GUI request model with variables, operation names, partial-data/error parsing, validated GUI editing and local HTTP integration coverage.
- GraphQL schema introspection through the CLI and native GUI, with parsed roots, fields, arguments, nested type references, enums, input fields, filtering and deprecated markers.
- SSE parser plus progressive CLI/native GUI subscriptions with chunk-safe event decoding, event metadata, bounded GUI history, JSON-lines output and local streaming coverage.
- WebSocket CLI and native GUI client for `ws://` and `wss://` with headers/auth, interactive text sends, named Git-friendly message presets, text/binary/pong output, ping replies, bounded reconnects/history and local integration coverage.
- native GUI HTTP, SSE and WebSocket workers support explicit cancellation, with cancellation-aware body/stream reads and local worker tests.
- Native GUI `Scripts` tab edits and persists imported pre-request/test source;
  explicit GUI previews now run in a worker and display test/log results without
  applying changes automatically; CLI runner execution remains explicit too.
  A session-only opt-in also runs those scripts in the HTTP send worker,
  applies pre-request mutations to that send, and keeps post-response failures
  visible while retaining the response.
- native GUI Transport tab with persisted local timeout, HTTP(S)/SOCKS proxy, bypass hosts, custom CA, client identity and explicit insecure-TLS settings for HTTP/SSE/WebSocket/gRPC workflows; WebSocket and gRPC use HTTP CONNECT routing.
- Native GUI Body tab editors for raw text, JSON, XML, HTML, JavaScript and GraphQL plus URL-encoded fields, multipart text/file parts and binary file uploads, with disabled entries and optional content types preserved.
- Native GUI command palette with searchable request actions and keyboard shortcuts for new, save, send, cancel, response clearing and wrapping.
- Dynamic gRPC `.proto` compilation with service/method discovery plus unary, server-streaming, client-streaming and bidirectional CLI calls using protobuf JSON, metadata, HTTPS webpki roots, custom PEM CAs, combined PEM client identities and explicit HTTP CONNECT proxy routing; explicit insecure TLS remains pending.
- CLI gRPC server reflection discovery supports protocol v1 with a v1alpha fallback, keeps reflected descriptors in memory and reports services, methods and streaming shapes as text or JSON.
- Native GUI gRPC requests now persist local proto/include paths or server-reflection mode/host, method paths and metadata, edit protobuf JSON bodies, and execute unary plus finite streaming shapes through the same dynamic descriptor model; GUI local-proto and reflection worker coverage is backed by local tonic HTTP/2 servers.
- Persistable response assertions for exact/ranged status, present/equal/containing headers, body text and JSON Pointer values, evaluated by the runner without Node.js.
- Opt-in Node.js script bridge with basic `pm.*`, `pm.test` and runner assertion results.
- Script compatibility boundary now carries explicit variable unsets, globals,
  read-only iteration data, request header mutations and bounded source size;
  the child environment is reduced to `PATH`, serialized input and stdout/stderr
  pipes have explicit size caps, and the worker bounds process duration plus
  captured logs and test results.
- On Node versions exposing both `--permission` and `--allow-net`, the script
  bridge enables Node's permission model for defense in depth: bounded network
  callbacks remain available while filesystem, child-process, worker and addon
  permissions stay disabled by default; the VM is still not a hostile-code
  sandbox.
- Common response assertions now cover headers, cookies, status health/categories, numeric/type/regex and negated expectations.
- The Node bridge now tests a broader explicit expectation subset including
  stable deep equality, keys, length, oneOf, empty and numeric range matchers;
  broad Chai/Postman parity remains intentionally partial.
- `pm.sendRequest` now has a tested, opt-in callback slice for bounded HTTP(S)
  subrequests with headers, raw bodies, response text/JSON and response
  assertions; direct bridge and collection-runner integration are covered, and
  it intentionally does not claim Postly transport parity.
- The script bridge now exposes Postman-shaped request URL, query, cookie and
  body facades, with URL/query mutations and raw/JSON/form body edits converted
  back into the native request model through a dedicated regression test.
- Common response-side Postman assertions now include optional header values,
  JSON body path checks, non-empty body checks and header/cookie object views;
  matcher coverage includes exact/all/any keys, contain, deep properties and
  common numeric aliases with regression coverage.
- Stateful cookie jar, response `Set-Cookie` metadata and explicit request-cookie editing; saved workspaces persist a bounded ignored local jar.
- reusable runner results with pass/fail status, deterministic order, fail-fast and cooperative cancellation.
- runner summaries now expose a deterministic HTTP status distribution in JSON
  and the pretty reporter.
- runner iteration data from JSON objects/arrays plus pretty, JSON and JUnit reporters.
- CLI collection runs can target an exact folder and its nested request folders.
- A dedicated CLI reference documents headless requests, folder runs, reports,
  transport flags, protocol commands and exit-status behavior.
- postly snippet generates reviewable cURL, JavaScript, Python, Rust, Go, Java,
  C# and PHP source from the saved request model while preserving placeholders
  and warning when credentials or unsupported body semantics need review.
- A local `cargo xtask bench` harness measures real Postman import and generated
  1,000-request workspace open/search operations without publishing invented
  competitor comparisons.
- `cargo xtask compat` executes checked-in Postman collection/environment and
  OpenAPI fixtures, reporting fixture execution separately from manual-review
  request mapping instead of claiming full behavioral parity.
- Local `cargo xtask fuzz` targets cURL parsing, variable interpolation and
  malformed Postman imports with a bounded smoke run; fuzz artifacts remain
  ignored and no GitHub Actions workflow is required.
- Native GUI crash recovery persists a bounded, private draft snapshot with
  atomic replacement, Unix `0600` permissions, automatic restore as a new
  unsaved request and an explicit discard action.
- Native GUI environment editing creates, updates and renames local environment
  files, preserves disabled flags, masks existing keychain-backed values and
  sends newly entered secret values through the OS credential store.
- Workspace TOML writes now replace canonical files through same-directory
  temporary files, with coverage that no temporary destination remains after a
  replacement.
- Native GUI saved-request tabs support dirty indicators, activation,
  close-others, reordering and restoration from ignored local path-only state.
- Native GUI developer console retains bounded execution, protocol and script
  events, exposes warnings/errors separately and redacts known sensitive values
  before display.
- Native GUI appearance supports persisted dark, light and system themes while
  deriving secondary text, panels and headings from the active egui palette.
- Native GUI responses can be saved as named canonical response examples for
  local mocks, with a bounded UTF-8 body check and explicit secret-data warning.
- `postly mock` serves saved response examples through a deterministic local HTTP
  server, with method/path routing, response headers and bodies, status codes,
  bounded request-header parsing, optional per-example delay and `--once` mode.
- `postly docs` generates deterministic local Markdown for collections while
  omitting header/auth values and response bodies by default; example bodies
  require an explicit opt-in.
- The dependency-free `website/` showcase has responsive navigation, SEO
  metadata, JSON-LD and source-backed documentation links; local browser QA
  covers 390px and 1440px layouts, mobile menu state and response-tab switching.
- Ignored shallow research corpus for Bruno, Yaak and Posting.

Not yet implemented:

- Desktop GUI polish, richer response preview features beyond the current
  lightweight format detection/coloring, and manual responsive/accessibility QA.
- Embedded/hardened script runtime and broader pm.* compatibility beyond the
  tested scoped-variable/request-header/response subset.
- Broader Postman-compatible test/assertion cases beyond the current explicit native slice.
- transactional workspace restore and broader multi-document crash recovery.
- Encrypted/PKCS#12 identities, passphrase handling, per-domain certificate association and explicit insecure TLS remain open; gRPC custom PEM CA/client identity and SOCKS5 routing are now available in CLI and GUI.
- OpenAPI cyclic/remote references, richer schema generation beyond the current composed/format-aware samples and deeper protocol-specific GUI tooling.
- richer deterministic protocol test server tooling beyond the HTTP mock,
  cross-client/memory benchmarks and production release validation beyond the
  local package smoke checks.

## Verification

The GUI recovery slice adds round-trip coverage for an unsaved JSON draft,
private file permissions on Unix and explicit discard without changing saved
requests. The GraphQL schema slice adds core parser coverage for roots, nested type
references, arguments and malformed introspection, a local HTTP CLI test, and a
native GUI worker test that confirms schemas stay out of request history. The
gRPC reflection adds a local tonic server test covering service discovery and
descriptor hydration; the CLI and GUI expose the same discovery path with
verified HTTP(S) transport. The gRPC GUI slice adds persisted request
configuration, a local unary worker round-trip through a dynamic `.proto`
descriptor and a reflection-backed worker round-trip.

CLI and GUI gRPC proxy tests complete a real HTTP CONNECT tunnel and relay
HTTP/2 traffic.

`cargo xtask check` is the required validation command for this milestone. The CLI was exercised against deterministic local HTTP servers: a real JSON response was received and formatted, then a saved request created with new request was sent and run with structured JSON output. Iteration data was exercised twice through JSON and JUnit reporters, an environment created with env set resolved two variables into a saved request, a cURL command was imported and sent with HTTP 201, OpenAPI YAML generated two requests, an OpenAPI YAML document was fetched from a local HTTP URL, same-directory external references were resolved while source traversal and cycles were reported, an imported Postman test executed through the opt-in Node bridge, a two-request cookie exchange verified jar reuse plus response metadata, a manual cookie round-trip verified the ignored local jar and file-size guard, a GraphQL query with variables was sent through the structured CLI command, an SSE stream was decoded progressively from a local endpoint and reconnected with `Last-Event-ID`, WebSocket echo and bounded reconnect tests completed real handshake/send/receive/close cycles, gRPC unary/server/client/bidirectional calls completed against a local tonic HTTP/2 server and a separate local mutual-TLS gRPC call verified custom CA plus client identity, OAuth 2.0 Client Credentials fetched one cached token for two API requests, a local Device Authorization flow completed device-code prompting, pending polling and bearer delivery, and a local Authorization Code + PKCE flow completed browser URL construction, state-checked loopback callback, token exchange and bearer delivery, history filters/clear/retention were tested, workspace metadata search was exercised across collections without matching secrets, and a native collection was exported and imported back with its request semantics. The CLI mock router is covered by unit tests for method/path matching, query omission, saved status/body/delay and generic 404 behavior. The HTTP core also completes local encrypted HTTPS with a custom CA and mutual TLS with a client identity, while invalid certificate paths/material fail before network I/O. The native app has state tests for JSON/auth/GraphQL/OAuth/script editing, workspace search, async local HTTP/SSE/WebSocket workers, bounded event/message history, history reopen, cancellation during body/stream reads and SSE reconnection; browser callback integration is covered at the shared core and desktop opener/manual window interaction still needs manual desktop QA. The machine initially reached zero free space during a debug build; the generated Postly target directory was cleaned and subsequent validation used low-debug-info artifacts. The complete workspace check now passes locally; packaging, external review and manual desktop QA remain separate gates.
The latest transport slice adds shared SOCKS5 stream negotiation for CLI/GUI
WebSocket and gRPC connectors, with local handshake/relay coverage; explicit
SOCKS5 username/password credentials are supported.

The OAuth Device Authorization slice adds RFC 8628-style device-code
requests, safe verification prompts in the CLI/GUI, bounded polling with
pending/slow-down handling, Postman migration/export and local approval
integration coverage. The Authorization Code + PKCE slice now adds a bounded
loopback callback with state validation, ephemeral verifier generation and
explicit CLI / automatic GUI browser opening. The AWS Signature V4 slice adds
runtime canonical signing for buffered HTTP requests, temporary session-token
support, Postman awsv4 import/export, local header-delivery coverage and a
deterministic AWS S3 signature-calculation regression vector.

The release packaging gate has since been validated locally with the macOS
ARM64 archive, internal SHA256 manifest and packaged CLI smoke checks. External
review, public distribution and manual desktop QA remain separate gates.

The first public `v0.1.0` macOS Apple Silicon technical preview is now
published as a GitHub Release with the validated archive and installation
notes. It is intentionally not presented as notarized, cross-platform or
production-ready; those release gates remain explicit.

The OpenAPI export slice is covered by a native JSON/YAML serialization test
and a real CLI smoke run from a temporary workspace; operation paths, server
variables, security metadata and request/response examples were inspected.

On this macOS arm64 workspace run, `cargo xtask compat --json` passed all 6
checked-in fixtures; its separate request-mapping signal was 12/17 (70.59%),
with the remaining cases retained as explicit manual review. The latest local
`cargo xtask bench --json` run on macOS arm64 reported medians of 7.43 ms for
the already-built CLI `--help` startup, 5.75 ms for the Postman variant import,
151.07 ms to open a generated 1,000-request workspace and 152.20 ms to search
it. The startup samples ranged from 6.44 ms to 681.82 ms, so these are local
measurements rather than competitor claims or universal performance
guarantees.

The latest local validation also passed `cargo xtask fuzz`: the three checked-in
targets (`curl_command`, `variables` and `postman_import`) each completed 256
bounded runs without a crash. `cargo xtask package` built the current macOS
arm64 release artifacts, executed the packaged CLI's `--version` and `--help`
smokes, verified the archive listing and reported SHA-256
`a42b743032f0c05c0a26afaeb05a2b9c615746097cdf925a79e1c3b1e24c2276`.

## Next highest-value work

1. Add importer fixtures for more Postman body/auth/URL variants.
2. Prototype an embedded or isolated script runtime before enabling broader
   compatibility by default; keep the opt-in Node boundary explicit even with
   its resource guards.
3. Extend the explicit assertion model with broader Postman-compatible cases.
4. Extend response preview features and complete desktop accessibility/responsive QA.

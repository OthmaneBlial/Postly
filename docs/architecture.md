# Postly architecture

## Current boundary

The repository is a Cargo workspace with four packages:

- postly-core: durable models, variable resolution, filesystem storage, Postman import, HTTP execution and runner orchestration.
- postly: the CLI, intentionally thin over the core.
- postly-xtask: local formatting, linting and test orchestration.
- postly-app: native desktop presentation and asynchronous interaction with the core.

The core is the product boundary. The native desktop UI and CLI call the same
request, persistence and runner services instead of reimplementing request
behavior in a frontend.

## Canonical files

Project-critical data is ordinary TOML:

~~~text
postly.toml
collections/<collection>/postly.collection.toml
collections/<collection>/requests/<folder>/<request>.postly.toml
environments/<environment>.postly-env.toml
~~~

Request files carry stable UUIDs, readable names and all supported request semantics. Filesystem paths currently provide deterministic ordering; explicit ordering metadata can be added later if the UI needs drag-and-drop ordering. Canonical TOML writes use a unique same-directory temporary file, flush it, then replace the destination, so a process interrupted during serialization does not leave a half-written destination on macOS. Request and environment relocation also remove the newly written destination if cleanup of the old path fails, avoiding a silent duplicate. Import transactions snapshot canonical files and journal newly created directories, restoring files and removing only empty directories created by an uncommitted transaction. The optional `.postly/history.jsonl` file is machine-local metadata and must never become a prerequisite for opening a project. The native GUI's optional `.postly/recovery.json` is a bounded, private multi-document draft snapshot; dirty tabs are restored as new unsaved requests and are never canonical workspace data. The current format reads the previous single-draft version so an upgrade does not discard an existing recovery file.

The native GUI keeps saved request-tab paths and the active tab in the ignored
`.postly/gui-tabs.json` preference file. Tabs are presentation state only: a
tab points to a canonical request file, while an unsaved draft remains in
memory and is covered by the separate recovery snapshot.

The GUI persists its appearance preference alongside transport settings in
`.postly/gui-settings.json`. Dark, light and system mode are mapped to egui's
theme preference; the interface uses the active palette for custom panels and
secondary text so light mode remains readable.

The CLI mock server is a read-only runtime over the same canonical files. It
loads saved response examples, derives routes from each request method and URL
path, and never writes to the workspace while serving. Mock-only delay metadata
is stored as a Postly-native extension and is preserved when exporting to
Postman JSON.

## Runtime flow

~~~text
CLI or native UI
  -> VariableContext
  -> Request model
  -> HttpEngine
  -> HttpResponse
  -> renderer / reporter
~~~

The HTTP engine uses a bounded timeout and redirect policy, plus a cookie jar shared by cloned engine handles. Saved workspaces can opt into a bounded ignored JSON cookie file; unsaved callers keep an in-memory jar. Insecure certificate acceptance is an explicit option rather than a default. HTTPS accepts custom PEM roots and PEM client identities through Rustls; `.p12`/`.pfx` client identities use the native TLS backend and receive a transient passphrase only in memory. The native GUI can select exact-host or wildcard certificate associations after URL variable resolution for HTTP, SSE, WebSocket and gRPC flows; paths persist locally while passphrases remain session-only. Variable diagnostics are surfaced before network I/O.

## Next decisions

The embedded scripting runtime, OS secret storage and advanced protocol crates remain open until small prototypes and cross-platform validation produce evidence. The first GUI decision is recorded in ADR-0002; the current opt-in script boundary and its limitations are recorded in ADR-0003.

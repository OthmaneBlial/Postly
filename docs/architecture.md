# Postly architecture

## Current boundary

The repository is a Cargo workspace with four packages:

- postly-core: durable models, variable resolution, filesystem storage, Postman import, HTTP execution and runner orchestration.
- postly: the CLI, intentionally thin over the core.
- postly-xtask: local formatting, linting and test orchestration.
- postly-app: native desktop presentation and asynchronous interaction with the core.

The core is the product boundary. A future desktop UI must call the same request, persistence and runner services instead of reimplementing request behavior in a frontend.

## Canonical files

Project-critical data is ordinary TOML:

~~~text
postly.toml
collections/<collection>/postly.collection.toml
collections/<collection>/requests/<folder>/<request>.postly.toml
environments/<environment>.postly-env.toml
~~~

Request files carry stable UUIDs, readable names and all supported request semantics. Filesystem paths currently provide deterministic ordering; explicit ordering metadata can be added later if the UI needs drag-and-drop ordering. Canonical TOML writes use a same-directory temporary file followed by replacement, so a process interrupted during serialization does not leave a half-written destination on macOS. The optional `.postly/history.jsonl` file is machine-local metadata and must never become a prerequisite for opening a project. The native GUI's optional `.postly/recovery.json` is a bounded, private draft snapshot; it is restored as a new unsaved request and is never canonical workspace data.

## Runtime flow

~~~text
CLI or native UI
  -> VariableContext
  -> Request model
  -> HttpEngine
  -> HttpResponse
  -> renderer / reporter
~~~

The HTTP engine uses a bounded timeout and redirect policy, plus a cookie jar shared by cloned engine handles. Saved workspaces can opt into a bounded ignored JSON cookie file; unsaved callers keep an in-memory jar. Insecure certificate acceptance is an explicit option rather than a default. Variable diagnostics are surfaced before network I/O.

## Next decisions

The embedded scripting runtime, OS secret storage and advanced protocol crates remain open until small prototypes and cross-platform validation produce evidence. The first GUI decision is recorded in ADR-0002; the current opt-in script boundary and its limitations are recorded in ADR-0003.

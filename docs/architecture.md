# Postly architecture

## Current boundary

The repository is a Cargo workspace with three packages:

- postly-core: durable models, variable resolution, filesystem storage, Postman import, HTTP execution and runner orchestration.
- postly: the CLI, intentionally thin over the core.
- postly-xtask: local formatting, linting and test orchestration.

The core is the product boundary. A future desktop UI must call the same request, persistence and runner services instead of reimplementing request behavior in a frontend.

## Canonical files

Project-critical data is ordinary TOML:

~~~text
postly.toml
collections/<collection>/postly.collection.toml
collections/<collection>/requests/<folder>/<request>.postly.toml
environments/<environment>.postly-env.toml
~~~

Request files carry stable UUIDs, readable names and all supported request semantics. Filesystem paths currently provide deterministic ordering; explicit ordering metadata can be added later if the UI needs drag-and-drop ordering. The optional `.postly/history.jsonl` file is machine-local metadata and must never become a prerequisite for opening a project.

## Runtime flow

~~~text
CLI or UI
  -> VariableContext
  -> Request model
  -> HttpEngine
  -> HttpResponse
  -> renderer / reporter
~~~

The HTTP engine uses a bounded timeout and redirect policy. Insecure certificate acceptance is an explicit option rather than a default. Variable diagnostics are surfaced before network I/O.

## Next decisions

The GUI framework, embedded scripting runtime, OS secret storage, history database and advanced protocol crates remain open until small prototypes and cross-platform validation produce evidence. See ADR-0001 for the initial GUI decision criteria.

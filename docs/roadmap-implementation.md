# Roadmap implementation evidence

The original audit is in [ROADMAP.md](../ROADMAP.md). This log records delivered
behavior separately from releases and external user validation.

## Native onboarding and local Orders API

- Rust 1.95.0 is pinned, matching the GUI dependency requirements.
- Starting the desktop app without a path opens a welcome screen without
  initializing the working directory. Users can open, create or try a project.
- Creating a project or starter rejects nonempty folders. Opening an existing
  project validates its manifest before initializing the native workspace.
- The starter creates two editable requests, five native assertions and a
  reusable response example. A built-in loopback-only server supplies real
  filtered Orders responses. Its lifetime belongs to the desktop session.
- `postly demo` exposes the same starter and API in the CLI, including a
  `--serve-only` mode for restarting the API without changing saved files.
- The repository includes a public Postman fixture and a step-by-step guide
  under `examples/orders/`.
- Real native inspection revealed a transparent sidebar that appeared black
  in the light theme and clipped request actions. The sidebar now uses the
  theme's panel background; secondary actions and streaming controls use menus.

Validation on the local macOS ARM64 machine with Rust 1.95.0:

- `cargo xtask check`: 256 tests passed (38 CLI, 63 GUI, 154 core, 1 xtask),
  formatting and Clippy passed.
- `cargo build --locked --workspace`: succeeded.
- Native welcome screen inspected; its Try action created a real workspace.
- CLI run against that GUI-created workspace: two HTTP 200 responses, five
  assertions passed. Separate CLI-created starter produced the same result.
- Public Postman example imported with two supported requests and no warnings;
  running that imported collection produced two HTTP 200 responses.

This is local development evidence, not a fresh-machine installation test or
external usability study. No new public binary is established by this entry.

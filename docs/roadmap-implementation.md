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

## Contribution entry points

`CONTRIBUTING.md` now describes setup, architecture, local checks and fixture
boundaries. GitHub has bug and migration form definitions plus a PR template.
Six scoped contribution candidates include source locations and acceptance
criteria in `docs/contribution-candidates.md`. YAML syntax was validated with
Ruby's YAML parser. These are prepared candidates; no issue assignments or
outside contributions are claimed.

## Desktop import and collection runner

`crates/postly-app/src/workflows.rs` adds background import and collection runs
using the existing shared core. Postman and local OpenAPI files import into a
new/empty destination; the result retains migration warnings before the user
opens that project. Existing drafts must be saved before switching.

The runner uses the selected collection and environment, supports nested-folder
selection, explicit script opt-in, fail-fast, cancellation, JSON report export
and opening a result's request in the editor. It explicitly runs saved files.

Validation:

- All 259 workspace tests passed; Clippy passed.
- New regressions cover both import formats, nonempty-destination protection
  and a real two-request run with an intentionally failed assertion.
- In the actual macOS window, the Orders runner displayed two passed requests
  and five assertions. The GUI imported the public Postman fixture, displayed
  two requests / zero warnings and opened the resulting project. The CLI then
  ran that GUI-imported project successfully with two HTTP 200 responses.

The detailed guide is [desktop workflows](desktop-workflows.md). External
usability sessions and the remaining roadmap gates are still open.

## Structural response comparison

The core now compares JSON bodies with deterministic JSON Pointer paths,
added/removed/changed values, explicit subtree exclusions and bounded input,
depth, work and result counts. The desktop can compare a received response with
a saved example or chosen JSON file. Closing the comparison discards its
in-memory baseline and result; it creates no persistent response snapshot.

Validation: 263 workspace tests passed (including four comparison regressions),
Clippy and the locked build passed. In the native GUI, changing the Orders
filter from `paid` to `pending` produced 10 structural differences against the
saved example. Excluding `/orders` left one difference at `/count` and visibly
reported one excluded subtree. This confirms the displayed comparison uses
the real received response and applies the user's explicit exclusion.

## Native release packaging candidate

Version `0.2.0-preview.1` now has native-host packaging with target-correct
executable names, explicit locked release builds, source/toolchain provenance,
recursive archive checksums and an extracted-CLI smoke run against the local
Orders API. macOS adds the existing brand icon, an app bundle and a verified
DMG. The CLI statically bundles OpenSSL instead of requiring Homebrew.

Installation, update and rollback instructions accompany the package. Platform
availability, optional Node scripting and ad-hoc/not-notarized status are
explicit. This does not create a public release or establish other OS support.

Validation: 265 tests, formatting and Clippy passed; compatibility fixtures
passed 10/10 with 27/31 request mappings. The mounted DMG's CLI passed two
requests and five assertions, and its app passed signature verification and
launched through Launch Services. Exact-bundle visual inspection remains open:
system screenshot capture failed during this session. Clean-machine testing,
signing credentials and cross-platform validation are also external gates.
See the [candidate report](release-validation-v0.2.0-preview.1.md).

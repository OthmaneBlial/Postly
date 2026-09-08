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

## Progressive module extraction

The native app keeps its touched UI areas in `welcome.rs`, `navigation.rs`,
`design.rs`, `workflows.rs` and `comparison.rs`. The CLI now routes its
workspace-oriented `init`, `list`, `validate` and `search` commands through
`workspace_commands.rs`; their output and shared `Workspace` semantics remain
unchanged. This is an incremental boundary, not a claim that the remaining
large command files have been rewritten wholesale.

After the extraction, `cargo xtask check` passed formatting, Clippy with
warnings denied and all 279 workspace tests (38 CLI, 70 GUI, 159 core and
12 xtask).

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
DMG. The CLI statically bundles OpenSSL instead of requiring Homebrew. A
cross-link on macOS also produced Linux x64 release CLI and GUI ELF binaries,
then MinGW produced Windows x64 PE32+ CLI and GUI binaries, plus Mach-O x86_64
CLI and GUI binaries. The Intel CLI passed a two-request/five-assertion demo
under Rosetta 2. A temporary Intel archive copy also passed recursive checksum,
CLI and three-second GUI replay under Rosetta. These are still cross-host
checks, not physical-machine validation or a published Intel asset.

Installation, update and rollback instructions accompany the package. Platform
availability, optional Node scripting and ad-hoc/not-notarized status are
explicit. This local evidence does not establish other OS support or a
clean-machine installation. The macOS Apple Silicon package was subsequently
published as the prerelease [v0.2.0-preview.1](https://github.com/OthmaneBlial/Postly/releases/tag/v0.2.0-preview.1).

Validation: 265 tests, formatting and Clippy passed; compatibility fixtures
passed 10/10 with 27/31 request mappings. The mounted DMG's CLI passed two
requests and five assertions, and its app passed signature verification and
launched through Launch Services. Exact-bundle visual inspection remains open:
system screenshot capture failed during this session. Clean-machine testing,
signing credentials and cross-platform validation are also external gates.
See the [candidate report](release-validation-v0.2.0-preview.1.md).

## Native visual system and response workspace

The desktop now uses bundled IBM Plex Sans, opaque theme-aware text, a graphite
dark theme and a separate light palette. New projects start dark; existing
theme preferences are preserved. Navigation and design helpers are extracted
from the main application module. Font licenses ship with the app and archives.

The response inspector uses a resizable right-hand pane when the content area
is wide enough and stacks below the editor in smaller windows. Compact response
controls and consistent JSON line heights leave more space for the actual body.
Request renaming remains available under More, exports under Export, and the
request editor scrolls independently. No protocol or transport controls were
removed. The bundled Postly logo is shared by the navigation header and runtime
window/Dock icon; the macOS ICNS uses the same geometry as the website SVG.

Regression checks cover theme text contrast (body, muted and syntax text on panel/field
backgrounds), pointer/keyboard navigation, adaptive editor reachability and the
bundled icon. These bounded checks are not a full accessibility certification.

Local validation: formatting, Clippy and all 270 workspace tests passed (38 CLI,
70 GUI, 159 core, 3 packaging). The rebuilt native app was inspected in dark and
light themes, sent a real Orders request, and displayed the full 19-line JSON
in the wide inspector. At 980 px width, the editor and stacked response remain
reachable. The Dock was captured with the custom icon after a direct binary
launch. Light JSON token colors were darkened after visual inspection.

During real demo validation, a macOS accepted-socket timing issue was fixed:
the loopback server explicitly waits for incoming bytes within its existing
timeout. A regression connects before sending request data and checks HTTP 200.

The final real-product video is included in the public prerelease and remains
served by the site. The older `v0.1.0` release does not contain this UI.

## Product site and first-run documentation

Both static pages were rebuilt around a shared editorial visual system, bundled
Plex typography and a real native screenshot. The simulated HTML app and the
nonfunctional first-run example URL were removed. The README leads with the
real UI and a loopback starter; source compilation no longer has a 60-second
promise. Site and docs distinguish source preview from the old public download.

Local browser checks: landing and documentation layouts at 1422 and 433 CSS
pixels had no horizontal overflow. Images loaded, demo/build copy buttons
returned the exact commands, and guide filtering showed matches and an empty
state correctly. Browser warning/error log was empty. `node --check
website/script.js` and `node tools/check-website.mjs` passed; the latter checks
37 local URLs, copy targets, source-document links and stylesheet font assets.
Publication and video integration are tracked separately from these checks.

## Real native demo and controlled player

The final clean-commit macOS candidate was recorded in one continuous session.
The 66-second, 1920×1080 H.264 MP4 shows real requests, structural comparison,
explicit exclusions and a two-request/five-assertion desktop run. It contains
normal-speed native footage, not a mock interface. Captions sit outside the
app image; the original recording and action log are retained locally.
See [production provenance](demo-production.md) for exact source and hashes.

The site has an inline player and a dedicated demo page with controls, chapter
buttons and a transcript. The README uses a real poster linked to the full
player, not an animated GIF or an unsupported external video tag. Validation:
complete decoding, raw scene inspection, final frame inspection, browser
playback/pause and chapter seeking. The 390 CSS-pixel mobile layout revealed
and corrected a fixed video-height issue. Full-screen requests are rejected
in the test browser; the explicit fallback message was verified instead of
claiming this mode passed. Other browsers/devices remain untested.

Publication verified: the public MP4 hash matches the local export and the
release asset, the public player plays and seeks by chapter, and GitHub renders
the README poster with its link to the full player. The code and media
milestone is `ddfd0c7`; the Pages content was published as `524d0199` in the
site's existing `master` branch. The release-status refresh was then published
as `da4cd967` in the same `Postly/` folder. Exact release URLs and hashes are recorded in
[the public-release report](measurements/2026-09-08-public-release.md).

## README discovery path

On 8 September, the README was reordered around the native demo, actual download
status, concrete benefits, the local Orders example and short current limits.
The detailed transport/protocol inventory now lives in the linked
[feature reference](features.md). The preview label is visible at the top;
the old public binary remains explicitly distinguished from the filmed build.
All 57 local Markdown targets across the edited README, feature reference and
benchmark guide resolved. This is a document/link check, not evidence that four
out of five external readers understand the product; that usability gate remains
open.

## Release measurement provenance and local gates

On 8 September, the benchmark stopped choosing an unrelated debug CLI when a
release harness was requested. It now requires the sibling CLI, records Cargo
profile/optimization/target, hashes both binaries and preserves every raw sample.
The implementation (`0823312`) passed the full 272-test local quality gate.
Both executables were then rebuilt together at that clean commit and the
release benchmark plus compatibility suite completed successfully.

The [dated report](measurements/2026-09-08-validation.md) contains measured
medians, unmodified JSON, method and limitations. Four fuzz targets also finished
256 executions each without a crash, starting from empty corpora. The limited
input sizes are explicit; this is not a security certification. GUI performance,
other OSes, independent installation and external user gates remain open.

## Seeded robustness checks and native performance fixture

The fuzz smoke now merges ten reviewed fictional seeds without overwriting
discovered inputs, exercises nested/cyclic variable contexts and runs 1,024
inputs per target with explicit process limits. All four runs completed without
a crash; the full quality gate passed 279 tests, formatting and Clippy.
Regression tests prove the seeds reach valid request/workspace parsing, not
only early syntax errors. See the [seeded report](measurements/2026-09-08-seeded-fuzz.md).

A deterministic native performance workspace generator supplies 10,000 unique
requests. The CLI validator reported no issues and the generator's no-overwrite
behavior was checked. The actual capture attempt encountered a locked macOS
session, so no GUI timing or idle-memory claim is made. The
[GUI measurement protocol](gui-performance.md) retains that gate explicitly.

## First real native performance baseline

After macOS became unlocked, the external observer completed five native runs
on the same clean-commit app used for the demo, with 10,000 fictional requests.
The [raw report and inspected frames](measurements/2026-09-08-macos-gui.md)
record startup, idle RSS, unique/broad searches, opening the correct request and
scrolling to the last result. Window registration median was 1,576.87 ms;
idle app RSS median was 234,992 KiB. Capture-based interaction numbers remain
explicit observation bounds, not exact render times or a claim of smooth FPS.
The earlier locked-session failure is superseded for this local measurement;
independent installations, other OSes and external usability remain open.

## Update preservation evidence

The old public `v0.1.0` macOS archive and the current `0.2.0-preview.1` package
were extracted into separate temporary directories. A fictional workspace and
light theme preference survived old-CLI creation, new-CLI list/search/validate
and a three-second launch of the new extracted GUI; canonical files and the
preference JSON matched byte-for-byte. See the [dated report](measurements/2026-09-08-update-preservation.md).
This is one local archive pair, not clean-machine, keychain or cross-platform
evidence.

The published archive was also replayed in a fresh temporary HOME with a
minimal system PATH: no Cargo or Node was available, the generated Orders
workspace validated and ran two HTTP 200 requests with five assertions, the
internal checksums passed, and the packaged GUI stayed alive for three seconds.
This strengthens the local end-user path without replacing an independent Mac
or a target-machine test. See the [isolated package report](measurements/2026-09-08-isolated-package.md).

The portable `tools/replay-package.sh` script now codifies that archive replay
without Cargo or Node: it checks the recursive manifest, validates the
packaged CLI, creates a random-port loopback Orders workspace, runs two HTTP
requests with five assertions, and optionally keeps the GUI alive for three
seconds. It is ready to run on a genuinely independent machine; a local run
still remains development-host evidence until that external gate is exercised.

After the icon metadata and cross-target documentation changes, `cargo xtask
package` was rerun from clean `HEAD` `892a00d`. The new Apple Silicon archive
recorded `source_dirty: false`; its extracted checksum, CLI, loopback, validation,
run and three-second GUI replay all passed. The generated artifact remains a
local candidate and does not replace the published release asset.

The Windows ZIP now has a matching no-toolchain replayer at
`tools/replay-package.ps1`. PowerShell Core 7.6.5 parsed the script locally;
the MinGW PE binaries were also staged into a temporary 82-entry ZIP with
checksums and `unzip -t` passing. A malformed-archive guard also failed with
the expected missing-`postly.exe` diagnostic under PowerShell Core. Execution
of real PE binaries remains intentionally unclaimed until a Windows machine
can run the CLI and GUI.

The latest clean candidate was rebuilt from `3786317` after the product
feedback form landed. Its manifest reports `source_dirty: false`, and the
portable macOS replay passed checksums, CLI, loopback requests, five assertions
and the three-second GUI smoke. Bundle inspection confirmed both macOS icon
metadata keys resolve to the bundled `Postly.icns`; this confirms the desktop
icon fix in the candidate without claiming a fresh-machine install.

The landing page now links directly to the structured feedback form as well as
the bug form. That one-line site update was published to the existing Pages
branch at `6d20db2`; a cache-busted HTTPS fetch returned the new link after the
normal Pages propagation delay.

The target-machine handoff is now consolidated in the [external validation
report template](measurements/external-validation-template.md). It records
checksums, clean-account isolation, native GUI/icon checks, credential-store
outcomes and explicit headless skips without turning cross-compiles into
platform claims.

## Cross-target compilation evidence

On the macOS ARM64 host, temporary Zig 0.16.0 linker wrappers allowed the
locked shared CLI dependency graph to compile for `x86_64-unknown-linux-gnu`
and `x86_64-pc-windows-gnu`. This is useful source-compatibility evidence, but
it is not a package or runtime result. A subsequent Linux release cross-link
produced both CLI and GUI ELF x64 binaries, a MinGW cross-link produced Windows
PE32+ CLI and GUI binaries, and an x86_64-apple-darwin build produced Mach-O
x86_64 binaries. The Intel CLI ran under Rosetta 2, but no physical Intel Mac
or target-native Linux/Windows runtime validation is claimed; the open gate and
exact command outcomes are recorded in
[the cross-platform report](measurements/2026-09-08-cross-platform.md).

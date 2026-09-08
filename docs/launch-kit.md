# Postly launch kit

Prepared 8 September 2026 for the `0.2.0-preview.1` preview. These are drafts,
not published messages. Before posting, re-check each community's current rules,
link policy, title limits and launch-day etiquette. Replace the release URL only
after the matching asset has been published and its checksum verified.

## Message pillars

Every draft uses one of three concrete pillars:

1. **Git-native API work** — requests and environments stay in reviewable local
   files beside the code.
2. **One project, two surfaces** — the same Rust core powers the native desktop
   workspace and the headless CLI.
3. **Migration with boundaries** — Postman/OpenAPI imports produce diagnostics;
   unsupported or script-bearing input is not silently treated as compatible.

The ask is intentionally specific: report one installation blocker, one import
that needs manual review, or one step in the example that is hard to reproduce.
Stars are not the requested outcome; useful feedback is.

## Show HN draft

**Title:** Postly — a native API workspace that keeps requests in your repo

**Body:**

I built Postly because an API request is often part of the codebase, but the
client used to author it is somewhere else. Postly stores collections and
environments as local TOML, imports Postman Collection v2.1/OpenAPI, and runs
the same saved requests from a native desktop app or a headless CLI.

The preview includes a deterministic local Orders API, so the first request
doesn't depend on an account or a public service. The 66-second recording is a
real native session: send a request, compare the response with a saved example,
exclude a volatile subtree, save the request, then run the collection and its
assertions.

Demo: `https://othmaneblial.github.io/Postly/demo.html`
Source and install notes: `https://github.com/OthmaneBlial/Postly`

This is an early macOS Apple Silicon preview. The candidate is ad-hoc signed,
not notarized; Windows/Linux/Intel assets and clean-machine validation are not
being claimed. Imported scripts are opt-in and require Node.js; they are not a
hostile-code sandbox. Compatibility is fixture-backed (27/31 request mappings
in the current report), not a 100% Postman claim.

I am the creator. If you try it, the most useful feedback is the first concrete
blocker: download/install, opening the example, importing a small collection,
or running the same request from the CLI. What would you expect to see in the
first five minutes that is missing?

**Posting checklist:** verify the release asset and checksum first; include the
real demo link; remove the draft's backticks if HN formatting makes the URL less
obvious; disclose that I built it; do not ask for stars or duplicate the post.

## Rust community draft

**Title:** Postly 0.2.0-preview.1 — Rust core shared by a native API client and CLI

**Body:**

Postly is a local-first API workspace written in Rust. The desktop app uses
egui/eframe; the CLI, importer, runner and HTTP engine share the same request
model. Collections are ordinary TOML files, which makes a request reviewable
with `git diff` and usable without a hosted workspace.

The preview's reproducible path is a loopback Orders example: open it in the
native app, receive real JSON, change a filter, compare against a saved response,
then run the collection with five assertions from the CLI. The repository keeps
compatibility fixtures, seeded parser fuzz targets, and local benchmark reports.

Demo: `https://othmaneblial.github.io/Postly/demo.html`
Code: `https://github.com/OthmaneBlial/Postly`

The current local quality gate is 279 tests with Clippy warnings denied. That
number is from this macOS development machine, not a cross-platform guarantee.
The preview is ad-hoc signed and not notarized; the fuzz smoke is bounded and
not a security certification. I am the creator. Rust-focused feedback on the
core/app boundary, diagnostics or a reproducible parser edge case is welcome.

**Posting checklist:** check the community's current self-promotion and link
rules; keep the technical details in the repository instead of pasting a sales
pitch; state the exact preview version; invite an issue with a minimal fixture,
never real credentials.

## Backend/API community draft

**Title:** A repo-native API client with a real local migration path

**Body:**

The useful part of an API client is the workflow around the request: migrate a
collection, understand what changed, inspect a real response, save the request
next to the service, and run it again in CI or a terminal. Postly puts that
workflow in local project files and gives the same workspace a native desktop
surface and a CLI.

Try the fictional Orders example first — it starts a loopback API and needs no
account or external endpoint. Then change `paid` to `pending`, compare the JSON
to a saved example, and run the two-request collection with five assertions.

Demo: `https://othmaneblial.github.io/Postly/demo.html`
Repository: `https://github.com/OthmaneBlial/Postly`

This is a preview, not a promise of full Postman parity. Import diagnostics keep
manual-review cases visible, scripts are explicit opt-in, and remote APIs still
need the network. Current downloads are macOS Apple Silicon only; the candidate
is unsigned for distribution purposes beyond ad-hoc signing and is not notarized.
I built Postly and would value one concrete report: where did the migration stop
being understandable, or which request shape should be covered by a fixture?

**Posting checklist:** verify the destination's current promotion policy and
flair; use the community's preferred link format; disclose affiliation; do not
cross-post unchanged copy; reply with fixes and reproduction links rather than
asking for stars.

## Follow-up and measurement

Use one canonical issue or discussion thread per report. The repository's
[product feedback form](https://github.com/OthmaneBlial/Postly/issues/new?template=feedback.yml)
captures the same fields without asking for an account-specific payload. Ask
testers to record:

| Step | What to record |
| --- | --- |
| Install | OS/CPU, asset name, checksum result, first error or success time |
| First response | Whether the local example opened and returned HTTP 200 |
| Migration | Source format, imported request count, warnings and first unclear step |
| Repeat use | Whether the same files were run again from the CLI at J+7 |

Do not collect tokens, request bodies from private services or hidden telemetry.
Separate asset downloads from active users and report the date with any GitHub
counter. Respond to reproducible issues, add a fixture where safe, and publish a
small correction/demo only after the fix is validated locally.

## Suggested sequence

1. Publish the matching preview release and verify its public hashes.
2. Post one audience-specific draft, then watch for concrete setup/import issues.
3. Fix and test the first reproducible blocker before posting another draft.
4. Recheck the current rules and links before every later post; archive the
   outcome and date in the release notes.

No social post is scheduled or sent by this document.

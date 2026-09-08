# Local validation — 8 September 2026

Host: macOS 26.6, Apple Silicon `Mac14,2`. These are development-machine
observations, not independent installation, cross-platform or adoption evidence.
This benchmark report predates the final package commit; the public binary is
now `v0.2.0-preview.1`, while the measured executable provenance remains the
commit recorded below.

## Quality and compatibility

The local `cargo xtask check` gate passed formatting, Clippy with warnings denied
and **272 tests**: 38 CLI, 70 GUI, 159 core and 5 xtask/packaging. Environment:
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`.
The committed benchmark implementation is `0823312`; no GitHub Actions were
added. The two new tests cover matching-profile CLI selection and retained raw
samples/statistics.

The release harness's `compat --json` passed all 10 checked-in fixtures.
Request mapping remains 27/31, with four manual-review cases. This is not full
Postman behavioral parity.

## Release benchmark

Both programs were rebuilt together with the locked graph at clean commit
`0823312`, using Rust 1.95.0:

```bash
CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0 cargo build --locked \
  --release --target aarch64-apple-darwin -p postly -p postly-xtask
./target/aarch64-apple-darwin/release/postly-xtask bench --json
./target/aarch64-apple-darwin/release/postly-xtask compat --json
```

[Unmodified JSON output](2026-09-08-macos-arm64-release.json) contains the
five samples for each operation, executable hashes, version, target and
`source_dirty: false`. Its summary statistics were independently recalculated
from the sample arrays. The build completed before measurement; no concurrent
Cargo job was running. Other normal desktop processes were not stopped.

| Operation | Median ms | Min–max ms |
| --- | ---: | ---: |
| CLI process launch and `--help` | 8.80 | 8.66–14.78 |
| Import eight-request Postman fixture | 89.57 | 79.16–92.89 |
| Open/load 1,000 requests | 35.92 | 35.24–72.49 |
| Search 1,000 requests | 35.76 | 35.63–35.83 |
| Open/load 10,000 requests | 419.05 | 410.57–449.50 |
| Search 10,000 requests | 412.34 | 412.26–413.60 |
| Run 100 requests against local HTTP server | 13.50 | 13.40–29.76 |

CLI startup median peak RSS: **9,360 KiB**, measured with `/usr/bin/time -l`.
This is neither GUI idle memory nor startup-to-first-painted-frame time.
Process launch timing includes the measurement wrapper. Import samples include
temporary workspace creation and cleanup. Generated-workspace construction is
outside the load/search timers; caches were not flushed, and the five samples
run sequentially with no discarded warm-up. Treat this as a small local baseline,
not a cold-start or competitor comparison. Core search time does not establish
the latency of typing, selecting or scrolling in the native GUI.

## Benchmark provenance guard follow-up

The harness was exercised once with an existing sibling CLI that still reported
`postly 0.1.0`. The new version guard rejected that stale binary instead of
publishing a misleading measurement. After rebuilding `postly` and
`postly-xtask` together from clean commit `2ec7530`, the benchmark completed
with `source_dirty: false`, target `aarch64-apple-darwin` and CLI
`postly 0.2.0-preview.1` (CLI SHA-256
`2b18a0607045db7b37f605e6b355e615d28d364b98101e0d14283bdb36c852b8`).

The clean rerun medians were: CLI help **12.60 ms** (13,840 KiB peak RSS),
Postman import **79.11 ms**, open/search 1,000 requests **171.07 / 175.07 ms**,
open/search 10,000 requests **1,847.11 / 1,893.39 ms**, and the local 100-request
runner **30.81 ms**. This is a follow-up debug-profile sample, not a replacement
for the release-profile JSON above; both runs remain tied to their recorded
source and binary provenance.

## Bounded fuzz smoke

`cargo xtask fuzz` completed successfully: `cargo +nightly fuzz check`, then
four targets with `-runs=256`, all without a crash. Tooling: cargo-fuzz 0.13.2,
Rust `1.99.0-nightly (c98d0cb27 2026-08-12)`, AddressSanitizer-enabled targets.

| Target | Logged random seed | Completed executions |
| --- | ---: | ---: |
| `curl_command` | 3834637739 | 256 |
| `variables` | 3838630323 | 256 |
| `postman_import` | 3845328886 | 256 |
| `native_workspace` | 3851304469 | 256 |

This run began before the benchmark commit, with documentation/xtask edits in
progress. The fuzz targets and core sources were unchanged; the core Git tree
at both `070a674` and `0823312` is
`91e2fdd0d6d61c9dadab1b94dade5e299ff49e59`. Cargo updated the fuzz lockfile's
local core version to `0.2.0-preview.1`; no dependency versions changed.

Important limitation: all four corpora started empty. At this short run budget,
the logged maximum mutation length stayed at four bytes. This proves the local
fuzz toolchain executes, but is weak evidence for complete collection parsing.
Use meaningful public seeds and longer campaigns before drawing robustness
conclusions. Local logs and generated corpora remain ignored under
`tmp/roadmap-validation-2026-09-08/` and `fuzz/corpus/`; never add private inputs.

## Still open

GUI startup, idle memory and 10,000-request navigation measurements; seeded and
longer fuzz campaigns; clean-machine install/update/rollback; Windows/Linux/Intel
validation; signing credentials; public candidate release and matching download;
voluntary external usability sessions and launch feedback. None of those gates
is marked complete by these local numbers.

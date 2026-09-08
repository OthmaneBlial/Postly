# Benchmarks

Measured baseline: [8 September 2026, macOS ARM64 release](measurements/2026-09-08-validation.md)
with [raw samples and binary provenance](measurements/2026-09-08-macos-arm64-release.json).

Postly does not publish invented speed or memory multipliers. The repository
contains a local benchmark harness that produces measurements on the machine
where it is run:

```bash
cargo xtask bench
cargo xtask bench --json > bench-generated/local.json
```

Build both the CLI and harness together first (`cargo build --locked -p postly
-p postly-xtask`). For release measurements, explicitly use the release harness:

```bash
cargo build --locked --release -p postly -p postly-xtask
mkdir -p bench-generated
./target/release/postly-xtask bench --json > bench-generated/release.json
```

With an explicit target triple, use that triple's output directory instead.
The harness only measures the CLI beside its own executable. It refuses to
fall back to a debug binary when a matching release binary is missing. This
also supports Cargo target-directory overrides without guessing `target/`.

The command currently covers a CLI `--help` startup, an eight-request Postman
import, generated 1,000- and 10,000-request workspace open/search paths and a
deterministic 100-request local HTTP runner workload. Each operation runs five
samples and prints median, minimum and maximum duration. JSON also retains all
samples in observation order. On macOS, the startup
measurement also records the median peak resident set size reported by
`/usr/bin/time -l`; other platforms omit that optional field until a
platform-specific process measurement is added. The startup measurement
launches the already-built local CLI; it does not include a build.
Temporary benchmark workspaces are created outside the repository; only the
ignored `bench-generated/` destination may contain output.

Results are meaningful only with their context: the JSON/text output records the
current commit, OS, architecture, hardware when available, OS version, Rust
toolchain, Cargo build profile/optimization level/target, source dirty state,
measured executable paths, SHA-256 hashes and CLI version. The profile describes
the compiled harness, not whichever binary directory happens to exist. Rebuild
both programs at a clean commit before publishing a report: a workspace revision
alone does not prove a previously built binary came from that revision.
The harness also rejects a sibling CLI whose reported version differs from the
workspace version, preventing stale local binaries from entering a report.
The output intentionally does not compare
Postly to Postman, Bruno or any other client. Add a controlled competitor version
and methodology before publishing a comparison.

The benchmark suite is still a foundation. Large response rendering,
larger runner-throughput matrices and cross-platform runs remain
future additions; the HTTP engine nevertheless enforces a configurable 100 MiB
default cap for buffered response bodies so malformed or unbounded endpoints
cannot grow the process without limit.

Use the [native GUI protocol and 10,000-request fixture](gui-performance.md)
for rendered measurements. A [first macOS native baseline](measurements/2026-09-08-macos-gui.md)
now records startup, idle RSS and navigation separately from CLI/core timings.
It is not a continuous-scroll FPS, per-keystroke or cross-platform result.

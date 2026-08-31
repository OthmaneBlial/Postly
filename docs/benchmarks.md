# Benchmarks

Postly does not publish invented speed or memory multipliers. The repository
contains a local benchmark harness that produces measurements on the machine
where it is run:

```bash
cargo xtask bench
cargo xtask bench --json > bench-generated/local.json
```

The command currently covers a CLI `--help` startup, an eight-request Postman
import, generated 1,000- and 10,000-request workspace open/search paths and a
deterministic 100-request local HTTP runner workload. Each operation runs five
samples and prints median, minimum and maximum duration. On macOS, the startup
measurement also records the median peak resident set size reported by
`/usr/bin/time -l`; other platforms omit that optional field until a
platform-specific process measurement is added. The startup measurement
launches the already-built local CLI; it does not include a build.
Temporary benchmark workspaces are created outside the repository; only the
ignored `bench-generated/` destination may contain output.

Results are meaningful only with their context: commit, OS, hardware, Rust
toolchain, build mode and filesystem. The output intentionally does not compare
Postly to Postman, Bruno or any other client. Add a controlled competitor
version and methodology before publishing a comparison.

The benchmark suite is still a foundation. Idle memory, large response
rendering, larger runner-throughput matrices and cross-platform runs remain
future additions; the HTTP engine nevertheless enforces a configurable 100 MiB
default cap for buffered response bodies so malformed or unbounded endpoints
cannot grow the process without limit.

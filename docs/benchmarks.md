# Benchmarks

Postly does not publish invented speed or memory multipliers. The repository
contains a local benchmark harness that produces measurements on the machine
where it is run:

```bash
cargo xtask bench
cargo xtask bench --json > bench-generated/local.json
```

The command currently covers a seven-request Postman import and generated
1,000-request workspace open/search paths. Each operation runs five samples and
prints median, minimum and maximum duration. Temporary benchmark workspaces are
created outside the repository; only the ignored `bench-generated/` destination
may contain output.

Results are meaningful only with their context: commit, OS, hardware, Rust
toolchain, build mode and filesystem. The output intentionally does not compare
Postly to Postman, Bruno or any other client. Add a controlled competitor
version and methodology before publishing a comparison.

The benchmark suite is still a foundation. Startup, idle memory, large response
rendering, runner throughput and cross-platform runs remain future additions.

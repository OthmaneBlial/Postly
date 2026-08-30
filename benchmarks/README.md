# Postly benchmarks

This directory documents the local benchmark contract. Generated results belong
under `bench-generated/`, which is ignored and must not be committed as if they
were universal product claims.

Run the current reproducible suite from the repository root:

```bash
cargo xtask bench
cargo xtask bench --json > bench-generated/local.json
```

The suite measures five samples each for:

- importing the checked-in seven-request Postman variant fixture;
- opening a generated workspace containing 1,000 request files;
- searching that 1,000-request workspace for metadata matches.

The workspace is generated in a temporary directory and is deleted after the
run. The JSON output records the target OS and architecture, sample count,
median, minimum and maximum milliseconds. It is a measurement harness, not a
claim that Postly is faster than Postman or another client.

When publishing or comparing a result, record the Postly revision, hardware,
OS version, Rust toolchain, build profile, filesystem, competing version and
methodology alongside the output. Repeat runs after cold and warm starts where
startup or memory is being evaluated. Do not compare numbers from different
machines without labeling the difference.

Future benchmarks should add large response rendering, runner throughput,
startup, idle memory and 10 MB/100 MB response behavior before any performance
advantage is advertised.

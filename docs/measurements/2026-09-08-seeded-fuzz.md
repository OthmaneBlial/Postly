# Seeded fuzz validation — 8 September 2026

This follows the [empty-corpus baseline](2026-09-08-validation.md). Local host:
macOS 26.6, Apple Silicon `Mac14,2`; cargo-fuzz 0.13.2 and Rust
`1.99.0-nightly (c98d0cb27 2026-08-12)`. No remote workflow was used.

## What changed and passed

`cargo xtask fuzz` now prepares reviewed public seeds, checks the targets with
nightly, then runs each target with:

```text
-runs=1024 -seed=1 -rss_limit_mb=512 -timeout=5
```

All four processes exited successfully, with no crash or limit failure. The
existing generated corpora were retained and merged, not erased. Initial
counts below therefore include both the earlier discoveries and the new seeds.

| Target | Reviewed seeds | Initial corpus files | Largest initial input | Completed runs | Final logged RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| cURL | 2 | 16 | 145 bytes | 1,024 | 74 MiB |
| Variables | 2 | 7 | 89 bytes | 1,024 | 242 MiB |
| Postman import | 2 | 24 | 455 bytes | 1,024 | 83 MiB |
| Native workspace | 4 | 35 | 193 bytes | 1,024 | 167 MiB |

These RSS observations belong to AddressSanitizer/libFuzzer processes, not
the application's idle memory. The per-input timeout and campaign RSS ceiling
are test controls, not demonstrated product guarantees. A 1,024-run smoke is
still a short campaign; these results establish neither security certification,
full parser coverage nor compatibility with arbitrary user collections.

The full quality gate passed **279 tests** (38 CLI, 70 GUI, 159 core, 12 xtask),
formatting and Clippy with warnings denied. Six new xtask tests cover seed
presence, real Postman/cURL request construction, a valid TOML workspace,
repeatability, explicit missing-input failure and preservation of existing
corpus bytes. The variable fuzzer now also covers nested/cyclic context values.
The fuzz orchestration was extracted into its own module as it was changed.

Executed commands:

```bash
cargo fmt --all
cargo +nightly fmt --manifest-path fuzz/Cargo.toml
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo xtask check
./target/debug/postly-xtask fuzz
```

Full local logs: `tmp/roadmap-validation-2026-09-08/seeded-check.log` and
`tmp/roadmap-validation-2026-09-08/seeded-fuzz.log`. Generated corpora and crash
artifacts remain ignored. Source seeds are versioned in `fuzz/seeds/` and contain
only fictional data; importing them never executes scripts or sends requests.

## Separate GUI gate

The new [10,000-request GUI fixture](../gui-performance.md) passed the actual
CLI validator and a no-overwrite check. Native capture was blocked by the
locked macOS session. No GUI performance result is substituted with these
fuzz numbers or the earlier CLI/core benchmark.

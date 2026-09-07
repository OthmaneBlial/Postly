# Local fuzzing

Postly keeps fuzzing local and reproducible. There is no GitHub Actions
workflow for it, and generated targets, corpora and crash artifacts are
ignored by the repository.

The workspace contains targets for high-value parser boundaries:

- `curl_command` exercises shell-token parsing and request construction.
- `variables` exercises bounded nested variable interpolation.
- `postman_import` exercises malformed and partial Collection v2.1 documents
  through the filesystem importer.
- `native_workspace` exercises malformed manifest, collection, request and
  environment TOML through the read-only workspace validator.

Install `cargo-fuzz` and a nightly Rust toolchain once, then type-check and run
a bounded smoke pass:

~~~bash
cargo xtask fuzz
~~~

`cargo xtask fuzz` invokes `cargo +nightly fuzz` explicitly because the
libFuzzer AddressSanitizer flags are not available on stable Rust. If nightly
is not installed, the command fails with the toolchain installation hint
instead of reporting a false pass.

Before running, xtask merges the reviewed fictional inputs from
[`fuzz/seeds/`](../fuzz/seeds/) into each ignored local corpus. Content-addressed
seed names make this repeatable without overwriting previously discovered
inputs. Missing seed sets and conflicting files fail explicitly. The current
smoke budget is **1,024 executions per target**, using `-seed=1`, a 512 MiB
libFuzzer RSS limit and a five-second per-input timeout. Those are campaign
limits, not memory/time guarantees made by the product. A limit failure must
be investigated, not changed to a pass.

The native TOML seeds form a valid workspace; the cURL/Postman seeds reach
request construction in regression tests. The variable target has known,
nested, missing and cyclic values, and also resolves fuzz input as a variable
value. Targets parse/import local data; they do not send the seeded HTTP
requests or execute Postman scripts.

The fixed random seed alone does not make results identical across runs:
compiler instrumentation and the accumulated local corpus also matter. For a
controlled longer campaign, copy the reviewed seeds to a **new** temporary
corpus, supply that corpus explicitly and retain the toolchain, arguments and
log. Do not erase the existing corpus to get a cleaner-looking result.

Run a target for a longer local session:

~~~bash
cargo +nightly fuzz run curl_command --fuzz-dir fuzz
cargo +nightly fuzz run postman_import --fuzz-dir fuzz -- -max_total_time=60
cargo +nightly fuzz run native_workspace --fuzz-dir fuzz -- -max_total_time=60
~~~

Crash inputs are written under `fuzz/artifacts/`; preserve a minimized input
as a regression fixture only after checking that it contains no credentials or
customer data. Fuzzing is a robustness signal, not proof of semantic
compatibility or a security sandbox.

The [8 September local smoke report](measurements/2026-09-08-validation.md)
records all four target outcomes and seeds, including the limitations of a
original 256-execution run from empty corpora. It is a historical baseline;
subsequent seeded results should be recorded separately.

The [seeded follow-up](measurements/2026-09-08-seeded-fuzz.md) records all four
1,024-run outcomes, initial corpus sizes and their bounded interpretation.

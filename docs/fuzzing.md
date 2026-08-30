# Local fuzzing

Postly keeps fuzzing local and reproducible. There is no GitHub Actions
workflow for it, and generated targets, corpora and crash artifacts are
ignored by the repository.

The workspace contains targets for high-value parser boundaries:

- `curl_command` exercises shell-token parsing and request construction.
- `variables` exercises bounded nested variable interpolation.
- `postman_import` exercises malformed and partial Collection v2.1 documents
  through the filesystem importer.

Install `cargo-fuzz` and a nightly Rust toolchain once, then type-check and run
a bounded smoke pass:

~~~bash
cargo xtask fuzz
~~~

`cargo xtask fuzz` invokes `cargo +nightly fuzz` explicitly because the
libFuzzer AddressSanitizer flags are not available on stable Rust. If nightly
is not installed, the command fails with the toolchain installation hint
instead of reporting a false pass.

Run a target for a longer local session:

~~~bash
cargo fuzz run curl_command --fuzz-dir fuzz
cargo fuzz run postman_import --fuzz-dir fuzz -- -max_total_time=60
~~~

Crash inputs are written under `fuzz/artifacts/`; preserve a minimized input
as a regression fixture only after checking that it contains no credentials or
customer data. Fuzzing is a robustness signal, not proof of semantic
compatibility or a security sandbox.

# Contributing to Postly

Postly is a native API workspace and CLI with a shared Rust core. A useful
contribution makes a real request workflow easier, more reliable or easier to
understand. Reproducible bug reports and anonymized migration fixtures count.

## Set up

1. Clone the repository and install [rustup](https://rustup.rs/). The checked-in
   toolchain file selects Rust 1.95.0, rustfmt and Clippy.
2. Run `cargo build --locked --workspace`.
3. Run `cargo run -- demo ./tmp/orders-demo` in one terminal, then
   `cargo run -p postly-app -- ./tmp/orders-demo` in another. The server uses
   fictional data and only listens on loopback. Use a new folder for each example.

Node.js is optional for using the app; it is needed to exercise the scripting
bridge and its tests. Linux contributors also need the native graphics and TLS
development libraries used by eframe, OpenSSL and native-tls. If a native
dependency prevents a build, include the distribution and missing library in
your report rather than disabling certificate verification.

## Find your way around

| Area | Location |
| --- | --- |
| Request models, transport, import/export, tests | `crates/postly-core/src/` |
| Native workspace and onboarding | `crates/postly-app/src/` |
| CLI commands and reporting | `crates/postly-cli/src/main.rs` |
| Local quality gates and packaging | `crates/postly-xtask/src/main.rs` |
| Import fixtures | `compat/` |
| Runnable example | `examples/orders/` |
| Documentation and website | `docs/`, `website/` |

Start with the [contribution candidates](docs/contribution-candidates.md), then
check existing issues to avoid duplicating work. Explain the intended change
before beginning a large feature. Product priorities are in [ROADMAP.md](ROADMAP.md).

## Make and validate a change

Keep a PR focused on one problem. For a behavior change, reproduce the failure
and add the smallest useful regression test. Reuse the shared core instead of
implementing different request semantics in the GUI and CLI. When changing the
GUI, launch the actual app and include before/after screenshots with fictional
data; unit tests alone do not establish usability.

```bash
cargo fmt --all
cargo xtask check
cargo xtask compat
```

Run the relevant benchmarks, fuzz targets or package smoke checks when the
change touches those boundaries. Report the actual commands and outcomes in
your PR. The project intentionally does not use GitHub Actions: checks run
locally, and platform-specific claims need evidence from that platform.

To reduce build storage use:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo xtask check
```

## Data and compatibility

Never submit credentials, tokens, customer payloads, private endpoints or
unredacted environments. Prefer tiny collections using `127.0.0.1`, fictional
values and no scripts. The ignored `base/` directory contains research material
and must not be committed. Keep generated build output and recording rushes
out of Git.

Import success is not proof of behavioral parity. Retain unsupported cases as
visible warnings and update the relevant compatibility documentation. Security
issues should follow [SECURITY.md](SECURITY.md), not a public reproduction that
exposes confidential data.

Be direct and respectful in reviews. Discuss the code and the user's workflow;
give maintainers enough context to reproduce the result without private messages.

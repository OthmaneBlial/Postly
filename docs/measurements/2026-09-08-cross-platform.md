# Cross-platform build gate — 2026-09-08

The release host is macOS 26.6 on Apple Silicon (`aarch64-apple-darwin`). This
report records the local cross-target attempt; it is deliberately not a
Windows/Linux compatibility claim.

## Checks performed

- Rust standard libraries for `x86_64-unknown-linux-gnu` and
  `x86_64-pc-windows-gnu` are installed.
- `cargo check --locked --target x86_64-unknown-linux-gnu -p postly -p postly-xtask`
  was attempted. It stopped in `ring` because `x86_64-linux-gnu-gcc` is not
  available.
- A Clang fallback with `--target=x86_64-unknown-linux-gnu` was attempted. It
  stopped because the host has no Linux sysroot (`assert.h` was unavailable).
- No Windows linker, Windows runtime or target desktop session is available on
  this host, so no Windows package or execution was produced.
- A temporary Podman Linux VM was created to obtain a real Linux runtime, but
  `vfkit` exited with code 1 before its SSH/API became available. The VM was
  removed after the failed attempt; no Linux execution claim is made.

## Updated cross-target evidence

After the initial toolchain-only attempt, Zig 0.16.0 was installed locally and
used through temporary linker wrappers (kept outside the repository). With
`--locked`, the shared CLI and its Rust dependency graph now compile for both
targets:

- `cargo check --locked --target x86_64-unknown-linux-gnu -p postly` passed.
- `cargo check --locked --target x86_64-pc-windows-gnu -p postly` passed.

These checks include the CLI's OpenSSL and `ring` dependency build scripts, but
they are compile-only evidence from macOS. They do not prove that a Linux or
Windows executable links, launches, renders the GUI, or behaves correctly at
runtime.

The following stronger checks remain open:

- A Linux release link was attempted and failed at the final link step with
  unresolved OpenSSL symbols (`BIO_ctrl`, `SSL_CTX_ctrl`, and related symbols).
- `postly-app` and `postly-xtask` Linux checks still require a target OpenSSL
  sysroot/pkg-config setup; the host's macOS OpenSSL is not a valid substitute.
- No Windows linker/runtime or Intel Mac machine is available here.

## Consequence

The only released artifact remains the tested macOS Apple Silicon preview.
The new CLI checks narrow the remaining work to target-native linking,
packaging, and runtime validation; they do not expand the advertised platform
matrix yet.
Installing a Rust target is not equivalent to compiling, launching or testing
the application on that operating system. Closing this gate requires a Linux
x64 builder/runtime and a Windows x64 builder/runtime, followed by the
per-platform packaging, smoke and GUI checks described in `docs/install.md`.

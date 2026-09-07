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

## Consequence

The only released artifact remains the tested macOS Apple Silicon preview.
Installing a Rust target is not equivalent to compiling, launching or testing
the application on that operating system. Closing this gate requires a Linux
x64 builder/runtime and a Windows x64 builder/runtime, followed by the
per-platform packaging, smoke and GUI checks described in `docs/install.md`.

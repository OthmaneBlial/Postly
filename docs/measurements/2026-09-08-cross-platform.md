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
- At this initial stage no Windows linker, Windows runtime or target desktop
  session was available, so no Windows package or execution was produced; a
  MinGW linker was installed and exercised in the later run below.
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

The following release-link checks were then run with the same temporary Zig
wrappers and a clean target directory:

- `cargo build --locked --release --target x86_64-unknown-linux-gnu -p postly
  -p postly-app` passed. `file` identifies both outputs as stripped ELF x86-64
  executables (CLI 23 MiB, GUI 35 MiB).
- This is a macOS cross-link result only. The ELF files were not executed, put
  in a Linux package, or inspected on a Linux desktop; no Linux compatibility
  claim follows from it.
- The first Windows attempt with Zig reached the final GUI link but failed
  because that temporary linker had no `msvcrt` import library. This was a
  linker-tool limitation, not a source failure.
- A second run with Homebrew `mingw-w64` 14.0.0_3 (GCC 16.2.0, native `ar`,
  `ranlib` and `dlltool`) passed for both packages. `file` identifies
  `postly.exe` as PE32+ console x86-64 (39 MiB) and `postly-gui.exe` as PE32+
  GUI x86-64 (45 MiB). The import table contains the expected Windows system
  DLLs; no macOS libraries are linked.
- The PE files were not executed or packaged on Windows, and no Windows
  runtime, Linux runtime or Intel Mac machine is available here.
- A fresh retry with Podman 5.2.5 AppleHV (4 GiB, rootful) also left the guest
  socket and SSH endpoint refusing connections. It was stopped and removed
  after the bounded wait; this confirms the local virtualization gate is still
  unavailable rather than providing a target-runtime result.
- Rust target `x86_64-apple-darwin` was installed and a locked release build
  produced both Mach-O x86_64 binaries. `otool -L` on the CLI lists only
  macOS system frameworks and `/usr/lib/libSystem.B.dylib`.
- The Intel CLI then ran under Rosetta 2 with `--version`, `--help`, a fresh
  loopback Orders server and `run`: two requests returned HTTP 200 and all five
  assertions passed. Rosetta is an ARM-host translation check, not evidence
  from physical Intel hardware; the GUI bundle was not opened in that mode.
- For a packaging smoke, the Intel binaries were staged into a copy of the
  macOS archive, the manifest target/architecture and recursive `SHA256SUMS`
  were refreshed, and `tools/replay-package.sh` passed checksums, the loopback
  run and a three-second GUI process smoke under Rosetta. This archive was
  temporary and is not a published release asset.

## Consequence

The only released artifact remains the tested macOS Apple Silicon preview.
The CLI checks plus Linux, Windows and Intel cross-links narrow the remaining
work to target-native packaging/runtime validation. They do not expand the
advertised platform matrix yet.
Installing a Rust target is not equivalent to compiling, launching or testing
the application on that operating system. Closing this gate requires a Linux
x64 builder/runtime and a Windows x64 builder/runtime, followed by the
per-platform packaging, smoke and GUI checks described in `docs/install.md`.

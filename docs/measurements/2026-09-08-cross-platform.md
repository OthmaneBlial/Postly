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
- Those PE binaries were staged into a temporary Windows ZIP layout with a
  Windows manifest (`platform=windows`, `target=x86_64-pc-windows-gnu`), a
  refreshed recursive `SHA256SUMS`, and 82 entries. `unzip -t` passed. The ZIP
  is a local packaging candidate only; the PowerShell replayer must run on
  Windows before any asset is published.
- The PE files were not executed or packaged on Windows, and no Windows
  runtime, Linux runtime or Intel Mac machine is available here.
- A fresh retry with Podman 5.2.5 AppleHV (4 GiB, rootful) also left the guest
  socket and SSH endpoint refusing connections. It was stopped and removed
  after the bounded wait; this confirms the local virtualization gate is still
  unavailable rather than providing a target-runtime result.
- A later fresh rootless AppleHV VM (4 GiB, 20 GiB disk) reached the `vfkit`
  running state but never completed first boot: the guest emitted ARP traffic,
  while SSH and the forwarded Podman API kept refusing or resetting
  connections. The VM process was stopped after a bounded wait. This repeats
  the same host virtualization limitation; no Linux runtime result is inferred.
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
- The Windows GUI build now generates a PNG-backed ICO from
  `crates/postly-app/assets/postly-icon.png` during the build and embeds it via
  a Windows resource. The MinGW release output is identified as a PE32+ GUI
  executable with a `.rsrc` section; the generated resource is recognized as a
  512×512 PNG-backed icon. This confirms resource embedding on the macOS
  cross-build host, not Explorer rendering or a Windows runtime.
- The target-aware packager then produced
  `postly-v0.2.0-preview.1-windows-x86_64.zip` from clean `07f02d7`. Its
  manifest records `source_dirty: false` and `x86_64-pc-windows-gnu`; all 73
  ZIP entries passed `unzip -t` and the archive SHA-256 is
  `6f24425ca0dc9fd8cf0dd8b5b6b79bd7a0a8782025a03053be867dbdfd6a6d3c`.
  Runtime execution and native Explorer inspection are still required before
  publication.

## Consequence

The only released artifact remains the tested macOS Apple Silicon preview.
The CLI checks plus Linux, Windows and Intel cross-links narrow the remaining
work to target-native packaging/runtime validation. They do not expand the
advertised platform matrix yet.
Installing a Rust target is not equivalent to compiling, launching or testing
the application on that operating system. Closing this gate requires a Linux
x64 builder/runtime and a Windows x64 builder/runtime, followed by the
per-platform packaging, smoke and GUI checks described in `docs/install.md`.

## Linux desktop icon integration (source-host check)

The Linux packaging path now carries
`io.github.othmaneblial.postly.svg` together with `install-linux-desktop.sh`.
On the macOS release host, `sh -n` passed and a temporary package replay
verified that the helper installs the SVG into the user hicolor theme and
writes a `.desktop` file with `Icon=io.github.othmaneblial.postly`, an absolute
`Exec` path and a matching `Path`, including when the package directory contains
spaces. The reverse-DNS name prevents collisions with a stale system-wide
`postly` icon. This is a static/helper check only; it does not claim a Linux
desktop session, icon cache refresh or GUI runtime validation.

After the desktop icon identity fix, a clean target-aware package was rebuilt
from commit `a177319`. The manifest records `source_dirty: false` and target
`x86_64-unknown-linux-gnu`; the 24 MiB archive passed recursive checksum and
extraction checks. Its SHA-256 is
`75bcf57f481ac4efffbcb5783dbdb93055544f1dd6688fd1650b9a8b9d88db73`.
The extracted launcher uses `Icon=io.github.othmaneblial.postly` and carries
the byte-identical `io.github.othmaneblial.postly.svg` from `website/logo.svg`.
The ELF and desktop session remain unexecuted on Linux, so this is packaging
and icon-parity evidence from the macOS host only.

The macOS Apple Silicon package was then rebuilt from clean `9a7632b` after
adding the modern `CFBundleIconName` declaration. The manifest reports
`source_dirty: false`; `plutil -lint` passed and the extracted app contains
`CFBundleIconFile`, `CFBundleIconFiles` and `CFBundleIconName`, all resolving to
the bundled `Postly.icns` identity. The archive SHA-256 is
`06e8192e1b50de28d2fa75aa287c03e9ac0bf5a13c713670a7c2ac2d5514a91b` and the
DMG SHA-256 is
`127948cf73c143a9792062a952ff566ea5f14e52eaf4f29f7ae8053c12713ef2`.
The local package replay and GUI smoke passed; a separate Finder/Dock check on
an independent machine remains open.

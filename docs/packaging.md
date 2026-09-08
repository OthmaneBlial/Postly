# Local release packaging

Run on the OS and architecture you intend to distribute:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0 cargo xtask package
```

The packager also accepts an explicit Rust target when a cross-linker is
available on the host:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo xtask package --target x86_64-pc-windows-gnu
```

The target triple controls executable suffixes, archive format, manifest
platform/architecture and target-specific assets. A cross-target package still
verifies extraction and recursive checksums, but deliberately skips executing
the target binaries; run `tools/replay-package.ps1` or
`tools/replay-package.sh` on the native target before calling it tested.

Packaging builds the CLI and GUI with the locked graph and the toolchain's
explicit host target. It does not upload a release. OpenSSL is statically built
from the vendored dependency for the CLI; building it requires a C compiler,
make and Perl. Users of the resulting macOS binaries do not need Homebrew.

The command writes generated artifacts under ignored `dist/`:

- macOS: a `.tar.gz` and DMG containing an icon-bearing `Postly.app` with both
  binaries, its bundle metadata and license resources;
- Windows: a ZIP using `.exe` names throughout packaging and smoke checks, with
  the canonical Postly icon embedded in the GUI executable;
- Linux: a `.tar.gz`, including an installable desktop entry, the canonical SVG
  icon and `install-linux-desktop.sh` for a rootless per-user integration;
- per-target `*-SHA256SUMS` and `*-manifest.json` files, with version, exact
  source commit, dirty status, target triple, toolchain and signing status.

The archives contain their own recursive checksum manifest, installation guide,
documentation, MIT/OpenSSL licenses and public Orders example. Packaging extracts the archive,
checks every entry against the manifest and runs the extracted CLI. macOS also
checks linked libraries, verifies ad-hoc signatures, creates and verifies the
DMG. The app opens onboarding when launched from Finder.

Generated files for the same version/target are replaced by a later successful
package run. Source files and other versions in `dist/` are not deleted. Staging
directories are temporary and discarded when packaging returns.

Before publication, commit all source changes and rerun packaging. A manifest
with `source_dirty: true` is a development artifact, not release provenance.
Combine the per-target checksum lists into the release's public `SHA256SUMS`
only after collecting the final, independently tested assets.

## Gates that packaging does not establish

Ad-hoc signing is not Apple Developer ID signing or notarization. Windows
packages are unsigned. Do not describe cross-platform installation, external
usability, keychain behavior or an update/rollback as tested without checking
them on the relevant machine. Native-host packaging support is not a substitute
for release assets or end-user validation.

This repository intentionally has no GitHub Actions. Use the local quality,
compatibility, benchmark and fuzz commands before making a release. Record OS,
architecture, artifact hash and actual outcomes in a release validation report.
The installation and rollback guide is [install.md](install.md).

The exact 8 September archive-pair result and hashes are in the
[preservation report](measurements/2026-09-08-update-preservation.md).

The Linux helper installs the packaged `postly.svg` into the user's hicolor
theme and writes a launcher with an absolute `Exec` path, so launching from a
desktop menu uses the same icon and binary as the extracted archive. It does
not require root and never touches a workspace. Validate its shell syntax with
`sh -n tools/install-linux-desktop.sh`; running it is a user-scoped filesystem
change and is intentionally separate from the archive smoke test.

The no-toolchain archive replay is available as `tools/replay-package.sh` for
tar archives and `tools/replay-package.ps1` for Windows ZIPs. Both verify the
embedded checksums, create the loopback Orders example, and perform a bounded
GUI process smoke test. Use the matching script on each target machine before
treating an archive as tested; each headless escape hatch is explicit and does
not establish desktop startup.

The update-preservation script in that guide exercises a real old/new archive
pair on fictional temporary data. It checks the files and a GUI theme preference
before and after read-only CLI operations and a new-GUI launch; it does not claim
that every future schema, secret-store migration or operating-system installer
will preserve state.

The macOS app bundle declares `Postly.icns` explicitly in both Finder metadata
keys (`CFBundleIconFile` and `CFBundleIconFiles`). This gives Launch Services a
stable path to the same asset used by the native window and website instead of
relying on a default icon. The ICNS is rasterized from that shared geometry. To
regenerate all sizes:

```bash
# Choose a fresh output directory; the renderer refuses to overwrite PNGs.
swift tools/render-macos-icon.swift ./tmp/Postly.iconset
iconutil -c icns ./tmp/Postly.iconset -o packaging/Postly.icns
```

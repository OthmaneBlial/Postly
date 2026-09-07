# Local release packaging

Run on the OS and architecture you intend to distribute:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0 cargo xtask package
```

Packaging builds the CLI and GUI with the locked graph and the toolchain's
explicit host target. It does not upload a release. OpenSSL is statically built
from the vendored dependency for the CLI; building it requires a C compiler,
make and Perl. Users of the resulting macOS binaries do not need Homebrew.

The command writes generated artifacts under ignored `dist/`:

- macOS: a `.tar.gz` and DMG containing an icon-bearing `Postly.app` with both
  binaries, its bundle metadata and license resources;
- Windows: a ZIP using `.exe` names throughout packaging and smoke checks;
- Linux: a `.tar.gz`, including an installable desktop entry and SVG icon;
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

The update-preservation script in that guide exercises a real old/new archive
pair on fictional temporary data. It checks the files and a GUI theme preference
before and after read-only CLI operations and a new-GUI launch; it does not claim
that every future schema, secret-store migration or operating-system installer
will preserve state.

The macOS icon is rasterized from the existing logo geometry. To regenerate:

```bash
# Choose a fresh output directory; the renderer refuses to overwrite PNGs.
swift tools/render-macos-icon.swift ./tmp/Postly.iconset
iconutil -c icns ./tmp/Postly.iconset -o packaging/Postly.icns
```

# Update and rollback preservation check — 8 September 2026

This is a local macOS Apple Silicon check using fictional data. It does not
replace a clean-machine installer test or prove migration for every future
schema.

## Inputs

- Previous public archive: `postly-v0.1.0-macos-aarch64.tar.gz`, downloaded
  from the GitHub `v0.1.0` release. SHA-256:
  `dbed9a64a45d0087d369f774363e41b1b7bc42e81a5709999ff52971ed317175`.
- New candidate archive: `postly-v0.2.0-preview.1-macos-aarch64.tar.gz`,
  built from the clean release commit recorded in its bundled
  `postly-package.json`. Its final SHA-256 is recorded by the release's
  `SHA256SUMS` asset and the GitHub release notes; the checked-in package
  manifest is the provenance authority for the exact source commit.
- Host: macOS 26.6, Apple Silicon `Mac14,2`, Rust 1.95.0.

## Procedure

[`tools/verify-update-preservation.sh`](../../tools/verify-update-preservation.sh)
extracts both archives into private temporary directories. The old CLI creates
a new Postly workspace and one Health request. The check adds a fictional
light-theme `.postly/gui-settings.json`, records SHA-256 values for every
canonical project file, then runs the old `list`, the new `list`, `search` and
read-only `validate --output-json`. It launches the new extracted GUI for three
seconds and terminates only that child. Finally it compares the canonical file
hashes and the preference hash byte-for-byte.

Command and result:

```bash
tools/verify-update-preservation.sh \
  tmp/roadmap-validation-2026-09-08/old-release/postly-v0.1.0-macos-aarch64.tar.gz \
  dist/postly-v0.2.0-preview.1-macos-aarch64.tar.gz

update preservation: PASS
workspace files and GUI preferences unchanged
```

The GUI portion ran with the macOS session unlocked. `POSTLY_SKIP_GUI=1` exists
for headless environments but was not used for this result. Runtime recovery,
tab and cookie files are intentionally excluded from the canonical-file hash;
they are session state, while collections, environments, project manifest and
the theme preference are the preservation contract. No real credentials or
customer request bodies entered the fixture.

## Interpretation

This validates one old/new macOS archive pair and a read-only launch path. It
does not test Developer ID/Gatekeeper behavior, Windows/Linux/Intel builds,
keychain migration, an automatic updater (none exists), or a rollback of a
schema that the old version cannot read. The documented rollback remains:
close Postly, restore a checksum-verified previous archive and keep a workspace
backup. A future schema change must extend this check before its release gate
is marked complete.

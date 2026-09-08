# External validation report template

Use this template on a machine that has not been used to build Postly. Copy it
to a dated file such as `2026-09-15-windows-x64.md`, fill in the observed
values, and attach only sanitized logs or screenshots. A cross-compile, a fresh
temporary `HOME` on the release host, or a translated process is useful
evidence but does not replace a target-native result.

## Identity and isolation

| Field | Observed value |
| --- | --- |
| Tester / consent to publish anonymized result | |
| Date and timezone | |
| Postly release and asset filename | |
| Download URL | |
| Expected SHA-256 / checksum file | |
| Machine OS and version | |
| CPU architecture | |
| Clean user account or machine description | |
| Rust/Cargo present before test? | `yes` / `no` |
| Node.js present before test? | `yes` / `no` |

Do not include usernames, home-directory paths, tokens, private URLs or private
request/response bodies. Use the fictional Orders example for the first run.

## Archive and CLI replay

Record the exact output, replacing paths and hostnames with placeholders before
sharing it.

### macOS or Linux tar archive

```bash
shasum -a 256 postly-*.tar.gz       # macOS
sha256sum postly-*.tar.gz           # Linux
tools/replay-package.sh /path/to/postly-*.tar.gz
```

### Windows ZIP archive

```powershell
Get-FileHash -Algorithm SHA256 .\postly-*.zip
.\tools\replay-package.ps1 .\postly-*.zip
```

| Check | Result / evidence link |
| --- | --- |
| Public checksum matches | |
| Embedded `SHA256SUMS` passes | |
| `postly --version` and `--help` | |
| Loopback Orders API starts | |
| Two requests return HTTP 200 | |
| Five assertions pass | |
| Replayer exits successfully | |

The headless `-SkipGui`/`POSTLY_SKIP_GUI=1` escape hatch is allowed only when
the machine has no graphical session. It must be marked as a skipped GUI test,
not as a successful desktop validation.

## Native desktop checklist

Run the packaged GUI from the extracted archive or installed app, not a Cargo
build. Record the actual asset and executable path used.

| Check | Result / evidence link |
| --- | --- |
| GUI opens without a terminal or development toolchain | |
| Window title and native layout appear | |
| Postly icon matches the in-app/site logo | |
| Try the Orders example creates a new workspace | |
| Health request returns `status: ok` | |
| List orders returns two fictional paid orders | |
| `pending` filter returns one order | |
| Save and reopen preserve the request | |
| Credential-store behavior is understood for this OS | |
| No unexpected firewall, library or permission error | |

For update/rollback evidence, use a disposable copy of a workspace and record
the old and new archive names. Confirm the files and preferences after each
step; never use a production workspace for this check.

## Platform-specific notes

- **macOS Apple Silicon / Intel:** record `uname -m`, Finder launch, Gatekeeper
  outcome and whether the app was signed or notarized. Rosetta execution must
  be labeled translation, not physical Intel evidence.
- **Windows x64:** record the Windows build, ZIP extraction path (including a
  path with spaces), SmartScreen result, `.exe` names, GUI launch and the
  Windows credential-store outcome. Do not run the script from WSL as a
  substitute for Windows.
- **Linux x64:** record distribution/version, desktop session, graphics/TLS
  libraries, `.desktop` installation result and keyring behavior. Include the
  exact missing-library diagnostic if launch fails.

## Triage and sign-off

If a check fails, keep the archive and sanitized command output, describe the
first failing step, and open a bug or feedback report. Do not mark a platform
as supported because only the CLI passed. A platform gate is complete only when
the target-native archive, CLI replay and GUI checklist all have evidence.

| Gate | Status | Maintainer notes / follow-up commit |
| --- | --- | --- |
| Independent clean-machine install and replay | `open` / `passed` | |
| Native Windows validation | `open` / `passed` / `not applicable` | |
| Native Linux validation | `open` / `passed` / `not applicable` | |
| Physical Intel macOS validation | `open` / `passed` / `not applicable` | |

# Install Postly Preview

Use an asset from the [GitHub Releases page](https://github.com/OthmaneBlial/Postly/releases)
that matches your OS and CPU. These instructions describe the `0.2.0-preview.1`
packaging; the old `v0.1.0` archive does not include the new desktop workflows.

## macOS Apple Silicon

1. Download the macOS ARM64 `.dmg` and the matching `SHA256SUMS` file.
2. From the download folder, verify its SHA-256 against that file:
   `shasum -a 256 postly-v0.2.0-preview.1-macos-aarch64.dmg`.
3. Open the DMG and drag **Postly.app** to **Applications**.
4. Open Postly from Applications and choose **Try the example**, **Create
   project** or **Open project**. Choose an empty folder for a new example.

This preview is ad-hoc signed, **not Developer ID signed or notarized**. macOS
may block a downloaded app. After verifying its source and checksum, use the
per-app **Open Anyway** option in System Settings → Privacy & Security if you
choose to run it. Do not disable Gatekeeper globally. Some managed machines
will disallow unnotarized apps entirely.

The DMG contains both the desktop app and CLI. Run the bundled CLI with:

```bash
/Applications/Postly.app/Contents/MacOS/postly --help
/Applications/Postly.app/Contents/MacOS/postly demo ./orders-demo
```

The `.tar.gz` alternative contains standalone `postly` and `postly-gui`
executables as well as `Postly.app`. Extract it and check the internal
`SHA256SUMS` from inside the extracted folder using `shasum -a 256 -c SHA256SUMS`.
Run `./postly --help` or open `Postly.app`. OpenSSL is bundled statically in
the CLI; Homebrew is not an end-user requirement.

## Windows and Linux

Only download these platforms when a matching asset is actually listed in the
release. A build script supporting an OS does not mean a tested download exists.

Windows packaging creates a ZIP containing `postly.exe` and `postly-gui.exe`.
The GUI executable embeds the same Postly icon used by the native window, so
Explorer shortcuts do not fall back to a generic application icon. Extract it
into a writable folder and run `postly-gui.exe`. Verify a downloaded archive
using PowerShell `Get-FileHash -Algorithm SHA256 PATH` and compare with the
release checksum. Unsigned previews may trigger SmartScreen or organization
policy. A working OS credential store is needed for secret environments.

Linux packaging creates a `.tar.gz` with the two executables, an SVG icon, a
desktop entry and a user-install helper. Extract, verify with
`sha256sum -c SHA256SUMS`, and launch `./postly-gui`. Native graphics libraries
and an available graphical session are required; use the CLI for headless work.
To install a launcher for the extracted app without root, run:

```bash
./install-linux-desktop.sh /path/to/postly-v0.2.0-preview.1-linux-x86_64
```

The helper copies the same `postly.svg` used by the package into the user's
`hicolor` icon theme and writes a `.desktop` entry whose `Exec` points to that
exact extracted GUI path. It does not copy or delete project data. Remove the
user launcher and icon manually to uninstall this integration; the package
directory and your workspaces remain untouched.
Distribution-specific dependency and keyring behavior requires testing on that
distribution before it is claimed supported.

Node.js is optional. It is required only when explicitly enabling the Postman
script bridge. The native demo, requests and assertions do not require Node.

## Updating and going back

Quit Postly before replacing the app or extracted executables. Keep your API
project directories separately from the installation folder: requests and
preferences live in those directories, not in the app bundle. Back up or commit
your request files before testing a preview; local environments and `.postly/`
state should remain private.

There is no automatic updater or silent workspace migration in this preview.
To roll back, close Postly and reinstall a previously downloaded, checksum-verified
version. Restore a workspace backup if the older version cannot read fields
introduced later. Preview compatibility with older versions is not guaranteed;
do not overwrite your only workspace copy to test a rollback.

To uninstall, remove the app/extracted executables. Your project folders remain.
Secrets stored in the OS credential store are separate from the installation;
do not assume removing the binary erases them.

### Verify an update before using it on a real project

The repository includes a local preservation check for a previous and a new
macOS archive. It creates only fictional data in a temporary directory, records
every workspace-file hash and a light-theme GUI preference, runs read-only list,
search and validation commands with both CLIs, launches the new GUI child, then
confirms the files and preference JSON are byte-for-byte unchanged:

```bash
tools/verify-update-preservation.sh \
  /path/to/postly-v0.1.0-macos-aarch64.tar.gz \
  /path/to/postly-v0.2.0-preview.1-macos-aarch64.tar.gz
```

Run it with an unlocked graphical session. `POSTLY_SKIP_GUI=1` is available for
headless checks but does not validate GUI startup. The script never operates on
the current directory and terminates only the GUI process it started. Treat a
failed preservation check as a stop condition; restore from a backup before
trying another preview.

The 8 September local result is recorded in the
[update-preservation report](measurements/2026-09-08-update-preservation.md).

### Replay a package without the development toolchain

For a repeatable end-user smoke test, use the repository's portable archive
replayer. It extracts into a temporary directory, verifies every entry in the
embedded `SHA256SUMS`, checks the packaged CLI, creates the loopback Orders
example, runs its two requests and five assertions, then keeps the GUI alive
for three seconds. It does not call Cargo or Node and never touches the
current directory:

```bash
tools/replay-package.sh \
  /path/to/postly-v0.2.0-preview.1-macos-aarch64.tar.gz
```

Run it on the target machine with a graphical session. Set
`POSTLY_SKIP_GUI=1` only for a deliberately headless CLI replay; that mode
does not validate desktop startup. The script is evidence for the extracted
archive path, not a substitute for Finder/Gatekeeper checks or a separate
physical machine.

For a Windows ZIP, run the PowerShell equivalent from a normal PowerShell
session (no Cargo or Node is required):

```powershell
.\tools\replay-package.ps1 .\postly-v0.2.0-preview.1-windows-x86_64.zip
```

Use `-SkipGui` only on a deliberately headless Windows session. The script
checks every entry in `SHA256SUMS`, the packaged CLI version/help, the loopback
Orders example, its five assertions and a three-second GUI process smoke.

When an archive was cross-built on another OS, the packager intentionally omits
the executable smoke. Run this replayer on Windows before publishing the asset;
an ELF/PE link or a checksum alone is not runtime validation.

When recording a result from a machine independent of the build host, use the
[external validation report template](measurements/external-validation-template.md).
It separates target-native GUI evidence from a deliberately headless replay
and captures the OS, architecture, checksum and credential-store outcome.

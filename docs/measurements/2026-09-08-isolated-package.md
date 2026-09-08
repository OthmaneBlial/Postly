# Isolated package replay — 2026-09-08

This is an end-user-environment replay on the development Mac. It strengthens
the package evidence but is **not** an independent clean-machine result.

## Method

- Archive: `postly-v0.2.0-preview.1-macos-aarch64.tar.gz`
- Archive SHA-256: `1b78a9600c81bc7abf2fd32d2ff3a86a9299f42facda6dd2a897d8bc2ef59344`
- Extracted into a fresh temporary directory with a fresh temporary `HOME`
- Process environment reduced to `PATH=/usr/bin:/bin`, `HOME=<temporary>`,
  `TMPDIR=<temporary>` and `LANG=C`
- `cargo` and `node` were absent from that PATH

## Results

- `postly --version` returned `postly 0.2.0-preview.1`.
- `postly demo <temporary-workspace> --port 4017` created the starter and
  served the loopback health endpoint with `local=true` and `status=ok`.
- `postly validate <temporary-workspace>` reported one collection, two
  requests and zero environments with a valid workspace.
- `postly run <temporary-workspace>` returned two HTTP 200 responses and five
  passing assertions.
- The archive's complete internal `SHA256SUMS` list passed.
- The extracted `Postly.app/Contents/MacOS/postly-gui` process stayed alive for
  three seconds when launched with the same isolated environment and workspace.
- The portable `tools/replay-package.sh` was then run against the same archive
  with `PATH=/usr/bin:/bin`; its checksum, CLI, loopback, validation, run and
  three-second GUI checks passed without Cargo or Node on the PATH.
- A second headless invocation with `env -i PATH=/usr/bin:/bin TMPDIR=/tmp
  LANG=C` also passed, confirming that the CLI replay does not depend on the
  shell's inherited developer environment. The GUI was intentionally skipped
  for this invocation.

## Boundary

The kernel, display server, macOS frameworks and physical machine are the
development host. This does not prove Finder/Gatekeeper behavior for a fresh
user account, keychain integration, another Mac, or another operating system.

## Latest clean candidate replay

After the structured feedback form was added, the same package path was
rebuilt from clean commit `3786317bea3504f0f443d9d67341131d3904ae08`. The
manifest recorded `source_dirty: false`, target `aarch64-apple-darwin` and
architecture `aarch64`. The regenerated archive SHA-256 was
`1f0830032c9672b4da88401efc0a8375ea6387aca8e7609961c4f047fbb3fdd7`.

`tools/replay-package.sh` passed against that archive: recursive checksums,
CLI version/help, the loopback Orders API, workspace validation, two requests,
five assertions and the three-second GUI process smoke all passed. Extracted
bundle inspection also confirmed `CFBundleIconFile` and `CFBundleIconFiles`
both point to `Postly.icns`, which is present in `Postly.app/Contents/Resources`.
This is still a development-host replay; it does not close the independent
machine, Finder or target-OS gates above.

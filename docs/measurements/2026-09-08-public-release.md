# Public release verification — 2026-09-08

This report verifies the GitHub prerelease after publication. It does not
replace an installation on a clean machine or testing on another operating
system.

## Release identity

- Release: [v0.2.0-preview.1](https://github.com/OthmaneBlial/Postly/releases/tag/v0.2.0-preview.1)
- Channel: GitHub prerelease (`isPrerelease=true`)
- Published: 2026-09-07 22:53:36 UTC
- Target commit: `493cf8cbfac1a743bf4136e1f62ff5d622e23fe3`
- Target: `aarch64-apple-darwin`
- Public asset count: 7

## Download and hash checks

The assets were downloaded with `gh release download` into a fresh temporary
directory. The archive checksum file passed `shasum -a 256 -c`:

| Asset | SHA-256 |
| --- | --- |
| `postly-v0.2.0-preview.1-macos-aarch64.tar.gz` | `1b78a9600c81bc7abf2fd32d2ff3a86a9299f42facda6dd2a897d8bc2ef59344` |
| `postly-v0.2.0-preview.1-macos-aarch64.dmg` | `cc95489f01aeb01551f5d3f55240fe37071c2279a9e4f46fa1e69863e02563b3` |
| `postly-demo.mp4` | `b4ef8c9a8705429026eb664c1ffabadece4cfc72ac6675dd6261406b63749c26` |

The public MP4 was fetched directly from the release URL and decoded with
FFmpeg. It is H.264, 1920×1080, 30 fps, 66 seconds and 1,262,689 bytes. Its
hash matches the source export and the copy served by GitHub Pages.

The published manifest records `source_dirty=false`, release profile,
Rust 1.95.0 and `signing: ad-hoc; not notarized`. The release notes repeat the
platform scope, optional Node script boundary and the absence of Windows,
Linux and Intel macOS assets.

## Remaining gates

- A clean-machine install and replay before publication were not performed.
- The public URL/hash check above is complete; it does not imply Gatekeeper,
  keychain or cross-platform validation.
- Developer ID signing/notarization still requires certificates.
- External tester feedback and a follow-up corrective release remain open.

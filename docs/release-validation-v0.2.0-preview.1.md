# Preview candidate validation — 0.2.0-preview.1

Local checks on 7 September 2026, macOS 26.6, Apple Silicon (`Mac14,2`),
Rust 1.95.0. This is not an independent clean-machine installation report.
The candidate remains unpublished until its release gates are addressed.

## Packaging and runtime evidence

- `cargo xtask check` passed formatting, Clippy with warnings denied and all
  265 tests (38 CLI, 66 GUI, 158 core, 3 packaging/xtask).
- `cargo xtask package` built the explicit `aarch64-apple-darwin` target with
  the locked dependency graph and the optimized release profile.
- The tar archive was extracted and every internal manifest entry checked.
  Its extracted CLI passed two real loopback HTTP requests and five assertions.
- `hdiutil verify` passed for the generated DMG. The image mounted read-only;
  `codesign --verify --deep --strict` passed on its `Postly.app`.
- The CLI inside the mounted app reported `postly 0.2.0-preview.1`, created a
  fresh Orders workspace and passed both requests and all five assertions.
- Both binaries' linked libraries were checked during packaging. Only macOS
  system libraries/frameworks were present; the CLI no longer links to a
  Homebrew OpenSSL location. OpenSSL's license accompanies the binaries.
- Launch Services opened the mounted app without a workspace argument. The
  process remained running and macOS reported its titled native window.
  Screenshot capture failed in this session despite reported capture access;
  rendered inspection of this exact release bundle is therefore **pending**.
- Compatibility fixture execution passed 10/10. Request mapping was 27/31;
  four manual-review cases remain, not full behavioral compatibility.

The initial packaging pass used uncommitted source and correctly recorded
`source_dirty: true`. Before publication, rebuild from a clean commit and use
the final `*-manifest.json` and `*-SHA256SUMS`; development artifact hashes are
not release checksums. Generated binaries remain in ignored `dist/`, not Git.

## Rebuilt native-design candidate

The replacement candidate was built from clean commit
`ff6732be39597ae13623d6b0bdcd7cd5a9e059ca` after the native UI and runtime icon
changes. Its manifest records `source_dirty: false`. The full local gate now
passes **270 tests** (38 CLI, 70 GUI, 159 core, 3 packaging), formatting and
Clippy. Font license files accompany both the archive and app resources.

The rebuilt DMG mounted read-only and passed strict deep signature validation.
Launch Services opened this exact bundle on the Orders workspace. Its new
native UI was captured successfully, resolving the earlier local screenshot
limitation. The CLI inside this bundle again passed two real requests and five
assertions. This remains testing on the development Mac, not an independent
installation test.

Final local candidate SHA-256 values:

```text
5923b577e298d076977b2af136e5dc8668d314d12ab182edd9a6d5349f4f92ea  postly-v0.2.0-preview.1-macos-aarch64.tar.gz
6e4bbdc9fcae0c2d54988036c16f97375b46434f8ff8c9fa5aea4853ea2b3c0b  postly-v0.2.0-preview.1-macos-aarch64.dmg
```

## Open gates

- Public asset/checksum verification after release approval.
- Actual Finder installation, update
  and rollback on an independent machine without a development environment.
- Developer ID signing and notarization: no usable signing identity was
  available locally. Ad-hoc signing must not be described as notarization.
- Windows, Linux and Intel macOS native builds and real-machine validation.
- GUI performance, deeper fuzz campaigns and externally observed usability
  sessions. The [8 September report](measurements/2026-09-08-validation.md)
  now supplies a release CLI/core benchmark and bounded local fuzz smoke;
  these do not replace rendered GUI or external checks.

Video evidence is tracked separately in the roadmap implementation log. No
public release, independent user test or cross-platform success is implied by
this report.

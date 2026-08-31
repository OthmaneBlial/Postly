# Local packaging

Postly has a local packaging command for producing a reviewable macOS/Linux
or Windows-targeted release directory from the current Rust toolchain. It does
not publish a release or upload artifacts:

~~~bash
CARGO_PROFILE_RELEASE_DEBUG=0 cargo xtask package
~~~

The command builds the CLI and native GUI with `--locked`, runs the packaged CLI
with `--version` and `--help`, checks the archive listing, then writes an
ignored `dist/` directory containing:

- `postly` and `postly-gui`;
- a copy of the README and MIT license;
- `postly-package.json` with the local platform and architecture;
- `SHA256SUMS` for every packaged file;
- a `.tar.gz` archive and its printed SHA-256 digest.

The result is a local artifact for smoke testing and review. The GUI binary is
packaged but still needs manual desktop launch/accessibility QA. This is not a
signed installer, notarized macOS application bundle, cross-compiled release,
registry publication or proof of end-user installation. Those remain separate
release gates.

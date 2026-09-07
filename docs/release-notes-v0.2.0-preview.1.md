# Postly 0.2.0-preview.1

A native API workspace for your repository: import a collection, inspect real
responses and run the same saved requests from the desktop or terminal.

## New in this preview

- A first-launch screen to open, create or try a project without initializing
  the process's current directory.
- An Orders example with a built-in loopback API, two editable requests, five
  native assertions and a saved response example. No Rust, Python or Node is
  needed to try it in the packaged app.
- Guided desktop import for Postman v2.1 and local OpenAPI 3.0/3.1 files, with
  migration warnings retained before opening the imported project.
- A desktop collection runner with environment selection, folder filtering,
  fail-fast, cancellation, explicit script opt-in and JSON report export.
- Structural JSON response comparison against a saved example or chosen file,
  including explicit exclusions for volatile subtrees and visible work limits.
- A native macOS app bundle and DMG, plus statically bundled OpenSSL in the CLI
  so an end user does not need a Homebrew OpenSSL installation.
- Fixes for light-theme panel backgrounds and clipped request actions.

This preview also includes the transport, authentication, import and protocol
work committed after the original v0.1.0 snapshot. See the versioned
compatibility guide for the precise supported boundaries.

## Download and try

Choose a listed asset for your actual OS and architecture. Verify its checksum
against `SHA256SUMS` before opening it. On macOS ARM64, open the DMG, drag
`Postly.app` to Applications and choose **Try the example**. Use a new or empty
folder for the example; existing folders are never overwritten by onboarding.

The macOS app also includes the CLI:

```bash
/Applications/Postly.app/Contents/MacOS/postly --help
```

The tar archive contains standalone CLI/GUI executables, the app bundle,
installation instructions, licenses, examples and internal checksums.

## Limits

This is a prerelease. macOS artifacts are ad-hoc signed, not Apple Developer ID
signed or notarized. Gatekeeper and organization policy may block them. Follow
the included installation guide; do not disable global OS security controls.

Only platforms with actual assets are available for download. Native-host
packaging code for another OS is not a tested release for that OS. Independent
clean-machine installation, broader accessibility checks and external usability
studies remain separate gates.

Imported scripts remain opt-in, require Node.js and are not claimed to be a
hostile-code sandbox. GUI OpenAPI import resolves supported local references;
explicit remote-reference workflows remain in the CLI. JSON comparison is
bounded and covers response bodies, not status/headers. Compatibility is
fixture-backed and does not imply complete Postman behavioral parity.

There is no automatic updater. Keep workspaces outside the installation folder,
back up before upgrading and consult the included rollback instructions.

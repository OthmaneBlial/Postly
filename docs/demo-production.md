# Real native product demo — 7 September 2026

[Full player and transcript](https://othmaneblial.github.io/Postly/demo.html)
· [MP4](https://othmaneblial.github.io/Postly/assets/postly-demo.mp4)

## Product and capture provenance

- Product: `0.2.0-preview.1`, clean source commit
  `ff6732be39597ae13623d6b0bdcd7cd5a9e059ca`.
- Executable: `Postly.app/Contents/MacOS/postly-gui` from the mounted candidate
  DMG, opened through macOS Launch Services. This is the native Rust app, not
  the website or a reconstructed interface.
- Machine: macOS 26.6, Apple Silicon Mac14,2, Rust 1.95.0.
- Candidate DMG SHA-256:
  `6e4bbdc9fcae0c2d54988036c16f97375b46434f8ff8c9fa5aea4853ea2b3c0b`.
- Server: the CLI from the same app bundle, running
  `postly demo --serve-only --port 3991` on loopback. The dedicated Orders
  workspace contains fictional data only. No account, credential or remote API.
- Raw capture: one continuous 65.983-second window recording, 2784×1650 pixels
  including macOS window padding. Commands: `screencapture -x -v -V66 -l WINDOW_ID -k OUTPUT.mov`.
  Mouse and text actions were driven through the real OS UI.
- Raw SHA-256:
  `a7a1601d8af8b902b5e02f649fa98d1da2b14b74eabc4dd37acc4d86c702ec85`.
- Raw recording, action timestamps, capture driver and CLI run log are retained
  locally in ignored `tmp/demo-2026-09-07/`. They are not public assets.

A first take was rejected after a physical-key shortcut closed the app during
automation. Layout-aware text entry was verified before the successful second
take. No frames from the rejected recording appear in the public video.

## What is shown

1. Send the paid-orders request and receive a real HTTP 200 JSON response.
2. Change `status` to `pending`, send, and receive one matching order.
3. Compare with the saved paid-orders example: ten structural differences.
4. Exclude `/orders`: one changed field remains at `/count`.
5. Restore `paid`, send and save the request to its TOML file.
6. Run the saved collection: two passed requests, zero failures, five assertions.
7. Return to the request and send again.

The desktop runner uses the shared CLI engine. A separate actual CLI execution
against the same workspace passed both requests and five assertions; it is
logged, but not depicted as terminal footage in this video. Import and mocks
are documented capabilities, not scenes claimed to be filmed here.

## Edit and export

The `ffmpeg-video-editor` web-optimized profile was used: H.264 High, CRF 23,
slow preset, level 4.0, yuv420p and faststart. Output is **1920×1080, 30 fps,
66 seconds, 1,262,689 bytes**, with no audio track.

Only window padding was cropped (`2560:1426:112:76`), then the whole window was
scaled proportionally into a 1920×964 area and padded to 1920×1080. Seven Plex
caption overlays occupy the added bottom strip, outside the app. There are no
scene cuts, recreated screens, response substitutions or speed changes. The
small timing values inside the app are individual requests, not benchmarks.

The local FFmpeg build lacks text/subtitle rendering filters, so caption PNGs
were rendered with AppKit and composited with FFmpeg's timed `overlay` filter.
The caption text is also supplied as WebVTT and an HTML transcript. The poster
is a frame extracted at three seconds from the final MP4, not a mockup.

Final MP4 SHA-256:
`b4ef8c9a8705429026eb664c1ffabadece4cfc72ac6675dd6261406b63749c26`.

## Verification boundaries

The raw scene contact sheet and final response/runner frames were inspected.
Full decoding and browser playback checks are separate from external usability
testing. The recording does not imply notarization, clean-machine installation,
Windows/Linux support or publication of a new binary release.

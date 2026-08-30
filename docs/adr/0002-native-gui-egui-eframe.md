# ADR 0002: Native Rust GUI with egui and eframe

## Status

Accepted for the first desktop vertical slice.

## Context

Postly needs a real desktop request workspace while keeping HTTP, persistence,
variable resolution and execution in Rust. The first GUI milestone must also
remain small enough to validate locally on constrained machines.

## Decision

Use `egui` for the immediate-mode UI and `eframe` for the native application
shell. The GUI lives in `postly-app` and calls `postly-core` directly. The
initial renderer is `glow`, with accessibility support enabled and the heavier
`wgpu` feature disabled for the local build.

The GUI owns presentation state only: selected request, editor tabs, pending
work and response view. It does not reimplement HTTP or filesystem formats.
Network work runs on a worker thread and the UI remains responsive while the
core async engine is executing.

## Alternatives considered

- Iced: a credible Rust-first option, but a larger first integration surface
  for the existing immediate request editor prototype.
- Slint: strong declarative UI potential, but introduces a separate language
  and toolchain before Postly has stable UI primitives.
- Tauri: useful for web UI packaging, but would move too much of the product
  surface toward JavaScript and weaken the Rust-first boundary.
- Native platform UI: potentially best platform integration, but too costly
  for a first cross-platform vertical slice.

## Consequences

The current GUI is a native, testable foundation rather than finished desktop
parity. Large response virtualization, richer text editing, keychain-backed
secrets, persistent/manual cookie management and advanced protocol editors remain explicit milestones.
The app must keep a rendering smoke test and continue to share all request
behavior with the CLI/core.

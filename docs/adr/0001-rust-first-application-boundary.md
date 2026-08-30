# ADR-0001: Keep application behavior in a reusable Rust core

Status: accepted for the foundation milestone

Date: 2026-08-30

## Context

Postly must be a credible Postman alternative without an account. The CLI, desktop client and future automation need identical request semantics, local persistence and migration behavior. A frontend-heavy design would make the product difficult to reuse headlessly and would weaken the Rust-first promise.

## Decision

Keep request models, variable resolution, filesystem persistence, import/export, transport execution and runner orchestration in Rust crates. The current CLI is a thin consumer. A desktop GUI will be selected after a small prototype validates editor quality, accessibility, startup, memory and large-response behavior.

The initial canonical project format is one-file-per-request TOML under a collection directory. This is readable, diffable and independent of an opaque database.

## Alternatives considered

- JavaScript/Electron-first: fastest UI iteration, but conflicts with the native/local-first positioning and makes CLI parity harder.
- Tauri with most logic in JavaScript: acceptable only when Rust remains the real product core; rejected as the initial boundary because it encourages duplicated behavior.
- Immediate commitment to Iced, Slint or egui: premature before a response editor and large payload prototype establish the actual constraints.

## Consequences

The first milestone is CLI-led rather than screenshot-led. This creates a testable product core and makes future UI work safer, but it delays visual polish until the behavior boundary is proven. GUI selection remains an evidence-based follow-up rather than an assumption.

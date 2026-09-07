# Small contribution candidates

These are prepared tasks, not claims that issues have already been opened or
assigned. Check the current source and issues before starting. Each candidate
should stay in one focused PR.

| Task | Where to start | Acceptance criterion |
| --- | --- | --- |
| Add a malformed cURL quoting fixture | `crates/postly-core/src/curl.rs` | An unterminated single/double quote produces an actionable error; valid quoted URLs remain unchanged. Check existing coverage first. |
| Improve an OpenAPI import warning | `crates/postly-core/src/openapi.rs`, `compat/openapi/` | A concrete unsupported construct names the affected operation and survives as a regression fixture. |
| Complete command palette documentation | `docs/shortcuts.md`, `CommandPaletteAction` in the GUI | Every current action and actual shortcut is documented; verify keyboard navigation in the running app. |
| Audit one light-theme response component | `crates/postly-app/src/main.rs` | Capture the same response in both themes, fix a demonstrated contrast issue, and preserve text selection and wrapping. |
| Add an example assertion exercise | `examples/orders/README.md` | A beginner can intentionally fail a status or JSON assertion and recover; all commands match the current binary. |
| Improve installation error guidance on Linux | `docs/packaging.md`, `docs/development.md` | Record a real missing-library diagnostic and the verified distribution-specific prerequisite; do not claim untested distributions work. |

For new behavioral coverage, demonstrate that the fixture exercises the stated
boundary. For documentation, test the commands you add. Do not make a test pass
by dropping the unsupported data silently or removing validation.

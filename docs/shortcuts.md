# Keyboard shortcuts

Postly keeps the common request actions close to the keyboard while preserving the
local-first workflow. On macOS use `⌘`; on Windows and Linux use `Ctrl` where a
command modifier is shown.

## Global shortcuts

| Shortcut | Action |
| --- | --- |
| `⌘K` / `Ctrl+K` | Open or close the command palette |
| `⌘N` / `Ctrl+N` | Create a new request |
| `⌘S` / `Ctrl+S` | Save the current request |
| `Esc` | Close the command palette |

## Command palette

Open the palette with `⌘K` or `Ctrl+K`, type to filter, then use `↑` and `↓` to
choose an action. Press `Enter` to run it. Available actions include:

- New request
- Save current request
- Send current request
- Cancel active operation
- Clear response
- Toggle response wrapping

The palette is intentionally local and state-based: actions call the same guarded
workspace operations as the visible GUI controls.

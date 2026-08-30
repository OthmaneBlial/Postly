# Reference projects

These repositories are shallow local research checkouts under the ignored base/ directory. No source code is copied into Postly.

| Project | License | Revision studied | Technology | Useful observations |
| --- | --- | --- | --- | --- |
| [Bruno](https://github.com/usebruno/bruno) | MIT | cb29460d223fb64c2329f1de292c4e493259fe79 | TypeScript monorepo, desktop/web clients, Electron/Tauri-related packages | Strong collection/import surface and a useful local-file workflow; Postly should learn from behavior while keeping its own format and Rust core. |
| [Yaak](https://github.com/mountain-loop/yaak) | MIT | 50cccf1d25fa2be8f78a2a8a4f34a059b4011fb8 | Rust workspace with Tauri client, HTTP, gRPC, WebSocket, SSE and TLS crates | Evidence that a Rust-owned shared core can cover multiple protocol surfaces; its crate boundaries are a useful comparison for Postly's future protocol work. |
| [Posting](https://github.com/darrenburns/posting) | Apache-2.0 | 56703a11513e8e74e681b4f859f31945b71e746f | Python/httpx/Textual terminal client | Excellent terminal-first ergonomics, OpenAPI-oriented workflow and low-chrome interaction ideas; Postly's CLI should stay scriptable even as a GUI is added. |

Research rules:

- Keep the clones ignored and shallow where possible.
- Re-check upstream licenses before consulting implementation details.
- Record behavioral or architectural lessons, not copied code.
- Treat Postman as the primary compatibility baseline; reference projects are secondary.

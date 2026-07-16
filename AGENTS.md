# telekinesis

## product

**CLI + TUI host** for the **rotary** (rx4) agent harness engine.

- ux: minimal/fast (pi-first, codex second)
- TUI built with crepuscularity-tui (ratatui-based)
- no harness reimplementation — rx4 owns the loop

## stack

- **Rust** — the entire product is Rust (no Zig)
- crepuscularity-tui (`ui/tui`) — ratatui-based TUI with hot-reloadable `shell.crepus` template
- **rx4** crate — Cargo dependency (not a submodule), the rotary harness engine
- tokio — async runtime, channels between TUI and agent loop

## commands (required quality)

```bash
cd ui/tui && cargo build
cd ui/tui && cargo run
cd ui/tui && cargo test
cd ui/tui && cargo clippy
```

## rules

- TUI uses rx4 **directly** (in-process, via tokio channels) — not IPC in the current implementation.
- new agent features land in **rotary (rx4)** first, then surface via slash commands here.
- prefer small slash commands that map to rx4 methods.
- no hard-coded api keys or telemetry.

## commits

english conventional commits, e.g. `feat(tui): expose /scope and /permissions`.

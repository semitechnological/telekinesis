# Telekinesis

**Agent meta-harness.** A multi-surface agent workspace: control coding agents from your phone, desktop GUI, or terminal TUI, and switch between devices without losing context.

Agent = Model + Harness. The model writes; the harness gives it tools, memory, loops, sandboxes, and controls. Telekinesis is the harness.

## Influences

- **[Ruflo](https://github.com/ruvnet/ruflo)** — agent meta-harness, multi-agent coordination, session trees.
- **[t3code](https://github.com/pingdotgg/t3code)** — biggest UX influence. Minimal web GUI for coding agents, typed event boundary, web + mobile + desktop from one repo.
- **[pi](https://github.com/earendil-works/pi)** — agent system influence. Event-driven agent loop, skills, extensions, pi packages, TUI sessions.
- **[Zed](https://github.com/zed-industries/zed)** — ACP server reference. Agent Client Protocol, External Agents, thread hosting.
- **[super.engineering](https://super.engineering)** — linking layer. Parallel agent orchestration across worktrees and repos.
- **[OpenCode](https://github.com/anomalyco/opencode)** / **[Crush](https://github.com/charmbracelet/crush)** / **[Zero](https://github.com/gitlawb/zero)** — provider gateway, LSP for agents, MCP, skills, hooks, plugins.
- **[Crepuscularity](https://github.com/semitechnological/crepuscularity)** — UI system. One `.crepus` syntax drives phone, GUI, TUI, web, and browser extensions.

## Stack

- **UI:** Crepuscularity (`.crepus` DSL, Rust/GPUI runtime)
- **Backend / agent runtime:** Zig
- **Agent model:** pi-style skills + extensions + event loop
- **UX model:** t3-style minimal web GUI, light and fast
- **Inter-agent protocol:** ACP (Agent Client Protocol)
- **Cross-device:** own signalling + P2P (QUIC/WebRTC)
- **Provider gateway:** self-hosted API for monetization

## Project Layout

```
telekinesis/
  src/            — Zig backend (agent runtime, net, providers, ACP)
  ui/             — Crepuscularity templates
  docs/           — Architecture and agent docs (Chinese)
  references/     — Git submodules of influence repos
  plugins/        — Pi-compatible extension shim + examples
  build.zig       — Zig build
  crepus.toml     — Crepuscularity manifest
```

## Build

```bash
zig build              # compile core
zig build run          # run demos
zig build run -- serve # start IPC server
zig build test         # run tests

# TUI (requires `zig build run -- serve` first)
cd ui/tui && cargo run

# GUI (requires `zig build run -- serve` first)
cd ui/gui && cargo run
```

## Demo Commands

```bash
zig build run -- agent      # Run agent loop demo
zig build run -- provider   # List providers
zig build run -- session    # Session CRUD demo
zig build run -- lsp        # List LSP languages
zig build run -- plugin     # Load pi extension
zig build run -- acp        # Spawn ACP child agent
zig build run -- serve      # Start IPC server
```

## Status

All 12 Zig modules scaffolded and tested. TUI and GUI both functional. Active development:

| Area | Status |
|------|--------|
| Agent loop (agent.zig) | ✅ Full lifecycle + tools |
| Provider gateway (provider.zig) | ✅ HTTP + SSE streaming |
| Session tree (session.zig) | ✅ Fork/merge/persist |
| Plugin system (plugin.zig) | ✅ Pi-compat Bun + JSONRPC |
| IPC server (ipc.zig) | ✅ 15 methods + event push |
| LSP integration (lsp.zig) | ✅ 8 language servers |
| ACP host (acp.zig) | ⚠️ Subprocess scaffolding |
| Networking (net.zig) | ⚠️ Stubs, needs QUIC |
| TUI (ui/tui) | ✅ Functional, basic |
| GUI (ui/gui) | ✅ Functional, basic |

See `AGENTS.md` for full project guide and `docs/ARCHITECTURE.zh.md` for architecture.


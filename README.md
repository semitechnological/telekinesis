# Telekinesis

A multi-surface agent workspace: control coding agents from your phone, desktop GUI, or terminal TUI, and switch between devices without losing context.

## Influences

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
  build.zig       — Zig build
  crepus.toml     — Crepuscularity manifest
```

## Build

```bash
zig build
zig build run
zig build test
```

## Status

Early scaffold. See `docs/ARCHITECTURE.zh.md` and `AGENTS.md` (Chinese) for the current plan.

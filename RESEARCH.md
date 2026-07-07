# Telekinesis Research Summary

Reference material gathered for the `semitechnological/telekinesis` project.

## References Cloned

| Repository | Location | Size | Why it matters |
|------------|----------|------|----------------|
| pingdotgg/t3code | `references/t3code` | ~260M | Biggest UX influence |
| earendil-works/pi | `references/pi` | ~23M | Agent system influence with plugins |
| zed-industries/zed | `references/zed` | ~107M | ACP server reference |
| semitechnological/crepuscularity | researched online | — | UI system for phone/GUI/TUI apps |
| super.engineering | researched online | — | Superconductor — links everything else |

## 1. t3code — UX Influence

**What it is:** A minimal web GUI for coding agents (Codex, Claude, Cursor, OpenCode). It is very early, not yet accepting contributions.

**Architecture (from `docs/architecture/overview.md`):**
- Browser: React + Vite app (`apps/web`) with WebSocket transport state machine.
- Server: Node.js WebSocket + static server (`apps/server`) wrapping `codex app-server` via JSON-RPC over stdio.
- Shared: `packages/contracts` for schemas, `packages/shared` for runtime utilities, `packages/client-runtime` for web/mobile shared code.
- Mobile: `apps/mobile` is an Expo/React Native app (`@t3tools/mobile`).
- Desktop: `apps/desktop` is an Electron-ish Vite+ desktop shell.
- Tooling: Uses `vp` (Vite+) and `bun`.

**UX takeaways for Telekinesis:**
- Simple web-first GUI that wraps existing CLI agents rather than replacing them.
- Typed push events as the boundary between runtime and UI.
- Web + mobile + desktop from one monorepo.
- Minimal, predictable behavior under load and reconnects.

**Build gate:** `vp check` and `vp run typecheck`.

## 2. pi — Agent System Influence with Plugins

**What it is:** A minimal terminal coding agent harness with a plugin/extension model.

**Packages:**
- `packages/ai`: Unified multi-provider LLM API.
- `packages/agent`: Agent runtime with tool calling and state management.
- `packages/coding-agent`: Interactive CLI/TUI coding agent.
- `packages/tui`: Minimal terminal UI framework with differential rendering.
- `packages/orchestrator`: Orchestration layer.

**Plugin model:**
- **Extensions**: TypeScript modules that register tools, commands, event handlers, and custom UI components. Auto-discovered from `~/.pi/agent/extensions/` and project-local `.pi/extensions/`.
- **Skills**: Markdown-based capability packages following the Agent Skills standard (`agentskills.io`). Auto-discovered from `~/.pi/agent/skills/` and `.pi/skills/`.
- **Pi Packages**: Bundle extensions, skills, prompts, and themes via npm or git.
- **Themes**: Hot-reloadable TUI themes.

**Agent runtime takeaways:**
- Event-driven agent loop (`agent_start`, `turn_start`, `message_start/end`, `message_update`, `tool_execution_*`, `turn_end`, `agent_end`).
- Parallel vs sequential tool execution.
- Steering and follow-up message queues.
- Session tree with forking/cloning/branching.
- Compaction on context overflow.

**Build gate:** `npm run check`.

## 3. Zed — ACP Server Reference

**What it is:** A high-performance, multiplayer code editor with a native Rust UI framework (GPUI) and an AI agent subsystem.

**Relevant AI/agent architecture:**
- **Zed Agent**: Native agent using Zed-configured LLM providers, tools, skills, instructions, MCP.
- **External Agents**: ACP-integrated agents (Claude, Codex, OpenCode, Copilot, Cursor, Pi) running as separate processes and communicating with Zed over the Agent Client Protocol (ACP).
- **Terminal Threads**: Native CLI/TUI agents running inside Zed terminals.

**ACP implementation crates:**
- `crates/acp_thread`: ACP connection, diff, terminal, mention handling.
- `crates/acp_tools`: Tool forwarding over ACP.
- `crates/agent_servers`: Agent server abstraction, including ACP and custom integrations.
- Uses `agent_client_protocol` crate for schema v1.

**ACP protocol facts:**
- ACP = Agent Client Protocol (`https://agentclientprotocol.com`).
- Registry: `https://agentclientprotocol.com/registry` / GitHub `agentclientprotocol/registry`.
- Manifest format: `agent.json` with `id`, `name`, `version`, `description`, `distribution`, `authMethods`, etc.
- Distribution types: `binary`, `npx`, `uvx`.
- Zed hosts External Agent threads in its Agent Panel and Threads Sidebar while the agent owns its runtime, auth, model, tools, and config.

**ACP server takeaways for Telekinesis:**
- Telekinesis should implement an ACP-compatible server/host so external agents can plug in.
- Keep a clear boundary: host owns the thread UI; agent owns runtime/auth/config.
- Forward tools and MCP servers over ACP.
- Support multiple agent threads side-by-side.

## 4. super.engineering — Superconductor / Linking Layer

**What it is:** A native macOS app (100% Rust, GPU-rendered) for running parallel coding agents in isolated git worktrees.

**Key features:**
- Parallel by default; unlimited agents scaled by hardware.
- Agent-agnostic: Claude Code, Codex, Gemini CLI, OpenCode, Cursor Agent, any CLI agent.
- Native Apple Silicon performance; no Electron/Tauri.
- Multiple layouts, picture-in-picture, per-workspace themes, built-in terminal multiplexer.
- Git workflow: commit, push, PR, merge from one UI.
- Cross-repo workspaces, custom commands, native notifications.

**Superconductor role in Telekinesis:**
- The thing that links everything else: multiple agents, multiple repos, phone/GUI/TUI surfaces, in one workspace.
- Inspiration for parallel agent orchestration, worktree isolation, and unified control surface.

## 5. Crepuscularity — UI System

**What it is:** A multi-backend UI framework from `semitechnological/crepuscularity`. Think React Native turned into a systems UI toolkit.

**Core idea:**
- Write UI in `.crepus` (indentation-based DSL) and compile/render it to multiple backends.
- Backends: GPUI desktop, Ratatui TUI, browser extensions (MV3), HTML/React web, native mobile (SwiftUI/Jetpack Compose via View IR), embedded panels, LVGL Pro.
- Build-time via `view!` macro or runtime with hot reload.

**Relevant crates/packages:**
- `crepuscularity` (core lib + `view!` macro)
- `crepuscularity-runtime` (runtime parser, GPUI renderer, hot reload)
- `crepuscularity-tui` (Ratatui backend)
- `crepuscularity-native` (JSON View IR for SwiftUI/Jetpack Compose)
- `crepuscularity-cli` (`crepus` commands)

**Crepuscularity role in Telekinesis:**
- The UI system that lets the same interface code drive phone apps, GUI apps, and TUI apps that link together.
- Telekinesis should probably consume `crepuscularity` as a dependency, not reimplement its own UI.

## Open Questions for Planning

1. **Primary language/runtime:** Crepuscularity is Rust-first; pi is TypeScript-first; t3code is TypeScript/Bun; Zed is Rust/GPUI; superconductor is Rust. Telekinesis likely leans Rust with a TypeScript plugin bridge, but needs a decision.
2. **Repo scope:** Is Telekinesis a monorepo containing the ACP server, agent harness, and UI shells, or a meta-project linking the existing crepuscularity repo?
3. **ACP implementation:** Use the official `agent_client_protocol` crate if available, or implement the protocol from scratch?
4. **Plugin model:** Adopt pi-style TypeScript extensions + Agent Skills, or a Rust-first plugin model, or both?
5. **Phone/GUI/TUI strategy:** Use crepuscularity-native for mobile, crepuscularity-runtime for desktop GUI, crepuscularity-tui for terminal.
6. **Distribution:** How will agents be installed? ACP Registry + local manifests + npm/git packages?
7. **Name collision:** The project name `telekinesis` already exists as an unrelated OSS project in some contexts; confirm the org is fine with it.

## Suggested README Positioning

- t3code: biggest UX influence — minimal web GUI, multi-surface, typed event boundary.
- pi: agent system influence — event-driven agent loop, plugins, skills, TUI, sessions.
- zed: ACP server reference — Agent Client Protocol, External Agents, thread hosting.
- superconductor: linking layer — parallel agents, cross-repo workspace, unified control surface.
- crepuscularity: UI system — one `.crepus` language for phone, GUI, and TUI apps.

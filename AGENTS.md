# telekinesis

## Product

**CLI + TUI host** for the **rotary (rx4)** agent harness engine.

- UX: minimal/fast (pi-first, codex second)
- TUI built with crepuscularity-tui (ratatui-based)
- No harness reimplementation — rx4 owns the loop
- **Owns pi protocol compat** (moved from rotary): JSONL v3 sessions, RPC
  over stdin/stdout, pi tool name mapping, extension protocol via QuickJS,
  capability policy, SDK surface

## Architecture

```mermaid
flowchart TD
  subgraph TK["telekinesis"]
    TUI["TUI (crepuscularity-tui)<br/>sidebar · themes · slash palette"]
    CLI["CLI<br/>login · exec (headless one-shot)"]
    Pi["pi protocol compat<br/>JSONL v3 · RPC · extensions · QuickJS"]
    Slash["slash commands<br/>/model /scope /mcp /todo /clear /cost"]
  end
  TK -->|"tokio channels — in-process"| RX4
  subgraph RX4["rx4 harness engine"]
    Loop["agent loop + streaming events"]
    Tools["tools + computer-use + MCP"]
    Prov["providers (OpenAI/Anthropic/Ollama)"]
    Sess["sessions · memory · graph memory"]
    Skills["skill engine + curator + background review"]
  end
```

## Stack

- **Rust** — the entire product is Rust
- crepuscularity-tui (`ui/tui`) — ratatui-based TUI with hot-reloadable
  `shell.crepus` template — **primary surface**
- **rx4** crate — latest version from crates.io. Default `tk` features:
  providers + builtin-tools. Opt-in: `mcp`, `search` (darash), computer-use,
  skills, graph-memory. `--features full` enables all of those.
- tokio — async runtime, channels between TUI and agent loop
- **pi protocol compat** — JSONL v3 sessions and embed SDK (dead RPC/extension
  surfaces removed)

## UI surfaces

| Surface | Path | Status | Notes |
|---|---|---|---|
| TUI | `ui/tui` | ✅ Active | Primary surface, ratatui-based, in-process rx4 |
| GUI | `ui/gui` | 🧪 Experimental | GPUI native window; embeds rx4 directly today |

## Pi protocol layer

```mermaid
flowchart TD
  subgraph Pi["pi protocol compat (telekinesis-owned)"]
    Sess["JSONL v3 sessions<br/>fork/merge, appendEntry"]
    Sdk["embed SDK<br/>create_agent_session"]
  end
  Pi -->|drives| RX4["rx4 agent loop (in-process)"]
```

## Slash command flow

```mermaid
flowchart TD
  Input["user types /command"] --> Parse["telekinesis host parser"]
  Parse --> Match{"known command?"}
  Match -->|/model| Model["set_model on rx4 Agent"]
  Match -->|/scope| Scope["apply_scope on rx4 Agent"]
  Match -->|/mcp| Mcp["list MCP tools / config help"]
  Match -->|/todo| Todo["host todo surface note"]
  Match -->|/clear| Clear["clear messages + reset cost"]
  Match -->|/cost| Cost["show cost breakdown"]
  Match -->|/help| Help["list commands"]
  Match -->|/quit /exit| Quit["exit TUI"]
  Match -->|unknown| Err["show error"]
  Model --> Agent["rx4 Agent (in-process)"]
  Scope --> Agent
```

## Commands (required quality)

```bash
cd ui/tui && cargo build
cd ui/tui && cargo run
cd ui/tui && cargo test
cd ui/tui && cargo clippy
```

## Rules

- TUI currently uses rx4 directly in-process via tokio channels. Shared host
  runtime owns future multi-surface transport; do not add surface-specific
  harness loops.
- New agent features land in **rotary (rx4)** first, then surface via slash
  commands here.
- Prefer small slash commands that map to rx4 methods.
- telekinesis owns pi protocol compat — rotary no longer carries it.
- Product layer surfaces: MCP config (`ui/tui/src/mcp_config.rs` + `/mcp`,
  `--features mcp`), approval args, OS sandbox policy — do not reimplement
  harness loop.
- Optional AVO loop is scripts-only (`scripts/adapters/`, `docs/AVO.md`);
  do not vendor avo-lite or add MCP/search to default features.
- No hard-coded API keys or telemetry.

## Commits

English Conventional Commits, e.g. `feat(tui): expose /scope and /permissions`.

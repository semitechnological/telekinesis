# telekinesis ↔ rotary (rx4)

rotary (rx4) is the **agent harness engine**. telekinesis is the
**CLI + TUI** host. telekinesis also owns the **pi protocol compat** layer
(moved out of rotary).

## Architecture

```mermaid
flowchart TD
  subgraph TK["telekinesis"]
    TUI["TUI (crepuscularity-tui)"]
    Pi["pi protocol compat<br/>JSONL v3 · RPC · extensions · QuickJS"]
  end
  TK -->|"tokio channels — in-process"| RX4
  subgraph RX4["rx4 harness engine"]
    Loop["agent loop + streaming"]
    Tools["tools + computer-use + MCP"]
    Sess["sessions · memory · graph memory"]
    Skills["skill engine + curator + background review + dream"]
  end
```

## Wire

- rx4 is consumed as a published Cargo dependency. Coordinated local work may
  temporarily use a path dependency.
- `ui/tui/src/main.rs` currently imports rx4 directly and drives the loop
  in-process via tokio channels. A shared telekinesis host runtime is the
  target boundary for additional surfaces.
- builtin tools + computer-use tools registered at startup; MCP tools from
  `~/.telekinesis/mcp.json` connected best-effort:

```rust
let mut tools = ToolRegistry::new();
register_builtin_tools(&mut tools);
rx4::computer_use::register_tools(&mut tools);
// host: connect_mcp_tools(&mut tools) — stdio + http + sse from ~/.telekinesis/mcp.json
agent.set_tools(tools);
agent.set_policy(Policy::workspace_write().with_os_sandbox(true));
let _ = agent.enable_os_sandbox();
```

## Bump harness

```bash
# path dep: rebuild against local rotary
cd ui/tui && cargo check
# crates.io (when not on path):
# cargo update -p rx4 && cargo test
```

## rx4 API used by TUI

`Agent::new`, `set_scope`, `set_model`, `set_provider`, `set_tools`,
`set_workspace_root`, `set_policy`, `enable_os_sandbox`, `subscribe`, `prompt`,
`Scope` (Coding/Research/Plan/Ask/ComputerUse), `ToolRegistry`,
`register_builtin_tools`, `computer_use::register_tools`, `McpClient` (feature `mcp`).

Events: `Rx4Event` lifecycle (AgentStart, TurnStart, MessageStart/Delta/End,
ToolCall, **ApprovalRequired** (includes `arguments`), ToolExecutionStart/End,
TurnEnd, AgentEnd, Error) delivered over a tokio channel.

Hooks: `HookRegistry` lifecycle observe (`BeforeTool`/`AfterTool`/…). Engine
hooks are currently fire-and-forget (`HookFn`); deny/modify lands when engine
ships gating — host should not invent a second permission system.

## Boundary

rotary owns reusable engine capabilities and typed events. telekinesis owns
product lifecycle, persistence, scheduling, transport, pi compatibility, and
surface presentation. Rotary modules currently containing host adapters are
migration inventory, not a reason to duplicate host behavior.

See the canonical decision record:
[telekinesis ADR-001](https://github.com/semitechnological/telekinesis/blob/main/docs/ADR-001-rotary-engine-telekinesis-host.md).

## rx4 (rotary) modules

| module | role |
|---|---|
| `agent` | event-driven loop, tool registry, streaming, parallel tool execution |
| `provider` | multi-provider openai-compatible client, websocket prewarming |
| `tools` | builtins: read/write/edit/bash/grep/find/ls; scope lists also name spawn_agent/code_intel aliases |
| `computer_use` | computer-use tools (`cu_*`, 13) via Praefectus |
| `session` | session tree (fork/merge) + store |
| `compaction` | semantic context compaction with token estimation |
| `models` | model registry with compat config and override logic |
| `skill_engine` | skill creation from experience, bayesian confidence, skill.md export |
| `background_review` | background review loop — observe turns, distill learning signals |
| `skill_curator` | skill lifecycle curator — Active→Stale→Archived, consolidation |
| `embeddings` | vector embeddings for semantic skill matching (Gemini / Ollama) |
| `graph_memory` | knowledge graph, pagerank, community detection, dream consolidation |
| `dream_scheduler` | dream cycle runner — graph consolidation capability (host schedules) |
| `model_router` | tiered routing (lite/standard/heavy/subagent), proactive monitor |
| `multiagent` | coordinator/worker/reviewer/researcher roles, event bus |
| `subagent` | subagent spawning with worktree isolation |
| `mcp` | json-rpc 2.0 over **stdio / http / sse** (`McpClient`/`McpRegistry`); host loads config + registers tools |
| `lsp` | json-rpc lsp client, diagnostics, references, definition |
| `sandbox` | OS sandbox via `Policy.enable_os_sandbox` + `Agent::enable_os_sandbox` (seatbelt/bwrap) |
| `secrets` | secret detection and redaction |
| `prompt_cache` | anthropic cache_control, cache stats tracking |
| `cost` | per-model pricing registry, session cost breakdown |
| `repomap` | pagerank-ranked symbol extraction, token-budgeted summary |
| `routing` | smart routing (simple/strong classifier) |
| `rollout` | rollout persistence, trace writer |
| `sse` | optimized sse parser |
| `marketplace` | plugin marketplace with installer and blocklist |

> pi protocol compat is **no longer in rx4** — telekinesis owns it
> (JSONL v3 sessions, RPC, extension runtime via QuickJS).

## Computer-use

Enabled via the `computer-use` Cargo feature on rx4 (`dep:praefectus`).
`rx4::computer_use::register_tools(&mut tools)` registers the 13 `cu_*`
tools through Praefectus. Native Rust, no FFI.

## MCP host config

File: `~/.telekinesis/mcp.json`

```json
{
  "servers": [
    {
      "name": "fs",
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    },
    {
      "name": "remote",
      "transport": "http",
      "url": "https://example.invalid/mcp"
    }
  ]
}
```

- `stdio` servers: host spawns via `McpClient::connect_stdio`, lists tools,
  registers `mcp__{name}__{tool}` on the agent `ToolRegistry`.
- `http` / `sse`: host connects via `McpClient::connect_http` / `connect_sse / connect_sse_get` (optional headers).
  Startup never fails if MCP is down.
- `/mcp` slash command lists connected tools or prints config help.

## Approvals

`Event::ApprovalRequired(ApprovalRequest)` carries `tool_name`, `arguments`,
`reason`, flags. TUI permission prompt and system line show **args**, not name
only. Hosts that implement `Approver` receive full `ToolCall`.

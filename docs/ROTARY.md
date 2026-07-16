# telekinesis ↔ rotary (rx4)

rotary (rx4) is the **agent harness engine**. telekinesis is the **cli + tui**.

## wire

- rx4 is a **Cargo dependency** (`rx4 = "0.3"` in `ui/tui/Cargo.toml`), not a submodule
- `ui/tui/src/main.rs` imports rx4 directly and drives the agent loop in-process via tokio
- builtin tools + computer-use tools are registered at startup:
  ```rust
  let mut tools = ToolRegistry::new();
  register_builtin_tools(&mut tools);
  rx4::computer_use::register_tools(&mut tools);
  agent.set_tools(tools);
  ```

## bump harness

```bash
cd ui/tui && cargo update -p rx4
cargo test
git add ui/tui/Cargo.lock && git commit -m "chore: bump rx4"
```

## rx4 api used by tui

`Agent::new`, `set_scope`, `set_model`, `set_provider`, `set_tools`, `set_workspace_root`,
`subscribe`, `prompt`, `Scope` (Coding/Research/Plan/Ask/ComputerUse), `ToolRegistry`,
`register_builtin_tools`, `computer_use::register_tools`.

events: `Rx4Event` lifecycle (AgentStart, TurnStart, MessageStart/Delta/End, ToolCall,
ToolExecutionStart/End, TurnEnd, AgentEnd, Error) delivered over a tokio channel.

## rx4 (rotary) modules

| module | role |
|---|---|
| `agent` | event-driven loop, tool registry, streaming, parallel tool execution |
| `provider` | multi-provider openai-compatible client, websocket prewarming |
| `tools` | built-in filesystem/shell/subagent/code_intel tools (7) |
| `computer_use` | computer-use tools (`cu_*`, 13) via rs_peekaboo |
| `session` | session tree (fork/merge) + store |
| `compaction` | semantic context compaction with token estimation |
| `models` | model registry with compat config and override logic |
| `skill_engine` | skill creation from experience, bayesian confidence, skill.md export |
| `graph_memory` | knowledge graph, pagerank, community detection, dream consolidation |
| `model_router` | tiered routing (lite/standard/heavy/subagent), proactive monitor |
| `multiagent` | coordinator/worker/reviewer/researcher roles, event bus |
| `subagent` | subagent spawning with worktree isolation |
| `mcp` | json-rpc 2.0 over stdio mcp client, tool routing |
| `lsp` | json-rpc lsp client, diagnostics, references, definition |
| `sandbox` | os-level sandbox (macos seatbelt, linux bubblewrap, userspace) |
| `secrets` | secret detection and redaction |
| `prompt_cache` | anthropic cache_control, cache stats tracking |
| `cost` | per-model pricing registry, session cost breakdown |
| `repomap` | pagerank-ranked symbol extraction, token-budgeted summary |
| `routing` | smart routing (simple/strong classifier) |
| `rollout` | rollout persistence, trace writer |
| `sse` | optimized sse parser |
| `marketplace` | plugin marketplace with installer and blocklist |
| `pi` | pi protocol compat: jsonl v3 sessions, rpc, extension runtime |

## computer-use

enabled via the `computer-use` Cargo feature on rx4 (`dep:rs_peekaboo`).
`rx4::computer_use::register_tools(&mut tools)` registers the 13 `cu_*` tools.

# telekinesis ↔ rotary (rx4)

rotary (rx4) is the **agent harness engine**. telekinesis is the **cli + tui**.

## wire

- submodule: `vendor/rotary` → https://github.com/tschk/rotary
- `build.zig` builds `rotary` module from `vendor/rotary/src/root.zig`
- `src/root.zig` re-exports the full rotary public api
- `telekinesis serve` attaches agent, tools, plugins, session store, acp host

## bump harness

```bash
cd vendor/rotary && git pull origin main && cd ../..
zig build test
git add vendor/rotary && git commit -m "chore: bump rotary"
```

## ipc methods used by tui

`state`, `prompt`, `set_model`, `tools`, `plugins`, `messages`,
`session_list|create|load|save|clear|fork|merge|tree`, `ping`,
`context`, `usage`, `set_scope`, `set_permissions`, `approve_tool`, `deny_tool`, `compact`

events: json-rpc notifications `method: "event"` with typed agent lifecycle payloads.

## rotary (rx4) modules

| module | role |
|---|---|
| `agent` | event-driven loop, tool registry, streaming, parallel tool execution |
| `provider` | multi-provider openai-compatible client, websocket prewarming |
| `tools` | built-in filesystem/shell/subagent/code_intel tools |
| `session` | session tree (fork/merge) + store |
| `compaction` | semantic context compaction with token estimation |
| `models` | model registry with compat config and override logic |
| `skill_engine` | skill creation from experience, bayesian confidence, skil.md export |
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

built when rotary is compiled with `-Dpeekaboo=true` (rs_peekaboo c abi via equilibrium).
default pure-zig rotary still registers `cu_*` tools; they error clearly until linked.

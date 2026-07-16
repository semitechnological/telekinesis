# telekinesis

**the coding agent cli + tui.** powered by the [rotary](https://github.com/tschk/rotary) (rx4) harness engine and [crepuscularity-tui](https://github.com/tschk/crepuscularity).

feel: **pi > codex > grok** — minimal, fast, typed event boundary. no duplicate harness logic; rotary owns the loop.

```
┌──────────────────────────────────┐
│  telekinesis tui (crepuscularity) │
│  sidebar · themes · autocomplete  │
│  cost tracking · context bar      │
└──────────────┬───────────────────┘
               │ json-rpc ipc (unix socket)
┌──────────────▼───────────────────┐
│  telekinesis serve                │
│   └─ rotary (rx4) harness engine  │
│       ├─ skill engine             │
│       ├─ graph memory + dream     │
│       ├─ model router (tiered)    │
│       ├─ multi-agent coordination │
│       ├─ mcp + lsp clients        │
│       └─ os sandbox               │
└──────────────────────────────────┘
```

## install / build

```bash
git submodule update --init --recursive
zig build
# tui
cd ui/tui && cargo build --release
```

## usage

```bash
# 1) harness daemon
telekinesis serve
# optional: telekinesis serve /tmp/tk.sock

# 2) tui (crepuscularity-tui)
telekinesis tui
# or: cargo run --manifest-path ui/tui/cargo.toml

# non-interactive (codex/opencode-style)
telekinesis exec "summarize this repo"
telekinesis exec --json "list all todos"

# harness probes
telekinesis scope
telekinesis provider
telekinesis version
telekinesis doctor
```

## tui features

| feature | description |
|---|---|
| sidebar (ctrl+b) | session list, tool list, plugin list |
| slash autocomplete | filtered command list as you type `/` |
| input history | up/down arrows, persisted to `~/.telekinesis/input_history.json` |
| permission prompts | y/n/always dialog when tools need approval |
| context usage bar | green/amber/red percentage of context window |
| cost tracking | running cost in status bar, `/cost` for breakdown |
| themes | auto, dark, light, dracula, nord, gruvbox, tokyo-night, catppuccin |
| streaming cursor | blinking cursor at end of streaming content |
| role colors | user=blue, assistant=green, tool=amber, system=zinc |
| tool call blocks | bordered blocks with tool name and args |
| diff blocks | green/red line coloring for file edits |
| keyboard shortcuts | ctrl+b/l/r, shift+tab, page up/down, home/end |

## tui slash commands

| command | action |
|---|---|
| `/model [name]` | show / set model |
| `/tools` | list rotary tools |
| `/plugins` | list plugins |
| `/scope name` | coding · research · plan · ask · computer_use |
| `/permissions [mode]` | full_access · read_only · workspace_write · deny_all |
| `/compact` | context compact |
| `/context` | show context usage |
| `/cost` | show cost breakdown |
| `/theme [name]` | set theme |
| `/sessions` `/new` `/save` `/load` `/fork` `/merge` `/tree` | sessions |
| `/doctor` | environment diagnostics |
| `/clear` `/help` `/quit` | util |

## rotary (rx4) features exposed

- agent loop + streaming events over ipc
- built-in tools + optional computer-use (`cu_*`, embedded rs_peekaboo)
- scopes, permissions, hooks, sessions, plugins/skills, providers
- **skill engine** — creates reusable skills from conversations, bayesian confidence tracking
- **graph memory** — knowledge graph with pagerank, community detection, dream consolidation
- **model router** — tiered routing: lite (skill/memory), standard (code gen), heavy (architecture), subagent (model-chosen)
- **multi-agent coordination** — coordinator/worker/reviewer/researcher roles with event bus
- **mcp client** — json-rpc 2.0 over stdio, tool routing with `mcp__{server}__{tool}` prefixing
- **lsp client** — diagnostics, references, definition via json-rpc
- **os sandbox** — macos seatbelt, linux bubblewrap, userspace fallback
- **prompt caching** — anthropic ephemeral cache_control, cache stats tracking
- **cost tracking** — per-model pricing registry, session cost breakdown
- **repo map** — pagerank-ranked symbol extraction, token-budgeted summary
- **secret redaction** — detects api keys, tokens, private keys before output
- project instruction files (`agents.md` etc.) loaded on `serve`

## layout

```
telekinesis/
  vendor/rotary/     git submodule — harness engine (rx4)
  src/main.zig       cli (serve, tui, exec, demos)
  src/root.zig       re-export rotary
  src/net.zig        product p2p/quic (optional, not yet wired)
  ui/tui/            crepuscularity-tui shell (ratatui + hot templates)
  ui/shell.crepus    hot-reloadable tui template
  ui/gui/            optional gpui (not yet wired)
  ui/web/            optional web (not yet wired)
  docs/              architecture docs
```

## why this split

| concern | owner |
|---|---|
| loop, tools, providers, permissions, computer-use | **rotary (rx4)** |
| cli, tui, multi-device product, branding | **telekinesis** |

inspired by t3code's typed ui/runtime boundary, codex noninteractive + approvals, opencode multi-provider sessions, zero's tui, crush's hooks, grok-build's dream memory — implemented as a thin host on a solid harness engine.

## license

see repo; rotary is mit.

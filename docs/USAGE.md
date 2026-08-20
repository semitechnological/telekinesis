# Usage

Product docs for `tk`. Architecture and the rotary contract live in
[ARCHITECTURE.md](ARCHITECTURE.md) and [ROTARY.md](ROTARY.md).

## Install / build

```bash
curl -fsSL https://raw.githubusercontent.com/tschk/telekinesis/main/install.sh | bash
```

```bash
cargo install telekinesis
cargo install telekinesis --features full
```

```bash
cd ui/tui && cargo build --release
# binary: ui/tui/target/release/tk
```

Default `tk` is the lightest useful coding CLI (`pi-compat` + rx4
`providers` / `builtin-tools`). Everything else is opt-in:

| feature | default | what it adds |
|---|---|---|
| `pi-compat` | yes | JSONL v3 sessions + embed SDK |
| `mcp` | no | `rx4/mcp` + `~/.telekinesis/mcp.json` discover/register |
| `search` | no | darash `web_search` tool |
| `computer-use` | no | `cu_*` tools (Praefectus) |
| `skills` | no | rx4 skill engine |
| `graph-memory` | no | rx4 graph memory / dream |
| `full` | no | `mcp` + `search` + `computer-use` + `skills` + `graph-memory` |

## CLI

```bash
tk login grok
tk login openai
tk

tk exec "summarize this repo"
tk exec --json --cwd /workspace "list the rust crates"
printf '%s\n' "review the diff" | tk exec -
printf '%s\n' "review the diff" | tk --no-yolo

XAI_API_KEY=... tk
```

Default non-TTY / `tk exec` is yolo (`AlwaysAllow`); `--no-yolo` denies
Ask-class tools.

## OAuth providers

| provider | flag |
|---|---|
| grok (xai) | `tk login grok` |
| openai (chatgpt) | `tk login openai` |
| claude (anthropic) | `tk login claude` |
| gemini (google) | `tk login gemini` |
| copilot (github) | `tk login copilot` |
| kimi (moonshot) | `tk login kimi` |
| antigravity | `tk login antigravity` |

## TUI

| feature | description |
|---|---|
| sidebar (ctrl+b) | session list, tool list, plugin list |
| slash autocomplete | filtered command list as you type `/` |
| input history | up/down arrows, persisted to `~/.telekinesis/input_history.json` |
| permission prompts | y/n/always dialog; shows tool name **and arguments** |
| plan approval | whole-turn rx4 preview before tool execution; y/n in the TUI |
| context usage bar | green/amber/red percentage of context window |
| cost tracking | running cost in status bar, `/cost` for breakdown |
| themes | auto, dark, light, dracula, nord, gruvbox, tokyo-night, catppuccin |
| streaming cursor | blinking cursor at end of streaming content |
| role colors | user=blue, assistant=green, tool=amber, system=zinc |
| tool call blocks | bordered blocks with tool name and args |
| diff blocks | green/red line coloring for file edits |

The TUI enables whole-turn plan approval by default when a turn contains tool
calls. Set `TK_PLAN_APPROVAL=off` for non-interactive compatibility or
`TK_PLAN_APPROVAL=bypass` for an explicit yolo mode; `/plan-approval` changes
the setting for the current session.

Tool exposure can be narrowed at startup with `TK_TOOL_PROFILE=minimal|coding|full`;
the default remains the host registry compiled into the binary. `minimal` keeps
built-ins (and MCP tools when built with `--features mcp`), while `coding` also
enables subagents. `cu_*` needs `--features computer-use` (or `full`);
`web_search` needs `--features search` (or `full`); MCP discover/register needs
`--features mcp` (or `full`). `TK_TOOL_PROFILE` cannot add compiled-out tools.
Budget controls cap a run at 24 hours or 1,000 tool iterations; larger values
are accepted but clamped and reported as such.

Model, scope, and effort are persisted to `~/.telekinesis/prefs.json`.

## Slash commands

| command | action |
|---|---|
| `/model [name]` | show / set model (persisted across sessions) |
| `/config` | interactive config menu (model · scope · effort · login) |
| `/config show` | print runtime configuration + auth status |
| `/scope <name>` | coding · research · plan · ask · computer_use (persisted; computer_use needs `--features full`) |
| `/plan <task>` | read-only implementation plan with files, risks, and checks |
| `/review [target]` | read-only findings-only review of a target or workspace |
| `/budget [<cost>\|cost <usd>\|time <seconds>\|turns <count>\|clear]` | bound cost, duration, or tool iterations |
| `/plan-approval ask\|bypass\|off` | review, automatically allow, or disable whole-turn plan gates |
| `/mcp` | list connected MCP tools + `~/.telekinesis/mcp.json` help (`--features mcp` or `full`; otherwise tells you to rebuild) |
| `/search` | darash `web_search` status (`--features search` or `full`; otherwise tells you to rebuild) |
| `/todo` | host surface note (engine todo tool when available) |
| `/sessions` | list JSONL sessions for this project (newest first) |
| `/resume <n>` | switch to a session listed by `/sessions` |
| `/clear` | clear messages + reset cost |
| `/cost` | show cost breakdown |
| `/help` | list commands |
| `/commands [name]` | list commands / show usage for one (alias of `/help`) |
| `/quit` `/exit` | quit |

Slash suggestions show each command's description; typing `/model <partial>`
fuzzy-completes model names across configured providers.

## Keyboard shortcuts

| key | action |
|---|---|
| `Enter` | submit prompt |
| `Shift+Enter` | new line |
| `Esc` | cancel task / close menus / clear input |
| `Ctrl+C` | interrupt / clear draft (press again with empty input to exit) |
| `Ctrl+L` | clear screen |
| `Ctrl+B` | toggle header |
| `←` / `→` | move input cursor |
| `Ctrl+←` / `Ctrl+→` (or `Alt+←/→`) | move by word |
| `Home` / `End` | cursor to start / end of input |
| `Ctrl+Home` / `Ctrl+End` | jump to top / bottom of chat |
| `Ctrl+A` / `Ctrl+E` | cursor to start / end of input |
| `Ctrl+K` / `Ctrl+U` | delete to end / start of input |
| `Ctrl+W`, `Ctrl+Backspace`, `Alt+Backspace` | delete word backwards |
| `Ctrl+Z` | undo last edit |
| `Delete` | delete character after cursor |
| `Up` / `Down` | input history |
| `Shift+Tab` | cycle reasoning effort |
| `Alt+Shift+←/→` | cycle agent scope (coding → research → plan → ask → computer_use) |
| `PgUp` / `PgDn` | scroll chat view |

Model selector: type to search **across all configured providers** with
fuzzy ranking (`provider`, `provider/id`, and bare id all match — e.g. `codex 55`
finds `gpt-5.5`); the provider rails collapse while a query is active.
`←/→` provider, `↑/↓` model, `Enter` apply, `Esc` cancel.

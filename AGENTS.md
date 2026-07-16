# telekinesis

## product

sole **cli + crepuscularity-tui** host for the **rotary** (rx4) agent harness engine.

- ux: minimal/fast (pi-first, codex second)
- typed ipc event boundary (t3code-style)
- no harness reimplementation

## stack

- zig cli (`src/main.zig`) → rotary ipc server
- crepuscularity-tui (`ui/tui`) with hot-reloadable `shell.crepus` template
- rotary submodule `vendor/rotary` (rx4)
- optional quic product layer `src/net.zig` (not yet wired)

## commands (required quality)

```bash
git submodule update --init --recursive
zig build
zig build test
zig build run -- serve
# separate terminal
cd ui/tui && cargo run
```

## rules

- ui talks only over json-rpc to `serve` — never import agent loop internals.
- new agent features land in **rotary**, then surface via ipc/slash here.
- prefer small slash commands that map to rotary methods.
- no hard-coded api keys or telemetry.

## commits

english conventional commits, e.g. `feat(tui): expose /scope and /permissions`.

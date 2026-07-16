# Telekinesis

**Multi-surface agent workspace.** Control coding agents from phone, desktop GUI, or terminal TUI and switch devices without losing context.

The agent **harness** itself lives in **[rotary](https://github.com/tschk/rotary)** (git submodule at `vendor/rotary`). Telekinesis is the product layer: Crepuscularity UI, P2P/QUIC mesh, packaging.

`Agent = Model + Harness + Surfaces.`  
rotary owns harness. telekinesis owns surfaces + networking.

## Stack

- **Harness:** [rotary](https://github.com/tschk/rotary) (Zig 0.16) — loop, tools, providers, plugins, permissions, ACP, IPC
- **UI:** Crepuscularity (`.crepus` DSL, Rust/GPUI / TUI)
- **Cross-device:** own signalling + QUIC P2P (`src/net.zig`)
- **Protocol:** Agent Client Protocol (ACP) for external agents

## Layout

```
telekinesis/
  vendor/rotary/  — git submodule: general-purpose agent harness
  src/
    root.zig      — re-exports rotary + product net
    main.zig      — CLI demos / IPC server
    net.zig       — P2P / QUIC
  ui/             — Crepuscularity TUI/GUI/web
  docs/
  plugins/        — pi extension examples
```

## Build

```bash
git submodule update --init --recursive
zig build
zig build run -- agent
zig build run -- serve
zig build test
```

UI shells still need `telekinesis serve` for IPC, then:

```bash
cd ui/tui && cargo run
cd ui/gui && cargo run
```

## Relationship to rotary

| Concern | Owner |
|---|---|
| Agent loop, tools, providers, sessions, plugins | rotary |
| Permissions, hooks, slash, compact, extract | rotary |
| ACP / LSP / IPC daemon APIs | rotary |
| Multi-surface UI | telekinesis |
| P2P QUIC mesh | telekinesis (`net.zig`) |
| Product packaging / branding | telekinesis |

Update harness:

```bash
cd vendor/rotary && git pull origin main
cd ../.. && git add vendor/rotary && git commit -m "chore: bump rotary"
```

## Influences

t3code (UX), pi (agent loop), Zed ACP, OpenCode/Crush/Zero, Crepuscularity, super.engineering.

## License

See repository license; rotary is MIT.

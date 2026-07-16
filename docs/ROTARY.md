# telekinesis ↔ rotary

telekinesis no longer vendors a private copy of the agent loop.

## Wiring

- Git submodule: `vendor/rotary` → https://github.com/tschk/rotary
- `build.zig` creates a `rotary` module from `vendor/rotary/src/root.zig` and imports it into telekinesis + the exe
- `src/root.zig` re-exports rotary's public surface so existing `telekinesis.Agent` call sites keep working
- Product-only code remains local: `src/net.zig` (QUIC mesh), UI shells under `ui/`

## Update harness

```bash
git submodule update --init --recursive
cd vendor/rotary && git checkout main && git pull
cd ../..
zig build test
git add vendor/rotary
git commit -m "chore: bump rotary"
```

## Do not

- Reintroduce `src/agent.zig` etc. — change rotary instead
- Treat rotary as a symlink-only hack without submodule history
- Mix product UI permissions with rotary policy without going through `Agent.setPolicy`
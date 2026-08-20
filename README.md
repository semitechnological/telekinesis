# telekinesis (tk)

[![license](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/telekinesis.svg)](https://crates.io/crates/telekinesis)

AI coding agent CLI + TUI, powered by [rotary](https://github.com/tschk/rotary)
(rx4) and [crepuscularity-tui](https://github.com/tschk/crepuscularity).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/tschk/telekinesis/main/install.sh | bash
```

```bash
cargo install telekinesis
cargo install telekinesis --features full
```

## Usage

```bash
tk login grok
tk
tk exec "summarize this repo"
printf '%s\n' "review the diff" | tk --no-yolo
```

Default non-TTY / `tk exec` is yolo (`AlwaysAllow`); `--no-yolo` denies Ask-class tools.

Streaming TUI, slash commands, OAuth login, slim default binary. Details:
[docs](docs/README.md).

## License

MPL-2.0

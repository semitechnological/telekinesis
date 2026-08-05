#!/usr/bin/env bash
set -euo pipefail

# Repository-local evaluator for autoresearch and harness comparisons.
# Usage: scripts/benchmark_task.sh [cargo test/build arguments]
# Keep this deterministic and side-effect free: callers should run it from an
# isolated worktree and provide any required environment explicitly.

case "${1:-test}" in
  test)
    shift || true
    exec cargo test --manifest-path ui/tui/Cargo.toml "$@"
    ;;
  build)
    shift || true
    exec cargo build --manifest-path ui/tui/Cargo.toml "$@"
    ;;
  clippy)
    shift || true
    exec cargo clippy --manifest-path ui/tui/Cargo.toml --all-targets -- -D warnings "$@"
    ;;
  *)
    echo "usage: $0 [test|build|clippy] [args...]" >&2
    exit 2
    ;;
esac

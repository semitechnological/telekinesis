#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: agent-tk.sh <candidate-dir> <prompt-file>" >&2
  echo "runs: tk exec --cwd <candidate-dir> [- --model <AVO_*_MODEL>] -" >&2
  echo "prompt is stdin to tk exec; default non-TTY yolo is unchanged" >&2
  exit 2
fi

candidate=$(cd "$1" && pwd) || {
  echo "agent-tk: not a directory: $1" >&2
  exit 2
}
prompt=$2
if [ ! -f "$prompt" ]; then
  echo "agent-tk: prompt file missing: $prompt" >&2
  exit 2
fi

tk="${TK:-tk}"
model="${AVO_DRIVER_MODEL:-${AVO_SUPERVISOR_MODEL:-}}"
cd "$candidate"
if [ -n "$model" ]; then
  exec "$tk" exec --cwd "$candidate" --model "$model" - <"$prompt"
fi
exec "$tk" exec --cwd "$candidate" - <"$prompt"

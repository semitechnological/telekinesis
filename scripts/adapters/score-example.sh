#!/usr/bin/env bash
set -euo pipefail

dir="${1:?usage: score-example.sh <candidate-dir>}"
cd "$dir" || {
  echo "score-example: not a directory: $dir" >&2
  exit 3
}

correct=true
if [ -n "${AVO_CORRECT_CMD:-}" ]; then
  if ! sh -c "$AVO_CORRECT_CMD" >/dev/null 2>&1; then
    correct=false
  fi
fi

objective="${AVO_OBJECTIVE:-0}"
note="${AVO_NOTE:-example score; set AVO_CORRECT_CMD / AVO_OBJECTIVE / AVO_NOTE}"
if [ "$correct" != "true" ]; then
  objective=0
  note="correctness command failed"
fi

export SCORE_CORRECT="$correct"
export SCORE_OBJECTIVE="$objective"
export SCORE_NOTE="$note"
python3 - <<'PY'
import json, os
print(json.dumps({
    "correct": os.environ["SCORE_CORRECT"] == "true",
    "objective": float(os.environ["SCORE_OBJECTIVE"]),
    "metrics": {},
    "note": os.environ["SCORE_NOTE"],
    "artifacts": [],
}))
PY

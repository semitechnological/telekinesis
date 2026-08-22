#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
agent="$root/scripts/adapters/agent-tk.sh"
score="$root/scripts/adapters/score-example.sh"

test -x "$agent"
test -x "$score"

set +e
"$agent" >/tmp/agent-tk-usage 2>&1
agent_status=$?
set -e
test "$agent_status" -eq 2
grep -q usage /tmp/agent-tk-usage

"$score" "$root" | python3 -c '
import json, sys
obj = json.load(sys.stdin)
assert obj["correct"] is True
assert isinstance(obj["objective"], (int, float))
assert isinstance(obj["metrics"], dict)
assert isinstance(obj["note"], str) and obj["note"]
assert isinstance(obj["artifacts"], list)
'

AVO_CORRECT_CMD="false" AVO_NOTE="unused" "$score" "$root" | python3 -c '
import json, sys
obj = json.load(sys.stdin)
assert obj["correct"] is False
assert obj["objective"] == 0
assert obj["note"] == "correctness command failed"
assert obj["artifacts"] == []
'

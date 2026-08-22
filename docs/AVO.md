# AVO on telekinesis

Keep [avo-lite](https://github.com/Git-on-my-level/avo-lite) (MIT, bash+git+jq)
as the loop. telekinesis only supplies a `tk exec` agent adapter and a score
template. Do not vendor avo-lite here.

Ideas we keep:

1. One idempotent tick (`init` / `tick` / `status`).
2. Scored immutable lineage fed back as the next prompt.
3. Hard `correct` gate, separate from higher-is-better `objective`.
4. No-LLM stall detect, then a stronger-model supervisor redirect.
5. Work on `avo/<task>` only. Never commit to `main`. Never push.
6. Agent and scorer are pluggable commands.

## Wire it

```bash
# avo-lite lives outside this repo
export PATH="/path/to/avo-lite/scripts:$PATH"

avo init speedup \
  --goal  "maximize a higher-is-better objective" \
  --score "$PWD/scripts/adapters/score-example.sh" \
  --agent "$PWD/scripts/adapters/agent-tk.sh" \
  --mode  rank

avo tick
avo status
```

`scripts/adapters/agent-tk.sh <candidate-dir> <prompt-file>` runs
`tk exec --cwd <dir> -` with the prompt on stdin. Default non-TTY yolo
(`AlwaysAllow`) stays as-is; the adapter never passes `--no-yolo`.

Set `AVO_DRIVER_MODEL` / `AVO_SUPERVISOR_MODEL` for the cheap-driver /
expensive-supervisor split. The adapter forwards the active one as
`tk exec --model`. `TK` overrides the binary name.

Replace `score-example.sh` with a real `f`. It must print one JSON object and
exit 0 when evaluation *completed* (pass or fail); non-zero is infra failure:

```json
{"correct": true, "objective": 0.87, "metrics": {}, "note": "what changed", "artifacts": []}
```

`correct` is the hard gate. `objective` is required in `rank` mode. `note` is
what the next agent reads. Optional `AVO_CORRECT_CMD` / `AVO_OBJECTIVE` /
`AVO_NOTE` are only for the example template.

`scripts/check-avo-adapters.sh` checks that the adapter is executable and that
the example scorer prints this contract.

There is no `tk avo` subcommand. The default binary stays slim; rx4 stays
`default-features = false` with providers + builtin-tools only.

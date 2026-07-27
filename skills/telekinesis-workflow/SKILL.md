---
id: telekinesis-agent-workflow
name: telekinesis-agent-workflow
description: Execute repository changes with an inspect-plan-implement-verify loop.
tags: [coding, planning, review, verification]
trigger_patterns: [implement, feature, bug, refactor, plan, review]
---
# Telekinesis Agent Workflow

## Triggers

- implement
- feature
- bug
- refactor
- plan
- review

## Instructions

1. Read applicable project instructions, manifests, and the relevant implementation before deciding on a change.
2. Trace the requested behavior across callers and consumers. Reuse the engine or host capability already responsible for it.
3. For a plan or review, remain read-only and return concrete files, risks, and verification. For implementation, make the smallest complete change.
4. Verify with the repository's documented formatter, lint, type-check, build, and test commands. State only evidence-backed results.

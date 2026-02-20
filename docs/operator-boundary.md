# Othala vs OpenClaw Skill Boundary

## Put inside Othala (core orchestrator)

1. **Task lifecycle correctness**
   - chat/task states
   - retry policy
   - submit/merge transitions
   - stuck-task recovery semantics

2. **Mode-aware execution engine**
   - repo policy `stack` vs `merge`
   - submit strategy by mode
   - rebase/restack behavior by mode

3. **Merge detection and safety**
   - merged-state reconciliation
   - guard against false merged states
   - pending-change commit policy before submit

4. **Built-in reliability primitives**
   - daemon liveness checks
   - stale worktree detection
   - deterministic failure classification

5. **CLI-first observability APIs**
   - machine-readable status outputs
   - per-task logs/runs/events summaries
   - scripting-friendly commands for external operators

## Put inside OpenClaw skill (operator layer)

1. **Multi-repo orchestration policy**
   - which repos to run
   - per-repo mode assignment
   - periodic checks and escalation policy

2. **Vault-driven planning**
   - transform notes/objectives into new tasks
   - prioritize lanes (product, reliability, tests)
   - seed perpetual backlog

3. **Cross-channel UX (Telegram/SSH)**
   - status tables and summaries
   - blocked alerts and instructions
   - human-in-the-loop prompt routing

4. **Runbook automation**
   - prune/cleanup routines
   - daemon restart wrappers
   - hourly health and test-freshness reminders

5. **Meta-governance**
   - when to auto-merge vs require human review
   - non-prod fast loop vs prod safety loop

## Rule of thumb
- If it changes task semantics/state machine correctness: **Othala core**.
- If it changes policy, scheduling, messaging, or human workflows: **OpenClaw skill**.

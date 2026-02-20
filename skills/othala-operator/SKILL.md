---
name: othala-operator
description: Operate Othala as a multi-repo autonomous orchestrator with SSH and Telegram visibility. Use when setting up always-on daemons, repo mode policies (stack vs merge), blocked-task escalation, vault-driven task prompting, and hourly health/test reminders.
---

# Othala Operator

Run Othala as an operations system across multiple repos.

## Core loop

1. Keep one daemon per repo running.
2. Keep at least one `CHATTING` task per active repo (auto-seed keep-hot tasks when a repo drops to 0).
3. Convert vault notes into explicit Othala tasks.
4. Track blocked/stopped tasks and stale test coverage.
5. Escalate to Telegram when human input is needed.
6. Keep Othala itself as one continuously improving repo.

## Repo policy modes

Use `.othala/repo-mode.toml` in each repo:

- `mode = "stack"`: use Graphite stack flow (`submit_mode = "single"` or `"stack"` as needed).
- `mode = "merge"`: merge-as-you-go for low-risk/non-prod repos.

If `mode=merge`, run daemon with safer, fast verify profile and avoid deep stack growth.

## SSH visuals

Use scripts in `scripts/operator/`:

- `status_tree.sh` → concise tree for all repos.
- `dashboard_tmux.sh` → multi-pane live dashboard (watch + tails + status).
- `blocked_report.sh` → blocked/stopped tasks + stale test warning report.

## Telegram visuals

Use `status_tree.sh --telegram` output in periodic reports.

When blocked ratio is high, send escalation text:
- ask human to SSH in,
- include task IDs,
- include exact follow-up command(s).

## Human-in-the-loop prompts

If vault context is insufficient for a blocked decision:

1. Emit Telegram escalation.
2. Ask human to SSH and inspect with:
   - `othala watch --lines 40`
   - `othala tail <task-id> -f`
   - `othala logs <task-id>`
3. Continue by creating a follow-up task with clarified instruction.

## Idea-exhaustion behavior (must be in skill policy)

If the active backlog has no credible next PR ideas (after scanning repo TODOs + vault notes):

1. Send a short Telegram prompt asking Mugen for fresh priorities.
2. Include 3 concrete suggestion starters (not generic "what next?").
3. Propose a notes update pass (project page + objectives + implementation checklist) so autonomous seeding quality improves.
4. Resume auto-seeding immediately once new guidance is available.

## Hourly cadence

Every hour (or earlier if most repos blocked):

- Run `blocked_report.sh`.
- If blocked ratio exceeds threshold, escalate on Telegram.
- If test freshness is stale, send reminder and quick test report.

## Othala self-improvement lane

Always maintain one active lane in Othala repo for:

- orchestration reliability fixes,
- submit/retry policy hardening,
- verification/QA improvements,
- observability improvements.

See `references/ops-playbook.md` and `references/vault-prompting.md`.

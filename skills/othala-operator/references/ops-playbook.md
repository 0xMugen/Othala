# Othala Operator Playbook

## Managed repos (default)
- /home/server/clawd/projects/Othala
- /home/server/clawd/projects/PonziLand
- /home/server/clawd/projects/midgard

## Bring-up

For each repo:

1. Ensure clean trunk baseline.
2. Ensure Graphite is initialized (`gt init`) if stack mode.
3. Ensure `.othala/repo-mode.toml` exists.
4. Start daemon with explicit verify command when needed.

Example:

```bash
cd <repo>
othala daemon --skip-context-gen --verify-command "echo verify skipped" --skip-qa
```

## Failure handling policy

### Graphite auth failure
- Stop immediately (no retry burn).
- Action: `gt auth --token <token>` once globally.

### Trunk out-of-date failure
- Stop immediately (no retry burn).
- Action: `gt sync` or `git pull --rebase` on trunk, then resume task.

### Stale/missing worktree for old stacks
- Run stack hygiene:
  - prune old tasks
  - clean invalid worktree references
  - keep active branches only

## Visual operations

### SSH
- Live status: `scripts/operator/status_tree.sh --watch`
- Tmux dashboard: `scripts/operator/dashboard_tmux.sh`
- Detailed task logs: `othala tail <id> -f`

### Telegram
- Post concise status tree every hour.
- Trigger immediate escalation if blocked ratio >= threshold.

## Merge vs stack mode

- Stack mode (prod/high-risk): preserve branch stacks and review flow.
- Merge mode (non-prod/fast iteration): shorter cycle, merge quickly, keep stack shallow.

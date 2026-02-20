#!/usr/bin/env bash
set -euo pipefail

repo="${1:?usage: stack_janitor.sh <repo-path>}"
[[ -d "$repo" ]] || { echo "missing repo: $repo"; exit 1; }

mode_file="$repo/.othala/repo-mode.toml"
mode="stack"
if [[ -f "$mode_file" ]]; then
  mode=$(awk -F'"' '/^mode/ {print $2}' "$mode_file")
fi

cd "$repo"

# No-op for merge repos.
if [[ "$mode" != "stack" ]]; then
  echo "stack_janitor: $(basename "$repo") mode=$mode (skip)"
  exit 0
fi

# Keep trunk current, then restack. Ignore conflict exit here;
# daemon+task retries will handle conflict-aware follow-up.
(gt sync --no-restack --force --no-interactive >/dev/null 2>&1 || true)
(gt restack --no-interactive >/dev/null 2>&1 || true)

# Reconcile task states after stack movement.
othala daemon --once --skip-context-gen --skip-qa --verify-command "echo verify skipped" >/dev/null 2>&1 || true

echo "stack_janitor: $(basename "$repo") done"
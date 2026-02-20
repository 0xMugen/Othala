#!/usr/bin/env bash
set -euo pipefail

repo="${1:?usage: start_repo_daemon.sh <repo-path>}"
[[ -d "$repo" ]] || { echo "missing repo: $repo"; exit 1; }

mode_file="$repo/.othala/repo-mode.toml"
mode="stack"
if [[ -f "$mode_file" ]]; then
  mode=$(awk -F'"' '/^mode/ {print $2}' "$mode_file")
fi

cd "$repo"

verify_cmd="echo verify skipped"
extra_flags=(--skip-context-gen --skip-qa)

if [[ "$mode" == "merge" ]]; then
  verify_cmd="echo verify skipped"
fi

echo "Starting daemon for $(basename "$repo") mode=$mode"
exec othala daemon "${extra_flags[@]}" --verify-command "$verify_cmd"

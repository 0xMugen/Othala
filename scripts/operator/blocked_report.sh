#!/usr/bin/env bash
set -euo pipefail

REPOS=(
  "/home/server/clawd/projects/Othala"
  "/home/server/clawd/projects/PonziLand"
  "/home/server/clawd/projects/midgard"
)

THRESHOLD="${BLOCKED_THRESHOLD:-0.50}"

total=0
blocked=0

for repo in "${REPOS[@]}"; do
  [[ -d "$repo" ]] || continue
  cd "$repo"

  repo_total=$(othala chat list --json 2>/dev/null | jq 'length')
  repo_blocked=$(othala chat list --json 2>/dev/null | jq '[.[] | select(.state=="STOPPED")] | length')

  total=$((total + repo_total))
  blocked=$((blocked + repo_blocked))

  echo "[$(basename "$repo")] total=$repo_total stopped=$repo_blocked"
  othala chat list --json 2>/dev/null | jq -r '.[] | select(.state=="STOPPED") | "  - \(.id): \(.last_failure_reason // "-")"' | head -n 8
  echo

done

ratio="0"
if [[ "$total" -gt 0 ]]; then
  ratio=$(awk -v b="$blocked" -v t="$total" 'BEGIN{printf "%.2f", b/t}')
fi

echo "GLOBAL: blocked=$blocked total=$total ratio=$ratio threshold=$THRESHOLD"

awk -v r="$ratio" -v t="$THRESHOLD" 'BEGIN{exit !(r>=t)}' && {
  echo "ESCALATE=1"
  exit 10
}

echo "ESCALATE=0"

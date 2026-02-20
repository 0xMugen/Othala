#!/usr/bin/env bash
set -euo pipefail

REPOS=(
  "/home/server/clawd/projects/Othala"
  "/home/server/clawd/projects/PonziLand"
  "/home/server/clawd/projects/midgard"
)

MODE="once"
FORMAT="plain"
if [[ "${1:-}" == "--watch" ]]; then MODE="watch"; fi
if [[ "${1:-}" == "--telegram" || "${2:-}" == "--telegram" ]]; then FORMAT="telegram"; fi

print_once() {
  local now
  now="$(date -u '+%Y-%m-%d %H:%M:%SZ')"
  if [[ "$FORMAT" == "telegram" ]]; then
    echo "🧠 Othala status tree ($now)"
  else
    echo "=== Othala status tree ($now) ==="
  fi

  for repo in "${REPOS[@]}"; do
    local name
    name="$(basename "$repo")"
    if [[ ! -d "$repo" ]]; then
      echo "- $name: missing repo path"
      continue
    fi

    local summary
    summary=$(cd "$repo" && othala chat list --json 2>/dev/null | jq -r '
      def c(s): map(select(.state==s))|length;
      "ready=\(c("READY")) chatting=\(c("CHATTING")) stopped=\(c("STOPPED")) awaiting=\(c("AWAITING_MERGE")) merged=\(c("MERGED")) total=\(length)"')

    echo "- $name: $summary"

    cd "$repo"
    othala chat list --json 2>/dev/null | jq -r '
      sort_by(.updated_at) | reverse | .[:5] |
      .[] | "  • \(.id) [\(.state)] model=\(.preferred_model // "-") reason=\(.last_failure_reason // "-")"'
  done
}

if [[ "$MODE" == "watch" ]]; then
  while true; do
    clear
    print_once
    sleep 15
  done
else
  print_once
fi

#!/usr/bin/env bash
set -euo pipefail

# Multi-repo status table for Othala operations
REPOS=(
  "/home/server/clawd/projects/Othala"
  "/home/server/clawd/projects/PonziLand"
  "/home/server/clawd/projects/midgard"
  "/home/server/clawd/projects/survivor-valhalla"
  "/home/server/clawd/projects/guilds"
  "/home/server/clawd/projects/ClawMesh"
)

printf "%-20s %-8s %-8s %-8s %-8s %-8s %-10s %s\n" "REPO" "CHAT" "READY" "STOP" "AWAIT" "MERGED" "UPDATED" "WORKING_ON"
printf "%-20s %-8s %-8s %-8s %-8s %-8s %-10s %s\n" "--------------------" "--------" "--------" "--------" "--------" "--------" "----------" "----------"

for repo in "${REPOS[@]}"; do
  name="$(basename "$repo")"
  if [[ ! -d "$repo" ]]; then
    printf "%-20s %-8s %-8s %-8s %-8s %-8s %-10s %s\n" "$name" "-" "-" "-" "-" "-" "-" "missing repo"
    continue
  fi

  json="$(cd "$repo" && othala chat list --json 2>/dev/null || echo '[]')"

  chatting="$(jq '[.[] | select(.state=="CHATTING")] | length' <<<"$json")"
  ready="$(jq '[.[] | select(.state=="READY")] | length' <<<"$json")"
  stopped="$(jq '[.[] | select(.state=="STOPPED")] | length' <<<"$json")"
  awaiting="$(jq '[.[] | select(.state=="AWAITING_MERGE")] | length' <<<"$json")"
  merged="$(jq '[.[] | select(.state=="MERGED")] | length' <<<"$json")"

  top="$(jq -r 'sort_by(.updated_at) | reverse | .[0] // empty' <<<"$json")"
  if [[ -n "$top" ]]; then
    updated="$(jq -r '.updated_at // "-"' <<<"$top" | cut -c12-19)"
    title="$(jq -r '.title // "-"' <<<"$top" | tr '\n' ' ' | sed 's/  */ /g' | cut -c1-110)"
  else
    updated="-"
    title="idle"
  fi

  printf "%-20s %-8s %-8s %-8s %-8s %-8s %-10s %s\n" "$name" "$chatting" "$ready" "$stopped" "$awaiting" "$merged" "$updated" "$title"
done

#!/usr/bin/env bash
set -euo pipefail

BASE="/home/server/clawd/projects/Othala"
OUT_DIR="$BASE/logs/reliability"
mkdir -p "$OUT_DIR"

TS="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
DAY="$(date -u +"%Y-%m-%d")"
OUT_FILE="$OUT_DIR/${DAY}.jsonl"

repos=(
  "/home/server/clawd/projects/Othala"
  "/home/server/clawd/projects/PonziLand"
  "/home/server/clawd/projects/midgard"
  "/home/server/clawd/projects/survivor-valhalla"
)

for repo in "${repos[@]}"; do
  name="$(basename "$repo")"

  if ! json="$(cd "$repo" && othala chat list --json 2>/dev/null)"; then
    jq -nc --arg ts "$TS" --arg repo "$name" '{ts:$ts,repo:$repo,error:"chat_list_failed"}' >> "$OUT_FILE"
    continue
  fi

  jq -nc \
    --arg ts "$TS" \
    --arg repo "$name" \
    --argjson tasks "$json" \
    '{
      ts:$ts,
      repo:$repo,
      counts:{
        chatting: ($tasks|map(select(.state=="CHATTING"))|length),
        ready: ($tasks|map(select(.state=="READY"))|length),
        submitting: ($tasks|map(select(.state=="SUBMITTING"))|length),
        awaiting_merge: ($tasks|map(select(.state=="AWAITING_MERGE"))|length),
        merged: ($tasks|map(select(.state=="MERGED"))|length),
        stopped: ($tasks|map(select(.state=="STOPPED"))|length)
      },
      latest_task: ($tasks | sort_by(.updated_at // "") | reverse | .[0] // null)
    }' >> "$OUT_FILE"
done

echo "$OUT_FILE"
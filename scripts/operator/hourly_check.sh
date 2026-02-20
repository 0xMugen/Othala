#!/usr/bin/env bash
set -euo pipefail

BASE="/home/server/clawd/projects/Othala"
"$BASE/scripts/operator/reliability_snapshot.sh" >/dev/null 2>&1 || true
report="$($BASE/scripts/operator/blocked_report.sh || true)"
echo "$report"

if echo "$report" | grep -q 'ESCALATE=1'; then
  msg="Othala alert: repos are blocked. Please SSH in and review with: tmux attach -t othala-ops"
  if command -v openclaw >/dev/null 2>&1; then
    openclaw system event --text "$msg" --mode now || true
  fi
fi

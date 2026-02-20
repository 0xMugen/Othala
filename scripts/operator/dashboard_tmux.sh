#!/usr/bin/env bash
set -euo pipefail

SESSION="othala-ops"
BASE="/home/server/clawd/projects/Othala"

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required"
  exit 1
fi

if tmux has-session -t "$SESSION" 2>/dev/null; then
  tmux attach -t "$SESSION"
  exit 0
fi

tmux new-session -d -s "$SESSION" -n ops "cd $BASE && scripts/operator/status_tree.sh --watch"
tmux split-window -h -t "$SESSION":0 "cd $BASE && while true; do scripts/operator/blocked_report.sh; sleep 30; done"
tmux split-window -v -t "$SESSION":0.1 "cd $BASE && othala watch --lines 25"
tmux select-layout -t "$SESSION":0 tiled

echo "tmux session created: $SESSION"
echo "attach with: tmux attach -t $SESSION"
tmux attach -t "$SESSION"

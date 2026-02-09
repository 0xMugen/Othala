#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
task_id="${1:-T-E2E-READY-SCRIPT}"
original_branch="$(git -C "$repo_root" rev-parse --abbrev-ref HEAD 2>/dev/null || true)"

cleanup() {
  if [[ -n "$original_branch" ]]; then
    git -C "$repo_root" switch "$original_branch" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

tmp_root="$(mktemp -d /tmp/othala-e2e-config-XXXX)"
mkdir -p "$tmp_root/repos"

cat >"$tmp_root/org.toml" <<'TOML'
[models]
enabled = ["codex"]
policy = "adaptive"
min_approvals = 1

[concurrency]
per_repo = 10
claude = 10
codex = 10
gemini = 10

[graphite]
auto_submit = false
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"
TOML

cat >"$tmp_root/repos/othala.toml" <<TOML
repo_id = "othala"
repo_path = "$repo_root"
base_branch = "main"

[nix]
dev_shell = "nix develop"

[verify.quick]
commands = ["nix develop -c true"]

[verify.full]
commands = ["nix develop -c true"]

[graphite]
draft_on_start = true
submit_mode = "single"
TOML

cat >"$tmp_root/task.json" <<JSON
{
  "repo_id": "othala",
  "task_id": "$task_id",
  "title": "Scripted E2E READY workflow",
  "type": "feature",
  "role": "general",
  "preferred_model": "codex",
  "depends_on": [],
  "submit_mode": "single"
}
JSON

sqlite_path="$(mktemp -u /tmp/othala-e2e-XXXX.sqlite)"
event_root="$(mktemp -d /tmp/othala-e2e-events-XXXX)"

cd "$repo_root"
nix develop -c cargo run -p orchd --bin othala -- create-task \
  --org-config "$tmp_root/org.toml" \
  --repos-config-dir "$tmp_root/repos" \
  --spec "$tmp_root/task.json" \
  --sqlite-path "$sqlite_path" \
  --event-log-root "$event_root"

nix develop -c cargo run -p orchd --bin othala -- \
  --org-config "$tmp_root/org.toml" \
  --repos-config-dir "$tmp_root/repos" \
  --once \
  --sqlite-path "$sqlite_path" \
  --event-log-root "$event_root"

nix develop -c cargo run -p orchd --bin othala -- review-approve \
  --org-config "$tmp_root/org.toml" \
  --task-id "$task_id" \
  --reviewer codex \
  --verdict approve \
  --sqlite-path "$sqlite_path" \
  --event-log-root "$event_root"

nix develop -c cargo run -p orchd --bin othala -- \
  --org-config "$tmp_root/org.toml" \
  --repos-config-dir "$tmp_root/repos" \
  --once \
  --sqlite-path "$sqlite_path" \
  --event-log-root "$event_root"

echo
echo "Final task state:"
nix develop -c cargo run -p orchd --bin othala -- list-tasks \
  --org-config "$tmp_root/org.toml" \
  --sqlite-path "$sqlite_path" \
  --event-log-root "$event_root"

echo
echo "Temporary files:"
echo "  org config:    $tmp_root/org.toml"
echo "  repo configs:  $tmp_root/repos"
echo "  sqlite:        $sqlite_path"
echo "  event root:    $event_root"

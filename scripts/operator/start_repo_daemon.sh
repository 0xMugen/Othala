#!/usr/bin/env bash
set -euo pipefail

repo="${1:?usage: start_repo_daemon.sh <repo-path>}"
[[ -d "$repo" ]] || { echo "missing repo: $repo"; exit 1; }

mode_file="$repo/.othala/repo-mode.toml"
mode="stack"
if [[ -f "$mode_file" ]]; then
  mode=$(awk -F'"' '/^mode/ {print $2}' "$mode_file")
fi

choose_verify_command() {
  local root="$1"

  # Manual override wins.
  if [[ -f "$root/.othala/verify-command" ]]; then
    tr -d '\n' < "$root/.othala/verify-command"
    return 0
  fi

  # Rust
  if [[ -f "$root/Cargo.toml" ]]; then
    echo "cargo test --all-targets --all-features"
    return 0
  fi

  # Node/TS
  if [[ -f "$root/package.json" ]]; then
    if jq -e '.scripts.test' "$root/package.json" >/dev/null 2>&1; then
      if [[ -f "$root/pnpm-lock.yaml" ]]; then
        echo "pnpm test"
      elif [[ -f "$root/yarn.lock" ]]; then
        echo "yarn test"
      else
        echo "npm test"
      fi
      return 0
    fi
  fi

  # Python
  if [[ -f "$root/pyproject.toml" || -f "$root/pytest.ini" || -d "$root/tests" ]]; then
    echo "pytest -q"
    return 0
  fi

  echo "echo verify skipped"
}

cd "$repo"

verify_cmd="$(choose_verify_command "$repo")"
extra_flags=(--skip-context-gen)

# Keep QA disabled until QA agent hardening lands; still run real verify command.
extra_flags+=(--skip-qa)

echo "Starting daemon for $(basename "$repo") mode=$mode verify='${verify_cmd}'"
exec othala daemon "${extra_flags[@]}" --verify-command "$verify_cmd"

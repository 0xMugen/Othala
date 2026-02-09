#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/install-latest.sh [--repo <owner/name>] [--tag <release-tag>] [--method <auto|nix|cargo>] [--force]

Installs the `othala` CLI from the latest GitHub release tag.

Options:
  --repo     GitHub repo slug (default: 0xMugen/Othala)
  --tag      Release tag override (skip latest lookup, e.g. v0.1.0-alpha.3)
  --method   Install method: auto, nix, cargo (default: auto)
  --force    Pass --force to cargo install
USAGE
}

repo="0xMugen/Othala"
tag=""
method="auto"
force=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    --tag)
      tag="${2:-}"
      shift 2
      ;;
    --method)
      method="${2:-}"
      shift 2
      ;;
    --force)
      force=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "$repo" ]]; then
  echo "--repo must not be empty" >&2
  exit 1
fi

if [[ "$method" != "auto" && "$method" != "nix" && "$method" != "cargo" ]]; then
  echo "--method must be one of auto|nix|cargo" >&2
  exit 1
fi

resolve_latest_tag() {
  if command -v gh >/dev/null 2>&1 && gh auth status -h github.com >/dev/null 2>&1; then
    gh api "repos/${repo}/releases/latest" --jq .tag_name 2>/dev/null || true
    return
  fi

  if command -v curl >/dev/null 2>&1; then
    local latest_json
    latest_json="$(curl -fsSL "https://api.github.com/repos/${repo}/releases/latest")" || true
    if [[ -n "$latest_json" ]]; then
      printf '%s\n' "$latest_json" \
        | sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -n 1
    fi
  fi
}

if [[ -z "$tag" ]]; then
  tag="$(resolve_latest_tag)"
fi

if [[ -z "$tag" || "$tag" == "null" ]]; then
  echo "failed to resolve release tag for ${repo}" >&2
  echo "hint: pass --tag <release-tag> or authenticate gh with 'gh auth login'" >&2
  exit 1
fi

if [[ "$method" == "auto" ]]; then
  if command -v nix >/dev/null 2>&1; then
    method="nix"
  elif command -v cargo >/dev/null 2>&1; then
    method="cargo"
  else
    echo "install requires nix or cargo in PATH" >&2
    exit 1
  fi
fi

echo "installing othala from ${repo} release ${tag} using ${method}"

if [[ "$method" == "nix" ]]; then
  nix profile install "git+ssh://git@github.com/${repo}.git?ref=${tag}#othala"
  exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required for --method cargo" >&2
  exit 1
fi

cargo_args=(
  install
  --git "ssh://git@github.com/${repo}.git"
  --tag "${tag}"
  --locked
  --package orchd
  --bin othala
)

if [[ "$force" -eq 1 ]]; then
  cargo_args+=(--force)
fi

cargo "${cargo_args[@]}"

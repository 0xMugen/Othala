#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/install-latest.sh [--repo <owner/name>] [--method <auto|nix|cargo>] [--force]

Installs the `othala` CLI from the latest GitHub release tag.

Options:
  --repo     GitHub repo slug (default: 0xMugen/Othala)
  --method   Install method: auto, nix, cargo (default: auto)
  --force    Pass --force to cargo install
USAGE
}

repo="0xMugen/Othala"
method="auto"
force=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="${2:-}"
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

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to resolve the latest release tag" >&2
  exit 1
fi

latest_json="$(curl -fsSL "https://api.github.com/repos/${repo}/releases/latest")"
tag="$(
  printf '%s\n' "$latest_json" \
    | sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' \
    | head -n 1
)"

if [[ -z "$tag" || "$tag" == "null" ]]; then
  echo "failed to resolve latest release tag for ${repo}" >&2
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
  nix profile install "github:${repo}/${tag}#othala"
  exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required for --method cargo" >&2
  exit 1
fi

cargo_args=(
  install
  --git "https://github.com/${repo}.git"
  --tag "${tag}"
  --locked
  --package orchd
  --bin othala
)

if [[ "$force" -eq 1 ]]; then
  cargo_args+=(--force)
fi

cargo "${cargo_args[@]}"

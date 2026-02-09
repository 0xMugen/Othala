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

is_valid_tag() {
  local value="${1:-}"
  [[ -n "$value" ]] || return 1
  [[ "$value" != "null" ]] || return 1
  [[ "$value" != \{* ]] || return 1
  [[ "$value" != \[* ]] || return 1
  [[ "$value" =~ ^[A-Za-z0-9._/-]+$ ]] || return 1
}

extract_tag_name() {
  local payload="${1:-}"
  if [[ -z "$payload" ]]; then
    return 0
  fi
  printf '%s\n' "$payload" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    | head -n 1
}

extract_name() {
  local payload="${1:-}"
  if [[ -z "$payload" ]]; then
    return 0
  fi
  printf '%s\n' "$payload" \
    | sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    | head -n 1
}

resolve_latest_tag() {
  local candidate payload

  if command -v gh >/dev/null 2>&1 && gh auth status -h github.com >/dev/null 2>&1; then
    candidate="$(gh api "repos/${repo}/releases/latest" --jq '.tag_name // empty' 2>/dev/null || true)"
    if is_valid_tag "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi

    candidate="$(gh api "repos/${repo}/releases?per_page=20" --jq 'map(select(.draft == false)) | .[0].tag_name // empty' 2>/dev/null || true)"
    if is_valid_tag "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi

    candidate="$(gh api "repos/${repo}/tags?per_page=1" --jq '.[0].name // empty' 2>/dev/null || true)"
    if is_valid_tag "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi
  fi

  if command -v curl >/dev/null 2>&1; then
    payload="$(curl -fsSL "https://api.github.com/repos/${repo}/releases/latest" 2>/dev/null || true)"
    candidate="$(extract_tag_name "$payload")"
    if is_valid_tag "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi

    payload="$(curl -fsSL "https://api.github.com/repos/${repo}/releases?per_page=20" 2>/dev/null || true)"
    candidate="$(extract_tag_name "$payload")"
    if is_valid_tag "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi

    payload="$(curl -fsSL "https://api.github.com/repos/${repo}/tags?per_page=1" 2>/dev/null || true)"
    candidate="$(extract_name "$payload")"
    if is_valid_tag "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi
  fi

  return 1
}

install_nix_profile() {
  local flake_ref="${1:-}"
  [[ -n "$flake_ref" ]] || return 1

  if nix profile install "$flake_ref"; then
    return 0
  fi

  # If othala is already installed, replace it and retry once.
  if nix profile list 2>/dev/null | grep -Fq "othala"; then
    nix profile remove othala >/dev/null 2>&1 || true
    nix profile install "$flake_ref"
    return $?
  fi

  return 1
}

if [[ -z "$tag" ]]; then
  tag="$(resolve_latest_tag || true)"
fi

if ! is_valid_tag "$tag"; then
  echo "failed to resolve release tag for ${repo}" >&2
  echo "hint: pass --tag <release-tag> (example: --tag v0.1.0-alpha.3) or authenticate gh with 'gh auth login'" >&2
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
  if install_nix_profile "github:${repo}/${tag}#othala"; then
    exit 0
  fi
  if install_nix_profile "git+https://github.com/${repo}.git?ref=refs/tags/${tag}#othala"; then
    exit 0
  fi
  if install_nix_profile "git+ssh://git@github.com/${repo}.git?ref=refs/tags/${tag}#othala"; then
    exit 0
  fi
  echo "nix install failed for ${repo} (${tag})" >&2
  exit 1
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

if cargo "${cargo_args[@]}"; then
  exit 0
fi

fallback_args=(
  install
  --git "ssh://git@github.com/${repo}.git"
  --tag "${tag}"
  --locked
  --package orchd
  --bin othala
)

if [[ "$force" -eq 1 ]]; then
  fallback_args+=(--force)
fi

cargo "${fallback_args[@]}"

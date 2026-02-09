#!/usr/bin/env bash
set -euo pipefail

install_step=0

log_step() {
  install_step=$((install_step + 1))
  printf '[install:%02d] %s\n' "$install_step" "$*"
}

log_info() {
  printf '[install] %s\n' "$*"
}

strip_ansi() {
  sed -E 's/\x1B\[[0-9;]*[A-Za-z]//g'
}

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

next_nix_profile_priority() {
  local current_min found value
  local profile_json
  current_min=0
  found=0

  profile_json="$(nix profile list --json 2>/dev/null || true)"
  if [[ -n "$profile_json" ]]; then
    while IFS= read -r value; do
      [[ -n "$value" ]] || continue
      if [[ "$found" -eq 0 || "$value" -lt "$current_min" ]]; then
        current_min="$value"
        found=1
      fi
    done < <(
      printf '%s' "$profile_json" \
        | tr '{},' '\n' \
        | sed -n 's/^[[:space:]]*"priority":[[:space:]]*\(-\?[0-9]\+\)[[:space:]]*$/\1/p'
    )
  fi

  if [[ "$found" -eq 0 ]]; then
    printf '%s\n' "-1"
    return 0
  fi

  printf '%s\n' "$((current_min - 1))"
}

install_nix_profile() {
  local flake_ref="${1:-}"
  local priority
  local output cleaned rc attempt
  [[ -n "$flake_ref" ]] || return 1
  priority="$(next_nix_profile_priority)"
  log_step "Installing via nix profile (${flake_ref}) with priority ${priority}"

  for attempt in $(seq 1 32); do
    if output="$(nix profile install "$flake_ref" --priority "$priority" 2>&1)"; then
      if [[ -n "$output" ]]; then
        printf '%s\n' "$output"
      fi
      log_info "Installed successfully with priority ${priority}"
      return 0
    else
      rc=$?
      cleaned="$(printf '%s\n' "$output" | strip_ansi)"
      printf '%s\n' "$cleaned" >&2

      if ! printf '%s\n' "$cleaned" | grep -Fq "already provides the following file"; then
        return "$rc"
      fi

      log_info "Profile conflict at priority ${priority}; retrying with lower priority"
      priority=$((priority - 1))
      if [[ "$attempt" -eq 32 ]]; then
        break
      fi
    fi
  done

  echo "nix install failed after repeated profile priority conflicts" >&2
  return 1
}

if [[ -z "$tag" ]]; then
  log_step "Resolving latest release tag from ${repo}"
  tag="$(resolve_latest_tag || true)"
fi

if ! is_valid_tag "$tag"; then
  echo "failed to resolve release tag for ${repo}" >&2
  echo "hint: pass --tag <release-tag> (example: --tag v0.1.0-alpha.3) or authenticate gh with 'gh auth login'" >&2
  exit 1
fi

if [[ "$method" == "auto" ]]; then
  log_step "Selecting install method"
  if command -v nix >/dev/null 2>&1; then
    method="nix"
  elif command -v cargo >/dev/null 2>&1; then
    method="cargo"
  else
    echo "install requires nix or cargo in PATH" >&2
    exit 1
  fi
fi

log_step "Installing othala from ${repo} release ${tag} using ${method}"

if [[ "$method" == "nix" ]]; then
  log_info "Trying nix flake ref: github:${repo}/${tag}#othala"
  if install_nix_profile "github:${repo}/${tag}#othala"; then
    exit 0
  fi
  log_info "Primary nix ref failed, trying git+https fallback"
  if install_nix_profile "git+https://github.com/${repo}.git?ref=refs/tags/${tag}#othala"; then
    exit 0
  fi
  log_info "HTTPS fallback failed, trying git+ssh fallback"
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

log_step "Running cargo install over HTTPS"
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

log_info "HTTPS cargo install failed, retrying over SSH"
cargo "${fallback_args[@]}"

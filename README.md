# Othala

Org-wide AI coding orchestrator (MVP-2 track), built in Rust with:

- `Nix` for reproducible runtime and verification
- `Graphite (gt)` for branch/stack operations
- `SQLite + JSONL` for canonical state + audit events
- `TUI + Web UI` for command/control and merge assistance

## Current Status

This repository is **mid-MVP-2**.
Orchestrator validation checks are run as part of cross-project test workflows.
This line exists solely to validate cross-project orchestrator patching.
Cross-project orchestrator validation notes may appear here as harmless metadata.
Additional orchestrator validation markers may be added during automated test assignments.
This README includes a harmless note confirming orchestrator validation coverage.

- Workspace and crate layout are in place.
- Core orchestrator state model and persistence are implemented.
- Daemon boot + scheduler tick + runtime tick are implemented.
- Task initialization now executes real Graphite/worktree flow.
- Quick verify execution and READY promotion path are wired.
- Web merge queue and sandbox verify runner are implemented.

Open gaps (before calling this production-ready):

- Full autonomous agent epoch orchestration in `orchd` is not complete.
- Reviewer automation (Claude/Codex/Gemini CLI-driven review generation) is not complete.
- End-to-end auto-submit to GitHub is environment-dependent and not fully hardened.
- TUI supports core in-app lifecycle controls, but interactive task creation from inside TUI is not wired yet.

## Prerequisites

- Nix with flakes enabled
- Graphite CLI (`gt`)
- Rust toolchain (provided by flake dev shell)
- Optional model CLIs on PATH: `claude`, `codex`, `gemini`

## Install (Latest Release)

Install `othala` from the latest GitHub release tag:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/0xMugen/Othala/main/scripts/install-latest.sh)
```

If raw GitHub returns `404` (private/authenticated repo setup), run from a clone:

```bash
git clone git@github.com:0xMugen/Othala.git
cd Othala
./scripts/install-latest.sh
```

After install, start the interactive setup wizard:

```bash
othala wizard
```

Open the command center TUI:

```bash
othala
```

Run one orchestration tick:

```bash
othala daemon --once
```

## Dev Quick Start

1. Enter dev shell:

```bash
nix develop
```

2. Run checks:

```bash
cargo fmt
cargo test -p orchd
cargo check
```

3. Run `othala` TUI from source:

```bash
cargo run -p orchd --bin othala
```

4. Run one scheduler/runtime tick from source:

```bash
cargo run -p orchd --bin othala -- daemon --once
```

5. Create a task:

```bash
cargo run -p orchd --bin othala -- create-task --spec templates/task-spec.example.json
```

6. List tasks:

```bash
cargo run -p orchd --bin othala -- list-tasks
```

## CLI Commands

### Run daemon

```bash
othala daemon \
  --org-config config/org.toml \
  --repos-config-dir config/repos \
  --sqlite-path .orch/state.sqlite \
  --event-log-root .orch/events
```

### Setup wizard (interactive)

```bash
othala wizard
```

### Setup model selection (non-interactive)

```bash
othala setup --enable claude,codex --per-model-concurrency 5
```

### Record a manual review decision

Used to feed approvals into the review gate:

```bash
othala review-approve \
  --task-id T123 \
  --reviewer codex \
  --verdict approve
```

Valid verdicts: `approve`, `request_changes`, `block`.

## process-compose

`process-compose.yaml` includes:

- `orchd`
- `orch-web`
- `orch-tui`

Run:

```bash
process-compose up
```

## Tested Workflow (Current)

A validated local flow (with controlled test config) is:

1. `create-task` -> `QUEUED`
2. daemon tick -> `INITIALIZING` -> `DRAFT_PR_OPEN` -> `RUNNING`
3. daemon tick starts and runs quick verify -> `REVIEWING`
4. `review-approve` records required approvals
5. daemon tick promotes task -> `READY` (or `SUBMITTING` when auto-submit enabled)

You can run this exact controlled path with:

```bash
scripts/e2e-ready.sh
```

## Project Layout

```text
crates/
  orch-core/
  orchd/
  orch-git/
  orch-graphite/
  orch-verify/
  orch-agents/
  orch-notify/
  orch-tui/
  orch-web/
```

## Notes

- Runtime state is stored under `.orch/` (gitignored).
- Graphite operations are intentionally wrapped through `orch-graphite`.
- For controlled local validation, prefer temporary config + sqlite paths.
- Post-auth submit tests need authenticated GitHub access and an up-to-date trunk (`gt sync`) before non-interactive `gt submit`.
- This README includes a no-op line for orchestrator test coverage.
- Harmless README touch for auth-failure handling test.

## Operator skill (multi-repo)

- Skill package path: `skills/othala-operator/`
- SSH visual tree: `scripts/operator/status_tree.sh --watch`
- SSH dashboard (tmux): `scripts/operator/dashboard_tmux.sh`
- Blocked/stale report: `scripts/operator/blocked_report.sh`
- Hourly escalation hook: `scripts/operator/hourly_check.sh`

Per-repo mode is declared in `.othala/repo-mode.toml`:
- `mode = "stack"` for stack-first repos.
- `mode = "merge"` for merge-as-you-go repos.

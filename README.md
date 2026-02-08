# Othala

Org-wide AI coding orchestrator (MVP-2 track), built in Rust with:

- `Nix` for reproducible runtime and verification
- `Graphite (gt)` for branch/stack operations
- `SQLite + JSONL` for canonical state + audit events
- `TUI + Web UI` for command/control and merge assistance

## Current Status

This repository is **mid-MVP-2**.

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
- TUI action dispatch is present, but not fully wired to daemon-side command APIs.

## Prerequisites

- Nix with flakes enabled
- Graphite CLI (`gt`)
- Rust toolchain (provided by flake dev shell)
- Optional model CLIs on PATH: `claude`, `codex`, `gemini`

## Quick Start

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

3. Start daemon once (bootstrap + one scheduler/runtime tick):

```bash
cargo run -p orchd -- --once
```

4. Create a task:

```bash
cargo run -p orchd -- create-task --spec templates/task-spec.example.json
```

5. List tasks:

```bash
cargo run -p orchd -- list-tasks
```

## Daemon Commands

### Run daemon

```bash
cargo run -p orchd -- \
  --org-config config/org.toml \
  --repos-config-dir config/repos \
  --sqlite-path .orch/state.sqlite \
  --event-log-root .orch/events
```

### Setup model selection

```bash
cargo run -p orchd -- setup --enable claude,codex --per-model-concurrency 5
```

### Record a manual review decision

Used to feed approvals into the review gate:

```bash
cargo run -p orchd -- review-approve \
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

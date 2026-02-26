# Othala

Org-wide AI coding orchestrator, built in Rust with:

- `Nix` for reproducible runtime and verification
- `Graphite (gt)` for branch/stack operations
- `SQLite + JSONL` for canonical state + audit events
- `TUI + Web UI` for command/control and merge assistance

## Current Status

This repository is **production-hardened** with self-healing pipelines, operator-grade observability, and autonomous E2E test coverage.

- Workspace and crate layout are in place.
- Core orchestrator state model and persistence are implemented.
- Daemon boot + scheduler tick + runtime tick are implemented.
- Task initialization executes real Graphite/worktree flow.
- Quick verify execution and READY promotion path are wired.
- Web merge queue and sandbox verify runner are implemented.
- **Install Wizard v2** — first-run readiness checks with scored remediation.
- **Graphite reliability** — auto-track worktree branches, repair command, push fallback.
- **QA self-heal pipeline** — failure classification, retry policy, auto-fix spawning.
- **Context generation observability** — latency, coverage, cache, token budget, stale warnings.
- **Delta-based operator reporting** — meaningful state changes only, noise suppression.
- **Mission vault** — requirement parsing, coverage matrix, semantic dedup, gap detection.
- **E2E orchestration test suite** — scenario runner, chaos injection, soak tests.

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

The wizard runs 14 readiness checks across 4 categories (critical tools, config, models, permissions), produces a 0–100 readiness score, and provides actionable remediation hints.

**CI mode** (non-interactive, exits with code 0/1):

```bash
othala wizard --ci
othala wizard --ci --json
```

**Check-only mode** (print report, always exit 0):

```bash
othala wizard --check-only
```

### Setup model selection (non-interactive)

```bash
othala setup --enable claude,codex --per-model-concurrency 5
```

### Graphite repair

Detect and repair Graphite branch tracking divergence:

```bash
othala graphite-repair
othala graphite-repair --dry-run
othala graphite-repair --json
```

### Context generation status

Show context generation telemetry: latency, coverage, cache hits, token budget, stale warnings:

```bash
othala context-status
othala context-status --json
```

### Mission completeness

Show requirement coverage matrix, semantic dedup, gaps:

```bash
othala mission-status
othala mission-status --json
```

### E2E orchestration test suite

Run built-in orchestration scenarios (happy path, retry, chaos, multi-task, verify loop, QA red→green):

```bash
othala e2e-scenarios
othala e2e-scenarios --json
```

Run soak test (sustained tick simulation with stuck-task detection):

```bash
othala e2e-scenarios --soak
othala e2e-scenarios --soak --soak-ticks 5000
othala e2e-scenarios --soak --chaos        # with fault injection
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
  orch-core/       # Core types, state machine, events, config
  orchd/           # Main daemon binary + all orchestration modules
  orch-git/        # Git operations
  orch-graphite/   # Graphite CLI wrapper
  orch-verify/     # Verification framework
  orch-agents/     # Agent spawning + supervision
  orch-notify/     # Notification dispatch
  orch-tui/        # Terminal UI
  orch-web/        # Web dashboard + merge queue
```

### Key orchd modules (production-readiness)

| Module | Purpose | Tests |
|--------|---------|-------|
| `wizard.rs` | Install wizard v2 — readiness score, remediation, CI mode | 7 |
| `graphite_agent.rs` | Graphite reliability — auto-track, repair, push fallback | 3+ |
| `qa_self_heal.rs` | QA self-heal — failure classifier, retry, auto-fix | 15 |
| `context_gen_telemetry.rs` | Context observability — latency, coverage, tokens | 18 |
| `delta_report.rs` | Delta reporting — state-change detection, suppression | 25 |
| `mission_vault.rs` | Mission vault — requirements, coverage, dedup, gaps | 21 |
| `e2e_scenarios.rs` | E2E scenarios — runner, chaos, soak framework | 25 |

## Notes

- Runtime state is stored under `.orch/` (gitignored).
- Graphite operations are intentionally wrapped through `orch-graphite`.
- For controlled local validation, prefer temporary config + sqlite paths.
- Post-auth submit tests need authenticated GitHub access and an up-to-date trunk (`gt sync`) before non-interactive `gt submit`.

## Operator skill (multi-repo)

- Skill package path: `skills/othala-operator/`
- SSH visual tree: `scripts/operator/status_tree.sh --watch`
- SSH dashboard (tmux): `scripts/operator/dashboard_tmux.sh`
- Blocked/stale report: `scripts/operator/blocked_report.sh`
- Hourly escalation hook: `scripts/operator/hourly_check.sh`

Per-repo mode is declared in `.othala/repo-mode.toml`:
- `mode = "stack"` for stack-first repos.
- `mode = "merge"` for merge-as-you-go repos.

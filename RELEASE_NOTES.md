# Release Notes — Production Readiness Campaign

## Overview

This release hardens Othala into a production-grade, self-healing AI coding orchestrator. Seven feature PRs deliver operator-grade observability, autonomous failure recovery, and comprehensive test coverage.

## New Features

### Install Wizard v2 (PR #69)

First-run excellence with 14 readiness checks across 4 weighted categories:
- **Critical tools** (40pts): nix, cargo, gt, git, sqlite3
- **Configuration** (20pts): org.toml, repos config, .othala dir
- **Models** (25pts): claude, codex, gemini availability
- **Permissions** (15pts): repo write access, .othala writability

Produces a 0–100 readiness score with actionable remediation hints. Supports `--ci` mode (exit 0/1), `--check-only`, and `--json` output.

### Graphite Reliability Hardening (PR #70)

Eliminates "untracked branch" blockers:
- `ensure_tracked()` — auto-detects and fixes untracked worktree branches before submit
- `git_push_fallback()` — falls back to git push when `gt submit` fails
- `graphite-repair` CLI command — detect and repair branch tracking divergence
- Divergence detection tests

### QA Self-Heal Pipeline (PR #71)

Autonomous failure recovery for the QA gate:
- **Failure classification**: Regression, Flaky, EnvironmentIssue, AcceptanceGap, Unknown
- **Retry policy**: configurable max retries, exponential backoff, per-class behavior
- **Auto-fix**: generates targeted fix prompts for regressions and acceptance gaps
- **Red→green tracking**: detects when a previously-failing test suite recovers
- **Merge unblock**: automatically clears merge blocks when QA transitions to green

### Context Generation Observability (PR #72)

Full telemetry for the context generation pipeline:
- **Metrics**: generation count, success/failure, latency (min/max/avg), cache hits/misses
- **Token budget**: estimated prompt tokens, utilization percentage
- **Staleness**: per-file age assessment with configurable thresholds
- **Coverage**: scan repo files against generated context for gap detection
- **CLI**: `othala context-status` with `--json` support

### Delta-based Operator Reporting (PR #73)

Replaces noisy full-state dumps with meaningful change-only reports:
- **10 change types**: task state, task added/removed, model health, context gen, QA, pipeline, generation/merge/stop counts
- **Suppression policy**: configurable rate limiting, NO_REPLY for idle ticks, cooldown periods
- **DeltaReporter**: stateful tick processing, compares consecutive snapshots
- **Output**: colored terminal rendering + JSON schema
- Integrated into daemon loop (DaemonState.delta_reporter)

### Vault-driven Mission Completeness (PR #74)

Tracks requirement coverage across the task portfolio:
- **Requirement model**: parsed from markdown checklists with priority (Must/Should/Nice)
- **Coverage matrix**: Jaccard keyword overlap between requirements and tasks
- **Semantic dedup**: groups similar requirements, flags redundancy
- **Gap detection**: identifies uncovered requirements, suggests task titles
- **CLI**: `othala mission-status` with `--json` support

### E2E Orchestration Test Suite (PR #75)

Tests the orchestrator itself (not per-repo compile/test pipelines):
- **Scenario runner**: declarative scenarios with 13 step types
- **6 built-in scenarios**: happy path, agent retry, chaos crash, multi-task, verify loop, QA red→green
- **Chaos injection**: 7 fault types (AgentCrash, GraphiteFailure, ContextGenFailure, ModelHealthDrop, NetworkOutage, DiskFull, AgentHang)
- **Soak test framework**: configurable tick count, stuck-task detection, error rate limits, periodic progress reports
- **CLI**: `othala e2e-scenarios` / `othala e2e-scenarios --soak --chaos`

## Test Summary

| Module | New Tests |
|--------|-----------|
| wizard.rs | 7 |
| graphite_agent.rs | 3 |
| qa_self_heal.rs | 15 |
| context_gen_telemetry.rs | 18 |
| delta_report.rs | 25 |
| mission_vault.rs | 21 |
| e2e_scenarios.rs | 25 |
| **Total new** | **114** |

All 59 binary tests + 845+ lib tests continue to pass.

## Breaking Changes

None. All new features are additive. Existing CLI commands, config formats, and APIs are unchanged.

## Upgrade Path

1. Update to latest binary: `./scripts/install-latest.sh`
2. Run readiness check: `othala wizard --check-only`
3. Address any remediation hints
4. Run E2E scenarios: `othala e2e-scenarios`
5. Start daemon: `othala daemon`

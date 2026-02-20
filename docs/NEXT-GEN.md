# Othala Next-Gen: Multi-Agent Orchestration

## Status: Sprint 1 Complete (2h mark)

### Commits
1. `5c1d0b3` - Core infrastructure (agent dispatch, problem classifier, context manager, E2E, metrics)
2. `160689a` - Sisyphus error recovery loop

### New Modules (6 total, ~4000 LOC)

| Module | Purpose | Tests |
|--------|---------|-------|
| `agent_dispatch.rs` | Routes tasks to optimal agent (Hephaestus/Sisyphus/Librarian/Explorer) | 8 |
| `problem_classifier.rs` | Classifies errors for routing (compile/logic/permission/network) | 8 |
| `context_manager.rs` | Rich context passing to agents (history, blockers, assumptions) | 6 |
| `e2e_tester.rs` | Post-merge E2E test harness | 6 |
| `orchestration_metrics.rs` | Per-task and per-agent effectiveness tracking | 5 |
| `sisyphus_recovery.rs` | Sisyphus-in-the-loop error recovery | 6 |

**Total: 39 new tests, all passing**

### Key Differentiators

1. **Multi-Agent Team**
   - Hephaestus (Codex): Code generation, implementation
   - Sisyphus (Claude Opus): Deep thinking, error recovery
   - Librarian (Claude Sonnet): Documentation, review
   - Explorer (Claude Haiku): Quick fixes, exploration

2. **Sisyphus Error Recovery Loop**
   - When task → STOPPED: spawn Sisyphus with full context
   - Sisyphus diagnoses root cause, implements fix
   - Auto-retry; escalate after 2 failed Sisyphus rounds
   - This is what makes Othala *better* than Sisyphus alone

3. **Problem Classification**
   - Compile errors → Hephaestus
   - Logic errors (test failures) → Sisyphus
   - Permission errors → Human escalation
   - Network errors → Wait and retry

4. **Smart Context**
   - Task lineage (prior attempts, models used, failures)
   - Repo-specific knowledge (blockers, assumptions, decisions)
   - Error pattern matching (similar past errors and resolutions)

5. **E2E Testing Gate**
   - Compile → lint → unit → integration pipeline
   - Per-repo `.othala/e2e-spec.toml` configuration
   - Block merge on E2E failure (override for non-prod)

### Next Steps (Sprint 2)

1. **Daemon Integration** - Wire new modules into `daemon_loop.rs`
2. **Repo Configs** - Create `.othala/e2e-spec.toml` for all repos
3. **Verify Commands** - Split fast/heavy verify
4. **Live Testing** - Test against real repos

### Agent Role Matrix

| Task Type | Primary | Fallback |
|-----------|---------|----------|
| Implementation | Hephaestus | Sisyphus |
| Bug Fix (simple) | Hephaestus | Explorer |
| Bug Fix (complex) | Sisyphus | Hephaestus |
| Documentation | Librarian | Explorer |
| Code Review | Librarian | Sisyphus |
| Error Recovery | Sisyphus | - |
| Quick Fix | Explorer | Hephaestus |

### Metrics We'll Track

- **Time to merge** (avg, median)
- **E2E pass rate** per repo
- **Agent success rates** (first attempt vs recovery)
- **Error class distribution**
- **Sisyphus recovery effectiveness**

### Validation Targets (8h mark)

1. ✅ Proof: Task routed to correct agent based on intent
2. ⏳ Proof: Error → Sisyphus spawned → fixed → merged
3. ⏳ Dashboard: Before/after metrics comparison

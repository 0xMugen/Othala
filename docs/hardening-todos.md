# Othala Hardening TODOs (pre-share)

## 1) Graphite edge-case resilience
- Deterministic recovery playbooks for auth/trunk/restack failures.
- Zero retry-burn loops; explicit terminal reasons when not auto-healable.
- Safe auto-heal path for trunk sync + checked-out/staged worktree conflicts.

## 2) Perpetual planner quality
- Improve vault+repo task ideation quality (avoid low-value churn).
- Introduce scoring/filtering: impact, confidence, blast radius, testability.
- Require concrete acceptance + verification in generated keep-hot tasks.

## 3) Mode semantics end-to-end
- Enforce mode inheritance on new tasks (`stack` vs `merge`) everywhere.
- Add migration/reconciliation for legacy mixed-mode tasks.
- Ensure stack mode continues on top of awaiting parents without merge gating.

## 4) Release-grade observability
- Turn reliability snapshots into actionable KPIs:
  - recovery success rate
  - submit/merge latency
  - stuck-duration SLA
  - false-alert rate
  - idea-quality hit rate
- Provide compact operator status summary from these KPIs.

## Exit criteria
- 48h run, no critical stalls.
- >95% auto-recovery on known failure classes.
- 0 false merged/awaiting transitions during soak.
- Keep-hot lane maintained across active repos.

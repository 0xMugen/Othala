#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Othala E2E Test — runs a REAL task through the full pipeline with live AI.
#
# Usage:
#   ./scripts/e2e-test.sh [--model claude|codex|gemini] [--timeout 600]
#
# What it does:
#   1. Creates a real chat task via CLI
#   2. Starts the daemon (spawns actual AI agent)
#   3. Waits for the agent to complete, QA to pass, pipeline to finish
#   4. Verifies the task reached a terminal state
#   5. Reports pass/fail with details
#
# Requirements:
#   - Run from the Othala repo root
#   - Inside `nix develop` shell (or wrap with `nix develop --command`)
#   - At least one AI model available (claude/codex/gemini)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

MODEL="${MODEL:-claude}"
TIMEOUT="${TIMEOUT:-600}"
VERIFY_CMD="${VERIFY_CMD:-cargo check --workspace}"
TASK_TITLE="${TASK_TITLE:-E2E test: add is_submitting helper to TaskState}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log() { echo -e "${CYAN}[e2e]${NC} $*" >&2; }
pass() { echo -e "${GREEN}${BOLD}PASS${NC} $*" >&2; }
fail() { echo -e "${RED}${BOLD}FAIL${NC} $*" >&2; }
warn() { echo -e "${YELLOW}WARN${NC} $*" >&2; }

# Parse args
while [[ $# -gt 0 ]]; do
	case "$1" in
	--model)
		MODEL="$2"
		shift 2
		;;
	--timeout)
		TIMEOUT="$2"
		shift 2
		;;
	--title)
		TASK_TITLE="$2"
		shift 2
		;;
	--verify)
		VERIFY_CMD="$2"
		shift 2
		;;
	*)
		echo "Unknown arg: $1"
		exit 1
		;;
	esac
done

OTHALA="cargo run -p orchd --bin othala --"

# ─── Pre-flight checks ───────────────────────────────────────────────────────

log "Pre-flight checks..."

if ! command -v cargo &>/dev/null; then
	fail "cargo not found. Run inside nix develop."
	exit 1
fi

if ! command -v "$MODEL" &>/dev/null; then
	warn "$MODEL CLI not found in PATH — daemon may fail to spawn agent"
fi

if ! git rev-parse --is-inside-work-tree &>/dev/null; then
	fail "Not inside a git repository"
	exit 1
fi

log "Building othala binary..."
cargo build -p orchd --bin othala 2>&1 | tail -3

# ─── Step 1: Create the task ─────────────────────────────────────────────────

log "Creating task: ${TASK_TITLE}"
TASK_JSON=$($OTHALA chat new --repo othala --title "$TASK_TITLE" --model "$MODEL" --json 2>/dev/null)
TASK_ID=$(echo "$TASK_JSON" | jq -r '.id')
BRANCH=$(echo "$TASK_JSON" | jq -r '.branch_name // empty')
WORKTREE=$(echo "$TASK_JSON" | jq -r '.worktree_path // empty')

if [[ -z "$TASK_ID" || "$TASK_ID" == "null" ]]; then
	fail "Failed to create task. Output: $TASK_JSON"
	exit 1
fi

log "Task created: ${BOLD}$TASK_ID${NC}"
log "  Branch:   $BRANCH"
log "  Worktree: $WORKTREE"

# ─── Step 2: Verify initial state ────────────────────────────────────────────

STATE=$($OTHALA status "$TASK_ID" --json 2>/dev/null | jq -r '.state')
if [[ "$STATE" != "CHATTING" ]]; then
	fail "Expected initial state CHATTING, got: $STATE"
	exit 1
fi
pass "Initial state is CHATTING"

# ─── Step 3: Run the daemon ──────────────────────────────────────────────────

log "Starting daemon (model=$MODEL, timeout=${TIMEOUT}s, exit-on-idle)..."
log "  Verify command: $VERIFY_CMD"

DAEMON_LOG=$(mktemp /tmp/othala-e2e-daemon-XXXX.log)
$OTHALA daemon \
	--timeout "$TIMEOUT" \
	--exit-on-idle \
	--skip-context-gen \
	--verify-command "$VERIFY_CMD" \
	>"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

log "Daemon started (PID=$DAEMON_PID, log=$DAEMON_LOG)"

cleanup() {
	if kill -0 "$DAEMON_PID" 2>/dev/null; then
		log "Cleaning up daemon (PID=$DAEMON_PID)..."
		kill "$DAEMON_PID" 2>/dev/null || true
		wait "$DAEMON_PID" 2>/dev/null || true
	fi
}
trap cleanup EXIT

# ─── Step 4: Poll until completion ───────────────────────────────────────────

log "Polling task state..."
POLL_INTERVAL=5
ELAPSED=0
LAST_STATE=""

while kill -0 "$DAEMON_PID" 2>/dev/null; do
	STATE=$($OTHALA status "$TASK_ID" --json 2>/dev/null | jq -r '.state // "UNKNOWN"')

	if [[ "$STATE" != "$LAST_STATE" ]]; then
		log "  State: ${BOLD}$STATE${NC} (${ELAPSED}s elapsed)"
		LAST_STATE="$STATE"
	fi

	case "$STATE" in
	MERGED | AWAITING_MERGE)
		pass "Task reached $STATE after ${ELAPSED}s"
		break
		;;
	STOPPED)
		fail "Task STOPPED (agent exhausted retries or was killed)"
		REASON=$($OTHALA status "$TASK_ID" --json 2>/dev/null | jq -r '.last_failure_reason // "unknown"')
		log "  Failure reason: $REASON"
		break
		;;
	esac

	sleep "$POLL_INTERVAL"
	ELAPSED=$((ELAPSED + POLL_INTERVAL))
done

# Wait for daemon to finish if still running
if kill -0 "$DAEMON_PID" 2>/dev/null; then
	log "Waiting for daemon to exit..."
	wait "$DAEMON_PID" 2>/dev/null || true
fi

# ─── Step 5: Collect results ─────────────────────────────────────────────────

log ""
log "═══════════════════════════════════════════════════════════════"
log "  E2E TEST RESULTS"
log "═══════════════════════════════════════════════════════════════"

FINAL_STATE=$($OTHALA status "$TASK_ID" --json 2>/dev/null | jq -r '.state // "UNKNOWN"')
RETRY_COUNT=$($OTHALA status "$TASK_ID" --json 2>/dev/null | jq -r '.retry_count // 0')

log "  Task ID:     $TASK_ID"
log "  Final State: $FINAL_STATE"
log "  Model:       $MODEL"
log "  Retries:     $RETRY_COUNT"
log "  Duration:    ${ELAPSED}s"
log ""

# Check branch has commits beyond the initial empty commit
if [[ -n "$BRANCH" ]] && git rev-parse --verify "$BRANCH" &>/dev/null; then
	COMMIT_COUNT=$(git log "main..$BRANCH" --oneline 2>/dev/null | wc -l)
	log "  Commits on $BRANCH: $COMMIT_COUNT"
	if [[ "$COMMIT_COUNT" -gt 1 ]]; then
		pass "Branch has agent commits"
	else
		warn "Branch has no agent commits beyond initial empty commit"
	fi
fi

# Check QA results
if [[ -d ".othala/qa/results" ]]; then
	QA_FILES=$(find .othala/qa/results -name "*.json" -newer ".orch/state.sqlite" 2>/dev/null | wc -l)
	log "  QA result files: $QA_FILES"
else
	warn "No QA results directory found"
fi

# Check events
if [[ -d ".orch/events" ]]; then
	EVENT_COUNT=$(wc -l <".orch/events/global.jsonl" 2>/dev/null || echo 0)
	log "  Events logged: $EVENT_COUNT"
fi

log ""

# Final verdict
case "$FINAL_STATE" in
AWAITING_MERGE | MERGED)
	pass "E2E test PASSED — task completed successfully"
	EXIT_CODE=0
	;;
STOPPED)
	fail "E2E test FAILED — task stopped after $RETRY_COUNT retries"
	EXIT_CODE=1
	;;
CHATTING)
	fail "E2E test FAILED — task still chatting (timeout?)"
	EXIT_CODE=1
	;;
*)
	fail "E2E test FAILED — unexpected final state: $FINAL_STATE"
	EXIT_CODE=1
	;;
esac

log ""
log "Daemon log: $DAEMON_LOG"
log "To inspect: $OTHALA status $TASK_ID"
log "To delete:  $OTHALA delete $TASK_ID"

exit "$EXIT_CODE"

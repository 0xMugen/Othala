#!/usr/bin/env bash
# Generate orchestration metrics snapshot
# Usage: ./scripts/metrics-snapshot.sh [output_dir]

set -euo pipefail

OUTPUT_DIR="${1:-logs/metrics}"
mkdir -p "$OUTPUT_DIR"

TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
SNAPSHOT_FILE="$OUTPUT_DIR/snapshot-$(date -u +"%Y%m%d-%H%M%S").json"

echo "Generating metrics snapshot at $TIMESTAMP"

# Collect state counts from SQLite
STATE_COUNTS=$(sqlite3 -json .othala/state.db 2>/dev/null <<'SQL' || echo '[]'
SELECT state, COUNT(*) as count
FROM tasks
GROUP BY state
ORDER BY state
SQL
)

# Collect recent events
RECENT_EVENTS=$(sqlite3 -json .othala/state.db 2>/dev/null <<'SQL' || echo '[]'
SELECT kind, COUNT(*) as count
FROM events
WHERE at > datetime('now', '-1 hour')
GROUP BY kind
ORDER BY count DESC
SQL
)

# Build snapshot JSON
cat > "$SNAPSHOT_FILE" << EOF
{
  "timestamp": "$TIMESTAMP",
  "state_counts": $STATE_COUNTS,
  "recent_events": $RECENT_EVENTS,
  "metrics": {
    "verify_fast_available": $([ -f ".othala/verify-fast" ] && echo "true" || echo "false"),
    "e2e_spec_available": $([ -f ".othala/e2e-spec.toml" ] && echo "true" || echo "false"),
    "next_gen_enabled": true
  }
}
EOF

echo "Snapshot saved to: $SNAPSHOT_FILE"

# Also append to JSONL log
echo "{\"timestamp\":\"$TIMESTAMP\",\"state_counts\":$STATE_COUNTS}" >> "$OUTPUT_DIR/snapshots.jsonl"

# Print summary
echo ""
echo "=== Orchestration Summary ==="
echo "Timestamp: $TIMESTAMP"
echo ""
echo "State Counts:"
echo "$STATE_COUNTS" | jq -r '.[] | "  \(.state): \(.count)"' 2>/dev/null || echo "  (no data)"
echo ""
echo "Recent Events (1h):"
echo "$RECENT_EVENTS" | jq -r '.[] | "  \(.kind): \(.count)"' 2>/dev/null || echo "  (no data)"

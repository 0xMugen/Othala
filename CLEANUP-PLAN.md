# MVP Cleanup Plan

## Overview

Simplify Othala to core MVP: chats that auto-submit to Graphite with clean stacking.

## State Simplification

### Current States (16)
```
Queued, Initializing, DraftPrOpen, Running, Restacking, RestackConflict,
VerifyingQuick, VerifyingFull, Reviewing, NeedsHuman, Ready, Submitting,
AwaitingMerge, Merged, Failed, Paused
```

### MVP States (6)
```
Chatting, Ready, Submitting, Restacking, AwaitingMerge, Merged
```

## Files to Modify

### orch-core/src/state.rs
- [ ] Reduce TaskState enum to 6 states
- [ ] Remove VerifyTier (just pass/fail)
- [ ] Remove ReviewStatus (no review gate for MVP)

### orch-core/src/types.rs
- [ ] Rename Task → Chat
- [ ] Simplify fields

### orch-core/src/config.rs
- [ ] Simplify org config
- [ ] Remove review policy settings
- [ ] Remove multi-tier verify settings

### orchd/src/service.rs
- [ ] Simplify state machine transitions
- [ ] Remove review gate logic
- [ ] Simplify verify to single pass/fail

### orchd/src/scheduler.rs
- [ ] Simplify scheduling logic
- [ ] Focus on dependency resolution

### orchd/src/main.rs
- [ ] Simplify CLI commands
- [ ] Rename task → chat

## Crates to Stub/Remove

### Remove (post-MVP)
- [ ] `orch-web/` — stub out, keep for later
- [ ] `orch-notify/` — stub out
- [ ] `orch-agents/src/setup.rs` — simplify to single model

### Simplify
- [ ] `orch-verify/` — inline simple verify
- [ ] `orch-git/` — merge into orch-graphite

## Tests to Update

All tests referencing old states need updating.

## Migration Path

1. Add new states alongside old
2. Update transitions
3. Remove old states
4. Update tests
5. Clean up dead code

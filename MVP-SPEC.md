# Othala MVP Specification

## Goal

A minimal AI coding orchestrator that:
1. Manages **chats** (AI coding sessions)
2. **Auto-submits** completed chats to Graphite
3. **Auto-restacks** when multiple PRs are ready (clean single stack diff)
4. **Chains chats** — new chats start on top of previous ready-to-merge PR
5. **Dependencies** — chat B can depend on chat A (starts when A finishes)

## Simplified State Machine

```
CHATTING → READY → SUBMITTING → AWAITING_MERGE → MERGED
              ↓
         RESTACKING (if stack needs rebase)
```

### States

| State | Description |
|-------|-------------|
| `CHATTING` | Active AI conversation working on code |
| `READY` | Chat complete, code verified, ready to submit |
| `SUBMITTING` | Submitting to Graphite |
| `RESTACKING` | Rebasing onto updated parent |
| `AWAITING_MERGE` | PR submitted, waiting for merge |
| `MERGED` | PR merged, done |

### Removed States (not MVP)

- ~~`QUEUED`~~ — chats start immediately
- ~~`INITIALIZING`~~ — folded into chat start
- ~~`DRAFT_PR_OPEN`~~ — no draft PRs, just working branch
- ~~`VERIFYING_QUICK`/`VERIFYING_FULL`~~ — simple verify pass/fail
- ~~`REVIEWING`~~ — no AI review gate for MVP
- ~~`NEEDS_HUMAN`~~ — handle as chat pause
- ~~`PAUSED`~~ — just stop the chat
- ~~`FAILED`~~ — retry or abandon

## Core Entities

### Chat

```rust
struct Chat {
    id: ChatId,
    repo_id: RepoId,
    title: String,
    branch: String,
    model: ModelKind,
    state: ChatState,
    depends_on: Vec<ChatId>,  // explicit dependencies
    parent_chat: Option<ChatId>,  // implicit: stacks on top of
    created_at: DateTime,
    completed_at: Option<DateTime>,
}
```

### Stack

Chats form a stack via `parent_chat`:

```
main
 └── chat-A (parent_chat: None)
      └── chat-B (parent_chat: A)
           └── chat-C (parent_chat: B)
```

When A merges, B automatically restacks onto main.

## Auto-Submit Flow

1. Chat completes (model says "done")
2. Run quick verify (cargo check, lint)
3. If pass → state = READY
4. Submit to Graphite: `gt submit`
5. If stack needs rebase: `gt restack`
6. State = AWAITING_MERGE

## Dependency Resolution

**Explicit:** `depends_on: [chat-X]` — this chat won't start until chat-X reaches MERGED.

**Implicit:** `parent_chat: chat-Y` — this chat's branch starts from chat-Y's HEAD, and will restack when Y merges.

## Simplified Crates

### Keep (simplified)
- `orch-core` — types, config (simplified)
- `orchd` — main daemon, scheduler (simplified)
- `orch-graphite` — Graphite CLI wrapper
- `orch-tui` — terminal UI (simplified)

### Remove or Stub
- `orch-verify` — inline into orchd, just `cargo check`
- `orch-agents` — MVP: single model per chat, no multi-model orchestration
- `orch-web` — defer to post-MVP
- `orch-notify` — defer to post-MVP
- `orch-git` — fold into orch-graphite

## Config (Simplified)

```toml
# org.toml
[models]
default = "claude"

[graphite]
auto_submit = true

[verify]
command = "cargo check && cargo test"
```

## CLI Commands

```bash
othala                        # TUI
othala chat new --title "..."  # Start new chat
othala chat list              # List chats
othala chat status <id>       # Status of chat
othala daemon                 # Run orchestration loop
```

## Success Criteria

1. Can start a chat that creates code on a branch
2. Chat completion auto-submits to Graphite
3. Starting a second chat stacks on first
4. When first merges, second auto-restacks
5. Can specify chat B depends on chat A

# Vault-driven prompting

Use vault notes to create concrete Othala tasks.

## Inputs
- Project notes from vault-human project pages
- Daily notes with bugs, feature requests, blockers
- QA gaps and test incidents

## Prompt shape

Use this template:

```text
Task: <single clear objective>
Context: <concise vault facts>
Acceptance:
- <check 1>
- <check 2>
Verification command:
<repo specific command>
Signals:
- [patch_ready] when ready
- [needs_human] when blocked
```

## Escalation rule

If missing data for safe implementation:
1. mark blocked with exact missing input,
2. send Telegram ask-to-SSH message,
3. include one-line command list for inspection.

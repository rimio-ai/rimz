# Kiro CLI protocol reference

> RimZ's verified mapping is in [adapter_kiro.md](../../internals/agents/adapter_kiro.md). This document records the pinned upstream surface and stock Kiro CLI 2.12.1 evidence.

This reference targets the early-access v3 engine selected by `kiro-cli chat --v3`, not the older embedded engine. Verification baseline: **2026-07-13**, Kiro CLI **2.12.1**, authenticated stock interactive TUI.

## Upstream sources

| Surface | Official source |
| --- | --- |
| v3 overview and compatibility | <https://kiro.dev/docs/cli/v3/> · <https://kiro.dev/docs/cli/v3/feature-overview/> |
| v3 hooks and triggers | <https://kiro.dev/docs/cli/v3/hooks/> · <https://kiro.dev/docs/hooks/types/> |
| capability permissions | <https://kiro.dev/docs/cli/v3/permissions/> |
| sessions and context | <https://kiro.dev/docs/cli/chat/session-management/> · <https://kiro.dev/docs/cli/chat/context/> |
| models and credits | <https://kiro.dev/docs/cli/models/> · <https://kiro.dev/docs/cli/billing/related-questions/> |
| configuration and `KIRO_HOME` | <https://kiro.dev/docs/cli/chat/configuration/> · <https://kiro.dev/docs/cli/reference/settings/> |

## Launch and process surface

Fresh interactive launch is `kiro-cli chat --v3`; exact resume is `kiro-cli chat --v3 --resume-id <sess_uuid>`. The installed launcher runs `kiro-cli`, while the v3 engine can appear as `kiro-cli-chat`. `kiro-cli-term` is the shell-integration daemon and does not identify an agent pane.

Kiro accepts chat-level model and effort flags and the interactive `/compact` command. `/rewind` is interactive-only. ACP and `--no-interactive` remain separate supervised-client contracts rather than implicit substitutes for the stock TUI.

## Verified stock session store

Kiro writes the stock v3 session under:

```text
${KIRO_HOME:-~/.kiro}/sessions/
  <first 16 lowercase hex characters of sha256(exact absolute cwd bytes)>/
    sess_<uuid>/
      session.json
      messages.jsonl
```

Observed `session.json` fields include `id`, `schemaVersion: "1.0.0"`, `dataModelVersion: 1`, `workspacePaths`, `createdAt`, and `lastModifiedAt`; `status` is absent in the valid newborn file and appears after activity. The `id` matches the directory basename. Kiro creates this cwd-scoped metadata and a zero-byte `messages.jsonl` roughly two seconds after launch, before the first prompt.

Each `messages.jsonl` line carries `{id,timestamp,payload}`. Observed payload types are:

- `user` with `content`;
- `assistant` with `operationType` and `content`;
- `turn_start` and `turn_end` with `executionId`;
- `pending_interaction` and `interaction_resolved` keyed by `toolCallId`;
- `tool_call` and `tool_result` keyed by `toolCallId`;
- `session_metadata` with `key: "contextUsage"` and numeric `value.usagePercentage`;
- `usage_summary` with credit-denominated prompt-turn summaries;
- `session_event` with `category: "session_pause"` and success context;
- `steering_inclusion` and `session_start` internal/bootstrap records.

Physical order is authoritative. In the captured successful turn, `session_start` arrived after assistant output, usage, pause, and `turn_end`; timestamp sorting or treating `session_start.content` as conversation text would corrupt the transcript.

The captured approval turn ordered `pending_interaction(tool_approval)` → `interaction_resolved` → approved `tool_call(fs_write)` → successful `tool_result` → assistant `Say` → successful pause/turn end. This proves visible waiting and acting transitions. It does not establish failure, rejection, or cancellation shapes.

`usage_summary` reports credits. Kiro v3 exposes no authoritative machine-readable token totals, context-window size, active model, or session USD, and credit meaning is not universal, so RimZ exposes none of those figures. Only the explicit context percentage is mapped.

## Excluded session classes

`${KIRO_HOME:-~/.kiro}/sessions/cli/<session-id>.history` is readline history: it contains submitted prompt and slash-command text without assistant output, timestamps, context, or tool results. It is exclusion evidence rather than the structured transcript.

UUID-only ACP/v2 JSON and JSONL paths describe a different session class and are excluded. `KIRO_ACP_RECORD_PATH`, manual transcript exports, and pane capture are opt-in or point-in-time diagnostics rather than durable stock-session truth.

## Negative hook and supervised evidence

Kiro CLI 2.12.1 did not execute attempted user or project standalone hook configurations for `SessionStart`, `UserPromptSubmit`, `PostToolUse`, or `Stop`. No command invocation or stdin payload was observed, and the CLI exposed no validation command that made the configuration reproducible. RimZ therefore installs no Kiro hooks.

Pulled store records support live display and history but cannot block on completion or guarantee final output. `rimz agents kiro -p` remains unsupported until one executable transport proves permission handling, cancellation, exact completion, output, exit status, transcript retention, and session identity with fixtures.

## Evidence boundary

Expand claims only with redacted stock captures. Preserve physical record order and prove any new schema version, error/cancel boundary, tool vocabulary, permission outcome, or account surface independently. Hook execution, pulled transcript state, supervised execution, and spend remain separate capabilities.

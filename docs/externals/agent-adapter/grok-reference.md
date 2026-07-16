# Grok Build protocol reference

> The agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md) and the account/spend contract is [providers.md](../../internals/agents/providers.md). This document describes the upstream surface for a Grok Build adapter; it does not claim that `AgentKind::Grok` or an internal Grok mapping has landed.

This is the single home for the **Grok Build upstream protocol surface** RimZ can bind to: native lifecycle hooks, their JSON envelope and decision schema, durable session sidecars, structured headless output, Agent Client Protocol (ACP), authentication, and billing extensions. It mirrors the open-source Grok Build tree and its bundled user guide at the pinned revision below so a contributor can refresh it when upstream moves.

Coverage is **depth on the recommended stock-TUI seam, breadth as an index**. Native file hooks and the sidecars they identify preserve Grok's own terminal UI and carry the lifecycle RimZ needs. Headless JSON and ACP are documented as separate launch modes, not silently substituted for the user's TUI.

## Upstream sources

This mirror was refreshed from Grok Build `0.1.220-alpha.4` at commit [`c68e39f60462f28d9be5e683d9cbe2c57b1a5027`](https://github.com/xai-org/grok-build/tree/c68e39f60462f28d9be5e683d9cbe2c57b1a5027).

| Surface | Source |
| --- | --- |
| Product entry, binary names, and repository layout | [README](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/README.md) |
| Authentication | [user guide: authentication](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md) |
| Hooks, discovery, trust, compatibility, and command wire | [user guide: hooks](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-pager/docs/user-guide/10-hooks.md) |
| Hook event types and payload fields | [hook event source](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-hooks/src/event.rs) |
| Hook discovery and cross-source deduplication | [hook discovery source](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-hooks/src/discovery.rs) |
| Hook dispatch and command/HTTP runners | [dispatcher](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-hooks/src/dispatcher.rs), [command runner](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-hooks/src/runner/command.rs), [HTTP runner](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-hooks/src/runner/http.rs) |
| Hook fire sites and attention notifications | [turn lifecycle](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/session/acp_session_impl/turn.rs), [tool calls](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_calls.rs), [notification projection](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/session/acp_session_impl/hook_dispatch.rs) |
| Compaction and subagent hook fire sites | [compaction](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/session/compaction.rs), [session updates](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/session/acp_session_impl/updates.rs) |
| Sessions, resume, fork, rewind, and on-disk files | [user guide: sessions](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-pager/docs/user-guide/17-sessions.md), [session persistence](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/session/persistence.rs), [JSONL storage](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/session/storage/jsonl/mod.rs) |
| Session context and activity signals | [signals source](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/session/signals.rs) |
| Headless flags, output, spend, and exits | [user guide: headless mode](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-pager/docs/user-guide/14-headless-mode.md) |
| ACP launch modes and extension catalog | [user guide: agent mode](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-pager/docs/user-guide/15-agent-mode.md) |
| ACP client-registered hook wire | [client hook registration](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/extensions/hooks.rs), [client hook dispatch](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/session/acp_session/hooks.rs) |
| Subagent behavior | [user guide: subagents](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-pager/docs/user-guide/16-subagents.md) |
| Permissions and trust | [user guide: permissions](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md) |
| Auth-store schema | [auth model source](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/auth/model.rs) |
| ACP billing extension | [billing extension source](https://github.com/xai-org/grok-build/blob/c68e39f60462f28d9be5e683d9cbe2c57b1a5027/crates/codegen/xai-grok-shell/src/extensions/billing.rs) |

## Recommended adapter shape

Use native file hooks for lifecycle, then enrich the rollup from the session directory named by `transcriptPath`. Keep pane-process liveness as the presence backstop. Use structured headless output only for RimZ-supervised `-p` runs, and use ACP only when RimZ explicitly launches and owns an ACP client session.

| RimZ need | Preferred Grok surface | Notes |
| --- | --- | --- |
| Strong session identity | `sessionId` on every hook | A UUID; stable across resume, compaction, rewind, and model changes |
| Registration and termination | `SessionStart` / `SessionEnd` hooks | `SessionEnd` is process-session termination, not a turn end |
| Turn bracket | `UserPromptSubmit` / `Stop` hooks | `StopFailure` precedes an error `Stop` |
| Proof of work and edits | `PostToolUse` hook | Classify edits from typed tool names such as `search_replace` and `apply_patch` |
| Awaiting-user attention | `Notification` hook | Structured notification type and message identify tool permission, plan approval, question, and diff review waits |
| Compaction bracket | `PreCompact` / `PostCompact` hooks | `source` is `manual` or `auto` |
| Child agents | `SubagentStart` / `SubagentStop` hooks | Use `subagentId` as child identity and the envelope `sessionId` as parent identity |
| Model and effort | `summary.json` | `SessionStart.modelId` and `agentType` are currently omitted by the stock fire site |
| Live context fill | last `_meta.totalTokens` in `updates.jsonl` | Divide by the active model context window; tolerate a torn trailing line |
| Completed-turn counters | `signals.json` | A durable but turn-boundary snapshot, so it can lag live work |
| Supervised result and spend | `--output-format streaming-json` | The terminal `end` or `error` record carries the final run projection |
| Auth presence and owner | `auth.json` metadata | Redact `key` and `refresh_token`; hash secrets only when a stable account key needs them |
| Billing windows | ACP `x.ai/billing` when RimZ owns ACP | The stock TUI exposes `/usage`, but no standalone documented machine-readable command |

Native hooks observe attention but do not answer it. Keep structured native answers absent from the stock-TUI adapter until Grok exposes a tested responder seam; route human text through RimZ's pane send path. ACP reverse requests can carry typed answers only in the separately owned ACP mode.

## Executable and launch modes

Official installations expose `grok`; a source build produces `xai-grok-pager`. `grok --version` is the capability probe.

| Mode | Invocation | Surface |
| --- | --- | --- |
| Interactive TUI | `grok [flags]` | Native terminal UI plus file hooks |
| Headless | `grok -p <prompt> --output-format streaming-json` | NDJSON result stream plus the same in-process file hooks |
| ACP stdio | `grok agent stdio` | JSON-RPC over stdio |
| ACP WebSocket server | `grok agent serve --bind <addr> --secret <secret>` | JSON-RPC over a hosted WebSocket transport |
| Headless over hosted ACP | `grok agent headless --grok-ws-url <url>` | Headless client connected to an ACP server |

`--model`, `--reasoning-effort` / `--effort`, `--permission-mode`, `--allow`, `--deny`, `--sandbox`, and `--cwd` apply to the interactive and headless surfaces. `--yolo` enables always-approve. Preserve the user's launch flags and cwd when RimZ wraps a TUI session.

## Session identity, resume, and fork

Grok sessions use UUID identifiers. New native IDs are UUIDv7; `--session-id <uuid>` accepts a caller-selected UUID only for creation and fails when the ID is invalid or already exists under the target session directory.

| Operation | TUI | Headless | Identity effect |
| --- | --- | --- | --- |
| New | normal launch or `/new` (`/clear` alias) | default `grok -p` or `--session-id <uuid>` | Creates a new session ID |
| Resume | `/resume` or launch resume flow | `--resume <id>` | Reuses the existing session ID |
| Continue latest | resume flow | `--continue` | Reuses the latest session for the current cwd |
| Fork | `/fork [--worktree\|--no-worktree] [directive]` | `--resume <id> --fork-session` or `--continue --fork-session` | Creates a new ID and records `parent_session_id` |
| Rewind | `/rewind` | no dedicated headless flag | Retains the ID, truncates the active conversational branch, and restores tracked files |
| Compact | `/compact` or automatic threshold | automatic during a headless run | Retains the ID |

The session directory is:

```text
${GROK_HOME:-~/.grok}/sessions/<URL-encoded-cwd>/<session-id>/
```

`summary.json` makes the directory resumable. Important files include:

| File | Role |
| --- | --- |
| `summary.json` | Session identity, cwd, model, effort, title, timestamps, fork metadata, and agent name |
| `updates.jsonl` | Durable ACP and xAI update stream; this is the path exported as `transcriptPath` |
| `chat_history.jsonl` | Raw model-facing conversation history |
| `signals.json` | Completed-turn session counters and context snapshot |
| `plan.json` / `plan_mode.json` | Plan content and plan-mode lifecycle state |
| `rewind_points.jsonl` | Rewind/checkpoint metadata |
| `subagents/` | Child-session material |

## Native file hooks

A native hook is a command or HTTPS endpoint Grok invokes at a lifecycle point. Command hooks receive one JSON object on stdin. Only `PreToolUse` consumes a blocking decision from stdout; passive-event stdout is ignored.

### Discovery, order, and trust

Grok merges all enabled sources rather than selecting one file:

| Scope | Sources |
| --- | --- |
| Global | `~/.grok/hooks/*.json`, configured Claude-compatible global settings including `~/.claude/settings.json`, `~/.cursor/hooks.json`, and plugin hooks |
| Project | `<workspace>/.grok/hooks/*.json`, project Claude-compatible settings, `<workspace>/.cursor/hooks.json`, and project plugin hooks |

Global hooks run before project hooks. Directory entries are sorted lexicographically, earlier configured sources run first within a scope, and handlers run sequentially in registry order. The first explicit deny ends a blocking chain.

Grok deduplicates an identical `(event, command_raw, url_raw, matcher)` across sources and keeps the first occurrence, so a global definition wins over its project duplicate. The registry is a point-in-time snapshot; a new session sees disk edits, and the TUI's explicit reload refreshes the active session.

Project hook, MCP, and LSP execution is gated by folder trust stored in `~/.grok/trusted_folders.toml` and managed through `/hooks-trust` or `--trust`. Global hooks are trusted. Install a RimZ hook in the native, bounded `~/.grok/hooks/rimz.json` source rather than depending on a project grant.

Grok scans Claude and Cursor hook files for compatibility. A RimZ adapter must distinguish its Grok envelope from any Claude or Cursor hook payload that reaches the same helper; shared filenames alone do not identify the provider.

The trust hash covers every executable source Grok can load: native and compatibility hook commands/URLs, plugin hooks, MCP/LSP commands, permission configuration, and any other command-producing Grok config.

### Configuration shape

Each native JSON file carries a `hooks` object. An event maps to ordered matcher groups, and each group contains ordered handlers:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "bin/safety-check.sh",
            "timeout": 10,
            "env": { "POLICY": "strict" }
          }
        ]
      }
    ]
  }
}
```

`type` is `command` or `http`; HTTP handlers use `url` instead of `command`. `timeout` is seconds and defaults to 5. Relative command paths resolve from the hook source directory, while commands execute with the workspace root as cwd.

Matchers are regular expressions over the resolved tool name for `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, and `PermissionDenied`, and over `notificationType` for `Notification`. `SessionStart`, `SessionEnd`, `Stop`, and `UserPromptSubmit` reject matchers; other lifecycle events ignore them. MCP meta-dispatch resolves to `server__tool`, so match the qualified name.

Claude-style aliases remain active alongside the original expression:

| Alias | Native tool |
| --- | --- |
| `Bash` | `run_terminal_command` |
| `Read` | `read_file`, `hashline_read` |
| `Edit`, `MultiEdit` | `search_replace`, `hashline_edit` |
| `Write` | `write`, `search_replace`, `hashline_edit` |
| `Grep` | `grep`, `hashline_grep` |
| `Glob`, `ListDir` | `list_dir` |
| `WebSearch` | `web_search` |
| `Task` | `spawn_subagent` |

The pinned tree's compatibility table maps `Bash` to `run_terminal_command`, while its standard Grok Build tool bundle exposes `run_terminal_cmd`. Hook dispatch sends the actual wire tool name. Match `Bash|run_terminal_cmd` when a hook must cover both profiles; do not assume the compatibility alias covers an unlisted native spelling.

### Common input envelope

All event names serialize in snake_case inside a camelCase envelope:

```json
{
  "hookEventName": "pre_tool_use",
  "sessionId": "019e0000-0000-7000-8000-000000000001",
  "cwd": "/workspace/project",
  "workspaceRoot": "/workspace/project",
  "timestamp": "2026-07-16T12:00:00Z",
  "transcriptPath": "/home/user/.grok/sessions/%2Fworkspace%2Fproject/019e.../updates.jsonl",
  "clientIdentifier": "optional client label",
  "promptId": "optional current prompt id",
  "toolName": "run_terminal_command",
  "toolUseId": "call-1",
  "toolInput": { "command": "cargo check" },
  "toolInputTruncated": false,
  "permissionMode": "default"
}
```

`transcriptPath`, `clientIdentifier`, and `promptId` are omitted when unavailable. The stock shell currently leaves `clientIdentifier` absent. Session hooks and compaction/subagent lifecycle can lack `promptId`; turn and tool hooks carry the active prompt ID. Current `permissionMode` labels are `plan`, `bypassPermissions`, `auto`, and `default`.

`transcriptPath` points to `updates.jsonl` only after that file exists. Treat it as an optional discovery hint and derive the same directory from `sessionId` plus cwd when necessary.

`toolInput` and `toolResult` are each capped at 128 KiB of serialized JSON. An oversized value becomes a truncated string and its paired `*Truncated` field is true.

### Event catalog and payloads

| Event | Fires | Event-specific fields |
| --- | --- | --- |
| `SessionStart` | a new or resumed session starts | `source`, optional `modelId`, optional `agentType` |
| `UserPromptSubmit` | the user prompt enters the turn | optional `prompt` |
| `PreToolUse` | before tool execution; the only blocking hook | `toolName`, `toolUseId`, `toolInput`, `toolInputTruncated`, optional `permissionMode`, optional `subagentType` |
| `PostToolUse` | after successful tool execution | pre-tool fields plus `toolResult`, `toolResultTruncated`, optional `durationMs`, `isBackgrounded` |
| `PostToolUseFailure` | after tool execution errors | tool fields plus `error` |
| `PermissionDenied` | the permission system has denied a tool | tool name, ID, input, and truncation flag |
| `Notification` | a user-attention or agent-error notification is raised | `notificationType`, optional `message`, `title`, `level` |
| `StopFailure` | a turn ends with an API/runtime error | `error` |
| `Stop` | any turn finishes | `reason` |
| `SubagentStart` | a child begins | `subagentId`, `subagentType`, optional `description` |
| `SubagentStop` | a child finishes | start fields plus optional `exitCode`, `durationMs`; `SubagentEnd` is a configuration alias |
| `PreCompact` | context compaction begins | `source` (`manual` or `auto`) |
| `PostCompact` | context compaction succeeds | `source` (`manual` or `auto`) |
| `SessionEnd` | the session actor terminates | `reason`, optional `turnCount`, optional `toolCallCount` |

The current `SessionStart` fire site sends `source = "new"` for an empty history and `source = "load"` otherwise, with no `modelId` or `agentType`; read `summary.json.current_model_id` and `summary.json.agent_name` instead. Current normal turn-end reasons are `end_turn`, `cancelled`, and `error`. `MaxTurnsReached` folds into `cancelled`.

On actor shutdown Grok emits `SessionEnd(reason = "channel_closed" | "shutdown")` and then a same-reason `Stop`. Once `SessionEnd` tombstones the session, ignore that trailing turn marker for lifecycle state.

`StopFailure` fires before `Stop(reason = "error")` and carries the error text. Preserve it as turn context, then close the turn on `Stop`.

### RimZ lifecycle mapping

| Grok evidence | RimZ lifecycle evidence |
| --- | --- |
| `SessionStart` | `Registered` with strong `sessionId` identity |
| `UserPromptSubmit` | `TurnStarted` |
| `PostToolUse` | `ToolUsed`; mark `edited = true` for typed edit tools, including `search_replace` and `apply_patch` |
| `Notification(permission_prompt, "Tool permission requested")` | `AwaitingInput(Permission)` |
| `Notification(permission_prompt, "Plan approval requested")` | `AwaitingInput(PlanApproval)` |
| `Notification(elicitation_dialog, "User question requested")` | `AwaitingInput(Question)` |
| `Notification(permission_prompt, "Diff review requested")` | `AwaitingInput(Permission)` |
| `Notification(agent_error, …)` | error context; wait for `Stop` to close the turn |
| `Stop(reason = "end_turn")` | clean `TurnEnded` |
| `Stop(reason = "cancelled")` | `TurnInterrupted` |
| `StopFailure` then `Stop(reason = "error")` | errored `TurnEnded` with the failure detail |
| `PreCompact` / `PostCompact` | `Compacting` / `CompactionEnded` with `auto` derived from `source` |
| `SubagentStart` / `SubagentStop` | `SubagentStarted` / `SubagentStopped` |
| `SessionEnd` | `Ended` |

`PermissionDenied` reports a completed denial and does not prove a pending prompt. The `Notification` event is the attention signal.

The subagent lifecycle envelope is emitted by the parent session actor: use `subagentId` as the child `agent_id` and the envelope's `sessionId` as `parent_agent_id`. Tool hooks running inside a child use the child's session ID and may carry `subagentType`. `SubagentStop.exitCode` is 0 for `completed`, 1 for `failed`, -1 for `cancelled`, and absent for an unknown status.

### Decisions and exit codes

For `PreToolUse`, stdout accepts:

```json
{ "decision": "allow" }
```

or:

```json
{ "decision": "deny", "reason": "Unsafe command detected" }
```

| Exit | Blocking result |
| --- | --- |
| `0` | Parse JSON when present; empty output allows |
| `2` | Deny fallback |
| other | Record a hook failure and allow |

A JSON decision overrides the process exit code. Timeouts, crashes, malformed output, and missing variables fail open. Passive hook stdout and exit-code decisions are observational only.

Command hooks receive these reserved variables: `GROK_HOOK_EVENT`, `GROK_HOOK_NAME`, `GROK_SESSION_ID`, `GROK_WORKSPACE_ROOT`, and the compatibility alias `CLAUDE_PROJECT_DIR`. Plugin hooks also receive `GROK_PLUGIN_ROOT` and `GROK_PLUGIN_DATA` plus compatibility aliases. Handler `env` values cannot override reserved variables.

The command runner uses direct execution for simple argv and a shell for syntax that needs expansion or composition. It bounds captured stdout and stderr to 64 KiB each. Hook stdout remains the decision channel; diagnostics belong on stderr.

HTTP hooks POST the same envelope as JSON. They require HTTPS, resolve and reject private, link-local, and cloud-metadata addresses, and permit loopback addresses. A 2xx empty response allows; valid JSON `allow` or `deny` decides regardless of status. Invalid JSON on 2xx and transport failures fail open, while an empty or malformed non-2xx response records a failure and also fails open at dispatch.

## Durable session sidecars

Sidecars enrich lifecycle after the hook establishes `sessionId`. Read them best-effort and keep hooks plus process liveness authoritative for live status.

### `summary.json`

`summary.json` is rewritten with a temp file plus rename under a stable sidecar lock. Parse the structured object and tolerate added fields.

| Field | Use |
| --- | --- |
| `info.id`, `info.cwd` | Session identity and original workspace cwd |
| `current_model_id` | Live persisted model, including mid-session model switches |
| `reasoning_effort` | `none`, `minimal`, `low`, `medium`, `high`, or `xhigh` when set |
| `agent_name` | Active named agent/profile |
| `created_at`, `updated_at`, `last_active_at` | Creation, metadata update, and content activity clocks |
| `generated_title`, `title_is_manual`, `session_summary` | Display title metadata |
| `parent_session_id`, `forked_at`, `session_kind` | Fork and child lineage |
| `sandbox_profile` | Effective persisted sandbox profile |
| `num_messages`, `num_chat_messages` | Persisted message counts |

Use `current_model_id` rather than the optional hook `modelId`. Use the model catalog or configured model metadata to obtain the context-window denominator.

### `updates.jsonl`

Each line is a durable update envelope:

```json
{
  "timestamp": 1784203200,
  "method": "session/update",
  "params": {
    "sessionId": "019e...",
    "update": {
      "sessionUpdate": "agent_message_chunk",
      "content": { "type": "text", "text": "Done" }
    },
    "_meta": {
      "totalTokens": 42000,
      "eventId": "019e...-42",
      "agentTimestampMs": 1784203200123,
      "promptId": "prompt-1",
      "streamStartMs": 1784203199000,
      "turnStartMs": 1784203198500
    }
  }
}
```

`method` is `session/update` for standard ACP updates and `_x.ai/session/update` for Grok extensions. Standard update variants include `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`, and `plan`; the extension catalog is open-ended.

Every current ordinary ACP update carries `_meta.totalTokens`, a monotonically allocated `eventId`, and `agentTimestampMs`. `promptId`, stream/turn clocks, update descriptors, and replay/chunk fields are conditional. Extension updates carry event IDs but do not necessarily carry `totalTokens`.

For live context, scan backward for the last parseable `params._meta.totalTokens`; it is the estimated active context, not cumulative usage. Bound the tail read, tolerate unknown update types, and ignore a torn final JSONL record after a crash. Rewind markers define the live branch, so transcript rendering must apply Grok's rewind semantics rather than concatenate every historic line.

### `signals.json`

`signals.json` serializes `SessionSignals` in camelCase. Useful fields are:

| Field | Meaning |
| --- | --- |
| `turnCount`, `assistantMessageCount` | Completed bookkeeping counters |
| `errorCount`, `toolFailureCount`, `cancellationCount` | Error and interruption counters |
| `compactionCount`, `totalTokensBeforeCompaction` | Compaction history |
| `contextWindowUsage`, `contextTokensUsed`, `contextWindowTokens` | Context snapshot and denominator |
| `toolCallCount`, `toolsUsed` | Tool activity |
| `modelsUsed`, `primaryModelId` | Model history and current primary model |
| `sessionDurationSeconds` | Session duration at the last sync |

The file is a completed-turn snapshot and can lag a running, failed, or cancelled turn. Prefer `updates.jsonl` for live context and hooks for live activity. The signals file carries no authoritative spend or billing amount.

## Headless structured output

`-p`, `--prompt-json`, or `--prompt-file` selects headless mode. Use `--output-format streaming-json` for RimZ-supervised runs; it preserves incremental output and a structured terminal record.

The stream is NDJSON:

```json
{"type":"text","data":"Fixed"}
{"type":"thought","data":"Checking the tests"}
{"type":"end","stopReason":"EndTurn","sessionId":"019e...","requestId":"req-1","num_turns":7,"usage":{"input_tokens":7210,"cache_read_input_tokens":41000,"output_tokens":1893,"reasoning_tokens":412,"total_tokens":50103},"modelUsage":{"grok-build":{"inputTokens":7210,"outputTokens":1893,"cacheReadInputTokens":41000,"modelCalls":7,"costUSD":0.01268905}},"total_cost_usd":0.01268905,"total_cost_usd_ticks":126890500}
```

| Type | Meaning |
| --- | --- |
| `text` | Assistant response chunk |
| `thought` | Reasoning chunk |
| `end` | Successful terminal record with session/result metadata and available spend |
| `error` | Failure record with `message` and any frozen spend fields |
| `max_turns_reached`, `auto_compact_*` | Current extension events; treat the catalog as non-exhaustive |

`--output-format json` emits the terminal projection once; `plain` emits human text. The `end` object is the last successful streaming event.

Usage fields follow these rules:

- `usage.input_tokens` is uncached input; `cache_read_input_tokens` is cache-hit input; `total_tokens = input_tokens + cache_read_input_tokens + output_tokens`.
- `reasoning_tokens` is a component of output accounting and does not add again to `total_tokens`.
- `num_turns` counts main-agent model rounds recorded on the prompt ledger; subagent calls stay in `modelUsage.*.modelCalls`.
- `modelUsage` groups tokens, calls, and complete cost by model.
- `total_cost_usd_ticks` uses 10,000,000,000 ticks per USD and is the reconciliation-safe amount.
- Complete cost fields are omitted when upstream did not report cost, when `cost_is_partial` is true, or when `usage_is_incomplete` makes the aggregate unsafe. Absence means unknown, not free.
- A prompt that never reaches the model omits spend fields.

Headless exits 0 on success, 1 on error, 130 on SIGINT, and 143 on SIGTERM. The same native hooks run inside headless mode, so use hooks for consistent lifecycle and the terminal stream for the supervised result.

## Agent Client Protocol

ACP is the structured embedding mode. It uses JSON-RPC and the standard `initialize`, `session/new`, `session/prompt`, `session/update`, and `session/load` cycle. Grok extends it with methods under `x.ai/*` for session information, hooks, permissions, questions, plan approval, auth, billing, and other product features.

ACP exposes the richest typed interaction surface, including permission reverse requests plus `x.ai/ask_user_question` and `x.ai/exit_plan_mode`. Use those only when RimZ launches and owns the ACP client; an ACP controller is a different product surface from observing the user's normal TUI.

### Client-registered hooks

An ACP client can register callbacks in `session/new` metadata:

```json
{
  "_meta": {
    "x.ai/hooks": {
      "PreToolUse": [
        {
          "matcher": "Bash",
          "hookCallbackIds": ["rimz-pre-tool"],
          "timeout": 5
        }
      ],
      "Stop": [
        { "hookCallbackIds": ["rimz-stop"] }
      ]
    }
  }
}
```

`PreToolUse` dispatches one awaited reverse request per matching callback through `x.ai/hooks/run`. Every other event sends fire-and-forget `x.ai/hooks/event` notifications. The payload is the native hook envelope plus `hookCallbackId`.

The blocking response is:

```json
{ "decision": "deny", "systemMessage": "Policy blocked this call" }
```

`decision` is `continue` or `deny`; unknown and malformed values fail open. Callback gates run concurrently. Each callback has its own timeout, defaults to 30 seconds, and is capped at 300 seconds. The first observed deny blocks the tool.

Client hooks supplement file hooks rather than replacing them. Reconnect metadata updates registrations only when `x.ai/hooks` is present; an empty object explicitly clears them.

## Authentication and account surfaces

`grok login` starts browser OAuth by default; `grok logout` clears the active login. Grok hot-reloads `~/.grok/auth.json`. `XAI_API_KEY` is the fallback only when no active session token is available, and per-model configured keys take precedence for their model.

The auth store maps scopes to records. Relevant metadata includes `auth_mode`, `user_id`, `email`, `first_name`, `last_name`, profile/principal/team/organization fields, `expires_at`, and blocked-account state. Secret fields include `key` and `refresh_token`; never log, render, or persist them in RimZ state. Derive an opaque digest only when no stable non-secret account identifier exists.

Known `auth_mode` values are `web_login`, `oidc`, `external`, and `api_key`. Enterprise login can use an external provider or OIDC; first-party OAuth remains the normal interactive flow.

In ACP mode, `x.ai/auth/info` provides structured identity metadata to the connected client. `x.ai/billing` queries the authenticated Grok service and can return credit usage, current billing period, on-demand cap/usage, prepaid balance, unified-billing state, and subscription tier. Treat that extension as available to an owned ACP process, not as permission to extract a bearer token and reproduce Grok's private HTTP request.

The stock TUI presents `/usage`, but the pinned build documents no standalone machine-readable billing-status command. A TUI adapter therefore reports account presence/owner from redacted local metadata and leaves billing windows unavailable unless a supported owned-process surface supplies them.

## Adapter safety notes

- Parse every JSON surface structurally and tolerate unknown fields and event variants.
- Key lifecycle by `sessionId`; key child lifecycle by `(parent sessionId, subagentId)`.
- Preserve hook stdout for decisions and send diagnostics to stderr.
- Install a native Grok hook with an idempotent merge and preserve every pre-existing Grok, Claude-compatible, Cursor-compatible, and plugin hook.
- Include every Grok command-executing configuration surface in project trust and its stale diff.
- Read `summary.json` and `signals.json` as replaceable snapshots and `updates.jsonl` as an append stream with a tolerable torn tail.
- Keep pane-process liveness as the final presence check when hooks or sidecar reads lag.
- Keep file hooks passive for RimZ lifecycle observation; a lifecycle helper emits no deny decision.
- Redact prompt text, tool input/output, transcript content, hook `env`, URLs after environment expansion, and auth secrets from diagnostics.

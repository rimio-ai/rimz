# Qwen Code protocol reference

This is the single home for the **Qwen Code upstream protocol surface** relevant to RimZ: lifecycle hooks and their decision channel, live process/session identity, dual-output observation and control, statusline enrichment, session JSONL, authentication, subagents, resume and fork behavior, permission modes, and headless execution. It mirrors Qwen's published documentation and the open-source `QwenLM/qwen-code` wire types, with source links pinned for implementation work.

Coverage is **depth on viable adapter inputs, breadth as an index**. The hook, statusline, dual-output, runtime-sidecar, and transcript shapes are detailed enough to implement typed parsers. ACP and daemon mode are indexed so an implementer can choose them deliberately rather than confusing them with observation of a stock interactive pane.

## Refresh target and upstream sources

This mirror was refreshed against Qwen Code **0.19.10** at source commit [`095bd160918086a3a33192133e7923635f08f973`](https://github.com/QwenLM/qwen-code/tree/095bd160918086a3a33192133e7923635f08f973). Re-fetch the documentation and compare the linked types before implementation because Qwen Code is actively developing hooks, session persistence, subagents, dual output, and daemon mode.

| Surface | Source |
| --- | --- |
| Project, installation, feature overview | <https://github.com/QwenLM/qwen-code> |
| Hooks, events, payloads, outputs, execution | <https://qwenlm.github.io/qwen-code-docs/en/users/features/hooks/> |
| Hook wire types | [`packages/core/src/hooks/types.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/core/src/hooks/types.ts) |
| Hook runner, registry, and trust | [`hookRunner.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/core/src/hooks/hookRunner.ts), [`hookRegistry.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/core/src/hooks/hookRegistry.ts), [`trustedHooks.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/core/src/hooks/trustedHooks.ts) |
| Configuration layers, environment, settings | <https://qwenlm.github.io/qwen-code-docs/en/users/configuration/settings/> |
| Authentication and providers | <https://qwenlm.github.io/qwen-code-docs/en/users/configuration/auth/>, <https://qwenlm.github.io/qwen-code-docs/en/users/configuration/model-providers/> |
| Session commands and machine-readable listing | <https://qwenlm.github.io/qwen-code-docs/en/users/features/commands/> |
| Session JSONL writer and types | [`chatRecordingService.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/core/src/services/chatRecordingService.ts) |
| Session loader and active-branch reconstruction | [`sessionService.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/core/src/services/sessionService.ts) |
| Usage output normalization | [`tokenEstimation.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/core/src/services/tokenEstimation.ts) |
| Runtime PID/session sidecar | [`runtimeStatus.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/core/src/utils/runtimeStatus.ts) |
| Statusline JSON | <https://qwenlm.github.io/qwen-code-docs/en/users/features/status-line/> |
| Interactive dual-output protocol | <https://qwenlm.github.io/qwen-code-docs/en/users/features/dual-output/> |
| Dual-output implementation and protocol version | [`DualOutputBridge.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/cli/src/dualOutput/DualOutputBridge.ts) |
| Structured message types | [`packages/cli/src/nonInteractive/types.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/cli/src/nonInteractive/types.ts) |
| Headless mode and exits | <https://qwenlm.github.io/qwen-code-docs/en/users/features/headless/> |
| CLI option definitions | [`packages/cli/src/config/config.ts`](https://github.com/QwenLM/qwen-code/blob/095bd160918086a3a33192133e7923635f08f973/packages/cli/src/config/config.ts) |
| Permission modes | <https://qwenlm.github.io/qwen-code-docs/en/users/features/approval-mode/> |
| Subagents | <https://qwenlm.github.io/qwen-code-docs/en/users/features/sub-agents/> |
| ACP daemon/server mode | <https://qwenlm.github.io/qwen-code-docs/en/users/qwen-serve/> |

## Recommended adapter shape

Use **command hooks** as lifecycle truth. They carry session identity and transcript path on every event, bracket turns and compaction, report subagents and failures, and expose synchronous permission decisions while preserving the stock interactive TUI.

Use the **runtime sidecar** to bind a live pane process to its session before or independently of hook delivery. Use the **statusline command** for live model, context, token, and file-change enrichment. Use **session JSONL** for durable context and historical token/spend reconstruction.

Treat **dual output** as an optional structured pane sidecar, not the first lifecycle dependency. It gives real-time assistant/tool messages, typed permission requests, and a reverse prompt/permission channel, but a bad path, disconnected consumer, or full FIFO disables itself while the TUI continues. Hooks and the durable store remain correctness; dual output improves latency and can support native answers later.

| RimZ need | Primary surface | Backstop / note |
| --- | --- | --- |
| Session identity and registration | `SessionStart.session_id` | `<session>.runtime.json` binds PID to session directly |
| Turn start and prompt | `UserPromptSubmit.prompt` | dual-output `user` event |
| Clean / failed completion | `Stop` / `StopFailure` | completed assistant envelope and transcript tail |
| Tool work and acting phase | `PostToolUse` | dual-output tool use/result |
| Permission wait | `PermissionRequest` | dual-output `control_request`; notification is weaker evidence |
| User question / plan approval | `PreToolUse` tool classification | live-verify canonical tool ids |
| Compaction | `PreCompact` + `PostCompact` | `SessionStart(source = compact)` is extra close evidence |
| Subagents | `SubagentStart` + `SubagentStop` | child transcript path on stop |
| Model and context | command statusline | newest assistant transcript record |
| Session tokens/spend | session JSONL `usageMetadata` | statusline metrics are live cumulative enrichment |
| Auth/account | merged settings plus credential-source presence | no stable auth-status or provider-quota command |
| Supervised run | `-p --output-format stream-json` | preserve native exit code |
| Native resume/fork | `--continue`, `--resume`, `--fork-session` | direct |

Qwen Code was originally based on Gemini CLI v0.8.2 but has developed independently since Qwen Code v0.1. Do not reuse the legacy Gemini CLI event names, transcript schema, auth assumptions, or model-limit table merely because portions of the codebase retain Gemini naming.

## Session identity, resume, fork, and process binding

`qwen --continue` resumes the newest session for the current project. `qwen --resume <session-id>` resumes a specific session; bare `--resume` opens the interactive picker. `qwen --resume <id> --fork-session` and `qwen --continue --fork-session` copy the active conversation into a new session identity while leaving the source intact.

`--session-id <id>` assigns the identity for a run; version-gate it before RimZ relies on caller-assigned IDs. `/clear` ends the current identity and starts another. `/branch` forks the current conversation. `/rewind` changes the active history branch and can restore files; it is not a new session. `/compress` and `/compress-fast` compact history without changing the logical session.

`qwen sessions list --json [--limit N]` writes one JSON object per line with `sessionId`, `startTime`, `mtime`, `prompt`, `gitBranch`, `customTitle`, `titleSource`, `filePath`, and `cwd`. stderr carries the pagination hint.

### Runtime PID/session sidecar

Every interactive session atomically writes:

```text
<runtime-base>/projects/<sanitized-cwd>/chats/<session-id>.runtime.json
```

`<runtime-base>` is `QWEN_RUNTIME_DIR` when set, then the configured runtime output directory, then `QWEN_HOME`/`~/.qwen`. The schema is versioned independently:

```json
{
  "schema_version": 1,
  "pid": 43120,
  "session_id": "UUID",
  "work_dir": "/absolute/project/path",
  "hostname": "host",
  "started_at": 1783700000.125,
  "qwen_version": "0.19.10"
}
```

`started_at` is epoch seconds with sub-second precision and `qwen_version` may be `null`. A session or cwd/worktree transition refreshes the applicable sidecar.

The file intentionally remains after clean exit and crash. Verify that `pid` is alive and belongs to the pane's expected descendant process, then require `work_dir` and session location to agree with the workspace; PID reuse can otherwise select a stale sidecar. Unknown schema versions and malformed fields fail soft. Hooks still establish durable registration, while the sidecar makes pane association explicit without scraping argv or terminal text.

## Hooks

A command hook runs at a lifecycle point, receives one JSON object on **stdin**, and returns a decision object on **stdout**. Logs go to stderr. Hooks live under `hooks.<EventName>[]` in `settings.json`:

```json
{
  "hooks": {
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "rimz hooks qwen" }] }],
    "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "rimz hooks qwen" }] }]
  }
}
```

Each event entry accepts `matcher`, `sequential`, and `hooks`. Hooks run in parallel by default; `sequential: true` gives ordered execution. A command accepts `command`, optional `name`, `description`, `timeout` in milliseconds (default 60,000), `env`, `shell` (`bash` or `powershell`), `statusMessage`, and `async`.

HTTP and prompt hooks are executable surfaces but RimZ should install only a local command. HTTP hooks POST the same JSON and support URL/environment allowlists and SSRF checks. Prompt hooks spend a model call to produce `{ "ok": boolean, "reason"?, "additionalContext"? }`. Function hooks are session-internal rather than a public settings API.

`disableAllHooks: true`, `--safe-mode`, and safe/bare startup paths disable configured hooks. Project hooks require a trusted workspace; Qwen also records trusted project hook identifiers in `~/.qwen/trusted_hooks.json`. User, project, extension, and session hooks may all fire. Preserve unrelated entries, hash every executable field RimZ adds, and preflight the effective merged configuration.

### Common input

```json
{
  "session_id": "string",
  "transcript_path": "/absolute/path/to/session.jsonl",
  "cwd": "/current/working/directory",
  "hook_event_name": "SessionStart",
  "timestamp": "ISO 8601"
}
```

Subagent contexts additionally carry `agent_id` and `agent_type` where applicable. Parse `session_id` as required, retain the hook-provided absolute transcript path, and tolerate unknown fields. `permission_mode` is `default | plan | auto_edit | auto | yolo`; CLI spelling uses `auto-edit`, while hook JSON uses `auto_edit`.

### Output and exit semantics

```json
{
  "continue": true,
  "stopReason": "feedback when stopping",
  "suppressOutput": false,
  "systemMessage": "message for the session",
  "terminalSequence": "optional terminal sequence",
  "decision": "ask | block | deny | approve | allow",
  "reason": "decision explanation",
  "hookSpecificOutput": { "hookEventName": "PreToolUse", "additionalContext": "optional" }
}
```

Exit **0** parses stdout as JSON. Exit **2** is blocking: stdout is ignored and stderr becomes model feedback. Other nonzero exits are non-blocking and stderr appears only in debug mode. `StopFailure` ignores all outputs and exits. `PostCompact` output is logging-only. Async command hooks cannot control an operation that already continued.

The neutral RimZ path writes no logs to stdout and returns empty JSON or empty stdout with exit 0 after live verification. Golden-test the exact neutral bytes against the target release.

### Event catalog and implementation fields

| Event | Event-specific input | Decision / adapter use |
| --- | --- | --- |
| `SessionStart` | `permission_mode`, `source` (`startup|resume|clear|compact|branch`), `model`, optional `agent_type` | register; carry model and transcript |
| `UserPromptSubmit` | `prompt` | turn start; may block/add context |
| `UserPromptExpansion` | `command_name`, `command_args`, expanded `prompt` | index; may block/add context |
| `PreToolUse` | permission, tool name/input, `tool_use_id`, optional provider `tool_call_id` | wait classification; allow/deny/ask/update input |
| `PostToolUse` | same identity/input plus `tool_response` | completed work and edit phase |
| `PostToolUseFailure` | tool identity/input, `error`, optional `is_interrupt` | work/error enrichment; turn may continue |
| `PostToolBatch` | `permission_mode`, typed `tool_calls[]` | index; batch context/control |
| `PermissionRequest` | permission, tool name/input, optional suggestions | synchronous human wait or automatic decision |
| `PermissionDenied` | tool identity/input, `reason` (`classifier_blocked|classifier_unavailable`) | completed auto denial, not a wait |
| `Stop` | active flag, last assistant text, optional context, `background_tasks[]`, `crons[]` | clean turn end; detect background park |
| `MessageDisplay` | stable `message_id`, cumulative `displayed_text`, `is_final` | fire-and-forget display observation before `Stop` |
| `StopFailure` | typed `error`, optional details and last text | failed/paused evidence; fire-and-forget |
| `SubagentStart` | permission, `agent_id`, `agent_type` | child start |
| `SubagentStop` | start fields plus active flag, transcript, last text, tasks/crons | child stop; output can block |
| `PreCompact` | `trigger` (`manual|auto`), `custom_instructions` | compaction opener |
| `PostCompact` | `trigger`, `compact_summary` | compaction close; output cannot control |
| `SessionEnd` | `reason` (`clear|logout|prompt_input_exit|bypass_permissions_disabled|other`) | ended; liveness backstop |
| `Notification` | message/title/type (`permission_prompt|idle_prompt|auth_success|elicitation_dialog`) | attention enrichment; elicitation is not implemented |
| `InstructionsLoaded` | file, memory type, load reason, optional include-parent paths | index only |
| `TodoCreated` / `TodoCompleted` | todo data and `phase` (`validation|postWrite`) | validation may block; post-write cannot undo |

`Stop` context fields are optional: `context_usage` is a ratio and may exceed 1, `context_limit` is tokens, and `input_tokens` is the provider-normalized prompt count. The assistant JSONL write rides Qwen's serialized async recording queue, while `Stop` does not await a recorder flush; a hook can therefore observe the direct prompt count before the just-finished assistant record reaches disk. Correlate the latest transcript record by exact `promptTokenCount` before using its model, window, total, or category split. Background tasks carry `id`, `status`, `agent_type`, `started_at`, and optional `description`. `StopFailure.error` is `rate_limit | authentication_failed | billing_error | invalid_request | server_error | max_output_tokens | unknown`.

The key decisions are:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow | deny | ask",
    "permissionDecisionReason": "required explanation",
    "updatedInput": {},
    "additionalContext": "optional"
  }
}
```

`ask` opens the native confirmation. In headless runs and background subagents, it falls back to deny.

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PermissionRequest",
    "decision": {
      "behavior": "allow | deny",
      "updatedInput": {},
      "updatedPermissions": [],
      "message": "optional",
      "interrupt": false
    }
  }
}
```

`Stop` and `SubagentStop` block through top-level `decision: "block"` plus `reason`. A RimZ observation returns neutral output and lets the native UI own the decision.

### Native-event mapping for a first adapter

| Qwen observation | RimZ signal/enrichment | Notes |
| --- | --- | --- |
| `SessionStart` | `registered` | eager; carry model/transcript |
| `UserPromptSubmit` | `turn_started` | authoritative prompt boundary |
| ordinary `PostToolUse` | `tool_used { edits: false }` | proves work and clears waiting |
| structured editor `PostToolUse` | `tool_used { edits: true }` | live-verify canonical ids |
| `PermissionRequest` | `awaiting_input` | classify question/plan/permission by tool |
| question/plan `PreToolUse` | typed `awaiting_input` | needed if its dialog lacks PermissionRequest |
| `Stop` | clean `turn_ended` | preserve background park when tasks remain |
| `StopFailure` | errored `turn_ended` plus interruption | typed error refines retry/pause projection |
| subagent bracket | child start/stop | parent id is absent; correlate through root context/transcript |
| compact bracket | compacting/ended | trigger controls automatic/manual close |
| `SessionEnd` | ended | pane/process liveness still reaps |

Do not classify `PermissionDenied` as waiting. Use only structurally known file-edit tools for `edits: true`; shell remains work without typed edit proof. Live-capture the built-in tool ids, especially question and plan-exit tools, because the hooks reference gives examples rather than a versioned canonical catalog.

## Statusline JSON

Command mode at `ui.statusLine` runs a shell command, writes one JSON object to stdin, and renders up to two stdout lines. RimZ can wrap the user's command and forward stdin/stdout unchanged. The timeout is five seconds; event-driven updates are debounced 300 ms, and `refreshInterval` adds a timer with a one-second minimum.

```json
{
  "session_id": "UUID",
  "version": "0.19.10",
  "model": { "display_name": "[DeepSeek] deepseek-v4-pro" },
  "context_window": {
    "context_window_size": 1000000,
    "used_percentage": 3.9,
    "remaining_percentage": 96.1,
    "current_usage": 38727,
    "total_input_tokens": 30000,
    "total_output_tokens": 5000
  },
  "workspace": { "current_dir": "/work/project" },
  "git": { "branch": "main" },
  "worktree": {
    "name": "fix-auth",
    "path": "/work/project/.qwen/worktrees/fix-auth",
    "branch": "fix-auth",
    "original_cwd": "/work/project",
    "original_branch": "main"
  },
  "metrics": {
    "models": {
      "qwen3-coder-plus": {
        "api": { "total_requests": 10, "total_errors": 0, "total_latency_ms": 5000 },
        "tokens": { "prompt": 30000, "completion": 5000, "total": 35000, "cached": 10000, "thoughts": 2000 }
      }
    },
    "files": { "total_lines_added": 120, "total_lines_removed": 30 }
  },
  "vim": { "mode": "INSERT" }
}
```

`git`, `worktree`, and `vim` are absent when inactive. `current_usage` is the latest API call's whole prompt/context occupancy and is the numerator behind the live percentage; it is a scalar gauge, not uncached fresh input. `metrics.models` is keyed by every model used, so routing or `/model` changes can produce multiple entries.

Registry model names can carry a provider label such as `[DeepSeek] deepseek-v4-pro`. Qwen's preset renderer removes a leading `/^\[[^\]]*\]\s*/` decoration before showing the model. Consumers of command-mode JSON receive the decorated label and apply the same stripping rule before their own canonical model formatting.

Statusline is the preferred live enrichment channel because it supplies the upstream-selected context window instead of requiring a Qwen/provider limit table. Treat every field as optional and token categories as extensible. Hash the complete executable configuration, preserve the prior command, and keep wrapper diagnostics off stdout.

Preset statusline mode has no command or stdin surface. Leave it untouched and rely on transcript enrichment, or present a visible conversion workflow; never silently replace the user's preset.

## Interactive dual output

`qwen --json-file <path> --input-file <path>` leaves the TUI on stdio 0/1/2 while writing JSONL to a separate file/FIFO and polling a regular input file for commands. `--json-fd N` works for a plain child spawn, but PTY hosts generally cannot pass fd 3+, so tmux/Zellij panes require `--json-file`.

The first event is a capability handshake:

```json
{
  "type": "system",
  "subtype": "session_start",
  "uuid": "event UUID",
  "session_id": "session UUID",
  "data": {
    "session_id": "session UUID",
    "cwd": "/work/project",
    "protocol_version": 1,
    "version": "0.19.10",
    "supported_events": ["system", "user", "assistant", "stream_event", "result", "control_request", "control_response"]
  }
}
```

Feature-detect `protocol_version` and `supported_events`; older versions may omit them. The channel shares the headless `stream-json` schema and always includes partial messages:

```jsonc
{ "type": "user", "session_id": "...", "message": { "role": "user", "content": [] }, "parent_tool_use_id": null }
{ "type": "assistant", "uuid": "...", "session_id": "...", "message": { "id": "...", "role": "assistant", "model": "...", "content": [{ "type": "text", "text": "..." }], "stop_reason": null, "usage": { "input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 2, "total_tokens": 15 } }, "parent_tool_use_id": null }
{ "type": "stream_event", "event": { "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "fragment" } }, "session_id": "..." }
{ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "...", "content": "...", "is_error": false }] }, "parent_tool_use_id": null }
```

Assistant content blocks are `text`, `thinking`, or `tool_use`. Current messages contain one block category, so one model turn may emit multiple completed assistant envelopes as the category changes. `parent_tool_use_id` identifies subagent output when non-null. Do not treat every assistant envelope as the root turn's `Stop`; hooks own that boundary.

Permission control is explicit:

```json
{
  "type": "control_request",
  "request_id": "request UUID",
  "request": {
    "subtype": "can_use_tool",
    "tool_name": "run_shell_command",
    "tool_use_id": "tool id",
    "input": { "command": "..." },
    "permission_suggestions": null,
    "blocked_path": null
  }
}
```

The input file accepts:

```jsonc
{ "type": "submit", "text": "follow-up prompt" }
{ "type": "confirmation_response", "request_id": "request UUID", "allowed": true }
```

Submits queue until idle. Confirmations dispatch immediately; the first native or external answer wins, and late answers drop. A `control_response` reports success or error. This supports future boolean permission answers, while typed question/plan answers still require native dialog integration unless the protocol grows another control request.

`--input-file` must be a regular file because Qwen polls size every 500 ms; output may be a file or FIFO. Use per-session paths in a mode-0700 directory. A bad target, EPIPE, adapter exception, or more than 1 MiB buffered disables the bridge without stopping Qwen. Clean shutdown emits `system/session_end`; a closed stream without it is abnormal but does not prove the TUI died. Dual output is latency/control enrichment, never lifecycle truth.

## Session transcript JSONL

With `general.chatRecording` enabled (default), Qwen writes:

```text
<runtime-base>/projects/<sanitized-cwd>/chats/<session-id>.jsonl
```

Disabling recording also disables resume. Use the hook's `transcript_path` for the live file and `qwen sessions list --json` for historical discovery. Every append-only record is self-contained:

```jsonc
{
  "uuid": "record UUID",
  "parentUuid": "previous active record UUID or null",
  "sessionId": "session UUID",
  "timestamp": "ISO 8601",
  "type": "user | assistant | tool_result | system",
  "subtype": "optional typed event subtype",
  "cwd": "/project/root",
  "version": "0.19.10",
  "gitBranch": "main",
  "message": { "role": "user | model", "parts": [] },
  "usageMetadata": {},
  "model": "provider model id",
  "contextWindowSize": 131072,
  "toolCallResult": {},
  "systemPayload": {},
  "agentId": "optional child id",
  "agentName": "optional child name",
  "agentColor": "optional UI hint",
  "isSidechain": true,
  "externalInputKind": "message | notification",
  "forkedFrom": { "sessionId": "source session", "messageUuid": "source record" }
}
```

`message` is the Google `Content` shape Qwen uses internally: `role` plus `parts`, including `text`, `functionCall`, `functionResponse`, and thoughts. `toolCallResult` is extensible UI recovery metadata. Parse both structurally and tolerate unknown keys.

Assistant records carry `model`, optional `contextWindowSize`, and normalized `usageMetadata`: `promptTokenCount`, `candidatesTokenCount`, `totalTokenCount`, `cachedContentTokenCount`, `thoughtsTokenCount`, and `toolUsePromptTokenCount`. Use the newest active assistant's `totalTokenCount` as current request size and its `contextWindowSize` as divisor. Fall back to statusline rather than hard-coding a provider limit.

For historical usage, sum each active assistant once. `promptTokenCount` includes cached prompt tokens; price uncached input as saturating prompt minus cached and cached separately. Qwen's resume normalization requires prompt accounting, prefers saturating `totalTokenCount - promptTokenCount` for output, and falls back to candidates alone when candidates exceed thoughts or candidates plus thoughts otherwise. `toolUsePromptTokenCount` is already part of the reported prompt and does not add to a derived total. Qwen supports OpenAI-compatible, Anthropic, Gemini, Vertex, Qwen, and local providers, so model alone is insufficient to infer billing. Retain provider identity where available and leave dollars unknown when metering is not established.

### Active-branch and special-record semantics

`uuid`/`parentUuid` form the active conversation tree. Rewind appends `system/rewind` and re-roots subsequent parent links; abandoned descendants remain. Reconstruct the chain selected by the latest active tail rather than summing physical lines. Fork copies records to a new `sessionId`, rebuilds parents by write order, and adds `forkedFrom` metadata.

`system/chat_compression` stores `systemPayload.info` plus `systemPayload.compressedHistory`, the exact `Content[]` sent after compression. It changes resume history without erasing UI-visible records. Other current subtypes include `slash_command`, `ui_telemetry`, `at_command`, `attribution_snapshot`, `notification`, `cron`, `mid_turn_user_message`, `custom_title`, `parent_session`, `rewind`, `agent_bootstrap`, `agent_launch_prompt`, `file_history_snapshot`, `session_artifact_event`, and `session_artifact_snapshot`. Preserve unknown subtypes and exclude them from token totals unless they carry documented usage.

Writes are queued and flushed on orderly teardown. A hook can precede the newest transcript append, so lifecycle ingestion uses hook fields directly and transcript tailing stays enrichment. The sidecar and transcript can move when a session changes cwd; follow the newest hook/status path rather than pinning launch cwd.

## Subagents

Qwen exposes dedicated start/stop hooks, separate contexts, and child transcript paths. Declare `Capabilities.subagents` after a live fixture proves root/child delivery and parent association.

Named subagents start with isolated context and return inline. Fork subagents inherit the parent's conversation/system/tool prefix, run detached, and do not automatically feed results back. Fork children cannot recursively fork and currently share the parent's cwd.

Definitions live in project `.qwen/agents/`, user `~/.qwen/agents/`, and extensions. Markdown/YAML frontmatter can select model, approval mode, tools, disallowed tools, MCP servers, and hooks. Per-agent hooks are executable and belong in the trust hash. Current upstream warns they are session-registered but not scoped at firing time: concurrent agents' hooks can fire for one another.

`SubagentStart` supplies the child ID/type but no parent ID. `SubagentStop` adds its transcript. Live-verify whether root event context is sufficient, and inspect transcript `agentId`/`isSidechain` plus `parent_session` records for durable correlation. Never merge child/root merely because they share a session or cwd.

## Authentication, account, quota, and spend

Qwen is multi-provider. `security.auth.selectedType` selects a protocol/provider, `model.name` selects the model, and `modelProviders` defines endpoints and credential environment keys. Built-in types include `openai`, `anthropic`, `gemini`, `vertex-ai`, and discontinued `qwen-oauth`; custom provider ids map through `providerProtocol`.

Credential resolution is provider-specific. CLI `--openai-api-key` wins for OpenAI-compatible providers, followed by process environment, the first discovered `.qwen/.env` or `.env`, then settings `env`. Common variables are `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_MODEL`/`QWEN_MODEL`, `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, `ANTHROPIC_MODEL`, `GEMINI_API_KEY`, `GEMINI_MODEL`, `GOOGLE_API_KEY`, and `GOOGLE_MODEL`. Alibaba Coding Plan commonly uses `BAILIAN_CODING_PLAN_API_KEY` with its dedicated regional endpoint.

The removed `qwen auth status` prints a migration notice; `/doctor` is interactive. A first account probe reads only non-secret merged selection metadata and tests credential-source **presence**, never prints or persists key values. Report provider/model when determinable and fail soft for ADC, custom endpoints, and external secret managers.

Qwen OAuth's browser-login free tier was discontinued on 2026-04-15 and is no longer selectable. Legacy `~/.qwen/oauth_creds.json` may exist but is not a basis for new support.

Alibaba Coding Plan advertises weekly quota, but Qwen publishes no stable machine-readable remaining-quota API or CLI command. Other providers have different contracts. Omit balance/windows until implementing a provider-specific official API.

Local token insight is direct from statusline, headless results, and transcript. Dollar spend needs provider plus RimZ pricing; leave it unknown for subscription/quota plans, local models, unknown custom endpoints, and unclear billing categories.

## Headless and supervised runs

`qwen -p <prompt>` runs one headless turn. Stdin is prepended and `-p` appended. A positional query defaults to one-shot; `-i/--prompt-interactive` runs a prompt then stays interactive. RimZ passes `-p` explicitly for supervised runs and launches bare `qwen` for panes.

`--output-format json` buffers an array of messages. `stream-json` writes JSONL; add `--include-partial-messages` for deltas. The final result is:

```jsonc
{
  "type": "result",
  "subtype": "success | error_max_turns | error_during_execution",
  "uuid": "...",
  "session_id": "...",
  "is_error": false,
  "duration_ms": 1200,
  "duration_api_ms": 900,
  "num_turns": 2,
  "result": "final text",
  "usage": { "input_tokens": 100, "output_tokens": 20, "cache_read_input_tokens": 30, "total_tokens": 120 },
  "modelUsage": {},
  "permission_denials": [],
  "stats": {}
}
```

Error results carry an `error` object instead of result text. Preserve unknown fields. Documented exits include 0 success, 1 general/API failure, 42 invalid input, 53 maximum turns, 55 budget exceeded, and 130 SIGINT. Require a successful terminal result and process exit, preserving the process code as script verdict.

`--max-session-turns`, `--max-wall-time`, and `--max-tool-calls` bound headless work. Tool budget counts root dispatches but not subagent inner tools; terminal `structured_output` is exempt. `QWEN_CODE_UNATTENDED_RETRY=1` retries 429/529 indefinitely with capped backoff and stderr heartbeats; pair it with wall time. `--yolo` does not enable sandboxing.

## CLI and environment surface

| Surface | Meaning for RimZ |
| --- | --- |
| `qwen --version` | version probe |
| `qwen` | stock interactive pane |
| `qwen -i <prompt>` | prompt then interactive |
| `qwen -p <prompt>` | supervised run |
| `--continue` / `--resume <id>` | native resume |
| `--fork-session` | fork resumed history |
| `--session-id <id>` | caller-selected identity; version-gate |
| `--model <id>` | startup model override |
| `--system-prompt <text>` / `--append-system-prompt <text>` | direct prompt override/append; these accept text rather than file paths |
| `--approval-mode <plan|default|auto-edit|auto|yolo>` | permission mapping |
| `--yolo` | full auto-approval |
| `--allowed-tools` / `--exclude-tools` | confirmation bypass / tool removal |
| `--sandbox` / `QWEN_SANDBOX=1` | sandbox, separate from approval |
| `--include-directories` / `--add-dir` | extra workspace roots |
| `--worktree [slug|PR]` | upstream worktree, distinct from RimZ's |
| `--json-file`, `--input-file` | interactive structured channels |
| `--output-format <text|json|stream-json>` | supervised output |
| `QWEN_HOME` / `QWEN_RUNTIME_DIR` | config root / runtime root |
| `QWEN_CODE_SAFE_MODE=true` | disable customizations |
| `NO_COLOR` | suppress ANSI where supported |

Map RimZ suffixes as plan → `plan`, ask → `default`, auto → `auto-edit`, and yolo → `yolo`. Qwen's classifier-driven `auto` is distinct; expose it only through explicit launch args until RimZ defines cross-provider semantics.

Configuration precedence is defaults, system defaults, user settings, project settings, overriding system settings, environment/`.env`, then CLI. Preserve all layers and account for `QWEN_CODE_SYSTEM_DEFAULTS_PATH`, `QWEN_CODE_SYSTEM_SETTINGS_PATH`, `QWEN_HOME`, and `QWEN_RUNTIME_DIR`.

## ACP and daemon mode index

`qwen --acp` starts ACP over stdio. `qwen serve` hosts shared sessions over HTTP/SSE and owns ACP children. They provide structured prompts, permissions, session lifecycle, model changes, and replay to dedicated clients.

They do not observe an independent stock TUI pane. Adoption changes RimZ into a protocol host and adds daemon auth, reconnect, and ownership concerns. Keep them out of the first adapter; revisit for a programmatic runner or remote control after pane-first support is stable.

## Implementation checklist and live verification gaps

1. Add a typed `qwen` descriptor with eager hooks, native resume/fork, and subagents; use the runtime sidecar for PID/session binding.
2. Install one neutral command hook for `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionRequest`, `Stop`, `StopFailure`, `SubagentStart`, `SubagentStop`, `PreCompact`, `PostCompact`, and `SessionEnd`.
3. Preserve unrelated hooks, preflight disable/safe/bare/trust/configuration layers, and hash full executable hook/statusline definitions.
4. Parse around required session identity, retain unknowns, reserve stdout, and add golden fixtures for permissions, question, plan, background park, API error, compaction, clear, branch, and children.
5. Wrap command statusline without changing rendered stdout; leave preset intact and parse multi-model metrics as a map.
6. Fold JSONL by active parent chain, honor rewind/compression, retain unknown system subtypes, and exclude abandoned branches/child sidechains from root totals.
7. Use transcript context size plus newest total tokens for durable context and statusline for live context; never assume one limit across providers.
8. Drive `-p --output-format stream-json`, preserve exits, and test stdin ordering, resume/fork, budgets, denials, and partial messages.
9. Keep dual output optional: isolate paths, feature-detect v1, tolerate disablement, and add boolean answers only after security/race coverage.
10. Report provider/model plus credential-source presence without secrets; omit quota/balance where no stable official API exists.
11. Live-capture canonical edit/question/plan tool ids; verify every native dialog, Esc cancellation, shell activity, background parking, and failure coverage.
12. Verify subagent parent correlation, named/fork/background hook delivery, failure verdicts, transcript paths, depth, and parent tasks before enabling all capabilities.
13. Verify sidecars across clear, branch, fork, worktree, cwd change, crash, PID reuse, and runtime-root overrides; require process liveness.
14. Compare the target release's `HookEventName`, `ChatRecord`, dual-output protocol, CLI options, and statusline payload. The published hook event table trails the source enum in places, so source-backed compatibility fixtures are required.

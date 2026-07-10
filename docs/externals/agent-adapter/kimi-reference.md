# Kimi Code CLI protocol reference

> RimZ does not yet ship a Kimi Code CLI adapter. This document records the upstream surfaces needed to build one. The agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md), and the account, balance, spend, and pricing contract is [providers.md](../../internals/agents/providers.md).

This is the single home for the **MoonshotAI/Kimi Code CLI upstream protocol surface** relevant to RimZ: lifecycle hooks, durable Wire events, session identity and storage, structured approvals and questions, context and token usage, authentication and quota, subagents, resume and fork behavior, permission modes, and non-interactive execution.

Coverage is **depth on viable adapter inputs, breadth as an index**. The hook payloads, `wire.jsonl` envelopes, context records, and blocking-request shapes are detailed enough to implement typed parsers. Wire server mode and ACP are indexed so an implementer can distinguish them from observation of the stock interactive CLI.

## Refresh target and product identity

This mirror was refreshed against Kimi Code CLI **1.48.0**, Wire protocol **1.10**, at source commit [`2c34efbbc6c7cfe40770623281e87c138ff8eb6c`](https://github.com/MoonshotAI/kimi-cli/tree/2c34efbbc6c7cfe40770623281e87c138ff8eb6c). `kimi info --json` reports `kimi_cli_version`, `agent_spec_versions`, `wire_protocol_version`, and `python_version`; use that command for runtime feature gates.

The executable and PyPI distribution remain `kimi` and `kimi-cli`. Upstream now calls this Python product **Kimi Code CLI** and is replacing it with the separate next-generation [`MoonshotAI/kimi-code`](https://github.com/MoonshotAI/kimi-code) product. The old CLI's `/upgrade` command installs the successor and migrates configuration and sessions. Treat the two implementations as separate adapter kinds until their native protocols have been compared and version-gated; this document describes `MoonshotAI/kimi-cli`, not the successor and not a Kimi model version.

## Upstream sources

Re-fetch the published pages and compare the pinned source when refreshing this mirror. The docs describe the supported interface; source links resolve details that the docs omit.

| Surface | Source |
| --- | --- |
| Repository, install, and migration notice | <https://github.com/MoonshotAI/kimi-cli> |
| CLI options and subcommands | <https://moonshotai.github.io/kimi-cli/en/reference/kimi-command.html> |
| Hooks | <https://moonshotai.github.io/kimi-cli/en/customization/hooks.html> |
| Hook payload builders | [`hooks/events.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/hooks/events.py) |
| Hook execution and decision parser | [`hooks/runner.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/hooks/runner.py), [`hooks/engine.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/hooks/engine.py) |
| Sessions and context | <https://moonshotai.github.io/kimi-cli/en/guides/sessions.html> |
| Data locations | <https://moonshotai.github.io/kimi-cli/en/configuration/data-locations.html> |
| Session lookup and creation | [`session.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/session.py), [`metadata.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/metadata.py) |
| Session fork and turn truncation | [`session_fork.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/session_fork.py) |
| Wire protocol | <https://moonshotai.github.io/kimi-cli/en/customization/wire-mode.html> |
| Wire types and persisted envelopes | [`wire/types.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/wire/types.py), [`wire/file.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/wire/file.py) |
| Context JSONL parser and writer | [`soul/context.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/soul/context.py), [`kosong/message.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/packages/kosong/src/kosong/message.py) |
| Print mode, JSONL, and exit codes | <https://moonshotai.github.io/kimi-cli/en/customization/print-mode.html> |
| Providers and model configuration | <https://moonshotai.github.io/kimi-cli/en/configuration/providers.html>, <https://moonshotai.github.io/kimi-cli/en/configuration/config-files.html> |
| Environment overrides | <https://moonshotai.github.io/kimi-cli/en/configuration/env-vars.html> |
| Agents, tools, and subagents | <https://moonshotai.github.io/kimi-cli/en/customization/agents.html> |
| Approval runtime | [`soul/approval.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/soul/approval.py) |
| OAuth credential storage and refresh | [`auth/oauth.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/auth/oauth.py) |
| Kimi Code quota endpoint and response parser | [`ui/shell/usage.py`](https://github.com/MoonshotAI/kimi-cli/blob/2c34efbbc6c7cfe40770623281e87c138ff8eb6c/src/kimi_cli/ui/shell/usage.py) |
| ACP server | <https://moonshotai.github.io/kimi-cli/en/reference/kimi-acp.html> |

## Recommended adapter shape

Keep the stock interactive `kimi` process in the pane. Use **command hooks** to register session identity and deliver low-latency lifecycle boundaries. After `SessionStart`, derive and tail that session's **durable `wire.jsonl`** for typed turn, work, blocking-request, compaction, context, and subagent observations. Use **`context.jsonl`** for transcript/context reconstruction and the last `_usage` record as a restart backstop.

Hooks alone cannot implement full attention routing: they expose no approval-request event, no structured question payload, no model, no transcript path, and no subagent ID. The durable Wire log supplies all of those except the live model name. Conversely, the Wire log is session-scoped and does not announce its own session ID, so the `SessionStart` hook supplies the binding.

| RimZ need | Primary surface | Backstop / note |
| --- | --- | --- |
| Pane-to-session binding | `SessionStart.session_id` + `cwd` | derive the session directory from `kimi.json`; do not guess from newest mtime |
| Turn start and prompt | Wire `TurnBegin.user_input` | `UserPromptSubmit.prompt` is earlier and string-only |
| Turn boundary close | Wire `TurnEnd` | combine with `StopFailure`, `StepInterrupted`, and retry evidence to classify outcome |
| Failed completion | `StopFailure` hook + Wire `StepInterrupted` / `StepRetry` | `StopFailure.error_type` is an exception class name, not a stable enum |
| Acting / tool work | Wire `ToolCall` + `ToolResult` | hooks provide `PreToolUse`, `PostToolUse`, and failures |
| Permission wait | Wire `ApprovalRequest` | no corresponding command hook exists |
| User question | Wire `QuestionRequest` | typed request from `AskUserQuestion` |
| Plan approval | Wire `PlanDisplay` followed by `ApprovalRequest` | correlate by tool call; `ExitPlanMode` is the native tool |
| Compaction | Wire `CompactionBegin` / `CompactionEnd` | hooks add trigger and before/after token counts |
| Context fill and step tokens | Wire `StatusUpdate` | latest `context.jsonl` `_usage` is input-context backstop |
| Subagents | Wire `SubagentEvent` | hooks omit `agent_id` and parent tool-call ID |
| Model | effective config plus launch `--model` | model changes rewrite default config and reload the same session |
| Auth/account | managed provider + OAuth credential presence | never expose token bytes |
| Kimi Code quota | authenticated `GET <managed-base>/usages` | Kimi Code platform only; no spend/cost field |
| Supervised run | `--print --output-format=stream-json` | preserve native exit `0`, `1`, or `75` |
| Native resume | `--session <id>` / `--resume <id>` | `--continue` selects the worktree's most recent session |
| Native fork | interactive `/fork` | no documented non-interactive fork flag |

Treat hook delivery and Wire tailing as at-least-once observation inputs. Deduplicate lifecycle facts by session, native type, stable request/tool ID where present, and persisted record offset. Parse unknown fields and unknown Wire message types forward-compatibly.

## Executable, version, and process binding

`kimi` with no mode flag starts the stock shell UI. Relevant launch flags are:

| Flag | Meaning |
| --- | --- |
| `--version`, `-V` | print CLI version |
| `kimi info --json` | print CLI, agent-spec, Wire-protocol, and Python versions |
| `--work-dir`, `-w` | set the project directory |
| `--add-dir <path>` | extend workspace scope; repeatable and persisted in session state |
| `--model`, `-m` | select a configured model alias for this launch |
| `--agent <name>` | select built-in `default` or `okabe` agent |
| `--agent-file <path>` | load a custom agent specification |
| `--plan` | start or resume with plan mode forced on |
| `--yolo`, `-y` | auto-approve tools while keeping `AskUserQuestion` interactive |
| `--afk` | auto-approve tools and auto-dismiss questions |
| `--continue`, `-C` | resume the current working directory's latest session |
| `--session [id]`, `--resume [id]`, `-S`, `-r` | resume ID, create that ID if absent, or show picker with no ID |
| `--prompt`, `-p`, `--command`, `-c` | submit one prompt |
| `--print` | non-interactive mode and runtime AFK behavior |
| `--wire` | JSON-RPC Wire server over stdio; experimental |

The CLI publishes no PID/session sidecar and its normal process argv does not contain a newly generated session ID. Bind the pane process through the hook's `session_id`, then keep pane/process liveness as instance truth under RimZ's normal rules.

`/new`, `/sessions`, `/fork`, `/undo`, `/model`, and UI-switch commands reload the in-process session surface. The old instance executes `SessionEnd`; the replacement executes `SessionStart`. Model switching persists `default_model` and `default_thinking`, then reloads the same session ID.

## Configuration and trust surface

The default application root is `~/.kimi`; `KIMI_SHARE_DIR` replaces that root. The default config is `~/.kimi/config.toml`, while `--config-file` and `--config` select another file or inline TOML/JSON. A RimZ hook installer must edit the effective file deliberately and preserve unrelated `[[hooks]]` entries.

Hooks are executable configuration:

```toml
[[hooks]]
event = "SessionStart"
command = "rimz hooks kimi"
timeout = 10

[[hooks]]
event = "UserPromptSubmit"
command = "rimz hooks kimi"
timeout = 10

[[hooks]]
event = "PreToolUse"
matcher = ".*"
command = "rimz hooks kimi"
timeout = 10
```

Each hook definition has `event`, `command`, optional regex `matcher` (empty matches all), and `timeout` in seconds (default 30, accepted range 1–600). Commands execute through the platform shell with the session project directory as cwd. Multiple matching hooks run in parallel; identical command strings are deduplicated for one trigger.

The current implementation exposes no documented project-level hook tier or hook trust prompt: hooks come from the loaded application config. Include every installed command and its timeout/matcher in RimZ's trust hash, preview the exact config diff, and keep hook stdout reserved for the upstream decision channel.

## Session identity and durable files

The share directory contains:

```text
~/.kimi/                              # or KIMI_SHARE_DIR
├── config.toml
├── kimi.json
├── credentials/
├── sessions/
│   └── <work-dir-key>/
│       └── <session-id>/
│           ├── context.jsonl
│           ├── wire.jsonl
│           ├── state.json
│           └── subagents/<agent-id>/...
└── logs/kimi.log
```

`kimi.json.work_dirs[]` maps a canonical work directory to `path`, `kaos`, and `last_session_id`. For local sessions, `<work-dir-key>` is lowercase MD5 of the canonical path's UTF-8 bytes. Remote KAOS sessions prefix the hash with `<kaos>_`. Prefer parsing `kimi.json` and selecting the exact `path`/`kaos` record over reimplementing canonicalization.

After a hook binds `(session_id, cwd)`, resolve:

```text
<share>/sessions/<work-dir-key>/<session-id>/wire.jsonl
<share>/sessions/<work-dir-key>/<session-id>/context.jsonl
<share>/sessions/<work-dir-key>/<session-id>/state.json
```

`state.json` is versioned with `version: 1` and is written atomically. Adapter-relevant fields are `approval.yolo`, `approval.afk`, `approval.auto_approve_actions`, `plan_mode`, `additional_dirs`, `custom_title`, `todos`, and archive metadata. Read it as enrichment; live Wire requests remain the authority for a current wait.

`--continue` uses `last_session_id` for the canonical work directory. `--session <id>` resumes the ID when found and creates a new session with that caller-supplied ID when absent. A normal new session receives a UUID string.

## Command hooks

A hook receives one JSON object on **stdin**. Exit status and output control the native operation; logs belong on stderr.

### Common input

Every event contains exactly these common fields at the pinned release:

```json
{
  "hook_event_name": "PreToolUse",
  "session_id": "2de94d41-...",
  "cwd": "/absolute/project/path"
}
```

There is no `transcript_path`, model, permission mode, parent agent ID, or timestamp. Unknown fields remain forward-compatible.

### Event catalog and payloads

| Event | Matcher value | Event-specific input | Timing and adapter use |
| --- | --- | --- | --- |
| `SessionStart` | `startup` or `resume` | `source` | awaited after CLI initialization; register session |
| `UserPromptSubmit` | prompt text | `prompt` | awaited before Wire `TurnBegin`; blockable |
| `PreToolUse` | tool name | `tool_name`, `tool_input`, `tool_call_id` | awaited before the tool and its approval path; blockable |
| `PostToolUse` | tool name | tool identity/input, `tool_output` | fire-and-forget after return; output truncated to 2,000 characters |
| `PostToolUseFailure` | tool name | tool identity/input, `error` | fire-and-forget on raised exception |
| `Stop` | empty | `stop_hook_active` | awaited after the turn body, before Wire `TurnEnd`; can cause one corrective turn |
| `StopFailure` | exception class name | `error_type`, `error_message` | fire-and-forget after fatal step failure |
| `SessionEnd` | `exit` | `reason` currently `exit` | awaited in teardown with a five-second outer timeout |
| `SubagentStart` | agent type | `agent_name`, `prompt` | awaited; prompt truncated to 500 characters |
| `SubagentStop` | agent type | `agent_name`, `response` | fire-and-forget; response truncated to 500 characters |
| `PreCompact` | trigger | `trigger`, `token_count` | awaited before compaction |
| `PostCompact` | trigger | `trigger`, `estimated_token_count` | fire-and-forget after rebuilt context is persisted |
| `Notification` | notification type | `sink`, `notification_type`, `title`, `body`, `severity` | fires when a queued notification is delivered to the LLM |

Compaction `trigger` is `auto`, `manual`, or `manual-with-prompt`. The docs' broad “trigger reason” wording should not be parsed as a closed future-proof enum.

Subagent hooks identify only the type name. They do not carry the persistent `agent_id`, parent tool-call ID, foreground/background mode, or child context path; use Wire and the subagent store for identity.

`Notification` observes notifications delivered to the LLM, including completed background work. It is not the UI approval-request channel and must not be used as permission-wait truth.

### Output and exit semantics

| Result | Native behavior |
| --- | --- |
| exit `0`, empty stdout | allow |
| exit `0`, non-JSON stdout | allow; 1.48.0 retains it in `HookResult` but the lifecycle callers do not append it to context |
| exit `0`, JSON stdout with deny below | block |
| exit `2` | block; trimmed stderr becomes the reason |
| any other exit | fail open; stderr is logged |
| timeout, spawn failure, invalid matcher, or engine exception | fail open |

The only structured decision parsed at this release is:

```json
{
  "hookSpecificOutput": {
    "permissionDecision": "deny",
    "permissionDecisionReason": "explanation returned to the model"
  }
}
```

`hookEventName` may be included but the parser does not require or validate it. Values other than `permissionDecision: "deny"` allow. The neutral RimZ observation path is empty stdout with exit 0. Golden-test those exact bytes against the supported release.

Stop-hook blocking with a non-empty reason runs one extra internal user turn using that reason. The second stop check is suppressed by the in-memory anti-loop flag. The hook payload's first call has `stop_hook_active: false`; the current source does not invoke the hook again with `true`, despite the published page's generic anti-loop description.

## Durable Wire log

Every stock shell session records merged Wire messages to `wire.jsonl`; Wire server mode is not required. The first line is metadata:

```json
{"type":"metadata","protocol_version":"1.10"}
```

Every later line is one complete record:

```json
{
  "timestamp": 1770000000.125,
  "message": {
    "type": "TurnBegin",
    "payload": {"user_input": "fix the parser"}
  }
}
```

`timestamp` is Unix epoch seconds as a float. `message.type` selects the typed payload. The recorder writes complete merged text/thinking/tool-argument parts, so a tailer sees coarser chunks than an unmerged live Wire client but retains all control-flow events.

Each append opens the file in append mode and writes one JSON line; only `state.json` has an explicit atomic-write contract. A live tailer must hold an incomplete final line until newline, tolerate malformed/unknown records, remember byte offset plus file identity, and rescan safely after replacement or truncation. `/undo` and `/fork` create new session directories; context clear/compaction rotates `context.jsonl` but keeps the session's Wire log.

### Control-flow and status messages

| `message.type` | Key payload | Meaning |
| --- | --- | --- |
| `TurnBegin` | `user_input: string | ContentPart[]` | authoritative user-visible turn opener |
| `SteerInput` | same | follow-up inserted between steps |
| `TurnEnd` | `{}` | turn bracket close; protocol permits omission on interruption, while 1.48.0 emits it in a `finally` path after any begun turn |
| `StepBegin` | `n` | model/tool loop step starts, numbered from 1 |
| `StepInterrupted` | `{}` | current step interrupted by error or user action |
| `StepRetry` | `n`, `next_attempt`, `max_attempts`, `wait_s`, `error_type`, optional `status_code` | transient error entered retry wait |
| `CompactionBegin` / `CompactionEnd` | `{}` | context rebuild bracket |
| `StatusUpdate` | context, token, plan, message fields below | live enrichment after a model step or mode change |
| `PlanDisplay` | `content`, `file_path` | plan shown before its approval request |
| `HookTriggered` | `event`, `target`, `hook_count` | hook batch started |
| `HookResolved` | `event`, `target`, `action`, `reason`, `duration_ms` | hook batch completed |
| `Notification` | notification record | UI/client notification |
| `BtwBegin` / `BtwEnd` | `id`, question; response/error | isolated `/btw` side question |

`StatusUpdate` fields are independently optional/null and mean “no change” when absent:

```json
{
  "context_usage": 0.42,
  "context_tokens": 110100,
  "max_context_tokens": 262144,
  "token_usage": {
    "input_other": 90000,
    "output": 1200,
    "input_cache_read": 20000,
    "input_cache_creation": 100
  },
  "message_id": "provider-message-id",
  "plan_mode": false
}
```

`context_usage` is a ratio, not a percentage. `context_tokens` describes the prompt context before the just-completed step. `token_usage` describes that step: total input is `input_other + input_cache_read + input_cache_creation`; output is separate. Fold partial `StatusUpdate` objects over the previous live snapshot.

`StepRetry` is pause/retry evidence, while an eventual successful step continues in the same turn. A 429 or 5xx is not a completed turn merely because retry began.

### Content and tool messages

`ContentPart` uses `payload.type` to distinguish `text`, `think`, `image_url`, `audio_url`, and `video_url`. Text has `text`; thinking has `think` plus optional encrypted/signature data. Do not surface raw reasoning as ordinary assistant output.

`ToolCall` carries:

```json
{
  "type": "function",
  "id": "tool-call-id",
  "function": {"name": "Shell", "arguments": "{\"command\":\"cargo check\"}"},
  "extras": null
}
```

`function.arguments` is a JSON-encoded string and may be null. Parse it structurally after the outer envelope. `ToolResult` correlates by `tool_call_id`; its `return_value` contains `is_error`, `output`, `message`, `display[]`, and optional `extras`.

Canonical built-in names relevant to lifecycle classification include `WriteFile`, `StrReplaceFile`, `Shell`, `Agent`, `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode`, `SetTodoList`, `TaskList`, `TaskOutput`, and `TaskStop`. Treat `WriteFile` and `StrReplaceFile` as edit proof. Treat all other completed tools as work proof unless a future mapping deliberately specializes them.

## Blocking approvals, questions, and plan review

The stock UI writes blocking Wire requests into `wire.jsonl` before awaiting the user and writes a resolution event after the answer. This gives RimZ durable wait detection without taking over the native UI.

### Approval request

```json
{
  "id": "request-uuid",
  "tool_call_id": "tool-call-id",
  "sender": "Shell",
  "action": "shell:execute",
  "description": "Run cargo check",
  "display": [],
  "source_kind": "foreground_turn",
  "source_id": "turn-or-task-id",
  "agent_id": null,
  "subagent_type": null,
  "source_description": null
}
```

`source_kind` is `foreground_turn` or `background_agent`. The source and subagent fields are optional for compatibility. A later `ApprovalResponse` closes the request:

```json
{"request_id":"request-uuid","response":"approve","feedback":""}
```

`response` is `approve`, `approve_for_session`, or `reject`. Key pending approvals by request ID, not tool-call ID: more than one approval can exist and “approve for session” may resolve other pending requests with the same action.

### Structured user question

`QuestionRequest` contains `id`, `tool_call_id`, and one to four `questions`. Each question has `question`, optional `header`, two to four `options` (`label`, optional `description`), `multi_select`, and newer optional body/“Other” labels. The native response maps exact question text to a selected label or comma-separated labels.

The persisted stock log records the request but does not define a distinct public `QuestionResponse` Wire event in the event union. Close the wait when its `AskUserQuestion` tool result arrives, when the turn/session ends, or when a newer state proves cancellation. A future native-answer implementation can use Wire server mode's synchronous `request` response, but ordinary RimZ pane messaging should continue through pane send.

### Plan approval

Plan mode restricts the agent to read-only exploration and a plan file. `ExitPlanMode` emits `PlanDisplay { content, file_path }`, then enters the normal approval runtime. Classify the correlated `ApprovalRequest` as `PlanApproval` when its `tool_call_id` belongs to `ExitPlanMode`. Use `PlanDisplay.content` as the plan body and keep the native approval choices in the Kimi UI.

YOLO auto-approves tools but leaves questions available. AFK auto-approves tools and auto-dismisses `AskUserQuestion`. Both flags persist in `state.json`; `--print` applies runtime AFK even when the persisted flag is false.

## Native-event mapping for a first adapter

| Kimi observation | RimZ signal/enrichment | Notes |
| --- | --- | --- |
| `SessionStart` hook | `registered` | bind exact session and open its Wire tail |
| Wire `TurnBegin` | `turn_started` | preserve text/content input |
| Wire `ToolCall` / `ToolResult` | `tool_used` | editor names set `edits: true`; tool start also proves acting |
| Wire `ApprovalRequest` | `awaiting_input(Permission)` | classify source and tool |
| Wire `QuestionRequest` | `awaiting_input(Question)` | include structured choices |
| `PlanDisplay` + approval for `ExitPlanMode` | `awaiting_input(PlanApproval)` | plan body comes from display event |
| Wire `ApprovalResponse` / correlated tool result | clear wait | response does not itself end the turn |
| Wire `TurnEnd` | `turn_ended` | classify clean vs interrupted/failed from preceding native evidence |
| `StopFailure` hook | failed `turn_ended` / pause enrichment | classify retryable state from Wire and native exit, not exception spelling alone |
| Wire `StepRetry` | retry/rate-limit pause enrichment | keep the turn open |
| Wire compaction bracket | `compacting` / `compaction_ended` | hook trigger enriches manual vs auto |
| `SessionEnd` hook | `ended` | pane/process liveness remains the backstop |
| Wire `StatusUpdate` | context/token/plan enrichment | fold optional fields |
| Wire `SubagentEvent` | child observation | identity and parent correlation below |

Do not emit a second turn start from `UserPromptSubmit` after Wire `TurnBegin` is available. The hook is an early latency hint and policy surface; the Wire record is durable truth for the completed binding.

`Stop` occurs before Wire `TurnEnd` and may initiate a corrective turn without a second outer `TurnBegin`. Use it as a latency hint only. `TurnEnd` closes the bracket for both normal and interrupted turns in 1.48.0; `StopFailure`, `StepInterrupted`, retry exhaustion, and process/session death determine abnormal outcome.

## Subagents and background work

The default root agent exposes persistent `coder`, `explore`, and `plan` subagent types through the `Agent` tool. A subagent has a stable `agent_id`, isolated context, optional model override, and foreground or background execution. Subagents cannot nest `Agent`.

Root `wire.jsonl` wraps child events as:

```json
{
  "type": "SubagentEvent",
  "payload": {
    "parent_tool_call_id": "root-agent-tool-call",
    "agent_id": "child-id",
    "subagent_type": "coder",
    "event": {"type": "StatusUpdate", "payload": {"context_usage": 0.2}}
  }
}
```

Recursively parse the nested event with the same Wire event parser. Key the child by `(root session_id, agent_id)`, carry `parent_tool_call_id` for tree placement, and use `subagent_type` as the kind/role label. Older Wire records may use `task_tool_call_id` instead of `parent_tool_call_id`; upstream accepts that compatibility alias.

Child storage is:

```text
<session>/subagents/<agent-id>/
├── context.jsonl
├── wire.jsonl
├── meta.json
├── prompt.txt
└── output
```

The parent nested events are the simplest live feed; child files support context and recovery. `meta.json` status values include `idle`, `running_foreground`, `running_background`, `completed`, `failed`, and `killed` at this release.

Background shell and agent tasks have a separate durable store and can be `created`, `starting`, `running`, `awaiting_approval`, `completed`, `failed`, `killed`, or `lost`. Their completion reaches the root as a `Notification`. Preserve a root agent's working state when a foreground turn ends but active background work remains only after the implementation has joined this task store explicitly; `Stop` hooks do not carry a background-task snapshot.

## Context transcript JSONL

`context.jsonl` is the model-facing conversation used for restore. It begins with a frozen system prompt and interleaves message and internal records:

```jsonl
{"role":"_system_prompt","content":"..."}
{"role":"_checkpoint","id":0}
{"role":"user","content":"fix the parser"}
{"role":"assistant","content":[{"type":"text","text":"..."}],"tool_calls":[...]}
{"role":"tool","tool_call_id":"tool-call-id","content":"..."}
{"role":"_usage","token_count":90123}
```

Internal roles are:

| Role | Fields | Meaning |
| --- | --- | --- |
| `_system_prompt` | string `content` | frozen prompt restored with session |
| `_checkpoint` | integer `id` | rewind boundary |
| `_usage` | integer `token_count` | latest known prompt input-token count |

Conversation roles are `system`, `user`, `assistant`, and `tool`. `content` may be a string or an array of typed content parts. Assistant messages may include `tool_calls[]`; tool messages correlate through `tool_call_id`; `partial` may appear.

The last `_usage.token_count` resets the known context count to the provider-reported input tokens for that step. Messages after it contribute only a local text-token estimate until another model response. Use Wire `StatusUpdate` for the live exact split and `_usage` as restore/backstop data.

Compaction rotates the old `context.jsonl`, writes a new frozen system prompt, checkpoint, compacted summary messages, and a new estimated `_usage`. `/clear` also rotates the context file and rebuilds the system prompt but does not create a new session ID or reset approval/subagent/additional-directory state. A transcript tailer must detect inode replacement.

Use Wire `TurnBegin`/`TurnEnd` to enumerate user-visible turns. Synthetic checkpoint user messages such as `<system>CHECKPOINT N</system>` are internal and do not open turns.

## Model, context, tokens, and cost

The active model alias resolves through `config.toml`:

```toml
default_model = "kimi-for-coding"

[models.kimi-for-coding]
provider = "managed:kimi-code"
model = "kimi-for-coding"
display_name = "Kimi for Coding"
max_context_size = 262144
capabilities = ["thinking", "image_in", "video_in"]
```

Launch `--model` overrides the default for that invocation. `/model` persists a new default and reloads the same session. Hooks and `StatusUpdate` do not carry a model identifier, so an adapter must combine launch argv with the effective config and refresh on session reload/config change. If certainty is unavailable, omit model rather than attributing tokens to the wrong model.

`max_context_size` comes from model config or managed `/models` refresh. Current context fill and the provider step split come from `StatusUpdate`. Sum step usage over the active session for cumulative tokens, deduplicating by Wire record and preferably `message_id`.

Kimi Code CLI records no per-step USD cost and publishes no pricing table in these surfaces. Cost requires RimZ pricing data keyed by provider/model; mark cost unknown when model or pricing is unavailable. Kimi Code subscription quota units are not documented as billable tokens and must not be converted into dollars.

## Authentication, account, and quota

`kimi login` and `/login` use the Kimi Code OAuth device flow, fetch managed models, and write managed provider/model/service config. `kimi logout` removes the managed provider/models and OAuth credentials. Login requires the default config location.

OAuth tokens live in `<share>/credentials/kimi-code.json` with mode 0600. The file contains `access_token`, `refresh_token`, `expires_at`, `scope`, `token_type`, and `expires_in`. Older keyring credentials migrate to the file. Treat file existence plus a non-expired/refreshable shape as an idle auth hint; never log, render, hash, or copy token values.

API-key providers store `api_key` in config or receive it from environment. Provider kinds include Kimi, OpenAI-compatible, Anthropic, Gemini, and Vertex AI; an adapter named Kimi describes the CLI process, while provider/account attribution follows the selected model's provider.

For a managed Kimi Code model only, `/usage` performs authenticated:

```text
GET https://api.kimi.com/coding/v1/usages
Authorization: Bearer <resolved OAuth or API token>
```

The response parser accepts an optional summary object at `usage` and zero or more `limits[]`. A row can be direct or nested at `detail`; accepted fields are:

| Field | Meaning |
| --- | --- |
| `limit` | quota ceiling |
| `used` | consumed units |
| `remaining` | alternative to `used`; client derives `limit - remaining` |
| `name`, `title`, `scope` | display label |
| `reset_at`, `resetAt`, `reset_time`, `resetTime` | absolute reset time, commonly ISO 8601 |
| `reset_in`, `resetIn`, `ttl`, numeric `window` | seconds until reset |
| `window.duration`, `window.timeUnit` | label such as 5h/7d |

The endpoint has no public standalone schema and may change. Parse optional fields, retain raw units, and fail soft. HTTP 401 means auth failure, 404 means usage unavailable, and timeouts/client failures are enrichment failures rather than session failures.

## Headless and supervised runs

Use `--print` for one supervised non-interactive turn. It enables runtime AFK, exits after the instructions, and supports text or JSONL:

```sh
kimi --print -p "Run the focused checks" --output-format=stream-json
```

`--input-format=stream-json` reads user `Message` objects from stdin until EOF. `--output-format=stream-json` emits assistant and tool messages as JSONL using the same `role`, `content`, `tool_calls`, and `tool_call_id` shapes as context messages. `--final-message-only` suppresses intermediate output; `--quiet` aliases print + text + final-only.

Native exit codes are:

| Code | Meaning |
| --- | --- |
| `0` | success |
| `1` | permanent failure, including config, auth, and quota exhaustion |
| `75` | retryable failure, including 429, 5xx, and connection timeout |

Preserve these exit codes for `rimz agents -p`. Keep the supervised pane session and its hooks/Wire log as lifecycle truth; parse stream JSON only as the requested output format.

## Resume, clear, undo, and fork

`kimi --continue` resumes the newest session recorded for the canonical current directory. `kimi --session <id>` and `kimi --resume <id>` resume a specific ID and create it when absent. Bare `--session`/`--resume` opens a picker in shell mode.

`/new` creates and switches to a fresh session. `/clear` and `/reset` rebuild context in the same session and preserve session state. `/sessions` switches identities. Every non-empty exit prints `kimi -r <session-id>` as a resume hint.

`/fork` creates a new UUID session and copies `wire.jsonl` and `context.jsonl` through a selected turn, plus referenced video uploads. It assigns a `Fork: <source-title>` title. `/undo` uses the same truncation machinery for an earlier boundary. There is no documented CLI `--fork-session` option, so RimZ cannot currently express native fork as launch argv without calling an internal API or driving the interactive command. Keep this as an explicit adapter capability gap.

## Wire server and ACP index

`kimi --wire` exposes JSON-RPC 2.0 over stdin/stdout. It supports `initialize`, `prompt`, `replay`, `steer`, `set_plan_mode`, and `cancel`; server notifications carry events, while `request` messages synchronously carry approvals, questions, external tool calls, and subscribed hooks. `initialize` negotiates protocol version, `supports_question`, external tools, and hook subscriptions.

Wire server mode is a viable future native-control integration, but it replaces the stock shell UI's stdio contract and would require RimZ to host a client/UI. Use the stock CLI plus command hooks and its durable Wire log for the first adapter.

`kimi acp` is a multi-session Agent Client Protocol server for IDE integrations. ACP maps approvals to `session/request_permission` and exposes its own session lifecycle. It is not an observation interface for an independently running stock pane and is outside the first adapter.

## Implementation checklist and live verification gaps

1. Add a typed Kimi descriptor for executable `kimi`, aliases, permission-mode launch args, lazy/eager registration behavior, and minimum supported version.
2. Install the lifecycle hook set with an exact diff, preserve unrelated config, and include executable hook fields in the trust hash.
3. Parse every hook payload into a Kimi-native enum before mapping it to `AgentLifecycleObservation`; reserve stdout for neutral/decision bytes.
4. Resolve the exact session directory from `KIMI_SHARE_DIR`, parsed `kimi.json`, hook `cwd`, and hook `session_id`.
5. Implement a newline-safe, rotation-aware `wire.jsonl` tailer keyed by session and persisted byte offset; version-gate from the metadata header.
6. Parse Wire envelopes with unknown-type tolerance and typed inner parsers for turns, tools, status, approvals, questions, plans, compaction, and subagents.
7. Deduplicate hook latency hints against durable Wire facts without suppressing distinct retries or corrective stop turns.
8. Track pending approvals by request ID and questions by request/tool ID; close them on typed resolution, correlated tool result, turn end, session end, or proven cancellation.
9. Parse `context.jsonl` for durable transcript/context backfill and detect file replacement after clear or compaction.
10. Derive the effective model from launch args plus config; refresh after reload, and leave it unknown when attribution is ambiguous.
11. Sum token usage from deduplicated `StatusUpdate` records; keep subscription quota units distinct from tokens and USD cost.
12. Implement OAuth/account probing without exposing credentials and quota probing as best-effort enrichment.
13. Implement supervised print mode with JSONL parsing and exact exit-code preservation.
14. Golden-test hook stdin/stdout, Wire metadata/records, request resolution, context rotations, config trust, and malformed/unknown-field behavior.

Before declaring support, live-verify these release-sensitive points against an installed target version:

- the minimum release that contains all 13 hooks and Wire protocol 1.10;
- exact hook behavior when stdout contains plain text on each event;
- whether a stop hook ever emits `stop_hook_active: true` in a newer release;
- ordering and persisted records for approve, reject, question dismissal, plan approval, Ctrl-C, EOF, `/new`, `/sessions`, `/clear`, `/undo`, and `/fork`;
- whether all stock UI modes persist `ApprovalRequest` and `QuestionRequest` before blocking;
- config behavior under `--config`, `--config-file`, and `KIMI_SHARE_DIR`;
- model attribution after `--model`, `/model`, managed-model refresh, and session resume;
- subagent event completeness for foreground, background, resumed, failed, and approval-blocked children;
- quota payloads for every subscription tier RimZ intends to render;
- the successor `MoonshotAI/kimi-code` executable collision and migration behavior, so RimZ refuses an incompatible protocol instead of silently misparsing it.

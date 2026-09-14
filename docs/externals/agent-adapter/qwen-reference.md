# Qwen Code protocol reference

This page mirrors the Qwen Code surfaces an adapter binds to: session identity and resume, command hooks and their decision channel, the statusline payload, session JSONL, subagents, interactive dual output, headless runs, authentication, and the CLI. It records what upstream ships; how RimZ maps these surfaces onto its own types is in [adapter_qwen.md](../../internals/agents/adapter_qwen.md).

## Baseline and sources

This page mirrors Qwen Code **0.23.3**, tag [`v0.23.3`](https://github.com/QwenLM/qwen-code/tree/v0.23.3) (commit `b695664b8df06d06c625db3e30b97045d82092c7`), released 2026-09-10 and published as npm `@qwen-code/qwen-code` `latest`. Docs and source were read on 2026-09-13. Source citations are paths under `packages/` at that tag. The Alibaba quota request is pinned separately to a CodexBar commit, named in its row below.

| Surface | Source |
| --- | --- |
| Project and installation | <https://github.com/QwenLM/qwen-code> |
| Hooks: events, payloads, outputs, execution | <https://qwenlm.github.io/qwen-code-docs/en/users/features/hooks/> |
| Hook wire types | [`core/src/hooks/types.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/hooks/types.ts) |
| Hook runner, planner, event builders, trust | [`hookRunner.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/hooks/hookRunner.ts), [`hookPlanner.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/hooks/hookPlanner.ts), [`hookEventHandler.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/hooks/hookEventHandler.ts), [`trustedHooks.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/hooks/trustedHooks.ts) |
| Settings layers and environment | <https://qwenlm.github.io/qwen-code-docs/en/users/configuration/settings/> |
| Authentication and providers | <https://qwenlm.github.io/qwen-code-docs/en/users/configuration/auth/>, <https://qwenlm.github.io/qwen-code-docs/en/users/configuration/model-providers/> |
| Alibaba Coding Plan API-key quota request (third party) | [`AlibabaCodingPlanUsageFetcher.swift`](https://github.com/steipete/CodexBar/blob/c61e01e774c449b06324a1cc260af7c77cf17d47/Sources/CodexBarCore/Providers/Alibaba/AlibabaCodingPlanUsageFetcher.swift) at CodexBar commit [`c61e01e`](https://github.com/steipete/CodexBar/tree/c61e01e774c449b06324a1cc260af7c77cf17d47) |
| Session commands | <https://qwenlm.github.io/qwen-code-docs/en/users/features/commands/>, [`cli/src/commands/sessions/`](https://github.com/QwenLM/qwen-code/tree/v0.23.3/packages/cli/src/commands/sessions) |
| Live session registry | [`core/src/services/session-registry.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/services/session-registry.ts) |
| Runtime PID/session sidecar | [`core/src/utils/runtimeStatus.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/utils/runtimeStatus.ts) |
| Session JSONL writer and loader | [`chatRecordingService.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/services/chatRecordingService.ts), [`sessionService.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/services/sessionService.ts) |
| Usage normalization | [`core/src/services/tokenEstimation.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/services/tokenEstimation.ts) |
| Subagent transcripts and metadata | [`core/src/agents/agent-transcript.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/agents/agent-transcript.ts), <https://qwenlm.github.io/qwen-code-docs/en/users/features/sub-agents/> |
| Statusline JSON | <https://qwenlm.github.io/qwen-code-docs/en/users/features/status-line/> |
| Interactive dual output | <https://qwenlm.github.io/qwen-code-docs/en/users/features/dual-output/>, [`DualOutputBridge.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/cli/src/dualOutput/DualOutputBridge.ts) |
| Structured message types | [`cli/src/nonInteractive/types.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/cli/src/nonInteractive/types.ts) |
| Headless mode, budgets, exit codes | <https://qwenlm.github.io/qwen-code-docs/en/users/features/headless/>, [`core/src/utils/errors.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/utils/errors.ts) |
| CLI options | [`cli/src/config/config.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/cli/src/config/config.ts), [`cli/src/config/top-level-options.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/cli/src/config/top-level-options.ts) |
| System-prompt file override | [`core/src/core/prompts.ts`](https://github.com/QwenLM/qwen-code/blob/v0.23.3/packages/core/src/core/prompts.ts) (`isSystemMdActive`, `getCoreSystemPrompt`) |
| Approval modes | <https://qwenlm.github.io/qwen-code-docs/en/users/features/approval-mode/> |
| ACP daemon mode | <https://qwenlm.github.io/qwen-code-docs/en/users/qwen-serve/> |

## Surfaces at a glance

Qwen Code exposes several overlapping observation surfaces, and they differ in liveness and durability. Qwen Code began as a fork of Gemini CLI v0.8.2 and has developed independently since v0.1: parts of the codebase keep Gemini naming (the transcript's Google `Content` shape, for example), but its event names, transcript schema, auth, and model limits are its own.

| Surface | Carries | Durability |
| --- | --- | --- |
| [Command hooks](#hooks) | session id and transcript path on every event, turn and tool boundaries, permission decisions, subagent and compaction brackets | synchronous per event; nothing persisted |
| [Runtime sidecar](#runtime-pidsession-sidecar) | PID to session binding, work dir, version | file kept after exit and crash |
| [Live session registry](#live-session-registry) | running sessions with liveness checks | record removed on exit |
| [Statusline JSON](#statusline-json) | model, context window and occupancy, cumulative per-model tokens, file-change totals | live only, debounced |
| [Session JSONL](#session-transcript-jsonl) | full conversation tree, per-response usage and context window, system records | append-only file |
| [Dual output](#interactive-dual-output) | real-time `stream-json` events and a reverse prompt and permission channel beside the TUI | best effort; disables itself on error |
| [Headless runs](#headless-runs) | one-shot `text`, `json`, or `stream-json` output with a terminal result and typed exit codes | process lifetime |

## Sessions

### Resume, fork, and session commands

A session is one `sessionId` with one JSONL file. The CLI and slash commands below create, continue, or branch it.

| Command | Effect on session identity |
| --- | --- |
| `qwen --continue` (`-c`) | resumes the newest session for the current project |
| `qwen --resume <id>` (`-r`) | resumes that session; bare `--resume` opens the picker |
| `--fork-session` | with `--resume` or `--continue`, copies the active conversation into a new session id and leaves the source intact |
| `--session-id <id>` | assigns the id for this run |
| `/clear` | ends the current id and starts another |
| `/branch` | forks the current conversation into a new id |
| `/rewind` | moves the active history branch and can restore files; same id |
| `/compress` (alias `/summarize`), `/compress-fast` | compact history; same id |
| `/delete` | deletes a selected session and fires [`SessionDelete`](#event-catalog) |

`general.chatRecording` (default on, CLI `--chat-recording`) controls whether sessions are written at all. With recording off, `--continue` and `--resume` do not work.

### Listing sessions

`qwen sessions list --json [--limit N]` writes one JSON object per line for recorded sessions, 20 by default. Each object carries `sessionId`, `startTime`, `mtime`, `prompt`, `gitBranch`, `customTitle`, `titleSource`, `filePath`, and `cwd`; absent optional values are `null`. The pagination hint goes to stderr (`cli/src/commands/sessions/list.ts`, `toJsonItem`).

### Live session registry

`qwen sessions ps --json` lists the running sessions, one registry record per line with `ipcToken` removed (`cli/src/commands/sessions/ps.ts`). Each top-level session writes `${QWEN_HOME:-~/.qwen}/sessions/<pid>.json` at startup and unlinks it on exit; a process hosting several sessions (the `qwen --acp` child a daemon spawns) writes `<pid>-<8 hex>.json` per session (`core/src/services/session-registry.ts`).

| Field | Meaning |
| --- | --- |
| `schemaVersion` | record schema version |
| `pid`, `procStart`, `pidNs` | process id, start-time token against PID reuse, PID-namespace id (`null` where unavailable) |
| `sessionId`, `cwd`, `name` | session id, working directory, short display label |
| `startedAt` | epoch milliseconds |
| `qwenVersion` | CLI version or `null` |
| `kind` | `tui`, `headless`, `serve`, or `external`; absent on records from writers that predate the field (the TUI) |
| `ipcPath` | peer-messaging socket, when the session has one |

A reader treats a record as live only when its PID runs with a matching start token in the reader's PID namespace and boot; failing records are swept during enumeration. The registry is scoped to one `QWEN_HOME`. `qwen sessions controllers` manages the controller tokens that may drive sessions.

### Runtime PID/session sidecar

Every interactive session atomically writes a sidecar that binds its PID to its session:

```text
<runtime-base>/projects/<sanitized-cwd>/chats/<session-id>.runtime.json
```

`<runtime-base>` resolves as `QWEN_RUNTIME_DIR`, then the configured runtime output directory, then `QWEN_HOME` or `~/.qwen`. The keys are snake_case to match kimi-cli's `runtime.json`, and the schema is versioned on its own:

```json
{
  "schema_version": 1,
  "pid": 43120,
  "session_id": "UUID",
  "work_dir": "/absolute/project/path",
  "hostname": "host",
  "started_at": 1783700000.125,
  "qwen_version": "0.23.3"
}
```

`started_at` is epoch seconds with sub-second precision, and `qwen_version` may be `null`. A session change or a cwd or worktree transition rewrites the applicable sidecar.

The file stays after clean exit and after a crash, so its presence says nothing about liveness (`core/src/services/session-registry.ts` states this as the reason the registry exists). A consumer checks that `pid` is alive and is the expected process, and that `work_dir` and the file's location agree with the workspace, because PID reuse can otherwise select a stale sidecar.

## Hooks

A command hook runs at a lifecycle point, receives one JSON object on stdin, and returns its decision on stdout; logs go to stderr. `qwen hooks` (or `/hooks` interactively) manages them.

### Configuration

Hooks live under `hooks.<EventName>[]` in `settings.json`. Each entry is a `HookDefinition` with an optional `matcher`, optional `sequential`, and a `hooks` array:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "ask_user_question|exit_plan_mode",
        "hooks": [{ "type": "command", "command": "my-hook", "timeout": 10000 }]
      }
    ]
  }
}
```

Hooks in one entry run in parallel by default; `sequential: true` runs them in order and lets each modify the input for the next.

| Command hook field | Type | Meaning |
| --- | --- | --- |
| `type` | `"command"` | required |
| `command` | string | required; run through `shell` |
| `name`, `description` | string | logging labels; `name:command` is the hook's trust key |
| `timeout` | number | milliseconds, default 60,000 (`hookRunner.ts`, `DEFAULT_HOOK_TIMEOUT`) |
| `env` | object | extra environment |
| `shell` | `"bash"` or `"powershell"` | shell selection |
| `statusMessage` | string | shown while the hook runs |
| `async` | boolean | runs in the background; cannot return decision control |

The other hook types share the event and input contract:

| Type | Behaviour | Distinct fields |
| --- | --- | --- |
| `http` | POSTs the input JSON; URL allowlist (`security.allowedHttpHookUrls`), DNS validation, private-range blocking unless `security.allowPrivateNetworkHooks` is set from user or system scope, cloud metadata always blocked, redirects disabled | `url`, `headers`, `allowedEnvVars`, `timeout` in seconds (default 600), `once`, `if` |
| `prompt` | sends the input to a model and expects `{ "ok": boolean, "reason"?, "additionalContext"? }` | `prompt` (with `$ARGUMENTS`), `model`, `timeout` in seconds (default 30) |
| `function` | trusted in-process callback registered by session code; not a settings surface | `callback`, `errorMessage` |

On timeout or cancellation a command hook's whole process tree is killed: a POSIX process group gets SIGTERM then SIGKILL, and Windows uses `taskkill` (`hookRunner.ts`).

### Disabling and trust

Top-level `disableAllHooks: true`, `--safe-mode` (or `QWEN_CODE_SAFE_MODE=true`), and `--bare` startup disable configured hooks. Hooks come from system, user, project, extension, and session sources (`HooksConfigSource`), and all sources fire. Project hooks require a trusted workspace, and Qwen records trusted project hook keys (`name:command`) per project path in `~/.qwen/trusted_hooks.json` (`trustedHooks.ts`).

### Matchers

`matcher` is a regular expression; an empty string or `*` matches everything. Each event matches against one target, and events without a target always fire (`hookPlanner.ts`, `getHookMatcherTarget`).

| Events | Matcher target |
| --- | --- |
| `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionRequest`, `PermissionDenied` | runtime tool id (`write_file`, `run_shell_command`, ...); display names such as `WriteFile` are accepted as aliases |
| `SubagentStart`, `SubagentStop` | agent type |
| `PreCompact`, `PostCompact` | trigger |
| `SessionStart`, `SessionEnd` | source or reason |
| `StopFailure` | error type |
| `Notification` | notification type |
| `InstructionsLoaded` | file path |
| `UserPromptExpansion` | command name |
| `UserPromptSubmit`, `Stop`, `MessageDisplay`, `PostToolBatch`, `SessionDelete`, `TodoCreated`, `TodoCompleted` | none |

The runtime tool ids live in `core/src/tools/tool-names.ts` (`ToolNames`): for example `edit`, `write_file`, `notebook_edit`, `run_shell_command`, `agent`, `ask_user_question`, and `exit_plan_mode`. The hooks docs give examples rather than a versioned catalog, so that file is the reference.

### Common input

Every event carries the `HookInput` base (`types.ts`):

```json
{
  "session_id": "string",
  "source_type": "optional: integration that created the session",
  "source_id": "optional",
  "transcript_path": "/absolute/path/to/session.jsonl",
  "cwd": "/current/working/directory",
  "hook_event_name": "SessionStart",
  "timestamp": "ISO 8601"
}
```

Inside a subagent, events also carry `agent_id` and `agent_type`. Upstream documents hook input as forward-extensible: new optional fields can appear on existing events, and consumers ignore unknown keys. `permission_mode` values are `default`, `plan`, `auto_edit`, `auto`, and `yolo`; the CLI spells the third `auto-edit`.

### Output and exit codes

Hook output is one JSON object with common fields, a top-level decision, and event-specific control:

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

| Exit code | Behaviour |
| --- | --- |
| `0` | success; stdout is parsed as JSON |
| `2` | blocking error; stdout is ignored and stderr becomes feedback to the model |
| other | non-blocking error; stderr shows only in debug mode and execution continues |

`StopFailure`, `MessageDisplay`, and `SessionDelete` are fire-and-forget: output and exit codes are ignored, and their command hooks run in a child that survives Qwen's exit (`hookRunner.ts`, `survivesParentExit`). `PostCompact` output has no control effect. An async hook's result reaches the next turn through `systemMessage` or `additionalContext`.

### Event catalog

| Event | Event-specific input | Control |
| --- | --- | --- |
| `SessionStart` | `permission_mode`, `source` (`startup`, `resume`, `clear`, `compact`, `branch`), `model`, optional `agent_type` (`Bash`, `Explorer`, `Plan`, `Custom`) | `additionalContext` |
| `SessionEnd` | `reason` (`clear`, `logout`, `prompt_input_exit`, `bypass_permissions_disabled`, `other`) | none |
| `SessionDelete` | `deleted_session_id` | fire-and-forget |
| `UserPromptSubmit` | `prompt`, optional `submitted_prompt` (the text as submitted) | block, `additionalContext` |
| `UserPromptExpansion` | `command_name`, `command_args`, expanded `prompt` | block, `additionalContext` (escaped, 10,000 characters max) |
| `InstructionsLoaded` | `file_path`, `memory_type` (`user`, `project`, `local`, `extension`), `load_reason` (`session_start`, `include`, `refresh`), optional `trigger_file_path`, `parent_file_path` | none |
| `PreToolUse` | `permission_mode`, `tool_name`, `tool_input`, `tool_use_id`, optional provider `tool_call_id` | allow, deny, ask, `updatedInput` |
| `PermissionRequest` | `permission_mode`, `tool_name`, `tool_input`, optional `permission_suggestions` (`{ type, tool? }[]`) | allow or deny with updates |
| `PermissionDenied` | `tool_name`, `tool_input`, `tool_use_id`, optional `tool_call_id`, `reason` (`classifier_blocked`, `classifier_unavailable`) | none; fires only when `auto` mode's classifier denies a call |
| `PostToolUse` | `permission_mode`, `tool_name`, `tool_input`, `tool_response`, `tool_use_id`, optional `tool_call_id` | block, `additionalContext`, `artifacts` |
| `PostToolUseFailure` | `permission_mode`, `tool_use_id`, optional `tool_call_id`, `tool_name`, `tool_input`, `error`, optional `is_interrupt` | `additionalContext`, `artifacts` |
| `PostToolBatch` | `permission_mode`, `tool_calls[]` (`tool_name`, `tool_input`, `tool_use_id`, optional `tool_call_id`, `status` `success`/`error`/`cancelled`, optional `tool_response`) | block or stop the batch, `additionalContext` |
| `MessageDisplay` | `message_id`, cumulative `displayed_text`, `is_final` | fire-and-forget |
| `Stop` | `stop_hook_active`, `last_assistant_message`, `background_tasks[]`, `crons[]`, optional `context_usage`, `context_limit`, `input_tokens` | block with `reason` |
| `StopFailure` | `error` (`rate_limit`, `authentication_failed`, `billing_error`, `invalid_request`, `server_error`, `max_output_tokens`, `loop_detected`, `unknown`), optional `error_details`, `last_assistant_message` | fire-and-forget |
| `SubagentStart` | `permission_mode`, `agent_id`, `agent_type` | `additionalContext` |
| `SubagentStop` | `permission_mode`, `stop_hook_active`, `agent_id`, `agent_type`, `agent_transcript_path`, `last_assistant_message`, `background_tasks[]`, `crons[]` | block with `reason` |
| `PreCompact` | `trigger` (`manual`, `auto`), `custom_instructions` | `additionalContext` |
| `PostCompact` | `trigger`, `compact_summary` | none |
| `Notification` | `message`, optional `title`, `notification_type` (`permission_prompt`, `idle_prompt`, `auth_success`, `elicitation_dialog`) | none |
| `TodoCreated` | `todo_id`, `todo_content`, `todo_status`, `all_todos[]` (`id`, `content`, `status`, optional `blockedBy`), `phase` (`validation`, `postWrite`) | block in `validation` only |
| `TodoCompleted` | `todo_id`, `todo_content`, `previous_status`, `all_todos[]`, `phase` | block in `validation` only |

`StopFailure` fires instead of `Stop` when an API error or loop detection ends the turn. `MessageDisplay` fires at most every 200 ms while a reply streams and always once with `is_final: true`.

**Stop context and transcript timing.** `context_usage` is a ratio that may exceed 1, `context_limit` is in tokens, and `input_tokens` is the provider-normalized prompt count. `Stop` does not await the transcript writer, so a hook can run before the just-finished assistant record reaches disk; the transcript record whose `promptTokenCount` equals `input_tokens` is the one that belongs to the turn. A background task entry carries `id`, `status`, `agent_type`, `started_at`, and optional `description`; a cron entry carries `id`, `schedule`, `prompt`, `recurring`, `enabled`, and optional `next_run` and `last_run`.

### Decision shapes

`PreToolUse` decides through `hookSpecificOutput`; a top-level `decision` of `allow`/`approve`, `deny`/`block`, or `ask` is the fallback (`PreToolUseHookOutput`):

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow | deny | ask",
    "permissionDecisionReason": "explanation",
    "updatedInput": {},
    "additionalContext": "optional"
  }
}
```

`ask` opens the native confirmation, which shows a diff for edit-class tools. In headless runs and background subagents, where nothing can prompt, `ask` falls back to `deny`.

`PermissionRequest` answers the dialog itself:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PermissionRequest",
    "decision": {
      "behavior": "allow | deny",
      "updatedInput": {},
      "updatedPermissions": [{ "type": "string", "tool": "optional" }],
      "message": "optional deny message",
      "interrupt": false
    }
  }
}
```

`Stop` and `SubagentStop` block with top-level `decision: "block"` plus `reason`, which becomes feedback for another turn. Empty stdout with exit 0 is a neutral answer for every event.

## Statusline JSON

Command mode at `ui.statusLine` runs a shell command, writes one JSON object to its stdin, and renders up to two lines of its stdout. The command times out after five seconds, event-driven updates are debounced by 300 ms, and `refreshInterval` adds a timer with a one-second minimum.

```json
{
  "session_id": "UUID",
  "version": "0.23.3",
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

`git`, `worktree`, and `vim` are absent when inactive. `current_usage` is the latest API call's whole prompt occupancy and the numerator of `used_percentage`; it is a scalar gauge that includes cached input. `context_window_size` is the window Qwen selected for the model, so a consumer needs no provider limit table. `metrics.models` is keyed by every model the session used, so routing or `/model` changes produce several entries, and its token counters are cumulative.

Registry model names can carry a provider label such as `[DeepSeek] deepseek-v4-pro`. Qwen's own preset renderer strips a leading `/^\[[^\]]*\]\s*/` before display; command-mode JSON delivers the decorated label.

Preset statusline mode has no command and no stdin payload.

## Session transcript JSONL

With chat recording on, Qwen appends one self-contained JSON record per line to:

```text
<runtime-base>/projects/<sanitized-cwd>/chats/<session-id>.jsonl
```

`<runtime-base>` resolves as for the [runtime sidecar](#runtime-pidsession-sidecar). Hook input carries the live file as `transcript_path`, and `qwen sessions list --json` returns historical paths as `filePath`.

### Record fields

| Field | Meaning (`ChatRecord` in `chatRecordingService.ts`) |
| --- | --- |
| `uuid`, `parentUuid` | record id and parent in the active conversation tree; `parentUuid` is `null` at the root |
| `sessionId`, `timestamp` | session id and ISO 8601 time |
| `type` | `user`, `assistant`, `tool_result`, or `system` |
| `subtype` | system record kind (see below) |
| `cwd`, `version`, `gitBranch` | working directory, CLI version, branch when available |
| `message` | Google `Content`: `role` (`user` or `model`) plus `parts` with `text`, `functionCall`, `functionResponse`, and thought parts |
| `usageMetadata`, `model`, `contextWindowSize` | assistant usage, model id, and the context window used for that response |
| `toolCallResult` | extensible UI recovery metadata for a tool call |
| `systemPayload` | payload for system records |
| `agentId`, `agentName`, `agentColor`, `isSidechain` | set on records a subagent produced |
| `agentRunId`, `agentRound` | writer execution and round within it, for subagent records |
| `externalInputKind` | `message` or `notification` for injected external input |
| `forkedFrom` | `{ sessionId, messageUuid }` on every record `/branch` copied |
| `daemonPromptId`, `provenance`, `goalContext` | daemon admission id, source classification, and Goal turn ownership |

### Usage

Assistant records carry normalized `usageMetadata`: `promptTokenCount`, `candidatesTokenCount`, `totalTokenCount`, `cachedContentTokenCount`, `thoughtsTokenCount`, and `toolUsePromptTokenCount`. `promptTokenCount` includes cached prompt tokens, and `toolUsePromptTokenCount` is already inside it. Qwen's resume normalization requires prompt accounting, derives output as saturating `totalTokenCount - promptTokenCount` when a total exists, and otherwise uses candidates alone when candidates exceed thoughts and candidates plus thoughts when they do not (`tokenEstimation.ts`). `cachedContentTokenCount` does not separate explicit from implicit cache hits.

Qwen routes to OpenAI-compatible, OpenAI Responses, Anthropic, Gemini, Vertex AI, and local providers, so a model id alone does not identify the biller.

### Active branch and system records

`uuid` and `parentUuid` form a tree, and the active conversation is the chain behind the latest active tail; physical line order is not conversation order. `/rewind` appends a `system/rewind` record (`systemPayload.truncatedCount`) and re-roots later parent links, leaving abandoned descendants in the file. A fork copies records to a new `sessionId`, rebuilds parents by write order, and sets `forkedFrom`.

`system/chat_compression` stores `systemPayload.info` and `systemPayload.compressedHistory`, the exact `Content[]` the model sees after compression; it changes resume history without removing UI-visible records. `system/custom_title` stores `customTitle` and an optional `titleSource` (absent means manual). `system/parent_session` stores `parentSessionId`.

The full `subtype` union at 0.23.3 is `chat_compression`, `slash_command`, `ui_telemetry`, `at_command`, `attribution_snapshot`, `notification`, `cron`, `mid_turn_user_message`, `custom_title`, `parent_session`, `session_source`, `session_model`, `rewind`, `agent_bootstrap`, `agent_launch_prompt`, `agent_retry`, `agent_session_ready`, `file_history_snapshot`, `user_text_elements`, `session_artifact_event`, `session_artifact_snapshot`, `session_sources_snapshot`, `branch_checkpoint`, `goal_state`, `goal_runtime`, `realtime_message`, and `turn_result`. `session_source` stores `{ sourceType, sourceId? }`, the same attribution hooks expose as `source_type` and `source_id`; `session_model` stores `{ modelId, authType, baseUrl?, isRuntime? }`; `turn_result` stores a turn's `promptId`, `state` (`completed`, `cancelled`, `error`), and timing.

Writes go through a serialized queue flushed on orderly teardown, so a hook can precede the newest append. A session that changes cwd moves its sidecar and transcript with it.

### Subagent transcripts and metadata

A subagent writes its own files under the project directory, beside `chats/` (`agent-transcript.ts`):

```text
<runtime-base>/projects/<sanitized-cwd>/subagents/<session-id>/agent-<agent-id>.jsonl
<runtime-base>/projects/<sanitized-cwd>/subagents/<session-id>/agent-<agent-id>.meta.json
```

Path components replace every character outside `[A-Za-z0-9_-]` with `_`. The `.jsonl` file holds `ChatRecord`-shaped records, and a transient `.jsonl.stream` file holds live text while the writer is open. The `.meta.json` sidecar (`AgentMeta`) is written for background and foreground launches:

| Field | Meaning |
| --- | --- |
| `agentId`, `agentType`, `description`, `subagentName` | child identity, type, task description, and config name |
| `parentSessionId`, `parentAgentId`, `toolUseId` | launching session, launching subagent for nested forks (`null` at top level), and the parent's tool call |
| `createdAt`, `lastUpdatedAt`, `status` | ISO 8601 times; `running`, `completed`, `failed`, `cancelled`, or `paused` |
| `model`, `persistedCliFlags` | concrete model id; launch flags (`approvalMode`, `bare`, `safeMode`, `sandbox`, `screenReader`, `model`, `authType`) |
| `depth`, `isBackgrounded`, `isolation`, `resolvedApprovalMode`, `executionAllowedTools` | nesting depth, async launch, `worktree` isolation, approval mode, and fork tool restriction |
| `stats`, `recentActivities`, `lastError`, `resumeCount` | terminal summary, capped recent tool activity, last error, resume attempts |

## Subagents

Qwen has two subagent kinds. A named subagent starts with a fresh context and returns its result inline. A fork (`subagent_type: "fork"`) inherits the parent's conversation, system prompt, and tool declarations, runs detached, and does not feed its result back automatically. A fork cannot fork again, runs in the parent's cwd, and is isolated from sibling forks' directives.

| Fork option | Meaning |
| --- | --- |
| `fork_turns` | inherit only a bounded window of recent user turns |
| `fork_tools` | allowlist of canonical tool names or MCP server patterns (`mcp__github`); calls outside it are rejected before approval; an empty array denies all |
| `fork_profile` | a saved restriction in `.qwen/fork-profiles/<name>.md`; not combinable with `fork_tools` |

Definitions live in project `.qwen/agents/`, user `~/.qwen/agents/`, and extensions, as Markdown with YAML frontmatter. The frontmatter can set the model, `approvalMode` (`default`, `plan`, `auto-edit`, `yolo`, or `bubble`), `tools`, `disallowedTools`, MCP servers, and hooks. Per-agent hooks are executable configuration. Upstream documents a v1 limitation: they register at session scope, so when two subagents with different per-agent hooks run concurrently, each one's hooks fire for the other's events.

`--max-subagent-depth` (default 5, maximum 100; setting `model.maxSubagentDepth`) bounds nesting. `SubagentStart` and `SubagentStop` carry the child's `agent_id` and `agent_type` and the root `session_id`, but no parent agent id; the [metadata sidecar](#subagent-transcripts-and-metadata) carries `parentSessionId`, `parentAgentId`, and `toolUseId`.

## Interactive dual output

`qwen --json-file <path> --input-file <path>` keeps the TUI on stdio while writing `stream-json` events to a separate file or FIFO and polling a regular file for commands. `--json-fd N` writes to an inherited descriptor, which suits a plain child spawn; a PTY host such as tmux or Zellij cannot pass descriptors above 2, so panes use `--json-file`.

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
    "protocol_version": 2,
    "version": "0.23.3",
    "supported_events": ["system", "user", "assistant", "stream_event", "result", "control_request", "control_response"]
  }
}
```

`protocol_version` is 2 from 0.21.11: version 2 bounds textual `tool_result.content` to 65,536 UTF-8 bytes after serialization and replaces longer values with head and tail previews (`DualOutputBridge.ts`, `DUAL_OUTPUT_PROTOCOL_VERSION`). Releases that predate the handshake fields omit `protocol_version` and `supported_events`, so a consumer feature-detects both. The channel shares the headless `stream-json` schema and always includes partial messages:

```jsonc
{ "type": "user", "session_id": "...", "message": { "role": "user", "content": [] }, "parent_tool_use_id": null }
{ "type": "assistant", "uuid": "...", "session_id": "...", "message": { "id": "...", "role": "assistant", "model": "...", "content": [{ "type": "text", "text": "..." }], "stop_reason": null, "usage": { "input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 2, "total_tokens": 15 } }, "parent_tool_use_id": null }
{ "type": "stream_event", "event": { "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "fragment" } }, "session_id": "..." }
{ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "...", "content": "...", "is_error": false }] }, "parent_tool_use_id": null }
```

Assistant content blocks are `text`, `thinking`, or `tool_use`. One message holds one block category, so a single model turn can emit several completed assistant envelopes. A non-null `parent_tool_use_id` marks subagent output.

Permission prompts arrive as control requests; `permission_suggestions` is `null` or an array such as `[{ "type": "allow", "label": "Allow Command", ... }]`:

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

The input file accepts two commands:

```jsonc
{ "type": "submit", "text": "follow-up prompt" }
{ "type": "confirmation_response", "request_id": "request UUID", "allowed": true }
```

Submits queue until the session is idle. Confirmations dispatch immediately: the first native or external answer wins, later answers drop, and a `control_response` reports success or error. The protocol has no control request for typed question or plan answers.

`--input-file` must be a regular file, because Qwen polls it with `fs.watchFile` every 500 ms; the output target may be a file or FIFO. A bad target, EPIPE, adapter exception, or more than 1 MiB buffered disables the bridge while Qwen keeps running. Clean shutdown emits `system/session_end`; a stream that closes without it ended abnormally, which does not by itself mean the TUI died.

## Headless runs

`qwen <query>` runs one headless turn: the positional prompt defaults to one-shot, and `-i/--prompt-interactive <prompt>` runs a prompt then stays interactive. `-p/--prompt` also runs headless and appends its text to stdin input; the CLI marks it deprecated in favour of the positional prompt (`top-level-options.ts`), while the headless docs still use it. `-p` cannot combine with a positional prompt or with `-i`.

| Option | Meaning |
| --- | --- |
| `--output-format text\|json\|stream-json` (`-o`) | `json` buffers an array of messages; `stream-json` writes JSONL |
| `--include-partial-messages` | adds `stream_event` deltas to `stream-json` |
| `--input-format text\|stream-json` | stdin format |
| `--json-schema <json\|@path>` | registers a synthetic `structured_output` tool the final answer must satisfy; the session ends on the first valid call |

The terminal `result` message (`CLIResultMessage` in `nonInteractive/types.ts`):

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

Error results carry an optional `error` object (`type`, `message`) instead of `result`. `modelUsage` and `stats` are optional, and the type admits extra keys. `stream-json` also carries `goal_state` events for the Goal continuation feature.

Exit codes come from the `FatalError` subclasses in `core/src/utils/errors.ts`:

| Code | Cause |
| --- | --- |
| 0 | success |
| 1 | general or API failure |
| 41 | authentication (`FatalAuthenticationError`) |
| 42 | invalid input (`FatalInputError`) |
| 44 | sandbox (`FatalSandboxError`) |
| 52 | configuration (`FatalConfigError`) |
| 53 | turn limit (`FatalTurnLimitedError`) |
| 54 | tool execution (`FatalToolExecutionError`) |
| 55 | run budget exceeded (`FatalBudgetExceededError`) |
| 130 | cancellation, SIGINT (`FatalCancellationError`) |

Three flags bound unattended work; each is unlimited (`-1`) by default. `--max-session-turns` exits 53. `--max-wall-time` takes seconds or a duration (`30s`, `5m`, `1.5h`; minimum 1 s) and exits 55. `--max-tool-calls` counts root tool dispatches, success or failure, excluding subagent inner tools and `structured_output`, caps at 1,000,000, and exits 55. `QWEN_CODE_UNATTENDED_RETRY=1` retries 429 and 529 responses indefinitely with capped backoff and stderr heartbeats. `--yolo` does not enable the sandbox.

## Authentication, providers, and quota

Qwen is multi-provider, and the effective provider comes from several settings together:

| Setting | Role |
| --- | --- |
| `security.auth.selectedType` | SDK protocol: `openai`, `openai-responses`, `anthropic`, `gemini`, `vertex-ai`, or the discontinued `qwen-oauth` (also CLI `--auth-type`) |
| `model.name` | selected model id |
| `model.baseUrl` | disambiguates duplicate model ids |
| `modelProviders` | provider id to `ModelConfig[]` (`id`, optional `envKey`, `name`, `description`, `baseUrl`, `generationConfig`); built-in ids must be auth types |
| `providerProtocol` | top-level map routing a custom provider id through a built-in protocol |

`selectedType: "openai"` alone therefore does not identify who bills the call.

Credentials resolve per provider. CLI `--openai-api-key` and `--openai-base-url` win for OpenAI-compatible providers. Otherwise the runtime reads `process.env[envKey]`, where environment values come from the process, `${QWEN_HOME:-~/.qwen}/.env`, and settings `env`; a model entry without `envKey` falls back to its auth type's default key, such as `OPENAI_API_KEY`. Common variables are `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_MODEL`, `QWEN_MODEL`, `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, `ANTHROPIC_MODEL`, `GEMINI_API_KEY`, `GEMINI_MODEL`, `GOOGLE_API_KEY`, and `GOOGLE_MODEL`. Credentials are never persisted in settings.

Alibaba sells two plans through OpenAI-compatible provider entries. A provider-specific key takes effect only when a provider entry declares it as `envKey`:

| Plan | Key | Endpoints |
| --- | --- | --- |
| Coding Plan | `BAILIAN_CODING_PLAN_API_KEY` | China `https://coding.dashscope.aliyuncs.com/v1`, international `https://coding-intl.dashscope.aliyuncs.com/v1` |
| Token Plan | `BAILIAN_TOKEN_PLAN_API_KEY` | China `https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1`, Singapore `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` |

`qwen auth` is removed: it prints a notice that points interactive users at `/auth`, headless users at provider environment variables, and auth status at `/doctor` (`cli/src/commands/auth.ts`). Qwen OAuth's free tier was discontinued on 2026-04-15, and the auth docs state it is not a selectable `/auth` entry; the removal notice still tells OAuth users to use `/auth`. A legacy `~/.qwen/oauth_creds.json` may remain.

Qwen publishes no machine-readable remaining-quota API or command. The only inspected quota wire is third party: CodexBar's `AlibabaCodingPlanUsageFetcher.swift` posts a Coding Plan API key to the region's fixed Alibaba console `/data/api.json` host and reads an active instance's 5-hour, 7-day, and 30-day quotas. It is not a Qwen contract. Provider-billed dollars need provider billing facts that neither Qwen nor its transcript supplies.

## CLI and environment reference

| Flag or variable | Meaning |
| --- | --- |
| `qwen --version` (`-v`) | version |
| `qwen [query..]` | interactive TUI without a query; one-shot headless with one |
| `-i, --prompt-interactive <prompt>` | run a prompt, then stay interactive |
| `-p, --prompt <prompt>` | headless; deprecated |
| `-c, --continue` / `-r, --resume [id]` / `--fork-session` / `--session-id <id>` | [session identity](#resume-fork-and-session-commands) |
| `-m, --model <id>` | startup model |
| `--fallback-model <id>` | up to three fallbacks for 429, 503, and 529 capacity errors |
| `--auth-type <type>` | `openai`, `openai-responses`, `anthropic`, `qwen-oauth`, `gemini`, `vertex-ai` |
| `--system-prompt <text>` / `--append-system-prompt <text>` | replace or append the main system prompt for this run; both take text |
| `--output-style <name>` | output style for this run |
| `--approval-mode <mode>` | `plan`, `default`, `auto-edit`, `auto` (LLM classifier approves safe actions), `yolo` |
| `-y, --yolo` | approve all tools |
| `--allowed-tools` / `--exclude-tools` / `--core-tools` | skip confirmation / remove tools / core tool set |
| `--disabled-slash-commands` | hide slash commands; merges `slashCommands.disabled` and `QWEN_DISABLED_SLASH_COMMANDS` |
| `-s, --sandbox` / `QWEN_SANDBOX=1` | sandbox, independent of approval mode |
| `--include-directories`, `--add-dir` | extra workspace roots |
| `--worktree [slug\|#PR\|URL]` | start inside `<repoRoot>/.qwen/worktrees/<slug>/` |
| `--json-fd`, `--json-file`, `--input-file` | [dual output](#interactive-dual-output) |
| `-o, --output-format`, `--input-format`, `--include-partial-messages`, `--json-schema` | [headless output](#headless-runs) |
| `--max-session-turns`, `--max-wall-time`, `--max-tool-calls` | [run budgets](#headless-runs) |
| `--max-subagent-depth <n>` | subagent nesting limit |
| `--chat-recording` | toggle session recording |
| `--bare` | skip implicit startup discovery; honour only explicit CLI inputs |
| `--safe-mode` / `QWEN_CODE_SAFE_MODE=true` | disable context files, hooks, extensions, skills, and MCP servers |
| `--channel <name>` | caller identity: `VSCode`, `ACP`, `SDK`, `CI`, `desktop`, `daemon` |
| `--acp` | ACP over stdio |
| `QWEN_SYSTEM_MD` | replace the base system prompt verbatim from a file; `1` or `true` selects `.qwen/system.md`, `0` or `false` disables; a missing file is a hard error; output style and `QWEN_SYSTEM_IDENTITY_MD` are ignored while it applies |
| `QWEN_HOME` / `QWEN_RUNTIME_DIR` | config root / runtime root |
| `QWEN_CODE_SUPPRESS_YOLO_WARNING=1` | silence the `--yolo` startup warning |
| `NO_COLOR` | suppress ANSI where supported |

Configuration layers apply in this order, later winning: defaults, system defaults, user settings, project settings, system settings, environment and `.env`, then CLI flags. `QWEN_CODE_SYSTEM_DEFAULTS_PATH` and `QWEN_CODE_SYSTEM_SETTINGS_PATH` relocate the two system layers.

## ACP and daemon mode

`qwen --acp` speaks ACP over stdio. `qwen serve` hosts shared sessions over HTTP and SSE as an experimental daemon that owns ACP children: it binds `127.0.0.1:4170` by default, is tokenless on loopback unless `--require-auth`, generates a bearer token for a non-loopback bind when neither `--token` nor `QWEN_SERVER_TOKEN` supplies one, caps live sessions (`--max-sessions`, default 32), and serves a Web Shell unless `--no-web`. Both give a dedicated client structured prompts, permissions, session lifecycle, model changes, and replay. Neither observes a separately launched TUI pane.

## Upstream scope

The surfaces above cover what Qwen Code ships at 0.23.3. Several exist that a pane observer need not adopt: dual output's reverse channel, the live session registry's peer-messaging socket, ACP and `qwen serve`, `goal_state` stream events and records, and delegation of a subagent turn to an external ACP agent. Which of these RimZ reads, and the gaps it records, are in [adapter_qwen.md](../../internals/agents/adapter_qwen.md#known-gaps).

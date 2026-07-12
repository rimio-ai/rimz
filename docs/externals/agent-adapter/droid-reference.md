# Factory Droid CLI protocol reference

> RimZ's shipped mapping is [droid.md](../../internals/agents/droid.md). This document records the upstream protocol facts that mapping is built on; the agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md), and the account, balance, spend, and pricing contract is [providers.md](../../internals/agents/providers.md).

This is the single home for the **Factory Droid CLI upstream protocol surface** relevant to RimZ: lifecycle hooks, session identity and transcripts, permissions and questions, compaction, subagents and missions, model and autonomy settings, resume and fork behavior, non-interactive execution, the stream JSON-RPC transport, authentication, and usage.

Stock-pane coverage targets the installed release named below. Structured `droid exec` coverage stays pinned to the public SDK revision named below until that transport is implemented and refreshed. This reference calls out the places where Factory publishes no stable observation contract rather than promoting an inference into protocol.

## Refresh target and product identity

The stock CLI and hook surface were refreshed against an installed **Droid CLI 0.170.0** and Factory's live official documentation. The structured exec sections remain pinned to public TypeScript SDK **0.6.0**, Factory protocol **1.51.0**, at source commit [`d960f18f3a5a3bdbbc867a2177275a794663b175`](https://github.com/Factory-AI/droid-sdk-typescript/tree/d960f18f3a5a3bdbbc867a2177275a794663b175).

The executable is `droid`. `droid -v` or `droid --version` prints its version, and `droid update` installs the latest release. RimZ records the tested release as a fixture-freshness boundary rather than applying a runtime version gate; refresh the hook goldens and protocol research when behavior drifts. This reference intentionally carries no compatibility contract for older Droid releases.

Factory publishes the CLI as a proprietary binary. The public SDK is the official typed source for the `droid exec` JSON-RPC protocol, while the documentation is authoritative for the stock CLI, hook configuration, and command behavior.

## Upstream sources

Re-fetch the live pages and compare the pinned SDK source when refreshing this mirror.

| Surface | Source |
| --- | --- |
| CLI overview and installation | <https://docs.factory.ai/cli/getting-started/overview> |
| CLI commands, flags, resume/fork, and exit codes | <https://docs.factory.ai/reference/cli-reference> |
| Latest release | <https://docs.factory.ai/changelog/release-notes> |
| Settings and configuration locations | <https://docs.factory.ai/cli/configuration/settings> |
| Hook guide | <https://docs.factory.ai/cli/configuration/hooks-guide> |
| Hook events, payloads, decisions, and execution | <https://docs.factory.ai/reference/hooks-reference> |
| Autonomy levels | <https://docs.factory.ai/cli/user-guides/auto-run> |
| Specification mode | <https://docs.factory.ai/cli/user-guides/specification-mode> |
| Custom Droids / subagents | <https://docs.factory.ai/cli/configuration/custom-droids> |
| Headless execution and raw JSON-RPC | <https://docs.factory.ai/cli/droid-exec/overview> |
| TypeScript SDK | <https://github.com/Factory-AI/droid-sdk-typescript> |
| JSON-RPC envelope and version constants | [`shared.ts`](https://github.com/Factory-AI/droid-sdk-typescript/blob/d960f18f3a5a3bdbbc867a2177275a794663b175/src/schemas/shared.ts), [`constants.ts`](https://github.com/Factory-AI/droid-sdk-typescript/blob/d960f18f3a5a3bdbbc867a2177275a794663b175/src/schemas/constants.ts) |
| Client requests and session results | [`client.ts`](https://github.com/Factory-AI/droid-sdk-typescript/blob/d960f18f3a5a3bdbbc867a2177275a794663b175/src/schemas/client.ts) |
| Notifications, permissions, and questions | [`server.ts`](https://github.com/Factory-AI/droid-sdk-typescript/blob/d960f18f3a5a3bdbbc867a2177275a794663b175/src/schemas/server.ts) |
| Protocol enums | [`enums.ts`](https://github.com/Factory-AI/droid-sdk-typescript/blob/d960f18f3a5a3bdbbc867a2177275a794663b175/src/schemas/enums.ts) |
| Message and content-block schemas | [`messages.ts`](https://github.com/Factory-AI/droid-sdk-typescript/blob/d960f18f3a5a3bdbbc867a2177275a794663b175/src/schemas/messages.ts) |
| Process transport | [`transport.ts`](https://github.com/Factory-AI/droid-sdk-typescript/blob/d960f18f3a5a3bdbbc867a2177275a794663b175/src/transport.ts) |
| SDK usage guide | [`sdk-usage-guide.md`](https://github.com/Factory-AI/droid-sdk-typescript/blob/d960f18f3a5a3bdbbc867a2177275a794663b175/docs/sdk-usage-guide.md) |

## Recommended adapter shape

Keep the stock interactive `droid` TUI in the pane. Install **command hooks** for session registration, turn boundaries, completed tool work, compaction, and session termination. Retain the hook-provided `transcript_path` as identity/enrichment metadata, but do not parse the file until Factory publishes its schema or RimZ deliberately adds a version-pinned, reverse-engineered parser with fixtures.

Use a separate **`droid exec` path** for RimZ supervised `-p` runs. Its `--output-format json` result is sufficient for a one-shot run; `stream-jsonrpc` provides typed live state, permission and question requests, token/context data, interruption, compaction, fork, and multi-turn control when the harness needs them. The JSON-RPC process replaces the interactive TUI and therefore does not enrich a concurrently running stock pane session.

The latest official surfaces do not support a fully faithful stock-pane adapter. Hooks have no permission-request event, question event, subagent-start event, model field, token/context field, or structured turn-error event. `Notification` reports permission attention only as a human message and also fires after 60 seconds of input idleness. Treat a first interactive adapter as basic lifecycle support, and keep capabilities that need a structured source disabled.

| RimZ need | Stock interactive pane | Structured `droid exec` path |
| --- | --- | --- |
| Pane-to-session binding | `SessionStart.session_id`, `cwd`, `transcript_path` | initialize/load response `sessionId` |
| Turn start and prompt | `UserPromptSubmit.prompt` | `droid.add_user_message` plus user `create_message` |
| Clean turn close | `Stop` | working-state transition from non-idle to `idle` |
| Failed turn close | no structured hook | `error` notification plus process exit |
| Acting / tool work | `PostToolUse.tool_name` | `tool_call`, `tool_result`, working state `executing_tool` |
| Permission wait | `Notification.message`, ambiguous and unstructured | `droid.request_permission` |
| User question | no dedicated hook | `droid.ask_user` |
| Plan approval | no dedicated hook; `PreToolUse` sees the tool but not whether it blocks | permission detail `exit_spec_mode` |
| Compaction open | `PreCompact.trigger` | working state `compacting_conversation` or explicit compact request |
| Compaction close | following `SessionStart.source = "compact"` | compact result `newSessionId` then load replacement |
| Context fill | unavailable | `droid.get_context_stats` |
| Token usage | unavailable | `session_token_usage_changed` |
| Model and effort | no interactive launch flag (config/`--settings` inference only); live `/model` changes are invisible | init/load `settings`, `settings_updated` |
| Subagents | only identity-less `SubagentStop` | Task tool progress may carry `subagentSessionId`; missions expose worker session IDs |
| Auth presence | browser login behavior or `FACTORY_API_KEY`; no machine-readable status command | stored-login fallback or explicit API key |
| Quota / rate windows | no official local API | no official SDK field |
| Native resume | `--resume [sessionId]` | `--session-id`, `droid.load_session` |
| Native fork | `--fork <sessionId>` / `/fork` | `--fork`, `droid.fork_session` |

Treat all hook and JSON-RPC objects as forward-extensible: require the fields documented here, ignore unknown keys, and quarantine unknown discriminants rather than failing the whole stream. Process every hook invocation because the payload has no event ID or timestamp and two legitimate repeated events may be byte-identical; the native hook surface offers no safe content-based deduplication key.

## Executable, launch modes, and version

`droid` starts the interactive TUI. A positional prompt starts the same TUI and immediately submits that prompt. Adapter-relevant interactive flags are:

| Flag | Meaning |
| --- | --- |
| `-v`, `--version` | print the CLI version |
| `-r`, `--resume [sessionId]` | resume the named session or the most recently modified session when omitted |
| `--fork <sessionId>` | copy the session and resume the copy under a fresh ID |
| `--worktree [name]`, `-w [name]` | run in a native sibling Git worktree |
| `--worktree-dir <path>` | directory for worktree creation |
| `--append-system-prompt <text>` | append text to the system prompt |
| `--append-system-prompt-file <path>` | append a file to the system prompt |
| `--settings <path>` | merge a runtime settings file for this process only |
| `--auto low\|medium\|high` | start this interactive session at the selected autonomy level |
| `--use-spec` | start this interactive session in specification mode |
| `--cwd <path>` | set the working directory |

Interactive 0.170.0 exposes **no** `-m`/`--model` or `-r`/`--reasoning-effort` flag: verified against the installed binary's `droid --help` and by the fact that `droid --model` does not raise commander's "argument missing" error the way `droid --auto` and `droid --cwd` do (it launches the TUI, treating `--model` as an ignored unknown option). Interactive `-r` is `--resume`. Model and reasoning effort are `droid exec`-only launch flags (below) or in-session controls (`/model`, Tab); a runtime model can also ride `--settings <path>`. The interactive CLI accepts unknown options silently, so an ignored `--model <id>` would fold the id into the positional prompt rather than failing — do not emit exec-only flags on the interactive launch.

`droid exec` is the non-interactive command. Its adapter-relevant flags are:

| Flag | Meaning |
| --- | --- |
| `-f`, `--file <path>` | read the prompt from a file; stdin is also accepted |
| `-o`, `--output-format <format>` | `text`, `json`, `stream-json`, or `stream-jsonrpc` |
| `--input-format stream-jsonrpc` | accept line-delimited multi-turn JSON-RPC |
| `-s`, `--session-id <id>` | continue an existing session |
| `--fork <id>` | fork an existing session and continue the copy |
| `-m`, `--model <id>` | select the execution model |
| `-r`, `--reasoning-effort <level>` | select reasoning effort |
| `--use-spec` | begin in specification mode |
| `--spec-model`, `--spec-reasoning-effort` | select the planning model and effort |
| `--auto low\|medium\|high` | pre-authorize work through the selected risk tier |
| `--skip-permissions-unsafe` | bypass every permission check |
| `--enabled-tools`, `--disabled-tools` | override tool availability |
| `--cwd <path>` | set the working directory |
| `--worktree [name]`, `--worktree-dir <path>` | isolate the run in a Git worktree |
| `--mission` | use multi-agent mission orchestration |
| `--worker-model`, `--validator-model` | choose mission role models |
| `--tag`, `--log-group-id` | attach searchable run metadata |

The normal process argv does not publish the freshly generated interactive session ID. Bind a pane only after `SessionStart`; do not choose the newest transcript by mtime.

## Configuration, hierarchy, and trust

The user settings file is `~/.factory/settings.json` on macOS/Linux and `%USERPROFILE%\.factory\settings.json` on Windows. Current configuration also supports `settings.local.json` beside a user or project settings file and project `.factory/settings.json` / `.factory/settings.local.json`. Enterprise managed policy and plugin hooks join those sources.

Hooks are executable configuration:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/rimz hooks droid"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/rimz hooks droid",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
```

Each event maps to an array of matcher groups, and each group contains a `hooks` array. A command hook has `type: "command"`, `command`, and optional `timeout` in seconds. `PreToolUse` and `PostToolUse` accept a case-sensitive exact string or regular expression matcher; `*`, an empty string, and an omitted matcher select every tool. Other events do not use tool matchers, though `PreCompact` and `SessionStart` have documented event matcher values.

Commands run through the shell in Droid's current working directory with Droid's environment. `$FACTORY_PROJECT_DIR` points to the absolute directory where Droid started. Use the absolute RimZ executable path because the current directory may change. Matching commands run in parallel, and duplicate identical command strings are deduplicated. The default per-command timeout is 60 seconds.

Plugin hook files merge with user and project hooks and may interpolate `${DROID_PLUGIN_ROOT}`. Include every effective hook command, timeout, matcher, settings tier, and enabled plugin-provided executable hook in RimZ's trust review. Preview the exact settings diff before installation. Droid snapshots hooks at session start, warns when settings are edited externally, and requires `/hooks` review before changes affect that live session.

Hook stdout is the upstream decision channel. The neutral RimZ observation response is empty stdout and exit 0. Send diagnostics to stderr or RimZ logs, and give helper children fresh stdio.

## Command hooks

A hook receives one JSON object on stdin. Every current hook payload contains:

```json
{
  "session_id": "00893aaf-19fa-41d2-8238-13269b9b3ca0",
  "transcript_path": "/home/me/.factory/projects/.../00893aaf-19fa-41d2-8238-13269b9b3ca0.jsonl",
  "cwd": "/home/me/project",
  "permission_mode": "off",
  "hook_event_name": "UserPromptSubmit"
}
```

`permission_mode` is one of `off`, `spec`, `auto-low`, `auto-medium`, or `auto-high`. It is useful as per-event enrichment, but it combines interaction and autonomy into a display value and is not session identity.

### Event catalog and lifecycle mapping

| Event | Event-specific fields | Timing | RimZ mapping |
| --- | --- | --- | --- |
| `SessionStart` | `source` | new session or replacement session after resume, clear, or compact | `Registered`; `source = compact` also closes compaction |
| `UserPromptSubmit` | `prompt` | before Droid processes a submitted prompt | `TurnStarted`, task/prompt |
| `PreToolUse` | `tool_name`, `tool_input` | after parameters exist, before permission and execution | policy only; not proof of work or waiting |
| `PostToolUse` | `tool_name`, `tool_input`, `tool_response` | immediately after successful completion | `ToolUsed`; file-editing subset sets `edits` |
| `Notification` | `message` | permission attention, or input idle for at least 60 seconds | silent enrichment only unless a tested structured discriminator appears |
| `Stop` | `stop_hook_active` | response finished; omitted on user interrupt | clean `TurnEnded` |
| `SubagentStop` | `stop_hook_active` | Task sub-droid finished | insufficient identity; do not emit a child lifecycle event |
| `PreCompact` | `trigger`, `custom_instructions` | before manual or automatic compaction | `Compacting` |
| `SessionEnd` | `reason` | session closes | `Ended` when the pane session truly terminates |

`SessionStart.source` is `startup`, `resume`, `clear`, or `compact`. Resume currently starts a replacement session under the hood, so do not assume the hook's new `session_id` equals the requested resume ID. `clear` and `/new` end the old session and establish a new one. `compact` is the only official hook-side close signal for a `PreCompact` bracket; Droid has no `PostCompact` hook.

`SessionEnd.reason` is `clear`, `logout`, `prompt_input_exit`, or `other`. A clear/new transition produces a tombstone for the old ID followed by registration of the new ID. Pane/process liveness remains instance truth.

`PreCompact.trigger` is `manual` or `auto`. `custom_instructions` carries the argument passed to `/compact` for manual compaction and is empty for automatic compaction. Map a following compact-source registration carefully: if its session ID changes, close/count compaction on the old session and register the replacement rather than applying `CompactionEnded` to an unseen ID.

`Stop` does not fire after a user interrupt. The official hook set has no interruption or failure event, so pane liveness plus a future documented transcript/side-channel contract must provide the missing death certificate. Do not project a quiet running row to failure from hook silence alone.

### Tool classification

Official common tool names include:

| Tool | RimZ treatment |
| --- | --- |
| `Create`, `Edit`, `ApplyPatch` | mutating `ToolUsed { edits: true }` after `PostToolUse` |
| `Execute` | work without a file-edit proof; keep the existing turn phase |
| `Glob`, `Grep`, `Read`, `LS` | read-only work; keep reasoning phase |
| `FetchUrl`, `WebSearch` | read-only network work |
| `Task` | subagent delegation; hook payload has no child identity |
| `TodoWrite` | work/enrichment without file-edit proof |

MCP tools use `mcp__<server>__<tool>`. Their mutation semantics are server-defined, so the generic adapter must not infer file edits from the name.

The exact `tool_input` and `tool_response` shape varies by tool. The hook reference gives `Create` as the worked shape:

```json
{
  "tool_name": "Create",
  "tool_input": {
    "file_path": "/path/to/file.txt",
    "content": "file content"
  },
  "tool_response": {
    "filePath": "/path/to/file.txt",
    "success": true
  }
}
```

Treat both values as opaque JSON outside fields covered by a tool-specific fixture. Avoid persisting source content from either payload.

### Hook output and exit semantics

| Result | Droid behavior |
| --- | --- |
| exit `0`, empty stdout | allow; neutral observation |
| exit `0`, plain stdout | shown in detailed transcript; for `UserPromptSubmit` and `SessionStart`, added to model context |
| exit `0`, structured JSON | apply the event-specific decision |
| exit `2` | event-specific blocking/feedback behavior using stderr |
| any other exit | non-blocking error; show stderr and continue |

Exit 2 blocks `PreToolUse`, `UserPromptSubmit`, `Stop`, and `SubagentStop`. On `PostToolUse` the tool has already run and stderr becomes feedback; on `Notification`, `PreCompact`, `SessionStart`, and `SessionEnd` it cannot block the native operation.

All structured outputs may carry:

```json
{
  "continue": true,
  "stopReason": "reason shown to the user when continue is false",
  "suppressOutput": true,
  "systemMessage": "warning shown to the user"
}
```

`continue: false` stops processing and takes precedence over event-specific `decision` fields. It is not the neutral observation path.

`PreToolUse` uses:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "reason",
    "updatedInput": {}
  }
}
```

`permissionDecision` is `allow`, `deny`, or `ask`. `allow` bypasses the permission UI, `deny` rejects the call and feeds the reason to Droid, and `ask` forces the native confirmation UI. `updatedInput` replaces or adds tool input fields. The older top-level `decision: "approve"|"block"` form is deprecated in the latest documentation and should not be emitted by RimZ.

`PostToolUse` and `UserPromptSubmit` may return top-level `decision: "block"`, `reason`, and `hookSpecificOutput.additionalContext`. Post-tool blocking feeds corrective feedback after the action; prompt blocking erases the prompt and shows the reason only to the user. `Stop` and `SubagentStop` use `decision: "block"` plus a required `reason` to request another agent step. `stop_hook_active` prevents an infinite continuation loop.

RimZ's observation hook emits none of these decisions. Golden-test byte-empty stdout, exit 0, and stderr-only diagnostics against Droid 0.170.0.

## Waiting and structured asks

The stock interactive hook API cannot authoritatively emit RimZ `AwaitingInput`.

`Notification` fires when Droid needs permission and when the prompt has been idle for at least 60 seconds. Its only event-specific field is a display `message`; there is no request ID, tool-call ID, question list, plan, or closed notification subtype. `PreToolUse` proves only that Droid proposed a call, since allowed calls proceed without waiting. There is no `PermissionRequest`, `AskUser`, or permission-resolved hook.

Keep structured asks disabled for the first stock-pane adapter. If a later implementation temporarily classifies notification strings, label it version-pinned heuristic enrichment, never durable permission truth, and clear it on newer activity under the shared waiting guard.

The headless JSON-RPC path supplies authoritative requests, detailed in [Structured permissions and questions](#structured-permissions-and-questions).

## Session identity, transcripts, resume, and fork

Every hook carries `session_id` and `transcript_path`. The published examples place transcripts under:

```text
~/.factory/projects/<project-key>/<session-id>.jsonl
```

Factory documents the path as conversation JSON and uses a `.jsonl` example, but publishes no record schema, durability rules, directory-key algorithm, rotation contract, or locking semantics. Use the exact hook-provided path. Store it as carry-forward metadata, expand `~` explicitly, and do not derive it from cwd or session ID.

The official docs suggest processing the transcript to prevent stop-hook loops, but that statement does not define a parser contract. A transcript parser needs a separate, current-version capture study and golden fixtures before implementation; it remains a best-effort private format even then.

Interactive resume and fork behavior:

- `droid --resume <sessionId>` resumes a named session; an omitted ID selects the most recently modified session.
- `/sessions` lists/selects sessions for the current directory plus favorites.
- `/fork` and `droid --fork <sessionId>` duplicate all messages and continue under a fresh session ID.
- `/rewind-conversation` branches from an earlier point and can restore file changes.
- `/clear` and `/new` start a fresh session.
- `/compress [instructions]`, `/compact`, and `/handoff` compact history; current protocol compaction returns a fresh session ID.

Never merge old and replacement IDs in the rollup. Preserve explicit parent/fork provenance only when a structured result reports it; the hook payload does not.

## Model, reasoning, interaction, and autonomy

Current settings relevant to an adapter are:

| Setting / protocol field | Values |
| --- | --- |
| `model` / `modelId` | available Factory or configured custom model ID |
| `reasoningEffort` | model-dependent; public protocol includes `none`, `dynamic`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |
| `interactionMode` | `auto`, `spec`, or mission/AGI mode in the protocol |
| `autonomyLevel` | `off`, `low`, `medium`, `high` |
| `cloudSessionSync` | mirror CLI sessions to Factory web when true |

`commandAllowlist` pre-approves configured commands, while `commandDenylist` always requires confirmation; a command in both follows the denylist. Commands in neither fall back to the session autonomy level. These lists affect whether the adapter will observe a wait, but they are policy enrichment rather than lifecycle truth.

Spec mode is a read-only planning phase. Interactive Shift+Tab cycles Auto, Spec, and Mission; Ctrl+L cycles autonomy; Tab cycles reasoning effort; `/model` switches model during the session. Hook payload `permission_mode` reports `off`, `spec`, or `auto-<level>` at each event but omits model and reasoning effort.

The stock interactive CLI now accepts launch-scoped `--auto <level>` and `--use-spec`, while its unsafe bypass remains exec-only. Map interactive RimZ modes as follows:

| RimZ mode | Interactive Droid launch |
| --- | --- |
| ask | omit an override and retain the configured autonomy/native prompt UI |
| plan | `--use-spec` |
| auto | `--auto medium` as the closest local-development tier |
| yolo | unsupported; interactive Droid exposes no unsafe bypass |

The structured exec path has its own permission mapping:

| RimZ mode | Droid exec launch |
| --- | --- |
| ask | omit `--auto` (read-only/spec behavior; permission violations fail fast) |
| plan | `--use-spec` with no mutation authority |
| auto | `--auto medium` as the closest local-development tier |
| yolo | `--skip-permissions-unsafe`, only under RimZ's existing explicit unsafe posture |

For an ask-mode supervised exec run that may require mutation, use `stream-jsonrpc`, omit `--auto`, and answer `droid.request_permission`; plain one-shot exec cannot stop for a human and fails fast when requested work exceeds its authority. Keep persistent settings user-owned and fail fast when a requested posture has no launch-scoped equivalent.

### Status line is not an observation API

`statusLine` configures a shell command whose stdout Droid renders above the input, with optional `padding` and `maxRows`; `/statusline` manages it interactively. Factory does not document structured session JSON on this command's stdin or environment. Treat it as user-owned presentation and leave it untouched. It cannot supply the model, context, cost, or session binding that Claude's statusline supplies to RimZ.

## Subagents and missions

Custom Droids are Task-tool subagents defined as Markdown under project `.factory/droids/` or user `~/.factory/droids/`. Each has its own prompt, model or `inherit`, and tool policy. Project definitions override user definitions with the same name. A Task child receives an isolated context window, and its live tool progress streams through the parent Task call.

The hook API exposes only `SubagentStop`, with the common fields and `stop_hook_active`. It has no `SubagentStart`, child session ID, parent ID, Task tool-call ID, subagent type, model, transcript path for the child, or success bit. It cannot back RimZ child rows.

The structured protocol has two useful but distinct child surfaces:

- `tool_progress_update.update.subagentSessionId` may identify the child behind a Task tool call.
- Mission orchestration emits `mission_worker_started { workerSessionId }` and `mission_worker_completed { workerSessionId, exitCode }`, and load results carry mission worker IDs/state.

Mission workers are orchestration sessions, not necessarily the same product concept as ordinary Task subagents. Declare separate capability handling until fixtures prove the parent/child binding and terminal semantics for each. A worker exit code of zero maps to clean `SubagentStopped`; nonzero maps to errored.

## Headless one-shot output

`droid exec` is read-only by default and exits nonzero on a permission violation, tool error, or unmet objective. Factory documents exit 0 as success and all nonzero codes as failure; it does not publish a finer numeric catalog.

`--output-format json` emits one result object:

```json
{
  "type": "result",
  "subtype": "success",
  "is_error": false,
  "duration_ms": 5657,
  "num_turns": 1,
  "result": "final assistant text",
  "session_id": "8af22e0a-d222-42c6-8c7e-7a059e391b0b"
}
```

Parse `type`, `subtype`, `is_error`, `duration_ms`, `num_turns`, `result`, and `session_id`; tolerate additional fields. Preserve the process exit status as the supervised-run verdict rather than trusting assistant text.

`--output-format stream-json` is listed as a supported format, but the current live docs do not publish its complete event schema. Prefer the typed JSON-RPC format for a new streaming implementation.

## Stream JSON-RPC transport

Start the protocol process with:

```sh
droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc
```

Add the desired `--auto`/model/cwd flags to the process launch. Each stdin and stdout line is one complete JSON-RPC message. Stdout is protocol-only; stderr carries process diagnostics. Serialize writes so concurrent requests cannot interleave. On shutdown, close stdin, send SIGTERM, allow a bounded grace period, then kill the child if needed; the official SDK uses five seconds.

### Envelope and versioning

Every current envelope contains:

```json
{
  "jsonrpc": "2.0",
  "factoryApiVersion": "1.0.0",
  "factoryProtocolVersion": "1.51.0",
  "type": "request",
  "id": "client-generated-unique-id",
  "method": "droid.initialize_session",
  "params": {}
}
```

`factoryApiVersion` is a frozen required legacy envelope value. Negotiate and gate on `factoryProtocolVersion`. Requests have `type: "request"`, string `id`, method, and params. Responses have matching `id` plus `result`, or `error { code, message, data? }`. Notifications have `type: "notification"`, method, and params with no ID.

Current error codes are JSON-RPC parse/invalid request/method/params/internal errors plus `-32001` authentication, `-32004` entity not found, and `-32005` session disconnected.

### Session initialization and turn control

Begin with one of:

```json
{
  "method": "droid.initialize_session",
  "params": {
    "machineId": "rimz-host-id",
    "cwd": "/absolute/project",
    "modelId": "model-id",
    "interactionMode": "auto",
    "autonomyLevel": "off",
    "reasoningEffort": "high"
  }
}
```

```json
{
  "method": "droid.load_session",
  "params": {
    "sessionId": "existing-session-id"
  }
}
```

Initialize returns `sessionId`, opaque `session`, effective `settings`, and optional MCP servers, Git repo, and available models. Load returns opaque `session`, effective settings, and optional pending permissions, pending questions, agent-loop flag, queued messages, cwd, token usage, mission snapshot, and parent calling session/tool IDs. Use these pending arrays to reconstruct waits after reconnect, but parse their contents only through the corresponding request schema when possible.

Submit a turn with:

```json
{
  "method": "droid.add_user_message",
  "params": {
    "messageId": "optional-client-id",
    "text": "fix the parser"
  }
}
```

`images`, text/PDF `files`, and a JSON-schema structured `outputFormat` are optional. `droid.interrupt_session` and `droid.close_session` accept empty params except that close may include `reason: clear|logout|prompt_input_exit|other`.

Other lifecycle-relevant client methods are:

| Method | Params | Result |
| --- | --- | --- |
| `droid.update_session_settings` | model, effort, interaction/autonomy, tool overrides | empty |
| `droid.compact_session` | optional `customInstructions` | `newSessionId`, `removedCount` |
| `droid.fork_session` | `{}` | `newSessionId` |
| `droid.get_context_stats` | `{}` | context stats below |
| `droid.rename_session` | `title` | `success` |
| `droid.kill_worker_session` | `workerSessionId` | empty |

### Notifications and lifecycle mapping

Every event arrives as:

```json
{
  "type": "notification",
  "method": "droid.session_notification",
  "params": {
    "notification": {
      "type": "droid_working_state_changed",
      "newState": "executing_tool"
    }
  }
}
```

Current working states are `idle`, `streaming_assistant_message`, `waiting_for_tool_confirmation`, `executing_tool`, and `compacting_conversation`.

| Notification | Key fields | RimZ use |
| --- | --- | --- |
| `droid_working_state_changed` | `newState` | turn/phase truth; a non-idle → idle transition ends one turn |
| `create_message` | full `message`, optional `parentId`, `requestId` | durable-grained user/assistant/tool message |
| `assistant_text_delta` / `assistant_text_complete` | `messageId`, `blockIndex`, delta | live answer only |
| `thinking_text_delta` / `thinking_text_complete` | message/block IDs, delta, optional duration | reasoning enrichment only |
| `tool_call` | `toolUse { id, name, input }` | work starts; correlate results/permissions |
| `tool_result` | `messageId`, `toolUseId`, content, optional `isError` | completed work and error evidence |
| `tool_progress_update` | tool IDs/name, typed update | progress; optional `subagentSessionId` |
| `error` | message, `errorType`, timestamp, optional detail | failed-turn evidence |
| `session_token_usage_changed` | `sessionId`, token usage | token/context enrichment |
| `settings_updated` | current model/mode/effort/tool overrides | live carry-forward settings |
| `permission_resolved` | request/tool IDs, selected option | clear structured wait |
| `mission_worker_started` / `mission_worker_completed` | worker ID, terminal exit code | mission child lifecycle |
| `hook_execution_started` / `hook_execution_completed` | hook IDs, event, commands/results | diagnostics only; avoid double-counting command hooks |

`create_message.message` has `id`, role `user|assistant|tool|system`, `content[]`, numeric `createdAt`/`updatedAt`, and optional `parentId`, visibility, and `isError`. Content blocks are discriminated by `type`: `text`, `image`, `thinking`, `redacted_thinking`, `tool_use`, `tool_result`, or `document`. Persist user-visible text according to RimZ privacy policy; exclude thinking and redacted thinking from lifecycle and durable transcript by default.

The official SDK defines turn completion as a working-state transition from any non-idle state back to `idle`. Mirror that bracket. `streaming_assistant_message` is still running/reasoning, `executing_tool` is running and moves to acting only when the correlated tool edits files, `waiting_for_tool_confirmation` is waiting, and `compacting_conversation` opens the compacting head.

### Structured permissions and questions

`droid.request_permission` is a server-to-client request and must receive a response with the same JSON-RPC `id`. Params contain `toolUses[]` plus the exact `options[]` the current CLI permits:

```json
{
  "method": "droid.request_permission",
  "params": {
    "toolUses": [
      {
        "toolUse": {
          "type": "tool_use",
          "id": "tool-1",
          "name": "Execute",
          "input": { "command": "cargo xtask gate" }
        },
        "confirmationType": "exec",
        "details": {
          "type": "exec",
          "fullCommand": "cargo xtask gate",
          "command": "cargo xtask gate",
          "impactLevel": "medium"
        }
      }
    ],
    "options": [
      { "label": "Run once", "value": "proceed_once" },
      { "label": "Cancel", "value": "cancel" }
    ]
  }
}
```

Confirmation detail types are `edit`, `exec`, `create`, `ask_user`, `exit_spec_mode`, `propose_mission`, `start_mission_run`, `apply_patch`, and `mcp_tool`. The typed details carry file paths/content or diffs, commands/impact, parsed questionnaires, spec plans/options, mission proposals, or MCP tool identity as appropriate. Avoid persisting file content, patches, and commands beyond the active ask record.

Answer with one offered value rather than synthesizing a choice:

```json
{
  "selectedOption": "proceed_once",
  "comment": "optional user comment"
}
```

Known current values include `proceed_once`, `proceed_always`, the auto-run and new-session variants for low/medium/high, `proceed_edit`, and `cancel`. Treat the server-provided `options` list as authoritative.

`droid.ask_user` carries a `toolCallId` and questions:

```json
{
  "method": "droid.ask_user",
  "params": {
    "toolCallId": "tool-ask-1",
    "questions": [
      {
        "index": 0,
        "topic": "Database",
        "question": "Which migration strategy?",
        "options": ["online", "maintenance window"]
      }
    ]
  }
}
```

Answer with:

```json
{
  "cancelled": false,
  "answers": [
    {
      "index": 0,
      "question": "Which migration strategy?",
      "answer": "online"
    }
  ]
}
```

Map `exit_spec_mode` to plan approval, ordinary permission types to permission, and `droid.ask_user` to question. When no RimZ/native UI handler is available, the official SDK's safe default is cancellation rather than implicit approval.

### Context and token usage

`droid.get_context_stats` returns:

```json
{
  "used": 42000,
  "remaining": 158000,
  "limit": 200000,
  "accuracy": "exact",
  "updatedAt": "2026-05-07T12:00:00.000Z"
}
```

`accuracy` is `exact` or `estimated`. Derive RimZ context percent as `used / limit`, guarding zero and clamping to the range. Prefer these direct stats over reconstructing fill from cumulative token usage.

`session_token_usage_changed.tokenUsage` contains `inputTokens`, `outputTokens`, `cacheCreationTokens`, `cacheReadTokens`, and `thinkingTokens`. These are session usage counters, not the current context composition and not dollar cost. The current exec notification schema publishes no price, spend, rate-limit window, reset time, or subscription plan.

## Authentication, account, quota, and logs

Interactive first run opens browser sign-in. Headless automation may set `FACTORY_API_KEY`; Factory instructs users to create it in the Factory settings page. The official SDK permits exec mode to fall back to the CLI's stored login when no API key value is supplied.

Factory publishes no `droid auth status --json` equivalent, supported credential-file schema, local token location, quota endpoint, rate-limit response, or machine-readable plan probe for the CLI. Do not scan keyrings or configuration for token bytes. An adapter can report auth as unknown until a cheap official command or protocol handshake succeeds, and must keep quota/balance capabilities disabled.

`/stats`, `/cost`, `/account`, and `/billing` are interactive user interfaces, not parser contracts. The public Analytics API is an organization reporting surface and is not documented as the logged-in CLI account/budget source RimZ needs.

`droid --debug` emits diagnostic details. `FACTORY_LOG_FILE` selects a log file and `FACTORY_DISABLE_KEYRING` controls keyring use, but neither is a lifecycle protocol. Keep debug logs out of the adapter truth path and never ingest them as a transcript.

## Implementation checklist

Before enabling the adapter:

1. Pin stock-hook fixtures to `droid --version` 0.170.0 and structured exec fixtures to protocol `factoryProtocolVersion` 1.51.0.
2. Install the minimal hook set: `SessionStart`, `UserPromptSubmit`, `PostToolUse`, `Stop`, `PreCompact`, and `SessionEnd`; install `Notification` only for silent enrichment and `SubagentStop` only when a future payload supplies identity.
3. Add trust-hash fixtures for every settings tier, matcher, command, timeout, and plugin hook that can execute.
4. Golden-test every hook stdin payload and byte-empty success stdout on the pinned binary.
5. Verify startup, resume, clear/new, manual compact, automatic compact, normal exit, Ctrl+C interrupt, successful tool, failed tool, and permission wait in a real TUI pane.
6. Keep structured asks, subagent rows, live model, context, spend, quota, and rate windows capability-disabled on the stock interactive adapter.
7. Implement supervised runs with `droid exec --output-format json`; preserve native nonzero exit status.
8. Add `stream-jsonrpc` only behind exact envelope/protocol gates, typed request/notification parsers, request timeouts, serialized stdin writes, and bounded child cleanup.
9. Test JSON-RPC permission batches, AskUser, plan approval, interrupt, reconnect with pending asks, compact-to-new-session, fork, token usage, context stats, unknown notification types, malformed lines, and unexpected process death.
10. Re-research the latest release rather than adding old-version branches when any fixture changes.

## Known gaps that block full parity

The latest official stock-pane surface leaves these implementation gaps:

- no authoritative permission/question/plan request or resolution hook;
- no failed-turn or user-interrupt hook;
- no `PostCompact` hook and compaction may replace the session ID;
- no subagent start or child identity in hooks;
- no model, effort, token, context, cost, quota, or rate-limit hook fields;
- no published transcript record/durability schema;
- no machine-readable auth/account status command;
- no documented way to apply a one-launch interactive autonomy override without touching persistent settings.

Keep these gaps explicit in `Capabilities`. The JSON-RPC transport closes most lifecycle and ask gaps for headless sessions, but it is a separate agent surface and does not observe the stock interactive TUI running in a RimZ pane.

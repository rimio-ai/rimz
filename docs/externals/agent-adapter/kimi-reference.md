# Kimi Code protocol reference

This page mirrors the Kimi Code surfaces an adapter binds to: the CLI and its launch flags, the data root and session files, command hooks, the durable per-agent `wire.jsonl` records, approvals and questions, subagents and background tasks, OAuth login and managed usage, prompt mode, and resume and fork. It records what Kimi Code ships. How RimZ maps these surfaces onto its own types, and which of them it wires, is in [adapter_kimi.md](../../internals/agents/adapter_kimi.md); the provider-neutral lifecycle contract is [model.md](../../internals/agents/model.md), and accounts and spend are in [providers.md](../../internals/agents/providers.md).

**Baseline.** Kimi Code **0.42.0** (npm `@moonshot-ai/kimi-code` `latest`, tag `@moonshot-ai/kimi-code@0.42.0`, commit [`6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb`](https://github.com/MoonshotAI/kimi-code/tree/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb), published 2026-09-09), agent-record protocol **1.5**. Docs and source were read 2026-09-13, and the host binary (`kimi --version` prints `0.42.0`) matches the baseline. Source anchors are paths at that commit: `core/` abbreviates `packages/agent-core-v2/src/`, `cli/` abbreviates `apps/kimi-code/src/cli/`, and `oauth/` abbreviates `packages/oauth/src/`. Every CLI surface (the TUI, `kimi -p`, `kimi acp`, `kimi web`) runs on the `agent-core-v2` engine; the legacy engine and its `KIMI_CODE_LEGACY_FLAG` fallback are gone.

**Product identity.** The executable is `kimi`, the application root is `~/.kimi-code`, and the package is [`MoonshotAI/kimi-code`](https://github.com/MoonshotAI/kimi-code). The retired Python [`MoonshotAI/kimi-cli`](https://github.com/MoonshotAI/kimi-cli) also installs a `kimi` executable, uses `~/.kimi`, and speaks a different Wire protocol; it is outside this page. `kimi migrate` imports a legacy installation's config, MCP servers, REPL history, skills, and sessions, and does not copy OAuth credentials, MCP authorizations, or plugins ([migration guide](https://moonshotai.github.io/kimi-code/en/guides/migration.html)).

## Upstream sources

The docs site is unversioned and trails the source in places; each disagreement is flagged at its claim. The generated `packages/agent-core-v2/docs/wire-manifest.d.ts` is the version-exact catalog of durable record types, and `CHANGELOG.md` under `apps/kimi-code/` is the per-release change list.

| Surface | Source |
| --- | --- |
| Repository, releases, changelog | <https://github.com/MoonshotAI/kimi-code>, [`apps/kimi-code/CHANGELOG.md`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/apps/kimi-code/CHANGELOG.md), <https://moonshotai.github.io/kimi-code/en/release-notes/changelog.html> |
| CLI options and subcommands | <https://moonshotai.github.io/kimi-code/en/reference/kimi-command.html>, [`cli/commands.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/apps/kimi-code/src/cli/commands.ts), [`cli/options.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/apps/kimi-code/src/cli/options.ts) |
| Hooks | <https://moonshotai.github.io/kimi-code/en/customization/hooks.html>, [`core/features/externalHooks/`](https://github.com/MoonshotAI/kimi-code/tree/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/src/features/externalHooks) |
| Sessions and data locations | <https://moonshotai.github.io/kimi-code/en/guides/sessions.html>, <https://moonshotai.github.io/kimi-code/en/configuration/data-locations.html>, [`core/workspace/sessionLifecycle/sessionLifecycleService.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/src/workspace/sessionLifecycle/sessionLifecycleService.ts), [`core/session/sessionMetadata/`](https://github.com/MoonshotAI/kimi-code/tree/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/src/session/sessionMetadata) |
| Durable agent records | [`packages/agent-core-v2/docs/wire-manifest.d.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/docs/wire-manifest.d.ts), [`core/wire/`](https://github.com/MoonshotAI/kimi-code/tree/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/src/wire), [`core/persistence/backends/node-fs/appendLogStore.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/src/persistence/backends/node-fs/appendLogStore.ts) |
| Approvals, questions, permission modes | <https://moonshotai.github.io/kimi-code/en/guides/interaction.html>, <https://moonshotai.github.io/kimi-code/en/reference/tools.html>, [`core/agent/toolApproval/toolApprovalService.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/src/agent/toolApproval/toolApprovalService.ts), [`core/agent/tools/ask-user-question/`](https://github.com/MoonshotAI/kimi-code/tree/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/src/agent/tools/ask-user-question) |
| Configuration and environment | <https://moonshotai.github.io/kimi-code/en/configuration/config-files.html>, <https://moonshotai.github.io/kimi-code/en/configuration/env-vars.html> |
| Agents, subagents, background tasks | <https://moonshotai.github.io/kimi-code/en/customization/agents.html>, [`core/session/subagent/`](https://github.com/MoonshotAI/kimi-code/tree/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/src/session/subagent), [`core/agent/task/`](https://github.com/MoonshotAI/kimi-code/tree/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/agent-core-v2/src/agent/task) |
| OAuth, regions, managed usage | [`oauth/region.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/oauth/src/region.ts), [`oauth/storage.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/oauth/src/storage.ts), [`oauth/managed-kimi-code.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/oauth/src/managed-kimi-code.ts), [`oauth/managed-usage.ts`](https://github.com/MoonshotAI/kimi-code/blob/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/oauth/src/managed-usage.ts) |
| Local server | <https://moonshotai.github.io/kimi-code/en/reference/server-api.html>, [`packages/kap-server`](https://github.com/MoonshotAI/kimi-code/tree/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/kap-server) |
| ACP | <https://moonshotai.github.io/kimi-code/en/reference/kimi-acp.html>, [`packages/acp-server`](https://github.com/MoonshotAI/kimi-code/tree/6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb/packages/acp-server) |

## CLI

`kimi` with no arguments creates a new session and opens the terminal UI. The official installer places a standalone executable at `~/.kimi-code/bin/kimi`; the npm package is an alternate install path.

### Launch flags

| Flag | Meaning |
| --- | --- |
| `-V`, `--version` | print the version |
| `-S`, `--session [id]` | resume that session; bare `--session` opens the picker |
| `-r`, `--resume [id]` | hidden alias for `--session` |
| `-c`, `--continue` | resume the working directory's most recent session |
| `-m`, `--model <alias>` | model alias for this launch; defaults to `default_model` in `config.toml` |
| `-p`, `--prompt <text>` | run one prompt non-interactively (see [Prompt mode](#prompt-mode)) |
| `--output-format text\|stream-json` | prompt-mode output; `KIMI_MODEL_OUTPUT_FORMAT` sets the default and the flag wins |
| `-y`, `--yolo` | start in `yolo` permission mode (UI name "Ask When Needed"); hidden aliases `--yes`, `--auto-approve` |
| `--auto` | start in `auto` permission mode (UI name "Never Ask") |
| `--plan` | start in plan mode |
| `--agent <name>` | main-agent profile for the new session |
| `--agent-file <path>` | load one Markdown agent definition and select it for the new session |
| `--skills-dir <dir>` | load skills from this directory instead of the discovered user and project directories; repeatable |
| `--add-dir <dir>` | add a workspace directory; repeatable |

`validateOptions` in `cli/options.ts` rejects these combinations: `--continue` with `--session`; `--yolo` with `--auto`; `--prompt` with `--yolo`, `--auto`, or `--plan`; `--output-format` without `--prompt`; `--agent` or `--agent-file` with `--session` or `--continue` (the agent is bound at session creation and restored on resume); `--agent` with `--agent-file`; and a bare `--session` in prompt mode.

### Subcommands

| Command | Purpose |
| --- | --- |
| `kimi login [--region mainland-cn\|global]` | RFC 8628 device-code login; the docs page says it takes no flags, but the 0.42.0 binary accepts `--region` |
| `kimi fork [sessionId] [--cwd <path>] [-y]` | fork a session (default: the most recent for the directory) and print `Forked to <id>` |
| `kimi session list [--cwd] [--all] [--archived] [--limit n] [--json]` | list sessions, most recently updated first |
| `kimi export [sessionId] [-o path] [-y] [--no-include-global-log]` | package a session directory as a ZIP |
| `kimi web` | run the local server and open the web UI (see [Other hosts](#other-hosts-sdk-kimi-web-and-acp)) |
| `kimi rc`, `kimi remote` | `kimi web` through Kimi Remote Control |
| `kimi acp [--login [--region]]` | Agent Client Protocol over stdio |
| `kimi provider add\|remove\|list\|catalog` | manage LLM providers non-interactively |
| `kimi doctor` | validate `config.toml` and `tui.toml` |
| `kimi vis [sessionId]` | open the session visualizer in a browser |
| `kimi migrate [--run] [--config-only]` | import a legacy `kimi-cli` installation |
| `kimi upgrade`, `kimi update` | upgrade the installation |
| `kimi server` | deprecated; prints a notice and exits 1, except `kimi server kill`, which stops a server started before 0.28.0 |

`kimi fork` and `kimi session list` ship in the binary but are absent from the CLI reference page.

### Process identity

The runtime sets `process.title` to `kimi-code` (`PROCESS_NAME` in `apps/kimi-code/src/constant/app.ts`), so process listings can show either `kimi` or `kimi-code`. The argv of an interactive launch carries no generated session id. `/new` and `/sessions` switch sessions inside the same process, so a pane's session identity comes from hooks, not from argv.

## Data root and sessions

The data root is `$KIMI_CODE_HOME`, falling back to `~/.kimi-code`, and every path below moves with it. The user config file is `$KIMI_CODE_HOME/config.toml`; there is no `--config-file` flag or inline config override. The layout, from the [data locations](https://moonshotai.github.io/kimi-code/en/configuration/data-locations.html) page and the session lifecycle source:

```text
$KIMI_CODE_HOME/                          # default ~/.kimi-code
├── config.toml
├── tui.toml
├── region                                # install-channel region marker
├── session_index.jsonl
├── workspaces.json
├── credentials/
│   ├── <name>.json                       # managed provider OAuth tokens
│   └── mcp/
├── sessions/
│   └── wd_<slug>_<sha256-prefix>/
│       └── session_<uuid>/
│           ├── state.json
│           ├── upcoming-goals.json
│           ├── logs/kimi-code.log
│           └── agents/
│               ├── main/
│               │   ├── wire.jsonl
│               │   ├── plans/<id>.md
│               │   ├── tasks/
│               │   └── cron/
│               └── agent-0/
│                   └── wire.jsonl
├── plugins/
├── user-history/<md5(workDir)>.jsonl
├── logs/kimi-code.log
└── bin/
```

The session directory key is `wd_<slug>_<hash>`: `slug` is the lowercased basename of the working directory with runs outside `[a-z0-9._-]` replaced by `-`, capped at 40 characters, and `hash` is the first 12 hex characters of the SHA-256 of the normalized path (`encodeWorkDirKey` in `core/_base/utils/workdir-slug.ts`). A session id is `session_<uuid>`.

Background tasks and scheduled tasks persist per agent under `agents/<agent-id>/tasks/` and `agents/<agent-id>/cron/` (`AgentTaskPersistence` in `core/agent/task/persist.ts`, rooted by `core/agent/task/taskService.ts`); a session-level `tasks/` directory written by older releases is read as a fallback. The data locations page still places `tasks/` and `cron/` at the session root.

### `session_index.jsonl`

The index is an append-only JSONL file at the data root. A session create appends `{"sessionId","sessionDir","workDir"}` with an absolute `sessionDir`, and a delete appends a tombstone `{"sessionId","deleted":true}` (`appendSessionIndexEntry` and the delete path in `sessionLifecycleService.ts`). A later line for the same id supersedes an earlier one. The default session list reads a minidb-backed read model (`[database]` config, `KIMI_CODE_PERSISTENCE_MINIDB_READMODEL`), and `session_index.jsonl` remains the file-level index.

### `state.json`

`state.json` is the session metadata document, version `2` (`SESSION_META_VERSION` in `core/session/sessionMetadata/sessionMetadata.ts`):

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | session id |
| `version` | number | `2` |
| `cwd` | string | working directory |
| `title`, `titleKind`, `isCustomTitle` | string, `replaceable`\|`generated`\|`custom`, boolean | session title and its provenance |
| `lastPrompt` | string | most recent prompt text |
| `createdAt`, `updatedAt`, `archivedAt` | epoch ms | timestamps |
| `archived` | boolean | archived flag |
| `forkedFrom` | string | source session id of a fork |
| `lastTurnReason` | `completed`\|`cancelled`\|`failed` | how the last turn ended |
| `agents` | object | agent id to `{homedir, type, parentAgentId, forkedFrom, labels, swarmItem}`, where `type` is `main`, `sub`, or `independent` |
| `custom` | object | caller metadata |

Older metadata used `workDir` for the working directory and carried no `id`. `normalizeSessionMeta` in `core/session/sessionMetadata/sessionMetadataService.ts` reads `workDir` as `cwd` and fills `id` from the session directory; the service writes the normalized document back when it adds missing `agents` or `custom` maps or migrates the title fields, so a reader must accept either shape on disk.

## Command hooks

Command hooks are the lifecycle and approval channel of the stock terminal UI. A hook receives one JSON object on stdin, and its exit code and stdout decide the outcome for the three blockable events.

### Configuration

Hooks are `[[hooks]]` entries in `$KIMI_CODE_HOME/config.toml`:

```toml
[[hooks]]
event = "PermissionRequest"
matcher = ".*"
command = "notify-approval"
timeout = 10
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `event` | string | yes | one of the events in the [catalog](#event-catalog) |
| `matcher` | string | no | JavaScript regex tested against the event's matcher value; empty or omitted matches everything; an invalid regex never matches |
| `command` | string | yes | shell command |
| `timeout` | integer | no | seconds, 1 to 600, default 30 |

`HookDefSchema` in `core/features/externalHooks/configSection.ts` is strict: any other key fails config loading. An enabled plugin can also declare hooks in its manifest with the same four fields; a plugin hook runs with the plugin root as its working directory and receives `KIMI_CODE_HOME` and `KIMI_PLUGIN_ROOT` in its environment ([plugins](https://moonshotai.github.io/kimi-code/en/customization/plugins.html#hooks-in-plugins)). There is no project-level hook file and no hook trust prompt.

Every matching hook for an event runs in parallel. Hooks with the same working directory and `command` run once per trigger (`runMatchedHooks` in `internal/matchHooks.ts` keys on `(cwd, command)`; the docs say identical `command` values run once, which is the same rule for `config.toml` hooks, whose working directory is always the session directory). Commands run through the platform shell with the session's project directory as working directory. On non-Windows platforms a hook runs in its own process group; on timeout or abort it receives `SIGTERM` and then `SIGKILL` 100 ms later (`runHook` in `internal/runHook.ts`).

### Input payload

Every payload carries these fields; top-level keys are converted from camelCase to snake_case, and nested objects such as `tool_input` and `display` keep their own casing:

```json
{
  "hook_event_name": "PreToolUse",
  "session_id": "session_2f1c9a4e-8d0b-4c7a-9f3e-5b6d7e8f9a01",
  "session_title": "Fix the login page",
  "client_type": "kimi_code_cli",
  "cwd": "/path/to/project"
}
```

`session_title` is absent until the session has a title. The common payload carries no agent id, transcript path, permission mode, pid, or timestamp. Hooks registered on the agent scope (tool, permission, prompt, turn, stop, compaction, and task hooks) fire for subagents as well as the main agent, with the root `session_id`; only `PermissionRequest` and `PermissionResult` add an `agent_id` (`AgentExternalHooksService` is contributed per agent in `externalHooksFeature.ts`).

### Event catalog

"Awaited" means Kimi Code waits for the hook before continuing; the rest are fire-and-forget. Fields are in addition to the common payload.

| Event | Matcher value | Awaited | Blockable | Event fields |
| --- | --- | --- | --- | --- |
| `SessionStart` | `startup` or `resume` | yes | no | `source`, `model`, `profile` |
| `SessionEnd` | `exit` or `archive` | yes | no | `reason` |
| `SessionHeartbeat` | empty | no | no | `uptime_ms`; every 60 s, only while a `SessionHeartbeat` hook is configured |
| `UserPromptSubmit` | text parts joined with spaces | yes | yes | `prompt` (content-part array), `is_steer` |
| `UserPromptQueued` | queued prompt text | no | no | `prompt_id`, `prompt`, `queue_length` |
| `TurnStarted` | origin kind (`user`, `task`, `system_trigger`, ...) | no | no | `turn_id`, `origin_kind`, `origin_name`, `prompt` |
| `PreToolUse` | tool name | yes | yes | `tool_name`, `tool_input`, `tool_call_id` |
| `PostToolUse` | tool name | no | no | `tool_name`, `tool_input`, `tool_call_id`, `tool_output` (text parts, first 2,000 characters) |
| `PostToolUseFailure` | tool name | no | no | `tool_name`, `tool_input`, `tool_call_id`, `error` (Kimi error payload) |
| `PermissionRequest` | tool name | no | no | `id`, `agent_id`, `turn_id`, `tool_call_id`, `tool_name`, `action`, `display`, `tool_input` |
| `PermissionResult` | tool name | no | no | the request fields plus `decision`, optional `scope`, `feedback`, `selected_label`, `error` |
| `Stop` | empty | yes | yes | `stop_hook_active` (always `false`) |
| `StopFailure` | error name | no | no | `error_type`, `error_message` |
| `Interrupt` | empty | no | no | `turn_id`, `reason: "cancelled"` |
| `SubagentStart` | profile name | yes | no | `agent_name`, `prompt` |
| `SubagentStop` | profile name | no | no | `agent_name`, `response` |
| `TaskStarted` | task kind (`agent`, `process`, `question`) | no | no | `task_id`, `kind`, `description`, `status`, `detached`, `started_at` |
| `PreCompact` | `manual` or `auto` | yes | no | `trigger`, `token_count` |
| `PostCompact` | `manual` or `auto` | no | no | `trigger`, `estimated_token_count` |
| `Notification` | notification type, for example `task.completed` | no | no | `sink: "context"`, `agent_id`, `notification_type`, `title`, `body`, `severity` (`info`\|`warning`), `source_kind`, `source_id` |

Payload sources are `agent/agentExternalHooksService.ts` and `session/sessionExternalHooksService.ts` under `core/features/externalHooks/`. The events fire at these boundaries:

- `SessionStart` fires when a session is created (`startup`) or reopened (`resume`). A fork does not fire it at creation; the fork fires `resume` when it is opened.
- `UserPromptSubmit` fires only for prompts whose origin kind is `user`, including steers into a running turn (`is_steer: true`). `SubagentStart` and `SubagentStop` carry the full prompt text and the subagent's full result summary.
- `Stop` fires when a step finishes with a finish reason other than `tool_calls` or `filtered` and no requests are pending, so it runs before the turn closes.
- `StopFailure` fires when a turn ends with reason `failed` and an error. `Interrupt` fires when a turn ends with reason `cancelled` (the docs add that timeouts and programmatic aborts do not fire it). A turn blocked by `UserPromptSubmit` fires neither `Stop`, `StopFailure`, nor `Interrupt`.
- `PermissionRequest` and `PermissionResult` fire only when the permission policy resolves to ask; see [Approvals](#approvals).

### Output and exit semantics

| Hook result | Behaviour |
| --- | --- |
| exit `0`, empty stdout | allow |
| exit `0`, plain stdout | allow; only `UserPromptSubmit` uses it, appending the text to context |
| exit `0`, JSON with `hookSpecificOutput.permissionDecision: "deny"` | block; `permissionDecisionReason` is the reason |
| exit `2` | block; trimmed stderr is the reason |
| any other exit, timeout, spawn failure, abort | allow (fail open) |

Structured stdout is a JSON object; `message` or `hookSpecificOutput.message` supplies the text `UserPromptSubmit` appends, and the deny shape is:

```json
{
  "hookSpecificOutput": {
    "permissionDecision": "deny",
    "permissionDecisionReason": "Please use rg instead of grep"
  }
}
```

A block acts only on the three blockable events, and the first blocking result wins; a block without a reason becomes `Blocked by <event> hook`.

| Event | Effect of a block | Effect of allow text |
| --- | --- | --- |
| `UserPromptSubmit` | the turn skips the model call and ends with reason `blocked`; the reason is appended as an assistant message with origin `hook_result` | each allowing hook's text is appended as a user message wrapped in `<hook_result hook_event="UserPromptSubmit">` |
| `PreToolUse` | the tool does not run and returns the reason as its error result | ignored |
| `Stop` | the reason is appended as a user message (origin `system_trigger`, name `stop_hook`) and the loop continues; later `Stop` triggers in the same turn skip the hooks | ignored |

`PreCompact` is awaited but its result is ignored.

## Agent records

Each agent writes an ordered record log at `agents/<agent-id>/wire.jsonl`. The log restores agent state on resume and is the durable transcript, model, and token source for a stock terminal session. It is unrelated to the Python CLI's `{timestamp, message:{type, payload}}` Wire envelope.

### File format

The first line is a metadata record, and every later line is one record with its payload spread at the top level, an `agentId`, and an optional millisecond `time`:

```json
{"type":"metadata","protocol_version":"1.5","created_at":1788000000000}
{"type":"turn.prompt","agentId":"main","input":[{"type":"text","text":"fix the parser"}],"origin":{"kind":"user"},"promptId":"prompt_01","time":1788000000100}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"tool.call","uuid":"...","turnId":"1","step":1,"stepUuid":"...","toolCallId":"call_01","name":"Bash","args":{"command":"cargo check"}},"time":1788000000200}
```

`protocol_version` is `1.5` (`WIRE_PROTOCOL_VERSION` in `core/wire/migration/migration.ts`). On read, Kimi Code migrates older records forward through 1.0 to 1.5 and refuses a log whose version is newer than its own. The 1.4 to 1.5 migration adds `wallClockResumedAt` to active `goal.create` and `goal.update` records and changes nothing else.

The node-fs append log (`core/persistence/backends/node-fs/appendLogStore.ts`) batches pending records and appends them with a durable write. A rewrite, used by undo, fork truncation, and repair, replaces the file atomically. A reader treats an unparseable final line without a newline as a torn tail. An unparseable complete line truncates the log to its valid prefix: `repairWireJournal` in `core/wire/repair.ts` saves the original as `wire.jsonl.bak` (once) and rewrites `wire.jsonl`.

### Records an adapter reads

| Record | Key fields | Meaning |
| --- | --- | --- |
| `metadata` | `protocol_version`, `created_at` | format version |
| `turn.prompt`, `turn.steer` | `input` (content parts), `origin` (object with `kind`), `promptId` on prompts | turn input; `origin.kind: "user"` marks human input |
| `turn.ended` | `turnId`, `reason` (`completed`\|`cancelled`\|`failed`\|`blocked`), optional `error`, `durationMs`, `stopReason` | turn close |
| `turn.cancel` | optional `turnId`, `target` (`active`\|`queued`), `reason` (`user_cancelled`\|`aborted`) | cancellation request |
| `turn.step.retrying` | `turnId`, `step`, `failedAttempt`, `nextAttempt`, `maxAttempts`, `delayMs`, `errorName`, `errorMessage`, optional `statusCode` | provider retry wait |
| `turn.step.interrupted` | `turnId`, `step`, `reason`, optional `message` | interrupted step |
| `config.update` | `modelAlias`, `profileName`, `thinkingEffort`, `systemPrompt`, `environmentDisclosure.cwd`, `disallowedTools` | effective agent configuration |
| `profile.bind` | `modelAlias`, `profileName`, `thinkingEffort`, `systemPrompt`, `activeToolNames`, `subagents` | profile bound at agent creation |
| `llm.request` | `kind` (`loop`\|`compaction`), `provider`, `model`, `modelAlias`, `thinkingEffort`, sampling and output limits, `systemPromptHash`, `toolsHash`, `messageCount` | one provider request |
| `usage.record` | `model`, `usage`, optional `usageScope` (`session`\|`turn`) | token accounting for one request |
| `context.append_loop_event` | `event` | step, content, and tool events (below) |
| `context.append_message` | `message` (`role`, `content`, `toolCalls`, `origin`, ...) | model-facing context message |
| `context.clear` | none | context reset |
| `context.apply_compaction` | `summary` or `contextSummary`, `compactedCount`, optional `tokensBefore`, `tokensAfter` | compacted context |
| `context.undo` | `count` | undo of recent context |
| `full_compaction.begin`, `.cancel`, `.complete` | `source` (`manual`\|`auto`), optional `instruction` on begin | full-compaction bracket |
| `interaction.request` | `id`, `kind` (`approval`\|`question`\|`user_tool`), optional `toolCallId`, `request` | a prompt shown to the user |
| `interaction.resolved` | `id`, `response` | its answer or cancellation |
| `permission.set_mode` | `mode` (`manual`\|`yolo`\|`auto`) | permission mode change |
| `permission.record_approval_result` | `turnId`, `toolCallId`, `toolName`, `action`, optional `sessionApprovalRule`, `result` | answered approval |
| `plan_mode.enter`, `.exit`, `.cancel`, `plan.revision` | plan `id`; revision `version`, `key`, `sha256`, `bytes` | plan mode and plan files |
| `task.started`, `task.terminated` | `info` (task info), optional `outputTail` on terminated | background task lifecycle |
| `forked` | none | fork marker appended to each agent log of a forked session |

The wire manifest's payload sketches omit `context.apply_compaction`'s shared base fields; `contextCompactionBaseShape` in `core/agent/contextMemory/contextEvents.ts` defines `tokensBefore` and `tokensAfter`. The manifest also flattens `origin` to its `kind` values, while the file stores the origin object.

### Loop events

`context.append_loop_event.event` is one of `step.begin`, `step.end`, `content.part`, `tool.call`, or `tool.result`. Each carries `uuid`, `turnId` (a string), and `step`; `tool.call` and `tool.result` carry `toolCallId`, and `tool.call` carries `name` and `args`. `step.end` carries `finishReason` and, for a step that completed normally, `usage` plus request timing fields (`endMachineStep` and `finishMachineStepProjection` in `core/agent/loop/loopService.ts`); an interrupted or errored step ends with `finishReason` `interrupted` or `error` and no `usage`. Assistant text is the ordered text `content.part` events between step boundaries; thinking parts and tool plumbing are separate part types.

### Tokens and context

`TokenUsage` (`core/human/llm/usage.ts`) splits `inputOther`, `output`, `inputCacheRead`, and `inputCacheCreation`, with an optional provider `raw` object. Every `usage.record` accounts for one request; `usageScope: "turn"` marks a request inside a turn, and a missing or `session` scope marks work outside a turn, such as full compaction. `step.end.usage` repeats the step's split. The live `agent.status.updated` event (model, `contextTokens`, `maxContextTokens`, plan, swarm, and tower modes) reaches SDK, server, and ACP clients and is not written to `wire.jsonl`. `[token_counting] strategy` (`measured+estimated` default, `measured`, `estimated`) chooses how the reported context size is derived; its `token_counting.*` records are listed below.

Kimi Code records tokens and no per-request price.

### Other record types

The manifest indexes 60 durable types. The rest, one line each: `cron.add`, `cron.cursor`, `cron.delete` (scheduled prompts); `file_history.checkpoint`, `file_history.tracked` (turn file history); `goal.create`, `goal.update`, `goal.clear` (goal mode); `interruptionReminder.recorded`; `llm.tools_snapshot` (content-addressed tool schemas); `mcp.tools_discovered`; `plugin.session_start`; `prompt.accepted`, `prompt.steered`, `prompt.completed`, `prompt.aborted` (prompt queue resolution); `runtime.set_binding`; `swarm_mode.enter`, `swarm_mode.exit`; `task.waitDelivered`; `token_counting.measured`, `.rebased`, `.truncated`, `.turn_recorded`; `tools.register_user_tool`, `tools.unregister_user_tool`, `tools.set_active_tools`, `tools.reset_active_tools`, `tools.update_store`; `tower_mode.enter`, `tower_mode.exit`.

## Approvals, questions, and plan mode

The terminal UI renders every prompt natively. Hooks observe approvals; questions have no hook of their own; and the durable `interaction.*` records cover both.

### Permission modes

| Mode | UI name | Behaviour |
| --- | --- | --- |
| `manual` | Always Ask (default) | read-only tools run; editing, shell, and other actions ask |
| `yolo` | Ask When Needed | routine tool calls run; sensitive files (`.env`, SSH keys), dangerous shell commands, and plan exit still ask; the agent can ask questions |
| `auto` | Never Ask | every approval is decided automatically, including plan exit; the agent never asks questions |

Source: [interaction guide](https://moonshotai.github.io/kimi-code/en/guides/interaction.html#the-three-permission-modes). The dangerous-command guard (`[permission] dangerous_command_guard`, `KIMI_CODE_DANGEROUS_COMMAND_GUARD`, on by default) asks before commands such as `shutdown`, `reboot`, or `rm -rf` in `manual` and `yolo` and is inactive in `auto` (`core/agent/permissionPolicy/policies/dangerous-command-ask.ts`). `default_permission_mode` and `default_plan_mode` in `config.toml` set the launch defaults.

### Approvals

An approval opens when the permission policy resolves a tool call to ask (`requestToolApproval` in `core/agent/toolApproval/toolApprovalService.ts`). Policy-approved, mode-approved, and statically denied calls open nothing and fire no hook. The sequence is:

1. Kimi Code builds the request `{id: "approval_<uuid>", sessionId, agentId, turnId, toolCallId, toolName, action, display}`, where `action` is the tool's description (default `Approve <tool>`) and `display` is a typed rendering of the input (`kind: "generic"` with `summary` and `detail` when the tool supplies none).
2. It dispatches `PermissionApprovalRequested` (the `PermissionRequest` hook, with `tool_input` added) and writes `interaction.request` with `kind: "approval"`.
3. On the answer it writes `interaction.resolved`, dispatches `PermissionApprovalResolved` (the `PermissionResult` hook) with `decision` `approved`, `rejected`, or `cancelled`, optional `scope: "session"` (approve for the session), `feedback`, and `selected_label`, and writes `permission.record_approval_result`.
4. If the request throws, `PermissionResult` fires with `decision: "error"` and `error`.

A user cancellation of the turn throws before step 3, so a cancelled turn can leave a `PermissionRequest` without a `PermissionResult`; the turn's `Interrupt` hook and `turn.ended` record close it. Open interactions for a turn are resolved as cancelled when the turn ends (`cancelInteractionsForTurn` in `core/agent/interaction/interactionWiring.ts`).

### Questions

`AskUserQuestion` is a tool (`core/agent/tools/ask-user-question/ask-user-question.ts`). Its input:

| Field | Type | Constraint |
| --- | --- | --- |
| `questions` | array | 1 to 4 items; question texts unique |
| `questions[].question` | string | required |
| `questions[].header` | string | default `""` |
| `questions[].options` | array of `{label, description}` | 2 to 4; labels unique within the question; the UI adds an "Other" choice |
| `questions[].multi_select` | boolean | default `false` |
| `background` | boolean | default `false` |

A foreground question shows through the interaction runtime and writes `interaction.request` with `kind: "question"`; its hook footprint is `PreToolUse` and then `PostToolUse` or `PostToolUseFailure` for `AskUserQuestion`. `background: true` starts a background task of kind `question` (`TaskStarted` fires), returns a task id at once, and delivers the answer to the agent in a later turn. In `auto` mode the agent does not ask, and a dismissed question counts as no answer.

### Plan mode

Plan mode writes plan files at `agents/<agent-id>/plans/<id>.md` (`planService.ts`) and records `plan_mode.*` and `plan.revision`. `ExitPlanMode` goes through the approval runtime, so it fires `PermissionRequest` with `tool_name: "ExitPlanMode"` in `manual` and `yolo` modes; in `auto` mode the exit is approved automatically and marked auto-approved in the tool result and transcript.

## Subagents and background tasks

The built-in subagent profiles are `coder` (the default), `explore` (read-only), and `plan` (no shell); none of them can dispatch further subagents ([agents](https://moonshotai.github.io/kimi-code/en/customization/agents.html)). A spawned child gets an id such as `agent-0`, a `state.json.agents` entry with `type: "sub"` and its `parentAgentId`, and its own `agents/<agent-id>/wire.jsonl`. A `[secondary_model]` pool can bind subagents to a different model, so a child's model comes from its own `config.update` and `llm.request` records. Subagent runs time out after `[subagent] timeout_ms` (`KIMI_SUBAGENT_TIMEOUT_MS`, default 2 hours; no timeout in prompt mode); `AgentSwarm` children use `[swarm] timeout_ms`.

The hooks name the child's profile and carry no child id, parent tool-call id, or background flag. The live event bus carries the exact identity: `subagent.spawned` (with `callerAgentId`, `description`, `swarmIndex`, `runInBackground`, `model`, `thinkingEffort`, `taskId`), `subagent.started`, `subagent.suspended`, `subagent.completed` (`resultSummary`, `usage`, `contextTokens`), and `subagent.failed` (`error`), keyed by `subagentId` (`core/session/subagent/mirrorAgentRun.ts`). These events reach SDK, server, and ACP clients and are not written to `wire.jsonl`.

Background work (shell commands, background subagents, background questions) is tracked as tasks. A task has a kind (`agent`, `process`, or `question`) and a status of `running`, `completed`, `failed`, `timed_out`, `killed`, or `lost` (`core/agent/task/types.ts`). Task records persist as `agents/<agent-id>/tasks/<task_id>.json` with output at `tasks/<task_id>/output.log`, and the owning agent's log records `task.started` and `task.terminated`. `TaskStarted` fires on start, and a terminal status delivered into context fires `Notification` with a `task.*` type. A foreground shell command that reaches its timeout moves to the background (`[background] bash_auto_background_on_timeout`, default on). A resumed session warns the model that tasks from the previous process may still be running.

## Authentication and managed usage

`kimi login`, `/login`, and `kimi acp --login` run the RFC 8628 device-code flow for the managed provider `managed:kimi-code`. Kimi Code has two regions, each a bundle of endpoints (`KIMI_REGION_PROFILES` in `oauth/region.ts`):

| Region | OAuth host | Managed API base |
| --- | --- | --- |
| `mainland-cn` (kimi.com, default) | `https://auth.kimi.com` | `https://api.kimi.com/coding/v1` |
| `global` (kimi.ai) | `https://auth.kimi.ai` | `https://api.kimi.ai/coding/v1` |

`resolveKimiRegion` picks the first match: `KIMI_CODE_OAUTH_HOST` or `KIMI_OAUTH_HOST`; the `oauthHost` persisted in the managed provider's OAuth reference in `config.toml`; a default-slot reference (mainland); the `$KIMI_CODE_HOME/region` marker written by the installer (`mainland-cn` or `global`, read only before the first login); then `mainland-cn`. `KIMI_CODE_BASE_URL` overrides the managed API base.

### Credentials

Login writes the provider entry `[providers."managed:kimi-code"]` (`type = "kimi"`, the base URL, and an `oauth` reference with `storage` and `key`) and stores the token in `credentials/` (`applyManagedKimiCodeConfig` and `resolveKimiCodeOAuthKey` in `oauth/managed-kimi-code.ts`):

- The default slot, used when the OAuth host is `https://auth.kimi.com` and the base is `https://api.kimi.com/coding/v1`, has key `oauth/kimi-code` and file `credentials/kimi-code.json`.
- Any other host and base pair, including a `global` login, has key `oauth/kimi-code-env-<16 hex>`, where the hex is the SHA-256 prefix of `{"oauthHost","baseUrl"}`, and file `credentials/kimi-code-env-<16 hex>.json`.

`FileTokenStorage` (`oauth/storage.ts`) creates the directory with mode 0700 and each file with mode 0600, writing to a temp file, fsyncing, and renaming. The file holds `access_token`, `refresh_token`, `expires_at` (epoch seconds), `scope`, `token_type`, and `expires_in`. Kimi Code refreshes the access token itself.

### Usage endpoint

`/usage` and the server's `GET /api/v1/oauth/usage` call the managed base's `/usages` with the stored access token (`fetchManagedUsage` in `oauth/managed-usage.ts`, 8 s timeout):

```text
GET https://api.kimi.com/coding/v1/usages        # api.kimi.ai for a global login
Authorization: Bearer <access token>
Accept: application/json
```

HTTP 401 is an authorization failure and 404 means the endpoint is unavailable for the account. The payload, with numbers as decimal strings:

```json
{
  "usage": {"used": "40", "limit": "1000", "resetTime": "2026-08-03T05:20:51Z"},
  "limits": [
    {"window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
     "detail": {"used": "1", "limit": "100", "resetTime": "2026-08-01T10:00:00Z"}}
  ],
  "boosterWallet": {
    "balance": {"type": "BOOSTER", "amount": "500000000", "amountLeft": "125000000"},
    "monthlyChargeLimitEnabled": true,
    "monthlyChargeLimit": {"priceInCents": "500", "currency": "USD"},
    "monthlyUsed": {"priceInCents": "125", "currency": "USD"}
  }
}
```

| Field | Meaning |
| --- | --- |
| `usage` | the plan's weekly bucket; the payload carries no window for it |
| `limits[].window` | `duration` plus `timeUnit` of `TIME_UNIT_MINUTE`, `TIME_UNIT_HOUR`, `TIME_UNIT_DAY`, or `TIME_UNIT_WEEK`; the five-hour limit arrives as 300 minutes |
| `limits[].detail`, `usage` | `used`, `limit`, `resetTime` (ISO 8601), optional `name` |
| `boosterWallet.balance` | `type: "BOOSTER"`; `amount` and `amountLeft` are fixed point at 1,000,000 units per cent |
| `boosterWallet.monthlyChargeLimit`, `.monthlyUsed` | `priceInCents` and ISO `currency`; a cap of 0 means unlimited |

`parseManagedUsagePayload` ignores a limit row whose `timeUnit` is outside those four and falls back to USD when no currency is present.

## Prompt mode

`kimi -p "<prompt>"` runs one prompt without the terminal UI:

```sh
kimi -p "Run the focused checks" --output-format stream-json
```

Prompt mode applies the `auto` permission policy, so it opens no approvals or questions; static deny rules still apply. In `text` format, assistant text goes to stdout, and thinking, tool progress, and the resume notice go to stderr. In `stream-json` format, stdout carries one JSON object per line (`cli/prompt-render.ts`):

| Line | Shape |
| --- | --- |
| version (first line) | `{"role":"meta","type":"system.version","version":"0.42.0"}`; `text` format writes `kimi version <v>` to stderr instead |
| assistant | `{"role":"assistant","content":"...","tool_calls":[{"type":"function","id","function":{"name","arguments"}}]}`; hook result text is also written as an assistant line |
| tool result | `{"role":"tool","tool_call_id":"...","content":"..."}` |
| retry | `{"role":"meta","type":"turn.step.retrying","failed_attempt","next_attempt","max_attempts","delay_ms","error_name","error_message","status_code"}` |
| resume hint | `{"role":"meta","type":"session.resume_hint","session_id","command":"kimi -r <id>","content"}` |

Thinking is not written to the JSON stream, and tool progress stays on stderr. When the main turn ends with background tasks or subagents pending, `[background] print_background_mode` decides what happens (env `KIMI_CODE_BACKGROUND_PRINT_BACKGROUND_MODE`, [config files](https://moonshotai.github.io/kimi-code/en/configuration/config-files.html#background)): `steer` (default) feeds each completion back as a new turn until none are pending, `drain` waits for them without feeding results back, and `exit` exits at once.

Exit codes: `0` on success. A failed, cancelled, or hook-blocked turn, a startup, auth, or provider error, or an option conflict exits `1` after writing the error to stderr (`main.ts`). A goal created with `kimi -p "/goal ..."` exits `0` when complete, `3` when blocked, and `6` when paused (`GOAL_EXIT_CODES` in `cli/goal-prompt.ts`).

## Resume and fork

`kimi --continue` resumes the working directory's most recent session, `kimi --session <id>` (or `-r <id>`) resumes an exact id, and a bare `--session` opens the picker. Inside the terminal UI, `/new` (alias `/clear`) creates and switches to a new session, `/sessions` (alias `/resume`) switches to an existing one, `/compact` compacts the current context, and `/undo` rewinds recent turns.

A fork copies a session into a new id and leaves the current session active: `/fork` in the terminal UI prints a `kimi --resume <id>` command, and `kimi fork [sessionId]` does the same from a shell and prints `Forked to <id>`. The copy excludes `state.json`, `logs/`, and `upcoming-goals.json`, writes a new `state.json` with `forkedFrom`, and appends a `forked` record to each agent log. Forking a session whose turn is running fails with an error. ACP clients fork through `session/fork`.

## Other hosts: SDK, `kimi web`, and ACP

These surfaces host a session inside Kimi Code's engine for a client; none of them observes a separately running terminal UI.

- The Node SDK (`packages/node-sdk`, `@moonshot-ai/kimi-code-sdk`) creates, resumes, and forks sessions and exposes the live event bus (including `agent.status.updated` and `subagent.*`), approval and question handlers, and context and status APIs. There is no published SDK reference page.
- `kimi web` runs the local REST and WebSocket server (`packages/kap-server`) and opens the web UI. It binds `127.0.0.1:58627` by default, taking the next free port when busy, and requires the bearer token printed at startup (`kimi web rotate-token` replaces it). `--host` binds all interfaces or a named host; `--dangerous-bypass-auth` turns off bearer authentication on every route. The session list filters on `activity.status` values `running`, `approval`, `question`, `failed`, and `idle`; the `event.session.status_changed` event keeps the legacy values `idle`, `running`, `awaiting_approval`, `awaiting_question`, and `aborted` ([server API](https://moonshotai.github.io/kimi-code/en/reference/server-api.html)).
- `kimi acp` speaks Agent Client Protocol over stdio, with `loadSession`, `session/list`, `session/resume`, `session/close`, `session/delete`, and `session/fork`, and maps approvals, questions, modes, and tool events into ACP ([kimi acp](https://moonshotai.github.io/kimi-code/en/reference/kimi-acp.html)).

## Upstream scope

Kimi Code publishes no hook for questions, no agent id on most hooks, and no durable `agent.status.updated` snapshot; a stock terminal session exposes identity for subagents only through `state.json.agents` and the child logs. Goal mode, tower mode, swarm mode, cron, and Remote Control are shipped surfaces this page indexes without depth. Which surfaces RimZ leaves unwired, and why, is in the Kimi adapter's [Known gaps](../../internals/agents/adapter_kimi.md#known-gaps).

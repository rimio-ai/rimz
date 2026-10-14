# Kiro CLI protocol reference

> This page records the upstream Kiro CLI surface. RimZ's mapping of it, and the gaps RimZ records, are in [adapter_kiro.md](../../internals/agents/adapter_kiro.md). The provider-neutral lifecycle contract is [model.md](../../internals/agents/model.md).

Kiro CLI is AWS's terminal coding agent. It ships three agent engines, and this page covers the v3 engine (Kiro's "CLI 3.0", available before the default-engine rollout) that `kiro-cli chat --v3` selects: the executables and engine selection, launch and resume, the local session store, hooks, permissions, headless and ACP modes, usage, and configuration. It makes no claim about what RimZ supports.

**Refresh baseline.** Kiro CLI 2.30.0, released 2026-10-12, checked 2026-10-12. The [stable manifest](https://prod.download.cli.kiro.dev/stable/latest/manifest.json) selects this version, and the [release feed](https://prod.download.cli.kiro.dev/stable/2.30.0/feed.json) dates it. Kiro CLI is closed source under the AWS Intellectual Property License and publishes no public release tags or commits ([Upgrading from Q CLI](https://kiro.dev/docs/upgrade-guides/migrating-from-q/)). The artifact's `BUILD-INFO` records build hash `a1561bc58200f7614b7cfb9761868e19adf812c4` and build time `2026-10-12T20:15:21.258740+00:00`.

Wire claims use the [Linux x86_64 release archive](https://prod.download.cli.kiro.dev/stable/2.30.0/kirocli-x86_64-linux.tar.xz), verified against manifest SHA-256 `11ee5ab2a8ea4faf5e64572e06e5fa662debefcb8ebd17b00313444574e3a062`. Its `kiro-cli-chat` embeds package `@kiro/agent` 0.66.33, including `dist/server/acp-server.js` ("engine bundle"), and `tui.js` ("TUI bundle"). Bundle citations below refer to this archive and name the relevant schema, method, or string. When docs disagree, this page follows the bundle. Only help and version probes run during this refresh.

**Inline pins.** Record order inside real turns comes from stock sessions captured on Kiro CLI 2.12.1 and 2.21.4 and was not re-captured, because running a session is outside a refresh pass. Each such claim retains its capture version. The redacted captures are RimZ's fixtures under `crates/rimz/src/agents/adapters/kiro/tests/fixtures/`.

**Targeted 2.31.0 check.** The home resolution, `--trust-all-tools`, positional prompt parsing, `SessionEnd`, and `user_input` facts were also checked statically against the [2.31.0 release bundle](https://prod.download.cli.kiro.dev/stable/2.31.0/kirocli-x86_64-linux.tar.xz). The rest of this page retains its 2.30.0 baseline and was not re-verified.

## Upstream sources

The docs are rolling and unversioned. Several pages cover the Kiro IDE and CLI together.

| Surface | Official source |
| --- | --- |
| CLI 3.0 overview and new features | <https://kiro.dev/docs/cli/v3/> · <https://kiro.dev/docs/cli/v3/new-features/> |
| Changelog and release feed | <https://kiro.dev/changelog/cli/> · `https://prod.download.cli.kiro.dev/stable/<version>/feed.json` |
| CLI commands and slash commands | <https://kiro.dev/docs/reference/cli-commands/> · <https://kiro.dev/docs/reference/slash-commands/> |
| Hooks | <https://kiro.dev/docs/hooks/> · <https://kiro.dev/docs/hooks/types/> · <https://kiro.dev/docs/hooks/actions/> · <https://kiro.dev/docs/cli/v3/hooks-migration/> |
| Permissions | <https://kiro.dev/docs/permissions/> · <https://kiro.dev/docs/cli/v3/permissions/> |
| Memory | <https://kiro.dev/docs/memory/> |
| Sessions and context | <https://kiro.dev/docs/cli/chat/session-management/> · <https://kiro.dev/docs/cli/chat/context/> |
| Headless mode and ACP | <https://kiro.dev/docs/cli/headless/> · <https://kiro.dev/docs/cli/acp/> |
| Models and credits | <https://kiro.dev/docs/models/> · <https://kiro.dev/docs/billing/related-questions/> |
| Configuration and settings | <https://kiro.dev/docs/configuration/> · <https://kiro.dev/docs/reference/settings/> |

These installed-binary commands are read-only. `kiro-cli-chat` is the binary `kiro-cli chat` runs, and a parse error exits before any session starts.

```sh
kiro-cli --version
kiro-cli --help-all
kiro-cli chat --help
kiro-cli acp --help
kiro-cli-chat --help
kiro-cli-chat chat --help
kiro-cli-chat serve --help
```

## Executables and engines

A Kiro CLI install puts three executables on `PATH` (`~/.local/bin/` on Linux) and extracts helper runtimes on first use.

| Executable or runtime | Role |
| --- | --- |
| `kiro-cli` | Launcher and desktop-integration commands. `kiro-cli chat` hands off to `kiro-cli-chat`, forwarding its arguments unchanged. |
| `kiro-cli-chat` | Chat CLI: argument parsing, the v1 and v2 engines, headless runs, and the launcher for the v3 terminal UI and engine. |
| `kiro-cli-term` | Shell-integration terminal process. It runs for integrated shells, not only for chat. |
| `bun` running `tui.js` | The terminal UI, extracted under `~/.local/share/kiro-cli/`. |
| `node` running `acp-server.js` | The v3 engine (Kiro calls it KAS), extracted under `~/.local/share/kiro-cli/kas/<version>-<sha256>/`. `KIRO_KAS_NODE_PATH` and `KIRO_KAS_SERVER_PATH` override the runtime and server path (TUI bundle). |

The engine is chosen per launch. `--agent-engine <v1|v2|v3>` selects it and defaults to `v2` (`kiro-cli chat --help`). `--v3` and `--v2` are shorthands. A saved `chat.agentEngine` setting changes the default, and `--v2` overrides that setting for one session without rewriting it ([changelog 2.21.4](https://kiro.dev/changelog/cli/)). Version 2.30.0 gradually offers a startup prompt to switch to v3 and upgrade agent configurations ([changelog](https://kiro.dev/changelog/cli/)). Accepting saves v3 as the default. UI and launch variants appear in the flag table below.

## Launch and resume

`kiro-cli chat` takes one optional positional argument, `[INPUT]`, "The first question to ask" (`kiro-cli chat --help`). The docs pass it without a separator: `kiro-cli chat --effort high "Refactor this module for testability"` ([CLI commands](https://kiro.dev/docs/reference/cli-commands/)).

The v3 TUI drops a prompt after `--` in 2.30.0. Its parser treats unknown options as flags with an optional value, so `--` consumes the following prompt (TUI bundle, argument parser). Pass the prompt directly as the positional argument. This parser also treats a prompt beginning with `-` as an option. A live 2.21.4 capture confirmed this behavior. No live turn was run for 2.30.0.

| Flag | Meaning (`kiro-cli-chat chat --help`, 2.30.0) |
| --- | --- |
| `--tui` · `--legacy-ui` (alias `--classic`) | Select the terminal UI or legacy harness. Engine selection uses `--agent-engine`, `--v3`, or `--v2`. |
| `--mode <default\|spec>` | Select the v3 mode. |
| `--cloud` · `--repo <REPO>` | Start a cloud session, optionally with comma-separated repository names. `--repo` applies only with `--cloud`. |
| `[INPUT]` | First prompt. With `--no-interactive`, stdin is read as the prompt when no positional argument is given. |
| `-r`, `--resume` | Resume the newest conversation for this directory and the current local or cloud placement. |
| `--resume-id <SESSION_ID>` | Resume a specific session. v3 session IDs are `sess_<uuid>`. |
| `--resume-picker` (alias `--list`) | Pick a conversation from this directory interactively. |
| `--sessions` | Open the v3 session dashboard. Closing it exits. |
| `--agent <AGENT>` | Agent profile to use. |
| `--model <MODEL>` | Model for the session. |
| `--effort <EFFORT>` | Initial effort: `low`, `medium`, `high`, `xhigh`, `max`. |
| `-a`, `--trust-all-tools` · `--trust-tools <NAMES>` | Session-wide trust, or exact tool IDs for non-interactive runs. See [Permissions](#permissions). |
| `--no-interactive` · `--output-format <text\|stream-json>` | Headless run (see [Headless and ACP](#headless-and-acp)). |
| `-l`, `--list-sessions` · `--all-cwds` · `-f`, `--format <plain\|json\|json-pretty>` | List saved sessions for this directory, or every directory. |
| `-d`, `--delete-session <SESSION_ID>` · `--session-source <v1\|v2>` | Delete a saved session. |
| `--list-models` | List models and exit. |
| `--require-mcp-startup` | Exit with code 3 when an enabled MCP server fails to start. |
| `-w`, `--wrap <always\|never\|auto>` | Line wrapping. |

The TUI parser recognizes `--trust-tools`, but interactive v3 sessions do not apply that trust list. See [Permissions](#permissions). `--list` opens the picker. `-l` lists sessions and exits.

An unresolved `--resume-id` reports an error instead of starting a replacement conversation. With cloud sessions enabled, a full ID also checks remote storage ([Session management](https://kiro.dev/docs/cli/chat/session-management/)).

Interactive commands that touch session state are slash commands inside the TUI ([Slash commands](https://kiro.dev/docs/reference/slash-commands/)).

| Command | Effect |
| --- | --- |
| `/compact` | Summarize the conversation to free context. The engine records compaction as a `tombstone` record with `kind: "summarization"` (engine bundle schema). |
| `/rewind` | Fork the conversation at an earlier turn. The engine records a revert as a `tombstone` with `kind: "checkpoint_revert"`. |
| `/model` · `/effort` | Change model or effort for the session. |
| `/context` | Show context-window use, with a per-tool token breakdown since 2.16.0. |
| `/usage` | Show plan, limits, and credits. |
| `/sessions` | Browse, search, resume, and clean up v3 sessions (2.21.0). |
| `/tools trust-all` | Enable session-wide tool approval, with a confirmation by default (2.24.0). |
| `/workflow` | Manage opt-in multi-step workflows (2.26.0). |
| `/memories` | Configure memory access and list memories for local v3 sessions (2.29.0). |
| `/tangent` | Create a conversation checkpoint to explore a side topic (2.16.0). The engine schema has `createdReason: "tangent"` for such sessions. |

## Session store

A new local TUI session sends the launch working directory as ACP `cwd` (TUI bundle, `newSession`). The v3 engine writes one directory per session, bucketed by workspace:

```text
~/.kiro/sessions/
  <workspace bucket: 16 lowercase hex characters>/
    sess_<uuid>/
      session.json
      messages.jsonl
      tool-outputs/<tool>-<8 hex>.txt      large tool output, saved whole (2.21.1)
      sub-executions/<sub-execution>.jsonl subagent records
~/.kiro/session-index/                     engine-maintained lookup index
```

The bucket is the first 16 hex characters of SHA-256 over the session's workspace paths. The engine normalizes each path (absolute, forward slashes, no trailing slash, lowercased on Windows), sorts them, and joins them with a NUL byte. A session with no workspace path uses the bucket `_global` (engine bundle, the `createHash("sha256")` call that feeds `computeWorkspaceHash`). For one workspace, the input is its normalized absolute path. To find an existing session, the engine tries the caller's bucket, then the session index, then every bucket directory. Local Kiro state uses owner-only access since 2.27.0 ([changelog](https://kiro.dev/changelog/cli/)).

In 2.31.0, the engine bundle's `P8o` resolves `userConfigDir` from `KIRO_HOME`, rejecting a relative path, a filesystem root, or a path equal to or containing `HOME`, and falling back to `$HOME/.kiro`. It derives `sessionsPath`, `homeGlobalHooksDir`, and the skills base from that directory. This agrees with the [settings reference](https://kiro.dev/docs/reference/settings/), which says `KIRO_HOME` overrides the global configuration and sessions directory. In 2.30.0 the engine instead used `$HOME/.kiro` irrespective of that override; the TUI used the override independently. The session-management page describes a "Local database (~/.kiro/)"; the v3 wire uses the file layout above ([Session management](https://kiro.dev/docs/cli/chat/session-management/)).

The engine creates `session.json` and an empty `messages.jsonl` before the first prompt (captured on 2.12.1). It validates `session.json` against this schema on load (engine bundle, the object with `schemaVersion`, `dataModelVersion`, and `workspacePaths`):

| Field | Type | Notes |
| --- | --- | --- |
| `schemaVersion` | string | `"1.0.0"`, the engine's write version. |
| `dataModelVersion` | non-negative integer, optional | `1`. Absent on legacy sessions. |
| `id` | string | `sess_<uuid>`, equal to the directory name. |
| `title` | string | Required. |
| `agentMode` | string | For example `vibe`. |
| `workspacePaths` | string array | Absolute workspace paths. The bucket input. |
| `rootPaths` | string array, optional | |
| `createdAt` · `lastModifiedAt` | string | ISO 8601 timestamps. |
| `status` | enum, optional | `in_progress`, `waiting_on_user`, `completed`, `idle`, `failed`. Absent in a newborn session. |
| `parentSessionId` · `parentExecutionId` | string, optional | Set on child sessions. |
| `createdReason` | enum, optional | `human`, `rewind`, `subagent`, `tangent`. |
| `modelId` · `effortLevel` · `autopilot` | optional | Persisted model, effort, and autonomy. |
| `executionTarget` | optional | `{ "kind": "local" }` or `{ "kind": "cloud-sandbox" }`. |
| `thinkingEnabled`, `contextSnapshotModelDisplay`, `workflowsEnabled`, `backgroundExecution` | optional | Model display and execution settings. |
| `memoryConfig` · `memoryConfigSource` | optional | Memory mode and reflection settings. Source: `legacy` or `explicit`. |
| `_meta.kiro.workflow` · `_meta.kiro.serverSession` | optional | Workflow ownership and server session metadata. |
| `lastCheckpointId`, `repositories`, `description`, `contextFiles`, and spec, review, and compaction switches | optional | |

The engine sets `status` from turn activity: `in_progress` when a turn starts, then `waiting_on_user`, `failed`, or `idle` when it ends (engine bundle, `persistActivityStatus` and `terminalActivityStatus`).

### Message records

The main transcript is `messages.jsonl`, one JSON object per line: `{"id": string, "timestamp": string, "payload": {"type": ..., ...}}`. The engine validates payloads against a discriminated union on `type` (engine bundle). The table lists every member, with full fields for the ones that carry turn state.

| `type` | Fields |
| --- | --- |
| `user` | `content`, `contextItems?`, `source?` (`chat`, `hook`, `api`, `steer`), `images?`, `documents?`, `kind?`, `_meta?` |
| `assistant` | `content`, `operationType?` (`Say`, `Reasoning`, `Print`, `Summary`), `executionId?`, `subExecutionId?`, reasoning signature fields, `_meta?` |
| `tool_call` | `toolCallId`, `toolName`, `args`, `status` (`pending`, `awaiting_approval`, `approved`, `denied`, `executing`, `completed`, `failed`), `title?`, `kind?` (`read`, `edit`, `execute`, `search`, `delete`, `move`, `fetch`, `think`, `switch_mode`, `other`), `actionType?`, `filePath?`, `executionId?`, `subExecutionId?`, snapshot IDs, `modelToolUseId?`, `_meta?` |
| `tool_result` | `toolCallId`, `content`, `success` (boolean), `durationMs?`, `metadata?`, `executionId?`, `subExecutionId?`, `images?` (`{data, mimeType}`), `_meta?` |
| `pending_interaction` | `interactionType` (`tool_approval`, `user_input`), `toolCallId`, `question`, `options` (approval options `{optionId, name, kind}` or question options `{title, description?, recommended?, subOptionsLabel?, subOptions?}`), `executionId`, `consentRound?`, `_meta?` |
| `interaction_resolved` | `toolCallId`, `outcome` (for example `selected`), `selectedOption?` (for example `accept`), `executionId`, `consentRound?` |
| `turn_start` | `executionId?` |
| `turn_end` | `stopReason` (non-empty string), `stopDetails?` (`{refusal?: {category?, explanation?, recommendedModel?}}`), `executionId?` |
| `session_event` | `category` (`session_start`, `session_restore`, `session_pause`, `session_resume`), `context?` (object) |
| `usage_summary` | `promptTurnSummaries` (array of `{usedTools?, unit?, unitPlural?, usage?}`), `elapsedTime?` (ms), `status?` (`success`, `failed`, `aborted`), `executionId?`, `requestIds?`, `throughput?`, `recoveries?`, `contextBreakdown?` |
| `session_metadata` | `key`, `value`, `executionId`. Keys written: `contextUsage` with `{usagePercentage: number}`, `recap` with `{text}`, and `modelFallback` (see [Usage, credits, and context](#usage-credits-and-context)). |
| `session_start` | `agentType`, `content` (bootstrap context, not conversation), `forcedRole`, `messageId`, `images?`, `documents?`, `steeringDocuments?`, `backgroundExecution?`, `composition?` |
| `steering_inclusion` | `documents` (IDs or `{id, displayName, content, scope?}`), `executionId?`, `subExecutionId?` |
| `ContextualHookInvoked` | `hookId`, `operationId`, `name`, `hookActionType` (`askAgent`, `runCommand`), `status` (`running`, `completed`, `failed`, `canceled`, `awaiting_approval`), `command?`, `output?`, `exitCode?`, `executionId?` |
| `sub_agent_start` · `sub_agent_progress` · `sub_agent_complete` | `parentExecutionId`, `subSessionId`. Start adds `subAgentName`, `prompt`, `explanation`. Progress adds `message`. Complete adds `response`, `status` (`success`, `error`, `cancelled`), `errorMessage?` |
| `tombstone` | `kind` (`checkpoint_revert`, `summarization`), `effectiveFromMessageId`, `metadata?` |
| `tool_revert` | `toolCallId` |
| `mode_change` | `fromMode`, `toMode`, `reason?` |
| `error` | `message`, `code?`, `details?` |
| `model_route` | `fromModel`, `toModel`, `reason` (`classifier`), `handoffMode` (`full`, `minimal`, `compacted`), `notification?` |
| `legacy_reasoning_replay` | `executionId`, `subExecutionId?`, `seal` |
| `system` | `content` |
| `agent_note` | `content`, `sectionChars?`, `noteUserRequests?` |

`consentRound` distinguishes repeated approval requests for one `toolCallId`. The engine matches resolutions by both fields (engine bundle, interaction identity comparison).

A record's physical position in the file is its order. Timestamps are not a sort key: in the captured turns `session_start` lands after the turn it bootstraps (captured on 2.12.1).

### Turn records

The engine flushes pending messages and synthesizes failed results for unfinished tool actions before closing a turn. For persisted turns, the closing sequence in 2.30.0 is (engine bundle, `persistTurnCompletion`): a refusal `assistant` record when the model refused, then `usage_summary` when a turn start time exists, then `session_event` with `category: "session_pause"`, then `turn_end`. The pause context is `{executionId, status}`, and both it and `usage_summary.status` derive from the stop reason:

| `turn_end.stopReason` | `session_pause` `context.status` and `usage_summary.status` |
| --- | --- |
| `cancelled` | `aborted` |
| `refusal`, `error` | `failed` |
| any other value, including `end_turn` and `content_filtered` | `success` |

When a session is loaded after an interruption, the engine appends a synthetic `turn_end` with `stopReason: "cancelled"` for a turn that never closed, and a `sub_agent_complete` with `status: "error"` for an unfinished subagent (engine bundle, ID suffix `-turn-end-synthetic`).

The older captures below show observed order, not a guarantee that every record appears. Follow `turn_end` for completion, rather than requiring `usage_summary`.

Two captured turns show the order inside a turn (captured on 2.12.1):

- Plain reply: `user`, `turn_start`, `assistant` (`Say`), `session_metadata` (`contextUsage`), `usage_summary`, `session_event` (`session_pause`, `success`), `turn_end` (`end_turn`), `session_start`.
- Approved write: `user`, `turn_start`, `pending_interaction` (`tool_approval`), `session_metadata`, `interaction_resolved` (`selected`, `accept`), `tool_call` (`fs_write`, `approved`), `tool_result` (`success: true`), `assistant` (`Say`), `session_metadata`, `session_event` (`session_pause`), `turn_end`, `session_start`.

A 2.21.4 fixture also records `fetch_cloud_config` before `turn_start`. Treat pre-turn tool records as valid.

A captured 2.21.4 turn that failed with `ModelThrottleError` wrote `user`, `turn_start`, `session_metadata`, `usage_summary` (`failed`), `session_event` (`session_pause`, `failed`), and `turn_end` (`error`), with no `assistant` record. `session_start` still followed the first turn. On 2.21.4, `/compact` wrote a `tombstone` and an `assistant` record under the same session id. No denial, cancellation, or `user_input` question turn was captured.

### Other session classes

`~/.kiro/sessions/cli/` holds the v2 engine's sessions: `<uuid>.json`, `<uuid>.jsonl`, and readline history files `<id>.history` (observed on 2.12.1). A `.history` file holds submitted prompt and slash-command text only, with no assistant output, timestamps, or tool results. `--session-source <v1|v2>` addresses the older stores.

Workflow steps can own backing sessions. Use the owning workflow to inspect, resume, retry, cancel, or delete them. They need not appear as independently resumable sessions ([Session management](https://kiro.dev/docs/cli/chat/session-management/)).

## Hooks

The v3 engine runs command and agent-prompt hooks from three sources (engine bundle, the standalone and agent-profile hook loaders):

| Source | Location | Notes |
| --- | --- | --- |
| Workspace | `<workspace root>/.kiro/hooks/*.json` | Files load in sorted filename order. |
| Global | `<userConfigDir>/hooks/*.json` | Added in 2.13.0: "Hooks placed in `~/.kiro/hooks/` now fire in every workspace automatically" ([changelog 2.13.0](https://kiro.dev/changelog/cli/)). Since 2.31.0 the home follows [Session store](#session-store). |
| Agent profile | the profile's `hooks` array | Each entry uses the standalone hook fields. Legacy object-form input converts `{command, matcher}` entries and drops `timeout_ms`, `max_output_size`, and `cache_ttl_seconds`. |

The terminal UI enables hooks with `hooks: {enabled: true, v2: true}` (TUI bundle). Here `v2` names the internal hook implementation. Standalone hook files use `version: "v1"` and a non-empty `hooks` array (engine bundle, hook schema). Agent profiles use a flat `hooks` array without that wrapper ([Hooks migration](https://kiro.dev/docs/cli/v3/hooks-migration/)).

| Field | Required | Meaning |
| --- | --- | --- |
| `version` | yes | `"v1"`. |
| `hooks` | yes | At least one hook. |
| `hooks[].name` | yes | Display name. |
| `hooks[].description` | no | Documentation only. |
| `hooks[].trigger` | yes | A trigger name or alias from the table below. |
| `hooks[].matcher` | no | Tool selector or regex for tool triggers. File triggers use a path regex. |
| `hooks[].action` | yes | `{"type": "command", "command": string}` or `{"type": "agent", "prompt": string}`. |
| `hooks[].timeout` | no | Seconds for command actions. Default `60`. `0` disables it. |
| `hooks[].enabled` | no | Default `true`. |
| `hooks[].confirm` | no | Stop only: `{question, options: [{id, label, run, continueReason?}], confirmCommand?}`. |

Triggers are PascalCase. The engine also accepts IDE and legacy CLI names as aliases (engine bundle, the trigger alias table).

| Trigger | Aliases | Fires | Matcher tests | Can block |
| --- | --- | --- | --- | --- |
| `SessionStart` | `sessionStart`, `agentSpawn` | Session start | not evaluated | no |
| `UserPromptSubmit` | `promptSubmit`, `userPromptSubmit` | Prompt submitted | not evaluated | yes |
| `PreToolUse` | `preToolUse` | Before a tool runs | tool selector or name regex | yes |
| `PostToolUse` | `postToolUse` | After a tool runs | tool selector or name regex | no |
| `SessionEnd` | none | Loaded session runtime teardown (since 2.25.0) | not evaluated | no |
| `Stop` | `agentStop`, `stop` | End of each agent turn | not evaluated | continues the turn |
| `PreTaskExec` · `PostTaskExec` | `preTaskExecution` · `postTaskExecution` | Around a spec task | not evaluated | Pre only |
| `PostFileCreate` · `PostFileSave` · `PostFileDelete` | `fileCreated` · `fileEdited`, `AfterFileEdit` · `fileDeleted` | After the agent changes a file | file path | no |
| `Manual` | `userTriggered` | Recognized, but the CLI `/hooks` UI offers no invocation action | not evaluated | no |

Since 2.25.0, `SessionEnd` means runtime teardown. It no longer aliases `Stop`. Existing hooks that require a per-turn event must use `Stop` (engine bundle, `hooks.trigger.sessionEndSemanticsChanged`). Teardown runs command actions only, once per loaded session runtime, with a shared five-second budget. Its stdin includes `reason`. Disposal maps reasons to `client_disconnect` or `shutdown` (engine bundle, `hooks.sessionEnd.budgetExhausted` and the disposal reason map).

Since 2.28.0, tool matchers accept categories `read`, `write`, `shell`, `web`, and `spec`. Source selectors include `@builtin`, `@mcp`, `@powers`, `@<server>`, and `@<server>/<tool>` ([Hook types](https://kiro.dev/docs/hooks/types/)). Plain selectors also fall back to a regex against the internal tool ID. File matchers remain regexes. `UserPromptSubmit` does not evaluate its matcher (engine bundle, matcher dispatch).

`Stop` runs when the turn graph reaches its stop step. Errors and cancellation can bypass it. Captured 2.12.1 turns invoke it per execution, before `turn_end`. A captured 2.21.4 throttled turn fired no `Stop`, and `/compact` fired no hook. These live observations were not repeated on 2.30.0.

The engine's server entry points still set `workspaceTrusted` to true, despite the hook module's untrusted-workspace suppression path. The engine command action substitutes `${WORKSPACE_ROOT}` with the session cwd. It does not substitute `{{filePath}}`, which older docs described (engine bundle, command action).

A command hook receives one JSON object on stdin (engine bundle, the hook input builder):

| Field | Present on |
| --- | --- |
| `session_id` | Every trigger. The session ID, also used for the session directory and `session.json.id`. |
| `hook_event_name` | Every trigger. Canonical PascalCase name, even when the definition uses an alias. |
| `cwd` | Every trigger. The session working directory. |
| `reason` | `SessionEnd` |
| `prompt` | `UserPromptSubmit` |
| `tool_name`, `tool_input` | `PreToolUse`, `PostToolUse` |
| `tool_response` | `PostToolUse` |
| `user_decision` | `Stop`, when a `confirm` prompt was answered: the chosen option |
| `spec_name`, `task_name` | `PreTaskExec`, `PostTaskExec` |
| `task_success` | `PostTaskExec` |
| `file_path` | `PostFileCreate`, `PostFileSave`, `PostFileDelete` |

The engine runs the command through the platform shell with the session cwd and its own environment, in a detached process group. A timeout or cancellation sends `SIGTERM` to the group, then `SIGKILL`. The exit code and stdout decide the effect (engine bundle, `resolveHookOutput` and the stop and permission decision parsers):

| Trigger | Exit 0 | Exit 2 | Other exit |
| --- | --- | --- | --- |
| `SessionStart` | stdout is added to context | ignored | ignored |
| `UserPromptSubmit` | stdout is appended to the prompt | prompt blocked. Stderr is the reason | ignored |
| `PreToolUse` | stdout `{"hookSpecificOutput": {"permissionDecision": "ask", "permissionDecisionReason"?}}` forces an approval prompt. Other decision strings are ignored | tool call blocked. Stderr is the reason | ignored |
| `PreTaskExec` | nothing | task blocked. Stderr is the reason | ignored |
| `Stop` | stdout `{"decision": "block", "reason"?}`, top level or under `hookSpecificOutput`, continues the turn | ignored | exit 1 continues the turn with stderr, or stdout when stderr is empty, as the reason. Other exits are ignored |
| `SessionEnd` and other triggers | nothing | ignored | ignored |

Output passed to the agent is wrapped in `<HOOK_INSTRUCTION>` tags. Agent-prompt actions append their prompt plus the stdin object as a fenced JSON block, and `Stop` and `SessionEnd` run command actions only. With `confirm`, a `Stop` hook asks first. `confirmCommand` runs with a 10-second timeout and may print `{"skip": true}` or replacement `question` and `options` ([Hooks](https://kiro.dev/docs/hooks/)). Turn hook invocations can append `ContextualHookInvoked` records to `messages.jsonl`. Their `hookId` is `<file path>#hook-<index>`. Do not assume every lifecycle hook writes a record. See [Undocumented behaviour](#undocumented-behaviour).

## Permissions

The v3 engine gates tools with capability rules ([Permissions](https://kiro.dev/docs/permissions/), [Permissions migration](https://kiro.dev/docs/cli/v3/permissions/)). A rule has `capability`, optional `match` and `exclude` globs, and `effect` (`allow`, `ask`, `deny`). Across all scopes the most restrictive effect wins.

| Scope | Location | Effects |
| --- | --- | --- |
| Kiro | mandatory built-in security rules, separate from default allowances | `deny`, `ask` |
| administration | `managed-settings.json` | `deny`, `ask` |
| user | `~/.kiro/settings/permissions.yaml` | all |
| workspace | `~/.kiro/workspace-roots/<hash>/permissions.yaml` | all |
| agent | the profile's `permissions` field | all |
| session | approvals given during the session, plus ACP `_meta.kiro.policyPreset` presets | all |

Capabilities are `fs_read`, `fs_write`, `shell`, `web_fetch`, `web_search`, `mcp`, `subagent`, `skill`, `power`, `context`, `diagnostics`, and `sandbox_network`, with meta-capabilities `all`, `builtin`, and `filesystem`. The built-in default policy allows `fs_read` on workspace files, read-only git commands (`git status`, `git log`, `git diff`), system-info commands (`pwd`, `whoami`, `uname`), and utility tools. It blocks writes to protected Kiro settings and asks before other operations. A permissions file adds rules to these defaults. See [Permissions](https://kiro.dev/docs/permissions/) for protected paths. An approval prompt is written to the session store as `pending_interaction` with `interactionType: "tool_approval"` (see [Message records](#message-records)).

`--trust-all-tools` and `/tools trust-all` grant session-wide trust. Non-interactive v3 runs also accept exact tool IDs through `--trust-tools`. The interactive v3 TUI ignores `--trust-tools` ([Permissions migration](https://kiro.dev/docs/cli/v3/permissions/)). Session trust cannot override a more restrictive rule.

## Headless and ACP

`kiro-cli chat --no-interactive` runs one prompt without the TUI ([Headless mode](https://kiro.dev/docs/cli/headless/)). The prompt is the positional argument or, when none is given, stdin. `--output-format stream-json` prints the run's events as JSON Lines on stdout and implies `--no-interactive`. It requires the v2 or v3 engine (added in 2.19.2). In v3, an interrupted run ends the stream with a `runError` record whose stage is `interrupted` (changelog 2.21.1). `kiro-cli-chat` names the final records `runFinished` and `runError`. Headless runs authenticate with `KIRO_API_KEY`.

V3 headless runs support hooks, knowledge bases, and code intelligence. Workflow runs remain attached through completion wakes for up to six hours by default. `KIRO_HEADLESS_WORKFLOW_TIMEOUT_SECS` lowers that bound ([changelog](https://kiro.dev/changelog/cli/)).

| Exit code | Condition |
| --- | --- |
| `1` | The requested agent fails to apply for a reason other than absence. |
| `3` | `--require-mcp-startup` is set and an enabled MCP server fails startup. |
| `4` | `--agent` names an unavailable agent, or a fresh v3 session has an unavailable `chat.defaultAgent`. Earlier output remains available. |

These headless failure codes follow the [release notes](https://kiro.dev/changelog/cli/). New v3 runs honor `chat.defaultModel` when no model is explicit. `--effort` applies to non-interactive runs.

`kiro-cli acp` serves the Agent Client Protocol over stdio JSON-RPC ([ACP](https://kiro.dev/docs/cli/acp/)). Its flags are `--agent`, `--model`, `--effort`, `--trust-all-tools`, `--trust-tools`, `--agent-engine` (default `v2`), and `--auth-method <cli|client>` (alias `--authMethod`). The default `cli` uses the CLI credential store. `client` forwards v3 access-token requests to the ACP client (`kiro-cli-chat acp --help`, 2.30.0). `kiro-cli serve [--port 8082]` starts a persistent WebSocket server for the v3 engine.

The engine exposes workflow extensions under `_kiro/workflow/` (engine bundle, workflow method registry):

| Methods | Surface |
| --- | --- |
| `new`, `invoke`, `load`, `inspect`, `list` | Create, run, and inspect workflows. `new` requires `parentSessionId` and exactly one of `workflowPath` or inline `workflow`. |
| `pause`, `resume`, `resumeAll`, `retry`, `cancel`, `delete`, `update` | Control existing workflows. |
| `listRecipes`, `listWatchHandlers` | Discover recipes and watch handlers. |

These are Kiro-specific ACP extensions. Their complete request and notification schemas remain outside this page's verified subset.

## Usage, credits, and context

Kiro meters credits, not tokens or dollars: "A credit is a unit of work in response to user prompts", metered to 0.01 credits ([Billing related questions](https://kiro.dev/docs/billing/related-questions/)). `usage_summary.promptTurnSummaries` carries credit usage, observed as `{"unit": "credit", "unitPlural": "credits", "usage": 0.1}` (captured on 2.12.1). `session_metadata` with `key: "contextUsage"` carries `value.usagePercentage`.

The 2.30.0 engine also persists optional telemetry in `usage_summary` (engine bundle, `persistTurnCompletion`):

| Field | Contents |
| --- | --- |
| `throughput` | Stream timestamps, duration, chunk and character counts, `estimatedTokens`, optional rates, and buckets. Token counts are estimates. |
| `recoveries` | Recovery mechanism names, as strings. |
| `contextBreakdown` | `schema: 1`, `modelCalls`, `totalChars`, `window: {sizeTokens, usagePercentage?}`, optional `compacted`, category measurements, and media counts. |

Model fallback notices persist as `session_metadata` with `key: "modelFallback"`. Its value carries `from`, `trigger`, optional `to`, `scope`, `reason`, `agentSubtaskId`, and `abandonedMessageIds`. The engine also sends ACP session info with `kind: "model_fallback"` (engine bundle, `handleModelFallback`). `model_route` records separately identify classifier-driven model changes. These fields expose more than the older percentage-only captures. They do not establish a dollar price.

`/context` shows context use. Its documentation notes that accurate usage updates when the next message is sent ([Context](https://kiro.dev/docs/cli/chat/context/)).

## Configuration

Configuration has global (`~/.kiro/`), project (`<project>/.kiro/`), and agent scopes, and the narrower scope wins ([Configuration](https://kiro.dev/docs/configuration/)). CLI settings live in `~/.kiro/settings/cli.json`, edited with `kiro-cli settings <key> <value>` ([Settings](https://kiro.dev/docs/reference/settings/)).

| Setting or variable | Meaning |
| --- | --- |
| `KIRO_HOME` | Override for `~/.kiro`, shared by the engine since 2.31.0. See validation under [Session store](#session-store). |
| `KIRO_API_KEY` | API-key authentication for headless runs. |
| `KIRO_HEADLESS_WORKFLOW_TIMEOUT_SECS` | Lower the default six-hour headless workflow wait. |
| `KIRO_SKIP_BINARY_PINNING=1` | Disable binary pinning for fixed-path installations. Do not use when an installer can replace the running binary ([changelog 2.22.0](https://kiro.dev/changelog/cli/)). |
| `NODE_USE_SYSTEM_CA` | V3 uses OS certificates by default. An explicit value, including `0`, takes precedence ([changelog 2.26.1](https://kiro.dev/changelog/cli/)). |
| `KIRO_ACP_RECORD_PATH` | JSONL recording of TUI ACP traffic. |
| `KIRO_KAS_NODE_PATH` · `KIRO_KAS_SERVER_PATH` | Node runtime and v3 engine server path. |
| `chat.agentEngine` | Default engine. |
| `chat.defaultModel` · `chat.defaultAgent` | Defaults for new sessions. |
| `api.timeout` · `api.streamIdleSoftTimeout` · `api.streamIdleHardTimeout` · `api.subagentTimeout` | Stream timeout (3600 s), idle warning (60 s), idle cancel (300 s), and subagent idle deadline (3600 s), from 2.19.0. |

Since 2.24.0, project `.env` files no longer load automatically into chats, MCP servers, or tools. Export required values before launch ([changelog](https://kiro.dev/changelog/cli/)).

V3 no longer provides `todo_list` as of 2.27.0. `chat.enableTodoList` has no effect in v3 ([changelog](https://kiro.dev/changelog/cli/)). With Workflows enabled, main-chat delegation uses workflows. The former "Workflows: sub-agent tool" setting no longer changes behavior as of 2.28.0.

Local v3 sessions support memory access modes through `/memories config`: read and write, read only, or off. Access changes apply next session. Automatic updates can continue after a session ends when enabled ([Memory](https://kiro.dev/docs/memory/)).

## Upstream scope

Kiro CLI 2.30.0 still lists v2 as the default engine. A saved setting or the startup migration prompt can select v3 instead ([CLI 3.0 overview](https://kiro.dev/docs/cli/v3/)). Most docs pages describe the IDE and CLI together, and several still describe the 2.x forms, so the tables above follow the engine bundle where the two differ. The gaps RimZ records against this surface are in the internals page's [Known gaps](../../internals/agents/adapter_kiro.md#known-gaps).

## Undocumented behaviour

1. Record order for denied, cancelled, and `user_input` turns on a live session.
2. The complete set of `turn_end.stopReason` values. The schema accepts any non-empty string. Live transitions to `session.json` status `completed` remain uncaptured.
3. The `stream-json` event schema beyond the `runError` and `runFinished` names.
4. Full workflow extension schemas, subagent-to-sub-execution file routing, and the workspace permissions hash algorithm. These need separate wire verification.
5. Timing of `SessionStart` relative to session creation, and why it records no `ContextualHookInvoked` entry: on 2.21.4 the command runs and receives its stdin payload, but the captured store holds invocation records only for `UserPromptSubmit` and `Stop`.

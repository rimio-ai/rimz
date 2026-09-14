# Kiro CLI protocol reference

> This page records the upstream Kiro CLI surface. RimZ's mapping of it, and the gaps RimZ records, are in [adapter_kiro.md](../../internals/agents/adapter_kiro.md); the provider-neutral lifecycle contract is [model.md](../../internals/agents/model.md).

Kiro CLI is AWS's terminal coding agent. It ships three agent engines, and this page covers the v3 engine (Kiro's "CLI 3.0", in early access) that `kiro-cli chat --v3` selects: the executables and engine selection, launch and resume, the local session store, hooks, permissions, headless and ACP modes, usage, and configuration. It makes no claim about what RimZ supports.

**Refresh baseline.** Kiro CLI 2.21.4, released 2026-09-11: the version the stable channel manifest pins (`https://prod.download.cli.kiro.dev/stable/latest/manifest.json`, which the installer at <https://cli.kiro.dev/install> and `kiro-cli update` read). Docs, changelog, release feed, and the installed 2.21.4 build were read on 2026-09-13. Kiro CLI is closed source under the AWS Intellectual Property License and publishes no tags or commits ([Upgrading from Q CLI](https://kiro.dev/docs/upgrade-guides/migrating-from-q/)). Wire claims the docs leave out are read from two JavaScript bundles embedded in the 2.21.4 `kiro-cli-chat` binary: the v3 engine, package `@kiro/agent` 0.63.3, file `dist/server/acp-server.js` (cited as "engine bundle"), and the terminal UI `tui.js` (cited as "TUI bundle"). Both are minified, so citations quote the string literal or schema that carries the claim.

**Inline pins.** Record order inside real turns comes from stock sessions captured on Kiro CLI 2.12.1 and was not re-captured, because running a session is outside a refresh pass. Each such claim is marked "captured on 2.12.1". The redacted captures are RimZ's fixtures under `crates/rimz/src/agents/adapters/kiro/tests/fixtures/`.

## Upstream sources

The docs are rolling and unversioned; several pages cover the Kiro IDE and CLI together.

| Surface | Official source |
| --- | --- |
| CLI 3.0 overview and new features | <https://kiro.dev/docs/cli/v3/> · <https://kiro.dev/docs/cli/v3/new-features/> |
| Changelog and release feed | <https://kiro.dev/changelog/cli/> · `https://prod.download.cli.kiro.dev/stable/<version>/feed.json` |
| CLI commands and slash commands | <https://kiro.dev/docs/reference/cli-commands/> · <https://kiro.dev/docs/reference/slash-commands/> |
| Hooks | <https://kiro.dev/docs/hooks/> · <https://kiro.dev/docs/hooks/types/> · <https://kiro.dev/docs/hooks/actions/> · <https://kiro.dev/docs/cli/v3/hooks-migration/> |
| Permissions | <https://kiro.dev/docs/permissions/> · <https://kiro.dev/docs/cli/v3/permissions/> |
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
kiro-cli-chat serve --help
```

## Executables and engines

A Kiro CLI install puts three executables on `PATH` (`~/.local/bin/` on Linux) and extracts helper runtimes on first use.

| Executable or runtime | Role |
| --- | --- |
| `kiro-cli` | Launcher and desktop-integration commands; `kiro-cli chat` hands off to `kiro-cli-chat`, forwarding its arguments unchanged. |
| `kiro-cli-chat` | Chat CLI: argument parsing, the v1 and v2 engines, headless runs, and the launcher for the v3 terminal UI and engine. |
| `kiro-cli-term` | Shell-integration terminal process; it runs for integrated shells, not only for chat. |
| `bun` running `tui.js` | The terminal UI, extracted under `~/.local/share/kiro-cli/`. |
| `node` running `acp-server.js` | The v3 engine (Kiro calls it KAS), extracted under `~/.local/share/kiro-cli/kas/<version>-<sha256>/`; `KIRO_KAS_NODE_PATH` and `KIRO_KAS_SERVER_PATH` override the runtime and server path (TUI bundle). |

The engine is chosen per launch. `--agent-engine <v1|v2|v3>` selects it and defaults to `v2` (`kiro-cli chat --help`); `--v3` and `--v2` are shorthands. A saved `chat.agentEngine` setting changes the default, and `--v2` overrides that setting for one session without rewriting it ([changelog 2.21.4](https://kiro.dev/changelog/cli/)). The v3 engine is early access: "V3 runs alongside your existing 2.x install" ([CLI 3.0 overview](https://kiro.dev/docs/cli/v3/)). Version 2.21.4 also carries `--tui`, `--legacy-ui` (alias `--classic`), `--mode <default|spec>` for v3, and `--cloud` with `--repo` for cloud-sandbox sessions.

## Launch and resume

`kiro-cli chat` takes one optional positional argument, `[INPUT]`, "The first question to ask" (`kiro-cli chat --help`). The docs pass it without a separator: `kiro-cli chat --effort high "Refactor this module for testability"` ([CLI commands](https://kiro.dev/docs/reference/cli-commands/)).

A prompt after `--` passes the Rust parser but the v3 terminal UI drops it. `kiro-cli chat --v3 -- --bogus extra` fails with `error: unexpected argument 'extra' found`, which shows that both `kiro-cli` and `kiro-cli-chat` accept `--` and bind the next token to `INPUT`. The TUI bundle then parses the argument list again with its own loop: a token that starts with `-` and is not a known flag is treated as an unknown option, and the loop consumes the following token as that option's value. Run against `["chat","--v3","--","ping"]`, the loop returns no `input`; against `["chat","--v3","ping"]` it returns `input: "ping"`. A live 2.21.4 session confirms the drop: `kiro-cli chat --v3 -- 'Reply with the single word ok'` opens a session that runs no turn, while the same prompt without `--` runs.

| Flag | Meaning (`kiro-cli chat --help`, 2.21.4) |
| --- | --- |
| `[INPUT]` | First prompt. With `--no-interactive`, stdin is read as the prompt when no positional argument is given. |
| `-r`, `--resume` | Resume the most recent conversation from this directory. |
| `--resume-id <SESSION_ID>` | Resume a specific session. v3 session IDs are `sess_<uuid>`. |
| `--resume-picker` (alias `--list`) | Pick a conversation from this directory interactively. |
| `--sessions` | Open the v3 session dashboard; closing it exits. |
| `--agent <AGENT>` | Agent profile to use. |
| `--model <MODEL>` | Model for the session. |
| `--effort <EFFORT>` | Initial effort: `low`, `medium`, `high`, `xhigh`, `max`. |
| `-a`, `--trust-all-tools` · `--trust-tools <NAMES>` | Tool trust. The v3 engine replaces both with `permissions.yaml` (see [Permissions](#permissions)). |
| `--no-interactive` · `--output-format <text\|stream-json>` | Headless run (see [Headless and ACP](#headless-and-acp)). |
| `-l`, `--list-sessions` · `--all-cwds` · `-f`, `--format <plain\|json\|json-pretty>` | List saved sessions for this directory, or every directory. |
| `-d`, `--delete-session <SESSION_ID>` · `--session-source <v1\|v2>` | Delete a saved session. |
| `--list-models` | List models and exit. |
| `--require-mcp-startup` | Exit with code 3 when an enabled MCP server fails to start. |
| `-w`, `--wrap <always\|never\|auto>` | Line wrapping. |

The docs' flag list omits `--model`; the 2.21.4 binary accepts it, and the TUI bundle forwards `--agent`, `--model`, `--effort`, `--trust-all-tools`, `--trust-tools`, and `--agent-engine` to the engine.

Interactive commands that touch session state are slash commands inside the TUI ([Slash commands](https://kiro.dev/docs/reference/slash-commands/)).

| Command | Effect |
| --- | --- |
| `/compact` | Summarize the conversation to free context. The engine records compaction as a `tombstone` record with `kind: "summarization"` (engine bundle schema). |
| `/rewind` | Fork the conversation at an earlier turn; the engine records a revert as a `tombstone` with `kind: "checkpoint_revert"`. |
| `/model` · `/effort` | Change model or effort for the session. |
| `/context` | Show context-window use, with a per-tool token breakdown since 2.16.0. |
| `/usage` | Show plan, limits, and credits. |
| `/sessions` | Browse, search, resume, and clean up v3 sessions (2.21.0). |
| `/tangent` | Create a conversation checkpoint to explore a side topic (2.16.0); the engine schema has `createdReason: "tangent"` for such sessions. |

## Session store

The v3 engine writes one directory per session, bucketed by workspace:

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

The bucket is the first 16 hex characters of SHA-256 over the session's workspace paths. The engine normalizes each path (absolute, forward slashes, no trailing slash, lowercased on Windows), sorts them, and joins them with a NUL byte; a session with no workspace path uses the bucket `_global` (engine bundle, the `createHash("sha256")` call that feeds `computeWorkspaceHash`). For one workspace this is SHA-256 of the exact absolute path. To find an existing session, the engine tries the caller's bucket, then the session index, then every bucket directory.

The sessions root and `KIRO_HOME` disagree between docs and wire. The settings reference says `KIRO_HOME` "Overrides the `~/.kiro` directory used for global agents, prompts, skills, steering, settings, and sessions" ([Settings](https://kiro.dev/docs/reference/settings/)). The engine bundle sets `sessionsPath` to `<homeDir>/.kiro/sessions`, with `homeDir` defaulting to Node's `os.homedir()`, and contains no `KIRO_HOME` read; the TUI bundle and `kiro-cli-chat` do read `KIRO_HOME` for their own paths. A live 2.21.4 launch with `KIRO_HOME` set wrote `settings/cli.json` under that directory but created the session under `$HOME/.kiro/sessions/`, and hooks under `$HOME/.kiro/hooks/` fired: the sessions root and global hooks ignore `KIRO_HOME`. The session-management page also describes storage as "SQLite database in `~/.kiro/`" ([Session management](https://kiro.dev/docs/cli/chat/session-management/)); that describes the v2 engine, and the v3 store is the file layout above.

The engine creates `session.json` and an empty `messages.jsonl` before the first prompt (captured on 2.12.1). It validates `session.json` against this schema on load (engine bundle, the object with `schemaVersion`, `dataModelVersion`, and `workspacePaths`):

| Field | Type | Notes |
| --- | --- | --- |
| `schemaVersion` | string | `"1.0.0"` in every session on the baseline host. |
| `dataModelVersion` | non-negative integer, optional | `1`; absent on legacy sessions. |
| `id` | string | `sess_<uuid>`, equal to the directory name. |
| `title` | string | Required. |
| `agentMode` | string | For example `vibe`. |
| `workspacePaths` | string array | Absolute workspace paths; the bucket input. |
| `rootPaths` | string array, optional | |
| `createdAt` · `lastModifiedAt` | string | ISO 8601 timestamps. |
| `status` | enum, optional | `in_progress`, `waiting_on_user`, `completed`, `idle`, `failed`. Absent in a newborn session. |
| `parentSessionId` · `parentExecutionId` | string, optional | Set on child sessions. |
| `createdReason` | enum, optional | `human`, `rewind`, `subagent`, `tangent`. |
| `modelId` · `effortLevel` · `autopilot` | optional | Persisted model, effort, and autonomy. |
| `executionTarget` | optional | `{ "kind": "local" }` or `{ "kind": "cloud-sandbox" }`. |
| `lastCheckpointId`, `repositories`, `description`, `contextFiles`, and spec, review, and compaction switches | optional | |

The engine sets `status` from turn activity: `in_progress` when a turn starts, then `waiting_on_user`, `failed`, or `idle` when it ends (engine bundle, `persistActivityStatus` and `terminalActivityStatus`).

### Message records

`messages.jsonl` is append-only, one JSON object per line: `{"id": string, "timestamp": string, "payload": {"type": ..., ...}}`. The engine validates payloads against a discriminated union on `type` (engine bundle); the table lists every member, with full fields for the ones that carry turn state.

| `type` | Fields |
| --- | --- |
| `user` | `content`, `contextItems?`, `source?` (`chat`, `hook`, `api`, `steer`), `images?`, `documents?`, `kind?`, `_meta?` |
| `assistant` | `content`, `operationType?` (`Say`, `Reasoning`, `Print`, `Summary`), `executionId?`, `subExecutionId?`, reasoning signature fields, `_meta?` |
| `tool_call` | `toolCallId`, `toolName`, `args`, `status` (`pending`, `awaiting_approval`, `approved`, `denied`, `executing`, `completed`, `failed`), `title?`, `kind?` (`read`, `edit`, `execute`, `search`, `delete`, `move`, `fetch`, `think`, `switch_mode`, `other`), `actionType?`, `filePath?`, `executionId?`, `subExecutionId?`, snapshot IDs, `_meta?` |
| `tool_result` | `toolCallId`, `content`, `success` (boolean), `durationMs?`, `metadata?`, `executionId?`, `subExecutionId?`, `_meta?` |
| `pending_interaction` | `interactionType` (`tool_approval`, `user_input`), `toolCallId`, `question`, `options` (approval options `{optionId, name, kind}` or question options `{title, description?, recommended?, subOptions?}`), `executionId`, `_meta?` |
| `interaction_resolved` | `toolCallId`, `outcome` (for example `selected`), `selectedOption?` (for example `accept`), `executionId` |
| `turn_start` | `executionId?` |
| `turn_end` | `stopReason` (non-empty string), `stopDetails?` (`{refusal?: {category?, explanation?, recommendedModel?}}`), `executionId?` |
| `session_event` | `category` (`session_start`, `session_restore`, `session_pause`, `session_resume`), `context?` (object) |
| `usage_summary` | `promptTurnSummaries` (array of `{usedTools?, unit?, unitPlural?, usage?}`), `elapsedTime?` (ms), `status?` (`success`, `failed`, `aborted`), `executionId?`, `requestIds?` |
| `session_metadata` | `key`, `value`, `executionId`. Keys written: `contextUsage` with `{usagePercentage: number}`, and `recap` with `{text}`. |
| `session_start` | `agentType`, `content` (bootstrap context, not conversation), `forcedRole`, `messageId`, `images?`, `documents?`, `steeringDocuments?` |
| `steering_inclusion` | `documents` (IDs or `{id, displayName, content, scope?}`), `executionId?`, `subExecutionId?` |
| `ContextualHookInvoked` | `hookId`, `operationId`, `name`, `hookActionType` (`askAgent`, `runCommand`), `status` (`running`, `completed`, `failed`, `canceled`, `awaiting_approval`), `command?`, `output?`, `exitCode?`, `executionId?` |
| `sub_agent_start` · `sub_agent_progress` · `sub_agent_complete` | `parentExecutionId`, `subSessionId`; start adds `subAgentName`, `prompt`, `explanation`; progress adds `message`; complete adds `response`, `status` (`success`, `error`, `cancelled`), `errorMessage?` |
| `tombstone` | `kind` (`checkpoint_revert`, `summarization`), `effectiveFromMessageId`, `metadata?` |
| `tool_revert` | `toolCallId` |
| `mode_change` | `fromMode`, `toMode`, `reason?` |
| `error` | `message`, `code?`, `details?` |
| `system` · `agent_note` | `content` |

A record's physical position in the file is its order. Timestamps are not a sort key: in the captured turns `session_start` lands after the turn it bootstraps (captured on 2.12.1).

### Turn records

The engine closes every turn with the same sequence (engine bundle, `persistTurnCompletion`): a refusal `assistant` record when the model refused, then `usage_summary`, then `session_event` with `category: "session_pause"`, then `turn_end`. The pause context is `{executionId, status}`, and both it and `usage_summary.status` derive from the stop reason:

| `turn_end.stopReason` | `session_pause` `context.status` and `usage_summary.status` |
| --- | --- |
| `cancelled` | `aborted` |
| `refusal`, `error` | `failed` |
| any other value, including `end_turn` | `success` |

When a session is loaded after an interruption, the engine appends a synthetic `turn_end` with `stopReason: "cancelled"` for a turn that never closed, and a `sub_agent_complete` with `status: "error"` for an unfinished subagent (engine bundle, ID suffix `-turn-end-synthetic`).

Two captured turns show the order inside a turn (captured on 2.12.1):

- Plain reply: `user`, `turn_start`, `assistant` (`Say`), `session_metadata` (`contextUsage`), `usage_summary`, `session_event` (`session_pause`, `success`), `turn_end` (`end_turn`), `session_start`.
- Approved write: `user`, `turn_start`, `pending_interaction` (`tool_approval`), `session_metadata`, `interaction_resolved` (`selected`, `accept`), `tool_call` (`fs_write`, `approved`), `tool_result` (`success: true`), `assistant` (`Say`), `session_metadata`, `session_event` (`session_pause`), `turn_end`, `session_start`.

A 2.21.4 turn that failed with `ModelThrottleError` wrote `user`, `turn_start`, `session_metadata`, `usage_summary` (`failed`), `session_event` (`session_pause`, `failed`), and `turn_end` (`error`), with no `assistant` record; `session_start` still followed the first turn. `/compact` wrote a `tombstone` and an `assistant` record under the same session id. No denial, cancellation, or `user_input` question turn was captured.

### Other session classes

`~/.kiro/sessions/cli/` holds the v2 engine's sessions: `<uuid>.json`, `<uuid>.jsonl`, and readline history files `<id>.history` (observed on 2.12.1). A `.history` file holds submitted prompt and slash-command text only, with no assistant output, timestamps, or tool results. `--session-source <v1|v2>` addresses the older stores. `KIRO_ACP_RECORD_PATH` names a JSONL file that records the TUI's ACP traffic for debugging ([Settings](https://kiro.dev/docs/reference/settings/)).

## Hooks

The v3 engine runs command and agent-prompt hooks from three sources (engine bundle, the standalone and agent-profile hook loaders):

| Source | Location | Notes |
| --- | --- | --- |
| Workspace | `<workspace root>/.kiro/hooks/*.json` | Files load in sorted filename order. |
| Global | `~/.kiro/hooks/*.json` | Added in 2.13.0: "Hooks placed in `~/.kiro/hooks/` now fire in every workspace automatically" ([changelog 2.13.0](https://kiro.dev/changelog/cli/)). The engine resolves `~` from its `homeDir`. |
| Agent profile | the profile's `hooks` field | The 2.x form, an object keyed by trigger with `{command, matcher}` entries, is converted; its `timeout_ms`, `max_output_size`, and `cache_ttl_seconds` fields are dropped. |

The terminal UI turns hooks on for every v3 session: it initializes the engine with `hooks: {enabled: true, v2: true}` (TUI bundle). On 2.12.1, before global hooks existed, the stock session store shows the split. Sessions whose workspace was the home directory, so that `~/.kiro/hooks/` was also the workspace hook directory, recorded `ContextualHookInvoked` with `status: "completed"` for command hooks from `~/.kiro/hooks/rimz.json`. Sessions in `/tmp` workspaces recorded none.

A hook file has `version` and `hooks` ([Hooks](https://kiro.dev/docs/hooks/); engine bundle schema, log text "Hook file does not match v2 schema"):

| Field | Required | Meaning |
| --- | --- | --- |
| `version` | yes | `"v1"`. |
| `hooks` | yes | At least one hook. |
| `hooks[].name` | yes | Display name. |
| `hooks[].description` | no | Documentation only. |
| `hooks[].trigger` | yes | A trigger name or alias from the table below. |
| `hooks[].matcher` | no | Regex, tested against the target the trigger table names. |
| `hooks[].action` | yes | `{"type": "command", "command": string}` or `{"type": "agent", "prompt": string}`. |
| `hooks[].timeout` | no | Seconds for command actions; default `60`; `0` disables it. |
| `hooks[].enabled` | no | Default `true`. |
| `hooks[].confirm` | no | Stop only: `{question, options: [{id, label, run, continueReason?}], confirmCommand?}`. |

Triggers are PascalCase; the engine also accepts IDE and 2.x names as aliases (engine bundle, the trigger alias table).

| Trigger | Aliases | Fires | Matcher tests | Can block |
| --- | --- | --- | --- | --- |
| `SessionStart` | `sessionStart`, `agentSpawn` | Session start | not evaluated | no |
| `UserPromptSubmit` | `promptSubmit`, `userPromptSubmit` | Prompt submitted | not evaluated | yes |
| `PreToolUse` | `preToolUse` | Before a tool runs | tool name | yes |
| `PostToolUse` | `postToolUse` | After a tool runs | tool name | no |
| `Stop` | `agentStop`, `stop`, `SessionEnd` | End of each agent turn | not evaluated | continues the turn |
| `PreTaskExec` · `PostTaskExec` | `preTaskExecution` · `postTaskExecution` | Around a spec task | not evaluated | Pre only |
| `PostFileCreate` · `PostFileSave` · `PostFileDelete` | `fileCreated` · `fileEdited`, `AfterFileEdit` · `fileDeleted` | After the agent changes a file | file path | no |
| `Manual` | `userTriggered` | On demand | not evaluated | no |

The docs disagree with the wire on three hook points. The hooks migration page says `Stop` fires when the "Session ends" and that a `UserPromptSubmit` matcher tests the prompt text ([Hooks migration](https://kiro.dev/docs/cli/v3/hooks-migration/)); the matcher code evaluates no matcher for `UserPromptSubmit`, and the 2.12.1 store records one `Stop` invocation per turn `executionId`. `Stop` runs only when the turn graph reaches its agent-stop step: `end_turn`, `max_tokens`, and `refusal` turns reach it, while a model error other than context overflow, or a cancel, throws out of the graph and skips it (engine bundle, `completeWithError` and `completeWithAbort`; a live throttled turn fired no `Stop`). It runs before `turn_end` is written, after the turn's `Say` record is on disk. `/compact` fires no hook. Both server entry points hardcode `workspaceTrusted` and `v2Hooks` to true, so the untrusted-workspace suppression path never runs in this build. The same page documents a `{{filePath}}` command template, which does not occur in the engine bundle; the command action substitutes only `${WORKSPACE_ROOT}`, with the session cwd. The hook actions page's CLI tab still describes the 2.x agent-config form (`timeout_ms`, default 30000 ms).

A command hook receives one JSON object on stdin (engine bundle, the hook input builder):

| Field | Present on |
| --- | --- |
| `session_id`, `hook_event_name`, `cwd` | every trigger |
| `prompt` | `UserPromptSubmit` |
| `tool_name`, `tool_input` | `PreToolUse`, `PostToolUse` |
| `tool_response` | `PostToolUse` |
| `user_decision` | `Stop`, when a `confirm` prompt was answered: the chosen option |
| `spec_name`, `task_name` | `PreTaskExec`, `PostTaskExec` |
| `task_success` | `PostTaskExec` |
| `file_path` | `PostFileCreate`, `PostFileSave`, `PostFileDelete` |

The engine runs the command through the platform shell with the session cwd and its own environment, in a detached process group; a timeout or cancellation sends `SIGTERM` to the group, then `SIGKILL`. The exit code and stdout decide the effect (engine bundle, `resolveHookOutput` and the stop and permission decision parsers):

| Trigger | Exit 0 | Exit 2 | Other exit |
| --- | --- | --- | --- |
| `SessionStart` | stdout is added to context | ignored | ignored |
| `UserPromptSubmit` | stdout is appended to the prompt | prompt blocked; stderr is the reason | ignored |
| `PreToolUse` | stdout `{"hookSpecificOutput": {"permissionDecision": "ask", "permissionDecisionReason"?}}` forces an approval prompt | tool call blocked; stderr is the reason | ignored |
| `PreTaskExec` | nothing | task blocked; stderr is the reason | ignored |
| `Stop` | stdout `{"decision": "block", "reason"?}`, top level or under `hookSpecificOutput`, continues the turn | ignored | exit 1 continues the turn with stderr, or stdout when stderr is empty, as the reason |
| other triggers | nothing | ignored | ignored |

Output passed to the agent is wrapped in `<HOOK_INSTRUCTION>` tags. Agent-prompt actions append their prompt plus the stdin object as a fenced JSON block, and `Stop` runs command actions only. With `confirm`, a `Stop` hook asks first; `confirmCommand` runs with a 10-second timeout and may print `{"skip": true}` or replacement `question` and `options` ([Hooks](https://kiro.dev/docs/hooks/)). Each hook run appends a `ContextualHookInvoked` record to `messages.jsonl`; its `hookId` is `<file path>#hook-<index>`.

## Permissions

The v3 engine gates tools with capability rules and ignores the trust flags ([Permissions](https://kiro.dev/docs/permissions/), [Permissions migration](https://kiro.dev/docs/cli/v3/permissions/)). A rule has `capability`, optional `match` and `exclude` globs, and `effect` (`allow`, `ask`, `deny`); across all scopes the most restrictive effect wins.

| Scope | Location | Effects |
| --- | --- | --- |
| Kiro | built in | `deny`, `ask` |
| administration | `managed-settings.json` | `deny`, `ask` |
| user | `~/.kiro/settings/permissions.yaml` | all |
| workspace | `~/.kiro/workspace-roots/<hash>/permissions.yaml` | all |
| agent | the profile's `permissions` field | all |
| session | approvals given during the session, plus ACP `_meta.kiro.policyPreset` presets | all |

Capabilities are `fs_read`, `fs_write`, `shell`, `web_fetch`, `web_search`, `mcp`, `subagent`, `skill`, `power`, `context`, `diagnostics`, and `sandbox_network`, with meta-capabilities `all`, `builtin`, and `filesystem`. With no `permissions.yaml`, the default policy allows `fs_read` on workspace files, read-only git commands (`git status`, `git log`, `git diff`), system-info commands (`pwd`, `whoami`, `uname`), and utility tools; everything else asks. An approval prompt is written to the session store as `pending_interaction` with `interactionType: "tool_approval"` (see [Message records](#message-records)).

## Headless and ACP

`kiro-cli chat --no-interactive` runs one prompt without the TUI ([Headless mode](https://kiro.dev/docs/cli/headless/)). The prompt is the positional argument or, when none is given, stdin. `--output-format stream-json` prints the run's events as JSON Lines on stdout and implies `--no-interactive`; it requires the v2 or v3 engine (added in 2.19.2). In v3, an interrupted run ends the stream with a `runError` record whose stage is `interrupted` (changelog 2.21.1); `kiro-cli-chat` names the final records `runFinished` and `runError`. Headless runs authenticate with `KIRO_API_KEY`.

`kiro-cli acp` serves the Agent Client Protocol over stdio JSON-RPC ([ACP](https://kiro.dev/docs/cli/acp/)). Its flags are `--agent`, `--model`, `--effort`, `--trust-all-tools`, `--trust-tools`, `--agent-engine` (default `v2`), and `--auth-method cli`, which keeps v3 token resolution inside the process (`kiro-cli acp --help`). `kiro-cli serve [--port 8082]` starts a persistent WebSocket server for the v3 engine.

## Usage, credits, and context

Kiro meters credits, not tokens or dollars: "A credit is a unit of work in response to user prompts", metered to 0.01 credits ([Billing related questions](https://kiro.dev/docs/billing/related-questions/)). The session store carries two usage figures. `usage_summary` records per-turn `promptTurnSummaries`, observed as `{"unit": "credit", "unitPlural": "credits", "usage": 0.1}` (captured on 2.12.1). `session_metadata` with `key: "contextUsage"` records `value.usagePercentage`, a number. No local file records token counts, the context-window size, the active model per turn, or a price. `/context` shows the percentage and notes that "The accurate usage is only calculated when you send your next message" ([Context](https://kiro.dev/docs/cli/chat/context/)).

## Configuration

Configuration has global (`~/.kiro/`), project (`<project>/.kiro/`), and agent scopes, and the narrower scope wins ([Configuration](https://kiro.dev/docs/configuration/)). CLI settings live in `~/.kiro/settings/cli.json`, edited with `kiro-cli settings <key> <value>` ([Settings](https://kiro.dev/docs/reference/settings/)).

| Setting or variable | Meaning |
| --- | --- |
| `KIRO_HOME` | Documented override for `~/.kiro`; see the disagreement under [Session store](#session-store). |
| `KIRO_API_KEY` | API-key authentication for headless runs. |
| `KIRO_ACP_RECORD_PATH` | JSONL recording of TUI ACP traffic. |
| `KIRO_KAS_NODE_PATH` · `KIRO_KAS_SERVER_PATH` | Node runtime and v3 engine server path. |
| `chat.agentEngine` | Default engine. |
| `chat.defaultModel` · `chat.defaultAgent` | Defaults for new sessions. |
| `api.timeout` · `api.streamIdleSoftTimeout` · `api.streamIdleHardTimeout` · `api.subagentTimeout` | Stream timeout (3600 s), idle warning (60 s), idle cancel (300 s), and subagent idle deadline (3600 s), from 2.19.0. |

## Upstream scope

Kiro presents CLI 3.0 as an early release that runs beside the 2.x engines, with v2 still the default engine on the baseline ([CLI 3.0 overview](https://kiro.dev/docs/cli/v3/)). Most docs pages describe the IDE and CLI together, and several still describe the 2.x forms, so the tables above follow the engine bundle where the two differ. The gaps RimZ records against this surface are in the internals page's [Known gaps](../../internals/agents/adapter_kiro.md#known-gaps).

## Undocumented behaviour

1. Record order for denied, cancelled, and `user_input` turns on a live session.
2. The `turn_end.stopReason` values beyond `end_turn`, `cancelled`, and `error`, and when `session.json` reaches `completed`.
3. The `stream-json` event schema beyond the `runError` and `runFinished` names.
4. Why `SessionStart` records no `ContextualHookInvoked` entry: on 2.21.4 the command runs and receives its stdin payload, but the captured store holds invocation records only for `UserPromptSubmit` and `Stop`.

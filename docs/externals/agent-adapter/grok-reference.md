# Grok Build protocol reference

> RimZ's mapping of this surface lives in [adapter_grok.md](../../internals/agents/adapter_grok.md): the lifecycle signals, transcript fold, context, account, and spend mapping, and the known gaps. The provider-neutral agent model is [model.md](../../internals/agents/model.md), and the account and spend model is [providers.md](../../internals/agents/providers.md).

This page mirrors the upstream surface of xAI's Grok Build CLI (`grok`) that a RimZ adapter binds to: native lifecycle hooks with their envelope, turn-end reports, and decision channel; the session directory and its durable files; structured headless output; the Agent Client Protocol (ACP) mode and its client-registered hooks; and the authentication store. It records what upstream ships. Coverage is depth on the stock-TUI surfaces the adapter reads (hooks, `summary.json`, `updates.jsonl`, `signals.json`, `events.jsonl`, `auth.json`) and breadth as an index for the rest.

## Baseline and sources

The page describes Grok Build **1.0.30**, published 2026-09-11 and read on 2026-09-13. That is the npm `@xai-official/grok` `latest` dist-tag and the version the official installer's `stable` channel serves (`https://x.ai/cli/stable`). Upstream publishes no release tags and no commit for a release: the open-source mirror [`xai-org/grok-build`](https://github.com/xai-org/grok-build) receives periodic "Synced from monorepo" commits. Source claims are read at commit [`37949780c144e37df692e3d669051a21fec24f20`](https://github.com/xai-org/grok-build/tree/37949780c144e37df692e3d669051a21fec24f20) (2026-09-09), the newest sync before 1.0.30, and a source path below is relative to `crates/codegen/` at that commit. CLI flags and subcommands come from `grok --help` on an installed 1.0.30 binary. The mirror's `xai-grok-pager/npm/grok/package.json` still reads `0.1.220-alpha.4` and does not track releases.

| Surface | Source |
| --- | --- |
| Authentication, credential precedence | [user guide: authentication](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md); `xai-grok-login/src/model.rs` `GrokAuth`, `AuthMode`; `xai-grok-login/src/storage.rs` `auth_json_path`; `xai-grok-shell/src/agent/config.rs` `resolve_credentials` |
| Hooks: sources, trust, events, decisions, env, HTTP | [user guide: hooks](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-pager/docs/user-guide/10-hooks.md) |
| Hook envelope, payloads, timeouts, discovery | `xai-grok-hooks/src/event.rs` `HookEventEnvelope`, `HookPayload`; `xai-grok-hooks/src/config.rs`; `xai-grok-hooks/src/discovery.rs` `registry_from_specs_deduped`; `xai-grok-shell/src/util/hooks.rs`; `xai-grok-config/src/loader.rs` `hook_config_layers_at` |
| Hook runners | `xai-grok-hooks/src/runner/mod.rs`, `runner/command.rs` `find_unresolved_env_vars`, `runner/http.rs` |
| Hook fire sites | `xai-grok-shell/src/session/acp_session_impl/` (`hook_dispatch.rs` `notification_hook_for_update`, `updates.rs`, `tool_calls.rs`, `turn.rs`, `stop_gate.rs`, `turn_end_hooks.rs`, `run_loop.rs` `fire_session_end_hooks`, `agent_ops.rs`); `xai-grok-shell/src/extensions/idle_prompt.rs` |
| Claude tool-name aliases | `xai-grok-tools/src/types/claude_alias.rs` |
| Headless flags, output formats, exit codes | [user guide: headless mode](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-pager/docs/user-guide/14-headless-mode.md); `xai-grok-shell/src/session/headless.rs` `stop_reason_wire`, `notification.rs` `project_result_usage` |
| ACP mode, extension catalog, `_meta` options | [user guide: agent mode](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-pager/docs/user-guide/15-agent-mode.md) |
| ACP client-registered hooks | `xai-grok-shell/src/extensions/hooks.rs` `ClientHookGroup`, `ClientHookResponse`; `xai-grok-shell/src/session/acp_session/hooks.rs` |
| ACP billing extension | `xai-grok-shell/src/extensions/billing.rs` |
| Subagents | [user guide: subagents](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-pager/docs/user-guide/16-subagents.md) |
| Sessions, resume, rewind, `grok usage` | [user guide: sessions](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-pager/docs/user-guide/17-sessions.md); `xai-grok-shell/src/session/persistence.rs` `Summary`, `session_dir_in`; `session/storage/mod.rs` `SessionUpdateEnvelope`; `session/signals.rs`; `session/usage_file.rs` `SessionUsageFile` |
| Session event log (`events.jsonl`) | `xai-grok-session-events/src/log.rs` `EventWriter`, `types.rs` `Event` |
| Permissions and permission modes | [user guide: permissions](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md) |
| Change history | the sync commit messages on [`main`](https://github.com/xai-org/grok-build/commits/main); npm [version times](https://registry.npmjs.org/@xai-official%2Fgrok) |

## Surface map

Each signal a stock-TUI integration needs has one upstream carrier. The sections below give the full shapes.

| Signal | Upstream surface |
| --- | --- |
| Session identity | `sessionId` on every hook envelope: a UUID, stable across resume, compaction, rewind, and model changes |
| Session start and end | `SessionStart` and `SessionEnd` hooks |
| Turn start | `UserPromptSubmit` hook |
| Turn end | exactly one of `Stop`, `StopFailure`, or `StopCancelled`, with the `idle_prompt` notification as the backstop for turns that report none |
| Tool activity | `PostToolUse` and `PostToolUseFailure` hooks |
| Awaiting the user | `Notification` hook (`permission_prompt`, `elicitation_dialog`); `events.jsonl` `permission_requested` / `permission_resolved` records |
| Compaction | `PreCompact` and `PostCompact` hooks |
| Subagents | `SubagentStart` from the parent session; `SubagentStop` inside the child session |
| Model, effort, title | `summary.json` |
| Live context fill | the last `_meta.totalTokens` in `updates.jsonl` |
| Completed-turn usage and cost | `turn_completed` records in `updates.jsonl`; `usage.json` via `grok usage` |
| Supervised run result | `--output-format streaming-json` terminal `end` or `error` record |
| Account identity | `auth.json` metadata |
| Billing windows | ACP `x.ai/billing`, or the TUI's `/usage` |

## Executable and launch modes

Official installs expose `grok`; a source build produces `xai-grok-pager`. `grok --version` prints `grok <version> (<build hash>)`, and `grok version --json` prints the same as JSON.

| Mode | Invocation | Surface |
| --- | --- | --- |
| Interactive TUI | `grok [flags] [PROMPT]` | Native terminal UI plus file hooks |
| Headless | `grok -p <prompt> --output-format streaming-json` | NDJSON result stream plus the same file hooks |
| ACP stdio | `grok agent stdio` | JSON-RPC over stdio |
| ACP WebSocket server | `grok agent serve [--bind <addr>] [--secret <secret>]` | JSON-RPC over WebSocket; `--bind` defaults to `127.0.0.1:2419`, and the secret is generated when omitted (`GROK_AGENT_SECRET`) |
| ACP over the relay | `grok agent headless --grok-ws-url <url>` | Headless agent connected through the Grok WebSocket relay |
| Shared leader | `grok agent --leader`, `grok agent leader` | Several clients share one backend over `~/.grok/leader.sock` |

These flags apply to the interactive and headless surfaces (`grok --help`, 1.0.30):

| Flag | Effect |
| --- | --- |
| `-p`, `--single <PROMPT>`; `--prompt-file <PATH>`; `--prompt-json <JSON>` | Select headless mode with a single-turn prompt |
| `-m`, `--model <MODEL>` | Model ID |
| `--reasoning-effort <EFFORT>` (alias `--effort`) | Reasoning effort |
| `--permission-mode <MODE>` | `default`, `acceptEdits`, `auto`, `dontAsk`, `bypassPermissions`, or `plan` |
| `--always-approve` (aliases `--yolo`, `--dangerously-skip-permissions`) | Auto-approve tool calls; same as `--permission-mode bypassPermissions` |
| `--allow <RULE>`, `--deny <RULE>` | Permission rules (compat aliases `--allowedTools`, `--disallowedTools`) |
| `--tools`, `--disallowed-tools` | Built-in tool allow and remove lists |
| `--sandbox <PROFILE>` | Sandbox profile (`GROK_SANDBOX`) |
| `--cwd <CWD>` | Working directory |
| `--max-turns <N>` | Maximum agent turns |
| `--no-plan`, `--no-subagents`, `--disable-web-search` | Disable plan mode, subagent spawning, or web tools |
| `-r`, `--resume [ID_OR_TITLE]`; `-c`, `--continue`; `--fork-session`; `-s`, `--session-id <UUID>` | Session selection (see [Sessions](#sessions)) |
| `-w`, `--worktree [NAME]`, `--worktree-ref <REF>` | Run the session in a new git worktree |
| `--output-format <FORMAT>` | `plain` (default), `json`, `streaming-json`, or `streaming-messages-json` |
| `--trust` | Grant folder trust for the project (accepted by the parser; not listed in `--help`) |

Subcommands include `login`, `logout`, `sessions` (`list`, `search`, `delete`), `usage`, `export`, `inspect [--json]`, `models`, `mcp`, `plugin`, `update [--check] [--json]`, and `version`.

## Sessions

Grok session IDs are UUIDs; Grok mints UUIDv7. `--session-id <uuid>` selects the ID of a **new** session and fails when the value is not a UUID or the ID already exists under the target session directory. With `--resume` or `--continue` it is valid only together with `--fork-session`, where it names the fork.

| Operation | TUI | Headless | Identity effect |
| --- | --- | --- | --- |
| New | launch, or `/new` (`/clear` alias) | default `grok -p`, or `--session-id <uuid>` | New ID |
| Resume | `/resume` | `--resume <id-or-title>`; bare `--resume` picks the most recent | Same ID |
| Continue latest | resume flow | `--continue` | Same ID, most recent session for the cwd |
| Fork | `/fork` | `--resume <id> --fork-session`, `--continue --fork-session` | New ID; `summary.json` records `parent_session_id` |
| Rewind | `/rewind` (`/undo` alias) | none | Same ID; truncates conversation history and leaves files on disk as they are |
| Compact | `/compact`, or the automatic threshold | automatic | Same ID |

`--resume` matches a non-UUID value against session titles for the current directory, ignoring case; a UUID-shaped value always means an ID. Among duplicate titles a sole manually renamed match wins, and otherwise the resume fails as ambiguous (`grok --help`).

The session directory is:

```text
${GROK_HOME:-~/.grok}/sessions/<URL-encoded-cwd>/<session-id>/
```

When the encoded cwd exceeds 255 bytes, Grok names the group with a slug plus a hash and records the original path in a `.cwd` file inside it (user guide: sessions, "Storage Layout"). A subagent's child session gets its own directory in the same tree; only its metadata nests under the parent (`subagents/<id>/meta.json`).

| File | Role |
| --- | --- |
| `summary.json` (+ `summary.json.lock`) | Index entry: identity, cwd, model, effort, title, timestamps, fork lineage ([below](#summaryjson)) |
| `updates.jsonl` | Authoritative ACP and xAI update stream that drives resume; the path hooks export as `transcriptPath` ([below](#updatesjsonl)) |
| `chat_history.jsonl` | Raw model-facing conversation |
| `events.jsonl` | Append-only session event log ([below](#eventsjsonl)) |
| `signals.json` | Session counters and context snapshot ([below](#signalsjson)) |
| `usage.json` | Persisted session and per-turn usage ([below](#usagejson-and-grok-usage)) |
| `plan.json`, `plan_mode.json` | TODO list and plan-mode state |
| `rewind_points.jsonl` | Rewind points |
| `feedback.jsonl`, `btw_history.jsonl` | Ratings, and `/btw` side questions |
| `system_prompt.txt`, `prompt_context.json`, `prompts/` | Rendered system prompt and its inputs |
| `compaction_checkpoints/`, `goal/`, `workflows/` | Compaction, goal-loop, and workflow state |
| `subagents/<id>/meta.json` | Child-session metadata |

## Native file hooks

A native hook is a command or HTTPS endpoint Grok calls at a lifecycle point with one JSON event. Four events read the hook's output: `PreToolUse` can deny or rewrite a tool call, `UserPromptSubmit` can block a prompt, `Stop` and `SubagentStop` can keep the agent working, and `PostToolUse` can send the model feedback or replace the tool output it sees. Every other event is passive and its stdout is ignored.

### Discovery and trust

Grok merges every enabled source; none replaces another (`xai-grok-shell/src/util/hooks.rs`, `xai-grok-config/src/loader.rs` `hook_config_layers_at`):

| Order | Source | Trust |
| --- | --- | --- |
| 1 | TOML config layers, highest authority first: `/etc/grok/requirements.toml`, `$GROK_HOME/requirements.toml`, `$GROK_HOME/config.toml`, `$GROK_HOME/managed_config.toml`, `/etc/grok/managed_config.toml` | Always |
| 2 | `$GROK_HOME/hooks/*.json`, plus absolute paths listed in `$GROK_HOME/hooks-paths` | Always |
| 3 | `~/.claude/settings.json` and `settings.local.json` (Claude compat) | Always |
| 4 | `~/.cursor/hooks.json` (Cursor compat) | Always |
| 5 | Project `.claude/settings.json` and `settings.local.json`, `<git-root>/.grok/hooks/*.json`, `.cursor/hooks.json` | Requires folder trust |
| 6 | Plugin hooks, then agent-definition inline hooks, appended after deduplication | Per plugin |

`$GROK_HOME` defaults to `~/.grok`. `[compat.<vendor>] hooks = false` in `config.toml` turns off a compatibility source. Deduplication keys on `(event, command_raw, url_raw, matcher)` with the canonical event name, so alias spellings collapse; the first copy wins unless a later copy comes from a higher-authority config layer (`discovery.rs` `registry_from_specs_deduped`). The registry is a snapshot: a new session sees disk edits, and the TUI's Hooks tab reload (`r`) refreshes the running session.

Project hooks run only in a trusted folder. Trust lives in `~/.grok/trusted_folders.toml`, is granted with `/hooks-trust` or `--trust`, and covers the folder's MCP and LSP servers, hooks, project instructions, project skills, and project permission rules together, including subdirectories of the same repository; a nested git checkout is a separate workspace. `GROK_FOLDER_TRUST=0` or `[folder_trust] enabled = false` ungates all of them (user guide: hooks, "Hook Locations"; permissions).

Claude and Cursor hook files load unchanged. Cursor's camelCase events map onto Grok's (`beforeShellExecution`, `beforeMCPExecution`, and `beforeReadFile` to `PreToolUse`; `afterShellExecution`, `afterMCPExecution`, `afterFileEdit`, `afterAgentResponse`, and `afterAgentThought` to `PostToolUse`; `beforeSubmitPrompt` to `UserPromptSubmit`). A helper registered in several of these files receives Grok's envelope from each of them; the file it was registered in does not identify the caller.

### Configuration shape

A JSON hook file carries a `hooks` object. Each event maps to ordered matcher groups, and each group holds ordered handlers:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "bin/safety-check.sh", "timeout": 10, "env": { "POLICY": "strict" } }
        ]
      }
    ]
  }
}
```

The TOML layers use the same structure as `[[hooks.<Event>]]` tables with an inner `hooks` array.

| Handler field | Meaning |
| --- | --- |
| `type` | `command` or `http` |
| `command` | Executable path relative to the hook file, or an inline shell command |
| `url` | HTTPS endpoint for `http` handlers |
| `timeout` | Seconds; the default depends on the event ([Timeouts](#decisions-exit-codes-and-timeouts)) |
| `env` | Extra environment for the handler; cannot override reserved variables |

Unrecognized event names are skipped, so a shared Claude or Cursor settings file still loads. `SubagentEnd` is accepted as an alias for `SubagentStop`.

### Matchers and tool names

A matcher is a regular expression over one field that depends on the event; an empty or omitted matcher matches everything (user guide: hooks, "Key Fields").

| Event | Matcher tests |
| --- | --- |
| `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionDenied` | Tool name |
| `Notification` | `notificationType` |
| `SubagentStart`, `SubagentStop` | Subagent type |
| `SessionStart` | `source` |
| `SessionEnd` | `reason` |
| `PreCompact`, `PostCompact` | Trigger (`manual` or `auto`) |
| `StopFailure` | `error` class |
| `StopCancelled` | `reason` |
| `Stop`, `UserPromptSubmit` | Nothing: a matcher is ignored with the warning `matcher on a {event} group is ignored (this event always fires)` |

MCP calls dispatched through `use_tool` appear as the qualified `server__tool` name. Claude-style names in a matcher also match their Grok tools, and the original spelling stays active (`xai-grok-tools/src/types/claude_alias.rs`):

| Alias | Grok tool |
| --- | --- |
| `Bash` | `run_terminal_command` |
| `Read` | `read_file`, `hashline_read` |
| `Edit`, `MultiEdit` | `search_replace`, `hashline_edit` |
| `Write` | `write`, `search_replace`, `hashline_edit` |
| `Grep` | `grep`, `hashline_grep` |
| `Glob`, `ListDir` | `list_dir` |
| `WebSearch` | `web_search` |
| `Task` | `spawn_subagent` |

The user guide's shorter alias list (`Read` to `read_file`; `Edit`, `Write`, and `MultiEdit` to `search_replace`) omits the hashline tools and `write`; the source table above is what dispatch uses. Hook payloads carry the tool's actual wire name.

### Common input envelope

Every event carries one camelCase envelope plus event-specific fields (`xai-grok-hooks/src/event.rs` `HookEventEnvelope`):

```json
{
  "hookEventName": "pre_tool_use",
  "hook_event_name": "PreToolUse",
  "sessionId": "019e0000-0000-7000-8000-000000000001",
  "cwd": "/workspace/project",
  "workspaceRoot": "/workspace/project",
  "timestamp": "2026-09-13T12:00:00Z",
  "transcriptPath": "/home/user/.grok/sessions/%2Fworkspace%2Fproject/019e0000-0000-7000-8000-000000000001/updates.jsonl",
  "promptId": "prompt-1",
  "permissionMode": "default",
  "toolName": "run_terminal_command",
  "toolUseId": "call-1",
  "toolInput": { "command": "cargo check" },
  "toolInputTruncated": false
}
```

| Field | Presence | Meaning |
| --- | --- | --- |
| `hookEventName` | always | Event name in snake_case (`pre_tool_use`, `stop_cancelled`) |
| `sessionId` | always | Session UUID of the session that fired the event |
| `cwd`, `workspaceRoot` | always | Working directory and workspace root |
| `timestamp` | always | RFC 3339 text, stamped at dispatch rather than when the turn ended |
| `transcriptPath` | optional | The session's `updates.jsonl`, present once that file exists |
| `promptId` | optional | The turn the event belongs to; absent for session-scoped events such as `SessionStart`, the `idle_prompt` notification, and the session-end `Stop` |
| `permissionMode` | optional | `default`, `auto`, `plan`, or `bypassPermissions`; the CLI's `acceptEdits` and `dontAsk` have no envelope label |
| `clientIdentifier` | optional | Defined in the envelope; the stock shell leaves it unset |

Grok also adds snake_case aliases for Claude compatibility; the camelCase keys are authoritative: `hook_event_name` (PascalCase value such as `"Stop"`), `session_id`, `transcript_path`, `permission_mode`, `tool_name`, `tool_input`, `tool_use_id`, `tool_response` (a copy of `toolResult`), `duration_ms`, and `is_interrupt` (`acp_session/hooks.rs` `make_hook_envelope`). Hook configuration keys are PascalCase, so one integration meets three conventions.

`toolInput` and `toolResult` are each capped at 128 KiB of serialized JSON; an oversized value arrives as a truncated string with its `*Truncated` flag true. Free-text fields are clipped in place with a `… [+N chars]` marker: `lastAssistantMessage` at 32,768 characters, `errorDetails` and `reasonDetails` at 1,000.

### Event catalog

| Event | Fires | Event-specific fields |
| --- | --- | --- |
| `SessionStart` | A root session starts; never for a subagent's own session | `source`: `new` for an empty history, `load` otherwise |
| `UserPromptSubmit` | A prompt enters the turn | `prompt`, `subagentType` inside a child |
| `PreToolUse` | Before a tool runs | `toolName`, `toolUseId`, `toolInput`, `toolInputTruncated`, `subagentType` inside a child |
| `PostToolUse` | A tool ran, including a built-in logical error such as a non-zero shell exit | pre-tool fields plus `toolResult`, `toolResultTruncated`, `durationMs`, `isBackgrounded` |
| `PostToolUseFailure` | A tool failed to dispatch, or an MCP tool returned an error result | tool fields plus `error`, `durationMs`, `isInterrupt` |
| `PermissionDenied` | The permission system denied a tool call | tool name, ID, input, truncation flag |
| `Notification` | A user-attention or agent-error notification | `notificationType`, `message`, `level`; `title` is defined and left unset ([Notifications](#notifications)) |
| `Stop` | A turn completed, or the session is tearing down | `reason`, `stopHookActive`, `lastAssistantMessage`, `backgroundTasks`, `sessionCrons` |
| `StopFailure` | A turn ended on an API error; replaces `Stop` | `error`, `errorDetails`, `lastAssistantMessage`, `subagentType` |
| `StopCancelled` | A turn ended without completing; replaces `Stop` | `reason`, `cancelledBy`, `cancelTrigger`, `reasonDetails`, `lastAssistantMessage`, `subagentType` |
| `SubagentStart` | The parent spawns a child | `subagentId`, `subagentType`, `description` |
| `SubagentStop` | A child's turn ends, inside the child | `phase` (`"gate"`), `subagentId`, `subagentType`, `stopHookActive`, `lastAssistantMessage` |
| `PreCompact`, `PostCompact` | Compaction begins, or succeeds | `source`: `manual` or `auto` |
| `SessionEnd` | The session actor terminates | `reason`: `channel_closed` or `shutdown`; `turnCount`, `toolCallCount`; `subagentType` for a child session |

The user guide lists `startup` and `resume` as example `SessionStart` matcher values, but the fire site sends only `new` and `load` (`acp_session_impl/agent_ops.rs`, `run_loop.rs`). `load` covers both resume and fork. `SessionStart` carries no `modelId` or `agentType`; `summary.json` holds the model and agent name.

### Turn-end reports

A turn that runs the model reports at most one of three events: `Stop` with `reason: "end_turn"` for a completion, `StopFailure` for an API error, or `StopCancelled` for any other non-completion (`turn_end_hooks.rs`, `turn_end.rs`). No `Stop` with `reason` `cancelled` or `error` is sent.

| `StopFailure.error` | Cause |
| --- | --- |
| `rate_limit` | HTTP 429, and capacity errors 503 and 529 |
| `authentication_failed` | HTTP 401 |
| `invalid_request` | Other 4xx |
| `server_error` | Other 5xx |
| `max_output_tokens` | Output limit reached |
| `unknown` | Anything the runtime cannot classify |

| `StopCancelled.reason` | `cancelledBy` | Cause |
| --- | --- | --- |
| `user_interrupt` | `user` | Ctrl+C, a client stop button, or ACP `session/cancel` |
| `permission_rejected` | `user` | The user declined a tool call |
| `permission_cancelled` | `user` | The user dismissed the permission prompt |
| `max_turns` | `runtime` | `--max-turns` reached |
| `no_progress` | `runtime` | The agent bailed out after repeated no-op rounds |
| `unknown` | `unknown` | Unclassified; treat any unrecognized value the same way |

`cancelTrigger` names the client gesture when one was given (the bundled pager sends `ctrl_c`, `mouse`, or `dashboard_stop`), clipped at 64 characters. For a declined tool call `reasonDetails` is `<tool>: <why>`.

The reports have ordering and coverage limits a consumer must handle (user guide: hooks, "Stop Decision Control"):

- A cancelled turn's report is dispatched off the session command loop, so it can arrive after the next turn's `UserPromptSubmit`; correlate by `promptId`, not by arrival order or `timestamp`.
- When a `Stop` gate blocks, `Stop` fires again for each continuation round with `stopHookActive: true`; a passive observer cannot tell a continuation fire from the final one. After 8 continuations Grok ends the turn without consulting hooks.
- An interrupt while a `Stop` hook runs kills it, and the turn then reports `StopCancelled`, so one turn can show a `Stop` followed by `StopCancelled`.
- Some turns report none of the three: bash-mode (`!`) and builtin slash commands that complete; cancel-and-send, rewind, or a queued prompt removed before it ran; a turn superseded while its report was being built; a turn whose stop hooks were all disabled, untrusted, or failed; and reports still queued at teardown.
- An interrupted bash-mode command reports `StopCancelled(user_interrupt)` with no preceding `UserPromptSubmit`.
- Inside a child session, `user_interrupt` does not fire `StopCancelled`; the child's own `max_turns`, `no_progress`, or declined permission does.

The `idle_prompt` notification is the backstop for these gaps: it fires 60 seconds after a turn ends (any outcome), needs one completed turn, and a new turn cancels it.

Session teardown runs in a fixed order (`run_loop.rs` `fire_session_end_hooks`, `stop_gate.rs`): `SessionEnd` dispatches first, then a `Stop` with the same `reason` (`channel_closed` or `shutdown`) and `stopHookActive: false` runs within a 5-second budget with any block ignored. The trailing `Stop` is skipped for subagent sessions. Teardown gives queued turn-end reports half a second, and each `SessionEnd` hook is bounded by `GROK_SESSION_END_HOOKS_TIMEOUT_MS` (default 1,500, maximum 60,000).

`Stop.backgroundTasks` entries carry `id`, `type` (`shell`, `monitor`, or `subagent`), `status`, and `command`, `description`, or `agentType` by type; `sessionCrons` entries carry `id`, `schedule` (a human-readable interval), `recurring`, and `prompt`. Both arrays are empty when nothing is in flight.

### Subagent events

`SubagentStart` and `SubagentStop` fire from different sessions (`acp_session_impl/updates.rs`, `stop_gate.rs` `build_stop_payload`):

| Event | Fired by | Envelope `sessionId` | `subagentId` |
| --- | --- | --- | --- |
| `SubagentStart` | Parent session | Parent's | The child |
| `SubagentStop` | Child session, as its stop gate | Child's | The child's own ID |

The parent fires no hook when a child finishes, and `SubagentStop` carries no exit code or duration. A child session fires its own `UserPromptSubmit`, tool, `StopFailure`, `StopCancelled`, and `SessionEnd` events, each with `subagentType` set; the same events omit `subagentType` in a root session. An agent definition's `Stop` hooks are remapped to `SubagentStop`.

### Notifications

`Notification` carries `notificationType`, `message`, and `level` (`hook_dispatch.rs` `notification_hook_for_update`, `extensions/idle_prompt.rs`, `tools/notification_bridge.rs`). The `message` strings below are the literals at the source commit; the user guide calls them display text that can change between releases.

| `notificationType` | `message` | Fires |
| --- | --- | --- |
| `permission_prompt` | `Tool permission requested` | A tool permission prompt is about to be shown to the user |
| `permission_prompt` | `Plan approval requested` | Plan approval is requested |
| `permission_prompt` | `Diff review requested` | Diff review is requested |
| `elicitation_dialog` | `User question requested` | The agent asks the user a question |
| `agent_error` | the error text (`level: "error"`) | An agent error is raised |
| `idle_prompt` | `Waiting for your next prompt` | 60 s after a turn ends; `GROK_IDLE_NOTIFICATION_DELAY_MS` overrides the delay |
| `task_complete` | `Background task completed: {task_id}` | A background task finishes |

`Tool permission requested` fires only when a real prompt will be shown (`xai-grok-workspace/src/permission/manager/mod.rs`); a call that always-approve, auto mode, or a saved grant approves raises none. `PermissionDenied` reports a completed denial and does not indicate a pending prompt.

### Decisions, exit codes, and timeouts

The events that read output accept these stdout JSON shapes (`xai-grok-hooks/src/runner/mod.rs`; user guide: hooks):

| Event | Output |
| --- | --- |
| `PreToolUse` | `{"decision": "allow" \| "deny" \| "ask" \| "defer", "reason": "…"}`, or `hookSpecificOutput.permissionDecision` with `permissionDecisionReason`, which wins when present; legacy `approve` and `block` are accepted. `hookSpecificOutput.updatedInput` rewrites the tool input; `hookSpecificOutput.additionalContext` adds a note for the model |
| `UserPromptSubmit` | `{"decision": "block", "reason": "…"}` rejects the prompt |
| `Stop`, `SubagentStop` | `{"decision": "block", "reason": "…"}` or `hookSpecificOutput.additionalContext` keeps the agent working; `{"continue": false, "stopReason": "…"}` forces the stop |
| `PostToolUse` | `{"decision": "block", "reason": "…"}` and `hookSpecificOutput.additionalContext` reach the model beside the result; `hookSpecificOutput.updatedToolOutput` (or MCP-only `updatedMCPToolOutput`) replaces the model's copy of the result |

An unknown `PreToolUse` decision token is a hook failure. `allow` means only "not blocked" and does not skip a permission prompt; `ask` forces the prompt; `defer` leaves the call to the normal permission flow.

| Exit | Result |
| --- | --- |
| `0` | Parse stdout JSON when present; no output allows |
| `2` | `PreToolUse`: deny, with the first stderr line as the reason when JSON gives none. `UserPromptSubmit`: block, with stderr as the message. `Stop`, `SubagentStop`: block, with the full stderr as feedback. `PostToolUse`: feed stderr to the model |
| other | Record a failure and fail open; a `PreToolUse` `deny` in stdout JSON still applies, and a valid `Stop` decision JSON wins over the exit code |

Timeouts, crashes, malformed output, and unset required variables fail open. A hook that exits non-zero keeps its deny or block but loses `updatedInput`, `additionalContext`, and output replacements.

| Events | Default `timeout` |
| --- | --- |
| `Stop`, `SubagentStop`, `PostToolUse` | 600 s |
| `UserPromptSubmit` | 30 s |
| All others, including `PreToolUse` | 5 s |

The defaults live in `xai-grok-hooks/src/config.rs`; a handler's `timeout` overrides them, and `SessionEnd` hooks are further bounded by `GROK_SESSION_END_HOOKS_TIMEOUT_MS`.

### Command runner and environment

A command containing a space, any of `|&;><$`, or a leading `~` runs through `sh -c`; other commands execute directly. The hook runs with the workspace root as its cwd, in its own process group, and captured stdout and stderr are each capped at 1 MiB with a ` [truncated]` suffix (`runner/command.rs`).

| Variable | Set for |
| --- | --- |
| `GROK_HOOK_EVENT` | Every hook: the snake_case event name |
| `GROK_HOOK_NAME` | Every hook: the configured hook name, plugin-prefixed for plugin hooks |
| `GROK_SESSION_ID` | Every hook |
| `GROK_WORKSPACE_ROOT` | Every hook |
| `CLAUDE_PROJECT_DIR` | Every hook: alias for `GROK_WORKSPACE_ROOT` |
| `GROK_PLUGIN_ROOT`, `GROK_PLUGIN_DATA` (plus Claude aliases) | Plugin hooks |

These names are reserved: an `env` entry for one is stripped at load with a warning.

`command` and `url` fields expand `$VAR` and `${VAR}`. Before spawning a shell command, the runner rejects any plain reference it cannot resolve from the reserved variables, the handler `env`, Grok's process environment, or a variable assigned or `read` inside the command itself. The hook then fails before spawn with `hook not executed: required env var(s) not set: {list}` (`runner/command.rs` `find_unresolved_env_vars`). References with a shell modifier such as `${VAR:-default}` are exempt. Shell parameters that are not exported variables, such as `$PPID`, are therefore rejected.

### HTTP hooks

An `http` handler POSTs the same envelope as JSON and reads a decision from the JSON response body. The URL must be HTTPS. The runner rejects private, link-local, and cloud-metadata addresses, including targets reached through a redirect, and permits loopback (`runner/http.rs`). Transport failures and invalid responses fail open.

## Session files

The files below are durable per-session state. `summary.json`, `signals.json`, and `usage.json` are replaced whole; `updates.jsonl` and `events.jsonl` are append-only, and a crash can leave a torn final line.

### `summary.json`

`summary.json` is the snake_case `Summary` struct (`persistence.rs`). A writer takes an fs2 lock on `summary.json.lock`, applies a read-modify-write, and publishes atomically with temp file, fsync, rename, and directory fsync (`summary_write.rs` `apply_patch_locked`).

| Field | Meaning |
| --- | --- |
| `info.id`, `info.cwd` | Session ID and working directory |
| `current_model_id` | Current model, including mid-session switches |
| `reasoning_effort` | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`, when set |
| `agent_name` | Agent definition active at the last save |
| `created_at`, `updated_at`, `last_active_at` | Creation, metadata-update, and activity clocks |
| `generated_title`, `title_is_manual`, `session_summary` | Title and summary; a manual `/rename` sets `title_is_manual` |
| `last_turn_summary`, `last_recap` | Short summary of the latest turn, and a bounded recap preview |
| `parent_session_id` | Source session of a fork or restore |
| `sandbox_profile` | Effective sandbox profile |
| `num_messages`, `num_chat_messages` | Update and chat-message counts |

### `updates.jsonl`

Each line is one `SessionUpdateEnvelope` (`session/storage/mod.rs`):

```json
{
  "timestamp": 1789300000,
  "method": "session/update",
  "params": {
    "sessionId": "019e0000-0000-7000-8000-000000000001",
    "update": {
      "sessionUpdate": "agent_message_chunk",
      "content": { "type": "text", "text": "Done" }
    },
    "_meta": {
      "totalTokens": 42000,
      "eventId": "019e0000-0000-7000-8000-000000000001-42",
      "agentTimestampMs": 1789300000123,
      "promptId": "prompt-1",
      "streamStartMs": 1789299999000,
      "turnStartMs": 1789299998500
    }
  }
}
```

| Field | Meaning |
| --- | --- |
| `timestamp` | Unix seconds (integer); nested `*Ms` clocks are Unix milliseconds |
| `method` | `session/update` for standard ACP updates; `_x.ai/session/update` for Grok extensions |
| `params.update.sessionUpdate` | Update variant: `user_message_chunk`, `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`, `plan`, and an open-ended extension set |
| `_meta.totalTokens` | Estimated active-context size at that update; not cumulative usage |
| `_meta.eventId`, `_meta.agentTimestampMs` | Monotonic event ID and agent clock |
| `_meta.promptId`, `streamStartMs`, `turnStartMs`, `updateType`, `updateParams`, `chunkId`, `isReplay` | Conditional |

Ordinary ACP updates carry `totalTokens`, `eventId`, and `agentTimestampMs`; extension updates carry an event ID but not necessarily `totalTokens`. `user_message_chunk` carries `_meta.promptIndex` on the update.

The extension update `rewind_marker` carries `target_prompt_index`. It rewinds the logical branch to that prompt boundary while the earlier lines stay in the file, so a reader that folds the stream must drop messages, token samples, and completed turns past that boundary before applying later lines.

A completed turn writes an `_x.ai/session/update` whose update is `turn_completed` (`notification.rs`):

```json
{
  "timestamp": 1789300001,
  "method": "_x.ai/session/update",
  "params": {
    "sessionId": "019e0000-0000-7000-8000-000000000001",
    "update": {
      "sessionUpdate": "turn_completed",
      "prompt_id": "prompt-1",
      "stop_reason": "end_turn",
      "agent_result": "Done",
      "usage": {
        "inputTokens": 51000,
        "cachedReadTokens": 41000,
        "cacheCreationTokens": 0,
        "outputTokens": 1893,
        "reasoningTokens": 412,
        "totalTokens": 52893,
        "modelCalls": 7,
        "apiDurationMs": 47000,
        "costUsdTicks": 126890500,
        "costIsPartial": false,
        "usageIsIncomplete": false,
        "numTurns": 7,
        "modelUsage": {
          "grok-4.5": {
            "inputTokens": 51000,
            "cachedReadTokens": 41000,
            "outputTokens": 1893,
            "reasoningTokens": 412,
            "costUsdTicks": 126890500,
            "costIsPartial": false
          }
        }
      }
    }
  }
}
```

| Field | Meaning |
| --- | --- |
| `prompt_id`, `stop_reason` | Turn identity and stop reason |
| `agent_result`, `error_kind`, `elapsed_ms` | Optional final text, error class, and wall time |
| `usage.inputTokens` | Full prompt input, including `cachedReadTokens` |
| `usage.cachedReadTokens`, `usage.cacheCreationTokens` | Cache-read and cache-creation input |
| `usage.outputTokens` | Output, already including `reasoningTokens` |
| `usage.costUsdTicks` | Optional cost at 10,000,000,000 ticks per USD |
| `usage.costIsPartial`, `usage.usageIsIncomplete` | Some calls lacked cost, or subagent usage could not be fully applied |
| `usage.modelUsage` | Per-model token and cost rows; rows can leave a residual against the aggregate |

An absent `costUsdTicks` means unreported, not free.

### `events.jsonl`

`events.jsonl` is an append-only session event log written by `EventWriter` (`xai-grok-session-events/src/log.rs`, `types.rs` `Event`). Each line is `{"ts": "<RFC 3339 ms>", "type": "<event>", ...}` with snake_case fields. Its records include `turn_started` (with `session_id`, `turn_number`, `model_id`, and `schema_version`), `tool_completed`, and `turn_ended`. The permission pair brackets a prompt:

```json
{"ts":"2026-09-13T04:21:38.748Z","type":"permission_requested","tool_name":"run_terminal_command"}
{"ts":"2026-09-13T04:21:40.816Z","type":"permission_resolved","tool_name":"run_terminal_command","decision":"allow","wait_ms":2067}
```

`decision` is `allow`, `deny`, `cancelled`, or `followup`. Neither record carries a request ID, so concurrent prompts for the same tool pair only by order.

### `signals.json`

`signals.json` serializes `SessionSignals` in camelCase (`session/signals.rs`). It is written at turn boundaries and lags a running turn.

| Field | Meaning |
| --- | --- |
| `turnCount`, `assistantMessageCount` | Turn and message counters |
| `errorCount`, `toolFailureCount`, `cancellationCount` | Error and interruption counters |
| `compactionCount`, `totalTokensBeforeCompaction` | Compaction history |
| `contextTokensUsed`, `contextWindowTokens`, `contextWindowUsage` | Context snapshot and the model's context window |
| `toolCallCount`, `toolsUsed` | Tool activity |
| `modelsUsed`, `primaryModelId` | Model history |
| `sessionDurationSeconds` | Session duration at the last write |

The file carries no spend amount.

### `usage.json` and `grok usage`

`grok usage <session-id> [turn]` prints the persisted `SessionUsageFile` as JSON (`xai-grok-pager/src/usage_cmd.rs`, `session/usage_file.rs`). The user guide recommends it over reading session files.

| Field | Meaning |
| --- | --- |
| `sessionId`, `updatedAt` | Session and last write |
| `session` | Whole-conversation totals, including history inherited by resume or fork |
| `turns[]` | One row per recorded turn; with `[turn]`, only that row |

`session` and each turn row carry `inputTokens`, `outputTokens`, `cachedReadTokens`, `cacheCreationTokens`, `reasoningTokens`, `totalTokens`, `modelCalls`, and optional `costUsdTicks`, `costIsPartial`, `usageIsIncomplete`, `turnCount`, `primaryModelId`, and `modelUsage`; a turn row adds `turnNumber` and optional `endedAt`. Errors print `Session '{id}' not found.`, `No usage recorded for session '{id}'.`, or `Turn {n} not found in session '{id}'.`

## Headless structured output

`-p`, `--prompt-file`, or `--prompt-json` selects headless mode, and the same file hooks run inside it. `--output-format` chooses the stream:

| Format | Output |
| --- | --- |
| `plain` | Human text |
| `json` | One object after the response: `text`, `stopReason`, `sessionId`, `requestId`, optional `thought`, and spend fields |
| `streaming-json` | NDJSON, one `type`-tagged object per line, derived from ACP session updates |
| `streaming-messages-json` | NDJSON in the Anthropic Messages API `stream-json` format (`system`/`init`, `assistant`, `user`, `result`); `--include-partial-messages` adds `stream_event` framing |

A `streaming-json` run looks like this:

```json
{"type":"thought","data":"Checking the tests"}
{"type":"tool_call","toolCallId":"call_1","title":"Read","kind":"read","status":"in_progress","toolName":"read_file","rawInput":{"path":"src/main.rs"},"content":[],"locations":[]}
{"type":"tool_call_update","toolCallId":"call_1","status":"completed","content":[],"rawOutput":{"lines":42},"locations":[]}
{"type":"text","data":"Fixed"}
{"type":"usage","messageId":"resp_1","stopReason":"end_turn","usage":{"input_tokens":812,"output_tokens":45,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"reasoning_tokens":0},"signature":"..."}
{"type":"end","stopReason":"end_turn","sessionId":"019e0000-0000-7000-8000-000000000001","requestId":"req-1","num_turns":7,"usage":{"input_tokens":7210,"cache_read_input_tokens":41000,"cache_creation_input_tokens":0,"output_tokens":1893,"reasoning_tokens":412,"total_tokens":50103},"modelUsage":{"grok-4.5":{"inputTokens":7210,"outputTokens":1893,"cacheReadInputTokens":41000,"modelCalls":7,"costUSD":0.01268905}},"total_cost_usd":0.01268905,"total_cost_usd_ticks":126890500}
```

| `type` | Meaning |
| --- | --- |
| `text`, `thought` | Response and reasoning chunks (`data`) |
| `tool_call`, `tool_call_update` | Tool start and progress, with ACP field names plus `toolName` |
| `usage` | One per model response: `messageId`, the provider's verbatim `stopReason` (such as `tool_use`), `usage`, `signature` |
| `plan`, `available_commands` | Current plan entries; tool and slash-command lists |
| `end` | Always the last record on success: `stopReason`, `sessionId`, `requestId`, and spend fields |
| `error` | Failure: `message`, plus spend fields when usage was recorded |

The catalog also includes `max_turns_reached` and `auto_compact_*` records and is non-exhaustive. `end.stopReason` is snake_case: `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, or `cancelled` (`headless.rs` `stop_reason_wire`).

Spend fields on `json`, `end`, and `error` follow one policy (`notification.rs` `project_result_usage`):

- `usage.input_tokens` and `modelUsage.*.inputTokens` are uncached input; `cache_read_input_tokens` and `cache_creation_input_tokens` are the cache buckets. This differs from `updates.jsonl`, where `inputTokens` includes cache reads.
- `total_tokens = input_tokens + cache_read_input_tokens + cache_creation_input_tokens + output_tokens`; `reasoning_tokens` is part of output.
- `usage` covers the prompt, including subagents that finished before turn end; compaction and other side-model calls are excluded.
- `num_turns` counts main-agent model rounds; subagent calls count only in `modelUsage.*.modelCalls`.
- `total_cost_usd` and `total_cost_usd_ticks` (10,000,000,000 ticks per USD) appear only for a complete cost. When `cost_is_partial` or `usage_is_incomplete` is true, every cost float is omitted, including `modelUsage.*.costUSD`.
- A prompt that never reached the model omits the spend fields.

The user guide documents exit codes 0 on success, 1 on error, 130 on SIGINT, and 143 on SIGTERM. In source, errors, max turns, and a closed connection return to `main`, which prints `Error: {e:#}` and exits 1 (`xai-grok-pager-bin/src/main.rs`); the signal codes were not traced to a handler at the source commit.

## Agent Client Protocol

ACP is the structured embedding mode: JSON-RPC 2.0 with the standard `initialize`, `session/new`, `session/load`, `session/prompt`, `session/update`, and `session/cancel` cycle. Grok also serves the standard `session/list`, `session/resume`, `session/close`, and `session/set_config_option` methods, and `session/new` and `session/load` responses carry a typed `configOptions` list with `model` and `reasoning_effort` (user guide: agent mode, "Session config options").

`session/new` accepts these `_meta` options:

| Field | Effect |
| --- | --- |
| `rules` | Extra rules appended to the system prompt |
| `systemPromptOverride` | Replacement system prompt |
| `agentProfile` | Agent profile name or JSON object |
| `yoloMode` | Always-approve for the session |
| `autoMode` | Auto permission mode, superseded by `yoloMode` |
| `x.ai/hooks` | Client-registered hooks ([below](#client-registered-hooks)) |

Grok extension methods live under `x.ai/` and the set is non-exhaustive; a client discovers it from the `initialize` response.

| Category | Methods |
| --- | --- |
| Interaction reverse requests | `x.ai/ask_user_question`, `x.ai/exit_plan_mode`, plus standard permission requests |
| Hooks | `x.ai/hooks/run`, `x.ai/hooks/event` |
| Session | `x.ai/session/fork`, `x.ai/session/state`, `x.ai/session/import`, `x.ai/session/resolve_local_for_worktree_resume` |
| Conversation | `x.ai/prompt_history`, `x.ai/rewind/*`, `x.ai/compact_conversation`, `x.ai/btw` (side question that does not interrupt the turn) |
| Account | `x.ai/auth/*`, `x.ai/auth/info`, `x.ai/billing` |
| Workspace | `x.ai/fs/*`, `x.ai/git/*`, `x.ai/git/worktree/*`, `x.ai/search/*`, `x.ai/terminal/*` |
| Notifications to the client | `x.ai/session/update`, `x.ai/session_notification` (diff review, retry state, auto-compact), `x.ai/fs_notify`, `x.ai/fs/index`, `x.ai/fs/index/delta`, `x.ai/search/fuzzy/status`, `x.ai/git/worktree/status` |

### Client-registered hooks

An ACP client registers callbacks in `session/new` metadata (`extensions/hooks.rs` `ClientHookGroup`):

```json
{
  "_meta": {
    "x.ai/hooks": {
      "PreToolUse": [
        { "matcher": "Bash", "hookCallbackIds": ["policy-pre-tool"], "timeout": 5 }
      ],
      "Stop": [
        { "hookCallbackIds": ["turn-ended"] }
      ]
    }
  }
}
```

`PreToolUse`, `Stop`, `SubagentStop`, and `PostToolUse` send one awaited `x.ai/hooks/run` request per unique matching callback; every other event, `UserPromptSubmit` included, sends a fire-and-forget `x.ai/hooks/event` notification. The params are the flattened hook envelope plus `hookCallbackId`. `initialize` advertises `_meta["x.ai/hooks"] = {"blockingEvents": ["PreToolUse", "Stop", "SubagentStop"], "decisions": ["deny", "block"], "stopSignals": ["continue", "stopReason", "additionalContext"]}`; `PostToolUse` gating is not yet advertised.

A blocking response looks like this:

```json
{ "decision": "deny", "systemMessage": "Policy blocked this call" }
```

| Response field | Meaning |
| --- | --- |
| `decision` | `continue` (default), `deny` (alias `block`), or `ask`; `ask`, unknown values, and malformed replies fail open |
| `systemMessage` (alias `reason`) | Message shown with a deny or block |
| `continue`, `stopReason` | Force a `Stop` gate to end the turn |
| `additionalContext` | Note for the model |

`timeout` is in seconds. Zero or a non-finite value falls back to the default of 30 seconds, or 600 seconds for `Stop`, `SubagentStop`, and `PostToolUse`, and values are capped at 600. Timeouts and transport errors fail open. Client hooks supplement file hooks. A reconnect updates registrations only when `x.ai/hooks` is present, and an empty object clears them.

## Authentication and billing

`grok login` signs in through xAI OAuth at `auth.x.ai` by default; `--device-auth` (alias `--device-code`) selects the device-code flow. `grok logout` clears cached credentials. Grok hot-reloads `auth.json`. Enterprise login uses an external auth-provider command (`auth_provider_command`) or OIDC (user guide: authentication).

`auth.json` lives at `GROK_AUTH_PATH` when that is set and non-empty, otherwise `$GROK_HOME/auth.json`, and is written with mode `0600` (`xai-grok-login/src/storage.rs` `auth_json_path`). It is a map from scope to one `GrokAuth` record; a stored API key uses scope `xai::api_key` (`xai-grok-login/src/model.rs`).

| Field | Meaning |
| --- | --- |
| `auth_mode` | `web_login` (alias `grok`), `oidc`, `external`, or `api_key` |
| `create_time`, `expires_at` | Record creation and token expiry; a token without a server expiry gets a 30-day lifetime |
| `user_id`, `email`, `first_name`, `last_name`, `profile_image_asset_id` | User identity |
| `principal_type`, `principal_id` | Principal |
| `team_id`, `team_name`, `team_role`, `organization_id`, `organization_name`, `organization_role` | Team and organization |
| `user_blocked_reason`, `team_blocked_reasons` | Blocked-account state |
| `coding_data_retention_opt_out`, `has_grok_code_access` | Account flags |
| `oidc_issuer`, `oidc_client_id` | OIDC login origin |
| `key`, `refresh_token` | Secrets |

Credentials resolve per request in this order (`xai-grok-shell/src/agent/config.rs` `resolve_credentials`):

1. A per-model `api_key` or `env_key` under `[model.<name>]` in `config.toml`.
2. A cached auth-provider token.
3. The session token from `auth.json`, only when the model's base URL may receive it.
4. `XAI_API_KEY`, or the legacy `GROK_CODE_XAI_API_KEY` (`xai-grok-login/src/auth_method.rs`).

API-key resolution is off under `disable_api_key_auth` or `GROK_DISABLE_API_KEY_AUTH`, and when `[auth] preferred_method = "oidc"`.

In ACP mode `x.ai/auth/info` returns identity metadata, and `x.ai/billing` queries the Grok service for credit usage percent, the current billing period, the on-demand cap and usage, prepaid balance, unified-billing state, and subscription tier (`extensions/billing.rs`). The TUI shows credit and billing on `/usage`. No CLI subcommand prints billing windows; `grok usage` covers token and cost totals only.

## Upstream scope

The GitHub mirror ships the harness and TUI source without release tags, so a source claim can be pinned only to the sync commit nearest a release, and the shipped binary can run slightly ahead of it. The installer's `alpha` channel serves builds ahead of `stable` (1.0.31 on 2026-09-13), and `grok update --alpha` or `--stable` switches channel.

Surfaces on this page that a stock-TUI integration can reach but that are not needed to observe a session include the blocking hook decisions, the `streaming-messages-json` format, ACP reverse requests with typed answers, `x.ai/billing`, and `usage.json`. Which surfaces RimZ wires, and the gaps that follow, are in [adapter_grok.md → Known gaps](../../internals/agents/adapter_grok.md#known-gaps).

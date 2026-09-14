# Cursor CLI protocol reference

> This page records the upstream Cursor CLI surface. RimZ's mapping of it, and the gaps RimZ records, are in [adapter_cursor.md](../../internals/agents/adapter_cursor.md); the provider-neutral lifecycle contract is [model.md](../../internals/agents/model.md), and the account and spend contract is [providers.md](../../internals/agents/providers.md).

The Cursor CLI is Cursor's terminal coding agent. This page covers what a contributor binds to: the executable and its updates, session identity, command hooks and their decision channel, the command status line, transcripts and local chat state, headless print mode, the ACP server, authentication, usage and pricing, and CLI configuration, modes, and permissions. It makes no claim about what RimZ supports.

**Refresh baseline.** Cursor CLI build `2026.09.10-fd3934a`, released 2026-09-10: the build the official installer at <https://cursor.com/install> pins (it downloads `https://downloads.cursor.com/lab/2026.09.10-fd3934a/<os>/<arch>/agent-cli-package.tar.gz`). Docs, changelog, and the installed build were read on 2026-09-13. Cursor publishes no source, tags, or commits, so the build token is the only identifier. Wire claims the docs leave out are read from the installed JavaScript bundle under `~/.local/share/cursor-agent/versions/2026.09.10-fd3934a/`, cited as a bundle module name (for example `../hooks/dist/index.js`) and, when the module sits outside `index.js`, the chunk file that carries it.

**Inline pins.** Some facts come from live sessions captured against build `2026.07.09-a3815c0` and were not re-captured on the baseline, because running a session is outside a refresh pass. Each is marked "captured on 2026.07.09" at the claim: `/clear` hook firing, the subagent hook probe, hook firing under `-p`, transcript row shapes and the resume rewrite, the pending `AskQuestion` and child chat records, the status line model display, and the `status`/`about` JSON shapes.

## Upstream sources

The docs are rolling and unversioned. Each page also serves as Markdown at the `.md` URL below.

| Surface | Official source |
| --- | --- |
| CLI overview, sessions, worktrees | <https://cursor.com/docs/cli/overview.md> · <https://cursor.com/docs/cli/using.md> |
| CLI changelog | <https://cursor.com/docs/cli/changelog.md> |
| Installation and updates | <https://cursor.com/docs/cli/installation.md> |
| Commands and launch options | <https://cursor.com/docs/cli/reference/parameters.md> |
| Slash commands | <https://cursor.com/docs/cli/reference/slash-commands.md> |
| Hooks | <https://cursor.com/docs/hooks.md> |
| Claude Code hook compatibility | <https://cursor.com/docs/reference/third-party-hooks.md> |
| Headless mode and output formats | <https://cursor.com/docs/cli/headless.md> · <https://cursor.com/docs/cli/reference/output-format.md> |
| ACP server | <https://cursor.com/docs/cli/acp.md> · <https://agentclientprotocol.com/> |
| Authentication | <https://cursor.com/docs/cli/reference/authentication.md> |
| Configuration and permissions | <https://cursor.com/docs/cli/reference/configuration.md> · <https://cursor.com/docs/cli/reference/permissions.md> |
| Subagents | <https://cursor.com/docs/subagents.md> |
| Run modes, Auto-review, sandboxing | <https://cursor.com/docs/agent/security/run-modes.md> · <https://cursor.com/docs/agent/security.md> |
| Models and pricing | <https://cursor.com/docs/models-and-pricing.md> |
| Team Admin API | <https://cursor.com/docs/account/teams/admin-api.md> |

These installed-binary commands are read-only. Pass `--disable-auto-update` (see [Executable, version, and updates](#executable-version-and-updates)) so collecting evidence never installs a build.

```sh
agent --version
agent --disable-auto-update --help
agent --disable-auto-update <command> --help
agent --disable-auto-update status --format json
agent --disable-auto-update about --format json
agent --disable-auto-update models
agent --disable-auto-update mcp list
```

## Executable, version, and updates

The installer places the build under `~/.local/share/cursor-agent/versions/<build>/` and links both `~/.local/bin/agent` and `~/.local/bin/cursor-agent` to the same `cursor-agent` entry point. `agent` is the documented name; `cursor-agent` is the older name and remains installed. Older build directories stay beside the current one.

`agent --version` prints the build token alone, in the form `YYYY.MM.DD-<7 hex>` with a zero-padded month and day (`2026.09.10-fd3934a` on the baseline). Hook payloads carry the same token as `cursor_version`, and hook processes receive it as `CURSOR_VERSION`.

The CLI updates itself. An interactive session schedules a background update check two seconds after start, skipped when `--disable-auto-update` is passed or `cli-config.json` sets `channel` to `static` (bundle: `./src/run-agent.tsx` in `1931.index.js`). `agent update` and `/update` update on demand. `--disable-auto-update` is hidden from help, and the docs name no channel values; the config schema accepts `static`, `prod`, `lab`, and `prod-stable-internal` (bundle: `../cursor-config/dist/schema.js`). Running sessions mark their install directory in use so update cleanup skips it (changelog, July 13, 2026).

## Surface index

| Concern | Upstream surface | What upstream does not publish |
| --- | --- | --- |
| Session identity | common hook `conversation_id`; [Sessions](#sessions) | parent lineage for `/fork`; structured `agent ls` output |
| Session start and end | [`sessionStart`, `sessionEnd`](#lifecycle-events) | a start event on resume or `/clear`; an end event per conversation |
| Turn start | [`beforeSubmitPrompt`](#lifecycle-events) | whether a message steered into a running turn fires it |
| Tool activity | [`preToolUse`, `postToolUse`, `postToolUseFailure`](#lifecycle-events) | |
| Turn end | [`stop.status`](#lifecycle-events) | |
| Compaction | [`preCompact`](#lifecycle-events) | a post-compaction event |
| Live context and model | [command status line](#command-status-line) | a versioned payload schema |
| Subagents | [`subagentStart`, `subagentStop`](#subagent-hooks); [child chats](#child-chats) | whether the CLI issues the subagent hook requests |
| Waiting for the user | [ACP requests](#acp-server) only | any local hook for permission, question, or plan approval |
| Transcript | [`transcript_path`](#transcripts) | the JSONL record schema and durability contract |
| Supervised output | [`-p --output-format stream-json`](#headless-print-mode) | token usage; hook firing |
| Account | [`status`, `about` JSON](#authentication-and-account) | a response schema; credential storage |
| Tokens and cost | [`stop` and `afterAgentResponse` token fields](#usage-tokens-and-pricing) | spend, quota, or a per-user usage API |

## Sessions

`conversation_id` identifies a conversation across turns. Every session hook carries it, and the hook executor adds `session_id` with the same value unless the caller supplied its own (bundle: `../hooks-exec/dist/index.js` in `190.index.js`). `generation_id` changes with every user message ([hooks.md, Common schema](https://cursor.com/docs/hooks.md)).

| Operation | CLI surface | Identity |
| --- | --- | --- |
| Start | `agent [prompt...]` | new `conversation_id` |
| Start with a chosen ID | `--new-session-id <uuid>` (hidden from help) | caller-provided ID (bundle: `./src/cli.ts`) |
| Resume a chosen chat | `agent --resume <chatId>`, `agent ls`, `/resume` | same `conversation_id` |
| Resume the latest chat | `agent resume` | same `conversation_id` |
| Continue the previous chat | `agent --continue` (alias for `--resume=-1`) | same `conversation_id` |
| Clear | `/clear` (aliases `/new`, `/new-chat`, `/newchat`) | new `conversation_id` in the same process |
| Fork | `/fork` | new session; no parent field is published |
| Summarize | `/summarize` (alias `/compress`) | same `conversation_id`; fires `preCompact` |
| Rewind | `/rewind` (enabled by `rewind` in `cli-config.json`) | same session; no identity contract published |
| Create an empty chat | `agent create-chat` | prints a chat ID; output format unpublished |
| Persistent session | `agent persist [prompt]`, `/detach`, `agent persist attach <session>`, `agent persist list`, `agent persist stop <session>`, `agent persist --resume` | survives terminal disconnect ([changelog, August 26, 2026](https://cursor.com/docs/cli/changelog.md)) |

Resume opens chats from every workspace by default, so a resumed conversation can report `workspace_roots` that differ from the launch directory ([changelog, July 6, 2026](https://cursor.com/docs/cli/changelog.md)).

Session hooks do not fire for every conversation a process holds:

- A launch with a resume target fires no `sessionStart`; the CLI calls it only when no resume ID is set (bundle: `./src/run-agent.tsx` in `1931.index.js`).
- `/clear` fires neither `sessionEnd` for the old conversation nor `sessionStart` for the new one. The next `beforeSubmitPrompt` is the first event with the new `conversation_id` (captured on 2026.07.09).
- `sessionEnd` fires once per process, with the `conversation_id` the process started with, so conversations created later in that process get no end event (bundle: `./src/run-agent.tsx`; captured on 2026.07.09).

## Hooks

Command hooks are processes Cursor spawns for a hook step. Each receives one JSON object on stdin and may return one JSON object on stdout ([hooks.md](https://cursor.com/docs/hooks.md)). Cursor watches the config files and reloads them on save.

### Sources, priority, and working directory

Cursor runs every matching hook from every source. The docs give the priority order Enterprise → Team → Project → User, then the Claude Code sources when third-party loading is enabled ([third-party-hooks.md](https://cursor.com/docs/reference/third-party-hooks.md)).

| Source | Location | Working directory |
| --- | --- | --- |
| Enterprise | macOS `/Library/Application Support/Cursor/hooks.json`; Linux and WSL `/etc/cursor/hooks.json`; Windows `C:\ProgramData\Cursor\hooks.json` | the config file's directory |
| Team | dashboard-distributed, synced every thirty minutes (Enterprise); stored at `.cursor/managed/active-team-hooks/hooks.json` under Cursor's data directory | the managed hooks directory |
| Project | `<project>/.cursor/hooks.json`, trusted workspaces only | project root |
| User | `~/.cursor/hooks.json` | `~/.cursor/` |
| Plugin | hooks shipped by installed plugins, including `--plugin-dir` | the plugin install directory; the project root for `stop` and `subagentStop` |
| Claude project local | `.claude/settings.local.json` | project root |
| Claude project | `.claude/settings.json` | project root |
| Claude user | `~/.claude/settings.json` | `~/.claude/` |

The docs name the first four rows and the Claude rows; the plugin row is [changelog, August 11, 2026](https://cursor.com/docs/cli/changelog.md). Team storage, plugin and Claude working directories, and a refusal to load a config through a path that contains a symlink come from the bundle (`../hooks-exec/dist/index.js` in `190.index.js`). All hooks for a step start together and run in parallel; responses merge in the order the table lists, as described in [Merged responses](#merged-responses).

### Claude Code hooks

Cursor loads Claude Code hook files when the account setting **Third-party skills** is on ([third-party-hooks.md](https://cursor.com/docs/reference/third-party-hooks.md)). The Claude events map as follows; `Notification` and `PermissionRequest` are ignored with a warning, and Claude has no `subagentStart`.

| Claude Code event | Cursor step |
| --- | --- |
| `PreToolUse` | `preToolUse` |
| `PostToolUse` | `postToolUse` |
| `UserPromptSubmit` | `beforeSubmitPrompt` |
| `Stop` | `stop` |
| `SubagentStop` | `subagentStop` |
| `SessionStart` | `sessionStart` |
| `SessionEnd` | `sessionEnd` |
| `PreCompact` | `preCompact` |

Tool matchers translate `Bash` to `Shell`, `Edit` to `Write`, and `mcp__<server>__<tool>` to `MCP:<tool>`; `Read`, `Write`, `Grep`, and `Task` pass through, and `Glob` is dropped. The bundle also passes `WebFetch` and `WebSearch` through, while the docs list both as unsupported (bundle: `../hooks/dist/index.js`). `SessionStart` and `PreCompact` trigger matchers are ignored, so those hooks fire for every trigger.

Converted Claude entries get `loop_limit: null` and `failClosed: false`. Claude response shapes are accepted: nested `hookSpecificOutput.permissionDecision`, `permissionDecisionReason`, and `updatedInput` map to `permission`, `user_message`, and `updated_input` on `preToolUse`, and `decision: "block"` with a `reason` becomes `followup_message` on `stop` and `subagentStop`.

The docs promise no rewrite of hook **input** into Claude's schema: a Claude hook receives Cursor's payload, which carries `cursor_version`.

### Configuration file

A hooks file is JSON with a positive integer `version` (currently `1`) and a `hooks` object keyed by step name. An unknown step name fails validation.

```json
{
  "version": 1,
  "hooks": {
    "sessionStart": [{ "command": "/abs/path/to/hook" }],
    "stop": [{ "command": "/abs/path/to/hook", "timeout": 10 }],
    "preToolUse": [{ "command": "/abs/path/to/hook", "matcher": "Shell|Write" }]
  }
}
```

| Field | Type and default | Behaviour |
| --- | --- | --- |
| `command` | string, required for command hooks | shell string, absolute path, or path relative to the source's working directory |
| `type` | `"command"` or `"prompt"`; default `"command"` | a prompt hook sends `prompt` (with `$ARGUMENTS` replaced by the input JSON) to an LLM that returns `{ ok, reason? }` |
| `prompt` | string, required for prompt hooks | the condition the LLM evaluates |
| `model` | string, optional, prompt hooks only | overrides the evaluation model |
| `timeout` | positive number of seconds; default `60` | a timed-out hook is a failure |
| `loop_limit` | positive integer or `null`; default `5` | skips a `stop` or `subagentStop` hook once `loop_count` reaches the limit; `null` removes it |
| `failClosed` | boolean; default `false` | turns a failure into a block (see [Exit codes and failures](#exit-codes-and-failures)) |
| `matcher` | string; a valid regular expression | see [Matchers](#matchers) |

The docs give `timeout` as "platform default" and `matcher` as type "object"; the values above are the bundle's (`../hooks/dist/index.js`: default timeout `60`, loop limit `5`, and a validator that requires a string matcher that compiles as a `RegExp`). A top-level `stop_hook_loop_limit` is deprecated and ignored with a warning.

### Process and environment

Cursor writes the input JSON to the hook's stdin. The CLI's executor also has an argv heredoc transport and, on Windows, a temp-file transport piped through PowerShell; the CLI entry points select stdin (bundle: `../hooks-exec/dist/index.js` and its callers). Hooks run outside Cursor's command sandbox. The docs do not name the shell that runs a `command` string.

| Variable | Value |
| --- | --- |
| `CURSOR_PROJECT_DIR` | absolute workspace path, present on every run |
| `CURSOR_VERSION` | build token |
| `CURSOR_USER_EMAIL` | account email, when known |
| `CURSOR_TRANSCRIPT_PATH` | the conversation transcript path, when the file exists |
| `CLAUDE_PROJECT_DIR` | same as `CURSOR_PROJECT_DIR` |
| `CURSOR_PLUGIN_ROOT`, `CLAUDE_PLUGIN_ROOT` | plugin install directory, plugin hooks only |
| `CURSOR_CODE_REMOTE` | `"true"` in a remote workspace, per the docs; the CLI executor does not set it |
| keys returned in `sessionStart.env` | passed to every later hook in the session |

A user hook's process working directory is `~/.cursor`, so `CURSOR_PROJECT_DIR` is the only absolute project path in a user hook's environment.

### Common input

Every session hook receives these fields beside its step's own fields. The executor fills `hook_event_name`, `cursor_version`, `workspace_roots`, `user_email`, `session_id`, and `transcript_path`; the step's caller fills the rest (bundle: `../hooks-exec/dist/index.js`).

```json
{
  "conversation_id": "string",
  "generation_id": "string",
  "session_id": "string",
  "model": "string",
  "model_id": "string",
  "model_params": [{ "id": "string", "value": "string" }],
  "hook_event_name": "string",
  "cursor_version": "string",
  "workspace_roots": ["/abs/path"],
  "user_email": "string | null",
  "transcript_path": "string | null"
}
```

| Field | Meaning |
| --- | --- |
| `conversation_id` | stable conversation ID |
| `generation_id` | changes with every user message; equals `conversation_id` on CLI `sessionStart` and `sessionEnd` |
| `session_id` | the caller's session ID, else `conversation_id` |
| `model` | legacy model slug |
| `model_id` | structured model ID; absent on CLI `sessionStart` and `sessionEnd` |
| `model_params` | selected parameters such as `thinking`, `context`, `effort`, and `fast`; omitted when empty |
| `hook_event_name` | step name |
| `cursor_version` | build token |
| `workspace_roots` | the docs describe multi-root workspaces; the CLI executor sends one element, the workspace path |
| `user_email` | account email or `null` |
| `transcript_path` | the transcript path if the file exists, else `null` |

`workspaceOpen` fires outside a session and omits `conversation_id`, `generation_id`, `model`, `session_id`, and `transcript_path`.

### Lifecycle events

Step-specific input fields below come from the docs and the generated request types in `../proto/dist/generated/agent/v1/hooks_pb.js`; the CLI's `sessionStart` and `sessionEnd` payloads come from `./src/run-agent.tsx`.

| Step | Fires | Step-specific input |
| --- | --- | --- |
| `sessionStart` | a new conversation starts; fire-and-forget | `is_background_agent` (`false` in the CLI), `composer_mode` (the `--mode` value when set) |
| `beforeSubmitPrompt` | after send, before the backend request | `prompt`, `attachments[]` (`{ type: "file" \| "rule", file_path }`), `composer_mode` |
| `preToolUse` | before any tool runs | `tool_name`, `tool_input`, `tool_use_id`, `cwd`, `agent_message` |
| `postToolUse` | a tool succeeds | `tool_name`, `tool_input`, `tool_output` (JSON string), `tool_use_id`, `cwd`, `duration` (ms) |
| `postToolUseFailure` | a tool errors, times out, or is denied | `tool_name`, `tool_input`, `tool_use_id`, `error_message`, `failure_type` (`error`, `timeout`, `permission_denied`), `duration` (ms), `is_interrupt` |
| `afterAgentResponse` | an assistant message completes | `text`; optional `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens` |
| `afterAgentThought` | a thinking block completes | `text`, optional `duration_ms` |
| `stop` | the agent loop ends | `status` (`completed`, `aborted`, `error`), `loop_count`; optional `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens` |
| `preCompact` | summarization begins, automatic or `/summarize`; observational | `trigger` (`auto`, `manual`), `context_usage_percent`, `context_tokens`, `context_window_size`, `message_count`, `messages_to_compact`, `is_first_compaction` |
| `sessionEnd` | the process exits; fire-and-forget | `reason` and `final_status` (both `completed`, `aborted` on SIGINT or SIGTERM, or `error` in the CLI), `duration_ms`, `is_background_agent`; the docs also list `window_close`, `user_close`, and `error_message` |

The docs omit the token fields; the generated `StopRequestQuery` and `AfterAgentResponseRequestQuery` types carry them. No event follows compaction.

### Subagent hooks

`subagentStart` fires before a Task subagent spawns and can deny it; `subagentStop` fires when the subagent completes, errors, or aborts. The docs' `subagentStop` example omits the ID fields; the generated types carry them.

| `SubagentStartRequestQuery` field | Type |
| --- | --- |
| `subagent_id` | string |
| `subagent_type` | string, for example `generalPurpose`, `explore`, `shell` |
| `task` | string |
| `parent_conversation_id` | string |
| `tool_call_id` | optional string |
| `subagent_model` | optional string |
| `is_parallel_worker` | boolean |
| `git_branch` | optional string |

| `SubagentStopRequestQuery` field | Type |
| --- | --- |
| `subagent_id`, `subagent_type`, `parent_conversation_id` | string |
| `status` | string: `completed`, `error`, `aborted` |
| `duration_ms`, `message_count`, `tool_call_count`, `loop_count` | number |
| `summary`, `error_message`, `git_branch`, `task`, `description` | optional string |
| `modified_files` | string array |
| `agent_transcript_path` | string or `null`, added by the executor from `(subagent_id, parent_conversation_id)` |

Both steps also carry the common conversation and model fields. Both are requests the Cursor backend issues to the CLI (bundle: `../agent-exec/dist/index.js`), not calls the CLI makes on its own. In a live probe on 2026.07.09, configured root hooks fired on every turn while four successful child launches produced no `subagentStart` or `subagentStop` process. The baseline sends the configured hook step names to the backend with each request (`hooksConfig.configuredSteps`); whether the backend now issues the subagent requests to the CLI is not re-verified.

### Other hooks

| Step | Input | Output |
| --- | --- | --- |
| `beforeShellExecution` | `command`, `cwd`, `sandbox` | permission decision |
| `afterShellExecution` | `command`, `output`, `duration`, `sandbox` | none |
| `beforeMCPExecution` | `tool_name`, `tool_input` (JSON string), `mcp_server_name`, plus `url` and `mcp_server_url` (HTTP or SSE) or `command` (stdio) | permission decision |
| `afterMCPExecution` | `tool_name`, `tool_input`, `mcp_server_name`, `mcp_server_url`, `result_json`, `duration` | none |
| `beforeReadFile` | `file_path`, `content`, `attachments[]` | permission decision |
| `afterFileEdit` | `file_path`, `edits[]` of `{ old_string, new_string }` | none |
| `beforeTabFileRead` | `file_path`, `content`; editor Tab completions only | permission decision |
| `afterTabFileEdit` | `file_path`, `edits[]` with `range`, `old_line`, `new_line`; editor Tab completions only | none |
| `workspaceOpen` | `workspace_roots`, `cursor_version`, `user_email`, `hook_event_name` | `pluginPaths[]` of plugin directories to load |

Cloud agents run a subset of these steps and never run user hooks ([hooks.md, Cloud agent support](https://cursor.com/docs/hooks.md)).

### Matchers

A matcher is a regular expression tested with JavaScript `RegExp.test` against one value per step: unanchored and case-sensitive. An empty matcher, `*`, or a step with no match value matches everything, and a pattern that fails to compile at match time also matches (bundle: `../hooks/dist/index.js`).

| Step | Value tested |
| --- | --- |
| `preToolUse`, `postToolUse`, `postToolUseFailure` | `tool_name`: `Shell`, `Read`, `Write`, `Grep`, `Delete`, `Task`, `MCP:<tool>`, and others |
| `subagentStart`, `subagentStop` | `subagent_type` |
| `beforeShellExecution`, `afterShellExecution` | the full command |
| `beforeMCPExecution`, `afterMCPExecution` | `MCP:<tool_name>` |
| `beforeReadFile` | `Read` |
| `afterFileEdit` | `Write` |
| `beforeTabFileRead` | `TabRead` |
| `afterTabFileEdit` | `TabWrite` |
| `beforeSubmitPrompt` | `UserPromptSubmit` |
| `stop` | `Stop` |
| `afterAgentResponse` | `AgentResponse` |
| `afterAgentThought` | `AgentThought` |

The docs list `TabRead` under `beforeReadFile` and `TabWrite` under `afterFileEdit`; the bundle tests those values only on the Tab steps.

### Outputs

An empty object `{}` is a valid response on every step and changes nothing. Output fields each step accepts (bundle validators in `../hooks/dist/index.js`, which pass unknown keys through):

| Step | Output fields |
| --- | --- |
| `preToolUse` | `permission` (`allow`, `deny`; `ask` validates but the docs say it is not enforced), `user_message`, `agent_message`, `updated_input` (object), `additional_context` |
| `beforeShellExecution`, `beforeMCPExecution` | `permission` (`allow`, `deny`, `ask`), `user_message`, `agent_message` |
| `beforeReadFile`, `beforeTabFileRead` | `permission` (`allow`, `deny`), `user_message` |
| `subagentStart` | `permission` (`allow`, `deny`; `ask` is treated as `deny`), `user_message` |
| `beforeSubmitPrompt` | `continue` (`false` blocks the prompt), `user_message`, `additional_context` |
| `sessionStart` | `env`, `additional_context`; `continue` and `user_message` validate but are not enforced |
| `postToolUse` | `updated_mcp_tool_output` (MCP tools only), `additional_context` |
| `postToolUseFailure` | `additional_context` |
| `stop` | `followup_message`, submitted as the next user message |
| `subagentStop` | `followup_message`, consumed only when `status` is `completed` |
| `preCompact` | `user_message` |
| `workspaceOpen` | `pluginPaths` |
| `afterShellExecution`, `afterMCPExecution`, `afterFileEdit`, `afterTabFileEdit`, `afterAgentResponse`, `afterAgentThought`, `sessionEnd` | none |

The docs omit `additional_context` on `beforeSubmitPrompt` and `postToolUseFailure`. Cursor reads stdout after trimming it; when the whole text is not JSON but ends in `}`, it parses the last `{...}` suffix that is.

### Merged responses

Only successful hook responses merge, in source order (bundle: `O` in `../hooks/dist/index.js`):

| Field | Merge |
| --- | --- |
| `permission` | most restrictive wins: `deny`, then `ask`, then `allow` |
| `user_message`, `agent_message` | joined with a `---` separator line |
| `additional_context` | joined on `sessionStart`, `beforeSubmitPrompt`, `preToolUse`, `postToolUse`, `postToolUseFailure`; otherwise overwritten |
| `continue` | logical AND |
| `sessionStart.env` | key-wise, a later source overwriting an earlier key |
| `pluginPaths` | de-duplicated union |
| any other field | the later response overwrites the earlier |

The docs say higher-priority sources take precedence in a conflict. The wire merges in source order, so for an overwritten field a lower-priority source's value wins.

### Exit codes and failures

"Permission steps" below are `beforeShellExecution`, `beforeMCPExecution`, `beforeReadFile`, `beforeTabFileRead`, `subagentStart`, and `preToolUse` (bundle: `../hooks/dist/index.js` and `../hooks-exec/dist/index.js`).

| Hook result | Behaviour |
| --- | --- |
| exit `0`, valid JSON | validated and merged |
| exit `0`, empty stdout | failure; no response |
| exit `0`, invalid JSON or a response that fails validation | on a permission step, blocked with `permission: "deny"` even when `failClosed` is `false`; on other steps, failure |
| exit `2` | block: `permission: "deny"` on a permission step and `continue: false` on `beforeSubmitPrompt` and `sessionStart` (not enforced there); the message is the trimmed stdout, else `Hook blocked with message: <stderr>`; no effect on other steps |
| other non-zero exit | failure |
| timeout or spawn error | failure |

A failure contributes no response, so the action proceeds. With `failClosed: true`, a failure blocks instead, which takes effect on permission steps and `beforeSubmitPrompt`. The docs say invalid JSON on `beforeShellExecution`, `beforeMCPExecution`, and `beforeReadFile` fails open; the wire blocks it.

## Waiting for the user

The local hook catalog has no event for a permission prompt, a question to the user, plan approval, or an idle notification. `preToolUse` fires before a tool runs whether or not Cursor then prompts. `beforeShellExecution` and `beforeMCPExecution` can return `ask`, which requests a prompt rather than reporting one.

Cursor's agent asks the user questions through the `AskQuestion` tool and proposes plans through `CreatePlan`. Both render in the terminal UI; the local traces they leave are recorded under [Root conversation state](#root-conversation-state). Mode-switch approvals auto-reject after a 15-second countdown ([changelog, August 11, 2026](https://cursor.com/docs/cli/changelog.md)).

Structured permission, question, and plan requests exist only in [ACP](#acp-server), where the client renders them.

## Command status line

`statusLine` in `~/.cursor/cli-config.json` runs a command that renders the status line. The docs mention the feature in the changelog only; the fields below come from the config schema (`../cursor-config/dist/schema.js`) and the runner (`./src/hooks/use-status-line.ts` in `1931.index.js`).

```json
{
  "statusLine": {
    "type": "command",
    "command": "/abs/path/to/statusline --flag",
    "padding": 0,
    "updateIntervalMs": 300,
    "timeoutMs": 2000
  }
}
```

| Field | Type and default |
| --- | --- |
| `type` | `"command"`, required |
| `command` | non-empty string, required |
| `padding` | non-negative integer; default `0` |
| `updateIntervalMs` | positive integer; default `300`, raised to at least `300` |
| `timeoutMs` | positive integer; default `2000`, raised to at least `50` |

Cursor splits `command` shell-style into argv, expands a leading `~` in the program, and spawns it directly without a shell, in the session's working directory with Cursor's environment. It writes one JSON payload to stdin and renders stdout with trailing newlines removed. Updates are throttled to `updateIntervalMs`; a newer update aborts a running one. A timeout or a non-zero exit with empty stdout leaves the previous text.

The payload (bundle: `./src/ui.tsx` in `1931.index.js`):

| Field | Content |
| --- | --- |
| `session_id` | session ID |
| `session_name` | optional session name; before the first prompt it can hold Cursor's own placeholder text (captured on 2026.07.09) |
| `transcript_path` | transcript path |
| `cwd` | working directory |
| `render_width_chars` | available width |
| `autorun` | run-everything state |
| `model` | `id`, `display_name`, optional `param_summary`, and `max_mode: true` when Max Mode is on |
| `workspace` | `current_dir`, `project_dir`, `added_dirs` |
| `version` | build token |
| `output_style` | `{ "name": "compact" \| "default" }` |
| `vim` | `{ "mode": ... }` when Vim mode is on |
| `worktree` | `{ name, path }` in a Cursor worktree |
| `context_window` | `total_input_tokens`, `total_output_tokens`, `context_window_size`, `used_percentage`, `remaining_percentage`, `current_usage` (input, output, cache-creation, and cache-read tokens) |

Nothing versions this payload. An explicit-model capture on 2026.07.09 reported `model.display_name` `"GPT-5.6 Sol 272K Medium"`, `model.param_summary` `"272K Medium"`, and `context_window.context_window_size` `200000`: the summary names the selected model option, and `context_window_size` is the live window.

## Transcripts and local chat state

### Transcripts

The CLI writes transcripts under `~/.cursor/projects/<project>/agent-transcripts/`, where `<project>` is the workspace path with every run of non-alphanumeric characters replaced by `-` and leading and trailing `-` trimmed (bundle: `../utils/dist/workspace-paths.js` in `index.js`; `../agent-transcript/dist/paths.js`).

| Transcript | Path under `agent-transcripts/` |
| --- | --- |
| Root conversation | `<conversation_id>/<conversation_id>.jsonl` |
| Subagent, as written by the baseline's subagent transcript store | `<parent_conversation_id>/subagents/<subagent_id>.jsonl` |
| Lookup fallbacks | `<id>/<id>.jsonl` for a subagent, then the legacy flat `<id>.jsonl`; `.txt` variants of each |

Hooks expose the root path as `transcript_path` and `CURSOR_TRANSCRIPT_PATH`, and `subagentStop` exposes the child path as `agent_transcript_path`. Each is the first candidate that exists, or `null`.

Upstream publishes no record schema. These minimized row shapes were captured on 2026.07.09:

```json
{"role":"user","message":{"content":[{"type":"text","text":"<redacted>"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"<redacted>"}]}}
{"type":"turn_ended","status":"success"}
```

The file also holds thinking and tool records. Assistant `text` blocks mix visible commentary with model thinking and carry no field that tells them apart. The captured writer also wrote `turn_ended` rows with `status` `aborted` and `error` and optional error text. An authenticated `--print --resume` run rewrote the whole file as a snapshot of the conversation instead of appending to it (captured on 2026.07.09).

### Chat store

The CLI keeps each conversation in `~/.cursor/chats/<md5>/<session id>/`, where `<md5>` is the lowercase hex MD5 of the absolute, resolved workspace path (bundle: `./src/state/index.ts`). A directory holds `store.db` and a `meta.json` sidecar.

`store.db` is SQLite in WAL mode with `PRAGMA user_version = 1` and a 5-second busy timeout (bundle: `../cursor-sdk-local-runtime/dist/run-store/sqlite-blob-store.js`):

```sql
CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
```

`meta` row `'0'` holds hex-encoded UTF-8 JSON. Captured on 2026.07.09, it carries `agentId`, `createdAt`, and `latestRootBlobId`, a 64-character lowercase hex SHA-256 of the root blob in `blobs`. The bundle also reads `name`, `mode`, `currentPlanUri`, and `subagentInfo` from it.

`meta.json` is written by `./src/state/chat-session-sidecar.ts` in `1931.index.js`:

| Field | Content |
| --- | --- |
| `schemaVersion` | `1` |
| `createdAtMs` | from store `createdAt` |
| `updatedAtMs` | milliseconds |
| `hasConversation` | `true` when the store has a `latestRootBlobId` |
| `title` | optional, from store `name` |
| `isSubagent` | `true` when the store has `subagentInfo` |
| `cwd` | optional workspace path |

The session list skips entries with `isSubagent` and entries without a conversation. When `meta.json` is missing, the lister backfills it from `store.db`, so a child directory can gain a `meta.json` with `isSubagent: true` after any resume listing.

### Root conversation state

The root blob is a protobuf `agent.v1.ConversationStateStructure` (bundle: `../proto/dist/generated/agent/v1/agent_pb.js`). Field `1` is repeated bytes `root_prompt_messages_json`, and field `4` is repeated string `pending_tool_calls`; other fields hold turns, todos, token details, the summary, and the plan.

Each `pending_tool_calls` entry is JSON for a pending assistant message. A synchronous question captured on 2026.07.09 was a single content item with `type: "tool-call"`, `toolName: "AskQuestion"`, a `toolCallId`, `args.questions[].prompt`, `args.runAsync` not `true`, and `providerOptions.cursor.pendingToolCallStartedAtMs`. The CLI committed that entry while the question was on screen and removed it after the answer. `meta.json` `updatedAtMs` advanced with it.

The pending entry records neither the terminal UI's focus nor a partially selected answer. The docs publish none of this state.

### Child chats

A subagent's chat directory sits in the same `chats/<md5>/` bucket, named by the child's `agentId`. Its `meta['0']` JSON adds `subagentInfo` with `parentAgentId`, `rootParentAgentId`, `toolCallId`, and `typeName` (bundle: `./src/subagent/cli-subagent-host-adapter.ts` in `7569.index.js`). Captured on 2026.07.09, a child directory had no `meta.json`, and the child's transcript ended with a `turn_ended` row once the child finished; the baseline's subagent transcript store writes that transcript under the parent's `subagents/` directory, as listed in [Transcripts](#transcripts).

Background subagent files under `~/.cursor/subagents/` have no published format.

## Headless print mode

`agent -p` (or `--print`) runs one non-interactive session, with access to write and shell tools. `--output-format` selects `text` (default), `json`, or `stream-json`. `--force` (alias `--yolo`) allows commands unless a deny rule matches, and `--trust` accepts the workspace trust prompt ([headless.md](https://cursor.com/docs/cli/headless.md)).

Headless and single-turn runs wait for delegated subagents before exiting ([changelog, August 11, 2026](https://cursor.com/docs/cli/changelog.md)). A `-p` run on 2026.07.09 completed without starting any configured hook process.

| Flag | Effect |
| --- | --- |
| `--output-format <text\|json\|stream-json>` | output format; only with `--print` |
| `--stream-partial-output` | streams text deltas; only with `stream-json` |
| `--show-thinking` (hidden) | includes thinking blocks in the `json` result |
| `--single-turn` (hidden) | finishes after the first user turn and its subagents |
| `--printenv` (hidden) | captures an environment snapshot after each terminal command |

### JSON result

A successful `--output-format json` run prints one newline-terminated object ([output-format.md](https://cursor.com/docs/cli/reference/output-format.md)):

```json
{
  "type": "result",
  "subtype": "success",
  "is_error": false,
  "duration_ms": 1234,
  "duration_api_ms": 1234,
  "result": "<full assistant text>",
  "session_id": "<uuid>",
  "request_id": "<optional request id>"
}
```

A failed run exits non-zero, writes the error to stderr, and may print no JSON.

### Stream JSON

`--output-format stream-json` prints one JSON object per line:

| `type` / `subtype` | Fields |
| --- | --- |
| `system` / `init` | `apiKeySource`, `cwd`, `session_id`, `model`, `permissionMode` |
| `user` | `message` (role and content), `session_id` |
| `assistant` | `message`, `session_id` |
| `tool_call` / `started` | `call_id`, `tool_call`, `session_id` |
| `tool_call` / `completed` | `call_id`, `tool_call` with its result, `session_id` |
| `result` / `success` | `duration_ms`, `duration_api_ms`, `is_error`, `result`, `session_id`, `request_id` |

Without `--stream-partial-output`, each `assistant` event is one complete message segment between tool calls. With it, an `assistant` event that has `timestamp_ms` and no `model_call_id` carries new text; one with both is a buffered duplicate sent before a tool call, and one with neither is the final duplicate.

`tool_call` holds a tagged object such as `readToolCall` or `writeToolCall` with `args` and, on completion, `result`; other tools use `function` with `name` and JSON `arguments`. Cursor may add fields. A failing stream can end without a `result` event. No event carries token usage, and thinking is omitted. The docs call `session_id` stable for one execution and do not say whether it equals the hook `conversation_id`.

## ACP server

`agent acp` runs a JSON-RPC 2.0 server over stdio with newline-delimited framing: requests and notifications arrive on stdin, responses and notifications leave on stdout, and logs may go to stderr ([acp.md](https://cursor.com/docs/cli/acp.md)). The command is hidden from help.

The request flow is `initialize`; `authenticate` with `methodId: "cursor_login"`; `session/new` or `session/load`; `session/prompt`; `session/update` notifications while the model streams; `session/request_permission` requests; and an optional `session/cancel`. Sessions support the `agent`, `plan`, and `ask` modes and the project and user `.cursor/mcp.json`; team dashboard MCP servers are unavailable.

| Method | Kind | Contract |
| --- | --- | --- |
| `session/request_permission` | request, blocking | client answers `allow-once`, `allow-always`, or `reject-once` |
| `cursor/ask_question` | request, blocking | `toolCallId`, optional `title`, `questions[]` of `{ id, prompt, options[] of { id, label }, allowMultiple? }`; answer, skip, or cancel |
| `cursor/create_plan` | request, blocking | Markdown plan and todos; accept, reject, or cancel |
| `cursor/update_todos` | notification | todo state updates |
| `cursor/task` | notification | subagent task completion |
| `cursor/generate_image` | notification | generated image output |

A client that never answers a blocking request stalls tool execution. The upstream ACP specification owns the standard request and update schemas.

Authentication before start-up uses `agent login`, `--api-key` or `CURSOR_API_KEY`, or `--auth-token` or `CURSOR_AUTH_TOKEN`. The root options `-e`/`--endpoint` and `-k`/`--insecure` apply too; `--auth-token` and `--insecure` are hidden from help.

## Authentication and account

| Command or input | Effect |
| --- | --- |
| `agent login` | browser login; `NO_OPEN_BROWSER=1` prints the URL instead, and pressing `q` shows a QR code |
| `agent logout`, `/logout` | signs out and clears stored credentials |
| `agent status`, `agent whoami` | authentication status; `--format json` |
| `agent about`, `/about` | version, system, and account details; `--format json` |
| `--api-key <key>`, `CURSOR_API_KEY` | API key authentication |
| `--auth-token <token>`, `CURSOR_AUTH_TOKEN` | auth token (hidden from help) |
| `-e, --endpoint <url>`, `CURSOR_API_ENDPOINT` | API endpoint; default `https://api2.cursor.sh` |
| `-H, --header 'Name: Value'` | extra request header, repeatable |
| `agent bedrock`, `/bedrock` | AWS Bedrock configuration, when the feature is enabled |
| `AGENT_CLI_CREDENTIAL_STORE=file` | stores credentials unencrypted in an owner-only file, for sandboxed environments ([changelog](https://cursor.com/docs/cli/changelog.md)); path and format unpublished |

The docs publish no JSON schema, exit codes, or error arms for `status` and `about`. An authenticated browser login on 2026.07.09 produced this sanitized `status --format json`:

```json
{
  "status": "authenticated",
  "isAuthenticated": true,
  "hasAccessToken": true,
  "hasRefreshToken": true,
  "userInfo": {
    "email": "<redacted>",
    "userId": 0,
    "firstName": "<redacted>",
    "lastName": "<redacted>",
    "createdAt": "<redacted>"
  }
}
```

The same login produced this `about --format json`, with non-empty `subscriptionTier` and `userEmail` strings and a `null` `lastRequestId`:

```json
{
  "cliVersion": "2026.07.09-a3815c0",
  "model": "<redacted>",
  "subscriptionTier": "<redacted>",
  "osPlatform": "<redacted>",
  "osArch": "<redacted>",
  "userEmail": "<redacted>",
  "terminalProgram": "<redacted>",
  "shell": "<redacted>",
  "lastRequestId": null
}
```

Logged-out, expired, API-key, service-account, proxy, and server-error responses are uncaptured. Credential storage has no published location or format. The config file caches `authInfo` (`email`, `displayName`, `teamId`, `teamName`, `userId`, `authId`, `organizationId`) as CLI-managed state.

## Usage, tokens, and pricing

The hook wire carries per-turn token counts: `input_tokens`, `output_tokens`, `cache_read_tokens`, and `cache_write_tokens` on `stop` and `afterAgentResponse` (see [Lifecycle events](#lifecycle-events)). Upstream does not document them, nor whether `input_tokens` includes the two cache classes. `preCompact.context_tokens` and the status line's `context_window` describe window occupancy.

`/usage` shows included-usage meters with Auto and API breakdowns, on-demand spend against the limit, the plan name, and the billing-cycle reset date ([changelog, July 13, 2026](https://cursor.com/docs/cli/changelog.md)). It is terminal UI only; the CLI has no machine-readable usage command.

Pricing ([models-and-pricing.md](https://cursor.com/docs/models-and-pricing.md)):

- Pro, Pro Plus, and Ultra include two monthly usage pools: Cursor Models (Grok 4.6, Grok 4.5, Composer 2.5) and Other Models, charged at each model's API price.
- Auto has Cost, Balance, and Intelligence modes and bills every request at the list price of the model it routes to. The page publishes no flat Auto rate.
- On Teams and Enterprise plans, third-party model requests add a Cursor Token Rate of $0.25 per million tokens, including when Auto routes to a third-party model. First-party Cursor models are exempt.
- Max Mode exists only on legacy request-based plans, billed at the model's API rate plus 20%.
- The page's per-model table lists input, cache-write, cache-read, and output rates per million tokens.

The team [Admin API](https://cursor.com/docs/account/teams/admin-api.md) returns usage events with `tokenUsage.inputTokens`, `outputTokens`, `cacheWriteTokens`, and `cacheReadTokens`, behind a team admin API key.

## Configuration, modes, and permissions

### Configuration files

| Scope | Path | Contents |
| --- | --- | --- |
| Global, macOS and Linux | `~/.cursor/cli-config.json` | all CLI settings |
| Global, Windows | `%USERPROFILE%\.cursor\cli-config.json` | all CLI settings |
| Global override | `$CURSOR_CONFIG_DIR/cli-config.json` | all CLI settings |
| Global, Linux and BSD | `$XDG_CONFIG_HOME/cursor/cli-config.json` | all CLI settings |
| Project | `<project>/.cursor/cli.json` | permissions only; ignored with the hidden `--disable-project-configs` |

The file is pure JSON with `version` `1`. Cursor repairs missing fields and moves a corrupted file aside as `.bad` ([configuration.md](https://cursor.com/docs/cli/reference/configuration.md)). The CLI rewrites the whole file from its schema through a temp file and rename, so keys outside the schema are dropped.

`cli-config.json` fields from `../cursor-config/dist/schema.js`, with defaults from the CLI's default config; CLI-managed caches are left out:

| Field | Type and default |
| --- | --- |
| `version` | number |
| `editor.vimMode` | boolean; `editor.defaultBehavior` `ide` or `agent` |
| `permissions.allow`, `permissions.deny` | string arrays of [permission tokens](#permission-tokens) |
| `approvalMode` | `allowlist` (default), `auto-review`, `unrestricted` |
| `sandbox.mode` | `disabled` (default), `enabled` |
| `sandbox.networkAccess` | `user_config_only`, `user_config_with_defaults` (default), `allow_all`; legacy `allowlist` and `enabled` map to the second and third |
| `sandbox.networkAllowlist` | string array |
| `sandbox.readBoundary` | `system`, `workspace` |
| `autoAcceptWebSearch` | boolean; default `false` |
| `webFetchDomainAllowlist` | string array |
| `statusLine` | see [Command status line](#command-status-line) |
| `model`, `selectedModel`, `modelParameters`, `maxMode` | model selection |
| `exploreSubagentModel` | `default` (default) or `inherit` |
| `subagentModels.explore` | Explore subagent model: `default`, `inherit`, `disabled`, or a model selection |
| `notifications` | boolean; default `true`; notifies when the agent finishes or needs input |
| `hints`, `modelSlashCommands`, `rewind` | booleans; default `true` |
| `display` | booleans `showLineNumbers`, `showThinkingBlocks`, `showStatusIndicators`, `showStatusLineRunningTime` (all default `false`); `mode` `zen` (default) or `standard` |
| `channel` | `static`, `prod`, `lab`, `prod-stable-internal` |
| `network.useHttp1ForAgent` | boolean; default `false` |
| `attribution.attributeCommitsToAgent`, `attribution.attributePRsToAgent` | booleans; default `true` |
| `bedrock` | `enabled`, `mode` (`access-key`, `team-role`), `region`, and role fields |

Proxies use `HTTP_PROXY`, `HTTPS_PROXY`, `NODE_USE_ENV_PROXY=1`, and `NODE_EXTRA_CA_CERTS`.

### Modes

| Mode | Launch | Behaviour |
| --- | --- | --- |
| Agent | default | full tool set, subject to approvals and sandbox |
| Plan | `--plan`, `--mode=plan`, `/plan` | analyzes and proposes plans without edits |
| Ask | `--mode=ask`, `/ask` | read-only questions |
| Debug | `/debug` | slash command only |
| Goal | `/goal [objective]` | a durable goal that continues across idle and headless runs; gated rollout |

Approval is a separate setting from mode: `approvalMode` chooses an allowlist, Auto-review (a server classifier runs safe calls and prompts for the rest), or unrestricted. `--force` selects run-everything for a session, `--auto-review` selects Auto-review, and `/run-everything` (alias `/auto-run`) toggles it. `--sandbox enabled|disabled` overrides `sandbox.mode`.

### Launch flags

From `agent --help` on the baseline:

| Flag | Effect |
| --- | --- |
| `[prompt...]` | initial prompt; after `--`, a first argument equal to a command name (such as `help` or `update`) runs that command |
| `--model <model>` | model; bracketed overrides such as `'claude-opus-4-8[context=1m,effort=high,fast=false]'` |
| `--list-models` | lists models and exits |
| `--mode <plan\|ask>`, `--plan` | start mode |
| `--resume [chatId]`, `--continue` | resume |
| `-f, --force`, `--yolo` | allow commands unless explicitly denied |
| `--auto-review` | Auto-review for the session |
| `--sandbox <enabled\|disabled>` | sandbox override |
| `--approve-mcps` | approves all MCP servers |
| `--trust` | trusts the workspace without prompting, interactive or headless ([changelog, July 20, 2026](https://cursor.com/docs/cli/changelog.md)) |
| `--workspace <path-or-name>` | workspace directory or saved workspace name; default the working directory |
| `--add-dir <path>` | extra workspace root, repeatable |
| `--plugin-dir <path>` | loads a local plugin directory, repeatable |
| `-w, --worktree [name]` | starts in a Git worktree at `~/.cursor/worktrees/<repo>/<name>` |
| `--worktree-base <branch>` | worktree base; default `HEAD` |
| `--skip-worktree-setup` | skips `.cursor/worktrees.json` setup commands, which otherwise run only in a trusted workspace |
| `-p`, `--output-format`, `--stream-partial-output` | see [Headless print mode](#headless-print-mode) |
| `--api-key`, `-H`, `-e` | see [Authentication and account](#authentication-and-account) |

The parameters page omits `--auto-review`, `--add-dir`, and `-e`, and still describes `--trust` as headless-only. Hidden options in `./src/cli.ts` besides those named above include `--disable-auto-update`, `--new-session-id <uuid>`, `--min-version <version>`, `--disable-project-configs`, and `--debug` (local log server).

### Permission tokens

`permissions.allow` and `permissions.deny` take these tokens ([permissions.md](https://cursor.com/docs/cli/reference/permissions.md)):

| Token | Controls |
| --- | --- |
| `Shell(commandBase)`, `Shell(command:args)` | shell commands, with globs |
| `Read(pathOrGlob)` | file reads |
| `Write(pathOrGlob)` | file writes |
| `WebFetch(domainOrPattern)` | web-fetch domains |
| `Mcp(server:tool)` | MCP tools |

Relative paths are workspace-scoped, absolute paths can reach outside it, globs use `**`, `*`, and `?`, and deny overrides allow.

Auto-review reads plain-English guidance from `~/.cursor/permissions.json` and `<project>/.cursor/permissions.json`, merged, as `autoRun.allow_instructions` and `autoRun.block_instructions`; a team dashboard configuration replaces both files. `sandbox.json` separately controls what sandboxed commands reach ([run-modes.md](https://cursor.com/docs/agent/security/run-modes.md)).

### Plugins and workers

| Command | Surface |
| --- | --- |
| `agent plugin`, `/plugin` | plugins and marketplaces; `agent plugin marketplace add <git-url>` with `--git-ref`, `list` (`--format json`), `update`, `remove` |
| `agent mcp login\|list\|list-tools\|enable\|disable` | MCP servers from `.cursor/mcp.json` or `~/.cursor/mcp.json` |
| `agent worker` | self-hosted Cloud Agent worker; `--pool [name]` (legacy alias `--single-use`), `--worker-dir`, `--idle-release-timeout`, `--computer-use`, `--share-desktop`; runs `sessionStart` and `sessionEnd` hooks on claim and release |
| `agent install-shell-integration`, `uninstall-shell-integration` | edits `~/.zshrc` |
| `agent generate-rule`, `agent rule` | creates a Cursor rule |

Plugins, worktree setup commands, MCP server commands, hooks, and the status line command are the CLI surfaces that execute configured commands.

## Upstream scope

Cursor positions the CLI as the terminal form of its agent, with ACP for custom clients ([overview.md](https://cursor.com/docs/cli/overview.md), [acp.md](https://cursor.com/docs/cli/acp.md)). Its docs are rolling and describe the IDE and CLI together, so a documented hook or field can be IDE-only; the tables above mark where the CLI's wire differs. The gaps RimZ records against this surface are in the internals page's [Known gaps](../../internals/agents/adapter_cursor.md#known-gaps).

## Undocumented behaviour

1. Whether the Cursor backend issues `subagentStart` and `subagentStop` requests to the CLI on the baseline.
2. Whether a message steered into a running turn ([changelog, August 11, 2026](https://cursor.com/docs/cli/changelog.md)) fires `beforeSubmitPrompt`.
3. Any local event for permission prompts, questions, plan approval, or idle; and any event after compaction.
4. The transcript record schema, enablement, rotation, and durability, and how resume, `/clear`, `/fork`, and `/summarize` change the file.
5. The chat store and `meta.json` formats, and fork lineage.
6. Whether the per-turn token fields on `stop` and `afterAgentResponse` count cache tokens inside `input_tokens`.
7. `status` and `about` responses when logged out, expired, on an API key, on a service account, behind a proxy, or on server error; and the credential store's location.
8. The shell that runs a hook `command` string, and any stdout size limit.
9. Whether headless `session_id` equals the hook `conversation_id` across `--resume`.
10. The status line payload's versioning, and whether its `session_id` equals `conversation_id`.
11. A machine-readable per-user usage, spend, or quota source.

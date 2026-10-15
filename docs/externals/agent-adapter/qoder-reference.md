# Qoder CLI protocol reference

This page records Qoder CLI's upstream integration surfaces for contributors evaluating an adapter. It makes no claim of RimZ support. The [adapter playbook](../../contributing/agent-adapters.md) owns the integration workflow.

## Evidence boundary and sources

Baseline: official rolling CLI documentation read on 2026-10-15. No installed release, minimum compatible version, or executable alias was verified. The documented executable is `qoder`. No Qoder installation, login, provider call, hook write, or private transcript read was performed.

The tables describe upstream contracts, not captured emissions. CLI and IDE hooks have separate documentation. This page uses the CLI pages only.

| Surface | Official source |
| --- | --- |
| Executable and installation | [Installation](https://docs.qoder.com/cli/installation) |
| Launch flags | [CLI reference](https://docs.qoder.com/cli/cli-reference) |
| Configuration and paths | [Settings](https://docs.qoder.com/cli/settings), [settings reference](https://docs.qoder.com/cli/settings-reference), [configuration scope](https://docs.qoder.com/cli/config-scope) |
| Sessions and storage | [Session management](https://docs.qoder.com/cli/sessions) |
| Hooks and event schemas | [Hooks](https://docs.qoder.com/cli/hooks), [hooks reference](https://docs.qoder.com/cli/hooks-reference) |
| Permissions and execution | [Permissions](https://docs.qoder.com/cli/permissions), [working modes](https://docs.qoder.com/cli/working-modes), [scripts](https://docs.qoder.com/cli/run-in-scripts) |
| Models and context | [Models](https://docs.qoder.com/cli/model), [custom models](https://docs.qoder.com/cli/custom-models), [slash commands](https://docs.qoder.com/cli/slash-reference) |
| Skills and native children | [Skills](https://docs.qoder.com/cli/Skills), [subagents](https://docs.qoder.com/cli/subagent) |
| Authentication and usage | [Authentication](https://docs.qoder.com/cli/authentication), [usage](https://docs.qoder.com/cli/usage) |
| Editor transport | [ACP](https://docs.qoder.com/cli/acp) |

The official [documentation index](https://docs.qoder.com/llms.txt) lists the case-sensitive `cli/Skills` path. The lowercase path is not the source used here.

## Launch and configuration

The [CLI reference](https://docs.qoder.com/cli/cli-reference) documents `qoder [options] [prompt text]`. A positional prompt selects non-interactive execution. An interactive launch with an initial task uses `--prompt-interactive`.

| Form or flag | Documented behavior |
| --- | --- |
| `qoder` | Open an interactive session. |
| `qoder --prompt-interactive 'Review the authentication flow'` | Run the supplied task, then enter interactive mode. Alias: `-i`. |
| `qoder -p 'Review the authentication flow'` | Print one response and exit. Alias: `--print`. |
| `--cwd <path>` | Set the working directory. Default: current directory. |
| `--config-dir <path>` | Override the user configuration directory. Default: `~/.qoder`. |
| `--settings <path-or-json>` | Highest-priority settings. A value beginning with `{` is inline JSON. |
| `--setting-sources <sources>` | Select configuration sources to load. |
| `--system-prompt <text>` | Replace the system prompt. |
| `--append-system-prompt <text>` | Append to the default system prompt. |
| `--max-turns <number>` | Limit turns per query. |
| `--output-format <format>` | `text` (default), `json`, or `stream-json`. Alias: `-o`. |
| `--input-format <format>` | `text` (default) or `stream-json`. |
| `--no-session-persistence` | Disable session writes and restoration. Valid only with `--print`. |
| `--acp` | Serve ACP over stdin/stdout. |

These are documented command forms, not tested examples. Leading-dash prompts and the parser's handling of `--` remain unverified. No generic positional-prompt rule should replace the interactive form above.

The [settings page](https://docs.qoder.com/cli/settings) orders precedence: defaults, user settings, project settings, local settings, then `--settings`. Objects merge recursively. Scalars override earlier values. Some arrays combine and deduplicate, while other arrays replace earlier values.

| File or variable | Scope |
| --- | --- |
| `~/.qoder/settings.json` | User settings. |
| `<project>/.qoder/settings.json` | Shared project settings. |
| `<project>/.qoder/settings.local.json` | Local project settings. |
| `QODER_CONFIG_DIR` | Custom user configuration directory. Default: `~/.qoder`. |

Project and local settings load only in trusted directories. Folder trust is enabled by default through `security.folderTrust.enabled`. The [hooks page](https://docs.qoder.com/cli/hooks#configuration) explicitly says same-event hooks combine across the three files. It does not settle hook-array merging for `--settings`. Precedence between `--config-dir` and `QODER_CONFIG_DIR` is not established here.

## Sessions and transcripts

The [sessions page](https://docs.qoder.com/cli/sessions) describes project-isolated sessions with unique IDs. It gives a UUID example without defining the accepted ID grammar.

| Operation | Documented surface |
| --- | --- |
| New session | `qoder`, or `/new` within the terminal UI. |
| Clear conversation | `/clear` clears history and starts a new session. |
| Continue latest | `--continue` or `-c`, for the current project. Interactive equivalent: `/continue`. |
| Resume by ID | `--resume <id>` or `-r <id>`. |
| Choose a session | `--resume` without an ID, or `/resume`. |
| Specify an ID | `--session-id <id>`. The sessions page says specify an ID. The CLI reference says resume that ID. Missing-ID behavior needs verification. |
| Fork a restored session | `--fork-session` with `--resume` or `--session-id`. The original remains intact. |
| Branch in the UI | `/branch`. |
| Name or rename | `--name <name>`, `-n <name>`, or `/rename`. |
| List or delete | `--list-sessions`, then `--delete-session <index>`. Deletion takes a list index. |
| Export | `/export` writes a conversation file or copies it to the clipboard. |

Default storage is documented as:

```text
~/.qoder/projects/<processed-project-path-name>/<session-id>.jsonl
~/.qoder/projects/<processed-project-path-name>/<session-id>/state.json
```

The JSONL file holds conversation logs. `state.json` holds session state. These paths follow the user configuration directory. The processing algorithm for project paths and both record schemas are unspecified on the sessions page.

Hooks supply `session_id` and `transcript_path`. No captured record proves their relationship to filenames or child-session IDs. Do not infer a Claude transcript schema from similar hook names.

## Hook configuration and transport

The [hooks page](https://docs.qoder.com/cli/hooks#configuration) defines an event map. Each event contains matcher groups, and each group contains hook entries. This structural example follows that schema and has not been executed:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/check-command",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
```

| Group field | Contract |
| --- | --- |
| `matcher` | Optional. Omission or `*` matches all. Supports exact values, pipe alternatives, and regex. |
| `hooks` | Required array of entries. |
| `async` | Run the group in the background. Results become context on the next model turn. |

The entry-level `if` filter accepts `ToolName` or `ToolName(arg_pattern)`. The argument pattern is a glob against the primary argument, such as Bash's `command` or Edit's `file_path`.

| Entry type | Execution and defaults |
| --- | --- |
| `command` | Shell command. `command` is required. Timeout defaults to 600 seconds. |
| `http` | POST the input JSON to `url`. Expect JSON hook output. Timeout defaults to 600 seconds. |
| `prompt` | Isolated model call returning `{ok, reason}`. `ok: false` blocks. Default timeout: 30 seconds. |
| `agent` | Isolated child using `StructuredOutput` with `{ok, reason}`. Defaults: 50 turns, 60 seconds. |

Command entries accept `shell` (`bash` or `powershell`), extra `env`, and an optional `args` array. With `args`, Qoder executes a binary directly and ignores `shell`. Shell quoting and globbing do not apply to exec-form arguments.

Other command controls include `async`, `asyncRewake`, `rewakeMessage`, `rewakeSummary`, `once`, and `statusMessage`. `asyncRewake` wakes the model after exit 2 using a system reminder. `once` removes a hook after successful execution only for session-scoped hooks. Background hooks cannot synchronously gate the current operation.

Hook stdin is JSON. The [common input contract](https://docs.qoder.com/cli/hooks#input) names:

| Field | Meaning |
| --- | --- |
| `session_id` | Current session identity. |
| `transcript_path` | Current conversation log path. |
| `cwd` | Current working directory. |
| `hook_event_name` | Native event name. |
| `permission_mode` | Permission mode, when provided. |
| `agent_id`, `agent_type` | Agent identity and type, when provided. |

Documented hook environment variables are `QODER_PROJECT_DIR`, `QODER_PLUGIN_ROOT`, and `QODER_PLUGIN_DATA`. Plugin variables apply to plugin-owned hooks. These are not a provider PID contract. Process ancestry and inherited room-variable behavior remain unverified.

## Hook events and payloads

The [hooks guide](https://docs.qoder.com/cli/hooks#event-reference) details 23 events. The [hooks reference](https://docs.qoder.com/cli/hooks-reference#event-types) lists 27. Its four additional names lack detailed payload contracts there: `TaskCreated`, `TaskCompleted`, `TeammateIdle`, and `Setup`. Their availability and version boundaries remain unknown.

The following table covers the 23 detailed events. Fields are additional to common stdin fields. "No" means exit 2 has no documented blocking effect.

| Event | Matcher target | Additional input fields | Exit 2 effect |
| --- | --- | --- | --- |
| `SessionStart` | `source` | `source`, `model` | No. |
| `SessionEnd` | `reason` | `reason` | No. |
| `UserPromptSubmit` | None | `prompt` | Reject the prompt. |
| `PreToolUse` | Tool name | `tool_name`, `tool_input`, `tool_use_id` | Prevent execution. |
| `PostToolUse` | Tool name | `tool_name`, `tool_input`, `tool_response`, `tool_use_id` | No. |
| `PostToolUseFailure` | Tool name | `tool_name`, `tool_input`, `tool_use_id`, `error`, `error_type`, `is_interrupt` | No. |
| `PermissionRequest` | Tool name | `tool_name`, `tool_input`, `permission_suggestions` | No. Use the JSON decision. |
| `PermissionDenied` | Tool name | `tool_name`, `tool_input`, `tool_use_id`, `reason` | No. |
| `Stop` | None | `stop_hook_active`, `last_assistant_message` | Continue the main conversation. |
| `StopFailure` | `error_type` | `error_type`, `error`, `error_details`, `last_assistant_message` | Output and exit code ignored. |
| `SubagentStart` | Agent type | `agent_id`, `agent_type` | No. |
| `SubagentStop` | Agent type | `agent_id`, `agent_type`, `stop_hook_active`, `agent_transcript_path`, `last_assistant_message` | Continue the child's conversation. |
| `PreCompact` | `trigger` | `trigger`, `custom_instructions` | Prevent compaction. |
| `PostCompact` | `trigger` | `trigger`, `compact_summary` | No. |
| `Notification` | `notification_type` | `notification_type`, `message`, `title`, `details` | No. |
| `InstructionsLoaded` | `load_reason` | `file_path`, `memory_type`, `load_reason`, `globs`, `trigger_file_path`, `parent_file_path` | Output and exit code ignored. |
| `ConfigChange` | `source` | `source`, `file_path` | Reject application to this session, except policy changes. |
| `CwdChanged` | None | `old_cwd`, `new_cwd` | No. |
| `FileChanged` | Filename basename | `file_path`, `event` | No. |
| `WorktreeCreate` | None | `name` | Every nonzero exit fails creation. |
| `WorktreeRemove` | None | `worktree_path` | Notification only. Failures surface through stderr. |
| `Elicitation` | `mcp_server_name` | `mcp_server_name`, `message`, `mode`, `url`, `elicitation_id`, `requested_schema` | Decline the request. |
| `ElicitationResult` | `mcp_server_name` | `mcp_server_name`, `action`, `content`, `mode`, `elicitation_id` | Replace the response with decline. |

The guide documents these selector values:

| Field | Values |
| --- | --- |
| `SessionStart.source` | `startup`, `resume`, `clear`, `compact`, `new`. |
| `SessionEnd.reason` | `clear`, `resume`, `logout`, `prompt_input_exit`, `bypass_permissions_disabled`, `other`. |
| `StopFailure.error_type` | `rate_limit`, `authentication_failed`, `billing_error`, `invalid_request`, `server_error`, `max_output_tokens`, `unknown`. |
| Compaction `trigger` | `manual`, `auto`. |
| `Notification.notification_type` | `permission_prompt`, `idle_prompt`, `auth_success`, `elicitation_dialog`, `elicitation_response`, `elicitation_complete`. |
| `InstructionsLoaded.load_reason` | `session_start`, `nested_traversal`, `path_glob_match`, `include`, `compact`. |
| `ConfigChange.source` | `user_settings`, `project_settings`, `local_settings`, `policy_settings`, `skills`, `agents`. Policy changes always apply. |
| `FileChanged.event` | `change`, `add`, `unlink`. |

Tool examples include `Bash`, `Write`, `Edit`, `Read`, `Glob`, and `Grep`. MCP tools use `mcp__server__tool`. MCP tool hooks can also carry `mcp_context` and `original_request_name`. Tool responses have tool-dependent shapes.

`Stop` denotes a response ending without pending tool calls. Its `stop_hook_active` flag identifies a turn driven by a Stop hook. `StopFailure` denotes an unexpected error. Neither establishes complete cancellation coverage. `PostToolUseFailure.is_interrupt` is a documented sample field, not proof of a turn-wide interruption event.

## Hook replies and approval decisions

The [output contract](https://docs.qoder.com/cli/hooks#output) uses command exit status and stdout together:

| Exit status | Meaning |
| --- | --- |
| `0` | Parse JSON stdout. Non-JSON text supplies context only for `SessionStart` and `UserPromptSubmit`. |
| `2` | Apply the event-specific blocking effect above. Stderr supplies feedback. |
| Other | Continue the main flow. Ignore stdout and log stderr as diagnostics. |

Event-specific exceptions, such as `StopFailure` and `WorktreeCreate`, take precedence over this general rule.

| Common JSON field | Effect |
| --- | --- |
| `continue: false` | Request that execution stop. `stopReason` explains why. |
| `suppressOutput` | Hide hook output from the user. |
| `systemMessage` | Display a message to the user. It does not enter model context. |
| `decision` | Top-level `allow` or `deny`. Denial has the event's exit-2 effect. |
| `reason` | Explain the decision. |
| `hookSpecificOutput` | Carry event-specific fields. Requires `hookEventName`. |

A missing `hookSpecificOutput.hookEventName` rejects the entire JSON response. For example, this documented shape requests a PreToolUse permission decision:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "ask"
  }
}
```

The [permissions page](https://docs.qoder.com/cli/permissions#pretooluse-hook) says `ask` continues the normal permission pipeline. The [hooks page](https://docs.qoder.com/cli/hooks#permissionrequest) describes it as the route to interactive authorization. These descriptions do not prove an unconditional prompt. Actual approval behavior needs a live check.

| Event | Fields inside `hookSpecificOutput`, alongside `hookEventName` |
| --- | --- |
| `SessionStart` | `additionalContext`. |
| `UserPromptSubmit` | `additionalContext`, `sessionTitle`. |
| `PreToolUse` | `permissionDecision` (`allow`, `deny`, `ask`), `permissionDecisionReason`, `updatedInput`, `additionalContext`. Specific decision and reason override top-level fields. |
| `PostToolUse` | `updatedToolOutput`, `updatedMCPToolOutput`, `additionalContext`. General output replacement takes precedence over MCP-only replacement. |
| `PostToolUseFailure` | `additionalContext`. |
| `PermissionRequest` | Nested `decision` object, described below. |
| `PermissionDenied` | `retry: true` requests another tool attempt. |
| `Stop`, `SubagentStop` | `clearContext: true`. |
| `SubagentStart`, `Notification` | `additionalContext`. |
| `CwdChanged`, `FileChanged` | `additionalContext`, `watchPaths` with absolute paths. |
| `WorktreeCreate` | `worktreePath`, an absolute path. Plain stdout can also supply the path. |
| `Elicitation`, `ElicitationResult` | `action` (`accept`, `decline`, `cancel`) and `content`. |

`PermissionRequest.decision.behavior` accepts `allow` or `deny`, with no `ask` behavior. Allow decisions can include `updatedInput` and `updatedPermissions`. Deny decisions can include `message` and `interrupt`. This full envelope illustrates the nesting without granting approval:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PermissionRequest",
    "decision": {
      "behavior": "deny",
      "message": "The command needs manual review.",
      "interrupt": false
    }
  }
}
```

Neither this schema nor an exit-0 definition proves that empty stdout preserves Qoder's native approval prompt. Empty-response neutrality remains unverified.

## Permissions and working modes

The [permission pipeline](https://docs.qoder.com/cli/permissions) runs PreToolUse first. An explicit allow or deny short-circuits normal checks. Otherwise, rules, safety checks, and the selected mode decide. A remaining `ask` runs PermissionRequest before the runtime handles confirmation.

| `--permission-mode` value | Documented behavior |
| --- | --- |
| `default` | Safe reads run automatically. Sensitive actions need approval. |
| `accept_edits` | Approve safe edits within working directories. Other checks remain. |
| `auto` | Delegate authorization to the agent. |
| `bypass_permissions` | Skip normal approval prompts, while retaining specified destructive-path protections. |
| `dont_ask` | Deny actions that would require confirmation. |
| `plan` | Compatibility value: use `default` permission behavior and enter Plan work state. |

`--yolo` and `--dangerously-skip-permissions` alias `bypass_permissions`. Non-default modes require a trusted directory. Otherwise, Qoder falls back to `default`. A PreToolUse denial still blocks in bypass mode.

A remaining `ask` opens a prompt in the terminal UI and becomes denial in plain headless execution. SDK hosts receive `canUseTool`. ACP clients receive `requestPermission`. The [script guide](https://docs.qoder.com/cli/run-in-scripts) also distinguishes host-driven `stream-json` confirmation from plain headless mode.

[Plan](https://docs.qoder.com/cli/working-modes#plan-before-execution-plan) is a work state for read-only exploration before approved execution. `/plan` enters or exits it. The external reference does not equate Qoder's work states with RimZ permission presets.

## Models, context, skills, and native children

The [model page](https://docs.qoder.com/cli/model) documents `--model`, `--reasoning-effort`, and `--context-window` as session-only overrides. `--list-models` prints the available catalog. Model availability, effort support, and context limits depend on the current server-side catalog.

[Custom models](https://docs.qoder.com/cli/custom-models) use the BYOK wizard in `/model`. The page warns against manual BYOK configuration in `settings.json`. It does not define a stable credential-file schema.

The [slash reference](https://docs.qoder.com/cli/slash-reference) documents `/compact`. Hooks distinguish manual from automatic compaction and expose a post-compaction summary. No numeric live-context hook gauge is specified on the hooks page.

| Extension | Documented contract |
| --- | --- |
| Skills | `<user-config>/skills/<name>/SKILL.md` or `<project>/.qoder/skills/<name>/SKILL.md`. Metadata requires `name` and `description`. |
| Skill activation | Automatic selection or `/skill-name`. The Skills page says user definitions win same-name project conflicts. |
| Subagent files | User `agents/*.md` or project `.qoder/agents/*.md`. YAML frontmatter supplies configuration, followed by the system prompt. |
| Subagent precedence | Built-in, user, project, plugin, then `--agents` JSON, from lowest to highest. |
| Main-thread agent | `--agent <name>` selects a loaded definition for the main session. |
| Temporary children | `--agents` defines process-scoped children as a JSON object keyed by name. |
| Child controls | Markdown fields include `tools`, `disallowedTools`, `skills`, `model`, `permissionMode`, `maxTurns`, `timeoutMins`, and session-scoped `hooks`. |
| Child lockdown | `disallowedTools: [Agent]` prevents further native child dispatch. This is a child-definition control. |

Sources: [Skills](https://docs.qoder.com/cli/Skills) and [subagents](https://docs.qoder.com/cli/subagent). The temporary JSON definition supports fewer fields than Markdown. It excludes `hooks`, `memory`, `background`, `isolation`, `temperature`, and `timeoutMins`. Plugin child definitions remove `hooks`, `mcpServers`, and `permissionMode` under a separate safety policy.

The CLI reference provides `--tools`, `--allowed-tools`, and `--disallowed-tools` for tool control. These do not establish a main-session skill allowlist. No such allowlist is verified here.

## Authentication, account usage, and output schemas

The [authentication page](https://docs.qoder.com/cli/authentication) documents interactive browser sign-in, a token option, and `QODER_PERSONAL_ACCESS_TOKEN` for automation. An environment PAT takes precedence over saved login credentials. Browser credentials refresh automatically. `/login` and `/logout` manage interactive authentication. Clearing a PAT requires removing the environment variable as well.

| Surface | Evidence and limit |
| --- | --- |
| `/status` | Interactive Usage and Status tabs. The authentication page describes quota and session information. |
| `/usage` | Plan and billing-cycle Credits, plus session duration and code-change statistics. [Usage](https://docs.qoder.com/cli/usage) describes Credits as the billing unit. |
| `statusLine.command` | Receives session JSON on stdin. The [settings reference](https://docs.qoder.com/cli/settings-reference#statusline-status-bar) does not supply its field schema. |
| `--output-format json` | Final result and metadata in one object. The [script guide](https://docs.qoder.com/cli/run-in-scripts) does not enumerate result fields. |
| `--output-format stream-json` | Line-oriented messages. The script guide does not enumerate message types or token fields. |
| Durable usage and cost | The sessions page specifies log locations, without token, cache, model, or dollar fields. |

These pages establish user-visible usage and structured transports. They do not establish a machine-readable account/quota API, a dollar conversion for Credits, or a durable per-turn spending schema. Credential storage paths and refresh-token record formats remain unverified.

## Live-validation gaps

The following questions require evidence beyond this documentation baseline. No observed fixtures accompany this reference.

| Question | Missing evidence |
| --- | --- |
| Which releases emit these events? | Installed-version help and event captures, including the four catalog-only events. |
| Do hooks fire in interactive and print modes? | Captures from each launch form and configuration source. |
| Does empty success preserve native approval? | A real approval prompt before and after the hook response. |
| Does PreToolUse `ask` force a prompt? | A live decision after the normal permission pipeline. |
| How do per-run hooks combine? | `--settings` tested alongside existing user, project, and local hooks. |
| Which process owns a hook? | Provider PID, helper ancestry, and environment captures. |
| What happens on cancel or failure? | Interrupted turns, failed tools, provider errors, and process-death captures. |
| How do IDs change? | `/new`, `/clear`, compaction, resume, fork, and child-session records. |
| What do files and streams contain? | Redacted JSONL, state, statusline, and structured-output schemas with usage fields. |
| How are prompts parsed? | Interactive initial prompts, leading dashes, and option separators. |
| How is the configuration root selected? | Conflicting flag and environment settings across session and hook paths. |

Any live validation needs approval before installation, authenticated calls, credential access, private-log inspection, or provider-home changes. A future adapter must keep these gaps explicit until the corresponding behavior is verified.

# Claude Code protocol reference

This page mirrors the Claude Code surfaces RimZ binds to: hook events with their stdin payloads and stdout decisions, the statusline JSON, the launch flags, agent view, Remote Control, project storage and managed pricing, the auth and OAuth usage surface, and the transcript JSONL. It records what upstream ships. How RimZ maps each surface onto its own types is [adapter_claude.md](../../internals/agents/adapter_claude.md); the agent-neutral model is [model.md](../../internals/agents/model.md) and the account and spend model is [providers.md](../../internals/agents/providers.md).

Coverage is depth on what RimZ wires and breadth as an index. The hook events, statusline fields, and decision shapes the adapter parses or emits get full shapes; the rest of the upstream catalog is listed so a contributor wiring something new knows it exists.

**Baseline.** Claude Code 2.1.270, the npm `latest` dist-tag, tag [`v2.1.270`](https://github.com/anthropics/claude-code/releases/tag/v2.1.270) at commit `2b40e76d3f03b9070e2431e0bd05b4f3ace77982`, released 2026-09-12. The official docs, the changelog, and the installed 2.1.270 binary were read on 2026-09-13. Upstream publishes no source, so claims that no docs page covers are marked as read from the 2.1.270 binary. A section pinned to another build names that build inline.

## Upstream sources

`docs.claude.com/en/docs/claude-code/*` redirects to `code.claude.com/docs/en/*`; the `code.claude.com` form is canonical. Every page also serves raw markdown at the same URL with a `.md` suffix.

| Surface | Source |
| --- | --- |
| Hooks: configuration, matchers, input, output, exit codes, every event | <https://code.claude.com/docs/en/hooks> |
| Statusline JSON and `subagentStatusLine` | <https://code.claude.com/docs/en/statusline> |
| CLI flags | <https://code.claude.com/docs/en/cli-reference>, <https://code.claude.com/docs/en/cli-usage> |
| Settings keys | <https://code.claude.com/docs/en/settings-reference>, <https://code.claude.com/docs/en/managed-settings> |
| Environment variables | <https://code.claude.com/docs/en/env-vars> |
| Auto-compaction window | <https://code.claude.com/docs/en/model-config#set-the-auto-compact-window> |
| Sessions, transcript location, project directory name | <https://code.claude.com/docs/en/sessions> |
| Agent view | <https://code.claude.com/docs/en/agent-view> |
| Remote Control | <https://code.claude.com/docs/en/remote-control> |
| Credential storage | <https://code.claude.com/docs/en/authentication> |
| Release history and version boundaries | <https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md> |
| `claude auth status` JSON, Keychain item name, OAuth usage endpoint, bridge pointer, transcript JSONL | No public schema; read from the 2.1.270 binary and its output |

## Hooks

A hook is a handler Claude Code runs at a lifecycle point. For a command handler, Claude Code writes the event's JSON payload to stdin and reads stdout and the exit code as the result ([hooks](https://code.claude.com/docs/en/hooks)).

### Configuration

Hooks live under `hooks.<EventName>[]` in user, project, local, and managed settings, in a plugin's `hooks/hooks.json`, and in skill or agent frontmatter. Each entry is a matcher group: an optional `matcher` and a `hooks` array of handlers. All matching handlers run in parallel, and an identical handler defined in several settings files runs once ([hook handler fields](https://code.claude.com/docs/en/hooks#hook-handler-fields)).

A matcher is evaluated by the characters it contains ([matcher patterns](https://code.claude.com/docs/en/hooks#matcher-patterns)):

| Matcher value | Evaluated as |
| --- | --- |
| `"*"`, `""`, or omitted | matches every occurrence |
| only letters, digits, `_`, `-`, spaces, `,`, and `\|` | an exact name, or a list of exact names separated by `\|` or `,`; commas need 2.1.191 and hyphens need 2.1.195 |
| anything else | an unanchored JavaScript regular expression |

`FileChanged` and `StopFailure` use a narrower exact set of letters, digits, `_`, and `|`. `UserPromptSubmit`, `Stop`, `PostToolBatch`, `TeammateIdle`, `TaskCreated`, `TaskCompleted`, `WorktreeCreate`, `WorktreeRemove`, `MessageDisplay`, and `CwdChanged` ignore matchers.

Handlers come in five types. `SessionStart` runs only `command` and `mcp_tool`; `PreModelSwitch` runs only `command`, `http`, and `mcp_tool`.

| `type` | Runs | Type-specific fields |
| --- | --- | --- |
| `command` | a shell command, or an executable when `args` is set | `command`, `args`, `async`, `asyncRewake`, `shell` (`bash` or `powershell`) |
| `http` | a POST of the payload to a URL; the response body carries the JSON output | `url`, `headers`, `allowedEnvVars` |
| `mcp_tool` | a tool on a connected MCP server; its text output is read like stdout | `server`, `tool`, `input` |
| `prompt` | a single-turn model evaluation | `prompt`, `model` |
| `agent` | a subagent with tool access (experimental) | `prompt`, `model` |

Every handler also accepts these fields ([common fields](https://code.claude.com/docs/en/hooks#common-fields)):

| Field | Meaning |
| --- | --- |
| `if` | one permission rule, such as `Bash(git *)`, evaluated only on tool events |
| `timeout` | seconds before cancel. Defaults: 600 for `command`, `http`, `mcp_tool`; 30 for `prompt`; 60 for `agent`. `UserPromptSubmit`, `PreModelSwitch`, and `PostModelSwitch` lower the first three to 30, and `MessageDisplay` to 10 |
| `statusMessage` | spinner text while the handler runs |
| `once` | remove after the first successful run; honored only in skill frontmatter |

A hook process inherits Claude Code's environment minus the `OTEL_*` exporter variables. `CLAUDE_EFFORT` carries the effort level, `CLAUDE_CODE_REMOTE` is `true` in cloud sessions, and `CLAUDE_CODE_BRIDGE_SESSION_ID` holds the `session_…` ID while a Remote Control connection is active (2.1.199 and later).

### Common input

Every event receives these fields beside its event-specific ones ([common input fields](https://code.claude.com/docs/en/hooks#common-input-fields)):

| Field | Meaning |
| --- | --- |
| `session_id` | current session ID |
| `prompt_id` | UUID of the user prompt being processed, equal to the OpenTelemetry `prompt.id`; absent before the first input; 2.1.196 and later |
| `transcript_path` | the session transcript; written asynchronously, so it can lag the current turn |
| `cwd` | working directory when the hook fires |
| `scratchpad_dir` | the session scratchpad directory, absent when there is none; 2.1.257 and later |
| `permission_mode` | `default`, `plan`, `acceptEdits`, `auto`, `dontAsk`, or `bypassPermissions`; the Manual mode arrives as `default`; not sent on every event |
| `effort` | `{ "level": "low" \| "medium" \| "high" \| "xhigh" \| "max" }`, the level actually run; sent on tool-use-context events (`PreToolUse`, `PostToolUse`, `Stop`, `SubagentStop`) when the model supports effort |
| `hook_event_name` | the event that fired |
| `agent_id` | subagent ID; present only when the hook fires inside a subagent |
| `agent_type` | agent name; present inside a subagent or when the session runs with `--agent`; a subagent's type wins |

Only `SessionStart` can receive `model`, and it can be omitted, for example after `/clear`. `PreModelSwitch` and `PostModelSwitch` carry `from_model` and `to_model` instead, so they are the upstream way to follow mid-session model changes. A model ID can carry a `[1m]` suffix that selects the 1,000,000-token context window ([model configuration](https://code.claude.com/docs/en/model-config)).

### Output

Claude Code reads stdout as JSON when it starts with `{` and ends with `}`, ignoring surrounding whitespace; any other stdout is plain text ([exit code 0](https://code.claude.com/docs/en/hooks#exit-code-0)). JSON is read on every exit code. A parse failure or a schema-validation failure is a non-blocking `<hook name> hook error` on every exit code except 2. Output strings, `additionalContext` and plain stdout included, are capped at 10,000 characters; longer text is saved to a file and replaced with a preview and path.

The JSON object carries universal fields, a top-level `decision` and `reason` on the events that use them, and `hookSpecificOutput` with a required `hookEventName` ([JSON output](https://code.claude.com/docs/en/hooks#json-output)):

| Universal field | Default | Meaning |
| --- | --- | --- |
| `continue` | `true` | `false` stops Claude after the hook; wins over any event decision |
| `stopReason` | none | message shown when `continue` is `false` |
| `suppressOutput` | `false` | accepted and ignored; a successful hook's stdout never reaches the transcript |
| `systemMessage` | none | warning shown to the user |
| `terminalSequence` | none | OSC `0`, `1`, `2`, `9`, `99`, `777`, or BEL for Claude Code to emit; anything else voids the field; interactive sessions only |

### Exit codes

The exit code and the JSON act together ([exit code output](https://code.claude.com/docs/en/hooks#exit-code-output)):

| Exit | Effect |
| --- | --- |
| `0` | success. Plain stdout becomes context for `UserPromptSubmit`, `UserPromptExpansion`, `SessionStart`, and `PostModelSwitch`, and goes to the debug log elsewhere. Stderr goes to the debug log |
| `2` | blocking error on the events that can block. It blocks even beside JSON `allow` and even when the JSON fails validation (2.1.214 and later). The block message is the JSON decision's reason when there is one, otherwise stderr |
| other | valid JSON alone decides and no error is reported. With plain or empty stdout, a non-blocking `<hook name> hook error` notice shows the first stderr line prefixed `Failed with non-blocking status code:` |
| timeout | the handler is canceled and its output discarded, so it renders no decision. A `PreModelSwitch` timeout blocks the switch |

Exit 2 depends on the event ([exit code 2 behavior per event](https://code.claude.com/docs/en/hooks#exit-code-2-behavior-per-event)). It blocks `PreToolUse`, `UserPromptSubmit`, `UserPromptExpansion`, `Stop`, `SubagentStop`, `TeammateIdle`, `TaskCreated`, `TaskCompleted`, `ConfigChange`, `PostToolBatch`, `PreCompact`, `PreModelSwitch`, `Elicitation`, and `ElicitationResult`; `WorktreeCreate` and `WorktreeRemove` fail on any nonzero exit. On `PermissionRequest` it is not honored and the permission flow proceeds. `PostToolUse` and `PostToolUseFailure` show stderr to Claude. `StopFailure` ignores output and exit code except `terminalSequence`. `SessionStart`, `SubagentStart`, `SessionEnd`, `PostCompact`, `PostModelSwitch`, `CwdChanged`, and `FileChanged` show stderr to the user only. `DirectoryAdded` sends stderr to the debug log, and `PermissionDenied`, `Notification`, `Setup`, `InstructionsLoaded`, and `MessageDisplay` ignore it.

### Decision control

Each event honors its own decision fields ([decision control](https://code.claude.com/docs/en/hooks#decision-control)):

| Events | Pattern | Fields |
| --- | --- | --- |
| `UserPromptSubmit`, `UserPromptExpansion`, `PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `Stop`, `SubagentStop`, `ConfigChange`, `PreCompact` | top-level `decision` | `decision: "block"`, `reason`; `Stop` and `SubagentStop` also take `hookSpecificOutput.additionalContext` |
| `TeammateIdle`, `TaskCompleted` | exit code or `continue: false` | exit 2 blocks; `continue: false` stops the teammate |
| `TaskCreated` | exit code or top-level `decision` | exit 2 or `decision: "block"` cancels the task; `continue: false` is ignored |
| `PreToolUse` | `hookSpecificOutput` | `permissionDecision` (`allow` \| `deny` \| `ask` \| `defer`), `permissionDecisionReason`, `updatedInput`, `additionalContext` |
| `PreModelSwitch` | `hookSpecificOutput` or top-level `decision` | `permissionDecision` (`allow` \| `deny` \| `ask`), `permissionDecisionReason`; `decision: "block"` cancels |
| `PermissionRequest` | `hookSpecificOutput` | `decision.behavior` (`allow` \| `deny`), `decision.updatedInput`, `decision.updatedPermissions`, `decision.message`, `decision.interrupt` |
| `PermissionDenied` | `hookSpecificOutput` | `retry: true` |
| `WorktreeCreate` | path return | a command prints the path; an HTTP hook returns `hookSpecificOutput.worktreePath` |
| `WorktreeRemove` | exit code | JSON is discarded |
| `Elicitation`, `ElicitationResult` | `hookSpecificOutput` | `action` (`accept` \| `decline` \| `cancel`), `content` |
| `MessageDisplay` | `hookSpecificOutput` | `displayContent`, display only |
| `SessionStart`, `SubagentStart`, `PostModelSwitch` | context only | `hookSpecificOutput.additionalContext`; `SessionStart` also takes `initialUserMessage`, `watchPaths`, `sessionTitle`, `reloadSkills` |
| `Setup`, `Notification`, `SessionEnd`, `PostCompact`, `InstructionsLoaded`, `StopFailure`, `CwdChanged`, `DirectoryAdded`, `FileChanged` | none | side effects only |

### Hooks RimZ wires

RimZ installs these 13 events; what each one means to RimZ, and the exact bytes it renders, are in [adapter_claude.md → Hooks and lifecycle](../../internals/agents/adapter_claude.md#hooks-and-lifecycle). The input columns list event-specific fields on top of the [common input](#common-input).

| Event | Fires | Matcher filters | Event-specific input |
| --- | --- | --- | --- |
| `SessionStart` | a session starts, resumes, clears, compacts, or forks | `source` | `source` (`startup` \| `resume` \| `clear` \| `compact` \| `fork`; `fork` from 2.1.214, reported as `resume` before), optional `model`, `agent_type`, `session_title` |
| `UserPromptSubmit` | a prompt is submitted, before processing | none | `prompt`; bracketed pastes arrive as `<pasted_content id="X">`, pasted text, and `</pasted_content id="X">`, each tag alone on its line and the close tag repeating the id, including pastes inside a longer prompt. **Observed in 2.1.278, not sourced**: no upstream documentation of this envelope was found |
| `PreToolUse` | before a tool call | tool name | `tool_name`, `tool_input`, `tool_use_id`; file-tool paths arrive absolute |
| `PermissionRequest` | Claude Code is about to show a permission prompt, or would auto-deny a call that cannot prompt | tool name | `tool_name`, `tool_input`, optional `permission_suggestions[]`; no `tool_use_id` |
| `PostToolUse` | after a tool call succeeds | tool name | `tool_name`, `tool_input`, `tool_response`, `tool_use_id`, optional `duration_ms` |
| `Notification` | Claude Code sends a notification | `notification_type` | `message`, optional `title`, `notification_type` |
| `SubagentStart` | a subagent is spawned or resumed | agent type | `agent_id`, `agent_type` |
| `SubagentStop` | a subagent finishes responding | agent type | `stop_hook_active`, `agent_id`, `agent_type`, `agent_transcript_path`, `last_assistant_message`, parent-scoped `background_tasks[]` and `session_crons[]` |
| `Stop` | the main agent finishes responding; not on user interrupt | none | `stop_hook_active`, `last_assistant_message`, `background_tasks[]`, `session_crons[]` (both 2.1.145 and later) |
| `StopFailure` | the turn ends on an API error, in place of `Stop` | `error` | `error`, optional `error_details`, optional `last_assistant_message` holding the API error text |
| `PreCompact` | before compaction | `trigger` | `trigger` (`manual` \| `auto`), `custom_instructions` (`null` for `auto` or a bare `/compact`) |
| `PostCompact` | after compaction | `trigger` | `trigger`, `compact_summary` |
| `SessionEnd` | the session ends | `reason` | `reason` (`clear` \| `resume` \| `logout` \| `prompt_input_exit` \| `other`) |

The event payloads carry these enumerations and nested shapes:

| Field | Values or shape |
| --- | --- |
| `SessionStart` on `resume` or `fork` with at least one prior response (2.1.251 and later) | adds `seconds_since_last_response`, `context_tokens`, `prompt_cache_likely_expired`, `estimated_cache_write_usd` |
| `Notification.notification_type` | `permission_prompt` (after about six seconds unanswered), `idle_prompt` (about 60 seconds after a response), `auth_success`, `elicitation_dialog`, `elicitation_url_dialog`, `elicitation_complete`, `elicitation_response`, `agent_needs_input` and `agent_completed` (2.1.198 and later), `quota_auto_resume_fired`, `quota_auto_resume_stale`, `quota_auto_resume_disabled` (2.1.234 and later) |
| `Stop.background_tasks[]` | `id`, `type` (`shell`, `subagent`, `monitor`, `workflow`, `teammate`, `cloud session`, `MCP task`), `status`, `description`, plus `command` for shell, `agent_type` for subagent, `server` and `tool` for monitor and MCP task, `name` for workflow; strings capped at 1000 characters |
| `Stop.session_crons[]` | `id`, `schedule`, `recurring`, `prompt` |
| `StopFailure.error` | `rate_limit`, `overloaded`, `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `billing_error`, `invalid_request`, `model_not_found`, `server_error`, `max_output_tokens`, `cloud_credential_error` (2.1.267 and later), `unknown`. `rate_limit` covers spend caps and per-model caps ("You've reached your Fable limit") as well as rate windows |
| `SessionEnd` timing | a 1.5-second budget, raised to the highest per-hook `timeout` up to 60 seconds, or set by `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS`; JSON output is discarded |

The wired events take these decision fields beyond the [decision control](#decision-control) table:

| Event | Fields |
| --- | --- |
| `UserPromptSubmit` | `decision: "block"` erases the prompt; `reason` is shown to the user only; `additionalContext`, `sessionTitle`, `suppressOriginalPrompt` |
| `PreToolUse` | precedence across hooks is `deny` > `defer` > `ask` > `allow`. `allow` on `AskUserQuestion` or `ExitPlanMode` needs `updatedInput`, which replaces the whole input. `defer` works only in `-p` with a single tool call. The top-level `approve` and `block` are deprecated aliases for `allow` and `deny` |
| `PermissionRequest` | `updatedPermissions[]` entries have `type` (`addRules`, `replaceRules`, `removeRules`, `setMode`, `addDirectories`, `removeDirectories`) and `destination` (`session`, `localSettings`, `projectSettings`, `userSettings`); deny and ask rules still apply to an `allow` |
| `PostToolUse` | `decision: "block"` adds `reason` beside the result; `additionalContext`, `classifierContext` (2.1.236 and later), `updatedToolOutput`, `updatedMCPToolOutput` |
| `Stop`, `SubagentStop` | `decision: "block"` requires `reason` and keeps the agent running; loops stop after 8 consecutive continuations |
| `PreCompact` | exit 2 or `decision: "block"` skips a proactive compaction; `systemMessage` and `continue` are discarded |

A `PermissionRequest` allow and a `PreToolUse` allow that answers a user question have these shapes:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow" } } }
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": {} } }
```

`PermissionRequest` hooks fire in `--print` mode from 2.1.268 on.

### Full event catalog

Upstream ships 33 events. The wired column marks the 13 RimZ installs.

| Event | Fires | Wired |
| --- | --- | :---: |
| `SessionStart` | a session starts or resumes | yes |
| `Setup` | `--init-only`, or `--init` or `--maintenance` in `-p` mode | |
| `InstructionsLoaded` | a `CLAUDE.md` or rules file loads | |
| `UserPromptSubmit` | a prompt is submitted | yes |
| `UserPromptExpansion` | a typed command expands into a prompt | |
| `MessageDisplay` | assistant text is displayed; hooks can transform or hide it (2.1.152 and later) | |
| `PreToolUse` | before a tool call | yes |
| `PermissionRequest` | a permission prompt is about to show | yes |
| `PostToolUse` | a tool call succeeds | yes |
| `PostToolUseFailure` | a tool call fails | |
| `PostToolBatch` | a batch of parallel tool calls resolves | |
| `PermissionDenied` | the auto-mode classifier denies a call | |
| `Notification` | Claude Code sends a notification | yes |
| `SubagentStart` | a subagent is spawned | yes |
| `SubagentStop` | a subagent finishes | yes |
| `TaskCreated` | `TaskCreate` creates a task | |
| `TaskCompleted` | a task is marked completed | |
| `Stop` | the main agent finishes responding | yes |
| `StopFailure` | the turn ends on an API error | yes |
| `TeammateIdle` | an agent-team teammate is about to idle | |
| `ConfigChange` | a configuration file changes mid-session | |
| `CwdChanged` | the working directory changes | |
| `DirectoryAdded` | `/add-dir` or the SDK `register_repo_root` adds a directory (2.1.219 and later) | |
| `FileChanged` | a watched file changes on disk | |
| `WorktreeCreate` | a worktree is being created | |
| `WorktreeRemove` | a worktree is being removed | |
| `PreCompact` | before compaction | yes |
| `PostCompact` | after compaction | yes |
| `PreModelSwitch` | before a requested model switch; input `from_model`, `to_model`, `requested_model`, `source` (`command` \| `picker` \| `sdk`), `context_tokens`, `prompt_cache_warm`, `cache_ttl`, `estimated_cache_write_usd`, `pricing` (2.1.251 and later) | |
| `PostModelSwitch` | after any model change, fallback and resume included; `PreModelSwitch` input plus `source` values `auto` and `resume` (2.1.251 and later) | |
| `SessionEnd` | the session ends | yes |
| `Elicitation` | an MCP server requests user input | |
| `ElicitationResult` | the user answers an elicitation | |

## Statusline JSON

Claude Code runs the `statusLine` command and pipes a JSON object to its stdin; each line the command prints is a status row ([statusline](https://code.claude.com/docs/en/statusline)). How RimZ wraps the command and what it reads is [adapter_claude.md → Rich context](../../internals/agents/adapter_claude.md#rich-context).

### Settings and triggers

The `statusLine` setting takes these fields:

| Field | Meaning |
| --- | --- |
| `type` | `"command"` |
| `command` | a script path or inline shell command |
| `padding` | extra horizontal characters; default `0` |
| `refreshInterval` | also re-run every N seconds; minimum `1`; unset runs on events only |
| `hideVimModeIndicator` | `true` suppresses the built-in `-- INSERT --` text |

The command runs once when a session starts or resumes, then again when an assistant message arrives, `/compact` finishes, the permission mode changes, vim mode toggles, the `command` setting changes, a `refreshInterval` timer elapses, a rate-limit window reaches its `resets_at`, or a warm prompt cache reaches its `expires_at`. Updates are debounced at 300 ms, except a `command` change, which runs at once. A new update cancels a run still in flight. Output is captured rather than attached to the terminal, so Claude Code sets `COLUMNS` and `LINES` (2.1.153 and later). The statusline makes no API calls.

### Schema

The upstream example, with the binary-read `remote` object added:

```json
{
  "cwd": "/current/working/directory",
  "session_id": "abc123...",
  "session_name": "my-session",
  "prompt_id": "550e8400-e29b-41d4-a716-446655440000",
  "transcript_path": "/path/to/transcript.jsonl",
  "model": { "id": "claude-opus-5", "display_name": "Opus" },
  "workspace": {
    "current_dir": "/current/working/directory",
    "project_dir": "/original/project/directory",
    "added_dirs": [],
    "git_worktree": "feature-xyz",
    "repo": { "host": "github.com", "owner": "anthropics", "name": "claude-code" }
  },
  "version": "2.1.90",
  "output_style": { "name": "default" },
  "cost": {
    "total_cost_usd": 0.01234,
    "total_duration_ms": 45000,
    "total_api_duration_ms": 2300,
    "total_lines_added": 156,
    "total_lines_removed": 23
  },
  "context_window": {
    "total_input_tokens": 15500,
    "total_output_tokens": 1200,
    "context_window_size": 200000,
    "used_percentage": 8,
    "remaining_percentage": 92,
    "current_usage": {
      "input_tokens": 8500,
      "output_tokens": 1200,
      "cache_creation_input_tokens": 5000,
      "cache_read_input_tokens": 2000
    }
  },
  "exceeds_200k_tokens": false,
  "prompt_cache": {
    "warm": true,
    "caching_observed": true,
    "ttl": "1h",
    "expires_at": 1738429200,
    "requests": 14,
    "misses": 2,
    "expected_rebuilds": 1,
    "hit_ratio": 0.91,
    "cache_write_tokens": 352000,
    "miss_recache_tokens": 310200,
    "last_miss_at": 1738425230,
    "last_miss_cause": { "causes": ["tools_changed"], "tools_added": 2, "tools_removed": 0 },
    "miss_causes": { "tools_changed": 2 },
    "recache_tokens_if_cold": 45000
  },
  "fast_mode": false,
  "effort": { "level": "high" },
  "thinking": { "enabled": true },
  "rate_limits": {
    "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600 },
    "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600 },
    "spend_limit": { "used_percentage": 62.8, "resets_at": 1740787200 }
  },
  "vim": { "mode": "NORMAL" },
  "agent": { "name": "security-reviewer" },
  "remote": { "session_id": "session_..." },
  "pr": {
    "number": 1234,
    "url": "https://github.com/anthropics/claude-code/pull/1234",
    "review_state": "pending"
  },
  "worktree": {
    "name": "my-feature",
    "path": "/path/to/.claude/worktrees/my-feature",
    "branch": "worktree-my-feature",
    "original_cwd": "/path/to/project",
    "original_branch": "main"
  }
}
```

### Field reference

Each field means the following ([available data](https://code.claude.com/docs/en/statusline#available-data)):

| Field | Meaning |
| --- | --- |
| `session_id` | session ID |
| `session_name` | the `--name` or `/rename` name, otherwise the AI-generated title; the default display name, such as `my-app-3f`, does not populate it |
| `prompt_id` | current user-prompt UUID, shared with hooks and OpenTelemetry; 2.1.196 and later |
| `transcript_path` | session transcript path |
| `cwd`, `workspace.current_dir` | working directory, same value |
| `workspace.project_dir` | directory Claude Code launched in |
| `workspace.added_dirs` | directories from `/add-dir` or `--add-dir`; `[]` when none |
| `workspace.git_worktree` | linked git worktree name |
| `workspace.repo.{host,owner,name}` | identity from the `origin` remote; a GitLab subgroup `owner` is the full `group/subgroup` path (2.1.260 and later) |
| `model.id`, `model.display_name` | current model |
| `version` | Claude Code version |
| `output_style.name` | current output style |
| `cost.total_cost_usd` | client-side estimate at list price, or at managed [`modelPricing`](#project-storage-and-managed-pricing) rates; resets on `/clear` (2.1.211 and later) |
| `cost.total_duration_ms`, `cost.total_api_duration_ms` | wall-clock time since session start, and time waiting on the API |
| `cost.total_lines_added`, `cost.total_lines_removed` | lines changed |
| `context_window.total_input_tokens` | `input_tokens` + `cache_creation_input_tokens` + `cache_read_input_tokens` of the latest response; current window, not cumulative (2.1.132 and later) |
| `context_window.total_output_tokens` | output tokens of the latest response |
| `context_window.context_window_size` | window in tokens: 200000 by default, 1000000 with extended context |
| `context_window.used_percentage`, `remaining_percentage` | fill computed from input-side tokens only, against the model's full window |
| `context_window.current_usage.{input_tokens,output_tokens,cache_creation_input_tokens,cache_read_input_tokens}` | per-component counts from the last API call |
| `exceeds_200k_tokens` | whether the latest response's input, cache, and output tokens exceed a fixed 200k |
| `prompt_cache` | main-conversation prompt-cache statistics, subagents excluded; 2.1.251 and later ([prompt cache fields](#prompt-cache-fields)) |
| `fast_mode` | whether fast mode is on |
| `effort.level` | live effort level, including `/effort` changes; Ultracode reports `xhigh` |
| `thinking.enabled` | whether extended thinking is on |
| `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}` | claude.ai window fill from 0 to 100, and reset in Unix epoch seconds |
| `rate_limits.spend_limit.{used_percentage,resets_at}` | Claude apps gateway spend limit; can exceed 100; 2.1.251 and later |
| `vim.mode` | `NORMAL`, `INSERT`, `VISUAL`, or `VISUAL LINE` |
| `agent.name` | agent name under `--agent` or agent settings |
| `remote.session_id` | Remote Control session ID while a connection is active. Read from the 2.1.270 binary; the statusline docs omit it |
| `pr.{number,url}` | open pull request for the branch, or the GitLab merge request (2.1.234 and later) |
| `pr.review_state` | `approved`, `pending`, `changes_requested`, or `draft`; for a merge request, `approved` means mergeable |
| `pr.kind` | `"mr"` for a GitLab merge request; absent for GitHub (2.1.234 and later) |
| `worktree.{name,path,branch,original_cwd,original_branch}` | the active `--worktree` session; `branch` and `original_branch` are absent for hook-based worktrees |

The statusline carries no model-scoped usage window.

Absent and null are distinct. `session_name`, `prompt_id`, `workspace.git_worktree`, `workspace.repo`, `effort`, `vim`, `agent`, `remote`, `pr`, `pr.review_state`, `pr.kind`, and `worktree` are absent until their data exists. `rate_limits` appears only for Pro and Max subscribers, or behind a gateway with a spend limit, after the first API response; each window can be absent on its own, and Claude Code drops a window once its `resets_at` passes. `prompt_cache` appears after the first main-conversation response. `context_window.current_usage` is `null` before the first API call and again after `/compact` until the next call; `used_percentage` and `remaining_percentage` can be `null` early in a session.

### Prompt cache fields

Timestamps are Unix epoch seconds ([prompt cache fields](https://code.claude.com/docs/en/statusline#prompt-cache-fields)).

| Field | Meaning |
| --- | --- |
| `warm` | the cached prefix is within its TTL |
| `caching_observed` | any response this session reported cache tokens |
| `ttl` | `"5m"` or `"1h"` |
| `expires_at` | when the prefix goes cold; `null` when the last response reported no cache tokens |
| `requests`, `misses`, `expected_rebuilds` | main-conversation requests, unexplained re-processing, and rebuilds after compaction or tool-result clearing |
| `hit_ratio` | cache reads over all input tokens, 0 to 1; `null` while all counts are zero |
| `cache_write_tokens`, `miss_recache_tokens` | tokens written to cache in total, and by misses |
| `last_miss_at` | time of the last miss; `null` without misses |
| `last_miss_cause` | `causes[]` (`tools_changed`, `system_prompt_changed`, `ttl_expired_5m`, `likely_server_side`), `tools_added`, `tools_removed`, `system_char_delta`; `null` until a diagnosed miss; 2.1.260 and later |
| `miss_causes` | count of diagnosed misses per cause; 2.1.260 and later |
| `recache_tokens_if_cold` | tokens the next request re-caches if the cache is cold; `null` right after compaction |

### `subagentStatusLine`

`subagentStatusLine` takes `{ "type": "command", "command": "…" }` and renders the body of each subagent row in the agent panel ([subagent status lines](https://code.claude.com/docs/en/statusline#subagent-status-lines)). It runs once per refresh tick with every visible row in one JSON object: the base hook fields, `columns` (usable row width), and `tasks[]`.

| Task field | Meaning |
| --- | --- |
| `id` | task ID, the key for output rows; for a subagent this is its `agent_id` |
| `name`, `type`, `status`, `description`, `label` | row identity and state |
| `startTime` | task start time |
| `model` | resolved model ID; omitted until resolved; 2.1.205 and later |
| `effort` | configured level string or numeric token budget; absent when inherited; 2.1.214 and later |
| `contextWindowSize` | that model's window in tokens; 2.1.205 and later |
| `tokenCount`, `tokenSamples` | tokens used, and samples over time |
| `cwd` | task working directory |

The command prints one JSON line per row it overrides, `{"id": "<task id>", "content": "<row body>"}`. Content renders as-is, ANSI and OSC 8 included; an empty `content` hides the row and an omitted `id` keeps the default row. The trust, `disableAllHooks`, and `allowManagedHooksOnly` gates of `statusLine` apply. A plugin can ship a default, which does not run under `allowManagedHooksOnly`.

## Launch flags

These flags shape a Claude Code launch ([CLI reference](https://code.claude.com/docs/en/cli-reference)). RimZ's argv for each launch concern is [adapter_claude.md → Launch](../../internals/agents/adapter_claude.md#launch).

| Flag | Meaning |
| --- | --- |
| `--resume`, `-r [value]` | resume by session ID, name, or absolute transcript path; bare opens the picker |
| `--continue`, `-c` | resume the newest conversation in the directory |
| `--fork-session` | with `--resume` or `--continue`, copy into a new session ID and leave the source untouched; the new session's `SessionStart` reports `source: "fork"` |
| `--session-id <uuid>` | use a specific session ID |
| `--name`, `-n` | session display name |
| `--system-prompt`, `--system-prompt-file` | replace the default system prompt; mutually exclusive |
| `--append-system-prompt`, `--append-system-prompt-file` | append to the default or replacement prompt |
| `--system-prompt-snapshot <on\|off>` | `on`, the default, records the prompt on the first request and reuses it on every later request and resume until compaction; `off` rebuilds per request; 2.1.257 and later |
| `--autocompact <auto\|tokens>` | session auto-compaction window; 2.1.221 and later ([below](#auto-compaction-window)) |
| `--remote-control`, `--rc [name]` | interactive session with Remote Control on ([below](#remote-control)) |
| `--bg`, `--background` | start as a background agent ([agent view](#agent-view)) |

With snapshot recording on, system-prompt flag text passed on a later `--resume` or `--continue` launch takes effect only after the conversation compacts or in a new conversation ([system prompt flags in resumed conversations](https://code.claude.com/docs/en/cli-usage#system-prompt-flags-in-resumed-conversations)). Before 2.1.265, any system-prompt flag turned recording off.

### Auto-compaction window

The window is set in three places, and `CLAUDE_CODE_AUTO_COMPACT_WINDOW` wins over `--autocompact`, which wins over the `autoCompactWindow` setting ([set the auto-compact window](https://code.claude.com/docs/en/model-config#set-the-auto-compact-window)). The `/autocompact` command writes the setting.

| Surface | Accepts |
| --- | --- |
| `--autocompact <auto\|tokens>` (2.1.221 and later) | `auto` for the model-tuned window; a plain count (`200000`); a `k` or `M` suffix (`500k`, `1M`); a bare 100 to 1000 meaning thousands (`200` is 200,000). Managed settings do not preempt the flag |
| `autoCompactWindow` setting | a token count from `100000` to `1000000`; unset picks the model-tuned window |
| `CLAUDE_CODE_AUTO_COMPACT_WINDOW` | a plain integer only; `500k` reads as `500` and clamps to 100K |
| `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` | 1 to 100, the percentage of the window at which compaction fires; it can only lower the threshold |

The valid range is 100K to 1M tokens, capped at the model's context window. The window is where compaction is allowed to fire, not an exact trigger count, and the statusline's `used_percentage` keeps measuring against the full model window.

### Per-launch skill overrides

[`skillOverrides`](https://code.claude.com/docs/en/skills) accepts `"user-invocable-only"`, which leaves explicit `/skill` expansion available while removing model invocation. [`--settings`](https://code.claude.com/docs/en/cli-reference) accepts a file or inline JSON. Local probes on 2.1.282 found that keys are skill directory names, only the last `--settings` takes effect, and `*` is not a wildcard.

## Agent view

`claude agents` opens agent view, one screen for every background session; `claude --bg`, `/background`, or `←` in a session sends a session to the background ([agent view](https://code.claude.com/docs/en/agent-view)). Agent view first shipped in 2.1.139. `--bg` cannot be combined with `-p`.

The `disableAgentView` setting and `CLAUDE_CODE_DISABLE_AGENT_VIEW=1` turn off `claude agents`, `--bg`, `/background`, and the on-demand supervisor. Whichever one turns it off, the other cannot turn it back on ([`disableAgentView`](https://code.claude.com/docs/en/settings-reference#disableagentview)).

## Remote control

Remote Control lets claude.ai and the Claude app drive local sessions ([Remote Control](https://code.claude.com/docs/en/remote-control)). It starts three ways: `claude remote-control` runs a server that hosts sessions, `claude --remote-control [name]` starts one interactive session with it on, and `/remote-control` connects a running session. `claude remote-control` first shipped in 2.1.51. What RimZ launches and checks is [adapter_claude.md → Remote control](../../internals/agents/adapter_claude.md#remote-control).

### Server flags

`claude remote-control` takes these flags after the subcommand; `claude remote-control --help` lists them at 2.1.270, except `--sandbox` and `--no-sandbox`, which only the docs list.

| Flag | Meaning |
| --- | --- |
| `--name <name>` | session title on claude.ai/code |
| `--remote-control-session-name-prefix <prefix>` | prefix for generated names; default hostname; also `CLAUDE_REMOTE_CONTROL_SESSION_NAME_PREFIX` |
| `--spawn <same-dir\|worktree\|session>` | `same-dir` (default) shares the directory; `worktree` gives each on-demand session a git worktree; `session` serves one session and exits with it |
| `--capacity <N>` | concurrent sessions; default 32; not with `--spawn session` |
| `--[no-]create-session-in-dir` | pre-create a session in the current directory; default on |
| `-c`, `--continue` | reattach the session the last server here started; 2.1.200 and later |
| `--session-id <id>` | reattach one session; 2.1.200 and later |
| `--permission-mode <mode>` | starting permission mode; `manual` aliases `default` |
| `--debug-file <path>`, `-v`, `--verbose` | logging |
| `--sandbox`, `--no-sandbox` | sandboxing, off by default |

From 2.1.248, the subcommand accepts its own flags when a global flag or a wrapper-injected option precedes it ([changelog](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md)).

### Requirements

Remote Control refuses to start when any of these holds ([requirements](https://code.claude.com/docs/en/remote-control#requirements)):

| Condition | Boundary |
| --- | --- |
| no claude.ai Pro, Max, Team, or Enterprise login; on Team and Enterprise the admin toggle is off | always |
| `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, or `apiKeyHelper` is set, in the environment or a settings `env` block, even beside a claude.ai login | 2.1.139 and later ([changelog](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md)) |
| the login is a long-lived `claude setup-token` or `CLAUDE_CODE_OAUTH_TOKEN` token, which cannot establish Remote Control | always |
| `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, or `CLAUDE_CODE_USE_FOUNDRY` routes the session | always |
| `ANTHROPIC_BASE_URL` points anywhere but `api.anthropic.com` | 2.1.196 and later |
| the session signs in through a Claude apps gateway | always |
| `DISABLE_TELEMETRY`, `DO_NOT_TRACK`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`, or `DISABLE_GROWTHBOOK` disables feature-flag evaluation | always |
| the directory has not been trusted by running `claude` there | always |
| `disableRemoteControl: true` | always |

### Settings and consent

Two settings govern Remote Control ([settings reference](https://code.claude.com/docs/en/settings-reference#disableremotecontrol)):

| Setting | Values |
| --- | --- |
| `disableRemoteControl` | `true` refuses `claude remote-control`, `--remote-control`, auto-start, and the in-session toggle; default `false` |
| `remoteControlAtStartup` | `true` connects every interactive session at start; `false` waits for `/remote-control`; unset follows the organization default. A `true` in project or local settings is ignored (2.1.222 and later). A legacy value in `~/.claude.json` is still read. `--remote-control` overrides `false` |

`claude remote-control` asks `Enable Remote Control? (y/n)` until the user accepts. In the 2.1.270 binary, only `y` or `yes` writes `"remoteDialogSeen": true` to `${CLAUDE_CONFIG_DIR:-$HOME}/.claude.json`; any other answer exits without writing. Declining stopped counting as consent in 2.1.257.

### Bridge pointer

A Remote Control host records itself in `<config>/projects/<project>/bridge-pointer.json`, where `<project>` is the [project directory name](#project-storage-and-managed-pricing). The schema is read from the 2.1.270 binary and has no docs page:

| Field | Type |
| --- | --- |
| `sessionId` | string, may be empty |
| `environmentId` | string |
| `source` | `"standalone"` or `"repl"` |
| `pid` | optional number |
| `procStart` | optional string, the process start token; Windows builds write `procStartFt` |
| `activeSessionIds` | optional string array |
| `activeSessionIdsPersistedAt` | optional epoch milliseconds |

### Environment in hosted sessions

This subsection is pinned to a Claude Code 2.1.209 live capture. An attached session runs as an SDK child shaped `<claude-version-bin> --print --sdk-url … --session-id cse_…`. The child and its hook processes inherit `CLAUDE_CODE_ENVIRONMENT_KIND=bridge` and a non-empty `CLAUDE_CODE_SESSION_ACCESS_TOKEN`, the session-ingress credential, which must stay private. The child also runs the user's globally installed hooks. Both variable names are still present in the 2.1.270 binary.

## Project storage and managed pricing

Claude Code stores transcripts at `<config>/projects/<project>/<session-id>.jsonl`, where `<config>` is `CLAUDE_CONFIG_DIR` or `~/.claude` ([sessions](https://code.claude.com/docs/en/sessions)). `<project>` is the working directory path with every non-alphanumeric character replaced by `-`; a converted name over 200 characters is truncated to 200 and suffixed with a hash of the full path.

`CLAUDE_CODE_PROJECT_DIR_NAME` replaces the derived `<project>` name (2.1.234 and later). Three rules apply ([name the project directory yourself](https://code.claude.com/docs/en/sessions#name-the-project-directory-yourself)):

- It is ignored unless `CLAUDE_CONFIG_DIR` is also set.
- It must be 1 to 64 letters, digits, `-`, or `_`, and not a Windows device name such as `con`; any other value falls back to the derived name.
- It is read once from the launch environment, never from a settings `env` block.

`modelPricing` makes Claude Code report cost at contracted rates in `/usage`, the statusline, the SDK `total_cost_usd`, `--max-budget-usd`, and OpenTelemetry ([`modelPricing`](https://code.claude.com/docs/en/settings-reference#modelpricing)). The changelog places it in 2.1.243, while the settings reference says 2.1.242, a version with no release. It is honored only from a managed source: server-managed settings, an MDM policy, `managed-settings.json`, or a policy helper. The file lives at `/etc/claude-code/managed-settings.json` on Linux and WSL, `/Library/Application Support/ClaudeCode/managed-settings.json` on macOS, and `C:\Program Files\ClaudeCode\managed-settings.json` on Windows; `*.json` files in the adjacent `managed-settings.d/` merge after it in alphabetical order ([managed settings](https://code.claude.com/docs/en/managed-settings)).

```json
{
  "modelPricing": {
    "multiplier": 0.85,
    "overrides": {
      "claude-sonnet-4-6": { "input": 2.4, "output": 12, "cacheRead": 0.24, "cacheWrite": 3 }
    }
  }
}
```

| Field | Rule |
| --- | --- |
| `multiplier` | greater than 0 and at most 1; scales every computed cost, override rows included |
| `overrides.<model>` | USD per million tokens; `input`, `output`, `cacheRead`, and `cacheWrite` all required, each 0 to 10000; `cacheWrite` covers five-minute and one-hour writes |
| row rates | used as written, with no fast-mode or US-only-inference surcharge |
| unparseable row or multiplier | dropped; the rest is kept |
| built-in model key | applies to every dated snapshot and provider-specific ID of that model |
| any other key | applies to that exact ID, and wins over a built-in row |

## Auth surface

`claude auth status` prints the login as JSON by default, or text with `--text`, and exits 0 when logged in and 1 when not ([CLI reference](https://code.claude.com/docs/en/cli-reference)). The docs publish no field list; these keys come from 2.1.270 output. What RimZ reads from them is [adapter_claude.md → Account and balance](../../internals/agents/adapter_claude.md#account-and-balance).

| Field | Meaning |
| --- | --- |
| `loggedIn` | whether a login is present |
| `authMethod` | login type, such as `claude.ai` |
| `apiProvider` | API provider in use |
| `subscriptionType` | plan tier, such as `max` or `pro` |
| `email`, `orgId`, `orgName` | account identity |
| `analyticsDisabled` | whether analytics are off |
| `configDirectory`, `projectsDirectory` | resolved config and projects directories; `configDirectory` since 2.1.268 |

### Credentials

Claude Code stores the login in `.credentials.json` under the config directory: `~/.claude/.credentials.json` with mode `0600` on Linux, `%USERPROFILE%\.claude\.credentials.json` on Windows ([credential management](https://code.claude.com/docs/en/authentication)). macOS uses the Keychain and falls back to the file when the Keychain rejects the write. Under `CLAUDE_CONFIG_DIR`, both the file and the Keychain entry follow that directory.

The Keychain item is a generic password with account `$USER` and service `Claude Code-credentials`. Under `CLAUDE_CONFIG_DIR`, the service gains a suffix of the first 8 hex characters of the SHA-256 of the directory, as in `Claude Code-credentials-1a2b3c4d`. The item name is read from the 2.1.270 binary.

The file holds a root `claudeAiOauth` object; the docs publish no schema, and these keys are read from the 2.1.270 binary:

| Field | Meaning |
| --- | --- |
| `accessToken` | bearer token for API calls |
| `refreshToken` | token Claude Code uses to refresh `accessToken` |
| `expiresAt` | expiry in epoch milliseconds |
| `scopes[]` | granted scopes, such as `user:profile` |

### OAuth usage endpoint

`GET https://api.anthropic.com/api/oauth/usage` returns the plan usage windows for an OAuth login. It needs `Authorization: Bearer <accessToken>` and `anthropic-beta: oauth-2025-04-20`. Claude Code 2.1.270 calls it with a 5-second timeout, and also as `/api/oauth/usage?at_wall=1&skip_spend=1`. No public schema exists, and the binary does not fix the units of `utilization`, `percent`, or the credit fields. These are the fields the 2.1.270 client reads; live responses carry more:

```json
{
  "five_hour": { "utilization": 12.5, "resets_at": "2026-09-21T14:13:20Z" },
  "seven_day": { "utilization": 37, "resets_at": "2026-09-27T09:06:40Z" },
  "extra_usage": { "is_enabled": true, "used_credits": 725, "monthly_limit": 5000 },
  "limits": [
    {
      "kind": "weekly_scoped",
      "group": "weekly",
      "percent": 58,
      "resets_at": "2026-09-27T09:06:40Z",
      "scope": { "model": { "display_name": "Fable" } }
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `five_hour`, `seven_day` | window with `utilization` and `resets_at` |
| `extra_usage.is_enabled` | whether paid extra usage is on |
| `extra_usage.used_credits`, `monthly_limit` | extra usage spent and its cap |
| `limits[]` | limit entries with `kind`, `group`, `percent`, `resets_at` (an ISO 8601 string), and `scope`. The client treats a `kind: "weekly_scoped"` entry with a string `scope.model.display_name` as a per-model weekly cap |

## Transcript JSONL

Anthropic publishes no schema for the transcript at `transcript_path`, so this section describes observed lines whose field names also appear in the 2.1.270 binary. How RimZ reads them is [adapter_claude.md → Context and transcript](../../internals/agents/adapter_claude.md#context-and-transcript).

Each line is one JSON object with a `type`, such as `user`, `assistant`, or `system`, and a `timestamp`. An assistant line carries a `message` object with `model` and `usage` (`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`). A `usage` can carry `iterations[]`; an iteration with `type: "advisor_message"` names its own `model` and usage and is a separately billed nested request. An Esc interrupt appends a `user` line whose text begins `[Request interrupted by user`.

### Transcript death certificate

A turn that dies on an API error leaves two lines, milliseconds apart: an `assistant` line flagged `isApiErrorMessage: true` whose text is the error, then a `system` line with subtype `turn_duration`. The assistant line also carries the same `error` value as `StopFailure.error` and the HTTP status as `apiErrorStatus`:

```jsonc
{"type": "assistant", "isApiErrorMessage": true, "error": "rate_limit", "apiErrorStatus": 429, "timestamp": "2026-06-04T02:56:32.919Z", "message": {"content": [{"type": "text", "text": "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model."}]}}
{"type": "system", "subtype": "turn_duration", "timestamp": "2026-06-04T02:56:32.923Z"}
```

The same failure fires [`StopFailure`](#hooks-rimz-wires) when hooks are installed, and its `last_assistant_message` holds the same error text. The transcript lines remain the evidence for sessions whose hooks were installed after the failure. How RimZ classifies the label is [adapter_claude.md → Turn-death marker](../../internals/agents/adapter_claude.md#turn-death-marker).

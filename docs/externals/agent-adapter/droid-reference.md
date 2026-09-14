# Factory Droid CLI protocol reference

> RimZ's mapping of this surface is [adapter_droid.md](../../internals/agents/adapter_droid.md). The agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md), and the account, balance, spend, and pricing contract is [providers.md](../../internals/agents/providers.md).

This page mirrors the Factory Droid CLI surfaces an adapter binds to: command-line launch, configuration, lifecycle hooks, session files, modes and models, subagents, `droid exec` and its stream JSON-RPC protocol, and authentication and usage. It records what upstream ships and makes no claim about what RimZ supports.

## Baseline

The page mirrors **Droid CLI 0.218.2**, read on 2026-09-13. Factory publishes no release date or source tag for it: the release notes stop at 0.209.0 (2026-09-01), and on 2026-09-13 `droid update --check` on 0.218.2 reported no newer build. Factory ships the CLI as a proprietary single-file binary whose embedded JavaScript bundle is readable; where this page cites "the 0.218.2 bundle", the claim was read from that bundle's string literals and schema definitions, because the docs leave it silent or contradict it.

Two sections carry their own pin:

- [Stream JSON-RPC protocol](#stream-json-rpc-protocol) is read from the npm package `@factory/droid-sdk` **0.9.1** (dist-tag `latest`, published 2026-09-04, `FACTORY_PROTOCOL_VERSION` `1.201.1`). The package's registry metadata carries no git commit, and the public `Factory-AI/droid-sdk-typescript` repository no longer holds the SDK source, so anchors are paths inside the tarball: `package/dist/chunk-YSKO6SPT.js` for schemas and `package/dist/node.js` for the process transport. The 0.218.2 binary speaks protocol `1.209.0`.
- [Session files](#session-files) describes private files with no published contract. Its shapes come from local session files and the 0.218.2 bundle's session schema; Factory publishes no schema for them.

The executable is `droid`. `droid -v` or `droid --version` prints the version, and `droid update` installs the latest release (`droid update --check` only reports).

## Upstream sources

| Surface | Source |
| --- | --- |
| Release notes | <https://docs.factory.ai/changelog/release-notes> |
| CLI overview | <https://docs.factory.ai/droid-cli/overview> |
| CLI commands, flags, slash commands, shortcuts, exit codes | <https://docs.factory.ai/droid-cli/cli-reference> |
| Settings | <https://docs.factory.ai/droid-cli/settings> |
| Hooks: configuration, events, payloads, decisions | <https://docs.factory.ai/harness/hooks> |
| Autonomy levels and command lists | <https://docs.factory.ai/autonomy-and-safety/auto-run> |
| Specification mode | <https://docs.factory.ai/autonomy-and-safety/specification-mode> |
| Custom droids (subagents) | <https://docs.factory.ai/harness/subagents> |
| Custom models (BYOK) | <https://docs.factory.ai/model-independence/byok> |
| `droid exec` and JSON-RPC | <https://docs.factory.ai/droid-exec/overview> |
| TypeScript SDK package | <https://www.npmjs.com/package/@factory/droid-sdk> (tarball `droid-sdk-0.9.1.tgz`) |
| SDK migration notes | <https://github.com/Factory-AI/droid-sdk-typescript/blob/main/MIGRATION.md> |

## Upstream scope

Droid has two observation surfaces. The stock interactive TUI exposes command hooks plus private session files; `droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc` exposes a typed JSON-RPC protocol for a process the client owns. The JSON-RPC protocol does not observe a stock TUI running in someone else's pane. The table indexes where each concern lives on each surface.

| Concern | Stock TUI: hooks and session files | `droid exec` stream JSON-RPC |
| --- | --- | --- |
| Session identity | every hook's `session_id` and `transcript_path`; `SessionStart.previous_session_id` | initialize or load result `sessionId` |
| Turn start | `UserPromptSubmit.prompt` | `droid.add_user_message` |
| Turn end and outcome | `Stop` (not on interrupt); no failure event | `agent_turn_completed.reason`; `droid_working_state_changed` to `idle` |
| Tool work | `PreToolUse`, `PostToolUse` (success only) | `tool_call`, `tool_result`, `tool_execution_phase_changed` |
| Permission wait | `Notification` with `notification_type: "permission_prompt"`; no request ID or options | `droid.request_permission` |
| Question | `Notification` with `notification_type: "elicitation_dialog"`; `AskUser` `tool_use` in the session file | `droid.ask_user` |
| Plan approval | none | permission `confirmationType: "exit_spec_mode"` |
| Compaction | `PreCompact`, then `SessionStart` with `source: "compact"`; no `PostCompact` | `compacting_conversation` state, `session_compacted`, `droid.compact_session` |
| Model, effort, tokens, context | session settings snapshot only | `settings_updated`, `session_token_usage_changed`, `droid.get_context_stats` |
| Subagents | `SubagentStop` with `task_name`, `task_result`, `task_error`; no start event or child ID | `child_session_available`, `tool_progress_update.update.subagentSessionId`, mission worker notifications |
| Session end | `SessionEnd.reason` | `droid.close_session`, process exit |
| Resume and fork | `--resume <id>`, `--fork <id>` | `--session-id`, `--fork`, `droid.load_session`, `droid.fork_session` |
| Auth status | `droid doctor --json --auth` | `-32001` authentication error |
| Quota and rate windows | none | none; `llm_retry.reason: "rate_limited"` and `model_rate_limited` turn outcome only |

RimZ's gaps against these surfaces are recorded in the internals page's [Known gaps](../../internals/agents/adapter_droid.md#known-gaps).

## Command line

### Interactive launch

`droid` starts the interactive TUI. A positional prompt starts the TUI and submits that prompt.

| Flag | Meaning |
| --- | --- |
| `-v`, `--version` | print the version |
| `-r`, `--resume [sessionId]` | resume the named session; without an ID, show a session picker |
| `--last` | with `--resume`, skip the picker and resume the most recent session in the current folder |
| `--fork <sessionId>` | fork the session and resume the fork |
| `--settings <path>` | merge a runtime settings file for this process only |
| `--append-system-prompt <text>` | append text to the system prompt |
| `--append-system-prompt-file <path>` | append a file's contents to the system prompt |
| `--auto <level>` | start at autonomy `low`, `medium`, or `high` |
| `--use-spec` | start in specification mode |
| `--cwd <path>` | set the working directory |
| `-w`, `--worktree [name]` | run in a Git worktree |
| `--worktree-dir <path>` | deprecated; help points to the desktop app (interactive) or `worktreeDirectory` in settings (`exec`) |
| `--disable-builtin-skills` | disable Factory-provided built-in skills |

Source: `droid --help` and `droid resume --help` on 0.218.2. The cli-reference page omits `--settings` and `--last`.

The interactive command has no `--model` or `--reasoning-effort` flag, and `-r` is `--resume`. Model and effort are exec flags, in-session controls (`/model`, Tab, Ctrl+N), or settings keys a `--settings` file can carry.

### Subcommands

| Command | Purpose |
| --- | --- |
| `droid resume [sessionId]` | resume, with `--fork`, `--last`, and the interactive launch flags |
| `droid exec [prompt]` | run non-interactively; see [`droid exec`](#droid-exec) |
| `droid search <query>` (alias `find`) | search local sessions (messages, documents, tool results) |
| `droid doctor` | diagnose configuration, connectivity, certificates, proxy, auth, and daemon; `--json`, `--verbose`, `--timeout <ms>`, and one-area filters `--connectivity`, `--config`, `--certs`, `--proxy`, `--auth`, `--daemon` |
| `droid update` | check for and install updates |
| `droid daemon` | run the Factory daemon server |
| `droid mcp`, `droid plugin`, `droid computer` | manage MCP servers, plugins, and relay computer registrations |

### Argument parsing

The 0.218.2 bundle builds the CLI with commander. The root program's default command is `tui`, declared with a variadic `[prompt...]` argument, `allowUnknownOption(true)`, and `allowExcessArguments(true)`. Three consequences follow:

- An unknown option such as `--model gpt-5` is not rejected. The TUI launches and the option's value can join the positional prompt. `droid --auto` and `droid --cwd` without a value do fail with commander's missing-argument error.
- `--` ends option parsing, so `droid -- <prompt>` passes an argument that begins with `-` as prompt text.
- After `--`, commander still matches the first operand against subcommand names before it falls back to `tui`. A prompt argv element that is exactly `exec`, `resume`, `search`, `find`, `doctor`, `update`, `daemon`, `mcp`, `plugin`, `computer`, or `help` runs that subcommand. A multi-word prompt passed as one argv element never matches.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | success |
| `1` | general runtime error |
| `2` | invalid CLI arguments or options |

Source: [cli-reference, Exit codes](https://docs.factory.ai/droid-cli/cli-reference). `droid exec` exits nonzero on a permission violation, tool error, or unmet objective and publishes no finer catalog.

## Configuration

### Settings files

The user settings file is `~/.factory/settings.json` (`%USERPROFILE%\.factory\settings.json` on Windows). `settings.local.json` beside the user file or beside a project's `.factory/settings.json` merges on top of its base file, and `--settings <path>` merges a file for one process. Organizations add managed settings through Enterprise Controls. The settings page names these files but does not state a full precedence order.

| Key | Values | Default |
| --- | --- | --- |
| `model` | model ID | product default |
| `reasoningEffort` | model-dependent | model-dependent |
| `sessionDefaultSettings.interactionMode` | `auto`, `spec` | `auto` |
| `sessionDefaultSettings.autonomyLevel` | `off`, `low`, `medium`, `high` | `off` |
| `sessionDefaultSettings.autonomyMode` | deprecated legacy combined mode | |
| `commandAllowlist` | commands that run without confirmation | safe defaults |
| `commandDenylist` | commands that always require confirmation | restrictive defaults |
| `commandBlocklist` | commands that never run, with no approval path | `[]` |
| `hooks` | hook definitions; see [Hook configuration](#hook-configuration) | |
| `hooksDisabled` | disable every hook | `false` |
| `showHookOutput` | show hook stdout and stderr in the transcript | unset |
| `statusLine` | `{ command, padding?, maxRows? }` | unset |
| `worktreeDirectory` | directory for `--worktree` | `~/.factory/worktrees` |
| `cloudSessionSync` | mirror CLI sessions to Factory web | `true` |
| `customModels` | see [Custom models](#custom-models) | `[]` |

Sources: [settings](https://docs.factory.ai/droid-cli/settings) and [auto-run](https://docs.factory.ai/autonomy-and-safety/auto-run). A command in both the allowlist and the denylist follows the denylist; `commandBlocklist` overrides both lists and `--skip-permissions-unsafe`. Commands in no list fall back to the session's autonomy level.

`statusLine` runs a shell command and renders its stdout above the input; `/statusline` edits it. Factory documents no structured session JSON on the command's stdin or environment, so it is presentation only and carries no model, context, cost, or session identity.

### Custom models

A `customModels[]` entry requires `model`. The [BYOK page](https://docs.factory.ai/model-independence/byok) documents `displayName`, `baseUrl`, `apiKey`, `apiKeyHelper`, `apiKeyHelperTtlMs`, `authMode`, `provider`, `maxOutputTokens`, `noImageSupport`, `extraArgs`, `extraHeaders`, and `bedrock`. The 0.218.2 bundle's settings schema also accepts optional `id`, `index`, `maxContextLimit`, `enableThinking`, `thinkingMaxTokens`, and `reasoningEffort`, none of which the BYOK page lists, and Droid writes `id` and `index` into entries it saves. `apiKey`, `apiKeyHelper`, `baseUrl`, `extraHeaders`, and `bedrock` can carry secrets.

A session selects a custom model with a `custom:` selector, as printed under "Custom Models" in `droid exec --help`. The 0.218.2 bundle forms it as `custom:<name>-<ordinal>`:

- `<name>` is `displayName` (or `model` when `displayName` is absent), trimmed, with each whitespace run replaced by one `-`.
- `<ordinal>` counts from `0` among loaded entries that share the same `<name>`. It is not the entry's position in the whole list.
- Entries without `baseUrl` (or `bedrock`) or with a placeholder API key such as `YOUR_API_KEY` are dropped before counting.

For example, a lone entry named `DeepSeek V4 Pro` gets `custom:DeepSeek-V4-Pro-0`. The BYOK page notes that `-r` does not apply to custom models.

The bundle still reads a legacy `config.json` in a Factory folder. Its `custom_models[]` entries use snake_case keys (`model`, `provider`, `model_display_name`, `base_url`, `api_key`, `max_context_limit`, `max_tokens`, `reasoning_effort`) and get the same selector grammar.

## Hooks

### Hook configuration

Hooks load from `~/.factory/hooks.json` (user), `.factory/hooks.json` (project), and organization-managed settings, or from a top-level `hooks` key in `settings.json`. A legacy `.factory/hooks/hooks.json` migrates to `.factory/hooks.json` on the next save and is archived as `hooks.migrated.json`. The managed setting `allowManagedHooksOnly: true` ignores user and project hooks. Plugin hook files merge with these and may reference `${DROID_PLUGIN_ROOT}` or its alias `${CLAUDE_PLUGIN_ROOT}`. Source: [hooks](https://docs.factory.ai/harness/hooks).

Each event maps to an array of matcher groups, and each group holds a `hooks` array of commands:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "type": "command", "command": "/absolute/path/to/hook", "timeout": 10 }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Create",
        "hooks": [
          { "type": "command", "command": "/absolute/path/to/hook" }
        ]
      }
    ]
  }
}
```

| Field | Meaning |
| --- | --- |
| `matcher` | tool-name filter for `PreToolUse` and `PostToolUse`: case-sensitive exact name or regular expression; `*`, empty, or omitted matches every tool |
| `commandRegex` | extra regular expression matched against an `Execute` call's shell command; an invalid expression is skipped |
| `type` | `"command"` |
| `command` | shell command |
| `timeout` | seconds; default 60 |

A hook command runs through the shell in Droid's working directory with Droid's environment. `$FACTORY_PROJECT_DIR` is the absolute directory where Droid started; the docs recommend absolute command paths because the working directory can change. Droid snapshots hooks at session start and warns when hook configuration changes outside the session; `/hooks` manages them. The docs no longer state whether matching commands run in parallel or whether identical command strings are deduplicated; the 0.218.2 bundle's hook execution records carry `isParallelExecution` and `parallelGroupId`.

The stock TUI runs its agent loop in a child `droid exec` process (see [Processes](#processes)), and the 0.218.2 bundle skips `SessionStart` hooks in any process whose mode is `terminal-ui`, so that child is where `SessionStart` fires.

### Hook input

A hook receives one JSON object on stdin. Every event carries the common fields:

```json
{
  "session_id": "00893aaf-19fa-41d2-8238-13269b9b3ca0",
  "transcript_path": "/home/me/.factory/sessions/-home-me-project/00893aaf-19fa-41d2-8238-13269b9b3ca0.jsonl",
  "cwd": "/home/me/project",
  "permission_mode": "off",
  "hook_event_name": "UserPromptSubmit",
  "message_id": "msg-id"
}
```

`permission_mode` is `off`, `spec`, `auto-low`, `auto-medium`, or `auto-high`: a display value that combines interaction mode and autonomy level. `message_id` is optional. The payload has no event ID or timestamp, so two legitimate events can be byte-identical.

| Event | When it fires | Event fields |
| --- | --- | --- |
| `SessionStart` | a session starts, resumes, clears, or compacts | `source`; optional `previous_session_id`, `calling_session_id` |
| `UserPromptSubmit` | before Droid processes a submitted prompt | `prompt`, `has_images` |
| `PreToolUse` | after tool parameters exist, before permission and execution | `tool_name`, `tool_input` |
| `PostToolUse` | after a tool completes successfully | `tool_name`, `tool_input`, `tool_response` |
| `Notification` | Droid asks the user for permission or an answer, or waits after an interrupt | `message`, `notification_type` |
| `Stop` | the agent finishes responding; not after a user interrupt | `stop_hook_active`, `tool_execution_count`, `elapsed_time` |
| `SubagentStop` | a Task subagent finishes | `task_name`, `task_result`, `task_error`, `stop_hook_active` |
| `PreCompact` | before manual or automatic compaction | `trigger`, `custom_instructions`, `message_count`, `estimated_tokens` |
| `SessionEnd` | a session closes | `reason`, `session_duration_ms`, `message_count` |

| Field | Values |
| --- | --- |
| `SessionStart.source` | `startup`, `resume`, `clear`, `compact` |
| `SessionEnd.reason` | `clear`, `logout`, `prompt_input_exit`, `other` |
| `PreCompact.trigger` | `manual`, `auto` |
| `Notification.notification_type` | `permission_prompt`, `idle_prompt`, `auth_success`, `elicitation_dialog` |

The docs list the event fields and enumerations; the 0.218.2 bundle adds the details below.

- `SessionStart` carries `previous_session_id` (the docs say only "optional prior session IDs") and `calling_session_id`, sets `message_id` except on `startup`, and passes a `CLAUDE_ENV_FILE` path (`droid-env-<session-id>.sh`).
- `SessionStart` with `source: "resume"` or `"compact"` can carry a `session_id` that differs from the resumed or compacted session. Droid has no `PostCompact` event.
- `SubagentStop.task_result` is a string (objects are JSON-encoded), and `stop_hook_active` is always `false`.
- `Notification.notification_type` call sites in the bundle: `permission_prompt` ("Factory CLI needs permission to execute N tool(s)"), `elicitation_dialog` ("Factory CLI is asking the user N question(s)", from `AskUser`), and `idle_prompt` ("Agent stopped by user and is waiting for input", after a user interrupt). The bundle has no `auth_success` call site and no timed idle notification; the docs state no idle threshold.
- `custom_instructions` is the argument given to manual compaction and empty for automatic compaction.

### Tool names

The 0.218.2 bundle names these built-in tools: `Read`, `LS`, `Glob`, `Grep`, `Create`, `Edit`, `ApplyPatch`, `Execute`, `Script`, `WaitForScript`, `TodoWrite`, `AskUser`, `WebSearch`, `FetchUrl`, `GenerateImage`, `ExitSpecMode`, and `Task`. `Create`, `Edit`, and `ApplyPatch` write files. MCP tools are named `mcp__<server>__<tool>`, and their side effects are server-defined. `droid exec --list-tools` prints the tools for the selected model.

`tool_input` and `tool_response` shapes vary by tool. The hooks page's `Create` example:

```json
{
  "tool_name": "Create",
  "tool_input": { "file_path": "/path/to/file.txt", "content": "file content" },
  "tool_response": { "filePath": "/path/to/file.txt", "success": true }
}
```

### Hook output and exit codes

Hook stdout is the decision channel. Empty stdout with exit 0 is the neutral result.

| Result | Droid behavior |
| --- | --- |
| exit `0`, empty stdout | continue |
| exit `0`, plain stdout | for `UserPromptSubmit` and `SessionStart`, added to model context |
| exit `0`, JSON stdout | apply the JSON fields below |
| exit `2` | block `PreToolUse` and `UserPromptSubmit`; feed stderr to Droid on `PostToolUse` and `Stop` |
| exit `3` | abort: `PreToolUse` stops the agent with stderr as the reason (the `/hooks` UI calls it "Immediately stops the entire droid session"); also blocks `UserPromptSubmit` |
| any other exit | non-blocking error; Droid records stderr and continues |

The docs list exits `0`, `2`, and other. Exit `3`, and `PreCompact` treating exit `2` or `3` as "blocked compaction", come from the 0.218.2 bundle.

Any JSON output may carry the common fields:

```json
{
  "continue": true,
  "stopReason": "shown to the user when continue is false",
  "suppressOutput": true,
  "systemMessage": "warning shown to the user"
}
```

`continue: false` stops processing and takes precedence over event-specific decisions.

| Event | Event-specific JSON |
| --- | --- |
| `PreToolUse` | `hookSpecificOutput.permissionDecision` (`allow` skips the permission UI, `deny` rejects the call and feeds the reason to Droid, `ask` forces the native prompt), `permissionDecisionReason`, `updatedInput` |
| `PostToolUse` | `decision: "block"`, `reason`, `hookSpecificOutput.additionalContext` |
| `UserPromptSubmit` | `decision: "block"` (erases the prompt and shows `reason` to the user only), `reason`, `additionalContext` |
| `Stop`, `SubagentStop` | `decision: "block"` with a required `reason` asks the agent to continue; `stop_hook_active` guards against loops |
| `SessionStart` | `hookSpecificOutput.additionalContext` |

The current docs do not mention a top-level `decision: "approve"`. The 0.218.2 bundle's output schema still accepts `decision: "block" | "approve"`.

## Sessions

### Session identity and resume

Every hook carries `session_id` and `transcript_path`. The argv of a freshly launched TUI does not contain its generated session ID, so a hook is the first place the ID appears.

| Command | Behavior |
| --- | --- |
| `droid --resume <id>`, `droid resume <id>` | resume the named session |
| `droid --resume`, `droid resume` | show a session picker; add `--last` for the most recent session in the current folder |
| `droid --fork <id>` | fork the session and resume the fork |
| `/fork` | copy the current session into a new session; the user stays in the original, and Droid prints a resume command for the copy |
| `/sessions` | list and select previous sessions |
| `/rewind-conversation` | rewind to an earlier message |
| `/new` | start a fresh session and reset model and autonomy to defaults |
| `/clear` | alias for `/new` that keeps the current model and autonomy |
| `/compress [instructions]` | compress the session into a new session with a summary; `/compact` and `/handoff` are aliases |
| `/archive` | archive the session (Ctrl+X) |

Sources: [cli-reference](https://docs.factory.ai/droid-cli/cli-reference) and the 0.218.2 bundle's command registry, which lists `/compact` and `/handoff` as `/compress` aliases that the docs omit.

### Session files

Factory publishes no schema, durability rule, directory-key algorithm, or locking contract for session files. The hook's `transcript_path` is the only published pointer. Local sessions sit at:

```text
~/.factory/sessions/<cwd-key>/<session-id>.jsonl
~/.factory/sessions/<cwd-key>/<session-id>.settings.json
```

Local `<cwd-key>` values replace each `/` in the working directory with `-`. The hooks docs' examples show a different project-cache path.

The JSONL file opens with a `session_start` record: `{"type":"session_start","version":2,"id",...}` with `title`, `owner`, `cwd`, and optional `parent`, `lastCwd`, and `organizationId`. The bundle rejects a file with no `session_start` record ("Invalid session file: missing session_start event"). Later records are `type: "message"` or `type: "compaction_state"`.

A `message` record carries a non-empty `id`, optional `parentId`, an RFC 3339 `timestamp`, and a nested `message`:

| Field | Meaning |
| --- | --- |
| `message.role` | `user` or `assistant` for conversation records |
| `message.content[]` | blocks: `text` (`text`), `thinking`, `tool_use` (`name`, `input`), `tool_result`, `document` |
| `message.visibility` | absent for visible conversation; `llm_only` for injected model context; `user_only` for UI records, including hook audit rows that carry `hookEventName` |
| `message.modelId`, `message.reasoningEffort` | raw model and effort on assistant messages |

`parentId` links form a tree, because rewind and branching append records whose parent is an earlier record. A native question appears as an assistant `tool_use` block named `AskUser`; a later tool result or assistant record follows the answer.

The sibling `<session-id>.settings.json` is a snapshot Droid rewrites during the session:

| Key | Meaning |
| --- | --- |
| `model`, `reasoningEffort` | current raw model selector and effort |
| `interactionMode`, `autonomyLevel`, `autonomyMode` | current mode |
| `tokenUsage` | root-session cumulative `inputTokens`, `outputTokens`, `cacheCreationTokens`, `cacheReadTokens`, `thinkingTokens`, optional `factoryCredits` |
| `inclusiveTokenUsage` | the same counters including child sessions |
| `childInclusiveTokenUsageBySessionId` | per-child counters |
| `lastCallTokenUsage` | the most recent model call: `inputTokens`, `cacheReadTokens`, optional `outputTokens` |

`lastCallTokenUsage` carries no cache-creation count at 0.218.2: both the bundle's schema (`pick({inputTokens, cacheReadTokens})` plus optional `outputTokens`) and local snapshots have only those three keys. The SDK describes it as the usage behind the compaction meter. `factoryCredits` is Factory's credit unit, not a currency amount.

### Processes

The stock TUI process runs as `droid [flags] [prompt]` and spawns its agent loop as a child `droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc` followed by extra arguments (ACP clients get `droid exec --output-format acp`). Both processes read the same hook configuration, and the hook payload names no emitting process. A `droid exec` launched directly has no TUI parent. Source: the 0.218.2 bundle's process spawner, which logs `[droid process] Spawning` with the argv.

## Modes, models, and effort

| Setting or protocol field | Values |
| --- | --- |
| model | Factory model ID or `custom:` selector; `droid exec --help` lists the catalog and each model's supported efforts and default |
| reasoning effort | model-dependent; the protocol enum is `none`, `dynamic`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |
| interaction mode | `auto`, `spec` in settings; the protocol adds `agi` and `mission` |
| autonomy level | `off`, `low`, `medium`, `high` |
| autonomy mode (deprecated) | `normal`, `spec`, `auto-low`, `auto-medium`, `auto-high`; hook `permission_mode` reports `normal` as `off` |

Specification mode is a read-only planning phase that ends with an `ExitSpecMode` plan approval. In the TUI, Shift+Tab toggles Normal and Spec mode (Mission mode is not in the cycle), Ctrl+L cycles autonomy, Tab cycles the model's reasoning efforts, Ctrl+N cycles models, and `/model` opens the model selector. Source: [specification mode](https://docs.factory.ai/autonomy-and-safety/specification-mode) and [cli-reference](https://docs.factory.ai/droid-cli/cli-reference).

The interactive TUI takes launch-scoped `--auto <level>` and `--use-spec`; `--skip-permissions-unsafe` exists only on `droid exec`.

## Subagents and missions

Custom droids are Task-tool subagents defined as Markdown files under project `.factory/droids/` or user `~/.factory/droids/`; a project definition overrides a user definition with the same name. Frontmatter sets the prompt, `model` (or `inherit`), `reasoningEffort`, tool policy, and `mcpServers`. A Task child gets its own context window, and its tool progress streams through the parent's Task call. Source: [subagents](https://docs.factory.ai/harness/subagents).

Hooks report only `SubagentStop`, with `task_name`, `task_result`, and `task_error` and no child session ID, parent tool-call ID, subagent type, model, or transcript path. There is no subagent start event.

Missions are multi-agent orchestration: `droid exec --mission` upgrades the session to an orchestrator that spawns worker sessions. It requires `--auto high` or `--skip-permissions-unsafe`, auto-approves proposals, and takes `--worker-model`, `--worker-reasoning-effort`, `--validator-model`, and `--validator-reasoning-effort`. Source: `droid exec --help` on 0.218.2; the droid-exec docs page does not list these flags.

## droid exec

`droid exec` runs one prompt non-interactively and is read-only unless given `--auto` or `--skip-permissions-unsafe`.

| Flag | Meaning |
| --- | --- |
| `[prompt]`, `-f`, `--file <path>` | prompt as argument, from a file, or from stdin |
| `-o`, `--output-format <format>` | `text` (default), `json`, `stream-json`, `stream-jsonrpc` |
| `--input-format <format>` | `stream-jsonrpc` for JSON-RPC control; `stream-json` for multi-turn (deprecated, see below) |
| `-s`, `--session-id <id>` | continue an existing session; requires a prompt; old messages are not replayed to output |
| `--fork <id>` | fork a session and continue the fork; requires a prompt |
| `-m`, `--model <id>` | model; default shown in help |
| `-r`, `--reasoning-effort <level>` | reasoning effort |
| `--use-spec`, `--spec-model <id>`, `--spec-reasoning-effort <level>` | start in spec mode and choose its model and effort |
| `--auto <level>` | autonomy `low`, `medium`, or `high` |
| `--skip-permissions-unsafe` | bypass every permission check; cannot combine with `--auto` |
| `--only-tools`, `--add-tools`, `--remove-tools <ids>` | restrict, add, or remove tools by ID or `MCP:<server>[/<tool>]` selector |
| `--list-tools` | print tools for the selected model and exit |
| `--disable-builtin-skills` | disable Factory-provided built-in skills |
| `--append-system-prompt <text>`, `--append-system-prompt-file <path>` | append to the system prompt |
| `--cwd <path>` | working directory |
| `-w`, `--worktree [name]` | run in a Git worktree |
| `--tag <spec>` | session tag, as a name or JSON object; repeatable |
| `--log-group-id <id>` | log group ID |
| `--mission` and mission model flags | see [Subagents and missions](#subagents-and-missions) |

Source: `droid exec --help` on 0.218.2. The docs disagree on tool flags: [cli-reference](https://docs.factory.ai/droid-cli/cli-reference) and [droid-exec](https://docs.factory.ai/droid-exec/overview) show `--restrict-tools`, `--additional-tools`, and `--disabled-tools`, while the binary's help lists `--only-tools`, `--add-tools`, and `--remove-tools`.

In stream JSON-RPC mode, CLI flags do not configure the session: `-m`, `--auto`, `-r`, and `--disable-builtin-skills` are validated, but session settings come from JSON-RPC requests (help text). With `--input-format stream-json`, the 0.218.2 bundle requires `--output-format stream-json` (or `debug`) and prints "Warning: --input-format stream-json is deprecated. Use --input-format stream-jsonrpc for daemon-compatible JSON-RPC protocol." The help text does not mark it deprecated. The bundle's output-format validator also accepts `acp`, `acp-daemon`, and `debug`, which help does not list.

### JSON output

`--output-format json` prints one result object:

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

The 0.218.2 bundle sets `subtype` to `"failure"` with `is_error: true` on failure, and adds a `usage` field the docs example omits. The docs publish no event schema for `--output-format stream-json`.

### Stream JSON-RPC protocol

This section is pinned to `@factory/droid-sdk` 0.9.1, protocol `1.201.1`; see [Baseline](#baseline). Start the process with:

```sh
droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc
```

Each stdin and stdout line is one JSON-RPC message; stdout carries only protocol messages and stderr carries diagnostics. The SDK's `ProcessTransport` chains writes so messages never interleave. On close it ends stdin, sends SIGTERM after 2000 ms if the process is still running, and SIGKILL 3000 ms later (`package/dist/node.js`, `ManagedProcessImpl.close`). Its default timeouts are 30 s per request, 60 s for session initialization, and 240 s for compaction.

#### Envelope

```json
{
  "jsonrpc": "2.0",
  "factoryApiVersion": "1.0.0",
  "factoryProtocolVersion": "1.201.1",
  "type": "request",
  "id": "client-generated-id",
  "method": "droid.initialize_session",
  "params": {}
}
```

| Field | Rule |
| --- | --- |
| `jsonrpc` | `"2.0"` |
| `factoryApiVersion` | required literal `"1.0.0"`, described as deprecated in favor of `factoryProtocolVersion` |
| `factoryProtocolVersion` | optional string; a mismatch is reported with `localFactoryProtocolVersion` and `peerFactoryProtocolVersion` |
| `_meta` | optional trace context: `traceparent`, `tracestate`, `requestAttribution` |
| `type` | `request` (string `id`, `method`, `params`), `response` (`id` plus `result` or `error { code, message, data? }`), or `notification` (`method`, `params`, no `id`) |

Error codes are the JSON-RPC set (`-32700`, `-32600`, `-32601`, `-32602`, `-32603`) plus `-32001` authentication, `-32004` entity not found, `-32005` session disconnected, and `-32006` conflict. Source: `JsonRpcEnvelopeSchema` and `JsonRpcErrorCode` in `package/dist/chunk-YSKO6SPT.js`.

#### Client requests

| Method | Params | Result |
| --- | --- | --- |
| `droid.initialize_session` | `machineId`, `cwd`; optional `sessionId`, `modelId`, `reasoningEffort`, `interactionMode`, `autonomyLevel`, `mcpServers`, `systemPrompt`, `autoRejectPermissionRequests`, `structuredOutputFormat`, `callingMetadata`, `tags`, `title`, `skipPermissionsUnsafe`, tool overrides, worktree options | `sessionId`, `session`, `settings`; optional `hostId`, `mcpServers`, `gitRepo`, `availableModels`, `worktree` |
| `droid.load_session` | `sessionId`; optional `messageLimit`, `loadAllMessages`, `autoRejectPermissionRequests`, `structuredOutputFormat` | `session`, `settings`; optional `pendingPermissions[]` and `pendingAskUserRequests[]` (request params plus `requestId`), `isAgentLoopInProgress`, `workingState`, `queuedMessages`, `tokenUsage`, `inclusiveTokenUsage`, `lastCallTokenUsage`, `agentTurnOutcome`, `hasOlderMessages`, `cwd`, `callingSessionId`, `callingToolUseId`, `mission`, `subagentInvocations` |
| `droid.add_user_message` | `text`; optional `messageId`, `content[]`, `images`, `imagePaths`, `files`, `outputFormat`, `queuePlacement` (`end_of_turn`, `end_of_loop`) | `{}` |
| `droid.interrupt_session` | `{}` | `{}` |
| `droid.close_session` | optional `reason`: `clear`, `logout`, `prompt_input_exit`, `other` | `{}` |
| `droid.update_session_settings` | optional `modelId`, `reasoningEffort`, `interactionMode`, `autonomyLevel`, `specModeModelId`, `specModeReasoningEffort`, `compactionTokenLimit`, tool overrides | `{}` |
| `droid.compact_session` | optional `customInstructions` | `newSessionId`, `removedCount` |
| `droid.fork_session` | optional `title`, `tags` | `newSessionId` |
| `droid.get_context_stats` | `{}` | `used`, `remaining`, `limit`, `accuracy` (`exact`, `estimated`), `updatedAt` |
| `droid.rename_session` | `title` | `success` |
| `droid.kill_worker_session` | `workerSessionId` | `{}` |
| `droid.change_working_directory` | `workingDirectory` | `resolvedPath` |

The SDK's `DroidServerMethod` enum also names `append_messages`, `resolve_queued_user_message`, `list_models`, `list_tools`, `list_skills`, `set_skill_disabled`, `list_commands`, `get_rewind_info`, `execute_rewind`, `get_context_breakdown`, `warmup_cache`, `submit_bug_report`, and eleven MCP management methods (`toggle_mcp_server`, `add_mcp_server`, `list_mcp_tools`, and so on).

#### Session notifications

Every event arrives as `droid.session_notification` with `params: { sessionId?, notification }`, discriminated by `notification.type`:

```json
{
  "type": "notification",
  "method": "droid.session_notification",
  "params": {
    "sessionId": "session-id",
    "notification": { "type": "droid_working_state_changed", "newState": "executing_tool" }
  }
}
```

| `type` | Key fields |
| --- | --- |
| `droid_working_state_changed` | `newState`: `idle`, `thinking`, `streaming_assistant_message`, `waiting_for_tool_confirmation`, `executing_tool`, `compacting_conversation` |
| `agent_turn_completed` | `reason`, optional `turnId`, `tokenUsage`, optional cumulative and child usage, `durationMs` |
| `create_message` | `message`, optional `parentId`, `requestId` |
| `assistant_text_delta`, `thinking_text_delta` | `messageId`, `blockIndex`, `textDelta` |
| `assistant_text_complete`, `thinking_text_complete` | `messageId`, `blockIndex`; thinking adds optional `durationMs` |
| `assistant_message_retracted` | `messageId` |
| `structured_output` | `messageId`, `structuredOutput` |
| `tool_call` | `toolUse { id, name, input }` |
| `tool_result` | tool-result block plus `messageId` |
| `tool_progress_update` | `toolUseId`, `toolName`, `update` (optional `subagentSessionId`) |
| `tool_execution_phase_changed` | `toolUseId`, `toolName`, `phase` |
| `tool_execution_heartbeat` | `toolUseId`, `toolName` |
| `permission_resolved` | `requestId`, `toolUseIds[]`, `selectedOption` |
| `error` | `message`, `errorType`, `timestamp`, optional `exitCode`, `error` |
| `llm_retry` | `attempt`, `reason`: `overloaded`, `rate_limited`, `timeout`, `network`, `empty_response`, `unknown` |
| `session_token_usage_changed` | `sessionId`, `tokenUsage`, optional `inclusiveTokenUsage`, `lastCallTokenUsage` |
| `settings_updated` | current model, effort, modes, tool overrides, tags |
| `session_compacted` | `summaryId`, `removedCount`, `visibleBoundaryMessageId` |
| `session_title_updated` | `title`, optional `updateType` |
| `session_working_directory_changed` | `cwd` |
| `child_session_available` | `childSessionId`, optional `toolUseId`, `subagentType`, `description`; `timestamp` |
| `mission_worker_started`, `mission_worker_completed` | `workerSessionId`; completed adds `exitCode` |
| `mission_state_changed`, `mission_features_changed`, `mission_progress_entry`, `mission_heartbeat` | mission state |
| `hook_execution_started`, `hook_execution_completed` | `hookId`, `hookEventName`, commands and results |
| `loop_state_changed`, `queued_messages_discarded`, `mcp_status_changed`, `mcp_auth_required`, `mcp_auth_completed` | loop, queue, and MCP state |

`agent_turn_completed.reason` is one of `completed`, `cancelled`, `permission_rejected`, `error`, `process_exit`, `spec_handoff`, `structured_output_missing`, `structured_output_invalid`, `structured_output_schema_invalid`, `model_usage_exhausted`, `model_authentication_failed`, `model_request_rejected`, `model_provider_unreachable`, `model_provider_unavailable`, `model_rate_limited`, `prompt_rejected`, `completion_persistence_failed`, or `no_approver_available`.

`create_message.message` has `id`, `role` (`user`, `assistant`, `tool`, `system`), `content[]` blocks (`text`, `image`, `thinking`, `redacted_thinking`, `tool_use`, `tool_result`, `document`), numeric `createdAt` and `updatedAt`, and optional `parentId`, `visibility` (`both`, `llm_only`, `user_only`), and `isError`.

`tokenUsage` has `inputTokens`, `outputTokens`, `cacheCreationTokens`, `cacheReadTokens`, `thinkingTokens`, and optional `factoryCredits`; `lastCallTokenUsage` has `inputTokens`, `cacheReadTokens`, and optional `outputTokens`. The protocol carries no price, rate-limit window, reset time, or plan.

#### Permissions and questions

`droid.request_permission` is a server-to-client request; the client answers with the same `id`.

```json
{
  "type": "request",
  "id": "server-id",
  "method": "droid.request_permission",
  "params": {
    "toolUses": [
      {
        "toolUse": { "type": "tool_use", "id": "tool-1", "name": "Execute", "input": { "command": "cargo test" } },
        "confirmationType": "exec",
        "details": { "type": "exec", "fullCommand": "cargo test", "command": "cargo test", "impactLevel": "medium" }
      }
    ],
    "options": [
      { "label": "Run once", "value": "proceed_once" },
      { "label": "Cancel", "value": "cancel" }
    ]
  }
}
```

`params` may also carry `associatedSessionIds[]`. `confirmationType` is `edit`, `exec`, `create`, `ask_user`, `exit_spec_mode`, `propose_mission`, `start_mission_run`, `apply_patch`, `mcp_tool`, `sandbox_violation`, or `droid_shield_violation`, each with a matching `details` shape; the 0.218.2 bundle's enum also has `recommend_mission` and `exit_mission_planning`. The response is `{ "selectedOption": "<value>", "comment"?: "...", "editedSpecContent"?: "..." }`, where `selectedOption` is one of the offered `options[].value` and `editedSpecContent` is required with `proceed_edit`. The outcome enum is `proceed_once`, `proceed_always`, `proceed_always_file`, `proceed_auto_run`, `proceed_auto_run_low`, `proceed_auto_run_medium`, `proceed_auto_run_high`, `proceed_new_session`, `proceed_new_session_low`, `proceed_new_session_medium`, `proceed_new_session_high`, `proceed_edit`, `proceed_always_tools`, `proceed_always_server`, `proceed_report_false_positive`, and `cancel`.

`droid.ask_user` carries a `toolCallId` and questions; the client answers with answers keyed by `index`:

```json
{
  "method": "droid.ask_user",
  "params": {
    "toolCallId": "tool-ask-1",
    "questions": [
      { "index": 1, "topic": "Database", "question": "Which migration strategy?", "options": ["online", "maintenance window"], "multiSelect": false }
    ]
  }
}
```

```json
{ "cancelled": false, "answers": [{ "index": 1, "question": "Which migration strategy?", "answer": "online" }] }
```

## Authentication, usage, and diagnostics

Interactive first run opens a browser sign-in. Automation sets `FACTORY_API_KEY` (created at <https://app.factory.ai/settings/api-keys>); `droid exec` without a key uses the stored login. Credentials are stored under `~/.factory/` or in the OS keyring, and Factory publishes no credential file schema. `FACTORY_DISABLE_KEYRING` and `FACTORY_LOG_FILE` are read by the 0.218.2 bundle but absent from the current reference pages.

`droid doctor --auth` checks credentials, and `--json` prints `{ "ok": <bool>, "results": [{ "id", "category", "status", "detail" }] }` with `status` `pass`, `warn`, or `fail`; `auth.verify` reports whether usable credentials exist. The shape was observed from 0.218.2; Factory documents no `doctor` page.

`/cost` shows session token usage and cost, `/stats [period]` shows usage statistics, and `/account` and `/billing` open the web settings. These are interactive views, not machine-readable surfaces. Factory publishes no local quota, balance, or rate-limit API for the CLI.

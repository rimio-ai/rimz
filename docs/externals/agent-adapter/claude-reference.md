# Claude Code protocol reference

> The mapping onto RimZ's internal types lives beside this doc: [adapter_claude.md](../../internals/agents/adapter_claude.md) maps the hooks, statusline, transcript, account, and spend surfaces onto RimZ's internal types; the agent-agnostic model is [model.md](../../internals/agents/model.md) and the account/spend model is [providers.md](../../internals/agents/providers.md).

This is the single home for the **Claude Code upstream protocol surface** RimZ binds to — the hook events, their stdin payloads and stdout decision schema, the statusline JSON, the auth surface, and the local-OAuth usage endpoint. It is a hand-maintained mirror of Anthropic's published docs plus the credential-file surfaces Claude Code itself uses, kept for fast lookup and pinned to the source URLs below so it can be refreshed when upstream moves. The [`ClaudeAdapter`](../../../crates/rimz/src/agents/adapters/claude/mod.rs) adapter is the only code that reads this surface; everything downstream of it speaks RimZ's internal types.

Coverage is **depth on what RimZ wires, breadth as an index**: the events, statusline fields, and decision shapes the adapter actually parses or emits are documented in full; the rest of the upstream catalog is listed so a contributor wiring a new event knows it exists.

## Upstream sources

Re-fetch these pages to refresh this mirror. `docs.claude.com/en/docs/claude-code/*` 301-redirects to `code.claude.com/docs/en/*` — the `code.claude.com` form is canonical.

| Surface | Source |
| --- | --- |
| Hooks reference (events, payloads, decision schema, exit codes) | <https://code.claude.com/docs/en/hooks> |
| Statusline (full JSON schema, `subagentStatusLine`) | <https://code.claude.com/docs/en/statusline> |
| Subagents | <https://code.claude.com/docs/en/sub-agents> |
| Agent view and background-session supervisor | <https://code.claude.com/docs/en/agent-view> |
| Settings (`statusLine` / `hooks` config keys, `disableAgentView`) | <https://code.claude.com/docs/en/settings> |
| CLI flags (system-prompt append) | <https://code.claude.com/docs/en/cli-reference> |
| Sessions (resume and fork CLI flags) | <https://code.claude.com/docs/en/sessions> |
| Remote Control (`remote-control`, `--remote-control`, `/remote-control`, version floor, settings) | <https://code.claude.com/docs/en/remote-control> |
| Release history (version floors and protocol additions) | <https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md> |
| OAuth usage endpoint | Claude Code credential-file traffic; no public schema page |
| Transcript JSONL | no official schema published — see [Transcript JSONL](#transcript-jsonl) below |

## Session resume and fork

`claude --resume <id>` reopens a session in place. `claude --resume <id> --fork-session` copies its conversation into a provider-assigned new session id and leaves the source session untouched; RimZ uses that native fork argv and sets the source worktree as the process cwd.

## System-prompt append

`--append-system-prompt <text>` appends launch-scoped text to Claude's default system prompt. RimZ uses the flag for the supervised-subagent no-delegation reminder; it remains separate from the user-facing typed replacement surface built on `--system-prompt-file`.

## Hooks

A hook is a command Claude Code runs at a lifecycle point. Claude writes a JSON payload to the hook's **stdin** and reads the hook's **stdout** as a decision. Each event is wired in `settings.json` under `hooks.<EventName>[]`, optionally gated by a `matcher` (a tool-name or source pattern).

### Common input

Every hook receives these fields on stdin (some are event- or context-gated):

```json
{
  "session_id": "string — current session identifier",
  "prompt_id": "UUID — current prompt correlation id; absent before first input (v2.1.196+)",
  "transcript_path": "string — path to the conversation JSONL",
  "cwd": "string — working directory when the hook is invoked",
  "permission_mode": "default | plan | acceptEdits | auto | dontAsk | bypassPermissions",
  "effort": { "level": "low | medium | high | xhigh | max" },
  "hook_event_name": "string — the event that fired",
  "agent_id": "string — subagent id, present only inside a subagent",
  "agent_type": "string — agent name, present under --agent or inside a subagent"
}
```

`prompt_id` correlates hook callbacks with the statusline and OpenTelemetry events for one user prompt. `permission_mode` and `effort` are not present on every event; `effort` rides events with a tool-use context (`PreToolUse`, `PostToolUse`, `Stop`, `SubagentStop`) when the model supports the parameter. RimZ parses around `permission_mode` without consuming it — the upstream still sends it; the agent model derives the turn phase from tool events instead. `agent_id` / `agent_type` appear only with `--agent` or inside a subagent. The transcript is written asynchronously and can lag the in-memory conversation at hook time; `Stop` and `SubagentStop` provide `last_assistant_message` for the just-finished response.

### Decision and output schema

On **exit 0**, Claude parses stdout as JSON. Universal fields:

```json
{
  "continue": "boolean — default true; false stops Claude entirely",
  "stopReason": "string — message shown when continue is false",
  "suppressOutput": "boolean — default false; hide stdout from the transcript",
  "systemMessage": "string — warning surfaced to the user",
  "decision": "block — top-level block for the events that support it",
  "reason": "string — explanation paired with a block",
  "hookSpecificOutput": { "hookEventName": "string", "...": "per-event fields below" }
}
```

Per-event decision control rides `hookSpecificOutput` (or, for the post-* and stop family, the top-level `decision: "block"` + `reason`):

| Event(s) | Decision pattern | Key fields |
| --- | --- | --- |
| `PreToolUse` | `hookSpecificOutput` | `permissionDecision` (`allow`\|`deny`\|`ask`\|`defer`), `permissionDecisionReason`, `updatedInput`, `additionalContext` |
| `PermissionRequest` | `hookSpecificOutput` | `decision.behavior` (`allow`\|`deny`), `decision.updatedInput`, `decision.appliedRule` |
| `PermissionDenied` | `hookSpecificOutput` | `retry` (boolean) |
| `UserPromptSubmit`, `UserPromptExpansion`, `PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `Stop`, `SubagentStop`, `ConfigChange`, `PreCompact` | top-level `decision` | `decision: "block"`, `reason`; `Stop` / `SubagentStop` also accept `hookSpecificOutput.additionalContext` |
| `TeammateIdle`, `TaskCreated`, `TaskCompleted` | exit / stop | exit 2 blocks; `continue: false` stops the teammate |
| `SessionStart`, `Setup`, `SubagentStart` | context-only | `hookSpecificOutput.additionalContext`; SessionStart also accepts `initialUserMessage`, `watchPaths`, `sessionTitle`, `reloadSkills` |
| `MessageDisplay` | display rewrite | `hookSpecificOutput.displayContent` |
| `WorktreeCreate` | path return | command stdout path, or `hookSpecificOutput.worktreePath` for HTTP |
| `Elicitation`, `ElicitationResult` | MCP interaction | `hookSpecificOutput.action` (`accept`\|`decline`\|`cancel`) and optional `content` |

### Exit codes

- **0** — success; stdout is parsed as the JSON above. For `UserPromptSubmit`, `UserPromptExpansion`, and `SessionStart`, plain stdout is injected as context Claude can read; for other events it goes to the debug log.
- **2** — blocking error; stdout and any JSON are ignored, stderr is fed back to Claude (e.g. `PreToolUse` blocks the call, `UserPromptSubmit` rejects the prompt, `Stop` prevents stopping). Exit 2 still blocks when stdout looks like JSON but fails the decision schema; a malformed success payload cannot downgrade the blocking exit.
- **other** — non-blocking error; a `<hook> hook error` notice plus the first stderr line surfaces and execution continues.

### Hooks RimZ wires

These are the events the [`ClaudeAdapter`](../../../crates/rimz/src/agents/adapters/claude/mod.rs) `INSTALLED_EVENTS` constant installs. The native-event → RimZ status mapping is the [adapter_claude.md → Hooks and lifecycle](../../internals/agents/adapter_claude.md#hooks-and-lifecycle); the columns here are the upstream fire-time and the event-specific stdin fields the adapter reads.

| Event | Fires | Event-specific input | RimZ channel |
| --- | --- | --- | --- |
| `SessionStart` | session begins or resumes | `source` (`startup`\|`resume`\|`clear`\|`compact`\|`fork`), `model`, `session_title` | lifecycle |
| `UserPromptSubmit` | prompt submitted, before processing | `prompt` | lifecycle |
| `PreToolUse` | before a tool call (can block) | `tool_name`, `tool_input` | lifecycle proof-of-work, or blocking when `tool_name` is `ExitPlanMode` / `AskUserQuestion` |
| `PostToolUse` | after a tool call succeeds | `tool_name`, `tool_input`, `tool_response` | lifecycle (silent; audit/enrichment) |
| `Stop` | Claude finishes responding | `stop_hook_active`, `last_assistant_message`; `background_tasks[]` and `session_crons[]` (v2.1.145+) | lifecycle |
| `StopFailure` | a turn ends on an API error instead of `Stop` | `error`, optional `error_details`, optional `last_assistant_message` | context-only turn-error marker |
| `SubagentStart` | a subagent is spawned | `agent_type`, `agent_id` | lifecycle |
| `SubagentStop` | a subagent finishes | `stop_hook_active`, `agent_type`, `agent_id`, `agent_transcript_path`, `last_assistant_message`; parent-scoped `background_tasks[]` and `session_crons[]` | lifecycle |
| `PreCompact` | before context compaction | `trigger` (`manual`\|`auto`) | lifecycle (`Compacting`) |
| `PostCompact` | after compaction completes | `trigger` (`manual`\|`auto`) | lifecycle (`CompactionEnded`) |
| `SessionEnd` | session terminates | `reason` | lifecycle (`ends_session`) |
| `Notification` | Claude Code sends a notification | `message` | lifecycle (silent) |
| `PermissionRequest` | a permission dialog appears | `tool_name`, `tool_input`, `permission_mode` | awaiting-user (sync) |

`ExitPlanMode` and `AskUserQuestion` have no dedicated install entry — they self-classify off `tool_name` on the broad `PreToolUse` hook.

Compaction uses `PreCompact` as the opener. `PostCompact` closes with a known trigger when it arrives, and `SessionStart` with `source = "compact"` is triggerless close evidence so RimZ still closes and counts the bracket when `PostCompact` is missed.

**Model field format.** Only `SessionStart` can receive `model`, and upstream does not guarantee it is present. Observed extended-context launches may carry the capability marker `claude-opus-4-8[1m]`, which signals a 1,000,000-token context window. RimZ strips the marker at reduce time ([model.md → The rollup](../../internals/agents/model.md#the-rollup)) and uses it to derive the window divisor ([adapter_claude.md → Context and transcript](../../internals/agents/adapter_claude.md#context-and-transcript)).

**Decision shapes RimZ renders.** A `PermissionRequest` answer:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow" } } }
```

A plan approval or user question answers on the `PreToolUse` event and **requires** `updatedInput` (a missing field is a hard render error):

```json
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": {} } }
```

The neutral path is empty stdout, exit 0. Exact bytes are the inline goldens in [`claude/mod.rs`](../../../crates/rimz/src/agents/adapters/claude/mod.rs).

### Full event catalog (index)

The complete upstream set. ✓ marks what RimZ wires today; the rest is available for future wiring.

| Event | Fires | Wired |
| --- | --- | :---: |
| `SessionStart` | session begins or resumes | ✓ |
| `Setup` | `--init-only`, or `--init`/`--maintenance` in `-p` mode | |
| `UserPromptSubmit` | prompt submitted, before processing | ✓ |
| `UserPromptExpansion` | a typed command expands into a prompt (can block) | |
| `PreToolUse` | before a tool call (can block) | ✓ |
| `PermissionRequest` | a permission dialog appears | ✓ |
| `PermissionDenied` | a call is denied by the auto-mode classifier | |
| `PostToolUse` | after a tool call succeeds | ✓ |
| `PostToolUseFailure` | after a tool call fails | |
| `PostToolBatch` | after a batch of parallel calls resolves | |
| `Notification` | Claude Code sends a notification | ✓ |
| `MessageDisplay` | while assistant message text is displayed | |
| `SubagentStart` | a subagent is spawned | ✓ |
| `SubagentStop` | a subagent finishes | ✓ |
| `TaskCreated` | a task is created via `TaskCreate` | |
| `TaskCompleted` | a task is marked completed | |
| `Stop` | Claude finishes responding | ✓ |
| `StopFailure` | the turn ends on an API error | ✓ |
| `TeammateIdle` | an agent-team teammate is about to idle | |
| `InstructionsLoaded` | a `CLAUDE.md` / rules file is loaded | |
| `ConfigChange` | a config file changes mid-session | |
| `DirectoryAdded` | a directory is added through `/add-dir` or `--add-dir` | |
| `CwdChanged` | the working directory changes | |
| `FileChanged` | a watched file changes on disk | |
| `WorktreeCreate` | a worktree is being created | |
| `WorktreeRemove` | a worktree is being removed | |
| `PreCompact` | before context compaction | ✓ |
| `PostCompact` | after compaction completes | ✓ |
| `Elicitation` | an MCP server requests user input | |
| `ElicitationResult` | after a user responds to an elicitation | |
| `SessionEnd` | session terminates | ✓ |

## Statusline JSON

Claude `exec`s the configured `statusLine` command on every render and pipes this JSON to its stdin. RimZ wraps that command with `rimz statusline feed --source claude`; [`StatuslinePayload`](../../../crates/rimz/src/agents/adapters/claude/statusline.rs) parses the blob and the wrap forwards it unchanged to any prior command. The statusline runs locally and consumes no API tokens.

Claude captures the command's stdio rather than attaching it to the terminal. Claude Code 2.1.153+ exports `COLUMNS` and `LINES` for scripts that need the current terminal dimensions.

**Update triggers.** The command runs after each new assistant message, after `/compact`, on a permission-mode change, and on a vim-mode toggle (debounced 300ms; an in-flight run is cancelled when a new update arrives). `refreshInterval` (seconds, min 1) adds a fixed timer for idle/time-based segments.

**Full schema** (the upstream example with verified additions):

```json
{
  "cwd": "/current/working/directory",
  "session_id": "abc123...",
  "session_name": "my-session",
  "prompt_id": "550e8400-e29b-41d4-a716-446655440000",
  "transcript_path": "/path/to/transcript.jsonl",
  "model": {
    "id": "claude-opus-4-8",
    "display_name": "Opus"
  },
  "workspace": {
    "current_dir": "/current/working/directory",
    "project_dir": "/original/project/directory",
    "added_dirs": [],
    "git_worktree": "feature-xyz",
    "repo": {
      "host": "github.com",
      "owner": "anthropics",
      "name": "claude-code"
    }
  },
  "version": "2.1.90",
  "output_style": {
    "name": "default"
  },
  "cost": {
    "total_cost_usd": 0.01234,
    "total_duration_ms": 45000,
    "total_api_duration_ms": 2300,
    "total_lines_added": 156,
    "total_lines_removed": 23
  },
  "costBasis": "managed",
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
  "effort": {
    "level": "high"
  },
  "fast_mode_state": "on",
  "fast_mode_disabled_reason": null,
  "thinking": {
    "enabled": true
  },
  "rate_limits": {
    "five_hour": {
      "used_percentage": 23.5,
      "resets_at": 1738425600
    },
    "seven_day": {
      "used_percentage": 41.2,
      "resets_at": 1738857600
    }
  },
  "vim": {
    "mode": "NORMAL"
  },
  "agent": {
    "name": "security-reviewer"
  },
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

**Field reference**:

| Field | Meaning |
| --- | --- |
| `model.id`, `model.display_name` | current model identifier and display name |
| `cwd`, `workspace.current_dir` | working directory (same value; `workspace.current_dir` preferred) |
| `workspace.project_dir` | directory Claude Code launched in |
| `workspace.added_dirs` | dirs added via `/add-dir` / `--add-dir`; `[]` if none |
| `workspace.git_worktree` | worktree name when inside a linked git worktree; absent in the main tree |
| `workspace.repo.{host,owner,name}` | repo identity from the `origin` remote; absent outside a repo |
| `cost.total_cost_usd` | estimated session cost, client-side |
| `cost.total_duration_ms` | wall-clock time since session start |
| `cost.total_api_duration_ms` | time spent waiting on API responses |
| `cost.total_lines_added`, `cost.total_lines_removed` | lines changed |
| `costBasis` | price table used for the most recent request on the current model: `list`\|`managed`\|`unknown`; overwritten per request, absent until pricing occurs, and consumers should treat absence as `list` |
| `context_window.total_input_tokens`, `total_output_tokens` | tokens in the current context window (current, not cumulative, since v2.1.132); RimZ skips them — `current_usage` carries the same window, split by component |
| `context_window.context_window_size` | max window in tokens (200000 default; 1000000 for extended-context models) |
| `context_window.used_percentage`, `remaining_percentage` | pre-calculated context fill (from input-side tokens only) |
| `context_window.current_usage.{input_tokens,output_tokens,cache_creation_input_tokens,cache_read_input_tokens}` | per-component token counts from the last API call |
| `exceeds_200k_tokens` | whether the most recent response's combined tokens exceed 200k (fixed threshold) |
| `effort.level` | reasoning effort (`low`\|`medium`\|`high`\|`xhigh`\|`max`); reflects live value including mid-session `/effort` changes; Ultracode is not a distinct level and reports as `xhigh`; absent when unsupported |
| `thinking.enabled` | whether extended thinking is on |
| `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}` | 5h/7d window fill (0–100) and reset (Unix epoch seconds) |
| `session_id`, `session_name` | session id; custom name from `--name` / `/rename`, otherwise the AI-generated session title when one exists |
| `prompt_id` | current user-prompt UUID, shared with hook and OpenTelemetry correlation; absent until first input (v2.1.196+) |
| `transcript_path` | conversation transcript path |
| `version` | Claude Code version |
| `output_style.name` | current output style |
| `vim.mode` | `NORMAL`\|`INSERT`\|`VISUAL`\|`VISUAL LINE` when vim mode is on |
| `agent.name` | agent name under `--agent` |
| `pr.{number,url,review_state}` | open PR for the branch; `review_state` ∈ `approved`\|`pending`\|`changes_requested`\|`draft` |
| `pr.kind` | `"mr"` for a GitLab merge request (conventionally displayed as `!N`); absent for GitHub pull requests |
| `fast_mode_state` | fast-mode availability: `on`\|`cooldown`\|`off` |
| `fast_mode_disabled_reason` | optional reason fast mode is not currently available |
| `worktree.{name,path,branch,original_cwd,original_branch}` | active `--worktree` session details |

**Absence vs null.** `session_name`, `prompt_id`, `workspace.git_worktree`, `workspace.repo`, `effort`, `vim`, `agent`, `pr`, `worktree`, `costBasis`, and the fast-mode fields are *absent* until their data exists; `rate_limits` appears only for Claude.ai Pro/Max after the first API response, and each window may be absent independently. `context_window.current_usage` is `null` before the first API call and again after `/compact` until the next call; `used_percentage` / `remaining_percentage` may be `null` early in a session. RimZ's parser treats every field as optional and tolerates unknown keys.

**`subagentStatusLine`.** A separate command (`"subagentStatusLine": { "type": "command", "command": "…" }`) renders each subagent row in the agent panel, replacing the default `name · description · token count` body with whatever the script prints. The command runs once per refresh tick with **all visible subagent rows as a single JSON object on stdin**. The input includes the [common hook fields](#common-input) plus `columns` (usable row width) and a `tasks` array, each task carrying `id`, `name`, `type`, `status`, `description`, `label`, `model`, `effort`, `startTime`, `tokenCount`, `tokenSamples`, and `cwd`. Write one JSON line to stdout per row to override: `{"id": "<task id>", "content": "<row body>"}`. The `content` string is rendered as-is, including ANSI escape codes and OSC 8 hyperlinks. Omit a task's `id` to keep its default rendering; emit an empty `content` to hide the row. The same trust and `disableAllHooks` gates that apply to `statusLine` apply here. Plugins can ship a default `subagentStatusLine` in their `settings.json`.

RimZ wraps this command like the session `statusLine` and harvests each task's `model`, `effort`, `description`, `tokenCount`, and `startTime` (keyed by `id`, the child `agent_id`) into a per-subagent sidecar the sidebar folds onto the child's row. The common `transcript_path` names the parent transcript; RimZ derives the child's sibling `subagents/agent-<id>.jsonl` and incrementally prices its per-request usage for the exact display-only child cost. It overrides no rows, so Claude's own panel renders unchanged. The harvest path is [`subagent_statusline.rs`](../../../crates/rimz/src/agents/adapters/claude/subagent_statusline.rs); the sidebar projection is in [sidebar.md](../../internals/sidebar/sidebar.md).

## Agent view

A bare `claude` launch opens the normal interactive session. `claude agents` opens agent view, and `claude --bg`, `/background`, or the left-arrow detach path moves a session under the per-user background supervisor. Agent view requires Claude Code 2.1.139+.

The `disableAgentView` setting and `CLAUDE_CODE_DISABLE_AGENT_VIEW=1` turn off that background-session surface. They do not select between agent view and the interactive REPL, and Remote Control server mode is independent of agent view. RimZ therefore leaves this upstream policy untouched on ordinary Claude pane launches.

## Remote control

Claude Code's remote-control host is `claude remote-control --spawn worktree`. RimZ launches that command directly in the `rimzd` view when `[remote_control] claude = true`, from the project root so each on-demand session is cut from the canonical repo.

In Claude Code 2.1.209, an attached remote session forks an SDK child shaped as `<claude-version-bin> --print --sdk-url … --session-id cse_…`. The child and its hook helpers inherit `CLAUDE_CODE_ENVIRONMENT_KIND=bridge` and a non-empty `CLAUDE_CODE_SESSION_ACCESS_TOKEN`; the latter is session-ingress authentication and must remain private. The SDK child also inherits globally installed Claude hooks even though the long-lived host itself is infrastructure.

Version gates RimZ enforces: remote control exists at Claude Code ≥ 2.1.51; Claude Code 2.1.128+ recognizes `disableRemoteControl: true`; API-key auth disables remote control at ≥ 2.1.157 when `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `apiKeyHelper`, or matching keys in settings `env` are active; long-lived setup tokens supplied through `CLAUDE_CODE_OAUTH_TOKEN` or settings `env` are blocked at the same gate because they can make model requests but cannot establish Remote Control; Claude Code ≥ 2.1.196 also rejects `ANTHROPIC_BASE_URL` values other than `https://api.anthropic.com` and the Bedrock, Vertex, and Foundry provider modes. Remote Control requires a full-scope session from `claude auth login`, and API keys are unsupported. An unknown `claude --version` applies only the version-independent `disableRemoteControl` gate and warns rather than guessing.

`remoteControlAtStartup: true` auto-enables remote control for ordinary Claude pane sessions; `false` disables auto-connect and an absent value follows the organization's default. RimZ reads an explicit `true` to light the provider dashboard's `⇅ rc` flag even when the RimZ daemon-host toggle is off; `disableRemoteControl: true` suppresses the auto flag. `$CLAUDE_CODE_REMOTE` marks remote web sessions, not local host readiness.

RimZ's remote-control preflight and badge read the user-level Claude `settings.json`, or the file named by `RIMZ_CLAUDE_SETTINGS` in tests and controlled environments. Claude Code also folds managed, local, and project settings; RimZ currently treats those tiers as upstream runtime policy and leaves their merge to Claude Code.

## Project storage and managed pricing

`CLAUDE_CODE_PROJECT_DIR_NAME` replaces the default flattened absolute-workspace name for the active `projects/<bucket>` transcript directory. It is intended for hosts that give each session its own Claude config directory and need a short per-project bucket name.

Claude Code 2.1.243 added `modelPricing` to machine managed settings. Managed settings live in `/etc/claude-code/managed-settings.json` on Linux/WSL, `/Library/Application Support/ClaudeCode/managed-settings.json` on macOS, and `C:\Program Files\ClaudeCode\managed-settings.json` on Windows; JSON fragments in the adjacent `managed-settings.d/` directory merge alphabetically. The recovered schema is:

```jsonc
{
  "modelPricing": {
    "multiplier": 0.8, // optional, > 0 and <= 1
    "overrides": {
      "claude-sonnet-4-6": {
        "input": 3.0,
        "output": 15.0,
        "cacheRead": 0.3,
        "cacheWrite": 3.75
      }
    }
  }
}
```

The four override rates are required USD-per-million-token values in `0..=10000`; `cacheWrite` prices both cache-write durations. A matching row is charged exactly as written before the optional multiplier, without fast-mode or long-context surcharges. Keys use the model ids Claude Code itself prices, including first-party and Bedrock forms. These values affect Claude's `/cost`, statusline, SDK `total_cost_usd`, `--max-budget-usd`, and OpenTelemetry cost estimates; they are estimates rather than invoices, and `/model` continues to show list-price labels.

## Auth surface

[`claude/account.rs`](../../../crates/rimz/src/agents/adapters/claude/account.rs) forks `claude auth status` (JSON) for the logged-in-but-idle probe. Fields it reads:

| Field | Meaning |
| --- | --- |
| `loggedIn` | whether a login is present |
| `authMethod` | login type; `apiKey` is unmetered, anything else metered |
| `subscriptionType` | plan tier (`max`, `pro`, …) → the account `plan` label |

[`oauth_usage.rs`](../../../crates/rimz/src/agents/adapters/claude/oauth_usage.rs) reads `~/.claude/.credentials.json` (macOS may hold the same JSON in the `Claude Code-credentials` Keychain item) and uses the root `claudeAiOauth` object:

| Field | Meaning |
| --- | --- |
| `accessToken` | Bearer token for the usage request |
| `refreshToken` | Preferred input to RimZ's non-secret account-owner digest; never sent, persisted, or refreshed by RimZ |
| `expiresAt` | epoch milliseconds; missing or expired tokens fail the probe |
| `scopes[]` | must include `user:profile` |

RimZ hashes the normalized `refreshToken`, falling back to `accessToken`, with a versioned Claude-specific SHA-256 domain to obtain a full lowercase account-owner key. Access-token rotation therefore keeps one cache owner when the refresh token stays stable; only the digest reaches `credits.json`, and neither source token is logged or persisted by RimZ.

On macOS, a missing credentials file falls back to `/usr/bin/security find-generic-password -s "Claude Code-credentials" -w` with null stdin and a 1.5-second subprocess deadline. Timeout and denied/nonzero results are quiet missing credentials; the bound prevents RimZ from waiting indefinitely, while macOS may still briefly present Keychain UI before the process exits.

The helper calls `GET https://api.anthropic.com/api/oauth/usage` with `Authorization: Bearer <accessToken>`, `Accept: application/json`, `anthropic-beta: oauth-2025-04-20`, and a `claude-code/<claude-version>` user agent when the version is known. `RIMZ_CLAUDE_OAUTH_USAGE_URL` overrides the URL for integration tests, and RimZ honors an override only for the official host or a loopback address. The path is read-only: RimZ does not refresh tokens or write the file or Keychain item. The parsed response shape:

```jsonc
{
  "five_hour": {
    "utilization": 12.5,                 // mapped
    "resets_at": "2026-09-21T14:13:20Z", // mapped
    "limit_dollars": null,               // present, ignored
    "used_dollars": null,                // present, ignored
    "remaining_dollars": null            // present, ignored
  },
  "seven_day": {
    "utilization": 7,                    // mapped
    "resets_at": "2026-09-27T09:06:40Z", // mapped
    "limit_dollars": null,               // present, ignored
    "used_dollars": null,                // present, ignored
    "remaining_dollars": null            // present, ignored
  },
  "extra_usage": {
    "is_enabled": true,        // mapped
    "used_credits": 725,       // cents, mapped
    "monthly_limit": 5000,     // cents, mapped
    "utilization": 14.5,       // present, ignored
    "currency": "USD",         // present, ignored
    "decimal_places": 2,       // present, ignored
    "disabled_reason": null,   // present, ignored
    "daily": null,             // present, ignored
    "weekly": null             // present, ignored
  },
  "limits": [],                       // present, ignored
  "spend": {},                        // present, ignored
  "member_dashboard_available": false // present, ignored
}
```

`five_hour` and `seven_day` map to 300- and 10080-minute `RateLimitWindow`s. `utilization` is a 0–100 percentage and RimZ rounds/clamps it the same way as statusline `used_percentage`; `1.0` means 1%, not a fully spent window. `extra_usage.is_enabled = false` maps to `ExtraCredits::Disabled`; otherwise `used_credits` and `monthly_limit` are cents converted to USD. The semantics (`metered` inference, plan→brand label, cache cadence) are in [adapter_claude.md → Account and balance](../../internals/agents/adapter_claude.md#account-and-balance).

## Transcript JSONL

Anthropic publishes **no official schema** for the conversation transcript at `transcript_path`. RimZ reads it best-effort and reverse-engineered: each assistant line carries a `message` object, and the newest `message.usage` (`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`) plus `message.model` feed the context gauge. Newer usage objects may also carry `iterations`; an iteration whose `type` is `advisor_message` names its own `model` and usage buckets and represents a separately billed nested request. The field → internal mapping and the window-divisor rule are in [adapter_claude.md → Context and transcript](../../internals/agents/adapter_claude.md#context-and-transcript); there is no source URL to pin.

### Transcript death certificate

A turn Claude aborts on a provider API error fires `StopFailure`, whose payload carries `error`, `error_details`, and `last_assistant_message` alongside the common hook fields. RimZ maps `error: "rate_limit"` to a rate-limit paused marker, `error: "overloaded"` to the backoff paused marker, and every other error through the capped assistant-message classifier so transient server labels park while terminal labels fail. The event writes only `AgentContext.turn_error`: no lifecycle envelope is appended, so the rollup stays `running` and display projection owns the pause/failure.

Older Claude sessions, or sessions whose hooks were installed after the failure, still leave a transcript death certificate. The transcript records the death twice, milliseconds apart:

```jsonc
{"type": "assistant", "isApiErrorMessage": true, "timestamp": "2026-06-04T02:56:32.919Z", "message": {"content": [{"type": "text", "text": "API Error: Overloaded"}]}}
{"type": "system", "subtype": "turn_duration", "timestamp": "2026-06-04T02:56:32.923Z"}
```

[`detect_turn_error`](../../../crates/rimz/src/agents/adapters/claude/statusline.rs) reads the flagged assistant entry off the bounded tail on each statusline push as the backstop. It classifies labels containing "spend limit" as spend-limit paused, labels containing "usage limit", "session limit", "rate limit", "quota", or "too many requests" as rate-limit paused, transient server/transport labels ("overloaded", "server is busy", "server error", "internal server error", "service unavailable", "bad gateway", "gateway timeout", "no response from api", "stalled", "timed out", "timeout", "connection error", "connection closed", "connection reset", "connection lost", "socket hang up", "broken pipe", "econnreset", "mid-response", "mid-stream", or "network error") as the backoff paused class, and other API-error labels as failed; the decision rule and the internal mapping are [adapter_claude.md → Turn-death marker](../../internals/agents/adapter_claude.md#turn-death-marker). Reverse-engineered like the rest of this section; no source URL to pin.

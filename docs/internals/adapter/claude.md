# Claude Code protocol reference

> The mapping onto Rimz's internal types lives beside this doc: [hooks.md](../hooks.md) maps hook events to lifecycle/feed channels, [transcript.md](../transcript.md) maps the statusline and transcript onto `AgentContext`, [account.md](../account.md) maps the auth surface onto account and balance.

This is the single home for the **Claude Code upstream protocol surface** Rimz binds to — the hook events, their stdin payloads and stdout decision schema, the statusline JSON, and the auth surface. It is a hand-maintained mirror of Anthropic's published docs, kept for fast lookup and pinned to the source URLs below so it can be refreshed when upstream moves. The [`ClaudeIntegration`](../../../crates/rimz/src/agents/claude.rs) adapter is the only code that reads this surface; everything downstream of it speaks Rimz's internal types.

Coverage is **depth on what Rimz wires, breadth as an index**: the events, statusline fields, and decision shapes the adapter actually parses or emits are documented in full; the rest of the upstream catalog is listed so a contributor wiring a new event knows it exists.

## Upstream sources

Re-fetch these pages to refresh this mirror. `docs.claude.com/en/docs/claude-code/*` 301-redirects to `code.claude.com/docs/en/*` — the `code.claude.com` form is canonical.

| Surface | Source |
| --- | --- |
| Hooks reference (events, payloads, decision schema, exit codes) | <https://code.claude.com/docs/en/hooks> |
| Statusline (full JSON schema, `subagentStatusLine`) | <https://code.claude.com/docs/en/statusline> |
| Subagents | <https://code.claude.com/docs/en/sub-agents> |
| Settings (`statusLine` / `hooks` config keys) | <https://code.claude.com/docs/en/settings> |
| Transcript JSONL | no official schema published — see [Transcript JSONL](#transcript-jsonl) below |

## Hooks

A hook is a command Claude Code runs at a lifecycle point. Claude writes a JSON payload to the hook's **stdin** and reads the hook's **stdout** as a decision. Each event is wired in `settings.json` under `hooks.<EventName>[]`, optionally gated by a `matcher` (a tool-name or source pattern).

### Common input

Every hook receives these fields on stdin (some are event- or context-gated):

```json
{
  "session_id": "string — current session identifier",
  "transcript_path": "string — path to the conversation JSONL",
  "cwd": "string — working directory when the hook is invoked",
  "permission_mode": "default | plan | acceptEdits | auto | dontAsk | bypassPermissions",
  "effort": { "level": "low | medium | high | xhigh | max" },
  "hook_event_name": "string — the event that fired",
  "agent_id": "string — subagent id, present only inside a subagent",
  "agent_type": "string — agent name, present under --agent or inside a subagent"
}
```

`permission_mode` and `effort` are not present on every event; `effort` rides events with a tool-use context (`PreToolUse`, `PostToolUse`, `Stop`, `SubagentStop`) when the model supports the parameter. `agent_id` / `agent_type` appear only with `--agent` or inside a subagent.

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
| `PreToolUse` | `hookSpecificOutput` | `permissionDecision` (`allow`\|`deny`\|`ask`\|`defer`), `permissionDecisionReason`, `modifiedInput`, `additionalContext` |
| `PermissionRequest` | `hookSpecificOutput` | `decision.behavior` (`allow`\|`deny`), `decision.updatedInput`, `decision.appliedRule` |
| `PermissionDenied` | `hookSpecificOutput` | `retry` (boolean) |
| `UserPromptSubmit`, `PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `Stop`, `SubagentStop`, `ConfigChange`, `PreCompact` | top-level `decision` | `decision: "block"`, `reason`, optional `hookSpecificOutput.additionalContext` |
| `SessionStart`, `Setup`, `SubagentStart` | context-only | `hookSpecificOutput.additionalContext`, `sessionTitle` (SessionStart) |

### Exit codes

- **0** — success; stdout is parsed as the JSON above. For `UserPromptSubmit`, `UserPromptExpansion`, and `SessionStart`, plain stdout is injected as context Claude can read; for other events it goes to the debug log.
- **2** — blocking error; stdout and any JSON are ignored, stderr is fed back to Claude (e.g. `PreToolUse` blocks the call, `UserPromptSubmit` rejects the prompt, `Stop` prevents stopping).
- **other** — non-blocking error; a `<hook> hook error` notice plus the first stderr line surfaces and execution continues.

### Hooks Rimz wires

These are the events the [`ClaudeIntegration`](../../../crates/rimz/src/agents/claude.rs) `INSTALLED_EVENTS` constant installs. The native-event → Rimz status mapping is the [hooks.md Claude appendix](../hooks.md#appendix--claude-code); the columns here are the upstream fire-time and the event-specific stdin fields the adapter reads.

| Event | Fires | Event-specific input | Rimz channel |
| --- | --- | --- | --- |
| `SessionStart` | session begins or resumes | `source` (`startup`\|`resume`\|`clear`\|`compact`), `model`, `session_title` | lifecycle |
| `UserPromptSubmit` | prompt submitted, before processing | `prompt` | lifecycle |
| `PreToolUse` | before a tool call (can block) | `tool_name`, `tool_input` | lifecycle, or blocking when `tool_name` is `ExitPlanMode` / `AskUserQuestion` |
| `PostToolUse` | after a tool call succeeds | `tool_name`, `tool_input`, `tool_response` | lifecycle (silent; audit/enrichment) |
| `Stop` | Claude finishes responding | `stop_hook_active`; `background_tasks[]` of `{status, description, command, id}` (v2.1.145+) | lifecycle |
| `SubagentStart` | a subagent is spawned | `agent_type`, `agent_id` | lifecycle |
| `SubagentStop` | a subagent finishes | `agent_type`, `agent_id`, `exit_code` | lifecycle |
| `PreCompact` | before context compaction | `trigger` (`manual`\|`auto`) | lifecycle (sets `compacting`) |
| `SessionEnd` | session terminates | `reason` | lifecycle (`ends_session`) |
| `Notification` | Claude Code sends a notification | `message` | lifecycle (silent) |
| `PermissionRequest` | a permission dialog appears | `tool_name`, `tool_input`, `permission_mode` | blocking-feed (sync) |

`ExitPlanMode` and `AskUserQuestion` have no dedicated install entry — they self-classify off `tool_name` on the broad `PreToolUse` hook.

**Decision shapes Rimz renders.** A `PermissionRequest` answer:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow" } } }
```

A plan approval or user question answers on the `PreToolUse` event and **requires** `updatedInput` (a missing field is a hard render error):

```json
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": {} } }
```

The neutral path (no resolver answered) is empty stdout, exit 0. Exact bytes are the inline goldens in [`claude.rs`](../../../crates/rimz/src/agents/claude.rs).

### Full event catalog (index)

The complete upstream set. ✓ marks what Rimz wires today; the rest is available for future wiring.

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
| `StopFailure` | the turn ends on an API error | |
| `TeammateIdle` | an agent-team teammate is about to idle | |
| `InstructionsLoaded` | a `CLAUDE.md` / rules file is loaded | |
| `ConfigChange` | a config file changes mid-session | |
| `CwdChanged` | the working directory changes | |
| `FileChanged` | a watched file changes on disk | |
| `WorktreeCreate` | a worktree is being created | |
| `WorktreeRemove` | a worktree is being removed | |
| `PreCompact` | before context compaction | ✓ |
| `PostCompact` | after compaction completes | |
| `Elicitation` | an MCP server requests user input | |
| `ElicitationResult` | after a user responds to an elicitation | |
| `SessionEnd` | session terminates | ✓ |

## Statusline JSON

Claude `exec`s the configured `statusLine` command on every render and pipes this JSON to its stdin. Rimz wraps that command with `rimz statusline feed --source claude`; [`StatuslinePayload`](../../../crates/rimz/src/agents/statusline.rs) parses the blob and the wrap forwards it unchanged to any prior command. The statusline runs locally and consumes no API tokens.

**Update triggers.** The command runs after each new assistant message, after `/compact`, on a permission-mode change, and on a vim-mode toggle (debounced 300ms; an in-flight run is cancelled when a new update arrives). `refreshInterval` (seconds, min 1) adds a fixed timer for idle/time-based segments.

**Full schema** (the verbatim upstream example):

```json
{
  "cwd": "/current/working/directory",
  "session_id": "abc123...",
  "session_name": "my-session",
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
| `context_window.total_input_tokens`, `total_output_tokens` | tokens in the current context window (current, not cumulative, since v2.1.132) |
| `context_window.context_window_size` | max window in tokens (200000 default; 1000000 for extended-context models) |
| `context_window.used_percentage`, `remaining_percentage` | pre-calculated context fill (from input-side tokens only) |
| `context_window.current_usage.{input_tokens,output_tokens,cache_creation_input_tokens,cache_read_input_tokens}` | per-component token counts from the last API call |
| `exceeds_200k_tokens` | whether the most recent response's combined tokens exceed 200k (fixed threshold) |
| `effort.level` | reasoning effort (`low`\|`medium`\|`high`\|`xhigh`\|`max`); absent when unsupported |
| `thinking.enabled` | whether extended thinking is on |
| `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}` | 5h/7d window fill (0–100) and reset (Unix epoch seconds) |
| `session_id`, `session_name` | session id; custom name from `--name` / `/rename` (absent if unset) |
| `transcript_path` | conversation transcript path |
| `version` | Claude Code version |
| `output_style.name` | current output style |
| `vim.mode` | `NORMAL`\|`INSERT`\|`VISUAL`\|`VISUAL LINE` when vim mode is on |
| `agent.name` | agent name under `--agent` |
| `pr.{number,url,review_state}` | open PR for the branch; `review_state` ∈ `approved`\|`pending`\|`changes_requested`\|`draft` |
| `worktree.{name,path,branch,original_cwd,original_branch}` | active `--worktree` session details |

**Absence vs null.** `session_name`, `workspace.git_worktree`, `workspace.repo`, `effort`, `vim`, `agent`, `pr`, `worktree` are *absent* unless their feature is active; `rate_limits` appears only for Claude.ai Pro/Max after the first API response, and each window may be absent independently. `context_window.current_usage` is `null` before the first API call and again after `/compact` until the next call; `used_percentage` / `remaining_percentage` may be `null` early in a session. Rimz's parser treats every field as optional and tolerates unknown keys.

**`subagentStatusLine`.** A separate command renders each subagent row in the agent panel. It receives the [common hook fields](#common-input) plus `columns` (usable row width) and a `tasks` array, each task carrying `id`, `name`, `type`, `status`, `description`, `label`, `startTime`, `tokenCount`, `tokenSamples`, and `cwd`. The command writes one `{"id": "<task id>", "content": "<row body>"}` line per row to override.

## Auth surface

[`account.rs`](../../../crates/rimz/src/agents/account.rs) forks `claude auth status` (JSON) for the logged-in-but-idle probe. Fields it reads:

| Field | Meaning |
| --- | --- |
| `logged_in` | whether a login is present |
| `auth_method` | login type; `apiKey` is unmetered, anything else metered |
| `subscription_type` | plan tier (`max`, `pro`, …) → the account `plan` label |

The 5h/7d balance windows have no source outside a live statusline — there is no idle balance probe for Claude. The semantics (`metered` inference, plan→brand label) are in [account.md](../account.md).

## Transcript JSONL

Anthropic publishes **no official schema** for the conversation transcript at `transcript_path`. Rimz reads it best-effort and reverse-engineered: each assistant line carries a `message` object, and the newest `message.usage` (`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`) plus `message.model` feed the context gauge. The field → internal mapping and the window-divisor rule are in [transcript.md](../transcript.md#appendix--claude-code); there is no source URL to pin.

# Claude Code adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The raw upstream protocol is in [claude-reference.md](../../externals/agent-adapter/claude-reference.md).

This page maps Claude Code onto RimZ's internal types: which hook carries which signal, how a Claude session launches, how Task-tool children become rows, where context, account, and spend figures come from, and how RimZ hosts `claude remote-control`. The code lives in [`agents/adapters/claude/`](../../../crates/rimz/src/agents/adapters/claude/mod.rs), and `ClaudeAdapter` in `mod.rs` implements every capability trait.

| Module | Owns |
| --- | --- |
| `mod.rs` | the `AgentSpec`, coverage declarations, the `CLAUDE_HOOKS` catalog, `decode_hook`, the launch settings merges, and transcript usage reads |
| `payloads.rs` | typed hook payloads |
| `ask.rs` | ask questions, answers, and `rimz answer` key plans |
| `install.rs` | the `settings.json` hook and statusline integration |
| `headless.rs` | the headless check form |
| `statusline.rs` | statusline parsing, the conversation parser, and the turn-death and turn-interruption scans |
| `subagent_statusline.rs`, `subagent_cost.rs`, `subagents.rs` | child statusline enrichment, child cost cursors, and interrupted-child repair |
| `local_context.rs` | the session-cumulative token fold |
| `local_sessions.rs` | local session discovery |
| `account.rs`, `oauth_usage.rs` | `claude auth status`, window normalization, the OAuth usage probe, and the account key |
| `spend.rs`, `managed_pricing.rs` | the full-history spend parser and the managed-settings price overlay |
| `remote_control.rs`, `remote_consent.rs`, `remote_liveness.rs` | remote-control readiness, consent, and host liveness |
| `folder_trust.rs`, `json_edit.rs` | folder-trust decisions, grant previews, and byte-preserving JSON leaf edits |

## Hooks and lifecycle

Every installed hook runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude`, and the event name comes from the payload's `hook_event_name`. The table lists what `decode_hook` makes of each event. The state machine the signals drive is in [model.md](./model.md#the-state-machine), and the upstream payloads are in [claude-reference.md](../../externals/agent-adapter/claude-reference.md#hooks-rimz-wires).

| Native event | Channel | [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs) | Notes |
| --- | --- | --- | --- |
| `SessionStart` | lifecycle | `Registered`, or `CompactionEnded { auto: None }` for source `compact` | stamps lineage ([conversation replacement](#conversation-replacement)) and the [birth account key](#birth-account-key); model from the payload |
| `UserPromptSubmit` | lifecycle | `TurnStarted` | sanitized text as `task` and `prompt` (below); a `<task-notification>` prompt drops the [background shells](#background-shells) it reports finished; a root prompt's reply can carry the [Environment block](../harness/fleet.md#environment-at-prompt-submit) as `hookSpecificOutput.additionalContext` |
| `Stop` | lifecycle | `TurnEnded { errored, parked_on_background }` | see [turn endings](#turn-endings); `background_tasks` replaces the [background shells](#background-shells) |
| `StopFailure` | lifecycle | none | writes `AgentContext.turn_error` ([turn endings](#turn-endings)) |
| `SessionEnd` | lifecycle | `Ended` | stamps `ended_at`; the runtime hides the retained resumable row |
| `Notification` | lifecycle | none | |
| `PreToolUse` | lifecycle | `ToolUsed { mutates: false, edits: false, name: None }` | proof of work only, keyed by `tool_use_id` |
| `PostToolUse` | lifecycle | `ToolUsed { mutates, edits, name }` | every tool, named, keyed by `tool_use_id`; answered asks record their answers; a backgrounded `Bash` starts a [background shell](#background-shells) |
| `PreCompact` | lifecycle | `Compacting` | opens the [compaction bracket](./model.md#the-compaction-bracket) |
| `PostCompact` | lifecycle | `CompactionEnded { auto }` | carries the manual or auto trigger |
| `SubagentStart` | lifecycle | `SubagentStarted` | see [Subagents](#subagents) |
| `SubagentStop` | lifecycle | `SubagentStopped { errored: false }` | see [Subagents](#subagents) |
| `PermissionRequest` | awaiting-user | `AwaitingInput { Permission }` | synchronous, keyless |
| `PreToolUse` for `ExitPlanMode` | awaiting-user | `AwaitingInput { PlanApproval }` | keyed by `tool_use_id` |
| `PreToolUse` for `AskUserQuestion` | awaiting-user | `AwaitingInput { Question }` | structured questions from `tool_input`, keyed by `tool_use_id` |

Every root hook also reads usage from the transcript ([Context and transcript](#context-and-transcript)). `final_message_for_lifecycle` attaches the final assistant message to a root `Stop` or ask and to a `SessionEnd`: the payload's `last_assistant_message`, else the newest assistant entry in the transcript tail. Effort comes from `Stop` and `SubagentStop` (`effort.level`), else the payload's `thinking_level`.

Prompt sanitizing removes balanced `<pasted_content id="X">` and `</pasted_content id="X">` tag lines (observed in Claude Code 2.1.278) and keeps the pasted content, even inside a longer human prompt. An unmatched tag, or a tag that shares its line with other text, stays.

Cursor can run Claude-compatible hook commands with Cursor-shaped payloads, so a payload that carries `cursor_version` is classified `Unknown` and never opens a catalog ask. Only the classification is quarantined: `decode_hook` still attaches the lifecycle observation, so a Cursor payload that resolves an identity can still record lifecycle state (`claude/tests/lifecycle.rs` pins both halves).

### Ask classification

The two ask tools ride the broad `PreToolUse` hook and classify from `tool_name` through the spec's `ToolClassification.blocking` list. The catalog installs no matcher for them, because Claude runs every matching matcher group and a dedicated one would fire twice. Claude 2.1.205 in manual mode also sends `PermissionRequest` for those two tools. `decode_hook` recognizes the tool name and returns no signal for that duplicate, so it cannot replace the structured ask the `PreToolUse` recorded.

### Native keys

Every signal derived from a `PreToolUse` or a `PostToolUse` carries that payload's `tool_use_id` as its [native key](./model.md#waiting-and-asks). Claude stamps one id per tool call, identical on the call's two edges and distinct across parallel calls (observed on Claude Code 2.1.278). A question the user has not answered therefore survives the tool calls Claude runs in parallel with the picker, and clears on its own `PostToolUse`. A build that omits the field degrades to the keyless behaviour, where any tool edge clears the wait.

`PermissionRequest` has no `tool_use_id` on the wire and stays keyless. Its clearing edge is the approved tool's own keyed `PostToolUse`. That edge still clears it, because the sibling rule holds only a wait whose open ask is keyed.

### Tool events

The spec's `ToolClassification` decides both bits: `mutates` for `Edit`, `Write`, `MultiEdit`, `NotebookEdit`, and `Bash`, and `edits` for the same list without `Bash`. `edits` moves the turn's [phase](./model.md#turn-phase) from reasoning to acting.

A broad `PreToolUse` is unnamed proof of work. Store appends it only under the rule in [model.md](./model.md#the-state-machine): when a subagent owns it, or when it clears a wait, closes a compaction bracket, or reconciles a resting row. Every `PostToolUse` carries its tool name, so Store always appends it, read-only tools and answered asks included, and the named completions feed the live session tool map.

### Turn endings

`Stop` carries two bits. `errored` comes from the shared `stop_payload_errored`. `parked_on_background` is set when the payload's `background_tasks` holds a task whose status is neither `completed` nor `failed`, or when `session_crons` is non-empty (both fields exist from Claude Code 2.1.145). [model.md](./model.md#turn-endings-and-parked-turns) turns those bits into the final status: a clean parked stop stays `running` with a `⋯ bg` marker, and an error always wins.

`StopFailure` is Claude's provider-error certificate. It appends no `agent.lifecycle` envelope, so the rollup stays `running`. It writes a turn-error marker instead, classified from `error` and the capped `last_assistant_message` label by the same `classify_api_error` the [turn-death marker](#turn-death-marker) uses. On `Stop`, when no `StopFailure` certificate applies, `decode_hook` runs the turn-death scan over the transcript tail. That scan covers Claude builds without `StopFailure` and sessions whose hooks were installed late.

### Background shells

A root session's hooks report its background shells as a `BackgroundShellReport` on the observation, and the rollup folds them into `AgentState.background_shells` ([model.md](./model.md#turn-endings-and-parked-turns)):

- A `PostToolUse` for `Bash` whose `tool_response` carries `backgroundTaskId` starts a shell with the `tool_input` command and description. Claude fires it at launch, both for `run_in_background` and for a foreground command auto-backgrounded past its timeout.
- A `Stop` with a `background_tasks` array replaces the list with its pending entries of `type` `shell`. An entry without `type` counts when it carries `command`. A `Stop` without the array (Claude Code before 2.1.145) leaves the list alone.
- A `UserPromptSubmit` whose prompt is one or more `<task-notification>` blocks drops each `<task-id>` whose `<status>` is not `running`.

Subagent hooks report nothing: they target the child row, and the parent's `Stop` lists every shell. `SubagentStop.background_tasks` is parent-scoped and ignored.

### Conversation replacement

A root `SessionStart` stamps [`SessionOrigin`](../../../crates/rimz/src/agents/observation.rs): `Fresh` for sources `startup` and `clear`, `Forked` for `fork`, and nothing for `resume` and unknown sources. `/clear` gives the same pane and process a new fresh session id, and the fresh-conversation rule in [instances.md](./instances.md#supersession) reaps the older row once its turn is at rest. A turn that `StopFailure` killed counts as at rest: the writer reads the certificate from the context sidecar and ends the row as `ReapedSuperseded`.

### Asks and answers

Claude's neutral reply is empty stdout, so every permission, plan, and question prompt stays in Claude's own UI and the waiting row routes you there. `rimz answer` drives the native prompt:

| Ask | Options | Keys sent |
| --- | --- | --- |
| `AskUserQuestion` | the question's own options: single pick, multi-select, and free text | navigation through the native picker |
| Permission | `allow` only | digit `1`, the stable first menu action |
| Plan approval | `approve` only, marked with a caution | Shift-Tab, which also turns on accept-edits mode |

Deny, keep-planning, refinement text, persistent grants, and manual-review approval without a new prompt are left to the pane.

Rejecting any of the three with Escape fires neither `PostToolUse` nor `Stop` (Claude Code 2.1.205). The only evidence is the transcript's `[Request interrupted by user` marker, which the next statusline push stamps as an interruption settle. The settle releases the ask, so the row leaves `rimz asks` and stops reserving pane input ([model.md](./model.md#waiting-and-asks)). The durable question record still closes on the next prompt: its `UserPromptSubmit` clears the wait and records that prompt as the answer.

Rejecting in the pane with typed feedback ends the same ask another way and fires no hook at all. Claude hands the feedback back to the model as that call's own `tool_result` and the turn runs on, so there is neither a `PostToolUse` (success only) nor an interruption marker. The proof arrives with the next keyed tool event. While a keyed ask is open and a different call reports, `tool_call_resolved` reads the transcript tail for a `tool_result` under the open ask's `tool_use_id`. When it finds one, the hook path records that ask's answer edge before the incoming event, and the row goes back to running ([model.md](./model.md#waiting-and-asks)). A rejection the model answers with text, and no further tool call, holds the ask until `Stop`.

### Install

Install merges into Claude's `settings.json` through the shared JSON-merge backend ([adapter.md](./adapter.md#hook-install)). The file resolves from the login env: `RIMZ_CLAUDE_SETTINGS` when set, then `settings.json` under the first entry of a comma-separated `CLAUDE_CONFIG_DIR`, then `$HOME/.claude/settings.json`. A named account's hooks therefore land in that account's home.

Install writes one block per event in `CLAUDE_HOOKS`, each marked `_rimz_managed`, with a 2-second timeout. Hooks append ingress and nudge the elected drainer, and reply-bearing hooks wait within that budget. Only `PermissionRequest` carries `_rimz_sync = true`, and an existing async marker on it is a hard install error. Install also wraps both statusline commands ([rich context](#rich-context) and [child enrichment](#child-statusline-and-cost)).

## Launch

The spec's `LaunchSpec` and `LaunchCapability` render every Claude launch:

| Launch concern | Claude argv or behaviour |
| --- | --- |
| initial prompt | positional, after `--` |
| resume | `claude --resume <session_id>` |
| fork | `claude --resume <session_id> --fork-session` |
| bare RimZ `--resume` | the native `claude --resume` picker |
| permission modes | ask `--permission-mode default`, auto `--permission-mode auto`, yolo `--dangerously-skip-permissions`, plan `--permission-mode plan` |
| turn limit | `--max-turns` |
| model and effort presets | `--model`, `--effort` |
| `auto-compact` preset | `--autocompact <tokens>`, a session-only window of 100k to 1M tokens ([upstream](../../externals/agent-adapter/claude-reference.md#auto-compaction-window)) |
| `system-prompt-file` preset | `--system-prompt-file`: the resolved absolute base path, or a content-addressed composed file when fragments exist |
| launch reminders | `--append-system-prompt`, internal only ([system prompt](#system-prompt)) |
| fast preset | `fastMode: true`, merged into the launch's one `--settings` ([settings merge](#settings-merge)) |
| compaction command | `/compact <brief>` |
| `rimz subagents` child | `--disallowedTools` gains `Agent`, merged with any tools the profile already denies, so a child cannot spawn Task-tool children |
| shared language servers configured | `--disallowedTools` gains `LSP` ([native LSP denial](#native-lsp-denial)) |
| skills home | `<config home>/skills` |
| temp root | `CLAUDE_CODE_TMPDIR` set to the launch's `TMPDIR` (`temp_dir_env_keys`), so Claude's internal temp files land in the agent's temp unit on macOS too, where Claude ignores `TMPDIR` ([upstream](../../externals/agent-adapter/claude-reference.md#temp-root)). Under scoped isolation the value is `/tmp`. |

`claude --resume <session_id>` continues the conversation under the same session id, and only `--fork-session` copies it into a new one ([upstream](../../externals/agent-adapter/claude-reference.md#launch-flags)). A resumed Claude row therefore keeps its identity. The picker's `SessionStart` binds the selected session, which revives a closed recorded row or adopts the provisional launch identity.

Smart and idle compaction append the configured summary brief after `/compact `. RimZ types the brief as a second write, so Claude's paste handling cannot swallow the slash command.

### Settings merge

Claude honours only the last `--settings` on its command line, so every RimZ addition merges into one settings document. [`merge_settings`](../../../crates/rimz/src/agents/adapters/claude/mod.rs) reads the profile's last `--settings` value (inline JSON, or a file path resolved against the cwd, both parsed as JSONC), applies one step's change, removes every `--settings` occurrence, and appends one. Arrays it extends gain only new entries, and profile entries stay first. The process compiler in `crates/rimz/src/harness/launch.rs` runs the steps in this order:

1. Host skill list. `skillOverrides` marks each unlisted skill directory `user-invocable-only` ([host mode](../sandbox.md#host-mode)).
2. Listed skill permissions. Each name in the profile's `skills` list adds an exact `Skill(<directory name>)` rule to `permissions.allow`. A name containing `*` gets no rule, and an empty set leaves argv untouched.
3. Definition permission rules. A definition's validated `allowed-tools` rules join `permissions.allow`. These may carry wildcards.
4. Routine RimZ permissions. Skipped when [`agents.allow-routine-rimz`](../../guide/configuration.md#routine-rimz-commands) is off (the machine-only switch defaults on). `permissions.allow` gains a `Bash(<prefix> *)` rule per routine RimZ command prefix and an exact `Skill(<name>)` rule per installed `rimz-*` skill. `sandbox.excludedCommands` gains the same prefixes. `permissions.additionalDirectories` gains the tmp and shared paths. `autoMode.environment` gains `$defaults` once, beside prose that names the routine commands and this launch's `$TMPDIR` and `$RIMZ_SHARED` paths.
5. Team notes. When the checkout has a team archive run for this launch's team, its directory joins `permissions.additionalDirectories`.
6. Command sandbox. `sandbox.filesystem.allowWrite` gains the room's state, runtime, and shared roots at every isolation level. Scoped isolation adds `enableWeakerNestedSandbox: true`, and `sandbox` isolation adds `enabled: true` and nothing else.
7. Fast mode. The fast preset rides outside the document until every other step has run, then merges `fastMode: true`.

Steps 2 to 6 apply to every permission mode, every isolation level, and child, resume, and fork launches alike. A preflight compile has no launch directories and skips them. Other user settings, other `autoMode` classifier arrays, and other sandbox keys survive the merge. RimZ checks no sandbox dependency, so Claude's own behaviour applies when its sandbox cannot start ([upstream contract and probes](../../externals/agent-adapter/claude-reference.md#per-launch-permissions-and-auto-mode)).

Routine skill names come from the provider skill root, then the RimZ library, through `crates/rimz/src/agents/skills.rs::enumerate`: only `rimz-*` directories that hold a `SKILL.md`, sorted by directory name, with the provider copy winning a collision. The tmp path is the one the agent sees (`/tmp` in the scoped view, the temp unit's host path otherwise). The shared path is the room's host `shared/` at every level. `crates/rimz/src/agents/capabilities.rs::LaunchCapability::allow_routine_rimz_args` is a no-op for other providers.

The merged document lands in one of two places. A profile settings file becomes a private content-addressed `settings.<sha256>.json` artifact under the runtime prompt directory. The host-skills step also makes inline profile settings private. Everything else stays inline JSON on argv. A later step reads the pending artifact's value, since the file does not exist yet, and re-digests it. Compilation writes nothing, and apply publishes the one artifact at every isolation level. Reapplying the merge is idempotent.

Invalid JSON, a non-object settings value, or a wrongly typed key a step extends refuses the launch before exec. The error names the source and the key, and each step adds its own fix:

| Step | Fix the error offers |
| --- | --- |
| listed skill permissions | `correct that key, or remove the profile's skills list` |
| definition permission rules | `correct that key, or remove the definition's allowed-tools list` |
| routine RimZ permissions | `correct that key, or set allow-routine-rimz = false`. An unreadable skill root names that root and offers fixing access or turning the switch off. |
| host skill list, team notes, command sandbox, fast mode | none beyond the source and key |

### System prompt

RimZ puts its merged launch reminder (team context, model identity, and delegation guidance) in `--append-system-prompt`. The flag is not a typed preset, so a user who wants provider-specific append text puts it in raw profile `args`.

On resume or fork, RimZ compares the configured system-prompt fingerprint with the fresh launch's baseline. The fingerprint is a lowercase SHA-256 over the composed replacement prompt and any native system-prompt text or file contents in profile args, never RimZ's per-launch reminder. An equal baseline keeps Claude's stored snapshot. A missing or different baseline adds `--system-prompt-snapshot off`, but only when the launch's executable reports version 2.1.257 or later.

The version probe runs the program resolved through the configured shell startup and launch environment, never the `claude` on ambient `PATH`. An unresolved program, or an unknown or older version, adds nothing. Fresh launches never add the switch. Resume and fork never replace the baseline, so later resumes keep rebuilding while the fingerprint differs, and a fork's own later resumes have no baseline. Editing a prompt file at the same path changes the fingerprint.

### Native LSP denial

Effective shared-language-server configuration calls `disable_native_lsp_args` only when the launched profile makes `rimz-lsp` callable, and it merges `LSP` into one `--disallowedTools` flag before subagent lockdown adds `Agent`. The denial follows configuration even when optional admission is refused, and profile `--tools` allowlists stay unchanged. Without a callable `rimz-lsp`, RimZ leaves the profile's flags alone, a user-configured `LSP` denial included. The [pinned precedence probe](../../externals/agent-adapter/claude-reference.md#native-lsp-denial) shows why this keeps Claude from starting private servers. Server lifecycle, callability, and reminders belong to [shared language servers](../lsp.md).

### Folder trust

[`folder_trust`](../../../crates/rimz/src/agents/adapters/claude/folder_trust.rs) models the [upstream folder-trust decision](../../externals/agent-adapter/claude-reference.md#folder-trust). It resolves `.claude.json` through the same login-aware path as remote-control consent. When a main repository root is supplied, it is the trust key, which covers a linked worktree. Outside Git, the cwd and its ancestors are checked. Both canonical and as-written paths match. Only `hasTrustDialogAccepted: true` decides, and `false` remains an offer to grant.

The grant key is the canonical main root or cwd. Claude never persists a home-directory grant, so that key returns a gap without a writable preview. Missing homes and unreadable, malformed, or non-object JSON also produce actionable gaps, so none of them appears decided.

The preview sets `projects.<key>.hasTrustDialogAccepted` through `json_edit.rs`. A borrowed JSON parser locates the edited value, so every unrelated byte and the key order survive. The preview is accepted only when it parses back to the original value with exactly that leaf changed. The neutral [`grant_folder_trust`](../../../crates/rimz/src/agents/folder_trust.rs) writer rejects bytes changed since the preview and publishes through the symlink-following provider-file writer. A provider write between that freshness check and the replacement is not serialized by this API.

### Headless form

`ClaudeHeadless` ([`claude/headless.rs`](../../../crates/rimz/src/agents/adapters/claude/headless.rs)) is the [headless form](./adapter.md#the-spec) that loop and workflow judges run. Its argv is `claude <profile args> -p --output-format json --json-schema <schema> -- <prompt>`. It merges `disableAllHooks = true` into the profile settings through the same [settings merge](#settings-merge), pending artifact included, so the check fires no RimZ hook and exactly one `--settings` survives.

`read_result` parses the last non-blank stdout line as Claude's JSON result:

| Field | Becomes |
| --- | --- |
| `is_error` with `result` or `subtype` | an error whose text passes through `claude/statusline.rs::classify_api_error`. `PausedRateLimit` and `PausedSpendLimit` become `ProviderLimit`, and every other class becomes `Other` |
| `structured_output` | the value. A result without it is an `Other` error |
| `total_cost_usd` | the cost, kept only when finite and non-negative |
| `usage` | input tokens (fresh plus cache-creation plus cache-read) and output tokens |

A line that is not valid JSON is an `Other` error with no tokens or cost.

## Subagents

Claude reports Task-tool children through `SubagentStart` and `SubagentStop`. The child's `agent_id` keys its row, and the payload's `session_id` becomes `parent_agent_id`, so the child nests under its parent ([model.md](./model.md#subagents)). The row's `task` is the child's `agent_type`, falling back to `subagent_type` or `description`.

Identity is never guessed. The shared [`resolve_subagent_identity`](../../../crates/rimz/src/agents/identity.rs) requires a child id distinct from the parent id. An event that fails the check is quarantined: it yields no observation, logs at `error!` under `rimz::agent::lifecycle`, and never folds onto or renames the parent's row.

Claude stamps `agent_id` on every payload fired inside a subagent, and `decode_hook` treats any payload whose `agent_id` differs from its `session_id` as the child's. A backgrounded child's tool events, permission requests, and compaction events therefore fold onto the child row. This boundary matters for attention: a child's tool events folded onto the parent would advance the parent's `last_activity` past `waiting_since` and release its waiting row while the parent is still blocked.

A subagent payload carries two transcripts: `transcript_path` is the parent's and `agent_transcript_path` is the child's. Only `SubagentStop` reads the child transcript, and no child event reads the parent's, so the parent's figures never land on the child's row. A child's model is the payload's `model` when present, else the child transcript's. A child carries the request split and no `total_tokens`. Its transcript's newest usage covers one request, not the run, so the sidebar types the child figure from the split (`SubAgentTokens::Window`) or a whole-run total.

`SubagentStop` has no outcome or exit-code field, so the close is always non-errored. The reducer carries the child's type label forward: a stop that omits or blanks `agent_type` leaves a started child labelled. A stop-only child with no type has too little identity for a row and is ignored at reduction.

Claude runs `/btw` as a sidechain that emits only `SubagentStop`, with a short child `agent_id` and the main session as parent, and writes no start hook, prompt hook, or main-session transcript entry. That stop stays out of the child rollup. It touches the parent's activity heartbeat and pulses an already-open active-time span without opening one.

### Interrupted children

Esc can end a child without `SubagentStop`. On the parent's next root `PreToolUse`, `PostToolUse`, or `Stop`, hook ingestion calls `spawned_subagents`. It reads each `subagents/agent-<id>.jsonl` beside the parent transcript and returns the children whose newest conversation record, with `agentId` equal to the filename id, is the `[Request interrupted by user` marker. The `agentId` match keeps a nested child's replay from settling its parent.

The store closes each returned child with `SubagentStopped` (`crates/rimz/src/store/writer/lifecycle.rs::derive_lifecycle_events`). The close is non-errored unless the child's row is already `Failed`, which keeps the failure. A child with no row is adopted, with identity from `agent-<id>.meta.json` and usage from its transcript. An existing child keeps its hook-established identity, while its model and tokens may update.

If Esc interrupts the whole parent turn, no parent hook remains to trigger the repair. The next prompt's supersession reap corrects the display, and that turn's first parent tool use or turn end closes the durable child bracket.

### Child statusline and cost

Install wraps Claude's `subagentStatusLine` with `rimz statusline feed --source claude --subagent`. [`subagent_statusline.rs`](../../../crates/rimz/src/agents/adapters/claude/subagent_statusline.rs) reads each entry of the `tasks` array (`id`, `agentType`, `model`, `effort`, `description`, `startTime`) into a per-child sidecar the sidebar folds onto the child row ([sidebar.md](../sidebar/sidebar.md)). The task kind in `type` is ignored and never serves as an agent-type fallback. The statusline model and effort paint a running child until lifecycle metadata supplies its own. Once the child transcript names a request model, that model replaces the statusline's.

The same feed prices the child. [`subagent_cost.rs`](../../../crates/rimz/src/agents/adapters/claude/subagent_cost.rs) derives `subagents/agent-<id>.jsonl` from the parent's `transcript_path` and advances a byte cursor through the child's usage records, pricing each request under its own model and cache tier. The cursor keeps the cumulative cost, the newest child model, and the last request key, so a contiguous richer duplicate replaces its earlier record. A request whose model is missing from the price book marks the cursor unpriced, and the figure is withheld instead of shown as a partial sum. The cursor records the price book and managed-settings fingerprints and replays the whole child transcript once when either changes. The figure is display-only, because the parent session's spend already includes its children.

## Context and transcript

Claude names the session log in every hook payload's `transcript_path`. For the hook-time usage read, `decode_hook` takes the newest assistant `message.usage` in the bounded tail:

| Transcript field | Internal |
| --- | --- |
| `input_tokens` + `cache_read_input_tokens` + `cache_creation_input_tokens` | context tokens, the gauge numerator |
| context tokens + `output_tokens` | `total_tokens`, root sessions only |
| `input_tokens` | `fresh_input_tokens` |
| `cache_read_input_tokens` | `cache_read_input_tokens` |
| `cache_creation_input_tokens` | `cache_write_input_tokens` |
| `output_tokens` | `output_tokens` |
| `message.model` | `model`, the bare id |

The four components travel with their total so the card can show where the window went. A warm session is mostly cache reads, and 250k tokens of cache reuse reads very differently from 250k of fresh input. When the whole file was read and holds no assistant usage or compaction boundary, the adapter reports `total_tokens = 0` and leaves the split `None`. A bounded tail without either supplies no reading, which preserves the prior total under the shared [reading rules](./adapter.md#reading-rules).

A finished compaction writes a `{"type":"system","subtype":"compact_boundary","compactMetadata":{…}}` record, and no usage-bearing record follows it until the next API call. This shape is observed in transcripts on disk, not documented upstream. Every usage record older than the boundary was measured before the compaction, so a boundary met first in the newest-first walk ends it. The reading is then `total_tokens = compactMetadata.postTokens` with no split, or no reading at all when `postTokens` is absent. A usage record newer than the boundary wins. A hook that fires before the boundary is written still re-reads the old record, which the rollup [withholds by value](./model.md#the-compaction-bracket).

### Context window

The transcript writes only the bare model id, so the gauge's divisor comes from elsewhere. A `[1m]` marker on the hook payload's model sets 1,000,000 tokens. Otherwise `context_window_for_model` uses the model's exact `max_input_tokens` from the shared price book, and an unknown model leaves the window unset so the spec default of 200,000 applies. A marker-less hook never lowers an established window. The statusline's `context_window_size` comes first in projection precedence, so neither inferred value overrides a provider-reported window.

### Session-cumulative usage

`local_context_refresh` folds each assistant API response once into session-cumulative input, output, cache-write, and cache-read counters ([`local_context.rs`](../../../crates/rimz/src/agents/adapters/claude/local_context.rs)). Claude writes one usage row per content block, and a row can expand into main-model and advisor-model usage, so `LocalSpendFold` keeps the last keyed response group and skips or replaces its contiguous duplicates. Among hooks, only `SessionStart`, `UserPromptSubmit`, `PostToolUse`, and `Stop` trigger the refresh.

The byte cursor, duplicate window, and sums are stored together, so steady refreshes parse only appended lines. A truncated file restarts the fold, and a stored fold that lacks the duplicate window replays once. This is why Claude keeps its own unchanged-source gate ([adapter.md](./adapter.md#context-sources)). The transcript path resolves in order: the hook's `transcript_path`, the stored path, then the newest `<config>/projects/*/<session_id>.jsonl`. The result updates `AgentTokenUsage.session_usage` and leaves the statusline's current-window fields and cost in charge.

### Conversation parser

`parse_transcript_messages` (`statusline::parse_messages`) is the one conversation parser for Claude transcripts. It keeps `user` and `assistant` entries with their timestamps and drops sidechain replay, `isMeta` user entries, and API-error assistant entries. Supervised streaming uses the default `stream_assistant_messages`, which filters that parse.

### Rich context

Claude runs its configured `statusLine` command on every render and pipes it a JSON blob ([schema](../../externals/agent-adapter/claude-reference.md#statusline-json)). Install points `statusLine` at `rimz statusline feed --source claude`, and [`observe_context`](../../../crates/rimz/src/agents/adapters/claude/statusline.rs) parses the blob into `AgentContext` and runs the turn-death and turn-interruption scans over the transcript tail. `fast_mode` supplies the live `AgentContext.fast` reading, including explicit off. A payload that omits it leaves the badge on its launch fallback. The tests in `statusline/tests.rs` pin the field shapes.

A user who already has a `statusLine` keeps it. Install wraps the original command, which receives the same JSON, and RimZ forwards its stdout and exit code, so the rendered line is unchanged. The original is stored verbatim under `_rimz_wrapped` and restored on uninstall. The wrap is a security surface: the consent prompt summarizes both statusline wraps, the install diff shows them in full, and the wrapped child's stdio is fully piped.

The statusline's `cost.total_cost_usd` covers only the live process and reads `0` after a resume. On a turn-ending hook, `supplement_realtime_cost` prices the whole session transcript and replaces the card cost only when that total is higher.

### Turn-death marker

The transcript tail is the backstop for a provider error that `StopFailure` did not report. [`detect_turn_error`](../../../crates/rimz/src/agents/adapters/claude/statusline.rs) runs on each statusline push and on `Stop`. It scans newest-first and skips sidechain entries, entries other than `assistant` and `user`, and entries without a parseable `timestamp`. The first entry left decides. An `assistant` entry flagged `isApiErrorMessage: true` emits `AgentContext.turn_error` with its timestamp, a label capped at 80 characters, and a class from `classify_api_error` over the entry's `error`, `apiErrorStatus`, and label. Any other deciding entry means the turn is alive or recovered.

Every statusline push re-derives the marker, so `StopFailure` shares the classifier, or the tail would overwrite the hook's class. `classify_api_error` checks in this order:

| Evidence | Class |
| --- | --- |
| a label the shared [`TurnErrorClass::classify_label`](../../../crates/rimz/src/agents/context.rs) reads as a spend limit (Claude tags spend caps `rate_limit` and 429 too) | `paused_spend_limit` |
| `error` of `oauth_org_not_allowed` or `billing_error` | `not_entitled`: an actionable failed row with the upstream label, never a resumable park |
| `error: "rate_limit"` or `apiErrorStatus: 429`, whatever the text (a model cap such as "You've reached your Fable limit") | `paused_rate_limit` |
| `error` of `overloaded` or `server_error`, whatever the text | `paused_overloaded` |
| anything else | `classify_label` over the label: rate-limit text or HTTP 429 pauses on the rate limit, transient server or connection text or HTTP 5xx pauses on backoff, and anything else fails |

The marker changes display only: [model.md](./model.md#displayed-status) pauses or fails the row, and the rollup does not change ([upstream shape](../../externals/agent-adapter/claude-reference.md#transcript-death-certificate)).

When the hook lifecycle merges a changed `not_entitled` marker, it marks the recorded agent login's [entitlement](./providers.md#entitlement) lapsed at first observation and makes its direct usage read due. Replayed drains skip this side effect, and a missing agent row warns instead of guessing a login. The statusline remains a reader. The transcript backstop reaches this producer through the ordinary hook merge.

### Turn-interruption marker

Esc fires no root lifecycle hook. Claude appends a timestamped `user` entry whose text begins `[Request interrupted by user` (the `for tool use` variant included). When the same scan finds that entry first, `observe_context` stamps `AgentContext.settle` as `Interrupted`. A marker newer than `last_activity` displays a falsely `running` row, or a waiting row whose ask Esc cancelled, as `idle`, and any newer hook activity clears it. The root marker changes display and delivery gates only, and the durable rollup keeps the reported status. Child markers drive the [interrupted-child repair](#interrupted-children).

## Local session discovery

[`local_sessions.rs`](../../../crates/rimz/src/agents/adapters/claude/local_sessions.rs) turns Claude's project store into identity-only [local session observations](./instances.md#local-session-observations). Hooks stay authoritative for lifecycle state, prompts, waits, asks, compaction, context, and clocks on a session with a durable row.

Discovery reads each config root: a valid `CLAUDE_CONFIG_DIR`, otherwise the XDG Claude directory and `~/.claude`. For each admitted workspace it looks in `projects/<bucket>`. It tries the bucket named by a non-empty `CLAUDE_CODE_PROJECT_DIR_NAME` first, then the absolute workspace path with every non-alphanumeric byte replaced by `-`. Both bucket names are only an index. A candidate is accepted when its first record carrying both `cwd` and `timestamp`, within the first 32 lines and 64 KiB, names the exact workspace, which also rejects collisions on a shared config root.

| Rule | Value |
| --- | --- |
| candidate files | `*.jsonl` whose stem is a UUID; the stem is the `claude --resume` id |
| records allowed before the first session record | metadata such as `mode`, `permission-mode`, `bridge-session`, `file-history-snapshot` |
| first session record | must not set `isSidechain` |
| activity bound | newest tail-record timestamp, else file mtime |
| cap | the 512 newest candidates per admitted workspace, so one workspace cannot evict another |

The producer keeps a catalog keyed by the ordered config roots and the normalized workspace batch. A directory topology change rebuilds the catalog, and a changed file re-runs only its head and tail validation. Restatting the known catalog before the newest-512 selection lets an older transcript whose mtime advances re-enter without a recursive walk. Unchanged accepted and rejected heads stay cached until their stamps change or the 30-second backstop expires.

## Account and balance

The account model these fields fill is [providers.md](./providers.md), and the per-provider cadences are in [providers.md](./providers.md#the-out-of-band-probe).

| Source | Read by | Produces |
| --- | --- | --- |
| [`claude auth status`](../../externals/agent-adapter/claude-reference.md#auth-surface) | `account::probe`, with null stdin and piped stdout and stderr | `plan` from `subscriptionType`, with `max` refined from the same login's credential metadata (below); `metered` false for `authMethod` `api_key` or legacy `apiKey`, true for another non-empty method, unknown when absent. The CLI exits 1 when logged out and still prints `"loggedIn": false`: that is `LoggedOut`, and any other non-zero exit is `Unavailable` |
| `claude --version` | the shared display-only version probe | the account's version field, also filled into entries whose login facts are fresh |
| statusline `rate_limits` | `observe_context` | 5h and 7d windows plus the gateway's durationless `spend` scope (label `$`), from epoch-second resets, marked `BestEffort`; spend percentage only, no dollar amounts or period |
| OAuth usage endpoint | [`oauth_usage.rs`](../../../crates/rimz/src/agents/adapters/claude/oauth_usage.rs) | the same windows from RFC 3339 resets, marked `Authoritative`; `extra_usage` cents as `ExtraCredits::Known { used_usd, limit_usd }` or `Disabled`; model sub-caps; `cedar_ember` as `ResetCredits` |
| OAuth profile endpoint | `AccountCapability::probe_account_profile` in the same reader | a recognised plan only when the profile's owner matches the fresh auth-status fingerprint; no entitlement, usage, or metering changes |

For a found `max` account, `account::probe` reads `claudeAiOauth.rateLimitTier` through the usage reader's account-home and Keychain resolution. `default_claude_max_5x` and `default_claude_max_20x` refine the plan to RimZ-normalized `max_5x` and `max_20x`, labelled `Claude Max 5x` and `Claude Max 20x`. A present credential `subscriptionType` other than `max` contradicts the refinement. Missing, unreadable, malformed, absent-tier, unknown-tier, or contradictory credentials leave `max` intact, and other account plans are untouched. This display-only read ignores token expiry and scopes, makes no profile request, and never changes the probe outcome or metering. The credential fields are observed in [Claude Code 2.1.306](../../externals/agent-adapter/claude-reference.md#credentials). They are cached provider metadata, not a fresh entitlement check.

The separate live profile read can override that saved plan. A `claude.ai` auth status with non-empty `email` and `orgId` carries an opaque SHA-256 fingerprint over the case-folded email and organization UUID. Neither raw value is kept, and Debug redacts the fingerprint. The profile must carry the same owner. `organization_type = "claude_pro"` maps to `pro`, and `claude_max` maps only with `default_claude_max_5x` or `default_claude_max_20x`. Other families, unknown Max tiers, and contradictory owners are answered but unrecognised, which leaves auth status in charge. Malformed or identity-less bodies and failed reads are failures, not answers. Subscription status and account feature flags do not decide the plan.

The capability uses an unexpired bearer with `user:profile`, without renewal, and one bounded GET: five seconds, no redirects, 512 KiB maximum, host-only breadcrumb. Its URL derives from the trusted usage origin, as reset preparation's does. The caller owns [cadence and resolution](./providers.md#the-out-of-band-probe).

[`account.rs`](../../../crates/rimz/src/agents/adapters/claude/account.rs) owns the shared 5h and 7d durations and utilization normalization for both window sources, the spend quota included: values at or above 100 clamp to 100, and values below 100 never round up to spent.

OAuth maps each `limits[]` entry of kind `weekly_scoped` with a non-empty `scope.model.display_name` to a model sub-cap: scope `model:<lowercased name>`, the name as label, and the 7-day parent duration. Usage stays on the model's own axis, so 58% used leaves 42% of that model's cap. Claude's statusline carries no model-scoped window, so sub-caps come from OAuth only. The sidebar draws a sub-cap as a tick on its parent's bar ([interface/sidebar.md](../../interface/sidebar.md)), and an elapsed sub-cap reads unknown until the provider reports it again ([providers.md](./providers.md#persistence-across-idle-sessions)).

### OAuth usage probe

The producer runs `rimz agents refresh-usage` for Claude and claims each run durably in `credits.json` on `OAUTH_USAGE_TTL` (five minutes). A due read runs whether or not a root Claude session is reporting statusline windows. It completes the account-scoped credits entry, and a response carrying usage also merges its windows, which then anchor fusion: a statusline reading paints over them only when observed later ([providers.md](./providers.md#across-sources-and-time)). The credentials file's mtime rides the claim as the cheap scheduling stamp.

The probe reads `.credentials.json` in the config home (the first `CLAUDE_CONFIG_DIR` entry, otherwise `$HOME/.claude`). The Keychain fallback applies when that file is missing, `CLAUDE_CONFIG_DIR` names no directory (unset, or only commas and blanks), and the host is macOS. The probe then asks the Keychain through `/usr/bin/security` with null stdin and a 1.5-second deadline. A timeout or a denied or failed call is a quiet `NoCredentials`, though macOS can still show Keychain UI briefly. The probe is read-only: it never refreshes a token or writes credentials.

The usage request appends `cedar_ember=1` through `url::Url`, which preserves any query on a trusted URL override. Its User-Agent is `claude-cli/<version> (external, cli)`, with the version from the `claude --version` probe, else `USER_AGENT_FALLBACK_VERSION` (`2.1.283`). Anthropic gates grants on both the exact format and a recent version ([observed protocol](../../externals/agent-adapter/claude-reference.md#oauth-usage-endpoint)). An installed old version is reported as it is and may receive no grants until upgraded. The fallback pin needs maintenance if Anthropic raises that floor, or fallback hosts silently show zero. Pi and OpenCode delegated Anthropic reads inherit this request and parser.

A usage HTTP 403 whose JSON body carries `oauth_not_allowed_for_organization` at `error.details.error_code` returns the provider-neutral `AccountUsageProbe::NotEntitled`, not `NoCredentials`. Plain 401 and other 403 responses remain settled credential rejections. The bounded HTTP transport hands the error body only to the adapter, and diagnostics keep the status, code, and host, never the body. Pi and OpenCode delegated Claude usage reads inherit this classification.

The usage observation owns the account key: a domain-separated SHA-256 of `refreshToken`, or of `accessToken` when there is no refresh token. Only the digest enters the shared cache, and token rotation keeps one owner while the refresh token is stable.

#### Reset credits

Every successful parse sets `reset_credits` to `Some`. Absent `cedar_ember`, ineligibility, and empty grants settle to zero, which clears a previously cached grant instead of preserving it through the cache's missing-field fallback. Eligible grants contribute the sum of `resets_left`. Each valid `ends_at` is repeated once per remaining reset and sorted by `ResetCredits::normalized`. Missing or invalid expiry leaves the count intact, and other grant flags do not filter the banked count. Each credit carries the effect `KeepsSchedule`: redeeming a Claude credit refills the window and leaves its reset where it was. Claude has a manual redemption path but no automatic redemption or forecast.

`crates/rimz/src/agents/adapters/claude/oauth_usage.rs::prepare_reset_credit` also reads the profile. It adds `skip_spend=1` only to its preview usage read, then reads `GET /api/oauth/profile` and keeps `organization.uuid` in its private action. The flag's effect on paid-usage fields is unverified, so periodic, delegated, and post-reset refresh reads keep the full `?cedar_ember=1` query. Both profile and claim URLs derive from the trusted usage URL's origin, so one loopback override covers the whole flow. A profile HTTP failure names that read, not usage.

Claimability is separate from the banked count. These create a neutral hold, checked in this order: absence or ineligibility, a future cooldown, no selected grant, or a selected grant that is paused, empty, future, expired, or not usable. A limit-only grant explains that restriction when the account is not at the limit. The shared manual consumer refuses holds before reserving.

The private action targets `next_grant_id` and sends `POST /api/organizations/{org}/reset_rate_limits` once through the non-retrying transport. Result strings map to the neutral outcomes, `cooldown` included, and unknown strings are not success. A failed claim leaves the durable reservation standing and names Settings > Usage as the place to check the unknown outcome. A `reset` alone refreshes usage. The wire is [observer evidence](../../externals/agent-adapter/claude-reference.md#oauth-usage-endpoint), not an Anthropic API contract, and request-id idempotency and live redemption remain unverified.

### Birth account key

A root `SessionStart` that maps to `Registered` stamps the same digest onto the session, so a session records the login it was born on. [`enrich_root_registration`](../../../crates/rimz/src/agents/adapters/claude/mod.rs) reads the credentials once per registration, from the same file with the same Keychain fallback. It applies neither the expiry check nor the `user:profile` scope check the usage probe applies, because the key identifies an account and authorizes nothing. A `compact` start maps to `CompactionEnded` and a subagent has a parent, so neither stamps a key. A `resume` start does, and rebinds the row to the login in force at that moment. What the key partitions is [providers.md](./providers.md#producer-aggregation), and where it lives is [model.md](./model.md#the-rollup).

The stamp reads the credentials file, not the token the Claude process sends, and RimZ cannot see whether a running process picked up an account switch. A `/clear` after switching accounts therefore registers the new file's key while the process may still bill the old account. The session's live windows then count against the new account until the process restarts.

## Cost

[`spend.rs`](../../../crates/rimz/src/agents/adapters/claude/spend.rs) parses Claude's full-history spend, read-only and sidebar-safe, and the [`SpendingWalker`](../../../crates/rimz/src/agents/spending/mod.rs) aggregates it into the configured headline, local calendar day, and trailing 7d, 30d, and 365d windows ([spending.md](./spending.md#cost-history)). The fleet walk reads every `**/*.jsonl` under each config root's `projects/`.

A Claude session spans several files, and the spec's `ThreadKey::SessionDir` makes the session directory the thread. The per-session seat fold resolves the main transcript plus every `subagents/*.jsonl` companion in either layout: `<session_id>.jsonl` beside `<session_id>/subagents/`, or `<session_id>/chat.jsonl` inside it.

Claude replays parent messages into each subagent file, so the walk deduplicates by `(message.id, requestId)` across files. It prefers the main-thread record, then the larger token total, then the record with speed metadata, and suppresses sidechain replay so the selected turn counts once.

Pricing works per request:

- A `message.usage` without `costUSD` is priced through the [price book](./spending.md#token-pricing), with input, output, 5-minute cache creation, 1-hour cache creation, and cache read each at its own rate. Current transcripts carry no `costUSD`.
- A transcript line that logs a positive `costUSD` uses that figure verbatim for the top-level request.
- Each `advisor_message` in `message.usage.iterations` is a separately billed request with its own model and tokens. It becomes a child entry keyed `<message.id>:advisor:<n>`, priced independently.
- When machine managed settings define `modelPricing` ([upstream](../../externals/agent-adapter/claude-reference.md#project-storage-and-managed-pricing)), [`managed_pricing.rs`](../../../crates/rimz/src/agents/adapters/claude/managed_pricing.rs) overlays its contracted rows and optional finite multiplier (`0 < m <= 10`) on the list book for spend, the session-cumulative fold, and child costs. Unreadable or invalid settings leave the list book as is.
- A model with no known price keeps its tokens with zero dollars while the pricing chase looks for a price. The `<synthetic>` model is dropped, because it is not an API model id.

Historical tool statistics count the `tool_use` blocks on the usage-bearing assistant messages the spend parser already reads, so the same deduplication counts retried and sidechain copies once.

## Remote control

RimZ hosts `claude remote-control --spawn worktree` in the room's `rimzd` view when `[remote_control] claude = true` ([rimzd.md](../rimzd.md)). The host argv always carries an env prefix: `env CLAUDE_CONFIG_DIR=<home>` when the room's login sets one, else `env -u CLAUDE_CONFIG_DIR` (`remote_control.rs::host_argv_with_login`). Passing `--spawn` also skips Claude's spawn-mode chooser. The provider side has three parts, and the upstream gates behind them are in [claude-reference.md](../../externals/agent-adapter/claude-reference.md#remote-control).

### Hooks from remote sessions

A remote session's SDK child inherits the user's Claude hooks, but its events are infrastructure. `hook_ingress` returns `Ignore` before stdin, workspace resolution, or store access, so the session never creates a RimZ card. The child is recognized by a non-empty `CLAUDE_CODE_SESSION_ACCESS_TOKEN`, by `CLAUDE_CODE_ENVIRONMENT_KIND=bridge`, or, for SDK children without those markers, by a walk of up to 32 ancestor processes that finds `claude remote-control`.

### Readiness

[`remote_control.rs`](../../../crates/rimz/src/agents/adapters/claude/remote_control.rs) returns one provider-neutral readiness result that room start, `rimz doctor`, daemon-view reconciliation, and runtime toggles all consume. It checks in order and stops at the first block:

| Check | Blocks when |
| --- | --- |
| install | `claude` is not on `PATH` |
| settings | `disableRemoteControl: true` in Claude's `settings.json` |
| consent | `remoteDialogSeen` in `.claude.json` is missing, or set to anything other than `true`; an unreadable file does not block |
| traffic switches | `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` or `DISABLE_GROWTHBOOK` is present in the launch environment or settings `env` |
| folder trust | the supplied project root has no Claude folder-trust decision under the selected login; Git rooms require the exact root key, while non-Git rooms also accept a trusted ancestor; machine-wide toggle preflight supplies no root and skips this check |
| version | `claude --version` reports below 2.1.51; an unreadable version warns and skips the version-gated checks |
| OAuth token (every version) | `CLAUDE_CODE_OAUTH_TOKEN` in the launch environment or settings `env` |
| authentication (2.1.139 and later) | `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` in the launch environment or settings `env`, or `apiKeyHelper` in settings |
| endpoint (2.1.196 and later) | an `ANTHROPIC_BASE_URL` other than `https://api.anthropic.com`, or `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, or `CLAUDE_CODE_USE_FOUNDRY`, in the launch environment or settings `env` |

Traffic, trust, and OAuth token checks apply even when the version is unknown. The authentication, OAuth token, and endpoint rows report together as one refusal that lists a fix for every source that applies. A long-lived `CLAUDE_CODE_OAUTH_TOKEN` blocks because it can make model requests but cannot establish Remote Control. Its structured fix names the shell and settings sources and `claude auth login`.

Each issue maps to a `Blocker`: one problem and ordered user-run fixes. Traffic fixes are marked privacy, and authentication and folder-trust fixes are marked security. A fix command that runs `claude` (`claude auth login`, `claude remote-control`) carries `CLAUDE_CONFIG_DIR=<home>` when the room's login sets one, so it acts on the account the host would run under. RimZ never applies these fixes. Only room judgment adds the permanent host-off command, and enabling a toggle does not suggest turning it off again.

An attended `rimz start` offers folder trust before readiness judges the host. A detached start, or an offer the user does not accept, warns with the trust fix and runs the blocked host off for this run, without a prompt.

A running host keeps the environment it was spawned with when `rimz accounts use` changes the room's launch default. The next host spawn uses the new default, and the switch does not replace a live host pane.

`remote_control_status` serves a different reader: it reads `remoteControlAtStartup: true` to light the provider dashboard's `⇅ rc` flag for ordinary pane sessions. It withholds the flag under `disableRemoteControl`, under either traffic switch, or when the OAuth token, authentication, or endpoint rows above would block, reaching that answer through the same source rule readiness uses. It skips folder trust, because the pane's own launch already trusted its folder.

### Consent

`claude remote-control` asks `Enable Remote Control? (y/n)` once per machine and records the answer as `remoteDialogSeen` in `${CLAUDE_CONFIG_DIR:-$HOME}/.claude.json` (`RIMZ_CLAUDE_GLOBAL_CONFIG` overrides the path). An unattended host pane would block on that prompt, so `prepare_runtime_control` seeds the key through [`remote_consent.rs`](../../../crates/rimz/src/agents/adapters/claude/remote_consent.rs) when remote control is enabled. It inserts `"remoteDialogSeen": true` as the root object's first member, so the rest of a file RimZ does not own keeps its order and formatting, re-parses the result, and replaces the file atomically. Seeding only fills a missing key: an explicit non-`true` value stays, and readiness refuses with the fix. Claude records the key whether the answer was `y` or `n`, so the key alone never proves consent. The RimZ config toggle is the operator's intent.

### Liveness

A managed pane keeps its launch argv as its title for its whole life, so pane presence reads healthy long after the host stops serving. [`remote_liveness.rs`](../../../crates/rimz/src/agents/adapters/claude/remote_liveness.rs) reads `<config>/projects/<bucket>/bridge-pointer.json` instead. It tries the `CLAUDE_CODE_PROJECT_DIR_NAME` bucket before the flattened workspace name, and checks that the recorded `pid` and `procStart` token still name a live process, the same evidence Claude uses to reuse a pointer. The first readable pointer decides: a live process is up, and a dead process, a missing `pid`, or an unparseable pointer is down. No pointer is unknown. The `⇅ rc` flag and doctor's serving line both read this result.

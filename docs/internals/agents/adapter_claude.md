# Claude Code adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The raw upstream protocol is in [claude-reference.md](../../externals/agent-adapter/claude-reference.md).

This page maps Claude Code onto RimZ's internal types: which hook carries which signal, how a Claude session launches, how Task-tool children become rows, where context, account, and spend figures come from, and how RimZ hosts `claude remote-control`. The code lives in [`agents/adapters/claude/`](../../../crates/rimz/src/agents/adapters/claude/mod.rs), and `ClaudeAdapter` in `mod.rs` implements every capability trait.

| Module | Owns |
| --- | --- |
| `mod.rs` | the `AgentSpec`, coverage declarations, the `CLAUDE_HOOKS` catalog, `decode_hook`, the launch settings merges, and transcript usage reads |
| `payloads.rs` | typed hook payloads |
| `ask.rs` | ask questions, answers, and `rimz answer` key plans |
| `install.rs` | the `settings.json` hook and statusline integration |
| `rimz_mod.rs`, `rimz_mod/` | the embedded RimZ mod (`plugin.json`, `hooks.json`, the `rimz.ts` hooks module), its folder writes, its drift check, and the parser of its report envelope |
| `statusline.rs` | statusline parsing, the conversation parser, and the turn-death and turn-interruption scans |
| `subagent_statusline.rs`, `subagent_cost.rs`, `subagents.rs` | child statusline enrichment, child cost cursors, and interrupted-child repair |
| `local_context.rs` | the session-cumulative token fold |
| `local_sessions.rs` | local session discovery |
| `local_sessions/tests.rs`, `local_sessions/tests/pane.rs` | full-scan and per-pane discovery tests, account isolation, and filesystem work budgets |
| `account.rs`, `oauth_usage.rs` | `claude auth status`, window normalization, the OAuth usage probe, and the account key |
| `spend.rs`, `managed_pricing.rs` | the full-history spend parser and the managed-settings price overlay |
| `remote_control.rs`, `remote_consent.rs`, `remote_liveness.rs` | remote-control readiness, consent, and host liveness |
| `folder_trust.rs`, `json_edit.rs` | folder-trust decisions, grant previews, and byte-preserving JSON leaf edits |

## Hooks and lifecycle

Every installed hook runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude`, and the event name comes from the payload's `hook_event_name`. The table lists what `decode_hook` makes of each event. The state machine the signals drive is in [model.md](./model.md#the-state-machine), and the upstream payloads are in [claude-reference.md](../../externals/agent-adapter/claude-reference.md#hooks-rimz-wires).

| Native event | Channel | [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs) | Notes |
| --- | --- | --- | --- |
| `SessionStart` | lifecycle | `Registered`, or `CompactionEnded { auto: None, echo: true }` for source `compact` | stamps lineage ([conversation replacement](#conversation-replacement)) and the [birth account key](#birth-account-key); model from the payload; source `compact` lowers an open head without counting or retiring ([the compaction bracket](./model.md#the-compaction-bracket)) |
| `UserPromptSubmit` | lifecycle | `TurnStarted` | sanitized text as `task` and `prompt` (below); a `<task-notification>` prompt drops the [background shells](#background-shells) it reports finished; a root prompt's reply can carry the [Environment block](../harness/fleet.md#environment-at-prompt-submit) as `hookSpecificOutput.additionalContext` |
| `Stop` | lifecycle | `TurnEnded { errored, parked_on_background }` | see [turn endings](#turn-endings); `background_tasks` replaces the [background shells](#background-shells), and clears the [background scripts](#background-scripts) when it lists no pending `code` task |
| `StopFailure` | lifecycle | `TurnEnded { errored: true }` when it writes a marker; otherwise none | writes `AgentContext.turn_error`, never a final assistant message ([turn endings](#turn-endings)) |
| `SessionEnd` | lifecycle | `Ended` | stamps `ended_at`; the runtime hides the retained resumable row |
| `Notification` | lifecycle | none | |
| `PreToolUse` | lifecycle | `ToolUsed { effect: Observed, name: None }` | proof of work, keyed by `tool_use_id`; a `REPL` call starts a [background script](#background-scripts), and that report is what Store appends it for |
| `PostToolUse` | lifecycle | `ToolUsed { effect, name }` | every tool, named, keyed by `tool_use_id`; answered asks record their answers; a backgrounded `Bash` starts a [background shell](#background-shells); a `REPL` call finishes its [background script](#background-scripts) |
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

Every signal derived from a `PreToolUse` or a `PostToolUse` carries that payload's `tool_use_id` as its [native key](./model.md#waiting-and-asks). Claude stamps one id per tool call, identical on the call's two edges and distinct across parallel calls (observed on Claude Code 2.1.278). A question the user has not answered therefore survives the tool calls Claude runs in parallel with the picker, and clears on its own `PostToolUse`. An ask whose payload omits the field degrades to the keyless behaviour, where any tool edge clears the wait. A tool payload without the field cannot clear an already keyed ask.

`PermissionRequest` has no `tool_use_id` on the wire and stays keyless. Its clearing edge is the approved tool's own keyed `PostToolUse`. That edge still clears it, because the sibling rule holds only a wait whose open ask is keyed.

### Tool events

The spec's `ToolClassification` decides the effect: `Editing` for `Edit`, `Write`, `MultiEdit`, and `NotebookEdit`, `Mutating` for `Bash`, and `Observed` for every other tool. `Editing` moves the turn's [phase](./model.md#turn-phase) from reasoning to acting.

A broad `PreToolUse` is unnamed proof of work. Store appends it only under the rule in [model.md](./model.md#the-state-machine): when a subagent owns it, when it carries a background script report, or when it clears a wait, closes a compaction bracket, or reconciles a resting row. Every `PostToolUse` carries its tool name, so Store always appends it, read-only tools and answered asks included, and the named completions feed the live session tool map.

### Turn endings

`Stop` carries two bits. `errored` comes from the shared `stop_payload_errored`. `parked_on_background` is set when the payload's `background_tasks` holds a task whose status is neither `completed` nor `failed`, or when `session_crons` is non-empty (both fields exist from Claude Code 2.1.145). [model.md](./model.md#turn-endings-and-parked-turns) turns those bits into the final status: a clean parked stop stays `running` with a `⋯ bg` marker, or `waiting` when a keyed ask is open, and an error always wins.

`StopFailure` is Claude's provider-error certificate. A non-empty `error` writes a turn-error marker, classified from `error` and the capped `last_assistant_message` label by the same `classify_api_error` the [turn-death marker](#turn-death-marker) uses. Whatever the class, it also appends an `agent.lifecycle` envelope with `TurnEnded { errored: true, parked_on_background: false }`, and the rollup becomes `failed` at that hook. A rate-limit, spend-limit, or overload marker displays the row `paused` and keeps its supervised run `running` ([completion](../harness/scripting.md#completion-folding-lifecycle-observations)); any other class fails the run unless the run belongs to a team or spans the session. A blank or missing `error` writes neither a marker nor a lifecycle envelope. The error label remains an `Error` transcript entry, never an assistant entry or the run's `last_message`.

On `Stop`, when no `StopFailure` certificate applies, `decode_hook` runs the turn-death scan over the transcript tail. That scan covers Claude builds without `StopFailure` and sessions whose hooks were installed late.

The RimZ mod's `turn.complete` report runs the whole resting-turn scan at every turn end, Esc included, which fires no `Stop` ([RimZ mod reports](#rimz-mod-reports)). Its `reason` decides no marker: only a main-loop `aborted` with no interruption in the tail makes the feed read the tail again, since Claude writes the interruption rows 45 to 100 ms after the event.

### Background shells

A root session's hooks report its background shells as a `BackgroundShellReport` on the observation, and the rollup folds them into `AgentState.background_shells` ([model.md](./model.md#turn-endings-and-parked-turns)):

- A `PostToolUse` for `Bash` whose `tool_response` carries `backgroundTaskId` starts a shell with the `tool_input` command and description. Claude fires it at launch, both for `run_in_background` and for a foreground command auto-backgrounded past its timeout.
- A `Stop` with a `background_tasks` array replaces the list with its pending entries of `type` `shell`. An entry without `type` counts when it carries `command`. A `Stop` without the array (Claude Code before 2.1.145) leaves the list alone.
- A `UserPromptSubmit` whose prompt is one or more `<task-notification>` blocks drops each `<task-id>` whose `<status>` is not `running`.

Subagent hooks report nothing: they target the child row, and the parent's `Stop` lists every shell. `SubagentStop.background_tasks` is parent-scoped and ignored.

### Background scripts

A root session's hooks report the code-mode scripts it runs through the `REPL` tool as a `BackgroundScriptReport` on the observation, keyed by the `REPL` call's `tool_use_id`, and the rollup folds them into `AgentState.background_scripts` ([model.md](./model.md#turn-endings-and-parked-turns)):

- A `PreToolUse` for `REPL` starts a script with the `tool_input` description. A repeated start for a known id keeps the first sighting.
- A `PostToolUse` for `REPL` finishes it. Claude sends it when the script settles, never at the dispatch receipt the model sees, and a script that outlives its turn sends it after that turn's `Stop`, under the same `tool_use_id`.
- A `Stop` whose `background_tasks` holds no pending entry of `type` `code` clears the list, the guard against a script that ended without a result hook. A `Stop` that still lists one leaves the list alone, since the task id matches no `tool_use_id`, and so does a `Stop` without the array.

The commands a script runs fire their own `Bash` hooks under `repl_`-prefixed ids, which report nothing here. A blocking-tool `PreToolUse` under a `repl_` id is an ask the script raised and now waits on: the observation sets `script_ask`, and the fold stamps every listed script's `held_by` with the ask's native key, since the payload names no parent call (Claude Code 2.1.332). A `PermissionRequest` carries no `tool_use_id`, so a permission prompt for a call inside a script holds no script. Subagent hooks, `SubagentStop` included, report nothing. RimZ decodes no failed-tool hook for Claude, so a script that throws is cleared by its `PostToolUse` if Claude sends one, else by the next `Stop`. The card shows a script only once it has run `BACKGROUND_SCRIPT_SHOWN_AFTER` (30 s) from first sight ([sidebar.md](../sidebar/sidebar.md#delegation-entries)). Any listed script lifts a resting card to `running` from first sight ([model.md](./model.md#sleeping)).

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

Rejecting any of the three with Escape fires neither `PostToolUse` nor `Stop` (Claude Code 2.1.205). The only evidence is the transcript's `[Request interrupted by user` marker, which the next statusline push or [mod report](#rimz-mod-reports) stamps as an interruption settle. The settle releases the ask, so the row leaves `rimz asks` and stops reserving pane input ([model.md](./model.md#waiting-and-asks)). The durable question record still closes on the next prompt: its `UserPromptSubmit` clears the wait and records that prompt as the answer.

Rejecting in the pane with typed feedback ends the same ask another way and fires no hook at all. Claude hands the feedback back to the model as that call's own `tool_result` and the turn runs on, so there is neither a `PostToolUse` (success only) nor an interruption marker. The proof arrives with the next keyed tool event or a clean parked `Stop`. While a keyed ask is open and a different call reports or the turn parks, `tool_call_resolved` reads the transcript tail for a `tool_result` under the open ask's `tool_use_id`. When it finds one, the hook path records that ask's answer edge before the incoming event, so a tool puts the row back to running and a parked `Stop` parks it ([model.md](./model.md#waiting-and-asks)). A rejection the model answers with text, and no further tool call, holds the ask until `Stop`, parked or not. A parked `Stop` whose bounded transcript tail no longer holds the result keeps the ask.

A REPL script can call `AskUserQuestion` itself. The nested call fires `PreToolUse` like a top-level one, keyed by its own `tool_use_id`, and the card can stay open after the agent ends its turn. Claude then lists the script in the `Stop` payload's `background_tasks`, so the stop parks, and a parked stop leaves a keyed ask open while the transcript holds no result for its call ([model.md](./model.md#waiting-and-asks)). The row stays `waiting`, and deliveries that respect asks hold, until the same-key `PostToolUse` or another closing edge.

### Install

Install merges into Claude's `settings.json` through the shared JSON-merge backend ([adapter.md](./adapter.md#hook-install)). The file resolves from the login env: `RIMZ_CLAUDE_SETTINGS` when set, then `settings.json` under the first entry of a comma-separated `CLAUDE_CONFIG_DIR`, then `$HOME/.claude/settings.json`. A named account's hooks therefore land in that account's home.

Install writes one block per event in `CLAUDE_HOOKS`, each marked `_rimz_managed`, with a 2-second timeout. Hooks append ingress and nudge the elected drainer, and reply-bearing hooks wait within that budget. Only `PermissionRequest` carries `_rimz_sync = true`, and an existing async marker on it is a hard install error. Install also wraps both statusline commands ([rich context](#rich-context) and [child enrichment](#child-statusline-and-cost)).

Install also installs the RimZ mod. `crates/rimz/src/agents/adapters/claude/install.rs::ClaudeManagedIntegration` writes the folder `crates/rimz/src/disk/paths.rs::claude_mod_dir_in` names (one for every account, resolved from the login env) before it touches `settings.json`, so a folder it cannot write fails the install with the settings unchanged. It writes only files whose bytes differ, since each rewrite prints a reload line in every open session. The settings change then carries one more managed value, the folder appended to `env.CLAUDE_CODE_PLUGIN_DIRS` (`crates/rimz/src/agents/managed_env_list.rs::ManagedEnvPath`, threaded through the `_with` methods of `ManagedJsonHookSpec`); an `env` or list of the wrong JSON type refuses at preview and install alike. Uninstall strips only that entry and keeps the folder, which machine-wide uninstall removes with the home. `installed` still reads the settings hooks alone, so a missing entry or mod file reads as drift (`outdated_files`), not as hooks missing; mod file drift counts only while the settings carry owned hooks. Claude writes `.claude-plugin/types/` into the folder at load, which the drift check ignores; in a scoped view the folder is read-only, and Claude 2.1.326 loads the mod from a read-only folder that has no `types/` yet, logging nothing about the write it skips. The mod carries `MOD_VERSION`, which the manifest and the module repeat and a unit test pins.

A managed policy can keep the mod from loading. `crates/rimz/src/agents/adapters/claude/mod_policy.rs::blocker` reads the managed settings files Claude reads (`managed-settings.json` under `/etc/claude-code` on Linux, `/Library/Application Support/ClaudeCode` on macOS, `C:\Program Files\ClaudeCode` on Windows, then each `managed-settings.d/*.json` in name order, a later file's value winning) for `allowManagedModsOnly`, `allowManagedHooksOnly` and `disableAllHooks` set to `true`, and answers one key with the file whose value stands: `disableAllHooks` ahead of `allowManagedHooksOnly` ahead of `allowManagedModsOnly`, so a policy that sets several is reported by a key that also turns the hooks off. A file it cannot read or parse, or a value other than `true`, blocks nothing, so a broken managed file never fails install or start; `disableSideloadFlags` is not read. Under a blocking key, install writes no mod folder and removes RimZ's own `CLAUDE_CODE_PLUGIN_DIRS` entry (`ManagedEnvPath::wanted` is `false`), leaving every other entry in the list, and the preview's consent line says it skips the mod and why (`crates/rimz/src/agents/mod.rs::ModPlan::Skipped`). Drift then counts a still-listed RimZ entry and never the folder, so a policy that appears after an install removes the entry at the next install and reads the folder as current; a policy that goes away lists the folder again and writes the mod at the next install. Uninstall strips the entry either way. Nothing refuses: the settings hooks still install, since a hook-only block is the administrator's to lift. The same reader runs at `rimz start` (`crates/rimz/src/room/judgment.rs::judge`), which warns with the key and the file, adds for the two hook keys that the policy also turns off RimZ's settings hooks, and saves the warning as a skipped setting named by the file; and in `rimz doctor`, which shows the policy on a blocked login's registered rows (under a hook-disabling key no session registers, and the saved start warning stands alone) in place of the [missing check-in and missing-signal flags](#rimz-mod-reports). The machine-wide file is read through the login env only so test builds can re-root it under `RIMZ_TEST_GROK_POLICY_ROOT`.

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
| fast preset | `fastMode: true` on, `fastMode: false` off, merged into the launch's one `--settings` ([settings merge](#settings-merge)) |
| native idle compaction | `idleCompaction: false` when RimZ covers the seat, unless launch settings already set it ([settings merge](#settings-merge)) |
| native auto-continue at a usage limit | `autoContinueAtUsageLimit: true` when `resume.auto_continue` is on, unless launch settings already set it ([settings merge](#settings-merge)); RimZ's rate-limit fallback waits a five-minute grace past the reset ([auto-continue](./providers.md#fire)) |
| compaction command | `/compact <brief>` |
| Native delegation disabled | for a `rimz subagents` child, or a launch with [callable `rimz-subagents` or the served `subagent` tool](../harness/fleet.md#launch-reminders) and an available catalog, `--disallowedTools` gains `Agent` in one final occurrence, even when `tools:` lists it |
| shared language servers configured | with the MCP `lsp` tool or callable `rimz-lsp`, `--disallowedTools` gains `LSP` ([native LSP denial](#native-lsp-denial)) |
| skills home | `<config home>/skills` |
| temp root | `CLAUDE_CODE_TMPDIR` set to the launch's `TMPDIR` (`temp_dir_env_keys`), so Claude's internal temp files land in the agent's temp unit on macOS too, where Claude ignores `TMPDIR` ([upstream](../../externals/agent-adapter/claude-reference.md#temp-root)). Under scoped isolation the value is `/tmp`. |

`claude --resume <session_id>` continues the conversation under the same session id, and only `--fork-session` copies it into a new one ([upstream](../../externals/agent-adapter/claude-reference.md#launch-flags)). A resumed Claude row therefore keeps its identity. The picker's `SessionStart` binds the selected session, which revives a closed recorded row or adopts the provisional launch identity.

Smart and idle compaction append the configured summary brief after `/compact `. RimZ types the brief as a second write, so Claude's paste handling cannot swallow the slash command.

### Settings merge

Claude honours only the last `--settings` on its command line, so every RimZ addition merges into one settings document. [`merge_settings`](../../../crates/rimz/src/agents/launch_settings.rs), shared with Antigravity, reads the profile's last `--settings` value (inline JSON, or a file path resolved against the cwd, both parsed as JSONC), applies one step's change, removes every `--settings` occurrence, and appends one. Arrays it extends gain only new entries, and profile entries stay first. The process compiler in `crates/rimz/src/harness/launch.rs` runs the steps in this order:

1. Host skill list. `skillOverrides` marks each unlisted skill directory `user-invocable-only` ([host mode](../sandbox.md#host-mode)).
2. Listed skill permissions. Each name in the profile's `skills` list adds an exact `Skill(<directory name>)` rule to `permissions.allow`. A name containing `*` gets no rule, and an empty set leaves argv untouched.
3. Definition permission rules. A definition's validated `allowed-tools` rules join `permissions.allow`. These may carry wildcards.
4. Routine RimZ permissions. Skipped when [`agents.allow-routine-rimz`](../../guide/configuration.md#routine-rimz-commands) is off (the machine-only switch defaults on). `permissions.allow` gains a `Bash(<prefix> *)` rule per routine RimZ command prefix, an exact `Skill(<name>)` rule per installed `rimz-*` skill, and `mcp__rimz__*` when the launch hands over [RimZ's MCP server](../../reference/cli/mcp.md#launch-registration). `sandbox.excludedCommands` gains the same prefixes. `permissions.additionalDirectories` gains the tmp and shared paths. `autoMode.environment` gains `$defaults` once, beside prose that names the routine commands and this launch's `$TMPDIR` and `$RIMZ_SHARED` paths.
5. Notes. The canonical directories of the launch's notes runs (a workflow run's `automation-notes/` folder and the checkout's team archive run for this launch's team, each when present) join `permissions.additionalDirectories`, then the canonical home-wide note store (`crates/rimz/src/disk/paths.rs::notes_dir`) when it exists. A missing store is skipped, not created.
6. Added directories. The launch's `add_dirs` joins `permissions.additionalDirectories`, independently of routine permissions. This grants file access without folder trust or loading that directory's configuration; Claude's native added-directory argv is never used.
7. Command sandbox. `sandbox.filesystem.allowWrite` gains the room's state, runtime, and shared roots at every isolation level. Scoped isolation adds `enableWeakerNestedSandbox: true`, and `sandbox` isolation adds `enabled: true` and nothing else.
8. Native idle compaction. Insert `idleCompaction: false` only if absent and RimZ covers the seat: a token floor resolved from the role, profile chain, then harness (never a launched child), and a configured prompt-cache lifetime above the margin for unknown, subscription, and API-key sign-in alike. Launch compilation has no live cache-write evidence or known sign-in. Effective project tables win over machine tables; runtime compaction shares the same floor resolver. A configuration change takes effect at the next launch or restart, not in an already running session.
9. Native auto-continue. Insert `autoContinueAtUsageLimit: true` only if absent, when the machine's `resume.auto_continue` is on, so Claude's own resume opens the turn after a usage limit and RimZ's auto-continue is the fallback. The adapter declares a five-minute grace (`crates/rimz/src/agents/capabilities.rs::LaunchCapability::native_auto_continue_grace`), and RimZ's rate-limit fallback fires only that long past the reset, when no turn has started. Every pane launch reaches it: fresh launches, team seats and restore seats, resumes, forks, restarts, molts, supervised pane children, and workflow judges. The process compile a supervised launch runs before its pane exists (`crates/rimz/src/harness/launch.rs::compile_managed_agent_process`) uses default reminders and does not. The key is read from the Claude Code 2.1.329 binary, not a published setting ([upstream](../../externals/agent-adapter/claude-reference.md#native-auto-continue-at-a-usage-limit)). Like idle compaction, it takes effect at the next launch or restart.
10. Fast mode. The fast preset rides outside the document until every other step has run, then merges `fastMode: true`, or `fastMode: false` for forced off, so a saved `/fast` stays off.

Steps 2 to 7 apply to every permission mode, every isolation level, and child, resume, and fork launches alike. A preflight compile has no room roots and skips routine RimZ permissions, notes, command sandbox settings, native idle compaction, and native auto-continue (steps 4, 5, 7, 8, and 9); it still merges added directories. Other user settings, other `autoMode` classifier arrays, and other sandbox keys survive the merge. RimZ checks no sandbox dependency, so Claude's own behaviour applies when its sandbox cannot start ([upstream contract and probes](../../externals/agent-adapter/claude-reference.md#per-launch-permissions-and-auto-mode)).

Routine skill names come from the provider skill root (RimZ-owned links skipped), then the RimZ library, then installed packs, through `crates/rimz/src/agents/skills.rs::enumerate`: only `rimz-*` directories that hold a `SKILL.md`, sorted by directory name. Higher tiers win; conflicting pack copies contribute no rule. The tmp path is the one the agent sees (`/tmp` in the scoped view, the temp unit's host path otherwise). The shared path is the room's host `shared/` at every level. `crates/rimz/src/agents/capabilities.rs::LaunchCapability::allow_routine_rimz_args` is also implemented by Antigravity ([adapter_antigravity.md](./adapter_antigravity.md#permission-rules-sandbox-and-added-dirs)) and is a no-op for other providers.

The merged document lands in one of two places. A profile settings file becomes a private content-addressed `settings.<sha256>.json` artifact under the runtime prompt directory. The host-skills step also makes inline profile settings private. Everything else stays inline JSON on argv. A later step reads the pending artifact's value, since the file does not exist yet, and re-digests it. Compilation writes nothing, and apply publishes the one artifact at every isolation level. Reapplying the merge is idempotent.

Invalid JSON, a non-object settings value, or a wrongly typed key a step extends refuses the launch before exec. The error names the source and the key, and each step adds its own fix:

| Step | Fix the error offers |
| --- | --- |
| listed skill permissions | `correct that key, or remove the profile's skills list` |
| definition permission rules | `correct that key, or remove the definition's allowed-tools list` |
| routine RimZ permissions | `correct that key, or set allow-routine-rimz = false`. An unreadable skill root names that root and offers fixing access or turning the switch off. |
| host skill list, notes, command sandbox, native idle compaction, fast mode | none beyond the source and key |

### System prompt

RimZ puts its merged launch reminder (team context, model identity, and delegation guidance) in `--append-system-prompt`. The flag is not a typed preset, so a user who wants provider-specific append text puts it in raw profile `args`.

On resume or fork, RimZ compares the configured system-prompt fingerprint with the fresh launch's baseline. The fingerprint is a lowercase SHA-256 over the composed replacement prompt and any native system-prompt text or file contents in profile args, never RimZ's per-launch reminder. An equal baseline keeps Claude's stored snapshot. A missing or different baseline flags the launch's [session step](../harness/fleet.md#the-launch-plan), which adds `--system-prompt-snapshot off` only when the program it resolved reports version 2.1.257 or later.

The step reads the version of the program it resolved on the pane's `PATH`, after the launch shell's startup files, never the `claude` on the launching process's `PATH`. An unknown or older version adds nothing. Fresh launches never add the switch. Resume and fork never replace the baseline, so later resumes keep rebuilding while the fingerprint differs, and a fork's own later resumes have no baseline. Editing a prompt file at the same path changes the fingerprint.

### Native LSP denial

Effective shared-language-server configuration calls `disable_native_lsp_args` only when the launch hands over the `lsp` tool of RimZ's MCP server or its tools and skills make `rimz-lsp` callable, and it merges `LSP` into one `--disallowedTools` flag before native delegation denial adds `Agent`. The denial follows configuration even when optional admission is refused, and profile `--tools` allowlists stay unchanged. Without either, RimZ leaves the profile's flags alone, a user-configured `LSP` denial included. The [pinned precedence probe](../../externals/agent-adapter/claude-reference.md#native-lsp-denial) shows why this keeps Claude from starting private servers. Server lifecycle, callability, and reminders belong to [shared language servers](../lsp.md).

### Folder trust

[`folder_trust`](../../../crates/rimz/src/agents/adapters/claude/folder_trust.rs) models the [upstream folder-trust decision](../../externals/agent-adapter/claude-reference.md#folder-trust). It resolves `.claude.json` through the same login-aware path as remote-control consent. When a main repository root is supplied, it is the trust key, which covers a linked worktree. Outside Git, the cwd and its ancestors are checked. Both canonical and as-written paths match. Only `hasTrustDialogAccepted: true` decides, and `false` remains an offer to grant.

The grant key is the canonical main root or cwd. Claude never persists a home-directory grant, so that key returns a gap without a writable preview. Missing homes and unreadable, malformed, or non-object JSON also produce actionable gaps, so none of them appears decided.

The preview sets `projects.<key>.hasTrustDialogAccepted` through `json_edit.rs`. A borrowed JSON parser locates the edited value, so every unrelated byte and the key order survive. The preview is accepted only when it parses back to the original value with exactly that leaf changed. The neutral [`grant_folder_trust`](../../../crates/rimz/src/agents/folder_trust.rs) writer rejects bytes changed since the preview and publishes through the symlink-following provider-file writer. A provider write between that freshness check and the replacement is not serialized by this API.

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

Child rows get their type, model, effort, description and start time from two feeds, each writing the same per-child sidecar the sidebar folds onto the child row ([sidebar.md](../sidebar/sidebar.md)): the [RimZ mod](#rimz-mod-reports) and the child status line. Once the mod has reported a child, the mod wins: `crates/rimz/src/store/subagent_context.rs::update` drops a later status-line push for that child (`SubagentReporter`), so a sparse push cannot erase what the mod wrote. Until then, as on a Claude that does not load the mod, the status line writes alone.

Install wraps Claude's `subagentStatusLine` with `rimz statusline feed --source claude --subagent`. [`subagent_statusline.rs`](../../../crates/rimz/src/agents/adapters/claude/subagent_statusline.rs) reads each entry of the `tasks` array (`id`, `agentType`, `model`, `effort`, `description`, `startTime`) into the child's sidecar. The task kind in `type` is ignored and never serves as an agent-type fallback. The statusline model and effort paint a running child until lifecycle metadata supplies its own. Once the child transcript names a request model, that model replaces the statusline's.

Both feeds price the child. [`subagent_cost.rs`](../../../crates/rimz/src/agents/adapters/claude/subagent_cost.rs) derives `subagents/agent-<id>.jsonl` from the parent's `transcript_path` and advances a byte cursor through the child's usage records, pricing each request under its own model and cache tier. The cursor keeps the cumulative cost, the newest child model, and the last request key, so a contiguous richer duplicate replaces its earlier record. A request whose model is missing from the price book marks the cursor unpriced, and the figure is withheld instead of shown as a partial sum. The cursor records the price book and managed-settings fingerprints and replays the whole child transcript once when either changes. The figure is display-only, because the parent session's spend already includes its children.

### RimZ mod reports

The mod ([install](#install)) only observes. Each hook in `rimz_mod/rimz.ts` lets `next(e)` finish first and returns what it gave unchanged; a report that fails, throws, or outlasts its 2-second cap is dropped. Every hook but `turn.step` awaits its report before it returns, and only those reports carry the cap. Claude loads the module only when each hook is passed to `on` inline or is a function or const declared at the top of the module file: a hook of any other form fails the whole module's load, and the session runs without the mod. The `turn.step` hook forwards the stream with `yield* next(e)`, reads the envelope, starts `rimz` with `$.process.spawn` without awaiting it, and returns the result, so the next model request never waits on `rimz`; the report drains the child to its end after the hook returned. It reports at these points:

| Mod event | When it reports | `agent_id` |
| --- | --- | --- |
| `session.start` | after the session starts (the check-in) | absent |
| `agent.spawn` | once `next(e)` resolves the spawned child | the `agentId` `next` resolved |
| `tool.call` | after every tool call, the main loop's included | the event's `agentId`, absent for the main loop |
| `turn.complete` | at the end of a child's turn and of the main loop's | the event's `agentId`, absent for the main loop |
| `turn.step` | after each main-loop model request whose result carries usage; a child's request and a request without usage send nothing | absent |

Each report runs `rimz mod feed --source claude` (argv, no shell; `RIMZ_BIN` when set) with one JSON envelope on stdin: `v` (the mod version), `event`, `session_id`, and, when the mod could read them, `transcript_path`, `claude_version`, `agent_id`, `agents`, which is `$.agent.list()` as returned, `usage`, which is `$.session.usage()` as returned, on `turn.complete` the event's `reason`, and on `turn.step` `step`, which is the request's `result.usage` as returned. An absent field is omitted, never sent as null. The feed (`crates/rimz/src/cli/statusline.rs::run_mod`) never forwards to a wrapped status line and never prints. It exits 0 on every input; a failure logs to stderr. `crates/rimz/src/agents/adapters/claude/rimz_mod.rs::observe` shapes the data and refuses, before the room is resolved, an envelope it cannot act on: malformed JSON, an unknown event, an empty `session_id` or `v`. Unknown fields are ignored. Each `agents` entry maps onto the child sidecar fields (`type` as the agent type, `effort` falling back to `lastRequest.effort`, `startTime` in epoch milliseconds). Claude types an effort as a level name or a number, and a number is stored as its decimal text, which the card paints as written. A field of any other shape drops alone, and only an entry without a string `id` drops whole. A listed child gets its sidecar written and its cost cursor advanced from `subagents/agent-<id>.jsonl` beside the envelope's `transcript_path`, as the status-line feed does ([child statusline](#child-statusline-and-cost)). An entry stays listed after its child ends, and a child the list no longer carries keeps its sidecar as it was.

The `usage` fields become the session's own readings, which `crates/rimz/src/store/agent_context.rs::merge_mod_reading` merges into its context sidecar ([rich context](#rich-context)):

| `usage` field | Internal |
| --- | --- |
| `context.tokens` | context tokens, the gauge numerator |
| `context.window` | `context_window_size` |
| `context.percent` | `used_percentage` |
| `rateLimits[]`: `kind`, `percentUsed`, `resetsAt` | the five-hour, weekly and spend windows the statusline's `rate_limits` give; `resetsAt` is ISO text, the same instant as the statusline's epoch seconds |
| `cost.usd` | `total_cost_usd`, which only rises |

The envelope's `claude_version` becomes `agent_version`, on every report. A `turn.step` report's `step` gives the readings of that one response:

| `step` field | Internal |
| --- | --- |
| `model` | `model_id`, the id the API reports |
| `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens` | the `current_usage` split |
| `input_tokens` + `cache_read_input_tokens` + `cache_creation_input_tokens` | context tokens, in place of `context.tokens` |

The split needs all four counts and one above zero; otherwise it drops, and the count comes from `usage` as on any report. An empty model drops alone. Taking the count from the split keeps the two on one response, the pairing [`AgentCard::call_split`](../../../crates/rimz/src/store/snapshot/row.rs) checks. Any other report takes its model, when it carries none, from the newest request in the transcript tail the scan reads, and takes that request's split only when its input side equals the report's own count; a `turn.step` report never reads either from the tail.

The figures are the engine's own, the ones the statusline shows (Claude Code 2.1.327). The first windows a session reports take the owner-start floor of the `claude` process, which is the feed's parent. A field of another shape drops alone, and a report without `usage` still checks in. A report that names a transcript also runs the [turn-death](#turn-death-marker), [denial](#turn-ending-denial-marker) and [interruption](#turn-interruption-marker) scans over its root tail, and reads fast mode from it. When the report is a main-loop `turn.complete` with `reason: "aborted"` and the tail holds no interruption, the feed reads it again, at most five more times 100 ms apart, and stops at the first marker. A marker lands only when its transcript stamp is newer than the stored marker of its kind, so a stale tail never moves one backwards. A transcript the feed cannot read keeps the readings.

Every envelope also upserts the session's check-in (`crates/rimz/src/store/mod_checkin.rs::record`): one record per `(kind, session id)` under the runtime `live/mod_checkin/` directory, holding the mod version, the Claude version, the first and the last report time, and when the mod last sent each signal: readings (a report whose `usage` carried a context window) and a turn report (a `tool.call` or `turn.complete` report whose transcript the scan read). A session whose start report was missed checks in at its next report. `rimz doctor` flags a live, registered Claude session with no check-in (`crates/rimz/src/cli/doctor/agents.rs::LiveSessions`), a child launched through `rimz subagents` included, as a warning whose fix reinstalls into the session's own login (`rimz hooks install claude` for the default home, `rimz accounts add claude <name>` for a named account) and restarts the session, naming an organization policy and a `CLAUDE_CODE_PLUGIN_DIRS` in Claude's own process environment as what else keeps the mod from loading. A login whose managed settings set a [blocking key](#install) is never flagged for either finding; its rows show the key and file instead, as information. A provider-native child (it runs inside its parent's session), a provisional launch card that never registered, an ended session and another kind are never flagged, and one session's check-in never moves another's finding. A session that started before the hooks were installed keeps the finding until it restarts, and so does one on a Claude without mod support. A checked-in session whose row has finished a turn since its check-in without a readings or a turn-report stamp is flagged once per missing signal, with a fix that reinstalls and restarts to load the current mod. The mod is never the lifecycle record: the settings hooks are.

Plan approval (`ExitPlanMode`) and the question picker (`AskUserQuestion`) behave in the mod as a Bash permission prompt does, probed on Claude 2.1.326: `tool.call` brackets the dialog, a rejection resolves `next(e)` with the "user doesn't want to proceed" error text, `turn.complete` follows with `reason: "answer"`, and no `session.stop` fires.

### Without the mod

A session whose mod never loads (an `allowManagedModsOnly` [policy](#install), a Claude without mod support, a session started before the install) still gets every reading and marker from its settings hooks and the stat-gated local refresh; it trades only latency. Under `allowManagedHooksOnly` or `disableAllHooks` the settings hooks do not run either, so the session never registers and RimZ reads nothing from it; the saved start warning is what `rimz doctor` shows.

| Reading | Lands at |
| --- | --- |
| context tokens | the next hook, from its transcript read |
| rate-limit windows | the next [OAuth usage probe](#oauth-usage-probe), within its 5 minutes |
| cost | the turn close (`Stop`), which prices the whole transcript |
| fast mode | the next hook-time transcript read of the newest request's `usage.speed` |
| interruption (Esc), turn death, turn-ending denial | the next watch or heavy-lane tick refresh after Claude writes the row |

The markers come from `crates/rimz/src/agents/adapters/claude/local_context.rs::refresh`, which runs the same resting-turn scan as the statusline and the mod over the bounded tail each time the transcript stat changes, and returns nothing when it has not (the `observed_at` rule in [`crates/rimz/src/agents/AGENTS.md`](../../../crates/rimz/src/agents/AGENTS.md)). Its patch sets `LocalContextPatch::markers_advance`, so a scanned marker lands only when its transcript stamp is newer than the stored marker of its kind: a `StopFailure` marker stamped at decode time is never moved back by a tail that still holds the same error.

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

A finished compaction writes a `{"type":"system","subtype":"compact_boundary","compactMetadata":{…}}` record, and no usage-bearing record follows it until the next API call. This shape is observed in transcripts on disk, not documented upstream. Every usage record older than the boundary was measured before the compaction, so a boundary met first in the newest-first walk ends it. The reading is then `total_tokens = compactMetadata.postTokens` with no split, or no reading at all when `postTokens` is absent. A usage record newer than the boundary wins. When the 64 KiB tail holds neither record and did not reach byte zero, Claude retries once with a 1 MiB budget. The compaction hooks (`PreCompact`, `SessionStart:compact`, and `PostCompact`) all fire before the boundary is written, so their reads can still return the old record, which the rollup [withholds by value](./model.md#the-compaction-bracket). After the boundary lands, the stat-gated local refresh (on hooks or heavy-lane ticks) publishes its size as `post_compaction_tokens` in the sidecar until a usage record follows it, filling the gap before the next prompt without counting the estimate as measured tokens.

### Context window

The transcript writes only the bare model id, so the gauge's divisor comes from elsewhere. A `[1m]` marker on the hook payload's model sets 1,000,000 tokens. Otherwise `context_window_for_model` uses the model's exact `max_input_tokens` from the shared price book, and an unknown model leaves the window unset so the spec default of 200,000 applies. A marker-less hook never lowers an established window. The `context_window_size` the statusline or the [mod](#rimz-mod-reports) reports comes first in projection precedence, so neither inferred value overrides a provider-reported window.

### Session-cumulative usage

`local_context_refresh` folds each assistant API response once into session-cumulative input, output, cache-write, and cache-read counters ([`local_context.rs`](../../../crates/rimz/src/agents/adapters/claude/local_context.rs)). Claude writes one usage row per content block, and a row can expand into main-model and advisor-model usage, so `LocalSpendFold` keeps the last keyed response group and skips or replaces its contiguous duplicates. Among hooks, only `SessionStart`, `UserPromptSubmit`, `PostToolUse`, and `Stop` trigger the refresh.

The byte cursor, duplicate window, and sums are stored together, so the fold's steady refreshes parse only appended lines. A truncated file restarts the fold, and a stored fold that lacks the duplicate window replays once. This is why Claude keeps its own unchanged-source gate ([adapter.md](./adapter.md#context-sources)). The transcript path resolves in order: the hook's `transcript_path`, the stored path, then the newest `<config>/projects/*/<session_id>.jsonl`. The result updates `AgentTokenUsage.session_usage` and leaves the current-window fields and cost to the statusline and the [mod](#rimz-mod-reports). Each pass that clears the gate also reads the bounded tail, 64 KiB with one retry at 1 MiB, through the same walk the hooks use: a newest `compact_boundary` with `postTokens` sets the sidecar's `post_compaction_tokens`, a newer usage record, a boundary without a size, or a whole file holding neither clears it, and a bounded read that finds neither, or a failed read, keeps it.

The same typed spend pass and byte cursor capture the newest main-chain cache write's retention. A 1h-only split is one hour; any 5m tokens, including a mixed split, mean five minutes. A write with no split is `Unsplit` and uses the settings chain rather than pricing's 5m assumption. Read-only requests, sidechain writes, and advisor iterations do not change it. An appended tail with no write keeps the observation; a new transcript path or a rewrite with no write clears it. Statusline refreshes preserve it ([lifetime resolution](../../guide/configuration.md#idle-compaction)). The local context refresh also reads the transcript's `api-request` entries (Claude Code 2.1.300 and later) whose `querySource` is `repl_main_thread` and publishes the latest `timestamp` as `AgentContext.recorded_request_at`: the latest by time, not by position, since a retry writes a second entry and an entry can land after the input it answers. Claude writes an entry for a request it then refuses for a usage limit, followed by an `isApiErrorMessage` entry: an entry answered by a main-conversation error entry of a limit class (`statusline::classify_api_error`, `paused_rate_limit` or `paused_spend_limit`) is no request, so the published value moves back to the entry before it, or clears when there was none. An overload or other error class answers a request that ran, so its entry still counts. Any other main-conversation entry settles the unanswered entry when it is stamped at or after it, or carries no readable stamp: Claude can append the next request's entry before the answer to the request before it, and that answer leaves the newer entry for its own error to take back. A new entry settles the one before it, since the main conversation sends one request at a time. The byte cursor's state carries the newest unanswered entry and the latest before it, so an error entry read by a later refresh than its request entry still takes it back; a rewrite or a new transcript path starts that state over with the whole file. It keeps and clears on the same rules as the write retention. The fleet spend walk skips these entries.

### Conversation parser

`parse_transcript_messages` (`statusline::parse_messages`) is the one conversation parser for Claude transcripts. It keeps `user` and `assistant` entries with their timestamps and drops sidechain replay, `isMeta` user entries, and API-error assistant entries. Supervised streaming uses the default `stream_assistant_messages`, which filters that parse.

### Rich context

Claude runs its configured `statusLine` command on every render and pipes it a JSON blob ([schema](../../externals/agent-adapter/claude-reference.md#statusline-json)). Install points `statusLine` at `rimz statusline feed --source claude`, and [`observe_context`](../../../crates/rimz/src/agents/adapters/claude/statusline.rs) parses the blob into `AgentContext` and runs the turn-death and turn-interruption scans over the transcript tail. `fast_mode` supplies the live `AgentContext.fast` reading, including explicit off. A payload that omits it leaves the badge on its launch fallback. Between pushes, the newest main-loop request's `usage.speed` in the transcript sets the same reading (`fast` is on, any other speed off) at each hook-time local refresh and each mod report. The tests in `statusline/tests.rs` pin the field shapes.

Two sources write the root session's sidecar. A statusline push is a whole-record write: it sets every field its payload carries and clears what it omits. A [mod report](#rimz-mod-reports) is a sparse merge: it sets what it carries and keeps what it does not (effort above all), with cost only rising. Fast mode and the markers come from the report's transcript scan, and a marker lands only when it is newer than the stored one of its kind.

The mod wins four readings, each from the first report that carries it, recorded per session on the sidecar record (`ModReadings` in [`record.rs`](../../../crates/rimz/src/agents/context/record.rs)): the model, the version, the token split, and the token count. From then on a statusline push leaves each as stored, whether its payload carries a value or omits it; it still writes every other field. A report that carries the model also clears the statusline's `model.display_name`, so the card labels the model from the id ([`model_label_source`](../../../crates/rimz/src/agents/model_display.rs)) and a model switch the mod reported is never relabelled with the old name. A reading the mod has never carried, as with a 1.1.0 mod that sends a count and no split, stays the statusline's. A report whose `usage` carries a window and no count follows a compaction and clears the stored count and split, so neither the mod nor a later push brings back the retired window; a report without `usage` clears nothing.

Only `session_name` reaches the session's context from the statusline payload alone: no mod API returns it. It feeds the card description (`select_activity_description` in [`state.rs`](../../../crates/rimz/src/agents/state.rs)). `agent_version` reaches the provider header through the account record, where it competes with the binary probe ([the account record](./providers.md#the-account-record)). The `current_usage` split feeds [`AgentCard::call_split`](../../../crates/rimz/src/store/snapshot/row.rs), where the lifecycle split stands in only when no token count is stored or its cache and fresh input sum to it.

A user who already has a `statusLine` keeps it. Install wraps the original command, which receives the same JSON, and RimZ forwards its stdout and exit code, so the rendered line is unchanged. The original is stored verbatim under `_rimz_wrapped` and restored on uninstall. The wrap is a security surface: the consent prompt summarizes both statusline wraps, the install diff shows them in full, and the wrapped child's stdio is fully piped.

The statusline's `cost.total_cost_usd` covers only the live process and reads `0` after a resume. On a turn-ending hook, `supplement_realtime_cost` prices the whole session transcript and replaces the card cost only when that total is higher.

### Turn-death marker

The transcript tail is the backstop for a provider error that `StopFailure` did not report. [`detect_turn_error`](../../../crates/rimz/src/agents/adapters/claude/statusline.rs) runs on each statusline push, each [mod report](#rimz-mod-reports), each [local refresh](#without-the-mod) whose transcript stat changed, and on `Stop`. It scans newest-first and skips sidechain entries, entries other than `assistant` and `user`, and entries without a parseable `timestamp`. The first entry left decides. An `assistant` entry flagged `isApiErrorMessage: true` emits `AgentContext.turn_error` with its timestamp, a label capped at 200 characters, and a class from `classify_api_error` over the entry's `error`, `apiErrorStatus`, and label. A `user` entry flagged `toolDenialEndsTurn: true` emits the [turn-ending denial marker](#turn-ending-denial-marker). Any other deciding entry means the turn is alive or recovered.

Every statusline push and mod report re-derives the marker, so `StopFailure` shares the classifier, or the tail would overwrite the hook's class. `classify_api_error` checks in this order:

| Evidence | Class |
| --- | --- |
| a label the shared [`TurnErrorClass::classify_label`](../../../crates/rimz/src/agents/context.rs) reads as a spend limit (Claude tags spend caps `rate_limit` and 429 too) | `paused_spend_limit` |
| `error` of `oauth_org_not_allowed` or `billing_error` | `not_entitled`: an actionable failed row with the upstream label, never a resumable park |
| `error: "rate_limit"` or `apiErrorStatus: 429`, whatever the text (a model cap such as "You've reached your Fable limit") | `paused_rate_limit` |
| `error` of `overloaded` or `server_error`, whatever the text | `paused_overloaded` |
| `unknown`/`invalid_request` or a label-only error matching the shared `classify_label` rule for `reached your specified[\w\s-]*?usage limits`, case-insensitive ([native error bodies](../../externals/agent-adapter/claude-reference.md#transcript-death-certificate)) | `failed`: an actionable turn, never a resumable park or a plan lapse |
| anything else | `classify_label` over the label: rate-limit text or HTTP 429 pauses on the rate limit, transient server or connection text or HTTP 5xx pauses on backoff, and anything else fails |

The tail-scan marker changes display only: [model.md](./model.md#displayed-status) pauses or fails the row, and the rollup does not change. A `StopFailure` classed by this table also ends the turn errored ([turn endings](#turn-endings)) ([upstream shape](../../externals/agent-adapter/claude-reference.md#transcript-death-certificate)).

When the hook lifecycle merges a changed `not_entitled` marker, it marks the recorded agent login's [entitlement](./providers.md#entitlement) lapsed at first observation and makes its direct usage read due. An unmetered API-key, key-helper, or Console login still records that lapse, but neither the sidebar nor `rimz accounts` projects it; its failed turn keeps Claude's own error message. Replayed drains skip this side effect, and a missing agent row warns instead of guessing a login. The statusline remains a reader. The transcript backstop reaches this producer through the ordinary hook merge.

### Turn-ending denial marker

In auto mode Claude can refuse a tool call and end the turn with it, and fire no `Stop` or `StopFailure` ([upstream shape](../../externals/agent-adapter/claude-reference.md#turn-ending-tool-denial)). When the deciding entry of the same scan is a `user` tool result with top-level `toolDenialEndsTurn: true`, the scan walks back the trailing run of such flagged entries, up to the first conversation entry without the flag, and reads every `permissionDecision` in it: Claude also flags a sibling call's result written after the abort and stamps it with that call's own decision, so the newest flagged entry need not be the refusal. A run holding an entry whose `permissionDecision.reasonCode` is `classifier:auto-mode:no-verdict` (the safety check returned no verdict, an outage that clears by itself) emits a `paused_overloaded` marker, so [auto-continue](../../guide/loops.md#auto-continue) retries it where it is on. Any other run emits `failed`. The marker's timestamp is the newest flagged entry's, so a sibling's later hook does not outrun it. Its label is the tool-result text of the no-verdict entry, else of the run's oldest entry whose `permissionDecision.decision` is `reject` (the refusal that caused the abort), else of the newest entry, capped at 200 characters. `ended_turn` stays `false`, as on the API-error marker: the row stays raw `running`, and the marker changes display only. Neither `toolDenialKind` nor the message text decides: a denial that does not end the turn, such as an ordinary `automode-unavailable` refusal with `reasonCode` `classifier:auto-mode:unavailable`, emits nothing. A later prompt becomes the deciding entry and clears the marker, as newer hook activity does. Like the interruption marker below, it lands at the next statusline run, mod report or local refresh.

### Turn-interruption marker

Esc fires no root lifecycle hook. Claude appends a timestamped `user` entry whose text begins `[Request interrupted by user` (the `for tool use` variant included). When the same scan finds that entry first, `observe_context` stamps `AgentContext.settle` as `Interrupted`. A marker newer than `last_activity` displays a falsely `running` row, or a waiting row whose ask Esc cancelled, as `idle`, and any newer hook activity clears it. The root marker changes display and delivery gates, and opens a `rimz subagents` child's [interrupt hold](../harness/scripting.md#interrupted-subagent-turns) when the room host carries it to the run; the durable rollup keeps the reported status. Escape itself does not run the statusline, so the marker reaches the sidecar at the first of three runs: the mod's `turn.complete` report, which reads the tail again for up to half a second while the row is missing ([RimZ mod reports](#rimz-mod-reports)), the next statusline run, at the user's `statusLine.refreshInterval` when one is set, else at Claude's next render trigger, and the next watch or tick [local refresh](#without-the-mod) after the row is written. Child markers drive the [interrupted-child repair](#interrupted-children).

## Local session discovery

[`local_sessions.rs`](../../../crates/rimz/src/agents/adapters/claude/local_sessions.rs) turns Claude's project store into identity-only [local session observations](./instances.md#local-session-observations). Hooks stay authoritative for lifecycle state, prompts, waits, asks, compaction, context, and clocks on a session with a durable row.

The full scan, per-pane discovery, conversation-presence check, context refresh's transcript lookup by session id, bridge-pointer probe, and spending all resolve roots through `crates/rimz/src/agents/adapters/claude/spend.rs::claude_config_roots`. Non-empty `CLAUDE_CONFIG_DIR` entries (comma-separated) supply the only roots, even when none exists or has `projects/`. When it yields no entries (unset, or only commas and blanks), the roots are `$XDG_CONFIG_HOME/claude` (else `$HOME/.config/claude`) and `$HOME/.claude`.

Claude declares `local_session_discovery = PerPane`. For each admitted workspace the room host looks in `projects/<bucket>`. It tries the bucket named by a non-empty `CLAUDE_CODE_PROJECT_DIR_NAME` first, then the absolute workspace path with every non-alphanumeric byte replaced by `-`. Both bucket names are only an index. A candidate is accepted when its first record carrying both `cwd` and `timestamp`, within the first 32 lines and 64 KiB, names the exact workspace, which also rejects collisions on a shared config root.

| Rule | Value |
| --- | --- |
| candidate files | top-level `*.jsonl` whose stem is a UUID; the stem is the `claude --resume` id |
| records allowed before the first session record | metadata such as `mode`, `permission-mode`, `bridge-session`, `file-history-snapshot` |
| first session record | must not set `isSidechain` |
| activity bound | newest tail-record timestamp, else file mtime |
| cap | named sessions uncapped; the newest 512 window sessions by creation per admitted workspace |

The room host computes `<config-root>/projects/<bucket>/<id>.jsonl` for every named session. Among duplicates across roots it keeps the newest activity. Finding a pane's own session closes that pane's fresh window. Otherwise the catalog retains top-level sessions created since the demand's clock; directory changes admit new files on the next pass, and a cold cache reconstructs the same window. Warm passes stat only named files, window candidates and watched folders; they list no folders and read no transcripts. The per-pane cache is keyed by account roots and the project-directory override, not the workspace set. Learned heads survive window closes and workspace changes; they are pruned only when a fresh listing of their folder no longer contains the file, or the account cache expires after 30 seconds unused. Rejected heads stay distinct from unfinished heads so a shared bucket's unrelated history adds no warm metadata work. An unfinished head is rechecked every pass only while its file was last written at or after the window clock, and otherwise when a directory change or the backstop reruns the listing, so an abandoned headless file adds none either. A 30-second backstop revalidates the catalog. `rimz agents resume` retains the recursive full scan and its newest-512 activity cap per workspace.

## Account and balance

The account model these fields fill is [providers.md](./providers.md), and the per-provider cadences are in [providers.md](./providers.md#the-out-of-band-probe).

| Source | Read by | Produces |
| --- | --- | --- |
| [`claude auth status`](../../externals/agent-adapter/claude-reference.md#auth-surface) | `account::probe`, with null stdin and piped stdout and stderr | `plan` from `subscriptionType`, with `max` refined from the same login's credential metadata (below); `metered` false for `api_key`, legacy `apiKey`, `api_key_helper`, or an `oauth_token` positively identified as Console (below). Another non-empty method is metered; absent or blank is unknown. The CLI exits 1 when logged out and still prints `"loggedIn": false`: that is `LoggedOut`, and any other non-zero exit is `Unavailable` |
| `claude --version` | the shared display-only version probe | the account's version field, also filled into entries whose login facts are fresh |
| statusline `rate_limits` | `observe_context` | 5h and 7d windows plus the gateway's durationless `spend` scope (label `$`), from epoch-second resets, marked `BestEffort`; spend percentage only, no dollar amounts or period |
| OAuth usage endpoint | [`oauth_usage.rs`](../../../crates/rimz/src/agents/adapters/claude/oauth_usage.rs) | the same windows from RFC 3339 resets, marked `Authoritative`; a lifted 5h when `five_hour` is absent or null and `seven_day` yields a window; `extra_usage` cents as `ExtraCredits::Known { used_usd, limit_usd }` or `Disabled`; model sub-caps; `cedar_ember` as `ResetCredits` |
| OAuth profile endpoint | `AccountCapability::probe_account_profile` in the same reader | recognised plan and optional display name only when the profile's owner matches the fresh auth-status fingerprint; no entitlement, usage, or metering changes |

The lifted-5h rule uses response shape, never plan names or `limits[]` session rows. A present `five_hour` object with 0% and no reset is a real window; one with both fields null remains unknown and is omitted, not lifted. A model sub-cap alone cannot establish the 7d window. The rule assumes an idle subscriber's unstarted 5h is a present object, not null; that idle shape remains unobserved ([usage observations](../../externals/agent-adapter/claude-reference.md#oauth-usage-endpoint)). The best-effort statusline never makes this inference.

Only `oauth_token` triggers Console classification. `account::parse_claude_auth` resolves the [profile store](../../externals/agent-adapter/claude-reference.md#auth-surface) from `login_env` and requires a usable `user_oauth` or `oidc_federation` config. For `user_oauth`, RimZ checks only credential-file metadata for a non-empty regular file, never its contents. A non-empty `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, or `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` in `login_env` or the login's settings `env` blocks Console classification. Missing settings have no blockers; unreadable or malformed settings keep the login metered. No network call is made, `ANTHROPIC_API_KEY` is not a blocker, and `claude.ai` stays metered even with a profile store present. Missing or unrecognised profile evidence also stays metered. The probe does not isolate the shared profile store per named account.

For a found `max` account, `account::probe` reads `claudeAiOauth.rateLimitTier` through the usage reader's account-home and Keychain resolution. `default_claude_max_5x` and `default_claude_max_20x` refine the plan to RimZ-normalized `max_5x` and `max_20x`, labelled `Claude Max 5x` and `Claude Max 20x`. A present credential `subscriptionType` other than `max` contradicts the refinement. Missing, unreadable, malformed, absent-tier, unknown-tier, or contradictory credentials leave `max` intact, and other account plans are untouched. This display-only read ignores token expiry and scopes, makes no profile request, and never changes the probe outcome or metering. The credential fields are observed in [Claude Code 2.1.306](../../externals/agent-adapter/claude-reference.md#credentials). They are cached provider metadata, not a fresh entitlement check.

The separate live profile read can override that saved plan. A `claude.ai` auth status with non-empty `email` and `orgId` carries an opaque SHA-256 fingerprint over the case-folded email and organization UUID. Neither raw value is kept, and Debug redacts the fingerprint. The profile must carry the same owner. `organization_type = "claude_pro"` maps to `pro`, and `claude_max` maps only with `default_claude_max_5x` or `default_claude_max_20x`. `claude_team` answers its non-empty single-line `organization.seat_tier` as the raw plan verbatim; no other family reads the seat tier. The observed Team Standard spelling is `team_standard`; `team_premium` is matched by analogy and remains unconfirmed. Without a usable seat tier, other families and unknown Max tiers are answered but unrecognised, which leaves auth status in charge. Contradictory owners also answer unrecognised, without accepting seat tiers or names. Malformed or identity-less bodies and failed reads are failures, not answers. Subscription status and account feature flags do not decide the plan. The spec's separate [capacity table](./plans.md#capacity-tables) maps recognized raw tiers to stable keys and weekly units, never from the display name.

An owner-matched profile also supplies `organization.plan_display_name`, independently of tier recognition. Whitespace runs collapse, control characters drop, and a blank result is no name. The bound name labels an existing plan verbatim with the `Claude` prefix (`Ultra` → `Claude Ultra`); without one the tier vocabulary remains the fallback. `plan_display_label` is ignored: it includes prose such as `Ultra plan`, not the bare product name. The normalized machine tier and saved-plan basis never contain the display name.

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

Every successful parse sets `reset_credits` to `Some`. Absent `cedar_ember`, ineligibility, and empty grants settle to zero, which clears a previously cached grant instead of preserving it through the cache's missing-field fallback. Each eligible grant with `resets_left` above zero becomes one `ResetGrant`: its remaining count, its `ends_at` as the expiry, the provider's `label` verbatim, its `clears` tokens untranslated, its `paused` flag, and its `starts_at`. `ResetCredits::from_grants` orders the grants by expiry, undated last, and derives the flat summary from them: the count is the sum of `resets_left`, and each valid `ends_at` is repeated once per remaining reset and sorted. Missing or invalid expiry leaves the count intact and the grant undated, and other grant flags do not filter the banked count. A `label`, `clears`, `paused` or `starts_at` that is `null` or mistyped reads as absent rather than failing the usage read. Each grant also keeps `usable_now` and `use_requires_limit`, and the grant whose `id` matches the envelope's `next_grant_id` is marked `selected` and carries the envelope's `cooldown_until`; the grant `id` itself, `at_limit`, `type`, `gives_back_weekly` and `resets_total` are not kept. A cached record without these fields selects no grant, so every reader of the claim facts treats it as refused. Each credit carries the effect `KeepsSchedule`: redeeming a Claude credit refills the window and leaves its reset where it was. Claude redeems by hand and automatically, each time on the selected grant alone; at level `rescue` the sidebar marker advises when to redeem by hand from these cached facts ([Auto-redeem](./providers.md#auto-redeem)).

`crates/rimz/src/agents/adapters/claude/oauth_usage.rs::prepare_reset_credit` also reads the profile. It adds `skip_spend=1` only to its preview usage read, then reads `GET /api/oauth/profile` and keeps `organization.uuid` in its private action. The flag's effect on paid-usage fields is unverified, so periodic, delegated, and post-reset refresh reads keep the full `?cedar_ember=1` query. Both profile and claim URLs derive from the trusted usage URL's origin, so one loopback override covers the whole flow. A profile HTTP failure names that read, not usage.

Claimability is separate from the banked count. These create a neutral hold, checked in this order: absence or ineligibility, a future cooldown, no selected grant, or a selected grant that is paused, empty, future, expired, or not usable. A limit-only grant explains that restriction when the account is not at the limit. Every check after eligibility is `crates/rimz/src/agents/credits.rs::RedeemRefusal::of`, which the preview's hold reads from the fresh wire and the sidebar's redeem marker reads from the cached grants (`ResetCredits::refusal`), so the two agree on what the provider accepts. The shared manual consumer refuses holds before reserving.

The private action targets `next_grant_id` and sends `POST /api/organizations/{org}/reset_rate_limits` once through the non-retrying transport. Result strings map to the neutral outcomes, `cooldown` included, and unknown strings are not success. A failed claim leaves the durable reservation standing and names Settings > Usage as the place to check the unknown outcome. A `reset` alone refreshes usage. The wire is [observer evidence](../../externals/agent-adapter/claude-reference.md#oauth-usage-endpoint), not an Anthropic API contract. A live manual redemption through this action succeeded on a user's account (`t2`), refilling the grant's windows, which three independent open-source clients' use of the same claim corroborates; no test against a live claim backs it. Request-id idempotency remains unverified, so after an unknown outcome the automatic helper relies on its ten-minute reservation and a fresh read, not on deduplication ([Auto-redeem](./providers.md#auto-redeem)).

### Birth account key

A root `SessionStart` that maps to `Registered` stamps the same digest onto the session, so a session records the login it was born on. [`enrich_root_registration`](../../../crates/rimz/src/agents/adapters/claude/mod.rs) reads the credentials once per registration, from the same file with the same Keychain fallback. It applies neither the expiry check nor the `user:profile` scope check the usage probe applies, because the key identifies an account and authorizes nothing. A `compact` start maps to `CompactionEnded` and a subagent has a parent, so neither stamps a key. A `resume` start does, and rebinds the row to the login in force at that moment. What the key partitions is [providers.md](./providers.md#producer-aggregation), and where it lives is [model.md](./model.md#the-rollup).

The stamp reads the credentials file, not the token the Claude process sends, and RimZ cannot see whether a running process picked up an account switch. A `/clear` after switching accounts therefore registers the new file's key while the process may still bill the old account. The session's live windows then count against the new account until the process restarts.

## Cost

[`spend.rs`](../../../crates/rimz/src/agents/adapters/claude/spend.rs) parses Claude's full-history spend, read-only and sidebar-safe, and the [`SpendingWalker`](../../../crates/rimz/src/agents/spending/mod.rs) aggregates it into the configured headline, local calendar day, and 7, 30, and 364-local-day windows, including today ([spending.md](./spending.md#cost-history)). The fleet walk reads every `**/*.jsonl` under each config root's `projects/`.

A Claude spend session is keyed by the UUID in its transcript path; a subagent transcript names its parent's UUID. The per-session seat fold resolves the main transcript plus every `subagents/*.jsonl` companion in either layout: `<session_id>.jsonl` beside `<session_id>/subagents/`, or `<session_id>/chat.jsonl` inside it.

Claude replays parent messages into each subagent file, so the walk deduplicates by `(message.id, requestId)` across files. It prefers the main-thread record, then the larger token total, then the record with speed metadata, and suppresses sidechain replay so the selected turn counts once.

Pricing works per request:

- A `message.usage` without `costUSD` is priced through the [price book](./spending.md#token-pricing), with input, output, 5-minute cache creation, 1-hour cache creation, and cache read each at its own rate. Current transcripts carry no `costUSD`.
- A transcript line that logs a positive `costUSD` uses that figure verbatim for the top-level request.
- Each `advisor_message` in `message.usage.iterations` is a separately billed request with its own model and tokens. It becomes a child entry keyed `<message.id>:advisor:<n>`, priced independently.
- When machine managed settings define `modelPricing` ([upstream](../../externals/agent-adapter/claude-reference.md#project-storage-and-managed-pricing)), [`managed_pricing.rs`](../../../crates/rimz/src/agents/adapters/claude/managed_pricing.rs) overlays its contracted rows and optional finite multiplier (`0 < m <= 10`) on the list book for spend, the session-cumulative fold, and child costs. Unreadable or invalid settings leave the list book as is.
- A model with no known price keeps its tokens with zero dollars while the pricing chase looks for a price. The `<synthetic>` model is dropped, because it is not an API model id.

Historical tool statistics count the `tool_use` blocks on the usage-bearing assistant messages the spend parser already reads, so the same deduplication counts retried and sidechain copies once.

## Remote control

RimZ hosts `claude remote-control --spawn worktree` in the room's `rimzd` view when `[remote_control] claude = true` ([rimzd.md](../rimzd.md)). The host argv always carries an env prefix: `env CLAUDE_CONFIG_DIR=<home>` when the room's login sets one, else `env -u CLAUDE_CONFIG_DIR` (`crates/rimz/src/agents/adapters/claude/remote_control.rs::host_argv_with_login`). Passing `--spawn` also skips Claude's spawn-mode chooser. The provider side has three parts, and the upstream gates behind them are in [claude-reference.md](../../externals/agent-adapter/claude-reference.md#remote-control).

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

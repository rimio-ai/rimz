# Pi adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md), and the raw upstream protocol is in [pi-reference.md](../../externals/agent-adapter/pi-reference.md).

Pi's only integration surface is in-process TypeScript extensions ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#extensions)), so RimZ ships one and owns the wire. The extension forwards Pi's events to `rimz hooks feed --source pi`, and the envelope schema is RimZ's: a field the extension stops sending is a RimZ bug. The decision direction inverts. Claude and Codex run RimZ as a hook child and read its stdout, while Pi's extension spawns RimZ as its own child and applies the answer through the handler's return value. The code lives in [`agents/adapters/pi/`](../../../crates/rimz/src/agents/adapters/pi/mod.rs):

| Module | Owns |
| --- | --- |
| `mod.rs` | `PI_DESCRIPTOR` (the `static AgentSpec`), the coverage declarations, the hook catalog `PI_HOOKS`, `PiAdapter::decode_hook`, `pi_observed_context`, and the install path `pi_extension_path` |
| [`extension.ts`](../../../crates/rimz/src/agents/adapters/pi/extension.ts) | the extension RimZ installs, embedded as `EXTENSION_SOURCE` |
| `payloads.rs` | `PiHookPayload`, the typed envelope, with its tolerant `parse_payload`, `PiCompactionReason`, and `agent_end_errored` |
| `ask.rs` | questionnaire questions, native answers, and the `rimz answer` key plan |
| `account.rs` | the auth-file probes over the shared [`delegated_account.rs`](../../../crates/rimz/src/agents/delegated_account.rs) seam, and the freshest-session provider read |
| `spend.rs` | the session spend parser and the path resolvers `pi_config_dir` and `pi_session_roots` |
| `transcript.rs` | the conversation stream for history and reply streaming |
| `tests/` | lifecycle mapping, asks, context projection, and install |

`crates/rimz/tests/integration/examples/pi_extension.rs::run_extension_harness` runs `extension.ts` under Node and asserts the payloads it feeds, so a change to the extension gets its test there.

## The extension

Every forwarded event spawns one `rimz hooks feed --source pi` child, with the envelope as JSON on stdin and `RIMZ_AGENT_PID` set to Pi's own pid. The extension runs `$RIMZ_BIN` when set, else `rimz` from `PATH`. Every event except `tool_call` is fire-and-forget: the child's stdout and stderr are discarded, and a spawn error or a broken pipe is swallowed, so a missing `rimz` never surfaces inside Pi.

`tool_call` is Pi's awaited pre-tool gate. Its handler reads the child's stdout and blocks the tool only when stdout parses as JSON with `block: true`. Empty or unparseable stdout, a spawn error, or a missing binary lets the tool run. Pi's `decode_hook` sets no reply, so stdout stays empty ([Neutral output](#neutral-output)). Pi awaits the handler with no timeout ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#blocking-dialogs-and-errors)), so a slow feed child delays the tool and its siblings.

The `envelope` helper stamps these fields on every event, then adds the event's own fields ([Hooks and lifecycle](#hooks-and-lifecycle)). Each field is best-effort, and an absent value is omitted from the JSON.

| Field | Source |
| --- | --- |
| `hook_event_name` | the event name |
| `session_id` | `ctx.sessionManager.getSessionId()` |
| `cwd` | `ctx.sessionManager.getCwd()`, else `ctx.cwd` |
| `model` | `ctx.model.id` |
| `effort` | `pi.getThinkingLevel()`, `off` through `max` |
| `session_name` | the latest `/name` title, from `session_info_changed` or hydration |
| `context_pct`, `context_window`, `total_tokens` | `ctx.getContextUsage()`, with the percentage and the token count rounded |
| `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_write_input_tokens` | the latest assistant call's usage |
| `total_cost_usd` | the extension's running session cost ([Cost accrual](#cost-accrual)) |
| `rate_limits` | the windows captured from the latest `after_provider_response` ([Account and balance](#account-and-balance)) |

The synthetic `subagent_started` and `subagent_stopped` events skip this set. `feedSubagent` sends only `hook_event_name`, the parent `session_id`, `cwd`, and the lineage fields.

The extension reads Pi's exported `VERSION` once. From 0.80.4 it waits for Pi's native `agent_settled`, and on an older Pi it feeds `agent_settled` itself at `agent_end` ([Turn boundaries](#turn-boundaries)). `ui_prompt_start` and `ui_prompt_end` exist only from Pi 0.84.4, so an older Pi raises no wait for extension dialogs. When `ctx.sessionManager.getBranch()` is missing or throws, the extension skips hydration and the standalone-usage refresh.

### Install

Install, preview, and uninstall own one whole file. `pi_extension_path` resolves it in this order:

1. `RIMZ_PI_EXTENSION`, when non-empty, overrides the whole path.
2. `$HOME/.pi/agent/extensions/rimz.ts`. Without `$HOME`, install fails.

The path is Pi's global discovery directory. Install never writes Pi's project-local `.pi/extensions/`, so it leaves the project trust hash untouched. The resolver ignores `PI_CODING_AGENT_DIR`, which moves Pi's whole config root, extensions included ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#cli-and-environment)), so with that variable set Pi does not discover the installed file.

The file's first line carries the `_rimz_managed` marker. A marked file is RimZ's, however edited since, and install rewrites it whole. Install and preview refuse an unmarked file at the path, and uninstall leaves one untouched: it is the user's own extension. `PI_MANAGED_SOURCE` is built with `upgradeable` true, so `ManagedSource::upgrade_available_at` reports a marked file that differs byte for byte from `EXTENSION_SOURCE`, and `rimz setup` offers a consented refresh. The older file stays installed until the user accepts, so an extension fix reaches a user only after that refresh.

Pi hot-reloads discovered extensions on `/reload`, which fires `session_shutdown` and `session_start` with `reason: "reload"` for the same session id. The extension skips that shutdown, because its fire-and-forget end signal could land after the re-registration and end the fresh row. Every `session_shutdown` that reaches RimZ is therefore a real end.

## Hooks and lifecycle

`decode_hook` maps each wire event to at most one [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs). The wire names are Pi's native event names, except the two `subagent_*` events, which the extension synthesizes. The upstream events and payloads are in [pi-reference.md](../../externals/agent-adapter/pi-reference.md#events-the-adapter-wires).

| Wire event | Signal | Fields the adapter reads beyond the envelope |
| --- | --- | --- |
| `session_start` | `Registered` | `reason`, and the worktree from `cwd` |
| `before_agent_start` | `TurnStarted` | `prompt`, sanitized into the task and the prompt |
| `agent_end` | none | `stop_reason`, `error_message`, `last_assistant_message`, plus `model` and `total_tokens` from the last assistant message |
| `agent_settled` | `TurnEnded { errored }`, or `TurnInterrupted` for `stop_reason: "aborted"` | the retained `agent_end` fields |
| `turn_end` | none | envelope only: publishes call usage and cost |
| `after_provider_response` | none | envelope only: publishes new rate-limit windows |
| `message_update` | none | envelope only, throttled ([Context and transcript](#context-and-transcript)) |
| `session_info_changed` | none | envelope only: publishes the new `session_name` |
| `model_select`, `thinking_level_select` | none | `model`, `effort` |
| `tool_execution_end` | `ToolUsed { mutates, edits, name, native_key }` | `tool_name`, `tool_call_id`, `is_error`, plus `tool_details` for `ask_user_question` |
| `tool_call` | `AwaitingInput { Question }` for `ask_user_question` with UI, else none | `tool_name`, `tool_input`, `tool_call_id`, `has_ui` |
| `ui_prompt_start` | `AwaitingInput { Question, detail }` | `ui_prompt_id`, `ui_prompt_kind`, `ui_prompt_title`, `has_ui` |
| `ui_prompt_end` | `ToolUsed` with no name | `ui_prompt_id` |
| `subagent_started` | `SubagentStarted` on the child | `subagent_id`, `subagent_label`, `subagent_source`, `cwd` |
| `subagent_stopped` | `SubagentStopped { errored }` on the child | as above, plus `errored` and an optional `total_tokens` the extension does not send |
| `session_before_compact` | `Compacting` | `compaction_reason`, `compaction_will_retry` |
| `session_compact` | `CompactionEnded { auto, failed: false }` | `compaction_reason` |
| `session_compact_failed` | `CompactionEnded { auto, failed: true }` | `compaction_reason` |
| `session_shutdown` | `Ended` | `reason` |

Root identity is direct. The extension runs inside the pane's Pi process and the session id exists from launch, so the descriptor declares `registers_lazily: false`. `parse_payload` degrades a payload that fails to deserialize to the empty default, keeping only `session_id` and any valid rate-limit windows.

The catalog marks `session_start`, `before_agent_start`, `agent_end`, `turn_end`, `message_update`, `tool_execution_end`, and `ui_prompt_end` as progress, so they touch the activity heartbeat ([model.md](./model.md#activity-clocks)). `tool_call` is not progress, so creating an ask never counts as work that un-blocks the row (`crates/rimz/src/agents/adapters/pi/tests/mod.rs::pi_activity_filter_excludes_the_blocking_gate`).

### Turn boundaries

RimZ's turn runs from `before_agent_start` to `agent_settled`. A Pi turn is one LLM call, and `agent_start`/`agent_end` bracket one low-level run that an automatic retry, an overflow compaction-and-retry, or a queued follow-up can still extend. `agent_settled` fires once none remains ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#extension-events)), so mapping the turn end there keeps those continuations from flashing the row idle.

The verdict travels from `agent_end` to `agent_settled` inside the extension. At `agent_end` the extension takes the last assistant message's `stopReason`, `errorMessage`, visible text, model, and `usage.totalTokens`, forwards them, and retains them per session. At `agent_settled` it feeds the retained fields again, then drops them:

| Retained verdict | Signal |
| --- | --- |
| native `aborted: true`, which overrides the retained `stop_reason` to `aborted` | `TurnInterrupted`, which closes the turn at `idle` ([model.md](./model.md#turn-endings-and-parked-turns)) |
| `stop_reason` `error`, or a non-empty `error_message` | `TurnEnded { errored: true }`, which resolves the row to `failed` |
| anything else | `TurnEnded { errored: false }` |

`decode_hook` sets `last_assistant_message` as the hook's final message on `agent_settled` only, which feeds the transcript and a supervised run's result without a transcript read. `before_agent_start` clears any retained verdict, and `agent_settled` also clears the open questionnaire set and the session's dialog key ([Asks](#asks)).

On a Pi before 0.80.4 the extension's `agent_end` handler does the settle work itself: it refreshes the standalone cost, feeds `agent_settled` with the verdict, and feeds the child stop. A Pi 1.2.0 run started by a custom message (`messageType: "custom"`) carries no `prompt`, so its turn starts without a task label.

### Tools

Every `tool_execution_end` produces `ToolUsed` with the tool name and Pi's `toolCallId` as the native key. The descriptor's `tools` lists decide the two flags: `bash`, `edit`, `write`, and `powershell` mutate, and only `edit` and `write` edit files. A read-only tool still produces `ToolUsed`, with both flags false, so its name reaches the live tool map. `ask_user_question` is always non-mutating, and its key closes the questionnaire wait.

### Compaction

Pi's compaction is a bracket: `session_before_compact` opens it, and either `session_compact` or `session_compact_failed` closes it ([model.md](./model.md#the-compaction-bracket)). `PiCompactionReason::auto_flag` maps `manual` to `auto: false`, `threshold` and `overflow` to `auto: true`, and a missing or unknown reason to `None`. A failed close never records a context reset or opens a resumed turn. An unrecoverable overflow can emit `session_compact_failed` with no opening event ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#extension-events)), and for the running turn that close is a no-op: status and turn timestamps stay, and no compaction is counted. `session_compact` also adds the compaction entry's cost to the running total.

### Session end

`session_shutdown` maps to `Ended`, and its catalog entry ends the session, so the row's end is observed ([model.md](./model.md#observed-ends-and-reaped-ends)). Pi fires it on quit (Ctrl+C, Ctrl+D, SIGHUP, SIGTERM) and when `/new`, `/resume`, or a fork replaces the session, so the old row ends and the new session id registers. Before feeding it the extension feeds the child stop and clears every per-session map and the captured windows. A lost multiplexer session is derived by the `rimz exec` wrapper, since Pi reports none.

## Asks

Pi runs ordinary tools unasked and has no native question or permission prompt. Both ask sources are extension surfaces, and both raise a `Question`.

### The questionnaire

[`@juicesharp/rpiv-ask-user-question`](../../externals/agent-adapter/pi-reference.md#rpiv-ask-user-question-questionnaire), when the user installs it, registers the `ask_user_question` tool and draws its questionnaire in Pi's pane. RimZ's extension sees the awaited `tool_call`, adds the call id to its open-questionnaire set, and forwards the input with `has_ui` and the call id. `decode_hook` classifies the call through the descriptor's `blocking` list and records `AwaitingInput { Question }` keyed by `tool_call_id` only when `has_ui` is not false. An absent `has_ui` counts as true, for an extension that predates the field. `ask::question_detail` parses the Claude-compatible `questions` input through the shared question decoder, which also accepts JSON-string arrays and objects, as rpiv does.

The matching `tool_execution_end` carries the same key, so a sibling tool's completion leaves the questionnaire open. It also carries rpiv's `result.details` as `tool_details`, which `ask::answer_detail` turns into structured answers:

| `details.answers[].kind` | Recorded `AskAnswer.chosen` |
| --- | --- |
| `option`, `custom` | the trimmed `answer` |
| `multi` | the checked `selected` labels, then the typed `answer` when non-empty |
| `cancelled: true`, or no usable answer | nothing: the completion only clears the wait |

Each answer keeps its `notes` as the note. The questionnaire-wide `globalNote` is not recorded, since it already reaches the model in the tool result.

### Extension dialogs

From Pi 0.84.4, every extension `ctx.ui` dialog (`select`, `confirm`, `input`, `editor`, `custom`) is bracketed by `ui_prompt_start` and `ui_prompt_end` ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#extension-ui-prompt-events)). The extension forwards a start only while a run is active and a UI exists. It skips these dialogs:

- a dialog while `ctx.isIdle()` is true, opened by a slash command the user just typed
- a dialog in RPC mode, or without a UI
- a dialog while an `ask_user_question` call is open, since the questionnaire is already the ask

A dialog outside a run would raise a wait that no boundary closes. Each forwarded dialog gets an extension-minted `ui_prompt:<n>` key, held per session. `ui_prompt_end` sends the same key as a `ToolUsed` with no name, which clears only that wait and never counts as a tool call. `agent_settled` drops the session's key, so a dialog still open at the settle closes silently and does not reopen the turn as out-of-turn tool activity.

The extension cannot tell a dialog that a slash command opened from one a tool opened, so a dialog the user opens with a command during a run raises a wait too. Pi's payload carries no dialog body or options, and `confirm` is not reliably a permission prompt, so every dialog is a generic `Question` whose detail is the trimmed title.

### Answering

`ask::answer_plan` drives the rpiv questionnaire with keys. Focus starts on option zero, and every question ends with a `Type something.` row ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#rpiv-ask-user-question-questionnaire)):

| Answer | Keys |
| --- | --- |
| single-select pick `n` | Down `n` times, Enter |
| multi-select picks | Down to each pick and Space, then Down past the custom row to `Next`, Enter |
| typed text, any shape | Down once per option to the custom row, paste, Enter |
| several questions | each question's keys, then Enter on the Submit tab |

The plan refuses a non-`Question` ask, an ask without structured questions (every extension dialog, with a hint to answer in the pane), an answer count that differs from the question count, picks combined with text in one answer, an out-of-range or repeated pick, and more than one pick on a single-select question.

### Neutral output

Pi's `decode_hook` returns no reply on any event, so the `tool_call` child's stdout is empty (`crates/rimz/src/agents/adapters/pi/tests/mod.rs::neutral_decision_shape_is_pinned`). Empty stdout lets an ordinary tool run and lets `ask_user_question` open rpiv's own UI.

## Subagents

Pi has no built-in subagent lifecycle. Children come from extensions such as [nicobailon's `pi-subagents`](../../externals/agent-adapter/pi-reference.md#nicobailon-pi-subagents) and [tintinweb's](../../externals/agent-adapter/pi-reference.md#tintinweb-tintinwebpi-subagents), and RimZ depends on neither one's event bus or registry. Each RimZ extension instance instead finds its own place in the process lineage at `session_start`:

1. Primary. The first instance in a process writes its session id to `globalThis[Symbol.for("rimz.pi.primary-session")]` and to `RIMZ_PI_PARENT_SESSION`. It keeps both current on every envelope, so a session switch moves them.
2. Child. A later instance whose session id differs from the marker (the global first, then the environment variable) records that id as its parent and feeds `subagent_started`, with the parent as `session_id`, its own session id as `subagent_id`, its label, and `subagent_source: "pi-session"`.

In-process children share the parent's `globalThis`. A child Pi subprocess inherits the environment marker, feeds its start, and then becomes the primary of its own process for its descendants. The child's ordinary events carry its own session id, so its model, effort, context, usage, and cost enrich the same nested row.

`decode_hook` resolves the pair through `resolve_subagent_identity` and quarantines drift. The store accepts either order between the child's registration and its `subagent_started`, setting the parent on an existing row through a `SubagentAdopted` event ([model.md](./model.md#the-rollup)), and flattens nested descendants onto the root row. Once a row has a parent, the hook path turns the child's own `TurnStarted` into `SubagentStarted` and its clean `TurnEnded` into `SubagentStopped` (`crates/rimz/src/cli/hooks/lifecycle/observe.rs::normalize_correlated_subagent_signal`).

The label is the child's Pi session name, else `PI_SUBAGENT_CHILD_AGENT` (set by nicobailon releases before v0.65.0), trimmed and cut to 80 characters, and absent when neither exists. Current nicobailon releases name the child session in its `before_agent_start`, after the row started unlabelled, so a `session_info_changed` that changes the label re-feeds `subagent_started` and the row takes the new task.

The child feeds `subagent_stopped` once, at its own `agent_settled` (or the pre-0.80.4 `agent_end` settle), with `errored` true for an `error` or `aborted` stop reason or any error message. A non-reload `session_shutdown` is the backstop for a subprocess that exits. A hard-killed subprocess can leave a running row through the current parent turn, and the parent's next prompt retires it. A later `before_agent_start` re-feeds the start, but the store ignores `SubagentStarted` after a terminal stop (`crates/rimz/src/agents/lifecycle.rs::map_status`), so a reused child row keeps its first verdict.

A child that does not load the RimZ extension stays invisible, as a root launched with `pi --no-extensions` does. nicobailon foreground children (v0.65.0 and later) run inside the parent process and never load global extensions, so they appear only when `subagents.defaultExtensions` or the agent's `extensions` lists the extension's absolute path (`~/.pi/agent/extensions/rimz.ts`). nicobailon's background runner loads global extensions unless the agent declares `extensions`. tintinweb's `extensions: false` and an allowlist that omits RimZ are deliberate opt-outs.

## Launch and resume

The descriptor declares these launch forms, pinned by `crates/rimz/src/agents/adapters/pi/tests/mod.rs::launch_resume_and_fork_argv_match_pi`:

| Form | argv |
| --- | --- |
| interactive | `pi`, then the preset args |
| with a prompt | `pi`, the preset args, `--`, then the prompt |
| resume | `pi --session <id>`, which restores a rollup-recorded session (a partial UUID also matches) |
| fork | `pi --fork <id>` |
| bare `--resume` | the native picker, `pi --resume` ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#cli-and-environment)) |
| manual compaction | `/compact`, with no instruction argument |
| permission mode, turn cap, interrupt key | none |

Profile presets render through `PresetMatchers`, pinned in `crates/rimz/src/agents/conformance.rs`:

| Preset | Rendered as |
| --- | --- |
| `model` | `--model <value>` |
| `effort` | `--thinking <value>` |
| `system-prompt-file` | `--system-prompt <text>`, the file's contents as one argument |
| `auto-compact` | skipped with a warning at launch |

The full replacement prompt is visible in the process argv, so RimZ refuses a text over 120 KiB before exec (`crates/rimz/src/harness/prompt_compose.rs::TEXT_PROMPT_LIMIT`). A Markdown agent definition that names no effort launches Pi at `xhigh` (`DEFINITIONS`).

A definition's typed tools list follows the role through engine changes and renders once for Pi through `DefinitionTools::Optional`. The [tool mapping](../../reference/definitions.md#tools-to-provider-arguments) emits one ordered, deduplicated `--tools` list, warns for dropped names and unenforced restrictions, and uses `--no-tools` for an empty or all-dropped list. An absent list adds no flag. A plain list keeps MCP tools callable; `--no-tools` disables built-in and extension tools, including MCP. A supplied list without `AskUserQuestion` or `ask_user_question` removes the rpiv question tool.

A plain resume or fork passes neither `--model` nor its dependent `--provider` flag: Pi restores the session's provider and model instead of resolving the launch's requested name again. The launch reminder makes no model claim and `RIMZ_AGENT_MODEL` stays unset, while the launch record keeps the requested name. An explicit model override still takes precedence.

### Launch reminders

Launch reminders ride the extension, because Pi's additive flag drops the user's file: any `--append-system-prompt` replaces the discovered `APPEND_SYSTEM.md` instead of adding to it ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#cli-and-environment)). `append_system_text_channel` returns `SystemTextChannel::ExtensionEnv`, and the launch exports the rendered text as `RIMZ_LAUNCH_REMINDERS` (`crates/rimz/src/harness/launch.rs`). It is always exported for Pi, empty when there is no reminder, so a parent's value never reaches a child launch.

The first extension instance in a process moves the text into a `globalThis` marker and deletes it from `process.env`, so tool subprocesses and child Pi processes never inherit it. The primary session's `before_agent_start` handler returns `systemPrompt` with the reminders appended to the prompt Pi built, `APPEND_SYSTEM.md` included. In-process children get none. A Pi that passes no `systemPrompt` to the event, or an extension that predates the channel, launches without reminders.

### Modes the extension cannot see

`--no-extensions` disables discovered extensions, so a session started with it feeds RimZ nothing. An explicit `-e` path still loads ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#extensions)). `-p` and `--mode json` run extensions without a UI: the extension stamps `has_ui: false`, so the questionnaire raises no ask, and rpiv itself fails the call with `no_custom_ui`. RPC mode forwards no dialog waits.

## Context and transcript

Pi's live context needs no transcript read (`transcript_tail_context: false`). `decode_hook` copies `model`, `effort`, the context gauge, and the token split onto each lifecycle observation. `pi_observed_context`, which `observe_context` also calls, builds an `AgentContext` from the same envelope: `session_name`, `model_id`, `effort`, `tokens` (window, used percentage, and the latest call's split), `cost`, and the rate-limit windows. It sets `cost` only for a total above zero, and returns nothing for an envelope that carries none of these fields.

The extension pushes a value change immediately on `model_select`, `thinking_level_select`, `session_info_changed`, `turn_end`, and `after_provider_response`. `message_update` fires per streaming delta, so `pushMessageUpdate` feeds it at most once per second per session, and only when the rounded gauge or the running cost differs from the last push. A change inside the second is sent by a trailing timer.

### Cost accrual

The extension keeps two cost buckets per session, and `total_cost_usd` is their sum:

| Bucket | Adds | When |
| --- | --- | --- |
| call cost | assistant and tool-result `usage.cost.total` | each `turn_end` |
| | the compaction entry's cost | `session_compact` |
| | the branch-summary entry's cost | `session_tree`, a subscription the extension does not forward |
| standalone cost | every `type: "usage"` entry on the branch, recomputed from `getBranch()` | before each `turn_end` and settle feed |

Only positive costs count. The standalone bucket is recomputed, never added to, so repeat scans do not recount an entry. Hydration runs at `session_start`, once per session id: it walks `getBranch()` for every cost-bearing entry, the latest assistant usage split, and the session name. A `pi --session` resume fires `session_start` with `reason: "resume"`, so a resumed row starts with the session's full cost.

The settled transcript walk can only raise the row's cost. On a turn end, `crates/rimz/src/cli/hooks/lifecycle/context.rs::supplement_realtime_cost` resolves the session file by its UUID and prices it through `parse_spend`. It skips the walk when the file's stat is unchanged and the row already has a total, and it keeps the live total when the transcript sum is not higher. `AgentContextRecord::merge_observed` accepts a pushed total only when it is not lower than the stored one. The row therefore shows the higher of the two figures. They differ when an entry has no direct cost: the transcript walk prices it from the price book, and the extension counts nothing for it.

### The conversation stream

`transcript::parse_messages` keeps `message` entries with role `user` or `assistant`. It takes their text from a string or from `text` blocks joined by newlines, and passes user text through `sanitize_user_prompt`, which peels RimZ and harness envelopes and drops a message that still carries a control tag. Thinking, tool calls, tool results, summaries, and extension records stay out. `agents history`, reply streaming, and every transcript-backed consumer read this stream.

## Account and balance

`PiAdapter::probe_account` and `probe_account_usage` ignore the login environment. Both read `auth.json` under `pi_config_dir()`, which is the process's `PI_CODING_AGENT_DIR`, else `$HOME/.pi/agent` ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#auth-file)). The arms and their cache times are in [providers.md](./providers.md#the-out-of-band-probe).

| Condition | `probe_account` result |
| --- | --- |
| `auth.json` missing, or an empty map | `LoggedOut` |
| any other read error | `Unavailable(ReadFailed)` |
| the file does not parse | `Unavailable(UnparseableOutput)` |
| selected credential `type: "oauth"` | `Found`, `metered: Some(true)` |
| selected credential `type: "api_key"` | `Found`, `metered: Some(false)` |
| any other type | `Found`, `metered: None` |

The probe selects one credential. It takes the newest session candidate (`crates/rimz/src/agents/adapters/pi/account.rs::used_provider`): a `.jsonl` file directly inside a session root or one directory below it, ordered by `(mtime, path)` with the greater path winning a tie. When its first line names a non-empty string `parentSession` that is an absolute path to a regular file (following symlinks), the parent's tail decides the provider; otherwise its own tail does. Header parsing is capped at 1 MiB. The read is one hop: the parent's header is never read. The depth limit excludes [nicobailon's nested runs](../../externals/agent-adapter/pi-reference.md#nicobailon-pi-subagents), including older releases without parent headers; reading [tintinweb's sibling children](../../externals/agent-adapter/pi-reference.md#tintinweb-tintinwebpi-subagents) through their parent keeps the child from selecting its own provider. Forks and clones carry the same header and follow the same rule.

The deciding file's provider is the last non-empty nested `message.provider` in its 64 KiB tail, or, only when that tail has none, the last non-empty top-level `provider` on a `model_change` entry. An assistant's provider wins even over a later `model_change`, so once a session has an assistant row, a virtual model's selection does not replace its physical provider (a probe before that row is a limit below). A `usage` entry's top-level provider never counts. With neither source, an unreadable deciding file, or no candidate, the probe falls back to the first OAuth entry in key order, then the first entry. It never tries an older candidate or the child's own tail after reading a provider-less parent. An empty, torn, or non-JSON first line names no parent, so the candidate's own tail decides. The plan names it (`Anthropic OAuth`, `OpenAI API Key`), the raw provider id rides `sub_provider`, and `auth.json`'s mtime is `credentials_updated_at_ms`. Only two OAuth providers carry a quota scope: `anthropic` (`anthropic/oauth`) and `openai-codex` (`openai/oauth`). Pi's `openai` OAuth login stays `OpenAI OAuth` and metered, with no scope.

Because the answer follows the newest candidate, Pi reports an inputs stamp (`AccountCapability::account_inputs_stamp`, `crates/rimz/src/agents/adapters/pi/account.rs::inputs_stamp_from`), and a cached record whose stamp moved re-probes behind the 10-second retry floor instead of waiting out the 10-minute TTL ([providers.md](./providers.md#the-out-of-band-probe)). The stamp is a SHA-256, truncated to 64 bits, over `auth.json`'s presence and mtime, each session root's mtime (`pi_session_roots()`: `PI_AGENT_DIR`, else `PI_CODING_AGENT_SESSION_DIR`, else `sessions/` under the config directory), and the name and mtime of each directory directly inside a root. Pi creates a session file at the session's first message, which moves its directory's mtime, while an append moves only the file, which the stamp leaves out. So a new session, a login, a logout, and an OAuth token refresh re-probe within seconds. A child spawn can still move the stamp; the re-probe reads the child through its parent. A model-picker switch inside a running session, a resume of a session that used another provider, and concurrent sessions on different providers do not move the stamp and wait for the TTL. The re-probe a new file triggers reads the deciding file's entries present at that instant. If it lands before the first assistant row, the rows that follow are appends, which move no stamp, so whatever that probe records stays until the TTL or the next stamp move. Eight accepted limits:

- A probe that lands between Pi creating a session file and writing its first entries (microseconds) finds no provider and records the key-order fallback.
- A new session on a virtual model records the provider its `model_change` lists, or the key-order fallback when `auth.json` has no credential for that provider, even when the model answers on another provider.
- A fork, clone, `rimz agents fork` (`pi --fork`), or `/new` carrying a usable parent reports its parent's provider, so a provider switch made inside a fork is not seen.
- A persisted tintinweb child of an in-memory parent (`pi --no-session`) has no `parentSession` and speaks for itself.
- A session whose parent file is gone, relative, or not a regular file speaks for itself.
- A persisted nested tintinweb child (`persist_session: true`, opt-in) speaks for the outer child, one hop up, not the original user session.
- A user session two or more directories below a root, such as `--session-dir ~/.pi/agent/sessions/a/b/`, is not a candidate. One directory down (`sessions/work/`) still counts. Pi's own `/resume` does not list the deeper session either.
- Two directory changes inside one filesystem timestamp tick can read as one.

The usage probe selects the same way but requires an OAuth credential with a non-empty, unexpired access token. It hands an `anthropic` token to Claude's usage fetcher and an `openai-codex` token, with its `accountId`, to Codex's, so each backing provider's normalization applies ([providers.md](./providers.md#per-provider-mapping)). An API key, an expired token, or any other provider (Pi's `openai` OAuth login included) settles as a no-credentials result, not as a failure to retry. The usage identity's account key is the `accountId` for the `openai/oauth` scope, else a SHA-256 of the refresh token (or the access token) under the domain `rimz/pi-oauth-account-key/v1`. Unmetered API-key logins get the API spend row ([providers.md](./providers.md#paid-and-api-rows)).

The extension also captures windows from `after_provider_response` headers. For each prefix it reads `-used-percent`, `-window-minutes`, and `-reset-after-seconds`, and keeps a window that has a used percentage or a reset:

| Header prefix | Default duration |
| --- | --- |
| `x-codex-primary`, `anthropic-ratelimit-unified-primary`, `-5h`, `-five-hour` | 300 minutes |
| `x-codex-secondary`, `anthropic-ratelimit-unified-secondary`, `-7d`, `-seven-day` | 10080 minutes |

A response with any such window replaces the captured set, and every later envelope carries it. `PiRateLimitWindow::to_domain` publishes each window as `WindowSource::BestEffort`.

## Cost

Full-history spend rides the shared spend pass ([spending.md](./spending.md#cost-history)). `pi_session_roots` picks the roots from the process environment: `PI_AGENT_DIR` (RimZ's comma-separated test override, not a Pi variable), else `PI_CODING_AGENT_SESSION_DIR`, else `pi_config_dir()/sessions`. `spending_sources` walks `**/*.jsonl` under each root and ignores the login environment. Pi writes one `<ISO-timestamp>_<uuid>.jsonl` per session under `--<cwd-with-dashes>--/`, and `spending_session_id` takes the UUID after the last `_`. The walk is recursive, so nicobailon child sessions, which Pi writes below the parent's session directory, are counted, as are tintinweb children that persist their session (top-level children persist by default since 0.17.0). The [account lookup](#account-and-balance) reads a top-level child through its parent and excludes nested nicobailon runs as candidates; the spend walk deliberately includes both. `hook_env_keys` carries both root variables into the hook apply child, so the settled walk reads the same files.

`spend::parse` takes the session header's `cwd` from the first line as the entries' origin, and the resume cursor keeps it for appended lines. It counts five shapes:

| Entry | Usage read from | Model for price-book fallback |
| --- | --- | --- |
| `message`, role `assistant` | `message.usage` | `responseModel`, else `model` |
| `message`, role `toolResult` | `message.usage` | none |
| `compaction`, `branch_summary` | top-level `usage` | none |
| `usage`, every kind (cache warming included) | top-level `usage` | top-level `model` |

A present non-negative `usage.cost.total` is authoritative, zero included, which is the usual figure under a subscription login. Without one, a token-bearing entry with a model is priced from the price book. An unpriced model contributes its tokens at zero and joins the price chase, and an entry with no model contributes tokens only. Any excess of `totalTokens` over the itemized parts is folded into output. Token and cost fields accept JSON numbers or numeric strings, and a malformed nested value reads as absent instead of dropping the entry. Pi sessions are single files, so the parser needs no cross-file dedup.

Historical tool statistics count the `toolCall` blocks inside the assistant entry that owns the usage. The extension's `tool_call` traffic is not a durable record and is never counted. Live, every `tool_execution_end` name feeds the session's tool map.

## Known gaps

`rimz coverage --wiring` prints the declared concern grid ([adapter.md](./adapter.md#declared-coverage)). The gaps below have a reason worth recording.

- Install ignores `PI_CODING_AGENT_DIR`, so a user who moves Pi's config root gets an extension Pi never loads ([Install](#install)).
- The account probes, the usage probe, and the spend roots read the process environment, not the caller's login map, so a named Pi login sees the ambient account.
- Pi has no native tool-permission policy or questionnaire. A permission-gate extension's dialog raises a generic question wait on Pi 0.84.4 and later, with no permission kind and no `rimz answer` support.
- Subagents stay partial: nicobailon foreground children stay invisible until the user lists the extension path for them, and a reused child row keeps its first verdict.
- Launch reminders need the current extension, so `pi --no-extensions` or an older extension launches without them.
- Idle stays partial because the shared concern also covers an idle-timeout notification nudge, which Pi does not expose. Pi's own `agent_settled` boundary is wired.
- A run whose setup fails after `before_agent_start` may never fire `agent_settled` ([pi-reference.md](../../externals/agent-adapter/pi-reference.md#extension-events)), and RimZ then receives no turn end for it.
- Background tasks, background shells, and remote control are unsupported. Pi has no native background-task signal and no remote-control surface.
- Older Pi releases lose features instead of failing. Without `getBranch()` there is no resume hydration, before 0.80.4 the turn ends at `agent_end`, and before 0.84.4 extension dialogs raise no wait.

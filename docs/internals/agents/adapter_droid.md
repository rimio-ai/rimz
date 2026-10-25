# Droid adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Binding and session death are in [instances.md](./instances.md). Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md), and the raw upstream protocol is in [droid-reference.md](../../externals/agent-adapter/droid-reference.md).

Factory Droid reports its lifecycle through native command hooks in `~/.factory/settings.json`, and RimZ enriches the card from Droid's private session files. The adapter drives only the stock interactive TUI. A supervised `rimz agents droid -p` launches that TUI with a positional prompt and completes from the native `Stop` hook. RimZ does not speak the `droid exec` JSON-RPC transport, and it never writes Droid's `statusLine` or persistent autonomy settings. The code lives in [`agents/adapters/droid/`](../../../crates/rimz/src/agents/adapters/droid/mod.rs):

| Module | Owns |
| --- | --- |
| `mod.rs` | `DROID_DESCRIPTOR` (the `static AgentSpec`), the coverage declarations, the hook catalog `DROID_HOOKS`, the hook command, `DroidAdapter::decode_hook`, `DroidAdapter::hook_ingress`, and `DroidAdapter::local_context_refresh` |
| `payloads.rs` | the typed `SessionStart`, `UserPromptSubmit`, and `Notification` payloads, with `DroidSessionStart::compacted_session_id` and `DroidNotification::ask_kind` |
| `process.rs` | `hook_process_disposition`, which tells the stock TUI from its internal worker |
| `install.rs` | `MANAGED_SOURCE`, the `settings.json` hook merge, and `droid_settings_path` |
| `transcript.rs` | the version-2 session-file reader: history, the assistant suffix, the final answer, the settings snapshot, and the `AskUser` projection |
| `config.rs` | the read-only `custom:` model catalogue (`resolve_custom_model_from_cwd`) |
| `spend.rs` | exact-model pricing of the settings snapshot (`parse`, `live_cost`) |
| `tests.rs` | install and uninstall, lifecycle mapping, compaction routing, notification types, the transcript and settings readers, streaming, pricing, and launch argv, over the Droid 0.170.0 fixture pair in `tests/fixtures/` |

## Hooks and lifecycle

`decode_hook` maps each native event to one [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs). The payloads are in [droid-reference.md](../../externals/agent-adapter/droid-reference.md#hook-input).

| Native event | Signal | Notes |
| --- | --- | --- |
| `SessionStart` (`startup`, `clear`) | `Registered`, origin `Fresh` | |
| `SessionStart` (`resume`) | `Registered` | Registers the hook's `session_id`, which need not echo the resume argument ([session identity](../../externals/agent-adapter/droid-reference.md#session-identity-and-resume)). |
| `SessionStart` (`compact`) | `CompactionEnded { auto: None }` | Fires only for a `/handoff` successor. See [Compaction](#compaction). |
| `UserPromptSubmit` | `TurnStarted` | The sanitized prompt becomes the task and the prompt. |
| `PostToolUse` | `ToolUsed { mutates, edits }` | `Create`, `Edit`, and `ApplyPatch` edit. `Execute` mutates without editing. |
| `Stop` | `TurnEnded { errored: false, parked_on_background: false }` | Droid sends no failure or background-work field. The stall window and the displayed-status ladder surface a failed turn. |
| `PreCompact` | `Compacting` | Opens the bracket. The decoder ignores the payload's `trigger`. |
| `SessionEnd` | `Ended` | Stamps `ended_at` at once. The retained resumable row is hidden. |
| `Notification` (`permission_prompt`) | `AwaitingInput { Permission }` | The prompt text is the ask's detail. |
| `Notification` (`elicitation_dialog`) | `AwaitingInput { Question }` | The `AskUser` question UI. |
| `Notification` (`idle_prompt`) | `TurnInterrupted` | Sent only after the user stops the agent. It settles the turn that `Stop` never closes. |
| `Notification` (no type, or another type) | none | The event records nothing. |

An observation carries the payload's `transcript_path` (a handoff close excepted, see [Compaction](#compaction)), and `transcript::identity` reads model and effort from the settings snapshot, else from the newest visible assistant record. `SubagentStop` carries a task name and result but no child identity, so RimZ does not install it and draws no child rows.

### Asks

A `Notification` with a `notification_type` arrives only when a prompt needs the user, so permission and question prompts reach `rimz asks` and the next `PostToolUse` returns the card to running. The payload carries no request id, options, or answer channel. The asks are keyless ([edges](./model.md#edges)), `rimz answer` is unsupported, and the user answers in Droid's pane.

The spec-mode exit approval uses the same tool confirmation and the same `permission_prompt`, so a plan approval reads as a permission ask. A Droid release whose `Notification` carries no type raises no hook ask. On every release, the local refresh also projects an active `AskUser` call from the transcript as a display-only native wait (see [The transcript](#the-transcript)), which covers the question case for such a release.

### Compaction

`PreCompact` opens the [compaction bracket](./model.md#the-compaction-bracket) for every compaction, and how it closes depends on the command:

- `/compress` compacts in place. The session id stays, and Droid fires no closing hook. [`agents/lifecycle.rs::step`](../../../crates/rimz/src/agents/lifecycle.rs) lifts the compacting head on the next lifecycle signal, and `AgentState::is_compacting` expires it after `COMPACTING_WINDOW_SECS` (90 s). Until then the card reads compacting and `message/deliver.rs` holds the agent's queued messages.
- `/handoff` starts a successor session, which fires `SessionStart` with `source: compact` and a `previous_session_id`. `DroidSessionStart::compacted_session_id` returns that id, and the close routes to it. The observation carries no `transcript_path`, so the compacted row keeps its own transcript. The successor registers on its next prompt or tool signal. A `compact` payload without `previous_session_id` closes the bracket on the reported `session_id`.

### Hook replies

The neutral reply is empty stdout with exit status zero, so Droid's own UI keeps control. The one non-empty reply is a supervised deadline rung on `PostToolUse` (`hook_context` is `HookContextReply::HookSpecificOutput { event_name: false }`), which writes `{"hookSpecificOutput":{"additionalContext":"..."}}`. Every other event takes no deadline context.

### Install

`MANAGED_SOURCE` merges one block per event in `DROID_HOOKS` into the `hooks` key of `~/.factory/settings.json`. The events are `SessionStart`, `UserPromptSubmit`, `PostToolUse`, `Notification`, `Stop`, `PreCompact`, and `SessionEnd`. `RIMZ_DROID_SETTINGS`, when non-empty, replaces the whole path, and without it an unset `HOME` fails the install.

Each block runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source droid` with a 2-second timeout and no matcher, so every completed tool reaches the classifier. The command resolves `rimz` through `PATH`, the shared shape of every built-in installer. Droid also loads hooks from `~/.factory/hooks.json` ([hook configuration](../../externals/agent-adapter/droid-reference.md#hook-configuration)), which RimZ neither reads nor writes (see [Known gaps](#known-gaps)).

### Runtime owner

The stock TUI spawns an internal `droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc` worker, and both processes run the same hooks ([processes](../../externals/agent-adapter/droid-reference.md#processes)). `DroidAdapter::hook_ingress` takes the emitter pid from `cli/hooks/owner.rs::hook_agent_pid` (the `RIMZ_AGENT_PID` the hook command exports, else an ancestor walk) and classifies that process through `process::hook_process_disposition`:

| Emitter | Disposition | Result |
| --- | --- | --- |
| A `droid` process whose argv has no `exec` subcommand | `StockTui` | `Ignore(DroidStockTui)`: `rimz hooks feed` exits 0 before it reads stdin or opens the store |
| A stream-JSON-RPC `droid exec` worker with a non-`exec` `droid` ancestor within 32 levels | `InternalWorker` | accepted, and the ancestor TUI's pid becomes the session's [`RuntimeOwner`](./instances.md#session-death) |
| A directly launched `droid exec`, any other process, or unreadable or cyclic ancestry | `Standalone` | accepted, owned by the emitter itself |
| No pid resolved | none | accepted with no owner pid |

The worker's stream is therefore the one canonical source, and liveness follows the pane's TUI. Classification fails open: process metadata RimZ cannot read never suppresses a hook.

## Launch and resume

Launch argv comes from `DROID_DESCRIPTOR.launch`. Interactive Droid accepts unknown options and can fold their values into the positional prompt ([argument parsing](../../externals/agent-adapter/droid-reference.md#argument-parsing)), so RimZ emits only flags the interactive TUI takes and refuses the rest.

| Launch input | Droid argv or behaviour |
| --- | --- |
| fresh launch | `droid -- <prompt>` |
| resume | `droid --resume <id>` |
| bare `--resume` | `droid --resume`, Droid's own picker |
| fork | `droid --fork <id>` |
| manual compaction | `/compress <instructions>`, folded to one line |
| `droid`, `droid-ask` | no argument: Droid's configured autonomy and native permission UI |
| `droid-auto` | `--auto medium` |
| `droid-plan` | `--use-spec` |
| Yolo | no argument: `--skip-permissions-unsafe` exists only on `droid exec`, and RimZ does not rewrite persistent autonomy settings |
| launch reminder | `--append-system-prompt <text>` (`SystemTextChannel::TextFlag`) |
| `system-prompt-file` and `append-system-prompt-files` | refused when the launch is finalized (`LaunchFinalizeError::UnsupportedSystemPrompt` in `harness/plan.rs`): Droid's CLI only appends, so it cannot replace the system prompt |
| `model`, `effort` profile fields | refused (`UnsupportedField`): interactive Droid has no `--model` or `--reasoning-effort` flag |
| `auto-compact` profile field | skipped with a warning at launch |
| fast mode | skipped with a warning and the alternative `toggle /fast inside the session` |

A user who wants Droid's append-only behaviour puts `--append-system-prompt-file <path>` in raw profile `args`. Model and effort are chosen in the session (`/model`, Tab) or through `droid exec`. Interactive `--settings <path>` can carry `model`, `reasoningEffort`, and `autonomyLevel`, but RimZ has no typed contract for such a side file (see [Known gaps](#known-gaps)).

## Context and transcript

Droid writes two private files per session, `<session-id>.jsonl` and its sibling `<session-id>.settings.json` ([session files](../../externals/agent-adapter/droid-reference.md#session-files)). Factory publishes no schema for either, so the reader pins the transcript format and abstains on drift.

### The transcript

`transcript.rs` reads a transcript only when its first record is `session_start` with `version: 2`. Any other version, a missing header, or a malformed parent graph reads as no history. Every hook's `transcript_path` is authoritative and carried forward.

- Complete history (`parse_messages`) starts at the physically latest visible message and follows its `parentId` chain, because rewinds and branches make the file a tree.
- Streaming (`parse_assistant_suffix`) emits newly appended visible assistant records in physical order, because a suffix lacks its ancestors.
- Only visible user and assistant `text` blocks reach history. A record with `visibility` (`llm_only` context, `user_only` UI rows) or a `hookEventName` (hook audit rows) is skipped, and so are thinking, tool, and document blocks.
- On `Stop`, `last_assistant_message` tail-reads the latest visible assistant answer. It feeds the durable transcript, the supervised result, streaming, and message-reply capture.
- The `AskUser` projection: when the latest visible record is an assistant `tool_use` named `AskUser`, the refresh sets a `NativeWait` settle at that record's timestamp. A later user, tool-result, or assistant record clears it.

### The settings snapshot

The settings snapshot supplies `model`, `reasoningEffort`, the root session's cumulative `tokenUsage`, and `lastCallTokenUsage`. When it lacks model or effort, the newest visible assistant record's `modelId` and `reasoningEffort` fill them. One header read gives the session cwd for custom-model resolution, and one tail projection gives both the assistant identity and the `AskUser` leaf.

RimZ keeps the root counters (input, output, cache creation, cache read, thinking) as session-lifetime usage. The displayed cumulative line folds cache creation into input and thinking into output, and keeps cache reads separate. `inclusiveTokenUsage` (child sessions) and `factoryCredits` are ignored. The last-call input and cache-read counts give current-context occupancy against the resolved capacity, so the `▣` gauge and the `▤ · ◌ ◍ ↘ ↗` composition render together and feed the smart-compaction thresholds. Droid 0.218.2 writes no last-call cache-creation count, and an older snapshot's count still folds in.

### Custom models

A `custom:` selector names a user-configured model, and `config.rs` rebuilds the catalogue Droid loads to resolve it. The module follows its own reading of the Droid 0.218.2 bundle. The reference records the selector grammar and the load filter ([custom models](../../externals/agent-adapter/droid-reference.md#custom-models)), but not the merge order or the provider rejection. The projection reads `id`, `displayName`, `model`, `index`, and `maxContextLimit`, the presence of `provider`, `baseUrl`, and `bedrock`, and whether `apiKey` is a template placeholder. Credentials and endpoints never cross the boundary.

An entry is loaded only when:

- it has a `model` and a `provider`
- it has a `baseUrl` or a `bedrock` object
- its `apiKey` is not a placeholder (`YOUR_API_KEY`, `YOUR_OPENAI_API_KEY`)

A provider outside Droid's four wire protocols (`anthropic`, `openai`, `generic-chat-completion-api`, `bedrock-converse`, with case and underscores folded) makes Droid reject the whole settings file, so RimZ abstains. A malformed settings file that is present also abstains. A rejected or unreadable legacy `config.json` is skipped.

Each file names its loaded entries `custom:<name>-<n>`. The name is `displayName` (else `model`) with each whitespace run folded to one hyphen, and `n` counts earlier entries of that name in the same file. A stored `id` wins over the generated one. The catalogue is assembled in this order:

1. In each folder, `settings.local.json` replaces its sibling `settings.json` list.
2. The folder's legacy `config.json` (`model_display_name`, snake_case keys) appends each entry whose model and id are both new.
3. The project folder (`<cwd>/.factory/`) comes before the user folder, and a repeated id is dropped.

The user folder is `droid_settings_path`, so `RIMZ_DROID_SETTINGS` moves it too. A selector resolves by the first match:

| Order | Match |
| --- | --- |
| 1 | an exact catalogue id |
| 2 | the pre-ordinal form `custom:<name>-<index>`, where `index` is the stored `index` or the entry's position |
| 3 | a unique entry of that name |
| 4 | an entry whose raw `model` equals the selector body |

A resolved entry supplies the canonical model id, the display name, and a positive `maxContextLimit`. An unresolved selector still gets a display label from `model_display::display_factory_custom_selector`, but no pricing identity, capacity, or dollars.

### Refresh source

`local_context_refresh` reads both files behind one stat gate. It publishes the JSONL as the context record's `transcript_path`, with the settings file's stat and the JSONL as its companion (`TranscriptCompanionStat`), so a write to either file changes the stat. JSONL writes surface the `AskUser` open and close edges faster than the producer tick. A write to the settings file alone is caught by the tick, and later hooks are the backstop when Factory writes just after `Stop` or a wakeup is missed.

The turn-end cost fallback (see [Cost](#cost)) publishes the settings path with a settings-only stat instead. The next refresh accepts either path, sees a different stat, and publishes the JSONL again.

## Account and balance

The adapter keeps the default `AccountCapability`, so the card shows no account panel and no rate-limit windows. Droid's one auth surface is `droid doctor --json --auth`, which RimZ does not call, and Droid publishes no plan, quota, or account usage ([authentication and usage](../../externals/agent-adapter/droid-reference.md#authentication-usage-and-diagnostics)).

## Cost

Droid writes no USD figure, so RimZ prices the root session's cumulative counters itself. `spend::price_exact_model` uses only an exact price-book row for a built-in model id or a resolved custom model, and a fuzzy family match never sets a rate. Thinking tokens price as output, cache creation as cache write, and cache reads as cache read. The card renders the result as ordinary dollars. Capacity comes from a positive `maxContextLimit` first, then from the exact row's `max_input_tokens`.

Two paths write the card cost:

| Path | When | What it reads |
| --- | --- | --- |
| `local_context_refresh` | every hook, tick, or watch refresh whose stat changed | the counters and model it already parsed, through `spend::live_cost` |
| the turn-end fallback, `cli/hooks/lifecycle/context.rs::supplement_realtime_cost` | `Stop`, when that refresh set no cost (an unchanged stat, or no exact price) | the settings snapshot again, through `DroidAdapter::session_transcript` and `spend::parse`, which repeat the header read and the custom-model resolution |

A refresh that runs without an exact price clears the card cost. A price-book change alone does not pass the stat gate, so the card keeps its old figure until the next file change or the next `Stop`, where the fallback reprices it with the current book.

The value has `Session` coverage, because the counters are cumulative for the root session. It enters cockpit spend and live agent and room budget enforcement. The adapter declares no `spending_sources`, so the value stays out of historical workspace and provider aggregates, `rimz stats`, provider and account totals, and account-day caps ([spending.md](./spending.md)). Account spend is unsupported.

## Known gaps

`rimz coverage --wiring` prints the declared wired, partial, and unsupported grid ([declared coverage](./adapter.md#declared-coverage)). These gaps have a reason worth recording.

- The adapter stays on the stock interactive pane. RimZ does not speak the worker's JSON-RPC transport. A direct supervised `droid exec` integration could add keyed permission and question requests with answers, structured model and effort updates, failed outcomes, and identified mission workers. It needs a process-lifecycle and ask-answer path larger than this adapter.
- Launch-time model and effort selection waits on the same work. `AgentDefinition` has no typed renderer or ownership contract for a provider settings file that mixes several launch fields, which interactive `--settings <path>` would need.
- In-place compaction has no close. The bracket ends only on the next lifecycle signal or after 90 s. Droid's private `compaction_state` session record marks completion, but Factory publishes no durability contract for it.
- A handoff close cannot register the successor. `AgentDefinition::decode_hook` returns one observation per event, so the event closes the old bracket and later activity registers the new row. Registering both needs a kind-agnostic multi-observation or session-replacement abstraction.
- Hook commands resolve `rimz` through `PATH`. Factory recommends an absolute hook command because the hook cwd can change. Resolving and migrating the running RimZ executable belongs in the shared hook-install abstraction, so that every built-in keeps the same trust hashing, drift detection, and upgrades.
- `~/.factory/hooks.json` is outside the install. The reference records that Droid loads it, but not how its lists combine with the `settings.json` hooks, or whether Droid's own hook saves move entries there. If they do, a RimZ reinstall or uninstall that edits only `settings.json` misses them. Owning both files needs a multi-file install source.
- A plan approval reads as a permission ask, because the spec-mode exit shares the tool confirmation and the `permission_prompt` notification.
- Whether `/compress` on an idle session shows a confirmation dialog first is unverified. If it does, the RimZ compaction verb waits on the user.
- No logged-in live-pane check covers handoff session replacement, typed notifications from the internal worker, or the first-turn posture of each permission suffix.

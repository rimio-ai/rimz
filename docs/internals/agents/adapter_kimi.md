# Kimi Code adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The raw upstream protocol is in [kimi-reference.md](../../externals/agent-adapter/kimi-reference.md).

Kimi Code is a built-in adapter, `KimiAdapter` in `crates/rimz/src/agents/adapters/kimi/`, with its spec in `KIMI_DESCRIPTOR`. Kimi runs in its stock interactive pane. Command hooks in Kimi's `config.toml` carry the lifecycle edges and the blocking waits, and the durable per-agent `wire.jsonl` record log supplies the transcript, model, tokens, child agents, and spend. Only the registry names the concrete adapter. Everything Kimi-specific stays inside the module, and other code reads it through the provider-neutral capabilities.

The adapter maps [`MoonshotAI/kimi-code`](https://github.com/MoonshotAI/kimi-code) (the TypeScript Kimi Code CLI) and nothing older. The retired Python `MoonshotAI/kimi-cli` is a different product: its `$KIMI_SHARE_DIR` and `~/.kimi` data root, `--afk` flag, enveloped Wire messages (`{timestamp,message:{type,payload}}`), and tool names are unsupported. Process binding accepts the executable name `kimi` and Kimi Code's runtime process title `kimi-code` (`process_names`), and binary discovery also searches `~/.kimi-code/bin`, where the official installer puts `kimi` (`extra_bin_dirs`).

| File | Owns |
| --- | --- |
| `mod.rs` | the spec, the hook catalog `KIMI_HOOKS`, hook decoding and the lifecycle mapping, launch argv, the context refresh and its gauge |
| `install.rs` | the config path, the `[[hooks]]` merge, entry validation, and the version floor |
| `payloads.rs` | the tolerant hook payload `KimiHookPayload` |
| `wire.rs` | the session lookup, the record decoder `decode_event`, the torn-safe `WireSnapshot`, attribution, and the context-fill folds |
| `subagents.rs` | child derivation (`derive_at`) and child-fired hook suppression (`child_fired`) |
| `transcript.rs` | the visible conversation fold and the latest assistant answer |
| `account.rs` | the credential slot and the account probe |
| `oauth_usage.rs` | the managed quota probe and the Booster projection |
| `spend.rs` | the spend parser and the configured default model |

## Data root

The data root is `$KIMI_CODE_HOME`, else `$HOME/.kimi-code` (`wire::kimi_home_from`). `LaunchCapability::config_home` resolves it from the login env its caller passes, but every other read on this page resolves it from the RimZ process env through `wire::kimi_home`: the hook install, the account and quota probes, the spend sources, and the session lookup. Kimi therefore has one data root per RimZ process, whatever login a caller names, and `KIMI_CODE_HOME` is absent from the provider-home key list that `cargo xtask invariants` keeps out of process-env reads.

## Hooks

### Install

Install merges `[[hooks]]` entries into one TOML file, `install::config_path`: `$RIMZ_KIMI_CONFIG` when it is non-empty, else `config.toml` under the data root. Every installed entry runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source kimi`, with matcher `.*` on the tool and permission events and a timeout of 10 seconds (4 for `SessionEnd`). An entry is RimZ's when its command contains `rimz hooks feed --source kimi` (`RIMZ_COMMAND_MARKER`). Install strips owned entries and appends the canonical set, so a reinstall is idempotent and every foreign entry stays in place. Uninstall strips owned entries the same way.

Two refusals guard the merge, and both apply to install and to preview:

- Version floor. A detected CLI below 2.1.1 refuses with `kimi upgrade -y` as the fix (`install::version_blocker`), and the same text is the `install_blocker`. An unknown or unparseable detected version passes.
- Entry validation. Before any rewrite, `install::validate_hooks` checks every existing entry against Kimi Code's 20-event catalog and its four-field schema (`event`, `matcher`, `command`, `timeout`, with a timeout from 1 to 600). Kimi Code ignores every hook while one entry is invalid, so an invalid foreign entry refuses with the config path, the entry number, and the fix, and leaves the file untouched. The installed check (`install::installed`) reads false for the same reason.

The catalog accepts `SubagentStart`, `SubagentStop`, `TaskStarted`, `UserPromptQueued`, and `SessionHeartbeat` in foreign entries, but RimZ installs none of them.

### Event mapping

`KIMI_HOOKS` is the installed set, and `kimi_root_signal` maps each event. A progress event touches the activity heartbeat ([what a decoded event carries](./adapter.md#what-a-decoded-event-carries)).

| Kimi event | RimZ signal | Progress |
| --- | --- | --- |
| `SessionStart` | `registered`, binding the pane to `session_id` and the main wire path | yes |
| `UserPromptSubmit` | `turn_started`, with the prompt's text parts as the task | yes |
| `TurnStarted` | `turn_started`, except for origin `user` (`UserPromptSubmit` already opened the turn), `retry`, and `system_trigger` named `subagent` | yes |
| `PreToolUse` for `AskUserQuestion` | `awaiting_input(Question)`, unless `tool_input.background` is true. Every other tool records nothing | no |
| `PermissionRequest` | `awaiting_input(Permission)`, or `PlanApproval` for `ExitPlanMode` | no |
| `PermissionResult` | a non-mutating `tool_used` that closes the wait | yes |
| `PostToolUse` | `tool_used`. `Write` and `Edit` set edit proof, and `Bash`, `Write`, and `Edit` set mutation proof | yes |
| `PostToolUseFailure` | a non-mutating `tool_used` | yes |
| `Stop`, `Interrupt` | clean `turn_ended` | yes |
| `StopFailure` | errored `turn_ended` with a turn error | no |
| `PreCompact`, `PostCompact` | `compacting`, `compaction_ended` with the `manual` or `auto` trigger | no |
| `SessionEnd` | `ended` | no |
| `Notification` | no signal. Triggers a context refresh, which is how a terminal background task reaches the root | yes |

`PreToolUse` is blockable upstream and installs with matcher `.*`, so every tool call waits for one `rimz hooks feed` before Kimi runs it ([output and exit semantics](../../externals/agent-adapter/kimi-reference.md#output-and-exit-semantics)). A `Stop`, `StopFailure`, `Interrupt`, or `TurnStarted` that a child fired records nothing ([child-fired hooks](#child-fired-hooks)).

### Asks

A permission ask opens only when Kimi's policy resolves a call to ask. Policy-approved, mode-approved, YOLO-approved, and statically denied calls fire no `PermissionRequest` ([approvals](../../externals/agent-adapter/kimi-reference.md#approvals)). The wait's detail is the payload's `action`, and a question's detail is its first question text.

Kimi exposes no question hook. `PreToolUse` for `AskUserQuestion` opens the wait, and the correlated post-tool hook, an interrupt, or a turn or session close resolves it. A question with `background: true` starts a background task and leaves the turn runnable, so its `PreToolUse` opens no wait.

Every Kimi ask is keyless. `KimiHookPayload` drops the payload's `tool_call_id`, and no signal carries a native key, so any `tool_used` on a waiting row clears the wait, a parallel sibling tool's `PostToolUse` included ([edges](./model.md#edges)).

### Turn endings

`Stop` is blockable upstream. RimZ's hook writes empty stdout and exits 0, so a normal stop stays a clean boundary. A user interrupt skips `Stop` and fires `Interrupt`, and a fatal turn fires `StopFailure`. `kimi_turn_error` labels the failure from `error_message`, else `error_type`, capped by `cap_turn_error_label`, and classifies the label with `TurnErrorClass::classify_label`.

On `Stop` and `StopFailure`, `kimi_final_message` reads the main wire and returns the latest assistant message that follows the newest user prompt (`transcript::latest_assistant_from_records`). A stop whose turn produced no answer yet returns none, and never an answer to an earlier prompt.

### Child-fired hooks

A child agent's turn fires the same session-shaped `Stop`, `StopFailure`, `Interrupt`, and `TurnStarted` hooks as the main agent, with the root `session_id`. `subagents::child_fired` reads the main wire and suppresses the hook when the session has spawned a child (any `subagent.spawned` record) and either of two conditions holds:

- The main step is open. The newest of `step.begin`, `step.end`, `turn.prompt`, and `turn.ended` is a `step.begin`. An `llm.request` alone does not open a step. This rule covers all four events.
- The latest turn is already closed with a reason this hook does not report. A `turn.ended` follows the newest `turn.prompt`, and its reason is any reason for `Stop`, any reason except `failed` for `StopFailure`, and any reason except `cancelled` for `Interrupt`. This rule does not cover `TurnStarted`.

A positive check is confirmed by rereads after 10, 20, 40, 80, and 100 ms, and any reread that clears lets the hook through. An unreadable wire or an unresolved session directory also lets it through. `PreCompact` and `PostCompact` always pass, because a main compaction can overlap an open step.

## Subagents

Children come from the main wire's durable records, not from hooks. After each recorded root observation that carries a session id, the shared derive pass (`derive_subagent_lifecycle` in `crates/rimz/src/cli/hooks/lifecycle/mod.rs`) calls `KimiAdapter::derive_subagent_observations`, which reads `subagent.spawned`, `subagent.completed`, `subagent.failed`, and `subagent.cancelled` (`subagents::derive_at`).

- Identity. A child's id is `<session-id>:<subagentId>`, with the root `session_id` as its parent. A `subagentId` that is not one normal path component is skipped, since it names the child's directory.
- Launch facts. A start carries the spawn's `subagentName`, sanitized `description`, `model`, and `thinkingEffort`. A missing model or effort falls back to the attribution in the child's own `agents/<subagentId>/wire.jsonl`, which is also the child's transcript path.
- Settlement. The fold keeps each child's latest spawn and its terminal record. `subagent.failed` settles errored, and `subagent.completed` and `subagent.cancelled` settle clean. Completion usage becomes the child's run-token total and is never charged as spend.
- Idempotence. The derive pass creates a row only for a child it has not seen, under a parent row that exists, and settles a row only once. A child respawned after it finished keeps its finished row.

`SubagentStart` and `SubagentStop` stay uninstalled, and `kimi_observation` drops them from a stale installation, because `SubagentStop` fires only on success. Background task state lives in each agent's `agents/<agent-id>/tasks/` tree, and the root learns of a terminal task only through `Notification`. The parent's transcript, model, and context tokens are main-wire only. Spend includes every child ([cost](#cost)).

## Launch and resume

The launch spec maps RimZ's permission modes onto Kimi's flags. What each Kimi mode allows is upstream behaviour ([permission modes](../../externals/agent-adapter/kimi-reference.md#permission-modes)).

| RimZ mode | Kimi argv |
| --- | --- |
| `ask` | no flag. Kimi starts in its own default: `default_permission_mode` from `config.toml`, or a resumed session's saved mode |
| `auto` | `--auto` |
| `yolo` | `--yolo` |
| `plan` | `--plan` |

| Surface | Mapping |
| --- | --- |
| Resume | `kimi --session <id>`. Bare `--resume` opens the native `kimi --session` picker |
| Fork | unsupported. Kimi forks only by copying a session without opening it ([resume and fork](../../externals/agent-adapter/kimi-reference.md#resume-and-fork)), so `rimz agents fork` refuses |
| Compaction | `/compact` sent through the pane, with no instruction text |
| Model preset | `--model` or `-m` |
| Fast preset | unsupported, with the fix `use --model kimi-for-coding-highspeed` |

A supervised run uses `KimiAdapter::launch_command`: `kimi [args] --prompt <prompt> --output-format stream-json`. Kimi rejects a permission flag in prompt mode, so the builder drops `--auto`, `--yolo`, and `--plan` from the passthrough arguments when it adds a prompt. Prompt mode applies Kimi's `auto` policy. The stream's lines are in [prompt mode](../../externals/agent-adapter/kimi-reference.md#prompt-mode).

## Context and transcript

### Session lookup

A session lives at `sessions/<bucket>/<session-id>/` under the data root, and `wire::session_dir` resolves it without reproducing Kimi's bucket-key algorithm:

1. Index. Parse every line of `session_index.jsonl` that names the session id, and take the latest line whose `sessionDir` validates. Kimi compacts this file, and its `workDir` can be stale.
2. Scan. With no valid index line, try `<bucket>/<session-id>` under every bucket in `sessions/`, and take the newest by modification time of the directory, its main wire, and its `state.json`.

A candidate validates when it canonicalizes under `sessions/`, its basename is the session id, and its `state.json` parses as an object. When the caller supplies a working directory, `state.json` must also record it: `cwd` in metadata version 2, or `workDir` in a session Kimi has not rewritten yet. The `SessionStart` binding and the final-message read pass the hook payload's `cwd`. The context refresh, the derive pass, and the child-fired check pass none.

`KimiAdapter::session_transcript` skips the lookup when the sidecar already holds a path that is an existing `<session-id>/agents/main/wire.jsonl`. The `SessionStart` binding and every changed-stat refresh write that path, so the shared filesystem watcher follows the wire even while the index is stale.

### The wire

The main record log is `agents/main/wire.jsonl`, and each child writes `agents/<agent-id>/wire.jsonl`. Records are top-level JSON objects tagged by `type`, opening with a metadata record such as `{"type":"metadata","protocol_version":"1.5",...}` ([file format](../../externals/agent-adapter/kimi-reference.md#file-format)). `wire::decode_event` maps the types the adapter reads and returns `Unknown` for every other type, so new record types and fields pass through.

A context refresh reads the main wire once per changed stat. `WireSnapshot::read` returns every complete record, which the cost fold consumes, and marks a record-aligned logical 64 KiB tail, which the context and attribution folds consume. The tail extends back to the start of an oversized newest record. The stat behind the gate is the main wire's, plus a companion stamp of every child wire's summed length and newest mtime, so a growing child invalidates the refresh.

The folds read these records:

| Records | Consumer |
| --- | --- |
| `turn.prompt`, `turn.steer` with a `user` origin | transcript: the sanitized human turns |
| `context.append_loop_event` steps and text parts | transcript: assistant steps, without thinking or tool plumbing |
| assistant-role `context.append_message` | transcript: explicit hook or block output. User and injected context messages are ignored |
| `config.update`, `llm.request` | attribution: display alias, provider and canonical model, thinking effort |
| `usage.record` | cost, and the newest turn-scoped record as the current-usage split |
| `step.end.usage`, `context.clear`, `context.apply_compaction` | the context fill |
| `subagent.*`, `turn.ended`, `step.begin`, `step.end`, `turn.prompt` | child derivation and the child-fired check |

The log also carries approval, interaction, compaction-bracket, retry, and task records ([other record types](../../externals/agent-adapter/kimi-reference.md#other-record-types)). No fold reads them: hooks own turn, approval, and question edges, and pane and process liveness reconcile a missed hook.

### Model and the context gauge

The model is the effective alias: the latest `config.update` or `llm.request` `modelAlias`, else the latest `llm.request` model, else the agent's model hint, else `default_model` from `config.toml`. `wire::normalize_model_alias` strips one leading `kimi-code/` wrapper and nothing else. The thinking effort is the latest non-empty `thinkingEffort` from either record.

Kimi's stock pane does not persist its live `agent.status.updated` ratio, so `wire::latest_context_tokens` derives the fill from boundaries in tail order:

- A `step.end` with nonzero usage sets the fill to the sum of its four fields (`inputOther`, `inputCacheRead`, `inputCacheCreation`, `output`). A zero-usage step keeps the earlier value.
- `context.clear` sets the fill to 0.
- `context.apply_compaction` sets it to `tokensAfter`.

When a clear or a compaction is newer than the latest counted step, the fill also publishes as `current_context_tokens` (`wire::context_replaced_since_step`), because the tail still holds the older step's split and the scalar must outrank it as the card's numerator. A step's own count includes its output and matches that step's split, so it publishes no scalar.

The window is `max_context_size` from `[models.<alias>.overrides]`, else from `[models.<alias>]`, in `config.toml`, else 262,144 tokens. With no context boundary in the tail, the refresh publishes the shared unknown-fill, zero-usage sentinel under `LocalTokenPatch::PreserveEstablished`, so an established meter holds its value ([reading rules](./adapter.md#reading-rules)).

The card's session preview is the `title` in the session's `state.json`. `wire::session_title` drops Kimi's pre-prompt `New Session` placeholder, so a new session shows no preview until Kimi titles it from the first prompt. A rename replaces the title.

## Account and balance

The account and the quota are kind-wide: one managed Kimi login per data root ([account scope](./providers.md#account-scope)). Both probes read the credential slot that the `oauth.key` of `[providers."managed:kimi-code"]` in `config.toml` names (`ManagedProvider::credential_name`). `oauth/kimi-code` is `credentials/kimi-code.json`, and a scoped key, such as the one a `global` (kimi.ai) login writes, is `credentials/kimi-code-env-<hex>.json`. A missing entry, or a key with characters other than ASCII letters, digits, `-`, and `_`, reads the default slot. Token bytes never enter output, logs, diagnostics, or hashes, and RimZ leaves token refresh and credential writes to Kimi Code.

The account probe (`account::probe`) reports a managed login, plan `Code`, when the file holds a non-empty refresh token or an access token whose `expires_at` is in the future. The credential file's mtime is the account's credential stamp.

The quota probe (`oauth_usage::probe`) has three guards, in order:

1. Base. The managed base resolves as Kimi Code resolves it: `$KIMI_CODE_BASE_URL`, then the `managed:kimi-code` `base_url`, then the mainland default. Only `https://api.kimi.com/coding/v1` and `https://api.kimi.ai/coding/v1` map to a usage URL. Any other base makes the probe unavailable before it reads the token, so a custom endpoint never receives the managed credential.
2. Token. The access token is used only when its numeric `expires_at` is more than 60 seconds away.
3. Request. A GET to the region's `/coding/v1/usages` sends `Accept: application/json` and the bearer, with redirects refused (`oauth_http_get`). A 404 reads as unavailable.

`parse_response` maps the `usages` block onto windows:

| Key | Window |
| --- | --- |
| `limit_5h` | 300 minutes |
| `limit_7d` | 10,080 minutes |
| `limit_month_total` | scope `month_total`, label `mo`, no fixed duration |
| `limit_month_code` | scope `month_code`, label `mcd`, no fixed duration |

A finite numeric or numeric-string `used_ratio` is clamped to 0..1 and rounded to a percentage, and an entry without one is dropped. An invalid `reset_time` leaves the window without a reset. The plan is the body's `plan` or `planName`, else `Code`. A body with a Booster wallet and no `usages` block yields an observation with no windows. A body with neither fails as schema drift, even when it names a plan.

The Booster wallet projects onto `ExtraCredits` (`parse_booster_wallet`) only when its balance type is `BOOSTER` with a positive amount, and only when every declared currency on the monthly cap and monthly use is USD. `ExtraCredits` carries USD alone, so a non-USD wallet publishes nothing rather than a mislabelled figure. The cap and use publish only when `monthlyChargeLimitEnabled` is true.

An API-key provider can run Kimi Code without managed OAuth. The managed account and quota stay kind-wide, and transcript spend covers a custom provider only where model attribution and the shared price book give a local estimate. Provider quota and local dollars stay separate facts.

## Cost

Spend comes only from `usage.record`. `spend::fold_records` walks records in order, keeps the latest `llm.request` as attribution in the cursor state, and prices each usage record from the first name the price book knows:

1. The record's own `model`.
2. The latest `llm.request` as `<provider>/<model>`.
3. The latest `llm.request` `model` alone.

With none priced, the record stays visible at zero cost under the request's normalized alias, else the record's model, and enters the shared unknown-model refresh path. The adapter never guesses a model such as K2.5. A record with no timestamp, or with no model label at all, is skipped. Every record counts toward session spend whatever its `usageScope`, full-compaction requests included. Only the turn scope also marks a record as the current turn's split.

Every spend consumer reads the main wire plus every `agents/<id>/wire.jsonl` in the session (`wire::session_wires`), and every entry's thread id is the session directory name, so child spend lands on the parent session. Fleet discovery globs `sessions/*/*/agents/*/wire.jsonl` (`spending_sources_at`) and gives each wire its own byte-offset cursor. The live card cost prices the main wire from the refresh snapshot and each child wire in full. The turn-end fallback prices the same set through `session_spend_transcripts`, and its upward-only reconciliation keeps the parent's child-inclusive total. `subagent.completed.usage` is never priced, so a child's spend counts once. Child cards get no live dollar refresh.

## Known gaps

`rimz coverage --wiring` prints the current wired, partial, and unsupported grid. The gaps below have a reason worth recording.

- Structured answering stays in the Kimi UI. RimZ has no SDK, server, or ACP client that can return Kimi's typed answer, so `rimz answer` is unsupported.
- Asks are keyless. RimZ keeps no `tool_call_id`, so a parallel sibling tool clears an open wait ([asks](#asks)).
- Background parking is unsupported. `Notification` reports a terminal task, but it does not prove the active-task set, and a clean `Stop` does not include it. `bg` stays unsupported until the adapter joins the durable task state and can prove the parent parked with work in flight.
- Subagent coverage is partial. A child appears at the next recorded root hook after its spawn record lands. A resumed child stays closed, because the shared derive pass has no run discriminator. A cancellation, an upstream timeout included, shows as finished, not failed. A child's compaction shows on the parent card, and a child cancelled before `PostCompact` leaves the card compacting until the next root signal.
- The data root follows the RimZ process env, not the caller's login ([data root](#data-root)).
- CLI compatibility is a floor, not an identity probe. Install refuses a detected version below 2.1.1 and passes an unknown one, so a colliding `kimi` executable with an unparseable version is not rejected.
- Supervised exit codes stay generic. An ordinary prompt exits 1 for a failed, cancelled, or hook-blocked turn and for startup, auth, and option errors alike, so a nonzero exit carries no failure class RimZ could map. A `/goal` prompt passes through unchanged and exits 3 when blocked and 6 when paused, which RimZ also does not map ([prompt mode](../../externals/agent-adapter/kimi-reference.md#prompt-mode)).

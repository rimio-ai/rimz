# Copilot adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). Upstream facts, and the Copilot CLI captures they rest on, are in [copilot-reference.md](../../externals/agent-adapter/copilot-reference.md).

GitHub Copilot CLI reaches RimZ through two RimZ-owned artifacts. A whole hook file of native camelCase command hooks carries lifecycle truth and the blocking asks. A managed command statusline in Copilot's `settings.json` carries live context and cost. When the statusline is not installed, metadata-only OpenTelemetry `chat` spans stand in for it. The adapter lives in `crates/rimz/src/agents/adapters/copilot/`:

| File | Owns |
| --- | --- |
| `mod.rs` | `COPILOT_DESCRIPTOR`, the hook catalog `COPILOT_HOOKS`, `CopilotAdapter::decode_hook`, the statusline spec, room and launch environment |
| `hooks.json` | the installed hook file, embedded as `HOOK_SOURCE` |
| `payloads.rs` | tolerant hook payload decoding, tool-call normalization, the command key |
| `subagent.rs` | the parent-transcript fold that joins child hook sessions |
| `install.rs` | `CopilotManagedIntegration`: paired hook-file and statusline install, readiness, uninstall |
| `settings.rs` | read-only `disableAllHooks` and `defaultPermissionMode` precedence |
| `paths.rs` | Copilot home, hook, settings, transcript, and OTel path resolution |
| `transcript.rs` | visible messages and the final assistant message from `events.jsonl` |
| `statusline.rs` | the statusline payload projection and its session cost |
| `otel.rs` | the OTel fallback reader |
| `account.rs`, `account_usage.rs` | the login identity probe and the account usage request |
| `spend.rs` | the shutdown-history spend parser |

## Hooks and lifecycle

Every installed command has the form `RIMZ_AGENT_PID=$PPID rimz hooks feed --source copilot --event <event> || true`, with a 30-second `timeoutSec`. `$PPID` is the Copilot process that spawned the hook shell, so that process becomes the runtime owner without an `exec`. Ancestry fallback recognizes `process_names` `copilot` and `node`. The `|| true` makes the hook fail open when RimZ is missing, panics, or rejects its arguments. Decision stdout stays empty, so Copilot's own permission engine and question UI decide every ask. Only `postToolUse` may reply, with deadline `additionalContext` (`HookContextReply::TopLevel`).

| Native event | Channel | Signal | Detail |
| --- | --- | --- | --- |
| `sessionStart` | lifecycle | `TurnStarted` when `initialPrompt` is non-empty, else `Registered` | `source` `startup` or `new` marks `SessionOrigin::Fresh`. `resume` keeps the existing lineage |
| `userPromptSubmitted` | lifecycle | `TurnStarted` | the sanitized `prompt` becomes the row's task and prompt |
| `preToolUse`, matcher `ask_user` | awaiting-user (`Question`) | `AwaitingInput` | question text from the call's arguments, see [Tool payloads](#tool-payloads-and-the-post-tool-edge) |
| `preToolUse`, any other tool | lifecycle | `ToolUsed`, not mutating | only a hook file with no matcher sends these |
| `notification` with `notification_type` `elicitation_dialog` | awaiting-user (`Question`) | `AwaitingInput` | `message` is the detail. Every other notification type produces no signal |
| `postToolUse`, `postToolUseFailure` | lifecycle | `ToolUsed` | mutation and edit flags, and the command key |
| `permissionRequest` | awaiting-user (`Permission`) | `AwaitingInput` | the tool name is the detail, the command key keys the ask |
| `agentStop` | lifecycle | `TurnEnded` | the final message comes from the validated transcript |
| `preCompact` | lifecycle | `Compacting` | opens the compaction bracket |
| `errorOccurred` | enrichment | none | a non-recoverable error becomes a turn-error marker |
| `sessionEnd` | lifecycle | `Ended` | see [instances.md](./instances.md#session-death) |

A child session's `userPromptSubmitted` and `agentStop` become `SubagentStarted` and `SubagentStopped` once the child is joined to its parent ([Subagents](#subagents)). The native `subagentStart` and `subagentStop` hooks are not installed: the first carries no child ID, and the second repeats what the child's `agentStop` reports.

Hook payloads decode through `payloads::parse_payload`, which accepts camelCase and snake_case field names. A payload that fails to deserialize as a whole decodes as an empty payload, so it carries no session and produces no observation. Routing reads the worktree from `worktree_path`, else `cwd`.

### Prompt-seeded starts

A prompt-seeded launch fires `userPromptSubmitted` before `sessionStart`, and `sessionStart.initialPrompt` repeats the prompt ([captured order](../../externals/agent-adapter/copilot-reference.md#captured-hook-wire)). The adapter maps a non-empty `initialPrompt` to a second `TurnStarted` without copying its text, so the turn the first edge opened stays open. A promptless `sessionStart` stays `Registered`. The clear and rebirth flows depend on that registration reset.

### Tool payloads and the post-tool edge

Copilot sends tool detail in two shapes: the singular `toolName` and `toolArgs`, and a batch `toolCalls: [{name, args}]`. `CopilotHookPayload::normalized_tool_calls` presents both as one list. Either argument field may be an object or a JSON-encoded string, and `toolInput` is an alias. When a batch holds `ask_user`, that call wins selection for the blocking class and the question text. Mutation and edit flags aggregate across the whole batch.

The question text is the first string under `question`, `prompt`, or `message`, else the first string value of the arguments. The ask carries one question with no options and `multi_select` false. A `postToolUseFailure` whose `error` is a plain string still decodes, because `CopilotHookError` accepts a string or the `{message}` object `errorOccurred` carries.

Every `postToolUse` and `postToolUseFailure` clears Waiting, even for a read-only, unnamed, or partly malformed call. Copilot fires `postToolUse` as soon as an `ask_user` answer is selected or dismissed, so a post-tool edge proves the boundary completed. The one exception is a keyed permission ask, below.

`ToolClassification` names the mutating tools `bash`, `powershell`, `create`, and `edit`, and the editing tools `create` and `edit`. Classification reads the tool name only, never the command text.

### Keyed permission asks

No Copilot hook carries a tool call ID, and a `permissionRequest` narrows `toolInput` to `{command}` while the same call's post-tool `toolArgs` adds more fields. `NormalizedToolCalls::command_key` therefore keys a call by the tool name plus a SHA-256 digest of the `command` argument. It returns no key for a batch or for a call without a `command` string.

A `permissionRequest` with a key opens a keyed ask. While it is open, a post-tool edge with a different key leaves the row Waiting (`agents/lifecycle.rs`, "sibling tool completed while a keyed ask is open"). A post-tool edge with the same key, or with no key, clears it. The key is the command text, so two calls that run a byte-identical command clear each other's asks.

### Turn errors

`errorOccurred` is an enrichment because recoverable model-call retries happen mid-turn. Only `recoverable: false` with a non-empty message creates an `AgentTurnError`. The label is trimmed and capped at 500 characters, and the payload's millisecond `timestamp` stamps it, else the current time. `agentStop` stays turn truth and `sessionEnd` session truth.

### Compaction

Copilot has no post-compaction hook. `preCompact` opens the bracket, and the shared state machine closes it on the next lifecycle signal, with the display window as the backstop for a missed edge. The descriptor declares compaction partial for that reason.

### Subagents

Copilot runs the installed prompt, tool, and `agentStop` hooks for each child, under the child's own `sessionId`. That ID is the top-level `agentId` of the parent's `subagent.started` record. When the record has no `agentId`, the parent task call's `toolCallId` is the fallback key (`subagent.rs::correlated`). The child's `agentStop.transcriptPath` names the parent's file, which the [transcript path rule](#context-and-transcript) rejects, so a child publishes no transcript path of its own.

The first join happens in `cli/hooks/lifecycle/observe.rs::correlate_subagent_observation`, on a child's `TurnStarted` or `TurnEnded`:

1. Candidates. The candidates are the other Copilot rows in the same pane, at most 64 (`MAX_SUBAGENT_PARENT_CANDIDATES`). Above that bound the join is skipped.
2. Fold. `CopilotAdapter::correlate_subagent` folds each candidate's transcript tail (`subagent.rs::fold`), reading only the `tool.execution_start` (tool `task`), `subagent.started`, and `subagent.completed` records.
3. Match. Exactly one candidate must match the child ID. Two matches quarantine the join.
4. Publish. The child is stored under the candidate's root parent, with its name and task recovered from the `task` call and the start model.

Once joined, the stored parent carries every later child edge, and the child's `postToolUse` updates the child card. The child's `agentStop` precedes `subagent.completed`. The parent's next `postToolUse` or `agentStop` reads the completed records (`CopilotAdapter::spawned_subagents`) and reconciles the completion model and exact `totalTokens` onto the established child. That pass never changes the child's parent, task, verdict, or pane, and a repeated pass is a no-op.

A child's `permissionRequest` carries the parent `sessionId`, so the wait lands on the parent card. When the child's post-tool edge carries that ask's command key, Store appends a derived `SubagentAskAnswered` edge to the parent and clears the wait (`store/writer/lifecycle.rs`). Any other child call leaves it open. A denied child call fires no post-tool hook, so that wait clears at the parent's next hook, normally the `task` call's `postToolUse`.

## Install

Install writes two artifacts as one change, through `CopilotManagedIntegration`:

| Artifact | Path | Override |
| --- | --- | --- |
| Hook file | `$COPILOT_HOME/hooks/rimz.json`, else `~/.copilot/hooks/rimz.json` | `RIMZ_COPILOT_HOOKS` |
| Statusline | the `statusLine` object in `$COPILOT_HOME/settings.json`, else `~/.copilot/settings.json` | `RIMZ_COPILOT_SETTINGS` |

Both paths resolve from the RimZ process's own environment (`paths::hooks_path`, `paths::settings_path`). The login environment the caller passes is ignored, so install, uninstall, and readiness always act on the ambient Copilot home.

The hook file is whole-file managed through its first-line `_rimz_managed` marker (`agents/managed_source.rs`). Install reclaims a marked file and refuses an unmarked file at the same path. The statusline is one reversible command object. Install refuses any existing `statusLine` that is not a command-mode object with a non-empty command. It wraps a foreign command by storing the complete prior value under `_rimz_wrapped`, and it canonicalizes an already-managed statusline without nesting. The managed object keeps the `padding` and `refreshInterval` rendering options. At run time `rimz statusline feed` passes the provider payload byte for byte to the wrapped command and forwards its stdout. It forwards a numeric exit status, and a wrapped command killed by a signal reports success.

Preview builds both candidates and shows both diffs before either file is written. Install then writes the settings file and the hook file in that order, and a failed hook write restores both originals (`settings_json::commit_pair`). Uninstall strips only the managed statusline first, restoring a wrapped value, then removes only the marked hook file. A failed hook removal restores the original settings bytes.

Readiness (`installed`) requires the marked hook file and the canonical statusline command together. Either artifact alone counts as a partial install (`managed_artifacts_present`), which repair and uninstall still see. The hook file is upgradeable (`COPILOT_MANAGED_SOURCE` passes `true`): a marked file whose bytes differ from the embedded source is offered for refresh by `rimz start` and `rimz setup`. Declining keeps the old commands, including any that deny on error.

Read-only probes, readiness and the wrapped-command read included, parse `settings.json` as JSONC. Install and uninstall parse it strictly and refuse a file with comments or trailing commas, so they never discard them.

The marker is an unknown top-level key, so the hook file works only while Copilot tolerates unknown keys. If a release rejects them, the marker moves into the first hook entry's `env` overlay, as the comment above `HOOK_SOURCE` records.

## Settings checks

Two Copilot settings can silence RimZ or the ask posture, and `settings.rs` resolves each one independently. A key's value comes from the first file that sets it:

1. The repository `.github/copilot/settings.local.json`.
2. The repository `.github/copilot/settings.json`.
3. The user `settings.json`: `RIMZ_COPILOT_SETTINGS`, else the login environment's `COPILOT_HOME`, else `HOME/.copilot`.

The repository root is the nearest ancestor of the cwd that holds a `.git` file or directory, else the cwd itself. Files parse as JSONC, and a missing, unreadable, or malformed file counts as unset. This matches upstream's order for these files ([configuration directory](../../externals/agent-adapter/copilot-reference.md#configuration-directory)). The probe does not read managed settings, environment variables, or flags, which upstream ranks around them.

| Setting | Conflict when | Where it is checked |
| --- | --- | --- |
| `disableAllHooks` | the effective value is `true` and RimZ's hooks are installed | `rimz doctor` shows an installed but disabled row at warning severity, with the file, key, and fix. Launch compilation refuses every permission mode |
| `defaultPermissionMode` | the effective value is a string other than `manual`, and the launch's extra arguments contain the ask vector `--mode interactive` | launch compilation refuses |

Both launch checks run in `harness/launch.rs`, before the argv is built. The refusal names the agent, file, key, value, and fix. For the user file the fix is a `copilot config defaultPermissionMode manual` command, prefixed with `COPILOT_HOME=<home>` when the account's home is not `~/.copilot`. Without RimZ's hooks installed, `disableAllHooks` silences nothing of RimZ's and the launch proceeds. Doctor resolves the settings for its own cwd and the ambient environment. Messages to an agent that is already running are not checked again.

The `defaultPermissionMode` check also runs on resume, though upstream says the setting does not apply to resumed sessions ([modes and permissions](../../externals/agent-adapter/copilot-reference.md#modes-and-permissions)). The refusal is conservative there.

## Launch and resume

| Launch | Argv |
| --- | --- |
| Fresh | `copilot` |
| Resume | `copilot --resume <session_id>` |
| Bare `--resume` | `copilot --resume`, Copilot's own picker |
| Prompt-seeded | `--interactive <prompt>` after profile and permission arguments |
| Ask | `--mode interactive`, which overrides `defaultMode` |
| Plan | `--plan` |
| Auto | `--autopilot` |
| Yolo | `--allow-all` |
| Model preset | `--model` |
| Effort preset | `--reasoning-effort`. `--effort` is also matched, so an existing value of either spelling is replaced |

Fork is unsupported: the descriptor declares no fork command, and upstream `/fork` exists only inside the interactive UI ([selecting a session](../../externals/agent-adapter/copilot-reference.md#selecting-a-session)). The descriptor also declares no `fast` or `auto-compact` preset, and `/compact` is the compaction command, without instructions.

RimZ passes the session ID to `--resume` as a separate argument. Upstream types the value as optional (`--resume[=VALUE]`), and `--session-id <id>` is the exact-match form with a required value if a parser stops binding it.

Prompt-seeded panes and supervised `rimz agents copilot -p` runs both launch the stock interactive UI with `--interactive`, keeping native asks and the hook-driven completion path. A supervised run takes its lifecycle and exit status from `agentStop` and pane liveness. On `agentStop` the adapter reads the final visible `assistant.message` from the validated transcript, so the run record, `agents wait --stream`, the durable transcript, and `agents history` share one source. Copilot's own non-interactive `-p` mode is not used (see [Known gaps](#known-gaps)).

## Context and transcript

The conversation source is `$COPILOT_HOME/session-state/<sessionId>/events.jsonl`, an append-only file. `paths::validated_transcript_path` accepts a hook's `transcriptPath` only when its file name is `events.jsonl` and its parent directory is named after the session ID. Without an accepted path, the adapter derives that path for a session ID that is one safe path component, and publishes it once the file exists. The check is structural: it does not require the path to sit under `COPILOT_HOME`.

The visible-message reader (`transcript.rs::parse_messages`) keeps only `user.message` and `assistant.message` records, reads `data.content`, and parses the RFC 3339 top-level `timestamp`. It does not filter on `agentId`, so a child's message record in the parent file reads as the parent's. User text passes through the shared control-prompt sanitizer, and assistant text is kept as written. Final-message extraction reads the shared 64 KiB record-aligned tail, and falls back to the full file only when the tail holds no assistant message.

### Statusline

The managed statusline runs `RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source copilot`. The payload's `session_id` binds the context sidecar to the lifecycle row. `statusline.rs::StatuslinePayload` maps these fields:

| Context value | Source, first present wins |
| --- | --- |
| Session name, CLI version | `session_name`, `version` |
| Model | `model.id`. For `auto`, the target after the rightmost `→` or `->` in `model.display_name` |
| Display label and effort | `model.display_name`, with a trailing effort qualifier such as ` (high)` split off as the effort |
| Window size | `displayed_context_limit`, `context_window_size` |
| Fill | `current_context_used_percentage`, `used_percentage`, else `current_context_tokens` over a positive window size |
| Remaining | `remaining_percentage` |
| Occupied tokens | `current_context_tokens` |
| Latest-call composition | `current_usage`, component for component |
| Session tokens | the cumulative `total_*` counters, below |
| Duration and line counts | `cost.total_duration_ms`, `total_api_duration_ms`, `total_lines_added`, `total_lines_removed` |

For an `auto` selector, the resolved target becomes both the model ID and the display label. Recognized terminal effort and multiplier qualifiers are stripped from it, and the first effort found is published. A malformed or targetless `auto` selector keeps the literal `auto` and the provider's display name.

Copilot's cumulative counters overlap, so the adapter makes them disjoint (`ContextWindow::session_usage`). `total_input_tokens` includes cache writes and cache reads, so fresh input is its saturating difference from `total_cache_write_tokens` and `total_cache_read_tokens`. `total_output_tokens` includes `total_reasoning_tokens`, so output is their saturating difference and reasoning is published as thinking. Display and local pricing add these fields once each. `total_tokens` and `last_call_input_tokens` are ambiguous and stay unread.

### OTel fallback

The OTel reader runs only while the canonical statusline is not installed for the login (`CopilotAdapter::local_context_refresh`). A missing, replaced, or unreadable statusline all count as not installed. While it is installed, OTel refresh is off, so a sparse span arriving late cannot replace the statusline's token scopes. Hook-triggered refreshes run on `sessionStart`, `userPromptSubmitted`, `postToolUse`, `postToolUseFailure`, and `agentStop`, and Tick and Watch refreshes run as well.

A newly born room's session environment (`room_env_from`) sets up the file exporter:

| Ambient configuration | Room environment |
| --- | --- |
| `COPILOT_OTEL_FILE_EXPORTER_PATH` is set | passed through unchanged, with any ambient OTel keys |
| An OTLP-only exporter (`OTEL_EXPORTER_OTLP_ENDPOINT`, or a `COPILOT_OTEL_EXPORTER_TYPE` other than `file`) | passed through unchanged, with a warning that the fallback has no file source |
| Neither | `COPILOT_OTEL_FILE_EXPORTER_PATH` set to the room's private `agent-telemetry/copilot-otel.jsonl` (`RuntimePaths::copilot_otel_path`) |

Separately, every Copilot spawn pins `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=false` through `launch_env`, applied over any configured environment, so no exporter receives message content. `COPILOT_OTEL_FILE_EXPORTER_PATH` is also a hook environment key, so an applied hook resolves the same file.

`paths::otel_source` picks the file in this order:

1. The anchored sidecar path, when that file exists.
2. The login environment's `COPILOT_OTEL_FILE_EXPORTER_PATH`.
3. The newest direct `.jsonl` child of `$COPILOT_HOME/otel`.

With none of these present and an OTLP-only configuration, there is no source.

`otel.rs::latest_chat_usage` reads the bounded tail and keeps `span` records named `chat` whose `gen_ai.conversation.id` equals the session ID exactly. It selects the span with the latest timestamp. The model is `gen_ai.response.model`, else `gen_ai.request.model`, else the lifecycle model. The span's input, cache read, cache write, and output counts become the latest-call composition. The refresh anchors the file even before it holds a usable span, so a later asynchronous append is read on the next Tick or Watch.

The OTel source supplies a model and a latest-call composition only, with no window size. Without a window size the card draws no fill gauge or window token. A composition with nonzero input or cache tokens counts as session history, and output alone does not (`AgentTokenUsage::used_tokens`). OTel carries no dollar figure: `github.copilot.cost` is a billing multiplier ([traces](../../externals/agent-adapter/copilot-reference.md#traces)).

The card's model label prefers the display name only when it contains a digit, then the context model ID, then the launch model (`agents/model_display.rs::model_label_source`).

## Account and balance

The account probe reads only the non-secret login identity in `$COPILOT_HOME/config.json`: `lastLoggedInUser`, else the first valid `loggedInUsers` entry (`CopilotConfig::active_identity`). The file is JSONC, read through the shared comment-tolerant reader. A github.com login reports as `account_id`, and an enterprise login as `login@host`, so identities on different GitHub hosts stay distinct. Host normalization accepts the CLI's scheme, path, and port forms, folds case and trailing dots, and rejects malformed authorities.

| Config state | Probe result |
| --- | --- |
| A valid identity | the account |
| Missing file, or valid file without an identity | logged out |
| Unreadable or malformed file | unavailable, on the short retry path |

An environment token alone never claims a login. The probe and the usage request read the RimZ process's own environment and ambient Copilot home, never the login environment (`CopilotAdapter::probe_account` and `probe_account_usage` ignore it).

For a metered login, the shared account-usage refresh sends `GET https://<api-host>/copilot_internal/user` (`account_usage.rs::probe_usage`). The shared `agents/credits.rs::oauth_http_get` makes up to three attempts with 5-second timeouts and 300 ms then 600 ms backoff. The credential and host resolve in this order:

| Input | Order |
| --- | --- |
| Credential | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, then the `copilotTokens` entry whose normalized `<host>:<login>` key matches the active identity |
| Host | `COPILOT_GH_HOST`, `GH_HOST`, the active identity's host, then `github.com` |
| API host | `api.github.com` for github.com, otherwise the host with one `api.` prefix, keeping a valid port |

A config token is used only when its identity's host equals the resolved host, so it never crosses identities or host overrides. Any present environment token wins, including Codespaces' auto-injected `GITHUB_TOKEN`, which upstream lets a `/login` account override ([authentication](../../externals/agent-adapter/copilot-reference.md#authentication-and-account)).

The request keeps only a versioned SHA-256 digest of the host and credential as its cache owner, and a config-backed read also carries `config.json`'s mtime. Headers, response bodies, plaintext credentials, and request paths stay out of errors and durable state. Faults split two ways (`CopilotUsageErr::fault`):

- Missing credentials, an unreadable config, an invalid host, and a provider-rejected credential settle quietly as no credentials.
- Transport failures, unexpected HTTP status, and unusable responses take the shared short retry and reporting path.

The response maps to named scopes. `quota_snapshots` wins per scope, and a missing scope falls back to `monthly_quotas` as entitlement and `limited_user_quotas` as remaining. The provider's `percent_remaining` wins, otherwise RimZ derives it from a positive entitlement and the remaining count, then clamps it. Resets accept RFC 3339 or a bare date. A zero-entitlement placeholder is dropped, and an explicit `unlimited` renders as a lifted row.

| Scope | Label |
| --- | --- |
| `chat` | `cr` when `token_based_billing` is true (`rimz accounts` prints "AI credits"), else `cht` |
| `premium_interactions` | `prm` |

Both scopes are authoritative and have no duration. Copilot therefore shows no 5h or 7d windows, and these allowances stay out of pace, roll-forward, auto-continue, and provider-capacity policy. A response with only a plan keeps the trimmed `copilot_plan`.

## Cost

Live cost comes from the statusline (`StatuslinePayload::cost`). A positive `ai_used.total_nano_aiu` wins: 1,000,000,000 nano AI units is one AI credit, billed at $0.01 ([billing](../../externals/agent-adapter/copilot-reference.md#billing-and-ai-credits)). Without credits, the session's disjoint token scopes are priced against the local price book: fresh input, output plus thinking, cache writes, and cache reads. The price is the one for the concrete model the statusline publishes, and a positive finite result becomes the estimated session cost. Model changes inside one session are not reconstructed, so every token is priced at the current model.

Historical spend (`spend.rs::parse`) reads each `$COPILOT_HOME/session-state/*/events.jsonl` file and takes the origin directory from `session.start.data.context.cwd`. Each `session.shutdown` record carries cumulative per-model counters, and the cursor keeps the last value of each, so a resumed parse emits only growth:

- Detailed `tokenDetails` win. Aggregate `usage.inputTokens` minus both cache categories is the fallback for fresh input.
- When `modelMetrics` is absent, a concrete top-level model and usage record is the fallback.
- An omitted field keeps its baseline. A field that goes down resets its baseline and emits nothing.
- A positive `totalNanoAiu` delta prices the entry at $0.01 per credit, even for a model the price book lacks.
- Without credits, known models use the price book, and unknown models keep zero-dollar token entries for the normal price-refresh path.
- A shutdown whose token delta is zero emits no entry, even when credits grew. The credit baseline still advances.
- A cursor that has token baselines but no credit baseline prices its next delta from tokens, since its first credit delta would replay the whole total.

A finalized session's cached entries keep their dollars until a `SPENDING_CACHE_VERSION` bump forces a cold reprice (`agents/spending/cache.rs`). A session cached with token-priced dollars keeps them, so `rimz stats` can show metered and estimated Copilot rows side by side. The descriptor declares account spend partial: shutdown credits give session dollars, and no account dollar ledger exists.

## Known gaps

`rimz coverage --wiring` prints the current wired, partial, and unsupported grid from `COPILOT_COVERAGE`. The gaps below are the ones whose reason the grid does not carry.

- Install ignores the login environment. Hook and statusline paths resolve from the RimZ process, so a named account with its own `COPILOT_HOME` gets readiness, refresh, and launch refusal judged against the ambient home ([Install](#install)).
- The settings probe is incomplete. It reads the three files in [Settings checks](#settings-checks), and not managed settings, organization-targeted Assisted defaults, environment variables, flags, or `.claude/settings*.json`.
- Contradictory profiles stay contradictory. Raw `--plan`, `--autopilot`, or `--mode` arguments beside an ask profile are not reconciled beyond the existing permission-vector stripping.
- Elicitation is asynchronous. Waiting can arrive after the user answers, and no event clears an elicitation, so clearing waits for later lifecycle activity. If `ask_user` also emits `elicitation_dialog`, its message may replace the question text and mint a fresh ask. Neither behaviour is captured.
- The batch matcher is unverified. A `toolCalls` batch has no top-level `toolName`, so the `ask_user` matcher may miss a batch that contains a question.
- Copilot's sandbox is unverified. If it blocks RimZ's home, the `|| true` guard keeps the tool from being denied, but card edges can be lost. A `sandbox.userPolicy` grant is a likely fix, not a verified one.
- Account dollars are unavailable. RimZ does not read keychains or browser cookies, write plaintext `copilotTokens`, query billing budgets, or synthesize extra-credit balances. An account on the legacy premium-request platform is billed per request, so its credit-based dollars are a list-rate figure.
- A child's permission ask can hold the parent. An approved call with a `command` argument clears at the child's matching post-tool edge. A denied call, or an approved tool without a `command`, holds the parent Waiting until the parent's next hook.
- Yolo has no policy preflight. Managed `permissions.disableBypassPermissionsMode = "disable"` suppresses `--allow-all`, so a `copilot-yolo` launch under that policy runs with normal permission prompts instead of failing at entry. The fail-fast fix resolves the merged policy at launch and refuses an unavailable bypass, and it waits on a merged-policy read.
- Native `-p` is not adopted. Copilot's non-interactive mode would add a second supervised-run backend and remove the pane's interactive answer surface.
- There is no default context window. `default_context_window` is `None` because the window depends on the live model and the `--context` tier, and the statusline's window size is authoritative.
- Remote control is unsupported. The statusline's `remote.connected` and premium-request counters establish no RimZ remote-control transport and no billing adjustment.
- Live behaviour not yet captured: permission variants, `ask_user` options, resume PID ancestry, multi-turn interactive streaming, and remote sessions. [Upstream scope](../../externals/agent-adapter/copilot-reference.md#upstream-scope) lists the contracts upstream leaves open.

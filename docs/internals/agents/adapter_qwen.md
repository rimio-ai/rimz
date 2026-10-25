# Qwen Code adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The raw upstream protocol is in [qwen-reference.md](../../externals/agent-adapter/qwen-reference.md).

Qwen Code is a built-in adapter, `QwenAdapter` in `crates/rimz/src/agents/adapters/qwen/`, with its spec in `QWEN_DESCRIPTOR`. It integrates through native hooks in Qwen's `settings.json`, a wrapped command-mode statusline, and the session JSONL Qwen writes under its runtime tree. Only the registry names the concrete adapter. Everything Qwen-specific (effective provider selection, the Alibaba quota probe, payloads, the statusline projection, installation, and spend) stays inside the module, and other code reads it through the provider-neutral capabilities.

| File | Owns |
| --- | --- |
| `mod.rs` | the spec, the hook catalog `QWEN_HOOKS`, hook decoding, the lifecycle mapping, transcript replay, and hook-boundary usage |
| `install.rs` | the settings path and the managed hook spec |
| `settings.rs` | the settings layers and the hook-disabling launch refusals |
| `payloads.rs` | native payload shapes, the transcript fold, and the subagent sidecar reader |
| `process.rs` | the hook owner process |
| `statusline.rs` | the statusline projection and its live cost |
| `selection.rs` | effective provider selection, the account key, and the managed-launch binding |
| `alibaba_usage.rs` | the Alibaba Coding Plan quota probe |
| `account.rs` | the account probe over `selection.rs` |
| `spend.rs` | spend discovery and the spend parser |
| `ask.rs` | question and plan detail for blocking asks |

## Hooks

### Install

Install writes into one settings file, which `install::qwen_settings_path` resolves from the login env: `RIMZ_QWEN_SETTINGS` when it is non-empty, else `$QWEN_HOME/settings.json`, else `$HOME/.qwen/settings.json`. Every account, quota, and credential read on this page uses the same file.

An entry is RimZ's when it carries `_rimz_managed` or when every handler's command contains `rimz hooks feed --source qwen`. Install strips owned entries, writes the canonical set, and leaves every other hook in place. Each handler runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source qwen` with a timeout of 2, in seconds (`QWEN_HOOK_TIMEOUT_SECS`). A captured hook also carries `QWEN_RUNTIME_DIR` (`hook_env_keys`), so the apply child resolves the same runtime tree as the hook.

Two traps sit in `crates/rimz/src/agents/managed_json_hooks.rs`:

- Install refuses an existing `async: true` handler only on an entry marked `_rimz_managed` whose matcher matches a synchronous catalog event (`reject_async_blocking`). An entry owned only by the command marker is replaced without that check.
- `installed_at` compares matcher, ownership, and the async flag for Qwen's `HandlerAsync` encoding, never the timeout. An entry with another timeout reads as installed until `rimz hooks install` rewrites it.

The statusline is installed with the hooks ([Statusline](#statusline)).

### Event mapping

| Native event | RimZ signal |
| --- | --- |
| `SessionStart` | registers. Source `compact` closes a compaction, and `startup` and `clear` mark the session `Fresh` |
| `UserPromptSubmit` | turn started. The card prompt prefers `submitted_prompt` (the typed text) over the expanded `prompt`. A present but blank `prompt` is an internal continuation and starts no turn |
| `PreToolUse` (matcher `ask_user_question\|exit_plan_mode`) | question or plan-approval wait, keyed by `tool_use_id` |
| `PermissionRequest` | permission wait. The two gate tools classify as question and plan-approval waits |
| `PostToolUse` | tool activity, with the spec's mutating and editing tool lists |
| `PostToolUseFailure` | tool activity, never mutating |
| `Stop` | turn ended. Pending `background_tasks` or a cron whose status is not `completed` or `failed` keeps the parent parked |
| `StopFailure` | errored turn end. `rate_limit` is `paused_rate_limit`, `server_error` is `paused_overloaded`, `loop_detected` is `failed`, and any other error classifies by its label (`TurnErrorClass::classify_label`) |
| `SubagentStart`, `SubagentStop` | child started and stopped ([Subagents](#subagents)) |
| `PreCompact`, `PostCompact` | compaction bracket |
| `SessionEnd` | ended |
| `Notification` | installed and decoded, with no lifecycle signal |

Hook stdout stays empty on the neutral path.

### Blocking asks

A question or plan approval can reach RimZ by two events, and both open the same wait. `PermissionRequest` covers the native approval stage. A gate tool that is auto-allowed skips that stage, so RimZ also installs a synchronous `PreToolUse` entry with the `ask_user_question|exit_plan_mode` matcher. Either event opens a typed wait keyed by `tool_use_id`. The matching `PostToolUse` or `PostToolUseFailure` closes it without opening a prompt boundary. `ask::question_detail` takes the question text from `ask_user_question` input and the plan from `exit_plan_mode` input. Any other tool's detail is the shared `permission_detail` line.

### Subagents

Qwen's native child bracket feeds the subagent tree. The child id is the payload's `agent_id`, its parent is the hook's root `session_id`, and its task is `agent_type`. At both ends RimZ reads the child's metadata sidecar, best-effort, through `payloads::read_subagent_meta`: `<project>/subagents/<session-id>/agent-<agent-id>.meta.json`, where `<project>` is the transcript's grandparent directory. `persistedCliFlags.model` supplies the child's model when the payload has none, `description` its description, and `agentType` or `subagentName` its task when the payload has no `agent_type`. Every child parents to the root session ([Known gaps](#known-gaps)).

### The hook owner process

Hook ingress must not record a pid that exits a moment later as the session's runtime owner. The installed command exports `RIMZ_AGENT_PID=$PPID`, which is the Qwen process for most events. Qwen runs `StopFailure`, `MessageDisplay`, and `SessionDelete` hooks in a child that survives Qwen's exit ([upstream](../../externals/agent-adapter/qwen-reference.md#output-and-exit-codes)), so their `$PPID` is that short-lived supervisor. `process::hook_owner_pid` recognizes the supervisor by its argv: a `qwen` or `node` program followed by `--input-type=commonjs --eval`. It substitutes the supervisor's parent when that parent's program is `qwen` or `node` and it is not a supervisor itself. The check reads only the program name, so any Node parent passes. An orphaned supervisor, reparented to pid 1, reports no owner, and the session keeps its prior runtime owner.

## Launch

### Preflight

Qwen 0.25.1 or newer is required. An older readable version refuses the launch with the fix `npm install -g @qwen-code/qwen-code@latest`, and an unreadable version abstains.

`settings::hook_blocker` refuses a launch whose configuration disables hooks, because RimZ tracks Qwen only through them. The refusal names the setting, where it came from, and the fix.

| Source | Refuses when |
| --- | --- |
| `tools.executionSandbox` | an object in the system-defaults, user, or system layer. A workspace value does not count. The fix is operator policy: remove it at its source, or run Qwen outside RimZ |
| `disableAllHooks` | `true` in the highest layer that sets it, workspace included. RimZ reads the workspace layer even where Qwen would not trust the folder |
| `--safe-mode` | set in the launch argv (before `--`) and not negated by a later `--no-safe-mode` |
| `--bare` | set in the launch argv and not negated by a later `--no-bare` |
| `QWEN_CODE_SAFE_MODE` | truthy (`1`, `true`, `yes`, `on`), unless the argv carries `--no-safe-mode` |
| `QWEN_CODE_SIMPLE` | truthy. `--no-bare` does not override it |

### Settings layers

`settings::layers` reads the layers that the launch refusals and spend discovery share, lowest precedence first. A layer that is missing, unreadable, or malformed is skipped, and Qwen reports its own errors.

| Layer | Path |
| --- | --- |
| system defaults | `$QWEN_CODE_SYSTEM_DEFAULTS_PATH`, else `system-defaults.json` beside the system settings |
| user | `$QWEN_HOME/settings.json`, else `~/.qwen/settings.json` |
| workspace | `<cwd>/.qwen/settings.json`, skipped when the cwd is the home directory or the caller has no cwd |
| system | `$QWEN_CODE_SYSTEM_SETTINGS_PATH`, else `/etc/qwen-code/settings.json` on Linux and `/Library/Application Support/QwenCode/settings.json` on macOS |

### Argv

| Launch | Argv |
| --- | --- |
| initial prompt | `-i <prompt>` |
| resume | `qwen --resume <id>`. A bare RimZ `--resume` opens the native `qwen --resume` picker ([upstream](../../externals/agent-adapter/qwen-reference.md#resume-fork-and-session-commands)) |
| fork | `qwen --resume <id> --fork-session` |
| permission mode | `--approval-mode`: Ask `default`, Auto `auto-edit`, Yolo `yolo`, Plan `plan`. Ask passes its value explicitly too |
| turn cap | `--max-session-turns` |
| model preset | `--model <name>` |
| launch reminders | `--append-system-prompt` |
| manual compaction | `/compress` typed into the pane (Qwen's alias is `/summarize`) |

RimZ adds no model by default (`default_model: None`). Qwen's `security.auth.selectedType` can route the session to a provider with its own model catalog, so a RimZ default could name a model that provider lacks. A Markdown definition's model preset adds `--model` explicitly.

The `system-prompt-file` preset travels in the environment, never in argv. The exec wrapper sets `QWEN_SYSTEM_MD` to the user's resolved base path when there are no fragments, or to the composed artifact when there are ([fleet.md](../harness/fleet.md#system-prompt-composition)). Qwen reads that file as a verbatim replacement of its base prompt, so prompt contents never enter the process argv. The preset's matcher (`PresetArgMatcher::EnvPathVar`) finds no argv occurrence, so a `--system-prompt` in a profile's `args` is not removed. It reaches Qwen beside `QWEN_SYSTEM_MD`, and the upstream reference does not record which of the two Qwen applies.

## Context and transcript

### Hook-boundary usage

At `SessionStart` and `Stop`, `usage_from_transcript` reads the whole transcript file, not a bounded tail. Tool hooks skip this read. `payloads::fold_transcript` indexes records by `uuid`, last write wins. It takes the newest record without `agentId` or `isSidechain: true` as the tail and walks its `parentUuid` ancestry, stopping at a missing parent or a cycle. The result is the active root branch, so a rewind drops the abandoned branch. A transcript with no usable root `uuid` keeps physical order.

The newest active-root `assistant` with `usageMetadata` supplies the model, the context window, the total, and the cache-read, fresh-prompt, and output split. At `Stop` the adapter accepts that split only when it belongs to the turn that stopped. The test is the summarized call's opener, the nearest earlier active-root `user` record that is not `mid_turn_user_message`, which includes cron and notification openers:

1. Both ids present. The opener's `promptId` must equal the payload's `prompt_id`, whatever the token counts. A `tool_result` after the summarized call means the turn's final call has not reached disk. The opener's id is then not used, and step 2 decides.
2. Either id absent, or the opener's id not used. The summary's `promptTokenCount` must equal the payload's `input_tokens`, when the payload has them.

Numeric strings parse as numbers. A malformed optional usage field stays absent and the record still counts. A readable transcript with no usage is an explicit zero, and an unreadable one is unknown. The latest active `system` record with subtype `custom_title` supplies the card description from `systemPayload.customTitle`. Until one exists, the shared rollup keeps the session's first usable prompt as its label.

### Statusline

The statusline carries the live context reading. Install wraps a command-mode `ui.statusLine`, or adds one where none exists, so `rimz statusline feed --source qwen` receives Qwen's statusline JSON and the user's own command keeps running under `_rimz_wrapped`. A statusline of another `type` has no command transport, so install leaves it untouched and context comes from the hook-boundary read alone (`managed_statusline::wrap_allowed`).

`statusline::StatuslinePayload::into_context` publishes the provider's context window, used and remaining percentages, and `current_usage` as the occupancy scalar (`current_context_tokens`). It also publishes the model display name with any leading `[provider]` decoration stripped, the version, the Vim mode, and the file-line totals. The card shows the scalar as its `▤` total. Where a transcript split from the hook-boundary read agrees with it, the card adds the split's markers, and where they disagree the scalar wins (`sidebar_pane/render/sections/agent_card/gauge.rs::context_tokens_line`). An explicit zero clears the occupancy after compaction. The cumulative `metrics.models` counters feed only the live cost ([Cost](#cost)) and never become card token categories.

### Replay

`parse_messages` drives complete conversation replay (`rimz agents history`) over the same active-root fold. Only these records render:

| Record | Renders as |
| --- | --- |
| `user` with no subtype, absent or `real_user` provenance, and no `backgroundTurn` | a human turn |
| `user` with subtype `mid_turn_user_message` | steering inside the current turn. It never replaces the turn's prompt |
| `assistant` | the reply |

Other user subtypes, system records, unknown records, and sidechain records stay out, though system and unknown records still link the ancestry. A record carries the Google `Content` shape, so its text is the join of its non-thought `text` parts. Thought, `functionCall`, and `functionResponse` parts carry no prose and drop out.

Incremental reads for `rimz message --wait` and `-p --stream` use `parse_physical_assistant_messages` instead. An appended assistant can point at a parent before the byte cursor, so the stream parser takes each new root `assistant` record in physical order and needs no ancestry on the page.

## Account and balance

### Effective selection

`selection::resolve` names the provider a session bills to, and both the account probe and the quota probe call it. It reads Qwen's JSONC settings file from [Install](#install) and joins `security.auth.selectedType`, `model.name`, the selected model's `baseUrl`, `modelProviders`, the top-level `providerProtocol` map, and the model's declared `envKey`. The transport protocol alone never decides the billing provider.

A model entry's `wireApi` decides its route under either OpenAI bucket, `providerProtocol` aliases included: `responses` selects `openai-responses` and `chat-completions` selects `openai`. Without `wireApi` the bucket's protocol applies. A `wireApi` under any other protocol makes the model unusable. An explicit `model.provider` does not change this: classification still uses the model's effective route.

| Selection | Result |
| --- | --- |
| the exact official Coding Plan endpoint for International or China, with its declared credential key (normally `BAILIAN_CODING_PLAN_API_KEY`) | metered, scope `SubProvider` Alibaba with the region, labelled "Alibaba Coding Plan (International)" or "(China)" |
| direct OpenAI (auth types `openai` and `openai-responses`), Anthropic, or Gemini API key on its official endpoint | unmetered, plan "API key" |
| no settings file, no `selectedType`, or no credential value | logged out |
| a custom endpoint, ADC or another external credential manager, an ambiguous provider record, or a credential that cannot be evaluated | unavailable, retried after a short TTL |

A credential value resolves from the login env, then the `.env` beside the settings file, then the settings file's `env` map. With `RIMZ_QWEN_SETTINGS` unset that file is `${QWEN_HOME:-~/.qwen}/.env`, as in Qwen. Values stay in memory. The account key (`Selection::account_key`) is a domain-separated SHA-256 over the provider, the region or `direct`, the credential variable name, and the secret. The same key therefore has one identity whichever source supplied it, and a rotated key in the same region is a new identity. Neither the credential nor the key reaches logs or rendered output.

### Alibaba quota

The Alibaba quota read is experimental. `alibaba_usage::probe` posts the selected API key to the region's fixed Alibaba console host. The transport takes no browser cookies, no endpoint override, no redirect, and no fallback to the other region. The response yields authoritative 5-hour, 7-day, and nominal 30-day (`mo`) windows from the chosen instance's quota. The billing month carries `fixed_period: true`, so its countdown runs even with no usage; 5-hour and week windows keep sliding-window detection ([not-started windows](./providers.md#not-started-windows)). All three retain integer request amounts in `WindowAmount`; percentages floor and reach 100 only at the cap, so 99.5% does not block a launch. Comparable request units let the dashboard mark a tighter longer allowance with a ceiling tick. `select_instance` takes the one explicitly active instance (`ACTIVE` or `VALID`), or a sole instance not explicitly inactive. A response with no instance list can still yield windows from top-level quota data.

The rate-limit cache entry carries the exact scope and account key that produced its windows. A switch of region, provider, or key replaces the entry, so the old account's windows neither paint nor gate the new one.

### Managed launches

Qwen is the one adapter that binds a managed launch to an exact account. [providers.md](./providers.md#account-scope) owns the four states and what each permits. A fresh managed launch or loop fire starts in `PendingResolution`. Once the cwd, launch environment, model, and argv are final, `selection::resolve_managed_launch` answers `Bound` with the Alibaba account they select, or `Unresolved`. A matching exhausted 5-hour, 7-day, or 30-day window blocks the launch before its run record or pane exists, and the run exits `125`. A matching loop fire records `budget skipped`. Surplus pacing uses the 5-hour and 7-day pair, and the 30-day window gates only when exhausted.

The resolver answers `Unresolved` when any of these holds:

- `QWEN_CODE_SYSTEM_DEFAULTS_PATH` or `QWEN_CODE_SYSTEM_SETTINGS_PATH` is set.
- A `.qwen/settings.json` other than the user settings file exists in the cwd or any ancestor.
- `OPENAI_BASE_URL` or `ANTHROPIC_BASE_URL` resolves from the env, the `.env`, or the settings `env`.
- The argv carries `--continue`, `--resume`, `--fork-session`, `--openai-api-key`, `--api-key`, `--provider`, `--base-url`, or `--auth-type`.
- The definition model, the argv `--model` values, `QWEN_MODEL`, and `OPENAI_MODEL` disagree.
- The selection is not an Alibaba Coding Plan account, or cannot be resolved.
- The loop task runs in a worktree.

The guard has holes. It ignores the default system settings files, and in argv it reads `--model` but not `-m`, and does not refuse `-c`, `-r`, `--openai-base-url`, or `--worktree`. A launch that uses any of these can bind to the settings file's account while Qwen runs on another ([Known gaps](#known-gaps)).

The binding is authority for one fresh launch decision, never a session identity. Qwen exposes no session-bound provider identity, and an interactive pane can switch providers. Hand-launched panes, `--wait` delivery, resumed sessions, displayed status, and reset-timed auto-continue therefore stay unbound. The native `StopFailure` rate-limit and overload classes remain the session's recovery signal. Transcript-priced Qwen dollars stay in the ordinary agent, room, and loop budgets.

## Cost

### Discovery

Spend comes from the session files Qwen writes: the direct `.jsonl` children of `<runtime-base>/projects/<project>/chats/`. Child logs, sidecars, other extensions, and JSONL elsewhere in the tree stay out. `spend::runtime_base` resolves the base from the login env that spend discovery receives:

1. `$QWEN_RUNTIME_DIR`, when non-empty.
2. `advanced.runtimeOutputDir` from the highest of the system, user, and system-defaults layers that sets a non-null value. Spend discovery has no cwd, so the workspace layer never counts. An absolute value is the base, and `~/...` expands against the login env's `$HOME`.
3. The Qwen home, `$QWEN_HOME` or `~/.qwen`. A value from step 2 that is empty, relative, `~user`, or not a string falls here, never to a lower layer.

With no base, Qwen has no spend source.

### Pricing a transcript

`spend::parse_qwen_spend` cold-folds every readable changed file and replaces that file's cached entries (`SpendUpdate::Replace`), so a root rewind retracts the abandoned branch's spend. It prices active-root `assistant` records plus every physical sidechain record. Each entry's uncached prompt, cache read, and output go through the price book. Output needs prompt accounting (`payloads::normalized_generated_output`):

1. With `totalTokenCount`, output is the total minus `promptTokenCount`.
2. Otherwise, when candidates exceed thoughts, the thoughts overlap them and output is the candidates.
3. Otherwise output is candidates plus thoughts.

A model missing from the book keeps its tokens at zero dollars and registers for the pricing refresh. Known-model dollars are local estimates from the shared price book. They feed RimZ's display and soft budgets, and they are neither Alibaba quota nor provider billing.

The transcript counts explicit and implicit cache hits together in `cachedContentTokenCount`. The `qwen3-coder-plus` and `qwen3-coder-flash` families (`pricing/overrides.rs::cache_ratios_for` matches a provider prefix and a `-` suffix) price that whole count at a fifth of input, the implicit-cache rate, where the price source publishes no cache-read rate ([spending.md](./spending.md#rates-the-sources-leave-unpublished)). Explicit hits may therefore be slightly overcounted. Other Qwen models use their own published rate or the shared fallback.

`uuid` is the entry's dedup key and `sessionId` its thread. `agentId` and `isSidechain` mark sidechain entries, so copied fork and child records dedup downstream. Root rewinds are pruned. Sidechain branches are not, because the transcript does not carry enough to reconstruct them.

### Live cost

The statusline's `metrics.models` counters give the card a live session cost (`StatuslinePayload::cost`, `CostCoverage::Session`). Each key's leading `[provider]` decoration is stripped, and each nonzero bucket is priced on its own and summed. One unpriced bucket suppresses the whole figure rather than publishing a partial subtotal. Empty and zero buckets are ignored. The figure uses API token rates, never provider billing or subscription valuation, so an off-book model or a plan without token rates shows no live cost.

## Known gaps

Run `rimz coverage` for the current wired, partial, and unsupported matrix. These are the gaps with a reason worth recording.

- **The managed-launch guard misses some overrides.** [Managed launches](#managed-launches) lists the flags and files `resolve_managed_launch` does not see. A launch that uses one can be gated on another account's windows.
- **The runtime sidecar is unread.** Hooks bind the session to the pane through the hook process and `RIMZ_AGENT_PID`. Qwen's `<session>.runtime.json` could establish that binding before the first hook and recover it after hook gaps. Reading it needs a shared adapter-owned pane and session attribution seam with descendant-process and pid-reuse checks. Qwen's live session registry (`qwen sessions ps`) has the same problem. Both wait for that seam rather than a Qwen-only binding path.
- **`SessionDelete` and nested subagent lineage are unmapped.** RimZ has no provider-neutral signal for a deleted session. The sidecar's `parentAgentId` could parent a child under another subagent, but the store and sidebar tree are not proven for that shape.
- **A child's errored close is not typed.** `SubagentStop` fires before Qwen patches the sidecar's `status`, and the payload carries no status field, so every child stops clean.
- **Dual output is unused.** `--json-file` with `--input-file` could improve prompt and permission coverage, but it changes who owns the pane launch and adds control-channel races. Hooks and the native pane stay the decision path.
- **Cost covers token rates only.** Multi-provider endpoints, subscription metering, and provider-specific off-book pricing are unmodeled.
- **Concurrent subagent delivery is unproven.** Qwen scopes hooks per invocation since 0.24.7, but no live fixture pins concurrent delivery and parent correlation.
- **Some behaviour needs a live check:** native dialog cancellation, runtime-sidecar transitions, and provider-specific billing.

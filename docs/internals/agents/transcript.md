# Agent transcripts and context

> See [DESIGN.md → Commitments](../../../DESIGN.md#commitments) for the no-transcript-correctness rule this doc operationalizes, and [hooks.md](./hooks.md) for the agent boundary it sits beside.

Every coding agent writes a session transcript and exposes a richer out-of-band surface. Rimz reads both to paint a row's context-window gauge, token count, model, cost, and rate-limit windows. This doc owns that read-path: where each provider's data lives, how Rimz parses it, and the per-provider mapping onto Rimz's internal types.

It is the **single home for context/usage enrichment mapping**: how each provider's transcript and rich-context transport fold onto Rimz's internal types ([`AgentContext`](../../../crates/rimz/src/agents/context.rs) and the `context_pct` / `total_tokens` / `model` fields of [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs)). The raw upstream shapes this mapping reads — the statusline JSON schema, the rollout event layout, the app-server methods — live in the per-provider reference: [claude-reference.md](../../externals/agent-adapter/claude-reference.md), [codex-reference.md](../../externals/agent-adapter/codex-reference.md), and [pi-reference.md](../../externals/agent-adapter/pi-reference.md) ([opencode-reference.md](../../externals/agent-adapter/opencode-reference.md) mirrors OpenCode ahead of its adapter). The seam is two-sided: this doc *produces* enrichment from a provider's surfaces; [agent.md](./agent.md#rich-context-agentcontext) *stores* it on the rollup and folds it onto the row. The rich-context blob also carries account and balance facts (plan, metered, the rate-limit windows); this doc owns the transport that delivers them, and [account.md](./account.md) owns what they mean and how they aggregate.

Enrichment is **never correctness**. A missing file, a torn line, an absent agent — each degrades to an omitted field, never a failed hook or a wrong decision. The decision/lifecycle half of the provider mapping (native event → status, decision JSON, install) lives in [hooks.md](./hooks.md); this is its quiet, display-only twin.

## Two sources

A session's context data has two origins. Both flow through one adapter — the [`AgentAdapter`](../../../crates/rimz/src/agents/mod.rs) — and normalize onto the same internal fields, so a new provider implements one or both and the rest of Rimz is unchanged.

**The transcript tail** is the universal floor. Every provider writes a JSONL session log, so every agent gets a context gauge from it. For Claude this is a low-frequency lifecycle read, because statusline owns the live `AgentContext`. For Codex the rollout tail is also the live token/cost source: progress hooks and the elected snapshot producer run a stat-gated local refresh that reads a bounded tail only when `(mtime, nanos, len)` changes, merges tokens/cost into the runtime sidecar, and wakes the sidebar.

**The rich-context transport** is the provider-specific upgrade, where a provider offers one. It carries everything the local read cannot or should not derive — rate-limit windows, account plan, PR info, model display name, version — on that provider's own cadence. Claude pushes it through statusline on render; Codex reads only read-only app-server methods in detached helpers and account probes. Each provider's transport differs, and transport payloads normalize through `observe_context` into one [`AgentContext`](../../../crates/rimz/src/agents/context.rs); local transcript refreshes use the adapter's separate `local_context_refresh` hook.

A provider whose hook wire Rimz authors has a third option: stamp the gauge **onto the hook payload itself**. Pi's extension does this on every envelope, so its gauge needs neither a tail nor a transport ([Appendix — Pi](#appendix--pi)).

|                     | transcript / local read                          | rich-context transport                         |
| ------------------- | ------------------------------------------------ | ---------------------------------------------- |
| Claude Code         | hook payload `transcript_path`                   | statusline pipe (`rimz statusline feed`)       |
| Codex               | `~/.codex/sessions/…/rollout-*.jsonl` (by id)    | `codex app-server` JSON-RPC (read-only)        |
| Pi                  | — (the gauge rides the hook payload itself)      | — (none)                                       |
| frequency           | turn boundaries; Codex progress hooks plus producer backstop | statusline render, detached helper, or account probe |
| produces            | `context_pct`, `total_tokens`, `model`; Codex also `AgentContext.tokens`, `AgentContext.cost`, and actual configured effort | provider-owned `AgentContext` fields such as limits, account, model display, version |
| target              | observation gauge fields and local context sidecar fields | `AgentContext` ([context.rs](../../../crates/rimz/src/agents/context.rs)) |

## Reading rules

The tail reader is provider-agnostic ([`read_transcript_tail`](../../../crates/rimz/src/agents/mod.rs)) and every adapter parses on top of it under the same rules:

- **Bounded.** Read at most the trailing 64 KB, so a multi-megabyte log never stalls a hook.
- **Newest-first.** Scan lines in reverse and take the most recent usage record; a truncated leading line from the tail seek simply fails to parse and is skipped.
- **Stop when found.** Bail as soon as the needed records are in hand (Codex tracks the latest `token_count` and `model` separately and stops once both are filled).
- **Lossy and forgiving.** Decode as lossy UTF-8; any IO or parse failure yields empty fields.
- **Zero vs unknown.** A transcript that opens cleanly but carries no usage yet is a *fresh* session — report an explicit `0%` so the bar draws empty rather than vanishing. A transcript that cannot be read stays unknown (`None`): "the agent did not report it," never a false zero.

## Adding a provider

A new agent earns its context gauge by implementing the transcript half, and its rich row by implementing the transport half — either alone is valid. The work mirrors [hooks.md → Adding an agent](./hooks.md#adding-an-agent):

1. **Locate the transcript** from whatever the hook payload offers (a path, or a session id plus a discovery walk).
2. **Map the usage record** onto raw context tokens (or a percentage) plus the cumulative total and the model, normalizing to the observation gauge fields.
3. **Map the transport**, if any, onto `AgentContext` through `observe_context` — every field `Option`, tolerantly parsed, so a sparse or evolved payload always parses.
4. **Stay best-effort** throughout: a failure is an omitted field, never an error.

Golden the mapping from a fixture tail and a fixture transport payload, including the fresh-session zero and the unreadable-unknown cases.

## Cost history

The gauge above reads a bounded *tail* for the live row. A second read-path walks the *whole* transcript history to total spend and token throughput — bucketed into four trailing windows, 24h / 7d / 30d / 365d, as a [`SpendTally`](../../../crates/rimz/src/agents/spending.rs). It lives in each adapter's `spend.rs` ([`claude`](../../../crates/rimz/src/agents/claude/spend.rs), [`codex`](../../../crates/rimz/src/agents/codex/spend.rs), [`pi`](../../../crates/rimz/src/agents/pi/spend.rs), shared walk helpers in [`transcript_fs`](../../../crates/rimz/src/agents/transcript_fs.rs)): one read-only parser per provider, resolved through `AgentAdapter::transcript_files` / `parse_spend`, that turns a provider's full session JSONL into per-entry cost, a four-way token split (`input` / `output` / `cache_write` / `cache_read`), and an entry timestamp, aggregated by [`spending::compute_spending`](../../../crates/rimz/src/agents/spending.rs). Each window carries that split — its `↘` input folds in `cache_write`, so the `◇` total is folded input plus output; `cache_read` rides apart — plus a `sessions` count of the distinct threads that ran in the window (a Claude session's subagent files fold under its `session_id` directory, so one thread counts once). The fleet tally feeds two consumers: the cockpit's `◎`/`¤`/breakdown summary with its trailing-24h count-up `$`, and the bottom [fleet ledger](../../interface/sidebar.md#the-fleet-ledger) (the static trailing-week and trailing-month rows). The read is incremental and user-scoped: the shared spending cache stores `(mtime, len, cursor)` per file, so an unchanged file is one stat and a grown file parses only its appended suffix from the cursor (Codex's cumulative-totals fold state rides the cursor). A shape change to the per-entry split bumps `SPENDING_CACHE_VERSION` so finalized sessions re-parse cleanly; a semantic change to the published aggregate bumps `PROVIDER_SPENDING_VERSION` so `provider-spending.json` recomputes once from the current entry cache without forcing JSONL re-parse. The whole walk runs at most once per user per `SPENDING_TTL`: between due walks every room serves the shared `provider-spending.json` exactly as a consumer tab reads it ([performance.md → Per-enrichment cadences](../health/performance.md#per-enrichment-cadences)).

It is **read-only and sidebar-safe** — no ledger writes — so it sits apart from the integration adapters. Two parsing concerns are provider-specific:

- **Dedup.** Claude replays a parent message into each subagent file with an inflated cost; `compute_spending` dedups by `(message.id, requestId)` across files and suppresses the sidechain replay so a turn is counted once. Pi and Codex sessions are single-file and need no cross-file dedup.
- **Cost source.** Claude and Codex log token counts, so `compute_spending` multiplies each turn's `message.usage` through the per-model [pricing table](./pricing.md) to dollars — input, output, cache-creation, and cache-read each at their own rate. Current Claude transcripts carry no `costUSD`; an older Claude turn that still logs a positive `costUSD` uses that authoritative figure verbatim instead. Pi logs dollars directly (`usage.cost.total` — [pi-reference.md → Session JSONL](../../externals/agent-adapter/pi-reference.md#session-jsonl)), used verbatim with no pricing-table multiplication.
- **Unknown prices.** A token-priced turn whose model misses the book contributes no entry, and the file cache records the trimmed model name plus its youngest timestamp for the pricing refresh chase; sentinel names such as Claude's `<synthetic>` are filtered out because they are not API model IDs, and unknowns older than the 365-day spend window do not chase. Once an active unknown model resolves, the file cold re-parses from byte zero, so entries skipped before the incremental cursor are recovered in the same due walk.

The producer ([`sidebar::produce`](../../../crates/rimz/src/sidebar/produce/spending.rs)) discovers all three fleet-wide — every Claude project dir, every Codex and Pi session — so each provider counts on the same footing regardless of which project it ran in. `compute_spending` returns one fleet total plus a **per-provider breakdown** — the cockpit shows the total, each dashboard panel its own provider's spend (see [account.md](./account.md#per-provider-spend)). Golden each parser from a fixture JSONL, including the dedup and zero/negative-cost cases.

---

## Appendix — Claude Code

**Transcript.** Claude names the active session log in every hook payload's `transcript_path`. Each assistant line carries a `message.usage` block; the newest one wins:

| Transcript field                                                          | Internal                                  |
| ------------------------------------------------------------------------- | ----------------------------------------- |
| `usage.input_tokens` + `usage.cache_read_input_tokens` + `usage.cache_creation_input_tokens` | context tokens (the gauge numerator) |
| context tokens + `usage.output_tokens`                                    | `total_tokens`                            |
| `message.model`                                                           | `model` (bare id — no capability marker)  |

Claude writes only the bare model id, so the **window divisor is resolved by the caller, not the transcript**: [`context_window_for`](../../../crates/rimz/src/agents/claude/mod.rs) reads the `[1m]` marker off the *hook payload's* `model` (1,000,000 tokens) and otherwise assumes the 200,000 standard window, then scales the context tokens to `context_pct`. The marker rides only the payload, never the transcript, so resolving the payload model first is what keeps the 1M gauge correct.

**Turn-death marker.** Claude's `StopFailure` hook is the precise provider-error certificate, and the transcript tail is the backstop when that hook did not fire or was installed late. On each statusline push, [`detect_turn_error`](../../../crates/rimz/src/agents/claude/statusline.rs) scans the bounded tail newest-first and lets the **first conversation-bearing entry decide**: an `assistant`/`user` entry with a parseable `timestamp`, skipping sidechain replay and non-conversation records (`system`, `file-history-snapshot`, `summary`). A flagged `assistant` entry (`isApiErrorMessage: true`) emits `AgentContext.turn_error` with `at`, a capped label, and a `class`: labels containing "usage limit" or "rate limit" become `PausedRateLimit`, labels containing "overloaded" become `PausedOverloaded`, and other API errors become `Failed`. Anything else means alive or recovered. The marker is display-only enrichment — the projection in [agent.md → Displayed status](./agent.md#displayed-status) pauses or fails the row, and the rollup never changes.

**Rich context.** Claude `exec`s its configured `statusLine` command on every render and pipes a JSON blob to stdin (the full schema is in [claude-reference.md → Statusline JSON](../../externals/agent-adapter/claude-reference.md#statusline-json)). Install points `statusLine` at `rimz statusline feed --source claude`; [`observe_context`](../../../crates/rimz/src/agents/claude/statusline.rs) parses that blob into `AgentContext`. When the user already has a `statusLine`, install **wraps** it rather than replacing it: it captures the JSON, passes it unchanged to the original command, and forwards that command's stdout and exit code, so rendering is visually identical. The original is stored verbatim under `_rimz_wrapped` and restored on uninstall. The wrap is a visible security surface — the consent gate summarizes it and the install diff shows it in full — and its child's stdio is fully piped, never inherited. Field-exact shapes are the inline goldens in [`statusline.rs`](../../../crates/rimz/src/agents/claude/statusline.rs).

Install wraps Claude's per-child `subagentStatusLine` the same way (at `rimz statusline feed --source claude --subagent`). That feed harvests each task's `description`, `tokenCount`, and `startTime` — parsed by [`subagent_statusline.rs`](../../../crates/rimz/src/agents/claude/subagent_statusline.rs) — into a per-child sidecar the sidebar folds onto the subagent row (see [sidebar.md](../sidebar/sidebar.md)). It is enrichment for Rimz's own expanded card, not a row override, so Claude's panel renders unchanged.

## Appendix — Codex

**Transcript.** Codex writes one rollout file per session at `~/.codex/sessions/YYYY/MM/DD/rollout-*-<session_id>.jsonl`. Given the payload's `session_id`, [`find_session_transcript`](../../../crates/rimz/src/agents/codex/mod.rs) descends the date tree newest-first, bounded by a day-directory budget so a large archive never stalls the walk; `RIMZ_CODEX_SESSIONS` overrides the root for tests. The rollout feeds usage, cost, model, and turn-death enrichment:

| Rollout event   | Field                                          | Internal                                  |
| --------------- | ---------------------------------------------- | ----------------------------------------- |
| `token_count`   | `payload.info.last_token_usage.input_tokens` / `payload.info.model_context_window` | `context_pct` (input × 100 ÷ window, clamped) |
| `token_count`   | `payload.info.last_token_usage.total_tokens`   | `total_tokens`                            |
| `token_count`   | `payload.info.last_token_usage.cached_input_tokens` | `cache_read_input_tokens` (the card's `◌`) |
| `token_count`   | `payload.info.last_token_usage.input_tokens − cached_input_tokens` | `fresh_input_tokens` (the card's `↘`; `input_tokens` includes the cached slice) |
| `token_count`   | `payload.info.last_token_usage.output_tokens`  | `output_tokens` (the card's `↗`)          |
| `token_count`   | `payload.info.total_token_usage`               | `cost` (cumulative totals priced through [pricing.md](./pricing.md)) |
| `turn_context`  | `payload.model`                                | `model` (display name)                    |

Unlike Claude — which stores raw tokens and derives the window from the payload model — Codex stores a **precomputed `context_pct`**, because the rollout carries the window (`model_context_window`) directly.

**Turn-death marker.** Codex writes provider-error evidence to the rollout tail, which is the local death certificate for Rimz's non-interfering read path. On `Stop`, [`detect_turn_error`](../../../crates/rimz/src/agents/codex/transcript.rs) scans the bounded tail newest-first. A newer live record (`agent_message`, `task_started`, `user_message`, or `turn_context`) clears the check; `token_count` is usage evidence, not recovery evidence. A timestamped `event_msg` error record (`turn_error`, `stream_error`, `error`, or `task_complete` carrying `error`) or schema-shaped `ErrorNotification` error object emits `AgentContext.turn_error` with `at`, a capped label, and a `class`. The detector maps Codex's app-server `codexErrorInfo` vocabulary first (`usageLimitExceeded` → `PausedRateLimit`, `serverOverloaded` → `PausedOverloaded`, other known variants → `Failed`), then falls back to label keywords: "usage limit", "rate limit", "quota", or "too many requests" pause for rate limit; "overloaded" or "server is busy" pauses for overload; everything else fails. On `Stop`, the same marker also makes the lifecycle turn end as errored, preventing a native clean Stop from painting a provider-killed turn as success. The projection in [agent.md → Displayed status](./agent.md#displayed-status) uses the marker to choose paused or failed for the row.

**Rich context.** Codex has no statusline, so its `AgentContext` is split. The rollout/config local read owns the live per-session usage fields (`tokens`, context percentage/window, cumulative `cost`, and the actual configured reasoning effort): `rimz hooks feed` attempts a local refresh inline on `SessionStart`, `UserPromptSubmit`, `PostToolUse`, and `Stop`, but only after the hook's decision channel is irrelevant and only when the stat gate says the rollout changed. The hidden `rimz codex refresh-context` helper performs the same local merge before any app-server work, the elder renderer's transcript watcher fires the refresh on the rollout write itself so mid-turn growth lands without waiting for a hook ([state.md → Push Channels](../sidebar/state.md#push-channels)), and the elected snapshot producer runs an in-process stat-gated backstop for visible root Codex rows. All four write the same runtime sidecar and wake the sidebar; none touches the durable ledger.

The official `codex app-server` (JSON-RPC 2.0 over stdio) owns the fields the rollout does not: rate-limit windows, account plan, model display name, thread preview/name, and agent version. The client speaks only **read-only, non-interfering** methods — the handshake, the rate-limit read, the model list, and stored-thread summary reads (`thread/read`, with `thread/list` as the preview/name fallback; the methods and their response schemas are in [codex-reference.md → App-server API](../../externals/agent-adapter/codex-reference.md#app-server-api)). It never calls `thread/resume`, `turn/start`, or any write, which would rejoin and own the user's live thread. `model/list.defaultReasoningEffort` is deliberately ignored for the row because it is a catalog recommendation/default, not the current session's configured effort.

The app-server trigger is never inline: a turn-boundary hook spawns `rimz codex refresh-context` detached with null stdio, so the hook returns before the round-trip and adds no latency. The helper throttles app-server-owned fields by their own `rate_limits_observed_at` stamp, so a fresh transcript/cost merge never suppresses a due account refresh and an app-server retry never suppresses local usage. The producer also keeps the account-scoped balance windows fresh between turns on a bounded cadence — the rate-limit refresh logic (and the `rimz codex refresh-rate-limits` idle path) lives in [account.md → Refresh cadences](./account.md#refresh-cadences). `RIMZ_CODEX_BIN` overrides the binary for tests. Connection preference is warmest-first, all best-effort with a cold-spawn floor so enrichment never depends on any one of them:

1. **The per-session broker** ([`broker.rs`](../../../crates/rimz/src/agents/codex/broker.rs), run as `rimz codex app-server serve` in the `rimzd` daemon tab) holds one warm, already-handshaked `codex app-server` and serves it over a unix socket, so each datapoint skips the cold handshake.
2. **The per-user remote-control daemon**, re-used via `codex app-server proxy --sock <path>` (overridable by `RIMZ_CODEX_APP_SERVER_SOCK`).
3. **A fresh cold-spawned `codex app-server`** — the always-present fallback, so headless / no-mux still enriches.

The one datapoint the app-server does **not** expose read-only is token / context-window usage: it rides only the live `thread/tokenUsage/updated` notification behind a subscribing `thread/resume`. Rimz therefore treats the rollout as authoritative for live Codex usage, including the `AgentContext.tokens` value the card reads; app-server unavailability can stale rate limits, display-name metadata, or the thread preview/name, but it cannot stall the context meter, token composition, or per-session cost.

## Appendix — Pi

Pi needs neither source: Rimz authors pi's hook wire ([pi-reference.md](../../externals/agent-adapter/pi-reference.md)), so the extension stamps the gauge onto every hook envelope — `context_pct` / `context_window` / `total_tokens` from the in-process `ctx.getContextUsage()` (rounded on the wire), plus `model` and the thinking level as `effort` — and `observe_lifecycle` reads it straight off the payload: payload-first with a `None` fallback, never a transcript tail. There is no rich-context transport — pi exposes no rate-limit or plan surface to carry ([account.md](./account.md)).

**Cost history** rides the shared spend pass above: one `<ISO-timestamp>_<uuid>.jsonl` per session under `~/.pi/agent/sessions/--<cwd-with-dashes>--/`, dollars read verbatim by [`pi/spend.rs`](../../../crates/rimz/src/agents/pi/spend.rs).

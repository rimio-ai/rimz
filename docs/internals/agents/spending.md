# Spend and token pricing

RimZ totals what agents cost by walking their transcripts, and prices token-only transcripts through a per-model price table. This page owns both: the cost coverage a live card carries, the full-history spending walk and its caches, the service that keeps one walk per namespace, and the [`PriceBook`](../../../crates/rimz/src/agents/pricing/mod.rs). Accounts, subscription windows, and paid-usage balances are [providers.md](./providers.md); the dollar caps that read these totals are [budget.md](../harness/budget.md).

Spend is enrichment. A missing transcript store, a failed price fetch, or an unknown model degrades to stale prices or zero-dollar token usage inside a real tally, never to an error.

The code lives in two modules:

| Module | Owns |
| --- | --- |
| [`agents/spending/`](../../../crates/rimz/src/agents/spending/mod.rs) | discovery, the cursor cache, the [`SpendingWalker`](../../../crates/rimz/src/agents/spending/mod.rs), aggregation into [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs), per-slot effort, the elected service, and the published caches |
| [`agents/pricing/`](../../../crates/rimz/src/agents/pricing/mod.rs) | the price row, the embedded snapshot, the upstream projection, the refresh and unknown-model chase, and cost arithmetic |

Each provider's transcript parser is its adapter's `spend.rs`, reached through `SpendingCapability::spending_sources` and `parse_spend` ([adapter.md](./adapter.md)).

## Live cost coverage

Every card cost carries a [`CostCoverage`](../../../crates/rimz/src/agents/context.rs), and the coverage decides where the number may travel.

| Coverage | Shape | May enter |
| --- | --- | --- |
| `Session` | cumulative | cockpit spend, live agent and room budgets |
| `CurrentUsage` | replace-style, point-in-time | the card only, because adding it over time would double-count |

`CostCoverage::contributes_to_live_spend` is the gate: only `Session` passes. Cursor's hook accumulator and Droid's cumulative settings counters have session coverage, whether the provider supplied dollars or RimZ priced its token counters. Antigravity's statusline cost is `CurrentUsage`. Both render as ordinary dollars on the card, and neither coverage creates provider history, provider or account totals, or account-day budget eligibility.

The provider panel's live session count is a separate input. The pane projection counts distinct identity-bearing root panes per kind after binding, so two durable conversation rows sharing one pane count once and a pane with no session yet counts zero. This room-local count never enters `SpendTally`, budgets, account spend, or `rimz stats`.

## Cost history

The spending walk reads the whole transcript and store history of every provider and totals spend, token throughput, and named tool calls into a [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs) per provider. Each tally holds the configured `[sidebar] spend_window` headline, a local-calendar-day `day` window in the publication's timezone, and trailing 7d, 30d, and 365d windows. `day` also populates model and session-role tallies for the held stats dashboard; it is independent of the headline serialized as `today`.

Each parsed file yields, per entry: a cost, a four-way token split (`input`, `output`, `cache_write`, `cache_read`), a per-name tool-call map where the provider exposes one, a timestamp, a provider-native thread id when one store holds many sessions, and one per-file origin path when the provider exposes one. A window folds `cache_write` into its `↘` input, so the `◇` total is folded input plus output, and `cache_read` rides apart. Its `sessions` count is the distinct threads that ran in the window.

### Discovery

The walk finds every registered spend store across every history pool, which is each provider's own home and each standalone account home ([providers.md](./providers.md#logins)): every Claude project directory, every Codex, Pi, and Copilot session file, every OpenCode database. A provider counts the same regardless of which project it ran in. A shared account's home is never walked, because its history is links into the provider's own home; `discovery::pool_bases` drops it, so each file is found once and stamped with its pool's key (`<kind>@default`, or the standalone account's own). The tree a reconcile sets entries aside into (`.rimz-aside/`) is outside every source, including Codex's whole-home walk. Each adapter declares its store topology once; complete transcript lookup walks that declaration on demand, and the discovery index below prunes it for warm walks. Transcript-only providers such as Kiro keep session lookup without entering aggregation, and a process plugin enters only when its manifest declares both transcript globs and a spend probe ([plugin.md](./plugin.md)).

[`discovery.rs`](../../../crates/rimz/src/agents/spending/discovery.rs) keeps a process-local directory index. An ordinary pass stats the roots plus the retained active-frontier directories, reuses unchanged nodes without `read_dir`, and stops returning files whose mtime is past the 365-day-plus-skew cutoff. A successful enumeration reconciles additions and deletions; a transient metadata or directory-read failure keeps the prior subtree and leaves it due. Every 15 minutes (`COMPLETE_RECONCILE_INTERVAL`) a complete reconciliation bypasses frontier and Codex date-partition pruning, which repairs coarse directory mtimes, scan races, and in-place writes below a pruned branch. The index is rebuilt after a restart and is never persisted.

### Per-provider parsing

Parsing is mostly shared. Two concerns differ by provider, and each adapter page carries the detail:

| Provider | Dedup | Cost source |
| --- | --- | --- |
| Claude | parent messages replayed into subagent files: exact duplicates keep the richest main-thread record (token count, then speed metadata, then timed duration) before sidechain suppression | tokens through the price book; an older transcript's positive `costUSD` is used as logged |
| Codex | copied rollouts: a provider-namespaced fingerprint over native timestamp, model, and token split | tokens through the price book |
| Copilot | each shutdown or model delta keyed by session plus native record id, with a timestamp-and-counters fallback | tokens through the price book; shutdown output already includes reasoning, and AI Credits stay outside USD |
| Pi | single-file sessions | its non-negative `usage.cost.total` when present, otherwise tokens through the price book |
| OpenCode | SQLite rows keyed by `session_id` | positive stored `cost`; zero-cost token rows through the price book |

A token-priced entry whose model misses the book still contributes tokens and sessions with zero dollars. The file cache records the trimmed model name and its youngest timestamp for the [unknown-model chase](#the-refresh). Sentinel names such as Claude's `<synthetic>` are skipped because they are not API model ids, and unknowns older than the 365-day window do not chase. When a chased model resolves, the file cold re-parses from byte zero, so its zero-dollar entries recover their spend on the same due walk.

### Session and seat folds

Two narrower reads share the walk's parsers and dedup.

[`spending::session_cost_usd`](../../../crates/rimz/src/agents/spending/mod.rs) is the turn-end live-card cost for adapters whose transport does not push a dollar total. It selects one session's rows through `session_entries`, applies the walk's `SidechainDedup` policy, and returns `Session` coverage whether each retained entry carried provider USD or was priced. Droid uses this boundary for its cumulative exact-table value and keeps `spending_sources()` empty, so its settings snapshot never reaches history.

[`spending::slot_effort`](../../../crates/rimz/src/agents/spending/effort.rs) is the lifetime figure for one durable agent seat. For each continuation, the adapter's session-spend-transcripts capability names every file carrying that session's work (Claude returns the main transcript plus its `subagents/*.jsonl` companions). The fold runs one `SidechainDedup` across all files and sessions, so resumed transcripts and sidechain copies cannot replay prior work, and prices every retained entry through the shared book. Attribution, `rimz agents show`, `rimz teams show`, and the sidebar's finished-cohort receipt all read this fold ([attribution.md](./attribution.md)).

### What reads the totals

The walk publishes machine-shared `provider-spending.json`, and every reader takes its figures from there or from the workspace cache derived beside it.

| Reader | Reads |
| --- | --- |
| provider panel headline | the kind's `SpendTally` headline: session count, token breakdown, and dollars ([providers.md](./providers.md#producer-aggregation)); serialized as `today` in the cache |
| `rimz accounts --json` | a CLI-local projection: `headline` and `window` name the configured headline; `today` reads the pool's `day_by_login` only when `day_cutoff_secs` matches the current local midnight in the configured timezone, otherwise null. `pool` identifies repeated tallies; consumers deduplicate by it. The cache and service wire retain their existing names. |
| fleet store `W:` and `M:` rows | the fleet-wide trailing-week and trailing-month totals; `$0.00` before any history exists ([interface](../../interface/sidebar.md#the-fleet-store)) |
| cockpit `◎`, `¤`, and breakdown | a workspace-scoped tally limited to the room's project root and grouped worktrees, omitting unknown-origin files |
| account daily caps | a local-calendar-day `SpendWindow` per kind (`day_by_provider`) and per history pool (`day_by_login`), computed for eligible kinds independently of the headline ([budget.md](../harness/budget.md)) |
| `rimz stats` | local-calendar-day and per-model buckets; `chart.days` holds model dollars for the last 30 local days, and `chart.weeks` for 52 seven-day blocks ending at `chart.today`, keyed by each block's last day ([stats.md](../stats.md)) |

The publication names its resolved `timezone`; both its day keys (local civil dates expressed as epoch-day numbers) and the reader's today index use that zone. `chart.today` records every walk's local epoch day in that history zone, anchoring weekly blocks and publication freshness independently of the configured/system-zone account-day cutoff. `ProviderSpendingCache::is_from_today` compares it with today's index in the published zone. Raw rows use their local date. Rows compacted beyond eight days carry only a UTC rollup's maximum timestamp, so their heatmap cell uses that timestamp's local date, an approximation that does not change window totals. Local-midnight boundaries are computed once per history pass, not by converting every row. Only the aggregate version changes; the raw spending cache stays readable without reparsing pruned transcripts.

An unnamed system timezone cannot be carried in the publication. History then uses and publishes UTC, with a tracing warning to set `timezone` in config. Named zones are unchanged. Account-day budget windows retain their existing configured/system-zone behavior.

A provider with no historical tally keeps the full headline template: its separately derived active-session count stays live, and the token and dollar positions paint as dim `–` placeholders. Antigravity takes this path because its current usage is replace-style and `$0.00` would be false.

The default session headline opens on a recorded user prompt and stays open while priced activity keeps each gap below five hours; autonomous activity never opens a burst by itself. The headline `$` and the local-day budget figure also count spend that has not flushed into the walk yet. The producer publishes recent per-session cumulative baselines in `workspace-spending.<scope_hash>.json`, and each consumer adds only the positive difference between a live card's cumulative cost and its walked baseline. Pre-window process cost never enters and flushed spend never counts twice. Store windows, headline tokens, and session counts stay walk-derived.

## The incremental cache

The shared `spending.json` cursor cache ([`cache.rs`](../../../crates/rimz/src/agents/spending/cache.rs)) lets a walk parse only what changed. Each source carries a logical stamp of `(primary mtime seconds and nanos, primary length, optional companion mtime and length)` beside its cursor and origin.

| Source state | Work |
| --- | --- |
| exact stamp hit | served from cache |
| changed primary, no companion, anchored cursor | validate the content anchor, then parse the suffix or merge a rewritten file |
| primary grew, no companion, legacy cursor | parse the appended suffix from the cursor |
| a companion appears, changes, or is removed | cold parse |
| several cold sources | parse in a bounded worker pool |
| a new source whose newest mtime predates the widest window plus a skew margin | skipped without a record; dead records past that boundary are evicted |

Append-only stores advance incrementally. Rewind-prone stores return an authoritative replacement: OpenCode ignores resume state for every changed SQLite database, cold-folds the whole mutable table, and replaces that file's cached entries, so in-place row completion never loses spend.

Claude cursors anchor their nonzero byte offset with the CRC-32 of up to 4 KiB immediately before it. A shrink or mismatched anchor restarts the parse, including after an in-place tombstone or a rename followed by regrowth. A non-heal fleet refresh keeps all previously counted rows and unknown models, adding only unseen `(message.id, requestId)` pairs inside the eight-day raw-retain window (keyless rows join too). Older fresh rows are excluded because their prior spend may already be in keyless rollups. Legacy cursors without anchors keep the old growth-only resume rule and become anchored on progress; unchanged stamps still skip reading. This optional field does not bump the cache version. Pricing heals still replace rather than merge, so a heal coinciding with a rewrite can lose rows removed from the source.

The Claude card's local token fold restarts on a rewrite rather than merging keyed history. Published session token counts remain monotonic, so they can stall until the rewritten fold overtakes the previous maximum.

Claude child cost cursors carry the same optional anchor and restart their per-request cost fold on a rewrite. Their displayed cost is not monotonic: removing requests from a child transcript can lower its cost. Older child cursors without an anchor retain the legacy resume behavior until they consume more records.

The cursor carries provider resume state where a parser needs it: Claude's object-shaped pricing fingerprint and response timing, Codex's cumulative-totals fold, response timing, service tier, and learned file origin (which survives cold re-parses), Copilot's per-model shutdown baselines and session-start directory, and Pi's session header directory. Rows dedup against retry writes within each parsed chunk. Finalized rows stay exact for 8 days, which covers the trailing week and cross-file retry and sidechain dedup; older rows compact into per-day, per-model, per-thread rollups out to 365 days.

Dirty cursor state persists on the first walk, after `SPENDING_PERSIST_MIN_INTERVAL` (5 minutes), or after cold-sized parse work. During a publishing walk, `WALK_CHECKPOINT_INTERVAL` (1 second) checkpoints cursor progress when the persist gate is open and publishes a current-stamped partial aggregate only when the cache changed since the previous checkpoint and that partial lowers no published figure: the trailing-year dollars of the fleet total, each provider, each login, and the walk's own workspace scope in its current-version sidecar. So the dashboard total climbs during a cold start, a restart resumes from the checkpoint, and a fresh walker whose cursor trails the publication never dips it; a withheld partial leaves `provider-spending.json` and the workspace sidecar untouched, stamp included. Only the finished walk lowers a published figure. The final cursor and aggregate are the authority; write failures log warnings with their paths.

The long-lived walker memo keeps only `(file index, entry index)` locations of dedup winners, keyed by cache generation and signature. Aggregation borrows paths and thread ids from the cache and allocates session strings only for the published live-baseline and session-key outputs. An append, truncate, compaction, or file-set change rebuilds the locations once.

Three version constants gate schema changes:

| Constant | Current version | Bump when | Effect |
| --- | --- | --- | --- |
| `SPENDING_CACHE_VERSION` ([`cache.rs`](../../../crates/rimz/src/agents/spending/cache.rs)) | 28: per-entry fast-mode flag | the entry split, response timing, cursor state, store-time dedup, origin metadata, pricing fallback, or logical stamp changes shape or parsed value | every cursor re-parses once |
| `PROVIDER_SPENDING_VERSION` ([`publish.rs`](../../../crates/rimz/src/agents/spending/publish.rs)) | 22: bounded daily and weekly model-dollar chart cells | the published aggregate or its pricing guarantees change without needing a re-parse | `provider-spending.json` recomputes once from the entry cache |
| `WORKSPACE_SPENDING_VERSION` ([`publish.rs`](../../../crates/rimz/src/agents/spending/publish.rs)) | 8: calendar-day tally window | the workspace tally or its walked baselines change | the workspace sidecar recomputes once from the entry cache |

Writers refuse a schema downgrade after a cheap leading-version probe. An older long-lived build therefore cannot blank a newer build's aggregate or force cursor cold walks: a bump costs one recompute, and the higher version holds.

### Response timing

`CachedEntry.timing` uses optional cache key `g`, containing `ResponseTiming` as `{"m": milliseconds, "o": output_tokens}`. Compaction copies timing and sums both counters, retaining timing when any constituent row was timed. `SpendWindow` sums these into `timed_ms` and `timed_output`; zero counters are omitted from published JSON. Stats reads the published per-model windows. Attribution folds transcript `CachedEntry.timing` through `SlotEffort`, retaining raw counters across member and report model merges. `SpendWindow::tokens_per_sec` and `SlotEffort::tokens_per_sec` share the rounded ratio of sums, never an average of response or member rates.

Both timed adapters attach rows only through `response_timing` in `agents/spending/cache.rs`. Its metric-defining constants are `MIN_TIMED_OUTPUT_TOKENS = 100`, `MIN_RESPONSE_MS = 200`, and `MAX_RESPONSE_MS = 300_000`: output must meet the minimum, elapsed time must exceed 200 ms, and 300,000 ms is inclusive. A threshold change requires a cursor-cache version bump.

- Claude: the latest timestamp across top-level, non-sidechain user lines fixes the start at a message's first assistant line. User timestamps need not be monotonic, and interleaved tool-result user lines cannot move an in-flight message's start. Each assistant line uses its own millisecond timestamp as the end; after token and speed-metadata ties, `should_replace_duplicate` prefers larger timed duration in both chunk and cross-file dedup. Advisor iteration rows are untimed. The cursor object carries the pricing fingerprint, latest user timestamp, and current message id with its fixed start, so a suffix parse can finish a streaming message.
- Codex: `task_started` and non-model response items advance the latest input timestamp until the first model item. Model items are reasoning, assistant messages, and function, custom-tool, local-shell, or web-search calls. `token_usage_record` ends the response, filters its output and elapsed time, and resets the input/model state. The next `token_count` consumes the pending timing only when its output count matches. Usage, cost, cumulative totals, and dedup keys still come exclusively from `token_count`. Replayed heads neither produce timing nor retain timing state. The cursor preserves in-flight and pending timing across resume. Histories without response records remain untimed; the count timestamp is not a fallback because it can include tool runtime.

Other adapters supply no timing. The user-facing definition and comparison limits are in the [insight guide](../../guide/insight.md#how-the-numbers-are-calculated).

## One walk per namespace

A namespace is one persistent state root plus one provider-discovery environment, and [`service.rs`](../../../crates/rimz/src/agents/spending/service.rs) keeps one warm `SpendingWalker` per namespace.

The first host-eligible long-lived client (a sidebar cache refresher or a held stats process) that finds no service takes the versioned owner lock, removes any stale socket, binds it with mode `0600`, and hosts the service for its process lifetime. Socket and lock names include the cursor, provider, and workspace schema versions plus a digest of the namespace, and each request repeats that identity for validation. The walker survives workspace producer churn and rehydrates from `spending.json` once after owner exit or reload.

One service thread admits connections and answers fresh-publication requests without touching the walker. A stale request atomically claims the walker until its reply is written and passes through a single-slot channel to one lifetime walker thread; a second stale request gets an immediate busy reply and serves the durable publications rather than queueing the caller's refresh tick. Every publishing refresh still takes the runtime `spending.lock`, which keeps mixed builds and the one-shot fallback safe.

The full walk runs at most once per namespace per `SPENDING_TTL` (15 seconds), except that crossing midnight in the published history zone expires the publication immediately, including timeout grace. Between walks, rooms and `rimz stats` serve `provider-spending.json`, and a sidebar whose service call fails serves compatible publications and retries on its next cache tick. One-shot sidebar processes never take the owner lock: when no owner answers, they serve any current-version publication regardless of age, with the workspace tally from the sidecar matching their scope, else the sole surviving sidecar in the runtime `lanes/` directory (the room's last publication after its scope drifted), else empty. Only when no current-version publication exists do they run the direct walker seeded from the disk cursor cache; a live sidebar owns freshening. One-shot `rimz stats` instead refreshes any publication from a previous history day so its selected-window breakdowns agree with Spend Today. Under an unnamed system zone, TTL and timeout-grace hits can retain two system-zone figures for up to 15 or 90 seconds after system midnight: the previous account-day maps, and each tally's headline under `spend_window = "today"`, whose cutoff (`uniform_headline_cutoff` in `aggregate.rs`) is the system-zone midnight too. Budget and accounts readers discard the account-day maps when their cutoff is not today's midnight; no reader discards the headline, so for that window the provider dashboard's today rows can show the previous day's total. Held stats uses the service and keeps no walker of its own; its refresh-error fallback still accepts any compatible publication. A room missing its workspace cache on the service path derives `workspace-spending.<scope_hash>.json` from the shared entry cache without taking the walk lock ([performance.md](../performance.md#per-enrichment-cadences)).

## Token pricing

Claude, Codex, and Copilot log token counts, so their dollars come from a per-model price table; Pi, OpenCode, and older Claude transcripts use it only for entries without a logged cost. A failed fetch, a missing snapshot, or an unknown model degrades to stale prices when available and otherwise to zero-dollar usage.

A [`Pricing`](../../../crates/rimz/src/agents/pricing/mod.rs) row, keyed by model in the `PriceBook`, carries input, output, cache-create, and cache-read rates; optional long-context rates for each class; an optional request-selected tier threshold; `cache_read_explicit`; the fast-mode multiplier; and an optional positive `max_input_tokens` capacity. Lookups are pure and network-free. The only network access is the gated refresh in `load_for_spending`.

Every price consumer uses the same book, the embedded snapshot plus the shared `pricing-cache.json`: history parsers, session folds, live-card transcript costs, and hook-side transcript reconciliation. An unknown model therefore heals on every USD surface once the chase lands its price, with no binary upgrade. [`cached_book`](../../../crates/rimz/src/agents/pricing/mod.rs) memoizes the book by cache path, modification time, and length, and stats the cache on each call so an atomic rewrite invalidates the memo.

### Price table precedence

The book is assembled from two ordered layers. Each layer is one projection by [`source.rs`](../../../crates/rimz/src/agents/pricing/source.rs): LiteLLM supplies the base catalogue, and the allowlisted models.dev catalogues fill missing models and fields.

| Layer | When | Source |
| --- | --- | --- |
| 1. Embedded snapshot | always, at process start | the compacted snapshot `build.rs` gzips into the binary; a fresh clone without the generated snapshot embeds an empty table |
| 2. Projected refresh | weekly, or early during an unknown-model chase | fresh LiteLLM and models.dev documents projected together into `pricing-cache.json`, overwriting embedded rows |

The authoritative models.dev catalogues are `anthropic`, `openai`, `google`, `xai`, `zai`, `zhipuai`, `alibaba`, and `moonshotai`, in that precedence order. They fill missing models and fields, including context capacities and request-selected context tiers.

Claude adds a provider-local layer when an administrator deploys `modelPricing` in the system `managed-settings.json` or an alphabetically merged `managed-settings.d/*.json` fragment. RimZ reads the same locations as Claude Code: `/etc/claude-code` on Linux and WSL, `/Library/Application Support/ClaudeCode` on macOS, and `C:\Program Files\ClaudeCode` on Windows. An exact override supplies the four USD-per-million-token rates; its cache-write rate covers both cache-write durations, and fast-mode or US-data-residency surcharges are not added. The optional multiplier scales every resulting Claude rate, overrides included. Rows no override names keep their capacity and tier metadata while their rates scale, and an override keeps the list row's context capacity. Invalid files, fields, and rows are ignored.

#### Aliasing a namespaced key

A LiteLLM key under a provider namespace also lands under the bare model id, so a lookup naming the model alone resolves: `anthropic.claude-3-5-haiku-20241022-v1:0` supplies `claude-3-5-haiku-20241022` and `claude-3-5-haiku`. Three rules keep an alias from carrying a price the model does not charge.

- An exact upstream row beats an alias, and a date-stripped alias reads the direct dated row when one exists. LiteLLM files Bedrock's Anthropic catalogue under `anthropic.`, and some Bedrock rows carry a markup: `claude-3-7-sonnet` takes Anthropic's 3.00/15.00 instead of Bedrock's 3.60/18.00.
- An alias keeps a version or date token of its own. `anthropic.claude-v2:1` spends its only version on the Bedrock revision suffix, and the bare `claude` it would leave is a word-boundary prefix of every Claude id, which would price the whole family from one 2023 row and hide new models from the chase.
- Regional (`eu.`, `au.`, `apac.`, `us.`, `global.`) and gateway (`vertex_ai/`, `azure_ai/`, `openrouter/`, `bedrock/`, `baseten/`, `deepinfra/`, `vercel_ai_gateway/`) prefixes never alias, so their markups stay addressable only by the full key.

#### Rates the sources leave unpublished

A missing cache rate falls back to the ccusage defaults, which are Anthropic's ratios: 1.25× input for a cache write and 0.1× input for a cache read. Where a provider bills differently and neither source publishes the rate, [`cache-rate-ratios.json`](../../../crates/rimz/src/agents/pricing/cache-rate-ratios.json) declares the model's own ratio against input, and [`fast-multiplier-overrides.json`](../../../crates/rimz/src/agents/pricing/fast-multiplier-overrides.json) does the same for the priority multiplier ([`overrides.rs`](../../../crates/rimz/src/agents/pricing/overrides.rs)). Both files hold ratios, so upstream stays the only source of absolute prices, and a row applies only where the source is silent: once upstream publishes the value, the entry stops applying. Two families use them: OpenAI bills a GPT-5 through GPT-5.5 cache write as plain input, and Alibaba discounts a cached Qwen 3 coder token to a fifth of input.

### The refresh

`rimz sidebar snapshot` is a one-shot process, so the refresh is disk-cached at `~/.rimz/cache/providers/pricing-cache.json`. The spending producer reads the embedded snapshot plus the cache while it holds the runtime spending lock, and re-fetches when the cache is older than a week. The cache carries a schema stamp and one projected model map; a stale shape is dropped. Each attempt fetches both sources and replaces the map only after both project successfully, so a partial outage keeps the last complete table. A failed fetch records its attempt time and backs off an hour. `RIMZ_PRICING_OFFLINE=1` skips every fetch, with `0`, `false`, and an empty value read as off; a source build without the generated snapshot then prices every token entry at zero.

The unknown-model chase rides the same producer walk and has no timer of its own. When the [walk](#per-provider-parsing) records a priceable model the book cannot price, the refresh may run early on a 30-minute gate. While the same unknowns persist, the gate doubles (1h, 2h, and so on) up to a 24h cap; a newly seen unknown resets it to 30 minutes. Any fetch attempt also starts a 30-minute floor, so a just-refreshed upstream has time to catch up. Failed chase attempts escalate the same way, and the chase state clears once every recorded unknown resolves.

`build.rs` never touches the network. It embeds the compacted snapshot at `crates/rimz/pricing/litellm-pricing.json` (or the `RIMZ_PRICING_JSON_PATH` override) and writes a gzipped copy into `OUT_DIR`, so builds stay hermetic. The hidden `rimz pricing-refresh` helper owns fetch and projection, and `cargo xtask pricing-refresh` delegates to it:

| Invocation | Does |
| --- | --- |
| `--out <path>` | writes the snapshot to that destination |
| `--check` | fetches without writing, and fails when the LiteLLM catalogue is implausibly small, an authoritative models.dev provider disappears, a built-in default model loses its price, or a long-context canary loses its tier |
| `RIMZ_PRICING_JSON_PATH`, `RIMZ_PRICING_MODELS_DEV_JSON_PATH` | substitute a local document for either fetch; `--check` requires both, so a partial override fails as a missing document |

`cargo xtask dist` runs the refresh before release builds, and the crate's Cargo `include` list carries the snapshot, so `cargo install rimz` embeds it too. GPT-6 Astra is the long-context canary: its fixtures cover both upstream shapes, standard and cache rates, and the whole-request price switch above 272,000 input tokens. Pricing capacity metadata does not change Codex's fresh-session context fallback.

### Resolving a model

`PriceBook::price` resolves a model id by exact match, then by a boundary-aware fuzzy scan: the longest stored key that is a word-boundary prefix of the normalized lookup wins. Normalization trims, lowercases, and maps `.` and `@` to `-`, so `claude-sonnet-4-20250514-via-bedrock` resolves to its base model. A purely numeric, non-date version bump is rejected, so a new `gpt-5`-family version is never priced as the old one; it resolves to its own entry or to no price. `PriceBook::exact_price` is the strict lookup for Droid custom mappings, where only the trimmed canonical key may supply capacity or cost.

### Computing token-priced cost

`Pricing::cost_of` prices one request's `TokenSplit`. The history parsers, Codex's resumable live rollout fold, and Cursor's generation-id turn pricing call it; Antigravity calls it for its replace-style current-context estimate, which stays outside live spend. Copilot and Qwen statuslines expose only session-cumulative totals with no request boundaries, so `Pricing::session_cost` estimates them at linear base rates instead of guessing long-context tiers from a sum.

Long-context tiers follow the publisher's semantics, and the book keeps both:

| Tier shape | Source | Rule |
| --- | --- | --- |
| marginal | LiteLLM `*_above_200k_tokens` rows | input, output, 5-minute cache creation, 1-hour cache creation, and cache read each switch rate after the first 200,000 tokens in that class |
| request-selected | OpenAI long-context rows | when the request's total input exceeds the model's threshold (272,000 for the covered GPT flagship families), every input, cached-input, and output token in that request uses the long-context rate |

The 5-minute cache-creation slice bills at the cache-create rate, the 1-hour slice at twice the input rate (long-context tiers included), and a fast or priority turn multiplies the finished cost by the model's fast multiplier. Claude marks a fast turn with `usage.speed`; Codex with the priority `service_tier` of the latest `thread_settings_applied` ([adapter_codex.md](./adapter_codex.md#cost)).

Two providers feed the split differently. Codex passes uncached input and output, and bills cached input at the cache-read rate when `cache_read_explicit` is set and at the input rate otherwise, so a model without a published discount does not discount cached tokens ([adapter_codex.md](./adapter_codex.md#cost)). Claude splits `message.usage.cache_creation` into 5-minute and 1-hour creation when present and treats flat `cache_creation_input_tokens` as 5-minute creation otherwise; managed contracted rates apply when deployed ([adapter_claude.md](./adapter_claude.md)).

## Session role provenance

`cache/providers/session-roles/<UTC-year>.jsonl` holds home-wide provenance independent of run records and room state. Each line carries `kind`, native `session`, `role` (`agent` or `subagent`), `profile`, and RFC 3339 `at`. A subagent requires a profile and carries no seat; an agent's profile is optional. New agent rows also carry `seat`: `"solo"`, or `{"team":"<team>","role":"<role>"}` when `RIMZ_TEAM` is set, its role from `RIMZ_AGENT_ROLE`, else the row's profile, else the provider kind. Missing seat means unrecorded, pre-ship provenance, not solo. Unknown fields are ignored and malformed lines are skipped. Appends use `append_record_bytes` followed by `sync_file_data`, without a lock; creating `session-roles/` syncs `cache/providers/`. Hook write errors warn and do not change the hook reply.

The lifecycle funnel records primary, derived, and replayed observations with a native id, excluding side conversations, when the observation creates the session's row in the room or registers the session (a fresh start or a resume). Every other frame skips the index: each hook frame applies in a fresh `rimz hooks apply` child, so the parse memo is cold. `session_roles::record` reads the current and previous year segments to keep first-wins provenance, which is why the hook writes only for a new or re-registering session. A session already live when its room upgraded is indexed at its next registration. A pane's run resolving to `subagent = true` supplies the subagent role, with its environment profile or the provider kind as fallback. Other observed sessions are agents. Native provider children inherit the pane role and seat: Claude companions join their parent's id; Codex children join their own observed id. Unobserved sessions are not attributed to a profile, but their spend remains in provider totals.

Readers fold previous-year then current-year segments, first record wins within retained provenance. On the first observation in a new year, the writer carries forward the previous year's role/profile/seat instead of replacing it with the pane's current identity. A session absent from both segments is recorded fresh. Creating a segment removes segments older than the previous year; a stat-keyed parse memo bounds retained memory to those two segments.

A Markdown team seat records profile `<team>.<role>`; a TOML `[agents.teams]` seat records its role binding's profile, and a standalone profile may itself contain a dot, so a profile string alone does not identify a team. `crates/rimz/src/agents/spending/session_roles.rs::SessionRole::placement` serves the spend join and effort fold: subagents always stay under their profile; a recorded team seat goes to its team and role; a recorded solo seat stays under its profile. An agent row without a seat uses a legacy first-dot split only when both parts are non-empty. Thus a pre-ship dotted solo profile appears as a team, and a pre-ship TOML seat with a plain profile stays solo. A solo agent without a profile uses its session's provider kind; a profile with that name merges into the same row. The launch wrapper clears `RIMZ_TEAM` and `RIMZ_AGENT_ROLE` when its identity has no team or no role, so a nested launch cannot inherit its caller's team or role ([env application](../harness/trust.md#env-application)); an inline role alone is not a team marker. A roleless team layout cell (a profile or kind, as in the built-in `peer` team) belongs to its team's launch, so it records a team seat keyed by its profile, else its provider kind, and never lands in Agents.

History publications (aggregate version 21) include `roles.agents` for solo profiles, `roles.teams` keyed by team then role, and `roles.subagents` by profile. Each leaf is a `RoleTally`: its `SpendTally` stays flattened into the existing spend-window fields, beside an `effort` object with `day`, `week`, `month`, and `year` windows. Each effort window carries `active_ms`, `from_user`, `from_teammates`, and `to_teammates`. Missing fields default to zero or empty. Native spend sessions count once per placement after deduplication, including a Claude parent's companion files. `roles.recorded_since_secs` is the earliest parsed role record's epoch second across both retained segments, over all roles; `roles.effort_since_secs` is the earliest retained effort record, independently of whether it joins a known role. Empty ledgers publish `None`; malformed tails contribute nothing. Retention means these are the first retained records, not necessarily the first ever; effort compaction can move its earliest stamp within a UTC day. Solo, team-role, and subagent spend together is a subset of fleet spend in every window; provider totals include every session independently of provenance. Published model values carry `{tally, kind}`: the producing kind with the greatest year dollars wins, ties broken by greater year tokens then ascending kind name. No model-family classifier participates.

## Session effort ledger

`cache/providers/session-effort/<UTC-date>.jsonl` holds home-wide active-time releases and message counts, independent of room and run retention. Each record carries provider `kind`, native `session`, RFC 3339 `at`, and a flattened `event`:

| Event | Payload | Meaning |
| --- | --- | --- |
| `active` | `active_ms` | Newly released active-time credit, in milliseconds. |
| `from_user` | none | A newly appended Prompt whose recorded origin is Human. |
| `from_agent` | optional `sender_kind`, `sender_session` | A newly appended Message; resolved sender identity uses the sender's native session. |
| `totals` | `active_ms`, `from_user`, `from_teammates`, `to_teammates` | The compaction record shape: totals for one provider, session, and UTC day, stamped at that day's latest event. |

The append chooses its daily segment from the current wall clock, not the event's `at`, so a delayed frame cannot write into an old segment. It reads no ledger data and takes no lock: one `append_record_bytes`, then `sync_file_data`, with parent sync on creation. Every append starts with a newline, separating it from a possibly torn previous tail without reading that tail. Readers ignore blank lines, malformed records, and unknown fields. Hook write errors warn without changing the hook reply.

The runtime active-time sidecar keeps a release watermark under its existing per-record lock. A Stop releases all unreleased credit; progress and pulses release on the next operation at least ten minutes after the last release. Their return value carries the released milliseconds (a pulse also returns its changed flag), and the hook appends only nonzero releases, after the sidecar update returns. The release uses the operation's timestamp, including the provider's error boundary for a Stop; side conversations credit their host. Progress and pulses at or before `last_progress` release nothing. A Stop releases even when stamped at or before `last_progress`, because a turn error's boundary can trail the last frame and its credit would otherwise wait for the session's next turn; the watermark already makes a repeated or replayed Stop release nothing. A pre-upgrade sidecar treats its existing credited total as already released on first touch, with no historical backfill. The grace-capped live tail used by attribution is not released until an operation credits it.

Message effort follows the transcript entry actually appended: an ask-answering prompt records Answer instead of Prompt and contributes nothing, as do harness prompts, waits, and subagent reports. A queued agent message resolves its sender's launch id through the hook's room snapshot; a parent-authored launch brief uses the parent's row. Missing sender rows or an unavailable snapshot leave sender identity absent, and a header-only message never credits a sender. Every received Message counts as `from_agent`, regardless of its sender's team; sender-side credit can only be derived when that identity resolved. Replay skips entries the transcript already recorded, so it appends effort only for newly written entries. A process crash between updating the sidecar or transcript and appending effort loses that record; replay of a completed effect does not double count it. That holds for process crashes only: the effort append is data-synced but the transcript entry and the sidecar it dedupes against are not, so after a power cut a replay can count a message or a release again.

The spending walker, checkpoint publisher, and empty-fleet publisher share `crates/rimz/src/agents/spending/session_roles.rs::load_history`, loading provenance and effort together. Effort joins by provider kind and native session through the same placement rule as spend. An unindexed session contributes no effort; an indexed session with no spend still gets a role entry. A `from_agent` record adds `from_teammates` to the receiver and, when resolved and indexed, `to_teammates` to the sender independently of the receiver's provenance. Windows use the spend fold's local-day cutoff and trailing 7/30/365-day membership, without a headline window. Active credit lands at the release timestamp, even across midnight, and messages at their transcript timestamp. Each retained daily segment has a process-wide stat-keyed parse memo; unchanged segments reuse their parsed records, and deleted, expired, or different-root segments leave the memo.

Only the elected spending engine maintains the ledger, never the hook. Segments whose entire UTC day is older than `RAW_RETAIN_SECS` (8 days) are durably replaced with `totals` records grouped by provider, native session, and the UTC day of each event's `at`. Sender-side message credit is materialized into the sender's totals. The latest event timestamp in each group becomes its `at`, preserving window totals up to UTC-day granularity; the seven-day window remains second-exact because those segments are not compacted. Already compacted segments are left untouched. Segments whose entire UTC day is older than 366 days are deleted with a directory sync. Maintenance errors warn without preventing spend publication.

## See also

- [providers.md](./providers.md): accounts, subscription windows, paid usage, and the provider panel the headline paints on.
- [budget.md](../harness/budget.md): the dollar caps that read session and account-day spend.
- [stats.md](../stats.md): the `rimz stats` reader over `provider-spending.json`.
- [attribution.md](./attribution.md): the seat fold consumers of `slot_effort`.
- [state.md](../sidebar/state.md#published-lanes): every published cache and its scope.

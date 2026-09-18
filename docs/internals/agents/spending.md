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

The spending walk reads the whole transcript and store history of every provider and totals spend, token throughput, and named tool calls into a [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs) per provider. Each tally holds the configured `[sidebar] spend_window` headline plus trailing 7d, 30d, and 365d windows.

Each parsed file yields, per entry: a cost, a four-way token split (`input`, `output`, `cache_write`, `cache_read`), a per-name tool-call map where the provider exposes one, a timestamp, a provider-native thread id when one store holds many sessions, and one per-file origin path when the provider exposes one. A window folds `cache_write` into its `↘` input, so the `◇` total is folded input plus output, and `cache_read` rides apart. Its `sessions` count is the distinct threads that ran in the window.

### Discovery

The walk finds every registered spend store across every declared account home: every Claude project directory, every Codex, Pi, and Copilot session file, every OpenCode database. A provider counts the same regardless of which project it ran in. Each adapter declares its store topology once; complete transcript lookup walks that declaration on demand, and the discovery index below prunes it for warm walks. Transcript-only providers such as Kiro keep session lookup without entering aggregation, and a process plugin enters only when its manifest declares both transcript globs and a spend probe ([plugin.md](./plugin.md)).

[`discovery.rs`](../../../crates/rimz/src/agents/spending/discovery.rs) keeps a process-local directory index. An ordinary pass stats the roots plus the retained active-frontier directories, reuses unchanged nodes without `read_dir`, and stops returning files whose mtime is past the 365-day-plus-skew cutoff. A successful enumeration reconciles additions and deletions; a transient metadata or directory-read failure keeps the prior subtree and leaves it due. Every 15 minutes (`COMPLETE_RECONCILE_INTERVAL`) a complete reconciliation bypasses frontier and Codex date-partition pruning, which repairs coarse directory mtimes, scan races, and in-place writes below a pruned branch. The index is rebuilt after a restart and is never persisted.

### Per-provider parsing

Parsing is mostly shared. Two concerns differ by provider, and each adapter page carries the detail:

| Provider | Dedup | Cost source |
| --- | --- | --- |
| Claude | parent messages replayed into subagent files: exact duplicates keep the richest main-thread record (token count, then speed metadata) before sidechain suppression | tokens through the price book; an older transcript's positive `costUSD` is used as logged |
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
| fleet store `W:` and `M:` rows | the fleet-wide trailing-week and trailing-month totals; `$0.00` before any history exists ([interface](../../interface/sidebar.md#the-fleet-store)) |
| cockpit `◎`, `¤`, and breakdown | a workspace-scoped tally limited to the room's project root and grouped worktrees, omitting unknown-origin files |
| account daily caps | a local-calendar-day `SpendWindow` per kind (`day_by_provider`) and per account (`day_by_login`), computed for eligible kinds independently of the headline ([budget.md](../harness/budget.md)) |
| `rimz stats` | UTC-day buckets and per-model buckets ([stats.md](../stats.md)) |

A provider with no historical tally keeps the full headline template: its separately derived active-session count stays live, and the token and dollar positions paint as dim `–` placeholders. Antigravity takes this path because its current usage is replace-style and `$0.00` would be false.

The default session headline opens on a recorded user prompt and stays open while priced activity keeps each gap below five hours; autonomous activity never opens a burst by itself. The headline `$` and the local-day budget figure also count spend that has not flushed into the walk yet. The producer publishes recent per-session cumulative baselines in `workspace-spending.<scope_hash>.json`, and each consumer adds only the positive difference between a live card's cumulative cost and its walked baseline. Pre-window process cost never enters and flushed spend never counts twice. Store windows, headline tokens, and session counts stay walk-derived.

## The incremental cache

The shared `spending.json` cursor cache ([`cache.rs`](../../../crates/rimz/src/agents/spending/cache.rs)) lets a walk parse only what changed. Each source carries a logical stamp of `(primary mtime seconds and nanos, primary length, optional companion mtime and length)` beside its cursor and origin.

| Source state | Work |
| --- | --- |
| exact stamp hit | served from cache |
| primary grew, no companion | parse the appended suffix from the cursor |
| a companion appears, changes, or is removed | cold parse |
| several cold sources | parse in a bounded worker pool |
| a new source whose newest mtime predates the widest window plus a skew margin | skipped without a record; dead records past that boundary are evicted |

Append-only stores advance incrementally. Rewind-prone stores return an authoritative replacement: OpenCode ignores resume state for every changed SQLite database, cold-folds the whole mutable table, and replaces that file's cached entries, so in-place row completion never loses spend.

The cursor carries provider resume state where a parser needs it: Codex's cumulative-totals fold and its learned file origin (which survives cold re-parses), Copilot's per-model shutdown baselines and session-start directory, and Pi's session header directory. Rows dedup against retry writes within each parsed chunk. Finalized rows stay exact for 8 days, which covers the trailing week and cross-file retry and sidechain dedup; older rows compact into per-day, per-model, per-thread rollups out to 365 days.

Dirty cursor state persists on the first walk, after `SPENDING_PERSIST_MIN_INTERVAL` (5 minutes), or after cold-sized parse work. During a publishing walk, `WALK_CHECKPOINT_INTERVAL` (1 second) publishes a current-stamped partial aggregate and checkpoints cursor progress when the persist gate is open, so the dashboard total climbs during a cold start and a restart resumes from the checkpoint. The final cursor and aggregate are the authority; write failures log warnings with their paths.

The long-lived walker memo keeps only `(file index, entry index)` locations of dedup winners, keyed by cache generation and signature. Aggregation borrows paths and thread ids from the cache and allocates session strings only for the published live-baseline and session-key outputs. An append, truncate, compaction, or file-set change rebuilds the locations once.

Two version constants gate schema changes:

| Constant | Bump when | Effect |
| --- | --- | --- |
| `SPENDING_CACHE_VERSION` ([`cache.rs`](../../../crates/rimz/src/agents/spending/cache.rs)) | the entry split, store-time dedup, origin metadata, pricing fallback, or logical stamp changes shape or parsed value | every cursor re-parses once |
| `PROVIDER_SPENDING_VERSION` ([`publish.rs`](../../../crates/rimz/src/agents/spending/publish.rs)) | the published aggregate or its pricing guarantees change without needing a re-parse | `provider-spending.json` recomputes once from the entry cache |

Writers refuse a schema downgrade after a cheap leading-version probe. An older long-lived build therefore cannot blank a newer build's aggregate or force cursor cold walks: a bump costs one recompute, and the higher version holds.

## One walk per namespace

A namespace is one persistent state root plus one provider-discovery environment, and [`service.rs`](../../../crates/rimz/src/agents/spending/service.rs) keeps one warm `SpendingWalker` per namespace.

The first host-eligible long-lived client (a sidebar cache refresher or a held stats process) that finds no service takes the versioned owner lock, removes any stale socket, binds it with mode `0600`, and hosts the service for its process lifetime. Socket and lock names include the cursor, provider, and workspace schema versions plus a digest of the namespace, and each request repeats that identity for validation. The walker survives workspace producer churn and rehydrates from `spending.json` once after owner exit or reload.

Connections are accepted concurrently. A request whose durable publications are fresh returns without touching the walker; one stale request takes the walker; a second stale request gets an immediate busy reply and serves the durable publications rather than queueing the caller's refresh tick. Every publishing refresh still takes the runtime `spending.lock`, which keeps mixed builds and the one-shot fallback safe.

The full walk runs at most once per namespace per `SPENDING_TTL` (15 seconds). Between walks, rooms and `rimz stats` serve `provider-spending.json`, and a sidebar whose service call fails serves compatible publications and retries on its next cache tick. One-shot processes never take the owner lock: a command that must answer uses a bounded direct walker seeded from the same disk cursor cache, while held stats uses the service and keeps no walker of its own. A room missing its workspace cache derives `workspace-spending.<scope_hash>.json` from the shared entry cache without taking the walk lock ([performance.md](../performance.md#per-enrichment-cadences)).

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

`rimz sidebar snapshot` is a one-shot process, so the refresh is disk-cached at `~/.rimz/cache/providers/pricing-cache.json`. The spending producer reads the embedded snapshot plus the cache while it holds the runtime spending lock, and re-fetches when the cache is older than a week. The cache carries a schema stamp and one projected model map; a stale shape is dropped. Each attempt fetches both sources and replaces the map only after both project successfully, so a partial outage keeps the last complete table. A failed fetch records its attempt time and backs off an hour. `RIMZ_PRICING_OFFLINE` skips every fetch; a source build without the generated snapshot then prices every token entry at zero.

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

The 5-minute cache-creation slice bills at the cache-create rate, the 1-hour slice at twice the input rate (long-context tiers included), and a fast or priority turn multiplies the finished cost by the model's fast multiplier.

Two providers feed the split differently. Codex passes uncached input and output, and bills cached input at the cache-read rate when `cache_read_explicit` is set and at the input rate otherwise, so a model without a published discount does not discount cached tokens ([adapter_codex.md](./adapter_codex.md#cost)). Claude splits `message.usage.cache_creation` into 5-minute and 1-hour creation when present and treats flat `cache_creation_input_tokens` as 5-minute creation otherwise; managed contracted rates apply when deployed ([adapter_claude.md](./adapter_claude.md)).

## See also

- [providers.md](./providers.md): accounts, subscription windows, paid usage, and the provider panel the headline paints on.
- [budget.md](../harness/budget.md): the dollar caps that read session and account-day spend.
- [stats.md](../stats.md): the `rimz stats` reader over `provider-spending.json`.
- [attribution.md](./attribution.md): the seat fold consumers of `slot_effort`.
- [state.md](../sidebar/state.md#published-lanes): every published cache and its scope.

# Spend and token pricing

RimZ totals what agents cost by walking their transcripts, and prices token-only transcripts through a per-model price table. This page owns both: the cost coverage a live card carries, the full-history spending walk and its caches, the service that keeps one walk per namespace, the [`PriceBook`](../../../crates/rimz/src/agents/pricing/mod.rs), and the three account-shared ledgers that preserve closed-day spend and attribute spend and effort to roles. Accounts, subscription windows, and paid-usage balances are [providers.md](./providers.md). The dollar caps that read these totals are [budget.md](../harness/budget.md).

Spend is enrichment. A missing transcript store, a failed price fetch, or an unknown model degrades to stale prices or to zero-dollar token usage inside a real tally, never to an error.

The code lives in two modules:

| Module | Owns |
| --- | --- |
| [`agents/spending/`](../../../crates/rimz/src/agents/spending/mod.rs) | discovery, the cursor cache, the [`SpendingWalker`](../../../crates/rimz/src/agents/spending/mod.rs), aggregation into [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs), per-seat effort, the elected service, the published caches, and the role, effort, and closed-day spend ledgers |
| [`agents/pricing/`](../../../crates/rimz/src/agents/pricing/mod.rs) | the price row, the embedded snapshot, the upstream projection, the refresh and unknown-model chase, and cost arithmetic |

A built-in provider's transcript parser is its adapter's `spend.rs`, reached through `SpendingCapability::spending_sources` and `parse_spend` ([adapter.md](./adapter.md)). A process plugin has no `spend.rs`: its `parse_spend` runs the manifest's spend probe (`crates/rimz/src/agents/adapters/plugin/mod.rs::PluginAdapter::parse_spend`) and ignores the price book, so the probe's figures are used as returned ([plugin.md](./plugin.md)).

## Live cost coverage

Every card cost carries a [`CostCoverage`](../../../crates/rimz/src/agents/context.rs), and the coverage decides where the number may travel.

| Coverage | Shape | May enter |
| --- | --- | --- |
| `Session` | cumulative | cockpit spend, live agent and room budgets |
| `CurrentUsage` | replace-style, point-in-time | the card only, because adding it over time would double-count |

`CostCoverage::contributes_to_live_spend` is the gate: only `Session` passes. Cursor's hook accumulator and Droid's cumulative settings counters have `Session` coverage, whether the provider supplied dollars or RimZ priced its token counters. Antigravity's statusline cost is `CurrentUsage`. Both coverages render as ordinary dollars on the card. Neither puts a card cost into provider history, provider or account totals, or account-day budget eligibility: those come from the walk alone.

The provider panel's live session count is a separate input. The pane projection counts distinct identity-bearing root panes per kind after binding, so two durable conversation rows sharing one pane count once, and a pane with no session yet counts zero. This room-local count never enters `SpendTally`, budgets, account spend, or `rimz stats`.

## Cost history

The spending walk reads the whole transcript and store history of every provider. It totals spend, token throughput, and named tool calls into one [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs) per provider. Each tally holds these windows:

| Window | Span | Read by |
| --- | --- | --- |
| headline | the configured `[sidebar] spend_window`, serialized as `today` | the provider panel |
| `day` | the local calendar day in the publication's timezone | model and session-role tallies for the held stats dashboard |
| `week`, `month`, `year` | last 7, 30, and 364 local calendar days, including today | the fleet store rows, stats, and role tallies |

`day` is independent of the headline, whatever `spend_window` selects. `crates/rimz/src/agents/spending/aggregate.rs::WindowStarts::new` computes all four starts once per fold in the history zone. Week begins at local midnight six days ago, month 29 days ago, and year 363 days ago. A timestamp at the start belongs to that window; a session uses its youngest entry. Windows nest and expire a whole oldest day at history-zone midnight, including across 23-hour and 25-hour days. The 365 × 24-hour bound remains for retention, parsing, and pricing, not year membership.

Once a UTC day's end is fourteen days old, the authoritative publishing walk records its deduplicated account-global rows in the [shared history store](#shared-history-store). Recorded account-days replace live rows in fleet, provider, login, model, chart, day, and role totals, including `login_month` for budgets. Session counts union both sources. Empty-fleet and checkpoint publications read the same history, so a deleted transcript or discarded cursor cannot remove a recorded day. A rebuild can replace its figure from present transcripts, keeping a day with none. The workspace slice, live baselines, and headline cutoffs remain live-only.

Each parsed file yields, per entry: a cost, a four-way token split (`input`, `output`, `cache_write`, `cache_read`), a per-name tool-call map where the provider exposes one, a timestamp, and a provider-native thread id when one store holds many sessions. The file also yields one origin path when the provider exposes one. A window folds `cache_write` into its `↘` input, so the `◇` total is folded input plus output, and `cache_read` rides apart. A window's `sessions` count is the distinct sessions that ran in it: the row's trimmed, non-empty `thread_id`, else the adapter's native session id read from the path, else its declared `ThreadKey` file grouping. Native ids are namespaced by provider kind. Claude's UUID-named main transcript and its subagent companions share one session; different UUIDs in the same project count separately.

### Discovery

The walk finds every registered spend store in every history pool. A pool is each provider's own home and each standalone account home ([logins.md](./logins.md#terms)), and its stores are, for example, every Claude project directory, every Codex, Pi, and Copilot session file, and every OpenCode database. A provider counts the same whichever project it ran in. A shared account's home is never walked, because its history is links into the provider's own home. `discovery::pool_bases` drops it, so each file is found once and stamped with its pool's key (`<kind>@default`, or the standalone account's own). The tree a reconcile sets entries aside into (`.rimz-aside/`) is outside every source, including Codex's whole-home walk.

Each adapter declares its store topology once. Complete transcript lookup walks that declaration on demand, and the discovery index below prunes it for warm walks. A transcript-only provider such as Kiro keeps session lookup without entering aggregation. A process plugin enters only when its manifest declares both transcript globs and a spend probe ([plugin.md](./plugin.md)).

[`discovery.rs`](../../../crates/rimz/src/agents/spending/discovery.rs) keeps a process-local directory index, rebuilt after a restart and never persisted. An ordinary pass stats the roots plus the retained active-frontier directories, reuses unchanged nodes without `read_dir`, and stops returning files whose mtime is past the 365-day-plus-skew cutoff. A successful enumeration reconciles additions and deletions. A transient metadata or directory-read failure keeps the prior subtree and leaves it due. Every 15 minutes (`COMPLETE_RECONCILE_INTERVAL`) a complete reconciliation bypasses frontier and Codex date-partition pruning. It repairs coarse directory mtimes, scan races, and in-place writes below a pruned branch. Its successful completion stamp also carries the once-per-reconciliation pending-rebuild check; a scope-only answer leaves that check for the next publishing walk.

### Per-provider parsing

Parsing is mostly shared. Two concerns differ by provider, and each adapter page carries the detail:

| Provider | Dedup | Cost source |
| --- | --- | --- |
| Claude | parent messages replayed into subagent files: exact duplicates keep the richest main-thread record (token count, then speed metadata, then timed duration) before sidechain suppression | tokens through the price book, or a positive logged `costUSD` as written |
| Codex | copied rollouts: a provider-namespaced fingerprint over native timestamp, model, and token split | tokens through the price book |
| Copilot | each shutdown or model delta keyed by session plus native record id, with a timestamp-and-counters fallback | the AI-credit delta at one cent per credit (10⁹ nano-AIU) when the model's baseline tracks credits, otherwise tokens through the price book. Shutdown output already includes reasoning. |
| Pi | single-file sessions | its non-negative `usage.cost.total` when present, otherwise tokens through the price book |
| OpenCode | SQLite rows keyed by `session_id`. A row with no tokens is dropped even with a positive `cost`, except a compaction row. | a positive stored `cost`, otherwise tokens through the price book. The database takes its first row's directory as its one origin for every session ([adapter_opencode.md](./adapter_opencode.md#cost)). |
| Qwen | `uuid` per record, keeping the active root branch plus sidechains, and each changed file replaces its cached entries | tokens through the price book ([adapter_qwen.md](./adapter_qwen.md#pricing-a-transcript)) |

A token-priced entry whose model misses the book still contributes tokens and sessions with zero dollars. The file cache records the trimmed model name and its youngest timestamp for the [unknown-model chase](#the-refresh). Sentinel names such as Claude's `<synthetic>` are skipped because they are not API model ids, and unknowns older than the 365-day window do not chase. When a chased model resolves, the walk cold re-parses the file from byte zero, so its zero-dollar entries recover their spend on the same due walk.

### Session and seat folds

Two narrower reads share the walk's parsers and dedup.

[`spending::session_cost_usd`](../../../crates/rimz/src/agents/spending/mod.rs) is the turn-end live-card cost for adapters whose transport does not push a dollar total. It parses the supplied primary file only, selects one session's rows through `session_entries`, applies the walk's `SidechainDedup` policy, and returns `Session` coverage whether each retained entry carried provider USD or was priced. Droid uses this boundary for its cumulative exact-table value and keeps `spending_sources()` empty, so its settings snapshot never reaches history.

The seat fold in [`effort.rs`](../../../crates/rimz/src/agents/spending/effort.rs) is the lifetime figure for one durable agent seat. For each continuation, the adapter's `session_spend_transcripts` names every file carrying that session's work: Claude returns the main transcript plus its `subagents/*.jsonl` companions. The fold runs one `SidechainDedup` across all files and sessions, so resumed transcripts and sidechain copies cannot replay prior work, and it prices every retained entry through the shared book. It has three entry points over the seats [attribution.md](./attribution.md#the-seat-fold) groups:

| Entry point | Consumers |
| --- | --- |
| `slot_effort` | `rimz agents show` |
| `slot_effort_with_memo` | `rimz teams list` and `rimz teams show`, and the sidebar's finished-cohort receipt, which reuse one `EffortParseMemo` across seats |
| `slot_effort_breakdown` | attribution, which needs the per-child split |

### What reads the totals

The walk publishes the machine-shared `provider-spending.json`, and every reader takes its figures from there or from the workspace cache derived beside it. Its optional `session_start_secs` records the machine-wide cockpit session start in Unix seconds in every headline mode (`session`, `24h`, and `today`), independently of headline and workspace cutoffs. It is absent when no prompt has opened a current session or an older writer published the cache. The current aggregate versions are listed under [Schema versions](#schema-versions).

| Reader | Reads |
| --- | --- |
| provider panel headline | the kind's `SpendTally` headline: session count, token breakdown, and dollars ([providers.md](./providers.md#producer-aggregation)) |
| `rimz accounts --json` | a CLI-local projection. `headline` and `window` name the configured headline. `today` reads the pool's `day_by_login` only when `day_cutoff_secs` matches the current local midnight in the configured timezone, else null. `pool` identifies repeated tallies, and consumers deduplicate by it. The cache and the service wire keep `today` as the headline's name. |
| fleet store `W:` and `M:` rows | the fleet-wide 7-local-day and 30-local-day totals, `$0.00` before any history exists ([interface](../../interface/sidebar.md#the-fleet-store)) |
| cockpit `◎`, `¤`, and breakdown | a workspace-scoped tally limited to the room's project root and grouped worktrees, omitting unknown-origin files |
| account daily caps | a local-calendar-day `SpendWindow` per kind (`day_by_provider`) and per history pool (`day_by_login`), computed for eligible kinds independently of the headline ([budget.md](../harness/budget.md)) |
| account monthly caps | a UTC-calendar-month `SpendWindow` per history pool (`month_by_login`), with `month_cutoff_secs` at 00:00 UTC on the 1st; raw entries and compacted UTC-day buckets use the same cutoff |
| `rimz stats` | local-calendar-day and per-model buckets; `chart.days` holds model dollars for the last 30 local days, and `chart.weeks` for 52 seven-day blocks ending at `chart.today`, keyed by each block's last day; `chart.hours` holds raw model dollars for today and the seven preceding local days, keyed by local epoch day × 24 plus wall-clock hour ([stats.md](../stats.md)) |

The publication names its resolved `timezone`. Its day keys (local civil dates as epoch-day numbers) and the reader's today index both use that zone. `chart.today` records each walk's local epoch day in that history zone, anchoring weekly blocks and publication freshness independently of the configured/system-zone account-day cutoff. `chart.hour` records the local wall-clock hour (0–23) at that same walk-start instant, so the forecast reads the hour that cut the day tally rather than the later publication stamp. `crates/rimz/src/agents/spending/publish.rs::ProviderSpendingCache::is_from_today` compares the day with today's index in the published zone. A raw row uses its local date. A row compacted beyond eight days carries only its UTC rollup's maximum timestamp, so its heatmap cell uses that timestamp's local date: an approximation shared by history cells and local-day window membership. The walk computes local-midnight boundaries once per history pass, not per row. Repeated fall-back hours share one hourly key and skipped spring-forward hours have none. Compacted rollups and zero-dollar rows create no hourly cells. Raw retention stays eight elapsed days, so the first hour of the oldest hourly day can be missing in a fall-back week. The cursor cache holds timestamps, so a change to local-day bucketing bumps only the aggregate version and re-parses no transcript.

An unnamed system timezone cannot be carried in the publication. History then uses and publishes UTC, with a tracing warning to set `timezone` in config. Account-day budget windows keep the configured zone, else the system zone, in that case too.

A provider with no historical tally keeps the full headline template. Its separately derived active-session count stays live, and the token and dollar positions paint as dim `–` placeholders. Antigravity takes this path because its current usage is replace-style, and `$0.00` would be false.

The default session headline opens on a recorded user prompt and stays open while priced activity keeps each gap below five hours (`SESSION_GAP_SECS`). Autonomous activity never opens a burst by itself. Lifecycle hooks record the prompts in the rotating `logs/user-inputs.log.jsonl` ([`user_input.rs`](../../../crates/rimz/src/agents/spending/user_input.rs)), and aggregation reads the current and rotated files.

The headline `$` and the local-day budget figure also count spend the walk has not reached yet. The producer publishes recent per-session cumulative baselines in `workspace-spending.<scope_hash>.json`, using the same session identity as the counts, materialized as `<kind>:<id>` for a native session or the grouped file path otherwise. Each baseline uses that session's own walked rows, deduplicated within that session. The fold reuses in-scope fleet winners unless a session holds a dedup component shared across owners, where an owner is a file plus the row's session and a component is its message id, or its provider dedup key when it has no message id. Only those contested sessions deduplicate their scoped rows again. A fork's copied history therefore contributes to its baseline as well as the source's, while walked totals count the shared history once. Each consumer adds only the positive difference between a live card's cumulative cost and its walked baseline. A Claude card compares against its own UUID's baseline, including subagent spend, never the other sessions in its project folder. Cost from before the window never enters, and flushed spend never counts twice. Store windows, headline tokens, and session counts stay walk-derived.

## The incremental cache

The shared `spending.json` cursor cache ([`cache.rs`](../../../crates/rimz/src/agents/spending/cache.rs)) lets a walk parse only what changed. Each source carries a logical stamp beside its cursor and origin: primary mtime in seconds and nanoseconds, primary length, and an optional companion mtime and length.

| Source state | Work |
| --- | --- |
| exact stamp hit | served from cache |
| changed primary, no companion, anchored cursor | validate the content anchor, then parse the suffix or merge a rewritten file |
| primary grew, no companion, cursor without an anchor | parse the appended suffix from the cursor |
| a companion appears, changes, or is removed | cold parse |
| several cold sources | parse in a bounded worker pool |
| a new source whose newest mtime predates the widest window plus a skew margin | skipped without a record, and dead records past that boundary are evicted |

Append-only stores advance incrementally. A rewind-prone store returns an authoritative replacement instead: OpenCode ignores resume state for every changed SQLite database, cold-folds the whole mutable table, and replaces that file's cached entries, so in-place row completion never loses spend.

### Claude rewrites

A Claude cursor anchors its nonzero byte offset with the CRC-32 of up to 4 KiB immediately before it. A shrink or a mismatched anchor restarts the parse, including after an in-place tombstone or a rename followed by regrowth. A refresh that is not a pricing heal then merges: it keeps every previously counted row and unknown model, and adds only unseen `(message.id, requestId)` pairs inside the eight-day raw-retain window (keyless rows join too). Rows older than that window are not added, because their prior spend may already sit in keyless rollups. A pricing heal replaces instead of merging, so a heal that coincides with a rewrite can lose rows removed from the source.

The anchor is an optional cursor field. A cursor without one resumes only on growth, gains an anchor on its next progress, and needs no cache-version bump. An unchanged stamp still skips reading.

Two Claude card folds follow the same anchor and restart on a rewrite:

- The card's local token fold. Published session token counts stay monotonic, so they stall until the restarted fold passes the previous maximum.
- A child's per-request cost fold. Its displayed cost is not monotonic: removing requests from a child transcript can lower it.

### Resume state and compaction

The cursor carries provider resume state where a parser needs it:

| Provider | Resume state |
| --- | --- |
| Claude | the object-shaped pricing fingerprint and response timing |
| Codex | the cumulative-totals fold, response timing, service tier, and learned file origin (which survives cold re-parses) |
| Copilot | per-model shutdown baselines and the session-start directory |
| Pi | the session header directory |

Rows dedup against retry writes within each parsed chunk. Finalized rows stay exact for 8 days (`RAW_RETAIN_SECS`), which covers the seven-local-day window and cross-file retry and sidechain dedup. Older rows compact into per-day, per-model, per-thread rollups out to 365 days. An older row whose message id two sessions share (a fork's copied history) stays raw in both files (`crates/rimz/src/agents/spending/cache.rs::retained_raw_ids`): a rollup keeps the global dedup winner only, which would drop the copy from the other session's live baseline. A message id or provider dedup key still held raw by an absent cursor file inside retention also stays raw in present copies, so a local walk cannot erase the identity a later closed-day recording needs to deduplicate them.

### Persisting and checkpoints

Dirty cursor state persists on the first walk, after `SPENDING_PERSIST_MIN_INTERVAL` (5 minutes), or after cold-sized parse work. The final cursor and aggregate are the authority, and write failures log warnings with their paths.

During a publishing walk, `WALK_CHECKPOINT_INTERVAL` (1 second) checkpoints cursor progress when the persist gate is open. It publishes a current-stamped partial aggregate only when the cache changed since the previous checkpoint and the partial lowers no published figure. The figures checked are the 364-local-day dollars of the fleet total, each provider, each login, and the walk's own workspace scope in its current-version sidecar. So the dashboard total climbs during a cold start, a restart resumes from the checkpoint, and a fresh walker whose cursor trails the publication never dips it. A withheld partial leaves `provider-spending.json` and the workspace sidecar untouched, stamp included. Only the finished walk lowers a published figure.

The long-lived walker memo keeps `(file index, entry index)` locations of dedup winners and one representative row per contested session, keyed by cache generation and signature. A single pass over cached rows finds contested sessions using borrowed component keys, with no per-row string allocation. Aggregation borrows paths and thread ids from the cache, and allocates session strings only for the published live-baseline and session-key outputs. An append, truncate, compaction, or file-set change rebuilds the locations once.

In memory, rows share model and tool names through one process-wide table and hold provider IDs as boxed strings. Tool counts retain a single `(name, count)` pair inline and use a sorted boxed slice for multiple names. Before inserting a new name, the table drops names no live handle references; its retained set is bounded by the distinct names live at the last insertion plus the new name. Cursor state stays as JSON text decoded on use; the Claude pricing gate decodes only its fingerprint. The `spending.json` bytes and version stay unchanged; agent-context sidecars may compact state whitespace without changing its value.

### Schema versions

Three version constants gate schema changes:

| Constant | Bump when | Effect |
| --- | --- | --- |
| `SPENDING_CACHE_VERSION` ([`cache.rs`](../../../crates/rimz/src/agents/spending/cache.rs)) | the entry split, response timing, cursor state, store-time dedup, origin metadata, pricing fallback, or logical stamp changes shape or parsed value | every cursor re-parses once |
| `PROVIDER_SPENDING_VERSION` ([`publish.rs`](../../../crates/rimz/src/agents/spending/publish.rs)) | the published aggregate or its pricing guarantees change without needing a re-parse | `provider-spending.json` recomputes once from the entry cache |
| `WORKSPACE_SPENDING_VERSION` ([`publish.rs`](../../../crates/rimz/src/agents/spending/publish.rs)) | the workspace tally or its walked baselines change | the workspace sidecar recomputes once from the entry cache |

`PROVIDER_SPENDING_VERSION` is 26: owned subagent spend, tokens, and active time also credit their owner's solo or team row, and `roles.ownerless_subagents` carries spend with no recorded owner. A v25 publication must be refolded. `WORKSPACE_SPENDING_VERSION` stays 10: counts and live baselines follow the adapter's native session, so Claude counts per UUID with native subagents folded under their parent. Both versions retain local-day windows; the provider publication also carries the walk hour and bounded hourly, daily, and weekly model-dollar chart cells. The raw `SPENDING_CACHE_VERSION` stays 29. A later cursor bump discards the cursor's absent-file rows but never the separately recorded closed days. A v23 provider file without `chart.hours` or `chart.hour` keeps its other values readable but is stale and must be recomputed. Both fields have serde defaults; a missing hour reads as zero.

Writers refuse a schema downgrade after a cheap leading-version probe. A long-lived build of a lower version therefore cannot blank a newer build's aggregate or force cursor cold walks: a bump costs one recompute, and the higher version holds.

### Response timing

`CachedEntry.timing` uses the optional cache key `g`, holding `ResponseTiming` as `{"m": milliseconds, "o": output_tokens}`. Compaction copies timing and sums both counters, and keeps timing when any constituent row was timed. `SpendWindow` sums them into `timed_ms` and `timed_output`, and published JSON omits zero counters. Stats reads the published per-model windows. Attribution folds transcript `CachedEntry.timing` through `SlotEffort`, keeping raw counters across member and report model merges. `SpendWindow::tokens_per_sec` and `SlotEffort::tokens_per_sec` share the rounded ratio of sums, never an average of response or member rates.

Both timed adapters attach timing only through `response_timing` in `agents/spending/cache.rs`. Its constants define the metric: output must reach `MIN_TIMED_OUTPUT_TOKENS` (100), elapsed time must exceed `MIN_RESPONSE_MS` (200), and `MAX_RESPONSE_MS` (300,000) is inclusive. A threshold change requires a cursor-cache version bump.

| Adapter | Start | End | Cursor carries |
| --- | --- | --- | --- |
| Claude | the latest timestamp across top-level, non-sidechain user lines, fixed at a message's first assistant line | each assistant line's own millisecond timestamp | the pricing fingerprint, the latest user timestamp, and the current message id with its fixed start, so a suffix parse can finish a streaming message |
| Codex | `task_started` and non-model response items advance the latest input timestamp until the first model item | `token_usage_record`, which filters output and elapsed time and resets the input and model state | in-flight and pending timing |

Claude user timestamps need not be monotonic, and interleaved tool-result user lines cannot move an in-flight message's start. After token and speed-metadata ties, `should_replace_duplicate` prefers the larger timed duration in both chunk and cross-file dedup. Advisor iteration rows are untimed.

Codex model items are reasoning, assistant messages, and function, custom-tool, local-shell, or web-search calls. The next `token_count` takes the pending timing only when its output count matches. Usage, cost, cumulative totals, and dedup keys come from `token_count` alone. Replayed heads neither produce timing nor keep timing state. A history without response records stays untimed: the count timestamp is no fallback, because it can include tool runtime.

Other adapters supply no timing. The user-facing definition and comparison limits are in the [insight guide](../../guide/insight.md#how-the-numbers-are-calculated).

## One walk per namespace

A namespace is one persistent state root plus one provider-discovery environment, and [`service.rs`](../../../crates/rimz/src/agents/spending/service.rs) keeps one warm `SpendingWalker` per namespace.

The first host-eligible long-lived client (a sidebar cache refresher or a held stats process) that finds no service takes the versioned owner lock. It removes any stale socket, binds it with mode `0600`, and hosts the service for its process lifetime. Socket and lock names include the cursor, provider, and workspace schema versions plus a digest of the namespace, and each request repeats that identity for validation. The walker survives workspace producer churn, and after owner exit or reload it rehydrates from `spending.json` once; on reload it releases its old cache and memo before decoding the replacement.

One service thread admits connections and answers fresh-publication requests without touching the walker. A stale request atomically claims the walker until its reply is written, and passes through a single-slot channel to one lifetime walker thread. A second stale request gets an immediate busy reply and serves the durable publications, so the caller's refresh tick never queues. Every publishing refresh still takes the runtime `spending.lock`, which keeps mixed builds and the one-shot fallback safe.

The full walk runs at most once per namespace per `SPENDING_TTL` (15 seconds). Within that TTL, each other room's missing or stale sidecar comes from a scope-only fold over the warm walker's fleet-wide dedup memo, with no fleet aggregation. Crossing midnight in the published history zone expires the publication at once, timeout grace included. Between walks, readers serve `provider-spending.json`, and each caller falls back in its own way:

| Caller | Fallback |
| --- | --- |
| live sidebar | when the service call fails, it serves compatible publications and retries on its next cache tick |
| one-shot sidebar (`rimz sidebar snapshot`) | never takes the owner lock. When no owner answers, it serves any current-version publication regardless of age, with the workspace tally from the sidecar matching its scope, else the sole surviving sidecar in the runtime `lanes/` directory (the room's last publication after its scope drifted), else empty. Only when no current-version publication exists does it run the direct walker, seeded from the disk cursor cache. |
| one-shot `rimz stats` | refreshes any publication from a previous history day, so its selected-window breakdowns agree with Spend Today |
| held `rimz stats` | uses the service and keeps no walker. Its refresh-error fallback accepts any compatible publication. |

With an unnamed system zone, history uses UTC while the account-day maps and the `spend_window = "today"` headline use system midnight (`crates/rimz/src/agents/spending/aggregate.rs::uniform_headline_cutoff`). TTL and timeout-grace hits can retain those system-zone figures for up to 15 or 90 seconds after system midnight. Budget and accounts readers discard account-day maps whose cutoff is not today's midnight. No reader discards the headline, so the provider dashboard's today rows can show the previous day's total during that window.

When the provider publication is fresh, a room with a missing or stale workspace cache on the service path joins the `spending.lock` election before deriving `workspace-spending.<scope_hash>.json` from the shared entry cache. It publishes only when it wins; a timeout derivation is served unpublished ([performance.md](../performance.md#per-enrichment-cadences)).

## Token pricing

The [per-provider table](#per-provider-parsing) says which entries need the price table: every Codex and Qwen entry, a Claude entry without a logged `costUSD`, a Copilot entry without a credit delta, and a Pi or OpenCode entry without a stored cost. A failed fetch, a missing snapshot, or an unknown model degrades to stale prices when they exist, else to zero-dollar usage.

A [`Pricing`](../../../crates/rimz/src/agents/pricing/mod.rs) row, keyed by model in the `PriceBook`, carries:

- input, output, cache-create, and cache-read rates
- optional long-context rates for each class
- an optional request-selected tier threshold
- `cache_read_explicit`
- the fast-mode multiplier
- an optional positive `max_input_tokens` capacity

Lookups are pure and network-free. The only network access is the gated refresh in `load_for_spending`.

Every price consumer uses the same book, the embedded snapshot plus the shared `pricing-cache.json`: history parsers, session and seat folds, live-card transcript costs, and hook-side transcript reconciliation. [`cached_book`](../../../crates/rimz/src/agents/pricing/mod.rs) memoizes the book by cache path, whole-second modification time, and length, and stats the cache on each call. An atomic rewrite therefore invalidates the memo, unless it keeps the length and lands within the same second (`crates/rimz/src/agents/pricing/mod.rs::cache_stamp`).

The main statusline supplies prices lazily through `crates/rimz/src/agents/capabilities.rs::ContextCapability::context_cost`. Antigravity, Copilot, and Qwen request the book only when pricing token usage. Copilot's native AI credits need no book, and Claude's main statusline keeps provider-reported dollars without loading prices. Claude's subagent statusline still loads the book and its fingerprint for transcript pricing.

An unknown model heals without a binary upgrade once the chase lands its price, but only where a surface parses the file again:

| Surface | After the chase lands a price |
| --- | --- |
| history walk | cold re-parses every file that recorded the model, so the totals heal on the same due walk |
| `session_cost_usd`, and the seat fold under a per-command memo | parse on every call, so they heal at once |
| the sidebar's finished-cohort receipt | its `EffortParseMemo` lives as long as the refresher and re-parses a file only when its stat changes, so an unchanged transcript keeps its zero-dollar entries |
| Codex's live rollout fold | skips an unchanged transcript (`crates/rimz/src/agents/adapters/codex/transcript.rs::refresh_transcript_context`) and resumes a grown one from its cursor (`crates/rimz/src/agents/adapters/codex/spend.rs::resume_live_fold`), so an entry it priced at zero stays at zero on the live card |

### Price table precedence

The book is assembled from two ordered layers. Each layer is one projection by [`source.rs`](../../../crates/rimz/src/agents/pricing/source.rs): LiteLLM supplies the base catalogue, and the allowlisted models.dev catalogues fill missing models and fields.

| Layer | When | Source |
| --- | --- | --- |
| 1. Embedded snapshot | on each book assembly | the compacted snapshot `build.rs` gzips into the binary. A source checkout without the generated snapshot embeds an empty table. |
| 2. Projected refresh | weekly, or early during an unknown-model chase | fresh LiteLLM and models.dev documents projected together into `pricing-cache.json`, overwriting embedded rows |

The authoritative models.dev catalogues are `anthropic`, `openai`, `google`, `xai`, `zai`, `zhipuai`, `alibaba`, and `moonshotai`, in that precedence order. They fill missing models and fields, including context capacities and request-selected context tiers.

Claude adds a provider-local layer when an administrator deploys `modelPricing` in the system `managed-settings.json` or an alphabetically merged `managed-settings.d/*.json` fragment. RimZ reads the same locations as Claude Code: `/etc/claude-code` on Linux and WSL, `/Library/Application Support/ClaudeCode` on macOS, and `C:\Program Files\ClaudeCode` on Windows.

- An exact override supplies the four USD-per-million-token rates. Its cache-write rate covers both cache-write durations, and fast-mode or US-data-residency surcharges are not added.
- The optional multiplier scales every resulting Claude rate, overrides included.
- A row no override names keeps its capacity and tier metadata while its rates scale, and an override keeps the list row's context capacity.
- Invalid files, fields, and rows are ignored.

#### Aliasing a namespaced key

A LiteLLM key under a provider namespace also lands under the bare model id, so a lookup naming the model alone resolves: `anthropic.claude-3-5-haiku-20241022-v1:0` supplies `claude-3-5-haiku-20241022` and `claude-3-5-haiku`. Three rules keep an alias from carrying a price the model does not charge.

- An exact upstream row beats an alias, and a date-stripped alias reads the direct dated row when one exists. LiteLLM files Bedrock's Anthropic catalogue under `anthropic.`, and some Bedrock rows carry a markup: `claude-3-7-sonnet` takes Anthropic's 3.00/15.00, not Bedrock's 3.60/18.00.
- An alias keeps a version or date token of its own. `anthropic.claude-v2:1` spends its only version on the Bedrock revision suffix. The bare `claude` it would leave is a word-boundary prefix of every Claude id, so it would price the whole family from one 2023 row and hide new models from the chase.
- Regional (`eu.`, `au.`, `apac.`, `us.`, `global.`) and gateway (`vertex_ai/`, `azure_ai/`, `openrouter/`, `bedrock/`, `baseten/`, `deepinfra/`, `vercel_ai_gateway/`) prefixes never alias, so their markups stay addressable only by the full key.

#### Rates the sources leave unpublished

A missing cache rate falls back to the ccusage defaults, which are Anthropic's ratios: 1.25× input for a cache write and 0.1× input for a cache read. Where a provider bills differently and neither source publishes the rate, [`cache-rate-ratios.json`](../../../crates/rimz/src/agents/pricing/cache-rate-ratios.json) declares the model's own ratio against input. [`fast-multiplier-overrides.json`](../../../crates/rimz/src/agents/pricing/fast-multiplier-overrides.json) does the same for the priority multiplier ([`overrides.rs`](../../../crates/rimz/src/agents/pricing/overrides.rs)).

Both files hold ratios, so upstream stays the only source of absolute prices. A row applies only where the source is silent, so an entry stops applying once upstream publishes the value. Two families use them: OpenAI bills a GPT-5 through GPT-5.5 cache write as plain input, and Alibaba discounts a cached Qwen 3 coder token to a fifth of input.

### The refresh

The spending producer reads the embedded snapshot plus `~/.rimz/cache/providers/pricing-cache.json` while it holds the runtime spending lock. The cache is on disk because `rimz sidebar snapshot` and one-shot `rimz stats` are short-lived processes, and a walk any of them triggers may write it. The producer re-fetches when the cache is older than a week.

- The cache carries a schema stamp and one projected model map, and a stale shape is dropped.
- Each attempt fetches both sources and replaces the map only after both project successfully, so a partial outage keeps the last complete table.
- A failed fetch records its attempt time and backs off an hour.
- `RIMZ_PRICING_OFFLINE=1` skips every fetch, and `0`, `false`, and an empty value read as off. Prices then come from the embedded snapshot and any existing cache, so a source build with neither prices every token entry at zero.

The unknown-model chase rides the same producer walk and has no timer of its own. When the [walk](#per-provider-parsing) records a priceable model the book cannot price, the refresh may run early on a 30-minute gate. While the same unknowns persist, the gate doubles (1h, 2h, and so on) up to a 24h cap, and a newly seen unknown resets it to 30 minutes. Any fetch attempt also starts a 30-minute floor, so a just-refreshed upstream has time to catch up. Failed chase attempts escalate the same way, and the chase state clears once every recorded unknown resolves.

`build.rs` never touches the network. It embeds the compacted snapshot at `crates/rimz/pricing/litellm-pricing.json` (or the `RIMZ_PRICING_JSON_PATH` override) and writes a gzipped copy into `OUT_DIR`, so builds stay hermetic. The hidden `rimz pricing-refresh` helper owns fetch and projection, and `cargo xtask pricing-refresh` delegates to it:

| Invocation | Does |
| --- | --- |
| `--out <path>` | writes the snapshot to that destination |
| `--check` | fetches without writing, and fails when the LiteLLM catalogue is implausibly small, an authoritative models.dev provider disappears, a built-in default model loses its price, or a long-context canary loses its tier |
| `RIMZ_PRICING_JSON_PATH`, `RIMZ_PRICING_MODELS_DEV_JSON_PATH` | substitute a local document for the LiteLLM or the models.dev fetch. Under `--check`, a LiteLLM override without a models.dev override fails. A models.dev override alone still fetches LiteLLM. |

`cargo xtask dist` runs the refresh before release builds, and the crate's Cargo `include` list carries the snapshot, so `cargo install rimz` embeds it too. GPT-6 Astra is the long-context canary: its fixtures cover both upstream shapes, standard and cache rates, and the whole-request price switch above 272,000 input tokens. Pricing capacity metadata does not change Codex's fresh-session context fallback.

### Resolving a model

`PriceBook::price` resolves a model id by exact match, then by a boundary-aware fuzzy scan: the longest stored key that is a word-boundary prefix of the normalized lookup wins. Normalization trims, lowercases, and maps `.` and `@` to `-`, so `claude-sonnet-4-20250514-via-bedrock` resolves to its base model. A purely numeric version bump that is not a date is rejected, so a new `gpt-5`-family version is never priced as the old one: it resolves to its own entry or to no price. `PriceBook::exact_price` is the strict lookup for Droid custom mappings, where only the trimmed canonical key may supply capacity or cost.

### Computing token-priced cost

`Pricing::cost_of` prices one request's `TokenSplit` at list rates. The history parsers other than Codex's, Codex headless judges, and Cursor's generation-id turn pricing call it. Antigravity calls it for its replace-style current-context estimate, which stays outside live spend. `Pricing::cost_of_with_rates` takes neutral `PricingRates`: `List` reuses that formula unchanged; `Base` clears every long-context tier and replaces the Fast multiplier before reusing the same formula. Only Codex rollout pricing supplies base rates, for the [per-request plan-burn basis](./adapter_codex.md#cost); its resumable live fold uses that same parser. Copilot and Qwen statuslines expose only session-cumulative totals with no request boundaries, so `Pricing::session_cost` estimates them at linear base rates and never guesses long-context tiers from a sum.

List-price long-context tiers follow the publisher's semantics, and the book keeps both:

| Tier shape | Source | Rule |
| --- | --- | --- |
| marginal | LiteLLM `*_above_200k_tokens` rows | input, output, 5-minute cache creation, 1-hour cache creation, and cache read each switch rate after the first 200,000 tokens in that class |
| request-selected | OpenAI long-context rows | when the request's total input exceeds the model's threshold (272,000 for the covered GPT flagship families), every input, cached-input, and output token in that request uses the long-context rate |

The 5-minute cache-creation slice bills at the cache-create rate, and the 1-hour slice at its explicit rate or, absent one, twice the input rate, long-context tiers included for list pricing. A fast or priority turn on list rates multiplies the finished cost by the model's fast multiplier. Claude marks a fast turn with `usage.speed`, and Codex with the priority `service_tier` of the latest `thread_settings_applied` ([adapter_codex.md](./adapter_codex.md#cost)).

Two providers feed the split differently. Codex passes uncached input and output. It bills cached input at the cache-read rate when `cache_read_explicit` is set and at the input rate otherwise, so a model without a published discount does not discount cached tokens ([adapter_codex.md](./adapter_codex.md#cost)). Claude splits `message.usage.cache_creation` into 5-minute and 1-hour creation when present, and treats flat `cache_creation_input_tokens` as 5-minute creation otherwise. Managed contracted rates apply when deployed ([adapter_claude.md](./adapter_claude.md)).

## Shared history store

Role, effort, and closed-day spend records live under `~/.rimz/accounts/.history/`, shared by every RimZ home whose accounts folder is shared. The per-home cursor and publications stay in `cache/providers/`. Uninstall and reset keep the history with the accounts folder.

```text
accounts/.history/
  format                            1\n
  maintain.lock                     exclusive non-blocking flock
  session-roles/<YYYY>[.<tag>].jsonl
  session-effort/<YYYY-MM-DD>[.<tag>].jsonl
  spend/<YYYY>[.<tag>].jsonl
```

The history root and its subdirectories are created at `02775`, and files at `0664`, independent of umask. Missing ancestors, including `accounts/`, follow the process umask without adding setgid. The history root takes the accounts directory's group on a best-effort basis; subdirectories take the history root's group, including after a racing first creation. Existing files are not chmodded by another user's append. Whole-file rewrites follow file symlinks through the shared provider-file writer.

Hook writers only append and data-sync. Each home's elected spending publisher and the rebuild helper try `maintain.lock` without waiting. While holding it, a missing format mark is durably created as `1`; a newer, malformed, or unreadable mark disables maintenance. Reads and hook appends remain allowed, but spend recording and rebuilding require the hold. Every ledger rewrite or segment removal holds this shared lock and requires a mark no greater than the build understands. A busy lock or a maintenance error never prevents ordinary spend publication.

Format 1 permits effort compaction after eight days, effort expiry after 366 days, role expiry before the previous UTC year, and spend-year expiry once the year's final UTC day lies wholly outside the 365-day retention bound. Roles and recorded spend days are never rewritten in place; a newer appended spend line supersedes an older line for the same account-day. A later build may add role fields without raising the mark; it must raise the mark before introducing effort fields or events an older compactor would lose, or extending retention beyond what an older maintainer keeps.

At each publishing walk, migration copies retained, nonempty legacy role and effort segments from this home's `cache/providers/` whole, then removes the originals and syncs their directory. Each copy is published durably without replacing an existing target as `<stem>.<tag>.jsonl`, where the tag is the first eight bytes of SHA-256 over the canonical RimZ home path, a zero byte, and the original bytes, encoded as 16 lowercase hex characters. An existing copy only needs its unchanged legacy original removed, so retrying the same bytes does not duplicate effort. Changed legacy bytes get their own copy on the next walk. If a crash falls between publishing the copy and removing the legacy file, and an older build appends to that file before the retry, the grown copy gets a new tag and its already copied effort counts twice. Role records are unaffected because the earliest `at` wins. Expired or empty legacy files are removed without copying; empty legacy directories are removed too. There is no legacy read fallback. A pre-upgrade writer in the same home can lose an append made between copying and removal.

Readers recognize bare segment stems and stems with one valid migration tag. Unknown filenames are ignored and never removed. Decoded segments are memoized by file stamp; deleted, expired, and different-root segments leave the memo.

### Closed-day spend lines

[`closed_days.rs`](../../../crates/rimz/src/agents/spending/closed_days.rs) records one JSON line per `(kind, account, UTC day)` under the maintenance hold. A day closes when its next UTC midnight plus fourteen days is no later than the walk's timestamp. Recording reads cursor entries inside the widest spend window, including absent files whose paths match a login's declared sources. Before dedup, an absent file is excluded when a present file of the same login has the same filename and the same path-derived native session id. File-keyed sessions are never classified as moved. This preserves deleted sessions with fixed transcript filenames and deleted subagents of a still-present session. All accounts participate in one dedup before splitting into account-days, after parsing and before raw-row compaction. An account with a failed first read that left no cursor entry records nothing, and recording stays armed for a retry. Empty successful parses and files intentionally skipped as out of window do not block recording. Recording runs again when the close-cutoff day, history, or account set changes, the cursor is replaced, or a parsed file holds an uncovered closed-day row inside retention. Changes confined to recent or already recorded days skip the cursor recording scan; local walks and busy locks retain pending recording work.

The automatic trigger is usage growth on a saved closed day, not a parsed file merely containing that day or repricing token-bearing rows. For each parsed file and UTC day, the refresh compares input + output + cache-write + cache-read tokens before and after parsing, before compaction. A positive delta inserts that UTC date into the cursor's `rebuild_days`, even when another file shrinks in the same walk. A file-day with zero tokens also grows when its logged dollars rise. If the ledger cannot be read, every grown closed day in retention stays pending; open days never become pending. Appending only recent rows, a pricing heal, a shrink alone, or a deletion adds nothing. An empty-cursor fill detects nothing, including after a lost cursor or version mismatch and when an interrupted fill resumes: a restore arriving before that fill completes needs `rimz stats --rebuild`. A reparse can still trigger when compaction previously dropped a fork's losing rows; the cold rebuild then finds no change.

`rebuild_days` is a set of `YYYY-MM-DD` UTC dates, defaults empty, is omitted when empty, and does not bump cursor version 29. Local walks retain it in memory for the next publisher. The cursor's `fill_in_progress` marker defaults false and is omitted when unset. An empty-cursor walk sets it before any checkpoint; checkpoints retain it, and the final write of a walk that folded every scheduled parse job clears it. Cold fills keep their checkpoints. Growth-detecting parse walks defer durable checkpoints until growth detection finishes, so parsed usage cannot reach disk without its pending evidence. Interval publications still run. Only pending-set changes not yet written and unfinished fills bypass the ordinary five-minute persist floor, including changes retained from an earlier local walk. An unchanged nonempty pending set does not bypass it; a fresh reconciliation still forces a cursor write before requesting the helper. Dates expire with the widest spend window even if maintenance stays unavailable, bounding the retained set; unclosed dates are discarded. A successful rebuild clears processed dates except those deferred by an unread account; a busy hold keeps them pending.

Only a non-rebuild publishing walk consumes the discovery index's own `last_complete` stamp, once per successful complete reconciliation. It persists the pending cursor before requesting a detached `rimz stats --rebuild --unattended` helper; local walks, rebuild walks, and scope-only returns never consume that stamp. Just before an otherwise-eligible request, the gate reads the history format mark without creating it. A newer, malformed or unreadable mark suppresses the helper but still consumes the reconciliation and keeps the days pending until a successful rebuild or retention expiry. A missing mark is usable. Repairing an unusable mark allows the next complete reconciliation to request the helper. A service walker therefore requests at most one helper per 15-minute reconciliation. A fresh one-shot walker's first discovery is complete and can request sooner while days remain pending. The helper takes the same producer lock as a requested rebuild and targets pending UTC days across every discovered account, keeping cross-account dedup coherent. It is silent and writes one `spend_rebuild` assist only when it appended at least one replacement, including known successful appends before a later I/O failure. `agents` only requests the helper; the CLI writes the assist outside the sidebar import graph.

The account is the canonical provider home relative to the canonical accounts root, or its absolute canonical path outside that root. Homes that cannot resolve or do not exist, including process plugins without provider homes, remain live-only. Readers map an account to the first matching history-pool login in discovery order, named before default, and ignore accounts this home does not use. An account that leaves the pool set (switched to shared or removed from config) stops showing its recorded days; its lines stay on disk until retention removes them and show again if the account returns.

Each line holds `kind`, `account`, `day` (`YYYY-MM-DD`), and nonempty `rows`. Valued rows group by source file, session, model, and origin and carry `session` (`{"native":"id"}` or `{"file":"path"}`), optional `origin` and `model`, latest `ts`, summed `usd`, `input`, `output`, `cache_write`, `cache_read`, optional `timing` (`{"ms":5000,"output":3400}`), `tools`, and OR-ed `fast`. The source file is not stored; rows may repeat the same session, model, and origin, preserving each file's timestamp and local-day placement. A session with raw entries on the day but no dedup winner gets a session-only row holding `session` and `ts` alone, with the youngest raw timestamp of that session-day. It counts toward the fleet, provider, and login session totals only, never model, role, day-chart, or token rows; `login_day`, `provider_day`, and `login_month` count dedup winners alone, as live does. A file session is relative to the discovered login home when inside it, otherwise absolute; the reader joins it onto its own login home. Default counts and flags may be omitted. These field names are independent of the cursor's short keys.

The newest decodable line for each account-day is effective: readers walk segments in path order and lines in file order, taking the last matching line. Tagged siblings sort before the plain year file, where writers append. Recording still skips saved account-days, even if another home's cursor disagrees; later homes can fill days nobody recorded. Each append starts with a newline and is data-synced; malformed and torn lines are skipped, and an interrupted append with no intact line can be retried. Recording does not reprice a saved day, including an unknown model still worth zero dollars at closing. The fourteen-day lag leaves time for price corrections before providers' usual transcript cleanup. A transcript copied across accounts can still count once per account when different homes record it.

`rimz stats --rebuild` takes the namespace's spending producer lock with a bounded wait and runs a fresh publishing walk without the engine's freshness or scope-only shortcuts. Incomplete discovery refuses the rebuild before parsing or publishing, leaving the prior publication untouched. After ordinary recording, it targets every saved closed UTC day for a discovered account inside the widest spend window; no scope appends a day before recording's close cutoff. The rebuild cold-parses every present discovered file, across all discovered accounts, whose last modification is no earlier than the earliest target day's start less the two-day parse skip margin, then uses recording's cross-account dedup and grouping. That rests on the premise the cold-parse window already uses: no row is stamped more than the margin after its file's last modification. Cursor rows cannot choose the files, because compaction drops a copy's rows once they lose dedup, and the copy is the day's only transcript once its winner is deleted. Absent cursor files do not participate. Accounts this home does not discover are untouched and unreported. A login with an unread discovered file, including a candidate whose stat succeeds but read fails, is deferred rather than appended or reported as kept; a readable empty file is not a read failure. Its readable files still participate in cross-account dedup. The requested command keeps its summary, warns with each deferred account and its UTC dates (consecutive days as ranges), says they stay pending, and exits 1. The unattended helper stays silent. Either scope leaves the deferred UTC days in the cursor's `rebuild_days`, in the same write that clears the days it processed, so the automatic helper retries them until the transcript reads or the day leaves the spend window.

A day with no rows from present transcripts keeps its saved line; the command warns once per account with its kept UTC-day count and dates, collapsing consecutive days into `start..end` ranges. A partial day follows the remaining transcripts and can fall. Parsing uses the walk's current price book, including its offline setting; transcript-logged costs remain as logged. An unchanged account-day appends nothing: rows match one to one in any order, every field exactly except dollars within a relative `1e-9`. A changed day appends a replacement with the same schema, leaving the preceding line as its backup. On shared history, the last home's rebuild wins. Owner placement joins a rebuilt line exactly as it joins any recorded line.

The rebuild probes the shared maintenance hold before parsing and releases it for the cold parse. It takes the hold again and rereads the effective ledger before comparing and appending, so another home's intervening append is the before figure. A busy maintenance lock refuses the rebuild with retry advice; an unusable format mark (newer, malformed or unreadable) refuses it with the mark file's path and no retry advice. Neither changes saved lines. A missing mark is usable and maintenance creates it under the lock. The finished-walk publication may lower figures; interval publications still refuse regressions. A requested rebuild prints the rebuilt UTC-day count and before/after dollars with the unchanged account-day count, `No saved day changed; N account-days unchanged.` when no replacement was appended, or `Nothing to rebuild.` when no saved days are in scope. It writes no assist.

## Session role provenance

`accounts/.history/session-roles/<UTC-year>[.<tag>].jsonl` records which role each native session ran in, across homes sharing accounts and independent of run records and room state. The walk joins it to spend to publish solo, team-role, and subagent tallies.

Each line carries `kind`, native `session`, `role` (`agent` or `subagent`), `profile`, and RFC 3339 `at`. A subagent requires a profile, carries no seat, and optionally carries a resolved `owner`: `{"team":"<team>","role":"<role>"}` or `{"profile":"<profile>"}`. Older rows and children whose parent could not be found have no owner. An agent's profile is optional, and an agent row carries `seat`: `"solo"`, or `{"team":"<team>","role":"<role>"}` when `RIMZ_TEAM` is set. The seat's role is `RIMZ_AGENT_ROLE`, else the row's profile, else the provider kind. A row without a seat uses the legacy placement heuristic below. Readers ignore unknown fields and skip malformed lines. The optional owner leaves the shared history `FORMAT` at 1.

### Writing a row

The lifecycle funnel writes a row for a primary, derived, or replayed observation with a native id, side conversations excluded, when the observation creates the session's row in the room or registers the session (a fresh start or a resume). Every other frame skips the index. Each hook frame applies in a fresh `rimz hooks apply` child with a cold parse memo, and `session_roles::record` reads every segment of the current and previous UTC year to preserve provenance, so writing on every frame would reread those files each time.

- A pane whose run resolves to `subagent = true` supplies the subagent role, with its environment profile, else the provider kind, as the profile. Only this case reads the room snapshot to resolve an owner: the pane's `RIMZ_AGENT_ID` finds its row through `AgentState::answers_to`, then `AgentState::parent_is` finds its parent. The parent's team and role identify a team owner, with role falling back to profile, then the parent's kind; a solo owner's profile falls back to the parent's kind. Missing rows or links leave the owner absent. A snapshot read failure debug-logs and still writes the role ownerless.
- Every other observed session is an agent.
- Native provider children inherit the pane's role, seat, and owner. Claude companions join their parent's id, and Codex children join their own observed id.
- An unobserved session has no row. Its spend still counts in provider totals.

Appends use exact-mode `append_record_bytes_mode` followed by `sync_file_data`, without a lock; directory creation syncs its parent. A hook write error warns and leaves the hook reply unchanged.

On the first observation in a new year, the writer carries the previous year's whole earliest role, including profile, seat, and owner, forward and ignores the pane's current identity. The first record wins even when it has no owner. A session absent from both retained years is recorded fresh. Only the shared maintainer removes older years, never the hook writer.

### Reading and placement

Readers fold every file of the previous and current UTC years, and the record with the earliest `at` wins for each provider and native session, independently of filename order. A stat-keyed parse memo retains only those files.

A profile string alone does not identify a team. A Markdown team seat records profile `<team>.<role>`, a TOML `[agents.teams]` seat records its role binding's profile, and a standalone profile may itself contain a dot. `crates/rimz/src/agents/spending/session_roles.rs::SessionRole::placement` serves both the spend join and the effort fold, and places a row by the first case that matches:

| Row | Placement |
| --- | --- |
| `subagent` | `Subagent` under its profile |
| `agent` with a team seat | `Team` under its team and role |
| `agent` without a seat, whose profile splits at its first dot into two non-empty parts | `Team` under those two parts |
| any other `agent` | `Solo` under its profile, else the session's provider kind |

The seatless split is a heuristic. A seatless dotted solo profile lands as a team, and a seatless TOML seat with a plain profile stays solo. A solo agent without a profile shares a row with a profile of its provider kind's name.

At both join sites, `crates/rimz/src/agents/spending/session_roles.rs::SessionRole::owner_placement` adds an owned subagent's dollars, token columns, and active time to its resolved solo profile or team-role placement. Owners never use the legacy profile split or the child's provider fallback. Sessions and messages stay with the subagent cross-cut, not the owner. Ownerless subagents contribute only to the cross-cut, and their spend also enters `roles.ownerless_subagents`. This late-bound join applies equally to live and recorded closed-day rows, without reparsing transcripts or rewriting closed days.

The launch wrapper clears `RIMZ_TEAM` and `RIMZ_AGENT_ROLE` when its identity has no team or no role, so a nested launch cannot inherit its caller's team or role ([env application](../harness/trust.md#env-application)). An inline role alone is not a team marker. A roleless team layout cell (a profile or kind, as in the built-in `peer` team) belongs to its team's launch: it records a team seat keyed by its profile, else its provider kind, and never lands in Agents.

### What the publication carries

`provider-spending.json` carries `roles.agents` for solo profiles, `roles.teams` keyed by team then role, and `roles.subagents` by profile. Each map leaf is a `RoleTally`: its `SpendTally` fields flattened in place, beside an `effort` object with `day`, `week`, `month`, and `year` windows. `roles.ownerless_subagents` is a `SpendTally`, defaulting to zero when absent. Each effort window carries `active_ms`, `from_user`, `from_teammates`, and `to_teammates`, and missing fields default to zero or empty. The role join uses the fleet's native session key. Sessions count once per direct placement after deduplication, a Claude parent's companion files and a Codex thread's revision files included; owner credit adds no sessions, so role session counts still sum to at most the fleet total in each window.

Two stamps say how far back the ledgers reach:

| Field | Value |
| --- | --- |
| `roles.recorded_since_secs` | the earliest parsed role record's epoch second across all files of the two retained years, over all roles |
| `roles.effort_since_secs` | the earliest retained effort record, whether or not it joins a known role |

Empty ledgers publish `None`, and malformed tails contribute nothing. Both stamps name the first retained record, not necessarily the first ever, and effort compaction can move its earliest stamp within a UTC day.

Solo and team-role spend together is a subset of fleet spend in every window. Their remainder includes unrecorded sessions and subagents without a recorded owner. Subagents is a cross-cut: owned work is already counted in its owner, and ownerless work is in the remainder. Provider totals include every session whatever its provenance. Published model values carry `{tally, kind}`: the producing kind with the greatest year dollars wins, with ties broken by greater year tokens, then by ascending kind name. No model-family classifier participates.

## Session effort ledger

`accounts/.history/session-effort/<UTC-date>[.<tag>].jsonl` holds active-time releases and message counts across homes sharing accounts, independent of room and run retention. Each record carries provider `kind`, native `session`, RFC 3339 `at`, and a flattened `event`:

| Event | Payload | Meaning |
| --- | --- | --- |
| `active` | `active_ms` | Newly released active-time credit, in milliseconds. |
| `from_user` | none | A newly appended Prompt whose recorded origin is Human. |
| `from_agent` | optional `sender_kind`, `sender_session` | A newly appended Message. A resolved sender identity uses the sender's native session. |
| `totals` | `active_ms`, `from_user`, `from_teammates`, `to_teammates` | The compaction record shape: totals for one provider, session, and UTC day, stamped at that day's latest event. |

### Appending

The append chooses its daily segment from the current wall clock, not from the event's `at`, so a delayed frame cannot write into an old segment. It reads no ledger data and takes no lock: one `append_record_bytes`, then `sync_file_data`, with a parent sync on creation. Every append starts with a newline, which separates it from a possibly torn previous tail without reading that tail. Readers ignore blank lines, malformed records, and unknown fields. A hook write error warns and leaves the hook reply unchanged.

### Active-time releases

The runtime active-time sidecar keeps a release watermark under its per-record lock, and the hook appends only nonzero releases, after the sidecar update returns.

| Operation | Releases |
| --- | --- |
| Stop | all unreleased credit, even when stamped at or before `last_progress` |
| progress or pulse | the unreleased credit, at least ten minutes after the last release. Nothing when stamped at or before `last_progress`. |

A Stop releases past `last_progress` because a turn error's boundary can trail the last frame, and its credit would otherwise wait for the session's next turn. The watermark makes a repeated or replayed Stop release nothing. Each operation returns the released milliseconds, and a pulse also returns its changed flag. The release uses the operation's timestamp, including the provider's error boundary for a Stop, and a side conversation credits its host. A sidecar without a watermark treats its credited total as released on first touch, so no credit is backfilled. The grace-capped live tail that attribution reads is not released until an operation credits it.

### Message effort

Message effort follows the transcript entry actually appended. An ask-answering prompt records Answer, not Prompt, and contributes nothing, as do harness prompts, waits, and subagent reports. A queued agent message resolves its sender's launch id through the hook's room snapshot, and a parent-authored launch brief uses the parent's row. A missing sender row or an unavailable snapshot leaves the sender identity absent, and a header-only message never credits a sender. Every received Message counts as `from_agent`, whatever its sender's team. Sender-side credit exists only when that identity resolved.

Replay skips entries the transcript already recorded, so it appends effort only for newly written entries. A process crash between updating the sidecar or transcript and appending effort loses that record, and replay of a completed effect does not double count it. That holds for process crashes only. The effort append is data-synced, but the transcript entry and the sidecar it dedupes against are not, so after a power cut a replay can count a message or a release again.

### Folding and compaction

The spending walker, the checkpoint publisher, and the empty-fleet publisher share `crates/rimz/src/agents/spending/session_roles.rs::load_history`, which loads provenance and effort together. Effort joins by provider kind and native session through the same placement rule as spend.

- An unindexed session contributes no effort.
- An indexed session with no spend gets a role entry only when it has a retained effort record inside the windows (`crates/rimz/src/agents/spending/session_effort.rs::fold`). Provenance alone creates none.
- A `from_agent` record adds `from_teammates` to the receiver. When the sender resolved and is indexed, it also adds `to_teammates` to the sender, whatever the receiver's provenance.

Windows use the spend fold's local-midnight starts and 7, 30, and 364-local-day membership, with no headline window. Active credit lands at the release timestamp, even across midnight, and messages at their transcript timestamp. Each retained daily segment has a process-wide stat-keyed parse memo. An unchanged segment reuses its parsed records, and a deleted, expired, or different-root segment leaves the memo.

Only an elected spending publisher holding the shared maintenance lock under a compatible format mark maintains the ledger, never the hook. A segment whose entire UTC day is older than `RAW_RETAIN_SECS` (8 days) is durably replaced in place with `totals` records grouped by provider, native session, and the UTC day of each event's `at`. Each tagged copy compacts under its own name, never into a sibling segment. Sender-side message credit is materialized into the sender's totals. The latest event timestamp in each group becomes its `at`, which preserves window totals to UTC-day granularity. The seven-local-day window keeps raw timestamps, because its segments are not compacted. Compacted segments are left untouched. A segment whose entire UTC day is older than 366 days is deleted with a directory sync. A maintenance error warns and does not block spend publication.

## See also

- [providers.md](./providers.md): accounts, subscription windows, paid usage, and the provider panel the headline paints on.
- [budget.md](../harness/budget.md): the dollar caps that read session and account-day spend.
- [stats.md](../stats.md): the `rimz stats` reader over `provider-spending.json`.
- [attribution.md](./attribution.md): the seat groups the seat fold reads, and attribution's own use of it.
- [state.md](../sidebar/state.md#published-lanes): every published cache and its scope.

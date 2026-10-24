# The stats panel

`rimz stats` renders account-global token and dollar history as a spend-first report or a held dashboard with a model spend chart, token heatmap, role breakdowns, compact activity summary and assist counts. It touches no agent and writes nothing to any session, and it runs in or out of a room and in or out of a project. Its only writes are the shared state directories and, on a cold run, the spending cache it computes ([where the figures come from](#where-the-figures-come-from)) and the effort ledger's maintenance, which compacts daily segments older than eight days and deletes those past a year ([spending.md](./agents/spending.md#session-effort-ledger)).

The command has two homes. A one-shot `rimz stats` prints the spend-first report and returns to the shell. `rimz stats --refresh` holds the compact dashboard open as a live screen, and that held pane is the default middle-column content of the `rimzd` view ([rimzd.md](./rimzd.md#what-the-view-contains)).

The code is [`crates/rimz/src/cli/stats/`](../../crates/rimz/src/cli/stats/mod.rs). The user-facing counterparts are the [Token Insight guide](../guide/insight.md), which explains what every figure means, and the [stats CLI reference](../reference/cli/stats.md).

## What it renders

The one-shot panel leads with dollars, tokens and spend per active day for Today, Week, Month, and Year, then selected-window Models, Providers, Agents, Teams, and Subagents, unassigned dollars, the local-day heatmap, and publication time with selected-window sessions and cost per session. The wordmark belongs only to the held view. Dollar totals use whole dollars from $10 up and cents below (`$0.12`), so a small fleet never reads `$0`; cost per session always keeps cents. Session and run counts use grouped thousands. Empty sections and zero-dollar rows without recorded effort are omitted; unpriced tokens still contribute to window totals and the heatmap. With no daily history and no selected-window role usage, the full report prints the fresh-machine copy and publication time instead.

`panel::render_spend` owns one-shot and unfolded profile text. `dashboard::Dashboard` owns pure held frames, including the spend chart, terminal fitting, wordmark, heatmap and assist counts.

## Where the figures come from

Stats owns no pricing or session placement. It reads `provider-spending.json` under the shared state root (`RuntimePaths::shared_provider_spending_path`), the aggregate the spending walk publishes with per-day buckets, per-model and per-provider tallies, and the trailing windows ([spending.md](./agents/spending.md#what-reads-the-totals)). `Stats::from_provider` is the whole translation: the cache's `days`, `models`, and `spending.by_provider` become `by_day`, `by_model`, and `by_provider`; `chart` retains bounded model dollars for 30 local days and 52 seven-day blocks. Solo profiles, team roles, and subagent profiles retain their published `RoleTally` spend and effort together. Independent first-retained role and effort times, timezone, and publication time travel with them. `RoleView::rows` sums a team's selected-window role spend with `SpendWindow::merge` and sums effort counters; it stores no second team total.

Three load paths in `mod.rs` answer a run:

| Path | Runs when | What happens |
| --- | --- | --- |
| Published cache | a one-shot run finds a current-version cache aggregated for today in its timezone | read and render, with no walk |
| Cold walk | a one-shot run finds the cache missing, on another history day, or on another schema version | `refresh_global_spending_direct` walks with a fresh `SpendingWalker`, coalescing on `shared_spending_lock` |
| Elected service | every refresh of the held `--refresh` dashboard | `spending::service::request` with `SpendingServiceStartup::HostEligible`, so the dashboard can host the warm walker ([spending.md](./agents/spending.md#one-walk-per-namespace)) |

A one-shot run refreshes when the cache's `chart.today` differs from today's local epoch day in its published `timezone`. `ProviderSpendingCache::is_from_today` owns this rule for both one-shot loaders and the producer's TTL and timeout grace. The history day comes from the walk's aggregation time, not `refreshed_at_ms`, which can land after midnight when a walk straddles it. The account-day `day_cutoff_secs` instead belongs to the configured/system zone and is not read through the history zone. After refreshing, stats rechecks the version and history day: a non-authoritative empty discovery can preserve an old nonzero publication, which the one-shot loader rejects with an error. Within the same history day its figures are as fresh as the last walk a sidebar producer, a held dashboard, or a cold run published. The held error fallback deliberately accepts any current-version cache.

The cold walk's single flight is short. A process that finds the lock held waits up to 15 steps of 20 ms (`SPENDING_WAIT_STEPS`, `SPENDING_WAIT_STEP` in `agents/spending/engine.rs`) for a fresh publication, then walks locally without publishing. Only the lock holder writes the cache, so concurrent cold runs each pay a full walk.

The cold walk shows a progress spinner (`Reading session files [bar] n/total`) only when the run is human-facing and both stdout and stderr are TTYs (`should_animate_cold_stats`); `--json` and pipes get no progress chrome. The spinner does not print a wordmark.

If the service call fails, the held dashboard falls back to any current-version published cache and reports an error only when none exists. Setup on every path creates the shared directories only (`ensure_shared_runtime`); stats never opens a workspace tree.

## Windows

`Window` has four variants in tab order: Today (local calendar day), Week, Month and Year (trailing 7/30/365 days). `--window` selects every breakdown and the held view's initial tab, defaulting to Today; Tab and Shift-Tab cycle the tabs. Today selects `SpendTally.day`, independently of the configured sidebar headline. The heatmap and four-window Spend row ignore selection. `--window` conflicts with the all-retained `--assists` timeline.

Today and Year preserve the recent 28-day activity count under the explicit `RECENT_ACTIVITY_DAYS` constant; their busiest day uses cached history. Week and Month scope activity and compare spend with the preceding equal span. Both text surfaces' heatmap keys use the full-history current streak in every window. One-shot Spend figures right-align to fixed column edges, so the Week and Month trends run into the next column's padding and no figure moves with or without a trend. Both text views use `per_active_day`: dollars and the count of days with tokens cover the same last 7/30/365 local calendar days, including today, rather than mixing trailing-second totals with calendar counts. The one-shot average shows `–` for Today. Chart titles compare Today with yesterday and show the shared average for Week, Month and Year. `trend_label` caps changes above 999% as `↑999%+` so labels stay within the one-shot table's fixed slots. Today and Year have no spend trend. Held assist counts use local midnight in the publication's timezone for Today and the selected trailing window otherwise; one retained-log read supplies all four suffixes per repaint. The standalone assist timeline reads both retained log generations without a date cutoff.

Every token total on the panel is `SpendWindow::display_tokens`, which adds cache-read tokens to input plus output. A day counts as active when its bucket has tokens, priced or not.

## The heatmap

Each cell is one local calendar day in the published timezone, columns are weeks opening on Monday, and the rightmost column is the current week with future days left blank. The week count comes from the terminal width (`weeks_for_terminal`: columns minus the 6-column gutter, halved, clamped to 4 through 52), so a narrow terminal shows as few as four weeks of history. `--dollars` shades by `DaySpend::usd` instead of tokens and retitles the block `Spend activity`.

Shading is relative to the visible grid (`Grid::build`, `shade`, `level` in `panel.rs`):

1. The ceiling is the 90th-percentile value among active days in the grid (`HEAT_CEILING_PERCENTILE`), so one outlier day does not flatten the rest.
2. A day's value over the ceiling is clamped to 1, raised to the power 0.5 (`HEAT_GAMMA`), and floored at 0.15 (`HEAT_TRACE_FLOOR`).
3. `level` maps the result onto the ramp `· ░ ▒ ▓ █`: `·` only for a day with no usage, and at least `░` for any active day.

The month row labels the column where each month begins, and the `Less · ░ ▒ ▓ █ More` key closes the block beside the activity count, current full-history streak and busiest date. One-shot spacing retains blank lines inside the heatmap; the held view removes them.

## The breakdowns

One-shot sections rank by dollars. Each keeps three rows plus a counted tail named for the section; `--agents`, `--teams`, `--subagents` and JSON are unfolded. One-shot and JSON shares divide by the selected window's fleet dollars. Providers includes all provider spend; solo, team, and subagent sections are subsets, not exclusive partitions of Providers. Role semantics and native-child attribution belong to [spending](./agents/spending.md). The one-shot `show_agents` predicate requires a selected-window profile with spend or recorded effort whose key is absent from `by_provider`. When Agents shows, provider-named rows remain included. Teams and Subagents hide only when empty. Explicit views and JSON never apply the hide predicate. Effort means any active-time or message counter, including sender-only credit.

One-shot text shows one muted coverage line above the first visible profile or team section when the selected window starts before `recorded_since_secs` or `effort_since_secs`, independently: `Window::assist_since` anchors Today at local midnight and the trailing windows at publication time minus 7/30/365 days. The roles clause reads `roles recorded since DATE, earlier spend counts as No role`. The effort clause reads `agent-time and messages recorded since DATE` on its own, or `agent-time and messages since DATE` after the roles clause and a middle dot. A report without visible profile or team sections omits the line. Explicit views keep the window rule even with no rows, and print a named-window empty-spend line. The held dashboard instead puts the applicable roles date in Where it went and never emits an effort-recording note.

One-shot Agents, Teams, and Subagents add `AGENT-TIME` after their session/run count, using the same `theme::fmt::active_duration` seconds formatter as attribution. Teams adds `MESSAGES`: summed incoming human prompts plus received agent messages, with the human count in parentheses when nonzero. Outgoing messages remain role-level JSON data and do not inflate the received total. Zero effort cells read `-`. Compact views put these before tokens; explicit views append them after the existing detailed columns. No panel unfolds team roles.

After the role sections, the full report prints `No role recorded` as `max(0, fleet − agents − teams − subagents)` and its fleet share, hiding values below one cent. Explicit role views omit it and the footer's session clause. The full footer selects sessions from the fleet tally, with `today`, `this week`, `this month`, or `this year`, and divides window dollars by sessions with two decimal places. Zero sessions omit the clause.

The held view puts Models full-width, Providers beside Where it went, Agents beside Subagents, then Teams full-width. Models has HIT, TOK/S and TOKENS; Teams has AGENT-TIME, MESSAGES and TOKENS. Provider names come from provider descriptors; model names use `model_display::display_model`, with `Unknown model` for an empty id. Model markers and chart segments use `palette::model_shade`, ranked within the published producing kind by Year dollars, then tokens, then id. Other is muted. Token units are uppercase. One-shot columns, count headers and tail nouns follow a typed section, never its display label.

Held rows rank by dollars, then work tokens, then key. Models and role sections keep named rows at 1% of their own section's dollars; providers skip the threshold. The limit is four named rows, or two from fit step 5, plus Other. A single-entry tail retains its name. `RoleWindow::merge` preserves spend, agent-time and messages through folding. Role bars and percentages divide by section dollars; model and provider shares divide by fleet dollars. Where it went splits Teams, Subagents, Agents and `max(0, fleet − roles)` as No role, normalizing that bar to the parts' sum. Teams hides without team spend.

The cache-hit column is `SpendWindow::cache_hit_percent`: `cache_read / (cache_read + input)`, where `input` already includes cache writes, rounded half up and absent for a zero denominator. `CacheHealth::classify` colors it green at 90% and above, yellow from 70% to 89%, and red below.

One-shot Providers and compact profile columns use `SpendWindow::tokens_per_sec`: rounded `timed_output * 1000 / timed_ms` for the selected window. An untimed row shows `-`. `SpendWindow::merge` sums both counters, so one-shot tails keep the ratio of sums. Held Models always keeps its TOK/S column, but Other prints `–`; held Providers and role rows omit the rate. Timing boundaries and filters belong to [spending](./agents/spending.md#response-timing).

The held chart stacks the drawn period's four largest models, largest at the bottom, plus Other. Daily bars span 30 days; weekly bars span the newest 52 blocks that fit, anchored to the publication's `chart.today` so a midnight repaint or unnamed system zone cannot shift them off their seven-day keys. Positive spend gets at least one cell. A daily dashed line averages the preceding 30 fleet days, missing days as zero; the y ceiling rounds up over both that mean and the tallest bar. The bar stride keeps room for the line's `┄ $X` tail only, so a caption too wide for the frame drops its `typical` word, and the bars never narrow for it. Days outside the selected window are muted; the final axis label is accented.

## Fitting the terminal

The dashboard is a pure string renderer over published stats, per-tab assists, the active tab and supplied terminal dimensions. Width is `min(cols, 113)`, centred independently of the heatmap week count. Below 112 columns pairs stack and bars shrink. Every line is clipped by display width while preserving ANSI styles. The final frame joins lines without a trailing newline, so replacing a frame of exactly the terminal height cannot scroll its first row offscreen.

The fitter renders all four tabs at each cumulative step and selects the first whose tallest frame fits: full layout; drop the wordmark; collapse Assists; reduce the chart from 12 to 8 rows; drop Agents and Subagents; reduce named-row limit from four to two; reduce the chart to 6 rows; drop Teams; reduce the chart to 4 rows; drop the heatmap. All tabs share the step. Beyond the last step the bottom is clipped to the terminal height. Unknown row count uses the full layout. Empty history retains the wordmark while it fits, then the fresh-machine copy and any assists. Sections have one blank separator and no bottom padding.

One-shot text is a scrollable report and does not use the held screen's row budget.

Held share bars use `resolve_panel_glyphs` to resolve the filled and track meter roles through the machine theme; colors stay on the CLI palette ([theme.md](./theme.md#glyphs)). Chart stacks use solid cells, while Where it went and the heatmap retain their density glyphs.

## The held dashboard

`--refresh` runs `hold::run_refresh`. `TerminalModeGuard` puts the terminal in raw mode on the alternate screen with mouse capture off, so the dashboard owns the pane without adding mux scrollback, and keypresses and stray mouse reports arrive as events the loop drains.

One reusable worker thread loads through the elected service each cycle and sends back one `Result<Stats>`, with a panic caught and turned into an error. The foreground polls for events every 100 ms (`REFRESH_POLL_TICK`) and starts the next cycle once the refresh has landed and the 60-second deadline (`REFRESH_INTERVAL`) has passed. Until the first successful frame, a landed result moves the deadline to 5 seconds out (`EMPTY_REFRESH_RETRY`).

A failed refresh never exits. The first failure in a streak logs a warning and later ones log at debug until a refresh succeeds. `main.rs` turns stderr logging off for `stats --refresh`, so warnings cannot smear the raw-mode frame; the reporting layer still receives them.

Each repaint picks one of three frames:

| State | Frame |
| --- | --- |
| a stats frame exists | the panel, even while later refreshes fail; no staleness marker |
| no frame yet, a refresh failed | the wordmark and a centred `Spending refresh unavailable - retrying. <cause>`, ellipsized to the panel width |
| no outcome yet | nothing is written |

A resize repaints the current frame, and the first successful refresh replaces the unavailable frame in place. Each repaint reads `assist_log::recent(root, None)` once; `TabAssists::from_records` folds the sorted suffix for each tab. The earliest retained record dates the Assists header only when later than the selected window's start. Spending is not reread on a tab switch. The frames are identical under `--hold`.

| Key | Outcome |
| --- | --- |
| `Tab` / `Shift-Tab` | cycle the window and repaint from the stats in hand, reloading only the assist log |
| `r` / `R` | reload the binary in place |
| `q`, `Ctrl-C` | quit, unless `--hold` is set |

Reload re-execs `reload::current_reexec_target()` with the original arguments. The `r` key returns the reload outcome directly; `SIGUSR1` sets a flag the cycle reads and clears, which is how `rimz reload` restarts running dashboards. Registering that handler replaces `SIGUSR1`'s default terminate disposition. When no re-exec target resolves, the dashboard keeps running.

`--hold` is a hidden flag that requires `--refresh` and exists for the daemon view: `q` and `Ctrl-C` become no-ops, and closing the pane still ends the process. `daemon_content::stats_argv` is exactly `rimz stats --refresh --hold`; how `[daemon]` panes replace it and how a pane-count change reaches a running room are in [rimzd.md](./rimzd.md#the-content-supervisor).

## Machine-readable surfaces

Molt records project to `molts` in the assist rollup and the `Molts:` dashboard category. They count as recorded after placement, retaining both agents' provider, identity, and label plus the inherited generation in timeline JSON and forensic text. The successor identity is a launch id, not a provider session id.

`--json` emits publication `as_of` (RFC 3339 with offset), `timezone`, `unit`, `window`, optional local-date `roles_since` and independent `effort_since`, selected-window sessions, streak fields, `windows.today/week/month/year`, unfolded selected-window `models`, `providers`, solo `agents`, `teams`, `subagents`, local-date `days`, and the selected-window assist rollup only. Models and providers include every row with dollars or work tokens; profiles and teams also qualify with effort alone, without a row cap. All arrays are present even when empty. Activity fields keep the full-history computation so `active_days_28` retains its meaning. Providers keeps optional `tokens_per_sec`; profiles carry no model. Agent and team rows count `sessions`, subagent rows `runs`, and all carry `active_secs`. Teams include incoming `messages: {total, from_user}` and role rows carrying `role`, `sessions`, `active_secs`, `from_user`, `from_teammates`, `to_teammates`, and the usual spend fields; all shares remain fleet-dollar shares. Explicit JSON emits `{as_of, window, agents|teams|subagents, roles_since?, effort_since?}` with only the selected array and the same admission.

`--assists` filters through `AssistEvent::took_effect`, then limits the newest-first timeline to 50 by default. `-n 0` selects all retained; `--all` includes ineffective attempts. JSON uses the same selection and emits `{as_of, rollup, events, older}`, with `as_of` the read time in the configured zone that spend history uses (`MachineConfig::time_zone`). The rollup covers all retained records, not just the displayed page.

The same predicate gates rollup counts: no error for stall notices, since an undelivered notice waits in the parent's queue; delivered for continues, keepalives, idle and flip compactions; stopped for idle stops; frozen or a deepening step for hibernation; relaunched for launch retries; a window reset for redemption, since a stamp write that fails after it does not undo it; no error for GC. Idle-stop withdrawals, molts, judge declines, workflow transitions, resident launches, model aliases, tier fallbacks, compact commands and room resumes count as recorded. A successful stop may carry a cleanup error and still counts. A partial GC with no top-level error counts, as its record represents a completed sweep.

`crates/rimz/src/cli/stats/assists.rs::category_entries` takes a `CategoryMode`. Dashboard mode omits held-attempt detail, counts compact requests as `compacts + held["compacts"]`, and hides zero-count categories. Timeline mode keeps effective counts and held detail. Both modes group counts by thousands and describe Auto-continue's duration as `(after 5.1h parked)`, not time saved. These are text projections only; the rollup and timeline predicate do not change.

Every attempt that did not take effect increments `held` under the count it would have joined (`AssistEvent::count_key`), printed beside that count in the timeline header as `Auto-compact: N (M held)`. A `tmp_evict` with no error increments `tmp_evictions`, not `sweeps`, and adds its bytes to `reclaimed_bytes`; a failed removal increments `held.tmp_evictions`. The timeline header's `Temp evictions:` row shows that count and its held attempts, separately from `Auto-gc:`. Auto-gc's byte detail retains the combined reclaimed-byte total. Its timeline keeps the handle and workspace, allocated reclaimed bytes, optional latest owner end, and the measured free bytes and floor. Temp-unit bytes count hardlinked files once per tree; measured free space may lag removal, and snapshots may retain the blocks. Ordinary GC reclaimed bytes remain apparent sizes. Hibernate hold reasons remain diagnostic rollup information regardless of effect. Only freeze records count as hibernations; a deepening step (not frozen, with reclaimed bytes) takes effect without adding one, and memory bytes sum the per-step deltas from both. Hibernation reclaimed bytes remain separate from GC reclaimed bytes. The [assist log](./harness/loops.md#the-assist-log) owns the durable records and rotation.

## Where the code lives

| File | Owns |
| --- | --- |
| `mod.rs` | `StatsArgs`, `Window`, `Stats`, the three load paths, the shared constants, and the wordmark |
| `panel.rs` | one-shot breakdowns, shared heatmap and formatting helpers, unavailable frame, and `emit` |
| `dashboard.rs` | pure held frame, chart, folding, geometry and measured fit ladder, plus per-tab assist projection |
| `hold.rs` | the `--refresh` loop, key handling, the reload signal and re-exec, and the cold-walk spinner |
| `assists.rs` | the assist fold, its panel rows, and the `--assists` timeline |
| `json.rs` | the `--json` document |
| `fmt.rs` | token, dollar, and day formatting, display names, and `weeks_for_terminal` |
| `tests.rs` | the unit suite |

## Tests

`tests.rs` and its `tests/held.rs` child cover rendered strings; held snapshots cover all four tabs at 119×76 and 119×40 plus empty history. One-shot assertions cover active-day averages, trend alignment, independent coverage dates and placement, agent-time columns, clamped unassigned spend, grouped sessions, footer scope, activity and fresh-machine copy. A geometry matrix covers widths 60–160 and heights 10–90, checking line count, display width, common fit step and absence of a final newline. The load paths run against temporary `RuntimePaths` (a published cache served without a walk, and a cold refresh publishing the rollups the sidebar reads). The held loop is driven through `key_outcome` and `HeldStats` without a terminal.

```sh
cargo xtask test 'cli::stats'
```

## See also

- [spending.md](./agents/spending.md): the walk, the incremental cache, the spending service, and the pricing behind every figure.
- [loops.md](./harness/loops.md#the-assist-log): the assist log and its rollup.
- [rimzd.md](./rimzd.md): the daemon view that holds the dashboard.
- [theme.md](./theme.md): the palette and glyph resolution the panel uses.
- [Token Insight guide](../guide/insight.md): what the figures mean, for someone using RimZ.

# The stats panel

`rimz stats` renders account-global token and dollar history as a spend-first report or a held dashboard: a model spend chart, a token heatmap, totals for four windows, spend by model, provider, and profile, a compact activity summary, and the assists RimZ performed for the user. It runs in or out of a room and in or out of a project, and it touches no agent and no session. Stats itself writes only the shared state directories (`RuntimePaths::ensure_shared_dirs`), and it never opens a workspace tree. A walk that stats triggers writes what any spending walk writes: the spending publication, the shared price table (`pricing-cache.json`) when its price load refreshes, and the [effort ledger](./agents/spending.md#session-effort-ledger) maintenance.

The command has three surfaces, and most rules on this page differ between them:

| Surface | Command | Renderer |
| --- | --- | --- |
| One-shot report | `rimz stats`, or one of `--agents`, `--teams`, `--subagents` for an explicit view | `panel::render_spend`, printed once as a scrollable report |
| Held dashboard | `rimz stats --refresh` | `dashboard::Dashboard`, repainted on the alternate screen, and the default middle-column content of the [`rimzd` view](./rimzd.md#what-the-view-contains) |
| JSON | `--json`, alone or with an explicit view, and `--assists --json` | `json.rs`, and `assists.rs` for the assist timeline |

The code is [`crates/rimz/src/cli/stats/`](../../crates/rimz/src/cli/stats/mod.rs). What every figure means for someone using RimZ is the [Token Insight guide](../guide/insight.md). Every flag and JSON field is in the [stats CLI reference](../reference/cli/stats.md).

## Where the figures come from

Stats owns no pricing and no session placement. It reads `provider-spending.json` under the shared state root (`RuntimePaths::shared_provider_spending_path`), the aggregate the spending walk publishes ([spending.md](./agents/spending.md#what-reads-the-totals)). `crates/rimz/src/cli/stats/mod.rs::Stats::from_provider` is the whole translation: the cache's `days`, `models`, and `spending.by_provider` become `by_day`, `by_model`, and `by_provider`, `chart` keeps model dollars for 30 local days and 52 seven-day blocks, and the solo, team, and subagent `RoleTally` maps keep their published spend and effort together. The two first-retained times (`recorded_since_secs` for roles, `effort_since_secs` for effort), the timezone, and the publication time travel with them. Team totals are computed on read: `RoleView::rows` merges each team's role windows, and no second team total is stored.

Three load paths answer a run:

| Path | Runs when | What happens |
| --- | --- | --- |
| Published cache | a one-shot or JSON run finds a current-version cache aggregated today | read and render, with no walk |
| Cold walk | a one-shot or JSON run finds the cache missing, on another schema version, or aggregated on another history day | `refresh_global_spending_direct` walks with a fresh `SpendingWalker`, coalescing on `shared_spending_lock` |
| Elected service | every refresh cycle of the held dashboard | `spending::service::request` with `SpendingServiceStartup::HostEligible`, so the dashboard process can host the warm walker ([spending.md](./agents/spending.md#one-walk-per-namespace)) |

"Aggregated today" compares `chart.today` with today's local epoch day in the cache's published timezone. `crates/rimz/src/agents/spending/publish.rs::ProviderSpendingCache::is_from_today` owns this rule for one-shot loaders and the producer's TTL and timeout grace. The history day comes from the walk's aggregation time, never `refreshed_at_ms`, which can land after midnight when a walk straddles it. The account-day `day_cutoff_secs` belongs to the configured/system zone and is not read through the history zone. After a cold walk, `load_or_refresh_stats` checks the version and history day again and fails with `Spending refresh unavailable: ...` when the result still does not cover today: a non-authoritative empty discovery can preserve an old nonzero publication, and the one-shot report refuses to print it as today's. Within one history day, the one-shot figures are as fresh as the last walk that a sidebar producer, a held dashboard, or a cold run published.

The cold walk's single flight is short. A process that finds the lock held polls up to 15 steps of 20 ms (`SPENDING_WAIT_STEPS` and `SPENDING_WAIT_STEP` in `crates/rimz/src/agents/spending/engine.rs`) and serves a fresh publication that lands meanwhile. When none lands, it walks locally without publishing (`compute_fleet_spending`). Only the lock holder writes the cache, so concurrent cold runs that outwait the poll each pay a full walk.

A cold walk shows the progress line `Reading session files [bar] n/total` through a delayed spinner only when the run is human-facing (no `--json`) and both stdout and stderr are terminals (`hold::should_animate_cold_stats`). Pipes and JSON get no progress output.

When the service call fails, the held dashboard falls back to any current-version publication, whatever its day, and reports the error only when none exists (`load_or_refresh_stats_via_service`).

## Windows

`Window` has four variants, in tab order: Today (the local calendar day in the publication's timezone), Week, Month, and Year (the last 7, 30, and 364 local calendar days, including today). `--window` selects the window for every breakdown, the insights, and the held dashboard's first tab, and defaults to Today. Today selects `SpendTally.day`, which is independent of the sidebar's configured headline window. The heatmap and the four-window totals ignore the selection.

Two token measures appear on the panel, and the difference is a trap:

| Measure | Definition | Used for |
| --- | --- | --- |
| Display tokens | `SpendWindow::display_tokens`: input (cache writes included) plus output plus cache reads | every token total printed, and the heatmap (`DaySpend.tokens` holds the same sum) |
| Work tokens | `SpendWindow::tokens`: input (cache writes included) plus output, without cache reads | row admission and ranking; held folding thresholds use dollars |

A day is active when its `DaySpend.tokens` is above zero, priced or not.

## The heatmap

Each cell is one local calendar day in the published timezone, columns are weeks opening on Monday, and the rightmost column is the current week, with future days left blank. The week count comes from the terminal width (`fmt::weeks_for_terminal`: columns minus the 6-column gutter, halved, clamped to 4 through 52), so a narrow terminal shows as few as four weeks of history. `--dollars` shades by `DaySpend::usd` instead of tokens and retitles the block from `Token activity` to `Spend activity`.

Shading is relative to the visible grid (`Grid::build`, `shade`, and `level` in `panel.rs`):

1. The ceiling is the 90th-percentile value among active days in the grid (`HEAT_CEILING_PERCENTILE`), so one outlier day does not flatten the rest.
2. A day's value over the ceiling is clamped to 1, raised to the power 0.5 (`HEAT_GAMMA`), and floored at 0.15 (`HEAT_TRACE_FLOOR`).
3. `level` maps the result onto the ramp `· ░ ▒ ▓ █`. A day whose value is zero reads `·`, and any other day reads at least `░`.

Under `--dollars` the value is dollars, so a token-active day with no priced usage reads `·` although it counts as active everywhere else.

The month row groups the columns by the month of each column's Monday and labels a group only when it spans at least two columns. A month that begins midweek is therefore labelled one column late, and a one-column fragment at either edge goes unlabelled. The `Less · ░ ▒ ▓ █ More` key closes the block beside the activity count, current full-history streak, and busiest date. One-shot spacing keeps blank lines inside the heatmap, and the held dashboard removes them.

## The breakdowns

Five sections break the selected window down: Models, Providers, Agents (solo profiles), Teams, and Subagents (delegated profiles). Providers carries all provider spend. The three profile sections are subsets of it, not a partition, and role semantics and native-child attribution belong to [spending.md](./agents/spending.md#session-role-provenance). One-shot and JSON shares divide by the selected window's fleet dollars. Held model and provider shares do too, while held role shares divide by their own section's dollars. A share against zero dollars is zero.

### Which rows enter

| Surface | A model or provider row enters with | A profile or team row enters with |
| --- | --- | --- |
| One-shot report | dollars | dollars or recorded effort |
| Held dashboard and JSON | dollars or work tokens | dollars, work tokens, or recorded effort |

Effort is any active-time or message counter, sender-only credit included. A section with no admitted rows is omitted, and unpriced tokens still count toward the window totals and the heatmap. The held dashboard also hides Teams without team spend.

The one-shot Agents section hides unless it would list a profile that is not named for a provider. `panel::show_agents` shows it when an admitted selected-window profile has a key absent from `by_provider`. Once Agents shows, its provider-named rows stay in it. One-shot Teams and Subagents hide only when empty. The held dashboard, explicit views, and JSON never apply the predicate.

### The one-shot report

The one-shot report opens with a Spend row of dollars, display tokens, and spend per active day for all four windows. Its Today column reads the `by_day` bucket for today, while Week, Month, and Year read the `SpendTally` windows. The breakdowns follow, with the [coverage line](#recorded-since-notes) above the first role section, then unassigned dollars, the heatmap, and `as of HH:MM` (with the date when the publication is from another day). An explicit view prints only its own section, with the coverage line, and no Spend row or heatmap. With no daily history and no selected-window role usage, the full report prints the fresh-machine copy and publication time instead.

Each section ranks by dollars, then name, keeps three rows, and merges the rest into a counted tail named for the section (`2 other models`). Explicit views are unfolded. Column choice follows the typed `SpendSection`, never its display label:

| Section | Columns after SPEND and SHARE |
| --- | --- |
| Models | INPUT, OUTPUT, CACHED, HIT |
| Providers | SESSIONS, TOKENS, HIT, TOK/S |
| Agents, Teams, Subagents (compact) | SESSIONS (RUNS for Subagents), AGENT-TIME, MESSAGES for Teams, TOKENS, HIT, TOK/S |
| Explicit view | INPUT, OUTPUT, CACHED, HIT, SESSIONS or RUNS, AGENT-TIME, MESSAGES for Teams |

AGENT-TIME formats seconds with `theme::fmt::active_duration`, the formatter attribution uses. MESSAGES is incoming human prompts plus received agent messages, with the human count in parentheses when nonzero (`12 (3 you)`). Outgoing messages stay in role-level JSON and do not inflate it. A zero effort cell reads `-`. No surface unfolds a team into its roles except JSON. Session and run counts use grouped thousands.

After the role sections, the full report prints `No role recorded` as `max(0, fleet - agents - teams - subagents)` and its fleet share, hiding values below one cent (`panel::render_spend`). The footer takes sessions from the selected fleet window, labels them `today`, `this week`, `this month`, or `this year`, and divides window dollars by sessions with two decimal places. Zero sessions omit that clause. Explicit views omit both unassigned dollars and the footer's session clause.

### Held dashboard rows

The held dashboard has no Spend row. After the optional wordmark it draws the heatmap, dollar tab rail, spend chart, breakdowns, Assists, and key hint (`crates/rimz/src/cli/stats/dashboard.rs::Dashboard`). Models is full-width, Providers sits beside Where it went, Agents beside Subagents, then Teams full-width. Models has HIT, optional TOK/S, and TOKENS, and Teams has AGENT-TIME, MESSAGES, and TOKENS. Agents and Subagents rows carry only dollars, a share bar, and the share. A Providers row carries dollars, its fleet share (`<1%` below one percent), `N% hit` only from a 1% share up, and grouped sessions (`crates/rimz/src/cli/stats/dashboard.rs::Dashboard::provider_lines`). Provider names come from provider descriptors and model names from `model_display::display_model`, with `Unknown model` for an empty id. Model markers and chart segments use `crates/rimz/src/cli/render/palette.rs::model_shade`, ranked within the published producing kind by Year dollars, then work tokens, then id. `Other` is muted.

Folding is measured against each section's own admitted dollars, not fleet totals. `dashboard::fold` ranks rows by dollars, then work tokens, then key. Models and role sections keep named rows at 1% of their own section's dollars, and providers skip the threshold. The limit is four named rows, or two from fit step 4, plus `Other`. A single-entry tail keeps its name. `RoleWindow::merge` preserves spend, agent-time, and messages through folding. Where it went splits Teams, Subagents, Agents, and `max(0, fleet - roles)` as No role, normalizing that bar and its percentages to the parts' sum.

The chart stacks the drawn period's four largest models, largest at the bottom, plus Other (`crates/rimz/src/cli/stats/dashboard.rs::Dashboard::chart_lines`). Today draws 24 wall-clock hours from `chart.hours`; Week seven days and Month thirty from `chart.days`; Year the newest weekly blocks that fit, up to 52, from `chart.weeks`. All are anchored to the publication's `chart.today`, preserving their keys over a midnight repaint. The plot has seven rows while the heatmap shows. Bars round to eighth cells, with at least `▁` for positive spend. Week and Month's dashed typical line averages the preceding 30 fleet days, missing days as zero. The y ceiling is the smallest of `{1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8} × 10^k` covering the drawn bars, forecast, yesterday overlay, and typical line. Its label tops the plot; half the ceiling labels the fourth of seven rows from the bottom. Bar stride reserves the worded `┄ typical $X` caption whenever bars can remain at least two columns wide; otherwise it reserves only `┄ $X`, retaining the word if it still fits. The final axis label is accented; it starts under the newest bar when that bar is wide enough to hold it, and otherwise ends at the plot edge on every range. Date labels that would touch or overlap the previous label are skipped.

The held-only forecast is today's dollars plus the previous seven local days' mean dollars in hours after the walk's local wall-clock hour, missing days as zero and clamped to at least spend so far (`crates/rimz/src/cli/stats/dashboard.rs::Dashboard::forecast`). The hour comes from `ChartSpend.hour`, taken at the same instant as `chart.today` and the day tally, not the later `refreshed_at_ms` or repaint time. Today prints `today X so far · ~F by midnight · yesterday Y`; yesterday's same-hour total draws faint cells above today's bar and fills future hours. Week, Month and Year's bar holding today extends by `F − X`. Its top segment's emitted model tone starts halfway toward the scheme background through `crates/rimz/src/theme/palette.rs::Palette::forecast_tone` and `crates/rimz/src/cli/render/palette.rs::model_forecast`, advancing when depth quantization would preserve the model tone or increase its contrast. It uses neither dim effects nor shade glyphs. No extension draws when `F = X`; an increase that rounds to no additional eighth leaves the spend tone unchanged. Today has no typical line or forecast extension.

In a cell shared with an extension, the spent eighths keep their model-tone glyph over a forecast-tone background. A partial spend cell whose yesterday overlay continues above it similarly uses a faint background. Spend is measured at the cell's spent midpoint against actual dollars, never the projected bar. The background carries the comparison to the cell top, overstating it by less than one cell while keeping spent eighths exact. `HeldStats::repaint` passes the stream's colour choice to the dashboard. With colour off (`NO_COLOR`, `CLICOLOR=0`, or `--color never`), the overlay and extension are omitted from both the plot and its ceiling; the Today header keeps the forecast and yesterday text.

The HIT column is `SpendWindow::cache_hit_percent`: `cache_read / (cache_read + input)`, where `input` already includes cache writes, rounded half up, and absent for a zero denominator. `CacheHealth::classify` colours it green at 90% and above, yellow from 70% to 89%, and red below.

The rate column is `SpendWindow::tokens_per_sec`: `timed_output * 1000 / timed_ms` for the selected window, rounded half up. One-shot Providers and compact profile rows print it, with `-` for an untimed row. `SpendWindow::merge` sums both counters, so one-shot tails carry the ratio of sums. Held Models hides TOK/S when every named row is untimed; `Other` always prints a U+2013 placeholder when the column shows and does not count toward that decision. Held Providers and role rows omit the rate. Timing boundaries and filters belong to [spending.md](./agents/spending.md#response-timing).

Held share bars use `panel::resolve_panel_glyphs` to resolve the filled and track meter roles through the machine theme, while colours stay on the CLI palette ([theme.md](./theme.md#glyphs)). Chart stacks use solid cells with eighth-block tops, and Where it went and the heatmap keep their density glyphs. Dollar totals use `fmt::fmt_usd`: whole dollars with thousands grouped from $10 up, and cents below (`$0.12`), so a small fleet never reads `$0`. Cost per session always keeps cents. Token units are uppercase.

### Recorded-since notes

A profile section can cover a window that starts before RimZ recorded roles or effort, and a muted coverage line says so. Each clause shows independently when the selected window starts before its first-retained time: `roles recorded since DATE, earlier spend counts as No role` for `recorded_since_secs`, and `agent-time and messages recorded since DATE` for `effort_since_secs`. After a roles clause and a middle dot, the effort clause shortens to `agent-time and messages since DATE`. The window start is `Window::assist_since` at publication time: local midnight for Today, and the start of local day today minus 6, 29, or 363 for the others.

The one-shot coverage line prints above the first visible profile section, and a report without a visible profile section omits it. An explicit view keeps the window rule even with no rows and prints a `No <noun> spend recorded (<window>).` line. The held dashboard puts the applicable roles date in Where it went and emits no effort-recording note. Effort-only history still renders profile sections when there are no spend-day buckets.

## Insights

The one-shot activity summary and JSON's `active_days_28` keep `panel::Activity::of`. The one-shot heatmap key follows this table:

| Figure | Today and Year | Week and Month |
| --- | --- | --- |
| Active days | active days of the last 28 (`RECENT_ACTIVITY_DAYS`) | active days in the window |
| Busiest date | the whole cached history | days in the window |
| Current streak on both text surfaces | the whole cached history | the whole cached history |
| Spend trend | none | the change against the preceding equal span, when that span has dollars |

The current streak counts back from today, or from yesterday when today has no activity yet, so a quiet morning does not break a streak.

The held heatmap instead counts active days and chooses the busiest within its drawn span, beginning at `Grid::col_monday(0)` and ending today. Its footer is identical on every tab: `N active days since Mon D · streak N days · busiest Mon D`, with singular `day` for one and `–` when no day is active. Both dates print `Mon D YYYY` when they fall in an earlier calendar year than today, keeping the day unpadded. The one-shot legend's date format is unchanged. The streak still reads the full history. A summary too wide beside the ramp key moves below it and wraps to the frame width.

Both text surfaces use `panel::per_active_day`: dollars and days with tokens cover the same last 7, 30, or 364 local calendar days, today included. The one-shot average prints a U+2013 placeholder for Today. Chart titles compare Today with yesterday and show the shared average for Week, Month, and Year. One-shot Spend figures right-align to fixed column edges, so Week and Month trends run into the next column's padding without moving any figure. `panel::trend_label` caps changes above 999% as `↑999%+` so they fit those slots. Today and Year have no spend trend.

## Fitting the terminal

The dashboard is a pure string renderer over published stats, per-tab assists, the active tab, and supplied terminal dimensions (`dashboard::Dashboard::frame`). crossterm appears only at the held loop's terminal edge (`tui::TerminalModeGuard`, `tui::replace_frame`, and key events), and the sidebar pane's ratatui widget stack is not on this path, so tests assert on rendered text.

Width is `min(cols, 113)`, centred independently of the heatmap week count. Below 112 columns pairs stack and bars shrink. Every line is clipped by display width while preserving ANSI styles. On a tty, top padding is `(rows − tallest_tab_height) / 2` at the chosen fit step, shared across tabs. The final frame joins lines without a trailing newline, so replacing a frame of exactly the terminal height cannot scroll its first row offscreen.

The fitter renders all four tabs at each cumulative step and picks the first whose tallest frame fits. All tabs share that step:

1. Full layout (step 0).
2. Drop the wordmark (step 1).
3. Collapse Assists (step 2).
4. Drop Agents and Subagents (step 3).
5. Reduce the named-row limit from four to two (step 4).
6. Drop Teams (step 5).
7. Drop the heatmap (step 6).
8. Reduce the plot from seven to four rows (step 7).
9. Reduce the plot to two rows (step 8).

Beyond the last step, the body is clipped to terminal height while the final hint stays. An unknown row count uses the full layout without vertical padding. Empty history keeps the wordmark while it fits, then the fresh-machine copy, any assists, and the hint. Sections have one blank separator and no bottom padding. Only the held dashboard fits: the one-shot report scrolls and has no row budget.

## The held dashboard

`--refresh` runs `hold::run_refresh`. `TerminalModeGuard` puts the terminal in raw mode on the alternate screen with mouse capture off, so the dashboard owns the pane without adding mux scrollback, and keypresses and stray mouse reports arrive as events the loop drains.

One reusable worker thread (`hold::RefreshWorker`) loads through the elected service each cycle and sends back one `Result<Stats>`, with a panic caught and turned into an error. The foreground polls for events every 100 ms (`REFRESH_POLL_TICK`) and starts the next cycle once the refresh has landed and the 60-second deadline (`REFRESH_INTERVAL`) has passed. Until the first successful frame, a landed result moves the deadline to 5 seconds out (`EMPTY_REFRESH_RETRY`).

A failed refresh never exits. The first failure in a streak logs a warning and later ones log at debug until a refresh succeeds. `main.rs::log_filter_for` turns stderr logging off for `stats --refresh` so a warning cannot smear the raw-mode frame, while the Sentry layer, when reporting is on, still captures it. A `RUST_LOG` in the environment overrides that default.

Each repaint picks one of three frames:

| State | Frame |
| --- | --- |
| a stats frame exists | the panel, even while later refreshes fail, with no staleness marker |
| no frame yet, a refresh failed | the wordmark and a centred `Spending refresh unavailable - retrying. <cause>`, ellipsized to the panel width |
| no outcome yet | nothing is written |

A history with no usage and no profile activity paints the wordmark while it fits, `No agent sessions recorded on this machine yet.`, the explanation that spend appears after the first session and comes from each agent's session files, any assist categories, and the hint. A resize repaints the current frame, and the first successful refresh replaces the unavailable frame in place. Each repaint reads `assist_log::recent(root, None)` once, and `dashboard::TabAssists::from_records` folds a sorted suffix for each tab. The earliest retained record dates the Assists header only when later than the selected window's start. A tab switch does not reread spending. Under `--hold` only quit handling and the hint differ.

| Key | Outcome |
| --- | --- |
| `←` / `→`, `h` / `l` | move one range without wrapping and repaint from the stats in hand |
| `t` / `w` / `m` / `y` (either case) | jump to Today / Week / Month / Year |
| `Tab` / `Shift-Tab` | cycle the window with wrap and repaint from the stats in hand |
| `r` / `R` | reload the binary in place |
| `q`, `Ctrl-C` | quit, unless `--hold` is set |

Reload re-execs `reload::current_reexec_target()` with the original arguments through `reload::exec_current_args`. The `r` key returns the reload outcome directly. `SIGUSR1` sets a flag that the cycle reads and clears, and registering that handler replaces `SIGUSR1`'s default terminate disposition. `rimz reload` signals standalone dashboards and stats children of non-following content supervisors. A following content supervisor instead stops its stats child, execs the room's recorded build, and starts stats fresh from that image ([rimzd.md](./rimzd.md#following-the-room-build)). When no re-exec target resolves, the dashboard keeps running.

The final hint is `←/→ range · t/w/m/y jump · r reload · q quit`. `--hold` is a hidden flag that requires `--refresh` and exists for the daemon view: the hint omits `q quit`, `q` and `Ctrl-C` become no-ops, and closing the pane still ends the process. `daemon_content::stats_argv` is exactly `rimz stats --refresh --hold`. How `[daemon]` panes replace it, and how a pane-count change reaches a running room, are in [rimzd.md](./rimzd.md#the-content-supervisor).

## Machine-readable surfaces

`--json` emits one document for the selected window (`json.rs::stats_json`):

| Field | Holds |
| --- | --- |
| `as_of`, `timezone` | publication time (RFC 3339 with offset) and its timezone |
| `window`, `unit` | the selected window, and `usd` or `tokens` from `--dollars` |
| `roles_since`, `effort_since` | the first-retained local dates, each omitted when unknown |
| `sessions` | selected-window sessions |
| `active_days_28`, `longest_streak`, `current_streak`, `most_active_day` | always the Year computation over the whole history, whatever the window, so `active_days_28` keeps its meaning |
| `windows.today/week/month/year` | display tokens and dollars per window, with tool counts when nonzero |
| `models`, `providers` | every row with dollars or work tokens, unfolded, where the empty model id is a row named `Other`, and each row has optional `cache_hit_pct` and `tokens_per_sec` |
| `agents`, `subagents` | every admitted profile, unfolded: `profile`, `sessions` (`runs` for subagents), `active_secs`, and the spend fields, with no model |
| `teams` | every admitted team with `sessions`, `active_secs`, incoming `messages: {total, from_user}`, the spend fields, and `roles[]` rows of `role`, `sessions`, `active_secs`, `from_user`, `from_teammates`, `to_teammates`, and the spend fields |
| `days` | every local-date bucket with tokens and dollars |
| `assists` | the selected-window [assist rollup](#the-assist-rollup), without events |

Every array is present even when empty, and every `share` is a fleet-dollar share. With an explicit view, `--json` emits `{as_of, window, agents|teams|subagents, roles_since?, effort_since?}` with only the selected array and the same admission (`json::roles_value`).

`--assists` prints the timeline instead (`assists::render_full`), and `--window` conflicts with it. It reads both retained log generations with no date cutoff, filters through `AssistEvent::took_effect`, and limits the newest-first list to 50 (`-n 0` shows all retained, `--all` adds the attempts that did not take effect). `--assists --json` emits the same selection as `{as_of, rollup, events, older}`, where `as_of` is the read time in `MachineConfig::time_zone`, the zone spend history uses, and the rollup covers every retained record, not just the page.

### The assist rollup

The rollup counts only assists that took effect. In the held dashboard and in `--json`, the records start where the selected window starts (`Window::assist_since` at the time of the read), with Today at local midnight in the publication's timezone. One retained-log read supplies all four dashboard suffixes per repaint. `AssistEvent::took_effect` is the one predicate for the rollup and the default timeline:

| Assist | Takes effect when |
| --- | --- |
| stall notice | it has no error once queued for the parent, including a notice later withdrawn because its child run ended before delivery |
| auto-continue, cache keepalive, idle compaction, flip compaction | it was delivered |
| idle stop | the agent's pane closed, even with a cleanup error |
| hibernation | it froze, or a deepening step reclaimed bytes |
| launch retry | the relaunch spawned |
| auto-redeem | a window reset, since a stamp write that fails afterwards does not undo it |
| GC and temp eviction | it has no top-level error, so a partial sweep counts |
| molt, idle-stop withdrawal, judge decline, workflow transitions, resident launch, model alias, tier fallback, compact command, room resume | always: each counts as recorded |

`crates/rimz/src/cli/stats/assists.rs::category_entries` takes a `CategoryMode`: Dashboard omits held-attempt detail, counts Auto-compact requests as `compacts + held["compacts"]`, and hides zero-count categories. Timeline keeps effective counts and held detail. Both modes group counts by thousands and describe Auto-continue's duration as `(after 5.1h parked)`, not time saved. These are text projections, not changes to the rollup or timeline predicate.

An attempt that did not take effect increments `held` under the count it would have joined (`AssistEvent::count_key`), and the timeline header prints it beside that count: `Auto-compact: N (M held)`. Four projections are easy to get wrong:

- A temp eviction increments `tmp_evictions`, never `sweeps`, and the timeline header's `Temp evictions:` row is separate from `Auto-gc:` and carries its held attempts. Its bytes join `reclaimed_bytes`, so the `Auto-gc:` byte detail is the combined GC and temp-eviction total. The timeline keeps the handle, workspace, allocated reclaimed bytes, optional latest owner end, measured free bytes, and floor. Temp-unit bytes count hardlinks once per tree, measured free space can lag removal, and snapshots can retain the blocks. Ordinary GC bytes are apparent sizes.
- Only a freeze increments `hibernations`. A deepening step takes effect without one, and `hibernate_reclaimed_bytes` sums the per-step deltas of both, kept apart from GC bytes. Hibernate hold reasons enter `hibernate_holds` whether or not the record took effect.
- `redeems` includes manual redemptions, which also count in `manual_redeems`. The `Auto-redeem:` row subtracts them, and no row prints manual redemptions, so their held attempts (`held.manual_redeems`) reach JSON only.
- The five workflow counts print as one `Workflow:` row, and the molts print as `Molts:`.

The durable records, their fields, and rotation belong to the [assist log](./harness/loops.md#the-assist-log).

## Where the code lives

| File | Owns |
| --- | --- |
| `mod.rs` | `StatsArgs`, `Window`, `RoleView`, `Stats`, the three load paths, the shared constants, and the wordmark |
| `panel.rs` | one-shot breakdowns, shared heatmap and formatting helpers, the unavailable frame, and `emit` |
| `dashboard.rs` | the pure held frame, chart, folding, geometry and measured fit ladder, and per-tab assist projection |
| `hold.rs` | the `--refresh` loop, key handling, the reload signal and re-exec, and the cold-walk spinner |
| `assists.rs` | the assist fold, its panel rows, and the `--assists` timeline |
| `json.rs` | the `--json` documents |
| `fmt.rs` | token, dollar, and day formatting, display names, and `weeks_for_terminal` |
| `tests.rs`, `tests/held.rs`, `tests/held/columns.rs`, `assists/tests.rs` | the unit suites and held-frame snapshots |

## Tests

`tests.rs` and its `tests/held.rs` child cover rendered strings. Held snapshots cover all four tabs at 119×76 and 119×40 plus empty history. A geometry matrix covers widths 60 to 160 and heights 10 to 90, checking line count, display width, a common fit step, and no final newline. `tests/held/columns.rs` pins whole 119-column rows (spend, provider, role, heatmap, and typical-line columns) to the reference mock. One-shot assertions cover active-day averages, trend alignment, independent coverage dates and placement, agent-time columns, clamped unassigned spend, grouped sessions, footer scope, activity, and fresh-machine copy. `assists/tests.rs` covers the fold and timeline lines, with one inline snapshot. The load paths run against temporary `RuntimePaths`: a published cache served without a walk, a publication served on its history day whatever its account day (`one_shot_serves_the_published_history_day_across_zones`), and a cold refresh publishing the rollups the sidebar reads. The held loop is driven through `key_outcome` and `HeldStats` without a terminal.

```sh
cargo xtask test 'cli::stats'
```

## See also

- [spending.md](./agents/spending.md): the walk, the incremental cache, the spending service, and the pricing behind every figure.
- [loops.md](./harness/loops.md#the-assist-log): the assist log and its writers.
- [rimzd.md](./rimzd.md): the daemon view that holds the dashboard.
- [theme.md](./theme.md): the palette and glyph resolution the panel uses.
- [Token Insight guide](../guide/insight.md): what the figures mean, for someone using RimZ.

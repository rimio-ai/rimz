# The stats panel

`rimz stats` renders account-global token and dollar history: a heatmap of daily use, totals for four windows, spend by model, provider, and profile, and the assists RimZ performed for the user. It runs in or out of a room and in or out of a project, and it touches no agent and no session. Stats itself writes only the shared state directories (`RuntimePaths::ensure_shared_dirs`), and it never opens a workspace tree. A walk that stats triggers writes what any spending walk writes: the spending publication, the shared price table (`pricing-cache.json`) when its price load refreshes, and the [effort ledger](./agents/spending.md#session-effort-ledger) maintenance.

The command has three surfaces, and most rules on this page differ between them:

| Surface | Command | Renderer |
| --- | --- | --- |
| One-shot report | `rimz stats`, or one of `--agents`, `--teams`, `--subagents` for an explicit view | `panel::render_spend`, printed once as a scrollable report |
| Held dashboard | `rimz stats --refresh` | `panel::render_panel`, repainted on the alternate screen, and the default middle-column content of the [`rimzd` view](./rimzd.md#what-the-view-contains) |
| JSON | `--json`, alone or with an explicit view, and `--assists --json` | `json.rs`, and `assists.rs` for the assist timeline |

The code is [`crates/rimz/src/cli/stats/`](../../crates/rimz/src/cli/stats/mod.rs). What every figure means for someone using RimZ is the [Token Insight guide](../guide/insight.md). Every flag and JSON field is in the [stats CLI reference](../reference/cli/stats.md).

## Where the figures come from

Stats owns no pricing and no session placement. It reads `provider-spending.json` under the shared state root (`RuntimePaths::shared_provider_spending_path`), the aggregate the spending walk publishes ([spending.md](./agents/spending.md#what-reads-the-totals)). `crates/rimz/src/cli/stats/mod.rs::Stats::from_provider` is the whole translation: the cache's `days`, `models`, and `spending.by_provider` become `by_day`, `by_model`, and `by_provider`, and the solo, team, and subagent `RoleTally` maps keep their published spend and effort together. The two first-retained times (`recorded_since_secs` for roles, `effort_since_secs` for effort), the timezone, the publication time, and the aggregation cutoff travel with them. Team totals are computed on read: `RoleView::rows` merges each team's role windows, and no second team total is stored.

Three load paths answer a run:

| Path | Runs when | What happens |
| --- | --- | --- |
| Published cache | a one-shot or JSON run finds a current-version cache aggregated today | read and render, with no walk |
| Cold walk | a one-shot or JSON run finds the cache missing, on another schema version, or aggregated on another local day | `refresh_global_spending_direct` walks with a fresh `SpendingWalker`, coalescing on `shared_spending_lock` |
| Elected service | every refresh cycle of the held dashboard | `spending::service::request` with `SpendingServiceStartup::HostEligible`, so the dashboard process can host the warm walker ([spending.md](./agents/spending.md#one-walk-per-namespace)) |

"Aggregated today" compares local days in the cache's timezone: `Stats::published_today` takes the day of `day_cutoff_secs`, the walk's aggregation time, and never `refreshed_at_ms`, which can land after midnight when a walk straddles it. The producer's TTL and timeout grace use the same day rule. After a cold walk, `load_or_refresh_stats` checks the version and day again and fails with `Spending refresh unavailable: ...` when the result still does not cover today: a non-authoritative empty discovery can preserve an old nonzero publication, and the one-shot report refuses to print it as today's. Within one day, the one-shot figures are as fresh as the last walk that a sidebar producer, a held dashboard, or a cold run published.

The cold walk's single flight is short. A process that finds the lock held polls up to 15 steps of 20 ms (`SPENDING_WAIT_STEPS` and `SPENDING_WAIT_STEP` in `crates/rimz/src/agents/spending/engine.rs`) and serves a fresh publication that lands meanwhile. When none lands, it walks locally without publishing (`compute_fleet_spending`). Only the lock holder writes the cache, so concurrent cold runs that outwait the poll each pay a full walk.

A cold walk shows the progress line `Reading session files [bar] n/total` through a delayed spinner only when the run is human-facing (no `--json`) and both stdout and stderr are terminals (`hold::should_animate_cold_stats`). Pipes and JSON get no progress output.

When the service call fails, the held dashboard falls back to any current-version publication, whatever its day, and reports the error only when none exists (`load_or_refresh_stats_via_service`).

## Windows

`Window` has four variants, in tab order: Today (the local calendar day in the publication's timezone), Week, Month, and Year (the trailing 7, 30, and 365 days). `--window` selects the window for every breakdown, the insights, and the held dashboard's first tab, and defaults to Today. Today selects `SpendTally.day`, which is independent of the sidebar's configured headline window. The heatmap and the four-window totals ignore the selection.

Two token measures appear on the panel, and the difference is a trap:

| Measure | Definition | Used for |
| --- | --- | --- |
| Display tokens | `SpendWindow::display_tokens`: input (cache writes included) plus output plus cache reads | every token total printed, and the heatmap (`DaySpend.tokens` holds the same sum) |
| Work tokens | `SpendWindow::tokens`: input (cache writes included) plus output, without cache reads | row admission, ranking, and folding significance |

A day is active when its `DaySpend.tokens` is above zero, priced or not.

## The heatmap

Each cell is one local calendar day in the published timezone, columns are weeks opening on Monday, and the rightmost column is the current week, with future days left blank. The week count comes from the terminal width (`fmt::weeks_for_terminal`: columns minus the 6-column gutter, halved, clamped to 4 through 52), so a narrow terminal shows as few as four weeks of history. `--dollars` shades by `DaySpend::usd` instead of tokens and retitles the block from `Token activity` to `Spend activity`.

Shading is relative to the visible grid (`Grid::build`, `shade`, and `level` in `panel.rs`):

1. The ceiling is the 90th-percentile value among active days in the grid (`HEAT_CEILING_PERCENTILE`), so one outlier day does not flatten the rest.
2. A day's value over the ceiling is clamped to 1, raised to the power 0.5 (`HEAT_GAMMA`), and floored at 0.15 (`HEAT_TRACE_FLOOR`).
3. `level` maps the result onto the ramp `· ░ ▒ ▓ █`. A day whose value is zero reads `·`, and any other day reads at least `░`.

Under `--dollars` the value is dollars, so a token-active day with no priced usage reads `·` although it counts as active everywhere else.

The month row groups the columns by the month of each column's Monday and labels a group only when it spans at least two columns. A month that begins midweek is therefore labelled one column late, and a one-column fragment at either edge goes unlabelled. The `Less · ░ ▒ ▓ █ More` key closes the block.

## The breakdowns

Five sections break the selected window down: Models, Providers, Agents (solo profiles), Teams, and Subagents (delegated profiles). Providers carries all provider spend. The three profile sections are subsets of it, not a partition, and role semantics and native-child attribution belong to [spending.md](./agents/spending.md#session-role-provenance). Every share on every surface divides by the selected window's fleet dollars, and a share against zero dollars is zero.

### Which rows enter

| Surface | A model or provider row enters with | A profile or team row enters with |
| --- | --- | --- |
| One-shot report | dollars | dollars or recorded effort |
| Held dashboard and JSON | dollars or work tokens | dollars, work tokens, or recorded effort |

Effort is any active-time or message counter, sender-only credit included. A section with no admitted rows is omitted, and unpriced tokens still count toward the window totals and the heatmap.

The Agents section hides on the one-shot report and the held dashboard unless it would list a profile that is not named for a provider. `panel::show_agents` is the one predicate, called with the asking surface's admission rule: it shows Agents when an admitted selected-window profile has a key absent from `by_provider`. Once Agents shows, its provider-named rows stay in it. Teams and Subagents hide only when empty. Explicit views and JSON never apply the predicate.

### The one-shot report

The one-shot report opens with a Spend row of dollars and display tokens for all four windows. Its Today column reads the `by_day` bucket for today, while Week, Month, and Year read the `SpendTally` windows. The breakdowns follow, then the recorded-since notes, the heatmap, and `as of HH:MM` (with the date when the publication is from another day). An explicit view prints only its own section, with the notes, and no Spend row or heatmap.

Each section ranks by dollars, then name, keeps three rows, and merges the rest into a counted tail named for the section (`2 other models`). Explicit views are unfolded. Column choice follows the typed `SpendSection`, never its display label:

| Section | Columns after SPEND and SHARE |
| --- | --- |
| Models | INPUT, OUTPUT, CACHED, HIT |
| Providers | SESSIONS, TOKENS, HIT, TOK/S |
| Agents, Teams, Subagents (compact) | SESSIONS (RUNS for Subagents), ACTIVE, MESSAGES for Teams, TOKENS, HIT, TOK/S |
| Explicit view | INPUT, OUTPUT, CACHED, HIT, SESSIONS or RUNS, ACTIVE, MESSAGES for Teams |

ACTIVE formats seconds with `theme::fmt::active_duration`, the formatter attribution uses. MESSAGES is incoming human prompts plus received agent messages, with the human count in parentheses when nonzero (`12 (3 you)`). Outgoing messages stay in role-level JSON and do not inflate it. A zero effort cell reads `-`. No surface unfolds a team into its roles except JSON.

### Held dashboard rows

The held dashboard has no Spend row. Its windows row is a tab bar of display tokens for all four windows, with the selected tab highlighted, and the five sections follow, one line per row, each with a dollar share and an optional share bar. It has no ACTIVE or MESSAGES columns. Provider names come from provider descriptors and model names from `model_display::display_model`, both painted in provider identity colours, a model taking the colour of its published producing kind. `Other`, the empty model id, and every profile and team name and bullet are muted.

Folding is measured against each section's own admitted rows, not fleet totals. `panel::fold_breakdown` keeps a row named while it holds at least 1% (`MIN_BREAKDOWN_SHARE`) of the section's dollars or of its work tokens, and it merges the rest into a muted `Other` row last, with its share of fleet dollars computed like any row's. The empty model id always folds. When the row cap requires more folding, the least significant named rows fold first, ranked by the greater of their two shares, and ties fold the later display row first. Display order is dollars, then work tokens, then key. `panel::natural_size` runs the same fold with no cap, and its count is the section's weight in [`fit`](#fitting-the-terminal).

The HIT column is `SpendWindow::cache_hit_percent`: `cache_read / (cache_read + input)`, where `input` already includes cache writes, rounded half up, and absent for a zero denominator. `CacheHealth::classify` colours it green at 90% and above, yellow from 70% to 89%, and red below.

The rate column is `SpendWindow::tokens_per_sec`: `timed_output * 1000 / timed_ms` for the selected window, rounded half up. The held dashboard prints it on Models and Providers rows, and omits it from profile sections. An untimed row shows `-`, and a section whose every row is untimed drops the column. `SpendWindow::merge` sums both counters, so `Other` carries the ratio of sums. Timing boundaries and filters belong to [spending.md](./agents/spending.md#response-timing).

All five held sections share one column layout (`panel::stat_section_layout`), chosen by what fits the panel width:

1. The full left column, the percentage, and a share bar of at least 10 cells.
2. The full left column and the percentage, with no bar.
3. The compact left column: models keep only dollars, and provider and profile rows drop the HIT and rate columns.

Glyphs come from `panel::resolve_panel_glyphs`, which resolves the token, session, and meter-bar glyph roles through the machine theme. A Nerd Font glyph set swaps `◎ ◇ ↘ ↗ ◌` and the bar characters, while colours stay on the CLI palette ([theme.md](./theme.md#glyphs)). Every dollar figure on every text surface uses `fmt::fmt_usd`: whole dollars with thousands grouped from $10 up, and cents below (`$0.12`), so a small fleet never reads `$0`.

### Recorded-since notes

A profile section can cover a window that starts before RimZ recorded roles or effort, and a muted note says so. Each of the two notes shows independently when the selected window starts before its first-retained time: `roles recorded since <date>; earlier spend shows under Providers only` for `recorded_since_secs`, and `active time and messages recorded since <date>` for `effort_since_secs`. The window start is `Window::assist_since` at publication time: local midnight for Today, and publication time minus 7, 30, or 365 days for the others.

The notes print below the last profile section, and only when a profile section is visible. An explicit view prints them even with no rows, after a `No <noun> spend recorded (<window>).` line. The held dashboard's empty-history frame shows neither. Effort-only history still renders the profile sections when there are no spend-day buckets.

## Insights

The held dashboard closes with two columns of activity figures for the selected window (`panel::insights_lines`, `panel::Activity::of`):

| Figure | Today and Year | Week and Month |
| --- | --- | --- |
| Sessions, Cost/session, Spend | the selected window | the selected window |
| Active days | active days of the last 28 (`RECENT_ACTIVITY_DAYS`) | active days in the window |
| Most active day, Longest streak, Current streak | the whole cached history | days in the window |
| Daily avg | trailing-year dollars over the trailing year's active days | window dollars over the window's active days |
| Spend trend | none | the change against the preceding equal span, when that span has dollars |

The current streak counts back from today, or from yesterday when today has no activity yet, so a quiet morning does not break a streak.

## Fitting the terminal

The panel is plain strings. `render_panel` builds a `Vec<String>` and `emit` writes it with one shared left pad. crossterm appears only at the held loop's terminal edge (`tui::TerminalModeGuard`, `tui::replace_frame`, and key events), and the sidebar pane's ratatui widget stack is not on this path, so tests assert on rendered text.

`PanelGeometry::current` reads the terminal size on every render. The week count fixes the panel width (`6 + 2 × weeks`), and the panel centres in the terminal. Rows are read only when stdout is a terminal, so a pipe carries `None` and always gets the full panel.

`fit` spends the row budget (terminal rows minus one) after the fixed rows, the section chrome, and the assist rows, and it keeps data ahead of chrome:

1. With room for everything, each breakdown gets its natural row count (named rows plus `Other` when present), capped at 6.
2. With less room, the breakdowns shrink toward 3 rows each (or their natural count, if smaller), and `allocate_breakdown_rows` splits the space in proportion to each section's natural size. The 9-row wordmark header stays.
3. When even those floors do not fit beside the header, the header drops, each section's cap becomes its 3-row floor, and each can shrink to 1 row.
4. When the 1-row sections still do not fit, Subagents drops, then Agents, then Teams. Models and Providers keep their rows.

The recorded-since notes reserve enough rows to wrap at the panel width while any profile section remains, and drop with the last one. Only the held dashboard fits: the one-shot report scrolls and has no row budget.

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

A history with no usage and no profile activity paints the wordmark, `No token usage recorded yet - run an agent and check back.`, and any assist categories. A resize repaints the current frame, and the first successful refresh replaces the unavailable frame in place. Every repaint rereads the assist log for the selected window, and the frames are the same under `--hold`.

| Key | Outcome |
| --- | --- |
| `Tab` / `Shift-Tab` | cycle the window and repaint from the stats in hand |
| `r` / `R` | reload the binary in place |
| `q`, `Ctrl-C` | quit, unless `--hold` is set |

Reload re-execs `reload::current_reexec_target()` with the original arguments. The `r` key returns the reload outcome directly. `SIGUSR1` sets a flag that the cycle reads and clears, which is how `rimz reload` restarts running dashboards, and registering that handler replaces `SIGUSR1`'s default terminate disposition. When no re-exec target resolves, the dashboard keeps running.

`--hold` is a hidden flag that requires `--refresh` and exists for the daemon view: `q` and `Ctrl-C` become no-ops, and closing the pane still ends the process. `daemon_content::stats_argv` is exactly `rimz stats --refresh --hold`. How `[daemon]` panes replace it, and how a pane-count change reaches a running room, are in [rimzd.md](./rimzd.md#the-content-supervisor).

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

The rollup counts only assists that took effect, and the held dashboard prints its nonzero categories. In the held dashboard and in `--json`, the records start where the selected window starts (`Window::assist_since` at the time of the read). `AssistEvent::took_effect` is the one predicate for the rollup and the default timeline:

| Assist | Takes effect when |
| --- | --- |
| stall notice | it has no error, since an undelivered notice waits in the parent's queue |
| auto-continue, cache keepalive, idle compaction, flip compaction | it was delivered |
| idle stop | the agent's pane closed, even with a cleanup error |
| hibernation | it froze, or a deepening step reclaimed bytes |
| launch retry | the relaunch spawned |
| auto-redeem | a window reset, since a stamp write that fails afterwards does not undo it |
| GC and temp eviction | it has no top-level error, so a partial sweep counts |
| molt, idle-stop withdrawal, judge decline, workflow transitions, resident launch, model alias, tier fallback, compact command, room resume | always: each counts as recorded |

An attempt that did not take effect increments `held` under the count it would have joined (`AssistEvent::count_key`), and the panel prints it beside that count: `Auto-compact: N (M held)`. Four projections are easy to get wrong:

- A temp eviction increments `tmp_evictions`, never `sweeps`, and the `Temp evictions:` row is separate from `Auto-gc:`. Its bytes join `reclaimed_bytes`, so the `Auto-gc:` byte detail is the combined GC and temp-eviction total.
- Only a freeze increments `hibernations`. A deepening step takes effect without one, and `hibernate_reclaimed_bytes` sums the per-step deltas of both, kept apart from GC bytes. Hibernate hold reasons enter `hibernate_holds` whether or not the record took effect.
- `redeems` includes manual redemptions, which also count in `manual_redeems`. The `Auto-redeem:` row subtracts them, and no row prints manual redemptions, so their held attempts (`held.manual_redeems`) reach JSON only.
- The five workflow counts print as one `Workflow:` row, and the molts print as `Molts:`.

The durable records, their fields, and rotation belong to the [assist log](./harness/loops.md#the-assist-log).

## Where the code lives

| File | Owns |
| --- | --- |
| `mod.rs` | `StatsArgs`, `Window`, `RoleView`, `Stats`, the three load paths, the shared constants, and the wordmark |
| `panel.rs` | geometry, `fit`, the heatmap, the windows row, all five breakdowns, the insights, the unavailable and empty frames, and `emit` |
| `hold.rs` | the `--refresh` loop, key handling, the reload signal and re-exec, and the cold-walk spinner |
| `assists.rs` | the assist fold, its panel rows, and the `--assists` timeline |
| `json.rs` | the `--json` documents |
| `fmt.rs` | token, dollar, and day formatting, display names, and `weeks_for_terminal` |
| `tests.rs`, `assists/tests.rs` | the unit suites |

## Tests

`tests.rs` holds unit tests over rendered strings, with no `.snap` files. `assists/tests.rs` covers the fold and the timeline lines, with one inline snapshot. The load paths run against temporary `RuntimePaths`: a published cache served without a walk, and a cold refresh publishing the rollups the sidebar reads. Panel tests strip ANSI with `strip_ansi` and assert layout, ranking, folding, and the `fit` ladder. The held loop is driven through `key_outcome` and `HeldStats` without a terminal.

```sh
cargo xtask test 'cli::stats'
```

## See also

- [spending.md](./agents/spending.md): the walk, the incremental cache, the spending service, and the pricing behind every figure.
- [loops.md](./harness/loops.md#the-assist-log): the assist log and its writers.
- [rimzd.md](./rimzd.md): the daemon view that holds the dashboard.
- [theme.md](./theme.md): the palette and glyph resolution the panel uses.
- [Token Insight guide](../guide/insight.md): what the figures mean, for someone using RimZ.

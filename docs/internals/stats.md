# The stats panel

`rimz stats` renders account-global token and dollar history: a heatmap of daily use, a windows row of totals, the spend by model and by agent, activity insights, and the assists RimZ performed for the user. It touches no agent and writes nothing to any session, and it runs in or out of a room and in or out of a project. Its only writes are the shared state directories and, on a cold run, the spending cache it computes ([where the figures come from](#where-the-figures-come-from)).

The command has two homes. A one-shot `rimz stats` prints the spend-first report and returns to the shell. `rimz stats --refresh` holds the compact dashboard open as a live screen, and that held pane is the default middle-column content of the `rimzd` view ([rimzd.md](./rimzd.md#what-the-view-contains)).

The code is [`crates/rimz/src/cli/stats/`](../../crates/rimz/src/cli/stats/mod.rs). The user-facing counterparts are the [Token Insight guide](../guide/insight.md), which explains what every figure means, and the [stats CLI reference](../reference/cli/stats.md).

## What it renders

The one-shot panel leads with dollars and tokens for Today, Week, Month, and Year, then trailing-year Models, Agents and Subagents, an unfolded Unattributed line, the local-day heatmap, and publication time. The wordmark belongs only to the held view. Empty sections and zero-dollar rows are omitted; unpriced tokens still contribute to window totals and the heatmap.

`panel::render_spend` owns one-shot and unfolded subagent text. `panel::render_panel` dispatches to it when no active held tab is supplied. Held frames retain their terminal fitting, wordmark, activity insights and assist counts.

## Where the figures come from

Stats owns no spend arithmetic. It reads `provider-spending.json` under the shared state root (`RuntimePaths::shared_provider_spending_path`), the aggregate the spending walk publishes with per-day buckets, per-model and per-provider tallies, and the trailing windows ([spending.md](./agents/spending.md#what-reads-the-totals)). `Stats::from_provider` is the whole translation: the cache's `days`, `models`, and `roles.agents` become `by_day`, `by_model`, and `by_agent`; subagent tallies, unattributed spend, timezone and publication time travel with them.

Three load paths in `mod.rs` answer a run:

| Path | Runs when | What happens |
| --- | --- | --- |
| Published cache | a one-shot run finds a cache whose `is_current_version` holds | read and render, with no walk and no freshness check |
| Cold walk | a one-shot run finds the cache missing or on another schema version | `refresh_global_spending_direct` walks with a fresh `SpendingWalker`, coalescing on `shared_spending_lock` |
| Elected service | every refresh of the held `--refresh` dashboard | `spending::service::request` with `SpendingServiceStartup::HostEligible`, so the dashboard can host the warm walker ([spending.md](./agents/spending.md#one-walk-per-namespace)) |

A one-shot run never refreshes a current-version cache, however old. Its figures are as fresh as the last walk a sidebar producer, a held dashboard, or a cold run published.

The cold walk's single flight is short. A process that finds the lock held waits up to 15 steps of 20 ms (`SPENDING_WAIT_STEPS`, `SPENDING_WAIT_STEP` in `agents/spending/engine.rs`) for a fresh publication, then walks locally without publishing. Only the lock holder writes the cache, so concurrent cold runs each pay a full walk.

The cold walk shows a progress spinner (`Reading session files [bar] n/total`) only when the run is human-facing and both stdout and stderr are TTYs (`should_animate_cold_stats`); `--json` and pipes get no progress chrome. The spinner does not print a wordmark.

If the service call fails, the held dashboard falls back to any current-version published cache and reports an error only when none exists. Setup on every path creates the shared directories only (`ensure_shared_runtime`); stats never opens a workspace tree.

## Windows

`Window` has three variants: Week, Month and Year (trailing 7/30/365 days). The held view starts on Year; Tab and Shift-Tab cycle the tabs. The heatmap ignores selection. One-shot and JSON breakdowns use Year, while Today reads the local-day bucket in the publication's timezone.

Year insights preserve the recent 28-day activity count under the explicit `RECENT_ACTIVITY_DAYS` constant; their streaks use the cached history. Week and Month scope activity and compare spend with the preceding equal span. Year has no spend trend. The held assist counts use the selected trailing window; the standalone assist timeline reads both retained log generations without a date cutoff.

Every token total on the panel is `SpendWindow::display_tokens`, which adds cache-read tokens to input plus output. A day counts as active when its bucket has tokens, priced or not.

## The heatmap

Each cell is one local calendar day in the published timezone, columns are weeks opening on Monday, and the rightmost column is the current week with future days left blank. The week count comes from the terminal width (`weeks_for_terminal`: columns minus the 6-column gutter, halved, clamped to 4 through 52), so a narrow terminal shows as few as four weeks of history. `--dollars` shades by `DaySpend::usd` instead of tokens and retitles the block `Spend activity`.

Shading is relative to the visible grid (`Grid::build`, `shade`, `level` in `panel.rs`):

1. The ceiling is the 90th-percentile value among active days in the grid (`HEAT_CEILING_PERCENTILE`), so one outlier day does not flatten the rest.
2. A day's value over the ceiling is clamped to 1, raised to the power 0.5 (`HEAT_GAMMA`), and floored at 0.15 (`HEAT_TRACE_FLOOR`).
3. `level` maps the result onto the ramp `· ░ ▒ ▓ █`: `·` only for a day with no usage, and at least `░` for any active day.

The month row labels the column where each month begins, and the `Less · ░ ▒ ▓ █ More` key closes the block.

## The breakdowns

One-shot sections rank non-zero spend by dollars. Each keeps three rows plus a counted tail (`N other models`, `N other agents`, `N other subagents`); `--subagents` and JSON are unfolded. All shares divide by the window's fleet dollars, unattributed included. Unattributed retains its own total and share, followed by every non-zero provider kind and its dollars, ranked by dollars with provider identity colours. Both JSON views expose `unattributed: {tally, by_provider}`, using trailing-year windows and provider-kind keys, omitting zero-dollar providers. Role semantics and native-child attribution belong to [spending](./agents/spending.md).

The held view keeps its compact rows and geometry budget, with dollar shares for both models and agents. Agent names come from provider descriptors; model names use `model_display::display_model`. Token units are uppercase.

The cache-hit column is `SpendWindow::cache_hit_percent`: `cache_read / (cache_read + input)`, where `input` already includes cache writes, rounded half up and absent for a zero denominator. `CacheHealth::classify` colors it green at 90% and above, yellow from 70% to 89%, and red below.

The final Models column uses `SpendWindow::tokens_per_sec`: rounded `timed_output * 1000 / timed_ms` for the selected window. An untimed row shows `-`; when every row is untimed the column disappears. `SpendWindow::merge` sums both counters, so `Other` keeps the ratio of sums. The number uses the row's normal foreground and `tok/s` the muted unit tone. Timing boundaries and filters belong to [spending](./agents/spending.md#response-timing).

Both sections share one column layout (`stat_section_layout`), chosen by what fits the panel width:

1. The full left column, the percentage, and a share bar of at least 10 cells.
2. The full left column and the percentage, with no bar.
3. The compact left column: models keep only dollars, agents drop the cache-hit column.

## Fitting the terminal

The panel is plain strings. `render_panel` builds a `Vec<String>` and `emit` writes it with one shared left pad; crossterm enters only for key events in the held loop, and the sidebar pane's ratatui stack is not on this path. Tests therefore assert on rendered text.

`PanelGeometry::current` reads the terminal once per render. The week count fixes the panel width (`6 + 2 × weeks`), and the panel centres in the terminal. Rows are read only when stdout is a TTY, so a pipe carries `None` and always gets the full panel.

`fit` spends the row budget (terminal rows minus one) after the fixed rows, the section chrome, and the assist rows, and it keeps data ahead of chrome:

1. With room for everything, each breakdown gets its natural row count, capped at 6.
2. With less room, the breakdowns shrink toward 3 rows each (or their natural count, if smaller), and `allocate_breakdown_rows` splits the space in proportion to each section's natural size. The 9-row wordmark header stays.
3. When even those floors do not fit beside the header, the header drops, each section's cap becomes its 3-row floor, and each can shrink to 1 row.

One-shot text is a scrollable report and does not use the held screen's row budget.

Glyphs come from `resolve_panel_glyphs`, which resolves the token, session, and meter-bar glyph roles through the machine theme, so a Nerd Font glyph set swaps `◎ ◇ ↘ ↗ ◌` and the bar characters while colors stay on the CLI palette ([theme.md](./theme.md#glyphs)).

## The held dashboard

`--refresh` runs `hold::run_refresh`. `TerminalModeGuard` puts the terminal in raw mode on the alternate screen with mouse capture off, so the dashboard owns the pane without adding mux scrollback, and keypresses and stray mouse reports arrive as events the loop drains.

Each cycle spawns a worker thread that loads through the elected service and sends back one `Result<Stats>`, with a panic caught and turned into an error. The foreground polls for events every 100 ms (`REFRESH_POLL_TICK`) and starts the next cycle once the refresh has landed and the 60-second deadline (`REFRESH_INTERVAL`) has passed. Until the first successful frame, a landed result moves the deadline to 5 seconds out (`EMPTY_REFRESH_RETRY`).

A failed refresh never exits. The first failure in a streak logs a warning and later ones log at debug until a refresh succeeds. `main.rs` turns stderr logging off for `stats --refresh`, so warnings cannot smear the raw-mode frame; the reporting layer still receives them.

Each repaint picks one of three frames:

| State | Frame |
| --- | --- |
| a stats frame exists | the panel, even while later refreshes fail; no staleness marker |
| no frame yet, a refresh failed | the wordmark and a centred `Spending refresh unavailable - retrying. <cause>`, ellipsized to the panel width |
| no outcome yet | nothing is written |

A resize repaints the current frame, and the first successful refresh replaces the unavailable frame in place. The frames are identical under `--hold`.

| Key | Outcome |
| --- | --- |
| `Tab` / `Shift-Tab` | cycle the window and repaint from the stats in hand, reloading only the assist log |
| `r` / `R` | reload the binary in place |
| `q`, `Ctrl-C` | quit, unless `--hold` is set |

Reload re-execs `reload::current_reexec_target()` with the original arguments. The `r` key returns the reload outcome directly; `SIGUSR1` sets a flag the cycle reads and clears, which is how `rimz reload` restarts running dashboards. Registering that handler replaces `SIGUSR1`'s default terminate disposition. When no re-exec target resolves, the dashboard keeps running.

`--hold` is a hidden flag that requires `--refresh` and exists for the daemon view: `q` and `Ctrl-C` become no-ops, and closing the pane still ends the process. `daemon_content::stats_argv` is exactly `rimz stats --refresh --hold`; how `[daemon]` panes replace it and how a pane-count change reaches a running room are in [rimzd.md](./rimzd.md#the-content-supervisor).

## Machine-readable surfaces

`--json` emits publication `as_of` (RFC 3339 with offset), `timezone`, `unit`, session and streak fields, `windows.today/week/month/year`, unfolded `models`, top-level `agents`, profile `subagents`, `unattributed`, local-date `days`, and the assist rollup only. `windows.week.usd` remains stable. This is a breaking change: agent membership and share semantics change, and assist events move to `--assists --json`. `--subagents --json` emits `{as_of, window: "year", subagents, unattributed}`.

`--assists` filters through `AssistEvent::took_effect`, then limits the newest-first timeline to 50 by default. `-n 0` selects all retained; `--all` includes ineffective attempts. JSON uses the same selection and emits `{as_of, rollup, events, older}`. The rollup covers all retained records, not just the displayed page.

The same predicate gates rollup counts: no error for stall notices, since an undelivered notice waits in the parent's queue; delivered for continues, keepalives, idle and flip compactions; stopped for idle stops; frozen or a deepening step for hibernation; relaunched for launch retries; a window reset for redemption, since a stamp write that fails after it does not undo it; no error for GC. Judge declines, resident launches, model aliases, tier fallbacks, compact commands and room resumes count as recorded. A successful stop may carry a cleanup error and still counts. A partial GC with no top-level error counts, as its record represents a completed sweep.

Every attempt that did not take effect increments `held` under the count it would have joined (`AssistEvent::count_key`), printed beside that count as `Auto-compact: N (M held)`. Hibernate hold reasons remain diagnostic rollup information regardless of effect. Only freeze records count as hibernations; a deepening step (not frozen, with reclaimed bytes) takes effect without adding one, and memory bytes sum the per-step deltas from both. Hibernation reclaimed bytes remain separate from GC reclaimed bytes. The [assist log](./harness/loops.md#the-assist-log) owns the durable records and rotation.

## Where the code lives

| File | Owns |
| --- | --- |
| `mod.rs` | `StatsArgs`, `Window`, `Stats`, the three load paths, the shared constants, and the wordmark |
| `panel.rs` | geometry, `fit`, the heatmap, the windows row, both breakdowns, the insights, the unavailable and empty frames, and `emit` |
| `hold.rs` | the `--refresh` loop, key handling, the reload signal and re-exec, and the cold-walk spinner |
| `assists.rs` | the assist fold, its panel rows, and the `--assists` timeline |
| `json.rs` | the `--json` document |
| `fmt.rs` | token, dollar, and day formatting, display names, and `weeks_for_terminal` |
| `tests.rs` | the unit suite |

## Tests

`tests.rs` holds pure unit tests over rendered strings, with no golden `.snap` frames. The load paths run against temporary `RuntimePaths` (a published cache served without a walk, and a cold refresh publishing the rollups the sidebar reads). Panel tests strip ANSI with `strip_ansi` and assert layout, ranking, folding, and the `fit` ladder. The held loop is driven through `key_outcome` and `HeldStats` without a terminal.

```sh
cargo xtask test 'cli::stats'
```

## See also

- [spending.md](./agents/spending.md): the walk, the incremental cache, the spending service, and the pricing behind every figure.
- [loops.md](./harness/loops.md#the-assist-log): the assist log and its rollup.
- [rimzd.md](./rimzd.md): the daemon view that holds the dashboard.
- [theme.md](./theme.md): the palette and glyph resolution the panel uses.
- [Token Insight guide](../guide/insight.md): what the figures mean, for someone using RimZ.

# The stats panel

`rimz stats` renders account-global token and dollar history: a heatmap of daily use, a windows row of totals, the spend by model and by agent, activity insights, and the assists RimZ performed for the user. It touches no agent and writes nothing to any session, and it runs in or out of a room and in or out of a project. Its only writes are the shared state directories and, on a cold run, the spending cache it computes ([where the figures come from](#where-the-figures-come-from)).

The command has two homes. A one-shot `rimz stats` prints the panel and returns to the shell, so it pipes and scrolls like any report. `rimz stats --refresh` holds the same panel open as a live screen, and that held pane is the default middle-column content of the `rimzd` view ([rimzd.md](./rimzd.md#what-the-view-contains)).

The code is [`crates/rimz/src/cli/stats/`](../../crates/rimz/src/cli/stats/mod.rs). The user-facing counterparts are the [Token Insight guide](../guide/insight.md), which explains what every figure means, and the [stats CLI reference](../reference/cli/stats.md).

## What it renders

A one-shot run on a machine with recorded history, default glyph set:

```
                        ██████╗ ██╗███╗   ███╗  ███████╗
                        ██╔══██╗██║████╗ ████║  ╚══███╔╝
                        ██████╔╝██║██╔████╔██║    ███╔╝
                        ██╔══██╗██║██║╚██╔╝██║   ███╔╝
                        ██║  ██║██║██║ ╚═╝ ██║  ███████╗
                        ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝  ╚══════╝
                    The control room for your coding agents

  Token activity

      Nov   Dec       Jan     Feb     Mar       Apr     May     Jun       Jul
  Mon ▒ ░ · ░ ░ ░ ░ · · · · · · · · · · · · · ░ ░ · · ▒ ▓ ▒ ░ ▒ ▒ ▓ ▓ ▓ ▒ █ █ ▓
      ░ ░ ░ ▒ ░ ▒ · · ░ · · · · · · · · · · ░ ░ · ▒ ▒ ▓ ▒ ▒ ▒ ▒ ░ ▓ ░ ▓ ▓ █ █
  Wed ░ ░ ░ ░ ░ ░ · · · · · · · · · · · · · ░ ░ ▒ ▓ ░ · ▒ ░ ░ ▒ ▒ ▓ ░ █ █ ▓ █
      · ░ ░ ▒ ░ · · · · · · · ░ · · · · · · ░ ░ ▒ ▓ ▒ ▓ ▒ · ▒ ▒ ▒ █ ▒ █ █ █ █
  Fri ░ · · ▒ ░ · · ░ · · · · · · · · · · · ░ ▒ ▒ ▓ ▒ ▒ ▒ · ▒ · ░ █ ▓ ▓ █ █ █
      · · · ░ · · · · · · · · · · · · · · ▒ · ▒ ░ ▒ ▒ ▒ ░ ░ ▒ · ▓ ▓ ▓ █ █ ▓ █
      · · · ▒ · · · · · · · · · · · · · · ░ ░ ▒ · · ▒ ▒ ▓ ░ ▒ ▒ ▓ ▒ █ █ ▓ ▓ █

  Less · ░ ▒ ▓ █ More

  All time 37.6B  ·  Week 13.2B  ·  Month 27.5B  ·  Year 37.6B

  Models
  ● GPT 5.5     $11,582 · ↘ 629.9m · ↗ 51.0m · ◌ 13.8b · 96%   36.2% ━━━━━━───────────
  ● GPT 5.6 Sol  $9,266 · ↘ 347.3m · ↗ 34.0m · ◌ 12.9b · 97%   29.0% ━━━━━────────────
  ● Opus 4.8     $6,411 · ↘ 210.6m · ↗ 66.9m · ◌  5.4b · 96%   20.0% ━━━──────────────
  ● Fable 5      $3,681 · ↘  70.1m · ↗ 17.6m · ◌  1.4b · 95%   11.5% ━━───────────────
  ● GPT 5.4        $649 · ↘  84.4m · ↗  6.9m · ◌  1.3b · 94%    2.0% ─────────────────
  ● Other          $397 · ↘  79.2m · ↗ 11.8m · ◌  1.1b · 93%    1.2% ─────────────────

  Agents
  ● Codex       $21,808 · ◎ 4003 · ◇ 30.3B · 97%         77.6% ━━━━━━━━━━━━━────
  ● Claude      $10,149 · ◎ 1025 · ◇  7.3B · 96%         19.9% ━━━──────────────
  ● Other           $30 · ◎  131 · ◇   36M · 80%          2.5% ─────────────────

  Sessions: 5,159              Spend: $31,986.33
  Active days: 28/28           Longest streak: 51 days
  Most active day: Jul 18      Current streak: 51 days
  Cost/session: $6.20          Daily avg: $191.53
```

From the top: the wordmark, the heatmap, the windows row, the Models and Agents breakdowns, and the insight rows. An `Assists` block follows the insights when the selected window holds a counted assist; this capture has none. With no recorded days, the panel prints one muted line (`No token usage recorded yet - run an agent and check back.`) in place of everything below the wordmark, plus the assists block when it has rows.

## Where the figures come from

Stats owns no spend arithmetic. It reads `provider-spending.json` under the shared state root (`RuntimePaths::shared_provider_spending_path`), the aggregate the spending walk publishes with per-day buckets, per-model and per-provider tallies, and the trailing windows ([spending.md](./agents/spending.md#what-reads-the-totals)). `Stats::from_provider` is the whole translation: the cache's `days`, `models`, and `by_provider` become `by_day`, `by_model`, and `by_agent`.

Three load paths in `mod.rs` answer a run:

| Path | Runs when | What happens |
| --- | --- | --- |
| Published cache | a one-shot run finds a cache whose `is_current_version` holds | read and render, with no walk and no freshness check |
| Cold walk | a one-shot run finds the cache missing or on another schema version | `refresh_global_spending_direct` walks with a fresh `SpendingWalker`, coalescing on `shared_spending_lock` |
| Elected service | every refresh of the held `--refresh` dashboard | `spending::service::request` with `SpendingServiceStartup::HostEligible`, so the dashboard can host the warm walker ([spending.md](./agents/spending.md#one-walk-per-namespace)) |

A one-shot run never refreshes a current-version cache, however old. Its figures are as fresh as the last walk a sidebar producer, a held dashboard, or a cold run published.

The cold walk's single flight is short. A process that finds the lock held waits up to 15 steps of 20 ms (`SPENDING_WAIT_STEPS`, `SPENDING_WAIT_STEP` in `agents/spending/engine.rs`) for a fresh publication, then walks locally without publishing. Only the lock holder writes the cache, so concurrent cold runs each pay a full walk.

The cold walk shows a progress spinner (`Reading session files [bar] n/total`) only when the run is human-facing and both stdout and stderr are TTYs (`should_animate_cold_stats`); `--json` and pipes get no progress chrome. When the spinner runs, the wordmark prints before the walk and the panel after it skips its own.

If the service call fails, the held dashboard falls back to any current-version published cache and reports an error only when none exists. Setup on every path creates the shared directories only (`ensure_shared_runtime`); stats never opens a workspace tree.

## Windows

`Window` has four variants, `AllTime`, `Week`, `Month`, and `Year`, and the selected one scopes everything below the windows row. The heatmap ignores it.

A one-shot run and `--json` always use `AllTime`: `render_panel` receives `active: None` and falls back to it. Only the held dashboard selects another window, with `Tab` and `Shift-Tab`, and its windows row renders as a tab bar with the active cell highlighted.

`Window::select` maps `AllTime` and `Year` onto the same trailing-365-day tally, because the cache carries no longer span. "All time" is a label, and its figures equal Year's. The insight and assist rules still differ between the two:

| Figure | All time | Week, Month, Year |
| --- | --- | --- |
| Models, Agents, Sessions, Spend, Cost/session | the 365-day tally | the 7-, 30-, or 365-day tally |
| Active days | active days in the trailing 28, shown as `N/28` | active days in the trailing 7, 30, or 365 |
| Most active day, streaks | every day in the cache | days inside the window |
| Daily avg | spend over active days in the trailing 365 | spend over active days in the window |
| Spend trend | none | Week and Month append `(↑n% vs prior week)` against the preceding equal span when that span had spend; Year shows none |
| Assists | every record | records from the last 7, 30, or 365 days (`assist_since`) |

Every token total on the panel is `SpendWindow::display_tokens`, which adds cache-read tokens to input plus output. A day counts as active when its bucket has tokens, priced or not.

## The heatmap

Each cell is one UTC day, columns are weeks opening on Monday, and the rightmost column is the current week with future days left blank. The week count comes from the terminal width (`weeks_for_terminal`: columns minus the 6-column gutter, halved, clamped to 4 through 52), so a narrow terminal shows as few as four weeks of history. `--dollars` shades by `DaySpend::usd` instead of tokens and retitles the block `Spend activity`.

Shading is relative to the visible grid (`Grid::build`, `shade`, `level` in `panel.rs`):

1. The ceiling is the 90th-percentile value among active days in the grid (`HEAT_CEILING_PERCENTILE`), so one outlier day does not flatten the rest.
2. A day's value over the ceiling is clamped to 1, raised to the power 0.5 (`HEAT_GAMMA`), and floored at 0.15 (`HEAT_TRACE_FLOOR`).
3. `level` maps the result onto the ramp `· ░ ▒ ▓ █`: `·` only for a day with no usage, and at least `░` for any active day.

The month row labels the column where each month begins, and the `Less · ░ ▒ ▓ █ More` key closes the block.

## The breakdowns

The Models and Agents sections share one row shape: a bullet, the name, a left column of figures, the share percentage, and a share bar. They differ in what ranks and divides:

| | Models | Agents |
| --- | --- | --- |
| Left column | dollars, `↘` input, `↗` output, `◌` cache read, cache hit | dollars, `◎` sessions, `◇` display tokens, cache hit |
| Sort | dollars, then tokens | sessions, then tokens |
| Share | the row's dollars over the window's model dollars | the row's sessions over all sessions in the window |
| Folds into `Other` | an empty model id, or under 1.0% of the window's model dollars | under 1.0% of the window's sessions |
| Bullet color | cool | the provider's identity color; `Other` muted |

Entries with zero tokens in the window are dropped. The 1.0% fold (`MIN_BREAKDOWN_SHARE`) applies first and does nothing when its denominator is zero. The row cap (`MAX_MODELS`, `MAX_AGENTS`, 6 each, or fewer when the terminal is short) then folds the tail into the same final `Other` row. Model names go through `model_display::display_model`, agent names through the adapter's display name.

The cache-hit column is `SpendWindow::cache_hit_percent`: `cache_read / (cache_read + input)`, where `input` already includes cache writes, rounded half up and absent for a zero denominator. `CacheHealth::classify` colors it green at 90% and above, yellow from 70% to 89%, and red below.

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

A run whose wordmark the cold-walk spinner already printed skips `fit` and renders every capped row.

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
| `Ctrl-C` | quit, unless `--hold` is set |

Reload re-execs `reload::current_reexec_target()` with the original arguments. The `r` key returns the reload outcome directly; `SIGUSR1` sets a flag the cycle reads and clears, which is how `rimz reload` restarts running dashboards. Registering that handler replaces `SIGUSR1`'s default terminate disposition. When no re-exec target resolves, the dashboard keeps running.

`--hold` is a hidden flag that requires `--refresh` and exists for the daemon view: `Ctrl-C` becomes a no-op, and closing the pane still ends the process. `daemon_content::stats_argv` is exactly `rimz stats --refresh --hold`; how `[daemon]` panes replace it and how a pane-count change reaches a running room are in [rimzd.md](./rimzd.md#the-content-supervisor).

## Machine-readable surfaces

`--json` (`json.rs`) emits the stats document instead of the panel. It always describes All time and conflicts with `--refresh`; `--dollars` only sets `unit` to `usd`.

| Field | Contents |
| --- | --- |
| `unit` | `tokens`, or `usd` under `--dollars` |
| `sessions` | the 365-day session count |
| `active_days_28`, `longest_streak`, `current_streak`, `most_active_day` | the All time insights; `most_active_day` omitted when there is none |
| `windows` | `week`, `month`, `year`, each with display `tokens` and `usd` |
| `models` | every model in the cache, unfolded and uncapped, sorted by dollars: id, display name, token split, `usd`, `share` as a fraction |
| `agents` | every agent with tokens, unfolded and uncapped, sorted by sessions: kind, name, tokens, `usd`, `sessions`, `share` |
| `days` | per-day `date`, `tokens`, `usd` |
| `assists` | the window label, the rollup, and every event |

Windows, models, and agents carry `tool_calls` and a per-name `tools` map, each omitted when zero or empty. Models and agents carry `cache_hit_pct`, omitted when the denominator is zero.

`--assists` (`assists.rs`) prints `assists (all)` with the non-zero category counts, then one forensic line per event, newest first, in the configured time zone; an empty log prints `no assists recorded`. It conflicts with `--json` and `--refresh`.

Assists come from the account-global assist log, which [loops.md](./harness/loops.md#the-assist-log) owns along with what counts as an assist and how its records fold. `AssistStats::from_records` folds them into four panel categories, and the panel prints only those with a non-zero count:

| Row | Counts |
| --- | --- |
| `Auto-continue:` | delivered continues, with the summed recovered hours |
| `Auto-compact:` | every `auto_compact` record, plus delivered `idle_compact` and `flip_compact` records |
| `Auto-redeem:` | redeem attempts, with the `reset` outcomes |
| `Auto-resume:` | rebirth restores, with the agents they brought back |

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

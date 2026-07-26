# The stats panel

`rimz stats` renders your account-global token and dollar history: a heatmap of daily use, totals for a chosen window, where the spend went by model and by agent, activity insights, and the automated assists RimZ performed on your behalf. It reads only — it touches no agent, writes nothing to any session, and answers from the shared spend cache the sidebar producer already maintains, so it runs in or out of a room and in or out of a project.

The command has two homes. A one-shot `rimz stats` prints the panel and returns to the shell, so it pipes and scrolls like any report. `rimz stats --refresh` holds the same panel open as a live screen, and that held pane is the default middle-column content of the `rimzd` runtime view.

The code is `crates/rimz/src/cli/stats/`. The user-facing counterparts are the [Token Insight guide](../guide/insight.md), which explains what every figure means, and the [stats CLI reference](../reference/cli/stats.md).

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

The `Assists` block appears under the insight rows when the account has recorded any; this capture has none.

Each heatmap cell is one UTC day on a five-step ramp that reserves `·` for a day with no usage and rises `░ ▒ ▓ █` through active days, scaled against the busiest day in view rather than an absolute ceiling. Weeks open on Monday. `--dollars` scales the same grid by spend instead of tokens.

## Where the figures come from

This module owns no spend arithmetic. Walking transcripts, pricing models, and publishing the aggregate all belong to the spending producer described in [providers.md](./agents/providers.md). Stats is a reader over one artifact: `provider-spending.json` under the shared state root (`RuntimePaths::shared_provider_spending_path`), which carries the per-day buckets, the per-model and per-agent tallies, and the trailing windows. `Stats::from_provider` is the whole translation.

Three load paths in `mod.rs` answer a run, in order of preference:

| Path | When it runs | What happens |
| --- | --- | --- |
| Published cache | The cache exists and `is_current_version` | Read and render. The common case, and the reason a warm `rimz stats` is instant. |
| Cold walk | The cache is missing or on an older shape | Single-flight through `coalesce` on `shared_spending_lock`: one process walks with a `SpendingWalker` and publishes, every other process waits and then reads what it published. |
| Elected service | The held `--refresh` worker thread | `spending::service::request` with `SpendingServiceStartup::HostEligible`, so a long-lived dashboard can become the warm owner and keeps no walker of its own. |

The cold walk shows a progress spinner only when the run is human-facing and both stdout and stderr are TTYs (`should_animate_cold_stats`), so a pipe gets the report and no progress chrome. If the service call fails, the held loop falls back to whatever is published rather than surfacing an error frame.

Account-global setup creates the shared roots only (`ensure_shared_runtime`); stats never opens a workspace tree.

## The window model

`Window` has four variants: `AllTime`, `Week`, `Month`, `Year`. Four behaviors are worth knowing before you read the render code.

- **The heatmap ignores the window.** It always draws the full available history, which is the trailing year the cache spans. The window scopes the model breakdown, the agent breakdown, and the insight rows beneath them.
- **All time and Year are the same number.** `Window::select` maps both onto the trailing-year tally, because a year is the longest span the cache carries. "All time" is a label, not a wider read.
- **Non-interactive runs are always All time.** `render_panel` receives `active: None` and falls back to `AllTime`, so the report shape stays fixed for pipes, scripts, and `--json`.
- **Only the held dashboard can change it.** `Tab` and `Shift-Tab` cycle the windows row into a tab bar; a one-shot run has no way to select a different window.

Every window's displayed token total uses [`SpendWindow::display_tokens`](../../crates/rimz/src/agents/spending/aggregate.rs), which adds cache-read tokens to the input-plus-output total. Tokens are attributed per model and per agent independently of pricing coverage, so an unpriced model still contributes its tokens to the breakdown. Spending cache v21 also carries the named tool-call map parsed with each supported response; aggregation sums those maps into `u64` totals per window, model, and agent, and the one-time version bump reparses finalized history.

Full-width model and agent rows append the input-side cache-hit percentage: `cache_read / (cache_read + input)`, where the aggregate `input` already includes cache writes. The integer uses round-half-up and stays absent for a zero denominator. The shared health classifier paints 90% and above green, 70–89% yellow, and lower values red. Compact rows omit the column.

Each named model must carry at least 1.0% of the window's model spend, and each named agent must carry at least 1.0% of its sessions. Smaller entries fold into `Other` before the row cap applies. A section with no priced model spend or no agent sessions has no defined percentage denominator, so its entries remain itemized until the cap applies. Machine-readable JSON keeps every entry separate.

## Rendering

The panel is plain strings, not widgets. `render_panel` builds a `Vec<String>` and `emit` writes it with a shared left pad. Crossterm enters only for key events in the held loop, and the sidebar pane's ratatui stack is not on this path at all — expect string arithmetic, and expect tests that assert on rendered text.

**Geometry.** `PanelGeometry::current` reads the terminal once per render. Columns choose the heatmap width through `weeks_for_terminal`, clamped between 4 and 52 weeks; the week count then fixes the panel width, and the panel centres in the terminal. Row count is read only when stdout is a TTY, so a pipe carries `None` and never degrades.

**Degradation.** `fit` spends a row budget with data outranking chrome. The wordmark drops first, before any data row. The two breakdowns then shrink toward a floor of three rows each, split by `allocate_breakdown_rows` in proportion to what each section naturally wants after the 1.0% fold. Both cap at six rows with the remaining tail folded into the same final `Other`, so a breakdown never grows without bound. A piped run keeps the full panel.

**Glyphs.** `resolve_panel_glyphs` reads the machine theme, so `[theme] style = "modern"` with `[theme.glyphs] set = "nerd_font"` swaps the token vocabulary (`◎ ◇ ↘ ↗ ◌`) for its Nerd Font equivalents while the CLI colors stay on the default palette. The [theme pipeline](./theme.md) owns that resolution.

**Empty state.** With no recorded days the panel prints one muted line instead of an empty grid, plus the assists block if any assists exist.

## The held dashboard

`--refresh` runs `hold::run_refresh`. `TerminalModeGuard` takes the terminal in raw mode on the alternate screen with mouse capture off, so the dashboard owns the full pane without adding mux scrollback, keypresses arrive as events rather than echoing, and stray mouse reports get drained instead of printed.

Each cycle spawns a worker thread that loads through the elected spending service and sends back a single `Result<Stats>`; the foreground polls for keys every 100ms against a 60-second refresh deadline. A failed refresh holds the last frame and logs the failure streak's first warning rather than exiting; consecutive failures drop to debug until a refresh succeeds. A dashboard that has no frame yet retries after 5 seconds, then returns to the 60-second cadence once a refresh succeeds. Stderr logging is off for this rendered command, so warnings cannot smear its raw-mode frame; the reporting layer still receives them.

Rendering follows three states. A current stats frame always wins, including while the latest refresh is failing, so a live panel gets no staleness marker. A failure before the first stats frame paints a centred unavailable message with the cause and retry status; resize and window keys repaint that same state. Before either outcome arrives, the dashboard writes nothing. The unavailable frame is identical for interactive `--refresh` and rimzd's `--refresh --hold`, and the first successful refresh replaces it in place.

| Key | Outcome |
| --- | --- |
| `Tab` / `Shift-Tab` | Cycle the selected window, repainting from the frame already in hand with no refetch |
| `r` | Reload the binary in place |
| `Ctrl-C` | Quit, unless `--hold` is set |

**Reload.** The `r` key and `SIGUSR1` set the same flag, which the cycle consumes once and turns into a re-exec of `reload::current_reexec_target()` with the original argv. `rimz reload` drives that signal remotely. Registering the handler replaces `SIGUSR1`'s default-terminate disposition, so the dashboard catches the signal instead of dying on it.

**`--hold`.** A hidden flag that requires `--refresh` and belongs to the daemon view. It makes `Ctrl-C` a no-op, so leaving the pane does not kill the dashboard, while closing the pane still ends the process. `daemon_content::stats_argv` is exactly `rimz stats --refresh --hold`; `[daemon]` in `config.toml` replaces or extends that pane, and the pane count takes effect on room restart.

## Machine-readable surfaces

`--json` (`json.rs`) emits the stats document instead of the panel: unit, session count, active-day and streak insights, the trailing windows, the per-model and per-agent breakdowns with optional `cache_hit_pct`, tool-call totals and per-name maps on every window and breakdown, the per-day buckets, and the assists rollup with its events. This is the stable surface for scripts. It renders All time and conflicts with `--refresh`.

`--assists` (`assists.rs`) prints the complete newest-first assist timeline instead of the dashboard, one line per event with its forensics. It conflicts with `--json` and `--refresh`.

Assists come from the durable `harness::assist_log`, folded into an `AssistRollup` of four categories: auto-continue with resumes and recovered time, auto-compact with its count, auto-redeem with attempts and resets, and auto-resume with restores and the sessions they brought back. The panel prints only the categories with a non-zero count; the timeline prints everything.

The line this draws is what counts as an assist. Automation that benefits the user earns a record here; RimZ repairing its own mux state does not, which is why focus repair keeps a durable record in the [diagnostics log](./diagnostics.md) rather than a row in this panel. A new smart strategy adds its record here when it acts for the user.

## Where the code lives

| File | What it owns |
| --- | --- |
| `mod.rs` | `StatsArgs`, `Window`, `Stats`, the three load paths, the shared constants, and the wordmark |
| `panel.rs` | Geometry, the `fit` degradation ladder, the unavailable and empty frames, the heatmap, both breakdowns, insights, and `emit` |
| `hold.rs` | The `--refresh` loop, key handling, the reload signal and re-exec, and the cold-load spinner |
| `assists.rs` | The assist rollup, its panel rows, and the `--assists` timeline |
| `json.rs` | The `--json` document |
| `fmt.rs` | Token, dollar, and day formatting, and week fitting |
| `tests.rs` | The unit suite |

## Tests

`tests.rs` covers this module as pure unit tests over rendered strings. There are no golden `.snap` frames here — that pattern belongs to the sidebar pane. The load paths run against temporary `RuntimePaths` (a published cache served without a walk, a cold refresh publishing the rollups the sidebar then reads), the panel is asserted through `strip_ansi` on layout, ranking, folding, and the fit ladder, and the held loop is driven through `key_outcome` and `HeldStats` without a terminal.

```sh
cargo xtask test stats
```

## See also

- [providers.md](./agents/providers.md) — accounts, spend computation, the incremental cache, and the pricing table behind every figure here.
- [theme.md](./theme.md) — the color pipeline and glyph catalog the panel resolves through.
- [Token Insight guide](../guide/insight.md) — what the figures mean, for the reader who is using RimZ rather than changing it.

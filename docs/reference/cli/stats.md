# Stats CLI

`rimz stats` prints your token and dollar history across every project on the machine: a heatmap of daily use, totals by window, the spend by model and by agent, activity insights, and the assists RimZ performed. It touches no agent and no session, and it runs anywhere, in or out of a room or a project. What each figure means and how dollars are priced is the [Token Insight guide](../../guide/insight.md); how the panel is computed and fitted to the terminal is [stats internals](../../internals/stats.md).

```sh
rimz stats                              # print the panel once
rimz stats --dollars                    # shade the heatmap by dollars instead of tokens
rimz stats --refresh                    # hold the panel open and repaint every minute
rimz stats --json | jq '.windows.week'  # the trailing week's tokens and dollars
rimz stats --assists                    # every assist RimZ recorded, newest first
```

| Flag | Effect |
| --- | --- |
| `--dollars` | Shade the heatmap by dollars spent and title it `Spend activity`. Under `--json` it only sets `unit` to `usd`; `--assists` ignores it. |
| `--json` | Print the [stats document](#json-output) instead of the panel. Conflicts with `--refresh`. |
| `--refresh` | Open the [held dashboard](#the-held-dashboard): a full-screen panel that refreshes every 60 seconds and re-centres on resize. Conflicts with `--json`. |
| `--assists` | Print the [assist timeline](#the-assist-timeline) instead of the panel. Conflicts with `--json` and `--refresh`. |

A conflicting pair exits `2`. Of the [global flags](../cli.md#global-flags), `--color` applies; `--root` and the backend flags are accepted and change nothing, because the figures are machine-wide.

## What the panel shows

The panel prints these blocks top to bottom:

| Block | Shows |
| --- | --- |
| Wordmark | The RimZ logo and tagline. Omitted when the terminal is too short to fit it beside the breakdowns. |
| `Token activity` | One cell per UTC day, one column per week, shaded `· ░ ▒ ▓ █` from no use to heavy use. The terminal width sets the span, from 4 weeks up to 52. |
| Windows row | Total tokens for `All time`, `Week`, `Month`, and `Year`. |
| `Models` | Up to six models ranked by dollars, with input, output, cache-read tokens, cache-hit percentage, and share of the window's dollars. |
| `Agents` | Up to six agent kinds ranked by sessions, with dollars, tokens, cache-hit percentage, and share of sessions. |
| Insights | Sessions, spend, cost per session, active days, most active day, streaks, and daily average. |
| `Assists` | Counts of auto-continues, auto-compactions, credit redemptions, and restored rooms, when any exist. |

A breakdown entry under 1% of its section, and any entry past the row cap, folds into a final `Other` row. A machine with no recorded usage prints the wordmark and `No token usage recorded yet - run an agent and check back.`

`All time` covers the trailing 365 days, because the history RimZ keeps spans one year, so its figures match `Year`. The two differ in the insights and assists: All time reports active days out of the last 28 (`Active days: N/28`), reads streaks across every cached day, and counts every assist on record. A one-shot run always shows All time; only the held dashboard selects another window, and the heatmap ignores the selection. The [guide](../../guide/insight.md#the-full-picture-rimz-stats) explains each insight line.

## How fresh the figures are

`rimz stats` reads the spending cache at `~/.rimz/shared/provider-spending.json` (under `$RIMZ_HOME` when it is set). A running sidebar keeps that cache current, and each mode treats it differently:

| Run | Cache | What happens |
| --- | --- | --- |
| `rimz stats`, `--json` | present | Prints the cache as it is, however old. Nothing is walked or fetched. |
| `rimz stats`, `--json` | missing, or written by an incompatible RimZ version | Reads every agent transcript on the machine, writes the cache, then prints. When the panel goes to a terminal, the wordmark prints first, then `Reading session files [bar] n/total`. |
| `--refresh` | any | Requests fresh figures every 60 seconds through the same spending service the sidebar uses, which rereads transcripts that changed. |
| `--assists` | not read | Reads only the assist log. |

A one-shot run outside a room can therefore print figures from the last time anything refreshed the cache. Run `rimz stats --refresh`, or open a room, to bring them up to date.

Every run except `--assists` creates RimZ's shared state directories when they are missing. Beyond that, only the transcript walk writes files or uses the network. The walk writes the spending cache, its walk cursor, and the price-table cache beside it, unless another process is already writing them. It fetches the price tables from LiteLLM and models.dev when the cached table is more than a week old, or sooner when your history holds models it has no price for. Set `RIMZ_PRICING_OFFLINE` to any value to skip every fetch; prices then come from the cached table and the table built into the binary. The walk and pricing are described in [spending internals](../../internals/agents/spending.md).

## The held dashboard

`rimz stats --refresh` takes over the pane on the terminal's alternate screen and repaints in place, leaving no scrollback. It is the default pane of the `rimzd` daemon view ([rimzd internals](../../internals/rimzd.md)).

| Key | Action |
| --- | --- |
| `Tab`, `Shift-Tab` | Select the next or previous window (All time, Week, Month, Year) for the breakdowns, insights, and assists. |
| `r`, `R` | Restart the dashboard on the current binary, keeping its flags. `rimz reload` does the same to every running dashboard. |
| `Ctrl-C` | Quit. |

No other key does anything; `q` does not quit. In the daemon view `Ctrl-C` is ignored too, and closing the pane ends the dashboard.

Week and Month add a spend comparison with the preceding week or month when that period had spend. A refresh that fails leaves the last frame on screen with no marker. When the first refresh fails, the dashboard shows `Spending refresh unavailable - retrying.` with the cause and tries again after 5 seconds.

## JSON output

`rimz stats --json` prints one pretty-printed object describing the All time window. Breakdowns carry every entry, with no `Other` folding and no row cap. A captured run, cut to one element per array:

```console
$ rimz stats --json
{
  "unit": "tokens",
  "sessions": 6860,
  "active_days_28": 27,
  "longest_streak": 72,
  "current_streak": 26,
  "most_active_day": "2026-07-18",
  "windows": {
    "week": {
      "tokens": 3634976121,
      "usd": 4228.222269640006,
      "tool_calls": 19853,
      ...
  "models": [
    {
      "model": "gpt-5.6-sol",
      "name": "GPT 5.6 Sol",
      "tokens": 25050389458,
      "input": 627793642,
      "output": 66083304,
      "cache_read": 24356512512,
      "tool_calls": 211015,
      ...
      "cache_hit_pct": 97,
      "usd": 13628.711094200002,
      "share": 0.383862868254068
    },
  ...
```

| Field | Contents |
| --- | --- |
| `unit` | `tokens`, or `usd` under `--dollars`. The token and dollar fields below are present either way. |
| `sessions` | Sessions in the trailing 365 days. |
| `active_days_28` | Days with usage among the last 28. |
| `longest_streak`, `current_streak` | Consecutive days with usage. |
| `most_active_day` | `YYYY-MM-DD` of the heaviest day. Omitted when there is no usage. |
| `windows` | `week`, `month`, and `year`, each with `tokens`, `usd`, `tool_calls`, and a `tools` map of calls by tool name. |
| `models` | One object per model, sorted by `usd`: `model` id, display `name`, `tokens`, `input`, `output`, `cache_read`, `tool_calls`, `tools`, `cache_hit_pct`, `usd`, and `share` of the total dollars as a fraction. |
| `agents` | One object per agent kind with usage, sorted by sessions: `kind`, display `name`, `tokens`, `usd`, `sessions`, `tool_calls`, `tools`, `cache_hit_pct`, and `share` of sessions as a fraction. |
| `days` | One object per UTC day with usage, oldest first: `date`, `tokens`, `usd`. |
| `assists` | `window` (`all`), a `rollup` of counts, and `events`, newest first. |

`tokens` is always input plus output plus cache-read tokens, and `input` includes cache writes. `tool_calls` and `tools` are omitted when zero or empty; `cache_hit_pct` is omitted when there is no input to measure.

The `assists.rollup` object holds `redeems` and `resets` (credit redemptions, and those that reset the budget), `resumes` and `recovered_secs` (delivered auto-continues, and the parked time they recovered), `compacts`, `restores` and `restored_sessions` (room restores, and the agents they brought back), and `sweeps` and `reclaimed_bytes` (completed automatic gc sweeps, and the bytes they freed). Each event carries an `assist` type (`auto_continue`, `auto_compact`, `idle_compact`, `flip_compact`, `auto_redeem`, `auto_resume`, or `auto_gc`), its `at` time in UTC, and the fields of that type. What each assist is lives in [the assist log](../../internals/harness/loops.md#the-assist-log).

## The assist timeline

`rimz stats --assists` prints every recorded assist as plain text: a header with the non-zero category counts, then one line per assist, newest first, with times in the configured `timezone`. Each line names what happened and then the identifiers to trace it, separated by `·`. A captured run, cut to one line per kind:

```console
$ rimz stats --assists
assists (all) — Auto-continue: 110 (+77.0h) · Auto-compact: 340 · Auto-redeem: 9 (6 resets) · Auto-resume: 11 (88 agents)
2026-09-14 12:07 ⌁ @planner flip compaction held — Reflect → Done — 134k ctx, threshold 120k · agent 6c474dfb-5c8c-4edf-a1ea-9e37572cb384 · message msg_06g9s6c235cm64o4 · delivered false
2026-09-12 19:21 ⌁ @coder auto-compact — 260k ctx cleared before delivery · agent 01a0950c-2a1d-7900-b4ec-b5c79226860a · message msg_06g9amhl56ac37d8 · threshold 258k
2026-09-12 15:15 ▶ @finder resumed — overload park, 15:12→15:15 (0.1h recovered) · agent 01a09475-8284-7c60-9bd3-3f1803b10e14 · message msg_06g98u2jncrghd65 · delivered true
2026-09-10 13:48 ↻ codex credit — blocked gain → budget reset ✓ · request 01a089db-f2cc-7970-a32f-8e1c1d3b743c · 2 credits · expiry 2026-10-04 13:42 · natural reset 2026-09-15 10:04 · windows 5h→unknown, 7d→2026-09-17 13:48
2026-09-08 10:56 ⟲ rebirth recovery — 13 agents restored after crash (#wait-all, #subagents-card, #subagents-wait, #codex-claude, #rimz) · workspace ws_f89e49906df0621ad2765112 · session rimz-rimz-f89e49
2026-07-30 08:49 ⌁ @coder idle compacted after 8.1h — 88k ctx · agent 019faea5-d8ce-7e92-a15e-3c9fad352934 · message msg_06fr0vovclep1ivs · delivered true
```

| Mark | Assist |
| --- | --- |
| `▶` | Auto-continue: a parked agent resumed, or `resume held` when delivery did not happen. |
| `⌁` | Compaction: before a delivery (`auto-compact`), after an idle gap (`idle compacted`), or at a team stage flip (`flip compaction`). `held` means it was not delivered. |
| `↻` | Credit redemption for a provider account, with its outcome. |
| `⟲` | Room restore after a crash or reboot, with the channels it brought back. |
| `♻` | Automatic `rimz gc` sweep: what it reclaimed and removed, with its cutoff, or `gc failed` with the error. |

With no records, the command prints `no assists recorded`. There is no JSON form of the timeline; use the `assists.events` array of `--json`.

## See also

- [Token Insight guide](../../guide/insight.md): what each figure means, the sidebar's live view of the same data, and how tokens are priced.
- [Providers CLI](./providers.md): per-account plan limits and balances.
- [Budget CLI](./budget.md): caps that park agents when spend crosses a line.
- [Stats internals](../../internals/stats.md): load paths, heatmap shading, and terminal fitting.

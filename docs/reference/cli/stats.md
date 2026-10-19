# Stats CLI

`rimz stats` prints your token and dollar history across every project on the machine: spend and token totals, model and role breakdowns, and a heatmap of daily use. Its held view adds activity insights and assist counts; `--assists` shows the timeline. It touches no agent and no session, and it runs anywhere, in or out of a room or a project. What each figure means and how dollars are priced is the [Token Insight guide](../../guide/insight.md); how the panel is computed and fitted to the terminal is [stats internals](../../internals/stats.md).

```sh
rimz stats                              # print the panel once
rimz stats --dollars                    # shade the heatmap by dollars instead of tokens
rimz stats --refresh                    # hold the panel open and repaint every minute
rimz stats --json | jq '.windows.week'  # the trailing week's tokens and dollars
rimz stats --assists                    # latest 50 effective assists, newest first
rimz stats --assists -n 0 --all          # all retained attempts, including held ones
rimz stats --subagents                  # every subagent profile, unfolded
```

| Flag | Effect |
| --- | --- |
| `--dollars` | Shade the heatmap by dollars spent and title it `Spend activity`. Under `--json` it only sets `unit` to `usd`; `--assists` ignores it. |
| `--json` | Print the [stats document](#json-output) instead of the panel. Conflicts with `--refresh`. |
| `--window <WINDOW>` | Select `today` (default), `week`, `month`, or `year` for breakdowns, JSON sessions and assists, and the held dashboard's initial tab. The Spend row always shows all four. Conflicts with `--assists`. |
| `--refresh` | Open the [held dashboard](#the-held-dashboard): a full-screen panel that refreshes every 60 seconds and re-centres on resize. Conflicts with `--json`. |
| `--assists` | Print the [assist timeline](#the-assist-timeline). Conflicts with `--subagents` and `--refresh`; supports `--json`. |
| `--subagents` | Show every non-zero subagent profile in the selected window, with spend, fleet dollar share, input, output, cached tokens, hit rate, and runs (distinct sessions). Supports `--json`; conflicts with `--refresh`. |
| `-n N` | With `--assists`, show at most N events (default 50); 0 means all retained. |
| `--all` | With `--assists`, include held and undelivered attempts. |

A conflicting pair exits `2`. Of the [global flags](../cli.md#global-flags), `--color` applies; `--root` and the backend flags are accepted and change nothing, because the figures are machine-wide.

## What the panel shows

History days use the configured `timezone`, or the system's named timezone. An unnamed system zone falls back to UTC; set `timezone` to an IANA name to select another zone.

The one-shot panel opens with dollars and tokens for Today, Week, Month, and Year. Today starts at local midnight; the other windows trail 7, 30, and 365 days.

Models, Agents, and Subagents follow, each labelled with the selected window, `(today)` by default. Every share is the row's dollars divided by fleet dollars in that window. Each section ranks by dollars, shows the top three non-zero rows, and folds the rest as `N other models`, `N other agents`, or `N other subagents`. Empty sections are omitted. Token units are uppercase. Models use the identity colour of their producing agent kind. Agents shows `SESSIONS TOKENS HIT TOK/S`; folded Subagents shows `RUNS TOKENS HIT TOK/S`.

Agents includes all spend by provider kind, including historical and unobserved sessions. Subagents groups sessions launched by `rimz subagents` under their profile (or provider kind for bare-kind launches), as a subset of Agents. Native provider children inherit their pane's role, not a separate subagent row. When the selected window begins before role recording did, a muted note below Subagents gives the first recorded date: earlier spend appears under Agents only. This includes the partial oldest day of a trailing window. The one-shot note also appears when no profile rows exist.

The local-day heatmap follows the breakdowns. The final `as of` time is the publication time in its published timezone, with a date when the publication is not from today. A cold one-shot run does not print the wordmark; the held dashboard keeps it when space permits.

## How fresh the figures are

`rimz stats` reads the spending cache at `~/.rimz/cache/providers/provider-spending.json` (under `$RIMZ_HOME` when it is set). A running sidebar keeps that cache current, and each mode treats it differently:

| Run | Cache | What happens |
| --- | --- | --- |
| `rimz stats`, `--json`, `--subagents` | present and its figures cover today in its timezone | Prints the cache as it is. Nothing is walked or fetched. |
| `rimz stats`, `--json`, `--subagents` | missing, its figures cover another local day, or written by an incompatible RimZ version | Reads every agent transcript on the machine, writes the cache, then prints. When the panel goes to a terminal, a `Reading session files [bar] n/total` spinner reports progress. |
| `--refresh` | any | Requests fresh figures every 60 seconds through the same spending service the sidebar uses, which rereads transcripts that changed. |
| `--assists` | not read | Reads only the assist log. |

A one-shot run can print figures from earlier today. A walk that begins before midnight still covers the earlier day even if it finishes after midnight, so a one-shot run refreshes it again. Run `rimz stats --refresh`, or open a room, for updates within the day.

If a refresh cannot read session files and only an older publication remains, a one-shot run exits with an error rather than showing yesterday's breakdowns as today. The older publication stays intact, and a held dashboard can continue showing it.

Every run except `--assists` creates RimZ's shared state directories when they are missing. Beyond that, only the transcript walk writes files or uses the network. The walk writes the spending cache, its walk cursor, and the price-table cache beside it, unless another process is already writing them. It fetches the price tables from LiteLLM and models.dev when the cached table is more than a week old, or sooner when your history holds models it has no price for. Set `RIMZ_PRICING_OFFLINE=1` to skip every fetch (`0`, `false`, and an empty value leave fetching on); prices then come from the cached table and the table built into the binary. The walk and pricing are described in [spending internals](../../internals/agents/spending.md).

## The held dashboard

`rimz stats --refresh` takes over the pane on the terminal's alternate screen and repaints in place, leaving no scrollback. It is the default pane of the `rimzd` daemon view ([rimzd internals](../../internals/rimzd.md)).

| Key | Action |
| --- | --- |
| `Tab`, `Shift-Tab` | Select the next or previous window (Today, Week, Month, Year) for the breakdowns, insights, and assists. |
| `r`, `R` | Restart the dashboard on the current binary, keeping its flags. `rimz reload` does the same to every running dashboard. |
| `q`, `Ctrl-C` | Quit. |

Other keys do nothing. In the daemon view both `q` and `Ctrl-C` are ignored; closing the pane ends the dashboard.

The dashboard starts on Today, or the tab selected by `--window`. It includes Models, Agents, and Subagents; Agents adds a `tok/s` column when any row has response timing. Short panes drop the wordmark before shrinking each section to one row, then omit Subagents and its recorded-since note if all three sections cannot fit. Today scopes the breakdowns, sessions, cost per session, spend, and assists to the local calendar day in the published timezone. Its activity rows and daily average keep Year's history, so today's spend can be compared with a typical active day. Today and Year have no spend trend; Week and Month add a spend comparison with the preceding week or month when that period had spend. A refresh that fails leaves the last frame on screen with no marker. When the first refresh fails, the dashboard shows `Spending refresh unavailable - retrying.` with the cause and tries again after 5 seconds. Reload returns to the initial tab specified by the flags.

## JSON output

`rimz stats --json` prints one pretty-printed object describing the selected window (today by default). Breakdowns carry every non-zero-spend entry in that window, with no `Other` folding and no row cap. To inspect a timed model over the year:

```sh
rimz stats --window year --json | jq 'first(.models[] | select(has("tokens_per_sec"))) | del(.tools)'
```

| Field | Contents |
| --- | --- |
| `as_of`, `timezone` | Publication time (RFC 3339 with offset) and the timezone used by the daily buckets. |
| `unit` | `tokens`, or `usd` under `--dollars`. The token and dollar fields below are present either way. |
| `window` | Selected window: `today`, `week`, `month`, or `year`. |
| `subagents_since` | First retained role-record date, `YYYY-MM-DD` in the published timezone. Omitted when no records exist. |
| `sessions` | Sessions in the selected window. |
| `active_days_28` | Days with usage among the last 28. |
| `longest_streak`, `current_streak` | Consecutive days with usage. |
| `most_active_day` | `YYYY-MM-DD` of the heaviest day. Omitted when there is no usage. |
| `windows` | `today`, `week`, `month`, and `year`, each with `tokens` and `usd`, plus `tool_calls` and a `tools` map of calls by tool name, each omitted when empty. `today` is the local calendar day in the published timezone. |
| `models` | One object per model, sorted by `usd`: `model` id, display `name`, `tokens`, `input`, `output`, `cache_read`, `tool_calls`, `tools`, `cache_hit_pct`, `tokens_per_sec`, `usd`, and `share` of the total dollars as a fraction. |
| `agents` | One object per provider kind with non-zero selected-window spend, sorted by dollars: `kind`, display `name`, `tokens`, `usd`, `sessions`, `tool_calls`, `tools`, `cache_hit_pct`, optional `tokens_per_sec`, and `share` of selected-window fleet dollars as a fraction. |
| `days` | One object per local calendar day in the published timezone with usage, oldest first: `date`, `tokens`, `usd`. |
| `subagents` | Every non-zero selected-window profile: `profile`, `runs`, `input`, `output`, `cached`, `cache_hit_pct`, `tokens`, `usd`, and selected-window fleet dollar `share`. |
| `assists` | The selected-window effective-assist rollup only, without events. |

`tokens` is always input plus output plus cache-read tokens, and `input` includes cache writes. `tool_calls` and `tools` are omitted when zero or empty; `cache_hit_pct` is omitted when there is no input to measure.

`tokens_per_sec` is a rounded integer from the selected-window tally, omitted when the model or agent has no timed responses. Activity days, streaks, and the most-active day retain their full-history meaning regardless of `--window`.

An `auto_gc` event carries `scope` (`room` or `machine`); records written before scopes existed read as `machine`. Totals cover the whole sweep's scope. The durable assist record additionally holds own-room `class_bytes` and `worktree_paths` (the first 50 removed paths in sweep order, `[]` for legacy records), which this JSON projection does not expose. `worktrees_removed` remains the full count, so a count above the path-list length indicates truncation on new records ([GC report](./maintenance.md#the-report)).

A `tmp_evict` event counts one successful temp-unit eviction toward `tmp_evictions`, not `sweeps`, and its allocated bytes toward `reclaimed_bytes`; an error counts under `held.tmp_evictions`. The `Temp evictions: N` category shows its own count and held attempts, separately from `Auto-gc:` sweeps; Auto-gc's byte detail totals both kinds. Its GC timeline row names the handle, reclaimed bytes, and workspace. JSON also carries optional `ended_at`, `free_bytes_after` (the measured filesystem-available space, which may lag removal), `floor_bytes`, and `error` on failure. Allocated bytes count hardlinked files once per tree; snapshots can retain those blocks after removal.

The `assists` object holds `judge_declines` (fresh judge polarity declines), `model_aliases` (catalog-derived model moves), `redeems` and `resets` (successful window-reset redemptions, and those whose outcome is `reset`), `manual_redeems` and `manual_resets` (the user-initiated subset, excluded from the `Auto-redeem:` text row), `resumes` and `recovered_secs` (delivered auto-continues, and the parked time they recovered), `compacts`, `keepalives` (delivered prompt-cache pings), `idle_stops` (`stop --when-idle` requests that stopped their agent), `molts` (fresh successor tabs opened), `launch_retries` (relaunches of agents that died at startup), `restores` and `restored_sessions` (room restores, and the agents they brought back in a tab confirmed open), `sweeps` (completed automatic gc sweeps), `tmp_evictions` (successful temp-unit pressure evictions), `reclaimed_bytes` (their combined bytes: apparent for sweeps, allocated for temp units), and `workflow_starts`, `workflow_steps`, `workflow_routes`, `workflow_parks`, and `workflow_ends` (workflow runs a loop task started, step attempts launched, routes including closed merged waits, parks, and runs that reached `done`, `failed`, or `canceled`). `held`, present when any attempt did not take effect, maps each of those count names to its held attempts. Timeline events carry an `assist` type (`stall_notice`, `resident_launch`, `tier_fallback`, `hibernate`, `judge_decline`, `model_alias`, `workflow_start`, `workflow_step`, `workflow_route`, `workflow_park`, `workflow_end`, `auto_continue`, `auto_compact`, `idle_compact`, `cache_keepalive`, `idle_stop`, `molt`, `launch_retry`, `flip_compact`, `auto_redeem`, `auto_resume`, `auto_gc`, or `tmp_evict`), its `at` time in UTC, and the fields of that type. Model moves carry `kind`, `login`, `alias`, `from`, and `to`. A redemption carries the account name as `login` when its record has one, and its text row names the account when it is not `default`, as in `↻ codex@spare credit`. Manual redemptions keep the `auto_redeem` record type with `reason: "manual"`; their event text says `manual`, and they are not automation assists. What each assist is lives in [the assist log](../../internals/harness/loops.md#the-assist-log).

The `Judge declines: N` category counts fresh judge declines, not later fires held by the same decision. Each `judge_decline` event carries `task`, `checkout`, judge `profile` and `kind`, `reason`, and optional `cost_usd`. Its timeline line names the task, profile, reason, checkout, and recorded cost. Manual fire, a new true condition edge, or an elapsed recheck can produce a fresh decline ([resident judge memory](./loop.md#waits-and-checks)).

The `Keepalive: N` category counts delivered pings. Each `cache_keepalive` event carries `kind`, `agent_id`, optional `label`, `idle_secs`, `waits`, optional `horizon_secs`, `message_id`, `delivered`, optional `error`, and `capped` (true on the ping that told the agent the keepalive maximum was reached; its `◷` line ends with `, limit reached`). `horizon_secs` is the [keep-warm](../../guide/configuration.md#keep-warm) finite maximum that was holding the agent (`max`, or the keepalive cap for a `while`-only hold); its timeline line reads `holding warm (2h)`. `waits` counts all pending waits, including subagent and team waits. Failed attempts appear with `--all` without increasing the count.

The `Idle stop: N` category counts [`stop --when-idle`](./agents.md#stop) requests that stopped their agent. Each `idle_stop` event carries `kind`, `agent_id`, `label`, `idle_secs` (how long the agent had rested), `idle_after_secs` (the requested threshold), optional `requested_by` (the asking agent's handle), `stopped` (the agent's own pane closed), and optional `error`. A stop that left the pane open appears with `--all` with `stopped: false` and leaves its request armed. `error` can accompany `stopped: true`: the agent closed and something else failed, such as a child's pane staying open.

The `Launch retry: N` category counts relaunches of agents whose CLI died before opening a session, one per [relaunch](./agents.md#launch-a-layout). Each `launch_retry` event carries `kind`, `label` (the agent's handle), `run_id` (omitted for a launch without a supervised run), `attempt` (which relaunch of that launch, from 1), `exit_code` (omitted when a signal killed the process), `startup_ms` (how long the process that died lived), `relaunched`, and optional `error`. A relaunch that could not spawn appears with `--all` and `relaunched: false`.

The `Workflow: N` category sums the five [workflow](./workflow.md) assist kinds. Every workflow event carries `run` (the `wf_` id) and `workflow`. `workflow_start`, emitted only for loop-created runs, adds `task` (the loop task), `checkout`, and `prompt` (whether the run has one); its line reads `workflow babysit-pr started by loop babysit-pr`. `workflow_step` adds `step`, `visit`, `seq`, and the attempt's `handle` and `run_id`; `workflow babysit-pr sweep launched (visit 2)`. `workflow_route` adds `node`, `outcome`, and `to` (the next node, or `@wait`, `@done`, `@failed`, `@user`); `workflow babysit-pr pr said needs-sweep → sweep`. A merged wait writes one route when it closes, with `count` set to its final poll count; its line reads `workflow babysit-pr pr waited ci-pending ×12`. An open wait contributes no route yet; other routes omit `count`, and older events without it still render as `said`. `workflow_park` adds `node`, `reason`, and optional `text`; `workflow babysit-pr triage parked: asked: Which fix ships first?`. `workflow_end` adds `state` (`done`, `failed`, or `canceled`); `workflow babysit-pr done`.

## The assist timeline

The `Molts: N` category counts agents moved into fresh sessions by `rimz agents molt`. The JSON rollup exposes `molts`; each `molt` event names the predecessor (`kind`, `agent_id`, `label`), the successor (`successor_kind`, `successor_id`, `successor_label`), and optional inherited `generation`. The successor id is its launch id. A molt counts once its new tab opens, even if the successor later fails or arming the predecessor's idle stop fails. Timeline lines show both handles; forensic lines retain both providers and ids.

By default, `rimz stats --assists` prints the latest 50 events that took effect, newest first, with the non-zero category counts over all retained records. Times use the configured timezone. Each line names the outcome and identifiers needed to trace it. `--all` includes held and undelivered attempts. When more remain, the trailer says `… N older; rimz stats --assists -n 0 for all`. Zero means all retained records across both log generations, not unlimited history.

The header counts each kind's attempts that did not take effect beside it, for example `Auto-compact: 2 (239 held)`. Delivery-based events count only when delivered, except a stall notice, which counts once queued for its parent and is held only on an error; stops, freezes and relaunches use their recorded success flags. Redemption counts require a window reset; GC requires no error. Molts, model aliases, tier fallbacks, resident launches, judge declines, compact commands and room resumes count as recorded.

`rimz stats --assists --json` emits `{as_of, rollup, events, older}`, respecting `-n` and `--all`; its `as_of` is the read time, with the offset of the configured timezone. `rimz stats --subagents --json` emits `{as_of, window, subagents, subagents_since?}`. The default stats JSON carries the assist rollup without events; its `agents` rows are provider kinds and their `share` is a dollar share.

| Mark | Assist |
| --- | --- |
| `▶` | Resident launch: a scheduled task opened its agents. |
| `↘` | Tier fallback: the profile used the next available model. |
| `▶` | Auto-continue: a parked agent resumed, or `resume held` when delivery did not happen. |
| `◷` | Cache keepalive: idle duration, the keep-warm horizon when one was holding, and pending-wait count, followed by agent, message, and delivery verdict. |
| `■` | Idle stop: an agent stopped by its `--when-idle` request, with how long it rested, the threshold, and who asked; `idle stop failed` carries the error. |
| `⤳` | Molt: the predecessor moved into a fresh successor session. |
| `↺` | Launch retry: an agent whose CLI exited before its session opened, with the exit code, how long it lived, and the attempt number; `relaunch failed` carries the error. |
| `⌁` | Compaction: before a delivery (`auto-compact`), after an idle gap (`idle compacted`), or at a team stage flip (`flip compaction`). `held` means it was not delivered. |
| `↻` | Credit redemption for a provider account, with its outcome. |
| `⟲` | Room restore after a crash or reboot, with the channels it brought back. |
| `♻` | Automatic `rimz gc` sweep: `gc swept` for room scope or `machine gc swept` for machine scope, followed by what it reclaimed and removed and its cutoff; `gc failed` carries the error. |

With no records, the command prints `no assists recorded`. Use `--assists --json` for its structured form.

## See also

- [Token Insight guide](../../guide/insight.md): what each figure means, the sidebar's live view of the same data, and how tokens are priced.
- [Providers CLI](./providers.md): per-account plan limits and balances.
- [Budget CLI](./budget.md): caps that park agents when spend crosses a line.
- [Stats internals](../../internals/stats.md): load paths, heatmap shading, and terminal fitting.

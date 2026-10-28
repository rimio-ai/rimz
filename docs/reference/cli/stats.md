# Stats CLI

`rimz stats` prints your token and dollar history across every project on the machine: spend and token totals, model and role breakdowns, and a heatmap of daily use. Its held view adds a stacked spend chart and assist counts; `--assists` shows the timeline. It touches no agent and no session, and it runs anywhere, in or out of a room or a project. What each figure means and how dollars are priced is the [Token Insight guide](../../guide/insight.md); how the panel is computed and fitted to the terminal is [stats internals](../../internals/stats.md).

```sh
rimz stats                              # print the panel once
rimz stats --dollars                    # shade the heatmap by dollars instead of tokens
rimz stats --refresh                    # hold the panel open and repaint every minute
rimz stats --json | jq '.windows.week'  # tokens and dollars over the last 7 local days
rimz stats --assists                    # latest 50 effective assists, newest first
rimz stats --assists -n 0 --all          # all retained attempts, including held ones
rimz stats --agents                     # every agent profile, unfolded
rimz stats --teams                      # every team, without per-role rows
rimz stats --subagents                  # every subagent profile, unfolded
```

| Flag | Effect |
| --- | --- |
| `--dollars` | Shade the heatmap by dollars spent and title it `Spend activity`. Under `--json` it only sets `unit` to `usd`; `--assists` ignores it. |
| `--json` | Print the [stats document](#json-output) instead of the panel. Conflicts with `--refresh`. |
| `--window <WINDOW>` | Select `today` (default), `week`, `month`, or `year` for breakdowns, the activity summary, sessions and assists, and the held dashboard's initial tab. The Spend table always shows all four. Conflicts with `--assists`. |
| `--refresh` | Open the [held dashboard](#the-held-dashboard): a full-screen panel that refreshes every 60 seconds and re-centres on resize. Conflicts with `--json`. |
| `--assists` | Print the [assist timeline](#the-assist-timeline). Conflicts with `--agents`, `--teams`, `--subagents`, and `--refresh`; supports `--json`. |
| `--agents` | Show every solo agent profile with spend or recorded effort in the selected window, including provider-named rows, with spend, fleet dollar share, input, output, cached tokens, hit rate, distinct sessions, and agent-time. With `--json`, also include profiles with work tokens and no spend. Conflicts with `--teams`, `--subagents`, `--refresh`, and `--assists`. |
| `--teams` | Show every team with spend or recorded effort in the selected window, with the agent columns plus messages. Team roles appear only in JSON. With `--json`, also include teams with work tokens and no spend. Conflicts with `--agents`, `--subagents`, `--refresh`, and `--assists`. |
| `--subagents` | Show every subagent profile with spend or recorded effort in the selected window, with the agent columns and runs (distinct sessions) instead of sessions. With `--json`, also include profiles with work tokens and no spend. Conflicts with `--agents`, `--teams`, `--refresh`, and `--assists`. |
| `-n N` | With `--assists`, show at most N events (default 50); 0 means all retained. |
| `--all` | With `--assists`, include held and undelivered attempts. |

A conflicting pair exits `2`. Of the [global flags](../cli.md#global-flags), `--color` applies; `--root` and the backend flags are accepted and change nothing, because the figures are machine-wide.

## What the panel shows

History days use the configured `timezone`, or the system's named timezone. An unnamed system zone falls back to UTC; set `timezone` to an IANA name to select another zone.

The one-shot panel opens with dollars and tokens for Today, Week, Month, and Year. Today starts at local midnight; the other windows cover the last 7, 30, and 364 local calendar days, including today. Week and Month dollars include a trend against the preceding equal window when it had spend, capped above 999% as `↑999%+`; dollar figures keep their alignment whether a trend is present or not. The `per active day` row uses the last 7, 30, or 364 local calendar days, including today: dollars over those dates divided by their days with tokens. Today shows `–`.

Models, Providers, Agents, Teams, then Subagents follow, each labelled with the selected window, `(today)` by default. Every share is the row's dollars divided by fleet dollars in that window. Each section ranks by dollars, shows the top three rows with spend or recorded effort, and folds the rest into a counted tail such as `N other teams`. Empty sections are omitted. Token units are uppercase, and session and run counts use grouped thousands. Models use provider identity colours; profile and team names stay muted. Providers shows `SESSIONS TOKENS HIT TOK/S`; compact Agents adds `AGENT-TIME` after `SESSIONS`, Teams adds `AGENT-TIME MESSAGES`, and Subagents adds `AGENT-TIME` after `RUNS`. Explicit views keep the input/output/cache columns and append effort after the session or run count.

Providers includes all spend by provider kind, including historical and unobserved sessions. Agents groups solo sessions by profile; Teams sums each team's seats into one row; Subagents groups sessions launched by `rimz subagents` by profile, even when launched by a team. These are subsets of Providers. A bare-kind launch uses its provider kind as its profile key, merging with a profile of that name. Agents hides unless a profile not named for a provider has spend or recorded effort in the selected window; when visible, it includes the provider-named rows too. Teams and Subagents hide only when empty. Native provider children inherit their pane's role, not a separate subagent row.

`AGENT-TIME` is estimated time working, summed across sessions and parallel seats: `45s`, `41m`, `6h12m`, or `1d03h`. `MESSAGES` counts incoming human prompts and agent messages once on the receiving side, for example `10 (1 you)`, or `10` without human prompts. Zero effort cells read `-`. A row with only agent-time or messages still appears. A team's sent-message split stays in `roles[]` in JSON, not in panel rows.

For example, a compact Teams row:

```text
  Teams (today)             SPEND   SHARE  SESSIONS  AGENT-TIME     MESSAGES   TOKENS  HIT TOK/S
  recon                      $576   55.6%        91         45m    2 (1 you)     1.1B  97%    91
```

One muted coverage line sits above the first visible profile or team section. When the window begins before role recording, it says `roles recorded since DATE, earlier spend counts as No role`. When only effort history starts later, it says `agent-time and messages recorded since DATE`. When both apply, they join as `roles recorded since DATE, earlier spend counts as No role · agent-time and messages since DATE`. Each date is compared with the window's local-midnight start, and the line disappears when all three sections hide. Explicit `--agents`, `--teams`, and `--subagents` text views list every qualifying row, or say no spend was recorded in the named window; they keep the applicable coverage line even when empty.

After the role sections, the full report adds `No role recorded`: fleet dollars minus Agents, Teams and Subagents, clamped at zero, with its share of fleet spend. It is hidden below one cent and omitted from explicit role views.

The local-day heatmap follows the breakdowns. Its legend carries an active-day count and busiest day plus the full-history current streak. Week and Month scope the count and busiest day to their window; Today and Year count the recent 28 days and choose the busiest from cached history. The final `as of` time is the publication time in its published timezone, with a date when the publication is not from today. The full report appends the selected window's session count and average cost, such as `390 sessions today at $2.63 each`; the other window labels are `this week`, `this month`, and `this year`. This clause is omitted for zero sessions and explicit role views. A cold one-shot run does not print the wordmark; the held dashboard keeps it when space permits.

With no daily history or role usage, the full report replaces the tables and heatmap with `No agent sessions recorded on this machine yet.` and `Spend appears here after your first agent session; rimz stats reads it from each agent's own session files.`, followed by the publication time.

## How fresh the figures are

`rimz stats` reads the spending cache at `~/.rimz/cache/providers/provider-spending.json` (under `$RIMZ_HOME` when it is set). A running sidebar keeps that cache current, and each mode treats it differently:

| Run | Cache | What happens |
| --- | --- | --- |
| `rimz stats`, `--json`, `--agents`, `--teams`, `--subagents` | present and its figures cover today in its timezone | Prints the cache as it is. Nothing is walked or fetched. |
| `rimz stats`, `--json`, `--agents`, `--teams`, `--subagents` | missing, its figures cover another local day, or written by an incompatible RimZ version | Reads every agent transcript on the machine, writes the cache, then prints. When the panel goes to a terminal, a `Reading session files [bar] n/total` spinner reports progress. |
| `--refresh` | any | Requests fresh figures every 60 seconds through the same spending service the sidebar uses, which rereads transcripts that changed. |
| `--assists` | not read | Reads only the assist log. |

A one-shot run can print figures from earlier today. A walk that begins before midnight still covers the earlier day even if it finishes after midnight, so a one-shot run refreshes it again. Run `rimz stats --refresh`, or open a room, for updates within the day.

If a refresh cannot read session files and only an older publication remains, a one-shot run exits with an error rather than showing yesterday's breakdowns as today. The older publication stays intact, and a held dashboard can continue showing it.

Every run except `--assists` creates RimZ's shared state directories when they are missing. Beyond that, only the transcript walk writes files or uses the network. The walk writes the spending cache, its walk cursor, and the price-table cache beside it, unless another process is already writing them. It fetches the price tables from LiteLLM and models.dev when the cached table is more than a week old, or sooner when your history holds models it has no price for. Set `RIMZ_PRICING_OFFLINE=1` to skip every fetch (`0`, `false`, and an empty value leave fetching on); prices then come from the cached table and the table built into the binary. The walk and pricing are described in [spending internals](../../internals/agents/spending.md).

## The held dashboard

`rimz stats --refresh` takes over the pane on the terminal's alternate screen and repaints in place, leaving no scrollback. It is the default pane of the `rimzd` daemon view ([rimzd internals](../../internals/rimzd.md)).

| Key | Action |
| --- | --- |
| `Tab`, `Shift-Tab` | Select the next or previous window (Today, Week, Month, Year) for spend, activity and assists. |
| `r`, `R` | Restart the dashboard on the current binary, keeping its flags. `rimz reload` does the same to every running dashboard. |
| `q`, `Ctrl-C` | Quit. |

Other keys do nothing. In the daemon view both `q` and `Ctrl-C` are ignored; closing the pane ends the dashboard.

The dashboard starts on Today, or the tab selected by `--window`, with dollar totals in its tab rail. Below the wordmark are a stacked spend chart, the token heatmap, Models, Providers beside Where it went, Agents beside Subagents, Teams, and Assists. Today, Week and Month share a 30-day daily chart; Year shows the newest seven-day blocks that fit, up to 52, and names the count in its title. The chart's four largest models keep their provider-derived shades across tabs and the Models rows; the remainder is Other. Bars outside the selected window are muted, and the final `today` or `now` label is accented. The daily chart's dashed typical line averages the preceding 30 days, including zero-spend days. Today compares today's dollars with yesterday; Week and Month compare equal adjacent windows and show spend per active day; Year shows spend per active day. Averages and trend labels follow the one-shot rules above. The heatmap key adds the selected activity count and busiest day, plus the full-history current streak. Today and Year keep the recent 28-day activity count.

Held breakdowns rank by dollars and keep four named rows plus Other. Models and role sections require a 1% share of their own section's dollars to stay named; providers have no threshold. A tail containing just one entry keeps that entry's name. Models show HIT, TOK/S and TOKENS; Other's TOK/S is `–`. Providers show dollars, fleet share, hit rate and grouped session counts; a provider under 1% shows `<1%` and sessions without hit rate. Where it went splits dollars into Teams, Subagents, Agents and No role, clamping the unassigned remainder at zero. Agents, Subagents and Teams show shares of their own section, not of the fleet. Teams adds AGENT-TIME, MESSAGES and TOKENS, summing effort in Other. The role-recording date appears in Where it went when needed; the dashboard has no effort-recording note.

The frame is centred at up to 113 columns, with one blank line between sections and no bottom padding. Below 112 columns, side-by-side sections stack and bars shrink. Every tab uses the same fit step, chosen by measuring all four rendered frames: drop the wordmark; collapse Assists to one line; reduce the chart from 12 to 8 rows; drop Agents and Subagents; keep two named rows instead of four; reduce the chart to 6 rows; drop Teams; reduce the chart to 4 rows; drop the heatmap. Any remaining overflow is clipped, never wrapped. A refresh that fails leaves the last frame on screen with no marker. When the first refresh fails, the dashboard shows `Spending refresh unavailable - retrying.` with the cause and tries again after 5 seconds. Reload returns to the initial tab specified by the flags.

The Assists block omits held-attempt detail and zero-count categories. `Auto-compact` counts requests, including undelivered ones; other categories count effects. The header adds `(records since …)` when retained history starts after the selected window. Its collapsed form names the active tab's category count and points to `rimz stats --assists`. Both the dashboard and the [assist timeline header](#the-assist-timeline) group counts by thousands, including reset and agent details, and label Auto-continue's duration as `(after 5.1h parked)`: time before resuming, not time saved.

## JSON output

`rimz stats --json` prints one pretty-printed object describing the selected window (today by default). Breakdowns carry every model, provider, profile, and team with dollars or work tokens in that window, plus profile and team rows with recorded effort alone, with no `Other` folding and no row cap. To inspect a timed model over the year:

```sh
rimz stats --window year --json | jq 'first(.models[] | select(has("tokens_per_sec"))) | del(.tools)'
```

| Field | Contents |
| --- | --- |
| `as_of`, `timezone` | Publication time (RFC 3339 with offset) and the timezone used by the daily buckets. |
| `unit` | `tokens`, or `usd` under `--dollars`. The token and dollar fields below are present either way. |
| `window` | Selected window: `today`, `week`, `month`, or `year`. |
| `roles_since` | First retained role-record date, `YYYY-MM-DD` in the published timezone, covering solo, team, and subagent sessions. Omitted when no records exist. |
| `effort_since` | First retained active-time or message record date, independently of role history; omitted when none exists. |
| `sessions` | Sessions in the selected window. |
| `active_days_28` | Days with usage among the last 28. |
| `longest_streak`, `current_streak` | Consecutive days with usage. |
| `most_active_day` | `YYYY-MM-DD` of the heaviest day. Omitted when there is no usage. |
| `windows` | `today`, `week`, `month`, and `year`, each with `tokens` and `usd`, plus `tool_calls` and a `tools` map of calls by tool name, each omitted when empty. `today` is the local calendar day in the published timezone. |
| `models` | One object per model, sorted by `usd`: `model` id, display `name`, `tokens`, `input`, `output`, `cache_read`, `tool_calls`, `tools`, `cache_hit_pct`, `tokens_per_sec`, `usd`, and `share` of the total dollars as a fraction. |
| `providers` | One object per provider kind with selected-window dollars or work tokens, sorted by dollars: `kind`, display `name`, `tokens`, `usd`, `sessions`, `tool_calls`, `tools`, `cache_hit_pct`, optional `tokens_per_sec`, and `share` of selected-window fleet dollars as a fraction. |
| `agents` | Every solo profile with selected-window dollars, work tokens, or effort, sorted by dollars then profile: `profile`, `sessions`, `active_secs`, `input`, `output`, `cached`, `cache_hit_pct`, `tokens`, `usd`, and selected-window fleet dollar `share`. Provider-named rows are included even when the panel hides the section. |
| `teams` | Every team with selected-window dollars, work tokens, or effort, sorted by dollars then team: `team`, `sessions`, `active_secs`, `messages: {total, from_user}`, `input`, `output`, `cached`, `cache_hit_pct`, `tokens`, `usd`, `share`, and `roles`. Each role has `role` (a roleless layout cell, as in the built-in `peer` team, uses its profile, else its provider kind), `sessions`, `active_secs`, `from_user`, `from_teammates`, `to_teammates`, and the same spend fields and fleet-dollar `share`. Team totals sum their roles; incoming messages count once, not again when sent. |
| `days` | One object per local calendar day in the published timezone with usage, oldest first: `date`, `tokens`, `usd`. |
| `subagents` | Every profile with selected-window dollars, work tokens, or effort: `profile`, `runs`, `active_secs`, `input`, `output`, `cached`, `cache_hit_pct`, `tokens`, `usd`, and selected-window fleet dollar `share`. |
| `assists` | The selected-window effective-assist rollup only, without events. |

`tokens` is always input plus output plus cache-read tokens, and `input` includes cache writes. `tool_calls` and `tools` are omitted when zero or empty.

`tier_fallbacks` counts one per fresh tier move, whether it passed an unbuildable family, a spent account, or both. Replayed stamps, including a molt without `--agent`, are not counted again. Its benefit line names the first pass (`not buildable` or the usage reason); the event retains all usage `skipped` entries and optional `unbuildable` entries with their full build errors.

`providers`, `agents`, `teams`, and `subagents` are always arrays, including when empty. `cache_hit_pct` in profile and team rows is `null` when there is no input to measure; in model and provider rows it is omitted. `tokens_per_sec` is a rounded integer from the selected-window tally, omitted when the model or provider has no timed responses. Activity days, streaks, and the most-active day retain their full-history meaning regardless of `--window`.

An `auto_gc` event carries `scope` (`room` or `machine`); records written before scopes existed read as `machine`. Totals cover the whole sweep's scope. The durable assist record additionally holds own-room `class_bytes` and `worktree_paths` (the first 50 removed paths in sweep order, `[]` for legacy records), which this JSON projection does not expose. `worktrees_removed` remains the full count, so a count above the path-list length indicates truncation on new records ([GC report](./maintenance.md#the-report)).

A `tmp_evict` event counts one successful temp-unit eviction toward `tmp_evictions`, not `sweeps`, and its allocated bytes toward `reclaimed_bytes`; an error counts under `held.tmp_evictions`. In the timeline header, the `Temp evictions: N` category shows its own count and held attempts, separately from `Auto-gc:` sweeps; Auto-gc's byte detail totals both kinds. Its GC timeline row names the handle, reclaimed bytes, and workspace. JSON also carries optional `ended_at`, `free_bytes_after` (the measured filesystem-available space, which may lag removal), `floor_bytes`, and `error` on failure. Allocated bytes count hardlinked files once per tree; snapshots can retain those blocks after removal.

The `assists` object holds `judge_declines` (fresh judge polarity declines), `model_aliases` (catalog-derived model moves), `redeems` and `resets` (successful window-reset redemptions, and those whose outcome is `reset`), `manual_redeems` and `manual_resets` (the user-initiated subset, excluded from the `Auto-redeem:` text row), `resumes` and `recovered_secs` (delivered auto-continues, and the time spent parked before those resumes), `compacts`, `keepalives` (delivered prompt-cache pings), `idle_stops` (`stop --when-idle` requests that stopped their agent), `idle_stop_withdrawals` (requests withdrawn by a prompt from the user), `molts` (fresh successor tabs opened), `launch_retries` (relaunches of agents that died at startup), `restores` and `restored_sessions` (room restores, and the agents they brought back in a tab confirmed open), `sweeps` (completed automatic gc sweeps), `tmp_evictions` (successful temp-unit pressure evictions), `reclaimed_bytes` (their combined bytes: apparent for sweeps, allocated for temp units), and `workflow_starts`, `workflow_steps`, `workflow_routes`, `workflow_parks`, and `workflow_ends` (workflow runs a loop task started, step attempts launched, routes including closed merged waits, parks, and runs that reached `done`, `failed`, or `canceled`). `held`, present when any attempt did not take effect, maps each of those count names to its held attempts. Timeline events carry an `assist` type (`stall_notice`, `resident_launch`, `tier_fallback`, `hibernate`, `judge_decline`, `model_alias`, `workflow_start`, `workflow_step`, `workflow_route`, `workflow_park`, `workflow_end`, `auto_continue`, `auto_compact`, `idle_compact`, `cache_keepalive`, `idle_stop`, `idle_stop_withdrawn`, `molt`, `launch_retry`, `flip_compact`, `auto_redeem`, `auto_resume`, `auto_gc`, or `tmp_evict`), its `at` time in UTC, and the fields of that type. Model moves carry `kind`, `login`, `alias`, `from`, and `to`. A redemption carries the account name as `login` when its record has one, and its text row names the account when it is not `default`, as in `↻ codex@spare credit`. Manual redemptions keep the `auto_redeem` record type with `reason: "manual"`; their event text says `manual`, and they are not automation assists. What each assist is lives in [the assist log](../../internals/harness/loops.md#the-assist-log).

The `Judge declines: N` category counts fresh judge declines, not later fires held by the same decision. Each `judge_decline` event carries `task`, `checkout`, judge `profile` and `kind`, `reason`, and optional `cost_usd`. Its timeline line names the task, profile, reason, checkout, and recorded cost. Manual fire, a new true condition edge, or an elapsed recheck can produce a fresh decline ([resident judge memory](./loop.md#waits-and-checks)).

The `Keepalive: N` category counts delivered pings. Each `cache_keepalive` event carries `kind`, `agent_id`, optional `label`, `idle_secs`, `waits`, optional `horizon_secs`, `message_id`, `delivered`, optional `error`, and `capped` (true on the ping that told the agent the keepalive maximum was reached; its `◷` line ends with `, limit reached`). `horizon_secs` is the [keep-warm](../../guide/configuration.md#keep-warm) finite maximum that was holding the agent (`max`, or the keepalive cap for a `while`-only hold); its timeline line reads `holding warm (2h)`. `waits` counts all pending waits, including subagent and team waits. Failed attempts appear with `--all` without increasing the count.

The `Idle stop: N` category counts [`stop --when-idle`](./agents.md#stop) requests that stopped their agent. Each `idle_stop` event carries `kind`, `agent_id`, `label`, `idle_secs` (how long the agent had rested), `idle_after_secs` (the requested threshold), optional `requested_by` (the asking agent's handle), `stopped` (the agent's own pane closed), and optional `error`. A stop that left the pane open appears with `--all` with `stopped: false` and leaves its request armed. `error` can accompany `stopped: true`: the agent closed and something else failed, such as a child's pane staying open.

The `Idle stop withdrawn: N` category counts requests withdrawn by a prompt from the user. Its JSON rollup key is `idle_stop_withdrawals`. Each `idle_stop_withdrawn` event carries `kind`, `agent_id`, optional `label`, `idle_after_secs` (the withdrawn threshold), optional `requested_by`, and `requested_at`. These events count as recorded, never held; their `↶` line names the agent, threshold, and requester when an agent asked.

The `Launch retry: N` category counts relaunches of agents whose CLI died before opening a session, one per [relaunch](./agents.md#launch-a-layout). Each `launch_retry` event carries `kind`, `label` (the agent's handle), `run_id` (omitted for a launch without a supervised run), `attempt` (which relaunch of that launch, from 1), `exit_code` (omitted when a signal killed the process), `startup_ms` (how long the process that died lived), `relaunched`, and optional `error`. A relaunch that could not spawn appears with `--all` and `relaunched: false`.

The `Workflow: N` category sums the five [workflow](./workflow.md) assist kinds. Every workflow event carries `run` (the `wf_` id) and `workflow`. `workflow_start`, emitted only for loop-created runs, adds `task` (the loop task), `checkout`, and `prompt` (whether the run has one); its line reads `workflow babysit-pr started by loop babysit-pr`. `workflow_step` adds `step`, `visit`, `seq`, and the attempt's `handle` and `run_id` (both null for a command step); `workflow babysit-pr sweep launched (visit 2)`. `workflow_route` adds `node`, `outcome`, and `to` (the next node, or `@wait`, `@done`, `@failed`, `@user`); `workflow babysit-pr pr said needs-sweep → sweep`. A merged wait writes one route when it closes, with `count` set to its final poll count; its line reads `workflow babysit-pr pr waited ci-pending ×12`. An open wait contributes no route yet; other routes omit `count`, and older events without it still render as `said`. `workflow_park` adds `node`, `reason`, and optional `text`; `workflow babysit-pr triage parked: asked: Which fix ships first?`. `workflow_end` adds `state` (`done`, `failed`, or `canceled`); `workflow babysit-pr done`.

## The assist timeline

The `Molts: N` category counts agents moved into fresh sessions by `rimz agents molt`. The JSON rollup exposes `molts`; each `molt` event names the predecessor (`kind`, `agent_id`, `label`), the successor (`successor_kind`, `successor_id`, `successor_label`), and optional inherited `generation`. The successor id is its launch id. A molt counts once its new tab opens, even if the successor later fails or arming the predecessor's idle stop fails. Timeline lines show both handles; forensic lines retain both providers and ids.

By default, `rimz stats --assists` prints the latest 50 events that took effect, newest first, with the non-zero category counts over all retained records. Times use the configured timezone. Each line names the outcome and identifiers needed to trace it. `--all` includes held and undelivered attempts. When more remain, the trailer says `… N older; rimz stats --assists -n 0 for all`. Zero means all retained records across both log generations, not unlimited history.

The header counts each kind's attempts that did not take effect beside it, for example `Auto-compact: 2 (239 held)`. Delivery-based events count only when delivered, except a stall notice, which counts once queued for its parent and is held only on an error. A stall notice still counts if later withdrawn because its child run ended before delivery. Stops, freezes and relaunches use their recorded success flags. Redemption counts require a window reset; GC requires no error. Molts, model aliases, tier fallbacks, resident launches, judge declines, compact commands and room resumes count as recorded.

`rimz stats --assists --json` emits `{as_of, rollup, events, older}`, respecting `-n` and `--all`; its `as_of` is the read time, with the offset of the configured timezone. `--agents --json`, `--teams --json`, and `--subagents --json` emit `{as_of, window, agents|teams|subagents, roles_since?, effort_since?}` with only the selected array. The default stats JSON carries the assist rollup without events; every breakdown's `share` is a dollar share.

| Mark | Assist |
| --- | --- |
| `▶` | Resident launch: a scheduled task opened its agents. |
| `↘` | Tier fallback: the profile used the next available model. |
| `▶` | Auto-continue: a parked agent resumed, or `resume held` when delivery did not happen. |
| `◷` | Cache keepalive: idle duration, the keep-warm horizon when one was holding, and pending-wait count, followed by agent, message, and delivery verdict. |
| `■` | Idle stop: an agent stopped by its `--when-idle` request, with how long it rested, the threshold, and who asked; `idle stop failed` carries the error. |
| `↶` | Idle stop withdrawn: a prompt from the user kept the session, with the withdrawn threshold and who asked for the stop. |
| `⤳` | Molt: the predecessor moved into a fresh successor session. |
| `↺` | Launch retry: an agent whose CLI exited before its session opened, with the exit code, how long it lived, and the attempt number; `relaunch failed` carries the error. |
| `⌁` | Compaction: before a delivery (`auto-compact`), after an idle gap (`idle compacted`), or at a team stage flip (`flip compaction`). `held` means it was not delivered. |
| `↻` | Credit redemption for a provider account, with its outcome. |
| `⟲` | Room restore after a crash or reboot, with the channels it brought back. |
| `♻` | Automatic `rimz gc` sweep: `gc swept` for room scope or `machine gc swept` for machine scope, followed by what it reclaimed and removed and its cutoff; `gc failed` carries the error. |

With no records, the command prints `no assists recorded`. Use `--assists --json` for its structured form.

## See also

- [Token Insight guide](../../guide/insight.md): what each figure means, the sidebar's live view of the same data, and how tokens are priced.
- [Accounts CLI](./accounts.md): per-account plan limits and balances.
- [Budget CLI](./budget.md): caps that park agents when spend crosses a line.
- [Stats internals](../../internals/stats.md): load paths, heatmap shading, and terminal fitting.

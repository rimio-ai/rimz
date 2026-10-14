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
| `--refresh` | Open the [held dashboard](#the-held-dashboard): a full-screen panel that refreshes every 60 seconds and re-centres on resize. Conflicts with `--json`. |
| `--assists` | Print the [assist timeline](#the-assist-timeline). Conflicts with `--subagents` and `--refresh`; supports `--json`. |
| `--subagents` | Show every non-zero trailing-year subagent profile, with spend, fleet dollar share, input, output, cached tokens, hit rate, top-spend model, and runs (distinct sessions). Supports `--json`; conflicts with `--refresh`. |
| `-n N` | With `--assists`, show at most N events (default 50); 0 means all retained. |
| `--all` | With `--assists`, include held and undelivered attempts. |

A conflicting pair exits `2`. Of the [global flags](../cli.md#global-flags), `--color` applies; `--root` and the backend flags are accepted and change nothing, because the figures are machine-wide.

## What the panel shows

History days use the configured `timezone`, or the system's named timezone. An unnamed system zone falls back to UTC; set `timezone` to an IANA name to select another zone.

The one-shot panel opens with dollars and tokens for Today, Week, Month, and Year. Today starts at local midnight; the other windows trail 7, 30, and 365 days.

Models, Agents, and Subagents follow, each labelled `(year)`. Every share is the row's dollars divided by fleet dollars, including unattributed spend. Each section ranks by dollars, shows the top three non-zero rows, and folds the rest as `N other models`, `N other agents`, or `N other subagents`. Empty sections are omitted. Token units are uppercase.

Agents includes top-level sessions RimZ observed through reporting hooks. Subagents groups sessions launched by `rimz subagents` under their profile (or provider kind for bare-kind launches). Native provider children inherit their pane's role, not a separate subagent row. Unattributed is never folded: it includes older sessions and spend with no matching role record, not just sessions outside RimZ. Its provider breakdown lists every non-zero provider kind and its dollars, ranked by dollars and coloured by provider identity; these are not agent roles.

The local-day heatmap follows the breakdowns. The final `as of` time is the publication time in its published timezone, with a date when the publication is not from today. A cold one-shot run does not print the wordmark; the held dashboard keeps it when space permits.

## How fresh the figures are

`rimz stats` reads the spending cache at `~/.rimz/cache/providers/provider-spending.json` (under `$RIMZ_HOME` when it is set). A running sidebar keeps that cache current, and each mode treats it differently:

| Run | Cache | What happens |
| --- | --- | --- |
| `rimz stats`, `--json` | present | Prints the cache as it is, however old. Nothing is walked or fetched. |
| `rimz stats`, `--json` | missing, or written by an incompatible RimZ version | Reads every agent transcript on the machine, writes the cache, then prints. When the panel goes to a terminal, a `Reading session files [bar] n/total` spinner reports progress. |
| `--refresh` | any | Requests fresh figures every 60 seconds through the same spending service the sidebar uses, which rereads transcripts that changed. |
| `--assists` | not read | Reads only the assist log. |

A one-shot run outside a room can therefore print figures from the last time anything refreshed the cache. Run `rimz stats --refresh`, or open a room, to bring them up to date.

Every run except `--assists` creates RimZ's shared state directories when they are missing. Beyond that, only the transcript walk writes files or uses the network. The walk writes the spending cache, its walk cursor, and the price-table cache beside it, unless another process is already writing them. It fetches the price tables from LiteLLM and models.dev when the cached table is more than a week old, or sooner when your history holds models it has no price for. Set `RIMZ_PRICING_OFFLINE=1` to skip every fetch (`0`, `false`, and an empty value leave fetching on); prices then come from the cached table and the table built into the binary. The walk and pricing are described in [spending internals](../../internals/agents/spending.md).

## The held dashboard

`rimz stats --refresh` takes over the pane on the terminal's alternate screen and repaints in place, leaving no scrollback. It is the default pane of the `rimzd` daemon view ([rimzd internals](../../internals/rimzd.md)).

| Key | Action |
| --- | --- |
| `Tab`, `Shift-Tab` | Select the next or previous window (Week, Month, Year) for the breakdowns, insights, and assists. |
| `r`, `R` | Restart the dashboard on the current binary, keeping its flags. `rimz reload` does the same to every running dashboard. |
| `q`, `Ctrl-C` | Quit. |

Other keys do nothing. In the daemon view both `q` and `Ctrl-C` are ignored; closing the pane ends the dashboard.

Week and Month add a spend comparison with the preceding week or month when that period had spend. A refresh that fails leaves the last frame on screen with no marker. When the first refresh fails, the dashboard shows `Spending refresh unavailable - retrying.` with the cause and tries again after 5 seconds.

## JSON output

`rimz stats --json` prints one pretty-printed object describing the trailing-year window. Breakdowns carry every non-zero-spend entry, with no `Other` folding and no row cap. A timed model from a captured run, with its per-tool map removed:

```console
$ rimz stats --json | jq 'first(.models[] | select(has("tokens_per_sec"))) | del(.tools)'
{
  "model": "gpt-5.6-sol",
  "name": "GPT 5.6 Sol",
  "tokens": 25445145571,
  "input": 640431451,
  "output": 67362952,
  "cache_read": 24737351168,
  "tool_calls": 214967,
  "cache_hit_pct": 97,
  "tokens_per_sec": 41,
  "usd": 13857.190752600001,
  "share": 0.2677802690397501
}
```

| Field | Contents |
| --- | --- |
| `as_of`, `timezone` | Publication time (RFC 3339 with offset) and the timezone used by the daily buckets. |
| `unit` | `tokens`, or `usd` under `--dollars`. The token and dollar fields below are present either way. |
| `sessions` | Sessions in the trailing 365 days. |
| `active_days_28` | Days with usage among the last 28. |
| `longest_streak`, `current_streak` | Consecutive days with usage. |
| `most_active_day` | `YYYY-MM-DD` of the heaviest day. Omitted when there is no usage. |
| `windows` | `today`, `week`, `month`, and `year`, each with `tokens` and `usd`. `week`, `month`, and `year` add `tool_calls` and a `tools` map of calls by tool name, each omitted when empty; tool calls are not bucketed by day, so `today` never carries them. |
| `models` | One object per model, sorted by `usd`: `model` id, display `name`, `tokens`, `input`, `output`, `cache_read`, `tool_calls`, `tools`, `cache_hit_pct`, `tokens_per_sec`, `usd`, and `share` of the total dollars as a fraction. |
| `agents` | One object per top-level agent kind with non-zero spend, sorted by dollars: `kind`, display `name`, `tokens`, `usd`, `sessions`, `tool_calls`, `tools`, `cache_hit_pct`, and `share` of fleet dollars as a fraction. |
| `days` | One object per local calendar day in the published timezone with usage, oldest first: `date`, `tokens`, `usd`. |
| `subagents` | Every non-zero profile: `profile`, top-spend `model`, `runs`, `input`, `output`, `cached`, `cache_hit_pct`, `tokens`, `usd`, and fleet dollar `share`. |
| `unattributed` | `tally`: trailing-year tokens and dollars with no role record; `by_provider`: provider-kind-keyed trailing-year windows, omitting zero-dollar providers. Same shape in `--subagents --json`. |
| `assists` | The trailing-year effective-assist rollup only, without events. |

`tokens` is always input plus output plus cache-read tokens, and `input` includes cache writes. `tool_calls` and `tools` are omitted when zero or empty; `cache_hit_pct` is omitted when there is no input to measure.

`tokens_per_sec` is a rounded integer from the trailing-year tally, omitted when the model has no timed responses.

An `auto_gc` event carries `scope` (`room` or `machine`); records written before scopes existed read as `machine`. Totals cover the whole sweep's scope. The durable assist record additionally holds own-room `class_bytes`, which this JSON projection does not expose ([GC report](./maintenance.md#the-report)).

The `assists` object holds `judge_declines` (fresh judge polarity declines), `model_aliases` (catalog-derived model moves), `redeems` and `resets` (successful window-reset redemptions, and those whose outcome is `reset`), `manual_redeems` and `manual_resets` (the user-initiated subset, excluded from the `Auto-redeem:` text row), `resumes` and `recovered_secs` (delivered auto-continues, and the parked time they recovered), `compacts`, `keepalives` (delivered prompt-cache pings), `idle_stops` (`stop --when-idle` requests that stopped their agent), `launch_retries` (relaunches of agents that died at startup), `restores` and `restored_sessions` (room restores, and the agents they brought back in a tab confirmed open), and `sweeps` and `reclaimed_bytes` (completed automatic gc sweeps, and the bytes they freed). `held`, present when any attempt did not take effect, maps each of those count names to its held attempts. Timeline events carry an `assist` type (`stall_notice`, `resident_launch`, `tier_fallback`, `hibernate`, `judge_decline`, `model_alias`, `auto_continue`, `auto_compact`, `idle_compact`, `cache_keepalive`, `idle_stop`, `launch_retry`, `flip_compact`, `auto_redeem`, `auto_resume`, or `auto_gc`), its `at` time in UTC, and the fields of that type. Model moves carry `kind`, `login`, `alias`, `from`, and `to`. A redemption carries the account name as `login` when its record has one, and its text row names the account when it is not `default`, as in `↻ codex@spare credit`. Manual redemptions keep the `auto_redeem` record type with `reason: "manual"`; their event text says `manual`, and they are not automation assists. What each assist is lives in [the assist log](../../internals/harness/loops.md#the-assist-log).

The `Judge declines: N` category counts fresh judge declines, not later fires held by the same decision. Each `judge_decline` event carries `task`, `checkout`, judge `profile` and `kind`, `reason`, and optional `cost_usd`. Its timeline line names the task, profile, reason, checkout, and recorded cost. Manual fire, a new true condition edge, or an elapsed recheck can produce a fresh decline ([resident judge memory](./loop.md#waits-and-checks)).

The `Keepalive: N` category counts delivered pings. Each `cache_keepalive` event carries `kind`, `agent_id`, optional `label`, `idle_secs`, `waits`, optional `horizon_secs`, `message_id`, `delivered`, optional `error`, and `capped` (true on the ping that told the agent the keepalive maximum was reached; its `◷` line ends with `, limit reached`). `horizon_secs` is the [keep-warm](../../guide/configuration.md#keep-warm) horizon that was holding the agent; its timeline line reads `holding warm (2h)`. `waits` counts all pending waits, including subagent and team waits. Failed attempts appear with `--all` without increasing the count.

The `Idle stop: N` category counts [`stop --when-idle`](./agents.md#stop) requests that stopped their agent. Each `idle_stop` event carries `kind`, `agent_id`, `label`, `idle_secs` (how long the agent had rested), `idle_after_secs` (the requested threshold), optional `requested_by` (the asking agent's handle), `stopped` (the agent's own pane closed), and optional `error`. A stop that left the pane open appears with `--all` with `stopped: false` and leaves its request armed. `error` can accompany `stopped: true`: the agent closed and something else failed, such as a child's pane staying open.

The `Launch retry: N` category counts relaunches of agents whose CLI died before opening a session, one per [relaunch](./agents.md#launch-a-layout). Each `launch_retry` event carries `kind`, `label` (the agent's handle), `run_id` (omitted for a launch without a supervised run), `attempt` (which relaunch of that launch, from 1), `exit_code` (omitted when a signal killed the process), `startup_ms` (how long the process that died lived), `relaunched`, and optional `error`. A relaunch that could not spawn appears with `--all` and `relaunched: false`.

## The assist timeline

By default, `rimz stats --assists` prints the latest 50 events that took effect, newest first, with the non-zero category counts over all retained records. Times use the configured timezone. Each line names the outcome and identifiers needed to trace it. `--all` includes held and undelivered attempts. When more remain, the trailer says `… N older; rimz stats --assists -n 0 for all`. Zero means all retained records across both log generations, not unlimited history.

The header counts each kind's attempts that did not take effect beside it, for example `Auto-compact: 2 (239 held)`. Delivery-based events count only when delivered, except a stall notice, which counts once queued for its parent and is held only on an error; stops, freezes and relaunches use their recorded success flags. Redemption counts require a window reset; GC requires no error. Model aliases, tier fallbacks, resident launches, judge declines, compact commands and room resumes count as recorded.

`rimz stats --assists --json` emits `{as_of, rollup, events, older}`, respecting `-n` and `--all`; its `as_of` is the read time, with the offset of the configured timezone. `rimz stats --subagents --json` emits `{as_of, window: "year", subagents, unattributed}`. The default stats JSON carries the assist rollup without events; its `agents` rows are top-level agents and their `share` is a dollar share.

| Mark | Assist |
| --- | --- |
| `▶` | Resident launch: a scheduled task opened its agents. |
| `↘` | Tier fallback: the profile used the next available model. |
| `▶` | Auto-continue: a parked agent resumed, or `resume held` when delivery did not happen. |
| `◷` | Cache keepalive: idle duration, the keep-warm horizon when one was holding, and pending-wait count, followed by agent, message, and delivery verdict. |
| `■` | Idle stop: an agent stopped by its `--when-idle` request, with how long it rested, the threshold, and who asked; `idle stop failed` carries the error. |
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

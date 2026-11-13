# Stats CLI

`rimz stats` prints your token and dollar history across every project on the machine: spend and token totals, model and role breakdowns, and a heatmap of daily use. Its held view adds a stacked spend chart and assist counts; `--assists` shows the timeline. It touches no agent and no session, and it runs anywhere, in or out of a room or a project. What each figure means and how dollars are priced is the [Token Insight guide](../../guide/insight.md); how the panel is computed and fitted to the terminal is [stats internals](../../internals/stats.md).

```sh
rimz stats                              # print the panel once
rimz stats --dollars                    # shade the heatmap by dollars instead of tokens
rimz stats --refresh                    # hold the panel open and repaint every minute
rimz stats --json | jq '.windows.week'  # tokens and dollars over the last 7 local days
rimz stats --assists                    # latest 50 effective assist lines, repeats folded
rimz stats --assists -n 0 --all         # all retained attempts, including held ones
rimz stats --agents                     # every agent profile, unfolded
rimz stats --teams                      # every team, without per-role rows
rimz stats --subagents                  # every subagent profile, unfolded
rimz stats --rebuild                    # rebuild saved days from present transcripts
```

| Flag | Effect |
| --- | --- |
| `--dollars` | Shade the heatmap by dollars spent and title it `Spend activity` (default `Token activity`), in the one-shot panel and the held dashboard. Under `--json` it only sets `unit` to `usd`. `--agents`, `--teams`, `--subagents`, and `--assists` ignore it. |
| `--json` | Print the [stats document](#json-output) instead of the panel. Conflicts with `--refresh` and `--rebuild`. |
| `--window <WINDOW>` | Select `today` (default), `week`, `month`, or `year` for breakdowns, sessions and assists, and the held dashboard's initial tab. The Spend table always shows all four; the heatmap summary follows the drawn grid. Conflicts with `--assists` and `--rebuild`. |
| `--refresh` | Open the [held dashboard](#the-held-dashboard): a full-screen panel that refreshes every 60 seconds and re-centres on resize. Conflicts with `--json`, `--assists`, `--agents`, `--teams`, `--subagents`, and `--rebuild`. |
| `--assists` | Print the [assist timeline](#the-assist-timeline) over all retained records. Supports `--json`. Conflicts with `--window`, `--refresh`, `--agents`, `--teams`, `--subagents`, and `--rebuild`. |
| `--agents` | Show every solo agent profile with spend or recorded effort in the selected window, including provider-named rows, with spend, fleet dollar share, input, output, cached tokens, hit rate, distinct sessions, and agent-time. With `--json`, also include profiles with work tokens and no spend. Conflicts with `--teams`, `--subagents`, `--refresh`, `--assists`, and `--rebuild`. |
| `--teams` | Show every team with spend or recorded effort in the selected window, with the agent columns plus messages. Team roles appear only in JSON. With `--json`, also include teams with work tokens and no spend. Conflicts with `--agents`, `--subagents`, `--refresh`, `--assists`, and `--rebuild`. |
| `--subagents` | Show every subagent profile with spend or recorded effort in the selected window, with the agent columns. These dollars are already counted in their owners or the remainder. With `--json`, also include profiles with work tokens and no spend. Conflicts with `--agents`, `--teams`, `--refresh`, `--assists`, and `--rebuild`. |
| `-n <N>` | Show at most N timeline lines, or N events with `--json` (default 50); 0 means all retained. Requires `--assists`. |
| `--all` | Include held and undelivered attempts and steps that changed nothing. Requires `--assists`. |
| `--rebuild` | [Rebuild saved UTC days](#rebuild-saved-days) from present transcripts at current prices. Conflicts with every other flag above. |

A conflicting pair, or `-n` or `--all` without `--assists`, exits `2`. Of the [global flags](../cli.md#global-flags), `--color` applies; `--root` and the backend flags are accepted and change nothing, because the figures are machine-wide.

## What the panel shows

History days use the configured `timezone`, or the system's named timezone. An unnamed system zone falls back to UTC; set `timezone` to an IANA name to select another zone.

The held dashboard, one-shot panel, and explicit role text views round dollars to whole amounts with grouped thousands: `$0`, `<$1` for positive amounts below a dollar, then `$1`, `$9`, or `$12,527`. JSON keeps raw numbers. The `--rebuild` receipt and `--assists` timeline keep cents.

The one-shot panel opens with the Spend table: Today, 7 days, 30 days, and 52 weeks. Today starts at local midnight; the other windows cover the last 7, 30, and 364 local calendar days, including today. The rows are `dollars (vs prior)`, `tokens + cache reads`, and `per active day`. The 7-day and 30-day dollars include a trend against the preceding equal window when it had spend, capped above 999% as `↑999%+`; a change rounding to zero reads `±0%`. Dollar figures keep their alignment whether a trend is present or not. Spend per active day divides each window's dollars by its days with tokens, including Today.

Models, Providers, Agents, Teams, then Subagents follow, each labelled `(today)`, `(7 days)`, `(30 days)`, or `(52 weeks)` for the selected window. Every share is the row's dollars divided by fleet dollars in that window. Each section ranks by dollars, shows the top three rows with spend or recorded effort, and folds the rest into a counted tail such as `N other teams`. Empty sections are omitted. Token units are uppercase, and session counts use grouped thousands. Models use provider identity colours, and a model with no recorded id reads `Unknown model`; profile and team names stay muted. Every section starts with `SPEND SHARE`. Models adds `INPUT OUTPUT CACHED HIT`. Providers adds `SESSIONS TOKENS HIT TOK/S`; compact Agents and Subagents add `AGENT-TIME` after `SESSIONS`, and Teams adds `AGENT-TIME MSGS (YOU)`. Explicit role views show `INPUT OUTPUT CACHED HIT SESSIONS AGENT-TIME`, plus `MSGS (YOU)` for Teams.

Providers includes all spend by provider kind, including historical and unobserved sessions. Agents groups solo seats by profile; Teams sums each team's seats into one row. Both include the dollars, tokens, and active time of subagents those seats launched, when their owner was recorded. Their sessions and messages count only the seats' own work. Subagents groups sessions launched by `rimz subagents` by profile as an already-counted cross-cut, not another owner. Native provider children inherit their pane's role. A bare-kind launch uses its provider kind as its profile key, merging with a profile of that name. Agents, Teams, and Subagents hide only when empty, including when Agents has only provider-named rows.

Claude sessions in one project count separately; a native subagent's work counts toward its parent session.

`AGENT-TIME` is estimated time working, summed across sessions and parallel seats: `45s`, `41m`, `6h12m`, or `1d03h`. `MSGS (YOU)` counts incoming human prompts and agent messages once on the receiving side, for example `10 (1)`, or `10` without human prompts. Zero effort cells read `-`. A row with only agent-time or messages still appears. A team's sent-message split stays in `roles[]` in JSON, not in panel rows.

For example, a compact Teams row:

```text
  Teams (today)             SPEND   SHARE  SESSIONS  AGENT-TIME   MSGS (YOU)   TOKENS  HIT TOK/S
  recon                      $576   55.6%        91         45m        2 (1)     1.1B  97%    91
```

Muted coverage text sits above the first visible profile or team section. When the window begins before role recording, it says `roles recorded since DATE, earlier spend counts as No owner recorded`. When only effort history starts later, it says `agent-time and messages recorded since DATE`. When both apply, they join with a middle dot, shortening the effort clause to `agent-time and messages since DATE` and wrapping to the Spend table's width. Each date is compared with the window's local-midnight start, and the text disappears when all three sections hide. Explicit `--agents`, `--teams`, and `--subagents` text views list every qualifying row, or print `No agent spend recorded (today).` with the view's noun and window (`last 7 days`, `last 30 days`, `last 52 weeks`); they keep the applicable coverage text even when empty.

Between Teams and Subagents, the full report adds `No owner recorded`: fleet dollars minus Agents and Teams, clamped at zero, with its share of fleet spend. It hides below one cent. A note beneath it gives the subagent spend without a recorded owner, when that amount reaches one cent. A muted note then states that Models, Providers, and Agents + Teams + No owner recorded each sum to fleet dollars. Subagents follows with an already-counted note, which explicit `--subagents` keeps. Explicit role views omit the remainder and the sum note.

The local-day heatmap follows the breakdowns in the full report; explicit role views have none. Its legend reads `N active days since Mon D · streak N days · busiest Mon D`, counting and ranking only the drawn grid while keeping the full-history current streak, regardless of the selected window. A date in an earlier calendar year adds the year as `Mon D YYYY`. The summary sits below the key, wrapping to the Spend table's width. The final `as of` time is the publication time in its published timezone, with a date when the publication is not from today. The full report appends the selected window's session count and average cost, such as `390 sessions today at $3 each`; the other window labels are `in the last 7 days`, `in the last 30 days`, and `in the last 52 weeks`. This clause is omitted for zero sessions and explicit role views. The one-shot panel never prints the wordmark; the held dashboard keeps it when space permits.

With no daily history or role usage, the full report replaces the tables and heatmap with `No agent sessions recorded on this machine yet.` and `Spend appears here after your first agent session; rimz stats reads it from each agent's own session files.`, followed by the publication time.

## How fresh the figures are

`rimz stats` reads the spending cache at `~/.rimz/cache/providers/provider-spending.json` (under `$RIMZ_HOME` when it is set). A running sidebar keeps that cache current, and each mode treats it differently:

| Run | Cache | What happens |
| --- | --- | --- |
| `rimz stats`, `--json`, `--agents`, `--teams`, `--subagents` | present and its figures cover today in its timezone | Prints the cache as it is. Nothing is walked or fetched. |
| `rimz stats`, `--json`, `--agents`, `--teams`, `--subagents` | missing, its figures cover another local day, or written by an incompatible RimZ version | Reads every agent transcript on the machine, writes the cache, then prints. When stdout and stderr are both terminals and the run is not `--json`, a `Reading session files [bar] n/total` spinner reports progress. |
| `--refresh` | any | Requests fresh figures every 60 seconds through the same spending service the sidebar uses, which rereads transcripts that changed. |
| `--assists` | not read | Reads only the assist log. |
| `--rebuild` | not read | Rereads transcripts for saved days ([Rebuild saved days](#rebuild-saved-days)). |

A one-shot run can print figures from earlier today. A walk that begins before midnight still covers the earlier day even if it finishes after midnight, so a one-shot run refreshes it again. Run `rimz stats --refresh`, or open a room, for updates within the day.

Recorded UTC days, closed fourteen days after their end, replace live account-days in these figures. They survive transcript deletion and cache rebuilds under `accounts/.history/`, shared with other homes using the same accounts folder and kept by uninstall and reset. The newest decodable line for an account-day is effective; `--rebuild` appends replacements without removing the preceding figures ([history rules](../../internals/agents/spending.md#closed-day-spend-lines)).

If a refresh cannot read session files and only an older publication remains, a one-shot run exits `1` with `Spending refresh unavailable: could not read today's figures from session files; check their availability and retry` rather than showing yesterday's breakdowns as today. The older publication stays intact, and a held dashboard can continue showing it.

Every run except `--assists` creates RimZ's shared state directories when they are missing. Beyond that, only the transcript walk writes files or uses the network. The walk writes the spending cache, its walk cursor, and the price-table cache beside it, unless another process is already writing them. It also migrates and maintains shared history, records newly closed days, and may start a background rebuild when restored or synced usage grows on a saved day. Automatic rebuilds leave a `spend_rebuild` assist with the changed days and before/after dollars. It fetches the price tables from LiteLLM and models.dev when the cached table is more than a week old, or sooner when your history holds models it has no price for. Set `RIMZ_PRICING_OFFLINE=1` to skip every fetch (`0`, `false`, and an empty value leave fetching on); prices then come only from the saved table, or from the built-in fresh-install seed when no usable table exists. The walk and pricing are described in [spending internals](../../internals/agents/spending.md).

## Rebuild saved days

`rimz stats --rebuild` rebuilds every saved UTC day in retention for discovered accounts from present transcripts, using current prices. A partially deleted day follows its remaining transcripts and may fall; a day with no transcripts keeps its saved figure. Transcript-logged costs stay as logged rather than taking a new list price. A requested rebuild writes no assist; automatic replacements write a `spend_rebuild` assist.

Progress goes to stderr. On a terminal, a `Rebuilding saved spend history` spinner turns into a `Reading session files [bar] n/total` line. When stderr is not a terminal, that line prints once at the start and once at the end. Stdout then prints one receipt line:

| Receipt | When |
| --- | --- |
| `Rebuilt N UTC days: $X → $Y; N account-days unchanged.` | At least one account-day changed. Dollars keep cents. |
| `No saved day changed; N account-days unchanged.` | Every compared account-day matched. |
| `Nothing to rebuild.` | No saved days exist. |

Stderr adds one warning per account in two cases, each naming its UTC dates with consecutive days as `start..end` ranges:

- `warning: <login> <dates> (N UTC days): no transcripts were found; the saved figures were kept.`
- `warning: could not read <login> transcripts; <dates> (N UTC days) stay pending.`

| Exit | When |
| --- | --- |
| `0` | Every requested account-day was compared, including unchanged or empty history and days kept because no transcripts remain. |
| `1` | An account had an unread transcript: stdout keeps its receipt and stderr prints the `stay pending` warning. |
| `1` | `transcript discovery was incomplete; saved spend history was not rebuilt`, before parsing or publishing. |
| `1` | The spending producer lock or the shared history maintenance lock is busy or unavailable. The message ends `retry rimz stats --rebuild`. |
| `1` | `spending history format mark is unusable: <path>`: the mark is newer, malformed, or unreadable. A missing mark is usable. |
| `1` | A history file could not be written. |

## The held dashboard

`rimz stats --refresh` takes over the pane on the terminal's alternate screen and repaints in place, leaving no scrollback. It is the default pane of the `rimzd` daemon view ([rimzd internals](../../internals/rimzd.md)).

| Key | Action |
| --- | --- |
| `←`, `→`, `h`, `l` | Move one range, stopping at Today and 52 weeks. |
| `t`, `w`, `m`, `y` (either case) | Jump to Today, 7 days, 30 days or 52 weeks. |
| `Tab`, `Shift-Tab` | Cycle the ranges with wrap. Spend breakdowns and assists follow the selected range. |
| Left click on a tab | Select that range. |
| `r`, `R` | Restart the dashboard on the current binary, keeping its flags. `rimz reload` does the same to every running dashboard. |
| `q`, `Ctrl-C` | Quit. |

Other keys do nothing. Clicks select only a tab's visible text, including its total and active brackets, not the separators. Wheel, release and drag events do nothing; hold Shift while dragging to select terminal text. The final line reads `←/→ range · t/w/m/y jump · r reload · q quit`. In the daemon view both `q` and `Ctrl-C` are ignored and the hint omits `q quit`; closing the pane ends the dashboard.

The dashboard starts on Today, or the tab selected by `--window`, with dollar totals in its tab rail. When the full rail does not fit, inactive tabs show only their labels and the active tab keeps its total. Below the wordmark are the heatmap (shaded by dollars under `--dollars`), tab rail, stacked spend chart, Models, Providers beside Where it went, Teams (when the window has team spend), Agents beside Subagents, Assists, and key hint. With no daily history or role usage, the two empty-state lines of the one-shot report replace everything between the wordmark and Assists. Section titles use normal foreground in bold. Chart and heatmap titles have one empty line beneath them. Today draws 24 local-hour bars; 7 days draws seven daily bars; 30 days draws thirty; 52 weeks draws the newest seven-day blocks that fit, up to 52, naming the count in its title. Each total covers its bars' days, except that below 60 columns the 52-week view keeps its total while drawing fewer weeks. The chart uses the Models table's named set, order and swatches, with the remainder in Other and no separate legend. Readable scheme hues nearest the provider brand stay consistent across tabs, but can move with the 30-day spending rank on refresh. Tops use eighth blocks, and even tiny positive spend stays visible. The 7-day, 30-day and 52-week views' dashed average line is the mean of the drawn bars, including today's partial bar (the newest week in the 52-week view) and missing bars as zero, without the forecast extension. Its compact dollar label sits on the y-axis with a faint `┼`, replacing any tick on that row. There is no line-end caption or average in a title. The 7-day and 30-day titles show only the trend against the prior equal window when it had spend. Ticks use round whole-dollar values in compact form (`$750`, `$1.5k`, `$10k`), placed on the nearest plot row; the scale ceiling and bar heights are unchanged. The label field is five columns, and `$0` stays on the axis row. No spend means no line or average label. Every tab has the same grid-scoped heatmap footer as the one-shot report, with unpadded dates and the year added for earlier calendar years.

At full width, Today's header shows `today $X so far · ~$Y by midnight · █ yesterday $Z`. When the title line overflows, it first drops `today $X so far · `, already shown in the active tab. On every tab, the left chart title then shortens to preserve comparison figures whenever they fit alone. With colour on, yesterday's same-hour dollars appear as solid `█` cells above the cell holding today's bar top, including future hours, in muted gray faded toward the background to match the header swatch. When that comparison reaches above today's bar, the same tone fills the top spend cell's background, closing the gap without changing or covering its spend glyph or model tone. The ghost height rounds to whole cells; the background fill can make the comparison appear up to one cell taller. The forecast is spend so far plus the previous seven local days' mean spend in hours after the spend snapshot's recorded local hour, counting missing days as zero and never falling below spend so far. It excludes the rest of that hour rather than extrapolating today's pace. The bar holding today in the 7-day, 30-day and 52-week views gets an extension in its model colour faded toward the background; Today shows the forecast only in its header. Where spend shares a cell with an extension above it, the spend glyph keeps its model tone and the forecast uses the background, reaching the cell top. With colour off (`NO_COLOR`, `CLICOLOR=0`, or `--color never`), the extension neither draws nor contributes to the ceiling, but yesterday's ghost and the header figures stay: the ghost and its swatch use `░`, with no background fill. Axis dates stay separated; Today's final `23` is muted, while `today` and `now` on the other tabs stay accented. The final label starts under the newest bar when it fits there, otherwise ending at the plot edge. These forecast and comparison marks are held-view features, not one-shot or JSON output.

Held breakdowns rank by dollars and keep up to seven named Models, four Agents, Subagents and Teams, and three Providers, each plus Other. Rows below 1% of their own section's dollars fold. A tail containing just one entry keeps its name even below that threshold or past the named-row limit. Models show HIT, TOK/S and TOKENS, hiding TOK/S when every named model row is untimed; Other's always-`–` rate does not keep it open. Providers show dollars, hit rate, grouped session counts and then the share of the section's sessions; shares that round below 1% are omitted. The Providers stacked bar also divides by sessions, while hit rate appears only at a fleet-dollar share of at least 1%. Where it went prints Teams, Agents and No owner on one line each, with fleet-dollar percentages and the ownerless remainder clamped at zero. Teams and Agents use accent and meta; No owner is faint. Profile and team markers and filled bars use their group's tone (cool for Subagents), with normal foreground names, and Other rows stay muted throughout. Models show fleet-dollar percentages; Agents, Subagents and Teams show shares of their own section. Their row bars fill relative to the largest drawn row in each section, including Other, with blank space rather than a track. Subagents remains an already-counted cross-cut but has no note in the held view. Teams adds AGENT-TIME, MSGS and TOKENS, summing effort in Other. MSGS is the total incoming count, or `-` at zero; message cells keep two-cell gutters even with wide counts. Unit letters in tokens, agent-time and Assists byte sizes and parked durations are muted without muting digits or count nouns. The role-recording date appears in Where it went when needed; the dashboard has no effort-recording note.

The frame is centred horizontally at up to 113 columns and vertically using the tallest tab at its chosen fit step, so switching tabs does not shift it. With no terminal row count there is no vertical padding. Sections have one blank separator and no bottom padding. Below 112 columns, side-by-side sections stack and bars shrink; a heatmap footer too wide beside its key moves below it and wraps within the frame. Every tab uses the same fit step, chosen by measuring all four rendered frames: drop the wordmark; collapse Assists to one line; drop the heatmap; drop Agents and Subagents; keep two named Models and Teams; drop Teams; reduce the seven-row plot to four rows, then two. Remaining overflow clips the body, preserving the final key hint. A refresh that fails leaves the last frame on screen with no marker. When the first refresh fails, the dashboard shows `Spending refresh unavailable - retrying.` with the cause and tries again after 5 seconds. Reload returns to the initial tab specified by the flags.

The Assists block counts the active tab's records and hides when it has none. It omits held-attempt detail and zero-count categories. `Auto-compact` counts requests, including undelivered ones; other categories count effects. The header adds `(records since …)` when retained history starts after the selected window. Its collapsed form reads `Assists  N kinds · rimz stats --assists for the timeline`. Both the dashboard and the [assist timeline header](#the-assist-timeline) group counts by thousands, including reset and agent details, and label Auto-continue's duration as `(after 5.1h parked)`: time before resuming, not time saved.

## JSON output

`rimz stats --json` prints one pretty-printed object describing the selected window (today by default). Breakdowns carry every model, provider, profile, and team with dollars or work tokens in that window, plus profile and team rows with recorded effort alone, with no `Other` folding and no row cap. To inspect a timed model over the year:

```sh
rimz stats --window year --json | jq 'first(.models[] | select(has("tokens_per_sec"))) | del(.tools)'
```

The fields, in the order they print:

| Field | Contents |
| --- | --- |
| `as_of` | Publication time, RFC 3339 with the published timezone's offset. |
| `window` | Selected window: `today`, `week`, `month`, or `year`. |
| `roles_since` | First retained role-record date, `YYYY-MM-DD` in the published timezone, covering solo, team, and subagent sessions. Omitted when no records exist. |
| `effort_since` | First retained active-time or message record date, independently of role history; omitted when none exists. |
| `timezone` | The timezone used by the daily buckets. |
| `unit` | `tokens`, or `usd` under `--dollars`. The token and dollar fields below are present either way. |
| `sessions` | Sessions in the selected window. |
| `active_days_28` | Days with usage among the last 28. |
| `longest_streak`, `current_streak` | Consecutive days with usage. |
| `most_active_day` | `YYYY-MM-DD` of the heaviest day. Omitted when there is no usage. |
| `windows` | `today`, `week`, `month`, and `year`, each with `tokens` and `usd`, plus `tool_calls` and a `tools` map of calls by tool name. `today` is the local calendar day in the published timezone. |
| `models` | One object per model, sorted by `usd` then tokens: `model` id, display `name` (`Other` for an empty id), `tokens`, `input`, `output`, `cache_read`, `tool_calls`, `tools`, `cache_hit_pct`, `tokens_per_sec`, `usd`, and `share` of selected-window fleet dollars as a fraction. |
| `providers` | One object per provider kind with selected-window dollars or work tokens, sorted by dollars then tokens: `kind`, display `name`, `tokens`, `usd`, `sessions`, `tool_calls`, `tools`, `cache_hit_pct`, `tokens_per_sec`, and `share` of selected-window fleet dollars as a fraction. |
| `agents` | Every solo profile with selected-window dollars, work tokens, or effort, sorted by dollars then profile: `profile`, `sessions`, `active_secs`, `input`, `output`, `cached`, `cache_hit_pct`, `tokens`, `usd`, and selected-window fleet dollar `share`. Provider-named rows are included. Spend, tokens, and active time include owned subagents; sessions do not. |
| `teams` | Every team with selected-window dollars, work tokens, or effort, sorted by dollars then team: `team`, `sessions`, `active_secs`, `messages: {total, from_user}`, `input`, `output`, `cached`, `cache_hit_pct`, `tokens`, `usd`, `share`, and `roles`. Each role has `role` (a roleless layout cell, as in the built-in `peer` team, uses its profile, else its provider kind), `sessions`, `active_secs`, `from_user`, `from_teammates`, `to_teammates`, and the same spend fields and fleet-dollar `share`, sorted by dollars then role. Team totals sum their roles; incoming messages count once, not again when sent. |
| `subagents` | Every profile with selected-window dollars, work tokens, or effort, with the `agents` fields and order. An already-counted cross-cut. |
| `no_owner` | Selected-window `usd` outside Agents and Teams, its fleet-dollar `share`, and `subagent_usd` with no recorded owner. |
| `days` | One object per local calendar day in the published timezone with usage, oldest first: `date`, `tokens`, `usd`. |
| `assists` | The selected-window [assist rollup](#assist-rollup), without events. |

`tokens` is always input plus output plus cache-read tokens, and `input` includes cache writes. `tool_calls` and `tools` are omitted when zero or empty.

`providers`, `agents`, `teams`, and `subagents` are always arrays, including when empty. `cache_hit_pct` in profile, team, and role rows is `null` when there is no input to measure; in model and provider rows it is omitted. `tokens_per_sec` is a rounded integer from the selected-window tally, omitted when the model or provider has no timed responses. Activity days, streaks, and the most-active day retain their full-history meaning regardless of `--window`. Every breakdown's `share` is a dollar share.

`--agents --json`, `--teams --json`, and `--subagents --json` emit `{as_of, window, agents|teams|subagents, roles_since?, effort_since?}` with only the selected array, in the shape above, and no `no_owner`.

## The assist timeline

By default, `rimz stats --assists` prints the latest 50 lines of assists that took effect, newest first, under a header of the non-zero category counts over all retained records: `assists (all retained) — Auto-compact: 2 (239 held) · Molts: 1`. Times use the configured timezone. Each line reads `YYYY-MM-DD HH:MM`, the outcome, then ` · `-separated identifiers such as the agent id, message id, workspace, or run. With no records, the command prints `no assists recorded`.

Repeated outcomes for the same subject and assist kind fold within that subject's history of that kind, even when other subjects or kinds interleave. A changed outcome or policy key, or a different `took over from` list, breaks the repeats; different agents and targets stay apart. A repeated line reads `YYYY-MM-DD HH:MM outcome · ×N over span · identifiers` (`over span` omitted under one second), using its newest member's time, reason, readings, and identifiers. Reclaimed bytes, GC removal and problem counts, judge cost, and recovered parked time are summed; idle, silence, and startup durations become ranges. These measured readings round down to whole minutes from one minute up and to whole hours from one day up (`59m`, `1h06m`, `1d02h`), while thresholds, the keep-warm horizon, and the GC cutoff print as configured. Repeated resumes omit the individual clock-time span. A workflow route with its own poll count never folds again, and neither does a spend rebuild, since each names its own days and dollars. Each folded member's own record is in `--assists --json`.

`-n` counts lines after folding, so each line's `×N` includes every matching record, even beyond the limit. `--all` includes held and undelivered attempts and steps that changed nothing. When more remain, the trailer says `… N older assists; rimz stats --assists -n 0 for all` (`1 older assist` in the singular), where `older` counts records outside the printed lines. The printed repeat counts plus `older` equal the filtered record count. Zero shows all retained records across both log generations, not unlimited history.

The header counts each kind's held or failed attempts beside it, with or without `--all`. Delivery-based events count only when delivered, except stall notices and stall nudges, which count once queued and are held only on an error. A stall notice still counts if later withdrawn because its child run ended before delivery. Stops and relaunches use their recorded success flags. Hibernation counts freezes regardless of reclaimed bytes; a deepening step takes effect only when it reclaims more than 0 bytes and adds those bytes to the total. A 0-byte deepening step is neither counted nor held, and appears only with `--all`. Redemption counts require a window reset; GC, temp evictions, and port forwards require no error. Molts, model aliases, tier fallbacks, empty-dir grants, untrusted-cwd fallbacks, resident launches, judge declines, compact commands, room resumes, idle-stop withdrawals, workflow events, and spend rebuilds count as recorded.

The header categories, in the order they print:

| Category | Rollup key | Detail in parentheses |
| --- | --- | --- |
| `Stall notice:` | `stall_notices` | |
| `Stall nudge:` | `stall_nudges` | |
| `Untrusted cwd:` | `untrusted_cwd_fallbacks` | |
| `Empty-dir trust:` | `empty_dir_grants` | |
| `Workflow:` | sum of the five `workflow_*` keys | |
| `Resident launches:` | `resident_launches` | |
| `Judge declines:` | `judge_declines` | |
| `Model aliases:` | `model_aliases` | |
| `Tier fallback:` | `tier_fallbacks` | |
| `Port forwards:` | `port_forwards` | |
| `Auto-continue:` | `resumes` | `after 5.1h parked` |
| `Auto-compact:` | `compacts` | |
| `Soft compact:` | `soft_compacts` | |
| `Keepalive:` | `keepalives` | |
| `Idle stop:` | `idle_stops` | |
| `Idle stop withdrawn:` | `idle_stop_withdrawals` | |
| `Molts:` | `molts` | |
| `Hibernate:` | `hibernations` | `1.2 GB reclaimed` |
| `Launch retry:` | `launch_retries` | |
| `Auto-redeem:` | `redeems` minus `manual_redeems` | automatic `N resets` |
| `Auto-resume:` | `restores` | `N agents` |
| `Auto-gc:` | `sweeps` | `reclaimed_bytes`, sweeps and temp evictions together |
| `Spend rebuild:` | `spend_rebuilds` | |
| `Temp evictions:` | `tmp_evictions` | |

In the timeline header, `N held` joins the detail. A line's mark names its kind:

| Mark | Assist |
| --- | --- |
| `▶` | Resident launch: an older record of a scheduled task opening its agents; loop-started runs record `workflow_start` once they start an agent, a judge or a message, and a [quiet fire](./loop.md#quiet-fires) records none. |
| `↘` | Tier fallback: the profile used the next available model. |
| `▶` | Auto-continue: a parked agent resumed, or `resume held` when delivery did not happen. |
| `◷` | Cache keepalive (`forced cache keepalive` for the ping sent past the typing hold): idle duration (a range for repeats), the keep-warm horizon when one was holding, and newest pending-wait count, followed by agent, message, and delivery verdict. |
| `■` | Idle stop: an agent stopped by its `--when-idle` request, with how long it rested, the threshold, and who asked; `idle stop failed` carries the error. |
| `↶` | Idle stop withdrawn: a prompt from the user kept the session, with the withdrawn threshold and who asked for the stop. |
| `⤳` | Molt: the predecessor moved into a fresh successor session. |
| `↺` | Launch retry: an agent whose CLI exited before its session opened, with the exit code, how long it lived, and the attempt number; `relaunch failed` carries the error. |
| `⌁` | Compaction: before a delivery (`auto-compact`), after an idle gap (`idle compacted`, with a duration range for repeats), or at a team stage flip (`flip compaction`). `held` means it was not delivered. |
| `⌁` | Soft compaction: a mid-turn wrap-up steer (`soft compact`), or `soft compact held` when it missed. The subsequent compact command is a separate `auto-compact` event. |
| `↻` | Credit redemption for a provider account, with its rule and outcome. |
| `⟲` | Room restore after a crash or reboot (`rebirth recovery`), with the agents it brought back. |
| `♻` | Automatic `rimz gc` sweep: `gc swept` for room scope or `machine gc swept` for machine scope, followed by what it reclaimed and removed, then its workspace and cutoff; `gc failed` carries the error. Temp evictions name the handle, reclaimed bytes, and workspace. `spend rebuilt` names rebuilt UTC days and before/after dollars, then parsed files, pending dates, and kept account-days. |

Stall notices and nudges, workflow events, model aliases, empty-dir grants, untrusted-cwd fallbacks, port forwards, judge declines, and hibernation lines carry no mark.

### Assist JSON

`rimz stats --assists --json` emits `{as_of, rollup, events, older}`, respecting `-n` and `--all`, with `-n` and `older` counting records rather than lines and one event per record. Its `as_of` is the read time, with the offset of the configured timezone. The `rollup` covers all retained records; the default stats JSON carries the same rollup for the selected window, without events.

#### Assist rollup

Counts are of assists that took effect, in the order they print. The five `workflow_*` keys are omitted when zero; every other count is always present.

| Key | Counts |
| --- | --- |
| `workflow_starts`, `workflow_steps`, `workflow_routes`, `workflow_parks`, `workflow_ends` | Workflow runs a loop task started, step attempts launched, routes including closed merged waits, parks, and runs that reached `done`, `failed`, or `canceled`. |
| `port_forwards` | Automatic forwards opened. |
| `empty_dir_grants` | Automatic empty-root folder-trust writes. |
| `untrusted_cwd_fallbacks` | Explicit undecided-cwd children started privately. |
| `molts` | Fresh successor tabs opened. |
| `stall_notices` | Stall notices queued for a parent without an error. |
| `stall_nudges` | Once-per-stage-visit team notices queued without an error, even when immediate delivery misses. |
| `resident_launches` | Older records of scheduled tasks that opened their agents; loop-started runs count under `workflow_starts`. |
| `judge_declines` | Fresh judge polarity declines. |
| `model_aliases` | Catalog-derived model moves. |
| `tier_fallbacks` | Fresh tier moves. |
| `redeems`, `resets` | Successful window-reset redemptions, and those whose outcome is `reset`. |
| `manual_redeems`, `manual_resets` | The user-initiated subset, excluded from the `Auto-redeem:` text row. |
| `resumes`, `recovered_secs` | Delivered auto-continues, and the time spent parked before those resumes. |
| `compacts` | Delivered `auto_compact`, `idle_compact`, and `flip_compact` events. |
| `soft_compacts` | Delivered mid-turn wrap-up steers. |
| `keepalives` | Delivered prompt-cache pings. |
| `idle_stops` | `stop --when-idle` requests that stopped their agent. |
| `idle_stop_withdrawals` | Requests withdrawn by a prompt from the user. |
| `hibernations`, `hibernate_reclaimed_bytes` | Freezes, and the bytes reclaimed by freezes and deepening steps. |
| `hibernate_holds` | Map from `kind/agent_id` to the newest hold reason recorded for that agent, held or not; `{}` when none. |
| `launch_retries` | Relaunches of agents that died at startup. |
| `restores`, `restored_sessions` | Room restores, and the agents they brought back in a tab confirmed open. |
| `sweeps` | Completed automatic gc sweeps. |
| `spend_rebuilds` | Automatic saved-day rebuilds that appended replacements. |
| `tmp_evictions` | Successful temp-unit pressure evictions. |
| `reclaimed_bytes` | Bytes freed by sweeps (apparent) and temp evictions (allocated). |
| `held` | Present when any attempt was held or failed: maps each count name to its held or failed attempts, excluding steps that changed nothing. A failed manual redemption counts under `manual_redeems`. |

#### Assist events

Every event carries `assist`, its type, and `at`, its time in UTC. A field marked `?` is omitted when absent; `task` on `workflow_start`, `handle` and `run_id` on `workflow_step`, and `text` on `workflow_park` print `null` instead. What each assist is lives in [the assist log](../../internals/harness/loops.md#the-assist-log).

| `assist` | Category | Fields |
| --- | --- | --- |
| `untrusted_cwd_fallback` | Untrusted cwd | `kind`, `login`, `target`, `cwd`, `profile?` |
| `spend_rebuild` | Spend rebuild | `days`, `files`, `rebuilt`, `kept` |
| `empty_dir_grant` | Empty-dir trust | `kind`, `login`, `key`, `path`, `profile?` |
| `port_forward` | Port forwards | `host`, `remote`, `local?`, `error?` |
| `workflow_start` | Workflow | `run`, `workflow`, `task`, `checkout`, `prompt` |
| `workflow_step` | Workflow | `run`, `workflow`, `step`, `visit`, `seq`, `handle`, `run_id` |
| `workflow_route` | Workflow | `run`, `workflow`, `node`, `outcome`, `to`, `count?` |
| `workflow_park` | Workflow | `run`, `workflow`, `node`, `reason`, `text` |
| `workflow_end` | Workflow | `run`, `workflow`, `state` |
| `molt` | Molts | `kind`, `agent_id`, `label`, `successor_kind`, `successor_id`, `successor_label`, `generation?` |
| `judge_decline` | Judge declines | `task`, `checkout`, `profile`, `kind`, `reason`, `cost_usd?` |
| `stall_notice` | Stall notice | `kind`, `agent_id`, `label?`, `parent`, `silent_secs`, `message_id`, `delivered`, `error?` |
| `stall_nudge` | Stall nudge | `kind`, `agent_id`, `label?`, `team`, `stage`, `quiet_secs`, `message_id`, `delivered`, `error?` |
| `resident_launch` | Resident launches | `task`, `checkout`, `condition?`, `stopped?`, `handles` |
| `model_alias` | Model aliases | `kind`, `login`, `alias`, `from`, `to` |
| `tier_fallback` | Tier fallback | `kind`, `agent_id`, `label?`, `profile`, `tier`, `model`, `used_tier?`, `skipped?`, `unbuildable?` |
| `auto_redeem` | Auto-redeem | `kind`, `login?`, `reason`, `request_id`, `credits`, `soonest_expiry?`, `natural_reset?`, `outcome?`, `windows_reset`, `window_resets`, `error?` |
| `auto_continue` | Auto-continue | `kind`, `agent_id`, `label?`, `park`, `parked_since?`, `delivered`, `message_id` |
| `auto_compact` | Auto-compact | `kind`, `agent_id`, `label?`, `threshold`, `occupied_tokens?`, `message_id` |
| `soft_compact` | Soft compact | `kind`, `agent_id`, `label?`, `threshold`, `occupied_tokens?`, `message_id`, `delivered`, `error?` |
| `cache_keepalive` | Keepalive | `kind`, `agent_id`, `label?`, `idle_secs`, `waits`, `horizon_secs?`, `message_id`, `delivered`, `error?`, `capped`, `forced` |
| `idle_compact` | Auto-compact | `kind`, `agent_id`, `label?`, `idle_secs`, `idle_after_secs?`, `occupied_tokens`, `message_id`, `delivered`, `error?` |
| `idle_stop` | Idle stop | `kind`, `agent_id`, `label`, `idle_secs`, `idle_after_secs`, `requested_by?`, `stopped`, `error?` |
| `idle_stop_withdrawn` | Idle stop withdrawn | `kind`, `agent_id`, `label?`, `idle_after_secs`, `requested_by?`, `requested_at` |
| `hibernate` | Hibernate | `kind`, `agent_id`, `label`, `idle_secs`, `frozen`, `reclaimed_bytes?`, `hold?`, `error?` |
| `launch_retry` | Launch retry | `kind`, `label`, `run_id?`, `attempt`, `exit_code?`, `startup_ms`, `relaunched`, `error?` |
| `flip_compact` | Auto-compact | `kind`, `agent_id`, `role`, `threshold`, `from?`, `to`, `occupied_tokens?`, `message_id?`, `delivered`, `error?` |
| `auto_resume` | Auto-resume | `workspace_id`, `session_name`, `cause?`, `recovered`, `labels` |
| `tmp_evict` | Temp evictions | `workspace_id`, `handle`, `reclaimed_bytes`, `ended_at?`, `free_bytes_after`, `floor_bytes`, `error?` |
| `auto_gc` | Auto-gc | `workspace_id`, `scope`, `older_than_secs`, `reclaimed_bytes`, `worktrees_removed`, `workspaces_pruned`, `files_removed`, `messages_archived`, `problems`, `error?` |

### Category notes

`Port forwards: N` counts successful report-driven opens by `rimz remote connect`. A server returning after stopping counts again; reopening an existing forward after link recovery does not. `local` is present only on success and `error` only on failure. Failed attempts count under `held.port_forwards` and appear with `--all`; persistent local-port exhaustion contributes one held attempt. These records live on the laptop, so read its stats, not the host's.

`Empty-dir trust: N` counts automatic folder-trust grants on the empty root. `login` is the account as `kind@name`, `key` the trusted folder, and `path` the changed provider file. The line names the provider, account, folder, and profile, then the provider file. A successful write counts even if a later launch preflight refuses; failures and grants completed by another writer produce no record.

`Untrusted cwd: N` counts fresh explicit-cwd children started privately instead of stopping at an undecided folder-trust screen. `target` is the original directory and `cwd` the private one. The line names the provider, account, target, and profile, then the private cwd. Explicit `--empty-dir` launches do not count as fallbacks.

`Judge declines: N` counts each judge decline in a run a loop task started, once: a run that then waits out the decline adds none until its judge answers again. The line names the task, profile, reason, checkout, and recorded cost. A new true condition edge or an elapsed recheck asks the judge again, and a manual fire starts a fresh run ([guard declines](./loop.md#waits-and-checks)).

`Tier fallback: N` counts one per fresh tier move, whether it passed an unbuildable family, a spent account, or both. Replayed stamps, including a molt without `--agent`, are not counted again. The line names the first pass (`not buildable` or the usage reason); the event retains all usage `skipped` entries and optional `unbuildable` entries with their full build errors.

`Model aliases: N` counts catalog-derived model moves; each event carries the account `login`, the `alias`, and the models it moved `from` and `to`.

`Auto-redeem: N` counts automatic redemptions that reset a window. A redemption carries the account name as `login` when its record has one, and its line names the account when it is not `default`, as in `↻ codex@spare credit`. Manual redemptions keep the `auto_redeem` record type with `reason: "manual"`; their line says `manual`, and they are not automation assists.

`Keepalive: N` counts delivered pings. `capped` is true on the ping that told the agent the keepalive maximum was reached; its `◷` line ends with `, limit reached`. `horizon_secs` is the [keep-warm](../../guide/configuration.md#keep-warm) finite maximum that was holding the agent (the resolved `max`, including the global default, or the keepalive cap for a `while` with no resolved maximum); its line reads `holding warm (2h)`. `forced` is true on the ping sent past the typing hold 30 s before the cache expired; its line reads `forced cache keepalive`, and a forced and a polite ping never fold into one row. `waits` counts all pending waits, including subagent and team waits. Failed attempts appear with `--all` without increasing the count.

`Soft compact: N` counts delivered mid-turn wrap-up steers, separately from the compact commands counted by `Auto-compact:`. A missed steer appears only with `--all` and in the held count, not as a delivered soft compaction.

`Idle stop: N` counts [`stop --when-idle`](./agents.md#stop) requests that stopped their agent. `idle_secs` is how long the agent had rested, `idle_after_secs` the requested threshold, `requested_by` the asking agent's handle, and `stopped` whether the stop ended the agent: no pane hosts it and its process is gone, or, for a process RimZ does not know, its end is recorded. A stop that did not appears with `--all` with `stopped: false` and its reason in `error`, such as its pane staying open or its process still running after SIGTERM; it leaves its request armed unless it closed the agent's pane. `error` can accompany `stopped: true`: the agent stopped and something else failed, such as a child's pane staying open.

`Idle stop withdrawn: N` counts requests withdrawn by a prompt from the user; `idle_after_secs` is the withdrawn threshold. These events count as recorded, never held; their `↶` line names the agent, threshold, and requester when an agent asked.

`Molts: N` counts agents moved into fresh sessions by `rimz agents molt`. Each event names the predecessor (`kind`, `agent_id`, `label`), the successor (`successor_kind`, `successor_id`, `successor_label`), and optional inherited `generation`. The successor id is its launch id. A molt counts once its new tab opens, even if the successor later fails or arming the predecessor's idle stop fails. The line shows both handles, then both providers and ids.

`Launch retry: N` counts relaunches of agents whose CLI died before opening a session, one per [relaunch](./agents.md#launch-a-layout). `label` is the agent's handle, `run_id` is omitted for a launch without a supervised run, `attempt` counts relaunches of that launch from 1, `exit_code` is omitted when a signal killed the process, and `startup_ms` is how long the process that died lived. A relaunch that could not spawn appears with `--all` and `relaunched: false`.

`Workflow: N` sums the five [workflow](./workflow.md) assist kinds. `run` is the `wf_` id. `workflow_start`, emitted only for loop-created runs, once the run first starts an agent, a judge or a message or ends other than done before that, carries `task` (the loop task) and `prompt` (whether the run has one); its line reads `workflow babysit-pr started by loop babysit-pr`. `workflow_step` carries the attempt's `handle` and `run_id` (both null for a command step); `workflow babysit-pr sweep launched (visit 2)`. `workflow_route` carries `to` (the next node, or `@wait`, `@done`, `@failed`, and legacy `@user`); `workflow babysit-pr pr said needs-sweep → sweep`. A merged wait writes one route when it closes, with `count` set to its final poll count; its line reads `workflow babysit-pr pr waited ci-pending ×12`. An open wait contributes no route yet; other routes omit `count`, and older events without it still render as `said`. `workflow_park` lines read `workflow babysit-pr triage parked: asked: Which fix ships first?`. `workflow_end` carries `state` (`done`, `failed`, or `canceled`); `workflow babysit-pr done`. Each line ends with ` · run <id>`.

`Auto-gc: N` counts completed automatic sweeps. An `auto_gc` event carries `scope` (`room` or `machine`); records written before scopes existed read as `machine`. Totals cover the whole sweep's scope. The durable assist record additionally holds own-room `class_bytes` and `worktree_paths` (the first 50 removed paths in sweep order, `[]` for legacy records), which this JSON projection does not expose. `worktrees_removed` remains the full count, so a count above the path-list length indicates truncation on new records ([GC report](./maintenance.md#the-report)).

`Temp evictions: N` counts successful temp-unit evictions, separately from `Auto-gc:` sweeps, and adds their allocated bytes to `reclaimed_bytes`; an error counts under `held.tmp_evictions`. Auto-gc's byte detail totals both kinds. The line names the handle, reclaimed bytes, and workspace, then the free space and floor. `free_bytes_after` is the measured filesystem-available space, which may lag removal; `ended_at` is when the agent ended; `error` is present on failure. Allocated bytes count hardlinked files once per tree; snapshots can retain those blocks after removal.

`Spend rebuild: N` counts automatic rebuilds, not account-days. Its line shows the distinct rebuilt UTC-day count and total dollars before and after. Each `spend_rebuild` event carries the pending `days` it started from (`YYYY-MM-DD`), parsed transcript `files`, `rebuilt` account-days (`login`, `day`, optional `before_usd`, `after_usd`), and `kept` account-days (`login`, `day`). An empty or unchanged rebuild writes no record. These records count as effective even when some other account-days were kept or deferred.

## See also

- [Token Insight guide](../../guide/insight.md): what each figure means, the sidebar's live view of the same data, and how tokens are priced.
- [Accounts CLI](./accounts.md): per-account plan limits and balances.
- [Budget CLI](./budget.md): caps that park agents when spend crosses a line.
- [Stats internals](../../internals/stats.md): load paths, heatmap shading, and terminal fitting.

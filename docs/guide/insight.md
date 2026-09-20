# Token insight

Your agents spend on every provider you use, and each provider keeps its own tally on its own billing page: none of them beside your code, none of them aware of the others. The one number you want, what the fleet cost and how hard it worked, is the one nobody puts in front of you.

If you have run `ccusage` over Claude's transcripts, you already know the answer is on disk. Every transcript-backed turn writes the model, the token counts, and the timestamps to a local file, and `ccusage` reads Claude's. RimZ reads all of them, folds them into one account-global picture, and keeps that picture live in the room while the work lands.

<p align="center">
  <img src="../rimz-stats.png" alt="The rimz stats dashboard: a year-long token heatmap, the All time / Week / Month / Year window row, per-model and per-agent spend breakdowns, and activity insights" width="100%">
  <br/><sub><code>rimz stats</code>: a year of token activity, then where it went by model and by agent.</sub>
</p>

You read the picture in three places. `rimz stats` prints the whole year on demand, from anywhere on the machine. The sidebar keeps a live slice in front of you: the cockpit for the room you are standing in, the provider dashboard for each account. And `rimz providers` prints the dashboard's account blocks as text when you are outside a room.

## The marks you read everywhere

Spend and tokens speak one small vocabulary across all three. Learn it once:

| mark | reads as |
|------|----------|
| `◎` | sessions: the distinct agent threads that ran in the window |
| `◇` | total tokens |
| `↘` | input tokens, with cache writes folded in |
| `↗` | output tokens |
| `◌` | cache-read tokens: context re-read from the provider's cache |
| `$` | dollar spend, in green, whether reported directly or priced locally |

`◇` counts differently in the sidebar than in `rimz stats`, which is worth knowing before you compare two of them. In the sidebar it is `↘` plus `↗`. In `rimz stats` it adds `◌` on top, for the whole volume that moved. Cache reads are usually most of that volume, which is why a sidebar row can read `◇ 16M` beside `◌ 198M`.

The complete glyph legend, with every meter and tone, is the [interface reference](../interface/sidebar.md#reading-the-glyphs).

## The full picture: rimz stats

`rimz stats` prints your account-global history: a heatmap of daily token use, totals for a chosen window, and where the spend went by model and by agent. It reads the same published cache the sidebar writes, so it runs inside a room or anywhere else on the machine, in or out of a project, and it touches no agent and no session. RimZ keeps a year of history, so All time means the trailing 365 days and matches Year.

Top to bottom:

- **The heatmap** is a contribution graph of tokens per day, one column per week. Your terminal's width sets how far back it reaches, from four weeks up to fifty-two. Shading is relative to the days on screen, with the ceiling at your 90th-percentile active day, so one record-breaking Tuesday does not flatten the rest of the grid into grey. `--dollars` shades by cost instead of tokens.
- **The window row** (All time, Week, Month, Year) scopes everything below it. The heatmap ignores it. A plain `rimz stats` prints the All-time view; in the held dashboard, `Tab` and `Shift-Tab` cycle the window.
- **Models** ranks where the dollars went, each model under its friendly name (`gpt-5.5` reads as `GPT 5.5`) with its token split, its cache-hit percentage, and its share of the window's dollars. **Agents** ranks by session count instead and leads each row with dollars. Both fold anything below 1% of their section into a final `Other` row, along with whatever falls past the sixth. An unpriced model keeps its own row as long as nothing in the section has a price, and its tokens count either way.
- **The insight lines** close it:

  ```console
    Sessions: 7,179              Spend: $38,120.53
    Active days: 28/28           Longest streak: 72 days
    Most active day: Jul 18      Current streak: 32 days
    Cost/session: $5.31          Daily avg: $171.71
  ```

  `Active days` counts the days in the window that saw any activity, except under All time, which keeps the recent cadence and reads `N/28`. `Daily avg` divides the window's spend by those active days rather than by every day on the calendar, so it tells you what a working day costs. Week and Month add a comparison against the preceding equal window, `(↑n% vs prior week)`, whenever that window had spend.
- **Assists** counts the work RimZ saved you, one row per category that has anything to report: delivered auto-continues and the parked hours they recovered, compactions, redeemed reset credits, rooms restored after a crash or reboot, and automatic `rimz gc` sweeps. `Auto-compact` covers every compaction RimZ ran ahead of a delivery, plus the idle and stage-flip compactions that were delivered. The whole section is absent until one of these has happened.

For a script, `rimz stats --json` is the same data unfolded. It gives you the per-day buckets to window yourself, the trailing week, month, and year, both breakdowns complete with no `Other` fold and no row cap, tool-call counts by tool name, and the assist rollup with its events. `rimz stats --assists` prints the assist timeline as text, newest first, with the message and agent ids to trace each one. Every flag and every JSON field is the [stats reference](../reference/cli/stats.md).

**The figures are as fresh as the last walk, and no fresher.** A running sidebar keeps the spending cache current, and a one-shot `rimz stats` prints that cache however old it is. With no cache to print, the run walks every agent transcript on the machine behind a progress bar and publishes what it found; that walk is also the only thing on this path that fetches the price table. So a one-shot run on a machine whose rooms have been closed all day shows you yesterday's figures. `rimz stats --refresh` fixes that: it holds the panel open on the alternate screen and repaints from a live reread every minute, leaving no scrollback behind. What each mode reads, writes, and fetches is [how fresh the figures are](../reference/cli/stats.md#how-fresh-the-figures-are).

## In the sidebar, live

The sidebar keeps two live slices of the same numbers in view while you work. The cockpit at the top scopes them to the room you are standing in; the provider dashboard pinned at the bottom widens them to each provider account, whole machine included.

### Per room: the cockpit

Two of the cockpit's lines carry the spend:

```
 ◎ 91                          ◇ 32M ↘ 28M ↗ 3M ◌ 472M    ← sessions · token breakdown
 ¤ 16 (2)                                      $420.00    ← live agents · unread · spend
```

The token breakdown sums every session that ran in this room during the spend window, and the dollar figure below it is the room's cost over that same window, counting up in an eased roll the moment any agent's cost moves. Both lines are scoped to the room: the project root and the worktrees grouped under it, never your whole machine.

A pane-backed child launched through `rimz subagents` owns its own session, so its cost adds here like any other agent's. A Claude subagent that reports inside its parent's transcript is already counted in the parent's figure, so its own card shows the split for information only. That card hides its cost entirely when any of the child's requests has no known rate, rather than print a figure it knows is short.

The window is yours to set with `[sidebar] spend_window` ([configuration](./configuration.md#sidebar-rendering)):

- `session` (the default): the current burst of work, opened by your latest prompt to any agent after a five-hour idle gap. The burst itself is one machine-global event, and the cockpit counts the part of it that ran in this room. Loop-fired turns and agent-to-agent messages count inside an open burst and keep it alive, but never open one.
- `24h`: a trailing twenty-four hours.
- `today`: since calendar midnight in your configured `timezone`.

To read one agent instead of the room, [`rimz agents show`](./fleet.md#manage-a-running-room) prints that agent's lifetime token split and dollar total across every session it has resumed, with its live tool-call summary, and `rimz agents history` breaks the same work down turn by turn.

### Per account: the provider dashboard

Pinned to the bottom of the sidebar is one block per provider, because a plan's limits belong to the login that every session of that provider shares, not to any single agent. A block names the account and plan, that provider's spend, and the account's budget bars.

The spend row is account-global: sessions, the token breakdown, and dollars pinned right, over the same window the cockpit uses. Under `session` it covers the whole machine-global burst, wherever on the machine that work ran. A provider with no spend history on disk keeps the row and its layout, printing `–` where the token and dollar figures would be, with `◎` counting only the sessions live in this room.

Totals rows close the dashboard, summing every provider across the trailing week and month. A wide pane gives them one row per window; a narrower one moves the dollars onto a third row of their own. These totals are account-global like `rimz stats`, counting every project on the machine, so one glance tells you where the week is going whichever room you are standing in.

Outside the sidebar, `rimz providers` prints the same picture as text, one block per account rather than one per provider, so a room's [named accounts](./accounts.md) each get their own. It probes every login and runs any usage read that is due, publishing what it learns into the caches the sidebar reads; it touches no agent and no room. The blocks, the flags, and the JSON form are the [providers reference](../reference/cli/providers.md).

#### Budget is not spend

A block's `5h` and `7d` bars measure a different thing from the dollar figure above them. They are your subscription plan's included usage, draining toward the reset printed beside them (`↻ 1h47m`), and a bar fills with what is left. Claude Max, ChatGPT Pro, and plans like them refill on those windows automatically, so the bars are your read on pace, not a bill. An API-key account has no such window and carries a single `api` row instead; an uncapped key reads as a full bar with `∞`.

A bar climbs the moment a running agent reports more usage, but a refill can reach it two minutes late: a provider sometimes grants a mid-window free reset, and RimZ waits for a second reading before it believes one, so that a garbled sample cannot refill a bar on its own. Where each reading comes from and which one wins is [providers internals](../internals/agents/providers.md).

When a window does empty mid-turn, the agent parks rather than fails, and with auto-continue it resumes itself once the window resets ([loops → keep the fleet moving](./loops.md#keep-the-fleet-moving)). What a full, spent, ticked, or dim bar is telling you is drawn in the [interface reference](../interface/sidebar.md#budget-bars).

#### Cap the spend you read here

Everything on this page reads; the same numbers can also enforce. A dollar cap on one turn, one agent, one loop task, the room's whole fleet, or a provider login parks the work when it crosses the line. A crossed room or account cap announces itself right here, turning the cockpit or the provider row alarm-red and explaining the stop as `$50.21 of $50/day`; a turn, agent, or task cap says so on that agent's card. The caps, the park, and what resumes the parked work are the [budgets guide](./budget.md).

## How the numbers are calculated

Every historical figure comes from the transcript and session files your agents already write to disk. RimZ never scrapes a pane or guesses from the screen. Claude, Codex, Grok, OpenCode, and Pi report spend end to end; Amp, Copilot, Kimi, and Qwen contribute what their own local records carry, and [agent support](../reference/agent-support.md#the-wiring-matrix) names the gap behind each of them. An agent whose CLI keeps no usable local record contributes nothing to history and shows only the live figure its card can read.

How a token becomes a dollar:

- A provider that logs a dollar cost per request is taken at its word. Pi, OpenCode, and older Claude transcripts do, and the price table only fills in entries they left without a cost.
- A provider that logs token counts is priced from a per-model table. RimZ ships one built into the binary and refreshes it weekly from the public LiteLLM and models.dev price lists, so a new model's rate lands without waiting on a RimZ release, and a model it cannot price at all pulls the next refresh forward. Set `RIMZ_PRICING_OFFLINE` to any value to stop every fetch and price from what is already on disk. Input, output, cache writes, and cache reads each price at their own rate, and a Claude organization that deploys contracted rates in its managed settings prices from those instead of from list.
- Long-context rates come in two shapes. Anthropic's apply per token class: each class prices on its own, at the higher rate for whatever part of it runs past 200,000 tokens. Models that carry a threshold of their own, OpenAI's GPT flagships among them, instead switch the entire request to long-context rates as soon as its input crosses that threshold, so every token in the request prices high.
- A model RimZ has no price for still contributes its tokens and its session to every total. Only its dollar column reads zero, and only until a price is found.

Live figures are priced the same way as historical ones, request by request, which is what lets a long-context tier land exactly. Copilot and Qwen are the exception: their statuslines publish only a session-cumulative total, with the request boundaries already summed away, so RimZ estimates those at base rates.

Scope splits in two. The cockpit is the room you are standing in. Everything else, the provider dashboard and all of `rimz stats`, is account-global, summed per provider account across every project on the machine. Windows follow the surface: the cockpit and the dashboard's spend rows share whichever `spend_window` you configured, the dashboard's totals rows are the trailing week and month, `rimz stats` adds year and all-time, and its heatmap buckets by calendar day.

Per-agent and per-team lifetime figures group every continuation of one agent and deduplicate the API calls a resume replayed, so an agent you have restarted five times reports one history rather than five overlapping ones. `rimz agents show`, `rimz teams show`, a finished group's sidebar receipt, and `rimz agents attribution` all use that same all-in fold across the agent and every subagent it launched.

Those four surfaces count a worktree's current life, not its whole past. RimZ stamps each worktree it creates with a creation time and counts only the sessions registered at or after it, so removing a worktree and starting again under the same name gives the new work its own tally, while the earlier sessions stay in your history and drop out of these four. A checkout RimZ did not create, your main clone included, carries no stamp and keeps reporting everything it has retained.

The same four surfaces carry estimated active time. RimZ opens a span while an agent shows progress, closes it when the agent stops, caps an open span once progress goes quiet, and adds the per-agent spans together, so three agents working in parallel can report more agent-time than the change took in wall-clock hours. Those runtime records age out under `rimz gc`, leaving an older attribution record with no active-time figure at all rather than a wrong one.

The cache-hit percentage measures how much prompt input came from cache: `cache read / (cache read + cache write + fresh input)`, rounded to the nearest whole percent, with output outside the denominator. A live agent card measures the whole provider session, matching its dollar figure; the Models and Agents rows measure the selected stats window. Green is 90% and up, yellow is 70 to 89, red is below 70, and a missing or zero denominator leaves the figure out.

Tool-call figures have two sources on purpose. An agent card counts the live tool-completion events of that session, while `rimz stats` rebuilds history from the provider's own transcripts, Codex rollouts, Pi sessions, and OpenCode tool parts. The historical maps therefore backfill sessions that predate an upgrade, and they can run ahead of the live count: Codex's hooks miss web search and other calls it does not hook, all of which its rollout history keeps. Only Claude, Codex, Pi, and OpenCode supply tool statistics at all; [agent support](../reference/agent-support.md#the-wiring-matrix) declares the rest.

Nothing here fails into a wrong-looking number. Spend is enrichment, never a precondition, so a missing binary, a logged-out account, or an unpriced model leaves an explicitly absent field or a zero inside a tally that is otherwise real. For the walk, the caches, and the price-table precedence, see [spending internals](../internals/agents/spending.md#token-pricing).

## Configuration

Three settings change what these figures mean or where you see them, all plain TOML:

- `[sidebar] spend_window` picks the cockpit and provider-row window (`session`, `24h`, `today`), and the global `timezone` sets the `today` cutoff and every displayed time ([configuration](./configuration.md#sidebar-rendering)).
- `[theme.display] provider_list`, `provider_tabs`, and `max_provider_blocks` choose which provider blocks the dashboard shows, in what order, and whether they stack or share a tab rail ([theming](./theme.md#display)).
- `[accounts.usage_limit_usd]` gives an account with no provider-reported cap a display ceiling, turning its full `∞` row into a bar that drains against trailing-month spend. It tunes the bar only; the provider still enforces the real limit ([configuration](./configuration.md#accounts)).

## See also

- [`rimz stats` reference](../reference/cli/stats.md): every flag, the JSON fields, and the assist timeline.
- [The sidebar](./sidebar.md): the column these figures sit in, and how it routes your attention.
- [Budgets](./budget.md): turn the spend read into an enforced cap, per turn, agent, task, room, or login.
- [Agents](./fleet.md#manage-a-running-room): one agent's cost, turn by turn and across its whole life.
- [The sidebar on screen](../interface/sidebar.md#the-provider-dashboard): every bar, tone, and glyph drawn exactly.
- [Spending internals](../internals/agents/spending.md): the transcript walk, the caches, and the price table in depth.

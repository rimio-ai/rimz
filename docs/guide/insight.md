# Token Insight

You run a fleet now, not a single agent, and the fleet spends. Tokens turn into dollars across every provider you use, and each keeps its own tally on its own billing page: none beside your code, none aware of the others. The one number you actually want, what the fleet cost and how hard it worked, is the one nobody puts in front of you.

<p align="center">
  <img src="../rimz-stats.png" alt="The rimz stats dashboard: a year-long token heatmap, the All time / Week / Month / Year window row, per-model and per-agent spend breakdowns, and activity insights" width="100%">
  <br/><sub><code>rimz stats</code>: a year of token activity, then where it went by model and by agent.</sub>
</p>

Token insight is that number, in the terminal. Every transcript-backed agent turn leaves a local usage store (the model, token counts, timestamps, and for some providers the dollar cost), and RimZ reads those files into one account-global picture across Claude, Codex, Amp, Pi, and OpenCode. If you have run `ccusage` over Claude's transcripts, this is the same trick widened across every provider with a usable local usage store and wired into the room so it updates as the work lands. Kinds without such a store surface less; the per-kind coverage is in [agent support](../reference/agent-support.md).

You read it two ways. `rimz stats` prints the whole history on demand, from anywhere. The sidebar keeps a live slice of the same data in front of you while you work.

## The full picture: rimz stats

`rimz stats` prints your account-global history: a heatmap of daily token use, the totals for a chosen window, and where the spend went by model and by agent. It reads the same data as the sidebar, so it runs inside a room or anywhere else on the machine, in or out of a project.

- **The heatmap** is a GitHub-style contribution graph of tokens per day, about a year of history, smoothly shaded from an idle day up to your heaviest. The scale reads against your own rhythm, resists one-off spike days, and spreads the mid-range perceptually so an active stretch stays visible. `--dollars` shades it by cost instead of tokens.
- **The window row** (All time, Week, Month, Year) scopes the Models, Agents, insight rows, and Assists below it, while the heatmap stays full-history. In the held dashboard, `Tab` and `Shift-Tab` cycle the window; a plain `rimz stats` prints the All-time view.
- **Models** ranks where the dollars went, each model under its friendly name (`gpt-5.5` reads as `GPT 5.5`) with its token split, cache-hit percentage, and share of the window. **Agents** ranks by session count across the five transcript-backed kinds and carries the same cache-health percentage. An entry below 1.0% of its section folds into `Other`, and the row cap folds any remaining long tail into the same row. Both breakdowns carry tokens as well as dollars, so an unpriced model remains itemized when the section has no priced spend and its tokens stay counted in every case.
- **The insight lines** close it: sessions and spend for the window, cost per session and average spend per active day over the same spend span, how many days saw activity, your heaviest single day, and your longest and current active-day streaks. Week and Month also compare spend with the immediately preceding equal window. All time keeps the recent `Active days: N/28` cadence line while its average, like its spend, covers the trailing year.
- **Assists** shows non-zero category counts for the ways RimZ saves work: delivered auto-continues, pre-delivery auto-compaction, reset-credit redemptions, and sessions restored automatically after a crash or reboot. The section stays absent until RimZ has a counted assist to report; `rimz stats --assists` carries the complete newest-first event timeline.

For a shell or a script, `rimz stats --json` emits the per-day buckets that scripts can use for their own trend windows, the usage windows, both breakdowns (including `cache_hit_pct` when the input-side denominator exists), the insights, and the structured assists rollup plus events. `rimz stats --assists` prints the complete newest-first assist timeline with the redeem request id and reset windows, continue and compact message and agent ids, and restored workspace and session. `rimz stats --refresh` instead holds the panel open and repaints it every minute; this is the live pane the `rimzd` daemon view carries.

`rimz stats` only reads. It touches no agent, writes nothing to your sessions, and prints from a cache RimZ keeps under its own state directory. Its one network call is the weekly price-table refresh described below, which `RIMZ_PRICING_OFFLINE=1` turns off.

## In the sidebar, live

The sidebar keeps two live slices of the same numbers in view while you work: the provider dashboard breaks your spend down by account, and the cockpit scopes it to the room you are standing in.

### The marks you read everywhere

Spend and tokens speak one small vocabulary across `rimz stats`, the provider dashboard, and the cockpit. Learn it once:

| mark | reads as |
|------|----------|
| `◎` | sessions: the distinct agent threads that ran in the window |
| `◇` | total tokens |
| `↘` | input tokens, with cache writes folded in |
| `↗` | output tokens |
| `◌` | cache-read tokens: context re-read from the provider's cache |
| `$` | dollar spend, in green, whether reported directly or priced locally |

Cache reads are usually most of the volume, so a busy day is mostly `◌`. The complete glyph legend, with every meter and tone, is the [interface reference](../interface/sidebar.md#reading-the-glyphs).

### Per account: the provider dashboard

Pinned to the bottom of the sidebar is one block per provider account, because a budget belongs to the login, not to any single agent. Each block names the account and plan, the CLI version, that provider's spend, and the account's budget:

```
  Claude v2.1.169 · Claude Max                    ⇅ rc
  ▐▛███▜▌  ◎ 53  ◇ 16M ↘ 13M ↗ 2M ◌ 198M       $188.88
 ▝▜█████▛▘ 5h ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱   ↻ 1h47m
   ▘▘ ▝▝   7d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱   ↻ 5d22h
```

When a provider has historical usage, the stats row is its account-global spend for the headline window: sessions, the token breakdown, and dollars pinned right. In `session` mode, every provider row shares one machine-global burst, opened by your prompt to any agent kind. A provider without spend history on disk shows only `◎` with the sessions active in this room; the token and dollar positions stay absent rather than read zero. Two fleet-total rows then close the dashboard, summing every supported provider across the trailing week and month:

```
  W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88
  M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66
```

These totals are account-global, like `rimz stats`: they count every project on the machine, so one glance tells you where the week is going regardless of which room you are in.

#### Budget is not spend

The `5h` and `7d` bars measure a different thing from the dollar figures. They are the included budget of your subscription plan, draining toward the reset printed beside them (`↻ 1h47m`), and they fill with what is left. A plan like Claude Max or ChatGPT Pro refills on its duration windows automatically, so those bars are your read on pace, not a bill. An API-key account has no such window, so its block shows a single `api` budget row instead; an uncapped key is a full bar with `∞`.

When a window empties mid-turn the agent parks rather than fails, and with auto-continue it resumes itself the moment the window resets ([loops → keep the fleet moving](./loops.md#keep-the-fleet-moving)). The exact bar tones, the reset colouring, and the not-yet-started window are drawn in the [interface reference](../interface/sidebar.md#zone-3--the-provider-dashboard); where the readings come from is [providers internals](../internals/agents/providers.md).

#### Cap the spend you read here

Everything on this page reads; the same numbers can also enforce. A dollar cap on an agent, a loop task, the room's whole fleet, or a provider login parks the work when it crosses the line, and a crossed cap announces itself here: the cockpit or provider row turns alarm-red and explains the stop as `$50.21 of $50/day`. The caps, the park, and what resumes it are the [budgets guide](./budget.md).

### Per room: the cockpit

The top of the sidebar narrows all of this to the room you are standing in. Two lines carry the spend:

```
 ◎ 91                          ◇ 32M ↘ 28M ↗ 3M ◌ 472M    ← sessions · token breakdown
 ¤ 16 (2)                                      $420.00    ← live agents · unread · spend
```

The token breakdown sums every durable session record that ran in the room's spend window, and the dollar figure below it is the room's cost for that same window, counting up in an eased roll the moment any agent's cost moves. Live transcript costs price each request before adding it to the session; statuslines that expose only session-cumulative totals use base-rate estimates because their sums no longer contain the request boundaries needed for exact long-context tiers. Both lines are scoped to this room: the project root and the worktrees grouped under it, never your whole machine.

The window is yours to set with `[sidebar] spend_window` ([configuration](./configuration.md#sidebar-rendering)):

- `session` (the default): the current burst of work, opened by your latest prompt to any agent after a five-hour idle gap. Provider rows share one machine-global burst; the cockpit applies the same rule within this room's scope. Loop-fired turns and agent-to-agent messages count inside an open burst and keep it alive, but never open one.
- `24h`: a trailing twenty-four hours.
- `today`: since calendar midnight in your configured `timezone`.

To read one agent's cost instead of the room's, [`rimz agents show`](./fleet.md#manage-a-running-room) prints that session's token split and dollar total.

## How the numbers are calculated

Historical figures come from the transcript and session files your agents already write to disk. RimZ never scrapes a pane or guesses from the screen; token counts come from provider-owned records and structured wires.

The cache-hit percentage measures prompt input served from cache: `cache read / (cache read + cache write + fresh input)`, rounded to the nearest whole percent. Output is outside the denominator. The agent card uses cumulative counters for that provider session, covering the same lifetime as its session dollar figure; the Models and Agents rows use the selected stats window. A missing or zero input-side denominator stays absent. Green means at least 90%, yellow means 70–89%, and red means below 70%.

Turning tokens into dollars is where the care goes:

- Providers that log a dollar cost per turn (Pi, and older Claude transcripts) are taken at their word.
- Providers that log token counts (Claude, Codex, and Amp through a private cache RimZ reads best-effort) are priced with a per-model table. RimZ ships a built-in table and refreshes it weekly from the public LiteLLM price list, so a new model's rate lands without waiting on a RimZ release. Input, output, cache writes, and cache reads each price at their own rate. Marginal 200k tiers price each token class independently; request-selected tiers such as OpenAI's covered GPT flagship models switch the whole request to long-context rates once its input crosses the model's threshold.
- A model RimZ has no price for still contributes its tokens and its session to every total. Only its dollar column reads zero, until a price is found. Token attribution never waits on pricing.

Two scopes and a set of windows keep the surfaces honest:

- The cockpit is scoped to the room you are in. The provider dashboard totals and everything in `rimz stats` are account-global, summed per provider account across every project on the machine.
- The cockpit window is `session`, `24h`, or `today` (above). The dashboard's totals rows are the trailing week and month, `rimz stats` adds year and all-time, and the heatmap buckets by calendar day.

None of this can fail into a wrong-looking number. Spend is enrichment, so a missing binary, a logged-out account, or an unpriced model degrades to an explicitly absent field or a zero inside a real historical tally, never a bad figure dressed up as a real one. Provider totals and locally priced token counters render identically as dollars. For the mechanism in full, the caches, the price-table precedence, and the window fusion, see [providers internals](../internals/agents/providers.md).

## Configuration

A few knobs, all plain TOML ([configuration](./configuration.md)):

- `[sidebar] spend_window` picks the cockpit window (`session`, `24h`, `today`), and `timezone` sets the `today` cutoff and the displayed times.
- `[theme.display] max_provider_blocks` and `provider_list` choose how many provider blocks the dashboard shows and in what order. A token-only provider ranks by spend like any other.
- `[accounts.usage_limit_usd]` sets a display ceiling per API-key provider, changing its full `∞` row into a month-budget bar that drains against trailing-month spend and shows `$left`. It tunes the bar only; the provider still enforces the real limit.

## See also

- [The sidebar](./sidebar.md): reading the cockpit and dashboard for attention, not just for spend.
- [Budgets](./budget.md): turn the spend read into an enforced cap, per agent, task, room, or login.
- [Agents](./fleet.md#manage-a-running-room): one agent's token split and cost with `rimz agents show`.
- [Loops](./loops.md#keep-the-fleet-moving): auto-continue when a budget window empties mid-turn.
- [Configuration](./configuration.md#sidebar-rendering): the spend window, timezone, and provider-display knobs.
- [The sidebar on screen](../interface/sidebar.md#zone-3--the-provider-dashboard): every bar, tone, and glyph drawn exactly.
- [Providers internals](../internals/agents/providers.md): accounts, budgets, spend, and the price table in depth.

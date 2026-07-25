# Sidebar

> The sidebar is the narrow column pinned beside your panes, and it answers one question: **which pane needs you, right now.** This page is how to read it — the zones, the cards, the states behind them, and the ranking that decides what sits on top. Every glyph, meter, and frame drawn exactly is the [interface reference](../interface/sidebar.md); the presence and ranking machinery is [internals → sidebar](../internals/sidebar/sidebar.md).

<p align="center">
  <img src="../rimz-sidebar.png" alt="The sidebar: cockpit on top, triaged agent cards by worktree, provider dashboard at the bottom" width="420">
</p>

## What it is

Working with coding agents today means one conversation per terminal: prompt, watch it run, answer when it stops. To run more agents you open more tabs and rotate through them, and every rotation is a context switch for your head as much as for the terminal — re-read scrollback to tell *thinking* from *waiting on me*, reload what this agent was doing, then find your way back to your own work. Nothing announces a finished turn or a blocking question; you learn about them whenever you next visit the tab, and until you do, an agent stopped on a permission prompt looks exactly like an agent reasoning. With no view of the whole, the fleet lives in your working memory, and working memory tops out around six to ten threads on a good day. It is exhausting well before it is full.

The sidebar is the missing view, and it is the core of RimZ. One narrow column beside the agents' panes holds one live card per agent — its state, what it is working on, how full its context window is, what it has cost — updated the moment the agent starts a turn, calls a tool, asks a question, or finishes. The top line counts the fleet by state, so a row of zeros means nothing needs you and you stay where you are. When an agent does need you, its card surfaces and marks itself unread; press `n` and you are in that agent's pane. Checking becomes a glance, a switch starts with the card's context already read, and finished work waits marked instead of hoping to be noticed — the ceiling on how many agents you can run stops being your memory.

You never work *in* the sidebar — it has no reply box and no approve button. It routes you to the pane, and you answer in the agent's own UI, where the full prompt and its safe defaults live.

## What the column is, underneath

Two facts make the column trustworthy, and both are worth knowing before you lean on it.

**Agents report themselves.** The reporting hooks you approved when you [installed hooks](./setup.md#install-agent-hooks) fire from inside each agent — session start, tool call, blocking question, turn end — so a card changes the moment the agent does, not when a scraper catches up, and nothing on a card is guessed from screen contents. Two derived states are RimZ's own judgment rather than an agent report; [the lifecycle](#the-agent-lifecycle) calls them out.

**The sidebar only reads.** It is one pane in your room running a renderer over the durable store those hooks write. Its only writes are its own display state — which cards you have read, a heartbeat — and jumping focuses a pane the same way your mux prefix would; your agents' processes, sessions, and files are untouched by anything on this page. Close the sidebar's pane and every agent keeps working; the next `rimz` attach or `rimz reload` brings it back, and nothing was missed in between, because a card that needs you stays unread and ranked in the store until you look. The column is a window, not a place where state lives.

That split — presence live, facts durable — is also what a row *is*: a row exists because a pane runs right now, and when the agent exits, its row goes with it. The history stays in the store, behind `rimz agents show`, `rimz agents logs`, and [`rimz transcript`](../reference/cli/agents.md).

## The layout

Top to bottom, the column stacks three zones plus pinned chrome:

```
┌───────────────────────────────────────────┐
│ cockpit        the whole fleet summary    │  pinned
├───────────────────────────────────────────┤
│ cards          one card per pane,         │  scrolls
│                grouped by worktree        │
├───────────────────────────────────────────┤
│ provider       accounts, budget,          │  pinned
│                token stats, pets          │
├───────────────────────────────────────────┤
│ footer         help · link health         │  pinned
└───────────────────────────────────────────┘
```

The cockpit and dashboard hold fixed positions, so the numbers you glance at never jump as agents change state; only the cards scroll between them.

### The cockpit

The top block reads the whole room in four lines:

- **Identity.** The workspace name and its path, so a glance confirms which project this room is.
- **Sessions and tokens.** How many agent sessions have run in the configured spend window, with the room's token breakdown pinned right: total, input, output, and cache-read. What the marks mean and how the figures add up is [Token Insight](./insight.md).
- **Live agents and spend.** How many agents are alive right now, an unread count like `(2)` when results or questions await you, the open-PR lane count `⑃ N`, and the room's dollar spend rolling up live as agents work. The PR chip turns red for failing CI, amber while CI runs, green when every known lane passes, and keeps its cool link tone when CI is unknown. Click the unread count to filter the cards to unread rows, or click the open-PR count to filter them to lanes whose branch has an open pull request.
- **The make-up line.** The fleet by state: `? 3  ! 1  ⏸ 0  ✓ 8` counts who asked you something, who failed, who is parked on a provider limit, and who holds a finished result, with live capacity (`⢿` working, `○` idle) on the right.

A row of zeros with no unread count means nothing needs you, so you can skip the scan entirely. Every non-zero bucket is a click target that filters the cards to that state in every tab of the room; clearing or replacing the lens in one tab updates them all. When the agent that most needs you scrolls out of view, an `↑ N need you` banner appears; clicking it brings that card back.

### The agent cards

The body: one card per pane, grouped under the worktree it lives in. A worktree is total isolation, so each group reads as one bounded block, and the group header names its active team as `· forge`, shows the branch's CI verdict at its HEAD commit, names a linked pull request (`#91`) when one exists, and carries the work's git story on the right: commits ahead and behind the trunk, lines added and removed (uncommitted and untracked work counts too), and a marker for where the work stands, from a plain branch through an open, merged, or closed pull request. An open or merged PR supplies the verdict beside its badge; a branch without a PR shows the bare `✓`, `✕`, or `◌` CI glyph. The team label is the unique team among the group's team members, so roleless shells and stray agents do not hide it; two distinct teams leave the header unlabeled. The `#91` badge opens that pull request through the terminal's OSC 8 hyperlink support; Zellij uses `osc8_hyperlinks true`, while tmux 3.4 or newer recognizes terminals whose `terminal-features` include `hyperlinks`. Panes outside every project checkout fold into a dim `external` divider at the very bottom.

Cards arrive already triaged (the [ranking below](#how-the-column-is-ordered) decides the order), and a busy worktree caps at six rows with a `+K more` line that only hides idle agents and quiet shells while work remains active. A finished line of work with several agents collapses completely to a muted-bold header and a two-line dim receipt, including unread success cards: `▸` marks the expandable roster line, a shared team name leads the members when present, each member keeps its status glyph and softened provider-brand name, overflow and quiet shells fold into `+n`, and tracked cost pins right; the totals line shows the pod's token breakdown followed by its aggregate session cache-hit percentage and pins its muted finished age on the right. A narrow multi-agent roster falls back to count-only `▸ +K done`; click the header or either receipt line, or apply the `s` status filter, to reveal the roster, then click the header to collapse it. Process-only groups and groups with one agent keep their rows visible. A focused or order-held member reveals the whole roster, so the pod is never half-collapsed.

### The provider dashboard

Budgets are account-scoped, one account shared by every session of a provider, so they live in a pinned panel at the bottom rather than on the cards. Each provider block names the account and plan (`Claude Max`, `ChatGPT Pro`) and drains a "mana" bar per budget window (5-hour, 7-day) toward its reset, so a glance reads how much of the plan is left. The block also carries that provider's spend, and two totals rows below sum the fleet across the trailing week and month. Reading those figures, and how every one is calculated, is [Token Insight](./insight.md).

With several providers the panel tabs, following whichever agent you have selected; `←`/`→` or a click picks one by hand. With [pets enabled](./pets.md), the companion rides the panel's right edge.

### Bottom chrome

The footer holds `? for help`, which opens the key and filter overlay in place. When you have been away, a quiet `zᶻ idle` badge appears; over SSH, a link-health badge shows the round-trip time. If the sidebar ever cannot read the room, a sticky alert takes the bottom line and says so, because a blank column should never masquerade as an empty room.

The sidebar follows its attached view at the explicit `[theme.display].width_percent`, otherwise 30% on views wider than 240 columns or when pets are enabled, and 25% otherwise, capped at `[theme.display].max_cols`. Press `a` to make it narrower and `d` to make it wider; each press calculates and validates an absolute room target before any pane moves, using 2 columns on tmux or Zellij's live-view increment of roughly 5%. The target persists and broadcasts immediately, then every tab, including the one where you pressed the key, converges through serialized feedback; observed pane widths confirm progress and never replace the target. A narrower press is ignored when Zellij's next reachable width would fall below 24 columns, while tmux lands exactly on that floor. The room target follows reloads and reattaches for the life of the session and may exceed `max_cols`; a newly created session starts again from the configured percentage policy and cap. Resizing with the multiplexer's own controls records no room width: with a recorded `a`/`d` width it snaps back live, and without one it persists until the next attach or `rimz reload` unless it exceeds `max_cols`. Use `a`/`d` to keep a width. These chords are configurable under `[sidebar.keys]`. Filters use `A` for all and `s` for success/done.

## The agent card

<p align="center">
  <img src="../rimz-card.png" alt="A finished agent card under its worktree header: identity and cost, description, context meter, token line, and the seven subagents it fanned out this turn" width="560">
  <br/><sub>A finished card with the subagents it fanned out this turn; the idle agent below collapses to a single line.</sub>
</p>

Each agent is a small stacked card, four lines at rest:

```
⢿ claude · Opus 4.8 · xhigh · 1m                $1.27    ← state · identity · cost
  store refactor                                         ← what it is working on
  ▣ ━━━━━━━━━━━━━━━━─────────────────────────── 38.2%    ← context meter: how full the window is
  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                   ◔ 8m    ← tokens in the window · last activity
```

- **The identity line.** The state glyph leads, animated while the agent works. Then the agent's handle (its team role, profile, or kind, so a team reads `planner` / `coder` / `reviewer`), the model, one reasoning-configuration token (effort or `thinking`), and the size of its context window. The session's dollar cost pins right and counts up live once the session has spent anything.
- **What it is working on.** The session's name or task, falling back to its first prompt so the card stays stable between turns. When a turn dies on a provider error, this line quotes the error (`API Error: Overloaded`) so the card says why without a jump.
- **The context meter.** How full the agent's context window is, as a bar and a percentage. The fill also shows where the window went: cache reads, cache writes, and fresh input paint as distinct runs, and the bar's tone shifts from calm toward red as the window fills.
- **The token line.** The absolute companion: tokens currently in the window, the same composition as markers, a `↻ N` count of completed context compactions, an amber `⟲ N` once consecutive identical tool calls reach the warning threshold, and, once the agent has been quiet for five minutes, its last-activity age pinned right, heating toward red as an hour approaches. The warning marker is annotation only; at the higher attention threshold the card escalates to `!`, and the next differing call clears both.

Selecting a card appends anything deeper without reshaping what is on screen: the **subagents** the agent spawned this turn appear underneath, each with its own live state, what the parent asked it to do, and, while it runs, its token spend, model, and elapsed time. A finished subagent keeps its `✓` or `!` verdict on the list until the parent's next turn. Subagents have no pane of their own, so they appear only here, nested under their parent.

How much of the card shows at rest is yours to tune with `card_density` ([theme.md → Display](./theme.md#display)): `compact` trims resting cards, `expanded` shows subagents everywhere.

## Process rows

A pane no agent has claimed (your editor, a shell, a build) renders as a slimmer, quieter row: the program's name, a hollow `○` when idle, a spinner while it does real work, and, for a working pane, its live command plus a CPU, memory, and I/O readout. Process rows sit below the agent cards in their worktree and never enter the cockpit tallies — they never ask for your attention. They are still jump targets, and the moment an agent starts in that pane the row becomes that agent's card.

## The agent lifecycle

Every card wears one state, and six cover the life of a session:

| glyph | state | meaning | needs you |
|-------|-------|---------|-----------|
| `○` | idle | alive, nothing in flight | no |
| `⢿` | running | working a turn | no |
| `?` | waiting | stopped mid-task for your answer: a permission, a plan approval, a question | **yes** |
| `!` | failed | the turn errored, died on a provider API error, or a running agent went silent past the stall window | **yes** |
| `⏸` | paused | stopped mid-turn on a provider rate limit or overload | when it recovers |
| `✓` | done | the turn finished cleanly and holds a result | a look, when convenient |

The full glyph vocabulary, including the transient heads that ride over a running card (thinking before the first file edit, compacting, waiting on subagents, parked on background work), is the [interface legend](../interface/sidebar.md#reading-the-glyphs).

A session's life traces one loop through those states:

```
 ●──launch──► idle ──prompt──► running ──┬── clean ──► done
                                 ▲   │   └── error ──► failed
                        answered │   │ asks you: permission,
                                 │   ▼ plan approval, question
                                 └─ waiting
```

- A fresh agent is **idle** until its first prompt; a prompt you type (or a queued `rimz message`) starts a turn.
- A **running** turn wears the working spinner (`⢿`, animated in the live column) from first token to last.
- An **ask pulls the card to `waiting`**, and answering it in the pane returns the agent to work; the card notices the answer even before the turn formally moves on.
- A turn ends **done** or **failed**; either way the agent is ready for its next prompt, and the state tells you whether to collect a result or unblock a problem.

Two states are RimZ's own judgment rather than an agent report. **Paused** is derived: when a turn stops because the provider's budget window is spent or the API is overloaded, the card parks at `⏸` instead of pretending to fail, and with [auto-continue](./configuration.md#resume) enabled it resumes by itself the moment the window resets or the backoff clears. **Stall** is the safety net: a running agent silent past the stall window (30 minutes by default) escalates to `!`, because silence that long usually means something needs a look; a parent quietly waiting on its subagents is exempt.

## Attention: the funnel

With two agents you watch panes; with ten lines of work you cannot, and the sidebar is built for the day you cannot. At that scale the unit of attention stops being the agent and becomes the **line of work** — a worktree and the team inside it — and the column reads as a triage list, not a status dump. Every glance answers three questions: who is blocked on me, what has delivered and waits on my verdict, and is anything burning silently.

The tiers, by distance from your next action:

1. **Blocked on you.** Asks (`?`) and failures (`!`), interleaved oldest-first: your work queue. Every minute an ask waits idles a loaded context — a whole team's, when the asker leads one — so these carry the most weight and climb as they age.
2. **Parked (`⏸`).** Blocked on the provider, not on you. There is nothing to answer until the limit recovers, and with [auto-continue](./loops.md#keep-the-fleet-moving) on, nothing to do at all; parks rank below the queue and above every calm card.
3. **Delivered, awaiting your verdict.** A `done` card leads the calm cards in its line of work because it holds a result: your review queue. This queue rots while it waits — the trunk moves on and the group header's behind count (`⇣`) climbs — so the header tells you when a delivered branch is getting more expensive to land.
4. **In flight, healthy.** Needs nothing and wants stable positions so your eyes can track it: running cards keep a flat weight and hold their place, with live cost and last-activity age as the cheap progress cues.
5. **Idle.** Spare capacity, relevant only when you have work to hand out. It reads last, and a freshly launched agent appends at the bottom instead of reshuffling the list.
6. **Merged or closed.** Zero attention, so it archives immediately. A lone agent stays visible under the muted-bold merged header, while a line with several agents collapses to that header plus the two-line dim receipt — an expandable `▸` team/member roster with cost pinned right, then token totals with the muted finished age pinned right. A merged header keeps only its verdict marker because the commit and churn figures are spent once the work lands. A narrow multi-agent roster falls back to `▸ +K done`; click the header or either receipt line to reveal it, then the header to collapse it, while a fresh run or ask revives it. Process rows do not trigger collapse. A focused or order-held member reveals the whole roster, so the pod is never half-collapsed.

The third question — burning silently — is the one no status dump answers, because a zombie run wears the same spinner as a healthy one. The column watches for you: a running agent silent past the stall window escalates to `!` and enters the work queue ([the lifecycle](#the-agent-lifecycle)), and a turn that dies on a provider error quotes the error on its own card, so quiet trouble surfaces in the same queue as loud trouble.

### From a glance to the pane

The cockpit compresses the fleet into one line, and the column below arrives already triaged: the card that needs you next sits at the top of its worktree, a soft wash marks every result, ask, and recovery you have not seen, and the one card that most needs you stays in motion until you read it.

You do not read where to go; you go. Press `n` (or `␣`) to jump to the next thing that needs you, oldest first, and RimZ focuses that agent's pane. The prompt and its safe defaults live in the agent's own UI, where the full context is, so you read and answer there; focusing the pane clears its unread mark, and `N` walks back. From any pane in the room, `Alt+p` focuses the sidebar and toggles back to your last working pane, so the whole loop runs without touching the mouse. The full key table is in [the interface reference](../interface/sidebar.md#jump--the-row-is-the-link).

Glance, jump, answer: that loop is the product. Desktop, bell, and command notifications carry the same cues when you are off-screen, and handlers or loops you wire can clear routine cues before they reach you ([notifications](./notifications.md), [loops and schedules](./loops.md)); the ranking below spends whatever attention is left.

### The unread inbox surfaces in place

A card turns *unread* the moment it enters `waiting`, `failed`, `paused`, or `done`, and stays unread until you focus its pane or mark it read, even after the agent recovers and moves on. The wash and blink mark it, the jump key walks unread rows oldest-actionable-first, and notifications ring it. The card keeps its place in the time and status order while the inbox gets you to it.

## How the column is ordered

The funnel above is the contract; the rules below implement it and settle the ties. Within a worktree, agent cards lead and process rows form a quiet tail. The exact ranking contract lives in [the internals](../internals/sidebar/sidebar.md#attention-ranking-and-the-cap).

### Time reshapes the order

Measured from each card's last activity, in three windows:

1. **Inside the first hour, blocked work climbs.** An ask, failure, or park grows more urgent the longer it waits, so a failure overdue fifty minutes outranks an ask from two minutes ago and blocked work reads oldest-first — the cheapest order to clear. Calm work keeps a flat weight, so live agents hold their place while they run. (The hour matches the agent's prompt-cache lifetime: answer inside it and the agent resumes warm.)
2. **Between one hour and twenty-four, everything cools.** Urgency decays instead of climbing: a stale ask still leads stale calm work, but the whole window sinks beneath anything currently hot, so yesterday's unanswered question stops competing with the agent blocked right now.
3. **Past twenty-four hours, a card sleeps.** It parks in an archive at the back, keeping only its state order, so an archived ask still reads above an archived idle agent.

A non-dirty merged or closed line of work with no running or attention member skips the clock and enters the archive immediately. A new run, ask, failure, or park revives it into the activity-ranked bands.

### Teams read as one

A co-launched team is one line of work, so it holds one contiguous block and takes one state derived from its members: any member asking or failed makes the team **blocked**, else a parked member makes it **paused**, else a running member makes it **working**, else a finished member makes it **done**, else it is **idle**. One blocked member lifts the whole block, so a planner waiting on you blocks its coder and reviewer too, whatever they are doing; a team where one member finished while others still run reads as working, because the team is done only when every member is. The block ranks by that derived state on its oldest blocked member's clock and stays contiguous, so teammates sit side by side in their declared role order.

### Activity decides among the calm

Attention always outranks git: a git-backed group with a blocked agent leads whatever its diff looks like. Among calm groups, activity answers *is this line moving?* first: **working** groups lead, **delivered** groups whose agents all succeeded follow, then **idle** agent groups, then process-only groups. Git refines groups at the same activity rung: **dirty** leads, **clean** follows, an unknown verdict follows clean, and **done** sinks. Done requires a merged or closed pull request or work that the trunk comparison classifies as merged; a pristine fork stays clean rather than reading as landed work.

### The shape that always holds

Two partitions hold regardless of any score, so the column keeps one stable outline:

- **Agent cards lead their worktree; process rows form the tail.** Shells, builds, and servers are context, so they seat below every agent card.
- **Project worktrees lead; out-of-project panes tail.** Panes outside every project checkout fold into the dim `external` divider that always sorts last, keeping an attention-only tally (`? n` / `! n`) so an out-of-project ask still surfaces.

The six-row cap trims only a worktree's idle and process tail while the line of work remains active. Anything active, blocked, parked, finished, unread, or focused stays visible under that ordinary cap. A terminal group is the deliberate exception when it has several agents: its non-dirty merged or closed verdict is acceptance evidence, so every row hides behind a two-line dim receipt; a lone agent stays visible under the muted merged header, even beside process rows. The expandable `▸` roster leads with the shared team name when present, names each member in its provider's softened brand tone beside its final status glyph, folds overflow and shells into `+n`, and pins tracked cost right; the totals line shows the detailed token split and pins the muted finished age right. A narrow multi-agent roster falls back to `▸ +K done`; click the header or either receipt line, or apply the `s` filter, to reveal it, then click the header to collapse it. Process-only groups stay expanded. A focused or order-held member reveals the whole roster, so the pod is never half-collapsed.

## Tuning

Five `[agents.attention]` knobs move the boundaries: `stalled_after_secs` (a silent agent escalates to `!`, 30 minutes), `tool_repeat_warn_after` (an identical-tool run gains `⟲`, 3 calls), `tool_repeat_attention_after` (that run escalates to `!`, 20 calls), `inactive_after_secs` (hot work ends, one hour), and `archive_after_secs` (a card sleeps, 24 hours). Details in [configuration.md](./configuration.md#sidebar-rendering).

## See also

- [The sidebar on screen](../interface/sidebar.md) — every glyph, meter, and frame drawn exactly, with the key table.
- [Token Insight](./insight.md) — read the cockpit and provider-dashboard figures, `rimz stats`, and how the numbers are calculated.
- [Messaging](./messaging.md) — the other half of the loop: reach the agent the sidebar surfaced.
- [Notifications](./notifications.md) — the same cues pushed to your desktop, phone, or a handler when you are off-screen.
- [Theming and pets](./theme.md) — restyle the column, its palette, and the companion.
- [Remote](./remote.md) — the link-health badge and the column rebuilt over SSH.
- [Configuration → sidebar rendering](./configuration.md#sidebar-rendering) — the render cadence and attention knobs.
- [Sidebar internals](../internals/sidebar/sidebar.md) — presence, the ranking contract, and reload.

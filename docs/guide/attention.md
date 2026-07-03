# Attention

One person, a fleet of agents, and only so much attention to spend. Rimz spends it for you: the sidebar surfaces which pane needs you and takes you straight to it, and you answer in the agent's own UI. The whole column arrives triaged, so triage is a glance instead of a scan, and one key walks you through whatever is waiting.

This page is the model behind that order: what counts as needing you, how you act on it, and why the cards sit where they do. The glyphs and rendered frames live in [the interface reference](../interface/sidebar.md), the exact ranking contract in [the internals](../internals/sidebar/sidebar.md#attention-ranking-and-the-cap), and the timing knobs in [configuration.md](../reference/configuration.md#sidebar-rendering).

## What needs you

An agent earns your attention when you are its blocker or its beneficiary. Four signals carry that, in descending pull:

- **An ask (`?`).** The agent stopped mid-task for your answer: a permission, a decision, a clarification. A whole loaded context sits idle and cooling until you reply, so asks carry the most weight.
- **A failure (`!`).** The turn ended in an error, or a running agent has gone silent past the stall window (30 minutes by default), and the work needs a look before it continues. A failure weighs nearly as much as an ask, so the two interleave by how long each has waited rather than by kind.
- **A park (`paused`).** The agent stopped mid-turn on a provider limit. The task is blocked, but there is nothing to answer until the limit recovers, so a park ranks below asks and failures and above all calm work.
- **Calm work, ranked by what it offers.** `success` leads the calm states because it holds a result; `running` needs nothing right now; `idle` is spare capacity and reads last, so a freshly launched agent appends at the bottom instead of reshuffling the list.

Only the ask, the failure, and the park call for you. Everything else is context: how the fleet is doing while nothing needs a hand.

## From a glance to the pane

The whole fleet compresses into one cockpit line at the top of the sidebar. `? 2  ! 1  ⏸ 0  ✓ 8` counts who needs an answer, who needs a look, who is parked, and who has a result; the right side tallies live and idle capacity. A row of zeros with no unread count means nothing needs you, so you can skip the scan entirely.

Below the line, the column is already triaged: the card that needs you next sits at the top of its worktree, a soft wash marks every result, ask, and recovery you have not seen, and the one card that most needs you breathes until you read it. When that lead card scrolls out of view, an `↑ N need you` banner points back to it.

You do not read where to go; you go. Press `n` (or `␣`) to jump to the next thing that needs you, oldest first, and Rimz focuses that agent's pane. The prompt and its safe defaults live in the agent's own UI, where the full context is, so you read and answer there; focusing the pane clears its unread mark, and `N` walks back. The full key table and glyph legend are in [the interface reference](../interface/sidebar.md#jump--the-row-is-the-link).

Glance, jump, answer: that loop is the product. When routine answers start to repeat, you can enrol a resolver to take them ahead of you, in a chain that still ends with you ([resolvers](../internals/agents/resolvers.md)).

## How the column is ordered

Within a worktree, agent cards lead and process rows (shells, builds, servers) form a quiet tail. The cards themselves sort by four questions asked in order: have you seen it, how long has it waited, what does it offer, and, among calm work, is it landed.

### The unread inbox leads

The top of the column is an inbox. A card turns *unread* the moment it enters `waiting`, `failed`, `paused`, or `success`, and stays unread until you focus its pane or mark it read, even after the agent recovers and moves on. Unread outranks every other card because it is exactly the set of events that happened for you: results, asks, and parked recoveries you have yet to see. The inbox holds a stable order, the most pressing kind first (asks, then failures, then parks, then results) and each oldest-first, so the jump key walks it predictably and the card you are about to read never slides out from under you. Reading a card is what releases it from the inbox into the aging order below.

### Urgency rises through the first hour

Time reshapes the rest, measured from each card's last activity, in three windows.

**Inside the first hour, a blocked agent grows more urgent the longer it waits.** One hour is the boundary the agent's own prompt cache crosses: answer within it and the agent resumes warm, past it the resume recomputes what the cache held, and it matches the natural horizon of attention. So an ask, failure, or park doubles its weight as it ages toward the hour: a failure overdue fifty minutes outranks an ask from two minutes ago, and blocked work reads oldest-first, the cheapest order to clear. Calm work keeps a flat weight through the hour, so live agents hold their place while they run.

**Between one hour and twenty-four, everything cools.** Past the hour the cache is gone and the resume costs the same whenever you get to it, so urgency decays steadily instead of climbing. A stale ask still leads stale calm work, but the whole window sinks beneath anything currently hot, so yesterday's unanswered question stops competing with the agent blocked right now.

**Past twenty-four hours, a card sleeps.** It parks in an archive at the back, keeping only its state order, so an archived ask still reads above an archived idle agent without competing against hot or warm work.

The unread inbox is exempt from all three windows: a result you have not seen holds the top regardless of age, and reading it is what lets the card age.

### Teams read as one

A co-launched team is one line of work, so it holds one contiguous block and takes one state derived from its members: any member asking or failed makes the team **blocked**, else a parked member makes it **paused**, else a running member makes it **working**, else a finished member makes it **success**, else it is **idle**. One blocked member lifts the whole block, so a planner waiting on you blocks its coder and reviewer too, whatever they are doing; a team where one member finished while others still run reads as working, because the team is done only when every member is. The block ranks by that derived state on its oldest blocked member's clock and stays contiguous, so teammates sit side by side in their declared role order.

### Git decides among the calm

Attention always outranks git: a worktree with a blocked agent leads whatever its diff looks like. Among worktrees whose agents are all calm, the git verdict answers *is this work landed?* **Dirty** leads, because uncommitted changes are unfinished business; **clean** follows, committed but still to land; **merged** sinks, because content on trunk with every agent resting means the work is done. A worktree with no git verdict sits between clean and merged: no evidence of pending work, and no proof it landed.

### The shape that always holds

Three partitions hold regardless of any score, so the column keeps one stable outline:

- **The unread inbox leads everything.**
- **Agent cards lead their worktree; process rows form the tail.** Shells, builds, and servers are context, so they seat below every agent card.
- **Project worktrees lead; out-of-project panes tail.** Panes outside every project checkout fold into a dim `external` divider that always sorts last, keeping an attention-only tally (`? n` / `! n`) so an out-of-project ask still surfaces.

A cap keeps a busy worktree scannable: each worktree shows up to six rows with a dim `+K more`, trimming only its idle and process tail. Anything active, blocked, parked, finished, or focused stays visible, so a card that might need you never hides behind the count.

## Tuning

Three `[agents.attention]` knobs move the boundaries: `stalled_after_secs` (a silent agent escalates to `!`, 30 minutes), `inactive_after_secs` (hot work ends, one hour), and `archive_after_secs` (a card sleeps, 24 hours). Details in [configuration.md](../reference/configuration.md#sidebar-rendering).

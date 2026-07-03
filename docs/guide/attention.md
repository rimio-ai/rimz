# Attention

Rimz routes attention: the sidebar keeps the card that needs you next at the top, so triage is a glance instead of a scan. This page explains the model behind that order — which states deserve attention, how urgency changes with time, and how teams and git state fold in. The rendered frames and glyph legend live in [the interface reference](../interface/sidebar.md), the exact ranking contract in [the internals](../internals/sidebar/sidebar.md#attention-ranking-and-the-cap), and the timing knobs in [configuration.md](../reference/configuration.md#sidebar-rendering).

## What deserves attention

An agent deserves your attention exactly when you are the blocker or the beneficiary:

- **An ask (`?`).** The agent stopped mid-task for your answer — a permission, a decision, a clarification. Until you answer, a whole loaded context sits idle and cooling, so asks carry the highest weight in the ranking.
- **A failure (`!`).** The turn ended in an error, or a running agent has been silent past the stall window (30 minutes by default) — either way the work needs a look before it can continue. Failures weigh nearly as much as asks, so the two interleave by how long they have waited rather than by kind.
- **A park (`paused`).** The agent stopped mid-turn on a provider limit. The task is blocked, but there is nothing to answer until the limit recovers, so a park ranks below asks and failures while staying above all calm work.
- **Calm work ranks by what it offers you.** `success` leads the calm states — it holds a result; `running` needs nothing right now; `idle` is available capacity and reads last, so a freshly launched agent appends at the bottom instead of reshuffling the list.

## The unread inbox

The top of the column is an inbox. A card turns *unread* when it enters `waiting`, `failed`, `paused`, or `success`, and stays unread until you focus it or mark it read — even if the agent has since recovered and moved on. Unread outranks everything else because it is exactly the set of events that happened *for you*: results, asks, and parked recoveries you have yet to see. Inbox order is oldest-actionable-first and holds still over time, so the `n` key (alias `␣`) that jumps to the next thing that needs you walks it top-down, and the card you are about to read stays where you saw it.

## Urgency and the one-hour window

Time reshapes urgency in three windows, all measured from the card's last activity.

**Inside the first hour, a blocked agent grows more urgent the longer it waits.** One hour is the boundary the agent's own prompt cache crosses — answer within it and the agent resumes warm; past it the resume recomputes what the cache held — and it matches the natural horizon of human attention. So an ask, failure, or park doubles its weight as it ages toward the hour: a failure overdue fifty minutes outranks an ask from two minutes ago, and blocked work reads oldest-first, which is the cheapest order to clear. Calm work keeps a flat weight through the hour, so live agents hold position while they work.

**Between one hour and twenty-four, everything cools.** Past the hour the cache is gone and the resume costs the same whenever you get to it, so urgency decays steadily instead of climbing. Stale asks still lead stale calm work, but the whole window sinks beneath anything currently hot — yesterday's unanswered question stops competing with the agent blocked right now.

**Past twenty-four hours, a card sleeps.** It parks in an archive at the back, keeping only its state order — an archived ask still reads above an archived idle agent — without competing against hot or warm work.

The unread inbox is exempt from all three windows: a result you have yet to see holds the top regardless of age, and reading it is what releases the card into the aging bands.

## Teams read as one

A co-launched team is one line of work, so it holds one contiguous block and takes one state derived from its members: any member asking or failed makes the team **blocked**, else any parked member makes it **paused**, else any running member makes it **working**, else a finished member makes it **success**, else it is **idle**. One blocked member lifts the whole block — a planner waiting on you blocks its coder and reviewer too, whatever they are doing — and a team where one member finished while others still run reads as working, because the work is done when the team is done. The block ranks by that derived state on the oldest blocked member's clock and stays contiguous, so teammates sit side by side in their declared role order.

## Git decides among the calm

Attention always outranks git: a worktree with a blocked agent leads whatever its diff looks like. Among worktrees whose agents are all calm, the git verdict answers *is this work landed?* — **dirty** leads, because uncommitted changes are unfinished business; **clean** follows, committed but still to land; **merged** sinks, because content landed on trunk with every agent resting means the work is done. A worktree without a git verdict sits between clean and merged: no evidence of pending work, but no proof it landed either.

## Structure that holds

Three partitions hold regardless of score, so the column keeps one stable shape:

- **The unread inbox leads everything.**
- **Agent cards lead their worktree; process rows form the tail.** Shells, builds, and servers are context, so they seat below every agent card.
- **Project worktrees lead; out-of-project panes tail.** Panes outside every project checkout fold into the dim `external` divider that always sorts last, keeping an attention-only tally (`? n` / `! n`) so an out-of-project ask still surfaces.

## Tuning

Three `[agents.attention]` knobs move the boundaries: `stalled_after_secs` (silent running escalates to `!`, 30 minutes), `inactive_after_secs` (hot work ends, one hour), and `archive_after_secs` (a card sleeps, 24 hours). Details in [configuration.md](../reference/configuration.md#sidebar-rendering).

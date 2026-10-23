# Attribution

This page owns how RimZ credits effort to the agents that worked a lane: which records a report admits, how several sessions fold into one contributor, and where each figure comes from. The command surface, `rimz agents attribution`, is [agents.md](../../reference/cli/agents.md#attribution). The fold and the report are [`agents/attribution.rs`](../../../crates/rimz/src/agents/attribution.rs), and lane lifetimes resolve in [`worktree::lane_lifetimes`](../../../crates/rimz/src/worktree.rs).

Attribution reads the audit rollup (`RuntimeScope::Audit`), so a teammate stays eligible after its pane exits and the runtime projection hides it from live cards. No multiplexer observation participates in correctness.

The seat fold has five consumers. Only the attribution report selects a lane and applies a branch. The others call `attribution::slot_groups`, which applies lifetime admission alone.

| Consumer | Call site | What it takes from the fold |
| --- | --- | --- |
| `rimz agents attribution` | `cli/agents_cmd/attribution.rs` → `attribution::build` | the whole report |
| `rimz agents show` | `cli/agents_cmd/show.rs` → `slot_groups`, then `spending::slot_effort` | seat effort and active time |
| `rimz agents list --json` | `cli/agents_cmd/list.rs` → `slot_groups` | seat active time |
| `rimz teams list`, `rimz teams show` | `cli/teams/list.rs::live_instances` → `slot_groups` | member cost |
| the sidebar's finished-cohort receipt | `sidebar/refresh/cohort_spend.rs::compute_cohort_effort` → `slot_groups` | cohort and seat totals |

## Selecting records

A report runs three filters in order: lane selection, lifetime admission, and branch membership. Lifetime admission is the only one every consumer applies.

### Lane selection

Lane selection picks root records, and launched children join through their durable parent link regardless of their own lane stamp. The CLI handler (`cli/agents_cmd/attribution.rs`) resolves a channel filter as `agents list` does: the `SCOPE` when given, else the caller's current channel unless `--all` is set. With a filter, it takes the roots `address::agent_in_worktree` places in it. Without one, it takes the roots whose lane path is the caller's checkout, or every root under `--all`. It sets provider-native subagents aside for the [subagent breakdown](#the-subagent-breakdown). The branch a report filters on also defaults from the selection, as the reference's `--branch` row describes.

### Lifetime admission

A lane lifetime decides which records still belong to their checkout. `worktree::lane_lifetimes` resolves one `LaneLifetimes` snapshot per report, keyed by each record's `AgentState::lane_path` (its `checkout`, else its `worktree_path`):

| Checkout at that path | `LaneLifetime` | Admits |
| --- | --- | --- |
| does not exist | `Removed` | nothing |
| holds a [worktree marker](../harness/worktrees.md#the-one-rule-rimz-touches-only-what-it-marked) | `Since` | records registered at or after the marker's `created_at` |
| exists without a marker | `Unbounded` | everything |
| cannot be read | `Unreadable` | nothing. CLI handlers warn once per path on stderr (`render::warn_unreadable_lanes`). The sidebar logs the omission and publishes the remaining figures. |

A record with no lane path is always admitted. The snapshot is a required parameter of the fold, so no consumer can skip admission.

Admission judges each record alone, by its own lane path and its own `registered_at`, before the fold separates roots from launched children. An excluded parent is not revived by an admitted child, and an admitted child does not follow an excluded parent out. A record with no `registered_at` has no birth to compare, so a marked lane excludes it. Provider-native subagents are never admitted directly: they reach a report only through an admitted parent.

Exclusion rewrites and deletes nothing. The audit records, the event log, and provider transcripts all remain, and `agents show` still reports an excluded session addressed by name, through the single-record fallback of `report::slot_records_for_agent`. `AttributionScope::since` publishes the lifetime boundary when the selected roots share one marked checkout (`LaneLifetimes::common_since`). It resolves from the selection, not from what survived admission, so a lane that admission emptied still names the timestamp that emptied it.

### Branch membership

Attribution alone filters by `AttributionScope.branch`, after lifetime admission and on roots only. A root qualifies when its `worktree_branches` set holds the branch, and launched children follow their admitted parent without a branch check. How the rollup accepts a branch into that set is [model.md](./model.md#the-rollup).

Figures stay whole-identity, so an identity observed on several branches reports its full effort under each. The rollup rebuilds `worktree_branches` from the active event log plus carryover, never from rotated archives. A carried record whose branches were never observed there has an empty set and fails every branch filter.

## The seat fold

One contributor can span several session records. Compaction continuations and `/clear` conversations mint fresh session ids while the contributor keeps its seat, so the fold keys each record by `SlotKey`: its channel, its provider kind, and the first stable slot it carries, in this order:

1. Team and role (`Slot::Role`).
2. Launch group and ordinal (`Slot::Cohort`).
3. An explicit name (`Slot::Name`).
4. The pane id (`Slot::Pane`).
5. The session id (`Slot::Session`).

Records with equal kind and slot in different channels stay separate seats. Within a seat, records sort by `registered_at`, then `last_activity`, and the newest record supplies the member's handle, role, model, and team.

A pane-backed child joins the seat of the parent that launched it (`attribution::fold_seats`), and the child's own continuations fold into one child slot inside that seat. The parent must be among the admitted roots that passed the branch filter. A child whose parent is not there, because the parent left the audit rollup or a filter excluded it, keeps its own slot under `Slot::Orphan`. Attribution drops an orphan slot, since it holds no parent identity, while `slot_groups` returns it to the other consumers as a group of its own.

A member's figures cover two different extents. Tokens, `cost_usd`, model rows, and active time are all-in: the seat plus every child it spawned. Asks, tool calls, compactions, and messages count the seat's own sessions only.

### The subagent breakdown

The breakdown groups a member's children by kind, merging provider-native and pane-backed children. A native child's kind comes from its `task`, and a launched child's from the `profile` of its newest continuation. `attribution::subagent_type` trims and lowercases the label, then keeps it only when it is non-empty, at most 24 characters, and free of whitespace and control characters. Any other label, or none, groups as `other` (`task: null` in JSON), so a task description is never exposed.

Each native child id counts once: the rollup's subagent records under the seat's sessions, plus any child id the parent's transcripts carry without a record. Each launched child slot counts once, however many continuations it holds. Kinds sort by cost descending (unpriced last), then count descending, then kind name, with `other` last at ties.

The breakdown is a share of the member's all-in figures and never adds to them. Native child spend comes from the parent session's spend transcripts ([spending.md](./spending.md#session-and-seat-folds)), and launched child spend from each child's own. Child transcript entries keep their child id through deduplication, so cost can group by kind without changing the member's total.

## Where the figures come from

Each figure keeps its source boundary.

| Figure | Source |
| --- | --- |
| identity, `registered_at`, `last_activity`, presence, `sessions`, tool calls, compactions | the audit rollup records of the seat |
| prompts from you, messages from teammates, asks and answered asks | RimZ's append-only conversation transcript (`transcript::read_all`), per session. An ask is answered when an `Answer` entry carries its id. |
| messages to teammates | the `from` handle on teammates' received `Message` entries, matched against each member's `petname::sender_handle` (role, name, kind) and channel, never the displayed list handle. Best-effort. |
| tokens, cost, model rows | each session's spend transcripts, parsed once by its adapter and priced through the shared price book (`spending::slot_effort_breakdown`) |
| member active seconds | per-session active-time sidecars, projected by `store::active_time::display_secs_for_keys` under the configured silence grace and summed across the seat and its launched children by `attribution::seat_active_secs` ([model.md](./model.md#activity-clocks)). A native child pulses its parent's record. |
| kind active seconds | the same sidecars, summed over every continuation of each launched child slot in the kind. Native children contribute no separate time, including in a kind that mixes both origins. |
| model `tokens_per_sec` | transcript `CachedEntry.timing` through `SlotEffort`: summed timed output over summed milliseconds per model, rounded. Report rows fold the raw counters over retained members, never member rates. |
| member `fast` | transcript `CachedEntry.fast` (wire key `f`) through `SlotEffortBreakdown.own_fast`: set when any counted turn of the seat's own sessions ran fast. Native and launched children never set it, and neither does the launch record. |
| kind `fast` | set when any counted turn of a child grouped into the kind ran fast |

A prompt counts as from you when `TranscriptEntry::origin` reads it as human. System nudges carry a sender stamp and read as RimZ. A nudge recorded without the stamp, which older logs hold, reads as human and counts. The transcript reader drops legacy paste-fragment prompts ([transcript.md](../harness/transcript.md#the-log)), so they never count.

Model rows split the member's all-in tokens and cost by transcript model id, from the same deduplicated entries. A blank or missing id joins the unnamed row (`unknown` in the panel, `null` in JSON), and a row with no tokens and no cost is omitted. Rows sort by cost descending (unpriced last), then tokens, then model id. The pricing read itself is [spending.md](./spending.md#token-pricing).

A missing time, cost, or throughput reads `null`, never zero:

- `active_secs` is `null` when none of the records it sums has a retained sidecar. Runtime GC can remove a sidecar before the audit record or transcript disappears.
- `cost_usd` is `null` when no counted entry carries a finite positive price, so a transcript without pricing reads `null`, and so does a priced total of zero.
- A kind's `active_secs` is `null` without a retained launched-child sidecar, so a kind with only native children always reads `null`.
- `tokens_per_sec` is `null` for an untimed model.

Report throughput is scoped to the retained members, not to the account-wide windows that stats use ([spending.md](./spending.md#response-timing)). Token splits and counters have no `null`: a missing transcript reads as a zero split, and a session absent from the conversation transcript counts zero.

## Membership

A folded seat stays in the report when it has a contribution, or when it opened a turn and the output is not Markdown (`--md` sets `require_contribution`). A contribution is any of: active time above zero, asks, tool calls, compactions, messages (from you, from teammates, or to teammates), tokens, a cost, or a subagent (`AttributionMember::has_contribution`).

A seat opened a turn when any of its records carries `turn_started_at`. That field is durable in the rollup, so a contributor stays eligible after GC removes its active-time sidecar, including on adapters whose transcripts supply no spend or named tools. A promptless launch has no `turn_started_at` until its first real turn, and a launch with a prompt stamps it at once. Which edges advance the boundary is [model.md](./model.md#edges). A completed compaction is a contribution in itself: it raises `compaction_count` even on a launch that never opened a turn, and that seat then appears in every output, Markdown included.

Retained members group by the team of their newest record: teams in name order, then one group for members with no team. Within a group, members order by launch ordinal, then first registration, then handle.

## See also

- [agents.md](../../reference/cli/agents.md#attribution): the `rimz agents attribution` command and its flags.
- [model.md](./model.md): the rollup attribution reads.
- [worktrees.md](../harness/worktrees.md): the marker whose `created_at` bounds a lane's lifetime.
- [spending.md](./spending.md): transcript spend and pricing.

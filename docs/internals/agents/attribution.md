# Attribution

This page owns how RimZ credits effort to the agents that worked a lane: which records a report admits, how several sessions fold into one contributor, and where each figure comes from. The command surface, `rimz agents attribution`, is [agents.md](../../reference/cli/agents.md#attribution). The code is [`agents/attribution.rs`](../../../crates/rimz/src/agents/attribution.rs), with lane lifetimes resolved by [`worktree::lane_lifetimes`](../../../crates/rimz/src/worktree.rs).

Attribution reads the audit rollup (`RuntimeScope::Audit`), so a teammate stays eligible after its pane exits and the runtime projection hides it from live cards. No multiplexer observation participates in correctness. The [seat fold](#the-seat-fold) this page describes has four consumers: the attribution CLI, `agents show` seat effort, `teams show` member cost, and the sidebar's finished-cohort receipt.

## Selecting records

A report runs three filters in order: lane selection, lifetime admission, and, for attribution alone, branch membership.

**Lane selection.** Lane filtering applies to root records. Launched children then join through their durable parent link regardless of their own lane stamp.

**Lifetime admission.** A lane lifetime decides which of the selected lane's records still belong to it. `worktree::lane_lifetimes` resolves one [`LaneLifetimes`](../../../crates/rimz/src/agents/attribution.rs) snapshot per report from the distinct `worktree_path` values the records carry:

| Checkout at that path | Admits |
| --- | --- |
| does not exist | nothing |
| holds a [worktree marker](../harness/worktrees.md#the-one-rule-rimz-touches-only-what-it-marked) | records registered at or after the marker's `created_at` |
| exists without a marker | everything (unbounded) |
| cannot be read | nothing; CLI handlers warn once per path on stderr, and the sidebar logs the omission and publishes the remaining figures |

The snapshot is a required parameter of the shared fold, so no consumer can skip admission. Admission judges each record by its own stamped checkout and its own `registered_at`, before the fold separates roots from launched children: an excluded parent is not revived by an admitted child, and an admitted child does not follow an excluded parent out. A record with no `registered_at` has no birth to compare and is excluded from a marked lane. Provider-native subagents and peers are never filtered directly; they reach a report only through an admitted parent.

Nothing is rewritten or deleted. The audit records, the event log, and provider transcripts all remain, and `agents show` still reports an excluded session addressed by name through its single-record path. `AttributionScope::since` publishes the lifetime boundary when the selected roots share one marked checkout. It resolves from the selection, not from what survived admission, so an emptied lane still names the timestamp that emptied it.

**Branch membership.** `AgentState.worktree_branches` accumulates the branches a session was observed on, and `worktree_branch` is the latest (the acceptance rule is in [model.md](./model.md#the-rollup)). Launch records stamp the real Git branch, and every lifecycle event carries both `worktree_path` and `worktree_branch`. Attribution alone applies `AttributionScope.branch`, after lifetime admission and to roots only: a root with no evidence for the branch is excluded, and launched children follow their admitted parent without a branch check. The other seat-fold consumers are scoped by lifetime only.

Figures stay whole-identity, so an identity observed on several branches can report its full effort under each. The rollup cache rebuilds branch membership from the active log plus carryover, not from rotated archives; there is no archive backfill, and legacy carryover records without branch evidence are excluded under a branch filter.

## The seat fold

One contributor can span several session records. Compaction continuations and `/clear` conversations mint fresh session ids while the contributor keeps its seat, so the fold groups records by provider kind plus the first stable slot available, in this order: team and role, launch group and ordinal, explicit name, pane id, then session id.

Pane-backed children join the seat that launched them, with child continuations deduplicated, and merge with provider-native children into one subagent breakdown grouped by task. An orphan whose parent has left the audit rollup is omitted. A member's `cost_usd` and token split are all-in across its seat and every child; active time, asks, tool calls, compactions, and messages count the seat only.

## Where the figures come from

Each figure keeps its source boundary.

| Figure | Source |
| --- | --- |
| identity, timestamps, tool calls, compaction counts, provider-subagent parent and type | the audit rollup |
| prompts, agent messages, asks | RimZ's append-only conversation transcript, per session |
| sent messages | sender handles on received agent messages, best-effort |
| tokens and dollars, model rows | each session's provider transcript, parsed once by its adapter and priced through the shared price book |
| estimated active seconds | per-session active-time sidecars, under the configured silence grace ([model.md](./model.md#activity-clocks)) |

Matched system nudges are sender-stamped and excluded from prompt counts. System nudges written before sender stamping existed cannot be told apart from user prompts.

"From you" counts exclude legacy paste-fragment prompts because the [transcript reader](../harness/transcript.md#the-log) drops them.

Provider-native child spend arrives inside the parent's transcript; pane-backed child spend comes from each child's own transcript and folds into the same member total. Child transcript entries keep their child id through deduplication, so cost can group by durable task without changing the all-in parent total. A missing, long, or whitespace-bearing task label groups as `other`, so task descriptions are never exposed. The same deduplicated entries split all-in tokens and cost into model rows by transcript model id; blank or missing ids share the unnamed row, and a row with neither tokens nor a price is omitted. The pricing read itself is [spending.md](./spending.md#token-pricing).

A missing source reads `null`, never zero. Runtime GC can remove an active-time sidecar before the audit record or transcript disappears, and a transcript can lack pricing.

## Membership

Within the selected scope, a seat drops out only when it never opened a turn and has no active time, asks, messages, tool calls, compactions, subagents, tokens, or recorded cost. The rollup's durable `turn_started_at` keeps a contributor eligible after GC removes its active-time sidecar, including adapters whose transcripts supply no spend or named tools.

A promptless launch leaves `turn_started_at` unset until a real turn opens; a launch with a prompt opens a turn at once. A resting registration or a successful compaction advances the boundary only after a turn has opened ([model.md](./model.md#edges)), so binding, adoption, reset, failure, and reaping never turn an untouched launch into a contributor. The panel and JSON output keep opened-turn rows that have no statistics; Markdown output requires a recorded contribution.

## See also

- [agents.md](../../reference/cli/agents.md#attribution): the `rimz agents attribution` command and its flags.
- [model.md](./model.md): the rollup attribution reads.
- [worktrees.md](../harness/worktrees.md): the marker whose `created_at` bounds a lane's lifetime.
- [spending.md](./spending.md): transcript spend and pricing.

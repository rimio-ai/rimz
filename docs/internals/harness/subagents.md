# Agent-launched subagents

> One agent delegating a bounded prompt to another. This page owns the child lifecycle: the agent-only doorway, what a launch desugars to, the ancestry stamp and its depth cap, how the caller-scoped verbs decide who its children are, and the boundary with the provider-native children that share the name. The supervised run underneath a child is [scripting.md](./scripting.md); the launch core it rides on is [fleet.md](./fleet.md).

## Two things are called a subagent

RimZ shows both under one product term and nests both under the same card, so the first thing to get straight is that they are different mechanisms with different truth.

A **provider-native subagent** is the agent's own child, running headless inside the parent's process. RimZ learns it exists only from `SubagentStarted` and `SubagentStopped` hook signals and folds it into the rollup as a child row. It has no pane, no run record, and no address — [model.md](../agents/model.md) owns it.

A **launched subagent** is a full RimZ agent: its own pane, its own provider process, its own durable run record, petname, and address. `rimz subagents` creates it. This page owns it.

One field separates them, and both predicates live in [`agents/state.rs`](../../../crates/rimz/src/agents/state.rs):

| Predicate | Test | Means |
| --- | --- | --- |
| `is_launched_child` | `parent_agent_id.is_some() && launch_depth.is_some()` | RimZ launched it; it has a pane and a run |
| `is_provider_subagent` | `parent_agent_id.is_some() && launch_depth.is_none()` | the provider launched it inside its own turn |

`launch_depth` is the discriminator: only a RimZ launch path stamps it. A row carrying a parent but no depth came from a hook, and every caller-scoped verb on this page filters it out.

Both kinds are flattened to a root ancestor, but by different code at different times: a launched child is flattened by the launcher when its stamp is minted, while a provider-native child is flattened by the store writer as it adopts the hook observation. Both then attach to the same parent card through one chokepoint, keyed on `parent_agent_id` alone, so the sidebar's subagent section is deliberately origin-blind and structurally one level deep.

Because a launched child is also a real agent, it would otherwise render twice — once nested, once as its own top-level card. The suppression happens in the pane projection rather than in the attach: a pane whose agent carries a parent binds as a *nested* agent, which records the pane without emitting a row, so long as the parent still has a row of its own. When the parent's row is gone and the child is still live, the child is promoted to a top-level row instead — the alternative would be a live pane that renders nowhere. So a launched child has either a row or a nested entry, never both.

The two also differ in reach: a pane-backed child is a peer for `rimz message` and `rimz pane` even while it renders under a parent card, whereas a provider-native child is display-only and not addressable at all.

## The doorway

Launch, fanout, and the caller-scoped `list`, `wait`, and `stop` verbs refuse outside a RimZ-launched agent ([`cli/subagents/mod.rs`](../../../crates/rimz/src/cli/subagents/mod.rs)). The test is one environment variable: `RIMZ_AGENT_KIND`, exported into every agent pane by the [exec wrapper](./fleet.md#the-exec-wrapper). A user shell has no such variable and gets pointed at `rimz agents` or `rimz teams` instead. The read-only `specs` catalog is deliberately exempt because it only loads machine configuration and does not depend on caller ancestry.

This is a usability boundary, not a security one — the same launch is expressible as `rimz agents … -p --bg`, and `subagents` exists so a delegating agent does not have to choose the supervision flags correctly. What the doorway buys is that every child launched through it is *uniformly* supervised, background, deadlined, and self-cleaning.

## Children cannot delegate again

Every prompt launched through `rimz subagents`, including every fanout entry, ends with a `<system_reminder>` telling the child to complete the work directly and not launch more agents. Where the provider exposes a verified native restriction, the exec compiler independently disables its delegation tool after profile arguments and configured environment have been applied:

| Provider | Process restriction |
| --- | --- |
| Claude | merges the profile's disallowed-tool values into one final `--disallowedTools` occurrence and denies `Agent` |
| Codex | replaces every `features.multi_agent` config override with one final false override |
| OpenCode | merges `"task":"deny"` into `OPENCODE_PERMISSION`, preserving the profile's other permission rules |
| Pi | prompt-only: Pi has no built-in subagents, and disabling all third-party extensions would remove unrelated child tools |
| Other adapters | prompt-only until a provider-native restriction is verified |

The restriction marker is internal to this doorway. An agent launched with `rimz agents` or as a team member keeps its normal provider tools even when it has launch ancestry.

This complements, rather than replaces, `max-launch-depth`. The depth check blocks a child from launching another process through RimZ, while the process restriction blocks the provider's native delegation tool. The prompt reminder covers every adapter and states both rules in the child's task context.

## What a launch desugars to

`rimz subagents <spec> <prompt>` builds an ordinary `AgentLaunchArgs` and hands it to the same background supervised launcher a fanout uses. The sugar is entirely in the defaults and the optional join:

| Field | Value | Why |
| --- | --- | --- |
| `print` | always `true` | a child is one bounded turn, never a session |
| `bg` | always `true` | single launch and fanout share one background-launch composition |
| join | absent unless `--wait[=DURATION]` | the parent normally keeps moving; the optional duration limits only its join |
| wrapper self-cleanup | on unless `--keep` | the child reclaims itself from its durable outcome after the parent returns |
| `timeout` | `--timeout`, else `[agents.subagents] timeout`, default `30m` | an unattended child must not run forever |
| `keep` | `--keep`, default false | the pane closes itself on completion |
| everything else | default | see the omissions below |

The default has the user-visible behavior of `rimz agents <spec> <prompt> -p --bg --timeout 30m`, while additionally arming the in-pane wrapper's self-cleanup because this doorway omits retries and verification. `--wait` leaves that launch unchanged, then passes the minted petname to the shared batch-wait path; `--wait=DURATION` adds a caller-side join deadline without changing the child's timeout. Everything after that point — the run record, the completion fold, the wake socket, pane reclamation — is [scripting.md](./scripting.md) unchanged, which is the reason this page does not restate any of it.

The doorway deliberately omits `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--top-level`, `--resume`, placement flags, output and input formats, retries, and verification. Each of those needs a decision the delegating agent is not well placed to make, and each is still reachable by calling `rimz agents` directly. `specs` lists kinds, profiles, and configured commands but never teams, because one launch produces one agent rather than a cohort.

## Single launch and fanout share one composition

`rimz subagents fanout` accepts a JSON array and desugars every entry through the same `SubagentLaunchArgs::into_agent_launch` path above. Task `timeout` wins over the fanout flag, which wins over the configured default. Fanout-level `keep` applies uniformly; per-task foreground, retention, and passthrough argv are not part of the data format.

Parsing, required fields, timeout syntax, and duplicate explicit names are validated across the entire array before the first side effect. Pane opens then happen sequentially in the caller process. This avoids racing two backend split operations against the same ambient pane, while the child processes themselves run in parallel as soon as each pane opens.

The supervised runner's background outcome carries the minted petname and run ID back to the subagents command. Single launch prints the identity directly, while fanout collects every identity. This avoids rediscovering children from a before/after store snapshot, which could confuse another launch racing in the same family.

By default, either form returns after launching; fanout can also render its collected run IDs as JSON. With `--wait`, single launch passes one petname and fanout passes its exact collected set to `agents_cmd::wait_agent_batch`. The normal multi-target wait renderer, caller-side deadline, and aggregate exit code therefore own both waited result paths.

A runtime failure during that loop aborts the remaining launches and reports every child already started. Those children are not rolled back: their durable run records, deadlines, self-cleanup, and caller-scoped `wait`/`stop` behavior remain the ordinary supervised lifecycle. Validation failures are different — because desugaring completed before the loop, they launch nothing.

## Ancestry, depth, and flattening

Ancestry resolves in [`plan.rs`](../../../crates/rimz/src/harness/plan.rs) as step 4 of [the compile path](./fleet.md#from-spec-to-panes) — **before** provider preflight, worktree creation, store append, or any mux action, so a refusal leaves nothing behind.

`resolve_launch_caller_from_env` finds the launching agent's durable row. It reads `RIMZ_AGENT_KIND` and `RIMZ_AGENT_ID`, then matches the row whose `launch_id` equals that id, with the kind corroborating the match so a stale cross-provider environment cannot attach a child to the wrong row. Only an agent process with no launch id at all — one that survived an upgrade — may fall back to an unambiguous live pane stamp.

`resolve_launch_ancestry` then produces the stamp:

```text
current_depth = caller.launch_depth ?? 0
current_depth >= max_depth               → refuse
parent_agent_id = caller.parent_agent_id ?? caller.agent_id   ← flattened
launch_depth    = current_depth + 1                           ← true
```

That third line is the whole flattening rule. A child inherits *its caller's* parent when the caller has one, and only otherwise points at the caller. So a grandchild is stamped with the same top-level parent as its parent, while `launch_depth` keeps counting honestly. True depth stays available for the cap; display ancestry collapses to one level of nesting under the original top-level agent.

`[agents] max-launch-depth` ([`config/agents.rs`](../../../crates/rimz/src/config/agents.rs)) defaults to `1`. With an unstamped top-level agent reading as depth 0, that permits children and refuses grandchildren.

Fanout does not change that accounting: each array entry is one ordinary launch from the same caller. At the default cap, a top-level agent may fan out, but any child attempting its own fanout is refused before the first task launches.

`--top-level` is the escape hatch, and it short-circuits ahead of everything above: no caller resolution, no store projection read, no depth check, no parent stamp. It is a `rimz agents` flag only. `rimz subagents` never sets it, so a child launched through this doorway cannot escape ancestry — which is what makes the depth cap meaningful against an agent that is itself writing the command line.

Both refusals are `LaunchAncestryError`, and their messages are written for a reader that is itself an agent: each states the refusal, explains the limit, and ends with *do not retry this command*. That phrasing is load-bearing, and so is an omission — the depth message never mentions `--top-level`, which a test pins deliberately. An agent that reads "launch refused" without a terminal instruction tends to retry with a variation, and an agent handed the name of the flag that bypasses the cap tends to use it. The message closes both doors.

## Who counts as the caller's children

`list`, `wait`, and `stop` all resolve membership the same way: `resolve_launch_caller_from_env` for the caller, then [`target::launched_children`](../../../crates/rimz/src/harness/target.rs) for the set. Sharing that resolver has one wart worth knowing before you chase it as a bug: a read-only `rimz subagents list` that cannot identify its caller reports `launch refused: …`, because the error text belongs to the launch path it borrows. That function keeps rows where `is_launched_child()` holds and `parent_agent_id` equals the caller's `agent_id`, sorted by registration.

Two consequences fall out of the flattening rule. At the default depth of one, that set is exactly the direct children. Above depth one, the top-level agent sees every descendant, and an intermediate child sees none of its own — because the grandchildren carry the top-level agent's id, not the intermediate's. The caller-scoped verbs follow durable display ancestry, not the true tree.

Which projection each verb reads is the other half:

| Verb | Projection | Effect |
| --- | --- | --- |
| `list`, `wait` | `RuntimeScope::Audit` | includes ended children, so completed work stays listable and joinable |
| `stop` | alive snapshot, `ended_at.is_none()` | only live children can be stopped |

`wait` with no names joins every child that has a run record; `--any` first filters to children whose newest run is still non-terminal, since reporting "the first to finish" is meaningless against an already-terminal set. Named children resolve against the child set alone, so an address that names some other agent in the room fails with ``not one of this agent's subagents`` rather than reaching outside the family. Each child's newest run is matched by `agent_id`, falling back to `agent_name`, taking the latest `started_at`.

## The lifecycle, end to end

Normal completion does not depend on the parent issuing a stop. That is worth stating plainly, because the shape of the API invites the opposite assumption.

1. The parent launches; the ancestry stamp and depth check pass; a run record and pane are created. By default the petname prints immediately; with `--wait`, the parent then joins the result.
2. The child runs its one turn. Its hooks fold a terminal status into the run record.
3. The child's own in-pane wrapper notices the terminal record, terminates the provider, and closes the pane independently of the parent. A surviving blocking parent also attempts the same idempotent reclamation after its wait returns; `wait` itself remains only a reader ([scripting.md § Reclaiming the run pane](./scripting.md#reclaiming-the-run-pane)).
4. The run record survives, so `list` and `wait` still report the outcome after the pane is gone.

`wait` is a reader over step 4 and has no part in step 3; a parent that never calls it changes nothing. The 30-minute deadline is the backstop for a child whose wrapper cannot finish the job: it is stored on the run record and enforced by the elected room producer, so it fires whether or not the parent is still alive. `--keep` opts out of step 3 and leaves reclamation to `rimz subagents stop` or `rimz gc`.

The one place the parent does reach its children is `stop`. Stopping a parent through `rimz agents stop` — or through `rimz teams stop` reaching that parent — stops its live pane-backed children first, so a cascade cannot strand a child whose only reason to exist was the parent's turn.

## What v1 leaves out

`restart` and `resume` are absent by design: the durable run record does not retain every launch argument needed to reproduce the supervised deadline, wait, and self-close contracts, and a partial reproduction would silently change the child's lifecycle. Relaunching the same spec and prompt is the supported path, which matches the model agents already have for their native Agent tool.

A child is addressable as `@<petname>`, but a supervised print-mode provider is not an interactive message consumer, so mid-run steering is not a contract to depend on. A message can park against the address; v1 does not resume a finished child to consume it.

## See also

- [scripting.md](./scripting.md): the supervised run every child is, including the wrapper self-close and the producer-enforced deadline.
- [fleet.md](./fleet.md): the launch, address, and reclaim machinery, and where ancestry sits in the compile path.
- [model.md](../agents/model.md): the rollup, and the provider-native subagent rows this page's predicates exclude.
- [cli/subagents.md](../../reference/cli/subagents.md): the user-facing command and flag surface.

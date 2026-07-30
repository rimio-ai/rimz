# Agent-launched subagents

> One agent delegating a bounded prompt to another. This page owns the child lifecycle: the agent-only launch and lifecycle verbs, what a launch desugars to, the direct-parent stamp and no-further-launch rule, how the caller-scoped verbs decide who its children are, and the boundary with the provider-native children that share the name. The supervised run underneath a child is [scripting.md](./scripting.md); the launch core it rides on is [fleet.md](./fleet.md).

## Two things are called a subagent

RimZ shows both under one product term and nests both under the same card, so the first thing to get straight is that they are different mechanisms with different truth.

A **provider-native subagent** is the agent's own child, running headless inside the parent's process. RimZ learns it exists only from `SubagentStarted` and `SubagentStopped` hook signals and folds it into the rollup as a child row. It has no pane, no run record, and no address — [model.md](../agents/model.md) owns it.

A **launched subagent** is a full RimZ agent: its own pane, its own provider process, its own durable run record, petname, and address. `rimz subagents` creates it. This page owns it.

One field separates them, and both predicates live in [`agents/state.rs`](../../../crates/rimz/src/agents/state.rs):

| Predicate | Test | Means |
| --- | --- | --- |
| `is_launched_child` | `parent_agent_id.is_some() && launch_depth.is_some()` | RimZ launched it; it has a pane and a run |
| `is_provider_subagent` | `parent_agent_id.is_some() && launch_depth.is_none()` | the provider launched it inside its own turn |

The field combination is the discriminator. A parent plus `launch_depth` comes from `rimz subagents`; a parent without it came from a hook, and every caller-scoped verb on this page filters that provider-native row out. A peer launched through `rimz agents` carries `launch_depth` without a parent and matches neither predicate.

A launched subagent points directly at its caller, which is necessarily top-level because subagents cannot launch again. Provider-native ancestry is flattened by the store writer as it adopts hook observations. Both then attach to the same parent card through one chokepoint, keyed on `parent_agent_id` alone, so the sidebar's subagent section is deliberately origin-blind and structurally one level deep.

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

The restriction marker is internal to this doorway. A peer launched with `rimz agents` or as a team member keeps its normal provider tools even when it carries a launch generation.

The launch planner independently rejects any `rimz agents` or `rimz subagents` call whose durable caller is a pane-backed child. That check blocks the child from launching another RimZ process, while the process restriction blocks the provider's native delegation tool. The prompt reminder covers every adapter and states both rules in the child's task context.

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

The default has the user-visible behavior of `rimz agents <spec> <prompt> -p --bg --timeout 30m`, while additionally arming the in-pane wrapper's self-cleanup because this doorway omits retries and verification. `--wait` leaves that launch unchanged, then passes the minted petname to the shared single-name wait path; `--wait=DURATION` adds a caller-side join deadline without changing the child's timeout. Everything after that point — the run record, the completion fold, the wake socket, pane reclamation — is [scripting.md](./scripting.md) unchanged, which is the reason this page does not restate any of it.

The no-delegation reminder rides a provider-native append-system-text launch flag for Claude, Qwen, and Droid, after any caller-supplied append text. Adapters without that capability keep the same tag-wrapped reminder at the end of the user prompt. The process compiler owns this choice at the adapter boundary, beside the native delegation lockdown, so preflight and the eventual exec compile the same provider argv.

## Pane zones

`RunPlacement::SubagentZone` keeps the subagent doorway outside the general placement/config surface while giving its panes a stable home. For a solo caller, the first child splits right from the caller and later children stack against the newest live pane-backed child. For a configured team member, the first child opens a `<view> subagents` companion tab and children launched by any member of that team cohort stack against the newest child there. Zellij uses its native pane stack; tmux maps the same seam to vertical splits. A failed solo-column split falls back to the companion tab, then to a generic run tab if that view cannot open. If an existing team companion cannot accept another pane, the overflow child likewise opens in a generic run tab instead of failing the run or duplicating the companion.

Zone mutation is serialized per workspace across the mux open and wrapper-bind wait. Anchor selection joins durable child ancestry and pane bindings with one authoritative mux pane list, so an ended `--keep` child remains an anchor while its pane is live and a dead pane never does. Before creating a team companion tab, the same mux reading reuses a live tab with the expected base view name after removing its sidebar-managed status glyph, including a child pane that has opened but not bound durably yet. If authoritative placement truth is unavailable, the child degrades to a generic run tab; placement never gates the durable run.

The wrapper records its binding asynchronously after the mux starts it, so a subagent launch polls the durable rollup for up to three seconds before releasing the zone lock and returning. Sequential fanouts therefore observe the preceding child's anchor in the normal path. A wrapper that does not bind within the cap does not fail its launch: a team launch can still find the live companion view from mux truth, while a solo launch takes the no-anchor strategy.

The doorway deliberately omits `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--resume`, placement flags, output and input formats, retries, and verification. Each of those needs a decision the delegating agent is not well placed to make, and each is still reachable by calling `rimz agents` directly. Its launch resolution and `specs` catalog read `[subagents.profiles]`, while `rimz agents` reads `[agents.profiles]`; a wrong-doorway profile names both sections in the error. Commands and teams remain shared. `specs` never lists teams because one launch produces one agent rather than a cohort.

## Single launch and fanout share one composition

`rimz subagents fanout` accepts a JSON array and desugars every entry through the same `SubagentLaunchArgs::into_agent_launch` path above. Task `timeout` wins over the fanout flag, which wins over the configured default. Fanout-level `keep` applies uniformly; per-task foreground, retention, and passthrough argv are not part of the data format.

Parsing, required fields, timeout syntax, and duplicate explicit names are validated across the entire array before the first side effect. Pane opens then happen sequentially in the caller process. This avoids racing two backend split operations against the same ambient pane, while the child processes themselves run in parallel as soon as each pane opens.

The supervised runner's background outcome carries the minted petname and run ID back to the subagents command. Single launch prints the identity directly, while fanout collects every identity. This avoids rediscovering children from a before/after store snapshot, which could confuse another launch racing in the same family.

By default, either form returns after launching; fanout can also render its collected run IDs as JSON. With `--wait`, both forms pass their exact collected names to `agents_cmd::wait_agent`. One child therefore uses the single-result renderer and prints only the answer (or the full JSON run record); plural fanouts use the shared multi-result renderer, which prints each answer as it settles beneath a child-name header or returns a labeled JSON map with each run's final message.

A runtime failure during that loop aborts the remaining launches and reports every child already started. Those children are not rolled back: their durable run records, deadlines, self-cleanup, and caller-scoped `wait`/`stop` behavior remain the ordinary supervised lifecycle. Validation failures are different — because desugaring completed before the loop, they launch nothing.

## Launch generations and parentage

Ancestry resolves in [`plan.rs`](../../../crates/rimz/src/harness/plan.rs) as step 4 of [the compile path](./fleet.md#from-spec-to-panes) — **before** provider preflight, worktree creation, store append, or any mux action, so a refusal leaves nothing behind.

`resolve_launch_caller_from_env` finds the launching agent's durable row. It reads `RIMZ_AGENT_KIND` and `RIMZ_AGENT_ID`, then matches the row whose `launch_id` equals that id, with the kind corroborating the match so a stale cross-provider environment cannot attach a child to the wrong row. Only an agent process with no launch id at all — one that survived an upgrade — may fall back to an unambiguous live pane stamp.

`resolve_launch_ancestry` reads the caller's durable `launch_depth` field as a launch generation:

```text
caller is a pane-backed subagent                → refuse
generation = caller.launch_depth ?? 0

rimz agents / rimz teams:
  generation >= max_chain_length                → refuse
  parent_agent_id = None
  launch_depth = generation + 1

rimz subagents:
  parent_agent_id = caller.agent_id
  launch_depth = generation + 1
```

The durable field keeps its historical wire name so old event logs still replay. A peer-chain agent has a generation but no parent, so `is_launched_child()` and `is_provider_subagent()` both remain false and every consumer treats it as top-level. A `rimz subagents` child has both a direct parent and a generation, so `is_launched_child()` is true. Provider-native subagents retain a parent with no launch generation.

`[agents] max-chain-length` ([`config/agents.rs`](../../../crates/rimz/src/config/agents.rs)) defaults to `3`. A human-started agent reads as generation 0; three successive peer launches produce generations 1, 2, and 3, and the generation-3 caller cannot launch another peer. Subagent launches are not chain-checked because a subagent cannot extend the chain.

Fanout does not change that accounting: each array entry gets the same direct parent and generation. Any child attempting its own launch or fanout is refused before the first task launches.

Ancestry failures are `LaunchAncestryError`, and their messages are written for a reader that is itself an agent: each states the refusal, explains the limit, and ends with *do not retry this command*. That phrasing is load-bearing. An agent that reads "launch refused" without a terminal instruction tends to retry with a variation; the message closes that door.

## Who counts as the caller's children

`list`, `wait`, and `stop` all resolve membership the same way: `resolve_launch_caller_from_env` for the caller, then [`target::launched_children`](../../../crates/rimz/src/harness/target.rs) for the set. Sharing that resolver has one wart worth knowing before you chase it as a bug: a read-only `rimz subagents list` that cannot identify its caller reports `launch refused: …`, because the error text belongs to the launch path it borrows. That function keeps rows where `is_launched_child()` holds and `parent_agent_id` equals the caller's `agent_id`, sorted by registration.

Because a subagent cannot launch again, that set is exactly the caller's direct children. Peer-chain agents carry no parent id and never enter it.

Which projection each verb reads is the other half:

| Verb | Projection | Effect |
| --- | --- | --- |
| `list`, `wait` | `RuntimeScope::Audit` | includes ended children, so completed work stays listable and joinable |
| `stop` | alive snapshot, `ended_at.is_none()` | only live children can be stopped |

`wait` with no names joins every child that has a run record; `--any` first filters to children whose newest run is still non-terminal, since reporting "the first to finish" is meaningless against an already-terminal set. Named children resolve against the child set alone, so an address that names some other agent in the room fails with ``not one of this agent's subagents`` rather than reaching outside the family. Each child's newest run is matched by `agent_id`, falling back to `agent_name`, taking the latest `started_at`.

## The lifecycle, end to end

Normal completion does not depend on the parent issuing a stop. That is worth stating plainly, because the shape of the API invites the opposite assumption.

1. The parent launches; the direct-parent stamp and subagent-caller check pass; a run record and pane are created. By default the petname prints immediately; with `--wait`, the parent then joins the result.
2. The child runs its one turn. Its hooks fold a terminal status into the run record.
3. The child's own in-pane wrapper notices the terminal record, terminates the provider, and closes the pane independently of the parent. A surviving blocking parent also attempts the same idempotent reclamation after its wait returns; `wait` itself remains only a reader ([scripting.md § Reclaiming the run pane](./scripting.md#reclaiming-the-run-pane)).
4. The run record survives, so `list` and `wait` still report the outcome after the pane is gone.

`wait` is a reader over step 4 and has no part in step 3; a parent that never calls it changes nothing. The 30-minute deadline is the backstop for a child whose wrapper cannot finish the job: it is stored on the run record and enforced by the elected room producer, so it fires whether or not the parent is still alive. `--keep` opts out of step 3 and leaves reclamation to `rimz subagents stop` or `rimz gc`.

The one place the parent does reach its children is `stop`. Stopping a parent through `rimz agents stop` — or through `rimz teams stop` reaching that parent — stops its live pane-backed children first, so a cascade cannot strand a child whose only reason to exist was the parent's turn.

## What v1 leaves out

`restart` and `resume` are absent by design: the durable run record does not retain every launch argument needed to reproduce the supervised deadline, wait, and self-close contracts, and a partial reproduction would silently change the child's lifecycle. Relaunching the same spec and prompt is the supported path, which matches the model agents already have for their native Agent tool.

The durable launch record also does not stamp which profile namespace produced a child. Generic restart and recovery posture therefore continue to resolve `[agents.profiles]`; a subagent-only profile degrades or refuses through the existing missing-profile path. Persisting the doorway scope with the launch event is the upgrade path.

A child is addressable as `@<petname>`, but a supervised print-mode provider is not an interactive message consumer, so mid-run steering is not a contract to depend on. A message can park against the address; v1 does not resume a finished child to consume it.

## See also

- [scripting.md](./scripting.md): the supervised run every child is, including the wrapper self-close and the producer-enforced deadline.
- [fleet.md](./fleet.md): the launch, address, and reclaim machinery, and where ancestry sits in the compile path.
- [model.md](../agents/model.md): the rollup, and the provider-native subagent rows this page's predicates exclude.
- [cli/subagents.md](../../reference/cli/subagents.md): the user-facing command and flag surface.

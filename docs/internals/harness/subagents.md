# Agent-launched subagents

> One agent delegating a bounded prompt to another. This page owns the child lifecycle: the agent-only launch and lifecycle verbs, what a launch desugars to, the direct-parent stamp and no-further-launch rule, how settled results report to the parent, how the caller-scoped verbs decide who its children are, and the boundary with the provider-native children that share the name. The supervised run underneath a child is [scripting.md](./scripting.md); the launch core it rides on is [fleet.md](./fleet.md).

## Two things are called a subagent

RimZ shows both under one product term and nests both under the same card, so the first thing to get straight is that they are different mechanisms with different truth.

A **provider-native subagent** is the agent's own child, running headless inside the parent's process. RimZ learns it exists only from `SubagentStarted` and `SubagentStopped` hook signals and folds it into the rollup as a child row. It has no pane, no run record, and no address — [model.md](../agents/model.md) owns it.

A **launched subagent** is a full RimZ agent: its own pane, provider process, durable run record, petname, launch profile, transcript, cost, and address. `rimz subagents` creates it. Its separate transcript spend folds into the parent's seat; provider-native child spend is already in the parent transcript. This page owns it.

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

Launch, fanout, and the caller-scoped `list`, `wait`, and `stop` verbs refuse outside a RimZ-launched agent ([`cli/subagents/mod.rs`](../../../crates/rimz/src/cli/subagents/mod.rs)). The test is one environment variable: `RIMZ_AGENT_KIND`, exported into every agent pane by the [exec wrapper](./fleet.md#the-exec-wrapper). A user shell has no such variable and gets pointed at `rimz agents` or `rimz teams` instead. The read-only `profiles` catalog is deliberately exempt because it only loads machine configuration and does not depend on caller ancestry.

This is a usability boundary, not a security one — the same launch is expressible as `rimz agents … -p --bg`, and `subagents` exists so a delegating agent does not have to choose the supervision flags correctly. What the doorway buys is that every child launched through it is *uniformly* supervised, background, deadlined, and self-cleaning.

## Children cannot delegate again

Every child launched through `rimz subagents`, including every fanout entry, receives a `<system_reminder>` telling it to complete the work directly and not launch more agents. Where the provider exposes a verified native restriction, the exec compiler independently disables its delegation tool after profile arguments and configured environment have been applied:

| Provider | Process restriction |
| --- | --- |
| Claude | merges the profile's disallowed-tool values into one final `--disallowedTools` occurrence and denies `Agent` |
| Codex | replaces every `features.multi_agent` config override with one final false override |
| OpenCode | merges `"task":"deny"` into `OPENCODE_PERMISSION`, preserving the profile's other permission rules |
| Pi | prompt-only: Pi has no built-in subagents, and disabling all third-party extensions would remove unrelated child tools |
| Other adapters | prompt-only until a provider-native restriction is verified |

The restriction marker is internal to this doorway. A peer launched with `rimz agents` or as a team member keeps its normal provider tools even when it carries a launch generation.

The launch planner independently rejects any `rimz agents` or `rimz subagents` call whose durable caller is a pane-backed child. That check blocks the child from launching another RimZ process, while the process restriction blocks the provider's native delegation tool. The reminder covers every adapter and states both rules in the child's instruction context.

## Parents learn their catalog

The exec wrapper builds every non-child's allowed catalog from its launching profile and appends an availability reminder on the adapter's native append-system-text channel. Fresh launches, resumes, forks, restarts, and recovery launches all pass through that wrapper. Claude, Codex, Qwen, and Droid receive the reminder; adapters without a native channel receive none because an interactive launch has no user prompt to extend safely.

The reminder points the agent at `Skill(rimz-subagents)` and lists the same filtered profiles as `rimz subagents profiles --json`. A profile with `subagents = []` instead receives a disabled reminder telling it to work directly; an available catalog with no configured profiles or commands receives dedicated nothing-configured text instead of an empty list. Both surfaces use `subagent_policy::catalog`, so catalog assembly, literal allowlist filtering, and the distinction between a disabled policy and an available but empty named catalog have one owner. If effective project config fails during crash recovery, reminder enrichment warns and is skipped rather than killing the recovered pane.

## What a launch desugars to

`rimz subagents <profile> <prompt>` builds an ordinary `AgentLaunchArgs` and hands it to the same background supervised launcher a fanout uses. The sugar is entirely in the defaults and the optional join:

| Field | Value | Why |
| --- | --- | --- |
| `print` | always `true` | a child is one bounded turn, never a session |
| `bg` | always `true` | single launch and fanout share one background-launch composition |
| join | absent unless `--wait[=DURATION]` | the parent normally keeps moving; the optional duration limits only its join |
| wrapper completion monitor | on unless `--keep` | the provider stops at the durable outcome and the wrapper closes the pane |
| `timeout` | `--timeout`, else `[agents.subagents] timeout`, default `30m` | an unattended child must not run forever |
| `keep` | `--keep`, default false | holds the pane after completion and past parent exit |
| everything else | default | see the omissions below |

The default has the user-visible behavior of `rimz agents <profile> <prompt> -p --bg --timeout 30m`, plus a completion report: the wrapper queues a message, stamps `ended_at`, and closes its pane after the one turn finishes. `--wait` leaves the background run unchanged and passes the minted petname to the shared single-name wait path; `--wait=DURATION` adds a caller-side join deadline without changing the child's timeout. The joiner suppresses the queued copy only when it actually prints the result, so an expired or shell-cut join still receives the report later.

The no-delegation reminder rides a provider-native append-system-text launch flag for Claude, Qwen, and Droid, after any caller-supplied append text. Codex receives it through `-c developer_instructions=…`, which produces a developer-role message separate from the user prompt and preserves Codex's built-in instructions. An argv-supplied `developer_instructions` value is composed before the reminder; a value in the user's base `~/.codex/config.toml` is overridden because it is not visible at launch composition time. Adapters without a native system-text channel keep the same tag-wrapped reminder at the end of the user prompt. The process compiler owns this choice at the adapter boundary, beside the native delegation lockdown, so preflight and the eventual exec compile the same provider argv.

## Pane zones

`RunPlacement::SubagentZone` keeps the subagent doorway outside the general placement/config surface while giving its panes a stable home. For a solo caller, the first child splits right from the caller and later children stack against the newest live pane-backed child. For a configured team member, the first child opens a `<view> subagents` companion tab immediately after the launcher's tab, and children launched by any member of that team cohort stack against the newest child there. Zellij uses its native pane stack; tmux maps the same seam to equal-height vertical rows without resizing the sidebar column. A failed solo-column split falls back to the companion tab, then to a generic run tab if that view cannot open. If an existing team companion cannot accept another pane, the overflow child likewise opens in a generic run tab instead of failing the run or duplicating the companion. When the last child pane closes, the companion sidebar's empty-view check closes itself and the mux removes the tab.

Zone mutation is serialized per workspace across the mux open and wrapper-bind wait. Anchor selection joins durable child ancestry and pane bindings with one authoritative mux pane list, so an ended `--keep` child remains an anchor while its pane is live and a dead pane never does. Before creating a team companion tab, the same mux reading reuses a live tab with the expected base view name after removing its sidebar-managed status glyph, including a child pane that has opened but not bound durably yet. If authoritative placement truth is unavailable, the child degrades to a generic run tab; placement never gates the durable run.

The wrapper records its binding asynchronously after the mux starts it, so a subagent launch polls the durable rollup for up to three seconds before releasing the zone lock and returning. Sequential fanouts therefore observe the preceding child's anchor in the normal path. A wrapper that does not bind within the cap does not fail its launch: a team launch can still find the live companion view from mux truth, while a solo launch takes the no-anchor strategy.

The doorway deliberately omits `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--resume`, placement flags, output and input formats, retries, and verification. Each of those needs a decision the delegating agent is not well placed to make, and each is still reachable by calling `rimz agents` directly. Its launch resolution and `profiles` catalog read `[subagents.profiles]`, while `rimz agents` reads `[agents.profiles]`; a wrong-doorway profile names both sections in the error. Commands and teams remain shared. `profiles` never lists teams because one launch produces one agent rather than a cohort.

## Caller policy

The supervised runner resolves the durable caller before it resolves a subagent profile. [`subagent_policy.rs`](../../../crates/rimz/src/harness/subagent_policy.rs) then applies the caller's `[agents.profiles]` policy: when that profile sets `subagents = [...]`, both the positional profile and any `--agent` rebase must appear literally in the list. Refusal happens before provider preflight, store append, or mux mutation. Its shared catalog filters both `rimz subagents profiles` and the parent's launch reminder; a user-shell catalog remains unfiltered.

## Single launch and fanout share one composition

`rimz subagents fanout` accepts a JSON array and desugars every entry through the same `SubagentLaunchArgs::into_agent_launch` path above. Task `timeout` wins over the fanout flag, which wins over the configured default. Fanout-level `keep` applies uniformly; per-task foreground, retention, and passthrough argv are not part of the data format.

Parsing, required fields, timeout syntax, and duplicate explicit names are validated across the entire array before the first side effect. Pane opens then happen sequentially in the caller process. This avoids racing two backend split operations against the same ambient pane, while the child processes themselves run in parallel as soon as each pane opens.

The supervised runner's background outcome carries the minted petname and run ID back to the subagents command. Single launch prints the identity directly, while fanout collects every identity. This avoids rediscovering children from a before/after store snapshot, which could confuse another launch racing in the same family.

By default, either form returns after launching and writes a stderr notice that each child will report back; fanout can also render its collected run IDs as JSON. With `--wait`, both forms pass their exact collected names to `agents_cmd::wait_agent`. One child therefore uses the single-result renderer and prints only the answer (or the full JSON run record); plural fanouts use the shared multi-result renderer, which prints each answer as it settles beneath a child-name header or returns a labeled JSON map with each run's final message.

A runtime failure during that loop aborts the remaining launches and reports every child already started. Those children are not rolled back: their durable run records, deadlines, self-cleanup, and caller-scoped `wait`/`stop` behavior remain the ordinary supervised lifecycle. Validation failures are different — because desugaring completed before the loop, they launch nothing.

## Launch generations and parentage

Ancestry resolves in the supervised runner before layout compilation — **before** provider preflight, worktree creation, store append, or any mux action, so a refusal leaves nothing behind.

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

1. The parent launches; the direct-parent stamp and subagent-caller check pass; a run record and pane are created. The petname prints immediately; with `--wait`, the parent then joins the result.
2. The child runs its one turn. Its hooks fold a terminal status into the run record.
3. The in-pane wrapper notices the terminal record and terminates the provider. After its child-exit fallback guarantees a terminal run, [`subagent_report`](../../../crates/rimz/src/cli/agents_cmd/subagent_report.rs) reads the parent and sibling rows from the audit projection and composes the settled outcome.
4. When the parent still exists, the wrapper queues a parked `MessageSender::Subagent { kind, name }` record with gate `Done`. Its `SUBAGENT_REPORT` envelope names the child; immediate pane delivery is best-effort latency over the durable run record.
5. The wrapper stamps the child row's `ended_at` and closes its pane. With `--keep`, it instead transfers runtime ownership back to itself and remains alive to hold the pane.
6. A kept wrapper closes after `rimz subagents stop`; parent exit does not reclaim it. Without `--keep`, the parent's durable end stamp or authoritative pane-absence probes remain an earlier-close backstop.
7. The run record survives the close, and runtime projection retains the ended child under its visible parent, so `list` and `wait` still report the outcome and the card keeps its verdict until the parent's next prompt boundary.

The still-running count uses each launched sibling's newest run at report composition time. Two children that settle together may each observe no non-terminal sibling and report that all have finished; both statements are true at their respective read. A missing or ended parent suppresses the report without changing the run.

Inline joins and wrapper reports use a two-field handshake on the run record. The joiner stamps `joined_at` at the point it prints a terminal run; the wrapper stores the queued message in `report_message_id`. Both mutations take the run lock. Whichever side observes the other's field cancels the queued message before delivery, while a join deadline or shell exit that never prints leaves the callback intact. This gives one result in every case without making the caller's join posture a launch-time policy.

`wait` never closes the pane. It is the manual re-join when the caller needs the result synchronously or wants to reread durable history; its only writes stamp inline delivery and cancel the duplicate completion report. `--keep` is the sole linger path, leaving reclamation to `rimz subagents stop` or `rimz gc`.

The watchdog probes every 60 seconds on its own thread, rereads the parent row so an in-place restart can move panes, and treats only `RequireAuthoritative` mux absence as a strike. The elected sidebar producer scans for orphans no more than once per minute as a slower durable-record backstop: a live child whose parent has been ended (or missing) for ten minutes starts a hidden repair helper. The helper rechecks the records, closes the child, records its durable end, and emits the warning diagnostic `subagent_orphan_reaped`. A failed close emits `subagent_orphan_repair_failed` and remains eligible for a later scan. Either warning means the wrapper watchdog missed its normal window rather than reporting routine parent shutdown.

Stopping a parent through `rimz agents stop` — or through `rimz teams stop` reaching that parent — still stops its live pane-backed children first.

## What v1 leaves out

`restart` and `resume` are absent by design: the durable run record does not retain every launch argument needed to reproduce the supervised deadline, wait, and self-close contracts, and a partial reproduction would silently change the child's lifecycle. Relaunching the same profile and prompt is the supported path, which matches the model agents already have for their native Agent tool.

The durable launch record also does not stamp which profile namespace produced a child. Generic restart and recovery posture therefore continue to resolve `[agents.profiles]`; a subagent-only profile degrades or refuses through the existing missing-profile path. Persisting the doorway scope with the launch event is the upgrade path.

A child is addressable as `@<petname>`, but a supervised print-mode provider is not an interactive message consumer, so mid-run steering is not a contract to depend on. A message can park against the address; v1 does not resume a finished child to consume it.

## See also

- [scripting.md](./scripting.md): the supervised run every child is, including the subagent retention exception.
- [fleet.md](./fleet.md): the launch, address, and reclaim machinery, and where ancestry sits in the compile path.
- [model.md](../agents/model.md): the rollup, and the provider-native subagent rows this page's predicates exclude.
- [cli/subagents.md](../../reference/cli/subagents.md): the user-facing command and flag surface.

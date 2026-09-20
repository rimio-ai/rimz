# Agent-launched subagents

> One agent delegating a bounded prompt to another. This page owns what a `rimz subagents` child adds to a supervised run: the agent-only doorway, what a launch desugars to, where the child's pane and checkout land, the direct-parent stamp and the rule that children cannot delegate, which children each verb selects, and how settled results reach the parent. The supervised run underneath a child is [scripting.md](./scripting.md); the launch core it rides on is [fleet.md](./fleet.md); the user-facing flags are [cli/subagents.md](../../reference/cli/subagents.md).

## Two things are called a subagent

RimZ shows two different mechanisms under one product term and nests both under the parent's card. They carry different truth, so keep them apart.

A **provider-native subagent** is the agent's own child, running headless inside the parent's process. RimZ learns it exists only from `SubagentStarted` and `SubagentStopped` hook signals and folds it into the rollup as a child row. It has no pane, no run record, and no address; [model.md](../agents/model.md#subagents) owns it.

A **launched subagent** is a full RimZ agent that `rimz subagents` creates: its own pane, provider process, durable run record, petname, launch profile, transcript, and address. The petname stays its address, while the sidebar card labels it by launch profile. `rimz transcript @<petname>` reads its conversation; channel and `@all` transcript views leave it out. Its spend folds into the parent's figures in the sidebar, `agents show`, teams, and attribution, beside provider-native spend ([attribution.md](../agents/attribution.md)). This page owns the launched form.

One field combination separates them. Both predicates live on `AgentState` in [`agents/state.rs`](../../../crates/rimz/src/agents/state.rs):

| Predicate | Test | Means |
| --- | --- | --- |
| `is_launched_child` | `parent_agent_id.is_some() && launch_depth.is_some()` | `rimz subagents` launched it; it has a pane and a run |
| `is_provider_subagent` | `parent_agent_id.is_some() && launch_depth.is_none()` | the provider launched it inside its own turn |

A peer launched through `rimz agents` carries `launch_depth` without a parent and matches neither predicate. When an agent launched it, the peer also carries `launched_by` (the caller's kind and launch id), which routes only the [fleet report](#the-lifecycle-end-to-end): nesting, reaping, cascade stop, and resume ignore it. `AgentState::launcher` returns the parent link for a launched child and `launched_by` otherwise, and `launcher_is` matches it the way `parent_is` does. Every caller-scoped verb on this page filters provider-native rows out.

Both kinds attach to a parent card through one chokepoint, `AgentState::parent_is`: the child's `parent_agent_id` matches a candidate whose `agent_id` or `launch_id` equals it, and `parent_agent_kind` (defaulting to the child's own kind) must equal the candidate's kind. A launched child's link names its caller's launch ([parentage](#launch-generations-and-parentage)); a provider-native link names the parent's session id, with deeper provider ancestry flattened by the store writer as it adopts hook observations. The sidebar's subagent section is therefore origin-blind and one level deep.

A launched child renders once. The pane projection ([`store/snapshot/panes.rs`](../../../crates/rimz/src/store/snapshot/panes.rs)) binds a pane whose agent carries a parent as a nested agent, which records the pane without emitting a top-level row, as long as some row of the parent's launch renders. When no row of that launch renders and the child is still live, the child is promoted to its own top-level row, so a live pane never renders nowhere.

The two also differ in reach. A launched child is a peer for `rimz message` and `rimz pane` even while it renders under a parent card; a provider-native child is display-only and has no address.

## The doorway

Launch, `fanout`, `wait`, and `stop` are agent-only ([`cli/subagents/mod.rs`](../../../crates/rimz/src/cli/subagents/mod.rs), `resolve_agent_caller`). The gate passes when the process carries the RimZ agent-kind launch environment, or, without it, when process ancestry matches a live durable agent. A user shell that passes neither gets an error pointing at `rimz subagents list`, `rimz agents`, and `rimz teams`. The gate reads only the environment's presence; a stale environment is refused later, when launch ancestry or `wait` and `stop` resolve the caller's durable row ([caller resolution](#launch-generations-and-parentage)).

`list`, `profiles`, and bare `rimz subagents` with no profile are read-only and open to any caller. `profiles` resolves optional caller context from existing room state without creating it. `list` selects the caller's children when the caller resolves to a durable agent row and the current channel's children otherwise ([who counts](#who-counts-as-a-launched-child)).

The doorway is a usability boundary, not a security one. The same launch is expressible as `rimz agents <profile> <prompt> -p --bg --timeout 30m`; `subagents` exists so a delegating agent does not have to choose the supervision flags, and so every child launched through it is uniformly supervised, background, deadlined, and self-cleaning.

## What a launch desugars to

`rimz subagents <profile> <prompt>` builds an ordinary `AgentLaunchArgs` in `SubagentLaunchArgs::into_agent_launch` and hands it to `launch_supervised_background`, the same background supervised launcher a fanout uses.

| Field | Value | Why |
| --- | --- | --- |
| `print` | always `true` | a child is one bounded turn, never a session |
| `bg` | always `true` | single launch and fanout share one background composition |
| `subagent` | always `true` | selects the parent stamp, profile namespace, pane zone, and no-delegation rules below |
| `self_cleanup_on_completion` | `true`, cleared by `--keep` | the wrapper stops the provider at the durable outcome and closes the pane |
| `timeout` | `--timeout`, else `[agents.subagents] timeout`, default `30m` | an unattended child must not run forever |
| `keep` | `--keep`, default false | holds the pane after completion and past parent exit |
| join | none unless `--wait[=DURATION]` | the parent normally keeps moving; the duration limits only the caller's join |

The pass-through fields are `--prompt-file`, `--model`, `--agent` (a re-base onto another profile or kind), `--effort`, `--isolation`, `--description`, `--max-turns`, and trailing argv. The doorway omits `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--resume`, placement flags, output and input formats, retries, and verification: each needs a decision the delegating agent is not placed to make, and each stays reachable through `rimz agents`.

A launch prints the minted petname and returns, with a stderr receipt, shared with `rimz agents -p --bg` through `supervised::output::write_background_receipt`, that names the fleet report, the agent-visible response path, and the `wait` command that blocks instead ([the lifecycle](#the-lifecycle-end-to-end)). `--wait` leaves the background run unchanged and passes that petname to the shared `agents_cmd::wait_agent` join; `--wait=DURATION` adds a caller-side deadline without changing the child's timeout. `--json` on a single launch requires `--wait`.

Launch resolution and the `profiles` catalog read `[subagents.profiles]`, while `rimz agents` reads `[agents.profiles]`; a profile named in the wrong section produces an error that names both. Commands and teams are shared between the two, but `profiles` never lists teams, because one launch produces one agent.

## Single launch and fanout share one composition

`rimz subagents fanout` reads a JSON task array from a file or stdin and desugars every entry through the same `into_agent_launch`. A task's `timeout` wins over the fanout's `--timeout`, which wins over the configured default. Fanout-level `--keep` applies to every task; per-task isolation, wait, retention, and passthrough argv are not part of the task format, which rejects unknown fields.

Parsing, required fields, and timeout syntax are validated across the whole array before the first side effect, so a validation failure launches nothing. Pane opens then run sequentially in the caller process, which avoids racing two backend splits against the same anchor pane; the child processes run in parallel as soon as each pane opens.

The supervised runner returns each child's minted petname and run id to the command, so fanout collects identities directly instead of diffing store snapshots, which another launch in the same family could confuse. Without `--wait`, fanout prints the names, or with `--json` a map of name to run id. With `--wait`, it passes the collected names to `wait_agent`: one child prints only its answer (or the JSON run record), and several print each answer as it settles beneath a child-name header, or a labeled JSON map.

A runtime failure partway through aborts the remaining launches and names every child already started. Those children are not rolled back; they keep the ordinary supervised lifecycle, and the error points at `rimz subagents wait` and `stop --all`.

## Caller policy

The supervised runner resolves the durable caller before it resolves a subagent profile. [`subagent_policy.rs`](../../../crates/rimz/src/harness/subagent_policy.rs) then applies the caller's `[agents.profiles]` entry: when it sets `subagents = [...]`, both the positional profile and any `--agent` re-base must appear literally in the list. Refusal happens before provider preflight, store append, or mux mutation.

`subagent_policy::catalog` is the one owner of catalog assembly, and both `rimz subagents profiles` and the parent's launch reminder read it ([below](#parents-learn-their-catalog)). It filters by the literal allowlist and distinguishes a disabled policy (`subagents = []`) from an available catalog that happens to be empty. A caller whose profile sets no `subagents` list, or that resolves to no profile (a user shell included), sees the unfiltered catalog.

## The child works where its parent works

A child's checkout is its parent's recorded checkout, not the directory the parent's shell happens to be in. The runner first resolves its workspace from `"."`, and a parent that writes a brief under `/tmp` and launches from there would hand the child `/tmp`. So right after ancestry, `anchor_subagent_workspace` ([`cli/supervised.rs`](../../../crates/rimz/src/cli/supervised.rs)) resolves the workspace again from the caller's `AgentState.worktree_path`, and everything after reads that value: profile qualification, channel inference, `RIMZ_WORKTREE_PATH`, and `resolve_launch_checkout`, whose cwd becomes the pane cwd, the provider cwd, the sidebar options, `RunRecord.worktree_path`, and the child's recorded checkout. When that path is a repository subdirectory selected by the parent's `--root`, it stays the child's `worktree_root`; workspace resolution supplies only the surrounding metadata.

The parent's `worktree_path` is stamped from its launch cwd (or filled from a hook process's cwd) and does not follow a shell `cd`. Both resolutions share the room pin's project root, so the store and effective config opened before the re-anchor stay correct.

The re-anchor applies only to a subagent request with a resolved caller; peers launched through `rimz agents` and loop fires keep the invoking cwd. A caller row with no recorded checkout keeps the invoking cwd and logs at debug. A recorded checkout that no longer exists refuses, naming the path, before any store append or mux action.

Provider preflight then runs against that checkout. Besides hook installation and trust, a Codex child needs an existing directory-trust decision for it, or the launch refuses with the fix instead of parking the child on Codex's trust screen until its deadline ([adapter_codex.md](../agents/adapter_codex.md#directory-trust-preflight)).

## Pane zones

Subagent panes land in `RunPlacement::SubagentZone`, a placement reserved for this doorway and absent from the general placement flags and config. `select_subagent_zone_strategy` ([`cli/supervised/pane.rs`](../../../crates/rimz/src/cli/supervised/pane.rs)) picks the strategy in this order:

| Order | Condition | Strategy |
| --- | --- | --- |
| 1 | a `<view> subagents` companion family for the caller's view is live | append to the first companion with room, or open the next numbered companion when all are full |
| 2 | the caller is a configured team member | open a new companion tab after the launcher's tab |
| 3 | a launched child of the caller is live beside the caller's pane (same session and view, tiled) | stack against the newest such child |
| 4 | the caller's pane is live | split right from the caller |
| 5 | none of the above | open a new companion tab |

Zellij stacks natively; tmux maps the stack to equal-height vertical rows. Members of a team sharing one view reuse that view's companion family instead of following the newest child into an unrelated run tab. A failed solo split also falls back to a companion tab, which rule 1 then reuses for later children.

Companion tabs tile instead of stacking. The shared [`mux/companion_layout.rs`](../../../crates/rimz/src/mux/companion_layout.rs) planner starts with two side-by-side columns and adds rows to the less populated column, targeting equal heights within a column and column widths proportional to their pane counts, so panes get roughly equal areas without moving processes between columns. `COMPANION_PANE_LIMIT` (8) caps a tab at four rows by two columns, excluding the sidebar; a third column would restructure existing columns instead of splitting locally. Before each append the backend rechecks physical occupancy, including retained and not-yet-bound terminals. Companions are tried in numeric order, and a full or unsplittable family grows into `<view> subagents 2`, then `3`. Small terminals and manually rearranged panes can force overflow before eight.

Balancing happens on append only, not after closes or terminal resizes, and leaves the sidebar and existing processes in place. Zellij's coarse resize steps make equality approximate, and background grid appends need its native no-focus support. A balancing failure after a successful spawn never launches the child again, and an uncertain spawn or a failed companion-tab response fails the launch instead of retrying the same durable run elsewhere. When the last child pane closes, the companion's sidebar sees an empty view and closes the tab.

Zone mutation is serialized per workspace by `lock_subagent_zone`, held across the mux open and the wrapper-bind wait. Anchor selection joins durable child ancestry and pane bindings with one authoritative mux pane list, so an ended `--keep` child stays an anchor while its pane is live and a dead pane never is. Companion discovery matches exact base or numbered view names after stripping sidebar status glyphs, grouped by the current mux view identity, and counts children that opened but have not bound yet. If the authoritative placement lookup is unavailable, the child degrades to a generic run tab.

The wrapper records its pane binding asynchronously, so a subagent launch polls the durable rollup for up to `SUBAGENT_PANE_BIND_TIMEOUT` (3 seconds) before releasing the zone lock. Sequential fanout launches therefore see the preceding child as an anchor. A wrapper that does not bind in time does not fail its launch: the next team launch still finds the companion from mux truth, and a solo launch takes the no-anchor strategy.

## Launch generations and parentage

Ancestry resolves in the supervised runner before layout compilation, provider preflight, worktree creation, store append, or any mux action, so a refusal leaves nothing behind.

The caller resolver ([`harness/ancestry.rs`](../../../crates/rimz/src/harness/ancestry.rs)) finds the calling agent's durable row in three tiers:

| Tier | Evidence | Resolves to |
| --- | --- | --- |
| launch environment with a launch id | `CallerIdentity::from_env` | the launch's current row through `address::launch_row` (a live row by pane-owner order, otherwise the latest active one), with kind corroborating |
| launch environment without a launch id | the agent kind plus the ambient pane id | the single live, same-kind, non-provider-subagent row stamped with that pane; more than one match refuses |
| no launch environment | `from_process_ancestry` | the nearest ancestor whose PID and process-start token match a live agent `RuntimeOwner`, preferring the current pane when rows share an owner |

Resolving a launch id to its current occupant, instead of to the first row carrying it, keeps a child off an ended predecessor after a compaction, a `/new`, or a fork. Kind corroboration stops a stale cross-provider environment from attaching a child to the wrong row. The second tier covers an agent process that was already running across an upgrade and so has no launch id.

`resolve_launch_ancestry` then reads the caller's durable `launch_depth` as a launch generation:

```text
caller is a launched child                      → refuse (SubagentCaller)
generation = caller.launch_depth ?? 0

rimz agents / rimz teams:
  generation >= max_chain_length                → refuse (ChainExceeded)
  parent_agent_id = None
  launch_depth = generation + 1

rimz subagents:
  parent_agent_id = caller.launch_id ?? caller.agent_id
  parent_agent_kind = caller.kind
  launch_depth = generation + 1
```

The stamp names the caller's launch, falling back to its session id only for a caller without a launch id. A launch outlives its conversation rows: compaction, `/clear`, a follow-latest switch, and a forked retry each end the old row, and a link to one row would read the child as parentless while the parent is still working. The field keeps the wire name `launch_depth` so existing event logs replay.

`[agents] max-chain-length` ([`config/agents.rs`](../../../crates/rimz/src/config/agents.rs)) defaults to `3`. A human-started agent is generation 0; three successive peer launches produce generations 1, 2, and 3, and the generation-3 agent cannot launch another peer. Subagent launches skip the chain check because a subagent cannot extend the chain. Every fanout entry gets the same parent and generation.

Ancestry failures are `LaunchAncestryError`, written for a reader that is itself an agent: each message states the refusal, explains the limit, and ends with *do not retry this command*. An agent that reads "launch refused" without a terminal instruction tends to retry with a variation, so the phrasing is load-bearing.

## Children cannot delegate again

Three independent layers keep a child from launching work of its own. Each covers a gap the others leave.

The launch planner refuses any `rimz agents`, `rimz teams`, or `rimz subagents` launch whose durable caller is a launched child (`SubagentCaller` above). That blocks another RimZ process.

Every child request, fanout entries included, carries a no-delegation body in its launch reminder telling it to do the work directly. It reaches Claude, Codex, Qwen, and Droid through their append-system-text channel, and every other adapter as a tag-wrapped suffix on the user prompt (`supervised_prompt` in [`cli/supervised/run.rs`](../../../crates/rimz/src/cli/supervised/run.rs)), without the model line. Channels, paragraph order, and the Codex `developer_instructions` route are owned by [fleet.md](./fleet.md#launch-reminders) and [adapter_codex.md](../agents/adapter_codex.md).

Where the provider exposes a verified native restriction, the exec compiler also disables its delegation tool, after profile arguments and configured environment are applied:

| Provider | Process restriction |
| --- | --- |
| Claude | merges the profile's disallowed tools into one final `--disallowedTools` occurrence that denies `Agent` |
| Codex | replaces every `features.multi_agent` override with one final false override |
| OpenCode | merges `"task":"deny"` into `OPENCODE_PERMISSION`, preserving the profile's other rules |
| Pi | prompt-only: Pi has no built-in subagents, and disabling all third-party extensions would remove unrelated tools |
| Other adapters | prompt-only until a native restriction is verified |

The restriction follows the request's `subagent` flag, not the launch generation, so a peer or team member launched with `rimz agents` keeps its normal provider tools.

### Parents learn their catalog

A non-child launch gets the other side of that paragraph: the subagent catalog its profile allows, built by the exec wrapper through `subagent_policy::catalog` on every fresh launch, resume, fork, restart, and recovery. It points the agent at `Skill(rimz-subagents)` and lists the same filtered profiles as `rimz subagents profiles --json`. A profile with `subagents = []` gets a disabled body telling it to work directly, and an available catalog with nothing configured gets dedicated nothing-configured text instead of an empty list. Only adapters with an append-system-text channel receive it, because an interactive launch has no user prompt to extend safely. If effective config fails to load during crash recovery, the wrapper warns and falls back to default reminders instead of killing the recovered pane: the catalog and team paragraphs drop, and the model line stays ([fleet.md](./fleet.md#launch-reminders)).

## Who counts as a launched child

Agent-scoped `list`, `wait`, and `stop` select through [`address::launched_children`](../../../crates/rimz/src/address.rs). It keeps rows where `is_launched_child` holds and `parent_is` accepts some row of the caller's launch, then sorts by registration. Widening the parent to its whole launch keeps the family together when a launched parent adopts its provider session id, and when a child stamped with one conversation's session id must list under the successor that replaced it. Because a child cannot launch again, the set is exactly the caller's direct launched children; peers carry no parent and provider-native children carry no generation.

A session-id link resolves only where the row it names is visible, which costs the sidebar one case. The harness verbs, the watchdog, and the orphan scan read `RuntimeScope::Audit`, which retains ended rows, so they reach the parent through an ended predecessor. The sidebar reads `RuntimeScope::Runtime`, which hides ended rows, so a child holding a session-id link renders promoted to its own card once that conversation ends. Callers with a launch id never produce such links.

| Verb | Caller | Projection | Selects |
| --- | --- | --- | --- |
| `list`, `wait` | resolved agent | `RuntimeScope::Audit` | the caller's children, ended ones included, so completed work stays listable and joinable |
| `list` | unresolved (user shell) | `RuntimeScope::Audit` | launched children in the current channel, or every channel when there is none |
| `stop` | resolved agent | alive snapshot, then `ended_at.is_none()` for `--all` | the caller's live children |
| `wait`, `stop` | unresolved | none | refuse; they need a durable caller |

`wait` requires at least one name; given none, it fails and prints the caller's child list, so a copied command never joins an older fleet. `--any` returns at the first named child to settle. Names resolve against the child set only, so an address naming another agent in the room fails with ``not one of this agent's subagents``. Each child's newest run is matched by `agent_id`, falling back to `agent_name`, with the latest `started_at`.

For a user shell, [`address::launched_children_in_channel`](../../../crates/rimz/src/address.rs) keeps every launched child that matches `Ctx::channel`. A child with a channel stamp matches it exactly, or as the dashed form of a branch-style filter; a child without one matches the channel composed from its worktree directory name. It never infers membership from a live parent, so ended parents and adopted identities do not hide child history. [`address::launched_parent`](../../../crates/rimz/src/address.rs) is the inverse join, so each row labels its parent launch's current row, and the table adds parent and channel columns.

A plain shell in the project directory cannot derive an in-place team's `<directory>/<team>` channel from cwd, so `Ctx::channel` is `None` there and user-shell `list` shows every channel; the channel column tells those in-place lanes apart. A RimZ-launched pane carries `RIMZ_CHANNEL`, and a shell in a separate worktree derives its worktree channel.

## The lifecycle, end to end

1. The parent launches. Caller policy and ancestry pass, the run record and pane are created, and the petname prints. With `--wait`, the parent then joins.
2. The child runs until its work ends. A clean turn end with an owed harness wake parks its run as `Running`, so the parent digest still waits for it; otherwise its hooks fold a terminal status into the run record.
3. The in-pane wrapper sees the terminal record and stops the provider. Once its child-exit fallback guarantees a terminal run, it calls the fleet reporter ([`cli/agents_cmd/subagent_report.rs`](../../../crates/rimz/src/cli/agents_cmd/subagent_report.rs), `report_settled_child`).
4. The reporter reads the launcher's current row (`AgentState::launcher`) and, through [`address::launched_fleet`](../../../crates/rimz/src/address.rs), the newest run of every row that launcher launched: its launched children plus the peers stamped with `launched_by`, so a `rimz agents -p --bg` run from an agent reports the same way. A missing or ended launcher, or any non-terminal newest run, queues nothing.
5. It keeps the terminal rows with neither `report_message_id` nor `joined_at`, and writes each non-empty `last_message` atomically to `StatePaths::subagents_dir/<handle>.output`, adding a trailing newline if missing. `TmpView::current` maps each path into the parent's view: `/tmp/rimz-subagents/<handle>.output` under sandbox isolation, the host path otherwise. A write or measurement failure aborts before any stamp, so the backstop can retry.
6. It mints the message id, stamps it onto every listed row as `report_message_id`, and only then queues a parked `MessageSender::Harness { notice: SubagentReport }` with gate `Done`. A queue failure clears the stamps so the backstop can retry. The `SUBAGENT_REPORT` envelope reads `From: @rimz`, and immediate pane delivery is best-effort latency over the durable records.
7. The wrapper stamps its own row's `ended_at` and closes its pane. With `--keep`, it instead lingers with a stderr line naming `rimz subagents stop`, until a stop signal arrives; parent exit does not reclaim it.
8. The run record survives the close, and the ended child stays under any visible row of its parent's launch, so `list` and `wait` still report the outcome and the card keeps its verdict until the parent's next prompt boundary. A retained child is at rest and never holds its parent in `running` ([model.md](../agents/model.md#observed-ends-and-reaped-ends)); the run record's own outcome is what `list`, `wait`, and the digest report.

`harness::fleet::FleetRuns` is the shared settlement rule for the reporter, its orphan-sweep backstop, and the owed-wake predicate. It selects each member's newest run with matching kind and either session id or a present matching name, deduplicated by run id. A supervised parent parks at a clean end while any selected run is live or terminal but neither joined nor reported; the queued digest then holds it until delivery opens its next turn ([parked runs](./scripting.md#parked-runs)).

The digest has a heading (`Your subagent settled:` or `All N subagents settled:`, with `background agent` in place of `subagent` unless every row's run is a subagent run) and one row per child: status, elapsed time, the last line of the failure tail for a non-completed run, the task (launcher `--description`, else a bounded first-line prompt preview), and the response path with `FileSummary::label` (estimated tokens and physical lines from `FileSummary::measure`), or `no response`. With two or more response files the heading appends `responses total {label}` over their summed `FileSummary`. It ends at its last row, with no trailing instruction.

A fleet is every child launched before the digest is composed: a child launched while siblings run joins it, and one launched after composition starts belongs to the next. `run::report::record_report_messages` stamps the whole row set under the workspace lock and backs off if any row already carries a message id, so two last settlers racing on the same fleet produce one digest, and the loser queues nothing.

### Joins, stops, and the digest

Inline joins, parent stops, and the digest share a two-field handshake on the run record ([`harness/run/report.rs`](../../../crates/rimz/src/harness/run/report.rs)). Both mutations take the workspace lock.

| Actor | Stamps | Then |
| --- | --- | --- |
| a join that prints a terminal run to an attended caller | `joined_at` | cancels the queued digest if every run carrying its id is joined |
| a blocking `rimz agents -p` driver, before it closes the pane of a terminal run | `joined_at` | same check |
| the parent's `rimz subagents stop` | `joined_at` on every selected child's newest run, before cancelling any | same check, then cancels the runs |
| the fleet reporter | `report_message_id` on every listed row, before queueing | nothing further |

A `joined_at` stamp excludes that row from a digest not yet composed. A digest already queued is cancelled only when every row it lists is joined, so an unread row keeps the notice, and a digest already delivered cannot be recalled. `stop` stamps the whole selection first because the first cancellation wakes a reporter that would otherwise list an unstamped sibling. This also lets `stop` cancel a queued digest for a `--keep` child once every listed row is joined or stopped.

Reading a response file does not stamp `joined_at`. Response files die with the room; the run record stays the truth. `wait` never closes a pane, still prints a failed child's transcript tail, and is the path for a caller that needs the child's text synchronously or wants to reread durable history.

### Attendance

A printed result counts as consumed only while the calling agent holds an open provider turn. The [wait print path](../../../crates/rimz/src/cli/agents_cmd/wait.rs) resolves the caller against `Store::snapshot_cached()` and asks `AgentState::holds_open_turn`. The cached snapshot attaches provider rest certificates, so a turn settled without a `Stop` hook reads as closed, while native waits and proposed plans keep the turn open. Attendance is re-read at each print: a background wait that outlives its parent's turn still prints, but leaves its rows unjoined so the digest reaches the parent at its next boundary.

An unresolved caller is a human shell and counts as attended; a failed snapshot read logs a warning and also counts as attended. `stop` skips the check because dismissal is deliberate.

### Backstops

The parent watchdog ([`harness/parent_watch.rs`](../../../crates/rimz/src/harness/parent_watch.rs)) runs on its own thread in every non-kept child's wrapper. Every `PROBE_INTERVAL` (60 seconds) it rereads the parent launch's current row, so an in-place restart can move panes. A durable end stamp cancels the child only when every row of the parent launch has ended; a conversation switch leaves a live successor and is not an end. Pane loss cancels only after `PANE_GONE_STRIKES` (3) `RequireAuthoritative` mux reads that lack the pane, plus a reconfirmation `RECONFIRM_DELAY` (500 ms) later; a cached roster hit counts as present.

The elected sidebar producer runs [`harness/orphan_sweep.rs`](../../../crates/rimz/src/harness/orphan_sweep.rs) at most once a minute. It reads durable records only and starts hidden helpers, each of which rechecks the records before acting:

| Scan | Condition | Helper action | Diagnostic |
| --- | --- | --- | --- |
| missed digest | a live launcher whose `launched_fleet` rows are all terminal, with at least one row neither reported nor joined | runs the same fleet reporter | `subagent_digest_backstopped` when it queues |
| orphan | a live, non-kept child whose parent launch's latest row ended, or that has no row, `ORPHAN_GRACE` (10 minutes) ago, measured from that end stamp or from the child's registration | closes the child and records its durable end | `subagent_orphan_reaped`, or `subagent_orphan_repair_failed`, which stays eligible for the next scan |

Row stamps make repeated passes and races idempotent. Each diagnostic means the normal wrapper path was missed ([diagnostics.md](../diagnostics.md)).

Neither scan covers a `--keep` child: its wrapper runs no watchdog, and the orphan scan skips kept runs, so only `rimz subagents stop` or a manual pane close ends it. Stopping the parent through `rimz agents stop`, or `rimz teams stop` reaching that parent, stops its live launched children first, kept ones included.

### Signals are not the settlement path

A child's lifecycle transitions also fire `agent.*` signals ([loops.md](./loops.md#the-signal-vocabulary)) carrying `session`, `handle`, and, when recorded, the parent's session id in `parent`. A parent could arm `rimz loop add child-ended --signal agent.ended --match session=<child> --wait --once`, but the digest and `wait` carry the child's text and run identity, while a signal carries only the transition. The self-wait guard refuses a subscription that does not name another agent, so `--match parent=<own session>` alone is rejected.

## What is left out

`restart` and `resume` are absent by design. The durable run record does not retain every launch argument needed to reproduce the deadline, wait, and self-close contracts, and a partial reproduction would silently change the child's lifecycle. Relaunching the same profile and prompt is the supported path, matching how agents treat their native Agent tool.

The durable launch record does not stamp which profile namespace produced a child. Generic restart and recovery therefore resolve `[agents.profiles]`, and a subagent-only profile degrades or refuses through the missing-profile path. Persisting the doorway scope with the launch event is the upgrade path.

A child is addressable as `@<petname>`, but a supervised print-mode provider is not an interactive message consumer, so mid-run steering is not a contract. A message can park against the address; nothing resumes a finished child to consume it.

## See also

- [scripting.md](./scripting.md): the supervised run every child is, including the subagent retention exception.
- [fleet.md](./fleet.md): the launch, address, and reclaim machinery, and the launch reminder order and channels.
- [model.md](../agents/model.md): the rollup, and the provider-native subagent rows this page's predicates exclude.
- [cli/subagents.md](../../reference/cli/subagents.md): the user-facing command and flag surface.

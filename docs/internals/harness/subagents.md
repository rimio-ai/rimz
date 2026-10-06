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

`list` and `profiles` are read-only and open to any caller. Bare `rimz subagents` with no profile, or a blank one, refuses with a usage error (exit 2) before the caller gate, so it too answers any caller. `profiles` resolves optional caller context from existing room state without creating it. `list` selects the caller's children when the caller resolves to a durable agent row and the current channel's children otherwise ([who counts](#who-counts-as-a-launched-child)).

The doorway is a usability boundary, not a security one. It uses the supervised background runner of `rimz agents <profile> <prompt> -p --bg`, adding the child-specific deadline ladder; `subagents` exists so a delegating agent does not have to choose the supervision flags, and so every child launched through it is uniformly supervised, background, deadlined, and self-cleaning.

## What a launch desugars to

`rimz subagents <profile> <prompt>` builds an ordinary `AgentLaunchArgs` in `SubagentLaunchArgs::into_agent_launch` and hands it to `launch_supervised_background`, the same background supervised launcher a fanout uses.

| Field | Value | Why |
| --- | --- | --- |
| `print` | always `true` | selects RimZ's supervised run, not provider print mode; interactive providers can take follow-up turns |
| `bg` | always `true` | single launch and fanout share one background composition |
| `subagent` | always `true` | selects the parent stamp, profile namespace, pane zone, and no-delegation rules below |
| `self_cleanup_on_completion` | `true`, cleared by `--keep` | the wrapper stops the provider after the parent's receiving turn ends and closes the pane |
| `timeout` | `--timeout`, else `[agents.subagents] timeout`, default `30m` | soft work bound for pending or running work, not terminal receipt waits |
| `warn` | `--warn`, else `[agents.subagents] warn`, default `["6m", "3m"]` | normalized offsets before the soft bound |
| `grace` | `--grace`, else `[agents.subagents] grace`, default `3m` | reporting time before producer-enforced kill; zero is stored absent |
| `keep` | `--keep`, default false | disables automatic completion cleanup and the parent watchdog; holds the pane past provider exit |
| `report_to` | `--detach` stores `nobody`; default `launcher` | a `nobody` run owes no report, holds no launcher, and survives the parent's exit; pane retention still follows `keep` |
| join | none unless `--wait[=DURATION]` | the parent normally keeps moving; the duration limits only the caller's join |

The pass-through fields are `--prompt-file`, `--model`, `--agent` (a re-base onto another profile or kind), `--effort`, `--isolation`, `--description`, `--max-turns`, and trailing argv. Isolation is subject to the [subagent cap](../sandbox.md#choosing-the-isolation): a child never runs looser than its parent's effective isolation, at launch and parent-message resume. The doorway omits `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--resume`, placement flags, output and input formats, retries, and verification: each needs a decision the delegating agent is not placed to make, and each stays reachable through `rimz agents`.

A launch prints the minted petname and returns, with a stderr receipt, shared with `rimz agents -p --bg` through `supervised::output::write_background_receipt`, that names the fleet report, the agent-visible response path, and the `wait` command that blocks instead ([the lifecycle](#the-lifecycle-end-to-end)). `--wait` leaves the background run unchanged and passes that petname to the shared `agents_cmd::wait_agent` join; `--wait=DURATION` adds a caller-side deadline without changing the child's timeout. `--json` on a single launch requires `--wait`.

Launch resolution and the `profiles` catalog read `[subagents.profiles]`, while `rimz agents` reads `[agents.profiles]`; a profile named in the wrong section produces an error that names both. Commands and teams are shared between the two, but `profiles` never lists teams, because one launch produces one agent.

## Single launch and fanout share one composition

`rimz subagents fanout` reads a JSON task array from a file or stdin and desugars every entry through the same `into_agent_launch`. For each of `timeout`, `warn`, and `grace`, a task's value wins over the fanout flag, which wins over the configured default. Fanout-level `--keep` applies to every task; per-task isolation, wait, retention, and passthrough argv are not part of the task format, which rejects unknown fields.

Parsing, required fields, and all deadline durations are validated across the whole array before the first side effect, so a validation failure launches nothing. Pane opens then run sequentially in the caller process, which avoids racing two backend splits against the same anchor pane; the child processes run in parallel as soon as each pane opens.

The supervised runner returns each child's minted petname and run id to the command, so fanout collects identities directly instead of diffing store snapshots, which another launch in the same family could confuse. Without `--wait`, fanout prints the names, or with `--json` a map of name to run id. With `--wait`, it passes the collected names to `wait_agent`: one child prints only its answer (or the JSON run record), and several print each answer as it settles beneath a child-name header, or a labeled JSON map.

A runtime failure partway through aborts the remaining launches and names every child already started. Those children are not rolled back; they keep the ordinary supervised lifecycle, and the error points at `rimz subagents wait` and `stop --all`.

## Caller policy

The supervised runner resolves the durable caller before it resolves a subagent profile. [`subagent_policy.rs`](../../../crates/rimz/src/harness/subagent_policy.rs) then applies the caller's `[agents.profiles]` entry: when it sets `subagents = [...]`, both the positional profile and any `--agent` re-base must appear literally in the list. Refusal happens before provider preflight, store append, or mux mutation.

`subagent_policy::catalog` is the one owner of catalog assembly, and both `rimz subagents profiles` and the parent's launch reminder read it ([below](#parents-learn-their-catalog)). It filters by the literal allowlist and distinguishes a disabled policy (`subagents = []`) from an available catalog that happens to be empty. A caller whose profile sets no `subagents` list, or that resolves to no profile (a user shell included), sees the unfiltered catalog.

## The child works where its parent works

By default, a child's checkout is its parent's recorded checkout, not the directory the parent's shell happens to be in. The runner first resolves its workspace from `"."`, and a parent that writes a brief under `/tmp` and launches from there would hand the child `/tmp`. So right after ancestry, `anchor_subagent_workspace` ([`cli/supervised.rs`](../../../crates/rimz/src/cli/supervised.rs)) resolves the workspace again from the caller's `AgentState.worktree_path`, and everything after reads that value: profile qualification, channel inference, `RIMZ_WORKTREE_PATH`, and `resolve_launch_checkout`, whose cwd becomes the pane cwd, the provider cwd, the sidebar options, `RunRecord.worktree_path`, and the child's recorded checkout. When that path is a repository subdirectory selected by the parent's `--root`, it stays the child's `worktree_root`; workspace resolution supplies only the surrounding metadata.

An explicit `--cwd` overrides this default after the parent checkout is validated. Entry resolution converts the caller's path to an existing canonical host directory; `resolve_launch_checkout` returns a user-owned checkout there. The pane, provider, channel basename, `RIMZ_WORKTREE_PATH`, and durable child/run `worktree_path` follow that directory without changing the room pin, store, or effective config. Restart, fork, child resume, and rebirth reuse the recorded directory; no new durable field or checkout cleanup ownership is introduced.

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

Companion tabs tile instead of stacking. The shared [`mux/companion_layout.rs`](../../../crates/rimz/src/mux/companion_layout.rs) planner starts with two side-by-side columns and adds rows to the less populated column, targeting equal column widths and equal heights within each column, without moving processes between columns: with three panes, the lone right pane keeps the full height and the two left panes split theirs. `COMPANION_PANE_LIMIT` (8) caps a tab at four rows by two columns, excluding the sidebar; a third column would restructure existing columns instead of splitting locally. Before each append the backend rechecks physical occupancy, including retained and not-yet-bound terminals. Companions are tried in numeric order, and a full or unsplittable family grows into `<view> subagents 2`, then `3`. Small terminals and manually rearranged panes can force overflow before eight.

Balancing happens on append only, not after closes or terminal resizes, and leaves the sidebar and existing processes in place. Zellij resizes in steps of 5% of the tab, so its heights land within one step of equal, and a step that overshoots a nearer target is undone. Background grid appends need Zellij's native no-focus support. A balancing failure after a successful spawn never launches the child again, and an uncertain spawn or a failed companion-tab response fails the launch instead of retrying the same durable run elsewhere. When the last child pane closes, the companion's sidebar sees an empty view and closes the tab.

Zone mutation is serialized per workspace by `lock_subagent_zone`, held across the mux open and the wrapper-bind wait. Anchor selection joins durable child ancestry and pane bindings with one authoritative mux pane list, so an ended `--keep` child stays an anchor while its pane is live and a dead pane never is. Companion discovery matches exact base or numbered view names after stripping sidebar status glyphs, grouped by the current mux view identity, and counts children that opened but have not bound yet. If the authoritative placement lookup is unavailable, the child degrades to a generic run tab.

The wrapper records its pane binding asynchronously, so a subagent launch polls the durable rollup for up to `SUBAGENT_PANE_BIND_TIMEOUT` (3 seconds) before releasing the zone lock. Sequential fanout launches therefore see the preceding child as an anchor. A wrapper that does not bind in time does not fail its launch: the next team launch still finds the companion from mux truth, and a solo launch takes the no-anchor strategy.

## Launch generations and parentage

Ancestry resolves in the supervised runner before layout compilation, provider preflight, worktree creation, store append, or any mux action, so a refusal leaves nothing behind.

Before opening the store, interactive and supervised launch entries check an explicit `--root` against the calling agent's verified environment pin. Different room ids refuse with `LaunchAncestryError::RoomMismatch`, naming both rooms and the caller and directing it to drop `--root` and use `--cwd` instead. The check creates no room directory. A matching room, an absent or invalid pin, or a human shell without agent identity leaves the existing resolution unchanged; non-launch commands retain cross-room `--root` access.

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

Ancestry failures are `LaunchAncestryError`, written for a reader that is itself an agent: each message states the refusal, explains the limit, and ends with *do not retry this command*. The named exception is `RoomMismatch`, which ends with the corrected command's `--cwd` fix because that variation can succeed. An agent that reads "launch refused" without a terminal instruction tends to retry with a variation, so the phrasing is load-bearing.

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

`list` joins the context sidecar onto the audit rows with `store::agent_context::attach_rest_certificates`, because an audit row carries no `AgentState::context` of its own, and prints `AgentState::rowless_status` as `AGENT`: the projection `rimz agents show` uses for an agent with no sidebar row, which a launched child never has. A displayed pausing marker reads `paused` and a fatal one `failed`; `RUN` is still the newest run's own status, so a child parked on a limit reads `paused` beside `running`. The join skips ended and raw-`failed` rows, so a child whose hooked turn end already failed its run reads `failed` in both columns. `--json` carries the displayed marker as `turn_error`. The sidebar's nested child entry does not take this projection yet and still shows raw status.

`wait` joins the run's answer, and a parked run has none, so it keeps blocking on a paused child until the run settles or its own `--timeout` expires.

`wait` requires at least one name; given none, it fails and prints the caller's child list, so a copied command never joins an older fleet. `--any` returns at the first named child to settle. Names resolve against the child set only, so an address naming another agent in the room fails with ``not one of this agent's subagents``. Each child's newest run is matched by `agent_id`, falling back to `agent_name`, with the latest `started_at`.

For a user shell, [`address::launched_children_in_channel`](../../../crates/rimz/src/address.rs) keeps every launched child that matches `Ctx::channel`. A child with a channel stamp matches it exactly, or as the dashed form of a branch-style filter; a child without one matches the channel composed from its worktree directory name. It never infers membership from a live parent, so ended parents and adopted identities do not hide child history. [`address::launched_parent`](../../../crates/rimz/src/address.rs) is the inverse join, so each row labels its parent launch's current row, and the table adds parent and channel columns.

A plain shell in the project directory cannot derive an in-place team's `<directory>/<team>` channel from cwd, so `Ctx::channel` is `None` there and user-shell `list` shows every channel; the channel column tells those in-place lanes apart. A RimZ-launched pane carries `RIMZ_CHANNEL`, and a shell in a separate worktree derives its worktree channel.

## The lifecycle, end to end

1. The parent launches. Caller policy and ancestry pass, the run record and pane are created, and the petname prints. With `--wait`, the parent then joins.
2. A provider that dies while starting is relaunched, up to `[agents] startup-relaunches` times (default `3`). When the provider exits and the run is still `Pending`, RimZ accepted no lifecycle observation from it; that is the whole test, and it is not proof that the provider did nothing. The wrapper holds `RUN_EXIT_TERMINAL_GRACE` for a late first hook while it asks `harness::run::startup_relaunch`: a fresh launch (not a resume or a fork), a nonzero exit, a provider the wrapper did not end itself, no stop or interrupt signal, and fewer relaunches than the cap. When all hold it prints one stderr line, waits `[agents] startup-relaunch-wait` (default `3s`), and asks again on fresh evidence, so a stop, an interrupt, an ended parent, a run that settled or timed out, or a late first hook during the wait cancels the relaunch and the last exit settles. Otherwise it spawns the same command again in the same pane, records the new `provider_pid`, hands the parent watchdog to the new process, and appends one [`launch_retry` assist](./loops.md#the-assist-log) per relaunch. Handle, run record, pane, and `deadline_at` are unchanged: the wait comes out of the run's deadline, `wait`, `list`, and the fleet rule see one starting child, and a death that is relaunched is never reported. When every relaunch is spent and the last process dies the same way, the exit settles as any exit does, and the wrapper says so on stderr only after the failure tail is captured, so the reported reason stays the provider's own output. A relaunch that fails to spawn records `relaunched: false` and settles the previous exit as an ordinary failure. No provider error text enters the decision, and only the last attempt's pane output is captured.
3. The child runs until its work ends. A clean turn end with an owed harness wake parks its run as `Running`, so the parent digest still waits for it; otherwise its hooks fold a terminal status into the run record.
4. `harness::run::update_record` publishes a subagent's response under the workspace lock before writing the newly terminal record, so any reader that sees the terminal record (`wait` loads it unlocked) finds the file too. `publish_response` writes non-empty `last_message` bytes to `run::response_path`: `out/<reader>/<agent_name>.output` for the first answer and `<agent_name>.<follow_ups + 1>.output` for later answers, where `RunRecord.reader` is the launcher's handle stored at creation (the run's own name when none was stored), adding a trailing newline if missing; empty or absent messages remove only that ordinal's file. Earlier answers remain untouched. Identical bytes leave the file untouched. A publish failure warns without failing the transition. The child's in-pane wrapper calls the fleet reporter for terminal records that owe any answer ([`cli/agents_cmd/subagent_report.rs`](../../../crates/rimz/src/cli/agents_cmd/subagent_report.rs), `report_settled_child`). Its exit path also calls it after guaranteeing a terminal run. One-shot providers exit on their own and close their panes as before; interactive providers stay available for follow-ups.
5. The reporter reads the launcher's current row (`AgentState::launcher`) and the runs selected by `FleetRuns` below for its launched children plus the peers stamped with `launched_by`. For an active launcher it ensures response files for every selected terminal row before checking for running siblings. This repairs failed transition publishes and publishes agent-launched `rimz agents -p --bg` peers, whose waits can return before the wrapper reports. A missing or ended launcher, or any selected non-terminal run, queues nothing.
6. It keeps terminal runs that owe a report (`RunRecord::owes_report`) and emits each answer with neither `report_message_id` nor `joined_at`, earlier answers first, then the current answer. It measures their published paths. A run without `agent_name` has no response path. Earlier answers retain metadata, not text: a missing earlier response file renders as `no response`, and the publisher repairs only the current answer. Every path is the host path; earlier answers come from `run::earlier_response_path`. Other publish or measurement failures abort before any stamp, so the backstop can retry.
7. It mints the message id, stamps it onto every listed answer as `report_message_id`, and only then queues a parked harness notice with gate `Done`. When every row is a subagent run, the notice is `SubagentReport` (`SUBAGENT_REPORT`); otherwise it is `AgentReport` (`AGENT_REPORT`), including mixed fleets. The same decision selects the heading noun. A queue failure clears only those answers' stamps so the backstop can retry. Both envelopes read `From: @rimz`, and immediate pane delivery is best-effort latency over the durable records.
8. After the parent's receiving turn ends, the wrapper stops the idle child, stamps its own row's `ended_at` and closes its pane. The receipt and hold checks below must all pass. With `--keep`, automatic completion cleanup is off; after provider exit the wrapper lingers with a stderr line naming `rimz subagents stop`, until a stop signal arrives. Parent exit alone does not reclaim it.
9. The run record survives the close, and the ended child stays under any visible row of its parent's launch, so `list` and `wait` still report the outcome and the card keeps its verdict until the parent's next prompt boundary. A retained child is at rest and never holds its parent in `running` ([model.md](../agents/model.md#observed-ends-and-reaped-ends)); the run record's own outcome is what `list`, `wait`, and the digest report.

[`cli/agents_cmd/exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs), `RunMonitor`, keeps the record poll at 250 ms and checks terminal receipt and hold state at 1 s intervals. Cleanup requires a terminal run with no answer still owed a report, no live waiter, current-answer receipt through `joined_at` or a `Delivered` digest in message history, no open child turn, no non-terminal queued message addressed to the child, and no open parent turn. `RunExecContext::parent_received_and_rested` reads the queue and history before `snapshot_cached`, whose rest certificates inform `holds_open_turn`. Pane send leaves a message `Sent`; the receiver's turn hook folds `TurnStarted` before acknowledging `Delivered`. An absent or ended parent counts as having no open turn.

[`harness/run.rs`](../../../crates/rimz/src/harness/run.rs), `fold_lifecycle`, reopens the same terminal subagent run on a matching `TurnStarted`. It carries the replaced answer into `earlier_answers` unless joined, retaining its digest link, and drops joined earlier entries. It increments `follow_ups`, sets `follow_up.started_at` to the reopen time with no prompt, resets `opened_by`, then sets `Running`, with `completed_at`, `parked_at`, `joined_at`, `report_message_id`, and `deadline_notice_at` cleared and the deadline re-armed to `now + timeout`. Old records without a stored timeout re-arm from the previous answer's start (`follow_up.started_at`, else `started_at`), so later reopens do not compound the timeout. The new fields default to empty on old records. The next completion is newly terminal again and can produce another digest without losing the earlier answer. Verification retries and park/unpark cycles keep the same ordinal. Other terminal runs remain absorbing.

`harness::fleet::FleetRuns` is the shared settlement rule for the reporter, its orphan-sweep backstop, and the owed-wake predicate. It selects each member's newest non-peer run with matching kind and either session id or a present matching name. For records carrying `RunRecord.peer`, it instead includes every open turn and every terminal turn neither joined nor reported, matching kind and launch or bound session identity, never name. The combined set is deduplicated by run id. A supervised parent parks at a clean end while any selected run is live or owes an answer's report; the queued digest then holds it until delivery opens its next turn ([parked runs](./scripting.md#parked-runs)).

`RunRecord.report_to` (`launcher`, or `nobody` for a `--detach` launch) is the policy of the answer the launch prompt opens, and two source rules carry it instead of a guard in each consumer. `RunRecord::answer_claims` never marks an answer on a `nobody` run as owed, so no digest row is built from it. `FleetRuns::of` drops a member whose selected run is `nobody`, after the newest-run pick so an older attached run does not resurface; the digest, its backstop, the owed-wake fleet term, and the pending-wait projection therefore all ignore detached work, running or settled, and a mixed fleet reports its attached members as soon as they settle. Lineage, the response file, the card, and an explicit join (`joined_at`) are unchanged. A subagent reopen sets the record back to `launcher` and does not carry the detached answer into `earlier_answers`, so only the follow-up is reported; a message that lands while the detached answer is still open is part of that answer. `update_record` publishes the response file of every newly terminal `nobody` run, the plain `-p --bg` run included, since no fleet reporter sees it: that terminal write is its one publication attempt, a failure warns, and `rimz agents wait` or `subagents wait` still reads the answer from the record. The receipt and the launch reminder make no report promise a detached launch would break.

Delivery confirmation stamps the hook's `RIMZ_RUN_ID` subagent through `harness/run/peer.rs` inside the [acknowledgment callback](./messaging.md#confirmation-and-retry), without reacquiring its workspace lock. Both prompt-correlated and oldest-sent-batch confirmations qualify. Only a nonterminal subagent bound to the confirming card, with neither peer nor team metadata, is written. Conversation records (human or agent senders) append unique message ids to `opened_by`; the first such confirmation fills an unset `follow_up.prompt` with their bodies joined by blank lines. Later deliveries, including while parked, append ids without replacing that task. Notice-only confirmations stamp nothing, and direct pane input supplies no task. Follow-up rows use this prompt's first line rather than the launch description; their elapsed time starts at reopen.

`PeerRun` records the persistent peer's `launch_id` and the launcher message ids in `opened_by` (empty for a launch prompt). Each launcher-opened turn has its own record, with `subagent = false`; terminal peer records never reopen. `RunRecord::prompt_origin` classifies their prompt as parent-authored. Peer responses publish on the same locked newly-terminal transition as subagent responses, but use `<agent_name>.<run_id>.output` rather than the subagent's follow-up ordinal under `StatePaths::subagents_dir`, so a later turn cannot overwrite an unreported answer. The reporter retries the same idempotent publisher, except for a detached run, which no reporter sees.

`harness/run/peer.rs` owns the peer-run interface: `create_peer_prompt` prepares a pending run under the workspace lock; `open_peer_run` finds a nonterminal run by kind and stable launch id; `record_run_delivery` runs inside the [acknowledgment callback](./messaging.md#confirmation-and-retry); `fail_peer_run` settles an open run and wakes its waiter. Creation and enrollment enforce at most one open run per peer. Further launcher messages join that run, including while parked, preserving its original task and adding opener ids. Human-launched agents, provider-native agents, and team seats (`AgentState::is_team_seat`) are excluded before scanning runs. The interface does not put a run id into the persistent pane's environment.

Interactive launch calls that interface after `begin_agent_launch_batch` assigns the prompt leader's identity and before placement opens a pane. Only an agent-launched, nonempty prompt gets a pending run; resume bypasses it. Compilation or placement failure fails the run. Native root turn-start and turn-end coverage gates both launch and message enrollment: unsupported adapters still launch, but their receipt explicitly promises no report. Supported launch receipts map the response through the launcher's `TmpView` and name the wait and follow-up commands by the peer's handle, `@<name>#<channel>`, which resolves the pending run before registration.

`create_peer_prompt` stamps `report_to` on a record it creates and returns a reused open run unchanged, so the receipt describes the record rather than the flag. A peer turn a later launcher message opens is a new record and reports to the launcher.

An agent-launched team reports once per `Done` or cohort death instead ([team reports](./teams.md#team-reports)). `address::launched_fleet` leaves team seats out of the per-member fleet digest, and `create_peer_prompt` hands a team seat's launch prompt to `harness/run/team.rs`. The read-only orphan sweep also selects live launchers with open runs whose entire team cohort has ended; the digest helper rechecks and settles those teams before settling peer turns.

The digest has a heading (`Your subagent settled:` or `All N subagents settled:`, with `background agent` in place of `subagent` unless every row's run is a subagent run) and one row per selected answer: its status, elapsed time from that answer's start, the last line of its failure tail for a non-completed answer, its task, and its ordinal response path with `FileSummary::label` (estimated tokens and physical lines from `FileSummary::measure`), or `no response`. The launch answer uses the launcher `--description`, else a bounded first-line prompt preview. Follow-ups use the bounded first line of their own stored prompt, omitting `task:` when unknown rather than repeating the launch task. A peer turn always uses its own prompt preview, not the launch-time description. Several rows may name the same child or peer, and the heading counts rows. With two or more response files the heading appends `responses total {label}` over their summed `FileSummary`. It ends at its last row, with no trailing instruction.

A fleet is every child launched before the digest is composed: a child launched while siblings run joins it, and one launched after composition starts belongs to the next. Response files land per answer; digest stamps wait for the fleet. `run::report::record_report_messages` stamps the selected `(run_id, ordinal)` set under the workspace lock, all or nothing, and backs off if any selected answer was already reported or joined, disappeared, or its run reopened. Thus two last settlers racing on the same fleet produce one digest, and the loser queues nothing.

### Joins, stops, and the digest

Inline joins, parent stops, and the digest share a two-field handshake per answer on the run record ([`harness/run/report.rs`](../../../crates/rimz/src/harness/run/report.rs)). Both mutations take the workspace lock.

| Actor | Stamps | Then |
| --- | --- | --- |
| a join that prints a terminal run to an attended caller | `joined_at` on the printed answer's ordinal only | cancels the queued digest if every answer carrying its id is joined |
| a blocking `rimz agents -p` driver, before it closes the pane of a terminal run | `joined_at` on the printed answer only | same check |
| the parent's `rimz subagents stop` | `joined_at` on every answer of each selected child's newest run, before cancelling any run | checks each linked digest, then cancels the runs |
| the fleet reporter | `report_message_id` on every listed row, before queueing | nothing further |

A `joined_at` stamp excludes that answer from a digest not yet composed; joining the current answer leaves unprinted earlier answers owed. A digest already queued is cancelled only when every answer it lists is joined, including answers carried through a reopen, so an unread row keeps the notice, and a digest already delivered cannot be recalled. `stop` stamps the whole selection first because the first run cancellation wakes a reporter that would otherwise list an unstamped sibling. This also lets `stop` cancel multiple queued digests for a `--keep` child once every listed answer is joined or stopped. The wrapper's report trigger and receipt gate both consult `RunRecord::owes_report`, so joining only the latest answer cannot hide an earlier owed answer from cleanup.

Reading a response file does not stamp `joined_at`. Response files die with the room; the run record stays the truth. `wait` never closes a pane, still prints a failed child's transcript tail, and is the path for a caller that needs the child's text synchronously or wants to reread durable history.

### Attendance

A printed result counts as consumed only while the calling agent holds an open provider turn. The [wait print path](../../../crates/rimz/src/cli/agents_cmd/wait.rs) resolves the caller against `Store::snapshot_cached()` and asks `AgentState::holds_open_turn`. The cached snapshot attaches provider rest certificates, so a turn settled without a `Stop` hook reads as closed, while native waits and proposed plans keep the turn open. Attendance is re-read at each print: a background wait that outlives its parent's turn still prints, but leaves its rows unjoined so the digest reaches the parent at its next boundary.

An unresolved caller is a human shell and counts as attended; a failed snapshot read logs a warning and also counts as attended. `stop` skips the check because dismissal is deliberate.

### Backstops

The parent watchdog ([`harness/parent_watch.rs`](../../../crates/rimz/src/harness/parent_watch.rs)) runs on its own thread in the wrapper of every child whose run does not survive its parent (`RunRecord::survives_parent`: kept, or reporting to nobody). Admission is decided once at wrapper start. Every `PROBE_INTERVAL` (60 seconds) it rereads the parent launch's current row, so an in-place restart can move panes. A durable end stamp cancels the child only when every row of the parent launch has ended; a conversation switch leaves a live successor and is not an end. Pane loss cancels only after `PANE_GONE_STRIKES` (3) `RequireAuthoritative` mux reads that lack the pane, plus a reconfirmation `RECONFIRM_DELAY` (500 ms) later; a cached roster hit counts as present.

The elected sidebar producer runs [`harness/orphan_sweep.rs`](../../../crates/rimz/src/harness/orphan_sweep.rs) at most once a minute. It reads durable records only and starts hidden helpers, each of which rechecks the records before acting:

| Scan | Condition | Helper action | Diagnostic |
| --- | --- | --- | --- |
| missed digest | a live launcher whose `launched_fleet` rows are all terminal, with at least one row neither reported nor joined | runs the same fleet reporter | `subagent_digest_backstopped` when it queues |
| abandoned peer turn | a launcher's open peer run whose row ended, whose recorded process died, or which is parked with no running or unreported runs in its own fleet and no armed wait for its session in the loop catalog | rechecks the peer; fails an ended or dead peer's turn, or checks a parked turn twice five seconds apart through `settle_stranded_park`, then runs the fleet reporter | `subagent_digest_backstopped` when it queues |
| orphan | a live child that does not survive its parent (neither kept nor detached) whose parent launch's latest row ended, or that has no row, `ORPHAN_GRACE` (10 minutes) ago, measured from that end stamp or from the child's registration | closes the child and records its durable end | `subagent_orphan_reaped`, or `subagent_orphan_repair_failed`, which stays eligible for the next scan |

Row stamps make repeated passes and races idempotent. Each diagnostic means the normal wrapper path was missed ([diagnostics.md](../diagnostics.md)).

Peer settlement never closes its pane. The read-only sweep delegates parked candidates only when their own fleet has no running or unreported runs and their run's session has no armed wait in the loop catalog. It still reads no message queue; the helper verifies that nothing is owed on both checks and keeps a racing wake turn intact. A directly exec'd provider can die before its first hook without an ended row: the wrapper attaches its process identity before exec, so the peer backstop also checks that recorded process's liveness, including for a `Pending` launch run. Unknown liveness is not death. Restart fails an open peer turn before opening the replacement, and room rebirth fails open peer turns whether their peers recover or not. Idle peers with no open turn create no failed result.

Neither scan covers a `--keep` child: its wrapper runs no watchdog, and the orphan scan skips kept runs, so only `rimz subagents stop` or a manual pane close ends it. A `--detach` child is exempt from both the same way, but keeps automatic completion cleanup: with nobody to receive its answer, `RunMonitor::poll` cleans up a terminal `nobody` record as soon as no waiter is live, without waiting for a parent turn, and after the resumed-wrapper reopen hold. Linger, timeout pane retention, and the resume exit policy read `keep` alone. Because watchdog admission is decided once, a detached child that a follow-up resumed and reopened as `launcher` runs without a watchdog for that wrapper's life; the orphan scan rereads the run and is its backstop. Stopping the parent through `rimz agents stop`, or `rimz teams stop` reaching that parent, stops its live launched children first, kept ones included.

A terminal child awaiting receipt can linger while its parent lives. Its bounds are parent loss through the watchdog or orphan sweep, the parent stop cascade, and `rimz subagents stop`, not `run_timeout`: [`harness/deadline.rs`](../../../crates/rimz/src/harness/deadline.rs) evaluates only `Pending` and `Running` records. `deadline_at` is the soft bound; `kill_due` uses `deadline_at + grace`. `due_rung` selects the latest crossed warning or stop above `deadline_notice_at`, and `run::claim_rung` claims it under the workspace lock. The hook feed skips provider-native children and asks a context-capable adapter to attach the rung only on an accepted event. `stop_channel` chooses hook context or pane steering, never both.

The read-only producer in [`harness/run_timeout.rs`](../../../crates/rimz/src/harness/run_timeout.rs) spawns one hidden `agents run-timeout` helper for either a pane stop or a kill. The helper stamps a pane stop before dispatching a `HarnessNotice::Deadline` steer; this notice never counts as an owed wake. At the kill bound it reads the provider transcript outside the lock, then `timeout_if_due` atomically fills an absent `last_message` and settles `TimedOut`; the usual terminal publisher writes the partial response. Heavy-lane tick cadence bounds enforcement precision, and without an elected producer there is no enforcement. Old records without grace still kill at `deadline_at` and never deliver a stop rung.

A child whose live turn died on a provider limit keeps its run open: the traced case is a Codex rollout ending in `usage_limit_exceeded` with no `Stop` hook, which leaves the row raw `running`, the run `running`, and a limit marker in the context sidecar. [`harness/park_notice.rs`](../../../crates/rimz/src/harness/park_notice.rs) is the read-only detector. On the heavy-lane tick it takes the run list `run_timeout::enforce` already read and the producer's context-joined agents, and `unnoticed_park` selects a nonterminal subagent run whose child is live, raw `running`, and parked under `auto_continue::limit_marker_active` (rate and spend classes; an overload or budget park is `paused` in `list` but gets no notice), whose launched parent is live, and whose `RunRecord::park_noticed_activity` is absent or older than the child's `last_activity`. Each match spawns one hidden `agents park-notice` helper.

The helper rereads the audit projection with rest certificates joined, folds the per-tool heartbeat onto it with `agent_activity::read_for_keys` and `SidebarSnapshot::with_agent_activity` (the fold the producer's enrich applies, so both sides name the park by the same heartbeat-raised `last_activity` and a claimed park stops the detector from spawning), and re-applies the predicate, then `run::claim_park_notice` stamps the child's `last_activity` on the run record under the workspace lock, and only a successful claim queues one `HarnessNotice::SubagentPaused` for the parent: gate `Done`, the parent's card and channel, pinned to the parent's pane when it has one, with one boundary attempt. A failed queue write gives the claim back. `last_activity` names the park because it is frozen while the turn is dead and advances on any resume, so helper races and restarts send one notice per park and a second park sends a second. The notice is not a fleet digest and never counts as an owed wake; the child's open run already owes the parent `Subagents`. It appends no assist record, like the other notices. Without an elected producer there is no notice, and it can be no sooner than the marker's write, which for a hookless stop is the rollout refresh. A hooked turn end over an errored rollout still settles the run `failed` at once and sends the fleet digest instead.

### Follow-ups and resume

After flushing a reply, `message --wait` calls `harness::run::report::claim_reply`. A caller must be the child's launcher holding an open turn (`AgentState::holds_open_turn`, as for ordinary joins), or the human shell. The claim polls durable run records for the sent message id in the current or earlier answer's `opened_by`, including `PeerRun.opened_by`; only a terminal matching answer is joined through `run::report::join_and_settle_digest`. It returns at once when no eligible record holds the id; while a matching answer is still open it polls, capped at two seconds and the command's remaining deadline, with no claim on expiry. A failed write never claims an answer; a missing final message or delivery failure also leaves it owed. Text fan-out claims after each printed reply; JSON claims only after the selected map is flushed. Earlier answers and unprinted `--any` losers remain owed. A queued multi-answer digest retains the existing partial-join duplicate rule.

The parent can message a live interactive child in the same session. For an ended child, [`cli/message/dispatch.rs`](../../../crates/rimz/src/cli/message/dispatch.rs), `recipient_miss`, calls the published `cli/subagents::resume_child` doorway before reporting a miss. It resolves the caller through ancestry against the audit projection and the target only among that caller's launched children. A user shell, peer, or sibling cannot resume it this way.

[`cli/subagents/resume.rs`](../../../crates/rimz/src/cli/subagents/resume.rs) checks the recorded cwd, subagent-profile posture, sandbox skills and capability, trust, login and provider session resume support, then the resolved login's [health](./fleet.md#every-path-that-builds-a-launch-layout), before opening a pane. A missing session, unsupported resume or degraded posture is a miss with its reason, never a fresh launch. The request reuses the child's session, launch identity, newest run and keep policy; cwd comes from the child without taking checkout ownership. Placement uses the parent's session and the ordinary subagent zone fallbacks. Once a non-ended row binds, dispatch retries once through the normal message path, including reply waits. For a non-lazy provider the message parks until its next lifecycle observation, normally registration, or one delivery window elapses; lazy-registering providers keep immediate delivery. The parent sees `queued` with the resuming reason while it waits. The message is the next prompt; `TurnStarted` reopens the run and the receiving-turn cleanup rule applies again. Explicit stop, failure and timeout do not prevent a later parent-message resume when its preconditions pass.

Every subagent resume reuses a run id and therefore stays supervised by the exec wrapper. This wrapped resume appends `rimz.agent-resumed` with `Registered` after recording its pane and before spawning the provider, as every resume does ([model.md](../agents/model.md)). This makes the row live without waiting for a lazily-registering provider's first hook. Attach alone changes placement, not lifecycle. When the reused run is terminal before spawn, the wrapper arms receiving-parent cleanup only after `TurnStarted` reopens it. The durable follow-up ordinal detects this even when the turn completes between monitor polls. Dispatch queues only after the row binds, so the wrapper's first poll cannot rely on a message record to hold cleanup off. A bind timeout asks the parent to check the pane or launch a new child.

A queued message whose child has ended is archived by the sweep, including ends stamped directly by the wrapper, reaper or rebirth rather than a provider hook. A provider end hook archives first through the lifecycle reactor; both name `rimz message @<handle>` as the parent's resume-and-resend path, through one reason formatter. If the parent resends before archival, the resumed child's older queued head stays first in FIFO order. A missing pane without an end stamp keeps the ordinary no-pane retry.

### Signals are not the settlement path

A child's lifecycle transitions also fire `agent.*` signals ([loops.md](./loops.md#the-signal-vocabulary)) carrying `session`, `handle`, and, when recorded, the parent's session id in `parent`. A parent could arm `rimz loop add child-ended --signal agent.ended --match session=<child> --wait --once`, but the digest and `wait` carry the child's text and run identity, while a signal carries only the transition. The self-wait guard refuses a subscription that does not name another agent, so `--match parent=<own session>` alone is rejected.

## What is left out

The startup relaunch above is the one automatic relaunch, and every fresh launch through the exec wrapper gets it: a subagent, an `agents -p` run (whose `--retries` stack on top, each attempt with its own relaunches), a loop spawn or check run, and a root launch, which has no run record and is judged by its launch card instead ([fleet.md § A startup death is relaunched first](./fleet.md#a-startup-death-is-relaunched-first)). A child whose run left `Pending` is never relaunched, nor is an exit with status 0, a stopped, timed-out, or interrupted child, or a resumed child, whose run is terminal and not `Pending`, so nothing durable says its session failed to open. Two cases are accepted, not handled. Every built-in adapter declares a prompt-submit or pre-invocation hook, but no provider's order is observed, so a provider that does side-effecting work before its first hook and then exits nonzero is relaunched and may repeat that work. And a hook from a dead provider that lands after both the grace and the wait binds the run to that dead session, so a later provider's observations are ignored and the child runs to its deadline and reports `timed out`; a hook that lands during the wait cancels the relaunch instead.

There are no `subagents restart` or `subagents resume` verbs. Parent-message resume continues an existing session; when its preconditions fail, launching the same profile and prompt creates a new child instead.

The durable launch record does not stamp which profile namespace produced a child. Generic restart therefore resolves `[agents.profiles]`, and a subagent-only profile degrades or refuses through the missing-profile path. Persisting the doorway scope with the launch event is the upgrade path. Room resume and rebirth exclude children entirely; [fleet.md § Resume and rebirth](./fleet.md#resume-and-rebirth) owns crash-time run settlement. The parent-message doorway explicitly selects the subagent profile scope; it does not change generic recovery.

A child is addressable as `@<petname>`. One-shot providers cannot be kept interactive by the wrapper; follow-ups after their exit require provider session resume support.

## See also

- [scripting.md](./scripting.md): the supervised run every child is, including the subagent retention exception.
- [fleet.md](./fleet.md): the launch, address, and reclaim machinery, and the launch reminder order and channels.
- [model.md](../agents/model.md): the rollup, and the provider-native subagent rows this page's predicates exclude.
- [cli/subagents.md](../../reference/cli/subagents.md): the user-facing command and flag surface.

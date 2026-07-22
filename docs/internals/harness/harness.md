# The agent harness

> The entry point for contributors working on the harness. This page orients you across the whole area, then owns the launch core: spawning a fleet, addressing it, resuming it, capping what it spends, and reclaiming what it leaves behind. The other five pages in this folder go deep on one job each.

## What the harness does

One agent in one thread is a conversation. Tens of agents across a dozen worktrees is a team, and a team needs a way to be started, named, driven, and cleaned up. The harness is that machinery.

It exists because RimZ has no API into the agents it runs. Every agent is a stock provider CLI in a real terminal pane, so the harness works the way a fast human would: it opens panes, types into them, watches the durable records the agents' hooks write, and closes panes when the work is done. The same machinery serves a human at the keyboard, a shell script, a CI gate, and one agent driving another.

## The rules that shape it

Four rules explain most of the design. When a piece of the code surprises you, one of these is usually the reason.

**Durable records are the truth.** Panes are latency: they can close, renumber, or wedge. Launch identity, message queues, run outcomes, and schedule history live in the store, so recovery reads state rather than guessing from a multiplexer. Every subsystem here has one durable record at its center, and everything else is an attempt against it.

**One compile target.** Every way of starting agents (an inline layout, a named team, a `-p` run, a resumed cohort, a reborn room) resolves to the same backend-neutral list of pane commands. Zellij and tmux receive identical input, so a feature written once works on both.

**One name for one agent.** A member is reachable by an address, `@handle#channel`, and the renderer that prints a handle is the exact inverse of the parser that reads one. Anything RimZ shows you, you can type back.

**No daemon.** Nothing in the harness runs a background service. Scheduled work, message wakeups, and unattended recovery all ride the tick of the room's elected sidebar producer, the elder. Close the room and the clock stops: nothing runs when you are not there, and nothing outlives the room you can see.

## One launch, end to end

`rimz agents claude,codex -w feat-a "start on the parser"` touches most of the subsystem. Following it once is the fastest way to see how the pieces connect; each step links to the section that owns it.

1. **Resolve the spec.** `claude,codex` parses into a `LayoutSpec` of two agent cells, with profiles and teams resolved from the effective config ([The layout IR](#the-layout-ir)).
2. **Finalize the launch.** Permission posture, presets, budget, and passthrough argv fold into each cell ([From spec to panes](#from-spec-to-panes)).
3. **Reconcile a prior cohort.** Because this is a multi-cell launch with an explicit `-w`, RimZ checks whether `feat-a` already held these agents, and may focus, resume, or offer to clear it instead of launching ([Cohort relaunch reconciliation](#cohort-relaunch-reconciliation)).
4. **Choose where it lands.** Two cells plus a worktree means a new tab. Placement is decided now, before anything durable exists, so a rejected placement leaves no debris ([Placement](#placement)).
5. **Create the worktree.** A marked Git worktree is added and seeded, and its name `feat-a` becomes the channel every cell is stamped with ([worktrees.md](./worktrees.md)).
6. **Mint identities.** The store writes a provisional row per cell with its name, channel, and cohort stamps, so both agents are addressable as `@claude#feat-a` and `@codex#feat-a` before either has run a turn ([The address](#the-address)).
7. **Open the panes.** Each cell compiles to a hidden `rimz agents exec` wrapper invocation carrying an `ExecRequest`. The wrapper validates it, then runs the stock provider CLI in the pane with the trailing prompt in its launch argv ([The exec wrapper](#the-exec-wrapper)).
8. **The agents report themselves.** Their lifecycle hooks write durable events, which the rollup folds into the state every later command reads: messages waiting for a turn boundary ([messaging.md](./messaging.md)), supervised runs waiting to complete ([scripting.md](./scripting.md)), and budget ticks watching the spend ([budget.md](./budget.md)).
9. **Reclaim what is left.** When a pane ends, the resident wrapper decides whether the exit was deliberate, and only then removes the worktree or closes the pane ([Reclaiming a pane](#reclaiming-a-pane)).

Steps 1 through 7 are the same for every entry point. A `-p` run, a scheduled loop fire, and a reborn room all reach the panes through this path with a different request on the front.

## Where the code lives

The harness is a product area, not a single Rust module. It spans four source trees, and the seven pages in this folder are grouped by the job they document rather than by the crate path they point at.

| Page | Owns | Source |
| --- | --- | --- |
| harness.md (this page) | Spawn, address, resume, reclaim | [`harness/`](../../../crates/rimz/src/harness) |
| [scripting.md](./scripting.md) | Supervised `-p` runs | [`harness/run.rs`](../../../crates/rimz/src/harness/run.rs), [`run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs), [`cli/supervised/`](../../../crates/rimz/src/cli/supervised) |
| [loops.md](./loops.md) | Scheduled tasks and unattended recovery | [`harness/schedule/`](../../../crates/rimz/src/harness/schedule), [`auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs), [`auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs) |
| [budget.md](./budget.md) | Dollar caps and the park they produce | [`harness/budget.rs`](../../../crates/rimz/src/harness/budget.rs), [`cli/budget.rs`](../../../crates/rimz/src/cli/budget.rs) |
| [messaging.md](./messaging.md) | Getting text into a running agent | [`message/`](../../../crates/rimz/src/message) |
| [worktrees.md](./worktrees.md) | RimZ-owned Git worktrees | [`worktree.rs`](../../../crates/rimz/src/worktree.rs) |
| [trust.md](./trust.md) | Which parts of a project config may execute | [`trust.rs`](../../../crates/rimz/src/trust.rs) |

Inside `harness/` itself, start here when you are looking for where a behaviour lives.

| File | Owns |
| --- | --- |
| [`spec.rs`](../../../crates/rimz/src/harness/spec.rs) | The layout IR: the inline grammar, team and profile resolution, virtual `<kind>-<mode>` cells, prompt-file path rooting, the prompt leader, and the name-collision rules that keep profile names addressable. |
| [`plan.rs`](../../../crates/rimz/src/harness/plan.rs) | Turning a spec into a launch: effective-config resolution, launch finalization, placement resolution, per-cell launch identities, and compilation to backend-neutral pane commands. |
| [`launch.rs`](../../../crates/rimz/src/harness/launch.rs) | The provider process: adapter argv for launch, resume, and fork; the hidden `ExecRequest` wire; launch environment composition; the login-shell wrapper; and preflight. |
| [`target.rs`](../../../crates/rimz/src/harness/target.rs) | The address: parsing `@handle#channel`, resolving it against a snapshot, binding a match to a live pane, and rendering the canonical handle back. |
| [`petname.rs`](../../../crates/rimz/src/harness/petname.rs) | The adjective-noun instance names, their collision check, and the deterministic fallback for records written before petnames existed. |
| [`resume.rs`](../../../crates/rimz/src/harness/resume.rs) | Resume planning for room rebirth, explicit cohort resume, and lane resume, plus `resolve_posture`, the relaunch posture seam every path shares. |
| [`rebirth.rs`](../../../crates/rimz/src/harness/rebirth.rs) | Two-phase inspection of the previous incarnation of a room, over the shared recovery plan. |
| [`budget.rs`](../../../crates/rimz/src/harness/budget.rs) | Dollar caps and their parks. See [budget.md](./budget.md). |
| [`run.rs`](../../../crates/rimz/src/harness/run.rs), [`run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs) | Supervised runs. See [scripting.md](./scripting.md). |
| [`schedule.rs`](../../../crates/rimz/src/harness/schedule.rs), [`schedule/`](../../../crates/rimz/src/harness/schedule) | Loop tasks and their runner. See [loops.md](./loops.md). |
| [`auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs), [`auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs), [`assist_log.rs`](../../../crates/rimz/src/harness/assist_log.rs) | Unattended recovery and its audit trail. See [loops.md § Recovery the elder runs](./loops.md#recovery-the-elder-runs). |

The CLI side lives in [`cli/agents_cmd/`](../../../crates/rimz/src/cli/agents_cmd) (launch, restart, resume, fork, stop, and the hidden `exec` wrapper), [`cli/supervised/`](../../../crates/rimz/src/cli/supervised) (the run driver both `agents -p` and loop fires call), and [`cli/loop_cmd/`](../../../crates/rimz/src/cli/loop_cmd). Those handlers parse flags, execute effects, and render; the rules live here.

### Where to start reading

Read this page first, then the page for whatever you are changing. If you are new to the whole area, [messaging.md](./messaging.md) is the most instructive second read: it is the subsystem where the durable-record rule has the most consequences, and the delivery pipeline touches nearly everything else.

### The state machines

Six state machines carry most of the subsystem's behaviour. Each has one owning type and one owning section.

| Machine | Type | Documented in |
| --- | --- | --- |
| Message lifecycle | `MessageStatus` | [messaging.md § Status lifecycle](./messaging.md#status-lifecycle) |
| Supervised run | `RunStatus`, whose `exit_code` is the caller contract | [scripting.md § Status and exit codes](./scripting.md#status-and-exit-codes) |
| Loop task timing and outcome | `TaskTiming`, `LoopRunResult` | [loops.md § Schedule shapes](./loops.md#schedule-shapes), [§ History, strikes, and pauses](./loops.md#history-strikes-and-pauses) |
| Dollar budget ledger | `BudgetVerdict` | [budget.md § The verdict](./budget.md#the-verdict) |
| Worktree removal | `ProtectionSet::assess` | [worktrees.md § The assessment](./worktrees.md#the-assessment) |
| Project trust | `TrustState` | [trust.md § States](./trust.md#states) |

Two more decision trees are not enum-shaped but behave like state machines: the [deliberate-exit classification](#reclaiming-a-pane) that decides what happens when an agent process ends, and the [park class](./loops.md#recovery-the-elder-runs) that decides when a stopped agent may resume itself.

## The vocabulary

Spawning separates three independent choices, so any combination is one command: **agents** choose which tools run, **layout** chooses the shape on screen, and **channel** chooses the cooperation lane. Three words name the parts.

A **channel** is one cooperation lane. It is backed by a durable bare name, a [worktree](./worktrees.md), an in-place named team stamped as `<dir>/<team>`, or the directory room itself. The sidebar groups by it, and an address narrows to it with `#<channel>`.

A **member** is an agent inside a channel, named by a **handle**: `@claude` the kind, `@planner` the profile, `@coder` the team role, `@writer` an explicit launch name, `@swift-otter` the minted petname.

An **address** joins the two as `@handle#channel`. It is how every command names who it reaches.

```text
one room, grouped into channels: named lanes, worktrees, teams, directories

  #feat-auth   @claude    planning       @codex  reviewing
  #design      @planner   outlining
  #deps        @codex     -p run (from CI)
  #docs        @planner   queued: "draft the API"

reach a member by @handle#channel, then:
  message --steer @claude  →  talk to it now
  message @codex           →  talk now if free, otherwise leave a task
  message --schedule 1h    →  leave a task no earlier than one hour from now
```

Sending is [messaging.md](./messaging.md). This page covers the two halves around it: getting a member to exist, and naming it.

## Launching a fleet

### The layout IR

`rimz agents <spec>` resolves either a named `[agents.teams]` entry or an inline DSL, and both compile to the same backend-neutral panes. The inline grammar is compact: commas split columns, plus signs tile rows within a column, and slashes stack rows within a column on Zellij.

```text
claude,codex+term          → Claude left; Codex tiled over a shell right
claude/codex/term          → two agents plus a shell in one Zellij stack; tmux tiles them
vim,htop+zsh               → raw command panes
claude-auto,codex-yolo     → agent cells with adapter-owned permission posture
claude:planner,codex:coder → agent cells with ad-hoc `@planner` and `@coder` handles
```

Each cell is one of: the built-in `term`, a registered agent kind, a virtual `<kind>-<mode>` variant, a configured profile, a configured command, or an executable found on PATH ([configuration.md](../../guide/configuration.md#agent-profiles-commands-and-teams)). Configured cells and built-ins resolve before PATH. An agent cell may carry an ad-hoc role as `<cell>:<role>`; inline roles follow team-role naming and addressing rules, stay unique within the spec, and apply only to agent cells.

A named team is an ordered role list. It opens as one column per role unless it declares its own `layout`, which uses the same row and column grammar and resolves declared role names before falling through to roleless cells. Team layout strings keep roles inside the team's declared list and do not accept the inline `:role` suffix. `<team>.<role>` launches one declared role with its team identity, placed like any single-agent launch.

Stacks are presentation only. Zellij renders a native stack with one expanded pane; tmux has no native stack, so the same cells become tiled rows.

### From spec to panes

The compile path is the seam the whole harness hangs off, and it runs in a fixed order.

1. **Resolve.** `plan::resolve_launch` reads the effective config (machine profiles and teams, merged with trust-filtered project config) and produces a `LayoutSpec` whose agent cells carry their profile-declared launch params.
2. **Validate.** The profile prompt-file validator runs next, so a moved `--system-prompt-file` fails at the entry point rather than inside a half-built tab.
3. **Finalize.** `plan::finalize_launch_layout` applies the launch-wide choices: permission posture, CLI presets and passthrough argv, budget, adapter-declared preset reconciliation and defaults, and supervised turn limits. An args-only model is adopted as identity; an adapter default is stamped only when no model was selected at all.
4. **Identify.** Each cell becomes a launch request with a name, a channel, and its cohort stamps, and the store mints provisional rows before any pane opens.
5. **Compile.** Each cell becomes a `LayoutPanes` entry. An agent cell compiles to the exec-wrapper argv, a command cell to its raw argv, and an empty argv reserves the pane for the user's shell.

Restart and resume stop after step 1 and replay only profile-declared settings, through the shared posture seam under [Resume](#resume-and-rebirth).

A trailing launch prompt attaches to exactly one agent identity: a named team's configured `leader` role, its first declared role by default, or otherwise the first unambiguous agent cell. Team and multi-cell launches stamp each member's cohort and order (`launch_group` and `launch_ordinal`, exported as `RIMZ_LAUNCH_GROUP` and `RIMZ_LAUNCH_ORDINAL`), so the sidebar keeps cards in definition order and resume can match a cohort later.

`launch::compile_agent_process` is the provider-process seam. It selects launch, resume, or fork argv from one typed request, composes trusted project, adapter, RimZ identity, and RTK environment in that order, applies the login-shell wrapper, and retains the raw provider argv for PATH preflight.

### The exec wrapper

Every agent pane runs the hidden `rimz agents exec <kind>` wrapper rather than the agent directly.

The command line carries two visible arguments, the kind and an optional `--worktree-path`, which together form the process-classification envelope that pane discovery reads. Everything else travels in one hidden compact JSON `ExecRequest`: the typed action (`Launch`, `Resume`, or `Fork`), the identity, the prompt, the run id, pane-lifetime flags, provider account binding state, and provider argv. The wrapper decodes and validates that request, cross-checking the payload kind and worktree against the visible envelope, before it launches anything. Backends never resolve agent kinds or worktrees; the wrapper does.

The wrapper then runs the agent in the pane, inheriting the pane's TTY. It launches through the user's shell-startup path when that shell and `/usr/bin/env` are available and falls back to direct exec otherwise, and it exports `RIMZ_RTK` from `[harness] rtk` so `cargo xtask` can route recognized cargo subcommands through `rtk`.

Whether the wrapper stays resident behind the agent is one predicate, `should_exec_agent_directly`. A plain in-place launch has no run to complete, no pane to close, and no worktree to reclaim, so the wrapper direct-execs the agent and disappears. Anything with post-exit work left (a supervised run, a close-on-exit pane, a worktree launch) keeps the wrapper alive as the parent, which makes it the attach point for [supervised runs](./scripting.md) and for [Reclaiming a pane](#reclaiming-a-pane).

Room birth also carries one generic adapter-enrichment environment map through the mux seam, so a stock agent typed directly into an ordinary work shell inherits the room baseline. RimZ-managed launches still apply their adapter `launch_env` last. Existing processes and shells cannot be upgraded retroactively; rebirth is the parity boundary on both backends.

### Placement

Each backend renders the same compiled layout into a tab, and a single non-worktree cell can instead run in the pane the user is already sitting in. Both backends receive the same `TabOptions` (session, title, cwd, focus flag, sidebar options, and the pre-built pane argv) and dock the global sidebar once before adding the layout cells; the per-backend split commands live in [`mux/`](../../../crates/rimz/src/mux/AGENTS.md).

**Placement resolves before the launch touches the store or creates a worktree**, so a rejected placement leaves no provisional rows or orphan worktree behind.

`plan::resolve_placement` takes the explicit flags first, then falls back to the per-machine [`[agents] placement`](../../guide/configuration.md#agent-profiles-commands-and-teams) policy.

| Situation | Where the layout lands |
| --- | --- |
| `--new-tab`, or no ambient pane to split | a new tab |
| `--new-pane` | a split of the current tab; an explicit flag that cannot be honored fails fast |
| policy `auto` (the default), single non-worktree cell, inside a room | the current pane: the CLI execs the wrapper argv in place, and the pane returns to its shell on exit |
| policy `pane`, single non-worktree cell, inside a room | a split of the current tab |
| policy `tab` | always a new tab |
| any policy, with a named channel, a multi-cell layout, or a worktree | a new tab |
| `--bg`, or create-on-miss | never in-place: the caller's pane stays available, so an in-place choice downgrades to a split |

An in-place launch resolves liveness from the pane rather than from an end trace, because no wrapper stays resident to write one.

Tab titles follow the address vocabulary: a named-channel or worktree launch names its tab `#<NAME>`, a named team launch names it `team:<name>` and stamps its in-place lane as `<dir>/<team>`, and any other non-worktree launch names it `<kind>:<dir>`. Mux tab names stay display-only. They are mutable and live outside the store, so they never form an address.

### Cohort relaunch reconciliation

Relaunching a team into a worktree that already held one is the case where a naive launch silently duplicates work. Reconciliation runs after the live-room preflight and before worktree resolution, whenever the command names a team or an inline layout with at least two agent cells *and* supplies an explicit `-w NAME`.

It derives the named worktree path without creating it, reads the audit rollup for matching root members in that path, and picks one of four outcomes.

| History in that worktree | Outcome |
| --- | --- |
| none | continue into the ordinary launch path |
| live members | focus the newest bound member and exit |
| closed, with dirty or unproven work | offer a worktree-scoped resume |
| closed, clean and content-landed | offer to remove the worktree, then continue into a fresh launch |

Named-team reconciliation considers every member of that team in the target worktree, including sibling roles when a single-role spec relaunches. Inline membership matches by launch group, then ordinal, then kind and role, with a final kind-only fallback for legacy role-less records.

## The address

Every member has an address you type like an @-mention: `@<handle>#<channel>`. The handle names who, the channel names where, and both read from context. [cli/agents.md → Addressing agents](../../reference/cli/agents.md#addressing-agents) is the handle catalog for users; this section is how an address resolves.

### Resolving the channel

The channel is the workspace segment the room already groups by, resolved in order: an explicit named channel, else a worktree name, else an in-place team stamped at launch as `<dir>/<team>`, else a directory basename fallback for unstamped agents ([messaging.md § Channels](./messaging.md#channels)). It matches by exact stamped lane, path basename, or full path.

The default is the channel the command runs in, and an inline `#<name>`, `--channel`, or `--worktree` overrides it. One rule is worth internalizing: **a bare directory workspace has no current channel, and no current channel means every channel**. It never silently narrows to "only worktree-less agents", so addressing the room from a plain directory still reaches the whole room. RimZ-launched panes carry `RIMZ_CHANNEL`, while `RIMZ_TEAM` stays cohort identity for team members.

Launch specs resolve against the same lane. A bare role qualifies to `<team>.<role>` when the lane's agents carry that team, so `rimz agents reviewer` in `#forge` launches the forge reviewer and stamps it into the lane it resolved from. The stamped `team` on those agents is the source of that inference, because the three lane shapes mean the channel string alone does not name a team. A bare role that also names a cell resolving to a different agent refuses rather than guessing. Branch names stay display metadata.

### Handle classes

A handle falls into three classes, narrowing from group to instance.

| Class | Examples | Matches | Can create? |
| --- | --- | --- | --- |
| Role | `@coder` | every agent launched under that team role in the channel | no |
| Type | `@codex` (kind), `@planner` (profile) | every agent of that kind or profile in the channel | yes |
| Instance | `@writer` (explicit `--name`), `@swift-otter` (petname), `@claude-2` (kind ordinal), a session-id prefix, `tmux:%1` (pane address) | exactly one running agent | no |

`@all` is the broadcast handle for the whole channel. Role names reserve built-in kind handles so kind addresses keep round-tripping, and a profile name that would read as `@all`, a kind ordinal, or a pane address is rejected at config load.

Only a type handle creates, because only a kind or profile carries what a launch needs. An instance handle names something that must already exist, and refuses with the fix.

### Arity decides the outcome

An address resolves against a fresh snapshot to zero, one, or many agents.

| Matches | Outcome |
| --- | --- |
| one | delivered |
| many | an ambiguity error listing the handles to pick one, unless `--all` or `@all` opts into fan-out. Fan-out delivers to every match, prefixes each delivery with the addressed handle (`@all,`, `@claude,`) so receivers read it as a group message, and skips a blocked agent while the rest send. |
| zero | a miss that names where the agent runs in another channel and lists live agents, or, with `--create`, launches it |

`--create` launches a missing agent straight from its address. `rimz message --steer @planner#design --create "draft the API"` opens a `planner` in `#design`, registering the named channel, with the text as its first prompt. With `--worktree feat/x` it creates or reuses that worktree instead.

Resolution has two sources and one matcher set over both: rollup sessions (`&AgentState`, used by management commands and parked message records) and the live agent panes the producer bound (`&PaneAgent`, used by `--steer` and send-now messages). Each command chooses its source. Pane binding then joins a match to exact lifecycle state, a same-channel provisional launch card, or a sessionless lazy target, so an agent is addressable before its first turn registers.

### Petnames and the canonical handle

The petname is the harness's stable per-instance fallback name. The store mints an adjective-noun pair at registration, collision-checked against the room's live names, refusing reserved command words and kind-shaped names, so a petname can never shadow `@all` or `@claude-2`. A session recorded before petnames existed re-derives one deterministically from its session id, so old logs still render a stable name.

The rendered handle is the shortest address that names exactly that agent, and it round-trips through the parser. RimZ renders it role first: the role when unique in scope, then the explicit `--name`, then the profile when unique, else the kind, else `@<kind>-<n>`, else the petname. A listing therefore always shows a handle you could type back, and a handle appears only when typing it reaches that one agent. One canonical renderer, the exact inverse of the parser, is shared by every agent-bearing listing; `target.rs` owns both halves and they are tested against each other.

## Resume and rebirth

Recovery is the other half of launching. The store remembers agents whose processes are gone, and resume turns those records back into panes.

### Three entry points

| Path | Trigger | Scope |
| --- | --- | --- |
| Room rebirth | a machine reboot or mux crash, at the next `rimz start` | the producer's persisted live roster, intersected with the audit rollup, seeding one tab per live-at-death lane or worktree before the new mux session starts |
| Cohort resume | `rimz agents <spec> --resume` (`--continue` is the visible alias) in a live room | one prior cohort matched from the spec, after its tab or pane was closed |
| Lane resume | `rimz agents resume <scope>` | one lane, resolved by `harness::resume` |

Rebirth restores a named team in its declared layout, resuming members that can resume and fresh-launching missing or unsupported agent cells so the shape stays whole; non-team lanes restore as one column. Cohort resume scopes to one exact worktree with `-w <NAME>` or the caller's current worktree, while a project-root resume keeps newest-by-spec behaviour.

Lane resume picks one of four actions, and `LaneResumeAction` names them: `List` when no scope was given, `Focus` on the freshest pane when every member is live, `SplitClosed` to plan flat resume commands beside a surviving live member, and `RestoreClosed` to reuse the rebirth team and flat split when every member is closed. In all cases the CLI preflights the planned provider kinds, through `LaneResumeAction::agent_kinds_needing_preflight`, before `LaneRestorePlan::materialize` allocates fresh team identities or any mux action runs, so a missing provider binary fails before it half-rebuilds a room.

### What matches what

- A **named team spec** matches prior root agents with the same `team`, then maps role cells by role, taking the newest member per role.
- An **inline multi-agent spec** matches the newest `launch_group` that maps onto its agent cells by `launch_ordinal`, falling back to kind when old records lack ordinals.
- A **single-agent spec** ignores cohort membership and resumes the newest dead or unknown root session of that kind.

Missing cells launch fresh in the matched cohort's cwd and channel, so the layout stays whole. Cleanly ended members stay candidates, so a closed team resumes while its worktree still exists. Subagents, empty session ids, and missing worktrees are never candidates. A matched member whose process is still live refuses the whole resume and names it, because launching beside it would duplicate the addressable role or kind. A kind whose adapter has no native resume argv launches fresh and is reported as such.

Flat resume keeps pane identity when a stamp survives: newest-first candidates sharing one pane collapse to the newest session. A rebirth boundary retires pane stamps, because pane ids renumber across a mux restart; an unstamped root stays a candidate and deduplicates by `(kind, session id)`. Subagents are excluded by their parent identity rather than by their lack of a pane.

`resume.max` bounds how many agents one reborn session auto-resumes (`DEFAULT_RESUME_MAX`, 128), so a long-lived workspace cannot fork-bomb the machine on birth. Anything past the cap is reported as a skip, never silently dropped, and every `ResumeSkip` carries its reason (`no resume CLI`, `no saved conversation`, or `over the resume cap`) into the start report.

### Discovering sessions the store never saw

When an explicit lane has no durable candidates, or every closed durable candidate has lost its provider conversation, lane planning asks its caller for adapter local-session observations, which come from the provider's own session files.

Each observation spans `[created_at, last_activity]`. Transitive interval overlap forms concurrent clusters, and the cluster containing the globally newest activity is the last concurrent working set. That cluster resumes newest first up to `resume.max`; every older cluster is reported and stays closed. Synthesized flat records carry only kind, exact session id, workspace, transcript, activity, and lane, because provider files cannot reconstruct RimZ-only roles or teams.

### Posture

A resumed pane runs `rimz agents exec <kind>` with a `Resume` action carrying the provider session id and the prior RimZ identity (name, profile, role, team, launch group, launch ordinal, channel). Its **posture** comes from `resume::resolve_posture`, the one seam every relaunch path shares: the argv the agent's profile renders, meaning model, effort, system-prompt files, permission mode, budget, and profile `args`. A session that launched as `@planner` comes back as a planner.

Team restore and cohort resume read posture straight off the layout cell their team or role binding already resolved. Flat and lane resume resolve the stored profile name against the effective config. The launch event's recorded permission mode fills in when the profile declares none.

Degradation is deliberate and asymmetric. A profile that is gone, broken, or now names a different provider degrades to a bare resume with a warning, because rebirth runs unattended and a recovery must never refuse. Interactive `restart` escalates the same provider switch to the user instead, since changing providers under a running agent is a decision, not a fallback.

Resume leaves one-off launch values out: the prompt, an explicit `--model` or `--effort` typed at the original launch, and passthrough argv were a single invocation's choice, not durable configuration. `--resume` and `--continue` conflict with those launch-shaping flags for the same reason, and take cwd and channel from the matched store cohort. `--worktree` in this mode is a resume scope, not a worktree-creation flag.

## Reclaiming a pane

When an agent exits, the resident [exec wrapper](#the-exec-wrapper) either leaves the pane usable or reclaims what automation owns. The decision turns on one question: was this exit *deliberate*?

```text
agent process exits
  │
  ├── clean child exit ─────────────────────────► deliberate
  └── abrupt (tab/pane close, signal)
        └── does the mux session still accept live pane closes?
              ├── yes (room alive, even mid-teardown) ─► deliberate
              └── no  (reboot, mux crash, wedged server, last tab closed)
                     └────────────────────────────────► not deliberate
```

A deliberate exit records the durable `rimz.agent-ended` trace before any slower cleanup, so that agent stays out of future recovery. A non-deliberate exit skips worktree cleanup and writes no trace, because recovery state should come from the sidebar producer's latest live roster instead. The liveness probe is stronger than a bare session listing, so a wedged-but-listed Zellij server still counts as abrupt, while a live room with missing sidebar chrome still treats a pane close as deliberate.

After the trace, the wrapper settles in three ways depending on what the launch asked for.

**Drop to a shell.** A clean interactive exit from a close-pane or worktree pane prints one hint and execs the user's shell in that pane, so the pane stays usable and any worktree stays inspectable. The hint is a runnable command rebuilt from the stored identity (`rimz agents forge.reviewer`). It teaches `--resume` when the ended session can actually be redeemed, meaning a real provider session id whose adapter compiles a resume command for this directory, and a bare relaunch otherwise. Running the resume takes that same pane back over rather than opening a lane tab, which closes the loop from the hint to the recovered session.

**Reclaim the worktree.** An agent launched with `--worktree-path` triggers worktree cleanup on supervised-run completion or on a deliberate signal or tab-close exit, which proves the branch's work landed before removing the tree and deleting its branch. Clean interactive quits deliberately do *not* reclaim: they drop to the idle shell and leave reclamation to `rimz gc`. Signal exits start the cleanup helper with null stdio in its own process group, so it can finish after the closing pane disappears. The helper, its decision table, and the `gc` sweep are [worktrees.md § Who triggers removal](./worktrees.md#who-triggers-removal).

**Close the pane.** A run pane closes itself when the launch set `close_pane_on_exit`. The supervised-run side of pane reclamation, including background runs and cancellation, is [scripting.md § Reclaiming the run pane](./scripting.md#reclaiming-the-run-pane).

## See also

- [scripting.md](./scripting.md): supervised `-p` runs: the run record, the wake socket, verify and retry, output formats.
- [loops.md](./loops.md): scheduled tasks: the task catalog, elder firing, the fire gate ladder, and the recovery automation the elder runs.
- [budget.md](./budget.md): dollar caps: the scopes, the ledgers, the verdict, the waiver, and the gate.
- [messaging.md](./messaging.md): how text actually reaches a pane.
- [worktrees.md](./worktrees.md): the Git worktrees a launch can land in.
- [trust.md](./trust.md): which parts of a launch spec can execute a command, and how a grant is proven.
- [model.md](../agents/model.md): the agent rollup and state machine the harness reads.

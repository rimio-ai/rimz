# The agent harness

> The entry point for contributors working on the harness. This page maps the whole area, then owns the launch core: spawning a fleet, addressing it, resuming it, and reclaiming what it leaves behind. The other nine pages in this folder each go deep on one job: supervised runs, subagents, loops, budgets, messaging, the transcript, worktrees, teams, and trust.

## What the harness does

The harness starts, names, drives, and cleans up agents. RimZ has no API into the agents it runs: every agent is a stock provider CLI in a real terminal pane. So the harness works the way a fast human would. It opens panes, types into them, watches the durable records the agents' hooks write, and closes panes when the work is done. The same machinery serves a human at the keyboard, a shell script, a CI gate, and one agent driving another.

## The rules that shape it

Four rules explain most of the design. When a piece of the code surprises you, one of these is usually the reason.

**Durable records are the truth.** Panes are latency: they can close, renumber, or wedge. Launch identity, message queues, run outcomes, and schedule history live in the store, so recovery reads state instead of guessing from a multiplexer. Every subsystem here has one durable record at its center, and everything else is an attempt against it.

**One compile target.** Every way of starting agents (an inline layout, a named team, a `-p` run, a resumed cohort, a reborn room) resolves to the same backend-neutral list of pane commands. Zellij and tmux receive identical input, so a feature written once works on both.

**One name for one agent.** A member is reachable by an address, `@handle#channel`, and the renderer that prints a handle is the exact inverse of the parser that reads one. Anything RimZ shows you, you can type back.

**No room-wide service.** Scheduled work, message wakeups, and unattended recovery ride the tick of the room's elected sidebar producer, the elder. Roots without an open room can opt into a one-shot OS timer tick, described in [loops.md § The external tick](./loops.md#the-external-tick). An optional [language-server broker](../lsp.md) is a per-server process bounded by agent leases, not a room scheduler.

## The vocabulary

Spawning separates three independent choices, so any combination is one command: **agents** choose which tools run, **layout** chooses the shape on screen, and **channel** chooses the cooperation lane.

A **channel** is one cooperation lane. It is backed by a durable bare name, a [worktree](./worktrees.md), an in-place named team stamped as `<dir>/<team>`, or the directory room itself. The sidebar groups by it, and an address narrows to it with `#<channel>`.

A **member** is an agent inside a channel, named by a **handle**: `@claude` the kind, `@planner` the profile, `@coder` the team role, `@writer` an explicit launch name, `@swift-otter` the minted petname.

An **address** joins the two as `@handle#channel`. Every command that reaches an agent names it this way.

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

## One launch, end to end

`rimz agents claude,codex -w feat-a "start on the parser"` touches most of the launch core. Each step links to the section that owns it.

1. **Resolve the spec.** `claude,codex` parses into a `LayoutSpec` of two agent cells, with profiles and teams resolved from the effective config ([The layout IR](#the-layout-ir)).
2. **Finalize the launch.** Isolation, permission posture, presets, passthrough argv, and budget fold into each cell ([From spec to panes](#from-spec-to-panes)).
3. **Choose where it lands.** Two cells plus a worktree means a new tab. Placement is decided before anything durable exists, so a rejected placement leaves no debris ([Placement](#placement)).
4. **Reconcile a prior cohort.** A multi-cell launch with an explicit `-w` checks whether `feat-a` already held these agents, and may focus them, offer a resume, launch fresh, or offer to clear the worktree ([Cohort relaunch reconciliation](#cohort-relaunch-reconciliation)).
5. **Create the worktree.** A marked Git worktree is added and seeded, and its name `feat-a` becomes the channel every cell is stamped with ([worktrees.md](./worktrees.md)).
6. **Mint identities.** The store writes a provisional row per cell with its name, channel, and cohort stamps, so both agents are addressable as `@claude#feat-a` and `@codex#feat-a` before either has run a turn ([The address](#the-address)).
7. **Open the panes.** Each cell compiles to a hidden `rimz agents exec` wrapper invocation carrying an `ExecRequest`. The wrapper materializes the prompts, then runs the stock provider CLI with the trailing prompt in its launch argv ([The exec wrapper](#the-exec-wrapper)).
8. **The agents report themselves.** Their lifecycle hooks write durable events, which the rollup folds into the state every later command reads: messages waiting for a turn boundary ([messaging.md](./messaging.md)), supervised runs waiting to complete ([scripting.md](./scripting.md)), and budget ticks watching the spend ([budget.md](./budget.md)).
9. **Reclaim what is left.** When the agent exits, the resident wrapper decides whether the exit was deliberate, and only then removes the worktree or closes the pane ([Reclaiming a pane](#reclaiming-a-pane)).

Placement and reconciliation belong to the interactive launch command. Every other entry point (a `-p` run, a scheduled loop fire, a resumed cohort, a reborn room) shares resolution, identity, compilation, and the exec wrapper, with a different request on the front.

## Where the code lives

The harness is a product area spanning several source modules, and the ten pages in this folder are grouped by the job they document.

| Page | Owns | Source |
| --- | --- | --- |
| fleet.md (this page) | Spawn, address, resume, reclaim | [`harness/`](../../../crates/rimz/src/harness), [`address.rs`](../../../crates/rimz/src/address.rs) |
| [scripting.md](./scripting.md) | Supervised `-p` runs | [`store/run.rs`](../../../crates/rimz/src/store/run.rs), [`harness/run.rs`](../../../crates/rimz/src/harness/run.rs), [`run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs), [`cli/supervised/`](../../../crates/rimz/src/cli/supervised) |
| [subagents.md](./subagents.md) | Agents launching supervised children | [`cli/subagents/`](../../../crates/rimz/src/cli/subagents), [`harness/plan.rs`](../../../crates/rimz/src/harness/plan.rs) |
| [loops.md](./loops.md) | Scheduled tasks, signals, waits, and the assist log | [`harness/schedule/`](../../../crates/rimz/src/harness/schedule), [`assist_log.rs`](../../../crates/rimz/src/harness/assist_log.rs) |
| [budget.md](./budget.md) | Dollar caps and the park they produce | [`harness/budget.rs`](../../../crates/rimz/src/harness/budget.rs), [`cli/budget.rs`](../../../crates/rimz/src/cli/budget.rs) |
| [messaging.md](./messaging.md) | Getting text into a running agent | [`message/`](../../../crates/rimz/src/message) |
| [transcript.md](./transcript.md) | The durable conversation log and asks | [`transcript.rs`](../../../crates/rimz/src/transcript.rs) |
| [worktrees.md](./worktrees.md) | RimZ-owned Git worktrees | [`worktree.rs`](../../../crates/rimz/src/worktree.rs) |
| [teams.md](./teams.md) | Team scratch files, the stage board, and stage flips | [`harness/scratch.rs`](../../../crates/rimz/src/harness/scratch.rs), [`harness/team_stage.rs`](../../../crates/rimz/src/harness/team_stage.rs) |
| [trust.md](./trust.md) | Which parts of a project config may execute | [`trust.rs`](../../../crates/rimz/src/trust.rs) |

Every file below is under `harness/` except the top-level `address.rs`, the petname grammar in `agents/`, and the durable run record in `store/`.

| File | Owns |
| --- | --- |
| [`spec.rs`](../../../crates/rimz/src/harness/spec.rs) | The layout IR: the inline grammar, team and profile resolution, virtual `<kind>-<mode>` cells, prompt-file path rooting, the prompt leader, and the name-collision rules that keep profile names addressable. |
| [`plan.rs`](../../../crates/rimz/src/harness/plan.rs) | Turning a spec into a launch: effective-config resolution, launch finalization, per-cell launch identities, and fresh or resumed compilation to backend-neutral pane commands. |
| [`launch_plan.rs`](../../../crates/rimz/src/harness/launch_plan.rs) | The exec wrapper's plan: `prepare_exec` makes its launch decisions through `compile`, which decides without writing; `apply` materializes. |
| [`launch.rs`](../../../crates/rimz/src/harness/launch.rs) | The provider process: adapter argv for launch, resume, and fork; the hidden `ExecRequest` wire; launch environment composition; the login-shell wrapper; and preflight. |
| [`prompt_compose.rs`](../../../crates/rimz/src/harness/prompt_compose.rs) | Ordered system-prompt composition, content-addressed runtime artifacts, and adapter replacement argv or environment. |
| [`launch_reminders.rs`](../../../crates/rimz/src/harness/launch_reminders.rs) | The one `<system_reminder>` tag: paragraph order and the model fragment. |
| [`launch_context.rs`](../../../crates/rimz/src/harness/launch_context.rs) | A team member's identity sentence, launch-time run state, and channel rule. |
| [`scratch.rs`](../../../crates/rimz/src/harness/scratch.rs) | Team memory-file scan and the advisory `blackboard.md` stage parser, shared by launch reminders and cohort reports. See [teams.md](./teams.md). |
| [`team_stage.rs`](../../../crates/rimz/src/harness/team_stage.rs) | `rimz teams flip`: locked board edits, the durable `team.stage` signal, owner delivery, and hand-off compaction. See [teams.md](./teams.md). |
| [`ancestry.rs`](../../../crates/rimz/src/harness/ancestry.rs) | Durable caller resolution and launch-chain policy. |
| [`subagent_policy.rs`](../../../crates/rimz/src/harness/subagent_policy.rs), [`parent_watch.rs`](../../../crates/rimz/src/harness/parent_watch.rs), [`orphan_sweep.rs`](../../../crates/rimz/src/harness/orphan_sweep.rs) | What a subagent caller may launch, the child's parent watchdog, and the elder's backstop when that watchdog fails. See [subagents.md](./subagents.md). |
| [`address.rs`](../../../crates/rimz/src/address.rs) | The address: parsing `@handle#channel`, resolving it against a snapshot, binding a match to a live pane, rendering the canonical handle back, and launch-instance grouping and lineage. |
| [`agents/petname.rs`](../../../crates/rimz/src/agents/petname.rs) | Adjective-noun instance names, name legality, and the deterministic fallback for a record with no stored name. |
| [`resume.rs`](../../../crates/rimz/src/harness/resume.rs) | Resume planning for room rebirth, cohort resume, and lane resume, plus `resolve_posture`, the relaunch posture seam every path shares. |
| [`rebirth.rs`](../../../crates/rimz/src/harness/rebirth.rs) | Two-phase inspection of the previous incarnation of a room, over the shared recovery plan. |
| [`budget.rs`](../../../crates/rimz/src/harness/budget.rs) | Dollar caps and their parks. See [budget.md](./budget.md). |
| [`store/run.rs`](../../../crates/rimz/src/store/run.rs), [`run.rs`](../../../crates/rimz/src/harness/run.rs), [`run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs), [`run_timeout.rs`](../../../crates/rimz/src/harness/run_timeout.rs) | The durable supervised-run record, its transitions, the waiter that receives its wake, and deadline detection. See [scripting.md](./scripting.md). |
| [`schedule.rs`](../../../crates/rimz/src/harness/schedule.rs), [`schedule/`](../../../crates/rimz/src/harness/schedule) | Loop tasks and their runner. See [loops.md](./loops.md). |
| [`auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs), [`auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs) | Unattended recovery of parked turns and spent windows. See [providers.md § Auto-continue](../agents/providers.md#auto-continue) and [§ Auto-redeem](../agents/providers.md#auto-redeem). |
| [`assist_log.rs`](../../../crates/rimz/src/harness/assist_log.rs) | The audit trail every unattended intervention appends to. See [loops.md § The assist log](./loops.md#the-assist-log). |
| [`idle_compact.rs`](../../../crates/rimz/src/harness/idle_compact.rs) | The elder's idle compaction check. See [messaging.md § Idle compaction](./messaging.md#idle-compaction). |
| [`idle_stop.rs`](../../../crates/rimz/src/harness/idle_stop.rs) | The elder's check of pending `stop --when-idle` requests and the decision its helper re-runs. See [loops.md § Recovery the elder runs](./loops.md#recovery-the-elder-runs). |
| [`auto_gc.rs`](../../../crates/rimz/src/harness/auto_gc.rs) | The elder's daily gc check and its sweep stamp. See [loops.md § Recovery the elder runs](./loops.md#recovery-the-elder-runs). |

The CLI side lives in [`cli/agents_cmd/`](../../../crates/rimz/src/cli/agents_cmd) (launch placement, reconciliation, restart, resume, fork, stop, and the hidden `exec` wrapper), [`cli/supervised/`](../../../crates/rimz/src/cli/supervised) (the run driver both `agents -p` and loop fires call), and [`cli/loop_cmd/`](../../../crates/rimz/src/cli/loop_cmd). Those handlers parse flags, execute effects, and render; the harness keeps provider and durable-state rules.

Read this page first, then the page for whatever you are changing. For a second read across the whole area, [messaging.md](./messaging.md) is the most instructive: the durable-record rule has the most consequences there, and the delivery pipeline touches nearly everything else.

### The state machines

Six state machines carry most of the area's behaviour. Each has one owning type and one owning section.

| Machine | Type | Documented in |
| --- | --- | --- |
| Message lifecycle | `MessageStatus` | [messaging.md § Status lifecycle](./messaging.md#status-lifecycle) |
| Supervised run | `RunStatus`, whose `exit_code` is the caller contract | [scripting.md § Status and exit codes](./scripting.md#status-and-exit-codes) |
| Loop task timing and outcome | `TaskTiming`, `LoopRunResult` | [loops.md § Schedule shapes](./loops.md#schedule-shapes), [§ History, strikes, and arming](./loops.md#history-strikes-and-arming) |
| Dollar budget ledger | `BudgetVerdict` | [budget.md § The verdict](./budget.md#the-verdict) |
| Worktree removal | `ProtectionSet::assess` | [worktrees.md § The assessment](./worktrees.md#the-assessment) |
| Project trust | `TrustState` | [trust.md § States](./trust.md#states) |

Two decision trees behave like state machines without an enum: the [deliberate-exit classification](#reclaiming-a-pane) that decides what happens when an agent process ends, and the [park class](../agents/providers.md#auto-continue) that decides when a stopped agent may resume itself.

## Launching a fleet

### The layout IR

`rimz agents <spec>` resolves either a named `[agents.teams]` entry or an inline spec, and both compile to the same backend-neutral panes. In the inline grammar, commas split columns, plus signs tile rows within a column, and slashes stack rows within a column on Zellij.

```text
claude,codex+term          → Claude left; Codex tiled over a shell right
claude/codex/term          → two agents plus a shell in one Zellij stack; tmux tiles them
vim,htop+zsh               → raw command panes
claude-auto,codex-yolo     → agent cells with adapter-owned permission posture
claude:planner,codex:coder → agent cells with ad-hoc `@planner` and `@coder` handles
```

Each cell is one of: the built-in `term`, a registered agent kind, a virtual `<kind>-<mode>` variant, a configured profile, a configured command, or an executable found on PATH ([configuration.md](../../guide/configuration.md#agent-profiles-commands-and-teams)). Configured cells and built-ins resolve before PATH. An agent cell may carry an ad-hoc role as `<cell>:<role>`; inline roles follow team-role naming and addressing rules, must be unique within the spec, and apply only to agent cells.

A named team is an ordered role list. It opens as one column per role unless it declares its own `layout`, which uses the same row and column grammar and resolves declared role names before roleless cells. A team layout may name only the team's declared roles and takes no `:role` suffix. `<team>.<role>` launches one declared role with its team identity, placed like any single-agent launch.

Stacks are presentation only. Zellij renders a native stack with one expanded pane; tmux has no native stack, so the same cells become tiled rows.

### From spec to panes

The compile path is the seam the whole harness hangs off, and it runs in a fixed order.

1. **Resolve.** `plan::resolve_launch` reads the effective config (machine profiles and teams, merged with trust-filtered project config) and produces a `LayoutSpec` whose agent cells carry their profile-declared launch params and typed system-prompt file paths. A launch-wide `--agent` override resolves as a replacement base; `spec::rebase_onto` merges it without changing the cell's profile or role identity (table below).
2. **Finalize.** `plan::finalize_launch_layout` applies the launch-wide choices to each agent cell in this order: isolation, permission posture, the preset (typed prompt-file override, preset argv, CLI model and effort), validation, passthrough argv, budget, then preset reconciliation and the adapter default model. Supervised turn limits apply in a second pass over all cells. Validation checks profile prompt files and the adapter's replacement support, so a moved prompt file or an adapter with no replacement mechanism fails at the entry point instead of inside a half-built tab; `compile_layout_panes` validates again. Profile-declared model, effort, and `auto-compact` render through typed presets and reconcile against matching raw `args`; `auto-compact` has no CLI override. An args-only model is adopted as identity, and an adapter default is stamped only when no model was selected at all. An explicit `--ask`/`--yolo` replaces a declared profile or virtual-cell mode: `LaunchSpec::strip_permission_args` removes every exact rendered permission vector of the adapter (longest first), then the chosen vector is appended and the mode stamped. A caller default (supervised `-p` without a flag, `auto`) only fills an unset mode.
3. **Resolve ancestry.** The launcher reads its stable launch identity and resolves the live durable caller. A caller that is itself a launched child is refused for every launch kind. A launch from a human shell carries no ancestry stamp. An agent caller's agents or teams launch gets a parentless peer-generation stamp and is checked against `[agents] max-chain-length`; its subagents launch gets a direct-parent stamp. A stale caller, an excessive chain, or a child caller refuses here, before provider preflight, worktree creation, store append, or mux action. A subagents launch then re-anchors its workspace at the caller's recorded checkout, and a vanished checkout refuses at the same point ([subagents.md](./subagents.md#the-child-works-where-its-parent-works)).
4. **Identify.** Each cell becomes a launch request with a name, a channel, its cohort stamps, and its optional ancestry stamp, and the store mints provisional rows before any pane opens.
5. **Compile.** Each cell becomes a `LayoutPanes` entry. An agent cell compiles to the exec-wrapper argv, a command cell to its raw argv, and an empty argv reserves the pane for the user's shell.

Restart, fork, and flat resume stop after step 1 and resolve posture through the seam under [Posture](#posture). Cohort resume matches the layout without `--agent` or `--tier`, then restores each member's record and applies overrides before preflight and pane compilation.

`rebase_onto` takes the engine from the replacement base and the role from the original cell:

| Field | Result |
| --- | --- |
| kind | the base's |
| model | the base's when it sets one; otherwise the original's on the same kind, and unset on a different kind |
| effort | the base's when it sets one, otherwise the original's |
| raw `args` | the original's, with the base filling a gap, on the same kind; the base's on a different kind |
| mode, budget, `auto-compact`, skills, system-prompt file | the original's, with the base filling a gap |
| appended system-prompt files | the base's fragments, then the original's |

A different kind takes model and args only from the base because provider-specific values do not carry across providers, and the replacement was explicit.

A trailing launch prompt attaches to exactly one agent identity: a named team's configured `leader` role, its first declared role by default, or otherwise the first unambiguous agent cell. Team and multi-cell launches stamp each member's cohort and order (`launch_group` and `launch_ordinal`, exported as `RIMZ_LAUNCH_GROUP` and `RIMZ_LAUNCH_ORDINAL`), so the sidebar keeps cards in definition order and resume can match a cohort later. One private key table in `harness/launch.rs` maps each env-backed launch parameter to its `RIMZ_*` key: `exec_identity_env` encodes the pane env through it, and the lifecycle hook decodes a root session's unset parameters back through `fill_launch_identity_env`.

`launch::compile_agent_process` is the provider-process compiler. It selects launch, resume, or fork argv from the request, composes trusted project, adapter, and RimZ identity environment in that order, applies the login-shell wrapper, and retains the raw provider argv for PATH preflight. The exec wrapper supplies any materialized prompt argv and environment at the final provider boundary.

### Every path that builds a launch layout

A rule that must hold for every launch (a durable record, a preflight, a stamp) has to hold on each of these paths. Only the first resolves from config at the entry point; the others rebuild from profiles or seeds already in memory, so they are the ones a new rule misses. The last column is where each path writes its `tier_fallback` assist ([loops.md § The assist log](./loops.md#the-assist-log)).

| Path | Builds its layout in | Records a tier fallback |
| --- | --- | --- |
| Fresh launch, `rimz agents <spec>` | `crates/rimz/src/cli/agents_cmd/launch_resolve.rs::resolve_finalized_layout` (a named team through `crates/rimz/src/harness/spec.rs::resolve_team`), then `crates/rimz/src/harness/plan.rs::compile_layout_panes` in `crates/rimz/src/cli/agents_cmd/launch.rs::launch_layout` | from the `on_launched` hook of `crates/rimz/src/cli/agents_cmd/placement.rs::execute`, after the tab or pane opens |
| Same-pane exec, the in-place branch of a fresh or cohort-resume launch | the same compiled panes, handed to `exec_wrapper_in_place` | from the same hook, just before the exec, since a successful exec never returns |
| Cohort resume, `rimz agents <spec> --resume` | `crates/rimz/src/harness/resume.rs::restore_routed_cells` over the finalized layout, then `compile_layout_panes` in `crates/rimz/src/cli/agents_cmd/launch.rs::launch_resume_layout` | from the `on_launched` hook, as for a fresh launch |
| Supervised run, `rimz subagents` and `-p` | one pane, built in `crates/rimz/src/cli/supervised/run.rs::execute_attempt` through `crates/rimz/src/cli/supervised.rs::run_pane_cmd` rather than `compile_layout_panes` | after `open_attempt_pane` returns |
| Team restore, in rebirth and lane resume | `crates/rimz/src/harness/resume.rs::plan_team_restore_tabs` (`resolve_team`, then `restore_routed_cells`), compiled by `crates/rimz/src/harness/resume.rs::materialize_team_restore_tab` | lane resume: `crates/rimz/src/cli/agents_cmd/resume.rs::resume_lane` after each `open_tab` succeeds; failure closes that tab's batch and every unopened tab's batch without assists. Rebirth: `crates/rimz/src/cli/room/mod.rs::birth_managed_room` after birth succeeds, for each team tab the backend confirmed open; a tab that did not open fails its batch in `crates/rimz/src/harness/rebirth.rs::SeededRecovery::confirm` |

The login health check is such a rule, and it also covers the relaunch paths that build no layout. Each site calls `crates/rimz/src/agents/login.rs::ProviderLogin::health` on the login it launches under, `default` included, over `LoginCatalog::native_ambient` so a caller inside a named-account pane judges the home the new pane resolves. The call follows the sandbox and compile preflights and precedes the first store write and mux action, so a home that is not a directory or hooks that are missing or untrusted refuse with the `BirthLoginErr` line and change nothing. The sites are `crates/rimz/src/cli/agents_cmd/launch.rs::preflight_cell` (fresh launch and cohort resume, per cell), `crates/rimz/src/cli/agents_cmd/restart.rs::restart_resolved` (restart, and `rimz teams restart` per member), `crates/rimz/src/cli/agents_cmd/resume.rs::resume_lane` (per planned kind, on the room's login for it), `crates/rimz/src/cli/agents_cmd/fork.rs::run_fork`, and `crates/rimz/src/cli/subagents/resume.rs::resume_resolved`. A supervised run keeps its own stricter gate, `crates/rimz/src/cli/supervised.rs::preflight_agent`. `rimz agents explain` runs none of this, so it still renders a plan for an agent whose hooks are gone. Room birth and account switching call `ProviderLogin::preflight`, which skips `default`. A process plugin declares no config home, so `health` passes it.

### The exec wrapper

Every agent pane runs the hidden `rimz agents exec <kind>` wrapper, which in turn runs the agent. Backends never resolve agent kinds or worktrees; the wrapper does.

#### The request envelope

The wrapper's command line carries two visible arguments, the kind and an optional `--worktree-path`, which together form the process-classification envelope that pane discovery reads. Everything else travels in one hidden compact JSON envelope: the typed action (`Launch`, `Resume`, or `Fork`), the identity, the launch prompt's path, typed system-prompt source paths, skills, the subagent flag, the run id, pane-lifetime flags, provider account binding state, and provider argv.

The launch prompt travels as a file. Every launch prompt is materialized as a content-addressed `prompt/task.<digest>.md` beneath the private runtime root, beside the system-prompt artifacts. The wrapper validates the request and cross-checks the visible kind and worktree before any artifact I/O, then establishes launch and run cleanup context and reads the prompt into the in-memory `ExecRequest`. Before planning, it refuses a request whose profile failed to load on the host, with that definition's `path: message`, failing the provisional launch and the supervised run the same way: a caller in a sandboxed pane skips the skill-listing check, so this is where the host judges its launch. A missing or unreadable artifact fails the provisional launch and any nonterminal supervised run, which wakes its waiter instead of leaving the parent waiting.

Keeping the prompt body out of the pane command avoids tmux's command-frame ceiling of roughly 16 KiB. The provider still receives the full prompt as one argv element, so RimZ enforces a separate 120 KiB (122880-byte) argv limit during launch preflight, before launch and supervised-run records are created, and again in the wrapper. A larger prompt fails with the limit named and advice to move detail into a file the agent reads. The runtime artifacts are rename-atomic caches outside the store: content addressing makes reentry idempotent, and runtime GC removes stale artifacts by age.

#### The launch plan

`launch_plan::prepare_exec` is the wrapper's one entry into these decisions. From the decoded envelope it runs the definition refusal, resolves isolation, preflights the sandbox, materializes the prompt, derives paths and the login, reads the recorded session for a resume or fork, resolves the model, reads the parent's row for a child, and compiles. It writes and prints nothing: the room's rows come through a lazy reader called only for a resume, a fork, or a child, and its `ExecPreparation` returns the model warnings and any alias move even when a later step fails, so the wrapper prints them and appends the alias assist either way. The wrapper keeps `apply`, the chdir, the version floor, pane stamps, and the process.

`launch_plan::compile` makes every decision and writes nothing. It takes the request, paths, effective configuration, the preflighted bubblewrap path, and the ambient environment; it reads prompt sources and skill metadata, builds reminders and provider argv and env, and plans the sandbox view. The resulting `LaunchPlan` holds the request, cwd, login, prompt plan, process stage, reminder channel, optional `SandboxPlan`, and warnings. The rendered reminder lives on `plan.process().reminder`, even when no provider channel can deliver it.

`launch_plan::apply` is the write boundary. It ensures runtime directories and writes the composed system-prompt and reentry prompt artifacts; it creates the launch's temp unit and the room's `shared/` on every launch, and with a sandbox plan applies the planned skill copies. Cwd changes, identity writes, pane operations, and provider execution stay outside both. The task-envelope planner is private to `launch`, and its deferred artifact writer is harness-only; `refactor-target.toml` admits these module dependencies and public surfaces explicitly.

[`rimz agents explain`](../../reference/cli/agents.md#explain-a-launch) renders the same compiled plan and never calls `apply`. Its profile branch shares launch resolution, finalization, and `launch_identity_requests`, but leaves the name and launch id unminted. A `LaunchAncestryError` from the caller's ancestry becomes a warning and the plan compiles without ancestry, since explain launches nothing; the launch path itself keeps refusing. Its `@handle` branch uses restart action selection and current-config `resolve_posture`, reports degradation as warnings, and refuses overrides. Both branches require successful sandbox and skill preflight. The plan it shows is computed from the invoking process's environment and the current config, so it can differ from what the target pane received: hidden provider instructions and original passthrough arguments are absent.

#### System prompt composition

A profile's system prompt is a base file plus ordered fragments. For a staged team role with a resolved base, composition is base → profile-chain fragments → role fragments → consensus → team-level `append-system-prompt-files`. `team_prompt.rs` owns the embedded `team_consensus.md` and the layer's derivation; `spec` attaches it once per role cell at team compile, after rebasing. `consensus-file` replaces the embedded text, and team paths resolve relative to the declaring config file. A CLI `--append-system-prompt-file` override replaces only the profile and role fragments, not the team layer.

A team is staged when `stages` is nonempty or any role has `owns`. Unstaged teams and roles without a base get no default layer. Explicit `consensus-file` or nonempty team `append-system-prompt-files` requires a staged team and a base on every role; preparation refuses otherwise. `prompt_compose` normalizes trailing newlines and joins pieces with one blank line and one trailing newline. With fragments or a team layer, path adapters receive a content-addressed `prompt/sys.<digest>.md` beneath the private runtime root; with neither, they receive the user's resolved base path directly.

| Adapter | Channel |
| --- | --- |
| Claude, Codex | path in argv |
| Qwen | path in `QWEN_SYSTEM_MD` |
| Pi | text in `--system-prompt` |

The wrapper removes matching raw replacement argv before adding the materialized value, so the typed setting wins on fresh launch, restart, fork, resume, and rebirth. The artifact is regenerated idempotently on every one of those paths.

#### Launch reminders

What RimZ tells the agent about itself arrives as one `<system_reminder>` tag, which the process compiler delivers through the adapter's append-system-text channel: merged into a native argv flag or config key, or, for Pi and OpenCode, exported as `RIMZ_LAUNCH_REMINDERS` for RimZ's in-process extension to append to the root session's system prompt. The wrapper collects the parts as a `LaunchReminders` value, and [`launch_reminders.rs`](../../../crates/rimz/src/harness/launch_reminders.rs) renders identity followed by `###` sections in a fixed order. Environment always carries the temp lines, so every launch carries the tag.

| Order | Content | Present when |
| --- | --- | --- |
| 1 | Team heading, identity, grouped pipeline and channel rule; otherwise a standalone model line | a non-child team member; otherwise the model line, when the launch knows its model and `model-reminder` is on |
| 2 | Loop: what launched the agent, how long it lives, and who is watching ([loops.md](./loops.md#the-loop-reminder)) | a loop fire's prompt leader (resident) or each attempt of a loop's single run; never a restart, resume, or check launch |
| 3 | Environment: cwd, optional `worktree` bullet naming its recorded base branch and primary checkout, shell-kind bullet, optional `lsp` bullet naming languages and `Skill(rimz-lsp)`, the `tmp` and `shared` bullets ([sandbox.md](../sandbox.md#launch-reminder)), then the team's memory-file listing unless the launch gets it [at prompt submit](#environment-at-prompt-submit) | every compiled launch, since it always carries the temp lines; team context always includes cwd, worktree follows cwd only in a linked checkout, shell requires env reminder |
| 4 | Subagents: catalog, or the child's no-delegation body | a non-child request with a catalog; every child request |
| 5 | Skills: invoke a skill whose description matches the work | the launch's profile `skills` list names any `rimz-*` skill |

The inputs come from configuration. For a non-child request the wrapper derives the profile's allowed subagent catalog from effective project and machine configuration. For a team member it resolves the effective team definition: worktree, role and team handles, channel, leader, every seat with its owned stages, session posture, and the declared scratch entries present at launch. `model-reminder` is read from the launched profile itself, looked up by the exact `LaunchParams.profile` name in `[subagents.profiles]` for a child and `[agents.profiles]` otherwise, with no walk up the `agent = …` chain. Unset, absent from config, or no profile at all means on. Because the lookup is by name, restart, fork, resume, and rebirth reproduce the same answer. A failed effective-config load warns and falls back to default reminders, which keep the model line. The temp lines come from the compiled plan, so they also survive that fallback.

The Team section (`launch_context.rs`) gives the member's identity, with a resumed or forked session noted when applicable, then the grouped pipeline. Fresh sessions get no session sentence, and the worktree path appears only as Environment's cwd. A non-leader seat on a team that declares its leader then gets the unchanged channel rule: no user reads its pane, and user-bound questions go to the leader. The leader's own channel rule stays in its prompt. Board-opening and finished-run decisions live in the consensus rather than the reminder.

`launch_plan::compile` switches the Environment bullets on; the cwd bullet names the provider cwd rather than the compiler's current directory. A linked worktree gets `- worktree: branched from <base>; primary checkout at <path>` directly after cwd, or `- worktree: primary checkout at <path>` without a recorded base. [`worktree.rs`](../../../crates/rimz/src/worktree.rs)'s `linked_worktree` reads the cwd's `.git` metadata for the primary checkout and the RimZ marker for the base, without spawning git; a primary checkout, a directory outside git, or unreadable git metadata gets no bullet. An unreadable marker omits only the base. These are records written at creation, not sampled git state. The primary path is unchanged in both isolations. The shell is the one the process compiler resolved for the wrapper: the machine's `[agents] shell` when set, else `proc::user_shell()` at compile time. It renders as its kind, the path's file name (`zsh`, `bash`); `None` omits the shell bullet, and `explain` names the shell the launch would run under. With `[agents] shell` set, `SHELL` and `CLAUDE_CODE_SHELL` name it too, so a provider that reads either runs its tools there (Claude only for bash or zsh, per [its tool shell rule](../../externals/agent-adapter/claude-reference.md#tool-shell)); without it, the bullet does not promise which shell the provider's tools use. `[agents] env-reminder` defaults to true; a trusted project's value overrides the machine value, while an unavailable effective config leaves it on. Every exec, including children, resume, fork, restart, and rebirth, probes afresh. The reminder carries no sampled git state: launch and the agent's first read can be far apart, so a snapshot would be stale truth. Git state arrives [at prompt submit](#environment-at-prompt-submit) where the adapter can carry it, and the agent runs git itself otherwise. The cwd, worktree values, and shell are escaped independently to preserve the one-tag boundary.

The pipeline line uses the team's declared `stages`, or the stages its roles own in role order. Consecutive stages with the same owner group together: `Pipeline: Explore, [Plan] (you) → Implement (@coder) → Reflect (you) → Done`. The current board stage uses the bracket notation of `team_stage::stage_strip`, including `[Done]`. Non-adjacent stages of one owner form separate groups. Seats owning no stage appear as `Also on the team: @scout.`; a team without stages gets `Members: @planner, @coder (you).` instead of a pipeline.

Environment ends with the [team memory-file listing](./teams.md#scratch-files), using the declared patterns and paths relative to cwd. It carries facts only: no board sentence, snapshot warning, or instructions to read or create files. A launch that gets the listing at prompt submit carries none here, so the listing is never in both places.

#### Environment at prompt submit

A listing or a branch sampled at launch can predate the notes an earlier stage owner is still writing. `launch_plan::compile` therefore decides, once per launch, to deliver the volatile facts with the prompt: when the machine's `[agents] runtime-env` is on (the default, and the answer when no effective config loads), the adapter declares `prompt_context` (Claude and Codex), and the provider's RimZ hooks are installed and trusted under the launch's login environment (`agents::preflight_hooks`), since the hook is what carries the block. The process compiler then sets `RIMZ_RUNTIME_ENV=1` and the reminder drops the listing; every other launch clears the variable, so a nested launch never inherits it, and keeps the listing in its reminder. A launch whose hooks are missing or untrusted is unstamped without a warning: `rimz start` and `rimz doctor` already report them. Hooks removed, or trust revoked, after launch lose the listing until the next launch. The key is machine-only: a project layer cannot set it and it is outside the trust hash.

The prompt-submit hook reads the switch from its own environment, then the provider process's, and attaches for a root agent's prompt only: never a native child's, a side conversation's, or a daemon-owned hook's. [`runtime_env.rs`](../../../crates/rimz/src/harness/runtime_env.rs) fires on the first prompt of each conversation in each provider process, by exclusively creating a marker under the runtime `live/runtime-env/` directory keyed by process identity and session id, and again on every prompt that carries a delivered stage notice. The claim is attempted on every qualifying prompt and refreshed while the conversation keeps prompting; `rimz gc` reaps a stale marker with the other live sidecars, and a marker that cannot be written fires again.

The block is one reminder under `### Environment`, `Sampled as this prompt was submitted.`: the `$ ls` listing exactly as the launch reminder renders it, for a team seat whose team declares patterns, then one fence with `git status --short --branch` and `git log -5 --oneline`, both run with `--no-optional-locks` under one two-second deadline. Both parts are sampled at the hook's resolved worktree root: the Git top level of the checkout the hook runs in, so a seat in a linked worktree gets that worktree's root. Outside Git it is the nearest `.rimz` marker directory, else the directory the hook runs in. The payload's `cwd` is never the root, because it follows the agent's shell while the team's patterns stay anchored at the checkout root; listed paths are relative to that root. The git fence appears only when both commands succeed, so a directory outside git, a repository with no commit, or a slow repository gets none. Sampled git lines are escaped like every other reminder text and cut at 200 characters after escaping, and status lines past the 4,000-character cap collapse into one `(N more lines)` line. The listing is uncapped, as it is at launch. With nothing to show the hook stays silent. The block is context for the model: the recorded prompt and `rimz agents logs` keep the text the user typed.

Team members never show model names; `model-reminder` has no effect on their Team section. Without a team paragraph the model becomes its own line, opening with the role or else the profile (`You are @planner, running on Opus 4.8.`, or `You are running on …` when the launch carries neither), unless `model-reminder = false`. A launch that knows no model gets no fragment instead of a guess at the provider default.

These paragraphs reach Claude, Qwen, Droid, Codex, and Grok through native launch arguments, and Pi and OpenCode through their RimZ extension. Other providers get only the child no-delegation body, through a user-prompt fallback.

#### Direct exec or resident wrapper

The wrapper runs the agent in the pane, inheriting the pane's TTY. It launches through the launch shell's startup path (the machine's `[agents] shell`, else the user's shell) when that shell and `/usr/bin/env` are available, and execs directly otherwise. A configured `[agents] shell` that cannot run refuses the launch instead.

Whether the wrapper stays behind the agent is one predicate, `should_exec_agent_directly`. A launch with post-exit work (a supervised run, a close-on-exit pane, a worktree) keeps the wrapper alive as the parent, which makes it the attach point for [supervised runs](./scripting.md) and for [Reclaiming a pane](#reclaiming-a-pane). A plain in-place launch has none of that work, but a fresh one can still die at startup, and only a surviving parent can [relaunch it](#a-startup-death-is-relaunched-first), so it keeps the wrapper too while `[agents] startup-relaunches` is above `0`. That wrapper holds no pane and no shell: it exits with the agent's exit code, or 1 when a signal killed it. With the cap at `0`, and for every in-place resume or fork (neither is ever relaunched), the wrapper execs the agent in its own place and disappears.

Room birth also carries one adapter-enrichment environment map through the mux seam, so a stock agent typed directly into an ordinary work shell inherits the room baseline. RimZ-managed launches still apply their adapter `launch_env` last. A process or shell already running keeps the environment it started with; rebirth is the parity boundary on both backends.

### Placement

`crates/rimz/src/harness/ancestry.rs::LaunchFocus` resolves the shared focus policy from `--bg` and the caller identity: `Keep` for either an agent caller or `--bg`, otherwise `Take`. Interactive launch shares its caller resolution with ancestry stamping; fork, lane resume, and restart resolve from their loaded agents, and team restart passes one decision to every member. `Keep` also suppresses already-live cohort and lane jumps and downgrades in-place launches to splits. Restart skips pre-focus and anchors its unfocused replacement to the old pane in the room session. The supervised `-p` return-now meaning of `--bg` is unchanged. See [multiplexer focus behavior](../multiplexers.md) for the Zellij 0.44 transient.

A layout lands in a new tab, a split of the current tab, or, for a single non-worktree cell, the pane the user is already sitting in. Both backends receive the same `TabOptions` (session, title, cwd, focus flag, sidebar options, the pre-built pane argv, and the environment map for every command pane) and dock the global sidebar once before adding the layout cells; the per-backend split commands live in [`mux/`](../../../crates/rimz/src/mux/AGENTS.md).

**Placement resolves before the launch touches the store or creates a worktree**, so a rejected placement leaves no provisional rows or orphan worktree behind. The CLI placement resolver takes explicit flags first, then falls back to the per-machine [`[agents] placement`](../../guide/configuration.md#agent-profiles-commands-and-teams) policy.

| Situation | Where the layout lands |
| --- | --- |
| `--new-tab`, or no ambient pane to split | a new tab |
| `--new-pane` | a split of the current tab; an explicit flag that cannot be honored fails fast |
| policy `auto` (the default), single non-worktree cell, inside a room | the current pane: the CLI execs the wrapper argv in place, and the pane returns to its shell on exit |
| policy `pane`, single non-worktree cell, inside a room | a split of the current tab |
| policy `tab` | always a new tab |
| any policy, with a named channel, a multi-cell layout, or a worktree | a new tab |
| `--bg`, an agent caller, or create-on-miss | never in place: the caller's pane stays available, so an in-place choice downgrades to a split |

An in-place launch resolves liveness from the pane instead of from an end trace, because no wrapper stays resident to write one.

Tab titles follow the address vocabulary. A named-channel or worktree launch names its tab `#<NAME>`, and a named team launch names it `team:<name>` and stamps its in-place lane as `<dir>/<team>`. Other launches join their pane names with `+`: profile else kind for agents, executable basename for commands, and shell basename for empty command cells, keeping three tokens before `+…` and omitting the directory suffix (`opus`, `nvim+claude`). An empty layout falls back to `term`. An in-place launch applies that title to its existing tab before exec and pins the pane's own launch name.

Titles are best-effort display. The sidebar producer returns a pane-named tab to the shell's name once no agent remains; tmux also restores inherited automatic naming and clears the pane-name pins. Scoped and user-chosen names that do not match the panes are kept ([multiplexers.md → tab names](../multiplexers.md#tab-names)). Mux tab names are mutable and live outside the store, so they never form an address.

### One team per checkout

A fresh team launch refuses a checkout held by a different team: a present member holds it, or, with no present members, a board stage other than `Done` holds it. `resume::inspect_team_hold` takes the holder's identity from root launch occupants in the audit store rows at the normalized checkout path and reads the stage through `scratch::board_stage`; no team rows, a missing stage, or a `Done` board with nobody present means no hold. In `launch_layout` this check follows the `--from-pr` holder lookup and precedes reconciliation and named-worktree creation, including for in-place, `--cwd`, and channel launches. Same-team reconciliation stays unchanged; explicit resume, rebirth, and channel resume are not gated. The audit read is unlocked, so concurrent launches can both pass. A board-stage refusal names the release too: a stale run is abandoned by setting the checkout's `blackboard.md` to `Stage: Done`.

### One team per channel

A fresh team launch also refuses a channel held at a different checkout, even by the same team, so separately launched cohorts cannot merge into one address or restored tab. `resume::inspect_channel_hold` uses the same root-occupant selection and hold logic as the checkout rule: the newest present member holds it, otherwise the newest member holds it while its checkout's board stage is pending and its team is the one `inspect_team_hold` names at that checkout, since a board records no team or channel and a later run there on another channel owns it. Rows without a recorded checkout are ignored. Channel matching uses `AgentState::channel`, including its worktree-basename fallback for unstamped rows.

The check runs just after the checkout check, before reconciliation, checkout settlement, channel registration, or store writes. `spec::resolve_room_channel` computes the eventual channel there, explicit, inferred for a role, or derived; a bare `-w` has no target yet and remains unchecked. The same checkout is exempt, comparing canonical paths where available and lexically normalized paths otherwise. This preserves symlinked `--cwd` launches and same-team reconciliation. The existing same-team, same-checkout fresh in-place launch gap remains: that launch can still duplicate a cohort. Explicit team resume, channel resume, and rebirth stay ungated. As with the checkout rule, the audit read is unlocked, so concurrent launches can both pass. Refusals name the channel, holder team and checkout, reason, resume command, and alternative channel; a pending-board refusal also names the holder's `blackboard.md` and `Stage: Done` release.

### Cohort relaunch reconciliation

Relaunching a team into a worktree that already held one is where a naive launch would silently duplicate work. Reconciliation runs whenever the command names a team, or an inline layout with at least two agent cells, and supplies an explicit `-w NAME` or reuses a holder through `--from-pr`. It runs after placement and the live-room preflight ([`reconcile.rs`](../../../crates/rimz/src/cli/agents_cmd/reconcile.rs)). For `-w`, it precedes worktree resolution; for `--from-pr`, it follows the holder lookup but precedes tip choice and settlement, including before a resume ([PR checkout ordering](./worktrees.md#from-a-pull-request)).

The caller supplies the named worktree path without creating it, or the PR branch holder's actual path. Reconciliation reads the audit rollup for matching root members in that path and picks one of four outcomes.

| History in that worktree | Outcome |
| --- | --- |
| none | continue into the ordinary launch path |
| live members | focus the newest bound member and exit under `LaunchFocus::Take`; under `Keep`, report already running without a jump |
| closed, with dirty or unproven work | offer resume / fresh / cancel, default resume |
| closed, clean and content-landed | offer remove / fresh / cancel, default cancel |

Both offers read one line through `cli::choose`, which takes the default on Enter and returns `None` for an ambiguous prefix, an unknown word, or EOF. The caller maps `None` and an explicit `cancel` to the same no-op and prints `canceled; nothing launched`, so a declined relaunch is never silent. Without a terminal on stdin neither offer prompts. Each prints its alternatives instead: `relaunch_commands` builds the resume and `--fresh` spellings for both teams and inline layouts, and the remove offer prints `rimz worktree remove <name>`.

Named-team reconciliation considers every member of that team in the target worktree, including sibling roles when a single-role spec relaunches. Inline membership matches by launch group, then ordinal, then kind and role, with a final kind-only fallback for records that carry no role. Ended and confirmed-dead members both count as closed. A worktree holding only non-live launch placeholders has no cohort, by the same admission rule resume uses, and proceeds to a fresh launch.

The fresh outcome is `Reconciled::Continue`. Launch then resolves the checkout with `reuse_existing`, the same path an absent cohort takes into an existing marked worktree: no worktree or branch is removed or recreated. `retire_removal` (session retirement plus channel message archival) fires only on the remove choice. Prior rows stay unchanged with their names reserved, and the fresh members mint new petnames in the same channel. Orphan message archival stays with `rimz gc`.

`--fresh` on `CohortLaunchArgs` takes the fresh outcome without prompting. The `Closed` arm returns `Continue` before the Git assessment, so it works on a dirty tree and a clean landed one alike, and leaves a clean landed tree standing. `Present` is checked first, so `--fresh` cannot duplicate a live cohort. `launch_layout` requires an explicit worktree name before it opens the store. The check lives there because the fused `team#lane` spelling fills `worktree` only after clap parsing, so a clap `requires` would reject it.

## The address

Every member has an address typed like an @-mention: `@<handle>#<channel>`. The handle names who, the channel names where, and both default from context. [cli/agents.md § Addressing agents](../../reference/cli/agents.md#addressing-agents) is the handle catalog for users; this section is how an address resolves.

### Resolving the channel

The channel is the workspace segment the room already groups by, resolved in order: an explicit named channel, else a worktree name, else an in-place team stamped at launch as `<dir>/<team>`, else a directory basename fallback for unstamped agents ([messaging.md § Channels](./messaging.md#channels)). Routing keys and durable records retain that value. The store fold derives `root_lane` for an unstamped agent whose worktree path is the workspace record's project root; `AgentState::lane_label` and `PaneAgent::lane_label` display it as `main`. Addresses and listing filters accept `#main`, the root basename, and the root path. The name remains unreserved: a stamped `main` lane shares the displayed name, with handles resolving the agents. Message and transcript root filters include channel-less historical records and records under the root basename, without rewriting them. Directory-room root pods display `main`; repo-root pods keep their branch labels. Native tab titles retain their routing names for resume matching.

The default is the channel the command runs in, and an inline `#<name>`, `--channel`, or `--worktree` overrides it. **A human shell in a bare directory workspace has no current channel, and no current channel means every channel.** An agent uses non-empty `RIMZ_CHANNEL`, else its stored card channel, before the worktree rule. The caller lookup is the same as `@me`; an ended caller's row still supplies its channel. An unavailable store or unresolved caller falls through silently. `RIMZ_TEAM` is cohort identity for team members.

A scoped miss names the searched `#lane`, notes when the ambient lane came from a directory name, labels root-lane matches `main`, and supplies a resolving correction address (or a corrected channel flag). The shared CLI error renderer adds a shell-quoted whole command on a `try:` line when the target appears verbatim in the invocation.

Launch specs resolve against the same lane. A bare role qualifies to `<team>.<role>` when the lane's agents carry that team, so `rimz agents reviewer` in `#forge` launches the forge reviewer and stamps it into the lane it resolved from. The inference reads the stamped `team` on those agents, because the three lane shapes mean the channel string alone does not name a team. A bare role that also names a cell resolving to a different agent refuses instead of guessing. Branch names are display metadata only.

### Handle classes

A handle falls into one of three classes, narrowing from group to instance.

| Class | Examples | Matches | Can create? |
| --- | --- | --- | --- |
| Role | `@coder` | every agent launched under that team role in the channel, except an unclaimed launch card (no registered session, no pane) while a claimed holder matches in its own lane | no |
| Type | `@codex` (kind), `@planner` (profile) | every agent of that kind or profile in the channel | yes |
| Instance | `@writer` (explicit `--name`), `@swift-otter` (petname), `@claude-2` (kind ordinal), a session-id prefix, `tmux:%1` (pane address) | exactly one running agent | no |

`@all` is the broadcast handle that resolves the whole channel. Role names reserve built-in kind handles so kind addresses keep round-tripping, and a profile name that would read as `@all`, a kind ordinal, or a pane address is rejected at config load.

Only a type handle creates, because only a kind or profile carries what a launch needs. An instance handle names something that must already exist, and refuses with the fix.

### Arity decides the outcome

An address resolves against a fresh snapshot to zero, one, or many agents.

| Matches | Outcome |
| --- | --- |
| one | delivered |
| many | an ambiguity error listing the handles to pick from, unless `--all` or `@all` opts into fan-out |
| zero | a miss that names where the agent runs in another channel and lists live agents, or, with `--create`, a launch |

Fan-out delivers to every match, prefixes each delivery with the addressed handle (`@all,`, `@claude,`), and skips a blocked agent while the rest send. Resolution itself is caller-agnostic; message dispatch then removes a RimZ-launched caller from an intrinsic `@all`, so an agent never messages itself.

`--create` launches a missing agent straight from its address. `rimz message --steer @planner#design --create "draft the API"` opens a `planner` in `#design` with the text as its first prompt. With `--worktree feat/x` it creates or reuses that worktree instead.

Resolution has two sources and one matcher set over both: rollup sessions (`&AgentState`, used by management commands and parked message records) and the live agent panes the producer bound (`&PaneAgent`, used by `--steer` and send-now messages). Each command chooses its source. Pane binding then joins a match to exact lifecycle state, a same-channel provisional launch card, or a sessionless lazy target, so an agent is addressable before its first turn registers.

### Petnames and the canonical handle

The petname is the stable per-instance fallback name. The store mints an adjective-noun pair at registration through [`agents::petname`](../../../crates/rimz/src/agents/petname.rs). It refuses reserved command words and kind-shaped names, so a petname can never shadow `@all` or `@claude-2`, and it is collision-checked against every name in the rollup (retained ended rows keep their names reserved) and against existing session-id prefixes. A session with no stored name re-derives one deterministically from its session id, so old logs still render a stable name.

The rendered handle is the shortest address that names exactly that agent, and it round-trips through the parser. The renderer tries, in order: the role when unique in scope (a launch card nobody claimed yields it to a claimed holder in its own lane, so a role survives a card left behind by a launch that died before binding, while across lanes both still match), the explicit `--name`, the profile when unique, the kind, the petname, then `@<kind>-<n>` when scoped, and finally the session id. A listing therefore always shows a handle you could type back, and a handle appears only when typing it reaches that one agent. Every agent-bearing listing shares this one renderer; `address.rs` owns both it and the parser, and tests them against each other.

Message headers instead use `agents::petname::sender_handle` (role, name, kind) so the reply address stays stable when peers join or leave; the [header contract](./messaging.md#the-message-header) covers its label and channel suffix.

## Resume and rebirth

The store remembers agents whose processes are gone, and resume turns those records back into panes.

### Three entry points

| Path | Trigger | Scope |
| --- | --- | --- |
| Room rebirth | a machine reboot or mux crash, at the next `rimz start` | root sessions from the producer's persisted live roster and the pending-recovery record, intersected with the audit rollup, seeding one tab per live-at-death lane or worktree after the new mux session is created; entry into an already-running room repairs settled seats and reports the agents still parked without opening tabs |
| Cohort resume | `rimz agents <spec> --resume` (`--continue` is the visible alias) in a live room | one prior cohort matched from the spec, after its tab or pane was closed |
| Lane resume | `rimz agents resume <scope>` | one lane, resolved by `harness::resume` |

Rebirth restores a named team in its declared layout, resuming members that can resume and fresh-launching missing or unsupported agent cells so the shape stays whole; other lanes restore as one column. Cohort resume scopes to one exact worktree with `-w <NAME>` or the caller's current worktree, while a resume from the project root takes the newest match for the spec.

The CLI applies `LaunchFocus` to every lane action: `Keep` reports an already-live lane without a jump and opens restored tabs and splits unfocused.

Lane resume picks one of four `LaneResumeAction` variants: `List` when no scope was given, `Focus` on the freshest pane when every member is live, `SplitClosed` to plan flat resume commands beside a surviving live member, and `RestoreClosed` to reuse the rebirth team and flat split when every member is closed. Discovery drops sessions the store records as children before selecting the newest concurrent set, for both listing and restoration. In every case the CLI runs the compile preflight and the [login health check](#every-path-that-builds-a-launch-layout) on the planned provider kinds through `LaneResumeAction::agent_kinds_needing_preflight` before `LaneRestorePlan::materialize` allocates fresh team identities or any mux action runs, so an untrusted launch environment or an account without hooks fails the whole command before it half-rebuilds a room.

Lane `--fresh` restores closed named-team tabs with every cell seeded `CohortSeed::Fresh`, using the current team layout in the saved cwd and channel. New sessions receive new handles and the team's board and memory-file reminders, without requiring saved provider conversations or matching the old login. Flat roots are skipped with deduplicated relaunch commands: `-w <worktree>` for a worktree lane, `--channel <channel>` for a project-root channel lane, and no place flag otherwise. `plan_lane_resume` refuses a lane with live root members, no durable candidates (including discovered-only lanes), or no restorable team; the last error carries the relaunch commands. `--bg` still applies. Team panes count toward `resume.max` as on ordinary resume.

### What matches what

| Spec | Matches |
| --- | --- |
| named team | prior root agents with the same `team`, role cells mapped by role, newest member per role |
| inline multi-agent | the newest `launch_group` that maps onto its agent cells by `launch_ordinal`, falling back to kind for records without ordinals |
| single agent | the newest root session of that kind, and of that profile when the spec named one, whatever its state |

Missing cells launch fresh in the matched cohort's cwd and channel, so the layout stays whole. A matched member whose process is still live refuses the whole resume and names it, because launching beside it would duplicate the addressable role or kind; for a single-agent spec that means a live newest session refuses even when an older one is dead. A kind whose adapter has no native resume argv launches fresh and is reported as such.

Cleanly ended members stay candidates, so a closed team resumes while its worktree still exists. No resume path plans a row with a parent identity: `harness::resume::root_session` excludes both RimZ-launched children and provider-native subagents. The parent relaunches any child it still needs. Empty session ids, missing worktrees, and launch placeholders that never adopted a session and are no longer live are also excluded.

The rebirth roster still carries children for crash protection. A birth does two separate things. The boundary records that an incarnation died: it parks the roster's agents in `records/pending-recovery.json`, appends `session.rebirth`, and deletes the roster, ending nobody. It cancels each roster child's newest open run and wakes its waiter; the child remains parked until settlement or the reaper ends it. An inspection-failed or supervised birth records only the boundary, so its children wait for the next live-entry settlement or reap. A room's death is not yet the death of its agent processes: each exec wrapper holds its provider through two signal graces after the pane's hangup. So the boundary inspection waits up to 3 seconds for the live owners of the agents it could offer to exit, without signalling them, and plans from the last read of both the process table and the event log, so a provider that records its own end while dying in the wait is ended, not offered; an owner still live then leaves its agent parked and unmentioned. A birth with no such owner waits for nothing, and a live-room settlement never waits. The settlement decides what happens to the root candidates, by one of four dispositions (`RebirthDisposition`):

| Disposition | Planned agents | Candidates the plan cannot resume |
| --- | --- | --- |
| `Defer` | stay pending | stay pending |
| `Decline` | ended `rimz.recovery-declined` | ended `rimz.recovery-declined` |
| `RecoverKeep` | resumed | stay pending |
| `RecoverDrop` | resumed | ended `rimz.not-resumed` |

Every disposition, including `Defer`, ends each non-live, unended child in scope as `rimz.child-not-resumed`: recovery never resumes a row with a parent, and the parent relaunches what it needs. Children are not recovery candidates and appear in neither the drop-rest question nor the parked count. Before stamping the end, settlement cancels the child's newest open run and wakes its waiter. Each durable end appends one informational `recovery_child_ended` diagnostic; a failed end append leaves the child pending.

Every disposition also ends a parked team member `rimz.seat-refilled` when a live member already fills its matched seat. Such members are not recovery candidates, and a partially live cohort plans no team tab: its empty seats fall back to individual resume planning. A planned Fresh replacement is different: its old session stays a candidate until its tab is confirmed open, then ends `rimz.seat-refilled`. A failed tab leaves it pending; `Defer` opens no tab and leaves it pending, while `Decline` ends it `rimz.recovery-declined`. Each refilled end appends one informational `recovery_seat_refilled` diagnostic, not an assist record. Explicit cohort and lane resumes settle the same way through `settle_refilled_seats`: once `rimz agents <spec> --resume` has placed its cohort or `rimz agents resume <lane>` has opened a team tab, the Fresh-seeded members' old sessions end `rimz.seat-refilled` and leave the pending record. Here `open_tab` returning successfully confirms placement, without a structural check; a tab that fails to open leaves its old sessions parked. Same-pane cohort placement settles before exec, since a successful exec never returns. `rimz agents resume --fresh` ends none.

Only an attended entry chooses a disposition that ends root recovery candidates; children and automatic seat refills are settled separately above. With recovery on, an interactive birth asks whether to recover the resumable agents (default yes; no is `Decline`), then lists the ones that cannot be resumed with their reasons and asks whether to drop them (default no, which keeps them pending). With recovery off (`--no-resume` or `resume.on_rebirth = false`) an attended entry declines without a prompt. An unattended birth recovers and keeps the rest when recovery is on, and defers when it is off. Missing-worktree agents appear in the second question with reason `worktree gone`. They stay pending under `Defer` and `RecoverKeep`; `RecoverDrop` ends them `rimz.worktree-gone`.

Every live-room entry, attended or unattended, reconciles through `RoomContext::reconcile_parked_recovery`. It takes `recovery.lock` before inspecting pending membership, drops already-ended entries, silently ends non-live children, and ends seats already filled by live members when recovery is on. Other root candidates stay parked under `Defer`, except that an attended entry with recovery off declines them. It writes no session event, roster, or boot marker, never touches a live agent, asks no recovery question, and opens no tab. The CLI prints at most one root count notice naming `rimz agents resume <lane>` and `rimz start --no-resume`, or the count ended by an attended decline. Web and remote confirmation switches apply only to births; a reconnect to a running room does not wait for recovery consent. This flow is the same on Zellij and tmux, even when no client is attached.

A rebirth settlement runs in two steps (`RebirthPlan::settle`, then `SeededRecovery::confirm`): resumed agents and planned Fresh replacements stay pending until their own tab is known to be open, and each tab settles alone. A confirmed tab's agents leave the pending record; a tab that did not open keeps its agents pending for a later rebirth or explicit resume, fails its team launch batch, and prints one warning naming the tab and the reason. A birth seeds its tabs with the session, so after the health gate it asks the backend (`MuxBackend::confirm_resume_tabs`) which planned tabs the session holds: a tab is open when a live tab of its name holds at least its planned panes besides the sidebar, each planned tab claiming its own live tab. A pane whose process exited does not count, including one tmux keeps under a user's `remain-on-exit`. The check reads tab structure only, waits a bounded time on Zellij for the layout to materialize, and runs on a failed health gate too before the error propagates. The birth holds `recovery.lock` from settlement through confirmation, so an entry that finds the room already live waits and then inspects the settled record instead of opening a second tab for agents still pending. Zellij closes an unconfirmed tab by the stable ID returned by `new-tab`, best effort, before returning the error, so a later recovery does not deliberately duplicate a half-open tab. Recovery counts and assists include only confirmed opened tabs.

`RebirthPlan::settle` cancels each ended child's newest nonterminal run through `run::cancel_and_wake`, including `keep` runs. Already-terminal runs stay untouched. The durable cancellation settles the parent's wait and lets its fleet digest report the result; the waiter wake datagram is best-effort.

Flat resume keeps pane identity when a stamp survives: newest-first candidates sharing one pane collapse to the newest session. A rebirth boundary retires pane stamps, because pane ids renumber across a mux restart; an unstamped root stays a candidate and deduplicates by `(kind, session id)`.

`resume.max` bounds how many agents one reborn session auto-resumes (`DEFAULT_RESUME_MAX`, 128), so a long-lived workspace cannot fork-bomb the machine on birth. Anything past the cap is reported as a skip. Every `ResumeSkip` carries its reason into the start report: `no resume CLI`, `no saved conversation`, `over the resume cap`, `no prompt replacement`, or `different account`. The recovery preview also lists missing checkouts as `worktree gone`.

### Discovering sessions the store never saw

When an explicit lane has no durable candidates, or every closed durable candidate has lost its provider conversation, lane planning asks its caller for adapter local-session observations, read from the provider's own session files.

Each observation spans `[created_at, last_activity]`. Transitive interval overlap forms concurrent clusters, and the cluster containing the newest activity is the last concurrent working set. That cluster resumes newest first up to `resume.max`; every older cluster is reported and stays closed. A synthesized flat record carries only kind, exact session id, workspace, and last activity, with no channel, because provider files cannot reconstruct RimZ-only roles, teams, or lanes.

### Posture

Before process compilation, `crates/rimz/src/harness/launch_plan.rs::prepare_exec` resolves a durable launch record's requested model through machine pins, the account-scoped adapter catalog, then passthrough, as on a fresh launch. Observed model switches do not alter the record. Only legacy sessions without a record replay the last observed session model when their posture names an alias (fork uses the source session), and an explicit `--model`, `--tier`, or `--agent` on resume skips that replay through `ExecIdentity.resume_model_override`; a missing observed model warns and resolves afresh. Resolution rewrites both provider argv and launch parameters, so the bound event, model environment, and reminder agree. `agents explain` remains read-only and shows the requested alias.

A resumed pane runs `rimz agents exec <kind>` with a `Resume` action carrying the provider session id and the prior RimZ identity (name, profile, role, team, launch group, launch ordinal, channel). Its **posture** uses the planner's `ResumeLaunchPosture`: model, effort, and `auto-compact` argv, system-prompt source paths, skills, permission mode, budget, profile `args`, and an optional isolation override. `resume::resolve_posture` supplies this projection for profile-based relaunches; cohort resume projects its finalized layout cells, rebuilt through the same resolution for recorded seeds. `ResumePosture` adds only the degradation reason. The native auto-compaction window replays through the current profile's rendered argv; no separate environment variable or durable launch field carries it. A session that launched as `@planner` comes back as a planner.

`resume::resolve_posture` resolves the stored profile against effective config, rebases onto `LaunchRecord.agent` when present, then overlays the record's verbatim model and effort. The profile and role identity stay unchanged. A missing base degrades with its name. With a record and no base, the provider's `Profile.definition_renders` entry is chosen by the session's kind, so a session routed to another provider stays on it after its tier clears. A record-less routed session's durable `LaunchParams.tier` still selects its provider's complete `Profile.definition_renders` entry and stamped model, without a tier walk or usage check; effort comes from the current definition or model default. The record wins over a contradictory tier and clears it. Restart, fork, subagent resume, unattended rebirth, and `explain @handle` share this selection.

Team restore and cohort resume rebuild every member through `resume::restore_routed_cells` before preflight and pane compilation, retaining team prompt and cohort identity. A record replays even without a tier stamp; legacy rows fall back to the stamped tier or current profile. A recorded team member keeps its role binding's prompt files and `args`, and `agents restart` keeps them too through `resume::resolve_member_posture`, which finds the member's role in its recorded team. Profile matching admits a single-profile or roleless-team member carrying a record or a tier stamp even when its provider differs from the current load choice; named team roles still match by role. Permission mode: an explicit `--ask` or `--yolo` wins; otherwise a recorded session replays its saved mode over any profile- or role-declared one, and `--agent` keeps the saved mode unless the new base declares one. Legacy rows keep the older precedence: the declared mode, else the launch event's recorded mode.

Degradation is deliberately asymmetric. A profile that is gone, broken, or now names a different provider degrades to a bare resume with a warning, because rebirth runs unattended and a recovery must never refuse. Interactive `restart` and `fork` escalate the same provider switch to the user instead, since changing providers under a running agent is a decision for the user.

`finalize_agent_cell` stamps model, effort, and the non-routed agent base into `LaunchRecord`. `ResumeLaunchPosture` supplies both record and tier to exec parameters; the wrapper's `agent.attached` saves record, tier (including clearing it), and present mode on every reopen. Isolation remains an independent present-value overlay. Cohort overrides apply after matching: `--agent` must keep the provider; `--tier` selects only same-provider entries in the requested row using fresh launch availability, without climbing, and logs tier-fallback assists for resumed as well as fresh seats. `--model` clears the tier. Cwd and channel come from the matched cohort; `--worktree` is a scope and creates nothing. `validate_resume_inputs` refuses only system-prompt files and passthrough on both explicit and reconcile entrances. A positional prompt becomes a queued Human message at gate Done, addressed to the leader's session or provisional fresh card before placement; it never becomes a provider launch prompt. A resumed leader's card is still ended at that point, so the wrapper's resume stamp releases the prompt, not the provider's registration, and it is delivered ahead of the leader's stage re-wake. Original prompts and passthrough argv are not replayed.

The wrapper attaches the resumed session before the provider starts. Once the provider process stage is ready, the resident exec wrapper appends `agent.attached`, binding the exact resumed session to the stable launch id exported to its process, the wrapper's pane, and the runtime owner of the agent process. When the wrapper execs the provider in place, it is that process, and the one record is complete. When it spawns the provider instead (a worktree, close-on-exit, or supervised launch), the first record carries the wrapper's own owner, so identity exists before the provider can call RimZ, and a second `agent.attached` stamps the spawned provider's pid immediately after the spawn. A resume appends `rimz.agent-resumed` on either path before the provider starts, so it precedes any hook the provider fires, and re-wakes the member's open team stage ([teams.md § Resume re-wake](./teams.md#resume-re-wake)). The provider-owned stamp is what proves a later same-pane conversation belongs to the same instance ([instances.md § Launch identity across conversations](../agents/instances.md#launch-identity-across-conversations)). A row with no launch id uses its provider session id for the new stable stamp, and a provider-store discovery gets an idle durable seed, so either can launch a child or be addressed before a lazily registering provider emits its first hook.

## Reclaiming a pane

When an agent exits, the resident [exec wrapper](#the-exec-wrapper) either leaves the pane usable or reclaims what automation owns. The decision turns on whether the exit was *deliberate* (`close_is_deliberate`). A supervising wrapper traps hangup and termination from before its first pane binding, so a pane closed at any point after the store names it owner still reaches this decision; a wrapper that execs its provider in place keeps the default dispositions and leaves the end to the provider.

```text
agent process exits
  │
  ├── clean child exit ─────────────────────────► deliberate
  └── abrupt (tab/pane close, signal)
        └── does the mux still list the session?
              ├── yes (room alive, even mid-teardown)
              │     └── is the agent in records/pending-recovery.json?
              │           ├── no ─────────────────────────► deliberate
              │           └── yes (the listed session is a reborn room) ─► not deliberate
              └── no  (reboot, mux crash, exited session, listing failed or timed out)
                     └────────────────────────────────► not deliberate
```

The probe is the backend's session listing (`MuxBackend::session_accepts_agent_close`). A session that is listed but otherwise unresponsive counts as deliberate; only a failed or timed-out listing marks the server as gone. A live room with missing sidebar chrome still treats a pane close as deliberate.

A listed name does not prove the room is the wrapper's own. A wrapper still on its exit ladder when the next start brings the room back sees the new session under the same name. Every rebirth parks the dead room's roster in the [pending-recovery record](../store.md#the-sessiondeath-record) before it creates the session, so the wrapper reads that record after the listing and treats an agent named there as lost, not closed: no end trace, no subscription retirement, no worktree cleanup, and the agent stays on offer. The reading holds because an agent bound to a pane is not waiting for recovery: once a wrapper's `agent.attached` is durable, `Store::attach_agent_pane` takes that session out of the record. An agent brought back while still pending, by `rimz agents resume` or in a seeded tab its birth could not confirm, is therefore an ordinary agent again, and closing its pane ends it. Membership at an abrupt exit means a rebirth parked the agent after this wrapper took its pane.

A deliberate exit records the durable `rimz.agent-ended` trace before any slower cleanup, so that agent stays out of automatic recovery. A parent's message can still [resume its ended child](./subagents.md#follow-ups-and-resume). Two deliberate exits skip the trace: a supervised non-subagent run that exits on run completion, whose run record already carries the outcome, and a subagent held open by `--keep`, whose successful stop stamps its end when it reclaims the pane ([scripting.md](./scripting.md#reclaiming-the-run-pane)). A non-deliberate exit skips both the trace and worktree cleanup, because recovery should come from the sidebar producer's latest live roster instead.

### A startup death is relaunched first

Before any of that, a fresh launch whose provider exits nonzero before its session opened is relaunched in the same pane, up to `[agents] startup-relaunches` times with `[agents] startup-relaunch-wait` before each. One rule, `AgentsConfig::startup_relaunch_wait`, reads that wait (0s through 24h) for the wrapper and for the fresh-launch entry points, which refuse a bad value before any pane opens; it is not checked at deserialize, where the lenient machine load would fall back to the defaults and launch anyway. The sequence, what cancels it, and its assist record are step 2 of [the subagent lifecycle](./subagents.md#the-lifecycle-end-to-end); the rule is `harness::run::startup_relaunch`, and it reads one of two durable facts. A launch with a run record died at startup when that run is still `Pending`. A root launch has no run, so the wrapper reads its launch card: still provisional (a snapshot row keyed by the launch id under the launch name), and the provider exited within 60 seconds of that attempt's spawn. The window is there because a provisional card is normal for a lazily registering provider nobody has prompted, and for any provider whose hooks are missing. One case is accepted: quitting such a provider with a nonzero status, without a signal, inside the window and before its first prompt reads as a startup death, and Ctrl-C during the announced wait is the way out. A resume or a fork is never relaunched: a resume has no provisional card, and a provider may create the forked session before its first hook, so a relaunch could fork twice. The card stays unfailed through the sequence and is marked `Failed` only when the wrapper settles.

### How the wrapper settles

Before any settlement, a surviving wrapper records a non-successful provider exit within 60s of spawn that it did not cause (no run-completion or parent-end stop, no signal to the wrapper) as the best-effort [`provider_startup_exit` diagnostic](../diagnostics.md#event-taxonomy), including launches, resumes, and forks, independently of run or card evidence.

After the trace, the wrapper settles in one of three ways, depending on what the launch asked for.

**Drop to a shell.** A clean exit from a close-pane or worktree pane that is not a supervised run prints one hint and execs the user's shell in that pane, so the pane stays usable and any worktree stays inspectable. The hint is a runnable command rebuilt from the stored identity (`rimz agents forge.reviewer`). It teaches `--resume` when the ended session can be redeemed, meaning a real provider session id whose adapter compiles a resume command for this directory, and a bare relaunch otherwise. Running the resume takes that same pane back over instead of opening a lane tab.

**Reclaim the worktree.** An agent launched with `--worktree-path` triggers worktree cleanup on supervised-run completion or on a deliberate signal or tab-close exit; the cleanup proves the branch's work landed before removing the tree and deleting its branch. A clean interactive quit does *not* reclaim: it drops to the idle shell and leaves reclamation to `rimz gc`. A signal exit starts the cleanup helper with null stdio in its own process group, so it can finish after the closing pane disappears. The helper, its decision table, and the `gc` sweep are [worktrees.md § Who triggers removal](./worktrees.md#who-triggers-removal).

**Close the pane.** A pane closes itself on provider exit when the launch set `close_pane_on_exit`, or when a non-kept subagent's parent has ended. An interactive subagent normally stays alive through its parent's receiving turn before cleanup stops it. The supervised-run side of pane reclamation, including background runs and cancellation, is [scripting.md § Reclaiming the run pane](./scripting.md#reclaiming-the-run-pane).

## See also

- [scripting.md](./scripting.md): supervised `-p` runs: the run record, the wake socket, verify and retry, output formats.
- [subagents.md](./subagents.md): agent-launched children, their parent stamp, and `--keep`.
- [loops.md](./loops.md): scheduled tasks: the task catalog, elder firing, the fire gate ladder, signals and waits, and the assist log.
- [budget.md](./budget.md): dollar caps: the scopes, the ledgers, the verdict, the waiver, and the gate.
- [messaging.md](./messaging.md): how text reaches a pane.
- [worktrees.md](./worktrees.md): the Git worktrees a launch can land in.
- [trust.md](./trust.md): which parts of a launch spec can execute a command, and how a grant is proven.
- [model.md](../agents/model.md): the agent rollup and state machine the harness reads.
- [instances.md](../agents/instances.md): pane binding, launch identity, and session death.

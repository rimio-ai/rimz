# The agent harness

> The entry point for contributors working on the harness. This page maps the whole area, then owns the launch core: spawning a fleet, addressing it, resuming it, and reclaiming what it leaves behind. The other eleven pages in this folder each go deep on one job: supervised runs, subagents, loops, workflows, budgets, hibernation, messaging, the transcript, worktrees, teams, and trust.

## What the harness does

The harness starts, names, drives, and cleans up agents. RimZ drives an interactive agent only through its terminal pane: every such agent is a stock provider CLI in a real pane. So the harness works the way a fast human would. It opens panes, types into them, watches the durable records the agents' hooks write, and closes panes when the work is done. The same machinery serves a human at the keyboard, a shell script, a CI gate, and one agent driving another. Headless loop checks and workflow judges are the one paneless route: they run a provider process directly ([loops.md](./loops.md)).

## The rules that shape it

Four rules explain most of the design. When a piece of the code surprises you, one of these is usually the reason.

**Durable records are the truth.** Panes are latency: they can close, renumber, or wedge. Launch identity, message queues, run outcomes, and schedule history live in the store, so recovery reads state instead of guessing from a multiplexer. Every subsystem here has one durable record at its center, and everything else is an attempt against it.

**One compile target.** Every way of starting agents (an inline layout, a named team, a `-p` run, a resumed cohort, a reborn room) resolves to the same backend-neutral list of pane commands. Zellij and tmux receive identical input, so a feature written once works on both.

**One name for one agent.** A member is reachable by an address, `@handle#channel`, and the renderer that prints a handle is the exact inverse of the parser that reads one. Anything RimZ shows you, you can type back.

**No room-wide service.** Scheduled work, message wakeups, and unattended recovery ride the tick of the room's sidebar host. Roots without an open room can opt into a one-shot OS timer tick, described in [loops.md § The external tick](./loops.md#the-external-tick). An optional [language-server broker](../lsp.md) is a per-server process bounded by agent leases, and schedules nothing.

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

Layout placement and cohort reconciliation belong to the library launcher; resident loops prepare there, perform takeover, then execute that preparation. Supervised launches prepare there too, with their stricter preflight recipe, while their CLI driver retains waits, streaming, verification, retry decisions, joins and cleanup. Both front ends reach one identity-batch, pane-compiler and placement tail. A retry reuses its first preparation, checkout and room without another preflight. Every entry point (a `-p` run, a scheduled loop fire, a resumed cohort, a reborn room) shares resolution, identity, compilation, and the exec wrapper, with a different request on the front.

## Where the code lives

The harness is a product area spanning several source modules, and the twelve pages in this folder are grouped by the job they document.

| Page | Owns | Source |
| --- | --- | --- |
| fleet.md (this page) | Spawn, address, resume, reclaim | [`harness/`](../../../crates/rimz/src/harness), [`address.rs`](../../../crates/rimz/src/address.rs) |
| [scripting.md](./scripting.md) | Supervised `-p` runs | [`store/run.rs`](../../../crates/rimz/src/store/run.rs), [`harness/run.rs`](../../../crates/rimz/src/harness/run.rs), [`run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs), [`cli/supervised/`](../../../crates/rimz/src/cli/supervised) |
| [subagents.md](./subagents.md) | Agents launching supervised children | [`cli/subagents/`](../../../crates/rimz/src/cli/subagents), [`harness/plan.rs`](../../../crates/rimz/src/harness/plan.rs) |
| [loops.md](./loops.md) | Scheduled tasks, signals, waits, and the assist log | [`harness/schedule/`](../../../crates/rimz/src/harness/schedule), [`assist_log.rs`](../../../crates/rimz/src/harness/assist_log.rs) |
| [workflow.md](./workflow.md) | Durable runs through a graph of checks and steps | [`harness/workflow/`](../../../crates/rimz/src/harness/workflow) |
| [budget.md](./budget.md) | Dollar caps and the park they produce | [`harness/budget.rs`](../../../crates/rimz/src/harness/budget.rs), [`cli/budget.rs`](../../../crates/rimz/src/cli/budget.rs) |
| [hibernate.md](./hibernate.md) | Freezing idle agents and waking them | [`harness/hibernate.rs`](../../../crates/rimz/src/harness/hibernate.rs), [`harness/hibernate/`](../../../crates/rimz/src/harness/hibernate) |
| [messaging.md](./messaging.md) | Getting text into a running agent | [`message/`](../../../crates/rimz/src/message) |
| [transcript.md](./transcript.md) | The durable conversation log and asks | [`transcript.rs`](../../../crates/rimz/src/transcript.rs) |
| [worktrees.md](./worktrees.md) | RimZ-owned Git worktrees | [`worktree.rs`](../../../crates/rimz/src/worktree.rs) |
| [teams.md](./teams.md) | Team run archive, memory listing, stage board, flips, resume re-wake, and team reports | [`harness/scratch.rs`](../../../crates/rimz/src/harness/scratch.rs), [`harness/team_stage.rs`](../../../crates/rimz/src/harness/team_stage.rs) |
| [trust.md](./trust.md) | Which parts of a project config may execute | [`trust.rs`](../../../crates/rimz/src/trust.rs) |

Every file below is under `harness/` except the top-level `address.rs`, the petname grammar in `agents/`, and the durable run record in `store/`. Each module's `//!` header is the authority on what it holds.

| File | Owns |
| --- | --- |
| [`launcher/`](../../../crates/rimz/src/launcher) | Layout and supervised preparation and shared execution, placement and child zones, checkout settlement, cohort reconciliation, trust verdicts, and LSP admission. |
| [`spec.rs`](../../../crates/rimz/src/harness/spec.rs) | The layout IR: the inline grammar, team and profile resolution, virtual `<kind>-<mode>` and `<base>-fast` cells, prompt-file path rooting, the prompt leader, and the name-collision rules that keep profile names addressable. |
| [`plan.rs`](../../../crates/rimz/src/harness/plan.rs) | Turning a spec into a launch: effective-config resolution, launch finalization, per-cell launch identities, and fresh or resumed compilation to backend-neutral pane commands. |
| [`launch_plan.rs`](../../../crates/rimz/src/harness/launch_plan.rs) | The exec wrapper's plan: `prepare_exec` makes its launch decisions through `compile`, which decides without writing; `apply` materializes. |
| [`launch.rs`](../../../crates/rimz/src/harness/launch.rs) | The provider process: adapter argv for launch, resume, and fork; the hidden `ExecRequest` wire; launch environment composition; the login-shell wrapper; and preflight. |
| [`prompt_compose.rs`](../../../crates/rimz/src/harness/prompt_compose.rs), [`team_prompt.rs`](../../../crates/rimz/src/harness/team_prompt.rs) | Ordered system-prompt composition, content-addressed runtime artifacts, and adapter replacement argv or environment; the team consensus layer. |
| [`launch_reminders.rs`](../../../crates/rimz/src/harness/launch_reminders.rs) | The one `<system_reminder>` tag: paragraph order and the model fragment. |
| [`launch_context.rs`](../../../crates/rimz/src/harness/launch_context.rs) | A team member's identity sentence, launch-time run state, and channel rule. |
| [`runtime_env.rs`](../../../crates/rimz/src/harness/runtime_env.rs) | The Environment block sampled at prompt submit. |
| [`headless.rs`](../../../crates/rimz/src/harness/headless.rs) | Profile resolution, launch preparation, and execution for headless loop checks and workflow judges. See [loops.md § Guard chains](./loops.md#guard-chains). |
| [`hook_drain.rs`](../../../crates/rimz/src/harness/hook_drain.rs) | The elected hook drainer and the socket leases sandboxed wrappers hold. See [adapter.md § The feed](../agents/adapter.md#the-feed). |
| [`scratch.rs`](../../../crates/rimz/src/harness/scratch.rs), [`board.rs`](../../../crates/rimz/src/harness/board.rs) | Team memory-file scan, the advisory `.rimz/team/blackboard.md` stage parser, and locked board writes. See [teams.md](./teams.md). |
| [`team_notes.rs`](../../../crates/rimz/src/harness/team_notes.rs) | Locked archive-run and checkout-link registration before a fresh launch, a cohort resume, or a restored team tab. See [teams.md](./teams.md#the-run-archive). |
| [`team_stage.rs`](../../../crates/rimz/src/harness/team_stage.rs) | `rimz teams flip`: locked board edits, the durable `team.stage` signal, owner delivery, and hand-off compaction. See [teams.md](./teams.md). |
| [`ancestry.rs`](../../../crates/rimz/src/harness/ancestry.rs) | Durable caller resolution, launch-chain policy, and the shared focus policy. |
| [`subagent_policy.rs`](../../../crates/rimz/src/harness/subagent_policy.rs), [`parent_watch.rs`](../../../crates/rimz/src/harness/parent_watch.rs), [`orphan_sweep.rs`](../../../crates/rimz/src/harness/orphan_sweep.rs) | What a subagent caller may launch, the child's parent watchdog, and the host's backstop when that watchdog fails. See [subagents.md](./subagents.md). |
| [`fleet.rs`](../../../crates/rimz/src/harness/fleet.rs), [`owed.rs`](../../../crates/rimz/src/harness/owed.rs), [`park_notice.rs`](../../../crates/rimz/src/harness/park_notice.rs), [`stall_notice.rs`](../../../crates/rimz/src/harness/stall_notice.rs) | A launcher's fleet settlement, the harness wakes still owed to a session, and the one parent notice per parked or stalled child. See [subagents.md](./subagents.md) and [scripting.md](./scripting.md). |
| [`address.rs`](../../../crates/rimz/src/address.rs) | The address: parsing `@handle#channel`, resolving it against a snapshot, binding a match to a live pane, rendering the canonical handle back, and launch-instance grouping and lineage. |
| [`agents/petname.rs`](../../../crates/rimz/src/agents/petname.rs) | Adjective-noun instance names, name legality, and the deterministic fallback for a record with no stored name. |
| [`resume.rs`](../../../crates/rimz/src/harness/resume.rs) | Resume planning for room rebirth, cohort resume, and lane resume, plus `resolve_posture`, the relaunch posture seam every path shares. |
| [`rebirth.rs`](../../../crates/rimz/src/harness/rebirth.rs) | Two-phase inspection of the previous incarnation of a room, over the shared recovery plan. |
| [`budget.rs`](../../../crates/rimz/src/harness/budget.rs), [`spend_ladder.rs`](../../../crates/rimz/src/harness/spend_ladder.rs) | Dollar caps and their parks ([budget.md](./budget.md)); the provider-evidence spend ladder for launch and redemption ([providers.md](../agents/providers.md)). |
| [`store/run.rs`](../../../crates/rimz/src/store/run.rs), [`run.rs`](../../../crates/rimz/src/harness/run.rs), [`run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs), [`run_timeout.rs`](../../../crates/rimz/src/harness/run_timeout.rs), [`deadline.rs`](../../../crates/rimz/src/harness/deadline.rs) | The durable supervised-run record, its transitions, the waiter that receives its wake, and deadline detection. See [scripting.md](./scripting.md). |
| [`schedule.rs`](../../../crates/rimz/src/harness/schedule.rs), [`schedule/`](../../../crates/rimz/src/harness/schedule) | Loop tasks and their runner. See [loops.md](./loops.md). |
| [`auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs), [`auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs) | Unattended recovery of parked turns and spent windows. See [providers.md § Auto-continue](../agents/providers.md#auto-continue) and [§ Auto-redeem](../agents/providers.md#auto-redeem). |
| [`assist_log.rs`](../../../crates/rimz/src/harness/assist_log.rs) | The assist records that user-benefiting automation appends. See [loops.md § The assist log](./loops.md#the-assist-log). |
| [`idle_compact.rs`](../../../crates/rimz/src/harness/idle_compact.rs), [`cache_keepalive.rs`](../../../crates/rimz/src/harness/cache_keepalive.rs) | The host's idle compaction check and its prompt-cache pings. See [messaging.md § Idle compaction](./messaging.md#idle-compaction) and [§ Prompt-cache keepalive](./messaging.md#prompt-cache-keepalive). |
| [`idle_stop.rs`](../../../crates/rimz/src/harness/idle_stop.rs) | The host's check of pending `stop --when-idle` requests and the decision its helper re-runs. See [loops.md § Recovery the host runs](./loops.md#recovery-the-host-runs). |
| [`auto_gc.rs`](../../../crates/rimz/src/harness/auto_gc.rs), [`tmp_floor.rs`](../../../crates/rimz/src/harness/tmp_floor.rs) | The host's daily gc check, sweep stamp, and read-only temp-floor trigger; machine-wide pressure eviction and doctor's read-only inventory. See [loops.md § Recovery the host runs](./loops.md#recovery-the-host-runs). |

The CLI side lives in [`cli/agents_cmd/`](../../../crates/rimz/src/cli/agents_cmd) (the layout entry, restart, resume, fork, molt, stop, and the hidden `exec` and `run-command` wrappers), [`cli/supervised/`](../../../crates/rimz/src/cli/supervised) (the run driver both `agents -p` and loop fires call), and [`cli/loop_cmd/`](../../../crates/rimz/src/cli/loop_cmd). Those handlers parse flags and render, delegating shared launch admission and placement to the launcher; the harness keeps provider and durable-state rules. The workflow host calls the launcher directly for agent and command layouts with turn or session leaders, and every cell is reclaimed when the attempt ends.

Read this page first, then the page for whatever you are changing. For a second read across the whole area, [messaging.md](./messaging.md) is the most instructive: the durable-record rule has the most consequences there, and the delivery pipeline touches nearly everything else.

### The state machines

Six state machines carry most of the area's behaviour. Each has one owning type and one owning section.

| Machine | Type | Documented in |
| --- | --- | --- |
| Message lifecycle | `MessageStatus` | [messaging.md § Status lifecycle](./messaging.md#status-lifecycle) |
| Supervised run | `RunStatus`, whose `exit_code` is the caller contract | [scripting.md § Status and exit codes](./scripting.md#status-and-exit-codes) |
| Loop task timing and outcome | `TaskTiming`, `LoopRunResult` | [loops.md § Schedule shapes](./loops.md#schedule-shapes), [§ History, strikes, and arming](./loops.md#history-strikes-and-arming) |
| Dollar budget ledger | `BudgetVerdict` | [budget.md § The verdict](./budget.md#the-verdict) |
| Worktree removal | `WorktreeState::keep_reasons` | [worktrees.md § The assessment](./worktrees.md#the-assessment) |
| Project trust | `TrustState` | [trust.md § States](./trust.md#states) |

Two decision trees behave like state machines without an enum: the [deliberate-exit classification](#reclaiming-a-pane) that decides what happens when an agent process ends, and the [park class](../agents/providers.md#auto-continue) that decides when a stopped agent may resume itself.

## Launching a fleet

### The layout IR

`rimz agents <spec>` resolves either a named `[agents.teams]` entry or an inline spec, and both compile to the same backend-neutral panes. In the inline grammar, commas split columns, plus signs tile rows within a column, and slashes stack rows within a column.

```text
claude,codex+term          → Claude left; Codex tiled over a shell right
claude/codex/term          → two agents plus a shell in one stack
vim,htop+zsh               → raw command panes
claude-auto,codex-yolo     → agent cells with adapter-owned permission posture
claude:planner,codex:coder → agent cells with ad-hoc `@planner` and `@coder` handles
```

Each cell is one of: the built-in `term`, a registered agent kind, a virtual `<kind>-<mode>` variant, a `<base>-fast` cell, a configured profile, a configured command, or an executable found on PATH ([configuration.md](../../guide/configuration.md#agent-profiles-commands-and-teams)). Configured cells and built-ins resolve before PATH. A `<base>-fast` cell whose kind has no fast mode warns and launches without it, never falling through to PATH (`crates/rimz/src/harness/spec.rs::fast_suffix_cell`). An agent cell may carry an ad-hoc role as `<cell>:<role>`. Inline roles follow team-role naming and addressing rules, must be unique within the spec, and apply only to agent cells.

A named team is an ordered role list. It opens as one column per role unless it declares its own `layout`, which uses the same row and column grammar. A team layout places each declared role exactly once and resolves declared role names before other cells; any other cell (a roleless agent, `term`, a command, a PATH executable) parses as it would inline (`crates/rimz/src/harness/spec.rs::compile_team_layout`). A role cell in a team layout takes no `:role` suffix. `<team>.<role>` launches one declared role with its team identity, placed like any single-agent launch.

Stacks are presentation only. Both backends keep one member expanded and open a member when it receives focus. Zellij stacks natively; tmux uses pane marks and server hooks ([multiplexers.md](../multiplexers.md#collapsed-stacks)).

### From spec to panes

The ordinary launch compile path is the seam the whole harness hangs off, and it runs in a fixed order.

1. **Resolve.** `plan::resolve_launch` reads the effective config (machine profiles and teams, merged with trust-filtered project config) and produces a `LayoutSpec` whose agent cells carry their profile-declared launch params and typed system-prompt file paths. A launch-wide `--agent` override resolves as a replacement base, which `spec::rebase_onto` merges without changing the cell's profile or role identity (table below).
2. **Finalize.** `plan::finalize_launch_layout` applies the launch-wide choices to each agent cell in this order: isolation, permission posture, the preset (typed prompt-file override, preset argv, CLI model and effort), validation, passthrough argv, budget, then preset reconciliation and the adapter default model. A second pass applies supervised turn limits over all cells. The finer rules of this step follow the list.
3. **Resolve ancestry.** The launcher reads its stable launch identity and resolves the live durable caller. A caller that is itself a launched child is refused for every launch kind. A launch from a human shell carries no ancestry stamp. An agent caller's agents or teams launch gets a parentless peer-generation stamp and is checked against `[agents] max-chain-length`; its subagents launch gets a direct-parent stamp. A stale caller, an excessive chain, or a child caller refuses here, before provider preflight, worktree creation, store append, or mux action. A subagents launch then re-anchors its workspace at the caller's recorded checkout, and a vanished checkout refuses at the same point ([subagents.md](./subagents.md#the-child-works-where-its-parent-works)).
4. **Identify.** Each cell becomes a launch request with a name, a channel, its cohort stamps, and its optional ancestry stamp, and the store mints provisional rows before any pane opens.
5. **Compile.** Each cell becomes a `LayoutPanes` entry. An agent cell compiles to the exec-wrapper argv, a command cell to its raw argv, and an empty argv reserves the pane for the user's shell.

Validation in step 2 checks profile prompt files and the adapter's replacement support, so a moved prompt file or an adapter with no replacement mechanism fails at the entry point instead of inside a half-built tab. `compile_layout_panes` validates again.

Presets carry the model, effort, and `auto-compact` a profile declares. They render through typed presets and reconcile against matching raw `args`, and `auto-compact` has no CLI override. Early rendering silently omits unsupported auto-compact and fast; the cell retains them until finalization warns once per skipped field and clears it before writing the launch record. Model and effort rendering stays strict. Injected machine compaction defaults ride separately from declared windows and apply only where the final engine supports them. A model set only in `args` is adopted as the cell's model identity. The adapter default model is stamped only when nothing selected a model.

Permission posture has one replacement rule. An explicit `--ask` or `--yolo` replaces a declared profile or virtual-cell mode: `LaunchSpec::strip_permission_args` removes every exact rendered permission vector of the adapter (longest first), then the chosen vector is appended and the mode stamped. A caller default (`auto` for a supervised `-p` without a flag) only fills an unset mode. Passthrough argv arrives after this replacement, so the strip never touches it.

A subagent's permission mode is capped before finalization. For a subagent with a caller, `plan_supervised_launch` checks the single-cell shape and the parent's recorded ceiling. An explicit mode above that ceiling refuses, while a higher default becomes `PermissionModeChoice::Explicit(lowered)`.

Restart, fork, and flat resume stop after step 1 and resolve posture through the seam under [Posture](#posture). Explicit single-session resume applies any launch overrides through the same rules as cohort resume. Isolation alone leaves the replayed record untouched, including rows that carry no record. Cohort resume matches the layout without `--agent` or `--tier`, then restores each member's record and applies overrides before preflight and pane compilation.

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

A trailing launch prompt attaches to exactly one agent identity: a named team's configured `leader` role, its first declared role by default, or otherwise the first unambiguous agent cell.

Cohort stamps let the sidebar keep cards in definition order and let resume match a cohort later (`crates/rimz/src/harness/plan.rs::launch_identity_requests`). A team member carries its team and, for a role cell, its role's index in the team's role list as `launch_ordinal`. An inline layout with two or more agent cells mints one `launch_group` for the layout and stamps each cell's index as its ordinal. Both travel to the pane as `RIMZ_LAUNCH_GROUP` and `RIMZ_LAUNCH_ORDINAL`. One private key table in `harness/launch.rs` maps each env-backed launch parameter to its `RIMZ_*` key: `exec_identity_env` encodes the pane env through it, and the lifecycle hook decodes a root session's unset parameters back through `fill_launch_identity_env`.

`launch::compile_agent_process` is the provider-process compiler. It selects launch, resume, or fork argv from the request, composes trusted project, adapter, and RimZ identity environment in that order, applies the login-shell wrapper, and retains the raw provider argv for PATH preflight. The exec wrapper supplies any materialized prompt argv and environment at the final provider boundary.

### Every path that builds a launch layout

A rule that must hold for every launch (a durable record, a preflight, a stamp) has to hold on each of these paths. Paths rebuilding from profiles or seeds already in memory need the same checks as entry-point resolution, so they are the ones a new rule misses. The last column is where each path writes its `tier_fallback` assist ([loops.md § The assist log](./loops.md#the-assist-log)).

| Path | Builds its layout in | Records a tier fallback |
| --- | --- | --- |
| Fresh launch, `rimz agents <spec>` | `crates/rimz/src/launcher/resolve.rs::resolve_finalized_layout` (a named team through `crates/rimz/src/harness/spec.rs::resolve_team`), then `crates/rimz/src/harness/plan.rs::compile_layout_panes` in `crates/rimz/src/launcher/execute.rs::execute` | from the `on_launched` hook of `crates/rimz/src/launcher/placement.rs::execute`, after the tab or pane opens |
| Same-pane exec, the in-place branch of a fresh or cohort-resume launch | the same compiled panes, handed to `exec_wrapper_in_place` | from the same hook, just before the exec, since a successful exec never returns |
| Cohort resume, `rimz agents <spec> --continue` | `crates/rimz/src/harness/resume.rs::restore_routed_cells` over the finalized layout, then `compile_layout_panes` in `crates/rimz/src/launcher/mod.rs::launch_resume_layout` | from the `on_launched` hook, as for a fresh launch |
| Supervised run, `rimz subagents` and `-p` | the same `crates/rimz/src/harness/plan.rs::compile_layout_panes` path in `crates/rimz/src/launcher/execute.rs::open_fresh` | after placement confirms the pane open |
| Molt successor | `crates/rimz/src/cli/agents_cmd/molt.rs::run_molt`, with fresh routing through `crates/rimz/src/cli/agents_cmd/molt.rs::replacement_posture` only for `--agent` | after successor placement, only for a fresh move with `--agent`; replayed stamps record nothing new |
| Team restore, in rebirth and lane resume | `crates/rimz/src/harness/resume.rs::plan_team_restore_tabs` (`resolve_team`, then `restore_routed_cells`), compiled by `crates/rimz/src/harness/resume.rs::materialize_team_restore_tab` | lane resume: `crates/rimz/src/cli/agents_cmd/resume.rs::resume_lane` after each `open_tab` succeeds; failure closes that tab's batch and every unopened tab's batch without assists. Rebirth: `crates/rimz/src/cli/room/mod.rs::birth_managed_room` after birth succeeds, for each team tab the backend confirmed open; a tab that did not open fails its batch in `crates/rimz/src/harness/rebirth.rs::SeededRecovery::confirm` |

A fresh tier walk records every entry passed ahead of the pick: `unbuildable` holds the family's full build error, and `skipped` holds usage reasons. Build passes are recorded even with usage routing off, and when every available family is spent and the walk retries its first buildable entry. Exclusions ordered after the pick are static validation information, not moves. Any move writes one `tier_fallback` assist at the placement boundary in the table.

`finalize_launch_layout` returns one `LaunchFinalizeWarning::TierMoved` per moved cell, including every pass and full build error. Interactive and supervised fresh launches print it once; headless judges keep it in their record. Cohort matching also finalizes a freshly routed layout before restoring recorded posture, so its launch printer suppresses these move warnings for `--continue` and an exact-id `--resume <id>`. A bare `--resume` opens the provider's picker on the freshly routed layout and prints them. Lane restore and rebirth print no finalize warnings. Receipts and agent reports read the preserved stamp independently.

The login health check is one such rule, and it also covers the relaunch paths that build no layout. Each site calls `crates/rimz/src/agents/login.rs::ProviderLogin::health` on the login it launches under, `default` included, with the ambient environment. A catalog-issued default drops an exported declared account home itself, so a caller inside a named-account pane judges the home the new pane resolves. The call follows the sandbox and compile preflights and precedes the first store write and mux action. A home that is not a directory or missing hooks refuse with the `BirthLoginErr` line and change nothing. Untrusted hooks that the provider can trust from its own prompt pass through `crates/rimz/src/launcher/trust.rs::preflight_hooks`: a human launch warns once per account and continues, and an unattended launch (agent-started, team, loop, or subagent resume) refuses. The sites are:

- `crates/rimz/src/launcher/mod.rs::check_cell` (fresh launch and cohort resume, per cell)
- `crates/rimz/src/cli/agents_cmd/restart.rs::restart_resolved` (restart, and `rimz teams restart` per member)
- `crates/rimz/src/cli/agents_cmd/resume.rs::resume_lane` (per planned kind, on the room's login for it)
- `crates/rimz/src/cli/agents_cmd/resume.rs::resume_selected` (single-session resume, from both `resume_agent` and `resume_provider`)
- `crates/rimz/src/cli/agents_cmd/fork.rs::run_fork`
- `crates/rimz/src/cli/agents_cmd/molt.rs::run_molt` (the successor, always judged agent-started)
- `crates/rimz/src/cli/subagents/resume.rs::resume_resolved`

Rebirth runs the same check once per planned kind in `crates/rimz/src/harness/rebirth.rs::plan_recovery`, without `preflight_hooks`: a failing kind's sessions are skipped as `account unavailable`, with the login error as the reason. Untrusted hooks the user can trust get a reason naming the trust command to run from their own shell.

A supervised run keeps its own stricter gate, `crates/rimz/src/launcher/supervised.rs::preflight_agent`. `rimz agents explain` runs none of this, so it still renders a plan for an agent whose hooks are gone. Room birth and account switching call `ProviderLogin::preflight`, which skips `default`. A process plugin declares no config home, so `health` passes it. Workflow layouts keep that recipe on the leader; companions use the layout recipe with the loop's trust verdicts and workflow-named refusals.

Folder trust follows login health at the same launch and relaunch sites, and `crates/rimz/src/launcher/trust.rs::preflight` decides by who launched (`launcher::trust::Unattended`). A fresh launch takes its class from a loop fire first, then an agent caller, then a team; a launch with none of these is a human launch.

| Launch | Undecided folder | Restricted folder (Codex's saved-untrusted decision) |
| --- | --- | --- |
| Human agent launch or relaunch | warn | warn |
| Agent-started agent or team launch (fresh) | refuse | refuse |
| Human team command, resident loop | warn | refuse |
| Subagent resume | refuse | refuse |

The directories checked differ by path. A fresh launch checks the pane's directory and its own main-repository root (no repository key outside Git), and a not-yet-created worktree uses its intended main root. Cohort resume checks its planned cwd with the caller's verdict, deduplicating per login and directory. For an agent without a recorded checkout, restart, single and lane resume, fork, and molt check the agent's recorded directory and its main-repository root the same way. Supervised launches use the stricter supervised directory preflight, which also refuses the login's `$HOME` for every kind. The headless loop judge passes Restricted but refuses `$HOME` too.

A resident loop checks its resolved spec at the target cwd before it opens the agent context or takes over occupants. Idempotent room preparation may precede that check, but no takeover or agent launch may. A refusal is recorded as a failed fire. `rimz start` prints its Restricted warning separately from its grant offer and decline record.

### Molt

`rimz agents molt` is self-succession: the agent hands its seat to one successor, launched on a note it wrote. `crates/rimz/src/cli/agents_cmd/molt.rs::run_molt` replays recorded posture, or re-bases the same role onto `--agent`, reserves one successor, and opens an unfocused tab on the note without a peer run for that turn. In-session `/model` changes are not recorded, so the successor does not carry them.

The successor inherits the seat instead of starting a new chain. It copies the predecessor's ancestry, generation, team seat, ordinal, and channel without calling the ancestry resolver or consuming the chain cap. Its tab title comes from the shared `default_tab_title` rule over the successor's restart cell, and its pane name from its profile, else its provider, so both match a fresh launch of the same seat.

Replaying a tier stamp records no tier fallback; `--agent` routes fresh and records any new move after placement. After placement, molt rekeys an open leader run, arms the predecessor's five-minute idle stop, and records the assist. A placement failure marks the reservation `Failed` and runs none of those follow-ups.

### The exec wrapper

Every agent pane runs the hidden `rimz agents exec <kind>` wrapper, which in turn runs the agent. Backends never resolve agent kinds or worktrees; the wrapper does.

#### The request envelope

The wrapper's command line carries two visible arguments, the kind and an optional `--worktree-path`, which together form the process-classification envelope that pane discovery reads. Everything else travels in one hidden compact JSON envelope: the typed action (`Launch`, `Pick`, `Resume`, or `Fork`), the identity, the launch prompt's path, typed system-prompt source paths, skills, the subagent flag, the run id, pane-lifetime flags, provider account binding state, and provider argv.

The launch prompt travels as a file. Every launch prompt is materialized as a content-addressed `prompt/task.<digest>.md` beneath the private runtime root, beside the system-prompt artifacts. The wrapper validates the request and cross-checks the visible kind and worktree before any artifact I/O, then establishes launch and run cleanup context and reads the prompt into the in-memory `ExecRequest`.

The wrapper is where the host judges a launch from a scoped pane. Before planning, it refuses a request whose profile failed to load on the host, with that definition's `path: message`, and fails the provisional launch and the supervised run. A caller in a scoped pane skips the skill-listing check, so this refusal is the first host-side one. A missing or unreadable prompt artifact fails the same way, and the failed run wakes its waiter instead of leaving the parent waiting.

Keeping the prompt body out of the pane command keeps the multiplexer command small whatever the prompt's size. The provider still receives the full prompt as one argv element, so RimZ enforces a separate 120 KiB (122880-byte) argv limit during launch preflight, before launch and supervised-run records are created, and again in the wrapper. A larger prompt fails with the limit named and advice to move detail into a file the agent reads. The runtime artifacts are rename-atomic caches outside the store: content addressing makes reentry idempotent, and runtime GC removes stale artifacts by age.

#### The launch plan

`launch_plan::prepare_exec` is the wrapper's one entry into these decisions. From the decoded envelope it runs the definition refusal, resolves isolation, preflights the scoped view, materializes the prompt, derives paths and the login, reads the recorded session for a resume or fork, resolves the model, reads the parent's row for a child, and compiles. The room's rows come through a lazy reader called only for a resume, a fork, or a child.

`prepare_exec` prints nothing and makes one write: its `link_account` callback reconciles the account home's links once the login resolves, before compilation, because compilation resolves the skill root through those links. Its `ExecPreparation` returns the link and model warnings and any alias move even when a later step fails, so the wrapper prints them and appends the alias assist either way. The wrapper keeps `apply`, the chdir, the version floor, pane stamps, and the process.

`launch_plan::compile` makes every decision and writes nothing. It takes the request, paths, effective configuration, the resolved isolation, the preflighted bubblewrap path, and the ambient environment. It reads prompt sources and skill metadata, builds reminders and provider argv and env, and plans the scoped view. The resulting `LaunchPlan` holds the request, cwd, login, prompt plan, process stage, reminder channel, optional `SandboxPlan`, and warnings. The rendered reminder lives on `plan.process().reminder`, even when no provider channel can deliver it.

`launch_plan::apply` is the write boundary. It ensures runtime directories and writes the composed system-prompt and reentry prompt artifacts. It creates the launch's temp unit and the room's `shared/` on every launch, and with a scoped-view plan applies the planned copies (skills and system SSH config). Cwd changes, identity writes, pane operations, and provider execution stay outside both `compile` and `apply`. The task-envelope planner is private to `launch`, and its deferred artifact writer is harness-only; `refactor-target.toml` admits these module dependencies and public surfaces explicitly.

[`rimz agents explain`](../../reference/cli/agents.md#explain-a-launch) renders the same compiled plan and never calls `apply`. Its profile branch shares launch resolution, finalization, and `launch_identity_requests`, but leaves the name and launch id unminted. A `LaunchAncestryError` from the caller's ancestry becomes a warning and the plan compiles without ancestry, since explain launches nothing; the launch path itself refuses. Its `@handle` branch uses restart action selection and current-config `resolve_posture`, reports degradation as warnings, and refuses overrides. Both branches require successful scoped-view and skill preflight. The plan it shows is computed from the invoking process's environment and the current config, so it can differ from what the target pane received: hidden provider instructions and original passthrough arguments are absent.

#### System prompt composition

A profile's system prompt is a base file plus ordered fragments. For a staged team role with a resolved base, composition is base → profile-chain fragments → role fragments → consensus → team-level `append-system-prompt-files`. `team_prompt.rs` owns the embedded `team_consensus.md` and the layer's derivation, and `spec` attaches it once per role cell at team compile, after rebasing. `consensus-file` replaces the embedded text, and team paths resolve relative to the declaring config file. A CLI `--append-system-prompt-file` override replaces only the profile and role fragments, and leaves the team layer in place.

A team is staged when `stages` is nonempty or any role has `owns`. Unstaged teams and roles without a base get no default layer. An explicit `consensus-file` or a nonempty team `append-system-prompt-files` requires a staged team and a base on every role, and preparation refuses otherwise. `prompt_compose` normalizes trailing newlines and joins pieces with one blank line and one trailing newline. With fragments or a team layer, path adapters receive a content-addressed `prompt/sys.<digest>.md` beneath the private runtime root; with neither, they receive the user's resolved base path directly.

| Adapter | Channel |
| --- | --- |
| Claude, Codex | path in argv |
| Qwen | path in `QWEN_SYSTEM_MD` |
| Pi | text in `--system-prompt` |

The wrapper removes matching raw replacement argv before adding the materialized value, so the typed setting wins on fresh launch, restart, fork, resume, and rebirth. The artifact is regenerated idempotently on every one of those paths.

#### Launch reminders

What RimZ tells the agent about itself arrives as one `<system_reminder>` tag, delivered through the adapter's append-system-text channel. Claude, Codex, Qwen, Droid, Grok, Qoder, and MiniMax take it in a native argv flag or config key. Pi, OpenCode, and OMP take it as `RIMZ_LAUNCH_REMINDERS`, which RimZ's in-process extension appends to the root session's system prompt. Every other provider gets only the child no-delegation body, through a user-prompt fallback.

The wrapper collects the parts as a `LaunchReminders` value, and [`launch_reminders.rs`](../../../crates/rimz/src/harness/launch_reminders.rs) renders identity followed by `###` sections in a fixed order. Environment always carries the temp lines, so every launch carries the tag.

| Order | Content | Present when |
| --- | --- | --- |
| 1 | Team heading, identity, grouped pipeline and channel rule; otherwise a standalone model line | a non-child team member with a role; otherwise the model line, when the launch knows its model and `model-reminder` is on |
| 2 | Loop: what launched the agent, how long it lives, and who is watching ([loops.md](./loops.md#the-loop-reminder)); Judge replaces the heading for a headless request | a loop fire's prompt leader (resident), each attempt of a loop's single run, or a headless judge carrying a reminder body; never a restart or resume |
| 3 | Environment: cwd, optional `worktree` bullet naming its recorded base branch and primary checkout, shell-kind bullet, optional `lsp` bullet naming the live language servers and `Skill(rimz-lsp)`, the `tmp` and `shared` bullets ([sandbox.md](../sandbox.md#launch-reminder)), then the team's memory-file listing unless the launch gets it [at prompt submit](#environment-at-prompt-submit) | every compiled launch, since it always carries the temp lines; team context always includes cwd, worktree follows cwd only in a linked checkout, shell requires `env-reminder`, and lsp requires live servers and a callable `rimz-lsp` |
| 4 | Subagents: catalog (the disabled body when empty), or the child's fixed assignment-and-report body | a non-child, non-headless request with a catalog and a callable `rimz-subagents`; every child request, whatever its skills |
| 5 | Skills: invoke a skill whose description matches the work | the launch's profile has no `skills` list, or the list names any `rimz-*` skill, children and judges included |

Callable means the launch's `skills` list is absent or contains that exact name. Installation is not consulted. `config::skill_is_callable` shares this rule with native LSP denial and `rimz agents validate`.

The inputs come from configuration. For a non-child, non-headless request the wrapper derives the profile's allowed subagent catalog from effective project and machine configuration. Headless judges get no catalog and keep native delegation ([policy](./subagents.md#native-delegation-policy)). For a team member it resolves the effective team definition: worktree, role and team handles, channel, leader, every seat with its owned stages, session posture, and the declared scratch entries present at launch. A failed effective-config load warns and falls back to default reminders, which keep the model line. The temp lines come from the compiled plan, so they also survive that fallback.

`model-reminder` is read from the launched profile itself, by the exact `LaunchParams.profile` name: in `[subagents.profiles]` for a child and `[agents.profiles]` otherwise, with no walk up the `agent = …` chain. Unset, absent from config, or no profile at all means on. Because the lookup is by name, restart, fork, resume, and rebirth reproduce the same answer.

The model line appears only where no Team paragraph does. A member with a Team paragraph never shows a model name, and `model-reminder` has no effect on it. A roleless team cell, or a member whose team definition is missing at launch, gets no Team paragraph, so it can get the model line. The line opens with the role or else the profile (`You are @planner, running on Opus 4.8.`, or `You are running on …` when the launch carries neither), unless `model-reminder = false`. A launch that knows no model gets no line instead of a guess at the provider default.

The Team section (`launch_context.rs`) gives the member's identity, with a resumed or forked session noted, then the grouped pipeline. Fresh sessions get no session sentence, and the worktree path appears only as Environment's cwd. A non-leader seat on a team that declares its leader then gets the channel rule: no user reads its pane, and user-bound questions go to the leader. The leader's own channel rule is in its prompt. Board-opening and finished-run decisions live in the consensus, outside the reminder.

The pipeline line uses the team's declared `stages`, or the stages its roles own in role order. Consecutive stages with the same owner group together: `Pipeline: Explore, [Plan] (you) → Implement (@coder) → Reflect (you) → Done`. The current board stage uses the bracket notation of `team_stage::stage_strip`, including `[Done]`. Non-adjacent stages of one owner form separate groups. Seats owning no stage appear as `Also on the team: @scout.`; a team without stages gets `Members: @planner, @coder (you).` instead of a pipeline.

Environment describes the launch as compiled, never sampled git state. `launch_plan::compile` switches its bullets on, and the cwd bullet names the provider cwd, which can differ from the compiler's current directory. The cwd, worktree values, and shell are escaped independently to preserve the one-tag boundary. Every exec, including children, resume, fork, restart, and rebirth, probes afresh. `[agents] env-reminder` defaults to true; a trusted project's value overrides the machine value, and an unavailable effective config leaves it on.

The worktree bullet comes from records written at creation. A linked worktree gets `- worktree: branched from <base>; primary checkout at <path>` directly after cwd, or `- worktree: primary checkout at <path>` without a recorded base. [`worktree.rs`](../../../crates/rimz/src/worktree.rs)'s `linked_worktree` walks up from the cwd to the nearest directory holding `.git`, so a launch below the worktree root resolves like the root. It reads that checkout's `.git` metadata for the primary checkout and the RimZ marker for the base, without spawning git. A primary checkout, a directory outside git, or unreadable git metadata gets no bullet, and an unreadable marker omits only the base. The primary path is the same at every isolation level.

The shell bullet names the shell the process compiler resolved for the wrapper: the machine's `[agents] shell` when set, else `proc::user_shell()` at compile time. It renders as its kind, the path's file name (`zsh`, `bash`), and `None` omits the bullet; `explain` names the shell the launch would run under. With `[agents] shell` set, `SHELL` and `CLAUDE_CODE_SHELL` name it too, so a provider that reads either runs its tools there (Claude only for bash or zsh, per [its tool shell rule](../../externals/agent-adapter/claude-reference.md#tool-shell)). Without it, the bullet does not promise which shell the provider's tools use.

Environment ends with the [team memory-file listing](./teams.md#the-memory-listing), using the fixed archive-memory pattern and the bare names of the files it matches. It carries facts only: no board sentence, snapshot warning, or instructions to read or create files. A launch that gets the listing at prompt submit carries none here, so the listing is never in both places. Git state is absent because launch and the agent's first read can be far apart; it arrives [at prompt submit](#environment-at-prompt-submit) where the adapter can carry it, and the agent runs git itself otherwise.

#### Environment at prompt submit

A listing or a branch sampled at launch can predate the notes an earlier stage owner is still writing, so some launches get those facts with each prompt instead. `launch_plan::compile` decides once per launch, and delivers at prompt submit when all three hold:

- the machine's `[agents] runtime-env` is on (the default, and the answer when no effective config loads)
- the adapter declares `prompt_context` (Claude and Codex)
- the provider's RimZ hooks are installed and trusted under the launch's login environment (`agents::preflight_hooks`), since the hook carries the block

The process compiler then sets `RIMZ_RUNTIME_ENV=1` and the reminder drops the listing. Every other launch clears the variable, so a nested launch never inherits it, and keeps the listing in its reminder. A launch whose hooks are missing or untrusted is unstamped without a warning, since `rimz start` and `rimz doctor` already report them. Hooks removed, or trust revoked, after launch lose the listing until the next launch. The key is machine-only: a project layer cannot set it and it is outside the trust hash.

The prompt-submit hook reads the switch from its own environment, then the provider process's, and attaches for a root agent's prompt only: never a native child's, a side conversation's, or a daemon-owned hook's. [`runtime_env.rs`](../../../crates/rimz/src/harness/runtime_env.rs) fires on the first prompt of each conversation in each provider process, and again on every prompt that carries a delivered stage notice. It claims the first prompt by exclusively creating a marker under the runtime `live/runtime-env/` directory, keyed by process identity and session id. The claim is attempted on every qualifying prompt and refreshed while the conversation keeps prompting. `rimz gc` reaps a stale marker with the other live sidecars, and a marker that cannot be written fires again.

The block is one reminder under `### Environment`, `Sampled as this prompt was submitted.`. For a staged team seat it carries the `$ ls` listing exactly as the launch reminder renders it, with the same bare names. Then one fence holds `git status --short --branch` and `git log -5 --oneline`, both run with `--no-optional-locks` under one two-second deadline. The git fence appears only when both commands succeed, so a directory outside git, a repository with no commit, or a slow repository gets none. With nothing to show the hook stays silent.

Both parts are sampled at the hook's resolved worktree root: the Git top level of the checkout the hook runs in, so a seat in a linked worktree gets that worktree's root. Outside Git it is the nearest `.rimz` marker directory, else the directory the hook runs in. The payload's `cwd` is never the root, because it follows the agent's shell while the team's patterns stay anchored at the checkout root.

The block has two caps. Each sampled git line is escaped like every other reminder text and cut at 200 characters after escaping. The whole block has a 4,000-byte budget (`BLOCK_CAP`), and status lines that do not fit after the heading, listing, and fence framing collapse into one `(N more lines)` line. The listing itself is uncapped, as it is at launch. The block is context for the model: the recorded prompt and `rimz agents logs` keep the text the user typed.

#### Resident wrapper

The wrapper runs the agent in the pane, inheriting the pane's TTY. It launches through the launch shell's startup path (the machine's `[agents] shell`, else the user's shell) when that shell and `/usr/bin/env` are available, and spawns the provider command otherwise. A configured `[agents] shell` that cannot run refuses the launch instead.

Every interactive launch keeps the wrapper as the provider's parent, including in-place launches, in-place forks, and launches with `[agents] startup-relaunches = 0`. The wrapper stamps the session ended and, on a non-run, non-lingering exit that was not abrupt, prints the resume or relaunch hint to stderr, except for an [unbound picker](#single-session-resume). A solo session's hint names its durable pet name; team members get their team-role resume command. In-place fresh launches, resumes, and forks return to the shell beneath the wrapper with the provider's exit code, or 1 when a signal killed it. Resume argv sets `close_pane_on_exit = !in_place`, so an in-place resume never nests a new shell. A close-on-exit pane or worktree launch instead replaces the wrapper with an idle shell for [Reclaiming a pane](#reclaiming-a-pane). The wrapper is also the attach point for [supervised runs](./scripting.md) and [startup relaunches](#a-startup-death-is-relaunched-first).

On Unix every surviving wrapper replaces its startup image after spawning and binding the provider. The same PID re-execs the binary with its original argv plus hidden `--supervise`, reading a versioned, launch-and-PID-specific handoff file in the launch's temp unit. The handoff carries the resolved workspace, so the parked image re-resolves nothing and spawns no helper before supervising. Helper children do not affect the [hosted-CLI walk](../agents/instances.md#recognizing-a-hosted-cli), which stops at the wrapper. Off Unix, supervision stays in process.

The parked image is thin and never folds the store. It reads run records and follows parent-launch frames, and delegates strand repair, receipt checks, final parent confirmation, and card evidence to hidden helper children. Retry stays in the thin image, which restores the saved terminal modes before the next provider spawn. Startup evidence drains queued hooks before its durable read. The handoff file is removed at settlement. An abandoned handoff remains until its whole temp unit is collected, and a killed child wrapper's handoff can remain in its live parent's shared unit.

A scoped launch always keeps a host-side wrapper, resumes and forks included. Before provider startup it waits for the host hook drainer to answer its quiet socket lease. That initial lease survives the image handoff until the parked image's own lease is answered, so the drainer stays leased throughout; the parked image then closes the inherited fd and holds its own lease for the provider's lifetime. The lease holds a path-only store handle, not a fold. A scoped launch also carries one inherited inotify instance into the park. A small host-side guard thread watches for protected-entry replacements and starts only a transient [repair helper](../sandbox.md#live-protected-bind-repair), and every startup retry re-targets the same instance.

The parked wrapper checks its lease every 200 ms and never prints reconnect outcomes into the pane. `crates/rimz/src/harness/drainer_outage.rs::Tracker` retains one gap across startup relaunches. A gap starts at the loop-top instant of the first check that finds loss, before its reconnect attempt; threshold and length judgments use a fresh instant after the attempt. A gap closed under 10 seconds leaves one contained diagnostic. Reaching 10 seconds writes an investigative warning, and recovery writes another without downgrading it ([diagnostic kinds](../diagnostics.md#hook-drain-evidence)). If the reconnecting attempt first reaches the threshold, only the recovered warning is written. Provider exit during an outage writes no recovery.

Every gap reaching 10 seconds publishes a `disk::lease` record at `lanes/hook-drainer-outages/<pid>` in the room's runtime lanes class, including a gap whose reconnecting check first reaches the threshold. Its JSON carries version `v: 1`, `agent_kind`, optional `agent_name`, and wall-clock `since_ms` converted at publication. The wrapper keeps it Held until the first lease check at or after the recovering check's post-attempt instant plus 3 seconds, then releases and removes it. Tracker drop releases it immediately, and process death frees it in the kernel. A new crossing replaces any pending recovery hold with the new gap's start. `crates/rimz/src/harness/drainer_outage.rs::read` returns only known-version Held records, ordered by start; missing, free, malformed, unknown-version and timed-out probes are skipped without failing a sidebar fold. The sidebar reads this runtime fact, never the diagnostic log. Wrappers parked on an older build retain their old reconnect output until their agents restart; the park wire and lease handoff are unchanged.

The 3-second recovery hold lets an on-screen view observe an outage that recovers between folds. The active line therefore stays after real recovery, and the lingering line's duration can include the hold and the next fetch cycle; doctor retains the exact gap. A visible view can still miss the alert if the provider exits within 3 seconds of recovery, cutting the hold short, or if a fold takes longer than about 2 seconds.

Room birth also carries one adapter-enrichment environment map through the mux seam, so a stock agent typed directly into an ordinary work shell inherits the room baseline. RimZ-managed launches apply their adapter `launch_env` after it. A process or shell already running keeps the environment it started with, and rebirth is the parity boundary on both backends.

The environment a RimZ-started process gets depends on its route, so a claim about which processes can carry a variable starts from this table:

| Route | Environment |
| --- | --- |
| Agent pane the harness opens | The multiplexer session's baseline, the room identity pin from `crates/rimz/src/room/mod.rs::pane_identity_env`, and the launch's own variables. Never the calling process's environment. |
| In-place launch | The calling process's environment, with the launch's variables over it: `crates/rimz/src/launcher/placement.rs::exec_wrapper_in_place` execs the wrapper in the caller's process without clearing it. |
| Headless loop check or workflow judge | The caller's whole environment minus `RIMZ_AGENT_*` and `RIMZ_CHANNEL`, plus the loop marker and the workspace pin (`crates/rimz/src/harness/headless.rs::AdmittedLaunch::execute_hosted`; per route in [loops.md](./loops.md)). A judge started inside a scoped view therefore carries that view's variables. |
| Hook apply child of the host drainer | Only the environment recorded in the hook frame, on a cleared environment (`crates/rimz/src/cli/hooks.rs::process_in_child`). |

### Placement

A layout lands in a new tab, a split of the current tab, or, for a single non-worktree cell, the pane the user is already sitting in. **Placement resolves before the launch touches the store or creates a worktree**, so a rejected placement leaves no provisional rows or orphan worktree behind. The CLI placement resolver takes explicit flags first, then falls back to the per-machine [`[agents] placement`](../../guide/configuration.md#agent-profiles-commands-and-teams) policy.

| Situation | Where the layout lands |
| --- | --- |
| `--new-tab`, or no ambient pane to split | a new tab |
| `--new-pane` | a split of the current tab; an explicit flag that cannot be honored fails fast |
| policy `auto` (the default), single non-worktree cell, inside a room | the current pane: the CLI execs the wrapper argv in place, and the pane returns to its shell on exit |
| policy `pane`, single non-worktree cell, inside a room | a split of the current tab |
| policy `tab` | always a new tab |
| any policy, with a named channel, a multi-cell layout, or a worktree | a new tab |
| `--bg`, an agent caller, or create-on-miss | never in place: the caller's pane stays available, so an in-place choice downgrades to a split |

Both backends receive the same `TabOptions` (session, title, cwd, focus flag, sidebar options, the pre-built pane argv, and the environment map for every command pane) and dock the global sidebar once before adding the layout cells. The per-backend split commands live in [`mux/`](../../../crates/rimz/src/mux/AGENTS.md).

Focus follows one policy, `crates/rimz/src/harness/ancestry.rs::LaunchFocus`, resolved from `--bg` and the caller identity: `Keep` for an agent caller or `--bg`, otherwise `Take`. Interactive launch shares its caller resolution with ancestry stamping. Fork, lane resume, and restart resolve it from their loaded agents, and team restart passes one decision to every member. `Keep` also suppresses already-live cohort and lane jumps and downgrades in-place launches to splits. Restart skips pre-focus and anchors its unfocused replacement to the old pane in the room session. On a supervised `-p` run, `--bg` also means return at once. See [multiplexer focus behavior](../multiplexers.md) for the Zellij 0.44 transient.

Tab titles follow the address vocabulary. A named-channel or worktree launch names its tab `#<NAME>`, and a named team launch names it `team:<name>` and stamps its in-place lane as `<dir>/<team>`. Other launches join their pane names with `+`: profile else kind for agents, executable basename for commands, and shell basename for empty command cells, keeping three tokens before `+…` and omitting the directory suffix (`opus`, `nvim+claude`). An empty layout falls back to `term`. An in-place launch applies that title to its existing tab before exec and pins the pane's own launch name.

Titles are best-effort display. The sidebar producer returns a pane-named tab to the shell's name once no agent remains; tmux also restores inherited automatic naming and clears the pane-name pins. Scoped and user-chosen names that do not match the panes are kept ([multiplexers.md → tab names](../multiplexers.md#tab-names)). Mux tab names are mutable and live outside the store, so they never form an address.

### One team per checkout

A fresh team launch refuses a checkout held by a different team. A present member holds it, or, with no present members, a board stage other than `Done` holds it. `resume::inspect_team_hold` takes the holder's identity and channel from root launch occupants in the audit store rows at the normalized checkout path, and reads the stage through `scratch::board_stage`. No team rows, a missing stage, or a `Done` board with nobody present means no hold.

Hold admission counts every root team row with a non-empty id and a checkout, including never-registered launch placeholders regardless of liveness; resume admission does not apply to holds. In `launcher::settle_layout` this check follows the `--from-pr` holder lookup and precedes reconciliation and named-worktree creation, including for in-place, `--cwd`, and channel launches. Same-team reconciliation is not gated, and neither are explicit resume, rebirth, and channel resume. The audit read is unlocked, so concurrent launches can both pass.

A board-stage refusal names the release: `rimz teams flip TEAM#LANE Done "abandoned"` records the transition and releases the hold without deleting the checkout. Addressed board commands use `resume::inspect_cohort_hold`, which filters the channel hold by team.

### One team per channel

A fresh team launch also refuses a channel held at a different checkout, even by the same team, so separately launched cohorts cannot merge into one address or restored tab. `resume::inspect_channel_hold` uses the same root-occupant selection and hold logic as the checkout rule. The newest present member holds the channel. With none present, the newest member holds it while its checkout's board stage is pending and its team is the one `inspect_team_hold` names at that checkout, since a board records no team or channel and a later run there on another channel owns it. Rows without a recorded checkout are ignored. Channel matching uses `AgentState::channel`, including its worktree-basename fallback for unstamped rows.

The check runs just after the checkout check, before reconciliation, checkout settlement, channel registration, or store writes. `spec::resolve_room_channel` computes the eventual channel there: explicit, inferred for a role, or derived. A bare `-w` has no target yet and is not checked. The same checkout is exempt, comparing canonical paths where available and lexically normalized paths otherwise, which keeps symlinked `--cwd` launches and same-team reconciliation working.

Two gaps are known. A fresh in-place launch of the same team at the same checkout passes both checks and can duplicate a cohort. As with the checkout rule, the audit read is unlocked, so concurrent launches can both pass. Explicit team resume, channel resume, and rebirth are not gated. Refusals name the channel, holder team and checkout, reason, resume command, and alternative channel; a pending-board refusal also names the addressed `rimz teams flip TEAM#LANE Done "<note>"` release command.

### Cohort relaunch reconciliation

Relaunching a team into a worktree that already held one is where a naive launch would silently duplicate work. Reconciliation runs whenever the command names a team, or an inline layout with at least two agent cells, and supplies an explicit `-w NAME` or reuses a holder through `--from-pr`. It runs after placement and the live-room preflight ([`reconcile.rs`](../../../crates/rimz/src/launcher/reconcile.rs)). For `-w`, it precedes worktree resolution; for `--from-pr`, it follows the holder lookup but precedes tip choice and settlement, including before a resume ([PR checkout ordering](./worktrees.md#pr-checkout-in-a-team-launch)).

The caller supplies the named worktree path without creating it, or the PR branch holder's actual path. Reconciliation reads the audit rollup for matching root members in that path and picks one of four outcomes.

| History in that worktree | Outcome |
| --- | --- |
| none | check entry into an existing unoccupied managed tree, then continue with a fresh launch |
| live members | with a prompt, dispatch to the live leader at the Done boundary and print the shared message receipt; without one, focus the newest bound member. `LaunchFocus::Keep` suppresses either focus jump. A missing leader refuses without sending |
| closed, with dirty or unproven work | offer resume / fresh / cancel, default resume |
| closed, clean and content-landed | offer remove / fresh / cancel, default cancel |

The harness returns the leader's identity and present member identities with `Present`. A declared leader role uses the same claimed-wins `address::role_holders` rule as stage delivery; inline layouts and roleless team cells use the cohort matcher. The launcher dispatches to that identity with ordinary caller attribution, focuses its pane when allowed, and uses the other present members for missing-leader message hints. Resume's pre-registration prompt queue is separate from this dispatch.

The library launcher's [`worktree_entry.rs`](../../../crates/rimz/src/launcher/worktree_entry.rs) owns entry checks and protected recreation; the shared CLI module presents offers and their facts: marker age, latest audit-row activity at or under the checkout, uncommitted count, and landed verdict. The harness's [`worktree_entry::read`](../../../crates/rimz/src/harness/worktree_entry.rs) takes a marked tree and owns the landed-memo read, Git inspection with the non-force removal rule, and audit activity over both lexical and physical checkout paths. `worktree_entry::occupied` applies the takeover occupancy rule over runtime-scoped rows.

Both offers read one line through `cli::choose`, which takes the default on Enter and returns `None` for an ambiguous prefix, an unknown word, or EOF. The caller maps `None` and an explicit `cancel` to an error with `canceled; nothing launched` and exit 1. Without a terminal on stdin neither offer prompts, and both refuse with exit 1, including the facts. The shared `relaunch_commands` builds shell-quoted, prompt-preserving resume and `--fresh` spellings for teams (`team#lane`) and inline layouts, and the remove offer also prints `rimz worktree remove <name>`.

Named-team reconciliation considers every member of that team in the target worktree, including sibling roles when a single-role spec relaunches. Inline membership matches by launch group, then ordinal, then kind and role, with a final kind-only fallback for records that carry no role. Ended and confirmed-dead members both count as closed. A worktree holding only non-live launch placeholders has no cohort, by the same admission rule resume uses, and returns `Reconciled::Absent` for the entry check.

`crates/rimz/src/launcher/worktree_entry.rs::check` is shared by `launcher::settle_layout` and supervised `prepare_supervised`, after cross-repository consent and before checkout resolution. It checks only explicit nonempty names, and returns without an offer for missing, unmanaged, or occupied trees. Occupancy comes from `schedule::takeover::read` over the cached alive snapshot, not the Audit projection used for history. Single-agent launches reach it directly, and cohorts reach it only after `Absent`, so Closed never prompts twice. Loop tasks, `--from-pr`, `--continue`, and `--resume` are exempt. The [reference entry table](../../reference/cli/agents.md#relaunch-into-a-named-worktree) owns the choices and exits, including exit `2` for supervised entry refusals.

The fresh outcome is `Reconciled::Continue`. Launch then resolves the checkout with `reuse_existing`: no worktree or branch is removed or recreated. The remove answer gathers `crates/rimz/src/launcher/worktree_entry.rs::explicit_removal_protections` after the prompt, from this room and, on a cross-repository launch, the room of the repository that owns the tree, and calls non-force `worktree::remove`. A live agent or a plain pane can refuse removal even when the cohort is closed. `retire_removal` (session retirement plus channel message archival) fires only after successful removal. On a fresh launch the prior rows keep their names reserved, and the fresh members mint new petnames in the same channel. Orphan message archival belongs to `rimz gc`.

`--fresh` on `CohortLaunchArgs` takes the fresh outcome without prompting. The `Closed` arm returns `Continue` before the Git assessment, so it works on a dirty tree and a clean landed one alike, and leaves a clean landed tree standing. `Present` is checked first, so `--fresh` cannot duplicate a live cohort. `CohortLaunchArgs::validate_fresh_worktree` requires an explicit name in both `launch_layout` and `into_supervised_request`. Validation follows the fused `team#lane` expansion, which fills `worktree` after clap parsing, so a clap `requires` would reject it.

## The address

Every member has an address typed like an @-mention: `@<handle>#<channel>`. The handle names who, the channel names where, and both default from context. [cli/agents.md § Addressing agents](../../reference/cli/agents.md#addressing-agents) is the handle catalog for users; this section is how an address resolves.

### Resolving the channel

The channel is the workspace segment the room already groups by, resolved in order: an explicit named channel, else a worktree name, else an in-place team stamped at launch as `<dir>/<team>`, else a lane-path directory basename fallback for unstamped agents ([messaging.md § Channels](./messaging.md#channels)). The lane path is the launch's recorded `checkout` when present, otherwise `worktree_path`; process placement always uses `worktree_path`. Routing keys and durable records keep that value.

The project root's lane displays as `main`. The store fold derives `root_lane` for an unstamped agent whose lane path is the workspace record's project root, and `AgentState::lane_label` and `PaneAgent::lane_label` display it as `main`. Addresses and listing filters accept `#main`, the root basename, and the root path. The name is not reserved: a stamped `main` lane shares the displayed name, and handles resolve the agents. Message and transcript root filters include channel-less historical records and records under the root basename, without rewriting them. Directory-room root pods display `main`; repo-root pods keep their branch labels. Native tab titles keep their routing names for resume matching.

The default is the channel the command runs in, and an inline `#<name>`, `--channel`, or `--worktree` overrides it. **A human shell in a bare directory workspace has no current channel, and no current channel means every channel.** An agent uses non-empty `RIMZ_CHANNEL`, else its stored card channel, before the worktree rule. The caller lookup is the same as `@me`, and an ended caller's row still supplies its channel. An unavailable store or unresolved caller falls through silently. `RIMZ_TEAM` is cohort identity for team members.

A scoped miss names the searched `#lane`, notes when the ambient lane came from a directory name, labels root-lane matches `main`, and supplies a resolving correction address (or a corrected channel flag). The shared CLI error renderer adds a shell-quoted whole command on a `try:` line when the target appears verbatim in the invocation.

Launch specs resolve against the same lane. A bare role qualifies to `<team>.<role>` when the lane's agents carry that team, so `rimz agents reviewer` in `#forge` launches the forge reviewer and stamps it into the lane it resolved from. A live agent in that lane already answering to `reviewer` [refuses the bare word](../../reference/cli/agents.md#launch-agents) under the same lane rule. The inference reads the stamped `team` on those agents, because the three lane shapes mean the channel string alone does not name a team. A bare role that also names a cell resolving to a different agent refuses instead of guessing. Branch names are display metadata only.

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

`--create` launches a missing agent straight from its address. `rimz message --steer @planner#design --create "draft the API"` opens a `planner` in `#design` with the text as its first prompt. With `--worktree feat/x` it selects that worktree instead, including the [named-worktree entry check](../../reference/cli/agents.md#relaunch-into-a-named-worktree); its refusal prints an agents fresh command carrying the message text.

Resolution has two sources and one matcher set over both: rollup sessions (`&AgentState`, used by management commands and parked message records) and the live agent panes the producer bound (`&PaneAgent`, used by `--steer` and send-now messages). Each command chooses its source. Pane binding then joins a match to exact lifecycle state, a same-channel provisional launch card, or a sessionless lazy target, so an agent is addressable before its first turn registers.

### Petnames and the canonical handle

The petname is the stable per-instance fallback name. The store mints an adjective-noun pair at registration through [`agents::petname`](../../../crates/rimz/src/agents/petname.rs). It refuses reserved command words and kind-shaped names, so a petname can never shadow `@all` or `@claude-2`. It is collision-checked against every name in the rollup (retained ended rows keep their names reserved) and against existing session-id prefixes. A session with no stored name re-derives one deterministically from its session id, with the same generator and reserved names.

The minter hashes the seed with each attempt number and tries 64 plain adjective-noun candidates (`UNSUFFIXED_ATTEMPTS`) before adding the attempt number as a suffix. Every candidate passes the same validity and collision checks. A valid, available stored name takes precedence over this fallback. A derived name depends on the names reserved when it is derived, so a nameless session's derived name can change after another session takes the candidate it would have used.

The rendered handle is the shortest address that names exactly that agent, and it round-trips through the parser. The renderer tries, in order: the role when unique in scope, the explicit `--name`, the profile when unique, the kind, the petname, then `@<kind>-<n>` when scoped, and finally the session id. A launch card nobody claimed yields its role to a claimed holder in its own lane, so a role survives a card left behind by a launch that died before binding; across lanes both still match. A listing therefore always shows a handle you could type back, and a handle appears only when typing it reaches that one agent. Every agent-bearing listing shares this one renderer; `address.rs` owns both it and the parser, and tests them against each other.

Message headers instead use `agents::petname::sender_handle` (role, name, kind) so the reply address stays stable when peers join or leave; the [header contract](./messaging.md#the-message-header) covers its label and channel suffix.

## Resume and rebirth

The store remembers agents whose processes are gone, and resume turns those records back into panes.

### Resume entry points

| Path | Trigger | Scope |
| --- | --- | --- |
| Room rebirth | a machine reboot or mux crash, at the next `rimz start` | root sessions from the producer's persisted live roster and the pending-recovery record, intersected with the audit rollup, seeding one tab per live-at-death lane or worktree after the new mux session is created; entry into an already-running room repairs settled seats and reports the agents still parked without opening tabs ([Rebirth](#rebirth)) |
| Cohort resume | `rimz agents <spec> --continue` in a live room | one prior cohort matched from the spec, after its tab or pane was closed ([What matches what](#what-matches-what)) |
| Lane resume | `rimz agents resume <scope>` | one lane, resolved by `harness::resume` ([Lane resume](#lane-resume)) |
| Session resume | `rimz agents resume @<ref>` | one retained closed solo root, qualified by `plan_recorded_resume` without cohort matching ([Single-session resume](#single-session-resume)) |
| Provider-session resume | `rimz agents <kind-or-profile> --resume=<id>` | exact audit id first, otherwise one session located in the room account's provider home |

No resume path plans a row with a parent identity: `harness::resume::root_session` excludes both RimZ-launched children and provider-native subagents, and the parent relaunches any child it still needs. `crates/rimz/src/agents/state.rs::AgentState::is_child_session` is the shared child gate for single-session resume and exit guidance: only `parent_agent_id` makes a row a child. A parentless peer is eligible at positive launch depth. Resume keeps its `launch_depth` and `launched_by`, preserving the chain budget, but does not reopen its supervised run or reconnect `report_to` to the original launcher.

The CLI applies `LaunchFocus` to every lane action: `Keep` reports an already-live lane without a jump and opens restored tabs and splits unfocused.

### Single-session resume

Session resume and provider-session resume share one path. Recorded rows go through the single-session planner, with its live, team, child, and history-pool refusals. An unrecorded id uses `crates/rimz/src/agents/login.rs::ProviderLogin::locate_session` to stamp the account and `crates/rimz/src/harness/resume.rs::plan_provider_resume` to feed the same flat candidate planner as lane discovery, with a minted handle and the selected solo profile. Claude's locator scans the account's projects and reads the cwd from JSONL; Codex's scans active and archived rollouts without the discovery or hook day budgets. Neither locator falls back to a different home.

Both entrances take launch overrides and machine isolation. They return the resolved isolation and skills presence for preflight, plus any newly selected tier fallback for an assist after successful placement; replaying a saved tier does not log that fallback again. They compare isolation only against the recorded effective stamp and return warnings as values. A rejected override returns a skip instead of degrading to a bare launch. The exact-id entrance resolves its layout without overrides, then applies them against the selected session, so an unsupported preset reports the session refusal.

Session resume uses the same account and conversation checks as lane resume, including the room's same-history-pool account selection. Its placement uses the single-cell cohort policy: an origin pane in the saved cwd can resume in place, `Keep` downgrades that to a split, and a launch from elsewhere opens a tab. Provider-native children are not ordinary addresses; the CLI checks them only to give the child refusal, never to launch them through this path.

Bare `--resume` opens the provider's own session picker. It uses the fresh single-cell launch batch with `crates/rimz/src/harness/launch.rs::ExecAction::Pick`, serialized as `{"action":"pick","extra_args":[…]}`. The adapter's `crates/rimz/src/agents/definition.rs::LaunchSpec::picker` supplies argv; profile posture and prompts follow ordinary fresh-launch compilation, without a positional prompt or headless form. Pick consumes the minted launch identity and has no attach target. The first identity-bearing hook adopts the provisional row for an unknown id, or retires it and revives an existing closed row with its old handle and history but the settings the picker ran with. Lazily registering providers (Codex and Kiro) bind at the first prompt, so their cards stay provisional until then. Pane-only discovery deliberately cannot bind a fresh pane to an old session.

A picker is not a fresh launch for `harness::run::ProviderExit::fresh_launch`, despite its launch batch, and never enters startup relaunches, even if the picker crashes: the user is present and can retry by hand. An exit before any session binds suppresses the resume or relaunch hint regardless of exit status, and a bound session's later exit gets the normal hint. The distinction uses durable session binding, never elapsed time, a provider-specific exit code, or the key that ended the picker. Hint suppression does not bypass `stamp_own_end_if_deliberate`: the unbound provisional card is still ended, so it cannot remain as a ghost. An unbound picker exit is neither a startup death nor a `launch_retry` assist; the independent [startup diagnostic](#how-the-wrapper-settles) still applies.

### Lane resume

Lane resume picks one of four `LaneResumeAction` variants:

| Action | When |
| --- | --- |
| `List` | no scope was given |
| `Focus` | every member is live; focuses the freshest pane |
| `SplitClosed` | a member is still live; plans flat resume commands beside it |
| `RestoreClosed` | every member is closed; reuses the rebirth team and flat split |

Discovery drops sessions the store records as children before selecting the newest concurrent set, for both listing and restoration. In every case the CLI runs the compile preflight and the [login health check](#every-path-that-builds-a-launch-layout) on the planned provider kinds through `LaneResumeAction::agent_kinds_needing_preflight`, before `LaneRestorePlan::materialize` allocates fresh team identities or any mux action runs. A launch environment that fails preflight or an account whose hooks refuse therefore fails the whole command before it half-rebuilds a room.

The CLI filters `List` to lanes with closed members and prints a quoted resume command for the first row; the harness summaries include all-live lanes. Empty and all-live listings have distinct nothing-to-resume messages. `SplitClosed` and `RestoreClosed` announce their counts before the first mux open, after any skipped-live lines, instead of printing an after-open receipt. A failed open keeps that announcement and reports the failure.

Lane `--fresh` restores closed named-team tabs with every cell seeded `CohortSeed::Fresh`, using the current team layout in the saved cwd and channel. New sessions receive new handles and the team's board and memory-file reminders, without requiring saved provider conversations or matching the old login. Flat roots are skipped with deduplicated relaunch commands: `-w <worktree>` for a worktree lane, `--channel <channel>` for a project-root channel lane, and no place flag otherwise. `plan_lane_resume` refuses a lane with live root members, no durable candidates (including discovered-only lanes), or no restorable team; the last error carries the relaunch commands. `--bg` applies as on any lane action. Team tabs reduce the `resume.max` allowance for flat sessions as on ordinary resume ([Rebirth](#rebirth)).

### What matches what

Cohort resume, `rimz agents <spec> --continue`, matches the spec against prior root sessions.

| Spec | Matches |
| --- | --- |
| named team | prior root agents with the same `team`, role cells mapped by role, newest member per role |
| inline multi-agent | the newest `launch_group` that maps onto its agent cells by `launch_ordinal`, falling back to kind for records without ordinals |
| single agent | the newest root session of that kind, and of that profile when the spec named one, whatever its state |

Missing cells launch fresh in the matched cohort's cwd and channel, so the layout stays whole. A matched member whose process is live refuses the whole resume and names it, because launching beside it would duplicate the addressable role or kind; for a single-agent spec that means a live newest session refuses even when an older one is dead. A single non-team cell whose matched session cannot resume refuses. Teams and multi-cell cohorts refill unresumable seats fresh and report them as such.

Cleanly ended members stay candidates, so a closed team resumes while its worktree exists. Empty session ids, missing worktrees, and launch placeholders that never adopted a session and are no longer live are excluded from member matching. When a named team matches no member, `crates/rimz/src/harness/resume.rs::plan_cohort_resume` falls back to its newest existing checkout in scope where `inspect_team_hold` names that team with a `BoardStage` hold. Every seat starts fresh in that checkout and the hold's channel, continuing the board without treating a placeholder as a conversation. Lane restore and rebirth use the member-only planner and never take this fallback.

Cohort resume scopes to one exact worktree with `-w <NAME>` or the caller's current worktree, while a resume from the project root takes the newest match for the spec. `CohortResumePlan` carries the canonical spec label, the selected lane label, and a matching-lane count. The count is the selected lane plus every other worktree or channel group where `match_cohort` finds a match with no live member, and the label shares the lane-list rule. The CLI announces this selection before placement, including same-pane takeover, then prints its member receipts. Worktree scoping can still keep several channels, so the alternatives hint directs the caller to list from the project root.

### Rebirth

Rebirth restores a named team in its declared layout, resuming members that can resume and fresh-launching missing or unsupported agent cells so the shape stays whole; other lanes restore as one column. A birth does two separate things: the boundary records that an incarnation died, and the settlement decides what happens to its agents.

The boundary ends nobody. It parks the roster's agents in `records/pending-recovery.json`, appends `session.rebirth`, and deletes the roster. The roster also carries children for crash protection: the boundary cancels each roster child's newest open run and wakes its waiter, and the child stays parked until settlement or the reaper ends it. An inspection-failed or supervised birth records only the boundary, so its children wait for the next live-entry settlement or reap.

A room's death is not yet the death of its agent processes: each exec wrapper holds its provider through two signal graces after the pane's hangup. So the boundary inspection waits up to 3 seconds for the live owners of the agents it could offer to exit, without signalling them. It plans from the last read of both the process table and the event log, so a provider that records its own end while dying in the wait is ended, not offered. An owner still live then leaves its agent parked and unmentioned. A birth with no such owner waits for nothing, and a live-room settlement never waits.

The settlement decides what happens to the root candidates by one of four dispositions (`RebirthDisposition`):

| Disposition | Planned agents | Candidates the plan cannot resume |
| --- | --- | --- |
| `Defer` | stay pending | stay pending |
| `Decline` | ended `rimz.recovery-declined` | ended `rimz.recovery-declined` |
| `RecoverKeep` | resumed | stay pending |
| `RecoverDrop` | resumed | ended `rimz.not-resumed` |

Only an attended entry chooses a disposition that ends root recovery candidates. With recovery on, an interactive birth asks whether to recover the resumable agents (default yes; no is `Decline`), then lists the ones that cannot be resumed with their reasons and asks whether to drop them (default no, which keeps them pending). With recovery off (`--no-resume` or `resume.on_rebirth = false`) an attended entry declines without a prompt. An unattended birth recovers and keeps the rest when recovery is on, and defers when it is off. Missing-worktree agents appear in the second question with reason `worktree gone`. They stay pending under `Defer` and `RecoverKeep`, and `RecoverDrop` ends them `rimz.worktree-gone`.

Children are settled apart from the dispositions. Every disposition, `Defer` included, ends each non-live, unended child in scope as `rimz.child-not-resumed`. Children are not recovery candidates and appear in neither the drop-rest question nor the parked count. `RebirthPlan::settle` first cancels each such child's newest nonterminal run through `run::cancel_and_wake`, `keep` runs included, and leaves already-terminal runs untouched. The durable cancellation settles the parent's wait and lets its fleet digest report the result; the waiter wake datagram is best-effort. Each durable end appends one informational `recovery_child_ended` diagnostic, and a failed end append leaves the child pending.

Refilled seats are settled apart too. Every disposition ends a parked team member `rimz.seat-refilled` when a live member already fills its matched seat. Such members are not recovery candidates, and a partially live cohort plans no team tab: its empty seats fall back to individual resume planning. A planned `Fresh` replacement differs: its old session stays a candidate until its tab is confirmed open, then ends `rimz.seat-refilled`. A failed tab leaves it pending, `Defer` opens no tab and leaves it pending, and `Decline` ends it `rimz.recovery-declined`. Each refilled end appends one informational `recovery_seat_refilled` diagnostic, not an assist record.

Explicit cohort and lane resumes settle refilled seats the same way, through `settle_refilled_seats`. Once `rimz agents <spec> --continue` has placed its cohort or `rimz agents resume <lane>` has opened a team tab, the `Fresh`-seeded members' old sessions end `rimz.seat-refilled` and leave the pending record. Here a successful `open_tab` confirms placement, without a structural check, and a tab that fails to open leaves its old sessions parked. Same-pane cohort placement settles before exec, since a successful exec never returns. `rimz agents resume --fresh` ends none.

A rebirth settlement runs in two steps, `RebirthPlan::settle` then `SeededRecovery::confirm`, so that each tab settles alone. Resumed agents and planned `Fresh` replacements stay pending until their own tab is known to be open. Settlement marks the resumed agents seeded in the pending record, so their wrappers' pane attachments do not release them before confirmation. A confirmed tab's agents leave the pending record. A tab that did not open returns its agents to parked for a later rebirth or explicit resume, fails its team launch batch, and prints one warning naming the tab and the reason. Recovery counts and assists include only confirmed opened tabs.

Confirmation asks the backend, because a birth seeds its tabs with the session. After the health gate, `MuxBackend::confirm_resume_tabs` reports which planned tabs the session holds: a tab is open when a live tab of its name holds at least its planned panes besides the sidebar, each planned tab claiming its own live tab. A pane whose process exited does not count, including one tmux keeps under a user's `remain-on-exit`. The check reads tab structure only, waits a bounded time on Zellij for the layout to materialize, and runs on a failed health gate too before the error propagates. Zellij closes an unconfirmed tab only by a stable ID proven new by topology, best effort, before returning the error, so a later recovery does not deliberately duplicate a half-open tab.

The birth holds `recovery.lock` from settlement through confirmation, so an entry that finds the room already live waits and then inspects the settled record instead of opening a second tab for agents still pending. Every live-room entry, attended or unattended, reconciles through `RoomContext::reconcile_parked_recovery`. It takes `recovery.lock` before inspecting pending membership, drops already-ended entries, silently ends non-live children, and ends seats already filled by live members when recovery is on. Other root candidates stay parked under `Defer`, except that an attended entry with recovery off declines them. A nonempty settlement also clears stale seeded markers, and an empty plan returns without a write.

A live-room entry writes no session event, roster, or boot marker, never resumes or ends a live agent, asks no recovery question, and opens no tab. The CLI prints at most one root count notice naming `rimz agents resume <lane>` and `rimz start --no-resume`, or the count ended by an attended decline. Web and remote confirmation switches apply only to births; a reconnect to a running room does not wait for recovery consent. This flow is the same on Zellij and tmux, even when no client is attached.

Flat resume keeps pane identity when a stamp survives: newest-first candidates sharing one pane collapse to the newest session. A rebirth boundary retires pane stamps, because pane ids renumber across a mux restart; an unstamped root stays a candidate and deduplicates by `(kind, session id)`.

`resume.max` (`DEFAULT_RESUME_MAX`, 128) bounds how many flat sessions one reborn session or lane restore auto-resumes, so a long-lived workspace cannot fork-bomb the machine on birth. Team tabs restore whole and are not capped: their pane count is subtracted from the allowance the flat sessions share (`crates/rimz/src/harness/rebirth.rs::plan_recovery`), so a team alone can exceed `resume.max`. Anything past the cap is reported as a skip. Every `ResumeSkip` carries its reason into the start report, from `crates/rimz/src/harness/resume.rs::ResumeSkipReason::label`: among them `no resume CLI`, `no saved conversation`, `over the resume cap`, `no prompt replacement`, `different account`, `account unavailable`, `scoped view unavailable`, and `configuration unavailable`. The recovery preview also lists missing checkouts as `worktree gone`.

### Discovering sessions the store never saw

When an explicit lane has no durable candidates, or every closed durable candidate has lost its provider conversation, lane planning asks its caller for adapter local-session observations, read from the provider's own session files.

Each observation spans `[created_at, last_activity]`. Transitive interval overlap forms concurrent clusters, and the cluster containing the newest activity is the last concurrent working set. That cluster resumes newest first up to `resume.max`; every older cluster is reported and stays closed. A synthesized flat record carries only kind, exact session id, workspace, and last activity, with no channel, because provider files cannot reconstruct RimZ-only roles, teams, or lanes.

### Posture

A resumed pane comes back as the same kind of agent it was. It runs `rimz agents exec <kind>` with a `Resume` action carrying the provider session id and the prior RimZ identity (name, profile, role, team, launch group, launch ordinal, channel), and is named like a fresh launch: stored profile, else kind, even when its posture degrades. Its **posture** is the planner's `ResumeLaunchPosture`: model, effort, and `auto-compact` argv, system-prompt source paths, skills, permission mode, budget, profile `args`, and an optional isolation override. A session that launched as `@planner` comes back as a planner.

`resume::resolve_posture` supplies this projection for profile-based relaunches, and `ResumePosture` adds only the degradation reason. It resolves the stored profile against effective config, rebases onto `LaunchRecord.agent` when present, then overlays the record's verbatim model and effort; the profile and role identity do not change. A missing base degrades with its name. With a record and no base, the provider's `Profile.definition_renders` entry is chosen by the session's kind, so a session routed to another provider stays on it after its tier clears. A routed session with no record selects its provider's complete `Profile.definition_renders` entry and stamped model through its durable `LaunchParams.tier`, without a tier walk or usage check, and takes effort from the current definition or model default. The record wins over a contradictory tier and clears it. Restart, fork, subagent resume, unattended rebirth, and `explain @handle` share this selection. The native auto-compaction window replays through the current profile's rendered argv; no separate environment variable or durable launch field carries it.

Team restore and cohort resume rebuild every member through `resume::restore_routed_cells` before preflight and pane compilation, keeping team prompt and cohort identity. A record replays even without a tier stamp; a row with no record falls back to the stamped tier or current profile. A recorded team member keeps its role binding's prompt files and `args`, and `agents restart` keeps them too through `resume::resolve_member_posture`, which finds the member's role in its recorded team. Profile matching admits a single-profile or roleless-team member carrying a record or a tier stamp even when its provider differs from the current load choice; named team roles match by role.

Permission mode on resume follows one precedence. An explicit `--ask` or `--yolo` wins. Otherwise a recorded session replays its saved mode over any profile- or role-declared one, and `--agent` keeps the saved mode unless the new base declares one. A row with no record uses the declared mode, else the launch event's recorded mode.

Degradation is deliberately asymmetric. A profile that is gone or now names a different provider degrades to a bare resume with a warning, because rebirth runs unattended and a recovery must never refuse. Interactive `restart` and `fork` escalate the same provider switch to the user instead, since changing providers under a running agent is a decision for the user. They also refuse a recorded profile whose Markdown definition failed to load, before posture resolution (`crates/rimz/src/config/effective.rs::LaunchAgents::block_failed_reference`).

`finalize_agent_cell` stamps model, effort, and the non-routed agent base into `LaunchRecord`. `ResumeLaunchPosture` supplies both record and tier to exec parameters, and the wrapper's `agent.attached` saves record, tier (including clearing it), and present mode on every reopen. Isolation is an independent present-value overlay.

Before process compilation, `crates/rimz/src/harness/launch_plan.rs::prepare_exec` resolves a durable launch record's requested model through machine pins, the account-scoped adapter catalog, then passthrough, as on a fresh launch. Observed model switches do not alter the record. Only sessions without a record replay the last observed session model when their posture names an alias (fork uses the source session). An explicit `--model`, `--tier`, or `--agent` on resume skips that replay through `ExecIdentity.resume_model_override`, and a missing observed model warns and resolves afresh. Resolution rewrites both provider argv and launch parameters, so the bound event, model environment, and reminder agree. `agents explain` is read-only and shows the requested alias.

Resume overrides apply after cohort matching or single-session selection through one rule body. `--agent` must keep the provider and replaces the old profile replay. `--tier` selects only same-provider entries in the requested row using fresh launch availability, without climbing, and logs tier-fallback assists for resumed as well as fresh seats. `--model` clears the tier. Cwd and channel come from the selected session or matched cohort; cohort `--worktree` is a scope and creates nothing. `validate_resume_inputs` refuses only system-prompt files and passthrough on both explicit and reconcile cohort entrances; the single-session entrances refuse them in clap. Original prompts and passthrough argv are not replayed.

A cohort's positional prompt becomes a queued Human message at gate Done, never a provider launch prompt. It is addressed to the leader's session, or its provisional fresh card, before placement. A resumed leader's card is still ended at that point, so the wrapper's resume stamp releases the prompt, not the provider's registration, and it is delivered ahead of the leader's stage re-wake.

The wrapper attaches the resumed session before the provider starts. Once the provider process stage is ready, the resident exec wrapper appends `agent.attached`, binding the exact resumed session to the stable launch id exported to its process and the wrapper's pane. The first record carries the wrapper's own owner, so identity exists before the provider can call RimZ, and a second `agent.attached` stamps the spawned provider's pid immediately after the spawn. A resume appends `rimz.agent-resumed` before the provider starts, so it precedes any hook the provider fires, and re-wakes the member's open team stage ([teams.md § Resume re-wake](./teams.md#resume-re-wake)). The provider-owned stamp is what proves a later same-pane conversation belongs to the same instance ([instances.md § Launch identity across conversations](../agents/instances.md#launch-identity-across-conversations)). A row with no launch id uses its provider session id for the new stable stamp, and a provider-store discovery gets an idle durable seed, so either can launch a child or be addressed before a lazily registering provider emits its first hook.

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

A listed name does not prove the room is the wrapper's own. A wrapper still on its exit ladder when the next start brings the room back sees the new session under the same name. Every rebirth parks the dead room's roster in the [pending-recovery record](../store.md#the-sessiondeath-record) before it creates the session, so the wrapper reads that record after the listing and treats an agent named there as lost, not closed: no end trace, no subscription retirement, no worktree cleanup, and the agent stays on offer. Membership at an abrupt exit means a rebirth parked the agent after this wrapper took its pane, or seeded it and has not yet confirmed its tab. Either way the wrapper infers no end; a clean provider exit still stamps one.

Attachment moves an agent out of the record. Once a wrapper's `agent.attached` is durable, `Store::attach_agent_pane` takes a parked session out of the record, but leaves a seeded session pending until its tab is confirmed. An explicit `rimz agents resume` therefore makes a parked agent ordinary again, and closing its pane ends it. An agent in an unconfirmed seeded tab instead returns to parked, keeping its reap shield even if its owner is live; settlements skip live owners, so it is not resumed twice.

A deliberate exit records the durable `rimz.agent-ended` trace before any slower cleanup, so that agent stays out of automatic recovery. A parent's message can still [resume its ended child](./subagents.md#follow-ups-and-resume). A supervised non-subagent run that exits on run completion skips the trace because its run record already carries the outcome. A kept subagent waits until its hold ends by Ctrl-C or parent end before recording the trace; SIGHUP/SIGTERM during the hold leaves the stamp to the [stop seam](./scripting.md#reclaiming-the-run-pane). A non-deliberate exit skips both the trace and worktree cleanup, because recovery should come from the sidebar producer's latest live roster instead.

### A startup death is relaunched first

Before any of that, a fresh launch whose provider exits nonzero before its session opened is relaunched in the same pane, up to `[agents] startup-relaunches` times with `[agents] startup-relaunch-wait` before each. One rule, `AgentsConfig::startup_relaunch_wait`, reads that wait (0s through 24h) for the wrapper and for the fresh-launch entry points. With `startup-relaunches` above zero, those entry points refuse a bad wait before any pane opens; with it at zero the wait is not read. The value is not checked at deserialize, where the lenient machine load would fall back to the defaults and launch anyway.

The rule is `harness::run::startup_relaunch`, and it reads one of two durable facts. A launch with a run record died at startup when that run is still `Pending`. A root launch has no run, so the wrapper reads its launch card: still provisional (a snapshot row keyed by the launch id under the launch name), and the provider exited within 60 seconds of that attempt's spawn. The window exists because a provisional card is normal for a lazily registering provider nobody has prompted, and for any provider whose hooks are missing. One case is accepted for fresh launches only: quitting such a provider with a nonzero status, without a signal, inside the window and before its first prompt reads as a startup death, and Ctrl-C during the announced wait is the way out.

A resume or a fork is never relaunched: a resume has no provisional card, and a provider may create the forked session before its first hook, so a relaunch could fork twice. [Pickers](#single-session-resume) are excluded even while their cards are provisional. The card stays unfailed through the sequence and is marked `Failed` only when the wrapper settles.

When the provider exits, the wrapper holds `RUN_EXIT_TERMINAL_GRACE` for a late first hook, then asks `startup_relaunch`. A relaunch needs all of these: a fresh launch, a nonzero exit, a provider the wrapper did not end itself, no stop or interrupt signal, and fewer relaunches than the cap. When they hold, the wrapper prints one stderr line, waits `[agents] startup-relaunch-wait` (default `3s`), and asks again on fresh evidence. A stop, an interrupt, an ended parent, a run that settled or timed out, or a late first hook during the wait cancels the relaunch, and the last exit settles. A launched child's wrapper confirms the parent once at each decision ([subagents.md § The parent watchdog](./subagents.md#the-parent-watchdog)).

Otherwise the wrapper spawns the same command again in the same pane, records the new `provider_pid`, hands the parent watchdog to the new process, and appends one [`launch_retry` assist](./loops.md#the-assist-log) per relaunch. Handle, run record, pane, and `deadline_at` stay the same. The wait comes out of the run's deadline, and `wait`, `list`, and the fleet rule see one starting child, so a death that is relaunched is never reported. When every relaunch is spent and the last process dies the same way, the exit settles as any exit does. The wrapper says so on stderr only after the failure tail is captured, so the reported reason stays the provider's own output. A relaunch that fails to spawn records `relaunched: false` and settles the previous exit as an ordinary failure. No provider error text enters the decision, and pane output is captured only after the last attempt ([capture bounds](./subagents.md#the-lifecycle-end-to-end)).

Every fresh launch through the exec wrapper gets this: a subagent, an `agents -p` run (whose `--retries` stack on top, each attempt with its own relaunches), a loop spawn or check run, and a root launch judged by its card. A run that left `Pending` is never relaunched, nor is an exit with status 0, a stopped, timed-out, or interrupted launch, or a resume, whose run is terminal and not `Pending`, so nothing durable says its session failed to open.

Two cases are accepted, not handled:

- Every built-in adapter declares a prompt-submit or pre-invocation hook, but no provider's order is observed. A provider that does side-effecting work before its first hook and then exits nonzero is relaunched and may repeat that work.
- A hook from a dead provider that lands after both the grace and the wait binds the run to that dead session. A later provider's observations are then ignored, and the run goes to its deadline and reports `timed out`. A hook that lands during the wait cancels the relaunch instead.

### How the wrapper settles

Before any settlement, a surviving wrapper records a non-successful provider exit within 60s of spawn that it did not cause (no run-completion or parent-end stop, no signal to the wrapper) as the best-effort [`provider_startup_exit` diagnostic](../diagnostics.md#event-taxonomy). This covers launches, pickers, resumes, and forks, independently of run or card evidence.

After the provider exits, the thin image releases any drainer lease, crosses its settlement boundary, and opens the store for the trace and cleanup. Run settlement drains queued hooks before deciding whether the provider exited before its terminal hook. A kept subagent's settled linger keeps that store open and is outside the parked-image memory budget. After the trace, the wrapper settles in one of three ways, depending on what the launch asked for.

**Drop to a shell.** A clean exit from a close-pane or worktree pane that is not a supervised run execs the user's shell in that pane, so the pane stays usable and any worktree stays inspectable. When an exit hint is due, a resumable solo session's hint is `rimz agents resume @<name>`, and a team member's is `rimz agents <team>.<role> --continue`. Resumable means a parentless real provider session id whose adapter compiles a resume command for this directory; otherwise the hint names a bare relaunch. The ended-session summary carries the recorded row's child classification into that decision, including the resume-target fallback. Running the resume takes that same pane back over instead of opening a lane tab. An in-place wrapper follows the same hint rule but returns to its existing shell.

**Reclaim the worktree.** An agent launched with `--worktree-path` triggers worktree cleanup on supervised-run completion or on a deliberate signal or tab-close exit. The cleanup proves the branch's work landed and that no agent, pane, armed loop task, or team board short of `Done` holds it, before removing the tree and deleting its branch. A clean interactive quit does *not* reclaim: it drops to the idle shell and leaves reclamation to `rimz gc`. A signal exit starts the cleanup helper with null stdio in its own process group, so it can finish after the closing pane disappears. The helper, its decision table, and the `gc` sweep are [worktrees.md § Who triggers removal](./worktrees.md#who-triggers-removal).

**Close the pane.** When it does not drop to a shell, a wrapper closes its pane if `close_pane_on_exit` is set or its parent ended. A subagent wrapper always closes its pane on exit, except a kept child stopped by SIGHUP/SIGTERM, which leaves pane reclamation to the stopper. An interactive subagent normally stays alive through its parent's receiving turn before cleanup stops it; `--keep` holds the pane after provider exit until parent end or Ctrl-C. The supervised-run side of pane reclamation, including background runs and cancellation, is [scripting.md § Reclaiming the run pane](./scripting.md#reclaiming-the-run-pane).

### Stopping an agent tree

Every agent-tree stop passes its whole root set through `agents_cmd::stop_resolved`. It closes children before parents, attempts every root even after a failure, and retires closed sessions' subscriptions. It then waits once for the runtime owners of the rows actually closed, ended rows and a refreshed molt successor included, so that a resume issued next does not race a dying provider.

The wait never signals an owner. It counts an owner only while `agent_liveness == Live`: absent, Daemon, and already-dead owners do not delay it, nor do failed closes. `store::runtime::await_owner_exits` and `OWNER_EXIT_BOUND` share the 3-second bound with rebirth, and for a stop the bound starts after the last close. The stop reads `runtime_projection(RuntimeScope::Audit)` after all closes to select owners, and again after waiting to decide which still run.

Human stop receipts name each still-running row's handle and pid and say a resume refuses until it exits, without changing the exit status. Team receipts omit the resume command when any member root still runs, and single-agent receipts omit it only when that root still runs. The idle-stop helper, loop takeover, and workflow host treat the bound as success without a notice. Run-only stops have no agent row to wait on.

## See also

- [scripting.md](./scripting.md): supervised `-p` runs: the run record, the wake socket, verify and retry, output formats.
- [subagents.md](./subagents.md): agent-launched children, their parent stamp, and `--keep`.
- [loops.md](./loops.md): scheduled tasks: the task catalog, host firing, the fire gate ladder, signals and waits, and the assist log.
- [workflow.md](./workflow.md): workflow runs: the graph, its checks and judges, and the attempt host.
- [budget.md](./budget.md): dollar caps: the scopes, the ledgers, the verdict, the waiver, and the gate.
- [hibernate.md](./hibernate.md): freezing idle agents and waking them.
- [messaging.md](./messaging.md): how text reaches a pane.
- [worktrees.md](./worktrees.md): the Git worktrees a launch can land in.
- [trust.md](./trust.md): which parts of a launch spec can execute a command, and how a grant is proven.
- [model.md](../agents/model.md): the agent rollup and state machine the harness reads.
- [instances.md](../agents/instances.md): pane binding, launch identity, and session death.

# Sidebar

This page owns the mechanics between the store and the painted sidebar: how a live pane becomes a row, how rows group and rank, which lines a card carries, how the frame composes, and how rendering launches, closes, recovers, and reloads. Four pages sit beside it. [state.md](./state.md) owns the data plane underneath: the fetch cycle, the published caches, realtime events, fusion, client views, focus intent, and cadences. [The interface reference](../../interface/sidebar.md) draws every glyph and frame and owns the words on screen. [instances.md](../agents/instances.md) owns how a session joins its pane from the session's side, and [DESIGN.md](../../../DESIGN.md) states the product commitments.

The sidebar is the narrow column pinned beside the work panes, and it answers one question: which pane needs the user now. One host paints every state of a room, from a bare shell to a fleet across worktrees, through detach and reattach. Only the snapshot changes between those states.

## The producer and the renderer

Every decision is made once, in the snapshot, and painting is a projection of it. Which agent owns a pane, which group a row belongs to, how rows rank, and what the cockpit counts are all resolved before a glyph is chosen. Painting therefore stays cheap and golden-testable (a snapshot in, a frame out), and CLI listings use the same view-model and binding rules as the pane.

That rule splits the code in two. The host producer folds the store over retained panes and the agent projection, and it alone publishes those inputs and the metrics history. One-shot CLI inspection and resolution compute privately, and passive CLI reads consume the host's publications. All of them share the fold and its repair rules. The renderer turns the resulting `SidebarSnapshot` into lines for one terminal. A behaviour that depends on the viewer (width, color depth, selection, scroll position, the active filter) belongs to the renderer. A fact about the room belongs to the producer.

The sidebar reads the store and never writes store truth. Most of its filesystem writes are runtime display files: its heartbeat, `lanes/unread.json`, the read-mark record, `lanes/focus-anchor.json`, the body filter, and the producer caches. Two writes fall outside the runtime directory. The host publishes `records/live-roster.json`, the recovery set a later birth reads ([resume-on-rebirth](#resume-on-rebirth)), with the cache write class because a lost write only narrows one recovery. Ambiguous unstamped pairings append to `audit/binding.log.jsonl` ([rung 3](#the-binding-ladder)). The host also keeps a best-effort status suffix on mux tab names. `cargo xtask invariants` enforces the read-only boundary with `ensure_sidebar_library_boundaries` for the data plane and `ensure_sidebar_renderer_boundaries` for the renderer.

Two commands split a bug between the halves. `rimz sidebar snapshot --json` runs the producer stages below and prints the result: if the wrong answer is already in the JSON, the bug is in the producer. `rimz sidebar frame` renders that snapshot without capturing through the mux, so a correct snapshot with a wrong frame points at `sidebar_pane/`. Both take `--workspace-id`. An id with no state on this machine and no `--session-name` names no room, so both skip the producer and render the empty rollup under the machine config, creating nothing under the state or runtime roots.

## From store to screen

A shared fold feeds per-attachment frames in this order.

1. Fold the event log into the rollup, every durable fact the store holds about the room.
2. Reduce lifecycle events into one `AgentState` per agent, carrying turn, phase, subagents, and model.
3. Reap. Stale sessions drop while the base snapshot assembles (`reap_stale_sessions`), and ghosts and relaunches the rollup still carries collapse against the pane frame (`reap_runtime`) before any pane binds.
4. Bind panes. Read the live pane frame and bind agents to panes. This is the [admission boundary](#presence-model): a pane the rules exclude folds no row, and an agent with no live pane is data, not presence.
5. Classify the rest. A pane no session claims becomes a [process row](#process-rows).
6. Attach children. Subagents nest under their parent rows, and their activity folds onto the parent's displayed clock ([sub-agent lists](#sub-agent-lists)).
7. Group and rank. Rows fold into groups, each row and group gets a score, and the roster sorts.
8. Enrich. Process metrics, provider panels, git and PR facts, spend tallies, and unread state attach to the ordered roster, and the presentation sort runs again.
9. Project to lines. Each attachment resolves the visible roster, composes the zones, and paints.

Steps 1 through 8 run once per host fold and publish into the host's views. Each attachment projects that fold for its own pane when it reduces, fusing shared events and the routed view's focus intent before step 9. The [fold spine](./state.md#the-fold-spine) owns the order inside step 8, and the views' retained fold seeds new attachments ([state.md](./state.md#one-fetch-cycle)).

A sidebar in an unviewed tab does not paint. A settled hidden attachment neither folds nor paints publications, so a capture of a hidden sidebar pane, including in a detached session, shows its last painted frame and not a fresh snapshot. A user jump within the host adds its target to `viewed_panes`, so the destination counts as watched and paints on arrival, before the tab switch. What a hidden attachment still does (heartbeats, wakeup drains, self-close, reload) and how arrival catches up are in [one fetch cycle](./state.md#one-fetch-cycle) and [the paint clock](./state.md#the-paint-clock).

## Where the code lives

The sidebar spans three module trees: the producer's view-model builder, the pane supervisors and painting host, and the data plane.

`crates/rimz/src/store/snapshot/` builds the view-model, in the order of the stages above.

| Module | Owns |
|---|---|
| [`fold.rs`](../../../crates/rimz/src/store/snapshot/fold.rs) | The resumable event-log fold, its persisted rollup cache, and the carryover that survives log rotation. |
| [`project.rs`](../../../crates/rimz/src/store/snapshot/project.rs) | The lifecycle reducer: `agent.lifecycle` events folded into one `AgentState` per agent. |
| [`panes.rs`](../../../crates/rimz/src/store/snapshot/panes.rs), [`panes/lazy.rs`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs) | Pane binding (`PaneBinder`), the own-view summary, unstamped pairing, and idle synthesis. |
| [`process.rs`](../../../crates/rimz/src/store/snapshot/process.rs) | Classifying panes no agent claims: wrappers, launchers, the supervised exec wrapper, agent-kind sniffing. |
| [`view/live.rs`](../../../crates/rimz/src/store/snapshot/view/live.rs) | Folding live panes into rows, plus the local-session and activity enrichments over them. |
| [`view/reap.rs`](../../../crates/rimz/src/store/snapshot/view/reap.rs) | Collapsing ghosts, relaunches, and stale sessions out of the roster. |
| [`view/layout.rs`](../../../crates/rimz/src/store/snapshot/view/layout.rs) | Group resolution (`GroupResolver`), the row rank key, and group comparison. |
| [`view/score.rs`](../../../crates/rimz/src/store/snapshot/view/score.rs) | The fixed-point attention score. |
| [`view/aggregate.rs`](../../../crates/rimz/src/store/snapshot/view/aggregate.rs) | Group assembly, status tallies, displayed-status projection, child attachment and activity folding. |
| [`view/providers.rs`](../../../crates/rimz/src/store/snapshot/view/providers.rs) | The provider dashboard panels. |
| [`assemble.rs`](../../../crates/rimz/src/store/snapshot/assemble.rs) | The read entry points, the persisted snapshot, and its lock-free fresh-latest fast path. |

The contract types are in [`view.rs`](../../../crates/rimz/src/store/snapshot/view.rs) and [`view/model.rs`](../../../crates/rimz/src/store/snapshot/view/model.rs) (`SidebarSnapshot` and its `Sidebar*` members) and [`row.rs`](../../../crates/rimz/src/store/snapshot/row.rs) (`SidebarRow` with its `AgentCard` and `ProcessCard` payloads). `SNAPSHOT_VERSION` gates cross-version adoption.

`crates/rimz/src/sidebar_pane/` holds the supervisors and the painting host. `app/` runs the attachment loops and `render/` paints.

| Module | Owns |
|---|---|
| [`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs), [`host.rs`](../../../crates/rimz/src/sidebar_pane/host.rs), [`attach.rs`](../../../crates/rimz/src/sidebar_pane/attach.rs) | The pane supervisor (build convergence, respawn, pane liveness, self-close confirmation), the session's only painter, and the attach wire between them. |
| [`app.rs`](../../../crates/rimz/src/sidebar_pane/app.rs) | Shared configuration, errors, panic diagnostics, and runtime-file cleanup. |
| [`app/attachment.rs`](../../../crates/rimz/src/sidebar_pane/app/attachment.rs), [`app/loop_state.rs`](../../../crates/rimz/src/sidebar_pane/app/loop_state.rs) | The fixed-timestep attachment loop, its wakeup dispatch, and the pane-local `PaneState` that borrows a client view only through `ViewAccess` ([client views](./state.md#client-views)). |
| [`app/fetch.rs`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs) | The off-thread fetch worker: the 250 ms post-cycle rest, merging of waiting requests, the data backstop, notification state, publication ([state.md](./state.md#one-fetch-cycle)). |
| [`app/views.rs`](../../../crates/rimz/src/sidebar_pane/app/views.rs), [`app/view_state.rs`](../../../crates/rimz/src/sidebar_pane/app/view_state.rs) | Fetch-content retention and replay, client membership, and lazy view reconciliation. |
| [`app/selection.rs`](../../../crates/rimz/src/sidebar_pane/app/selection.rs) | The identity-keyed highlight, the browse layer, and the key and mouse handlers. |
| [`app/input.rs`](../../../crates/rimz/src/sidebar_pane/app/input.rs), [`app/keymap.rs`](../../../crates/rimz/src/sidebar_pane/app/keymap.rs) | The input-socket wire codec and the configurable navigation keymap. |
| [`app/gate.rs`](../../../crates/rimz/src/sidebar_pane/app/gate.rs) | The last-resort hold that refuses a regressive frame. |
| [`app/health.rs`](../../../crates/rimz/src/sidebar_pane/app/health.rs) | Failure debounce and the sticky alert. |
| [`app/lifecycle.rs`](../../../crates/rimz/src/sidebar_pane/app/lifecycle.rs) | The self-close latch and the grow-resize paint hold. |
| [`app/layout_freeze.rs`](../../../crates/rimz/src/sidebar_pane/app/layout_freeze.rs) | The view-owned row and group order freeze. |
| [`app/width_control.rs`](../../../crates/rimz/src/sidebar_pane/app/width_control.rs) | The renderer-local pane width controller. |
| [`host/reload.rs`](../../../crates/rimz/src/sidebar_pane/host/reload.rs) | Detecting that the workspace build target changed. |
| [`view.rs`](../../../crates/rimz/src/sidebar_pane/view.rs) | `VisibleRoster`: body membership, the cap, and stable row ordinals, shared by render, browse, selection, and holds. |
| [`render/ui_state.rs`](../../../crates/rimz/src/sidebar_pane/render/ui_state.rs) | The portable `ViewState` and the pane-bound `UiState` render resources. |
| [`render/compose.rs`](../../../crates/rimz/src/sidebar_pane/render/compose.rs) | Zone composition, scroll resolution, and the bottom chrome. |
| [`render/sections/`](../../../crates/rimz/src/sidebar_pane/render/sections/mod.rs) | One module per zone: `cockpit`, `fleet` (the make-up line), `worktree`, `agent_card`, `process`, `provider`, `pets`. |
| [`render/labels/`](../../../crates/rimz/src/sidebar_pane/render/labels/mod.rs) | The glyph and meter vocabulary every section shares. |
| [`render/interaction.rs`](../../../crates/rimz/src/sidebar_pane/render/interaction.rs) | Typed hit geometry emitted with each painted frame. |
| [`render/theme.rs`](../../../crates/rimz/src/sidebar_pane/render/theme.rs) | Palette depth and the motion modifiers the terminal supports. |
| [`render/animation.rs`](../../../crates/rimz/src/sidebar_pane/render/animation.rs), [`odometer.rs`](../../../crates/rimz/src/sidebar_pane/render/odometer.rs), [`scrollbar.rs`](../../../crates/rimz/src/sidebar_pane/render/scrollbar.rs) | Motion, driven by the wall-clock animation phase. |

`crates/rimz/src/sidebar/` is the data plane: the published caches, the realtime overlay store, and fusion, over the wakeup wire in `crates/rimz/src/wakeup/`. Both are mapped in [state.md](./state.md#where-the-code-lives).

Where to start reading depends on the question:

- Why is this row here: [`view/live.rs`](../../../crates/rimz/src/store/snapshot/view/live.rs), then `panes.rs`.
- Why is this row in this position: [`view/layout.rs`](../../../crates/rimz/src/store/snapshot/view/layout.rs).
- Why does the card look like that: [`render/sections/agent_card/template.rs`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/template.rs).
- Why did the pane close, respawn, or reload: [`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs).

## Presence model

Row presence comes from the live pane frame. The producer enumerates the session's panes, reads each pane's foreground command and cwd, and resolves the cwd to a worktree. The fold emits at most one row per admitted pane, and no pane id is shared by two rows.

[`pane_admits_card`](../../../crates/rimz/src/store/snapshot/panes.rs) is the admission rule. It excludes the caller's own pane, sidebar chrome, and the remote-control and app-server hosts. In the [`rimzd` view](../rimzd.md) it admits only a pane with an agent durably stamped on it, which keeps the dashboard's infrastructure out while letting an agent started there render. Every other pane is admitted. The fold uses the same list for `agent_panes`, so a pane the rule drops is also unreachable by message delivery.

An agent with no live pane has no row. That covers a subagent, a ghost a kill left in the rollup, and a relaunch the [reaper](../agents/instances.md#session-death) has not yet collapsed. Such an agent cannot resurrect a row or latch onto another pane, and there is no `offline` status.

Several root sessions can claim one pane: a relaunch in place, a persistent in-process fork, or a provider that switches conversation ids. A side conversation (`/side`, `/btw`) is not among them: ingestion credits its hooks to the host card ([`crates/rimz/src/store/session_death.rs::side_conversation_host`](../../../crates/rimz/src/store/session_death.rs)). [`view/reap.rs`](../../../crates/rimz/src/store/snapshot/view/reap.rs) collapses a root when another is provably a replacement through [supersession](../agents/instances.md#supersession). Same-instance on-screen evidence lets a `FollowLatest` conversation left mid-turn yield, but never reaps an open ask. Without such evidence, a same-process thread fork survives unless a fallback proof retires it. Among the survivors, [same-pane ownership](../agents/instances.md#same-pane-ownership) picks the one root that owns the card, and every other root's activity folds display-only onto it.

Attention lives on the agent's own row. A blocking prompt puts the session's one row in `? waiting`, so a session never stacks a second row for its ask. When the ask holds and when it releases is in [model.md](../agents/model.md#waiting-and-asks). If the agent's pane closes, the row leaves with it and the durable state stays in the store.

Remote agents do not render, and this is a known gap. An agent that runs only in a daemon with no local pane (`claude remote-control --spawn`, or a Codex thread started from the web) reaches the rollup but has no pane to bind. Showing one needs a presence class that renders from the rollup alone, and none exists.

### The binding ladder

A pane binds by identity first, because its foreground command cannot name the agent on its own: Claude can run under `node`, and two same-kind agents in one worktree read identically. Every `agent.lifecycle` event carries the mux pane id its hook ran inside (`TMUX_PANE` or `ZELLIJ_PANE_ID`). Cwd and command are a guarded fallback, used only when no stamp claims the pane.

[`PaneBinder::resolve`](../../../crates/rimz/src/store/snapshot/panes.rs) walks each admitted pane down this ladder and stops at the first rung that matches. [instances.md](../agents/instances.md#binding-a-session) lists the same joins from the session's side.

1. Stamped root. The pane binds the root agent that stamped its exact id, under three guards:
   - A `session.rebirth` boundary retires every earlier stamp, because pane ids restart on rebirth.
   - A process-start guard refuses a stamp whose recorded pane process started after the live one.
   - For adapters that set `registers_lazily`, the live foreground or hosted process must name the same kind, and the session's activity must be no older than the pane's process start. The activity clock is waived when the agent's runtime owner is the live pane root or a process on the pane's hosted chain (`hosted_agent_lineage`, the pids the producer walked from the root down to the CLI). A launch typed into a pane's shell records its activity before the provider starts, so only process lineage proves its tenancy.

   Stamps come from the hook env or, for a daemon-routed session, from hook ingestion. Before it appends the first lifecycle event, ingestion reads the repaired pane frame and keeps the same-cwd, same-kind panes no live stamp owns. It breaks a tie by client focus, writes one recovered stamp, and appends every attempt to `audit/binding.log.jsonl`.
2. Stamped launched child. A pane stamped by an agent-launched child binds as a nested agent. It stays addressable and renders inside its parent's card ([sub-agent lists](#sub-agent-lists)).
3. Unstamped pairing. [`compute_lazy_agent_pairings_with_index`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs) pairs unstamped root sessions to same-kind, same-cwd candidate panes once per frame. A pane whose command line resumes an exact session (`codex resume <id>`, or the resume syntax the Kiro, Antigravity, and Grok adapters parse) pairs first. The producer reads that command line from the pane root or its sole agent child, at most eight levels down ([`crates/rimz/src/agents/registry.rs::resumed_session_id_for_root`](../../../crates/rimz/src/agents/registry.rs)). Each step skips RimZ helpers and children with no command line and abstains between two candidates ([`crates/rimz/src/proc/command.rs::sole_agent_child`](../../../crates/rimz/src/proc/command.rs)). The walk passes the exec wrapper and wrapper prefixes, and stops at the provider's own process without listing its children. A resolved id is retained only while the hosted process's pid and start remain the same, compared after hosted-stamp carry and pid-reuse validation. Every other outcome drops the prior id: a changed hosted process, a hosted carry that declines (absence proven, carry expired, start regressed, or a foreground agent of another kind), or a pid binding the reuse check clears. Discovery then reads the pane afresh where it still has a root pid and an agent to read; without a proven hosted identity it retries each production rather than carrying a prior id. An agent pane's no-id answer is remembered by the retained producer only after a provider is reached through no agent-classified prefix other than RimZ's exec wrapper. That answer is rechecked when the hosted process's pid, start or command line changes; incomplete walks and other agent-classified prefixes retry each production. The remaining sessions pair newest-first by `last_activity`. Each takes the free candidate whose process started latest at or before the session's first event, otherwise the first free candidate, provided its process start does not postdate the session's `last_activity` ([`pane_start_allows_bind`](../../../crates/rimz/src/store/snapshot/panes.rs)). A session whose stamp names a dead pane enters this pairing only when its adapter sets `registers_lazily`. A choice among several viable panes appends a breadcrumb to `audit/binding.log.jsonl`.
4. Idle synthesis. A recognized agent pane of a wired kind (installed hooks or a declared local-session observation path) renders a synthesized idle `○ <kind>` row ([`idle_agent_row`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs)) until a real session binds.
5. Process row or nothing. Before admission, [`repair_pane_frame`](../../../crates/rimz/src/sidebar/produce/panes.rs) fills a missing command from the pane root pid's command line, falling back to its base command name, unless the read fails or the program label is `rimz`. It also backfills a missing cwd from the root pid. A pane first seen in this produce with a known command but no resolved cwd is quarantined for one frame (a `NewbornQuarantined` diagnostic), so it does not blink in as an ungrouped row. Otherwise a known command becomes a [process row](#process-rows), and a pane with no readable command folds no row.

Providers with their own session stores also bind through local session observations, applied in `view/live.rs`. The rules are in [instances.md](../agents/instances.md#local-session-observations). The cases are pinned in [`view/tests/pane_binding/`](../../../crates/rimz/src/store/snapshot/view/tests/pane_binding) and [`view/tests/lazy_bind/`](../../../crates/rimz/src/store/snapshot/view/tests/lazy_bind).

Rungs 1 and 3 exist for daemon-routed sessions, which arrive unstamped by construction. Codex under remote control fires its hooks from the shared per-user app-server daemon, so the hook env carries no pane id and the hook pid is the daemon's. The session id exists only in the daemon's rollout and socket traffic, never in a client's environ or open files, so no client-pid join is possible ([adapter_codex.md](../agents/adapter_codex.md#session-registration-and-launch-quirks)).

### Honest reads across a mux hiccup

The mux roster can arrive incomplete: a partial pane set, or a live pane missing its command or cwd. Four guards keep a momentary glitch from reaching the screen.

| Guard | Where | What it does |
|---|---|---|
| Repaired fields | Producer and private reads | Joins a raced-null read to the prior process for that exact pane id (retained by the host, read passively by the CLI), and backfills a missing command and cwd from the process backend (ladder rung 5), so a new shell has a row under its worktree on first appearance. |
| Carried panes | Producer | A pane omitted by a cached read, or whose whole tab is omitted even by an authoritative server listing, carries forward while a matching non-zombie process proves its root alive. A pidless sibling in a tab with confirmed liveness carries through tab coherence, and a proven-dead sibling does not. A server listing refutes missing siblings in tabs it lists, even while their roots live. The renderer's own pane can carry across either read by identity. Surviving carries publish `pane_carry_forward`, with `cli_confirmed` reflecting the listing's authority ([diagnostic readings](../diagnostics.md#carry-kinds)). Sibling carries expire after `PANE_CARRY_TTL` (30 seconds). The renderer's own carry never expires. |
| Nameless and newborn panes | Projection | A pane with no readable command, or a pane first seen this produce without a resolved cwd, folds no row this frame instead of an anonymous row (ladder rung 5). The next read names it. A stamped pane still binds by id and keeps rendering. |
| Last-resort hold | Renderer | Absorbs a residual race that would erase or demote a stable row set. It holds an agent-to-process demotion only while the foreground command is unchanged or missing. A changed command means the agent exited and the shell returned, which commits at once. |

The producer repairs live in [`sidebar/produce/panes.rs`](../../../crates/rimz/src/sidebar/produce/panes.rs) and the hold in [`app/gate.rs`](../../../crates/rimz/src/sidebar_pane/app/gate.rs). The fusion rule that lets a real close override a carried pane is in [state.md](./state.md#fusion-rules). Carried panes and gate holds show as dim bottom notices and do not count as health failures. When pane discovery fails outright, the renderer keeps its last good frame and eventually raises the health alert ([degraded reads](#degraded-reads)).

## Ranking and grouping

### Worktree groups

A group is one unit of isolated work, drawn as one block under its header. The header carries no status tally: the cockpit owns the fleet make-up and each row carries its own glyph. A live or process-only group's header is a jump target. In a finished group with any agent, the header is an expand and collapse toggle.

Git stats live on the header and never on a row, so changes in a shared worktree never read as one agent's. The `+/-` churn is the worktree's change against its merge-base with trunk, and the `⇡`/`⇣` delta is the commit count over the same base.

The trunk glyph takes the first of three sources that has a verdict:

1. Reconciling: a local rebase or merge in flight.
2. The forge PR verdict: merged, closed, or open. An open PR with a merge-queue fact swaps its glyph for the queue glyph, in the open tone while queued and in the failing-CI tone once dequeued.
3. The local trunk relationship: merged, pristine, or a plain branch.

A pristine fork reads `≡ <trunk>`, and a merged PR shows the merge marker alone even when squash-merge ancestry leaves the local branch diverged. The trunk checkout keeps the plain branch glyph and carries no PR verdict. Trunk resolves per repo: `[sidebar] trunk` ([configuration.md](../../guide/configuration.md#sidebar-rendering)), then `main`, `master`, and the remote's advertised default. The facts come from [`refresh/git_stats.rs`](../../../crates/rimz/src/sidebar/refresh/git_stats.rs) and [`refresh/pr.rs`](../../../crates/rimz/src/sidebar/refresh/pr.rs). The landed verdict is in [worktrees.md](../harness/worktrees.md#what-the-sidebar-asks), and the look is in [the interface reference](../../interface/sidebar.md#worktree-headers).

[`GroupResolver::resolve`](../../../crates/rimz/src/store/snapshot/view/layout.rs) places a row by the first rule that matches.

| Row | Group |
|---|---|
| Carries a channel | That channel's pod. |
| Has no worktree path | A group named by its branch if it has one, otherwise `external`. |
| Path inside the repo's RimZ-owned worktree home | That worktree's `#channel` pod, matching message addressing. |
| Path inside a group root | The deepest containing root: a checkout in a repo room, a name-only root pod for the root of a directory room. |
| Path outside every root, in a room with no roots at all | Its own group per path. |
| Path outside every root, with a branch | A group named by the branch. |
| Anything else | `external`. |

Group roots are the repo's checkouts from `git worktree list` in a repo room. A directory room does not scan its tree. Every row that reports both a path and a branch also adds its path as a root, and rows on several branches at one path split by branch. The `external` group (a home shell, `/tmp`, CI) renders as a dim divider, always sorts last, and keeps an attention-only tally so an out-of-project ask still surfaces. The rules are pinned in [`view/tests/grouping/`](../../../crates/rimz/src/store/snapshot/view/tests/grouping).

### Team units and pipelines

A named team inside one group counts as one unit, and [`SidebarWorktreeGroup::unit_statuses`](../../../crates/rimz/src/store/snapshot/view/model.rs) gives its verdict for the cockpit, the lens, and the banner. Every member can add attention. The resolved stage owner alone decides the calm verdict: board `Done` gives success, and a successful owner before `Done` gives idle. A team with no owner prefers running, then sleeping, idle, and success. Attention suppresses every calm verdict except running. The rows that carry a bucket are the attention rows, the stage owner for calm, every member for board `Done`, or the matching rows when there is no owner. Only the cockpit suppresses success for a finished group.

A staged team's group carries `SidebarWorktreeGroup.pipeline`, projected from the producer's board-derived `lanes/pipeline.json` ([the pipeline lane](./state.md#workspace-enrichment)). The renderer draws it as one line below the header: the team badge, one dot per declared stage with the board's stage name and stage duration after the current dot, and the total run duration pinned right. `SidebarPipeline::position`, `span_secs`, and `total_secs` own the stage and clock logic, and future dots read `visited` for the `PipelineRevisited` tone. The current dot and name use the owner's status style at the shared animation phase. `SidebarWorktreeGroup::pipeline_owner` matches the group's team and card handle, and the verdict, the stuck projection, and the renderer all read that owner.

The pipeline line adds no attention and adds no selectable row. Its click targets the owner row if visible, else the first actionable visible team row, else the first visible row. A folded finished group omits the line and appends ` · <stage>` after the team name on its roster line. Layout and glyph states are in the [interface reference](../../interface/sidebar.md#worktree-headers).

A stage that has stopped moving reads as failed. After pipelines attach, [`SidebarSnapshot::project_stuck_stages`](../../../crates/rimz/src/store/snapshot/view.rs) projects the resolved owner to `Failed` when its unfinished unit has only idle or successful member attention statuses (current-child lifts included) and the latest member activity or stage start is at least `stalled_after_secs` old. The label reads `stage <Stage>: nothing running <age>`. The projection re-stamps attention scores, counts, and the finished verdict before the presentation sort, unread reconciliation, and notification evaluation. It changes no durable state, and the next fold clears it once member activity, a non-resting member, or a stage flip breaks the condition.

### Attention ranking and the cap

Within a group, agent cards lead and process rows form the tail. Rows order by three age bands, then by a fixed-point score inside each band. Read state never changes the band. Unread drives the wash and blink emphasis, the jump inbox, notifications, and the cap exemption, and nothing else.

| Band | Membership | Score inside the band |
|---|---|---|
| hot | Every alive `waiting` or `failed` row, plus other rows with `last_activity` inside `[agents.attention] inactive_after_secs` (default one hour). | Attention rows (`waiting`, `failed`, `paused`) heat from 1.0x to 2.0x across the window, and `waiting` and `failed` hold 2.0x afterward. Calm rows stay flat and keep pane creation order. |
| warm | Calm and `paused` rows past the inactive window, inside `archive_after_secs` (default 24 hours). | Decays from 1.0x to 0.0x without competing with hot work. |
| archive | Calm and `paused` rows past the archive window. | Flat status weight. |

The score is status weight times that time factor, both fixed-point ([`score.rs`](../../../crates/rimz/src/store/snapshot/view/score.rs)). The weights are spaced so the lowest attention state outranks the highest calm state. Why each status weighs what it does is in [model.md](../agents/model.md#status-and-phase). Process rows sit outside the bands: their clock is the foreground process start, so an idle shell stays live and seats below every agent card. A `paused` row resumes on its own through [auto-continue](../agents/providers.md#auto-continue).

Co-launched agents hold one contiguous block. A named-team launch and an inline multi-agent layout render as one cohort inside their group, teams in declared role-list order and inline launches in agent-cell order. The block takes its band from its members and its urgency from the first case that matches ([`CohortAccum::finish`](../../../crates/rimz/src/store/snapshot/view/layout.rs)):

| Members | Block urgency | Tie-break |
|---|---|---|
| Any `waiting` or `failed` | The strongest actionable member's score. | Oldest attention clock. |
| Any `paused` | The `paused` weight at the oldest paused member's time factor. | Oldest attention clock. |
| Otherwise | `running` if any member runs, else `success` if any succeeded, else `idle`, at the most recently active member's time factor. | Earliest pane creation order. |

Groups rank in this order ([`compare_groups`](../../../crates/rimz/src/store/snapshot/view/layout.rs)):

1. `external` groups last, whatever they hold.
2. Finished project groups after all unfinished project groups, including archived ones.
3. The band of the group's liveliest member.
4. The highest attention score among members in that band. Calm members add nothing here.
5. The group's calm activity in that band: working if any member runs, else finished if any member succeeded, else idle if any agent remains, else process-only.
6. Git: dirty, clean, unknown, then done. Done needs a merged or closed PR or a `WorktreeTrunkSync::Merged` verdict, so a pristine fork whose merge-base equals `HEAD` stays clean.
7. The earliest member's pane creation order, then the label.

A done group with no attention or running member enters the finished partition, and a revived member restores its unfinished rank. The presentation sort and each live overlay refresh stamp that verdict into `SidebarWorktreeGroup::finished`, so ranking and rendering agree. The seams are `sort_rows`, `compare_groups`, and the rank key in [`layout.rs`](../../../crates/rimz/src/store/snapshot/view/layout.rs), pinned in [`view/tests/ranking/`](../../../crates/rimz/src/store/snapshot/view/tests/ranking). The user-facing account is [the sidebar guide](../../guide/sidebar.md#how-the-column-is-ordered).

The cap is renderer-local and protects visible work. The snapshot carries every row. In the resting body each renderer shows at most `WORKTREE_ROW_CAP` (6) rows of a group ([`VisibleRoster`](../../../crates/rimz/src/sidebar_pane/view.rs)). Exempt rows always show: unread rows, agent rows in any status but `idle`, the focused row, rows the layout freeze holds, and the process row that is the group's only live member. They count toward the six, and the idle and process rows fill what is left in rank order. Six running cards therefore leave no room for an idle tail. The rest fold behind a dim `+K more`, and expanding the group shows every row plus `− less`. A lens or a search query lifts the cap.

### Finished groups

A finished group with any agent row collapses every row, unread successes included, behind a two-line dim receipt. A process-only group stays expanded.

- The `▸` roster line leads with the shared `AgentCard::team` value when present, then each agent's final status glyph and name in source order. It folds overflow and process rows into `+n` and pins the cohort's lifetime transcript-priced cost right. Before `cohort_effort` exists, it sums the hidden agents' session costs instead, and published effort wins even when its cost is unknown. Too narrow for one member, it reads `▸ +K done`.
- The totals line carries the lifetime `◇ ↘ ↗ ◌` token split and pins retained active time right, falling back to last-activity age once the active-time sidecars expire.
- The producer publishes the group totals and per-seat lifetime effort, keyed by row id, through the 60-second `lanes/cohort-spend.json` lane, and the renderer only formats them. The figures fold the same lifetime-admitted audit slots as [attribution](../agents/attribution.md#selecting-records), so a receipt counts only the checkout's current life.
- A revealed card from a finished population shows the seat's lifetime cost, so the cards sum to the receipt. A live card shows its session-scoped self-report.

A folded header and both receipt lines carry `HitTarget::ExpandGroup { key, row }`. The row names the first hidden agent in the same `finished_members` order that draws the receipt, even when no chip fits. The click inserts the expansion key, selects and focuses that member's live pane, and sets `ViewState::reveal_row` so the next paint reveals its card. An expanded header keeps `ToggleGroup`, which collapses without moving focus, and live `+K more` and `− less` tails use the same target. Both group targets have precedence 3, above the header hyperlink. Focus or the layout freeze on any member also reveals the whole group, and the collapse lands as a unit once focus leaves and the hold expires. A lens or query reveals matching rows uncollapsed, except that the success lens excludes finished groups. Reveal state belongs to the [client view](./state.md#client-views).

The landed chip folds the whole finished partition behind one line. `scroll_lines_windowed` in [`render/compose.rs`](../../../crates/rimz/src/sidebar_pane/render/compose.rs) places it centered between dotted rules, above the first finished group. `ViewState.landed_open` defaults closed and travels with the client view, and the chip's whole-line `HitTarget::ToggleLanded` makes the scroll offset authoritative and cancels easing like a group toggle. While closed, composition skips only finished groups with no projected rows, so focused, held, and individually expanded groups stay visible, and `VisibleRoster` and its ordinals do not change. Opening arms the one-paint `ViewState::landed_reveal`, and `resolve_scroll_offset` nudges the offset just enough to show the chip and the first revealed receipt together. A lens or query suppresses the chip and the skip. `render_expanded_line_ansi` opens it with every group, and fixed frames keep the closed default. The words, glyphs, and tones are in [the interface reference](../../interface/sidebar.md#row-cap-and-finished-groups).

### The layout freeze

The layout freeze keeps rows still after an interaction. Producer rank is the truth, but after a jump or browse, or when the focused agent's ask is answered or its turn starts, the view keeps the last painted row and group order and that frame's visible rows. Read state and content still update at once. [`LayoutFreeze`](../../../crates/rimz/src/sidebar_pane/app/layout_freeze.rs) belongs to the client view, not the pane.

`LayoutFreeze::armed_ms` starts at the interaction. The deadline is derived at each check: `LAYOUT_FREEZE` (5 seconds), or `LAYOUT_FREEZE_PARKED` (30 seconds) while any of this view's clients focuses its painting sidebar pane. A held or unpainted view is never parked, and leaving the pane restores the five-second deadline. Focusing the sidebar within an already-viewed tab never arms the freeze on its own. An arrival without a hold arms one over the view's last painted order, or over its first frame's order if it has never painted. Hidden folds never replace the painted order, and joining, forking, copying, or a same-host jump landing never restarts an existing hold.

Inside a hold, group order is kept within the current unfinished and finished partitions, with `external` last, so landing or reviving work crosses the partition boundary at once. Held frames match rows by id, then by pane id, so a launch-to-session rekey keeps its slot. A new row appends within its group, and a new group appends after live groups but before finished and external groups. On expiry, order, cap exemptions, and the open cards of previous selections release together in the next content fold and paint.

## The cards

Each agent is a small stacked card, drawn in [the interface reference](../../interface/sidebar.md#the-card). A row is base identity plus one payload: [`AgentCard`](../../../crates/rimz/src/store/snapshot/row.rs) for lifecycle and context fields, or [`ProcessCard`](../../../crates/rimz/src/store/snapshot/row.rs) for command and process metrics.

### The line template

A card's slot skeleton is fixed before any content fills it, and optional slots render only when their content applies. [`template`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/template.rs) maps four inputs to an ordered list of slots:

- Stage (`CardStage`). A card is `Fresh` while it is `idle` with no submitted prompt, no session history, and an empty context gauge. `labeled` means it carries a RimZ-authored description instead of the compose affordance. Otherwise it is `Engaged`. The renderer marks a read card `Stale` when its child-folded `last_activity` exceeds `[theme.display] stale_after_secs` (default 86400, `0` disables), unless reach, an open delegation section, or `expanded` density keeps it open. Only idle and success rows use the stale template. `CardStage::resolve` is that one decision, and both `template` callers (`row_lines` and the frame-rate check `delegation_motion`) read it.
- Expansion (`CardExpansion`). `reach` (`SelectionReach`) is `Selected` for the live selected row, `Held` for previous selections the freeze holds, and `Unreached` otherwise. Selected and held rows open their own engaged cards with stats, and teammates stay unreached. A fresh card opens its meter and compose dots only for the live selection. `delegation` (the sticky per-row override, else an opened card or `expanded` density) decides whether the delegation entries show.
- Density: `[theme.display] card_density`, one of `auto`, `expanded`, `compact`.
- Status, read by the stale, resting compact, and resting engaged arms.

| Card | Slots |
|---|---|
| Stale idle or success, unreached, density not `expanded` | stale |
| `compact`, unreached, `idle` | identity |
| `compact`, unreached, `running` or `waiting` | identity, description, question, gauge |
| `compact`, unreached, `paused`, `success`, `failed`, or `sleeping` | identity, description |
| Fresh unlabeled, not live selected | identity |
| Fresh labeled, not live selected | identity, description |
| Fresh unlabeled, live selected | identity, awaiting dots, question, gauge |
| Fresh labeled, live selected | identity, description, question, gauge |
| Engaged, unreached `idle` or `success`, delegation closed, density not `expanded` | identity, description, question, gauge |
| Engaged, otherwise unreached, density not `expanded` | identity, description, question, gauge, delegation |
| Engaged, selected or held, or `expanded` density | identity, description, question, gauge, tokens, cache, delegation |
| Engaged, delegation open | The applicable engaged shape plus delegation entries. |

A unit test pins every combination. Under `auto`, selection leaves the lines above the meter in place, inserts the stats lines below it, and appends delegation entries. A held selection keeps both additions until the layout freeze expires. `expanded` density keeps the stats lines on every engaged card, teammates included. Compact resting cards are shorter, and opening one restores its stage's shape. Selection styling stays on the focused row.

Expansion holds belong to the client view. They accumulate across clicks while `ViewState::freeze` is armed and release together when it clears. Composition builds the retained opening set before it measures the scroll zone, independently of clamp geometry or authoritative scrolling. An off-screen selection reveals only after this sidebar's own input.

The `Cache` slot appears wherever `Tokens` does. `row_lines` adds no live-selection gate, so a held card keeps its cache notice as well as its stats. `CacheAge::phase` is the one expiry decision for the age pin, the meter, the cache line, and the stale line's age: held, unknown, calm, closing, expired. The closing band is the larger of half the ceiling and `min(600, ceiling_secs)`, capped at `CACHE_WARNING_MAX_SECS` (one hour) so a long lifetime warns only in its last hour, and expiry stays strict `age > ceiling`. The line reads only the producer's optional `rewarm_cost_usd`, formatted through `theme::fmt::dollars_estimate`, and performs no pricing or I/O. The [interface cache line](../../interface/sidebar.md#the-card) owns the words, widths, and tones.

### Delegation entries

The `Delegation` slot is one standing line with the lifetime child count and cost, and the count of armed one-shot waits, background shells, and a pending idle stop. It renders empty until any of them exists. `DelegationEntries` lists, in order:

1. Subagents, in bands ([`bands.rs`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/bands.rs)).
2. Pending waits from the loop catalog, with background shells (from the rollup's durable `background_shells`, folded from adapter hook reports, [model.md](../agents/model.md#turn-endings-and-parked-turns)) merged in after the command and PID waits and ahead of signals.
3. The pending idle stop (`AgentCard.idle_stop`), attached at enrichment by `harness::idle_stop::project_requests` with its due time while the rest clock runs, so the renderer reads no record.
4. An optional `+K older` tail.

The bands classify `sub_agents` by status and the snapshot clock, never by the snapshot's `prior_turn` tag. Live children (not `success` or `failed`) come first in the projection's spawn order. Finished children follow by `last_activity` descending while younger than `[theme.display] recent_subagent_secs` and up to `max_recent_subagents`. The older rest shows when history is open, or when the live and recent bands are both empty, since a lone tail would only cost a second click. Every band renders on one token and model column grid measured over all children, so opening history never re-pads rows already on screen. The muted `  +K older` tail shows while history is closed, at least one live or recent row is visible, and `K = sub_agent_count − rows` is positive. Children the projection dropped count in `K` with no row to reveal. The shorter list is a prefix of the longer one, and a finishing child moves only from the end of the live band to the head of the recent band.

Subagents and waits share [`push_entry`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/entry.rs): a lead, a type word, an optional headline separated by `GlyphRole::Seam`, pinned right spans, and an optional prebuilt detail line. [`PendingWaitTrigger`](../../../crates/rimz/src/agents/state.rs) supplies `kind_word`, `headline`, and `detail`, and the renderer owns the separator and layout. Subagents supply their metadata grid as detail, and waits supply command or deadline text. The [interface entry table](../../interface/sidebar.md#subagents-and-waits) owns the per-kind line rules.

Two presentation states in the portable `ViewState` steer the entries. They travel with the client view, not through producer folds.

- `delegation_overrides` maps a row id to open or closed. The delegation line carries `HitTarget::ToggleDelegation { row, open }` with the state its click sets, so a closed selected card and an open unselected one are both one click away. The override outranks selection and density, survives prompts and selection moves, and is pruned only when the row leaves the snapshot.
- `delegation_history` maps a row id to the parent's `user_turn_started_at` at the `+K older` click (`HitTarget::ToggleDelegationHistory`). A changed turn stamp makes the entry inert at once and pruning removes it, so the next user-authored prompt folds the history, while automatic deliveries leave it open. Closing the section clears it too.

Both clicks pin manual scroll, then select and focus the parent's own live pane, not a child's. Without a parent pane they only toggle.

Two frame-rate rules follow from the slots. The serve loop holds the breath cadence while a visible selected card shows the unlabeled compose affordance. It holds the fast grid while a visible card's open delegation entries hold motion: a running child's working or thinking head, or a running shell job's working lead. An idle or sleeping child's status head holds nothing, and timer and signal leads are static. The parent's own status does not matter here.

### What fills the slots

Model and context enrichment is display-only and privacy-gated. Model and reasoning effort come from the session's [rich context](../agents/model.md#rich-context) or the hook and store scalars, and a missing field means the agent did not report it, never zero. Wait and child projections also determine the displayed work status, and attention follows the [status ladder](../agents/model.md#the-sidebar-ladder). Account-scoped budgets stay off the card, in the [provider dashboard](#provider-dashboard).

Context severity is one four-tier ramp, classified once in the domain ([`ContextSeverity::classify`](../../../crates/rimz/src/agents/state.rs)) and stamped on the row. The renderer maps the row's position between the configured stops ([configuration.md](../../guide/configuration.md#sidebar-bands)) to a continuous tone. `NO_COLOR` removes color only, and each bar's shape still carries the meter.

Three projection rules are not evident from the field catalog:

- The renderer prefers the rich `context` blob over the coarse scalars, except that `row.effort` wins over `context.effort`.
- A waiting card's description prefers `AgentCard::open_ask`: the first sentence ending in `?`, else the whole ask, word-wrapped over at most two indented lines with `…` for overflow or hard-cut words ([the visible rule](../../interface/sidebar.md#the-card)). `description_lines` shares `layout::wrap_words` with pet captions, marks the wrapper's dropped long-word tails, and keeps the usual emphasis. Other descriptions stay one line and use [`AgentCard::description`](../../../crates/rimz/src/store/snapshot/row.rs): the displayed error or pause label first, then the first Working-class wait while running with an idle or parked own turn, or the first Sleeping-class wait while sleeping, then activity (session name, provider thread preview, launch description, task, first prompt, latest prompt). Codex's generated title replaces its first-message preview.
- Display preferences travel in `SidebarSnapshot.sidebar`.

The status projection is pinned in [`view/tests/status/`](../../../crates/rimz/src/store/snapshot/view/tests/status) and [`project/tests/`](../../../crates/rimz/src/store/snapshot/project/tests).

### Unread and read receipts

Unread state is a runtime episode set (`lanes/unread.json`) plus one room read-mark record (`live/read-marks/marks.json`). The host opens an episode for a row whose displayed status is `success`, `failed`, `waiting`, or `paused` when no read mark reaches the row's `last_activity`, and every fold derives `SidebarRow::unread` from that file and the current marks. The cap keeps unread rows visible, so a row that returns to `running` or `idle` stays emphasized until read. Attaching to a busy room opens the current attention rows silently, without a burst of notifications.

| Action | Effect |
|---|---|
| Focusing a row's pane | Writes a receipt and clears that row. |
| Staying in a tab for `TAB_READ_DWELL` (2.5 seconds) | Clears the unread rows in that tab. Leaving earlier clears nothing. |
| `m` on an unread row, or `rimz sidebar mark-read` | Writes a manual receipt without focusing. |
| `M` | Writes manual receipts for every readable row. |
| `m` on a read row, or `rimz sidebar mark-unread` | Opens a fresh episode through [`sidebar::unread::mark_rows_unread`](../../../crates/rimz/src/sidebar/unread.rs), stamped at `max(last_activity, now)` so no existing receipt reaches it. |

Every writer max-updates the latest clear time per row under `read-marks.lock`, and a receipt clears an episode only when its clear time is at or after the episode, so an old focus cannot erase a later turn. Marks outlive the pane that wrote them, manual marks included, until the fetch worker prunes their rows against a produced room frame. Writes use cache-class temp-and-rename without fsync. If `marks.json` is absent, readers import the maximum of the legacy `manual.json` and valid `sidebar.<instance>.json` receipts with their original timestamps. The fetch worker's first prune, or an earlier changed write, materializes that import. Legacy receipts are reclaimed only by class-TTL GC, not by heartbeat expiry.

### Sub-agent lists

An expanded card lists two kinds of child: provider-native subagents and pane-backed agents another agent launched ([the two meanings of subagent](../harness/subagents.md#two-things-are-called-a-subagent)). Neither renders as its own row while its parent's launch has a rendered row. The rollup carries each child's root `parent_agent_id`, and projection nests every descendant in one flat list. A launched child's link names the parent's launch and a native child's names the parent session. [`AgentState::parent_is`](../../../crates/rimz/src/agents/state.rs) resolves both, and a cross-provider launched child also carries its parent's kind, so mixed links land under one card. The list orders by `registered_at`, and Codex's root-relative task path keeps nested lineage in the label. A launched child's pane stays addressable, but its redundant process row is suppressed.

A child that no rendered parent row claims is an orphan. A pane-backed launched child is promoted to its own top-level row, because its pane is live and would otherwise render nowhere. A paneless native child does not render and is logged ([subagents.md](../harness/subagents.md#who-counts-as-a-launched-child)).

The parent's user-turn boundary decides which children are current. [`attach_sub_agents_indexed`](../../../crates/rimz/src/store/snapshot/view/aggregate/subagents.rs) keeps every child the store still retains, and marks a child from before the boundary `prior_turn`. A prior-turn child stays listed only with its `success` or `failed` verdict, and other prior-turn children drop from the list. The renderer's bands ignore the tag, so a finished child keeps its `✓` or `!` in the recent or older band. Readers of `AgentCard::current_sub_agents` (the attention lift and pane below) skip it. A live launched child is always current. Otherwise the boundary moves as follows:

- A prompt RimZ delivers for an agent or the harness (`AGENT_MESSAGE`, `AGENT_REPORT`, `SUBAGENT_REPORT`, `TEAM_REPORT`, `WAIT`, `SIGNAL`) opens a provider turn (`turn_started_at`) but keeps the children current. The classifier reads the durable submitted prompt, and the prompt counts as harness-delivered only when every segment carries a non-user message header. `USER_MESSAGE`, bare leading composer text, or a missing prompt count as the user's, so headerless `--no-from` text and the budget-continue prompt move the boundary. Text typed after a pasted header's body cannot be told apart from that body and stays harness-delivered.
- A fresh user prompt, `/clear`, and manual `/compact` each move the boundary. Automatic mid-turn compaction resumes the same turn and keeps the children current.
- With no known boundary (including carryover older than `user_turn_started_at`), a child stops being current after three hours of inactivity, the ghost-session TTL. A running child past that TTL, or one a newer user turn superseded, is dropped as reaped. A known stamp survives log rotation.
- A launched child observed to end mid-turn (deadline, `rimz subagents stop`, or provider death) resolves to `failed` and shows `!` ([model.md](../agents/model.md#the-state-machine)). A reaper-inferred end rests it at `idle` with the calm `○` ([observed and reaped ends](../agents/model.md#observed-ends-and-reaped-ends)). Either way its elapsed time freezes at the end stamp.

A current waiting child of either origin lifts the parent's attention to `waiting`, unless the parent already has its own actionable status. A provider-native child's failure also lifts, and a launched child's failure or silence does not, because the native child has no pane of its own. [`SidebarRow::attention_status`](../../../crates/rimz/src/store/snapshot/row.rs) carries the lift into cockpit counts, filters, ranking, the inbox walk, and the CLI needs-you projection, without changing the parent's displayed status. Without the lift, a nested child's prompt would be absent from those surfaces. [`SidebarRow::attention_pane`](../../../crates/rimz/src/store/snapshot/row.rs) sends `n`, `N`, and `Space` to the first current waiting launched child with a live pane, in entry order. Own attention, native-only lifts, and paneless children keep the row's pane, a click on the parent still targets the parent, and the next walk step advances from the parent row that owns the focused child's pane. Notifications, unread episodes, tab suffixes, and lead glyphs read the parent's own displayed status.

Only a displayed `running` child holds the parent as delegating, including its waiting pet and stall exemption. A live launched child's provider park instead projects the parent card as `paused`, and the [status ladder's demotion](../agents/model.md#the-sidebar-ladder) turns both the entry and the parent `failed` when the park is spent.

An open child that goes quiet takes the stall rung. [`AgentState::silent_child_for`](../../../crates/rimz/src/agents/state.rs) matches running silence, or a prompted launched child whose first turn never started (raw and effective `Idle`, no `turn_ended_at`), and excludes sleep, pause, and displayed errors. Both cases use the configured stall window. `project_sub_agent` in [`view/aggregate/subagents.rs`](../../../crates/rimz/src/store/snapshot/view/aggregate/subagents.rs) projects a match to `Failed`, phase `Idle`, label `silent Nm`, and `SidebarSubAgent.stalled = true`, with its runtime frozen at the last activity. `sub_agent_finished` in [`agent_card/mod.rs`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/mod.rs) excludes that marker, so the line stays in the live band. Fresh activity clears the projection. Only launched children get the parent notice, owned by [the harness backstops](../harness/subagents.md#backstops).

Projection de-duplicates every retained child before the currentness filter and computes `sub_agent_count` and `sub_agent_cost_usd` across both origins. It also computes the launched-only `delegated_cost_usd`, which is added to the parent's own session cost. The all-origin figure is only a breakdown, because native child spend is already in the parent's transcript.

Each origin fills its entry from a different source:

| Child | Source |
|---|---|
| Claude native | Description, exact cost, start time, and the newest request's token split from the child transcript, folded on by [`with_subagent_context`](../../../crates/rimz/src/store/snapshot/view.rs). |
| Codex native | Child lifecycle observations: nickname, task path, role, model and effort, current context tokens, and the reported context window, with durable registration as the elapsed-time fallback. |
| Copilot native | The model from the parent's start record, with the cumulative `totalTokens` reconciled from the completion record at the next parent checkpoint. |
| Launched | Its launch profile and session-sidecar cost. Window occupancy through `context_used_tokens`, else the cumulative `session_usage` total of input, cache writes, and output, excluding cache reads. |

Token selection prefers `AgentState::context_used_tokens()` (`SubAgentTokens::Window`), then a positive `session_usage.displayed_total_tokens()`, then `usage.run_total_tokens` (`SubAgentTokens::Total` for either cumulative source), and otherwise shows no token cell. Claude's statusline hybrid counter is not a source. The incremental cost cursor keeps the latest request split even without pricing and fills only the fields lifecycle usage lacks, so the split that `SubagentStop` records wins once the child stops.

Window-token entries carry `context_window` from the child's context sidecar, else its lifecycle usage, and the provider's default window is never substituted. The glyph uses the worse of percent and token heat when a nonzero window is reported, and token heat alone otherwise. Total-token entries carry no window. A running Claude native child reports no window size, so it keeps token-only heat until `SubagentStop` names its model and the lifecycle hook resolves that model's window (the `[1m]` marker, else the price book).

`project_sub_agent` copies `kind` and `compaction_count` from the child's own `AgentState` for every origin. It sets `fast` only for launched children, from the child's `AgentState::fast_mode()` (the live reading over the launch fallback), and provider-native children always project false. Claude native, Codex native, and launched children have child-attributed compaction sources. Copilot native children have none and never show a count, so a missing count there proves nothing. The type word's brand tone (`Theme::body_brand`), the fast bolt, and the compaction count lay out as the [interface entry table](../../interface/sidebar.md#subagents-and-waits) draws them. The bolt follows effort, or the model label when effort is absent. The per-card model column counts only model labels and reserves no bolt cells, and neither the model nor the effort shortens to admit the bolt.

Child activity counts as the parent's. [`fold_child_activity_onto_parents`](../../../crates/rimz/src/store/snapshot/view/aggregate/subagents.rs) advances the parent row's displayed clock to the freshest child `last_activity`, so the stall check does not fire and the inactive sinks, ranking, and unread do not move while children work. The clock it replaces is recorded on the card as `own_last_activity`, whose one reader is the card's cache-age pin through `SidebarRow::own_last_activity`: a parent blocked on its children's reports makes no model call, so its prompt cache ages for the whole wait. A child still displayed `running` also makes a resting parent display running or delegating. A child holding a terminal verdict or projected stalled never does, and a stalled launched child's silence never lifts the parent's attention. A silent raw-running parent can then take its own root stall rung, and an idle or sleeping parent shows its own rest. The fold is display-only: the rollup keeps the parent's own clock, a blocked or budget-parked parent stays put, and own or delegated pause or failure evidence beats a ticking child. The cases are pinned in [`view/tests/subagents/`](../../../crates/rimz/src/store/snapshot/view/tests/subagents).

## Process rows

A pane no agent session claims renders a command row, one tier below the agent cards. It carries no capability line, enters no cockpit tally, and is still a jump target. [`row_from_process`](../../../crates/rimz/src/store/snapshot/process.rs) builds it. An idle row is one line, the lead glyph and the program (`zsh`, `vim`, `cargo`). A busy row anchors its first line on the pane root's process name (the shell that owns the pane), so the line stays put as commands come and go, and puts the foreground command on a second line when it adds something past that label.

The program label comes from the mux command through the shared parser in [`proc/command.rs`](../../../crates/rimz/src/proc/command.rs). The parser skips environment assignments and transparent wrappers (`sudo`, `doas`, `exec`, `command`, `nohup`, `nice`, `time`, `timeout`, `stdbuf`, `env`, and `sh`, `bash`, `zsh`, `dash`, or `fish` with `-c`). It projects a `node`, `nodejs`, `npx`, or `bun` script, or a supervised `rimz agents exec`, to its kind while keeping the launcher as the identity-bearing root. Shell script selection is lexical: the first simple command that is not assignment-only or `cd`, `export`, `set`, `source`, or `.` setup. The parser evaluates no expansions, redirections, or control flow, and it keeps the wrapper as the label on unsupported syntax, an incomplete wrapper, or exhausted nesting. Source spans let the details line shorten only the resolved binary's absolute path.

A shared-runtime basename such as tmux's bare `node` resolves to an agent label only when the producer proves a known CLI by walking one bounded single-child chain from the pane root over full `/proc` command lines ([instances.md](../agents/instances.md#recognizing-a-hosted-cli)). A chain that is unreadable, branches, lacks a start time, runs too deep, or matches nothing keeps the runtime label, and the proof never rewrites the mux command. On muxes that report only `comm`, even idle-looking shells are probed, and the matched `foreground_cmdline` feeds activity and details, never identity.

The lead glyph is `ProcessState`: `○` when idle, a braille spinner when busy, and an attention `!` when the process is stuck. [`process_state_from_tree`](../../../crates/rimz/src/sidebar/produce/metrics.rs) calls a process stuck when it is a zombie in two consecutive samples, or in uninterruptible sleep (`D`) for `PROCESS_D_STATE_STUCK_AFTER` (10 seconds) with no CPU or I/O progress. A foreground-command change resets the baseline, so a previous command cannot mark its replacement stuck. A working row wide enough right-pins a CPU, RSS, and I/O grid, and an idle shell stays bare.

Identity and metrics key off the pane's root pid. tmux reports it as `#{pane_pid}`. The Zellij presence plugin publishes it from `get_pane_pid` and follows `CommandChanged` and `CwdChanged`, so shell topology stays current without synchronous discovery. [`backfill_zellij_pane_pids`](../../../crates/rimz/src/sidebar/produce/metrics.rs) falls back to foreground-cmdline matching for older plugin builds and failed pid lookups. It abstains when the match is ambiguous (two idle shells in one cwd), because no stats beat another pane's stats.

A proven agent label changes a process row's class only through wiring. Once the adapter is wired, the pane becomes an idle agent card ([ladder rung 4](#the-binding-ladder)), and the first lifecycle event binds the session row. Without integration it stays a process row with the agent label.

When the foreground command runs through `sudo`, `su`, or `doas`, a bounded descendant scan can relabel the row to a known agent CLI running as another real uid, with a dim `(<user>)` marker (`claude (root)` over `sudo su`). The row stays a process row with no tally or dashboard account. `pane.command` keeps its original value, so cwd pairing and idle synthesis still refuse a foreign-user agent.

## Composing the frame

The sidebar is borderless, because it already sits inside a framed mux pane. A title line, hairline rules, and a one-cell left gutter (which is also the selection lane) carry the structure. The layout is drawn in [the interface reference](../../interface/sidebar.md), and these are the rules behind it.

- Three zones, two pinned. A top-pinned cockpit, a scrolling viewport over the groups, and bottom-pinned chrome (search line, dashboard, fleet store, footer, drainer and refresh alerts). The pinned zones reserve their rows first and the viewport takes the rest, so cards give way before either pinned zone clips. Each draw resolves `ViewState::scroll_offset` around a card's own first line, clamped to the zone. `ViewState::scroll_ease` supplies the visible row for the cards, the scrollbar, and hit geometry, and only wheel changes ease ([the paint clock](./state.md#the-paint-clock)).
- Fixed height where it counts. The repo dashboard reserves its rows whether or not the room has agents, and the make-up line holds its row once the room has an agent, so the body does not shift as agents change state. An empty or process-only room has no make-up line.
- Measure, then paint the window. Warm frames share one group composer across two walks. The first resolves geometry from attachment-local cached card heights, and the second paints only the cards that intersect the eased viewport. Off-screen cards, headers, and pipeline lines use empty placeholders with identical row ownership, and group tails still compose on both walks. Heights last for one epoch of shared groups, snapshot clock, width, theme identity, focus, and card configuration, keyed within it by row identity, resolved expansion, and selection. The cache holds its own group `Arc`, so copy-on-write unread edits invalidate it too. A new epoch paints once while filling all heights. Within a warm epoch, a miss measures the card once and carries its lines into the paint walk if visible.
- Cards anchor the viewport. From the previous target window, the composer picks the selected card if any of it was visible, otherwise the topmost surviving visible card, and keeps that card's first line at the same absolute pane row. That absorbs height changes above, banner changes, and re-sorts. Previous selections' open cards stay open, unselected, while the layout freeze is armed. Only non-selection height changes from data or configuration may move the card when the offset clamp leaves no room. Wheel, banner jump, and the group, delegation, history, and landed toggles make the offset authoritative for one paint without releasing frozen expansions, and anchoring resumes from that window. A correction translates an active wheel flight without restarting it and does not wake the auto scrollbar.
- Only own input reveals selection. A key or click that changes selection marks the next paint for reveal. An emitted `InputEffect::Focus` marks its target until a fold seats that pane's owning card. Any visible card line prevents a reveal. Native switches, external focus, attach seating, and row departure never reveal. A shared click placement places the card by identity instead, and a rowless focus anchor leaves it anchored.
- The unread banner reads the target window. Its visibility comes from where the cards will land, so its height stays fixed during a glide. Its arrow compares the lead's first scroll-map line with the painted offset: up when the lead is above the view or the filter hides it, down when below. A click minimally reveals the full-roster lead where it sits without changing selection, falling back to the top if the filter hides the lead or it has gone.
- Counts span the cap. The cockpit make-up derives unit buckets from the full roster, not the agent tally. A unit is a named team within one projected non-external group, or a solo row, and status-less processes count nowhere. `SidebarWorktreeGroup::unit_key` is shared by the lens and the banner count. `BodyFilter` builds per-group facts, uses `matches` for a row's own bucket membership and `shown` for unit-wide status projection, and counts distinct matching units in `total`. The [interface rules](../../interface/sidebar.md#the-make-up-line) own the active and rest precedence and the finished-group exception. Summary line 1 pairs headline facts, and its `◎` sessions and token breakdown read `[sidebar] spend_window` from the room's JSONL `workspace_value_tally`, not the live session sum. Line 2 carries the `¤` live-agent count, the open-PR count when non-zero, and count-up spend. The unread banner counts units with an unread actionable member, while its visibility follows the lead row.
- The body holds only live rows. History lives in the store behind `rimz transcript`, `rimz message list`, and `rimz doctor --audit`. An empty room leaves the body clear. An active health alert replaces the dashboard and footer in the bottom chrome and leaves the last committed cards in place ([degraded reads](#degraded-reads)).
- The focus marker names an off-screen selection. When derived focus moves the selection, the consuming fold arms a dwell: `FOCUS_MARKER_TMUX_DWELL` (500 ms) or `FOCUS_MARKER_ZELLIJ_DWELL` (300 ms), chosen by `crates/rimz/src/sidebar_pane/app.rs::RoomConfig::mux`, re-armed by each move. After the dwell, watched paints overlay the selected card's display name at the viewport edge, after `plan_zones` and placement capture, without changing card rows, anchor rows, the offset, or the banner choice. An up-marker yields to the unread banner, and a down-marker does not. Hidden paints suppress it. A paint that shows the selected card, or own input that changes selection, ends it. A marker click uses the banner's minimal-reveal path without changing selection.

The host keeps [client views](./state.md#client-views), shared while their clients view the same pane and forked when they split. Their anchors are relative to the scroll-zone top and translate only at composition, while geometry, caches, and hit maps stay with the painting pane. Only that painter consumes the view's one-shots and writes back scroll and drawn selection. The arrival, retention, and replay rules are in state.md.

The make-up line and search share one body lens. Each non-zero bucket is a click target that shows whole units with a matching member, a second click clears it, and a zero bucket has no hit target ([the look](../../interface/sidebar.md#the-cockpit)). The cockpit's open-PR count works the same way and paints as a chip while active. Unread filtering stays on `u` and per row. `ViewState::make_up_filter` holds a `BodyLens` with an independent pick and committed query. The renderer writes the whole lens atomically to room runtime ([`body_filter.rs`](../../../crates/rimz/src/sidebar/body_filter.rs)) and dispatches a payload-free `BodyFilterChanged` through the plane's in-memory subscriber fanout, the originator included. Peers adopt it on the event or on an authoritative fold, without a producer write. `reconcile_selection` clears and republishes only a pick whose full-roster unit count reaches zero, and keeps the query. A query with no matches stays set.

`VisibleRoster` uses the lens only to choose membership. Every shown card renders and opens as it would without a filter, selection walkers reach every shown row, and worktree steps land on the neighbouring group's head. Card clicks, `Enter`, inbox traversal, digits, arrows, pages, edges, and group motion all resolve their targets inside the lens, while the cockpit counts stay full-fleet. A lens change re-anchors the selection by pane identity without starting a browse pick.

Search editing lives in the client view's `ViewState::search_draft`, which overrides only the query `ViewState::visible_roster` uses. The rules a contributor must keep:

- The supervisor sends raw presses, and `resolve_key` in [`app/input.rs`](../../../crates/rimz/src/sidebar_pane/app/input.rs) resolves help, then typing, then normal bindings. The `alert_active` parameter of `handle_wakeup` in [`app/loop_state.rs`](../../../crates/rimz/src/sidebar_pane/app/loop_state.rs) blocks `Search` after help has had its turn.
- Each edit selects ordinal zero of the narrowed roster and clears browse, and a changed selection reveals only an off-screen match. Commit emits `SyncFilter` before `Focus` through `InputOutcome::effects`.
- While typing, a pane-focusing click resolves its target through `click_focus_pane` in [`app/selection.rs`](../../../crates/rimz/src/sidebar_pane/app/selection.rs) before it clears the query and emits focus, so a row ordinal still refers to the draft-narrowed roster. Other clicks commit before their ordinary action.
- Normal-mode `Esc` clears only the committed query, after configured bindings. Help consumes it, and an active alert does not block it. Own-pane focus loss or an active health alert discards only the draft.
- `build_bottom_chrome` in [`render/compose.rs`](../../../crates/rimz/src/sidebar_pane/render/compose.rs) puts the query and cursor, or the committed chip, in its first row. It replaces the dashboard separator when there is one and reserves an extra row otherwise. The match count uses the same live roster as the cards, against all snapshot rows. The search line stays visible under an active alert.

The user-facing rules are in [text search](../../interface/sidebar.md#text-search).

Hit-testing and keyboard motion agree with the painted frame through one mechanism. `FrameInteractions` carries bucket, count, dashboard-tab, banner, group-toggle, and row targets through the same translation and clipping as the rendered lines. Mouse targets and every keyboard walker use the same filtered `VisibleRoster` membership and ordinals as the body composer.

Pane width is a mux operation dispatched off the render thread. The room holds one shared width target. `a`, `d`, and a proven mouse drag pin it, and each renderer resolves that share against its own view and converges with one resize in flight, never adopting its own late resize as a drag. Width keys update a local target. On Zellij the first key of a burst re-reads the topology and re-resolves the room target, and later keys reuse both. tmux, whose geometry read is a subprocess, uses its stored view. After 300 ms of quiet the burst commits one pin, broadcast, remembered tmux width, and intent diagnostic. Exact backends accumulate the pending target and keep their native tolerance however far a nudge travels. Relative backends lead the measured width by at most one key step. Resize wakeups defer the pixel-capability probe until 300 ms of quiet. A settled drag needs a sibling count observed before the resize and unchanged afterward. Without that baseline the controller logs `rejected-unproven` and converges back to its target. Zellij fullscreen parks the controller until topology reports fullscreen cleared. The share math and record are in [multiplexers.md](../multiplexers.md#width), and the renderer-side controller is [`app/width_control.rs`](../../../crates/rimz/src/sidebar_pane/app/width_control.rs).

### Selection and jump

Selecting a row focuses that agent's pane through the snapshot's `pane` ref, and no mux pane number is ever printed. The key table is the [interface legend](../../interface/sidebar.md#keys-and-mouse), and the handlers are in [`selection.rs`](../../../crates/rimz/src/sidebar_pane/app/selection.rs). The row takes the user to the pane, and the prompt and its approve or deny stay in the agent's own UI.

Selection is derived from the view's clients. The baseline follows their agreed working-pane focus ([multiplexers.md](../multiplexers.md#who-is-looking-at-what)), filtered to a visible non-sidebar row and moved between pulls by a fused `FocusChanged` overlay. Disagreement holds the last baseline, and unrelated clients never choose it. It is keyed by pane (`ViewState::selected_pane`), so a reorder keeps the highlight on the same pane. With no visible selected pane, `ViewState::selected_index` is `None`: no card opens by selection, no lane, spines, or header seal render, the dashboard holds its last provider or the first panel, the pet rests, and scroll anchors the topmost surviving card. An open search draft alone may highlight a pane-less match.

The own view gates the baseline. When an own pane is configured but the frame does not list it, `session_focus_baseline` withholds focus without changing the snapshot's `focused_pane`. That covers the [startup seed](./state.md#the-room-host-and-its-attachments) and the first cached fetch fold alike. A new pane's first attachment stays unseated whether it starts a fresh view or joins its client's view ([client views](./state.md#client-views)), while a successor can seed a view without a baseline from its supervisor-carried hint. A selection established on this pane holds across a transient missing own view for as long as its row stays in the frame, and once that row leaves, the selection drops and the withheld focus seats nothing in its place. A frameless fold (`panes_produced_at_ms == None`) is not the row leaving: it holds the identity-keyed selection, browse pick, and baseline, and withholds only the index until a frame returns. A correction that lists the own pane and admits the focused row seats selection without revealing it.

- Child focus selects the parent card. A listed child's focused pane maps through `SidebarSnapshot::row_owning_pane` to its parent's own pane for the selection baseline. The parent stays visible through idle caps and finished-group collapse. A focused child writes no focus read receipt for the parent, but the delayed tab-view sweep still clears a parent whose own pane is in the viewed tab. The focused child's entry lines carry the selection-role spine only while the parent card is selected.
- Transient layers sit above the baseline. Browse (`↑`/`↓`, or a card or child click) pins a pick and ends when the derived baseline changes. Clicks paint the owning card selected before dispatching focus, and motion alone does not move focus. The [layout freeze](#the-layout-freeze) pins the last painted order after a jump, so cap exemptions settle with the reorder.
- Motion keys resolve in the host. The supervisor forwards raw presses with crossterm modifiers. The host resolves them with a `NavKeymap` built from the current snapshot's `[sidebar.keys]` and rebuilt only when that table changes, so an invalid chord warns once per edit. The mode (help, typing, normal) decides before the keymap, and typing swallows every `Ctrl` or `Alt` chord other than `Ctrl+n` and `Ctrl+p`. Rebinds reach each pane with its next refreshed snapshot, using the same bindings the help overlay draws. Page and screen-edge motions read the last painted `FrameInteractions::visible_row_span`, so `Ctrl+f` and `Ctrl+b` step by the visible row count and `H` and `L` target the screen's first and last row.
- A jump is a durable intent. `↵`, a click, a digit, or `␣` writes and broadcasts a `Requested` focus intent before the one-way focus command, and command acceptance moves it to `Applied`. A cross-tab jump within one host also pushes the target, its card placement, and the frozen order to peer attachments, and waits for a painted destination frame, every peer's release, or click + 500 ms before dispatch. Same-tab jumps, automatic repairs, and CLI focus do not push or wait. The destination keeps the clicked card on the same row relative to its scroll zone, and the jump focuses the snapshot's pane id directly, without a per-click mux roster lookup. A request or dispatch error restores pulled session focus if the failed card is still selected and leaves a recovered health notice unless an active alert already occupies that line. The handshake, the host-wide jump lease, click supersession, and rollback are in [focus intent](./state.md#focus-intent).
- The focus key reaches the sidebar from any pane. `[sidebar] focus_key` (default `Alt+p`) runs `rimz sidebar focus`. tmux binds it at session birth, and Zellij runs it from the presence plugin ([multiplexers.md](../multiplexers.md#room-keys)).

A pane-backed child entry click uses the same durable focus intent, parent-card placement, and layout freeze as a card click. The hit region covers both entry lines. Paneless entries fall back to the parent card, and keyboard navigation stays card-level. A realtime pane close clears the entry's pane binding before the next producer pull.

### Provider dashboard

Provider budgets are account-scoped and live in a pinned per-provider dashboard at the bottom, not on rows. Pi's block chooses the selected card's backing credential, then this view's remembered credential while available, then the top-level session-file classification; stats and spend stay whole-login. The snapshot carries alternate credential panels so the choice needs no fetch. The active tab follows the selected card's provider kind, holding the last agent kind over a process row or absent selection before falling back to the first panel. `←`/`→` or a tab click pins a manual pick that ends when the selection's provider kind changes or the tab leaves the dashboard. Tabs are the dashboard's only hit targets. Panel sourcing and aggregation are in [providers.md](../agents/providers.md), and the look is in [the interface reference](../../interface/sidebar.md#the-provider-dashboard).

- Remote-control hosts render as a flag. The snapshot filters the Claude host pane and the Codex app-server broker out of the room and paints a health-colored `⇅ rc` flag on the provider block: green up, red down, muted `rc off` when the room started without that host. The Claude host is up when its pane is in the published frame and, with remote control enabled, its runtime-control liveness record does not mark it down ([`enrich.rs`](../../../crates/rimz/src/sidebar/enrich.rs)). Codex health comes from live daemon PIDs in the reap cache. A stopped birth records a warning and turns blocked hosts off for that run without prompting, and a live entry warns without changing the applied policy.
- The fleet store seals the bottom with 7-local-day `W:` and 30-local-day `M:` totals from `SidebarSnapshot::value_tally`, the exact record beside the cockpit's live read.
- Money counts up. The cockpit `$` headline and each card's `$cost` roll toward the exact figure as an eased odometer, snapping on first paint and across epoch resets ([`odometer.rs`](../../../crates/rimz/src/sidebar_pane/render/odometer.rs)). The headline follows the cards through `SidebarSnapshot::today_spend_live_usd`: the walked workspace total plus, for each active live card, the amount its current cost exceeds the walked baseline for its session, never negative ([`live_spend.rs`](../../../crates/rimz/src/sidebar/refresh/live_spend.rs)). The renderer never lets the headline decrease within a `today_spend_epoch_secs` window. The figure is presentation, and `SpendTally` and per-session cost are the truth ([spending.md](../agents/spending.md#what-reads-the-totals)).

## The serve loop

`rimz sidebar serve` is the pane's command. Its supervisor owns the pane command's PID and the pane's terminal and hands the painting to the session's room host, so a painter crash and a reload resolve in place, while a confirmed self-close ends the pane ([the room host and its attachments](./state.md#the-room-host-and-its-attachments)). Each attachment runs on a fixed timestep, folds each wakeup through [`LoopState::step`](../../../crates/rimz/src/sidebar_pane/app/loop_state.rs), and paints from the last committed snapshot. The frame-activity read that decides the paint follows wakeup dispatch, so an input that starts a glide keeps its 16 ms boundary. Data flow into the loop is in [state.md](./state.md#one-fetch-cycle).

A pane the host is painting reaches its supervisor over the attach stream ([`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs)):

| Round ends with | Supervisor action |
|---|---|
| `self-close` | Confirm the tab is empty with the mux, then restore the tty and end the pane, or reject and reattach ([self-close](#self-close)). |
| `reload`, or a stable attachment painting the supervisor's target build | Preflight a promotable target before supervisor exec, otherwise reattach ([build promotion](#build-promotion)). |
| Failed attach, or a stream ending without a control word | Show the cause and retry delay, record `sidebar_host_unavailable`, and retry. |
| Confirmed pane absence | Record `supervisor_pane_gone`, remove runtime files, and stop supervision. |
| SIGTERM | Detach, preserve tty modes for handoff, remove runtime files, and stop supervision. |

The `baseline` control updates the supervisor's remembered selection baseline without ending the round. It survives host replacement and supervisor re-exec, and no other UI state does ([attach wire](./state.md#the-attach-wire)).

The host is the only painter, so every way of failing to reach it shares one plain-text notice and an exponential retry from one second to 60 seconds ([host unavailable](./state.md#host-unavailable)).

Each attachment owns its heartbeat lease in the host, naming the host's room socket in `wakeup_socket`. It publishes changed claims in full and renews unchanged claims by pathname mtime. The heartbeat record, its protocol version, and its TTL are wire, in [`wakeup/heartbeat.rs`](../../../crates/rimz/src/wakeup/heartbeat.rs). Refresh failures retry on every loop pass without advancing the success stamp, warning once per failure streak; detach removes the claim.

### Launch

`rimz`, `rimz start`, and cwd-based `rimz attach` ensure the workspace session exists, then launch sidebars best-effort. Both backends run the same renderer. On Zellij, the birth layout's `new_tab_template` carries a left `rimz-sidebar` pane, so every tab is born with one. On tmux, RimZ splits a left sidebar into the first window and repeats the split from an `after-new-window` hook ([multiplexers.md](../multiplexers.md#the-sidebar-and-the-after-new-window-hook)).

Launch is idempotent by heartbeat. Only readable, current-protocol, fresh heartbeats count as live, so a crashed or upgraded sidebar does not block relaunch. `sidebar-launch.lock` serializes check-then-spawn, so concurrent attaches do not each spawn one.

### Drawing

Every animation reads the wall-clock animation phase, not the age of the data, so motion stays smooth over stale data and golden tests pin each frame deterministically. A terminal resize is a wakeup too: the process holding the pane's terminal turns `SIGWINCH` into a socket nudge, the loop reads the new size from the pane's output, and the frame repaints through the synchronous input path.

Row emphasis has two depths. The read pulse is an OKLab lightness ramp on the row glyph. The unread blink hard-toggles between the resting tone and a bright crest across the lead glyph, name, description, and make-up buckets, so it reads as on and off at every color depth. Under `NO_COLOR` it keeps the shape and the on-phase bold. The renderer resolves palette depth, because terminal capability is local: `theme.mode = "auto"` emits RGB on truecolor terminals and quantized 256-color tones elsewhere ([theme.md](../theme.md#color-depth-and-graceful-degradation)).

### Self-close

A sidebar shares its tab with the user's panes and has no reason to outlive them. The attachment requests self-close, and the supervisor decides. On tmux the supervisor pins its own pane to `remain-on-exit off` at startup, so its exit also closes the pane under the user's retention settings. A failed pin only warns, and serving continues.

The attachment reads the sibling count from the producer's own-view summary ([`SelfCloseState::should_close`](../../../crates/rimz/src/sidebar_pane/app/lifecycle.rs)). An unknown count never closes.

- Once it has seen a working sibling, a zero count requests self-close at once.
- A sidebar that has never seen a sibling (session birth or resurrection, before the mux materializes the tab) first waits `SELF_CLOSE_EMPTY_CONFIRM` (5 seconds) of continuous emptiness, so a tab born sidebar-only still cleans itself up.
- When the sidebar's tab is listed, an authoritative server read that omits its sibling is final, even while that sibling's root process cleans up. A whole omitted tab keeps liveness and tab-coherence carry.
- A cached omission uses carry and a `PreferAuthoritative` re-pull before feeding the count. If that re-pull falls back to the cache, liveness carry still protects the sibling. A producer frame that omits the sidebar's own pane is also re-pulled through `PreferAuthoritative` before it feeds the count.

On `self-close` the supervisor runs one authoritative mux listing. A present own pane closes only when its view resolves and has no working sibling. An absent own pane ends supervision only when a second listing, after a short delay, reproduces the absence. Siblings, an unresolved view, a reappearing pane, or any probe error reject the request, record `self_close_rejected`, and reattach after a short retry delay, so the decision fails toward keeping the pane.

The renderer-side guards protect paint, not lifetime. A fused `PaneClosed` overlay deletes its card at once but never changes the sibling count ([state.md](./state.md#fusion-rules)). Closing the last sibling grows the sidebar to full width, so a bounded grow-resize hold in [`lifecycle.rs`](../../../crates/rimz/src/sidebar_pane/app/lifecycle.rs) suppresses the widened repaint while the request reaches the supervisor. The hold arms only when the grow lands beyond the room width target, so attaching a detached-born session repaints at once at its legitimate width. The birth path keeps the hold through its confirm window until a non-empty fold resets it or the supervisor accepts the request.

### Degraded reads

The attachment keeps its last committed frame. When a produce fails (a vanished store, a dead mux, a transient error), the loop reuses that frame and absorbs a single failure silently. A failure that persists past the debounce raises a sticky health alert pinned to the bottom edge (`! Sidebar degraded for 8s: snapshot failed: store not found`). The alert replaces the dashboard and footer, so it cannot scroll off, and the cards above it stay as last committed. On recovery the alert stays as a dim dismissable notice. An attachment recovers health only from a completed produce, so a paintable fast fold cannot hide repeated pane-read failure. [`health.rs`](../../../crates/rimz/src/sidebar_pane/app/health.rs) folds these outcomes into `Health`.

A degraded attachment stays attached with its last committed frame and active alert for as long as the failure lasts. The worker's data backstop keeps folding, and completed production recovers health without replacing the frame with a host-unavailable notice. A wedged worker is not replaced automatically, and if it wedges before reporting a failure, the frame can stay stale without an alert. This is a known gap.

A hook-drainer outage has its own view-local alert, independent of `Health.alert`. The snapshot's runtime-lease fact activates it; the first committed fold without that fact marks recovery on the snapshot clock, and the notice lingers until `x`. An active drainer alert cannot be dismissed and hides nothing, blocks no search, drops no draft, and never emits a refresh-health diagnostic. An active refresh alert suppresses its line; a recovered refresh alert renders directly below it. The label is derived at render from the matching kind/name rollup identity and its row's `display_name()`, falling back to `@name`, then the provider display name. Rendering both alerts reads only `SidebarSnapshot::now` ([state.md](./state.md#failure-modes)).

Across attach rounds the supervisor also checks that its pane still exists. A fresh presence roster containing the pane resets the strike count without a mux command. Each due probe counts only on an observation newer than the supervisor's last, and supervision ends only after three distinct authoritative absences, recorded as `supervisor_pane_gone`. The evidence rules are shared with orphan reaping in [multiplexers.md](../multiplexers.md#sidebar-orphan-reaping).

## Reload and repair

Reload replaces the running binary and keeps every pane. Repair fixes the pane structure. `rimz reload --repair` completes the upgrade, then runs repair as a separate operation.

### Build promotion

`rimz reload` is user-wide, independent of cwd, and upgrade-only. It stages the invoking executable once as an immutable user-scoped generation, records its verified path and digest as each live room's durable target, atomically refreshes the room's stable `rimz` hardlink or copy, and sends a version-stable reload word as a latency hint. Staging keys each generation by the binary header's linker identity (a streaming hash when absent), reuses a verified copy without reading the full source, and otherwise copies with kernel assistance where available and verifies the copy's identity before publishing it.

The durable target is the truth, and the reload word is only latency. The host observes the changed target and drains its attachments with `reload`. Each supervisor also stats the workspace record once per second and digest-verifies a changed target, so a missed datagram costs latency, not correctness. The CLI's heartbeat wait only reports: a sidebar that has not converged keeps retrying from the durable target without a repair.

Promotion is host-first. The old supervisor attaches to the target build's host, waits for it to paint through the stable-run window, emits `supervisor_convergence`, preflights the target's version command, and only then re-execs its own pane-command PID. A failed preflight records `supervisor_preflight_rejected` and keeps the old supervisor until the target changes. A crashing or unlaunchable host costs notice-and-retry under backoff while the old supervisor and pane stay alive, and a later record change breaks the backoff. A successful handoff keeps pane id, tab, geometry, focus, terminal raw mode, and mouse capture.

Bare reload creates and closes no panes. It still reloads refresh dashboards, sweeps stale runtime files, and reaps sidebar processes whose pane is gone, under the double-authoritative evidence rule in [multiplexers.md](../multiplexers.md#sidebar-orphan-reaping). A managed room always runs a sidebar, so a live session with no fresh, protocol-current heartbeat has either lost its pane or runs a renderer this build cannot signal (`fresh_sidebar_heartbeats` drops both). Reload converges neither, so `upgrade_live` counts it as `sidebar_missing` and the CLI report points at `--repair`, which mounts or replaces a pane ([the line](../../reference/cli/maintenance.md#reload-running-sidebars)). The sidebar `r` key and the stats dashboard `r` key take the same pane-preserving path. The host's coalesced decision is in [`host/reload.rs`](../../../crates/rimz/src/sidebar_pane/host/reload.rs), and the Zellij mount caveat is in [multiplexers.md](../multiplexers.md#zellij-backend-caveats).

A reload also restarts each live room's running hook drainer after recording the build, before sweeping runtime claims. Both backends share this step, and it leaves an absent drainer absent. The [store contract](../store.md#the-workspace-store) owns the frame-boundary stop and the queued-work handover. Machine-wide [language-server broker retirement](../lsp.md#build-retirement) runs even without a live room, leaving running servers uninterrupted and preserving leases for lazy revival.

### Zellij plugin upgrades

A reload upgrades the Zellij presence plugin only when its identity differs. The plugin configuration names the room's build-stable `rimz` pointer and includes the embedded wasm digest (computed lazily, once) plus a hash of the loaded configuration, and the plugin echoes both in its topology writer. A fresh matching writer counts as current. When it is the only loaded presence plugin, no pipe runs. When other plugin ids are loaded beside it, [`cleanup_current_presence_plugin_for`](../../../crates/rimz/src/mux/zellij/presence.rs) broadcasts retire, sweeps the stale instances, and reloads the accepted one. A stale, legacy, or mismatched writer runs the replace-and-retire path ([multiplexers.md](../multiplexers.md#retirement)) and reports the upgrade.

### Structural repair

`rimz sidebar repair` converges every view toward exactly one live sidebar. It closes duplicate and orphan sidebars, adds a missing one, and replaces an unclaimed one add-before-close, committing a replacement only after the new pane mounts in the intended view and publishes a current-build heartbeat. It shares attach's session reconcile lock and reload's orphan-reap gate. Each structurally repaired view leaves a [`sidebar_view_repaired` diagnostic](../diagnostics.md#event-taxonomy) naming the successfully closed and verified added panes, including effects before a later failure. Attach and supervised launch use the same recording path, and unchanged passes write nothing. The Zellij presence probe is diagnostic only: a silent or provably stale plugin warns, while structural repair still reads the native live pane list. The planner, lock, verdicts, and Zellij mount proof are in [multiplexers.md](../multiplexers.md#one-sidebar-per-view) and [in-place repair](../multiplexers.md#in-place-repair).

On Zellij, repair captures one unique fresh client view before changing structure, then restores focus through the same two-phase intent a jump uses: the exact live work pane, or the viewed tab's leftmost work sibling. Unavailable or ambiguous views skip the focus change. Adds and geometry repair in a detached Zellij session wait until a client attaches, while tmux mounts splits detached. Repair never closes the daemon view's managed hosts. That view counts as occupied and keeps one sidebar like any working view.

## Resume-on-rebirth

When the machine reboots or the mux server crashes, the agent processes are gone but the store remembers them, and RimZ offers to bring back the agents the sidebar producer last saw alive.

1. Every produce cycle, the host writes `records/live-roster.json`: the current pane-backed, full-session set. A publication that would drop an agent named in the roster on disk is written only while the mux still lists the host's session (`MuxBackend::session_accepts_agent_close`, probed after the snapshot was produced, where a failed or timed-out listing counts as not listed). Otherwise the file stays as it was and a rate-limited `live_roster_held` diagnostic names the agents kept. A host that outlives its session can read an agent-less room from the pane cache, and its view must not replace the set the birth recovers from. A publication that removes nothing is written without a probe. Provider-native subagents ride their parent, and pane-backed launched children are independent roster members.
2. A birth after a reboot or same-boot crash reads the roster and the pending-recovery record, and [`harness/rebirth.rs`](../../../crates/rimz/src/harness/rebirth.rs) intersects them with the audit rollup without writing. An interactive start prompts to recover the root agents, defaulting to yes, and asks separately, defaulting to no, before dropping the roots it cannot resume. [`room`](../../../crates/rimz/src/room/mod.rs) owns the mux birth order.
3. Before the session is created, the birth parks the roster's agents in `records/pending-recovery.json`. Once the session exists and before the new sidebar starts, settlement records `session.death{cause,lost_agents}`, archives crash caches, allocates fresh team members, settles the root candidates as the user decided, automatically ends non-live children (`rimz.child-not-resumed`), appends `session.rebirth`, and deletes `records/live-roster.json`. A resumed agent leaves the pending record only after the birth confirms its tab open. Only a user decision or a confirmed or live seat refill (`rimz.seat-refilled`) ends a root agent. The other roots stay parked, exempt from the reap, until a later rebirth asks or an explicit resume brings them back. Pending launched children are not exempt from the reap, and recovery never resumes them.

Room teardown captures the roster before killing the mux session, then restores it after the scoped process sweep confirms producer exit. That keeps the pre-teardown set for a later birth even when a dying producer publishes an empty roster during shutdown. The sweep also reaps hook feeds carrying the room's workspace pin, because they can publish caches after their pane dies, while reload's liveness-only sweep leaves hooks alone. The sweep checks process identity while waiting, escalates from TERM to KILL, and warns about survivors past its deadline. Teardown stays best-effort and restores the roster even then. Normal agent exits in a living room still narrow the roster, on the producer's next cycle that finds the session listed.

The plan (which sessions resume, team layout, `resume.max`, and the replayed posture) is in [fleet.md](../harness/fleet.md#resume-and-rebirth). Restored agents come back idle: the resume argv rehydrates the conversation without a prompt, so no tokens are spent until the user types. Each agent's `SessionStart` fires with `source: "resume"` and re-stamps the new pane, so the row rebinds with the same identity and team members keep their `@role` address.

Three paths skip recovery. `--no-resume` or `resume.on_rebirth = false` gives a room with no agents. The room `rimz reset` starts itself recovers nothing. A later `rimz start` after `rimz reset --no-start` recovers the roster the teardown preserved, and ends the prior root agents only when that start is attended, parking them otherwise.

This continuity is RimZ-owned and transcript-based. RimZ disables Zellij session serialization in its rooms, because Zellij resurrects command panes suspended and they read as dead.

## Notifications

Notifications are best-effort over the same attention model, and the store stays authoritative. The host opens unread episodes, applies `[notifications].triggers`, debounce, and focus suppression, spawns matching handlers, and sends `SidebarEvent::Notify` directly to its subscribed panes after their snapshots. One host timer reminds on the room's unread `waiting` and `failed` rows until they clear, spawning handlers once per interval with the room count. Each renderer writes terminal-local OSC and BEL bytes outside the draw cycle. The contract is in [notifications.md](./notifications.md).

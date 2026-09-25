# Sidebar

This page owns the mechanics between the store and the painted sidebar: how a live pane becomes a row, how rows group and rank, which lines a card carries, how the frame composes, and how the renderer process launches, closes itself, recovers, and reloads. Four pages sit beside it. [state.md](./state.md) owns the data plane underneath: producer election, the fetch cycle, the published caches, realtime events, fusion, and cadences. [The interface reference](../../interface/sidebar.md) draws every glyph and frame. [instances.md](../agents/instances.md) owns how a session joins its pane from the session's side, and [DESIGN.md](../../../DESIGN.md) states the product commitments.

The sidebar is the narrow column pinned beside the work panes, and it answers one question: which pane needs the user now. One renderer paints every state of a room, from a bare shell to a fleet across worktrees, through detach and reattach. Only the snapshot changes between those states.

## The producer and the renderer

Every decision is made once, in the snapshot, and painting is a projection of it. Which agent owns a pane, which group a row belongs to, how rows rank, and what the cockpit counts are all resolved before a glyph is chosen. Painting therefore stays cheap and golden-testable (a snapshot in, a frame out), and the CLI listings read the same view-model as the pane, so the two surfaces cannot disagree about the room.

That rule splits the code in two. **The producer** folds the store and the live pane roster into one `SidebarSnapshot`: presence, grouping, ranking, and every card field. **The renderer** turns that snapshot into lines for one terminal. `SidebarSnapshot` is the whole contract between them. A behaviour that depends on the viewer (width, color depth, selection, scroll position, the active filter) belongs to the renderer; a fact about the room belongs to the producer.

The sidebar is a client of the store and never a writer of it. Its filesystem writes are runtime display files: its heartbeat, read receipts, `lanes/unread.json`, `lanes/focus-anchor.json`, the body filter, and the producer caches. The elected producer also keeps a best-effort status suffix on mux tab names. None of these is durable truth. `cargo xtask invariants` enforces the boundary with `ensure_sidebar_library_boundaries` for the data plane and `ensure_sidebar_renderer_boundaries` for the renderer.

## From store to screen

One pass builds one frame, in this order.

1. **Fold the event log** into the rollup, every durable fact the store holds about the room.
2. **Reduce lifecycle events** into one `AgentState` per agent, carrying turn, phase, subagents, and model.
3. **Bind panes.** Read the live pane frame and bind agents to panes. This is the [admission boundary](#presence-model): a pane the rules exclude folds no row, and an agent with no live pane is data, not presence.
4. **Classify the rest.** A pane no session claims becomes a [process row](#process-rows).
5. **Reap** ghosts, relaunches, and stale sessions the rollup still carries.
6. **Group and rank.** Rows fold into groups, each row and group gets a score, and the roster sorts.
7. **Enrich.** Unread state, git stats, provider panels, subagent lists, and value tallies attach to the ordered roster.
8. **Serialize** the result as `SidebarSnapshot`.
9. **Project to lines.** The renderer resolves the visible roster, composes the zones, and paints.

Steps 1 through 8 run in the elected producer and are published once for every renderer in the session; the split, caches, and timings are [state.md](./state.md#one-fetch-cycle). Step 9 runs in each renderer.

Two commands split a bug between the halves. `rimz sidebar snapshot --json` runs steps 1 through 8 and prints the result: if the wrong answer is already in the JSON, the bug is in the producer. `rimz sidebar frame` renders that snapshot without capturing through the mux, so a correct snapshot with a wrong frame points at `sidebar_pane/`.

## Where the code lives

The sidebar spans three module trees: the producer's view-model builder, the renderer process, and the data plane.

**`crates/rimz/src/store/snapshot/` builds the view-model**, in the order of the stages above.

| Module | Owns |
|---|---|
| [`fold.rs`](../../../crates/rimz/src/store/snapshot/fold.rs) | the resumable event-log fold, its persisted rollup cache, and the carryover that survives log rotation |
| [`project.rs`](../../../crates/rimz/src/store/snapshot/project.rs) | the lifecycle reducer: `agent.lifecycle` events folded into one `AgentState` per agent |
| [`panes.rs`](../../../crates/rimz/src/store/snapshot/panes.rs), [`panes/lazy.rs`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs) | pane binding (`PaneBinder`), the own-view summary, unstamped pairing, and idle synthesis |
| [`process.rs`](../../../crates/rimz/src/store/snapshot/process.rs) | classifying panes no agent claims: wrappers, launchers, the supervised exec wrapper, agent-kind sniffing |
| [`view/live.rs`](../../../crates/rimz/src/store/snapshot/view/live.rs) | folding live panes into rows, plus the local-session and activity enrichments over them |
| [`view/reap.rs`](../../../crates/rimz/src/store/snapshot/view/reap.rs) | collapsing ghosts, relaunches, and stale sessions out of the roster |
| [`view/layout.rs`](../../../crates/rimz/src/store/snapshot/view/layout.rs) | group resolution (`GroupResolver`), the row rank key, and group comparison |
| [`view/score.rs`](../../../crates/rimz/src/store/snapshot/view/score.rs) | the fixed-point attention score |
| [`view/aggregate.rs`](../../../crates/rimz/src/store/snapshot/view/aggregate.rs) | group assembly, status tallies, displayed-status projection, child-activity folding |
| [`view/providers.rs`](../../../crates/rimz/src/store/snapshot/view/providers.rs) | the provider dashboard panels |
| [`assemble.rs`](../../../crates/rimz/src/store/snapshot/assemble.rs) | the read entry points, the persisted snapshot, and its lock-free fresh-latest fast path |

The contract types are in [`view.rs`](../../../crates/rimz/src/store/snapshot/view.rs) and [`view/model.rs`](../../../crates/rimz/src/store/snapshot/view/model.rs) (`SidebarSnapshot` and its `Sidebar*` members) and [`row.rs`](../../../crates/rimz/src/store/snapshot/row.rs) (`SidebarRow` with its `AgentCard` and `ProcessCard` payloads). `SNAPSHOT_VERSION` gates cross-version adoption.

**`crates/rimz/src/sidebar_pane/` is the renderer process.** `app/` runs the loop and `render/` paints.

| Module | Owns |
|---|---|
| [`app.rs`](../../../crates/rimz/src/sidebar_pane/app.rs), [`app/loop_state.rs`](../../../crates/rimz/src/sidebar_pane/app/loop_state.rs) | the fixed-timestep serve loop and its wakeup dispatch |
| [`app/fetch.rs`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs) | the off-thread fetch worker: cadence, election, request coalescing, notification state, publication ([state.md](./state.md#one-fetch-cycle)) |
| [`app/selection.rs`](../../../crates/rimz/src/sidebar_pane/app/selection.rs) | the identity-keyed highlight, the browse layer, and the key and mouse handlers |
| [`app/gate.rs`](../../../crates/rimz/src/sidebar_pane/app/gate.rs) | the last-resort hold that refuses a regressive frame |
| [`app/health.rs`](../../../crates/rimz/src/sidebar_pane/app/health.rs) | failure debounce, the sticky alert, and the give-up rule |
| [`app/lifecycle.rs`](../../../crates/rimz/src/sidebar_pane/app/lifecycle.rs) | the self-close latch and the grow-resize paint hold |
| [`app/order_hold.rs`](../../../crates/rimz/src/sidebar_pane/app/order_hold.rs) | the renderer-local row and group order freeze |
| [`app/reload.rs`](../../../crates/rimz/src/sidebar_pane/app/reload.rs) | detecting that the workspace build target changed |
| [`app/width_control.rs`](../../../crates/rimz/src/sidebar_pane/app/width_control.rs) | the renderer-local pane width controller |
| [`app/input.rs`](../../../crates/rimz/src/sidebar_pane/app/input.rs), [`app/keymap.rs`](../../../crates/rimz/src/sidebar_pane/app/keymap.rs) | the input-socket wire codec and the configurable navigation keymap |
| [`render/ui_state.rs`](../../../crates/rimz/src/sidebar_pane/render/ui_state.rs) | `UiState`: scroll offset, selection, body filter, and pet view |
| [`view.rs`](../../../crates/rimz/src/sidebar_pane/view.rs) | `VisibleRoster`: body membership, the cap, and stable row ordinals, shared by render, browse, selection, and holds |
| [`render/compose.rs`](../../../crates/rimz/src/sidebar_pane/render/compose.rs) | zone composition, scroll resolution, and the bottom chrome |
| [`render/sections/`](../../../crates/rimz/src/sidebar_pane/render/sections/mod.rs) | one module per zone: `cockpit`, `fleet` (the make-up line), `worktree`, `agent_card`, `process`, `provider`, `pets` |
| [`render/labels/`](../../../crates/rimz/src/sidebar_pane/render/labels/mod.rs) | the glyph and meter vocabulary every section shares |
| [`render/interaction.rs`](../../../crates/rimz/src/sidebar_pane/render/interaction.rs) | typed hit geometry emitted with each painted frame |
| [`render/theme.rs`](../../../crates/rimz/src/sidebar_pane/render/theme.rs) | palette depth and the motion modifiers the terminal supports |
| [`render/animation.rs`](../../../crates/rimz/src/sidebar_pane/render/animation.rs), [`odometer.rs`](../../../crates/rimz/src/sidebar_pane/render/odometer.rs), [`scrollbar.rs`](../../../crates/rimz/src/sidebar_pane/render/scrollbar.rs) | motion, driven by the wall-clock animation phase |
| [`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs) | the supervisor process: build convergence, respawn, pane liveness, and self-close confirmation |

**`crates/rimz/src/sidebar/` is the data plane**: producer election, the published caches, the realtime overlay store, and fusion, over the wakeup wire in `crates/rimz/src/wakeup/`. Both are mapped in [state.md](./state.md#where-the-code-lives).

Where to start reading depends on the question:

- Why is this row here: [`view/live.rs`](../../../crates/rimz/src/store/snapshot/view/live.rs), then `panes.rs`.
- Why is this row in this position: [`view/layout.rs`](../../../crates/rimz/src/store/snapshot/view/layout.rs).
- Why does the card look like that: [`render/sections/agent_card/template.rs`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/template.rs).
- Why did the pane close, respawn, or reload: [`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs).

## Presence model

Row presence comes from the live pane frame. The producer enumerates the session's panes, reads each pane's foreground command and cwd, resolves the cwd to a worktree, and the fold emits at most one row per admitted pane. No pane id is shared by two rows.

[`pane_admits_card`](../../../crates/rimz/src/store/snapshot/panes.rs) is the admission rule. It excludes the caller's own pane, sidebar chrome, and the remote-control and app-server hosts. In the [`rimzd` view](../rimzd.md) it admits only a pane with an agent durably stamped on it, which keeps the dashboard's infrastructure out while letting a loop-zone run render; every other pane is admitted. The rule is the same list the fold uses for `agent_panes`, so a pane it drops is also unreachable by message delivery.

An agent with no live pane has no row. That covers a subagent, a ghost a kill left in the rollup, and a relaunch the [reaper](../agents/instances.md#session-death) has not yet collapsed. Such an agent cannot resurrect a row or latch onto another pane, and there is no `offline` status.

Several root sessions can claim one pane: a relaunch in place, a Codex `/side` fork, or a provider that switches conversation ids. [`view/reap.rs`](../../../crates/rimz/src/store/snapshot/view/reap.rs) collapses an older root when a newer one is provably a different process, or when a rested owner gives way to a proven same-process conversation replacement. A same-process thread fork survives the reap. Among the survivors, [same-pane ownership](../agents/instances.md#same-pane-ownership) picks the one root that owns the card, and every other root's activity folds display-only onto it.

Attention lives on the agent's own row. A blocking prompt puts the session's one row in `? waiting`, so a session never stacks a second row for its ask; when the ask holds and when it releases are [model.md](../agents/model.md#waiting-and-asks). If the agent's pane closes, the row leaves with it and the durable state stays in the store.

Remote agents do not render. An agent that runs only in a daemon with no local pane (`claude remote-control --spawn`, or a Codex thread started from the web) reaches the rollup but has no pane to bind. Showing one needs a presence class that renders from the rollup alone, which does not exist; this is a known gap.

### The binding ladder

A pane binds by identity first, because its foreground command cannot name the agent on its own: Claude can run under `node`, and two same-kind agents in one worktree read identically. Every `agent.lifecycle` event carries the mux pane id its hook ran inside (`TMUX_PANE` or `ZELLIJ_PANE_ID`), and cwd and command are a guarded fallback used only when no stamp claims the pane.

[`PaneBinder::resolve`](../../../crates/rimz/src/store/snapshot/panes.rs) walks each admitted pane down this ladder and stops at the first rung that matches. [instances.md](../agents/instances.md#binding-a-session) lists the same joins from the session's side.

1. **Stamped root.** The pane binds the root agent that stamped its exact id. A `session.rebirth` boundary retires every earlier stamp, because pane ids restart on rebirth. A process-start guard refuses a stamp whose recorded pane process started after the live one. For adapters that set `registers_lazily`, a stamp also needs the live foreground or hosted process to name the same kind, and, unless the agent owns the live pane root, session activity no older than the pane's process start. Stamps come from the hook env or, for a daemon-routed session, from hook ingestion: before appending the first lifecycle event it reads the repaired pane frame, filters to same-cwd same-kind panes no live stamp owns, uses client focus to break a tie, writes one recovered stamp, and appends every attempt to `audit/binding.log.jsonl`.
2. **Stamped launched child.** A pane stamped by an agent-launched child binds as a nested agent. It stays addressable and renders inside its parent's card ([sub-agent lists](#sub-agent-lists)).
3. **Unstamped pairing.** [`compute_lazy_agent_pairings_with_index`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs) pairs unstamped root sessions to same-kind, same-cwd candidate panes once per frame. A pane whose command line resumes an exact session (`codex resume <id>`, or the resume syntax the Kiro, Antigravity, and Grok adapters parse) pairs first. The remaining sessions pair newest-first by `last_activity`: each takes the free candidate whose process started latest at or before the session's first event, and otherwise the first free candidate, provided its process start does not postdate the session's `last_activity` ([`pane_start_allows_bind`](../../../crates/rimz/src/store/snapshot/panes.rs)). A session whose stamp names a dead pane enters this pairing only when its adapter sets `registers_lazily`. A choice among several viable panes appends a `audit/binding.log.jsonl` breadcrumb.
4. **Idle synthesis.** A recognized agent pane of a wired kind (installed hooks or a declared local-session observation path) renders a synthesized idle `○ <kind>` row ([`idle_agent_row`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs)) until a real session binds.
5. **Process row or nothing.** A pane first seen in this produce with a known command but no resolved cwd is quarantined for one frame (a `NewbornQuarantined` diagnostic) so it does not blink in as an ungrouped row. Otherwise a known command becomes a [process row](#process-rows), and a pane with no readable command folds no row.

Providers with their own session stores also bind through local session observations, applied in `view/live.rs`; the rules are [instances.md](../agents/instances.md#local-session-observations). The cases are pinned in [`view/tests/pane_binding/`](../../../crates/rimz/src/store/snapshot/view/tests/pane_binding) and [`view/tests/lazy_bind/`](../../../crates/rimz/src/store/snapshot/view/tests/lazy_bind).

A daemon-routed session arrives unstamped by construction. Codex under remote control fires its hooks from the shared per-user app-server daemon, so the hook env carries no pane id and the hook pid is the daemon's. The session id exists only in the daemon's rollout and socket traffic, never in a client's environ or open files, so no client-pid join is possible ([adapter_codex.md](../agents/adapter_codex.md#session-registration-and-launch-quirks)). Rungs 1 and 3 exist for these sessions.

### Honest reads across a mux hiccup

The mux roster can arrive incomplete: a partial pane set, or a live pane missing its command or cwd. Four guards keep a momentary glitch from reaching the screen.

| Guard | Where | What it does |
|---|---|---|
| Repaired fields | producer | Joins a raced-null read to the last published process for that exact pane id, and backfills a missing cwd from the process backend, so a new pane groups under its worktree on first appearance. |
| Carried panes | producer | A pane a fresh read omits carries forward while a matching non-zombie process proves it, or by identity for the renderer's own pane. A pidless sibling in a tab with confirmed liveness carries through tab coherence; a proven-dead sibling does not. Carried panes are marked, publish a `pane_carry_forward` diagnostic, and expire after `PANE_CARRY_TTL` (30 seconds), except the renderer's own pane. |
| Nameless and newborn panes | projection | A pane with no readable command, or a pane first seen this produce without a resolved cwd, folds no row this frame instead of an anonymous row (ladder rung 5); the next read names it. A stamped pane still binds by id and keeps rendering. |
| Last-resort hold | renderer | Absorbs a residual race that would erase or demote a stable row set. It holds an agent-to-process demotion only while the foreground command is unchanged or missing; a changed command means the agent exited and the shell returned, which commits at once. |

The producer repairs live in [`sidebar/produce/panes.rs`](../../../crates/rimz/src/sidebar/produce/panes.rs), the hold in [`app/gate.rs`](../../../crates/rimz/src/sidebar_pane/app/gate.rs), and the fusion rule that lets a real close override a carried pane in [state.md](./state.md#fusion-rules). Carried panes and gate holds show as dim bottom notices without counting as health failures. When pane discovery fails outright, the renderer keeps its last good frame and eventually raises the health alert ([degraded reads](#degraded-reads-and-give-up)).

## Ranking and grouping

### Worktree groups

Each group is one block under its header, and a group is one unit of isolated work. The header carries no status tally: the cockpit owns the fleet make-up and each row carries its own glyph. A live group's header, or a finished group with at most one agent, is a jump target; a finished group with several agents makes the header an expand and collapse toggle.

Git stats live on the header and never on a row, so changes in a shared worktree never read as one agent's. The `+/-` churn is the worktree's change against its merge-base with trunk, and the `⇡`/`⇣` delta is the commit count over the same base.

The trunk glyph ranks reconciling (a local rebase or merge in flight) first, then the forge PR verdict (merged, closed, open), then the local trunk relationship (merged, pristine, plain branch). A pristine fork reads `≡ <trunk>`, and a merged PR shows the merge marker alone even when squash-merge ancestry leaves the local branch diverged. The trunk checkout keeps the plain branch glyph and carries no PR verdict. Trunk resolves per repo: `[sidebar] trunk` ([configuration.md](../../guide/configuration.md#sidebar-rendering)), then `main`, `master`, and the remote's advertised default. Sourcing is [`refresh/git_stats.rs`](../../../crates/rimz/src/sidebar/refresh/git_stats.rs) and [`refresh/pr.rs`](../../../crates/rimz/src/sidebar/refresh/pr.rs), the landed verdict is [worktrees.md](../harness/worktrees.md#what-the-sidebar-asks), and the look is [the interface reference](../../interface/sidebar.md#worktree-headers).

[`GroupResolver::resolve`](../../../crates/rimz/src/store/snapshot/view/layout.rs) places a row by the first rule that matches.

| Row | Group |
|---|---|
| carries a channel | that channel's pod |
| has no worktree path | a group named by its branch if it has one, otherwise `external` |
| path inside the repo's RimZ-owned worktree home | that worktree's `#channel` pod, matching message addressing and `rimz channel list` |
| path inside a group root | the deepest containing root: a checkout in a repo room, a name-only root pod for the root of a directory room |
| path outside every root, in a room with no roots at all | its own group per path |
| path outside every root that names the row's branch | a group named by the branch |
| anything else | `external` |

Group roots are the repo's checkouts from `git worktree list` in a repo room; a directory room does not scan its tree. Every row that reports both a path and a branch also adds its path as a root, and rows on several branches at one path split by branch. The `external` group (a home shell, `/tmp`, CI) renders as a dim divider, always sorts last, and keeps an attention-only tally so an out-of-project ask still surfaces. The rules are pinned in [`view/tests/grouping/`](../../../crates/rimz/src/store/snapshot/view/tests/grouping).

### Attention ranking and the cap

Within a group, agent cards lead and process rows form the tail. Rows order by three age bands, then by a fixed-point score inside each band. Read state never changes the band; unread drives the wash and blink emphasis, the jump inbox, notifications, and the cap exemption, and nothing else.

| Band | Membership | Score inside the band |
|---|---|---|
| hot | `last_activity` inside `[agents.attention] inactive_after_secs` (default one hour) | attention rows (`waiting`, `failed`, `paused`) heat from 1.0x to 2.0x across the window, so an older failure can outrank a fresh ask; calm rows (`success`, `running`, `idle`) stay flat and keep pane creation order |
| warm | past the inactive window, inside `archive_after_secs` (default 24 hours) | decays from 1.0x to 0.0x, so stale asks lead stale calm rows without competing with current work |
| archive | past the archive window | flat; archived asks sit above archived calm rows |

The score is status weight times that time factor, both fixed-point ([`score.rs`](../../../crates/rimz/src/store/snapshot/view/score.rs)). The weights are spaced so the lowest attention state outranks the highest calm state; why each status weighs what it does is [model.md](../agents/model.md#status-and-phase). Process rows sit outside the bands: their clock is the foreground process start, so an idle shell stays live and seats below every agent card. A `paused` row resumes on its own through [auto-continue](../agents/providers.md#auto-continue).

Co-launched agents hold one contiguous block. A named-team launch and an inline multi-agent layout render as one cohort inside their group, teams in declared role-list order and inline launches in agent-cell order. The block takes its state from its members, first match winning: any `waiting` or `failed` member makes it blocked, then any `paused` member parks it, then `running` makes it working, then `success`, else idle. Blocked and paused blocks rank by the oldest attention clock; calm blocks by the most recent member clock.

Groups rank in this order:

1. `external` groups last, whatever they hold.
2. The band of the group's liveliest member, then that member's attention score.
3. The group's calm activity in that band: working if any member runs, else finished if any member succeeded, else idle if any agent remains, else process-only.
4. Git: dirty, clean, unknown, then done. Done needs a merged or closed PR or a `WorktreeTrunkSync::Merged` verdict, so a pristine fork whose merge-base equals `HEAD` stays clean.
5. The earliest member's pane creation order, then the label.

A done group with no attention or running member moves to the archive band at once, and a revived member restores its band. The presentation sort and each live overlay refresh stamp that verdict into `SidebarWorktreeGroup::finished`, so ranking and rendering agree. The seams are `sort_rows`, `compare_groups`, and the rank key in [`layout.rs`](../../../crates/rimz/src/store/snapshot/view/layout.rs), pinned in [`view/tests/ranking/`](../../../crates/rimz/src/store/snapshot/view/tests/ranking); the user-facing account is [the sidebar guide](../../guide/sidebar.md#how-the-column-is-ordered).

A staged team's `SidebarWorktreeGroup.pipeline` adds a line below the header, projected from the producer's board-derived `lanes/pipeline.json` ([state.md](./state.md#the-fold-spine)). It carries the team badge, declared-stage dots with the board's stage name inlined, and right-pinned stage and run durations. `position()`, `span_secs(snapshot.now)`, and `total_secs(snapshot.now)` own stage and clock logic. `span_secs` sums earlier visits and the open visit; future dots consult `visited` for the `PipelineRevisited` tone; the renderer uses the owner's status style at the same animation phase for the current dot and name. Owner matching uses the group's team and the visible card's handle. The click region targets that row, else the first actionable visible team row, else the first visible row, without adding a selectable row. This display feeds no status, unread, attention ranking, tab status, or notification. Folded finished groups omit the line and append ` · <stage>` after the team name in their roster; unfolding restores it. Layout and glyph states are in the [interface reference](../../interface/sidebar.md#worktree-headers).

**The cap is renderer-local and protects visible work.** The snapshot carries every row. In the resting body each renderer caps a group's idle and process tail at `WORKTREE_ROW_CAP` (6) rows with a dim `+K more`, and expanding the group shows every row plus `− less`. Active, blocked, paused, finished, unread, and focused rows are exempt.

A finished group with several agent rows collapses every row, unread successes included, behind a two-line dim receipt. Process rows do not count toward "several", so a process-only group or a group with one agent stays expanded.

- The `▸` roster line leads with the shared `AgentCard::team` value when present, then each agent's final status glyph and name in source order, folds overflow and process rows into `+n`, and pins the cohort's lifetime transcript-priced cost right. Too narrow for one member, it reads `▸ +K done`.
- The totals line carries the lifetime `◇ ↘ ↗ ◌` token split and pins retained active time right, falling back to last-activity age once active-time sidecars expire.
- The producer publishes the group totals and per-seat lifetime effort, keyed by row id, through the 60-second `lanes/cohort-spend.json` lane; the renderer only formats them. The figures fold the same lifetime-admitted audit slots as [attribution](../agents/attribution.md#selecting-records), so a receipt counts only the checkout's current life.
- A revealed card from a finished population shows the seat's lifetime cost, so the cards sum to the receipt; a live card shows its session-scoped self-report.

The header, either receipt line, and the body status filter reveal the full roster, and the header collapses it again. Focus or the order hold on any member also reveals the whole group, and the collapse lands as a unit once focus leaves and the hold expires. The reveal state is renderer-local.

**The order hold keeps rows still after an interaction.** Producer rank is the truth, but after a jump or browse, or when the focused agent's ask is answered or its turn starts, the renderer keeps the last painted row and group order and that frame's visible rows for `REORDER_HOLD` (5 seconds), so a glance back finds the cards where they were. Held frames match rows by id, then by pane id, so a launch-to-session rekey keeps its slot. A row or group born during the hold splices in at its producer rank, so expiry does not move it. Read state clears immediately; only order and cap exemptions hold.

## The cards

Each agent is a small stacked card whose anatomy is drawn in [the interface reference](../../interface/sidebar.md#the-card). A row is base identity plus one payload: [`AgentCard`](../../../crates/rimz/src/store/snapshot/row.rs) for lifecycle and context fields, or [`ProcessCard`](../../../crates/rimz/src/store/snapshot/row.rs) for command and process metrics.

### The line template

A card's line set is fixed before any content fills it, so a provider that starts reporting a field cannot grow a line. [`template`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/template.rs) maps four inputs to an ordered list of slots:

- **Stage** (`CardStage`), from durable lifecycle facts only. A card is `Fresh` while it is `idle` with no submitted prompt, no session history, and an empty context gauge; `labeled` means it carries a RimZ-authored description instead of the compose affordance. Anything else is `Engaged`.
- **Expansion** (`CardExpansion`): `by_selection` (selected, including every visible teammate of a selected named-team member) picks the card shape; `delegation` (the sticky header override, else `by_selection` or `expanded` density) picks whether the delegation entries show.
- **Density**: `[theme.display] card_density`, one of `auto`, `expanded`, `compact`.
- **Status**, read only by the resting compact arm.

| Card | Slots |
|---|---|
| compact, not expanded: `idle` | identity |
| compact, not expanded: `running`, `waiting` | identity, description, gauge |
| compact, not expanded: `paused`, `success`, `failed`, `sleeping` | identity, description |
| fresh unlabeled, not expanded | identity |
| fresh labeled, not expanded | identity, description |
| fresh unlabeled, expanded | identity, awaiting dots, gauge |
| fresh labeled, expanded | identity, description, gauge |
| engaged, delegation closed | identity, description, gauge, tokens, delegation |
| engaged, delegation open | the above plus delegation entries |

A unit test pins every combination. Under `auto` and `expanded`, selection and the delegation section only append or drop the entries, so a card's standard lines never reflow. Compact is the exception: a resting compact card is shorter, and expanding it reflows to its stage's full shape. Selection styling stays on the focused row even when teammates expand with it.

The `Delegation` slot is one standing line with the lifetime child count and cost and the count of armed one-shot waits plus background shells; it renders empty until any exists. `DelegationEntries` lists subagents in bands, then pending waits with background shells merged in after the command and PID waits (ahead of signals), then an optional `+K older` tail. Pending waits come from the loop catalog at enrichment; background shells come from the rollup's durable `background_shells`, folded from adapter hook reports ([model.md](../agents/model.md#turn-endings-and-parked-turns)). Two frame-rate rules follow from the slots: the serve loop holds the breath cadence while a visible selected card shows the unlabeled compose affordance, and it holds the fast grid while a visible card's open delegation entries hold motion (a running child's working or thinking head, or a running shell job's working lead; an idle or sleeping child's status head holds nothing), whatever the parent's own status. Timer and signal leads are static and hold nothing.

Subagents and waits share [`entry.rs::push_entry`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/entry.rs): a lead, type word, optional headline separated by `GlyphRole::Seam`, pinned right spans, and an optional prebuilt detail line. [`PendingWaitTrigger`](../../../crates/rimz/src/agents/state.rs) supplies `kind_word`, `headline`, and `detail`; the renderer owns the separator and layout. Subagents supply their existing metadata grid as detail, while waits supply command or deadline text. The [interface entry table](../../interface/sidebar.md#subagents-and-waits) owns the per-kind line rules.

Delegation entries answer to two renderer-local states in `UiState`, neither of which travels back to the data plane:

- `delegation_overrides` maps a row id to open or closed. The delegation line carries `HitTarget::ToggleDelegation { row, open }` with the state its click sets, so a closed selected card and an open unselected one are both one click away. The override outranks selection and density, survives prompts and selection moves, and is pruned only when the row leaves the snapshot.
- `delegation_history` maps a row id to the parent's `user_turn_started_at` at the `+K older` click (`HitTarget::ToggleDelegationHistory`). A changed turn stamp makes it inert at once and pruning removes it, so the next user-authored prompt folds the history; automatic deliveries leave it open. Closing the section clears it too. Both clicks anchor selection, pin manual scroll, and never focus a pane.

The entries classify `sub_agents` by status and the snapshot clock ([`bands.rs`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/bands.rs)), never by the snapshot's `prior_turn` tag: live children (not `success`/`failed`) in the projection's spawn order, then finished children by `last_activity` descending while younger than `[theme.display] recent_subagent_secs` and up to `max_recent_subagents`, then the waits, then the older rest when history is open or when the live and recent bands are both empty (a lone tail would only cost a second click). Every band renders on one token and model column grid measured over all children, so opening history never re-pads rows already on screen. The muted `  +K older` tail shows while history is closed, at least one live or recent row is visible, and `K = sub_agent_count − rows` is positive; children the projection reaped count in `K` with no row to reveal. The shorter list is a prefix of the longer one, and a finishing child moves only from the live band to the head of the recent band.

### What fills the slots

Enrichment is display-only and privacy-gated, and none of it drives attention or routing. Model and reasoning effort come from the session's [rich context](../agents/model.md#rich-context) or the hook and store scalars; a missing field means the agent did not report it, never zero. Account-scoped budgets stay off the card, in the [provider dashboard](#provider-dashboard).

Context severity is one four-tier ramp classified once in the domain ([`ContextSeverity::classify`](../../../crates/rimz/src/agents/state.rs)) and stamped on the row; the renderer maps the row's position between the configured stops ([configuration.md](../../guide/configuration.md#sidebar-bands)) to a continuous tone. `NO_COLOR` removes color only, and each bar's shape still carries the meter.

Three projection rules are not evident from the field catalog:

- The renderer prefers the rich `context` blob over the coarse scalars, except that `row.effort` wins over `context.effort`.
- The description line reads the session name first, then the provider thread preview, task, and prompt, so Codex's generated title replaces its first-message preview.
- Display preferences travel in `SidebarSnapshot.sidebar`.

The status projection is pinned in [`view/tests/status/`](../../../crates/rimz/src/store/snapshot/view/tests/status) and [`project/tests/`](../../../crates/rimz/src/store/snapshot/project/tests).

### Unread and read receipts

Unread state is a runtime episode set (`lanes/unread.json`) plus runtime read receipts (`live/read-marks/`). The elected producer opens an episode for a row whose displayed status is `success`, `failed`, `waiting`, or `paused` when no read mark reaches the row's `last_activity`, and every fold derives `SidebarRow::unread` from that file and the merged receipts. The cap keeps unread rows visible, so a row that returns to `running` or `idle` stays emphasized until read. Attaching to a busy room opens the current attention rows silently, without a burst of notifications.

| Action | Effect |
|---|---|
| focusing a row's pane | writes a receipt and clears that row |
| staying in a tab for `TAB_READ_DWELL` (2.5 seconds) | clears the unread rows in that tab; leaving earlier clears nothing |
| `m` on an unread row, or `rimz sidebar mark-read` | writes a manual receipt without focusing |
| `M` | writes manual receipts for every readable row |
| `m` on a read row, or `rimz sidebar mark-unread` | opens a fresh episode through [`sidebar::unread::mark_rows_unread`](../../../crates/rimz/src/sidebar/unread.rs), stamped at `max(last_activity, now)` so no existing receipt reaches it |

Every fold merges the latest clear time per row, and a receipt clears an episode only when its clear time is at or after the episode, so an old focus cannot erase a later turn. Receipt files are disposable and are swept once their owning heartbeat expires.

### Sub-agent lists

An expanded card lists two kinds of child: provider-native subagents and pane-backed agents another agent launched ([the two meanings of subagent](../harness/subagents.md#two-things-are-called-a-subagent)). Neither renders as its own row while its parent's launch has a rendered row. The rollup carries each child's root `parent_agent_id`, and projection nests every descendant in one flat list. A launched child's link names the parent's launch and a native child's names the parent session; [`AgentState::parent_is`](../../../crates/rimz/src/agents/state.rs) resolves both, and a cross-provider launched child also carries its parent's kind, so mixed links land under one card. The list orders by `registered_at`, and Codex's root-relative task path keeps nested lineage in the label. A launched child's pane stays addressable, but its redundant process row is suppressed.

A child that no rendered parent row claims is an orphan. A pane-backed launched child is promoted to its own top-level row, because its pane is live and would otherwise render nowhere; a paneless native child does not render and is logged ([subagents.md](../harness/subagents.md#who-counts-as-a-launched-child)).

**A finished child stays listed until the parent's next user-authored prompt.** A running child is always listed, and `--keep` keeps a launched child's pane alive. Once a child ends it holds its `✓` or `!` verdict until the parent records a new `user_turn_started_at`.

- A prompt RimZ delivers for an agent or the harness (`AGENT_MESSAGE`, `SUBAGENT_REPORT`, `WAIT`, `SIGNAL`) opens a provider turn (`turn_started_at`) but keeps the entries. The classifier reads the durable submitted prompt: it counts as harness-delivered only when every segment carries a non-user message header. `USER_MESSAGE`, bare leading composer text, or a missing prompt count as the user's, so headerless `--no-from` text and the budget-continue prompt reset the list. Text typed after a pasted header's body cannot be told apart from that body and stays harness-delivered.
- A fresh user prompt, `/clear`, and manual `/compact` each advance the boundary; automatic mid-turn compaction resumes the same turn and keeps the entries.
- With no known user-turn boundary (including carryover older than `user_turn_started_at`), finished entries expire after three hours of child inactivity, the ghost-session TTL. A known stamp survives log rotation.
- A launched child observed to end mid-turn (deadline, `rimz subagents stop`, or provider death) resolves to `failed` and shows `!` ([model.md](../agents/model.md#the-state-machine)). A reaper-inferred end rests it at `idle` with the calm `○` ([observed and reaped ends](../agents/model.md#observed-ends-and-reaped-ends)). Either way its elapsed time freezes at the end stamp.

Before the user-turn filter, projection de-duplicates every child the store still retains and computes `sub_agent_count` and `sub_agent_cost_usd` across both origins. It also computes the launched-only `delegated_cost_usd`, which is added to the parent's own session cost; the all-origin figure is only a breakdown, because native child spend is already in the parent's transcript.

Each origin fills its entry from a different source:

| Child | Source |
|---|---|
| Claude native | the runtime `subagentStatusLine` feed: description, cumulative tokens, exact cost, start time, folded on by [`with_subagent_context`](../../../crates/rimz/src/store/snapshot/view.rs) |
| Codex native | child lifecycle observations: nickname, task path, role, model and effort, and current context tokens, with durable registration as the elapsed-time fallback |
| Copilot native | the model from the parent's start record, with the exact total reconciled from the completion record at the next parent checkpoint |
| launched | its launch profile and session-sidecar cost; the token headline is the cumulative `session_usage` total of input, cache writes, and output, excluding cache reads |

**Child activity counts as the parent's.** [`fold_child_activity_onto_parents`](../../../crates/rimz/src/store/snapshot/view/aggregate/subagents.rs) advances the parent row's displayed clock to the freshest child `last_activity`, so the stall check does not fire and the inactive sinks, ranking, and unread do not move while children work. The clock it replaces is recorded on the card as `own_last_activity`, whose one reader is the card's cache-age pin through `SidebarRow::own_last_activity`: a parent blocked on its children's reports makes no model call, so its prompt cache ages for the whole wait. A child still `running` also makes a resting parent display running or delegating; a child holding a terminal verdict never does. The fold is display-only: the rollup keeps the parent's own clock, a blocked or budget-parked parent stays put, and pause or failure evidence beats a ticking child. Cases are pinned in [`view/tests/subagents/`](../../../crates/rimz/src/store/snapshot/view/tests/subagents).

## Process rows

A pane no agent session claims renders a one-line command row, a lead glyph and the program (`zsh`, `vim`, `cargo`), one tier below the agent cards. It carries no capability line and enters no cockpit tally, and it is still a jump target.

The label comes from the mux command through the shared parser in [`proc/command.rs`](../../../crates/rimz/src/proc/command.rs). It skips environment assignments and transparent wrappers (`sudo`, `doas`, `exec`, `command`, `nohup`, `nice`, `time`, `timeout`, `stdbuf`, `env`, and `sh`, `bash`, `zsh`, `dash`, or `fish` with `-c`), and projects a `node`, `nodejs`, `npx`, or `bun` script or a supervised `rimz agents exec` to its kind while keeping the launcher as the identity-bearing root. Shell script selection is lexical: the first simple command that is not assignment-only or `cd`, `export`, `set`, `source`, or `.` setup. The parser evaluates no expansions, redirections, or control flow, and it keeps the wrapper as the label on unsupported syntax, an incomplete wrapper, or exhausted nesting. Source spans let the details line shorten only the resolved binary's absolute path.

A shared-runtime basename such as tmux's bare `node` resolves to an agent label only when the producer proves a known CLI by walking one bounded single-child chain from the pane root over full `/proc` command lines ([instances.md](../agents/instances.md#recognizing-a-hosted-cli)). A chain that is unreadable, branches, lacks a start time, runs too deep, or matches nothing keeps the runtime label, and the proof never rewrites the mux command. On muxes that report only `comm`, even idle-looking shells are probed, and the matched `foreground_cmdline` feeds activity and details, never identity.

The lead glyph is `ProcessState`: `○` when idle, a braille spinner when busy, and an attention `!` when process metrics show a repeated zombie or uninterruptible sleep for ten seconds with no CPU or I/O progress. A foreground-command change resets the baseline, so a previous command cannot mark its replacement stuck. A working row wide enough right-pins a CPU, RSS, and I/O grid; an idle shell stays bare.

Identity and metrics key off the pane's root pid. tmux reports it as `#{pane_pid}`, and the Zellij presence plugin publishes it from `get_pane_pid` and follows `CommandChanged` and `CwdChanged`, so shell topology stays current without synchronous discovery. [`backfill_zellij_pane_pids`](../../../crates/rimz/src/sidebar/produce/metrics.rs) falls back to foreground-cmdline matching for older plugin builds and failed pid lookups, and abstains when the match is ambiguous (two idle shells in one cwd), because no stats beat another pane's stats.

A proven agent label on a process row changes its class only through wiring: once the adapter is wired, the pane becomes an idle agent card ([ladder rung 4](#the-binding-ladder)), and the first lifecycle event binds the session row. Without integration it stays a process row with the agent label.

When the foreground command runs through `sudo`, `su`, or `doas`, a bounded descendant scan can relabel the row to a known agent CLI running as another real uid, with a dim `(<user>)` marker (`claude (root)` over `sudo su`). The row stays a process row with no tally or dashboard account, and `pane.command` keeps its original value, so cwd pairing and idle synthesis still refuse a foreign-user agent.

## Composing the frame

The sidebar is borderless, because it already sits inside a framed mux pane: a title line, hairline rules, and a one-cell left gutter (which is also the selection lane) carry the structure. The layout is drawn in [the interface reference](../../interface/sidebar.md); these are the rules behind it.

- **Three zones, two pinned.** A top-pinned cockpit, a scrolling viewport over the groups, and bottom-pinned chrome (dashboard, fleet store, footer, health alert). The pinned zones reserve their rows first and the viewport takes the rest, so cards give way before either pinned zone clips. `UiState::scroll_offset` resolves on every draw: clamped to the zone, then scrolled minimally so the selected card is fully in view.
- **Fixed height where it counts.** The repo dashboard and the cockpit make-up reserve their rows whether or not the room has agents, so the body does not shift as agents change state.
- **Selection drives auto-follow.** The viewport follows the selection unless a manual wheel pin holds it. The `↑ N need you` banner appears while the lead card has no line inside the window, and clicking it scrolls to the top.
- **A focus switch reveals the whole group.** When a fold adopts a focus change from outside the sidebar (a tab switch, or the first focused pane learned on attach), the next paint scrolls minimally so the focused card and its group header are both in view. A sidebar-initiated jump cancels this, because its focus anchor freezes the clicked row instead.
- **Counts span the cap.** The cockpit make-up sums each group's `status_counts` over the full roster, with every `running` agent counted as working. Summary line 1 pairs headline facts; the `◎` sessions and token breakdown read the `[sidebar] spend_window` from the JSONL `value_tally`, not the live session sum. Line 2 carries the `¤` live-agent count, the unread count when non-zero, and the count-up spend.
- **The body holds only live rows.** History lives in the store behind `rimz transcript`, `rimz message list`, and `rimz doctor --audit`. An empty room leaves the body clear, and an active health alert takes the body alone.

**The make-up line is the body's filter.** Each non-zero bucket is a click target that narrows the cards to its status, a second click clears it, and a zero bucket has no hit target ([the look](../../interface/sidebar.md#the-cockpit)). The cockpit's unread and open-PR counts work the same way, painting as a chip while active. The pick (`UiState::make_up_filter`) is the one browsing choice shared across a room's renderers: the picking renderer writes it atomically to room runtime ([`body_filter.rs`](../../../crates/rimz/src/sidebar/body_filter.rs)) and broadcasts a payload-free `BodyFilterChanged`, and every other renderer reloads it, so the lens survives a tab switch without a producer write. `reconcile_selection` clears and republishes it when its full-fleet count reaches zero. While the body narrows, the cockpit counts stay full-fleet, and card clicks, `Enter`, inbox traversal, digits, arrows, pages, edges, and group motion all stay inside the filter.

Hit-testing and keyboard motion agree with the painted frame through one mechanism. `FrameInteractions` carries bucket, count, dashboard-tab, banner, group-toggle, and row targets through the same translation and clipping as the rendered lines, and mouse targets and every keyboard walker use the same filtered `VisibleRoster` membership and ordinals as the body composer.

Pane width is a mux operation dispatched off the render thread. The room holds one shared width target; `a`, `d`, and a mouse drag pin it, and each renderer resolves that share against its own view and converges with one resize in flight, never adopting its own late resize as a drag. Zellij fullscreen parks the controller until topology reports fullscreen cleared. The share math and record are [multiplexers.md](../multiplexers.md#width), and the renderer-side controller is [`app/width_control.rs`](../../../crates/rimz/src/sidebar_pane/app/width_control.rs).

### Selection and jump

Selecting a row focuses that agent's pane through the snapshot's `pane` ref; no mux pane number is ever printed. The key table is the [interface legend](../../interface/sidebar.md#keys-and-mouse), and the handlers are [`selection.rs`](../../../crates/rimz/src/sidebar_pane/app/selection.rs).

- **Selection is derived from session focus.** The baseline is `SidebarSnapshot::focused_pane` ([multiplexers.md](../multiplexers.md#who-is-looking-at-what)), filtered to a visible non-sidebar row and moved between pulls by a fused `FocusChanged` overlay. It is keyed by pane (`UiState::selected_pane`), so a reorder keeps the highlight on the same pane. Selection can lag a frame but cannot desynchronize.
- **Transient layers sit above the baseline.** Browse (`↑`/`↓`) pins a pick without moving focus and ends when the derived baseline changes. The order hold pins the last painted order and visible rows after a jump, so cap exemptions settle with the reorder.
- **Motion keys resolve on the input thread.** It builds a `NavKeymap` from `[sidebar.keys]`, keeps crossterm modifiers, and sends the same wire actions as the fixed keys. Page and screen-edge motions read the last painted `FrameInteractions::visible_row_span`, so `Ctrl+f`/`Ctrl+b` step by the visible row count and `H`/`L` target the screen's first and last row.
- **A jump is a durable intent.** `↵`, a click, a digit, or `␣` writes and broadcasts a `Requested` focus intent before the one-way focus command, so every renderer adopts the target, viewport offset, and frozen order before the destination is visible; command acceptance moves it to `Applied` ([state.md](./state.md#focus-intent)). Every renderer seeds `UiState::scroll_offset` from the fresh anchor once, so a cross-tab jump keeps the destination card on the same screen row. Each jump checks pane id and `pane_process_start`, so a reused pane id never focuses a different process.
- **The focus key reaches the sidebar from any pane.** `[sidebar] focus_key` (default `Alt+p`) runs `rimz sidebar focus`: tmux binds it at session birth, and Zellij runs it from the presence plugin ([multiplexers.md](../multiplexers.md#room-keys)).

The row takes the user to the pane; the prompt and its approve or deny stay in the agent's own UI.

### Provider dashboard

Provider budgets are account-scoped: every session of a provider shares one account's windows and paid usage, so budgets live in a pinned per-provider dashboard at the bottom instead of on rows. With several accounts the dashboard is tabbed, one account at a time. The active tab follows the selected pane's provider; `←`/`→` or a tab click pins a manual pick that ends when the selection's provider changes or the tab leaves the dashboard. Tabs are the dashboard's only hit targets. Panel sourcing and aggregation are [providers.md](../agents/providers.md), and the look is [the interface reference](../../interface/sidebar.md#the-provider-dashboard).

- **Remote-control hosts render as a flag.** The snapshot filters the Claude host pane and the Codex app-server broker out of the room and paints a health-colored `⇅ rc` flag on the provider block: green up, red down. Claude health comes from its host pane in the published frame, Codex health from live daemon PIDs in the reap cache. `rimz start` refuses with the fix when a configured host is blocked by settings, version, or auth.
- **The fleet store seals the bottom** with trailing-week `W:` and trailing-month `M:` totals from `SidebarSnapshot::value_tally`: the exact record beside the cockpit's live read.
- **Money counts up.** The cockpit `$` headline and each card's `$cost` roll up toward the exact figure as an eased odometer, snapping on first paint and across epoch resets ([`odometer.rs`](../../../crates/rimz/src/sidebar_pane/render/odometer.rs)). The headline follows the cards through `SidebarSnapshot::today_spend_live_usd`: the walked workspace total excludes active live-card sessions, then adds their current costs back. The renderer never lets the headline decrease within a `today_spend_epoch_secs` window. The figure is presentation; `SpendTally` and per-session cost are the truth ([spending.md](../agents/spending.md#what-reads-the-totals)).

## The serve loop

`rimz sidebar serve` is the renderer process. A supervisor owns the pane command's PID and runs the TUI as a worker, so a worker crash, a reload, and a self-close request each resolve without losing the pane. The worker runs on a fixed timestep, folds each wakeup through [`loop_state.rs`](../../../crates/rimz/src/sidebar_pane/app/loop_state.rs), and paints from the last committed snapshot; data flow into it is [state.md](./state.md#one-fetch-cycle).

The worker tells the supervisor what it wants through its exit code ([`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs)).

| Worker exit | Supervisor action |
|---|---|
| `SELF_CLOSE_EXIT_CODE` (103) | confirm the tab is empty with the mux, then end the pane or respawn ([self-close](#self-close)) |
| `RESPAWN_EXIT_CODE` (102) | restore terminal state, record the death, respawn ([give-up](#degraded-reads-and-give-up)) |
| `RELOAD_EXIT_CODE` (100), or a handoff the supervisor requested | promote the new build ([build promotion](#build-promotion)) |
| anything else, including 0 and a signal | treat as unexpected and respawn |

Respawns back off exponentially from one second to 60 seconds and reset after a stable minute.

Each renderer writes its own heartbeat in process and binds a per-instance wakeup socket. The heartbeat record, its protocol version, and its TTL are wire, in [`wakeup/heartbeat.rs`](../../../crates/rimz/src/wakeup/heartbeat.rs). Heartbeat write failures log as best-effort liveness failures, and the normal relaunch path repairs them.

### Launch

`rimz`, `rimz start`, and cwd-based `rimz attach` ensure the workspace session exists, then launch sidebars best-effort. Both backends run the same renderer. On Zellij, the birth layout's `new_tab_template` carries a left `rimz-sidebar` pane, so every tab is born with one. On tmux, RimZ splits a left sidebar into the first window and repeats the split from an `after-new-window` hook ([multiplexers.md](../multiplexers.md#the-sidebar-and-the-after-new-window-hook)).

Launch is idempotent by heartbeat. Only readable, current-protocol, fresh heartbeats count as live, so a crashed or upgraded sidebar does not block relaunch, and `sidebar-launch.lock` serializes check-then-spawn so concurrent attaches do not each spawn one.

### Drawing

Every animation reads the wall-clock animation phase, not the age of the data, so motion stays smooth over stale data and golden tests pin each frame deterministically. A terminal resize is a wakeup too: a watcher turns `SIGWINCH` into a socket nudge, and the frame repaints through the synchronous input path.

Row emphasis has two depths. The read pulse is an OKLab lightness ramp on the row glyph. The unread blink hard-toggles between the resting tone and a bright crest across the lead glyph, name, description, and make-up buckets, so it reads as on and off at every color depth; under `NO_COLOR` it keeps the shape and the on-phase bold. Palette depth is resolved by the renderer, because terminal capability is local: `theme.mode = "auto"` emits RGB on truecolor terminals and quantized 256-color tones elsewhere ([theme.md](../theme.md#color-depth-and-graceful-degradation)).

### Self-close

A sidebar shares its tab with the user's panes and has no reason to outlive them. The worker requests self-close, and the supervisor decides.

The worker reads the sibling count from the producer's own-view summary ([`SelfCloseState::should_close`](../../../crates/rimz/src/sidebar_pane/app/lifecycle.rs)). Once it has seen a working sibling, a zero count requests self-close immediately, because the producer verifies any shrink toward empty before publishing it. A sidebar that has never seen a sibling (session birth or resurrection, before the mux materializes the tab) waits `SELF_CLOSE_EMPTY_CONFIRM` (5 seconds) of continuous emptiness first, so a tab born sidebar-only still cleans itself up. An unknown count never closes. A producer frame that omits the sidebar's own pane is re-pulled through the mux's `PreferAuthoritative` seam before it feeds the count.

On exit 103 the supervisor runs one authoritative mux listing. A present own pane closes only when its view resolves and has no working sibling. An absent own pane ends supervision only when a second listing, after a short delay, reproduces the absence. Siblings, an unresolved view, a reappearing pane, or any probe error reject the request, record a diagnostic, and respawn the worker, so the decision fails toward keeping the pane.

The renderer-side guards protect paint, not lifetime. A fused `PaneClosed` overlay deletes its card immediately but never changes the sibling count ([state.md](./state.md#fusion-rules)). Closing the last sibling grows the sidebar to full width, so a bounded grow-resize hold in [`lifecycle.rs`](../../../crates/rimz/src/sidebar_pane/app/lifecycle.rs) suppresses the widened repaint while the request reaches the supervisor. The hold arms only when the grow lands beyond the room width target, so attaching a detached-born session repaints at once at its legitimate width. The birth path keeps the hold through its confirm window until a non-empty fold resets it or the supervisor accepts the request.

### Degraded reads and give-up

Give-up is the counterpart of self-close: self-close asks to end the pane when the view empties, and give-up respawns in place when the view can no longer be read.

The worker keeps its last committed frame. When a produce fails (a vanished store, a dead mux, a transient error) the loop reuses that frame and absorbs a single failure silently. A failure that persists past the debounce raises a sticky health alert pinned to the bottom edge (`! Sidebar degraded for 8s: snapshot failed: store not found`), truncating the body so the alert cannot scroll off. On recovery the alert stays as a dim dismissable notice. A producer renderer recovers health only from a completed produce, so a paintable fast fold cannot hide repeated pane-read failure; a consumer recovers on its next successful published read. [`health.rs`](../../../crates/rimz/src/sidebar_pane/app/health.rs) folds these outcomes into `Health`.

A renderer degraded past `GIVE_UP_AFTER_DEGRADED` (30 seconds) exits 102, and the supervisor respawns it in the same pane.

Across worker generations the supervisor also watches that its pane still exists. A fresh presence roster containing the pane resets the strike count without a mux command; supervision ends only after three distinct authoritative absences. The evidence rules are shared with orphan reaping in [multiplexers.md](../multiplexers.md#sidebar-orphan-reaping).

## Reload and repair

Reload replaces the running binary and keeps every pane. Repair fixes the pane structure. `rimz reload --repair` completes the upgrade, then runs repair as a separate operation.

### Build promotion

`rimz reload` is user-wide, independent of cwd, and upgrade-only. It stages the invoking executable once as an immutable user-scoped generation, records its verified path and digest as each live room's durable target, atomically refreshes the room's stable `rimz` hardlink or copy, and sends a version-stable reload word as a latency hint. Each supervisor stats the workspace record once per second, digest-verifies a changed target, and asks its worker to hand off, so a missed datagram costs latency, not correctness. The CLI's heartbeat wait only reports: a sidebar that has not converged keeps retrying from the durable target without a repair.

Promotion is worker-first. The old supervisor launches the target as a worker, waits for it to serve through a stability window, preflights the target's version command, and only then re-execs its own pane-command PID. A crashing or unlaunchable target costs worker respawns under backoff while the old supervisor and pane stay alive, and a later record change breaks the backoff. A successful handoff keeps pane id, tab, geometry, focus, terminal raw mode, and mouse capture.

Bare reload creates and closes no panes. It still reloads refresh dashboards, sweeps stale runtime files, and reaps sidebar processes whose pane is gone, under the double-authoritative evidence rule in [multiplexers.md](../multiplexers.md#sidebar-orphan-reaping). Since a managed room always runs a sidebar, a live session with no fresh, protocol-current heartbeat has either lost its pane or is running a renderer this build cannot signal (`fresh_sidebar_heartbeats` drops both). Reload converges neither, so `upgrade_live` counts it as `sidebar_missing` and the CLI report points at `--repair`, which mounts or replaces a pane ([the line](../../reference/cli/maintenance.md#reload-running-sidebars)). The sidebar `r` key and the stats dashboard `r` key take the same pane-preserving path. The worker's decision is [`app/reload.rs`](../../../crates/rimz/src/sidebar_pane/app/reload.rs), and the Zellij mount caveat is [multiplexers.md](../multiplexers.md#zellij-backend-caveats).

### Zellij plugin upgrades

A reload upgrades the Zellij presence plugin only when its identity differs. The plugin configuration names the room's build-stable `rimz` pointer and includes the embedded wasm digest (computed lazily, once) plus a hash of the loaded configuration, and the plugin echoes both in its topology writer. A fresh matching writer counts as current and skips every start, reload, and retire pipe; a stale, legacy, or mismatched writer runs the replace-and-retire path ([multiplexers.md](../multiplexers.md#retirement)) and reports the upgrade.

### Structural repair

`rimz sidebar repair` converges every view toward exactly one live sidebar: it closes duplicate and orphan sidebars, adds a missing one, and replaces an unclaimed one add-before-close, committing a replacement only after the new pane mounts in the intended view and publishes a current-build heartbeat. It owns the Zellij presence-liveness precondition, and it shares reload's orphan-reap gate. The planner, verdicts, and Zellij mount proof are [multiplexers.md](../multiplexers.md#one-sidebar-per-view) and [in-place repair](../multiplexers.md#in-place-repair).

On Zellij, repair captures one unique fresh client view before changing structure, then restores focus through the same two-phase intent a jump uses: the exact live work pane, or the viewed tab's leftmost work sibling. Unavailable or ambiguous views skip the focus change. Adds and geometry repair in a detached Zellij session wait until a client attaches; tmux mounts splits detached. Repair never closes the daemon view's managed hosts; that view counts as occupied and keeps one sidebar like any working view.

## Resume-on-rebirth

When the machine reboots or the mux server crashes, the agent processes are gone but the store remembers them, and RimZ offers to bring back the agents the sidebar producer last saw alive.

1. Every produce cycle, the elected producer writes `records/live-roster.json`: the current pane-backed, full-session set. Provider-native subagents ride their parent; pane-backed launched children are independent candidates.
2. A birth after a reboot or same-boot crash reads the roster, and [`harness/rebirth.rs`](../../../crates/rimz/src/harness/rebirth.rs) intersects it with the audit rollup without writing. The CLI prompts to recover those agents, defaulting to yes; [`room`](../../../crates/rimz/src/room/mod.rs) owns the mux birth order.
3. After the room exists, materialization records `session.death{cause,lost_agents}`, archives crash caches, allocates fresh team members, stamps missing-worktree and other unrecovered sessions ended, appends `session.rebirth`, and consumes `records/live-roster.json`.

The plan (which sessions resume, team layout, `resume.max`, and the replayed posture) is [fleet.md](../harness/fleet.md#resume-and-rebirth). Restored agents come back idle: the resume argv rehydrates the conversation without a prompt, so no tokens are spent until the user types. Each agent's `SessionStart` fires with `source: "resume"` and re-stamps the new pane, so the row rebinds with the same identity and team members keep their `@role` address. `--no-resume` or `resume.on_rebirth = false` gives a fresh room, and `rimz reset` recovers nothing.

This continuity is RimZ-owned and transcript-based. RimZ disables Zellij session serialization in its rooms, because Zellij resurrects command panes suspended and they read as dead.

## Notifications

Notifications are best-effort over the same attention model, and the store stays authoritative. The elected producer opens unread episodes, applies `[notifications].triggers`, debounce, and focus suppression, spawns matching handlers, and broadcasts `SidebarEvent::Notify`. Each renderer re-rings its local unread `waiting` and `failed` rows at the reminder cadence until they clear, and writes terminal-local OSC and BEL bytes outside the draw cycle. The contract is [notifications.md](./notifications.md).

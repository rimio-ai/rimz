# Sidebar

> [DESIGN.md](../../../DESIGN.md) states the commitments this doc operationalizes, [the interface reference](../../interface/sidebar.md) draws the frame glyph by glyph, and [state.md](./state.md) owns the data plane beneath it: producer election, the fetch cycle, the published caches, events, fusion, and the cadences. This doc owns the mechanics between them: how a pane becomes a row, how rows rank and group, how the frame composes, and how the renderer process launches, reloads, and recovers.

The sidebar is the narrow column pinned beside your panes, and it answers one question: which pane needs you right now. One renderer paints every moment of a session, from a bare shell through a fleet across worktrees to detach and reattach. What changes between those moments is the snapshot, never the renderer.

**One rule shapes the module.** Every decision is made once, in the snapshot, and painting is a pure projection of it. Which agent owns a pane, which worktree a row belongs to, how rows rank, what the cockpit counts: all of it is resolved before a single glyph is chosen. A renderer maps that view-model to lines and never re-derives it. Two consequences follow. Painting stays cheap and testable, because a golden test feeds a snapshot in and diffs the frame out. And the CLI listings (`rimz agents list` and friends) read the same view-model as the native pane, so the two surfaces cannot disagree about what is happening in the room.

That rule splits the module in two, and the split is worth holding in mind while reading the rest of this page. **The producer** folds the store and the live pane roster into one `SidebarSnapshot`: presence, grouping, ranking, and every card field. **The renderer** turns that snapshot into lines for one terminal. `SidebarSnapshot` is the whole contract between them. When you are unsure where a behaviour belongs, ask whether it depends on the terminal in front of one viewer, or on what that viewer is currently looking at: width, color depth, selection, scroll position, and the cockpit lens are the renderer's, and the room's own facts are the producer's.

The sidebar is also a UI client over the store, never a writer of it. Its filesystem writes are only runtime display files (its heartbeat, read receipts, `unread.json`, `focus-anchor.json`, and the producer caches); the elected producer also maintains the best-effort status suffix on mux tab names. Neither is durable truth, and the sidebar imports no store-writer module. `cargo xtask invariants` enforces that boundary.

## From store to screen

One pass builds one frame. The stages run in this order, and each depends on the one before it.

1. **Fold the event log** into the rollup: every durable fact the store holds about the room.
2. **Reduce lifecycle events** into one `AgentState` per agent, carrying turn, phase, subagents, and model.
3. **Read the live pane frame** and bind agents to panes. This is the [admission boundary](#presence-model): a pane the rules exclude folds no row, and an agent with no live pane is data rather than presence.
4. **Classify the rest.** A pane no session claims becomes a [process row](#process-rows).
5. **Reap** ghosts, relaunches, and stale sessions the rollup still carries.
6. **Group and rank.** Rows fold into worktree groups, each row and group gets a score, and the roster sorts.
7. **Enrich.** Unread state, git stats, provider panels, subagent lists, and value tallies attach to the ordered roster.
8. **Serialize** as `SidebarSnapshot`, published for consumers and read by the renderer.
9. **Project to lines.** The renderer resolves the visible roster, composes the zones, and paints.

Steps 1 through 8 happen in the elected producer and are published once for every renderer in the session; the split, its caches, and its timings are [state.md](./state.md#one-fetch-cycle). Step 9 runs per renderer, on the local facts named above plus the terminal's color depth.

The command `rimz sidebar snapshot --json` runs steps 1 through 8 and prints the result. It is the fastest way to see what the renderer sees, and the fastest way to settle whether a bug lives in the producer or the renderer: if the wrong answer is already in the JSON, stop looking at `sidebar_pane/`.

If the snapshot JSON is right and the pane looks wrong, `rimz sidebar frame` reproduces the renderer's output without capturing through the mux.

## Where the code lives

The sidebar spans three module trees, one per half of the split above plus the data plane that feeds it.

**`crates/rimz/src/store/snapshot/` is the producer's view-model builder.** The table lists the modules in the order one snapshot runs them, matching stages 1 through 8 above.

| module | owns |
|---|---|
| [`fold.rs`](../../../crates/rimz/src/store/snapshot/fold.rs) | the resumable event-log fold, its persisted rollup cache, and the carryover that survives log rotation |
| [`project.rs`](../../../crates/rimz/src/store/snapshot/project.rs) | the lifecycle reducer: `agent.lifecycle` events folded into one `AgentState` per agent |
| [`panes.rs`](../../../crates/rimz/src/store/snapshot/panes.rs), [`panes/lazy.rs`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs) | pane binding: which agent owns which live pane, the own-view summary, and the unstamped recovery ladder |
| [`process.rs`](../../../crates/rimz/src/store/snapshot/process.rs) | command classification for panes no agent claims: launchers, `sudo`, the supervised exec wrapper, agent-kind sniffing |
| [`view/live.rs`](../../../crates/rimz/src/store/snapshot/view/live.rs) | folding live panes into rows, and the local-session and activity enrichments over them |
| [`view/reap.rs`](../../../crates/rimz/src/store/snapshot/view/reap.rs) | collapsing ghosts, relaunches, and stale sessions out of the roster |
| [`view/layout.rs`](../../../crates/rimz/src/store/snapshot/view/layout.rs) | grouping by worktree, the row rank key, and group comparison |
| [`view/score.rs`](../../../crates/rimz/src/store/snapshot/view/score.rs) | the fixed-point attention score |
| [`view/aggregate.rs`](../../../crates/rimz/src/store/snapshot/view/aggregate.rs) | group assembly, status tallies, displayed-status projection, child-activity folding |
| [`view/providers.rs`](../../../crates/rimz/src/store/snapshot/view/providers.rs) | the provider dashboard panels |
| [`assemble.rs`](../../../crates/rimz/src/store/snapshot/assemble.rs) | the read entry points, the persisted snapshot, and its lock-free fresh-latest fast path |

The contract types are [`view.rs`](../../../crates/rimz/src/store/snapshot/view.rs) and [`view/model.rs`](../../../crates/rimz/src/store/snapshot/view/model.rs) for `SidebarSnapshot` and its `Sidebar*` members, and [`row.rs`](../../../crates/rimz/src/store/snapshot/row.rs) for `SidebarRow` with its `AgentCard` and `ProcessCard` payloads. `SNAPSHOT_VERSION` gates cross-version adoption.

**`crates/rimz/src/sidebar_pane/` is the renderer process.** `app/` runs the loop, `render/` paints.

| module | owns |
|---|---|
| [`app.rs`](../../../crates/rimz/src/sidebar_pane/app.rs), [`app/loop_state.rs`](../../../crates/rimz/src/sidebar_pane/app/loop_state.rs) | the fixed-timestep serve loop and its wakeup dispatch |
| [`app/fetch.rs`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs) | the off-thread fetch worker: cadence, election, request coalescing, notification state, and publication ([state.md](./state.md#one-fetch-cycle)) |
| [`app/selection.rs`](../../../crates/rimz/src/sidebar_pane/app/selection.rs) | the identity-keyed highlight, the browse layer, and the key and mouse handlers |
| [`app/gate.rs`](../../../crates/rimz/src/sidebar_pane/app/gate.rs) | the last-resort hold that refuses a regressive frame |
| [`app/health.rs`](../../../crates/rimz/src/sidebar_pane/app/health.rs) | failure debounce, the sticky alert, and the give-up rule |
| [`app/lifecycle.rs`](../../../crates/rimz/src/sidebar_pane/app/lifecycle.rs) | the self-close request latch and the grow-resize paint hold |
| [`app/order_hold.rs`](../../../crates/rimz/src/sidebar_pane/app/order_hold.rs) | the renderer-local row and group order freeze |
| [`app/reload.rs`](../../../crates/rimz/src/sidebar_pane/app/reload.rs) | detecting that the workspace build target changed |
| [`app/width_control.rs`](../../../crates/rimz/src/sidebar_pane/app/width_control.rs) | the renderer-local pane width controller |
| [`app/input.rs`](../../../crates/rimz/src/sidebar_pane/app/input.rs), [`app/keymap.rs`](../../../crates/rimz/src/sidebar_pane/app/keymap.rs) | the input-socket wire codec and the configurable navigation keymap |
| [`render/ui_state.rs`](../../../crates/rimz/src/sidebar_pane/render/ui_state.rs) | `UiState`: the renderer-local scroll offset, selection, body filter, and pet view |
| [`view.rs`](../../../crates/rimz/src/sidebar_pane/view.rs) | `VisibleRoster`: body membership, the cap, and stable row ordinals, shared by render, browse, selection, and holds |
| [`render/compose.rs`](../../../crates/rimz/src/sidebar_pane/render/compose.rs) | zone composition, scroll resolution, and the bottom chrome |
| [`render/sections/`](../../../crates/rimz/src/sidebar_pane/render/sections/mod.rs) | one module per zone: `cockpit`, `fleet` (the make-up line), `worktree`, `agent_card`, `process`, `provider`, `pets` |
| [`render/labels/`](../../../crates/rimz/src/sidebar_pane/render/labels/mod.rs) | the glyph and meter vocabulary every section shares |
| [`render/interaction.rs`](../../../crates/rimz/src/sidebar_pane/render/interaction.rs) | typed hit geometry emitted with each painted frame |
| [`render/theme.rs`](../../../crates/rimz/src/sidebar_pane/render/theme.rs) | palette depth and the motion modifiers the terminal supports |
| [`render/animation.rs`](../../../crates/rimz/src/sidebar_pane/render/animation.rs), [`odometer.rs`](../../../crates/rimz/src/sidebar_pane/render/odometer.rs), [`scrollbar.rs`](../../../crates/rimz/src/sidebar_pane/render/scrollbar.rs) | motion, all driven by the wall-clock animation phase rather than data age |
| [`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs) | the supervisor process: build convergence, respawn, pane-liveness, and self-close confirmation |

**`crates/rimz/src/sidebar/` is the data plane.** Producer election, the published caches, realtime events, and fusion live there and are documented in [state.md](./state.md#where-the-code-lives).

Where to start reading depends on the question. For "why is this row here", start at [`view/live.rs`](../../../crates/rimz/src/store/snapshot/view/live.rs) and follow it into `panes.rs`. For "why is this row *there*", start at [`view/layout.rs`](../../../crates/rimz/src/store/snapshot/view/layout.rs). For "why does it look like that", start at [`render/sections/agent_card/template.rs`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/template.rs). For "why did the pane do that", start at [`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs).

## Presence model

**Row presence comes from the live pane frame, not the store.** The producer enumerates the session's panes, assembles them into `PaneFrame` tabs, reads each pane's foreground command and cwd, resolves the cwd to a worktree, and the fold emits one row per admitted pane. No pane id is ever shared by two rows.

[`pane_admits_card`](../../../crates/rimz/src/store/snapshot/panes.rs) is the admission rule. It admits work panes and excludes the caller's own pane, sidebar chrome, and the remote-control and app-server hosts. A pane running a shell becomes a [process row](#process-rows); a pane an agent stamped becomes that agent's row.

**No live pane, no row.** An agent with no live pane is data rather than presence, whether it is a subagent, a ghost a kill left in the rollup, or a relaunch the [reaper](../agents/model.md#liveness-and-presence) has not collapsed. It cannot resurrect a row or latch onto a stranger's pane, and there is no `offline` status.

A relaunch in place is the subtle case, because the old and new sessions claim the same pane. The [reaper](../../../crates/rimz/src/store/snapshot/view/reap.rs) collapses an older same-pane root under either of two proofs: the newer root is a provably different process (a genuine relaunch), or both roots carry fresh lineage from a same-pane `/clear` or `/new`. A same-process thread fork (Codex `/side` or `/btw`) is the exception that must survive: it carries `forked_from_id`, passes through the reap, and stays pinned to the earliest-registered primary.

**Attention lives on the agent's row.** A blocking prompt is agent state: the `awaiting_input` lifecycle signal puts the session's one row in `? waiting`, so a session never stacks a second row for its ask, and the row returns to work once activity passes `waiting_since` ([model.md](../agents/model.md#the-state-machine)). If the agent's pane is absent the row leaves the sidebar with it, and the durable state stays in the store.

**Remote agents are not shown yet.** A remote agent runs only in the daemon with no local pane (`claude remote-control --spawn`, or a Codex thread started from the web). Its status reaches this workspace's rollup, yet it renders nothing, because presence is pane-anchored and there is no pane to bind. Showing remote agents needs a presence class that renders from the rollup alone. This is a known gap, unbuilt this round.

### The binding ladder

**Binding is by identity first.** A pane's foreground command cannot name the agent on its own, because Claude can run under `node` and two same-kind agents in one worktree read identically. Every `agent.lifecycle` event carries the mux pane id the hook ran inside (`TMUX_PANE` or `ZELLIJ_PANE_ID`), and the snapshot binds each live pane to the one agent that stamped that exact id. Command and cwd form a guarded recovery path, used only after no stamp claims a pane.

Joining a session to its pane runs a short ladder, most certain first.

1. **Stamped pane id.** The live pane binds the agent that stamped its exact id. A `session.rebirth` boundary clears every prior stamp at once, because pane ids restart at zero on rebirth so every old stamp names a dead pane. A per-pane process-start guard is the backstop the boundary cannot see: it refuses a stamp whose session predates the pane the id now points at.
2. **Recovered stamp.** A daemon-routed [lazy-registering agent](../agents/model.md#the-instance-lifecycle) starts with no pane id in its hook env, so hook ingestion recovers a durable stamp before appending the first lifecycle event. It reads the same repaired pane frame the producer publishes, filters to same-cwd same-kind panes no other live stamp owns, and writes one candidate directly. Client focus disambiguates plural candidates. Every attempt appends its probes, candidates, and outcome to `binding.log.jsonl`.
3. **Exact-cwd recovery.** An unstamped session binds the live same-kind pane whose cwd equals the session worktree exactly, refused when the session's last activity predates the pane's process start ([`pane_start_allows_bind`](../../../crates/rimz/src/store/snapshot/panes.rs)), so a stale session never latches onto a freshly started pane. This is the normal join for a daemon-routed lazy session, and the rebirth repair for a session whose stamp was cleared while its process kept running. A reborn Codex pane launched as `codex resume <id>` binds that session ahead of the heuristics.
4. **Idle synthesis or process row.** With no session bound, a recognized agent pane with installed hooks or declared local-session discovery renders a synthesized idle `○ <kind>` row until its observation path swaps in the real session ([`idle_agent_row`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs)). A pane with no active observation path stays a process row.

The exact predicates live in [`panes.rs`](../../../crates/rimz/src/store/snapshot/panes.rs) and [`panes/lazy.rs`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs); the cases are pinned in [`view/tests/pane_binding/`](../../../crates/rimz/src/store/snapshot/view/tests/pane_binding) and [`lazy_bind/`](../../../crates/rimz/src/store/snapshot/view/tests/lazy_bind).

**Why a daemon-routed session is unstamped.** Codex under remote control fires its hooks from the shared per-user app-server daemon, so the hook env carries no pane id and its pid is the daemon's, shared by every client. The session id lives only inside the daemon's rollout and socket traffic, in neither the client's environ nor its open files, so a client-pid-to-session join is impossible by construction ([adapter_codex.md](../agents/adapter_codex.md#session-registration-and-launch-quirks)). RimZ therefore binds these sessions from live pane candidates, explicit `codex resume` argv, and pane process-start ordering, with cwd as the shared scope rather than the sole key.

### Honest reads across a mux hiccup

The mux roster can arrive incomplete: a partial pane set, or a live pane missing its command or cwd. Four guards keep a momentary glitch from reaching the screen as a lie.

- **Repaired fields.** The producer joins a fresh raced-null read to the last published process for that exact pane id, and backfills a missing cwd from the process backend, so a just-born pane groups under its worktree on first appearance rather than blinking in as an anonymous row.
- **Carried panes.** A pane a fresh read omits carries forward only while liveness proves it still exists: a matching non-zombie process, or the renderer's own pane by identity. Carried panes are marked, publish a `pane_carry_forward` diagnostic, and expire after `PANE_CARRY_TTL` (30 seconds), so a real exit drops promptly once the process proof is gone.
- **Nameless panes.** A pane with no readable identity after field repair folds no row this frame rather than an anonymous `process` row. The next read names it. An agent-stamped pane still binds by id, so it keeps rendering.
- **The renderer's last-resort hold.** Frame plausibility lives in the producer and row identity in the projection, so the gate absorbs only a residual race that would erase or demote a stable row set. It holds an agent-to-process demotion only when the foreground command is unchanged or missing (the phantom-flicker signature), because a command change means the agent exited in place and the pane returned to its shell, which commits immediately.

The producer-side repairs live in [`produce/panes/`](../../../crates/rimz/src/sidebar/produce/panes.rs); the renderer gate is [`gate.rs`](../../../crates/rimz/src/sidebar_pane/app/gate.rs); the fusion supersession that lets a real close pierce a carried pane is in [state.md](./state.md#fusion-rules).

When pane discovery itself fails, the renderer keeps the last good snapshot and, once the failure persists, raises the sticky health alert rather than inventing an empty room ([Degraded reads and give-up](#degraded-reads-and-give-up)).

## Ranking and grouping

### Worktree groups

A worktree is total isolation, so each group is one bounded block under its header. Two rules the renderer enforces:

- **The header carries no status tally.** The cockpit owns the fleet make-up and each row carries its own glyph. The header is a jump target while the group is live or finished with at most one agent, and an expand/collapse toggle when finished with several agents.
- **Git stats live on the header, never a row**, so shared-worktree changes never pretend to belong to one agent. The `+/-` churn is the worktree's total change against its merge-base with trunk, and the `⇡`/`⇣` delta is the commit count over the same base, so every number measures from the fork point.

The trunk-glyph ladder ranks reconciling (a local rebase or merge in flight) above local-merged, PR-merged, PR-closed, PR-open, and a plain branch. A pristine fork reads `≡ <trunk>`, landed work reads the merge glyph, and a merged PR collapses to that marker alone even when squash-merge ancestry leaves the local branch diverged. The trunk checkout always keeps the plain branch glyph and carries no PR verdict. The trunk resolves per repo, preference first: `[sidebar] trunk` ([configuration.md](../../guide/configuration.md#sidebar-rendering)), then `main`, `master`, and the remote's advertised default. Stat and PR sourcing is [`refresh/git_stats.rs`](../../../crates/rimz/src/sidebar/refresh/git_stats.rs) and [`refresh/pr.rs`](../../../crates/rimz/src/sidebar/refresh/pr.rs); the look is in [the interface reference](../../interface/sidebar.md#worktree-groups-and-the-selection-lane).

Which group a pane lands in follows one resolution order, first match winning.

1. A stamped channel pane groups under its channel pod.
2. An unstamped pane inside the repo's RimZ-owned worktree home folds into that worktree's `#channel` pod, matching message addressing and `rimz channel list`.
3. Any other pane groups by the deepest group root containing its cwd. A repo room enumerates its checkouts with `git worktree list`; a directory room does not scan its tree, and the panes the root itself claims form a name-only root pod.
4. A cwd outside every group root (a home shell, `/tmp`, CI) folds into a catch-all that renders as a dim `external` divider, always sorts last, and keeps an attention-only tally so an out-of-project ask still surfaces from the tail.

The rules are pinned in [`view/tests/grouping/`](../../../crates/rimz/src/store/snapshot/view/tests/grouping).

### Attention ranking and the cap

Within a worktree, agent cards lead and process rows form the command tail. Order reads as three age bands, with a fixed-point score inside each band. Read state never changes the band: unread drives the wash and blink emphasis, the jump inbox, notifications, and the cap's keep-visible rule, and nothing else.

| band | who is in it | how the score behaves |
|---|---|---|
| hot | `last_activity` inside the inactive window (`[agents.attention] inactive_after_secs`, one hour) | attention rows (`waiting`/`failed`/`paused`) heat 1.0x to 2.0x across the window, so an older failure can outrank a fresh ask; calm rows (`success`/`running`/`idle`) stay flat and hold pane creation order |
| warm | past the inactive window, inside `archive_after_secs` (24 hours) | decays 1.0x to 0.0x, so stale asks still lead stale calm rows without competing with current work |
| archive | past the archive window | flat again; the score only orders archived asks above archived idle rows |

The score itself is status weight times the time factor, both fixed-point ([`score.rs`](../../../crates/rimz/src/store/snapshot/view/score.rs)). Status weights are spaced so the lowest attention state still outranks the highest calm state. Process rows are exempt from the inactive and archive bands, because their activity clock is foreground-process start rather than attention: an idle shell stays live presence and seats below every agent card.

**Co-launched agents hold one block.** A named-team launch and an inline multi-agent layout render as one contiguous cohort inside their worktree or channel group. Team blocks use the declared role-list order, including custom layouts; inline blocks use agent-cell order. The block's state derives from its members, first match winning: any `waiting`/`failed` member makes it blocked, else any `paused` member parks it, else any `running` member makes it working, else any `success` member makes it success, else it is idle. Blocked and paused blocks use the oldest attention clock; calm blocks use the most recent member clock.

**Groups carry the same three bands, then a calm rung, then git.** A group partitions hot, warm, and archive by its liveliest member, and leads within a partition with that member's attention score. Below that urgency, three tiebreakers apply in order:

1. The winning band's calm activity sorts working, all-success, idle-agent, then process-only groups.
2. Git refines equal activity by dirty, clean, unknown, then done. Done requires a merged or closed PR or a `WorktreeTrunkSync::Merged` verdict, so a pristine fork stays clean even when merge-base equals `HEAD`.
3. Same-rank groups keep their earliest member's pane creation order, then label.

A non-empty done group with no attention or running member enters archive immediately, and a revived member restores its activity band. The producer's presentation sort and each live overlay refresh stamp this predicate into `SidebarWorktreeGroup::finished`, keeping ranking and rendering on one verdict. `external` groups tail unconditionally, whatever attention member they hold.

The seams are [`sort_rows`/`compare_groups` and the rank key](../../../crates/rimz/src/store/snapshot/view/layout.rs), with cases pinned in [`view/tests/ranking/`](../../../crates/rimz/src/store/snapshot/view/tests/ranking). The reader-facing reasoning is [the sidebar guide](../../guide/sidebar.md#how-the-column-is-ordered).

**The cap protects visible work.** The snapshot carries every row; capping is renderer-local. Each renderer ordinarily caps only a worktree's idle and process tail in the resting body, showing up to `WORKTREE_ROW_CAP` (6) rows with a dim `+K more`; expanding a live group shows every row plus `− less`. Active, blocked, paused, finished, unread, and focused rows are exempt from that cap.

A terminal `finished` group with several agent rows instead collapses every row, unread success included, behind a two-line dim receipt. Process rows do not count toward this threshold, so a process-only group and a group with one agent stay expanded.

The receipt's `▸` roster line leads at the content edge with the one shared `AgentCard::team` value when present, then each agent's final status glyph and display name in source order, with overflow and process rows folded into `+n` and the cohort's lifetime transcript-priced cost pinned right. The totals line carries the same durable lifetime `◇ ↘ ↗ ◌` token split and pins retained active time right, falling back to last-activity age after active-time sidecars expire. The producer publishes the group totals and per-seat lifetime effort keyed by agent-row id through the 60-second `cohort-spend.json` lane; the renderer only formats the projected values. Revealed cards from that finished population prefer the seat's lifetime cost, so they sum to the receipt, while live cards keep their session-scoped self-report. A multi-agent roster too narrow for one member falls back to `▸ +K done`.

Three things reveal the full roster behind a receipt: the header, either receipt line, and the body status filter; the header collapses it again. Focus or the order hold on any member also reveals the whole group, keeping the focused card visible and preventing a half-collapsed receipt, and the collapse lands as a unit once focus leaves and the hold expires. The state is renderer-local and clears when the group no longer hides a tail.

**Producer rank is truth, and the interactive renderer holds it still briefly.** After a jump or browse, or when the focused agent's ask is answered or its turn starts, the renderer keeps the last painted row and group order plus that frame's visible rows for `REORDER_HOLD` (5 seconds), so the glance back finds the cards where you left them. Frozen frames match rows by id, then by pane id, so launch-to-session identity rekeys keep their held slots. A row or group born during the hold splices into the held frame at its producer rank, so expiry does not reorder it. Read state clears immediately; only presentation order and cap exemptions hold.

**Auto-continue** is producer-owned enrichment for `paused` rows: the producer waits for the reset or backoff condition and queues a hidden `Resume` through the durable message pipeline, promoting the row to actionable `failed` when evidenced attempts exhaust rather than spinning silently.

## The cards

Each agent is a small stacked card. Its anatomy and meter grammar are drawn in [the interface reference](../../interface/sidebar.md#the-card); the mechanics are here.

**The card's line set is chosen before any content fills it.** [`CardStage`](../../../crates/rimz/src/sidebar_pane/render/sections/agent_card/template.rs) is `Fresh { labeled }` or `Engaged`, derived only from durable lifecycle facts: status, submitted prompt, session history, context gauge, and RimZ-authored labels, never provider presentation strings. A template table maps stage, expansion, and density to an ordered list of the six card slots (identity, description, awaiting dots, gauge, tokens, subagents). Status enters the table on one arm only, the resting compact card, which trims by status; every other arm reads stage, expansion, and density alone. A unit test pins all four dimensions across every combination. Enrichment only fills the chosen slots, so a card cannot grow a line because a provider started reporting a field.

`[theme.display] card_density` picks which arm of that table applies. `auto` is the standard shape with subagents on every card selection expands, `expanded` puts subagents on every card, and `compact` trims resting cards by status. Selecting a named-team member feeds the same expanded shape to every visible teammate, while selection styling remains exclusive to the focused row. Under auto and expanded, expansion only appends the deeper lines, so the card never reflows. Compact is the scoped exception: a resting compact card is shorter, and expanding it reflows up to its stage's full shape. A fresh expanded card opens the authored description or compose affordance plus the empty context meter, and the serve loop holds the breath cadence while any visible expanded card carries the unlabeled compose affordance.

**Capability is preferred, never guessed.** Model and reasoning effort come from the session's rich [context](../agents/model.md#rich-context-agentcontext) or the hook and store scalar, and a missing field means the agent did not report it, never a zero. Account-scoped budgets stay off the row: they live in the [provider dashboard](#provider-dashboard).

**Enrichments are display-only and privacy-gated**, and none drives attention or routing. Context severity is one four-tier ramp (calm green through yellow and amber to red), classified once in the domain ([`ContextSeverity::classify`](../../../crates/rimz/src/agents/state.rs)) and stamped on each row; the renderer maps the row's position in the config stops ([configuration.md](../../guide/configuration.md#sidebar-bands)) to a continuous tone. `NO_COLOR` suppresses color only, and every bar's shape still carries the meter.

A row is base identity plus one card payload: [`AgentCard`](../../../crates/rimz/src/store/snapshot/row.rs) for lifecycle and context fields, or [`ProcessCard`](../../../crates/rimz/src/store/snapshot/row.rs) for command and process metrics. Three projection rules are not evident from the field catalog.

- The renderer prefers the rich `context` blob over the coarse scalars, but prefers `row.effort` over `context.effort`.
- Codex line 2 reads the app-server thread preview then name, while other agents fall through session name, task, then prompt, so a row keeps a label once the turn ends.
- The display preferences ride `SidebarSnapshot.sidebar`.

The status projection is pinned in [`view/tests/status/`](../../../crates/rimz/src/store/snapshot/view/tests/status) and [`project/tests/`](../../../crates/rimz/src/store/snapshot/project/tests).

### Unread and read receipts

Unread state is a durable runtime episode set (`unread.json`) plus runtime read receipts (`read-marks/`). The elder opens an episode for a row whose displayed status is `success`, `failed`, `waiting`, or `paused` when no read mark reaches the row's `last_activity`, and every fold derives `SidebarRow::unread` from that file plus the merged receipts. The fold stamps the unread bit before rendering, and the renderer cap keeps unread rows visible, so a row that recovers to `running` or `idle` stays visible and emphasized until read. Attaching to an already-busy room opens the current attention rows silently, without replaying a push storm.

Clearing happens four ways. Focusing a row's pane writes a receipt and clears that row. Staying in the tab for `TAB_READ_DWELL` (2.5 seconds) clears unread siblings in it, and leaving before then leaves them unread. `rimz sidebar mark-read` and the `m` key write a durable manual receipt without focusing. Going the other way, `M` and `rimz sidebar mark-unread` open a fresh episode through the one mark-unread write path ([`sidebar::unread::mark_rows_unread`](../../../crates/rimz/src/sidebar/unread.rs)), stamped so no read receipt can reach it.

Every fold merges the maximum clear time per row, and a receipt clears an episode only when its clear time is at or after the episode, so an old focus clear cannot erase a later turn. Receipt files are disposable runtime sidecars, swept once the owning heartbeat expires.

### Sub-agent lists

The expanded card lists both provider-native subagents and pane-backed agents launched by another agent. Neither renders as its own row: the rollup carries each child's root `parent_agent_id` and the snapshot nests every descendant in one flat list at projection time. Cross-provider launched children also carry the parent's provider kind so the pair resolves to the correct card. The list orders by creation time ascending (`registered_at`), stable across refreshes, while Codex's root-relative task path preserves nested lineage in the label. A launched child's pane remains an addressable agent pane, but projection suppresses the redundant process row.

**Provider-native retention is turn-scoped.** A live child stays listed while it belongs to the parent's current turn, and a finished child holds its `✓`/`!` verdict until the parent records its next prompt boundary. A fresh prompt, a `/clear`, and a manual `/compact` each advance that boundary and clear the prior turn's children; automatic mid-turn compaction resumes the same turn and keeps them. The ghost-session TTL is the backstop when the parent never records a boundary. A pane-backed launched child instead follows its own session lifetime across parent turns and expires after it ends plus the same ghost TTL. A child whose parent row is absent is an orphan and never renders.

Claude's description, cumulative token count, exact cost, and start time are paneless enrichment harvested from `subagentStatusLine`; [`with_subagent_context`](../../../crates/rimz/src/store/snapshot/view.rs) folds them onto the child before projection. Codex instead carries nickname, task path, role, model and effort, and current context tokens through child lifecycle observations, with durable registration as the elapsed-time fallback.

**Child activity counts as the parent's.** At projection ([`fold_child_activity_onto_parents`](../../../crates/rimz/src/store/snapshot/view/aggregate/subagents.rs)) the freshest child `last_activity` advances the parent row's displayed clock, so the card's age stays honest and the stall check never false-fires while children work. A live child also makes a clean resting parent display running or delegating. The fold is display-only and guarded: the rollup keeps the parent's own clock, a blocked or budget-parked parent stays put, and pause or failure evidence beats a still-ticking child. Cases are pinned in [`view/tests/subagents/`](../../../crates/rimz/src/store/snapshot/view/tests/subagents).

## Process rows

A pane no agent session claims renders a bare command row: a lead glyph then the program it runs (`zsh`, `vim`, `cargo`), recessed one tier below the agent cards.

The mux command supplies the ordinary label, read past a `sudo` wrapper and through a `node` or `npx` launcher when the script is present. A shared-runtime basename such as tmux's bare `node` resolves to an agent label only when the producer proves a known CLI from one bounded root-to-single-child process-chain walk over full `/proc` command lines. An unreadable, branching, startless, depth-exhausted, or unclassified chain keeps the runtime label. The proof never rewrites the mux command, and tmux's matched `foreground_cmdline` enrichment stays display-only.

The lead glyph is `ProcessState`: a hollow `○` when idle, a braille spinner when busy, and an attention `!` when process metrics report a repeated zombie or uninterruptible sleep sustained for ten seconds with no CPU or I/O progress. A foreground-command change starts a fresh process-state baseline, so the prior tenant cannot mark its replacement stuck. A wide enough working row right-pins a fixed CPU, RSS, and I/O grid on line 1; an idle shell stays bare.

**Process identity and metrics key off the pane's root pid.** tmux reports it natively (`#{pane_pid}`), and the Zellij presence plugin publishes the equivalent from `get_pane_pid`; the producer then performs targeted process enrichment for command and cwd. The plugin also follows `CommandChanged` and `CwdChanged`, so shell topology stays current without synchronous pane discovery. [`backfill_zellij_pane_pids`](../../../crates/rimz/src/sidebar/produce/metrics.rs) retains foreground-cmdline matching as a fallback for old plugin generations and failed pid lookups; an ambiguous fallback, such as two idle shells in one cwd, abstains, because no stats beats a stranger's stats.

A process row carries no capability line and enters no cockpit tally, and it is still a jump target. Wiring the proven adapter promotes it to an idle agent card before a session exists, and the first lifecycle event binds the normal session row. Without installed integration it honestly stays a process row bearing the proven agent label.

When a pane's foreground command runs through `sudo`, `su`, or `doas`, a bounded descendant scan can relabel the row to a known agent CLI running as another real uid, carrying a dim `(<user>)` marker (`claude (root)` above `sudo su`). The row stays a process row: it contributes no tally or dashboard account, and the elevated hint never rewrites `pane.command`, so cwd recovery and idle synthesis still refuse it as a foreign-user agent.

## Composing the frame

The sidebar is **borderless**. It already lives inside a framed mux pane, so a title line, hairline rules, and a one-cell left gutter (which doubles as the selection lane) carry the structure instead of a second border. The on-screen layout is drawn in [the interface reference](../../interface/sidebar.md); the invariants behind it are here.

- **Three zones, two pinned.** The frame composes as a top-pinned cockpit, a scroll viewport over the worktree groups, and bottom-pinned chrome (dashboard, store, footer, alert). The pinned zones reserve their rows first and the viewport takes the remainder, so the cards give way before either pinned zone is clipped. `UiState::scroll_offset` resolves on every draw: clamped to the zone, then minimally auto-scrolled so the selected card sits fully in view.
- **Fixed height where it counts.** The repo dashboard and the cockpit make-up reserve their rows whether or not the room has agents, so the body never shifts vertically as agents change state. The footer and health alert are bottom-pinned chrome, reserved before the body so they can never scroll off.
- **Selection drives auto-follow.** The viewport follows the selection on every draw unless a manual wheel pin holds it. A fresh actionable unread surfaces through ranking, and the `↑ N need you` banner appears while the lead card has no line inside the resolved window; a banner click scrolls to the top.
- **Jump anchors are renderer-to-renderer frame hints.** A sidebar-initiated jump writes `focus-anchor.json` with the focused pane, the clicked card's current viewport offset, and the held order and visible-row set from the source frame. Every renderer reads it on the fold that adopts the focus and seeds `UiState::scroll_offset` once while the anchor is fresh, then adopts the same held frame. Frozen rows match by id first and pane id second, so identity rekeys do not evict held slots. A cross-tab jump therefore keeps the destination card on the same on-screen row instead of re-following that tab's previous selection or fresh rank.
- **A focus switch reveals the whole worktree.** When a fold adopts an external focus change (a tab switch, or the first focused pane the sidebar learns on attach) the renderer arms a one-shot group reveal: the next paint scrolls minimally so the focused card and its worktree header both sit in view. A sidebar-initiated jump cancels the reveal, because its fresh `focus-anchor` freezes the clicked row instead, so the reveal belongs only to focus moves originating outside the sidebar.
- **Counts span the cap, the summary spans the spend window.** The cockpit make-up buckets sum each group's `status_counts` from the full roster, and every `running` agent tallies as working (the thinking head is a per-row head, not a bucket). The resting body may hide calm tail rows, while an active make-up pick shows every matching row uncapped, so bucket counts and narrowed cards agree. The summary's line 1 pairs headline facts, where the `◎` sessions and the token breakdown read `[sidebar] spend_window` from the JSONL `value_tally` rather than the live session sum. Line 2 carries the `¤` live-agent head count, the steady unread count when non-zero, and the count-up spend.

**The make-up line is the body's filter.** Each non-zero bucket is a click target that narrows the cards to its status; re-click clears, and a zero bucket emits no hit ([the look](../../interface/sidebar.md#zone-1--the-cockpit)). The cockpit unread and open-PR counts use the same model: clicking a count applies its lens, and the picked count paints as a chip while active.

That pick (`UiState::make_up_filter`) is the one browsing choice shared across the room's renderers rather than held locally. The picking renderer atomically persists it to room runtime ([`body_filter.rs`](../../../crates/rimz/src/sidebar/body_filter.rs)) and broadcasts a payload-free `BodyFilterChanged` nudge; every other renderer reloads the value from runtime and adopts it, so no producer write is involved and the lens survives a tab switch. Explicit filter clicks and keys replace or clear it, and `reconcile_selection` clears and republishes it when its full-fleet count reaches zero. The cockpit counts stay full-fleet while the body narrows, so the line remains an honest room tally while card clicks, `Enter`, inbox traversal, digits, arrows, pages, edges, and worktree motion all stay inside the active scope.

One mechanism keeps hit-testing and keyboard motion agreeing with what was painted: `FrameInteractions` carries bucket, count, dashboard-tab, banner, group-toggle, and row targets through the same translation and clipping operations as the rendered lines, and mouse row targets and every keyboard walker consume the same filtered `VisibleRoster` membership and ordinal projection as the body composer.

The body carries only live rows. The sidebar shows what needs a decision or an action, and durable history lives in the store behind `rimz transcript`, `rimz message list`, and `rimz doctor --audit`. An empty room keeps the body clear under the cockpit and footer, and an active health alert takes the body alone.

**Pane width is a mux operation dispatched off the render thread.** [`mux/width.rs`](../../../crates/rimz/src/mux/width.rs) owns the pure share, step, and stop-band math used by both convergence engines; [`sidebar/width_target.rs`](../../../crates/rimz/src/sidebar/width_target.rs) owns the persisted room-wide record, its resolve/pin operations, and the broadcast renderers and room options consume. One room-runtime share always exists: unpinned policy follows the live view and configured percentage/cap, while `a`/`d` and mouse drags pin the selected proportion of the view ([multiplexers.md](../multiplexers.md)). Every renderer resolves that share against its own geometry, rounds fractional columns up, and converges to the nearest backend-reachable width with one nudge in flight. After a settled native resize, unchanged view width and sibling count identify a user drag and publish its exact measured share once; view changes re-render either policy or a pin at the new scale, structural sibling changes converge without adoption, and missing geometry never pins. Zellij fullscreen parks this controller until topology reports fullscreen cleared, so override geometry is never mistaken for a user resize.

### Selection and jump

You do not read where to go; you go. Selecting a row focuses that agent's pane via the `pane` ref on the snapshot, and no mux pane number is ever printed. The full key table is the [interface legend](../../interface/sidebar.md#jump--the-row-is-the-link); the mechanics it leans on are in [`selection.rs`](../../../crates/rimz/src/sidebar_pane/app/selection.rs).

- **Selection is derived state, scoped to the session focus register.** The baseline is `SidebarSnapshot::focused_pane` ([multiplexers.md](../multiplexers.md#who-is-looking-at-what)), filtered to a visible non-sidebar row and retargeted between pulls by a fused `FocusChanged` overlay. It is keyed by pane identity rather than row position (`UiState::selected_pane`), so a status-churn reorder re-anchors the highlight to the same pane. Selection cannot desynchronize, only lag a frame.
- **Transient layers ride above the baseline.** Browse (`↑`/`↓`) pins a pick without moving focus and may roam every visible row, ending when the derived baseline genuinely changes. The order hold pins the last painted order and keeps that frame's rows visible for the interaction window after a jump, so cap exemptions settle with the reorder.
- **Motion keys resolve before the loop sees them.** The input thread builds a configurable `NavKeymap` from `[sidebar.keys]`, preserves crossterm modifiers, and sends the same wire actions as fixed keys. Page and screen-edge motions query the last painted `FrameInteractions::visible_row_span`, so `Ctrl+f`/`Ctrl+b` step by visible row count and `H`/`L` target the current screen's first and last row without storing extra viewport state.
- **A jump self-corrects.** `↵`, a click, a digit, or `␣` records and broadcasts a durable `Requested` intent before the one-way focus command, so every renderer adopts the target, viewport offset, and frozen order before the destination is visible. Command acceptance moves that nonce to `Applied` without claiming native confirmation ([state.md](./state.md#focus-intent)). Every jump reconciles pane id and `pane_process_start`, so a reused id never silently focuses a stranger.
- **The focus key reaches the sidebar from any pane.** `[sidebar] focus_key` (default `Alt+p`) toggles `rimz sidebar focus`: tmux binds it at session birth, and Zellij runs it from the presence plugin ([multiplexers.md](../multiplexers.md#the-focus-key)).

The row routes you to the pane; the prompt and its approve or deny live in the agent's own UI, where the full context is.

### Provider dashboard

Provider budgets are **account-scoped, not session-scoped**: every session of a provider shares one account's included windows and paid-usage state. So they lift off the rows into a pinned per-provider dashboard at the bottom. With several accounts the dashboard is tabbed, one account deep at a time. The active tab follows the selected pane's provider, and `←`/`→` or a tab click pins a manual pick that ends when the selection-derived provider genuinely changes or its tab leaves the dashboard.

How each panel's account, plan, windows, and paid row are sourced and aggregated is [providers.md](../agents/providers.md); the look is [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard). This section owns the painting and the tab focus.

- **Remote-control hosts render as a flag.** They are infrastructure, so the snapshot filters the Claude host pane and the Codex app-server broker out of the room and paints a health-colored `⇅ rc` flag on the provider block instead: green up, red down. Claude health comes from its host pane in the published frame, and Codex health from live daemon PIDs in the reap cache. `rimz start` preflights configured hosts and refuses with the fix when one is blocked by settings, version, or auth.
- **The fleet store seals the bottom** with trailing-week `W:` and trailing-month `M:` totals fed by `SidebarSnapshot::value_tally`. These figures are static, the exact record beside the cockpit's coarse live read.
- **The money counts up.** The cockpit headline `$` and each card's `$cost` climb on an increase as an eased odometer roll toward the exact figure, snapping on the first paint and across epoch resets ([`render/odometer.rs`](../../../crates/rimz/src/sidebar_pane/render/odometer.rs)). The headline tracks cards live through `SidebarSnapshot::today_spend_live_usd`: walked workspace headline USD excludes active live-card sessions, then adds those cards' current costs back in full. The renderer ratchets the shown headline within a `today_spend_epoch_secs` window so it does not decrease until the configured spend window rolls. The figure is presentation only; the `SpendTally` and per-session cost are the truth.

The tabs are the panel's only hit targets, and everything else in the dashboard stays inert in the hit-test map.

## The serve loop

`rimz sidebar serve` is the renderer process. A supervisor owns the pane command PID and a worker owns the TUI, so a worker crash, a reload, and a self-close request each resolve without losing the pane. The loop runs on a fixed timestep, folds each wakeup through [`loop_state.rs`](../../../crates/rimz/src/sidebar_pane/app/loop_state.rs), and paints from the last committed snapshot.

Data flow through that loop is [state.md](./state.md#one-fetch-cycle)'s domain. The liveness contract belongs here: each renderer writes its own heartbeat in process and binds a per-instance wakeup socket, and the heartbeat carries the workspace, session, mux, instance id, protocol version, socket path, pane id, build id, semantic version, and last-seen timestamp.

### Launch

`rimz`, `rimz start`, and cwd-based `rimz attach` ensure the workspace session exists, then launch one sidebar pane best-effort. Both backends run the same native renderer. Zellij is born from a layout whose left `rimz-sidebar` pane doubles as the default tab template, so every tab is born with a sidebar; tmux splits a left sidebar into the first window and re-runs the split from an `after-new-window` hook for every later window.

Launch is **idempotent by heartbeat**. RimZ treats only readable, current-protocol, fresh heartbeats as live, so a crashed or upgraded sidebar does not suppress relaunch, and a launch lock serializes check-then-spawn so concurrent attaches do not each spawn a daemon.

### Drawing

Base composition owns the row animation. The gentle read pulse is an OKLab lightness ramp on the row glyph, while the deeper unread blink hard-toggles between the resting tone and a bright crest (a square wave rather than a swell) across the lead glyph, name, description, and the make-up buckets, so it reads as a clear on-off at every depth. `NO_COLOR` keeps the shape plus the on-pole bold weight.

Palette depth is separate: `theme.mode = "auto"` emits RGB on truecolor terminals and quantized 256-color tones elsewhere. The renderer resolves depth itself, because terminal capability is a local fact.

Every animation reads the wall-clock animation phase rather than the age of the fetched data, so motion stays smooth while the data behind it is stale, and golden tests pin each frame deterministically. A terminal resize is also a wakeup (a watcher turns `SIGWINCH` into a socket nudge) and repaints through the synchronous input path.

### Self-close

A sidebar shares its tab with the user's working panes and has no reason to outlive them. **The worker requests, and the supervisor decides.**

Each worker fold counts siblings from the shared pane frame and applies `SELF_CLOSE_EMPTY_CONFIRM` (5 seconds), and reaching zero requests self-close with exit code 103. The supervisor then runs a short authoritative mux listing: a present own pane closes only when its view resolves and has no working sibling, while an absent own pane ends supervision only after a second authoritative listing reproduces the absence after a short delay. Siblings, an unresolved view, a reappearing own pane, or any probe error reject the request, record diagnostics, and respawn the worker. Destructive lifetime decisions therefore fail toward keeping the pane. Worker exit 0 is unexpected and also respawns.

The cache-side guards are latency and paint protection rather than lifetime decisions. A fused `PaneClosed` overlay deletes its card immediately but never changes the sibling count ([state.md](./state.md#fusion-rules)); the producer-verified fold generates the request. Closing the last sibling first grows the sidebar to full width, so a bounded grow-resize hold ([`lifecycle.rs`](../../../crates/rimz/src/sidebar_pane/app/lifecycle.rs)) suppresses the widened repaint while the request reaches the supervisor's confirmation. The hold arms only when the grow lands beyond the room target, so attaching a detached-born session repaints immediately at its legitimate full width. The birth path keeps the same hold through its confirm window until a non-empty fold resets it or the supervisor accepts the empty-tab request.

### Degraded reads and give-up

This is the twin of self-close: self-close requests confirmation when the view empties, and give-up respawns in place when the view can no longer be read at all.

The sidebar keeps its committed render frame across iterations. When the in-process produce fails (a vanished store, a dead mux, a transient hiccup) the loop reuses that frame and absorbs a single blip silently. Once the failure persists past the debounce it raises a sticky **health alert** pinned to the bottom edge (`! Sidebar degraded for 8s: snapshot failed: store not found`), truncating the body before the alert so the notice can never scroll off; on recovery the alert lingers as a dim dismissable notice rather than erasing. Producer renderers recover health only from a completed produce, so a paintable published fast fold cannot mask repeated pane-read failure; consumer renderers have no produce lane and recover from their next successful published read. The app-private fetch reducer ([`health.rs`](../../../crates/rimz/src/sidebar_pane/app/health.rs)) folds those authoritative outcomes into the debounced, sticky `Health`.

A renderer degraded past `GIVE_UP_AFTER_DEGRADED` (30 seconds) exits with the supervisor's respawn status (102) instead of closing its pane. The supervisor restores terminal state, records the death, and respawns the worker in the same pane with exponential backoff from one second to 60 seconds, resetting after a stable minute.

The supervisor's **pane-liveness watchdog** persists across those worker generations. A fresh presence roster containing the pane resets its strikes without a mux command, an absent or unavailable hint escalates through the workspace's single-flight authoritative listing, and supervision ends only after three distinct authoritative absences. Self-close uses its own separate one-probe confirmation.

Heartbeat write failures log independently as best-effort liveness failures, and the normal liveness and relaunch path repairs them. Carried pane truth and gate holds surface as their own dim bottom notices without becoming health failures.

## Reload and repair

Two independent operations share this ground. **Reload** replaces the running binary and preserves every pane. **Repair** fixes the pane structure itself. `rimz reload --repair` is sugar that completes the upgrade first and then invokes the repair path as a separate operation.

### Build promotion

`rimz reload` is user-wide, cwd-independent, and upgrade-only. It stages the invoking executable once as an immutable user-scoped generation, records that verified path and digest as each live room's durable target, atomically refreshes the room's stable `rimz` hardlink or copy, and sends the version-stable reload word as a latency hint. Every supervisor stats the workspace record once per second, digest-verifies a changed target, and asks its worker to hand off, so a missed datagram changes latency rather than correctness. The CLI's heartbeat wait is reporting only: an unconverged sidebar keeps retrying from durable intent without a repair command.

**Promotion is worker-first.** The old supervisor launches the verified target as a reversible worker, waits for that worker to serve through the stability window, preflights the target's version command, and only then re-execs its own pane-command PID. A crashing or unlaunchable target therefore costs worker respawns under bounded backoff while the old supervisor and pane stay alive, and a later record change breaks the backoff and replaces the rejected target. A successful handoff preserves pane id, tab, geometry, focus, terminal raw mode, and mouse capture.

Bare reload creates and closes no panes. It still reloads refresh dashboards, reaps processes whose pane is already gone, and sweeps stale runtime files. The orphan reaper treats cache inclusion as positive liveness evidence and escalates cache omissions to two authoritative mux listings separated by a short delay; either authoritative failure preserves every candidate. Each spared cache divergence and each reaped PID appends a typed diagnostic carrying the mux observation stamps, and a reap also records whether SIGKILL was required. The sidebar `r` key and the stats dashboard `r` key drive the same pane-preserving request path.

The worker's decision is [`app/reload.rs`](../../../crates/rimz/src/sidebar_pane/app/reload.rs), supervisor convergence is [`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs), and the Zellij mount caveat is in [multiplexers.md](../multiplexers.md#zellij-backend-caveats).

### Zellij plugin upgrades

Plugin upgrades are gated by identity rather than run on every reload. The plugin configuration names the room's build-stable `rimz` pointer and includes the lazy-once embedded-wasm digest plus a hash of the loaded configuration, and the plugin echoes both values in its topology writer. A fresh matching writer counts as plugin-current and skips every start, reload, and retire pipe, while a stale, legacy, or mismatched writer runs the generation-proven replace-and-retire path and reports the upgrade loudly.

### Structural repair

`rimz sidebar repair` is the independent structural pass. It owns the Zellij presence-liveness precondition and processes one view at a time: it closes duplicate and orphan sidebars without replacement, adds a missing sidebar, and replaces a live unclaimed sidebar add-before-close.

A replacement commits only after the new pane mounts in the intended view and publishes a current-build heartbeat. Any timeout, unmounted add, or wrong-view mount cleans up the new pane and aborts the remaining transactions. Repair shares reload's double-authoritative process-reap gate and abstains on any confirmation failure.

On Zellij, repair captures one unique fresh client view before structural changes, then submits the same two-phase intent used by renderer jumps to restore that exact live work pane, or the current viewed tab's deterministic leftmost work sibling. Later native observations confirm or supersede the accepted action. Unavailable or distinct views skip focus mutation, and hidden tabs wait for switch repair. Birth seeds only the initial client-visible tab's leftmost work pane. An add or geometry repair in a detached Zellij session is deferred until a client attaches, while tmux mounts splits detached. The daemon view stays occupied, because its hosts are managed rather than user work.

## Resume-on-rebirth

When a session dies because the machine rebooted or the mux server crashed, the agent processes are gone but the store remembers them. RimZ offers to bring back the agents the sidebar producer last saw alive: a birth after a reboot or same-boot crash reads `live-roster.json`, intersects it with the audit rollup, and prompts to recover those agents as running panes grouped into `#channel` tabs, defaulting yes, so the room comes up where the user left off. This is RimZ-owned, transcript-based continuity, deliberately not Zellij session serialization, which resurrects command panes `start_suspended` and so reads as unhealthy. `rimz reset` is a manual fresh start and recovers no agents.

**The mechanism is producer state followed by a two-phase harness rebirth.** The elected producer writes the current pane-backed live full-session set each produce cycle; provider-native subagents ride their parent, while pane-backed agent-launched children remain independent recovery candidates. [`harness/rebirth.rs`](../../../crates/rimz/src/harness/rebirth.rs) inspects that roster and the audit projection without writing, the CLI owns the recovery prompt and presentation, and [`room`](../../../crates/rimz/src/room/mod.rs) owns mux birth ordering. After the room exists, harness rebirth materialization records `session.death{cause,lost_agents}`, archives crash caches, allocates fresh team members, stamps missing-worktree and other unrecovered lost sessions ended, appends `session.rebirth`, and finally consumes `live-roster.json`.

[`harness/resume.rs`](../../../crates/rimz/src/harness/resume.rs) supplies the shared team and flat planning: it keeps each full session bound to a pane in the dead incarnation, collapses reused-pane relaunches to the newest stamp, restores named teams in declared layout order, groups other survivors by worktree or named channel, and applies `resume.max` to the flat fleet after teams. Launched subagents replay their direct parent and generation, while parentless peer agents replay their generation without gaining a parent; a fresh replacement beside a matching seed inherits that same stamp. Each resumed pane runs the supervised exec wrapper with the carried launch identity and the adapter's resume argv, and each missing or unsupported team cell fresh-launches in the matched cwd and channel after recovery is accepted.

The restored agents come back **idle**: `--resume` rehydrates the conversation without a prompt, so no tokens are spent until the user types. Each agent's own `SessionStart` fires with `source: "resume"`, coalescing back onto the same rollup row and re-stamping its new pane, so the row rebinds with no new identity and team members keep their `@role` address. `--no-resume` (or `resume.on_rebirth = false`) opts out for a deliberately fresh start. The resume planner is owned by [fleet.md](../harness/fleet.md), and the loop clock the elder keeps by [loops.md](../harness/loops.md).

## Notifications

Notifications are best-effort polish over the same attention model this doc describes, and the store stays authoritative. The elected producer opens durable unread episodes, applies `[notifications].triggers`, debounce, and focus suppression, then spawns matching notification handlers and broadcasts `SidebarEvent::Notify`. A renderer re-rings local unread `waiting` and `failed` rows at the configured reminder cadence until they clear, and writes terminal-local OSC and BEL bytes outside the draw cycle. The full contract is [notifications.md](./notifications.md).

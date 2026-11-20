# Sidebar data plane

Local contract for `crates/rimz/src/sidebar/`, the view-model the renderer draws. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Behaviour lives in [sidebar.md](../../../../docs/internals/sidebar/sidebar.md) (presence, ranking, reload) and [state.md](../../../../docs/internals/sidebar/state.md) (fetch cycle, lanes, events, fusion, focus intent, cadences): read the matching section before changing one.

Before writing a rule keyed on a pane's tenant or row shape, a liveness test over `snapshot.agents`, or a value stamped at publication, read [Sidebar](../../../../docs/contributing/change-checklists.md#sidebar) and do what it asks.

## Invariants

`cargo xtask invariants` enforces each of these. It greps text, this file included, so a banned path is described in prose and never written out.

- **Read-only on the store.** No store-writer, run-wake, or broker import anywhere under this tree, tests included, nor in `wakeup/` or a harness file this graph reaches. [`store/`](../store/AGENTS.md) owns every write.
- **Event-log history enters through a `RollupCursor` fold.** A whole-log or offset read is rejected here and in [`sidebar_pane/`](../sidebar_pane/AGENTS.md), so snapshot cost follows the log tail and not its length.
- **Enrichment is projection only.** [`enrich.rs`](./enrich.rs) and its submodules, the spine that production and published-cache readers both run, spawn no subprocess and write no cache. Subprocess lanes live in [`refresh/`](./refresh/mod.rs).
- **The fold order is fixed.** The project root, worktree roots, root class, agent and subagent context, agent activity, and session-liveness folds land before live panes, because pane projection and grouping read them.
- **The fold takes the room's state paths from its caller.** Enrichment and agent projection resolve no ambient room path, which keeps each tick out of the machine-wide workspace listing.
- **Only [`produce/`](./produce/mod.rs) writes the published pane frame.** A realtime event never patches it.
- **No spending walker is constructed here.** Spend comes from the elected spending service. A unit test may construct a walker only in a module's sibling `tests.rs`.
- **[`agents/`](../agents/AGENTS.md) never imports this module.** Workspace inputs flow outward through `agents::spending`.

## Truth and latency

- Pulled truth, the store rollup plus the producer's pane frame, decides what is true. Events are latency: they overlay pulled truth in [`fuse.rs`](./fuse.rs) until the next pull supersedes them, and a missed one costs time, never correctness. Which events overlay, how they supersede, and when they expire are this module's policy, not the wire's.
- A pending focus intent outranks both while its anchor holds. This module only reads and folds the anchor; requests, applies, and clears go through [`mux::focus_anchor`](../mux/focus_anchor.rs).
- One host owns the room's data plane. Heartbeat records in [`wakeup::heartbeat`](../wakeup/heartbeat.rs) are launch and wakeup hints, never ownership votes.
- [`timing.rs`](./timing.rs) holds the cadences and TTLs the lanes and the renderer share. A bound that defines one module's own behaviour lives with that module.
- A key held to a slower cadence than its lane (a hibernated agent's checks, through `hibernated_check_due` in [`cache.rs`](./cache.rs)) is checked when the lane's gate opens for its own reasons. Its deadline never opens a shared gate, or N keys on staggered stamps open it every pass. A held item skips only its own probes: it stays in every cross-item count of its pass, with its retained values.
- A record reconciled only on final folds is reconciled inside `engine::reconcile_final_fold` in [`engine.rs`](./engine.rs), the one route the host and testkit playback have to those reconcilers, so a new record is added there. The renderer's host decides when a fold is final; which records reconcile, and in what order, is decided here. Every fold still reads the record, interim folds and published-cache readers included, so when the record orders rows (attention stamps), enrichment derives the value the next reconcile would record, not the stored one.
- The agent projection published for consumers ([`agent_projection.rs`](./agent_projection.rs)) is disposable: re-validate it before use.
- On the [`wakeup/`](../wakeup/mod.rs) wire, an optional field that older readers ignore and newer readers default needs no version bump. A change an older reader would reject or misread carries the envelope or heartbeat protocol version with it.

## Tests

Fold, fusion, cadence, and launch-orchestration tests stay in-module beside the file they cover. As a scoped exception to the repository unit-test default, launch tests may use isolated tempdirs for heartbeat files, but they spawn no process and contact no live mux.

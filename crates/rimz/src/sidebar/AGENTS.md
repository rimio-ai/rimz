# Sidebar data plane

Local contract for `crates/rimz/src/sidebar/` — the view-model the renderer draws. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Subsystem behaviour — presence, ranking, reload — lives in [docs/internals/sidebar/sidebar.md](../../../../docs/internals/sidebar/sidebar.md), and the data plane itself — fetch cycle, caches, events, fusion, cadences — lives in [state.md](../../../../docs/internals/sidebar/state.md).

## Read-only on the store

- This subsystem reads durable state and computes a `SidebarSnapshot`; [`store/`](../store/AGENTS.md) owns every write. `cargo xtask invariants` rejects a store-writer, run-wake, or broker import anywhere under this tree, tests included. The rule greps text, so state it in prose here rather than pasting the banned paths.
- Event-log history enters through a `RollupCursor` fold. A direct whole-log or offset read is rejected here and in [`sidebar_pane/`](../sidebar_pane/AGENTS.md), which is what keeps snapshot cost bounded by the log tail rather than its length.
- [`enrich.rs`](./enrich.rs) is the shared ordered projection spine production and published-cache readers run: it forks no subprocess and writes no cache. The invariant rejects every subprocess-spawning path inside it; subprocess lanes live in [`refresh/`](./refresh/mod.rs).
- The fold order is enforced: live panes land before project roots and worktree roots, so pane-derived identity is present when path enrichment reads it.
- **Invariant.** The fold takes the room's state paths from its caller. The gate rejects ambient room-path resolution in enrichment and agent projection, keeping each tick out of the machine-wide workspace listing.

## Truth and latency

- **Pulled truth is authoritative.** The store rollup plus the producer's pane frame decide what is true.
- **Events are latency.** Wakeup datagrams carrying the [`wakeup::events`](../wakeup/events.rs) vocabulary and [`presence.rs`](./presence.rs) plugin pushes are recorded once in the room plane's shared overlay store [`event_store.rs`](./event_store.rs) and overlay each pane's pulled truth in [`fuse.rs`](./fuse.rs) until the next pull supersedes them. A missed event costs a tick, never correctness. The wire is shared; which events overlay, how they supersede, and when they expire are this module's policy.
- **Pending focus intent outranks both** until the mux confirms the jump or the anchor in [`mux::focus_anchor`](../mux/focus_anchor.rs) expires, so a click never appears to bounce back. This subsystem only reads and folds the anchor; the renderer requests, applies, and clears intents through that module's API.
- **One host owns the room's data plane.** It folds once for subscribed panes and owns publication, notifications, timers, and watchers. Heartbeat records, TTL, and freshness scans in [`wakeup::heartbeat`](../wakeup/heartbeat.rs) remain launch and wakeup hints, not ownership votes.
- Sidebar cadence constants live in [`timing.rs`](./timing.rs) alone, so poll mode, push mode, and the heavy lanes stay legible against each other. A bound that defines another module's behaviour lives with that module: mux command deadlines in [`mux/`](../mux/AGENTS.md), the paint grid in `config`, GC marker lifetimes in `store::gc`.
- **A per-key slow cadence rides its lane's gate.** A key held to a longer interval, such as a hibernated agent's checks through `hibernated_check_due` in [`cache.rs`](./cache.rs), is checked when its lane's gate opens for its own reasons. Its deadline never opens a shared gate, or N keys on staggered stamps open it every pass. A held item skips only its own probes: it stays in every cross-item count of its pass, such as the pane repair's start census or a fleet budget, with its retained values.

## Lanes

- [`produce/`](./produce/mod.rs) owns retained host inputs and private CLI reads. Only the host publishes pane frames, agent projection, and metrics history; its fetch worker reuses those inputs in memory and shares the resulting fold with the refresher. Inspection still refreshes heavy-lane files.
- [`refresh/`](./refresh/mod.rs) owns the heavy lanes — git, provider accounts, credits, pull requests, rate limits, sessions, live spend, team pipelines. Each lane gates on its own TTL, except the pipeline lane, whose one board read per team group runs every pass and publishes only on change; all publish through the cache temp-then-rename helper.
- [`consumer.rs`](./consumer.rs) reads publications passively and supplies the enrichment core shared with retained host folds. Neither fold calls mux, git, or providers.
- The agent projection published for consumers ([`agent_projection.rs`](./agent_projection.rs)) is disposable and re-validated before use.

## Boundaries

- [`mux/`](../mux/AGENTS.md) supplies the pane roster behind the `PaneRef` seam in [`frame.rs`](./frame.rs); backend knowledge stops there. It also owns the runtime files the multiplexer writes or dispatches on: the focus anchor, the room width target, and the Zellij topology and presence-desired caches.
- [`wakeup/`](../wakeup/mod.rs) is the shared wire below this module: the heartbeat record, the event vocabulary, and the best-effort datagram send. `mux`, `store`, and `remote_control` send through it too, so a wire-visible change there carries the envelope or heartbeat protocol version with it.
- [`sidebar_pane/`](../sidebar_pane/AGENTS.md) renders the snapshot and holds no data-plane policy.
- [`agents/`](../agents/AGENTS.md) never imports `crate::sidebar`; the invariant enforces the direction, and workspace inputs flow outward through `agents::spending`.

## Tests

Fold, fusion, cadence, and launch orchestration over the private mux seam stay in-module beside the file they cover. As a scoped exception to the repository unit-test default, launch tests may use isolated tempdirs for heartbeat files, but spawn no process and contact no live mux. Public mux wiring and live-backend launch behaviour stay in [`tests/integration/`](../../tests/integration/AGENTS.md), alongside supervisor, snapshot, and unread behaviour; rendered flows belong to the journey tier and diff-stat cost to the performance tier.

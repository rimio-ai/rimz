# Sidebar data plane

Local contract for `crates/rimz/src/sidebar/` — the view-model the renderer draws. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Subsystem behaviour — presence, ranking, reload — lives in [docs/internals/sidebar/sidebar.md](../../../../docs/internals/sidebar/sidebar.md), and the data plane itself — election, fetch cycle, caches, events, fusion, cadences — lives in [state.md](../../../../docs/internals/sidebar/state.md).

## Read-only on the store

- This subsystem reads durable state and computes a `SidebarSnapshot`; [`store/`](../store/AGENTS.md) owns every write. `cargo xtask invariants` rejects a store-writer, run-wake, or broker import anywhere under this tree, tests included. The rule greps text, so state it in prose here rather than pasting the banned paths.
- Event-log history enters through a `RollupCursor` fold. A direct whole-log or offset read is rejected here and in [`sidebar_pane/`](../sidebar_pane/AGENTS.md), which is what keeps snapshot cost bounded by the log tail rather than its length.
- [`enrich.rs`](./enrich.rs) is the shared ordered projection spine both producer and consumer run: it forks no subprocess and writes no cache. The invariant rejects every subprocess-spawning path inside it; subprocess lanes live in [`refresh/`](./refresh/mod.rs).
- The fold order is enforced: live panes land before project roots and worktree roots, so pane-derived identity is present when path enrichment reads it.

## Truth and latency

- **Pulled truth is authoritative.** The store rollup plus the producer's pane frame decide what is true.
- **Events are latency.** Wakeup datagrams carrying the [`wakeup::events`](../wakeup/events.rs) vocabulary and [`presence.rs`](./presence.rs) plugin pushes land in the receiver's overlay store [`event_store.rs`](./event_store.rs) and overlay pulled truth in [`fuse.rs`](./fuse.rs) until the next pull supersedes them. A missed event costs a tick, never correctness. The wire is shared; which events overlay, how they supersede, and when they expire are this module's policy.
- **Pending focus intent outranks both** until the mux confirms the jump or the anchor in [`mux::focus_anchor`](../mux/focus_anchor.rs) expires, so a click never appears to bounce back. This subsystem only reads and folds the anchor; the renderer requests, applies, and clears intents through that module's API.
- **One producer per workspace.** The eldest live instance by UUIDv7 wins the election in [`mod.rs`](./mod.rs); younger renderers read the cache. The heartbeat is a latency hint and never blocks a fresh launch. Its record, TTL, and freshness scans are wire, in [`wakeup::heartbeat`](../wakeup/heartbeat.rs); the election and launch policy over them is this module's.
- Sidebar cadence constants live in [`timing.rs`](./timing.rs) alone, so poll mode, push mode, and the heavy lanes stay legible against each other. A bound that defines another module's behaviour lives with that module: mux command deadlines in [`mux/`](../mux/AGENTS.md), the paint grid in `config`, GC marker lifetimes in `store::gc`.

## Lanes

- [`produce/`](./produce/mod.rs) is the per-tick pipeline the elected producer runs: group roots, sidecars, pane overlay, refresh-lane projection.
- [`refresh/`](./refresh/mod.rs) owns the heavy lanes — git, provider accounts, credits, pull requests, rate limits, sessions, live spend. Each lane gates on its own TTL and publishes through the cache temp-then-rename helper.
- [`consumer.rs`](./consumer.rs) reads a fresh rollup over the producer's pane cache and calls no mux, git, or provider.
- Projections published for consumers — [`agent_projection.rs`](./agent_projection.rs), [`workspace_projection.rs`](./workspace_projection.rs) — are disposable and re-validated before use.

## Boundaries

- [`mux/`](../mux/AGENTS.md) supplies the pane roster behind the `PaneRef` seam in [`frame.rs`](./frame.rs); backend knowledge stops there. It also owns the runtime files the multiplexer writes or dispatches on: the focus anchor, the room width target, and the Zellij topology and presence-desired caches.
- [`wakeup/`](../wakeup/mod.rs) is the shared wire below this module: the heartbeat record, the event vocabulary, and the best-effort datagram send. `mux`, `store`, and `remote_control` send through it too, so a wire-visible change there carries the envelope or heartbeat protocol version with it.
- [`sidebar_pane/`](../sidebar_pane/AGENTS.md) renders the snapshot and holds no data-plane policy.
- [`agents/`](../agents/AGENTS.md) never imports `crate::sidebar`; the invariant enforces the direction, and workspace inputs flow outward through `agents::spending`.

## Tests

Fold, fusion, election, cadence, and launch orchestration over the private mux seam stay in-module beside the file they cover. As a scoped exception to the repository unit-test default, election and launch tests may use isolated tempdirs for heartbeat files, but spawn no process and contact no live mux. Public mux wiring and live-backend launch behaviour stay in [`tests/integration/`](../../tests/integration/AGENTS.md), alongside supervisor, snapshot, and unread behaviour; rendered flows belong to the journey tier and diff-stat cost to the performance tier.

# Multiplexer backends

Local contract for `crates/rimz/src/mux/` — the Zellij/tmux seam. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Backend behaviour — selection, session birth, layouts, focus, recovery — lives in [docs/internals/multiplexers.md](../../../../docs/internals/multiplexers.md).

## The seam

- Raw pane IDs live only inside the backend adapter; [`pane_from_env_value`](./mod.rs) is the one env→ID mapping, and everything leaves the module as a normalized `PaneId`.
- Every tmux command addresses the RimZ-owned server through [`tmux::managed_cmd`](./tmux.rs) or the backend's own `cmd`, runs from `/`, and clears `$TMUX`; `cargo xtask invariants` rejects a bare `tmux` argv. The endpoint derives from the runtime domain alone, so it never takes a workspace or `disk::paths::RuntimePaths` argument.
- Every control command runs through [`CommandSpec`](./command.rs) under its deadline; on the bound the child is killed and the caller gets `MuxErr::Timeout` — callers degrade, never block, because a wedged mux client otherwise hangs them forever.
- Feature work lands on both backends in the same change. A backend-only channel — the tmux control-mode presence watch, the Zellij presence plugin push — is a latency hint layered over the poll/socket truth.
- Cross-backend structural policy stays pure and above the backends: the one-sidebar-per-view rule and structural execution accounting live in [`reconcile.rs`](./reconcile.rs) as a deterministic planner, current-build heartbeat proof lives in [`mount_proof.rs`](./mount_proof.rs), and sizing math lives in [`width.rs`](./width.rs). Each backend collects inputs and executes the structural plan; native geometry convergence stays adapter-side because ordering and accounting differ.

## Ownership and direction

- `mux` never imports `sidebar`. The wire every renderer listens on (the heartbeat record, the event vocabulary, the datagram send) is the leaf [`wakeup/`](../wakeup/mod.rs) module, which sits beside this one in the layering, above `store`; `mux`, `store`, `remote_control`, and `sidebar` all reach it, and none of them reaches the sidebar through it.
- Every runtime file the multiplexer writes or dispatches on is owned here: the durable focus intent behind each RimZ-initiated jump in [`focus_anchor.rs`](./focus_anchor.rs), the room-wide width target in [`width_target.rs`](./width_target.rs), and the Zellij pane-topology and presence-desired caches in [`zellij/pane_topology.rs`](./zellij/pane_topology.rs). Their path, schema, and freshness rules stay here even where the writer sits elsewhere: the sidebar's Zellij wake ingestion publishes the topology cache through these helpers rather than spelling the file itself.
- A bound that defines another module's behaviour lives with that module. Command deadlines sit beside the commands they bound in [`zellij.rs`](./zellij.rs), the presence-stamp freshness both backends share sits at the mux root in [`mod.rs`](./mod.rs), and [`sidebar::timing`](../sidebar/timing.rs) keeps only the sidebar's own cadences.

## Tests

Planner, width, and selection logic stay unit-level — no mux required. Live-backend behaviour belongs in [`tests/integration/backend/`](../../tests/integration/backend/), which self-skips when the mux binary is absent.

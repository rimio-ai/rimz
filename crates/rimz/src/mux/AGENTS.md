# Multiplexer backends

Local contract for `crates/rimz/src/mux/` — the Zellij/tmux seam. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Backend behaviour — selection, session birth, layouts, focus, recovery — lives in [docs/internals/multiplexers.md](../../../../docs/internals/multiplexers.md).

## The seam

- Raw pane IDs live only inside the backend adapter; [`pane_from_env_value`](./mod.rs) is the one env→ID mapping, and everything leaves the module as a normalized `PaneId`.
- Every tmux command addresses the RimZ-owned server through [`tmux::managed_cmd`](./tmux.rs) or the backend's own `cmd`, runs from `/`, and clears `$TMUX`; `cargo xtask invariants` rejects a bare `tmux` argv. The endpoint derives from the runtime domain alone, so it never takes a workspace or `RuntimePaths` argument.
- Every control command runs through [`CommandSpec`](./command.rs) under its deadline; on the bound the child is killed and the caller gets `MuxErr::Timeout` — callers degrade, never block, because a wedged mux client otherwise hangs them forever.
- Feature work lands on both backends in the same change. A backend-only channel — the tmux control-mode presence watch, the Zellij presence plugin push — is a latency hint layered over the poll/socket truth.
- Cross-backend policy stays pure and above the backends: the one-sidebar-per-view rule and its execution accounting live in [`reconcile.rs`](./reconcile.rs) as a deterministic planner and sizing math in [`width.rs`](./width.rs); each backend collects inputs and executes the plan.

## Tests

Planner, width, and selection logic stay unit-level — no mux required. Live-backend behaviour belongs in [`tests/integration/backend/`](../../tests/integration/backend/), which self-skips when the mux binary is absent.

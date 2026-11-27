# Sidebar pane loop

Local contract for `crates/rimz/src/sidebar_pane/app/`: each pane's loop, its client views, and its input. Extends [sidebar_pane/AGENTS.md](../AGENTS.md).

- A pane pass reads the published record through `ViewAccess::record` in [`app/loop_state/shared.rs`](./loop_state/shared.rs), never the views handle, which takes the record mutex on every call and escapes the `ViewsRecord` pass counter the hidden-pass tests bound.
- Terminal output, mux calls, asset loads, and read-mark I/O run outside the views lock, and the locked step re-reads routing and the latest fold after that preparation.
- A pass that requests a frame because a published record field moved requests it on the pass that first sees the new value, never on every pass while its own state lags: paint can be held back, and a repeated request re-arms an immediate frame for the whole hold.
- An input that acts on what the pane drew reads a record captured at draw time (`UiState`, beside `painted_roster`), never `context.current`: the input route replays publications before the handler runs.
- A test of the frame after an input drives that input through `Rig::input` in [`app/loop_state/tests/mod.rs`](./loop_state/tests/mod.rs), with no fold before its assertion: handler tests under `app/selection/tests/` seat state through the fold's reconcile, so they miss state that only the input path keeps current.

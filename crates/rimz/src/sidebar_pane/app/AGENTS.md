# Sidebar pane loop

Local contract for `crates/rimz/src/sidebar_pane/app/`: each pane's loop, its client views, and its input. Extends [sidebar_pane/AGENTS.md](../AGENTS.md).

- Client views sit under one mutex, and a pane pass reads the published record through `ViewAccess::record` in [`app/loop_state/shared.rs`](./loop_state/shared.rs). Reading the views handle directly takes the record mutex on every call and escapes the `ViewsRecord` pass counter the hidden-pass tests bound. Terminal output, mux calls, asset loads, and read-mark I/O never hold the views lock, and the locked path re-reads routing and the latest fold after unlocked preparation.
- A pane pass that requests a frame because a published record field moved requests it on the pass that first sees the new value, never on every pass while its own state lags: paint can be held back, and a repeated request re-arms an immediate frame for the whole hold.
- An input that acts on what the pane drew reads a record captured at draw time (`UiState`, beside `painted_roster`), never `context.current`: the input route replays publications before the handler runs.
- On tmux a tab switch puts focus on the destination window's active pane, which can be that tab's own sidebar. A rule keyed on focus leaving the sidebar therefore also handles a view arriving at another sidebar pane.
- `LoopState` test rigs share views by default, as every attachment does in production. A test runs without views only when that path is its subject.
- A test of what the frame after an input shows drives that input through the `LoopState` rig (`Rig::input` in [`app/loop_state/tests/mod.rs`](./loop_state/tests/mod.rs)), with no fold before its assertion. Handler tests under `app/selection/tests/` seat state with `reconcile_selection`, the fold's reconcile, so they cannot show state that only the input path keeps current, such as the dashboard reconcile in `PaneState::reduce_input`.

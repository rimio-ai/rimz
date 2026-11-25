# Snapshot

Local contract for `crates/rimz/src/store/snapshot/`: the event-log fold, its lifecycle reducer, and the view model. Extends [store/AGENTS.md](../AGENTS.md).

- Reducers stay pure over the event log plus caller-supplied inputs: nothing here calls the mux.
- Before you change a persisted shape or what a reducer computes, read the doc comment of each thing it reaches: `SNAPSHOT_VERSION` in [`view.rs`](./view.rs) for the published snapshot, and `ROLLUP_CACHE_VERSION`, `RollupCache`, and `EventCarryover` in [`fold.rs`](./fold.rs) for the fold.
- A rule that changes how a lifecycle record folds is one function that the reducer and [`follow.rs`](../follow.rs) both call, since the follower folds apart from the rollup.
- A change to the fold or its reducer times the fold benches, base against head, before hand-off: [Timing comparisons](../../../../../docs/contributing/running-gates.md#timing-comparisons).

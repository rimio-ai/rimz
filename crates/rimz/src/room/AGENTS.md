# Room runtime

Local contract for `crates/rimz/src/room/` — managed room identity and lifecycle. Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

## Boundaries

- Room owns managed room identity and config derivation, sidebar and presence options, birth ordering, health recovery, reset runtime, and the destructive teardown [`teardown.rs`](./teardown.rs) that `rimz reset`, attended auto-reset, incompatible-room replacement, and uninstall all run.
- CLI owns prompts, presentation, and attach execution.
- `harness::rebirth` owns rebirth inspection, planning, and materialization.
- `mux` owns backend commands and layout mechanics, the cross-backend live-session snapshot room inventory borrows, the guarded process sweep teardown calls, and the room-wide width target room adopts, resolves, and clears at birth.
- `sidebar` owns launch election, its published caches, and the rebirth heartbeat purge. Store GC owns lifetime-class reclamation; teardown clears disposable runtime classes after the process sweep confirms exit or warns about survivors. A hard reset alone then ends the processes still inside the room's sandbox views, selected by `mux`, and refuses before it opens the store when one survives.
- `wakeup` owns the renderer heartbeat record and its TTL, which room reads to judge a session live.

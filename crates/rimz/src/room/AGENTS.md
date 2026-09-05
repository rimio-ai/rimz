# Room runtime

Local contract for `crates/rimz/src/room/` — managed room identity and lifecycle. Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

## Boundaries

- Room owns managed room identity and config derivation, sidebar and presence options, birth ordering, health recovery, reset runtime, and the destructive teardown [`teardown.rs`](./teardown.rs) that `rimz reset`, attended auto-reset, and uninstall all run.
- CLI owns prompts, presentation, and attach execution.
- `harness::rebirth` owns rebirth inspection, planning, and materialization.
- `mux` owns backend commands and layout mechanics, the guarded process sweep teardown calls, and the room-wide width target room adopts, resolves, and clears at birth.
- `sidebar` owns launch election, its published caches, the rebirth heartbeat purge, and the orphan runtime sweep room runs during teardown before the process sweep.
- `wakeup` owns the renderer heartbeat record and its TTL, which room reads to judge a session live.

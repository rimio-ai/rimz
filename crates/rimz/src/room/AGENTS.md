# Room runtime

Local contract for `crates/rimz/src/room/` — managed room identity and lifecycle. Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

## Boundaries

- Room owns managed room identity and config derivation, sidebar and presence options, birth ordering, health recovery, and reset runtime.
- CLI owns prompts, presentation, and attach execution.
- `harness::rebirth` owns rebirth inspection, planning, and materialization.
- `mux` owns backend commands and layout mechanics.
- `sidebar` owns launch election plus heartbeat, cache, and width-override mechanics.

# Room runtime

Local contract for `crates/rimz/src/room/`. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Admission, the health gate, reset, and the teardown sweep are specified in [multiplexers.md § Session lifecycle](../../../../docs/internals/multiplexers.md#session-lifecycle).

- Room owns the order of the lifecycle, and its neighbours own the mechanics: the CLI owns prompts, presentation, and attach execution; `harness::rebirth` owns rebirth inspection, planning, and materialization; `mux` owns backend commands, the live-session snapshot, and the process sweep teardown calls; `sidebar` owns launch election and the rebirth heartbeat purge; `wakeup` owns the heartbeat record and its TTL; store GC owns lifetime-class reclamation.
- [Admission](../../../../docs/internals/multiplexers.md#room-admission) runs before recovery inspection, before the owner or login pins are recorded, and before every startup prompt except the first-start multiplexer question, whose answer picks the backend that admits. Reset admits before it prompts or tears down. A step added to entry goes after admission.
- The room claim is released before birth returns, never held across client attach.
- Incompatible-room replacement takes an exclusive room claim before any teardown; a fresh sidebar heartbeat never bypasses it.
- A birth step whose failure leaves state every reader still accepts (the team-notes layout migration in [`birth.rs`](./birth.rs)) warns and continues; it never refuses birth.
- Fail-fast, settled: The room still refuses for what is not configuration: a missing, old, or unresponsive multiplexer, socket budgets, the project claim, store or runtime state, a birth inside a scoped pane, remote version skew, and a malformed or unusable `--account`.

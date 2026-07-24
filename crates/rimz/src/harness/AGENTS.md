# Agent harness

Local contract for `crates/rimz/src/harness/` — the spawn, address, drive, and reclaim machinery for agent sessions. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules.

Topic detail lives in [harness.md](../../../../docs/internals/harness/harness.md) (spawn, address, resume, reclaim), [scripting.md](../../../../docs/internals/harness/scripting.md) (supervised runs), [loops.md](../../../../docs/internals/harness/loops.md) (loop scheduling and the assist log), and [budget.md](../../../../docs/internals/harness/budget.md) (dollar caps and the park), with the shared delivery substrate in [messaging.md](../../../../docs/internals/harness/messaging.md).

## Boundaries

- The harness owns layout specs, effective launch resolution, profile prompt-file validation, launch finalization, provider-process compilation and argv, stable handles, supervised run records, the durable blocking-wait policy and run-wake socket (`run_wake.rs`), rebirth inspection and materialization, cohort and lane qualification/resume planning, relaunch posture resolution (`resume::resolve_posture`, shared by restart, fork, and every resume path), lane restore materialization, loop scheduling, hidden runner domain, auto-continue policy, and idle-compaction policy.
- Harness delivery rides `message`; it never reimplements queues, gates, retries, or transcript audit.
- CLI handlers keep argument parsing, presentation, and cross-command orchestration. Harness modules own pure domain rules, durable records, helper argv shape, and side-effect boundaries.
- Elder-fired helpers in `schedule/fire.rs`, `auto_continue.rs`, `auto_redeem.rs`, and `idle_compact.rs` spawn hidden CLI subprocesses with fresh null stdio.
- `auto_continue.rs`, `auto_redeem.rs`, and `idle_compact.rs` are in the sidebar import graph. Keep them free of store-writer, run-wake, and broker imports; runtime-cache writes through `store::atomic::write_temp_then_rename_cache` are the allowed durability path.
- Assist records append from detached helper CLI handlers and the loop runner; sidebar-graph modules only pass the evidence those writers need.

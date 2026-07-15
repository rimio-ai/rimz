# Agent harness

Local contract for `crates/rimz/src/harness/` — the spawn, address, drive, and reclaim machinery for agent sessions. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules.

Topic detail lives in [harness.md](../../../../docs/internals/harness/harness.md), with the shared delivery substrate in [messaging.md](../../../../docs/internals/harness/messaging.md).

## Boundaries

- The harness owns layout specs, effective launch resolution, profile prompt-file validation, launch finalization, provider-process compilation and argv, stable handles, supervised run records, the durable blocking-wait policy and run-wake socket (`run_wake.rs`), rebirth inspection and materialization, cohort and lane qualification/resume planning, lane restore materialization, loop scheduling, hidden runner domain, and auto-continue policy.
- Harness delivery rides `message`; it never reimplements queues, gates, retries, or transcript audit.
- CLI handlers keep argument parsing, presentation, and cross-command orchestration. Harness modules own pure domain rules, durable records, helper argv shape, and side-effect boundaries.
- Elder-fired helpers in `schedule/fire.rs` and `auto_continue.rs` spawn hidden CLI subprocesses with fresh null stdio.
- `auto_continue.rs` is in the sidebar import graph. Keep it free of store-writer, run-wake, and broker imports; runtime-cache writes through `store::atomic::write_temp_then_rename_cache` are the allowed durability path.

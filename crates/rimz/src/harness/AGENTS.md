# Agent harness

Local contract for `crates/rimz/src/harness/` — the spawn, address, drive, and reclaim machinery for agent sessions. Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

Topic detail lives in [fleet.md](../../../../docs/internals/harness/fleet.md) (spawn, address, resume, reclaim), [scripting.md](../../../../docs/internals/harness/scripting.md) (supervised runs), [loops.md](../../../../docs/internals/harness/loops.md) (loop scheduling, signals and one-shot wakes, and the assist log), and [budget.md](../../../../docs/internals/harness/budget.md) (dollar caps and the park), with the shared delivery substrate in [messaging.md](../../../../docs/internals/harness/messaging.md).

## Boundaries

- The harness owns layout specs, effective launch resolution, profile prompt-file validation, launch finalization, provider-process compilation and argv, launch reminder assembly (`launch_reminders.rs` owns the one `<system_reminder>` tag, its paragraph order, and the model line; the team and catalog producers return bodies and never wrap), stable handles, supervised run records, the durable blocking-wait policy and run-wake socket (`run_wake.rs`), rebirth inspection and materialization, cohort and lane qualification/resume planning, relaunch posture resolution (`resume::resolve_posture`, shared by restart, fork, and every resume path), lane restore materialization, loop scheduling, hidden runner domain, auto-continue policy, and idle-compaction policy.
- Harness delivery rides `message`; it never reimplements queues, gates, retries, or transcript audit.
- `schedule/signal.rs` owns the signal vocabulary: the `SignalName` grammar, the `agent.`/`wake.` prefixes reserved from user emitters, the lifecycle mapping, and `fire_signal`. Firing is in-process, with no queue and no replay: whichever process observes the event fires the tasks armed for it right there, and a signal nobody subscribes to is gone. A detached watcher is single-flight through its `RuntimePaths` lock, and losing that lock is an outcome the elder resolves, never something the watcher retries.
- Handle rendering and address resolution read the addressable peer set; raw root rows are for accounting scans.
- CLI handlers keep argument parsing, presentation, and cross-command orchestration. Harness modules own pure domain rules, durable records, helper argv shape, and side-effect boundaries.
- Elder-fired helpers in `schedule/fire.rs`, `auto_continue.rs`, `auto_redeem.rs`, and `idle_compact.rs` spawn hidden CLI subprocesses with fresh null stdio.
- `auto_continue.rs`, `auto_redeem.rs`, `budget.rs`, `idle_compact.rs`, `orphan_sweep.rs`, and `run_timeout.rs` are in the sidebar import graph. Keep their evaluation side read-only and delegate durable mutations or provider transport to detached helpers; runtime-cache writes through `disk::atomic::write_temp_then_rename_cache` are the allowed durability path.
- Assist records append from detached helper CLI handlers and the loop runner; sidebar-graph modules only pass the evidence those writers need.

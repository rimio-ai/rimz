# Agent harness

Local contract for `crates/rimz/src/harness/`, the spawn, drive, and reclaim machinery for agent sessions. Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

Behaviour is documented per topic under `docs/internals/harness/`: [fleet.md](../../../../docs/internals/harness/fleet.md) (launch, address, resume, reclaim), [scripting.md](../../../../docs/internals/harness/scripting.md) (supervised runs), [subagents.md](../../../../docs/internals/harness/subagents.md), [workflow.md](../../../../docs/internals/harness/workflow.md), [loops.md](../../../../docs/internals/harness/loops.md) (scheduling, signals, waits, the start throttle, the assist log), [teams.md](../../../../docs/internals/harness/teams.md), [budget.md](../../../../docs/internals/harness/budget.md), [hibernate.md](../../../../docs/internals/harness/hibernate.md), and [messaging.md](../../../../docs/internals/harness/messaging.md) (the delivery substrate).

## Boundaries

- CLI handlers keep argument parsing, presentation, and cross-command orchestration. Harness modules own domain rules, durable records, helper argv shape, and side-effect boundaries.
- Harness delivery rides `message`; it never reimplements queues, gates, retries, or transcript audit.
- `harness` never reaches `sidebar`: a helper that refreshes provider usage returns it, and its CLI entry publishes through `sidebar::refresh`.
- **Invariant.** `plan` never imports `resume` (`cargo xtask invariants`), and `harness::workflow` imports nothing from `harness::schedule` (its `[[module]]` entry in `refactor-target.toml`).

## The sidebar import graph

- **Invariant.** The store-write gate in `cargo xtask invariants` searches every harness file the sidebar's production code imports, directly or through other harness files. A file joins that set by being imported, not by a list, so a new import from a sidebar-reached file can fail the gate in a file the change never edited.
- A sidebar-reached file keeps its evaluation side read-only: it selects, decides, and paces, and a detached helper outside the graph does the durable mutation (an assist record included), the provider transport, and the stop. Its only writes are runtime caches through `disk::atomic::write_temp_then_rename_cache`, and its reads take no lock and run no migration.

## Host-fired helpers

- A host pass spawns its helper as a hidden CLI subprocess with fresh null stdio.
- A helper takes every config-derived value it needs (a team's leader role, a window) from its request, resolved by the host pass, or loads config from the room's project root, never from the checkout path in its request: project trust is keyed on the project root, so a linked worktree's config reads as untrusted and drops the project's `[agents.teams]`.

## One home per rule

Each of these is decided in one place. Call it; a second copy drifts.

- Launch: `launch_reminders.rs` owns the one `<\system_reminder>` tag: producers return bodies and never wrap, and no prompt file adds a tag.
- Relaunch: `resume::resolve_posture` is the one posture resolution for restart, fork, and every resume path.
- Supervised runs: every status transition is a locked read-modify-write in `run.rs` over the `store::run` record. `response_path` and `earlier_response_path` are the one derivation of a response file. A reflex that asks an agent to end its turn registers its remaining work in `owed.rs`, or a supervised run completes at that turn end.
- Fleet: `fleet.rs` holds the detached and held rules (`FleetRuns`, `owed_team_runs`, `ended_team_runs`), so no consumer filters on `report_to` or `limit_hold` itself.
- Signals: `WatchVerdict::label` is the one wording of a watch outcome, `wait_output_path` the one derivation of a wait's output file, and `From<&Signal> for SignalEventPayload` the one conversion into the store, so no store module reaches up for a harness signal type.
- Loop tasks: a wait row's target is matched only through `schedule/pending.rs`; `retire_session` is the one removal primitive and `retire_ended_sessions` the one spelling of "durably ended"; `in_flight_run` is the one lookup of a run lock's holder; terminal history rows are appended only through `run_log`, and the CLI renders the transition it gets back.

## Fail-fast

- Fail-fast, settled: a named exception to the root rule. Hibernation is enrichment on every platform, on by default and skipped where Linux, swap, a cgroup freezer, or a systemd user manager is unavailable. `rimz doctor` says why hibernation is skipped, and an explicit `[agents.hibernate] enabled = true` warns once at `rimz start` on Linux for a missing capability.


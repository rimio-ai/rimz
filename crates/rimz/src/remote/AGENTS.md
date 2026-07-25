# Remote attach

Local contract for `crates/rimz/src/remote/` — the SSH attach library. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Subsystem behaviour — attach and aliases, the reconnect supervisor, link health, port forwarding, bandwidth — lives in [docs/internals/remote.md](../../../../docs/internals/remote.md).

## Pure library, process-free

- This module parses targets, builds argv, classifies exits, and advances state machines from caller-supplied durations. It spawns no child, reads no clock, and writes no terminal.
- `cli/remote/` owns every process, timer, thread, and terminal write: the supervisor, the outage panel, link stats, tty restore, setup, and list. A new behaviour lands as a pure transition here plus its driver there.
- Every SSH invocation compiles to a [`mux::command::CommandSpec`](../mux/command.rs), so a remote command carries the same deadline and kill discipline as a local one.
- State machines take durations as arguments and return the next state. That is what makes reconnect backoff, recovery timing, and link health testable without sleeping.

## The two sides

- Local owns the SSH child, reconnect policy, terminal hygiene, the recovery panel, link measurement, and port forwards.
- Remote owns the room: workspace, session, sidebar, store, and the health gate.
- Neither side reaches across, and they share no environment beyond the launch-snippet variables. Keep a new signal on the side that can observe it.

## Layout

- [`mod.rs`](./mod.rs) — the `[user@]host:<session-or-path>` grammar, the guarded ssh command, and the autossh-style reconnect policy.
- [`aliases.rs`](./aliases.rs) — per-machine aliases in `remote.toml`; [`version.rs`](./version.rs) — client/host skew classification.
- [`link.rs`](./link.rs) — the health JSONL protocol and terminal-session transitions; [`reachability.rs`](./reachability.rs) — endpoint discovery and reconnect-wait state; [`recovery.rs`](./recovery.rs) — panel timing and checkpoints.
- [`forward.rs`](./forward.rs) — listener discovery and live ControlMaster forwards; [`web.rs`](./web.rs) and [`setup.rs`](./setup.rs) — argv builders whose I/O belongs to the CLI; [`tty.rs`](./tty.rs) — terminal-state hygiene.

## Tests

Grammar, classification, and every state transition stay in-module and pure. Attach behaviour that needs a subprocess lives in [`tests/integration/`](../../tests/integration/AGENTS.md), and rendered outage and reconnect flows belong to the journey tier.

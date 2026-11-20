# Remote attach

Local contract for `crates/rimz/src/remote/`, the SSH attach library. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). [remote.md](../../../../docs/internals/remote.md) owns subsystem behaviour, the [file map](../../../../docs/internals/remote.md#where-the-code-lives), and the [invariants to preserve](../../../../docs/internals/remote.md#invariants-worth-preserving).

- This module is pure: it parses targets, builds argv, classifies exits, and advances state machines from the instants and durations its caller supplies. It spawns no child or thread, reads no clock, and writes no terminal. Two kinds of I/O are admitted: `aliases.rs` reads and writes `remote.toml`, and `web.rs` binds loopback listeners to pick free local ports.
- `cli/remote/` owns every process, timer, thread, and terminal write. A new behaviour lands as a pure transition here, with a unit test, plus its driver there.
- Every SSH invocation compiles to a [`mux::command::CommandSpec`](../mux/command.rs), so a remote command carries the same deadline and kill discipline as a local one.
- The local side owns the SSH child and the link; the remote host's `rimz` owns the room. Neither reaches across, and they share no environment beyond the launch-snippet variables. Keep a new signal on the side that can observe it.
- A snippet the remote login shell runs is tested by executing the generated text under `sh`, and under `zsh` when present, with the network commands stubbed: a substring test cannot see a zsh-only reserved word such as `status`.
- Grammar, classification, and every state transition are tested in-module with no sleep. Attach behaviour that needs a subprocess lives in [`tests/integration/`](../../tests/integration/AGENTS.md) ([test seams](../../../../docs/internals/remote.md#test-seams)), and rendered outage and reconnect flows belong to the journey tier.

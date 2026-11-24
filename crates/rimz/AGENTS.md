# RimZ core

Local contract for `crates/rimz/`. Extends the root [AGENTS.md](../../AGENTS.md); each subsystem under `src/` and `tests/integration/` adds its own.

- **Invariant.** Production code resets a child's environment only through `child_process::clear_env_preserving_coverage`; `cargo xtask invariants` rejects a direct reset anywhere else under `src/`. The rule covers how a reset is done, not whether one is needed: a `rimz` helper child inherits the process environment.
- One agent is one launch id: a match that decides whether two rows are the same agent compares launch ids, never `(kind, session id)`.
- Domain modules carry no Zellij, tmux, or agent-specific knowledge: backend knowledge enters through `mux/`, agent knowledge through `agents/`.
- Outside `mux/`, a pane ID is always the normalized form (`zellij:terminal_3`, `tmux:%3`); raw IDs stay inside the backend adapters.
- A sentence or `console` block in rustdoc or help text follows [Sentences about the code](../../docs/AGENTS.md#sentences-about-the-code).

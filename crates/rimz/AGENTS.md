# RimZ core

Local contract for `crates/rimz/`. Extends the root [AGENTS.md](../../AGENTS.md); each subsystem under `src/` and `tests/integration/` adds its own.

- **Invariant.** Production code resets a child's environment only through `child_process::clear_env_preserving_coverage`; `cargo xtask invariants` rejects a direct reset anywhere else under `src/`. The rule covers how a reset is done, not whether one is needed: a `rimz` helper child inherits the process environment.
- Domain modules carry no Zellij, tmux, or agent-specific knowledge: backend knowledge enters through `mux/`, agent knowledge through `agents/`.
- Outside `mux/`, a pane ID is always the normalized form (`zellij:terminal_3`, `tmux:%3`); raw IDs stay inside the backend adapters.
- A `rimz` child that acts on an object of another room passes `--root <that room's root>`, or removes `RIMZ_PROJECT_ROOT` and `RIMZ_WORKSPACE_ID` when it must keep a checkout of its own: every room pane exports that pin, and `WorkspaceResolver::resolve_participant` ranks it above the directory.
- `web` never imports `sidebar_pane`: the kitty placeholder encoding both speak is the leaf `pixel_wire` module.

## Before a change

- A sentence or `console` block in rustdoc or help text follows [Sentences about the code](../../docs/AGENTS.md#sentences-about-the-code).
- A change to any `Cargo.toml` dependency, optional and feature-gated ones included, runs `cargo xtask externals`.
- A new `disk/` module gets a row in the Disk module table of [store.md](../../docs/internals/store.md).

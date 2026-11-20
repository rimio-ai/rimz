# RimZ core

Local contract for `crates/rimz/`. Extends the root [AGENTS.md](../../AGENTS.md); each subsystem under `src/` and `tests/integration/` adds its own.

- **Invariant.** Production code resets a child's environment only through `child_process::clear_env_preserving_coverage`; `cargo xtask invariants` rejects a direct reset anywhere else under `src/`. The rule covers how a reset is done, not whether one is needed: a `rimz` helper child inherits the process environment.
- Domain modules carry no Zellij, tmux, or agent-specific knowledge: backend knowledge enters through `mux/`, agent knowledge through `agents/`.
- Outside `mux/`, a pane ID is always the normalized form (`zellij:terminal_3`, `tmux:%3`); raw IDs stay inside the backend adapters.
- A run wake is matched on `(workspace_id, run_id)`, never on a PID alone.
- `web` never imports `sidebar_pane`: the kitty placeholder encoding both speak is the leaf `pixel_wire` module.

## Before a change

- A sentence or `console` block in rustdoc or help text: read [Doc sentences](../../docs/contributing/change-checklists.md#doc-sentences) and [Console blocks](../../docs/contributing/change-checklists.md#console-blocks) and do what they ask.
- `nix`, `libc`, or platform-gated code, or a new call to a `#[cfg(target_os = ...)]` item, test helpers included: run the [cross-target checks](../../docs/contributing/running-gates.md#cross-target-checks), since `gate` compiles the host target only.
- A change to any `Cargo.toml` dependency, including an optional or feature-gated one, runs `cargo xtask externals`: cargo vet audits every non-dev dependency at safe-to-deploy, and a dependency that only the `testkit` feature enables passes every per-crate check and fails only there.

# Multiplexer backends

Local contract for `crates/rimz/src/mux/`, the Zellij/tmux seam. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). [multiplexers.md](../../../../docs/internals/multiplexers.md) owns backend behaviour and the module map; read the three rules under [The division of labour](../../../../docs/internals/multiplexers.md#the-division-of-labour) before any code here. [tmux/AGENTS.md](./tmux/AGENTS.md) extends this one for tmux work, `tmux.rs` included.

## Commands

- **Invariant.** Every tmux command is built through [`tmux::managed_cmd`](./tmux.rs) or the backend's own `cmd`; `cargo xtask invariants` rejects a bare `tmux` argv. Fake `tmux` shims in tests step past the builders' three-word `-S <socket> -u` prefix by position, so a change to it moves every shim in the same commit.
- **Invariant.** Every Zellij action is built by `ZellijBackend::zellij_action`, which names its session; `cargo xtask invariants` rejects the action argv word on a line without the session flag.
- A `zellij` or `tmux` spec is built only in `tmux::tmux_cmd` and `ZellijBackend::cmd`. A command that can birth a server follows [the identity pin](../../../../docs/internals/multiplexers.md#the-identity-pin), and every control command follows [command discipline](../../../../docs/internals/multiplexers.md#command-discipline).
- A runtime record whose lock other writers acquire with a short bound holds no mux subprocess or other I/O of unbounded latency: read first, then lock, reload, and apply.

## Traps

- A pane missing from one listing is not proof it closed: even an authoritative listing may omit whole live tabs (`PaneListing::authoritative`). A decision from absence needs a second signal, such as the owner process's liveness or a tab the listing names.
- A change that closes or kills panes in a view the sidebar docks into leaves a working sibling standing, replaces the view, or closes the whole view (`MuxBackend::close_tab_of_pane`): a sidebar that has seen a sibling and then reads none [closes itself](../../../../docs/internals/sidebar/sidebar.md#self-close), taking a tmux window with it.

## Boundaries

- `mux` never imports `sidebar`. The wire every renderer listens on is the leaf [`wakeup/`](../wakeup/mod.rs) module below `store`.
- The path, schema, and freshness rules of every runtime file the multiplexer writes or dispatches on stay here, even where the writer sits elsewhere: another module publishes through the owning file's helpers and never spells the path.
- A bound lives with the module whose behaviour it defines; `sidebar::timing` holds only the sidebar's own cadences.

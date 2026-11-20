# Multiplexer backends

Local contract for `crates/rimz/src/mux/`, the Zellij/tmux seam. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). [multiplexers.md](../../../../docs/internals/multiplexers.md) owns backend behaviour and the module map; read the three rules under [The division of labour](../../../../docs/internals/multiplexers.md#the-division-of-labour) before any code here.

## Commands

- **Invariant.** Every tmux command is built through [`tmux::managed_cmd`](./tmux.rs) or the backend's own `cmd`; `cargo xtask invariants` rejects a bare `tmux` argv. Fake `tmux` shims in tests step past the builders' three-word `-S <socket> -u` prefix by position, so a change to it moves every shim in the same commit.
- **Invariant.** Every Zellij action is built by `ZellijBackend::zellij_action`, which names its session; `cargo xtask invariants` rejects the action argv word on a line without the session flag.
- A `zellij` or `tmux` spec is built only in `tmux::tmux_cmd` and `ZellijBackend::cmd`, which hand the child the user's `TMPDIR` back. Mux code reads the temp-root key list and never names a provider key.
- A managed tmux client runs from `/` with `$TMUX` cleared, so a pane-creating command passes `-c`.
- A command that can birth a server, on either backend, strips ambient mux identity (`CommandSpec::without_mux_context`) and the remote client channel (`REMOTE_CLIENT_ENV`). The launching process and the Zellij attach client keep that channel, which predecessor reaping needs.
- Every control command runs through [`CommandSpec`](./command.rs) under a deadline, and its caller degrades on `MuxErr::Timeout`, never blocks ([command discipline](../../../../docs/internals/multiplexers.md#command-discipline)).
- A runtime record whose lock other writers acquire with a short bound holds no mux subprocess or other I/O of unbounded latency: read first, then lock, reload, and apply.

## Traps

- A pane missing from one listing is not proof it closed: even an authoritative listing may omit whole live tabs (`PaneListing::authoritative`). A decision from absence needs a second signal, such as the owner process's liveness or a tab the listing names.
- Once the sidebar has seen a sibling, it exits on its first zero-sibling read and takes a tmux window with it ([`SelfCloseState::should_close`](../sidebar_pane/app/lifecycle.rs)). A change that closes or kills panes in a view the sidebar docks into leaves a working sibling standing or replaces the view.
- `plan_reconcile` returns only verdicts. tmux converges surviving sidebar widths outside the plan, and Zellij's `add_sidebar_to_tab` docks with `move-pane` and `stack-panes`, which swap or stack that tab's work panes. Before a plan or doc promises that a reconcile scope moves or resizes no existing pane, list every mux command each backend's `reconcile_sidebars` and its add path issue on panes the change did not create.
- tmux and Zellij forward outer-terminal focus reports to a pane that enabled `?1004`, with no pane change. A rule that reads a pane's terminal input (tty atime, bytes read) as human typing is probed, on both backends, against focus-out/in and against a popup or floating pane opened over the pane.
- The tmux client view (`parse_client_view` in [`tmux/parse.rs`](./tmux/parse.rs)) is the routing sample, not the list of clients that receive pane output: it leaves every `ignore-size` client out of `ClientView::clients` and the presence figures. The read-only share draws output and is listed only in `ClientPresence::unrouted_clients`; the room's control client draws nothing and is listed nowhere. A rule about who receives what a pane writes reads both sets.

## tmux

- A command nested inside another (`if-shell`, `run-shell -C`) resolves an omitted `-t` against the session's current window, not the pane the outer command targets. Give every nested window or pane command its own `-t`, and test it against a window that is not current.
- A `#{option}` format resolves pane options first, from the window's active pane, even in a `list-windows` row. Read an option tmux scopes to both windows and panes (`allow-passthrough`, `pane-border-format`) with `show-options -w -t @N`, and test a comparison against an active pane that carries a pane-local value.
- An option the user can also set in a RimZ session (session `status`, `status-format`, a window style) is restored to the user's value only behind a marker recording that RimZ wrote it: an unconditional unset in the default mode clears the user's own setting on every attach and reload. A mode the user chose in RimZ's config writes or unsets unconditionally. The live test sets a user value before each mode's first apply and asserts what survives.
- `resize-pane -x`/`-y` takes space from the nearest ancestor container of that direction, which can hold panes RimZ did not create. Resize only a pane whose container RimZ built, and test the resize with a foreign pane beside or above it.
- Stack hooks leave states that no RimZ command writes ([collapsed stacks](../../../../docs/internals/multiplexers.md#collapsed-stacks)): closing the open member while focus is elsewhere leaves no `@rimz_stack_open`, and the restore resizes every flagged member, a lone survivor included, while any marked pane in the window is collapsed. A change to stack placement tests a split from each of these states, and with a member exiting during the split.
- tmux 3.7 and later reject a client invocation of more than 1000 argv words, and every version rejects one packed command over 16 KiB (`command too long`). A command list that grows with windows or panes goes through a `source-file -` script on stdin.

## Boundaries

- `mux` never imports `sidebar`. The wire every renderer listens on is the leaf [`wakeup/`](../wakeup/mod.rs) module below `store`; every sender reaches down to it, and none reaches another module through it.
- The path, schema, and freshness rules of every runtime file the multiplexer writes or dispatches on stay here, even where the writer sits elsewhere: another module publishes through the owning file's helpers and never spells the path.
- A bound lives with the module whose behaviour it defines: command deadlines beside their commands, the presence-stamp freshness both backends share in [`mod.rs`](./mod.rs), and only the sidebar's own cadences in `sidebar::timing`.
- A caller that needs both backends' live sessions borrows `selection::LiveSessions`, the one batch snapshot, and does not list them again.

## Tests

Planner, width, and selection logic stay unit-level, with no mux required. Live-backend behaviour belongs in [`tests/integration/backend/`](../../tests/integration/backend/), which self-skips when the mux binary is absent.

A change to the tmux commands RimZ issues also runs the `backend::tmux::` live tests three times: under the local tmux, under CI's `TMUX_VERSION` from `ci/Dockerfile`, and under the 3.5 floor, each first on `PATH`. Layout behaviour differs across versions, and CI builds only its own.

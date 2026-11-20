# tmux backend

Local contract for `crates/rimz/src/mux/tmux/` and the `tmux.rs` beside it. Extends [mux/AGENTS.md](../AGENTS.md). [tmux backend](../../../../../docs/internals/multiplexers.md#tmux-backend) owns the backend's behaviour.

## Commands

- A pane-creating command passes `-c`, since [every managed client runs from `/`](../../../../../docs/internals/multiplexers.md#every-managed-client-runs-from-).
- A command nested inside another (`if-shell`, `run-shell -C`) resolves an omitted `-t` against the session's current window, not the pane the outer command targets. Give every nested window or pane command its own `-t`, and test it against a window that is not current.
- A `#{option}` format resolves pane options first, from the window's active pane, even in a `list-windows` row. Read an option tmux scopes to both windows and panes (`allow-passthrough`, `pane-border-format`) with `show-options -w -t @N`, and test a comparison against an active pane that carries a pane-local value.
- An option the user can also set in a RimZ session (session `status`, `status-format`, a window style) is restored to the user's value only behind a marker recording that RimZ wrote it: an unconditional unset in the default mode clears the user's own setting on every attach and reload. A mode the user chose in RimZ's config writes or unsets unconditionally. The live test sets a user value before each mode's first apply and asserts what survives.
- `resize-pane -x`/`-y` takes space from the nearest ancestor container of that direction, which can hold panes RimZ did not create. Resize only a pane whose container RimZ built, and test the resize with a foreign pane beside or above it.
- A change to stack placement tests a split from each state the stack hooks leave and no RimZ command writes (a stack with no open member, a lone flagged survivor while a marked pane is collapsed: [collapsed stacks](../../../../../docs/internals/multiplexers.md#collapsed-stacks)), and with a member exiting during the split.
- tmux 3.7 and later reject a client invocation of more than 1000 argv words, and every version rejects one packed command over 16 KiB (`command too long`). A command list that grows with windows or panes goes through a `source-file -` script on stdin.

## Tests

A change to the tmux commands RimZ issues also runs the `backend::tmux::` live tests three times: under the local tmux, under CI's `TMUX_VERSION` from `ci/Dockerfile`, and under the 3.5 floor, each first on `PATH`. Layout behaviour differs across versions, and CI builds only its own.

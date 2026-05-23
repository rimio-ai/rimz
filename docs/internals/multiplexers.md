# Multiplexers

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Zellij and tmux carry different communities and Rimz refuses to pick one. Both backends are first-class — same ledger, same bridge, same CLI, same sidebar model, tested against the same matrix. The multiplexer owns terminal mechanics (panes, views, attach/detach, scrollback, layout, resurrect); Rimz owns workspace state.

## Backend selection

Selection order, first match wins:

1. `--mux <name>` CLI flag,
2. active environment (`ZELLIJ` / `ZELLIJ_PANE_ID`, then `TMUX` / `TMUX_PANE`),
3. project config `[workspace] mux` (when not `auto`),
4. per-machine preference (`~/.config/rimz/preferences.toml`),
5. installed backend preference recorded by `rimz doctor`.

Both inputs are stable across worktrees: every worktree of one repo lands in the same session on the same backend.

## `MuxBackend`

All multiplexer-specific operations live behind one trait. Everything correctness-critical — the ledger, the per-request decision sockets, the resolver heartbeat, the wakeup socket, the feed/event schemas, the trust gate, the agent hooks — sits above this trait, identical across backends.

```text
ensure_session(name)
attach(name)
detach(name)
list_sessions()
list_panes(session)
new_view(session, opts)         Zellij tab / tmux window
split_pane(args)
focus_pane(pane_id)
rename_pane(pane_id, title)
capture_pane(pane_id, opts)     normalized output
send_keys(pane_id, text)
open_sidebar(session, launch)
wake_sidebar(session, bytes)
install_workspace_env(pane, env)
version()
```

Backend-specific fast paths cannot become correctness requirements. If a feature exists only on Zellij, the tmux backend must still pass the same matrix without it.

## Pane IDs

- Raw Zellij IDs look like `terminal_3` or `plugin_1`.
- Raw tmux IDs look like `%3`.
- Rimz-normalized IDs are `zellij:<raw>` and `tmux:<raw>`.

Normalized IDs travel through env vars (`RIMZ_PANE_ID`), feed items, snapshots, and CLI arguments. Raw IDs stay inside the backend adapter, where the multiplexer's native command expects them.

## Zellij backend

The sidebar runs as a WASM plugin pinned to the workspace session. Rimz requests only the permissions it needs (application state, focus actions, helper commands, opening panes/plugins, CLI pipe events). Denied permissions degrade the sidebar but do not break feed resolution, because resolution goes through CLI and ledger paths.

The Zellij-only fast path is `zellij pipe`: a broadcast pipe reaches every already-running plugin in milliseconds, eliminating the per-instance socket round-trip. Broadcast pipe (no `--plugin file:`) is what makes lazy-load impossible during normal hook delivery; targeted `--plugin file:` is reserved for `rimz setup` and `rimz doctor` self-tests where launching the plugin is the intent. The pipe is layered on top of the sidebar wakeup socket — it never replaces it.

### Zellij backend caveats

- **Pane IDs are positional, not stable.** Zellij does not expose a stable per-pane CLI handle; the spike returns `terminal_<id>` derived from the JSON `id` field of `zellij action list-panes -j -a` and filters plugins out. The `id` is unique within a session at a point in time but may be reused as panes close and reopen — feed items therefore carry `pane_process_start` so reconciliation can refuse a stale match.
- **Minimum version is 0.41.0.** Earlier Zellij builds lack the broadcast-pipe semantics Rimz relies on. `rimz doctor` reports the floor compliance; the constant lives in `crates/rimz/src/mux/zellij.rs::MIN_ZELLIJ_VERSION`.
- **Sidebar plugin lookup is XDG-based.** Rimz expects the WASM plugin at `${XDG_DATA_HOME:-$HOME/.local/share}/rimz/sidebar.wasm`. `open_sidebar` returns `MuxErr::NotInstalled` if the file is absent; doctor warns. The plugin crate lands in M1.
- **`wake_sidebar` is best-effort.** The UDP sidebar socket is the channel of record; the `zellij pipe` broadcast is a latency hint. Failures from the pipe call are logged at `debug` and do not error the ledger write that triggered the wakeup walk.

## tmux backend

The sidebar runs as a managed pane:

```text
tmux split-window -h -l 30 -b -t <session> 'rimz sidebar serve --mux tmux --session-name <session>'
```

Rimz stores the managed sidebar pane ID so reattach can target or recreate it. Optional status-line and popup integrations are opt-in and trust-gated because they execute shell snippets.

tmux pane creation passes `RIMZ_*` variables explicitly with `-e`. Layouts compile to tmux command sequences from the same layout IR Zellij uses.

### tmux backend caveats

- **`wake_sidebar` is a no-op.** tmux has no pipe-broadcast equivalent of `zellij pipe --name`; the sidebar wakeup socket is the only channel. The wakeup walk in `crates/rimz/src/ledger/wakeup.rs` still fans out UDP datagrams for tmux heartbeats — that path is identical across backends — but skips the per-session dedupe broadcast that Zellij benefits from. Latency parity therefore depends on the wakeup socket alone.
- **Minimum version is 3.2.0.** `split-window -e KEY=VAL` (needed for `RIMZ_*` env injection on the managed sidebar pane) and `display-popup` (used by the optional popup integration that M1 will trust-gate) both landed in tmux 3.2. `rimz doctor` reports the floor compliance; the constant lives in `crates/rimz/src/mux/tmux.rs::MIN_TMUX_VERSION`.
- **Server-less `list_sessions` is empty, not an error.** tmux exits 1 with `no server running` when the daemon hasn't been started yet. The backend swallows that specific stderr shape and returns an empty `Vec`, matching the Zellij contract (`zellij list-sessions` exits 0 with no output in the same state).
- **`open_sidebar` reports split creation.** The tmux command returns once the managed pane is created; the inner `rimz sidebar serve` process owns rendering, heartbeat, and wakeup handling inside that pane.
- **Per-test server isolation in CI.** Integration tests construct the backend via `TmuxBackend::with_socket(<tempdir>/tmux.sock)` to keep each test's `tmux` server off the user's default socket. Production code uses the unit-default constructor and inherits the system socket.

## Common contract

What both backends must deliver:

- **Detach and reattach are multiplexer features** — Rimz does not reimplement them.
- **Runtime correctness does not require a visible sidebar** — hooks, the bridge, and `rimz feed ask` work headless.
- **The ledger survives host restart; processes do not**, unless a host supervisor is wired (tmux-resurrect, Zellij resurrect, systemd unit).
- **`rimz doctor` reports** selected backend, versions, feature availability, sidebar liveness, socket-path headroom (the 108-byte `AF_UNIX` limit bites quickly), and any degraded modes.

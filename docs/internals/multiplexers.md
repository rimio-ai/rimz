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
name()
ensure_session(session_name, cwd)
attach_command(name) -> CommandSpec
detach(name)
list_sessions()
list_panes(session)
split_pane(args)
focus_pane(pane_id)
capture_pane(pane_id, opts)     normalized output
send_keys(pane_id, text)
open_sidebar(session_name, workspace_id, cwd, rimz_bin, width_percent)
wake_sidebar(session, bytes)
version()
```

Backend-specific fast paths cannot become correctness requirements. If a feature exists only on Zellij, the tmux backend must still pass the same matrix without it.

## Pane IDs

- Raw Zellij IDs look like `terminal_3` or `plugin_1`.
- Raw tmux IDs look like `%3`.
- Rimz-normalized IDs are `zellij:<raw>` and `tmux:<raw>`.

Normalized IDs travel through env vars (`RIMZ_PANE_ID`), feed items, snapshots, and CLI arguments. Raw IDs stay inside the backend adapter, where the multiplexer's native command expects them.

## Zellij backend

`ensure_session` is a no-op: Zellij creates sessions lazily, and the sidebar launch owns first birth. `open_sidebar` creates the session once, from a layout, and never touches it again:

```text
zellij attach --create-background <session> options --default-cwd <cwd> --default-layout <layout>
```

The layout is a vertical split — a left `rimz-sidebar` pane at the configured width percentage and a focused terminal on the right — and doubles as the `default_tab_template`, so every newly created tab is born the same way. The sidebar pane is `close_on_exit`, so it disappears when its own process exits (the self-close loop in [sidebar.md](./sidebar.md)). If the session already exists, `open_sidebar` is a no-op: a Zellij layout applies only at session birth, and the sidebar survives detach/reattach server-side, so there is no re-injection path. The sidebar heartbeat socket is the wakeup channel of record. `zellij pipe --name rimz::feed` remains a best-effort broadcast optimization layered on top of socket wakeups; it never creates the sidebar and never gates correctness.

A layout is the only way to place a left, sized pane at creation: `zellij run` splits only `right`/`down` and ignores `--width` for tiled panes, so the CLI cannot reproduce a left 30% pane after the fact. Touching the layout exactly once — never resizing, moving, or re-injecting — is therefore both simpler and the only reliable shape.

### Zellij backend caveats

- **Pane IDs are positional, not stable.** Zellij does not expose a stable per-pane CLI handle; the backend returns `terminal_<id>` derived from the JSON `id` field of `zellij action list-panes -j -a` and filters plugins out. The `id` is unique within a session at a point in time but may be reused as panes close and reopen — feed items therefore carry `pane_process_start` so reconciliation can refuse a stale match.
- **Minimum version is 0.41.0.** Earlier Zellij builds lack the broadcast-pipe semantics Rimz relies on. `rimz doctor` reports the floor compliance; the constant lives in `crates/rimz/src/mux/zellij.rs::MIN_ZELLIJ_VERSION`.
- **The layout file outlives the create call.** Zellij parses `--default-layout` asynchronously, after `attach --create-background` returns, so Rimz keeps the temp layout file on disk until the sidebar + terminal panes materialize, then deletes it.
- **New tabs focus the sidebar, not the terminal.** Only the explicit first `tab` can focus a template child; a tab opened from the `default_tab_template` lands focus on the sidebar pane. The user presses a focus key to move on; Rimz adds no post-creation focus command, which would reintroduce "touching after creation".
- **`wake_sidebar` is best-effort.** The UDP sidebar socket is the channel of record; the `zellij pipe` broadcast is a latency hint. Failures from the pipe call are logged at `debug` and do not error the ledger write that triggered the wakeup walk.

## tmux backend

The sidebar runs as a managed pane:

```text
tmux split-window -d -h -l <width>% -b -t <session> \
  <rimz-bin> sidebar serve --mux tmux --workspace-id <id> --session-name <session>
```

The pane is best-effort. A fresh sidebar heartbeat suppresses relaunch; a missing, stale, unreadable, or protocol-mismatched heartbeat lets `rimz start` / `rimz attach` open a new pane. Optional status-line and popup integrations are opt-in and trust-gated because they execute shell snippets.

The spawned `rimz sidebar serve` wrapper passes its own binary path to the renderer with `RIMZ_BIN`. Layouts compile to tmux command sequences from the same layout IR Zellij uses.

### tmux backend caveats

- **`wake_sidebar` is a no-op.** tmux has no pipe-broadcast equivalent of `zellij pipe --name`; the sidebar wakeup socket is the only channel. The wakeup walk in `crates/rimz/src/ledger/wakeup.rs` still fans out UDP datagrams for tmux heartbeats — that path is identical across backends — but skips the per-session dedupe broadcast that Zellij benefits from. Latency parity therefore depends on the wakeup socket alone.
- **Minimum version is 3.2.0.** `split-window -e KEY=VAL` (needed for `RIMZ_*` env injection on the managed sidebar pane) and `display-popup` (used by the optional popup integration that M1 will trust-gate) both landed in tmux 3.2. `rimz doctor` reports the floor compliance; the constant lives in `crates/rimz/src/mux/tmux.rs::MIN_TMUX_VERSION`.
- **Server-less `list_sessions` is empty, not an error.** tmux exits 1 with `no server running` when the daemon hasn't been started yet. The backend swallows that specific stderr shape and returns an empty `Vec`, matching the Zellij contract (`zellij list-sessions` exits 0 with no output in the same state).
- **`open_sidebar` reports split creation.** The tmux command returns once the managed pane is created; the inner `rimz sidebar serve` process owns rendering, heartbeat, and wakeup handling inside that pane. The sidebar self-closes the same way it does on Zellij — through the normalized `rimz pane list` — so a lone sidebar removes itself when its window's last working pane exits.
- **New windows don't get a sidebar yet.** The initial window gets one (the `-b` split places it left at creation); auto-adding a sidebar to windows opened later needs a tmux hook and is a follow-up.
- **Per-test server isolation in CI.** Integration tests construct the backend via `TmuxBackend::with_socket(<tempdir>/tmux.sock)` to keep each test's `tmux` server off the user's default socket. Production code uses the unit-default constructor and inherits the system socket.

## Common contract

What both backends must deliver:

- **Detach and reattach are multiplexer features** — Rimz does not reimplement them.
- **Runtime correctness does not require a visible sidebar** — hooks, the bridge, and `rimz feed ask` work headless.
- **The ledger survives host restart; processes do not**, unless a host supervisor is wired (tmux-resurrect, Zellij resurrect, systemd unit).
- **`rimz doctor` reports** selected backend, versions, feature availability, sidebar liveness, socket-path headroom (the 108-byte `AF_UNIX` limit bites quickly), and any degraded modes.

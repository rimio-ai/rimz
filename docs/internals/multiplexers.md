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
attach_command(name, config) -> CommandSpec
detach(name)
list_sessions()
list_panes(session)               id, view, foreground command, cwd
client_focused_panes(session)     the pane each attached client focuses
split_pane(args)
focus_pane(pane_id)
capture_pane(pane_id, opts)     normalized output
send_keys(pane_id, text)
open_sidebar(session_name, workspace_id, cwd, rimz_bin, width)
wake_sidebar(session, bytes)
version()
```

Backend-specific fast paths cannot become correctness requirements. If a feature exists only on Zellij, the tmux backend must still pass the same matrix without it.

### Pane metadata

`list_panes` reports each pane's **foreground command** and **cwd** alongside its id and view. The sidebar uses these for presence: a pane is a row, its command labels it, and its cwd groups it by worktree (see [sidebar.md → Presence model](./sidebar.md#presence-model)). Both fields are cross-backend:

| field        | tmux                       | Zellij (`list-panes -j`) |
|--------------|----------------------------|--------------------------|
| command      | `#{pane_current_command}`  | `pane_command`           |
| cwd          | `#{pane_current_path}`     | `pane_cwd`               |

`PaneRef.pane_process_start` stays the reused-id reconciliation key. Neither backend's pane PID feeds the sidebar: tmux's `#{pane_pid}` is the pane's *shell*, not the agent it launched, and Zellij exposes no pane PID at all. Agent liveness instead uses the agent's own pid, captured best-effort by its hook ([agent.md](./agent.md)) — so the parity floor for presence is command + cwd, which both backends meet.

### Two kinds of focus

`list_panes` reports `PaneRef.is_focused`: the **per-view active pane**. Both backends mark one such pane per view (Zellij's `list-panes` `is_focused`, tmux's `#{pane_active}`), so a session with N views reports N "focused" panes — the active pane *within* each tab/window, regardless of where the user is looking.

The pane a user actually looks at is **per-client**, and `client_focused_panes` reports it — one pane per attached client, deduped:

| backend | command |
|---------|---------|
| Zellij  | `zellij action list-clients` (column 2, `ZELLIJ_PANE_ID`, per client) |
| tmux    | `tmux list-clients -t <session> -F "#{pane_id}"` (per client) |

The producer overlays this set onto `PaneRef.client_focused` (see [sidebar.md](./sidebar.md)), so the sidebar focus mirror tracks the user, not every view's active pane. It is best-effort enrichment, never a precondition: a backend that cannot answer returns an empty set and the mirror holds. `is_focused` stays the per-view signal the row cap uses to keep each view's active row visible.

## Pane and view IDs

- Raw Zellij IDs look like `terminal_3` or `plugin_1`.
- Raw tmux IDs look like `%3`.
- Rimz-normalized IDs are `zellij:<raw>` and `tmux:<raw>`.

Normalized IDs travel through env vars (`RIMZ_PANE_ID`), feed items, snapshots, and CLI arguments. Raw IDs stay inside the backend adapter, where the multiplexer's native command expects them.

`PaneRef.view_id` names the view (tab/window) holding the pane by the backend's **internal** id — Zellij `tab_15`, tmux `@3`. It is an opaque grouping key that joins panes into views for sidebar-per-view bookkeeping.

**A view id is never the view's on-screen label.** Zellij's default tab names are themselves number-shaped (`Tab #16`), so an internal `tab_15` invites a lexical match against a tab *named* "Tab #15" — a join across two unrelated id spaces that lands on the wrong tab in any session that has ever closed one. The name is a sticky label minted at tab creation; the internal id is the backend's own counter; nothing reconciles them, and Zellij reports no per-pane tab name that could (`PaneRef.view_name` is tmux-only). Resolve "which tab holds this pane?" through the pane id — focus it, or read the live layout — never by matching `view_id` against a label.

## Zellij backend

`ensure_session` is a no-op: Zellij creates sessions lazily, and the sidebar launch owns first birth. `open_sidebar` branches on the session's liveness, reported by `zellij list-sessions`:

```text
zellij attach --create-background <session> options <room-options> --default-cwd <cwd> --default-layout <layout>
```

`<room-options>` comes from per-machine `[zellij]` config and is also present on `zellij attach --create <session> options …`: mouse click-through, focus-follows-mouse, pane frames, force-close behaviour, scrollback size, startup/release-note suppression, OSC52 clipboard target, copy-on-select, Kitty keyboard protocol, OSC8 hyperlinks, and session serialization. `mouse_mode = true` rides Zellij's default enabled state; `mouse_mode = false` emits `--mouse-mode false`. `mouse-click-through` is version-gated because older Zellij builds reject the flag; omitting it degrades to focus-then-click rather than aborting launch. Rimz leaves `focus_follows_mouse` off by default because Zellij 0.44 only sends the first click through when mouse click-through is true and focus-follows-mouse is false.

**Serialization off.** Rimz passes `--session-serialization false` on every birth and attach, so a dead Zellij server leaves nothing to resurrect. Resurrection is worse than useless here: agents and scripts cannot restore their running state, and Zellij brings the room back with every command pane re-suspended at a `Waiting to run` prompt and a dead mouse. With serialization off, a crashed server's session simply vanishes and the next start births a clean, running room. Disabling it costs nothing legitimate — serialization only matters on server death; detach/reattach keeps the server alive in memory and never consults the cache. tmux has no resurrection, so the flag is Zellij-only.

The default birth layout is the `default_tab_template`: a vertical split — a left `rimz-sidebar` pane at the default width percentage and a focused terminal on the right — above a one-row `zellij:compact-bar` plugin pane. The `sidebar.max_cols` cap never enters the template — a Zellij layout spells a fixed size or a percentage, not `min(percent, columns)` — so a pane born above the cap is shrunk to it by the freshly-born renderer's one creation-time `resize_sidebar_pane` (the step loop in `resize_sidebar_toward`, which never finishes above the cap). Because supplying a `default_tab_template` replaces Zellij's built-in one, which is what carries the tab/status bar, the layout re-adds the compact bar itself or every tab is born bare. The template — not a separate `tab` node — defines every tab, so the first tab and any the user opens later are born identically. The sidebar pane is `close_on_exit`, so it disappears when its own process exits (the self-close loop in [sidebar.md](./sidebar.md)). A Zellij layout applies only at session birth, so the branch is:

- **Live** — the session already carries its sidebar and owns every resize and split the user has made since. When the pane listing is clean and the sidebar heartbeat is trusted, `open_sidebar` is a no-op and the sidebar survives detach/reattach server-side. When a stale heartbeat requires replacement, Rimz only rebuilds an inspected live room; an uninspectable live room is left untouched and handled by the reset path.
- **Exited** (Zellij's `EXITED - attach to resurrect`) — a plain attach would resurrect the *serialized* layout, with the last geometry and every command pane re-suspended at a `Waiting to run` prompt. Rimz prefers a clean rebirth: `zellij delete-session <session> --force`, then create from the layout. (Distinct from a host reboot, where the session is fully absent.) With serialization off this state stops being minted, but the branch stays as defence for sessions serialized before the flag landed.
- **Absent** — first birth: create from the layout.

**The pre-attach health gate.** `open_sidebar` is best-effort and can be skipped (a fresh sidebar heartbeat short-circuits it) or fail without rebirthing, so it cannot be the only thing standing between the user and a resurrecting `attach --create`. `ensure_clean_session` is the authoritative gate `rimz start` (and the attach flows) run immediately before building the attach command. It classifies the live room with a tightly-bounded `list-panes` probe and treats both a held sidebar and any *held command pane* (the resurrection fingerprint) as not-clean. A clean live room is left untouched; an absent, exited, or inspected suspended room is deleted and reborn, RUNNING. A live room whose pane listing times out or fails is uninspectable, so Rimz reports it as stuck and requires the reset confirmation path before any destructive teardown. tmux has no resurrection, so its gate is a no-op.

**Reset.** A room that cannot be auto-healed needs an explicit destructive reset. `rimz reset` (and the single-confirm auto-offer `rimz start` raises on a stuck room) runs one teardown — `delete-session --force`, purge the serialized-session cache under every `~/.cache/zellij/contract_version_*/session_info/`, reap stale sidebar runtime files, and sweep orphaned servers / leaked daemons scoped to this user and the exact (path-unique) session name — then rebirths. Without a terminal to confirm, `rimz start` fails fast with the fix (run `rimz reset`) rather than destroying a session unattended. `rimz doctor` reports the same health verdict the gate acts on.

**Daemon view leads at birth.** When `rimz start` has a daemon view (the `rimzd` tab — see [configuration.md](../reference/configuration.md)), `open_sidebar` receives it and births a *two-tab* layout instead: the daemon tab (`sidebar | hosts…`) first, then the focused working tab (`sidebar | terminal`). Zellij has no CLI to reorder tabs after birth — `move-tab` acts on a connected client's transient focus, which an ephemeral `zellij action` cannot hold — so leading position is fixed here, in the birth layout, never by a later move. The same layout also carries a `new_tab_template` (distinct from `default_tab_template`: it applies only to tabs the user opens *later*) with the same `sidebar | terminal` shape, so future tabs keep their sidebar and terminal focus without the `children` focus-strand bug below. `open_background_view` is then the idempotent confirm — a no-op when the daemon tab already leads — and only appends (a non-leading tab) on the rare late-add path where a host became available after first birth.

**Resumed agents seed at birth.** A reborn session re-seeds the prior agents it remembers ([resume-on-rebirth](./sidebar.md#resume-on-rebirth)): the cli hands `open_sidebar` / `ensure_clean_session` a `SidebarPaneOptions.resume_panes` list, and the birth layout spells one `sidebar | agent` tab per pane, each a command pane running the agent's resume CLI (`claude --resume <id>`, `codex resume <id>`) in its worktree cwd. Born in a fresh layout they start *running*, never `start_suspended` — the same reason serialization is off. Focus lands on the freshest agent's tab. A plain birth (no daemon, no resume set) keeps the single-`default_tab_template` shape unchanged; the multi-tab layout is used only when a daemon and/or resumed agents lead.

The sidebar heartbeat socket is the only wakeup the walk fires. `zellij pipe --name rimz::feed` is the dormant primitive the opt-in plugin rail will consume once built; until then the walk spawns no pipe, and the native pane wakes over the socket alone. The pipe never creates the sidebar and never gates correctness.

A layout is the only way to place a left, sized pane at creation: `zellij run` splits only `right`/`down` and ignores `--width` for tiled panes, so the CLI cannot reproduce a left 30% pane after the fact. Touching the layout exactly once — never resizing, moving, or re-injecting — is therefore both simpler and the only reliable shape.

**Mouse passthrough.** Birth and attach pass `options --mouse-click-through true` on Zellij ≥ 0.44.0 (the release that added the option), and default `focus_follows_mouse = false`, so a single click both focuses the sidebar pane and reaches the renderer — a jump lands on the first click rather than the second. The click-through flag is version-gated and best-effort: older Zellij does not know it, so Rimz omits it and degrades to Zellij's default focus-then-click. Attach carries it onto an already-running session, which picks it up on the next attach. The renderer's selection model treats the resulting `from-pane → sidebar → target` focus transition correctly — see [sidebar.md → selection](./sidebar.md#jump--the-row-is-the-link).

### Zellij sidebar plugin rail (optional)

Zellij users can opt in (`[layout.zellij]` in [configuration.md](../reference/configuration.md)) to a wasm plugin that presents the same snapshot view-model as a docked, persistent left rail. It is launched with an idempotent `launch-or-focus-plugin`, so any `rimz` / attach re-summons it, and it fetches `rimz sidebar snapshot --json` through the host `run_command` bridge on the `zellij pipe --name rimz::feed` wakeup — no sockets, no ledger writes. The rail is the only way to dock the sidebar left as true chrome; the native pane remains the default and the fallback, and the plugin never gates correctness. Renderer details live in [sidebar.md](./sidebar.md).

### Zellij backend caveats

- **Pane IDs are positional, not stable.** Zellij does not expose a stable per-pane CLI handle; the backend returns `terminal_<id>` derived from the JSON `id` field of `zellij action list-panes -j -a` and filters plugins out. The `id` is unique within a session at a point in time but may be reused as panes close and reopen — feed items therefore carry `pane_process_start` so reconciliation can refuse a stale match.
- **Minimum version is 0.41.0.** Earlier Zellij builds lack the broadcast-pipe semantics Rimz relies on. `rimz doctor` reports the floor compliance; the constant lives in `crates/rimz/src/mux/zellij.rs::MIN_ZELLIJ_VERSION`.
- **The layout file outlives the create call.** Zellij parses `--default-layout` asynchronously, after `attach --create-background` returns, so Rimz keeps the temp layout file on disk until the sidebar + terminal panes materialize, then deletes it.
- **Every tab is born with a focused terminal, not `children`.** The template spells out the right pane as an explicit `pane focus=true` rather than Zellij's `children` placeholder. Zellij auto-fills a default terminal into a *top-level* `children` but never into one nested inside a split, so a nested `children` would leave every template-born tab with the sidebar alone and focus stranded on it. Spelling out the terminal gives the initial tab and every tab opened later a right pane with focus — no post-creation focus command, so Rimz never "touches after creation".
- **`wake_sidebar` is dormant.** The UDP sidebar socket is the channel of record. The wakeup walk no longer calls `wake_sidebar`: the `zellij pipe` broadcast it issued had no consumer (the plugin rail is unbuilt), so spawning a `zellij` subprocess per write per session was pure cost and was removed. The trait method is retained as the primitive the rail will re-arm — gated on rail presence so it stays dormant until then.
- **Control commands are time-bounded.** Every `CommandSpec::run` (both backends) waits at most `COMMAND_TIMEOUT` (30s) for the child, then SIGKILLs it and returns `MuxErr::Timeout`. A healthy control command answers in milliseconds; the bound exists because a `zellij action` client busy-loops at 100% CPU when its session server dies, which would otherwise hang the caller — and `rimz start` — forever. Output is drained on threads so a full pipe never deadlocks the wait. Callers treat mux commands best-effort, so a timeout degrades rather than blocks.
- **Per-test server isolation in CI.** Integration tests construct the backend via `ZellijBackend::with_runtime_dir(<tempdir>)` to keep each test's `zellij` server off the user's default — Zellij locates its server socket under `XDG_RUNTIME_DIR`, so a private runtime dir is a private server. This is the parity counterpart of `TmuxBackend::with_socket`; production code uses the unit-default constructor and inherits the ambient `XDG_RUNTIME_DIR`. Every Zellij command (and the `RIMZ_ZELLIJ_BIN` binary override the wakeup walk may set to a test shim) flows through the single `ZellijBackend::cmd` chokepoint, so one field threads that isolation everywhere.

## tmux backend

The sidebar runs as a managed pane:

```text
tmux split-window -d -h -l <width> -b -t <session> \
  <rimz-bin> sidebar serve --mux tmux --workspace-id <id> --session-name <session>
tmux set-hook -t <session> after-new-window \
  "split-window -h -b -d -l <width> '<rimz-bin> sidebar serve ...'"
```

`<width>` is always the default percentage (`30%`) — a detached session births at default geometry (80×24 until a client attaches), where an absolute size would be wrong. The `sidebar.max_cols` cap lands afterwards as the freshly-born renderer's one creation-time `resize_sidebar_pane`, an exact `resize-pane -x min(percent, max_cols)` against the live window width.

`ensure_session` also applies per-machine `[tmux]` room options. Session and window options stay scoped to the Rimz session (`mouse`, `history-limit`, `renumber-windows`, `allow-passthrough`, `aggressive-resize`, and pane border shape). Server-scoped options (`focus-events`, `set-clipboard`, `extended-keys`, `extended-keys-format`, `escape-time`) are runtime-global inside the tmux server because tmux has no per-session equivalent for clipboard and rich-key handling.

`open_sidebar` does both: it splits the sidebar into the initial window and installs a session-scoped `after-new-window` hook that re-runs the same left split in every window opened later. tmux has no tab template, so the hook is how it matches Zellij's `default_tab_template` parity — every view the user opens is born with a left sidebar and a focused right terminal (`-b` keeps the sidebar left, `-d` keeps focus on the new window's terminal).

The pane is best-effort. A fresh sidebar heartbeat suppresses relaunch; a missing, stale, unreadable, or protocol-mismatched heartbeat lets `rimz start` / `rimz attach` open a new pane. Optional status-line and popup integrations are opt-in and trust-gated because they execute shell snippets.

The spawned `rimz sidebar serve` wrapper passes its own binary path to the renderer with `RIMZ_BIN`. Layouts compile to tmux command sequences from the same layout IR Zellij uses.

tmux has no plugin surface to dock into, so the native pane is its only renderer. The docked-rail upgrade is Zellij-specific; tmux reaches the same sidebar surface through this managed pane.

### tmux backend caveats

- **`wake_sidebar` is a no-op.** tmux has no pipe-broadcast equivalent of `zellij pipe --name`; the sidebar wakeup socket is the only channel. The wakeup walk in `crates/rimz/src/ledger/wakeup.rs` fans out one UDP datagram per fresh heartbeat — that path is identical across both backends now that the dormant Zellij pipe no longer fires. Latency parity rests on the wakeup socket alone.
- **Minimum version is 3.2.0.** `split-window -e KEY=VAL` (needed for `RIMZ_*` env injection on the managed sidebar pane) and `display-popup` (used by the optional popup integration that M1 will trust-gate) both landed in tmux 3.2. `rimz doctor` reports the floor compliance; the constant lives in `crates/rimz/src/mux/tmux.rs::MIN_TMUX_VERSION`.
- **Server-less `list_sessions` is empty, not an error.** tmux exits 1 with `no server running` when the daemon hasn't been started yet. The backend swallows that specific stderr shape and returns an empty `Vec`, matching the Zellij contract (`zellij list-sessions` exits 0 with no output in the same state).
- **`open_sidebar` reports split creation.** The tmux command returns once the managed pane is created; the inner `rimz sidebar serve` process owns rendering, heartbeat, and wakeup handling inside that pane. The sidebar self-closes the same way it does on Zellij — through the normalized `rimz pane list` — so a lone sidebar removes itself when its window's last working pane exits.
- **Every window is born with a sidebar.** Alongside the initial split, `open_sidebar` installs an `after-new-window` hook so windows opened later get the same left sidebar + focused terminal. The hook runs `split-window` (not `new-window`), so it never recurses, and the sidebar self-close keeps a lone sidebar from outliving its window.
- **Resumed agents open as windows.** With the hook installed, `open_sidebar` re-seeds the reborn session's prior agents ([resume-on-rebirth](./sidebar.md#resume-on-rebirth)): one `new-window` per `resume_panes` entry, each born `sidebar | agent` as the hook docks the sidebar, focus landing on the freshest. Idempotent on the window name, so a re-run never doubles an agent window — tmux's parity with Zellij seeding the agent tabs into the birth layout.
- **Per-test server isolation in CI.** Integration tests construct the backend via `TmuxBackend::with_socket(<tempdir>/tmux.sock)` to keep each test's `tmux` server off the user's default socket. Production code uses the unit-default constructor and inherits the system socket.

## Common contract

What both backends must deliver:

- **Detach and reattach are multiplexer features** — Rimz does not reimplement them.
- **Runtime correctness does not require a visible sidebar** — hooks, the bridge, and `rimz feed ask` work headless.
- **The renderer is interchangeable and optional** — the native pane is the default on both backends; the Zellij plugin rail is an opt-in upgrade. Correctness never depends on which renderer (or none) is attached.
- **The ledger survives host restart; processes do not**, unless a host supervisor is wired (tmux-resurrect, Zellij resurrect, systemd unit).
- **`rimz doctor` reports** selected backend, versions, feature availability, sidebar liveness, socket-path headroom (the 108-byte `AF_UNIX` limit bites quickly), and any degraded modes.

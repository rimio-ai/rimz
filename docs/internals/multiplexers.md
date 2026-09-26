# Multiplexers

RimZ does not implement a terminal. Zellij and tmux already provide persistent sessions, pseudo-terminals, and pane geometry, so RimZ drives one of them and keeps its own job to durable workspace state, the agent model, and the sidebar. `crates/rimz/src/mux/` is that seam. Beside it sits one wasm plugin, `crates/rimz-presence-zellij/`, because a plugin is the only way Zellij pushes events to a host process.

Both backends are first class: same store, same CLI, same sidebar model, one test matrix. A feature that lands here lands twice.

This page is the RimZ side of the seam. The upstream surfaces (every Zellij option, every tmux format variable) are catalogued in [zellij-reference.md](../externals/mux-adapter/zellij-reference.md) and [tmux-reference.md](../externals/mux-adapter/tmux-reference.md). What a user configures is in [configuration → multiplexer room options](../guide/configuration.md#multiplexer-room-options) and the [Zellij and tmux guide](../guide/multiplexer.md).

## The division of labour

| The multiplexer owns | RimZ owns |
| --- | --- |
| Panes, tabs and windows, geometry | The durable store and its event log |
| The pseudo-terminal behind every pane | The agent model, messages, and runs |
| Attach, detach, scrollback, copy mode | Run and wakeup sockets, the trust gate, agent hooks |
| Session resurrection, where it exists | Rebirth: which agents come back and how |

Three rules follow from that split. Read them before any code in this module.

**Parity is the rule, and a fast path is a latency hint.** A backend-only capability always sits over shared truth. The Zellij presence plugin and the tmux control-mode watch both push topology, and both are optional: with either channel dead, its backend polls and passes the same test matrix. Correctness never reads from a push channel.

**Cross-backend policy stays pure and above the backends.** [`reconcile.rs`](../../crates/rimz/src/mux/reconcile.rs) owns the one-sidebar-per-view planner and its execution accounting, [`width.rs`](../../crates/rimz/src/mux/width.rs) owns sizing arithmetic, and [`sidebar/presence/projector.rs`](../../crates/rimz/src/sidebar/presence/projector.rs) turns normalized presence transitions into typed events over the vocabulary in [`wakeup/events.rs`](../../crates/rimz/src/wakeup/events.rs). These modules unit-test with no multiplexer installed. Each backend collects native facts and executes native effects. Geometry convergence stays in the adapters, because Zellij repairs geometry before structural execution and tmux only after structural success.

**Backends stay ignorant of agents.** The CLI hands `open_tab` backend-neutral pane argv, layout geometry, and an environment map applied to every command pane. Agent resolution, prompts, and worktree cleanup are already compiled into that argv (`rimz agents exec …`), so no backend knows what an agent kind or a worktree is. The layout IR is in [fleet.md](./harness/fleet.md#the-layout-ir); worktree cleanup is in [worktrees.md](./harness/worktrees.md#who-triggers-removal).

## Module map

Shared seam, `crates/rimz/src/mux/`:

| Path | Owns |
| --- | --- |
| [`mod.rs`](../../crates/rimz/src/mux/mod.rs) | The `MuxBackend` trait, its option and result types, `MuxErr`, and the one env-to-`PaneId` mapping. |
| [`selection.rs`](../../crates/rimz/src/mux/selection.rs) | Backend selection precedence. |
| [`command.rs`](../../crates/rimz/src/mux/command.rs) | `CommandSpec`: the bounded subprocess engine every control command runs through. |
| [`pane_writer.rs`](../../crates/rimz/src/mux/pane_writer.rs) | `PaneWriter`: the per-pane advisory write lock at `RuntimePaths::pane_write_lock` ([messaging.md → writing to the pane](./harness/messaging.md#writing-to-the-pane)). |
| [`reconcile.rs`](../../crates/rimz/src/mux/reconcile.rs) | The structural sidebar repair planner, pane-role precedence, and transaction executor. |
| [`mount_proof.rs`](../../crates/rimz/src/mux/mount_proof.rs) | Current-build heartbeat proof for panes mounted during repair. |
| [`width.rs`](../../crates/rimz/src/mux/width.rs) | Sidebar sizing: share resolution, native steps, and target spellings. |
| [`width_target.rs`](../../crates/rimz/src/mux/width_target.rs) | The room-runtime width record every renderer resolves against: its file, its pin flag, and the change broadcast. |
| [`companion_layout.rs`](../../crates/rimz/src/mux/companion_layout.rs) | Pure bounded column planning and equal-width, equal-height targets for [subagent companion tabs](./harness/subagents.md#pane-zones). |
| [`tab_name.rs`](../../crates/rimz/src/mux/tab_name.rs) | Pure pane-derived tab labels, name ownership, and `TabNameIntent`. |
| [`focus_anchor.rs`](../../crates/rimz/src/mux/focus_anchor.rs) | The durable two-phase intent behind every RimZ-initiated focus action ([state.md → focus intent](./sidebar/state.md#focus-intent)). |
| [`focus_key.rs`](../../crates/rimz/src/mux/focus_key.rs) | Parsing and rendering the `[sidebar] focus_key` and `zoom_key` chords. |
| [`recovery.rs`](../../crates/rimz/src/mux/recovery.rs) | The guarded process sweep behind room teardown. |
| [`domain.rs`](../../crates/rimz/src/mux/domain.rs) | `ProcessDomain`: the guard every heuristic process kill passes. |
| [`capabilities.rs`](../../crates/rimz/src/mux/capabilities.rs) | Static backend facts, such as whether a view is a tab or a window. |
| [`binaries.rs`](../../crates/rimz/src/mux/binaries.rs) | PATH and live-server binary probes for `rimz doctor`. |

Zellij, [`zellij.rs`](../../crates/rimz/src/mux/zellij.rs) plus `crates/rimz/src/mux/zellij/`:

| Path | Owns |
| --- | --- |
| [`backend.rs`](../../crates/rimz/src/mux/zellij/backend.rs) | The `MuxBackend` implementation. |
| [`layout.rs`](../../crates/rimz/src/mux/zellij/layout.rs) | KDL layout rendering for birth, the daemon view, resumed agents, and background tabs. Pure `&options → String`. |
| [`sidebar.rs`](../../crates/rimz/src/mux/zellij/sidebar.rs) | Sidebar birth, in-place recovery, and geometry convergence. |
| [`presence.rs`](../../crates/rimz/src/mux/zellij/presence.rs) | Plugin materialization, identity, load, retire and upgrade pipes, and the reload upgrade outcome. |
| [`pane_topology.rs`](../../crates/rimz/src/mux/zellij/pane_topology.rs) | The topology cache the plugin publishes: schema, path, freshness window, and the desired-presence record beside it. |
| [`raw_pane.rs`](../../crates/rimz/src/mux/zellij/raw_pane.rs) | Topology projection and sidebar classification. |
| [`session.rs`](../../crates/rimz/src/mux/zellij/session.rs) | Session discovery, topology-cache reads, the fresh-topology wait reload shares, and serialized-session cache discovery and purge. |
| [`socket.rs`](../../crates/rimz/src/mux/zellij/socket.rs) | IPC socket path budgeting, which is tight on macOS. |
| [`reap.rs`](../../crates/rimz/src/mux/zellij/reap.rs) | Pre-attach retirement of orphaned clients from one remote lineage. |
| [`parse.rs`](../../crates/rimz/src/mux/zellij/parse.rs), [`pane_pid.rs`](../../crates/rimz/src/mux/zellij/pane_pid.rs) | Command-output parsing helpers. |

tmux, [`tmux.rs`](../../crates/rimz/src/mux/tmux.rs) plus `crates/rimz/src/mux/tmux/`:

| Path | Owns |
| --- | --- |
| [`backend.rs`](../../crates/rimz/src/mux/tmux/backend.rs) | The `MuxBackend` implementation. |
| [`window.rs`](../../crates/rimz/src/mux/tmux/window.rs) | Window, pane, and tab-layout command helpers. |
| [`options.rs`](../../crates/rimz/src/mux/tmux/options.rs) | Room options, key bindings, hooks, and sidebar-pane classification. |
| [`presence.rs`](../../crates/rimz/src/mux/tmux/presence.rs) | The control-mode presence watch. |
| [`parse.rs`](../../crates/rimz/src/mux/tmux/parse.rs) | Command-output parsers. |

Both backends take named keys and bracketed-paste markers from [`pane::keys`](../../crates/rimz/src/pane/keys.rs). The presence plugin is its own crate ([below](#the-zellij-presence-plugin)).

Three modules outside `mux/` complete the picture. [`room/`](../../crates/rimz/src/room/mod.rs) sits above the trait and owns managed identity, birth and reset, the pre-attach health gate, and presence-load ordering. [`sidebar/presence/`](../../crates/rimz/src/sidebar/presence/projector.rs) turns normalized transitions from either backend into typed `SidebarEvent`s. [`wakeup/`](../../crates/rimz/src/wakeup/mod.rs) sits below both and is the wire those events travel on: the renderer heartbeat record, the versioned envelope, and the best-effort datagram send, which this seam uses without importing the sidebar.

## Choosing a backend

[`auto_detect_backend`](../../crates/rimz/src/mux/selection.rs) takes the first match:

1. the `--mux <name>` flag,
2. the active environment (`ZELLIJ` or `ZELLIJ_PANE_ID`, then `TMUX` or `TMUX_PANE`),
3. `[mux] default` from per-machine config, which fails fast when it names an uninstalled backend,
4. the installed binary, tmux preferred when both are present.

The flag and the environment decide before config loads, so a command run inside a live room always addresses that room.

Room identity is path-derived and shared across backends, so every worktree of one repository resolves to the same recorded session name on either backend. New sessions take the state directory name; a live older session keeps its recorded name until its next birth (see [the workspace record](./store.md#the-workspace-record)). A rival session under that name on the other backend would share the store while its panes stayed unreachable. Every entry point that addresses a live room (`rimz start`, `rimz reset`, `rimz attach`, `rimz web`, and supervised pane launches) therefore resolves through [`pick_mux_for_session`](../../crates/rimz/src/room/session.rs), so an auto-selected launch lands on the backend that already owns the room. Start attaches to it, reset tears it down there before rebirthing on the resolved default, and web hands the resolved session to the shared ttyd daemon, whose shim attaches through the owning backend. The session list behind that pick retries up to `LIST_SESSIONS_ATTEMPTS` (3) times, `LIST_SESSIONS_RETRY_DELAY` (250 ms) apart. [`ensure_single_backend_room`](../../crates/rimz/src/room/session.rs) guards birth itself and refuses an explicit `--mux` that names a backend other than the live room's owner.

## The `MuxBackend` trait

[`MuxBackend`](../../crates/rimz/src/mux/mod.rs) is the whole seam: every Zellij or tmux command in RimZ lives behind one of its methods.

| Group | Methods | Notes |
| --- | --- | --- |
| Session lifecycle | `name`, `ensure_session`, `attach_command`, `attach_existing_command`, `attach_readonly_command`, `detach`, `kill_session`, `list_sessions`, `list_sessions_within`, `session_liveness`, `version` | The attach methods return a `CommandSpec` for the CLI attach runner instead of running it. |
| Pane inventory | `list_panes`, `cached_pane_roster`, `client_view` | See [reading the room](#reading-the-room). |
| Pane I/O | `capture_pane`, `send_keys`, `send_key`, `paste_text` | `paste_text` writes one bracketed paste; the submit Enter is a separate `send_key`. Chunking, failure cleanup, and the write lock are in [messaging.md → writing to the pane](./harness/messaging.md#writing-to-the-pane). |
| Structure | `split_pane`, `append_companion_pane`, `open_tab`, `rename_tab`, `open_sidebar`, `open_background_view`, `close_pane`, `close_view_floating_panes` | Callers pass backend-neutral argv and layout geometry. |
| Focus and geometry | `focus_pane`, `toggle_fullscreen`, `sidebar_width_step`, `nudge_sidebar_width`, `record_sidebar_width_default`, `register_room_key` | |
| Health | `probe_session_health`, `ensure_clean_session`, `reconcile_sidebars`, `purge_resurrection_cache`, `resurrection_cache_paths`, `session_accepts_agent_close` | Several default to a no-op because they answer a Zellij-only question. |
| Presence | `ensure_presence_plugin` | Zellij only; tmux keeps the no-op default because its control-mode watch already pushes. |

A method with a sensible cross-backend answer carries a default implementation, so a backend implements only what it does differently. `ensure_clean_session` and `purge_resurrection_cache` exist because Zellij resurrects sessions and tmux does not; tmux takes the no-op and calling code stays branch-free.

Everything correctness-critical stays above the trait and is identical across backends: the store, the run and wakeup sockets, the event schema, the trust gate, and the agent hooks.

### Structure calls

`append_companion_pane` checks native occupancy before birth and balances the grid best-effort afterward; a `Full` result guarantees no payload ran. Companion-grid planning excludes sidebar chrome before checking occupancy or balancing, and on Zellij it declines a tab holding a fullscreen or suppressed pane. tmux recognizes the sidebar by its spawn argv as well as its title and current command, decoding the outer quoting of a single shell-command argument, so a hook-born sidebar counts as chrome before its title arrives.

`TabOptions::after` opens a new view immediately after the view containing an anchor pane; `None` appends. tmux resolves the pane to its window id and runs `new-window -a`. Zellij appends, focuses the new tab long enough to move it left past the required neighbours, then restores an unfocused launch by re-resolving the original pane's tab position. Placement is best-effort on both backends: a failure leaves the view appended and the launch continues.

`LayoutPanes::focused_pane` names the pane that holds focus inside the new view, counted in layout order; `0`, or an index past the last pane, keeps the leading pane. Zellij marks that pane `focus=true` in the tab layout. tmux splits every pane with `-d`, then runs `select-pane` on it. Team launches and resumes set it to the leader's pane.

### Command discipline

Every control command runs through [`CommandSpec`](../../crates/rimz/src/mux/command.rs) under a deadline. At the bound the child is SIGKILLed and the caller gets `MuxErr::Timeout`.

| Bound | Value | Why |
| --- | --- | --- |
| `COMMAND_TIMEOUT` | 30 s | A `zellij action` whose server died busy-loops at 100% CPU and would otherwise hang the caller. |
| `LIST_SESSIONS_TIMEOUT` | 3 s | A read-only local query on hot paths. |
| `TAB_RENAME_TIMEOUT` | 2 s | Tab status is disposable chrome, so a stuck backend drops the rename instead of holding a worker for the structural bound. |
| `SESSION_PROBE_TIMEOUT`, then `SESSION_PROBE_RETRY_TIMEOUT` | 1 s, then 3 s | The start-path responsiveness probe (`mux_responsive_preflight` in `cli/room/mod.rs`). A first timeout prints a `note:` line and retries once with the longer bound. |

A healthy command answers in milliseconds, and most callers treat a mux failure as best-effort, so a bound degrades the caller instead of blocking it. The start path is the exception: when the selected backend also times out on the retry, `rimz start` refuses with the backend's recovery command and the other backend as a fallback. Rival-backend and notice probes skip their enrichment and continue.

## Identity

### Pane and view IDs

Raw IDs stay inside the backend adapter, where native commands expect them (Zellij `terminal_3`, `plugin_1`; tmux `%3`). Everywhere else they travel normalized as `zellij:<raw>` or `tmux:<raw>`: in env vars (`RIMZ_PANE_ID`), store events, snapshots, and CLI arguments. [`pane_from_env_value`](../../crates/rimz/src/mux/mod.rs) is the one env-to-ID mapping, and `ensure_pane_backend` rejects a pane addressed to the wrong backend before any command runs.

`ViewId` names the view holding a pane (a Zellij tab, a tmux window) by the identity RimZ observes on every fast path: Zellij tab position (`tab_1`), tmux window id (`@3`). `PaneRef.view_id` is the flat read-side field, and the producer lifts it into `TabFrame.view_id` so per-view sidebar bookkeeping runs over typed topology.

**A view id is never the view's on-screen label.** Zellij's default tab names are number-shaped (`Tab #16`), so matching a positional `tab_15` against a tab named "Tab #15" joins two unrelated id spaces and picks the wrong tab in any session that has closed one. The label is minted at tab creation and does not follow position; `PaneRef.view_name` carries it for display only. Resolve "which view holds this pane?" through the pane id.

### Tab names

`rename_tab` takes a normalized pane anchor, never a display name or a positional `ViewId`. tmux accepts the pane directly as a `rename-window` target; Zellij resolves the pane through an authoritative listing and passes the tab's stable id to `rename-tab-by-id`.

An ordinary launch label is pane-name tokens joined by `+`: `<profile-or-kind>` for agents, the executable basename for commands, and the shell basename for empty command cells. The label keeps the first three tokens and adds `+…` for overflow, with no directory suffix (`opus`, `nvim+claude`).

Each rename carries a `TabNameIntent`:

| Intent | Effect |
| --- | --- |
| `Claim { pane_name }` | An in-place launch names the tab and pins the pane's own launch name. tmux clears any restoration marker; Zellij renames the anchor pane by id. |
| `Status { observed }` | Add the projected status glyph. On an automatically named tmux window, arm `@rimz_restore_automatic_rename` first; otherwise just rename. |
| `Rest { observed }` | Clear a stale glyph. If the tmux marker is armed, rename, restore inherited `automatic-rename`, and remove the marker; otherwise just rename. |
| `Release { observed }` | Rename to the shell's name. tmux restores inherited `automatic-rename`, removes the marker, and clears `@rimz_title` on every pane in the window. Zellij keeps the pane names, renames only the tab, and keeps that explicit name because it has no automatic tab naming. |

`Status`, `Rest`, and `Release` carry the tab name the producer projected them from, and apply only while the tab still bears it; `Claim` is unconditional. The producer applies its renames off-thread from a frame it observed earlier, so without the check a projection from a frame that predates an in-place launch overwrites that launch's `Claim` and nothing converges back. A miss is a silent `Ok`: the next frame projects again from the current name. tmux runs the whole intent in one command list: it stashes the observed and target names in `@rimz_rename_observed` and `@rimz_rename_target`, runs the intent's commands under `if-shell -F` on `#{==:#{window_name},#{@rimz_rename_observed}}` with the rename's name read back from `#{@rimz_rename_target}` (so no user name passes through format or command parsing), and unsets both options. Zellij has no conditional rename; it compares the name in the same `list-panes` listing that resolves the tab id, which leaves one round trip in which a rename can still slip between the check and `rename-tab-by-id`. The tmux pane listing replaces commas in window names with `_` and trims edge whitespace, so the observed name of a window whose name has a comma or edge whitespace never equals the raw `#{window_name}`: such a tab gets no projected glyph, and its user-chosen name is never rewritten.

The marker records only automatic naming that `Status` interrupted, for `Rest` to undo. It is not an ownership record, and `Release` does not read it.

The elected sidebar producer projects `Status`, `Rest`, and `Release` from the tab's contents, without waiting for an agent-exit hook. The status suffix is chrome, rebuilt from projected agent state, and never identity or durable truth. Name-based birth and resume checks strip built-in and configured status suffixes before comparing their idempotency keys.

Ownership decides whether a tab may be released, and it is derived from the name each time. Strip the status suffix, never release scoped `#` and `team:` names (even when a pane shares that name), ignore `…`, and require every remaining `+` token to match a work pane's launch name, in any order. Sidebar chrome and daemon-host panes do not count as work panes. A live agent row or a hosted agent in any work pane, floating panes included, blocks release even when idle, sleeping, or past the success-glyph timeout. A user name that does not match the panes is kept; a user name identical to a pane-derived label follows the same rule. A tab that is not pane-named still has a stale glyph cleared, including a tab left with only chrome or daemon panes.

### The identity pin

Session birth stamps the room's identity (`RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, through [`pin_env`](../../crates/rimz/src/workspace.rs)) and the registry's adapter-enrichment environment into the session. Every pane inherits it, and so does every agent and every in-pane hook child. A daemon-routed hook that misses the pin recovers it from the in-pane agent process ([adapter.md](./agents/adapter.md#hooks-resolve-the-room-they-live-in)).

Each backend pins at its own birth seam, and they differ in whether a live session can be updated:

- **tmux** sets identity and adapter env on `new-session` and re-asserts them idempotently on every ensure, so panes born later carry the current values. The same seam stamps `COLORTERM=truecolor` when the launching terminal advertises it, because tmux births panes under `tmux-256color` with an empty `COLORTERM`, and room apps read RGB capability from the session environment.
- **Zellij** carries the map on the spawning client's environment, from which the per-session server and every pane fork. There is no post-birth re-assert, so a session keeps the environment it was born with until rebirth. Zellij birth also stamps `TERM=xterm-256color` when the spawning environment has no `TERM`, so a non-PTY birth (remote-web prep, a headless launch) still yields panes with usable terminfo.

Rebirth re-pins on both backends. Panes also carry a pane-scope pin ([`pane_pin_env`](../../crates/rimz/src/workspace.rs)): `RIMZ=1`, `RIMZ_WORKTREE_PATH` set to the tab's cwd, and `RIMZ_CHANNEL` when the tab has a channel. A live tab applies it through `TabOptions.env` and a resume tab through `ResumeTab.env`, both rendered per pane (tmux `-e K=V`, Zellij an `env` argv prefix), so a re-seeded agent or a reborn `#channel` shell starts with the same identity a fresh launch gets. The harness builds `ResumeTab.env` from stored fields; the backends never derive a channel from a tab label.

A pane's launch-name pin is separate from room identity. tmux stores it in the pane option `@rimz_title`, sanitized like window names (`:` and `.` become `-`), and removes it from every pane in a released window. Zellij uses the pane name and keeps it on release.

### Pane metadata

`list_panes` reports each pane's id, view, foreground command, optional spawn command, optional title, cwd, and floating flag. The title is the pinned launch name when present, otherwise the multiplexer's pane title; the published pane frame keeps it for tab-name reconciliation.

The sidebar uses the foreground command for display, the spawn command for identity only while the pane root still runs the spawn program (or the foreground is briefly unreported), and cwd for worktree grouping ([sidebar.md → presence model](./sidebar/sidebar.md#presence-model)). A foreground shell therefore demotes a pane's historical agent argv. Foreground, title, and cwd exist on both backends. Spawn is optional because Zellij omits it for panes created with `action new-pane`, while tmux exposes the static `pane_start_command`. **The parity floor for presence is command plus cwd**, which both backends meet.

Zellij adds two wrinkles. The foreground command reaches RimZ as a full argv string, so every Zellij source (the plugin cache on ingest and the authoritative listing) clears a `rimz agents <spec>` launcher command before a consumer reads it. A layout-named `rimz-sidebar` pane always reports `rimz-sidebar` as its foreground, so it filters as chrome even when Zellij omits the command fields.

tmux exposes `pane_floating_flag` from 3.7; older supported releases expand the unknown format empty and report every pane tiled. Floating agent panes stay addressable but out of the room-row projection, and a self-closing sidebar view closes same-view floating panes before its tiled anchor exits.

Neither backend reports a per-pane process start, so RimZ derives `pane_process_start` from the process backend ([produce/panes.rs](../../crates/rimz/src/sidebar/produce/panes.rs)) and uses it as the key that refuses a match against a reused id. Both backends reuse ids:

- Zellij recycles pane ids within one session.
- tmux `%id`s are unique within one server lifetime, but the RimZ server exits when its last session ends, and the next command births a replacement that numbers from `%0` again. A durable record naming `%3` can outlive the pane it described.

The pane PID is only the process-walk root and the metrics binding, because it is the pane's shell, not the agent it launched. Agent liveness uses the agent's own PID, captured best-effort by its hook ([instances.md](./agents/instances.md#session-death)).

### Outer-terminal titles

RimZ sets the attached terminal's title for every pane it creates. Both backends show the room session and a short pane or process identity, and ignore a shell's OSC 0/1/2 host-and-path title.

Zellij computes the outer title from the focused pane. A non-empty layout or `new-pane --name` value takes precedence over the pane's OSC-derived title, so birth layouts name shell panes by shell basename, channel shells by channel label, and caller-identified command panes by that identity. Other command panes, and runtime splits, fall back to the executable basename.

tmux enables session-scoped `set-titles`. For a caller-identified pane the backend writes `@rimz_title`, which terminal escape sequences cannot change, and the fixed `set-titles-string` shows that value or falls back to `#{pane_current_command}`. The format excludes `#T` and `#{pane_title}`, which applications rewrite with OSC title sequences. Pane-scoped options arrived in tmux 3.1, below RimZ's 3.5 floor.

## Reading the room

Each backend has one authoritative roster and, optionally, one push channel that keeps reads fresher.

| | Zellij | tmux |
| --- | --- | --- |
| Authoritative roster | `zellij action list-panes --all --json` | `tmux list-panes` against the managed socket |
| Push channel | The presence plugin, publishing `pane-topology.json` | A control-mode client holding `refresh-client -B` |
| Client presence | Attached client count, from the plugin's `list_clients()` | Attached client count plus `last_input_ms` from `#{client_activity}` |
| Idle clock | None; presence is attach-only | Yes |

`PaneListOptions.consistency` says how far a caller trusts the push channel:

| `PaneReadConsistency` | Behaviour |
| --- | --- |
| `Cached` (the default) | Use a valid pushed topology, requesting a newer push when needed. |
| `PreferAuthoritative` | Query mux truth first, and fall back to a valid pushed topology. |
| `RequireAuthoritative` | Query mux truth and propagate failure. Only this level licenses a destructive decision from pane absence. |

**Absence in a cache is never proof.** `cached_pane_roster` states the same rule at the trait: a listed pane proves liveness, while `None` or a missing id only permits escalation to an authoritative read.

Both push channels feed the shared projector, [`sidebar/presence/projector.rs`](../../crates/rimz/src/sidebar/presence/projector.rs), which applies one launch-chrome and sidebar suppression policy and emits the same `SidebarEvent` taxonomy for either backend. Backend-specific state stays minimal: [`TmuxPresenceState`](../../crates/rimz/src/sidebar/presence/tmux.rs) keeps only what it needs to normalize out-of-order control-mode lines, and [`sidebar::presence`](../../crates/rimz/src/sidebar/presence.rs) normalizes plugin payloads. Which room change produces which event on each backend is in [state.md → what triggers a mux-derived event](./sidebar/state.md#what-triggers-a-mux-derived-event).

The producer stretches its pane-cache TTL while a backend's presence stamp is fresh, and topology changes still repaint through typed overlays plus a verifying pull. Stale Zellij topology triggers a cheap plugin pipe and a bounded cache wait; stale tmux presence returns to the steady poll. The budget is in [performance.md](./performance.md).

## Focus

Three questions hide behind the word: who is looking at which pane, how RimZ moves focus, and how a keystroke reaches the sidebar from any pane.

### Who is looking at what

`client_view` reports the panes attached clients are looking at. The producer publishes the distinct terminal set as `viewed_panes`, and that set gates every side effect that depends on a human looking at a pane: unread focus clears, tab-view sweeps, notification and reminder suppression, focused-tier cadences, and background paint suspension. Several clients viewing the same terminal agree; distinct terminal or plugin views are ambiguous.

`ClientView::unique_live_focus` resolves a single focus: fresh attached-client evidence counts only when every observation names the same live pane. Detailed client rows outrank summarized pane ids, and dead summarized panes do not invalidate one distinct live pane.

`PaneFrame.focused_pane` is the session's focus register:

| Sample | Effect on `focused_pane` |
| --- | --- |
| Fresh, every attached view names one distinct live terminal | Set to that pane |
| Fresh, and empty, plugin-only, dead, or distinct | Cleared |
| Unavailable | May hold the prior live value |
| Realtime `FocusChanged` between pulls | Updated |

The attached-client sample is the only runtime authority for focus. Hidden tabs carry no RimZ focus state, and the renderer's `UiState::baseline_pane` is only a local highlight and restoration hint. `PaneRef` and pane topology carry no focus bit: `rimz pane list` reports identity and process context without an active mark, hook recovery uses a fresh unique client view to choose among plural candidates, and `rimz sidebar focus --toggle` requires the same unambiguous view. Upstream roster focus marks never enter RimZ's model, diagnostics, binding, or repair decisions.

### Jumping to a pane

`focus_pane` is a one-way jump that crosses views on both backends: Zellij switches to the containing tab directly, and tmux selects the window, then the pane.

Every attached-client jump is wrapped in the two-phase intent in [`focus_anchor.rs`](../../crates/rimz/src/mux/focus_anchor.rs): `Requested` is durable and wakes every renderer before the command, and acceptance moves the same nonce to `Applied`. Native client observations then confirm, supersede, invalidate, or fence it; the phases and verdicts are in [state.md → focus intent](./sidebar/state.md#focus-intent). The separation matters on Zellij, where `action focus-pane-id` can move the visible pane and routed input without a matching `ListClients` update.

### Room keys

The sidebar's in-pane keys fire only when its pane is focused, so a room-scoped chord reaches it from any pane. The multiplexer intercepts the keystroke before it lands in the focused pane.

- **`[sidebar] focus_key`** (default `Alt+p`) runs `rimz sidebar focus --toggle`. It focuses this session's `rimz-sidebar` pane, or returns to a deterministic working sibling when the sidebar is already current, and acts only when one unique fresh client view proves which is current. An unavailable or distinct view returns a non-mutating ambiguity error.
- **`[sidebar] zoom_key`** (default `Alt+g`) runs `rimz pane zoom`. With one unique attached-client focus it toggles fullscreen for the focused work pane. If that pane is sidebar chrome, RimZ resolves a non-chrome sibling in the same view, records and applies the focus intent, then fullscreens the sibling. With no sibling the view is unchanged.

[`FocusChord`](../../crates/rimz/src/mux/focus_key.rs) parses both chords once (`Alt` or `Ctrl`, with `M-`/`C-` prefixes and `-`/`+` separators). `Alt` is the default because it passes through the terminal, Zellij's locked mode, and tmux's prefix; `off` or an empty value registers nothing. Registration is best-effort at session birth, so a convenience key never blocks a room.

The backends bind the chords differently:

- **tmux** binds a server-global root-table key that carries no room identity and resolves the pressing pane's session at keypress.
- **Zellij** routes through the presence plugin. RimZ passes both chords in the plugin's load configuration, and the plugin installs them in one runtime `Reconfigure`, each as a `MessagePluginId` action to its own instance. The focus pipe runs the sidebar toggle and the zoom pipe runs `rimz pane zoom`. The user's `config.kdl` is unchanged, and the bindings end with the session.

## One sidebar per view

Every occupied view should hold exactly one live sidebar pane. `reconcile_sidebars` converges toward that in place, without disturbing working panes and without recreating the session.

The planner in [`reconcile.rs`](../../crates/rimz/src/mux/reconcile.rs) is pure. Each backend groups its native listing into `ViewSidebars` (the view's sidebar panes in mux order, plus whether the view holds working panes or daemon hosts) and supplies native add, close, and verification effects. A view is occupied when it holds a working pane or a daemon host, so the [daemon view](./rimzd.md) gets the same verdicts as a working view; reconcile never closes the hosts themselves. `plan_reconcile` emits at most one verdict per view:

| View state | Verdict |
| --- | --- |
| Occupied, one sidebar claimed by a live renderer | `CloseDuplicates` for the rest, or nothing when it is the only one |
| Occupied, no sidebar | `Add` |
| Occupied, sidebars exist but none is claimed | `Replace`: add first, close the old ones only after the new pane mounts |
| Orphan (no working pane, no daemon host) | `CloseDuplicates` for every sidebar, so a wedged renderer collapses with its view |
| Orphan, while a live renderer is unlocated | `CloseDuplicates` for all but one sidebar |

The renderer applies the same empty-view rule without waiting for reconcile: once the last working pane exits, the sidebar closes itself ([sidebar.md → self-close](./sidebar/sidebar.md#self-close)), and both multiplexers remove a view when its last pane closes. A companion subagent tab therefore collapses after its final child finishes, with no close-tab primitive.

`SidebarLiveness` carries the claims: `claimed_panes` from fresh renderer heartbeats, plus `young_panes` inside the first-heartbeat grace window so a pane that just started is never reaped. `has_unlocated` marks a live renderer whose pane could not be placed and keeps the planner conservative. On Zellij repair it also carries the presence probe's observation floor, so the pass cannot judge width from topology older than the probe it already awaited. The launch pass has no floor: it still repairs pane count and docking but leaves width to the renderer, so attach does not wait on a topology dump.

Replacement is add-before-close. [`mount_proof.rs`](../../crates/rimz/src/mux/mount_proof.rs) waits up to six seconds for the new pane to publish a heartbeat naming the expected build before the old pane closes. A failed add leaves the user with the sidebar they had, and a pane running a stale binary never counts as the repair.

`SidebarRecovery` tallies the pass (`recovered`, `closed`, `failed`, `deferred`, `redocked`, `misdocked`). `execute_reconcile_plan` stops at the first failed verdict and counts it and every remaining verdict as `failed`. The pass is one best-effort attempt: nothing retries it, and a failure never escalates to a session rebirth. The next elder tick, toggle, or `rimz sidebar repair` runs a fresh pass.

### Width

[`width.rs`](../../crates/rimz/src/mux/width.rs) resolves one room target from configured policy and live geometry, and [`sidebar_pane/app/width_control.rs`](../../crates/rimz/src/sidebar_pane/app/width_control.rs) is the renderer-local controller that converges each pane toward it.

**The record.** The room-runtime record in [`width_target.rs`](../../crates/rimz/src/mux/width_target.rs) holds a `WidthPermille` (tenths of a percent of the full view) and a pin flag. An unpinned target follows `theme.display.width_percent` (a set value clamps to 10 through 90; unset means 30% for a view wider than 240 columns, otherwise 25%) and applies `theme.display.max_cols` whenever live view geometry is known. An `a`/`d` keypress or a mouse drag pins the resulting share verbatim, so an explicit choice may exceed the cap and keeps its proportion when the terminal resizes. A genuinely new session clears the record and returns to configured policy.

**Resolution.** Resolving produces a `SidebarTarget`: one share, the configured cap, and whether the user pinned it. That answer crosses the backend seam; `SidebarWidth` policy does not. A width repair renders columns against the view geometry it measured, rounding fractional shares up and clamping an unpinned default to the cap, while Zellij layouts spell the share as a whole percentage. Resolution is read-only except in two cases: birth geometry from a real terminal probe, or a live backend viewport proven for the current event, may adopt and rewrite the room share. A resolve without geometry keeps an existing share; with no record it returns the narrow policy fallback for that call without persisting it. Columns without geometry use the bare cap, because a detached layout's eventual view width is unknown.

**Keypress steps.** Every view receives the same exact permille share. An `a`/`d` keypress moves one backend column step, converts the result to a share, pins it atomically, and broadcasts `WidthTargetChanged`; each renderer resolves the share for its own view and converges with at most one mux resize in flight.

- **tmux** applies the absolute target in one command, and a narrower intent clamps at the 24-column floor (`MIN_ADJUSTABLE_WIDTH`).
- **Zellij** resizes only relatively, in steps of about 5% of the view, so views born on different width lattices can settle one native step apart. A wider keypress uses the floor of that step, so it never targets past the next reachable width; a narrower keypress uses the ceiling. The ceiling is also the stop-step width, which keeps a lattice width inside the settled band when native steps alternate between floor and ceiling. Each accepted intent issues one relative step per resize-feedback event or per `FEEDBACK_TIMEOUT` (1 second). A narrower intent clamps at 24 columns and is ignored once the pane is there. Missing topology rejects either direction, because a fabricated view would pin the wrong room-wide share.

**Convergence.** The renderer controller and reconcile-time repair share one settled band. For target `t` and backend stop step `s`, a width is settled when its distance from `t` is at most half of `max(s, 1)`, and a step is issued only while moving toward `t` can land strictly closer. tmux steps one column and lands exactly on `t`. Zellij reserves the ceiling of its relative step, and the renderer widens that estimate when observed feedback is larger. A step that increases the distance earns one reverse step: the renderer parks a reversal that restores at least the prior distance, and reconcile, which keeps no state between invocations, stops after its one reversal.

**Parking.** The renderer parks instead of spinning. A step whose feedback never arrives is retried once, then its no-progress cycle is retried once after `IDLE_RETRY` (5 seconds), which also re-probes the viewport and re-derives the target if the proven viewport changed. After `MAX_NO_PROGRESS_CYCLES` (2) unacknowledged cycles the renderer holds until its width moves, a structural change lands, or the target changes, and a convergence that spends `MAX_STEPS` (32) steps parks on its step budget. A transient resize failure, or an attach after a detached birth, therefore gets one delayed retry. Reconcile relies on its later passes instead.

**Structural changes.** Typed pane-open, pane-close, and topology-change events mark a structural resize before their overlay or refetch handling. An off-spec pane converges immediately to the existing target, and continuously tracked sibling counts are the pulled-truth fallback when an event is missed. The marker also keeps a stalled structural correction from being read as a mouse drag.

**Mouse drags.** The settle pass arms only when the measured pane sits outside the settled band. A view change re-resolves from config. Adopting a drag needs positive evidence: backend geometry exists, no structural marker falls within `STRUCTURAL_GUARD_MS` (2 seconds) of the resize, and the sibling observation is at least that much newer than the resize. While evidence is pending, including when geometry is missing, the controller waits without nudging, so it does not fight a real drag. The first measured movement consumes any remembered unacknowledged step: movement toward that step's target is late feedback from RimZ's own nudge and is never adopted, while movement away goes through drag classification. A drag that ends inside the band leaves the share untouched. Otherwise RimZ pins the measured width as the room share and broadcasts it once; the dragged pane keeps its width and other Zellij panes settle at their nearest reachable width.

### The fullscreen hold

Zellij fullscreen holds width convergence; it is not geometry to repair. The presence topology carries each pane's fullscreen bit, and any fullscreen pane holds its whole tab: Zellij geometry repair (`off_spec_sidebars`) skips the tab, and the renderer parks width control without arming its idle retry. A later topology observation with fullscreen cleared releases both. RimZ never resizes or redocks against the fullscreen pane's override geometry or its hidden siblings' stale geometry. Structural verdicts (`Add`, `Replace`, `CloseDuplicates`) still apply to a fullscreen tab.

## Session lifecycle

[`RoomContext`](../../crates/rimz/src/room/mod.rs) sits above the trait and owns the shared parts: managed identity, config derivation, birth, reset, pre-attach health, heartbeat purge, and presence-load ordering.

### The pre-attach health gate

`open_sidebar` is best-effort and can be skipped or fail, so it cannot be the only thing between the user and a resurrecting attach. A normal managed entry probes a live session before setup side effects and threads that verdict through birth; when no reusable live verdict exists, birth runs `ensure_clean_session` to create or cleanly rebirth the session. Supervised birth calls `ensure_clean_session` directly. Both paths must return an attachable verdict before presence load and attach preparation.

| Session state | Gate action | Verdict |
| --- | --- | --- |
| Live and responsive | Keep the room after a bounded direct pane probe | `Healthy` |
| Live but unresponsive | Keep the room and refuse attach | `Unresponsive` |
| Absent | Birth from the layout | `Reborn` |
| Exited (Zellij resurrection record) | Delete, then birth from the layout | `Reborn` |
| Still not live after a rebirth | Nothing further | `Stuck` |

tmux has no resurrection, so its gate is a no-op `Healthy`.

A Zellij IPC socket path that overflows is an environment precondition, not a health verdict: it classifies as `SocketPathTooLong`, reset is not offered, and `rimz doctor` prints the shorter-directory fix.

### Reset

A `Stuck` room needs a destructive reset. [`RoomContext::reset`](../../crates/rimz/src/room/mod.rs) gives explicit `rimz reset` and attended stuck recovery the same teardown and store reset, while the CLI keeps confirmation and report rendering. The teardown routine in [`room/teardown.rs`](../../crates/rimz/src/room/teardown.rs) kills the session and purges the serialized-session cache through the backend, reaps stale sidebar runtime files, and runs the mux process sweep for orphaned servers and leaked daemons, then the room is reborn. Each step is best-effort and independent, so one failure never blocks the others.

The process sweep in [`recovery.rs`](../../crates/rimz/src/mux/recovery.rs) is the dangerous step, because it signals processes by heuristic. Four scopes bound it: the real uid, the recorded session name as a whole command-line token (bounded by start/end, whitespace, or `/`), an exclusion of this process and its ancestors, and the inherited environment domain. Whole-token matching keeps prefix-related state directory names from selecting each other's processes. [`ProcessDomain`](../../crates/rimz/src/mux/domain.rs) is the last guard: a process in a foreign domain (a `cargo xtask sandbox`, another runtime root) is not RimZ's to signal, and a process whose environment cannot be read is spared.

Without a terminal, `rimz start` fails fast with the `rimz reset` fix instead of destroying a session unattended.

### Sidebar orphan reaping

Two paths kill a sidebar process whose pane is gone, and both require authoritative absence.

The supervisor's pane watchdog (`sidebar_pane/supervise.rs`) reads `cached_pane_roster` first as a latency hint. On escalation, one sidebar wins a workspace-and-session single-flight lock, lists with `RequireAuthoritative`, and atomically publishes mux kind, session, observation stamp, and pane ids, so peers consume that same observation. A sidebar counts at most one strike per observation, resets on presence, keeps its strikes on unknown evidence, and exits only after `PANE_GONE_STRIKES` (3) distinct authoritative absences. Lock contention, timeout, a stale or mismatched cache, parse failure, and mux failure all count as unknown and never trigger a local destructive fallback.

Reload and repair reap paneless sidebar processes ([`reload.rs`](../../crates/rimz/src/reload.rs)). A fresh cache can prove a pane present and skip the mux command, but a cache omission only nominates a process: two `RequireAuthoritative` rosters separated by a short delay must both omit the pane before RimZ signals it. The candidate must also prove, through its inherited environment, that it lives in the invoker's state and mux-socket namespace. If either roster fails, the whole reap aborts. A cache omission that either roster refutes records `pane_cache_divergence`, and every signalled process records `sidebar_orphan_reaped` with both observation stamps and the SIGKILL outcome.

## Zellij backend

One constraint shapes this backend: **a Zellij layout applies only at session birth, and a layout is the only way to create a pane already docked left at a set size.** RimZ therefore owns the birth layout and treats everything after it as convergence: close stray sidebars by id, add a missing sidebar in place, move it left, and converge its width toward the room target.

Zellij has no tab-width query, so RimZ infers the viewport from the rightmost tiled pane extent. A viewport is proven by shape and by time: the tab holds at least two tiled extents, and the topology observation is no older than the structural event or repair probe it is used to judge. A lone sidebar is the only extent until a sibling mounts, so the inference would measure the sidebar against itself, and the viewport stays unknown; a completed snapshot taken before the event is unproven too. Renderer and reconcile probes retry instead of resolving or resizing from either case. Layout-only birth paths may use their read-only configured seed, and live convergence adopts the room target once current topology proves the geometry.

`ensure_session` is a no-op because Zellij creates sessions lazily. The sidebar launch owns first birth through `attach --create-background` with a generated layout.

### The birth layout

Every tab has the same shape: a left `rimz-sidebar` pane and a focused terminal, above a one-row compact-bar plugin. A `new_tab_template` plus explicit birth tabs carry that shape.

These details in [`layout.rs`](../../crates/rimz/src/mux/zellij/layout.rs) are load-bearing:

- The sidebar command names the workspace's stable room-bin path, not one sweepable build generation, so the immutable `new_tab_template` keeps spawning working sidebars after reloads.
- The sidebar pane is borderless and `close_on_exit`, so work-pane frames can be styled while sidebar hit-testing starts at row 0, and the pane disappears when its process exits.
- Every sidebar width is a whole percentage, because Zellij pins a fixed-size layout pane against resize. With known geometry the percentage approximates the resolved share; detached birth tabs and the template keep configured percentage policy until a live view exists, and live convergence applies the exact target.
- Every tab is born with an explicit focused terminal. A `children` placeholder nested in a split is never filled and would leave focus on the sidebar alone.
- The layout file outlives the create call. Zellij parses `--default-layout` asynchronously, so the temp file stays on disk until the panes materialize.
- The layout ends with `session_serialization`, `disable_session_metadata`, and `stacked_pane_list false` ([room options](#room-options-and-the-cli-xor-problem)).

Birth branches on the session's liveness from `zellij list-sessions`:

| Liveness | Branch |
| --- | --- |
| Live | Before attach, a bounded direct `list-panes` probe must show the control plane responds. A responsive session already has its sidebar and owns every resize and split since. Sidebar launch trusts a fresh heartbeat; after a stale heartbeat it rebuilds only a room whose presence topology can be inspected, and otherwise leaves the sidebar alone. |
| `Exited` (`EXITED - attach to resurrect`) | Clean rebirth: delete, then create from the layout. With serialization off (the default) Zellij does not mint this state, but a session serialized with it on still can. |
| Absent | First birth: create from the layout. |

A session can stay in Zellij's live roster while its per-session screen thread no longer answers control commands. RimZ retries native pane queries within one `HEALTH_PROBE_TIMEOUT` (8 seconds) budget. When none succeeds it classifies the session as unresponsive, separate from a rebuildable exited session: it refuses to attach before entering the alternate screen, keeps the room and its panes, and points the user at `rimz doctor` to inspect or `rimz reset` to rebuild. tmux needs no per-session probe, because its session roster and pane queries share one server event loop.

### Room options and the CLI XOR problem

`<room-options>` combines RimZ's defaults with the optional `[zellij]` keys the user sets in RimZ config. Each maps onto a Zellij `options` flag ([reference → options catalog](../externals/mux-adapter/zellij-reference.md#options-catalog)). Flags newer than the floor are version-gated, so an older host keeps its default instead of aborting.

Mouse options need a second mechanism. Zellij XORs boolean CLI options against values already set in the user's `config.kdl`, so a CLI flag cannot set an absolute value for every user. Birth and attach still pass `--mouse-click-through true` and `--focus-follows-mouse false` as a hint for the birth window, and the presence plugin then applies RimZ's resolved values through `reconfigure(..., false)`, whose KDL path merges onto the live config absolutely and never writes the user's file. With the defaults on Zellij 0.44, a single click both focuses the sidebar pane and reaches the renderer, so a jump lands on the first click.

RimZ leaves `advanced_mouse_actions`, `mouse_hover_effects`, `mouse_mode`, and global `pane_frames` to `config.kdl` unless the user sets them in RimZ config.

**Serialization off by default.** `[zellij] session_serialization` defaults to `false`, and RimZ passes the configured value on every birth and attach. Resurrection does not help a room of agents: agents and scripts cannot restore their running state, so a resurrected room comes back as a wall of suspended command panes with a dead mouse. With serialization off, a crashed server's session vanishes, the next start births a clean running room, and RimZ owns rebirth ([resume on rebirth](./sidebar/sidebar.md#resume-on-rebirth)). The value is also written into the birth layout, because Zellij 0.44 drops `options` flags from `attach --create-background` before the detached server initializes. Attach first purges the room's resurrection cache, so a corrupt serialized layout cannot block a live session.

**Session metadata off by default.** `[zellij] disable_session_metadata` defaults to `true` and travels the same two routes. It stops Zellij's periodic `session-metadata.kdl` rewrite and its command-discovery `ps` loop, which at roughly 100 panes on 0.44.3 costs a visible share of Zellij server CPU. `Absent` and `Exited` sessions still converge through the same clean-rebirth gate, so the setting changes CPU cost, not room semantics.

### In-place repair

Live reinjection resolves a stable tab id from an existing work pane and runs `new-pane --tab-id` without a placement. Zellij answers before the pane mounts, so the backend verifies each step:

1. Mounted-pane discovery verifies the intended tab; the action's stdout is only a hint.
2. An add commits, or a replaced pane closes, only after a fresh current-build heartbeat.
3. A pane mounted in the wrong tab is cleaned up and aborts the pass.
4. Move, stack, retry, and verification reads use direct `list-panes --all --json` geometry after completed actions, with fresh presence topology as the recovery fallback.
5. Each left move crosses one adjacent pane, the current tiled-pane count bounds the swaps, and every step must strictly decrease the sidebar's `pane_x`.
6. Width convergence starts only after current geometry verifies a full-height left dock.

A timed-out authoritative read aborts the pass instead of falling back to the topology cache, so stale truth never drives a close or a spawn; the next elder or toggle pass retries.

RimZ passes `auto_layout=false` and `stacked_resize=true`, so `Alt+n` uses Zellij's native focused-pane split along the edge that suits the terminal's cell ratio, and closing a pane returns its space to the sibling it split from. It also pins `stacked_pane_list=false`. Zellij 0.45's list mode keeps collapsed stack members in `list-panes` but marks them suppressed and reports the stack's full rectangle for every member; RimZ filters suppressed panes and relies on per-pane geometry, so the classic representation keeps every agent observable with its own rectangle. The birth tree makes the sidebar and compact bar tree siblings. When an add nests the new sidebar into one row, the same transaction stacks every surviving work pane into the right column; repair of an arbitrary pre-existing multi-column layout only reports.

The producer's shrink-confirmation path bypasses `pane-topology.json` and reads `zellij action list-panes --all --json`, which carries Zellij's own live `pane_command` and `pane_cwd` for every terminal pane; the cached plugin copy fills only a field the listing left empty, and pid and geometry come from the cache the same way. If that query fails, the backend falls back to the topology cache with a debug log. tmux always lists from the server, so the authoritative flag changes nothing there.

### The daemon view and resumed births

`rimz start` always carries the `rimzd` runtime view, so `open_sidebar` births a two-tab layout: the runtime dashboard first, then the focused working tab. Writing that order into the birth layout avoids a visible post-birth tab move.

The runtime view is `sidebar | content | runtime`. What fills the columns, how its panes are identified by launch command, and how repair rebuilds them are in [rimzd.md](./rimzd.md#how-managed-panes-are-identified). Each managed Zellij pane carries its joined launch argv as an explicit pane name for that identity. tmux births multiple content or runtime panes as equal-height rows, with at most one row of rounding drift.

Scheduled loop runs split against the loop panel with Zellij's native stack, anchored through the panel's CLI pane context with `--near-current-pane`, so attached-client focus and the active tab stay put. tmux maps the stack to equal-height rows in the target column, computing each row from pane geometry and resizing only panes in that column, so the sidebar and neighbouring columns never move. Panel recreation and the new-tab fallback are in [rimzd.md → the loop zone](./rimzd.md#the-loop-zone).

A reborn session re-seeds its remembered agents: the birth layout spells one `sidebar | agents…` tab per worktree, each agent a command pane running its resume CLI in that worktree, with focus on the most recent. Panes born from a fresh layout start running, not suspended, which is the same reason serialization is off. One renderer handles plain, daemon, and resumed births.

### Tab-switch focus repair

Zellij can restore focus to the sidebar when the user switches tabs, which leaves them on chrome. Repair is a plugin observation plus a host verdict.

On a `Some(old) → Some(new)` tab switch, the plugin waits the focus settle window, takes one `list_clients()` observation, and publishes `switch-settled` with the active tab, a generation, and the full client views. It publishes no verdict.

The host classifies the observation against the accepted topology. A unique live work view in the active tab is healthy. A plugin view, the active tab's sidebar, or a live terminal in another tab is stranded, but only when the active tab has exactly one sidebar owner and a work sibling. Missing, detached, dead, superseded, foreign, and distinct-pane observations abstain. The same accepted sample also feeds host-side unique-live focus.

The renderer keeps its owner, TTL, client-ambiguity, and focus-intent guards, so automatic repair never overrides an explicit cross-tab jump ([sidebar.md → selection and jump](./sidebar/sidebar.md#selection-and-jump)).

### Zellij backend caveats

These are the upstream quirks the backend works around. The upstream surfaces are in the [reference](../externals/mux-adapter/zellij-reference.md).

- **Minimum version is 0.44.2** (`MIN_ZELLIJ_VERSION`), which `rimz doctor` reports as `meets_min_version`. Below it RimZ refuses the Zellij room and points at upgrading Zellij or using tmux. The floor is set by what the backend cannot work without: `action focus-pane-id` and `new-pane --tab-id` first ship in 0.44.1, so 0.44.0 can neither jump to a pane nor add a sidebar in place, and the `CommandChanged` event that feeds the plugin's foreground command first ships in 0.44.2. `stack-panes`, `advanced_mouse_actions`, `mouse_click_through`, and `mouse_hover_effects` exist at the floor; the two mouse flags stay gated on it, so an unparsed version omits them. `new-pane --no-focus` (0.45.0) is gated on its own version.
- **Pane IDs are positional, not stable.** Zellij has no stable per-pane CLI handle and reuses ids as panes close and reopen. Pane stamps carry `pane_process_start` so reconciliation can refuse a stale match.
- **`new-pane` answers before the pane mounts, and action stdout can cross clients.** The printed id is allocated before the screen thread mounts the pane, and a detached session can drop the mount entirely. Reconcile treats the id as a hint, discovers the mounted pane through plugin topology, and cleans up only a pane a fresh topology snapshot proves is a new `rimz-sidebar`.
- **`new-pane` can mount into a nested row.** A stable-tab add inherits the tab's split tree, so the sidebar can report `x=0` with a work pane spanning beneath it. Every add verifies the full-height left column and can stack the work panes it displaced without replacing their processes.
- **Named-session actions can print a session-not-found banner and exit 0.** A `--session <name>` action against an absent, exited, or still-registering session prints `Session '<name>' not found...` and an active-session list on stdout or stderr. RimZ classifies it as `MuxErr::SessionNotFound`; sidebar reconcile and daemon view launch defer quietly while the pre-attach gate owns rebirth.
- **A detached server can skip pane lifecycle processing until the next attach**, notably a last-pane exit and the relayout after a sibling closes. Reconcile defers adds on detached sessions, and the renderer's empty-tab self-close relies on its data-tick backstop, not resize delivery alone.
- **A server with no config file births a setup wizard that drops `new-pane` mounts** (layout-born panes mount normally). The test harness seeds a config at the home-relative path Zellij prefers; in production a first-time user dismisses the wizard once and reconcile retries the mount.
- **Plugin keybinds pause briefly on 0.44.x.** Zellij's `KeybindPipe` completion path can freeze the UI for about a second before a plugin keybind acts. The focus-key jump still lands.
- **New sessions use the state directory name** (`<basename-slug>-<hex>`). New slugs cap at 20 characters, leaving room for the usual 4- or 6-digit suffix under both RimZ's runtime socket directory budget and Zellij's macOS AF_UNIX socket budget. Existing directories are adopted unchanged; socket preflight still checks the actual path. Live recorded names survive upgrades until the next birth. Since the web relay port hashes the session name, that birth may also change its local port.
- **The presence plugin reports identity, geometry, foreground command, and cwd, not process identity.** The command follows `CommandChanged` and the cwd follows `CwdChanged`; RimZ resolves pid and process start host-side through the process backend and treats the spawn command as identity.
- **Tests isolate servers per test.** Tests construct the backend with a private runtime dir, since Zellij locates its server socket under `XDG_RUNTIME_DIR`. This is the counterpart of tmux's `with_socket`, and every command goes through the single `ZellijBackend::cmd` chokepoint, so one field threads isolation everywhere.

## The Zellij presence plugin

tmux offers a control-mode stream any process can attach to. Zellij does not: the only way to learn about pane changes as they happen is to run inside the Zellij server as a plugin. `crates/rimz-presence-zellij/` is that plugin, a headless wasm32-wasip1 binary loaded into every Zellij session RimZ manages. It renders nothing. Its contract is [`crates/rimz-presence-zellij/AGENTS.md`](../../crates/rimz-presence-zellij/AGENTS.md).

### The boundary: observations, never verdicts

The plugin publishes Zellij facts, and the host derives every meaning.

| The plugin owns | The host owns |
| --- | --- |
| Merged topology snapshots from Zellij's pane and tab manifests | Pane roles: which pane is a sidebar, which is an agent card |
| Attached-client observations, including the settled sample after a tab switch | Focus-repair decisions and the `SidebarEvent` taxonomy |
| Poke timing that Zellij's event model requires | Launch-chrome filtering and topology-writer authority |
| Capabilities that need plugin-only APIs: runtime keybinds, mouse `reconfigure`, fullscreen toggles, hiding or closing itself | Durable cache publication |

The split keeps product policy out of plugin releases: a change to what counts as chrome, when focus is stranded, or how an event maps ships in the `rimz` crate alone. Two rules follow. The plugin carries only facts that originate in Zellij's server state; a fact derivable from the OS goes through `pane_pid` on the host, which owns `/proc`. And a new wake shape is added only for a fact that an accepted snapshot diff cannot produce.

One session holds one plugin. Splitting control features across plugins would multiply lifecycle, permission, and writer-coordination work.

### Crate shape

The crate splits along the wasm boundary, which makes it testable.

| Module | Role |
| --- | --- |
| [`main.rs`](../../crates/rimz-presence-zellij/src/main.rs) | The wasm shell. Projects Zellij events into the engine, gathers runtime telemetry, and executes returned effects. Compiled only for wasm; host targets build a stub so `--workspace` builds and lints pass without the wasm toolchain. |
| [`engine.rs`](../../crates/rimz-presence-zellij/src/engine.rs) | The decision engine: room state, poke timing, focus correction, permission gating, topology publication. Returns `Vec<Effect>`. |
| [`policy.rs`](../../crates/rimz-presence-zellij/src/policy.rs) | Pure helpers and timing state machines: the stable-field hash, poke policy, foreground overlay. Time is injected as Unix milliseconds. |
| [`wire.rs`](../../crates/rimz-presence-zellij/src/wire.rs) | Every argv and KDL payload the shell sends to the host, and the pipe names. |

`engine`, `policy`, and `wire` use no `zellij-tile` type, so they compile and unit-test on the host target in the ordinary workspace test run. `zellij-tile` is a wasm-only dependency, because its shims call extern functions that exist only inside Zellij's plugin host.

The engine returns effects instead of performing them: `RunCommand`, `HideSelf`, `Reconfigure`, `TogglePaneFullscreen`, `CloseSelf`, `Unsubscribe`, `Resubscribe`, `SetTimeout`, `ListClients`. Every decision is a pure function from event to effect list, and the shell only projects. Inside the engine one canonical pane map is the source of truth: reducers retain partial manifests, patch event enrichment in place, and publish panes in deterministic tab and key order.

### What it publishes

The plugin subscribes to ten Zellij events: `PaneUpdate`, `TabUpdate`, `CommandChanged`, `CwdChanged`, `PaneClosed`, `Timer`, `PermissionRequestResult`, `RunCommandResult`, `SessionUpdate`, and `ListClients`.

Everything it publishes goes one way, as a fire-and-forget `run_command` fork of `rimz sidebar wake --reason <reason>` (`wire::WakeRequest`):

| Reason | Meaning |
| --- | --- |
| `panes-changed` | An announced snapshot: a room change worth an event broadcast. |
| `alive` | A silent snapshot for keepalives and explicit dumps: refresh the cache without broadcasting. Carries plugin telemetry. |
| `switch-settled` | The generation-bearing client observation after a tab switch settles, with the active tab. |

Each wake after the first manifest carries the live roster as repeated `--topology` values, at most 64 KiB each and omitted entirely above 1 MiB, while stamp and telemetry delivery continue. `rimz sidebar wake` concatenates the chunks in order and normalizes the payload. The topology payload includes the raw attached-client observations as `clients: [{ client_id, pane_id }]`, from which the host derives attached-client count, terminal views, and unique-live focus. A payload without `clients` falls back to its `focused_pane` field and keeps the producer-side `client_view` fallback active.

The first manifest after load names every pre-existing pane, so the host accepts it as a baseline, and an announced baseline emits only one topology nudge.

Five named pipes reach the plugin from the host:

| Pipe | Effect |
| --- | --- |
| `rimz:dump_topology` | Publish one immediate `alive` wake, bypassing the poke floor. Revives and resubscribes a muted same-id clone for that publish. |
| `rimz:focus_sidebar` | Fork `rimz sidebar focus --toggle`; the focus keybind sends this. |
| `rimz:zoom_pane` | Fork `rimz pane zoom`; the zoom keybind sends this. |
| `rimz:toggle_fullscreen` | Toggle fullscreen on the host-selected pane id (`ZellijBackend::toggle_fullscreen`). |
| `rimz:retire` | Retire this instance if the payload's writer identity outranks it ([retirement](#retirement)). |

### Poke discipline

Unthrottled, a busy room would fork `rimz` on every keystroke-driven event. [`policy.rs`](../../crates/rimz-presence-zellij/src/policy.rs) holds the timing:

| Rule | Value | Purpose |
| --- | --- | --- |
| Immediate first poke | 0 ms | The first change after quiet is never delayed. |
| `POKE_FLOOR_MS` | 100 ms | Duplicates inside the window collapse into one. |
| `SETTLE_POKE_MS` | 250 ms | Each accepted change schedules one, so a command change cannot strand the pre-change command. |
| `FOCUS_SETTLE_MS` | 250 ms | How long a tab switch waits before sampling clients. |
| `KEEPALIVE_MS` | 60 s | Keeps the presence stamp fresh while idle and requests a client-list self-heal. |

Title-only events are filtered out.

Client sampling has its own coordinator. Every `PaneUpdate` queues a coalesced general client query before topology deduplication, so an upstream update that changes only focus still refreshes attached-client truth. One untagged `ListClients` request is in flight at a time: the coordinator keeps the newest general or switch-settled purpose, expires a missing reply at the keepalive deadline, treats a reply after expiry as a general sample, and re-arms the superseded purpose.

Every host fork runs from `/`, so the session-lifetime plugin does not depend on the cwd of the CLI that loaded it.

### Loading and permissions

RimZ loads the plugin itself, never through the user's `config.kdl`, because a layout cannot load plugins.

The load verb is the idempotent `zellij … action pipe --plugin --skip-plugin-cache`, the one verb in Zellij 0.44 that works on a clientless session and carries the cache-bypass bit. Only owner flows use it: room birth, and `rimz reload` upgrade and repair. Generic pane and topology readers broadcast the name-only `rimz:dump_topology` pipe instead and never launch a plugin.

Load-time configuration pins the workspace, the session, the room's `rimz` pointer (`ws/<workspace-dir>/rimz`), runtime mouse options, the focus and zoom chords, `launch_scope=background`, the embedded-wasm digest (computed once, lazily), and a hash of the configuration itself. Every desired identity is created only through this pipe, so an identity-matching writer is a background instance and receives global pane and tab updates. Changing an identity launches another background writer; the host accepts its proof and retires the old identity.

The canonical artifact path is the same across upgrades, but Zellij keys its compiled-module cache by that path, not the wasm bytes. Every plugin-addressed pipe therefore skips the cache: a live identity treats the flag as a no-op, and a missing identity compiles the bytes currently installed at the path.

RimZ seeds Zellij's `permissions.kdl` cache for its embedded plugin, so the first attach shows no prompt, even in a clientless session:

| Permission | What it grants |
| --- | --- |
| `ReadApplicationState` | The pane, tab, session, and client manifests. |
| `RunCommands` | The `rimz sidebar wake`, `rimz sidebar focus`, and `rimz pane zoom` forks. |
| `Reconfigure` | Runtime mouse options and the optional focus and zoom keybinds, applied without writing `config.kdl`. |
| `ChangeApplicationState` | The fullscreen toggle for the host-selected pane id. |

The artifact path is canonicalized because Zellij keys the grant on the exact string. The security boundary is in [security.md](../guide/security.md#the-zellij-presence-plugin).

A Zellij room requires Zellij 0.44.2 or newer and a loadable plugin. An older host, a missing artifact, or a denied permission fails the Zellij backend's precondition, and `rimz doctor` names the first failing fix plus tmux as the alternative.

### Build identity and embedding

Every RimZ build embeds the plugin. Release binaries embed a fresh `cargo xtask build-plugin` artifact; the crates.io crate embeds the vendored wasm in `crates/rimz/presence/`. `cargo xtask plugin-refresh` builds that artifact with canonical path remaps (registry mirror cache keys, and local standard-library sources mapped back onto the `/rustc/<commit-hash>` root the toolchain emits without `rust-src`), bypasses compiler wrappers, and commits provenance beside it: the source-tree digest, the wasm digest, and the producing rustc version. Repository invariants bind both digests to the current tree and blob, every vendored embed verifies the wasm digest, and `cargo xtask checks` rebuilds with the recorded toolchain and requires byte-for-byte equality.

The wasm digest is the plugin's build identity, so each build is a distinct Zellij plugin identity, which lets an owner flow upgrade a clientless session.

The workspace record carries the staged `rimz_bin` and its `rimz_build` digest as one verified room target. Only the room owner claim and reload update the pair: `rimz start`, cwd-based `rimz attach`, and `rimz reload`. Attach by session name keeps the recorded owner, and other CLI re-records keep both values. Because the plugin configuration names the stable `rimz` pointer, a worktree build that asks for topology leaves the configuration string unchanged.

Owner flows materialize or refresh the shared embedded wasm artifact. Read-only topology refreshes use only an existing artifact or the development fallback beside the executable. The shared artifact therefore tracks the last owner build, and another session can run those bytes until its own owner refreshes them; writer fencing makes that harmless.

### Writer fencing

Zellij 0.44 runs one wasm instance per connected client and can keep instances for departed clients, so one plugin id can have both the current clone and an older same-id clone. Overlapping writers are normal, and the host arbitrates.

Every topology payload carries its plugin build and configuration plus the fallback generation `(loaded_at_ms, plugin_id)`. Owner launches atomically publish the desired identity in `presence-desired.json`.

[`sidebar::presence`](../../crates/rimz/src/sidebar/presence.rs) holds the state `locks/topology-writer.lock` for at most one second across the desired-record and cache reads, the rank comparison, cache replacement, and conflict update. A lock or write failure rejects the wake; nothing falls back to an unlocked write.

A writer matching both desired fields outranks every non-matching writer; load time, then plugin id, break ties. Without a desired record, ranking is generation order, and a payload without writer identity ranks at generation zero.

The gate accepts a wake when the cache is absent, the same-session cache is stale, or the incoming rank is at least the cached rank. A sole non-matching writer therefore keeps refreshing its own cache, and a desired writer wins any overlap.

An accepted write commits before writer-change diagnostics, conflict clearing, presence stamps, telemetry, and event broadcast. A rejected wake skips all of them: no presence stamp, no plugin-presence sample, no topology write, no sidebar event. A rejection updates `topology-writer-conflict.json` under the same lock and emits a rate-limited `topology_write_rejected` diagnostic. The reject count restarts whenever either writer changes, while the rate limit spans incidents. An accepted writer with a strictly higher rank removes the superseded conflict file, and doctor ignores an orphaned conflict file once the live cache carries a newer generation. An accepted writer change emits `topology_writer_changed`.

The rejected plugin learns about it too. A rejected publish exits with the stale-writer status 73, and three consecutive rejections retire the losing plugin in place by muting it and unsubscribing. It mutes instead of calling `close_self()`, which would unload every clone sharing the plugin id, the current one included. Only a successful topology publish resets the streak.

### Retirement

`rimz reload` touches the plugin only when it must. It reads the fresh topology cache, and when the writer echoes a `build` equal to the embedded-wasm digest and a `config` equal to the desired configuration hash, it confirms the live plugin roster contains only that writer id before counting the session current. Extra or missing ids run the retire-and-sweep path without reloading the accepted writer, and a failed live listing falls back to full convergence. An identity mismatch, a missing field, or a stale cache also converges and reports the upgrade.

Retirement requires proof that the replacement is alive. Reload waits for topology from the expected build and configuration at or after the flow's freshness floor, while session birth uses any matching proof the boot pipe already published instead of adding a startup wait. The retire broadcast carries that writer identity as JSON, and each instance decides for itself:

| Instance | Response to a retire broadcast |
| --- | --- |
| Different build or configuration | `CloseSelf`, regardless of load time |
| Same identity, outranks the payload generation | Ignore |
| Same identity, different plugin id, outranked | `CloseSelf` |
| Same identity, same plugin id, outranked | Mute and unsubscribe, revivable through `rimz:dump_topology` |

After the broadcast, the host lists every pane under a bounded deadline and closes each `rimz-presence-zellij` plugin id except the accepted writer's. That unloads later old-wasm instances and command-path zombies while keeping every same-id clone of the accepted writer. RimZ then sends the boot pipe again, which restores a writer if a retire closed the whole plugin id. A failed or timed-out listing leaves only the cooperative retire, with a manual session restart as the fallback. When a detached or degraded session cannot prove the replacement is alive, RimZ skips retirement and retries on a later owner flow.

`rimz reload` without `--repair` nudges its sidebars, which converge worker-first from the durable workspace record, and touches the plugin only when its echoed identity no longer matches the running build; it never changes pane structure ([sidebar.md → reload and repair](./sidebar/sidebar.md#reload-and-repair)). `rimz reload --repair` ensures the plugin first and requires a post-ensure topology publication before any topology-dependent work. A Zellij session that misses the bounded health proof reports no live presence channel and skips repair, while runtime cleanup still runs.

### Telemetry and failure reporting

The plugin subscribes to `RunCommandResult` and drains the reply to every command fork. Each reply carries the host's exit code and stderr, and the plugin retains the newest failure as an exit code, the first non-empty stderr line (at most 200 bytes), and a timestamp. That retained failure is how a wake's cause reaches `rimz doctor`.

`fold_failure` decides what the retained failure holds:

| Outcome | Effect on the retained failure |
| --- | --- |
| Topology or other failure | Replaces it, stamped with the time |
| Stale-writer rejection | None; that exit is the fence working, and recording it would hide the failure being investigated |
| Success | None, so the evidence outlives the recovery |

Success does not clear the record because wakes run far more often than telemetry is sampled; clearing would drop an intermittent failure's cause before any sample carried it. The host instead takes the cause from the window its counters measure and drops a stamp older than the window's first sample. The stamp is optional on the wire, and a failure without one stays usable instead of being dated to the epoch.

Three consecutive fork failures clear the configured `rimz_bin` and retry one `alive` wake through `rimz` on PATH; a successful fork resets the count.

The keepalive carries WASM memory pages, uptime, per-bucket command counts (completed, succeeded, stale-writer rejections, topology failures, other failures), the retained failure, and the Zellij version into the rotating `audit/plugin-presence.log.jsonl`. That file is the leak-investigation surface, because it separates plugin linear-memory growth from Zellij-native RSS growth.

## tmux backend

### The managed server endpoint

RimZ owns one tmux server per runtime domain, at `<runtime-root>/rimz/tmux/server`, holding one session per workspace, named after its state directory at birth. Every managed command runs `tmux -S <socket> …` through the single [`TmuxBackend::cmd`](../../crates/rimz/src/mux/tmux.rs) chokepoint. The socket is always set, so no command can reach the user's default server, and `cargo xtask invariants` rejects a bare `tmux` argv.

The endpoint derives from the resolved runtime root alone, so any caller reconstructs it without a workspace or `disk::paths::RuntimePaths` argument. Attach, ttyd, presence, pane I/O, list, reload, GC, sidebar, and doctor all address the same path. A disposable `XDG_RUNTIME_DIR` yields a different socket and a private server, which is what isolates sandboxes and tests.

Each managed session is stamped with the concrete `HOME`, `RIMZ_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`, and `XDG_RUNTIME_DIR` of the resolved domain, at birth and on every ensure, so a pane resolves the same store and endpoint as the client that created it. Socket identity and stamped environment are two projections of one runtime domain, derived together in [`disk::paths`](../../crates/rimz/src/disk/paths.rs).

Routing stays explicit. The user's default server serves only recordless external sessions and the exact endpoint a process inherits through `$TMUX`; managed commands clear `$TMUX` so an ambient session cannot capture them. `ProcessDomain` resolves a process's endpoint from `$TMUX` when present and from the managed socket otherwise, so the orphan sweep recognizes managed processes and spares the user's own server.

Before birth or attach, a read-only `has-session` probe of the default socket reports a same-named session left there by a RimZ release that used the default server, with the command that retires it (`tmux -S <default-socket> kill-session -t <session>`). `has-session` cannot start a server, so the probe never starts a default daemon, and unrelated sessions stay untouched.

Workspace reset and cleanup use `kill-session`. `kill-server` is reserved for explicit recovery of every RimZ tmux room at once, and is safe to suggest because it is scoped to the RimZ socket. Server-global options and root key bindings are shared across RimZ workspaces on that server and die with the last session, so `ensure_session` re-asserts them on every ensure.

### Every managed client runs from `/`

A tmux server inherits its working directory from the client that births it, and tmux's `spawn.c` applies a pane's `chdir(cwd)` only while `getcwd()` succeeds on the server. A server born in a directory that is later deleted (a disposable worktree, a swept tempdir) places every later pane in that deleted directory, even when RimZ passes an absolute `-c`.

`/` cannot be deleted or unmounted, so birth and rebirth always start from a readable directory. The Zellij plugin forks from `/` for the same reason.

Session birth checks the property: it reads the birth pane's `pane_current_path` back, and on a mismatch fails with `MuxErr::ServerCwdUnusable`, whose message gives the socket-scoped `tmux -S <socket> kill-server` fix. Reading the pane back tests what matters and works on every host; the daemon's own working directory is only a proxy and cannot be read on some platforms.

### Room options

`ensure_session` applies the per-machine `[tmux]` room options in one batched client call, and the `after-new-window` hook replays window options before docking the sidebar, so later windows match the birth window. Session and window options are scoped to the RimZ session; server-scoped options (clipboard, rich-key handling, focus events) are global to the RimZ server because tmux has no per-session equivalent.

The batch also does what the option list alone cannot express:

- writes `*:sync` at the fixed `terminal-features[240]` index for atomic redraws in pixel pets and TUIs, removes exact `*:sync` and `*:extkeys` entries found at other indices, and collapses an exact `*:hyperlinks` entry inherited from the user's config,
- writes `*:extkeys` at `terminal-features[241]` when `extended_keys` is on and unsets that index when it is off,
- writes `*:hyperlinks` at `terminal-features[242]` so OSC 8 links from the sidebar and panes stay clickable,
- when `extended_keys` is on (the default), registers root-table `S-Enter` and `M-Enter` bindings that send the configured modified-Enter sequence, so agents receive soft newlines without requesting modifyOtherKeys, and names `ESC[27u` as `user-keys[240]` bound to Escape, because tmux passes that modifier-less form into panes verbatim.

On tmux 3.5.x, extended-key mode breaks clean multiline clipboard paste; tmux 3.6 preserves paste bytes while modified keys still reach agents as CSI-u. Per-option semantics and RimZ's values are in the [reference → options](../externals/mux-adapter/tmux-reference.md#options); the config model is in [configuration.md](../guide/configuration.md#multiplexer-room-options).

When RimZ owns `pane-border-status`, it also writes a `pane-border-format` that fills the `rimz-sidebar` pane's border row with spaces, so work panes carry titled frames while the sidebar reads frameless, the tmux counterpart of Zellij's borderless sidebar. tmux borders are separators plus an optional top or bottom status row, and tmux does not draw the outer window edge, so a closed four-edge pane frame exists only on Zellij.

### Attach and the terminal

An attach launched by RimZ disables alternate scroll on the outer terminal and restores the saved prior mode when the client exits. tmux disables outer mouse reporting in `tty_start_tty` before its first repaint restores the requested mouse mode, and a terminal with alternate scroll enabled turns wheel ticks in that gap into arrow keys. The CLI brackets the client process with XTSAVE and XTRESTORE, because a tmux `client-detached` hook runs after the departed client's tty name is cleared. Ghostty 1.3.1 implements those operations for mode 1007, and a terminal that ignores them is left with alternate scroll off.

Terminal mode save slots are not a stack, so a one-shot SSH attach brackets only the local terminal and marks the remote launch as already bracketed through an environment variable; an older remote ignores the marker and does not bracket. Reconnect supervision has no local bracket, and the remote RimZ owns it. Zellij attaches go through the same lifecycle.

The waiting RimZ parent mirrors a client's `SIGTSTP` stop and resumes the child with its foreground job, keeping tmux's `prefix` + `C-z` suspend. Child exit codes pass through unchanged after terminal restoration.

### The sidebar and the `after-new-window` hook

tmux has no tab template, so a hook provides it. `open_sidebar` splits a left sidebar into the initial window at the launch seed and installs a session-scoped `after-new-window` hook that repeats the split in every later window.

The hook reads an absolute-column session option initialized from the resolved room share. Keypresses, adopted mouse drags, view changes, and reconcile passes refresh it, so a new window starts at the share rendered for the current view.

Two details keep the first prompt clean. Both are tmux-only, because Zellij births terminals from the layout at their final size:

- A plain default-shell window has an empty `pane_start_command`, so after the hook split sets the final width, the hook respawns only that work pane as the user's shell, which avoids zsh's `PROMPT_SP` end-of-line marker.
- Pristine birth installs a one-shot `client-attached` hook for the first work shell. A detached session can draw zsh's first prompt before the attaching client applies its size, and a resize during that draw leaves the `PROMPT_SP` `%` marker above the prompt. The hook skips control-mode clients, respawns the birth work pane after the first real attach, then removes itself. A room born without a probed terminal is fixed when the first later attach normalizes its detached geometry and records `default-size`.

A quick tmux kill-and-restart still takes the pristine birth path: once the room transition proves the session absent, it purges sidebar heartbeat files and clears the prior width target before creating the replacement. A fresh heartbeat from a dead sidebar therefore cannot send the new shell through the reconcile split that leaves the `%` marker.

Reconcile converges widths only against an attached, sized client's geometry or, while detached, the attaching terminal's probe. The detached path first aligns the session `default-size` and every window to that probe so the attach keeps the geometry; a daemon reload with neither basis re-asserts structure without changing panes or the recorded width. When `open_tab` temporarily expands a new window to the widest attached client, it re-asserts the sidebar at the live target before splitting agent columns, then restores tmux autosizing. Layouts compile to tmux command sequences from the same layout IR Zellij uses: every column is born first as a full-height horizontal split chained off the last column created, then each column's rows as vertical splits inside it. Both runs pass absolute cell sizes, so every rectangle is even at creation and no resize pass follows; splitting rows first would leave each later column nested inside the first column's top row.

The pane itself is best-effort: a fresh sidebar heartbeat suppresses a relaunch, while a missing, stale, unreadable, or protocol-mismatched heartbeat lets `rimz start` or `attach` open a new pane. For supervised agent panes the producer derives the wrapper spawn command from the process backend, as it derives `pane_process_start`, so lazily registering agents bind and panes group by worktree as they do on Zellij.

### The control-mode presence watch

The elected producer holds one control-mode client, [`PresenceWatch`](../../crates/rimz/src/mux/tmux/presence.rs), started from `sidebar_pane/app/tmux_watch.rs`, with a single `refresh-client -B` subscription.

[`TmuxPresenceState`](../../crates/rimz/src/sidebar/presence/tmux.rs) keeps only the stream state needed to normalize out-of-order lines (panes, current windows, pending inactive panes, floating status, seeding) and feeds pane observations, focus, view switches, and incomplete-layout nudges to the shared projector. Which notification becomes which event is in [state.md → what triggers a mux-derived event](./sidebar/state.md#what-triggers-a-mux-derived-event). Each overlay reaches every fresh sidebar immediately, the producer verifies structural changes with a fresh frame, and the watch refreshes the presence stamp on attach and on each classified line, which puts tmux on the same event-mode pane TTL as Zellij while the stream is alive.

The watch follows the control-mode contracts in the [reference → control mode](../externals/mux-adapter/tmux-reference.md#control-mode). It attaches with `-C` and `ignore-size,no-output`, holds stdin open because closing the pipe detaches the client, sends only `refresh-client -B`, drains notifications promptly because tmux force-exits a slow reader, and removes `$TMUX` from the child environment so tmux does not refuse a nested attach. The client is writable, which keeps tmux 3.7 `send-keys` working when the watch is a headless session's only attached client.

From tmux 3.7 a window's layout string repeats each floating pane, once inside the tiled tree and once in a trailing `<...>` suffix ([reference → layout strings](../externals/mux-adapter/tmux-reference.md#layout-strings)). The reader drops the suffix's ids and reports the tiled panes, so a window holding a floating pane keeps typed layout changes; the floating panes themselves arrive through the subscription. A zoomed window writes no suffix, so its floating panes still appear as leaves there; the consumer already skips panes it knows are floating.

A dead, refused, or idle watch degrades to the tmux poll, and the producer respawns it with backoff.

### Notification passthrough

Desktop notifications are terminal-local. The sidebar renderer writes OSC 777 and BEL bytes into its pane, SSH carries them to the local terminal, and the terminal decides whether to show a banner or play a sound.

tmux forwards OSC 777 wrapped as DCS passthrough when `allow-passthrough` is on, which is RimZ's default. At startup the sidebar raises its own pane's passthrough from `on` to `all` (`escalate_own_pane_passthrough` in `sidebar_pane/pixel/probe.rs`) so kitty graphics reach the terminal while its window is hidden ([pets.md → render tiers](./sidebar/pets.md#render-tiers)); DCS-wrapped notification bytes pass under the same setting.

Zellij drops notification OSCs, so `[notifications].desktop = "auto"` disables OSC there and notification handlers are the portable channel. The full contract is in [notifications.md](./sidebar/notifications.md).

### tmux backend caveats

- **Minimum version is 3.5.0** (`MIN_TMUX_VERSION`), set by the room options `ensure_session` applies (`extended-keys-format` arrived in 3.5); the batched sequence fails at the first unknown option. The command surface alone needs only 3.2. `rimz doctor` reports floor compliance.
- **A server-less `list_sessions` is empty, not an error.** tmux exits non-zero with `no server running` before the server starts; the backend returns an empty `Vec`, matching Zellij.
- **A server exits when its last session ends.** The endpoint is a stable path, not a long-lived daemon, so a leftover socket file is normal and the next command births a replacement. Pane and window ids restart from `%0` and `@0` across that boundary ([pane metadata](#pane-metadata)).
- **The sidebar self-closes** as it does on Zellij, through the normalized pane listing, so a lone sidebar removes itself when its window's last working pane exits. The `after-new-window` hook runs `split-window` and, for plain default-shell windows, `respawn-pane`, so it never recurses through `new-window`.
- **Resumed agents open as windows**, one `new-window` per remembered channel, named `#<channel>` and born `sidebar | agents…` as the hook docks the sidebar. `new-window -n` turns off automatic-rename, so the name is a stable idempotency key and a re-run never doubles a channel.
- **Tests isolate servers per test.** Tests point the backend at a private runtime root, so `with_socket` receives the same derived path production would use one domain over. A test that pairs a live server with `rimz` subprocesses builds both from one runtime root, because the subprocess resolves its own endpoint instead of inheriting `$TMUX`.

## What both backends guarantee

- **Detach and reattach are multiplexer features.** RimZ does not reimplement them.
- **Runtime correctness needs no visible sidebar.** Hooks, `rimz message`, and supervised runs work headless.
- **The renderer is optional.** The native pane is the default on both backends, and correctness never depends on which renderer, or none, is attached.
- **The store survives host restart; processes do not**, unless a host supervisor is wired (tmux-resurrect, Zellij resurrect, systemd).

### What `rimz doctor` reports

The selected backend, versions and floor compliance, PATH-visible backend binaries, backend server-log issues, feature availability, sidebar liveness, RimZ runtime socket headroom, the managed tmux server socket path, any same-named session left on the default tmux server with the command that retires it, Zellij IPC socket headroom when Zellij is selected, Zellij 0.45's live kitty-graphics handshake, and any degraded modes.

Backend adapters own server-log locations. Doctor's collector in [`mux_log.rs`](../../crates/rimz/src/cli/doctor/mux_log.rs) reads a bounded tail, assembles multi-line records, stamps each from the backend's line format, and drops everything at or before the `rimz doctor --clear` watermark. Its classifier names each issue: ordinary lifecycle records (a client leaving, a closed pane's pty, a late action acknowledgement) are `expected` and fold into counted lines, and everything else is `investigate`. A record wrapped in a generic header is named by its `Caused by:` chain, so two unrelated failures under one wrapper stay two issues.

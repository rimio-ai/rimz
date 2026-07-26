# Multiplexers

RimZ does not implement a terminal. Zellij and tmux already solved persistent sessions, pseudo-terminals, and pane geometry, so RimZ drives one of them and keeps its own job narrow: durable workspace state, the agent model, and the sidebar on top. `crates/rimz/src/mux/` is that seam. Beside it sits one wasm plugin, `crates/rimz-presence-zellij/`, because Zellij has no other way to push events at a host process.

Both backends are first class: same store, same CLI, same sidebar model, one test matrix. A feature that lands here lands twice.

This page is the RimZ side of the seam. The upstream surfaces themselves (every Zellij option, every tmux format variable) are catalogued in the externals mirrors, [zellij-reference.md](../externals/mux-adapter/zellij-reference.md) and [tmux-reference.md](../externals/mux-adapter/tmux-reference.md). What a user configures is [configuration → multiplexer room options](../guide/configuration.md#multiplexer-room-options) and the [Zellij and tmux guide](../guide/multiplexer.md).

## The division of labour

| The multiplexer owns | RimZ owns |
| --- | --- |
| Panes, tabs and windows, geometry | The durable store and its event log |
| The pseudo-terminal behind every pane | The agent model, messages, and runs |
| Attach, detach, scrollback, copy mode | Run and wakeup sockets, the trust gate, agent hooks |
| Session resurrection, where it exists | Rebirth: which agents come back and how |

Three rules follow from that split, and they are the ones to internalize before reading any code in this module.

**Parity is the rule; fast paths are the exception.** A backend-only capability is always a latency hint layered over shared truth. The Zellij presence plugin and the tmux control-mode watch both push topology, and both are optional: with either channel dead, its backend falls back to polling and passes the same test matrix. Correctness never reads from a push channel.

**Cross-backend policy stays pure and above the backends.** [`reconcile.rs`](../../crates/rimz/src/mux/reconcile.rs) owns the one-sidebar-per-view planner and its accounting, [`width.rs`](../../crates/rimz/src/mux/width.rs) owns sizing arithmetic, and [`sidebar/presence/projector.rs`](../../crates/rimz/src/sidebar/presence/projector.rs) owns the event taxonomy. Each backend collects native facts and executes native effects; neither decides policy. These modules unit-test with no multiplexer installed.

**Backends stay ignorant of agents.** The CLI hands `open_tab` backend-neutral pane argv and layout geometry. Agent resolution, prompts, and worktree cleanup are already compiled into that argv (`rimz agents exec …`), so no backend knows what an agent kind or a worktree is. The layout IR is in [harness.md](./harness/harness.md#the-layout-ir); worktree cleanup is in [worktrees.md](./harness/worktrees.md#who-triggers-removal).

## Module map

Shared seam, `crates/rimz/src/mux/`:

| Path | Owns |
| --- | --- |
| [`mod.rs`](../../crates/rimz/src/mux/mod.rs) | The `MuxBackend` trait, its option and result types, `MuxErr`, and the one env→`PaneId` mapping. |
| [`selection.rs`](../../crates/rimz/src/mux/selection.rs) | Backend selection precedence. |
| [`command.rs`](../../crates/rimz/src/mux/command.rs) | `CommandSpec`: the bounded subprocess engine every control command runs through. |
| [`reconcile.rs`](../../crates/rimz/src/mux/reconcile.rs) | The sidebar repair planner and its transaction executor. |
| [`width.rs`](../../crates/rimz/src/mux/width.rs) | Sidebar sizing: share resolution, backend rungs, and target spellings. |
| [`recovery.rs`](../../crates/rimz/src/mux/recovery.rs) | Destructive teardown shared by `rimz reset` and attended auto-reset. |
| [`domain.rs`](../../crates/rimz/src/mux/domain.rs) | `ProcessDomain`: the guard every heuristic process kill passes. |
| [`focus_key.rs`](../../crates/rimz/src/mux/focus_key.rs) | Parsing the `[sidebar] focus_key` chord both backends bind. |
| [`keys.rs`](../../crates/rimz/src/mux/keys.rs) | Named key presses and bracketed-paste markers. |
| [`capabilities.rs`](../../crates/rimz/src/mux/capabilities.rs) | Static backend facts, such as whether a view is a tab or a window. |
| [`binaries.rs`](../../crates/rimz/src/mux/binaries.rs) | PATH and live-server binary probes for `rimz doctor`. |
| [`logtail.rs`](../../crates/rimz/src/mux/logtail.rs) | The bounded server-log scanner behind `rimz doctor`. |

Zellij, `crates/rimz/src/mux/zellij/` plus [`zellij.rs`](../../crates/rimz/src/mux/zellij.rs):

| Path | Owns |
| --- | --- |
| [`backend.rs`](../../crates/rimz/src/mux/zellij/backend.rs) | The `MuxBackend` implementation. |
| [`layout.rs`](../../crates/rimz/src/mux/zellij/layout.rs) | KDL layout rendering: birth, daemon view, resumed agents, background tabs. Pure `&options → String`. |
| [`sidebar.rs`](../../crates/rimz/src/mux/zellij/sidebar.rs) | Sidebar birth, in-place recovery, and geometry convergence. |
| [`presence.rs`](../../crates/rimz/src/mux/zellij/presence.rs) | Plugin materialization, identity, load and retire pipes. |
| [`pane_topology.rs`](../../crates/rimz/src/mux/zellij/pane_topology.rs) | The topology cache the plugin publishes. |
| [`raw_pane.rs`](../../crates/rimz/src/mux/zellij/raw_pane.rs) | Topology projection and sidebar classification. |
| [`session.rs`](../../crates/rimz/src/mux/zellij/session.rs) | Session discovery and topology-cache reads. |
| [`socket.rs`](../../crates/rimz/src/mux/zellij/socket.rs) | IPC socket path budgeting, which is tight on macOS. |
| [`reap.rs`](../../crates/rimz/src/mux/zellij/reap.rs) | Pre-attach retirement of orphaned clients from one remote lineage. |
| [`parse.rs`](../../crates/rimz/src/mux/zellij/parse.rs), [`pane_pid.rs`](../../crates/rimz/src/mux/zellij/pane_pid.rs) | Command-output parsing helpers. |

tmux, `crates/rimz/src/mux/tmux/` plus [`tmux.rs`](../../crates/rimz/src/mux/tmux.rs):

| Path | Owns |
| --- | --- |
| [`backend.rs`](../../crates/rimz/src/mux/tmux/backend.rs) | The `MuxBackend` implementation. |
| [`window.rs`](../../crates/rimz/src/mux/tmux/window.rs) | Window, pane, and tab-layout command helpers. |
| [`options.rs`](../../crates/rimz/src/mux/tmux/options.rs) | Room options, key bindings, hooks, sidebar-pane classification. |
| [`presence.rs`](../../crates/rimz/src/mux/tmux/presence.rs) | The control-mode presence watch. |
| [`parse.rs`](../../crates/rimz/src/mux/tmux/parse.rs) | Command-output parsers. |

The presence plugin is its own crate; see [the Zellij presence plugin](#the-zellij-presence-plugin).

Two modules outside `mux/` complete the picture. [`room/`](../../crates/rimz/src/room/mod.rs) sits above the trait and owns managed identity, birth and reset, the pre-attach health gate, and presence-load ordering. [`sidebar/presence/`](../../crates/rimz/src/sidebar/presence/projector.rs) sits beside it and turns normalized transitions from either backend into typed `SidebarEvent`s.

## Choosing a backend

[`auto_detect_backend`](../../crates/rimz/src/mux/selection.rs) picks one, first match wins:

1. the `--mux <name>` flag,
2. the active environment (`ZELLIJ` / `ZELLIJ_PANE_ID`, then `TMUX` / `TMUX_PANE`),
3. `[mux] default` from per-machine config, which fails fast when it names an uninstalled backend,
4. the installed binary, tmux preferred when both are present.

The flag and the environment short-circuit before config is even loaded, so a command run inside a live room always addresses that room.

Selection is stable across worktrees: every worktree of one repository resolves to the same session on the same backend. Room identity is path-derived and shared across backends, which means a rival session under the same derived name on the other backend would share the store while its panes stayed unreachable. Four commands guard that: `rimz start`, `rimz reset`, `rimz attach`, and `rimz web` resolve through [`pick_mux_for_session`](../../crates/rimz/src/room/session.rs), so an auto-selected launch lands on the backend that already owns the live room. Start attaches to it; reset tears it down there before rebirthing on the resolved default; web passes the resolved session to the shared ttyd daemon, whose validated shim attaches through its owning backend. [`ensure_single_backend_room`](../../crates/rimz/src/room/session.rs) guards birth itself, refusing an explicit `--mux` that names a backend other than the live room's owner.

## The `MuxBackend` trait

[`MuxBackend`](../../crates/rimz/src/mux/mod.rs) is the whole seam: every Zellij or tmux command in RimZ lives behind one of its methods. They group into seven jobs.

| Group | Methods | Notes |
| --- | --- | --- |
| Session lifecycle | `ensure_session`, `attach_command`, `detach`, `kill_session`, `list_sessions`, `session_liveness`, `version` | `attach_command` hands a `CommandSpec` to the CLI attach runner rather than running it. |
| Pane inventory | `list_panes`, `cached_pane_roster`, `client_view` | See [reading the room](#reading-the-room). |
| Pane I/O | `capture_pane`, `send_keys`, `send_key`, `paste_text` | `paste_text` wraps one bracketed paste; the submit Enter follows separately as a keystroke. |
| Structure | `split_pane`, `open_tab`, `open_sidebar`, `open_background_view`, `close_pane`, `close_view_floating_panes` | Callers pass backend-neutral argv and layout geometry. |
| Focus and geometry | `focus_pane`, `sidebar_width_step`, `nudge_sidebar_width`, `record_sidebar_width_default`, `register_focus_key` | |
| Health | `probe_session_health`, `ensure_clean_session`, `reconcile_sidebars`, `purge_resurrection_cache`, `resurrection_cache_paths`, `session_accepts_agent_close` | Several default to a no-op because they answer a Zellij-only question. |
| Presence | `ensure_presence_plugin` | Zellij-only; tmux inherits the no-op default because its control-mode watch already pushes. |

Methods with a sensible cross-backend answer carry a default implementation, so a backend implements only what it does differently. `ensure_clean_session` and `purge_resurrection_cache` exist because Zellij resurrects sessions and tmux does not; tmux takes the no-op and the calling code stays branch-free.

Everything correctness-critical stays above the trait and is byte-identical across backends: the store, the run and wakeup sockets, the event schema, the trust gate, and the agent hooks.

### Command discipline

Every control command runs through [`CommandSpec`](../../crates/rimz/src/mux/command.rs) under a deadline. On the bound the child is SIGKILLed and the caller gets `MuxErr::Timeout`.

| Bound | Value | Why |
| --- | --- | --- |
| `COMMAND_TIMEOUT` | 30s | A wedged `zellij action` busy-loops at 100% CPU when its server dies, and would otherwise hang the caller forever. |
| `LIST_SESSIONS_TIMEOUT` | 3s | A read-only local query on hot paths. |
| Start-path session probe | 1s | A timeout here is treated as definitive rather than retried against a wedged server, and prints a console note. |

A healthy command answers in milliseconds, and callers treat mux commands as best-effort, so the bound degrades rather than blocks. When the selected backend is unresponsive at start, RimZ refuses the start with recovery steps; rival-backend and notice probes skip their enrichment and continue.

## Identity

### Pane and view IDs

Raw IDs stay inside the backend adapter, where the native command expects them (Zellij `terminal_3`, `plugin_1`; tmux `%3`). Everywhere else they travel normalized as `zellij:<raw>` or `tmux:<raw>`, through env vars (`RIMZ_PANE_ID`), store events, snapshots, and CLI arguments. [`pane_from_env_value`](../../crates/rimz/src/mux/mod.rs) is the one env→ID mapping, and `ensure_pane_backend` rejects a pane addressed to the wrong backend before any command runs.

`ViewId` names the view holding a pane (a Zellij tab, a tmux window) by the identity RimZ observes on every fast path: Zellij tab position (`tab_1`), tmux window id (`@3`). `PaneRef.view_id` is the flat read-side seam, and the producer lifts it into `TabFrame.view_id` so sidebar-per-view bookkeeping runs over typed topology.

**A view id is never the view's on-screen label.** Zellij's default tab names are themselves number-shaped (`Tab #16`), so matching a positional `tab_15` against a tab named "Tab #15" joins two unrelated id spaces and lands on the wrong tab in any session that has closed one. The label is sticky, minted at tab creation; `PaneRef.view_name` carries it for display only. Resolve "which view holds this pane?" through the pane id.

### The identity pin

Session birth stamps the room's identity (`RIMZ_WORKSPACE_ID`, `RIMZ_PROJECT_ROOT`, via [`pin_env`](../../crates/rimz/src/workspace.rs)) and the registry's opaque adapter-enrichment environment into the session. Every pane inherits it, and so does every agent and every in-pane hook child. A daemon-routed hook that misses the pin recovers it from the in-pane agent process ([adapter.md](./agents/adapter.md#hooks-resolve-the-room-they-live-in)).

Each backend pins at its own birth seam, and they differ in one way worth knowing.

tmux sets identity and adapter env on `new-session` and re-asserts them idempotently on every ensure, so panes born later carry them. The same seam stamps `COLORTERM=truecolor` when the launching terminal advertises it, because tmux births panes under `tmux-256color` with an empty `COLORTERM` and room apps read RGB capability from the session environment.

Zellij carries the map on the spawning client's environment; the per-session server and every pane fork from there, so inheritance is transitive. Zellij has no post-birth re-assert, so a session born before an env field existed keeps its old environment until rebirth. Zellij birth also stamps a `TERM=xterm-256color` fallback when the spawning environment carries none, so a non-PTY birth (remote-web prep, a headless launch) still yields panes with usable terminfo, while a present `TERM` rides through for local terminals.

Rebirth re-pins on both backends, and resume tabs are ordinary layout panes, so a re-seeded agent inherits the same contract.

### Pane metadata

`list_panes` reports each pane's foreground command, optional spawn command, optional title, cwd, view, and id.

The sidebar uses foreground for display, spawn for stable identity where present, and cwd for worktree grouping ([sidebar.md → presence model](./sidebar/sidebar.md#presence-model)). Foreground, title, and cwd are cross-backend. Spawn stays optional because Zellij omits it for panes created through `action new-pane`, while tmux exposes the static `pane_start_command`. **The parity floor for presence is command plus cwd**, which both backends meet.

Two Zellij-side wrinkles: the foreground and cwd fields are version-spanning ladders, so the adapter takes the first non-empty field across the names Zellij has emitted, and a layout-named `rimz-sidebar` pane always reports `rimz-sidebar` as its foreground so it filters as chrome even when Zellij omits the command fields.

`list_panes` marks floating panes on both backends. tmux exposes `pane_floating_flag` from 3.7; older supported releases expand the unknown format empty and therefore report tiled. Floating agent panes stay addressable but out of the room-row projection, and a self-closing sidebar view tears down same-view floating panes before its tiled anchor exits.

Neither backend reports a per-pane process start, so RimZ derives `pane_process_start` itself from the process backend ([produce/panes.rs](../../crates/rimz/src/sidebar/produce/panes.rs)) and uses it as the reused-id reconciliation key. That key guards both backends:

- Zellij recycles pane ids within one session.
- tmux `%id`s are unique within a server lifetime, but the RimZ-owned server exits once its last session ends, and the next command births a replacement numbering from `%0` again. A durable record naming `%3` can outlive the pane it described.

The pane PID is only the walk root and metrics binding, because a pane's PID is its shell rather than the agent it launched. Agent liveness uses the agent's own PID, captured best-effort by its hook ([model.md](./agents/model.md)).

## Reading the room

Each backend has one authoritative roster and, optionally, one push channel that makes reads fresher.

| | Zellij | tmux |
| --- | --- | --- |
| Authoritative roster | `zellij action list-panes --all --json` | `tmux list-panes` against the managed socket |
| Push channel | The presence plugin, publishing `pane-topology.json` | A control-mode client holding `refresh-client -B` |
| Client presence | Attached client count, from the plugin's `list_clients()` | Attached client count plus `last_input_ms` from `#{client_activity}` |
| Idle clock | None; presence is attach-only | Yes |

`PaneListOptions.consistency` selects how much a caller is willing to trust the push channel:

| `PaneReadConsistency` | Behaviour |
| --- | --- |
| `Cached` (the default) | Use a valid pushed topology, requesting a newer push when needed. |
| `PreferAuthoritative` | Query mux truth first, fall back to a valid pushed topology. |
| `RequireAuthoritative` | Query mux truth and propagate failure. Only this level licenses a destructive decision from pane absence. |

That last row is the load-bearing one. **Absence in a cache is never proof.** `cached_pane_roster` states the same rule at the trait: a listed pane proves liveness, while `None` or a missing id only permits escalation.

Both channels feed one shared host projector, [`sidebar/presence/projector.rs`](../../crates/rimz/src/sidebar/presence/projector.rs), which applies identical launch-chrome and sidebar suppression policy and emits the same `SidebarEvent` taxonomy from either backend. Backend-specific state stays minimal: [`TmuxPresenceState`](../../crates/rimz/src/sidebar/presence/tmux.rs) retains only what it takes to normalize out-of-order control-mode lines, and the Zellij side normalizes plugin payloads in [`sidebar::presence`](../../crates/rimz/src/sidebar/presence.rs). The event taxonomy and fusion rules are in [state.md](./sidebar/state.md).

The producer stretches its pane-cache TTL while a backend's presence stamp is fresh, and topology changes still repaint through typed overlays plus a verify-and-publish pair. Stale Zellij topology triggers a cheap plugin pipe and a bounded cache wait; stale tmux presence reverts to the steady poll within the freshness window. The budget math is in [performance.md](./performance.md).

## Focus

Focus is the subtlest part of this module, because three different questions hide behind the word.

### Who is looking at what

`client_view` reports the panes attached clients are actually looking at. The producer publishes the distinct terminal set as `viewed_panes`, and that gates every side effect that depends on a human looking at a pane: unread focus clears, tab-view sweeps, notification and reminder suppression, focused-tier cadences, and background paint suspension. Several clients viewing the same terminal agree; distinct terminal or plugin views are ambiguous.

`ClientView::unique_live_focus` is the resolver: fresh attached-client evidence counts only when every observation names the same live pane. Detailed client rows outrank summarized pane ids, and dead summarized panes do not invalidate one distinct live pane.

`PaneFrame.focused_pane` is the session presentation register, and its transitions are worth stating explicitly:

| Sample | Effect on `focused_pane` |
| --- | --- |
| Fresh, every attached view names one distinct live terminal | Set to that pane |
| Fresh, empty or plugin-only or dead or distinct | Cleared |
| Unavailable | May hold the prior live value |
| Realtime `FocusChanged` between pulls | Updated |

Hidden tabs carry no RimZ focus state, and the renderer's `UiState::baseline_pane` is only a local highlight and restoration hint. `PaneRef` and pane topology carry no focus bit at all: `rimz pane list` reports identity and process context without a per-tab active mark, hook recovery uses a fresh unique client view to disambiguate plural candidates, and `rimz sidebar focus --toggle` requires the same unambiguous view instead of guessing from the roster. Upstream roster focus marks never enter RimZ's model, diagnostics, binding, or repair decisions. The attached-client sample is the runtime authority.

### Jumping to a pane

`focus_pane` is the one-way jump primitive, and it lands cross-view on both backends: Zellij switches to the containing tab directly, tmux selects the window then the pane.

Every attached-client jump is wrapped in a two-phase global intent. `Requested` is durable before dispatch, command acceptance moves the same nonce to `Applied`, and a failure clears it. An applied intent supplies a short presentation target without fabricating an attached-client observation.

Native observations then resolve it. The exact unchanged pre-action client map is fenced after the short presentation window and yields unknown rather than snapping selection back to stale evidence. A target observation confirms the intent, a different pane supersedes it, and client replacement, detach, session replacement, or pane closure invalidates it. This separation matters on Zellij, where `action focus-pane-id` can move the visible pane and routed input without a causally matching `ListClients` update.

### The focus key

The sidebar's in-pane keys fire only when the sidebar pane is focused, so a room-scoped chord (`[sidebar] focus_key`, default `Alt+p`) reaches it from any pane. The keystroke lands in whatever pane is focused, so the multiplexer intercepts it; both backends run `rimz sidebar focus --toggle`, which focuses this session's `rimz-sidebar` pane or returns to a deterministic working sibling, and only when one unique fresh client view proves the sidebar is current. An unavailable or distinct view returns a non-mutating ambiguity error.

The chord is parsed once by [`FocusChord`](../../crates/rimz/src/mux/focus_key.rs) (`Alt` or `Ctrl`, with `M-`/`C-` and `-`/`+` separators). `Alt` is the default because it survives the terminal, Zellij's locked mode, and tmux's prefix; `off` or empty registers nothing. Registration is best-effort at session birth, so a convenience key never blocks a room.

The two backends bind it differently, because a tmux binding and a Zellij binding reach a pane differently:

- **tmux** binds a server-global root-table key that bakes in no room identity and resolves the pressing pane's session at keypress.
- **Zellij** routes through the presence plugin. RimZ passes the chord in the plugin's load configuration and the plugin binds it, once it holds the `Reconfigure` grant, to a runtime-only `MessagePluginId` action that messages its own plugin id. That reaches the exact loaded instance from any pane, leaves the user's `config.kdl` unchanged, and resets when the session ends. A user who declines `Reconfigure` can bind the same pipe by hand.

## One sidebar per view

Every working view should hold exactly one live sidebar pane. `reconcile_sidebars` converges toward that, in place, without disturbing working panes and without ever recreating the session.

The planner in [`reconcile.rs`](../../crates/rimz/src/mux/reconcile.rs) is pure. Each backend groups its native listing into `ViewSidebars` (the view's sidebar panes in mux order, plus whether the view holds working panes or daemon hosts) and supplies native add, close, and verification effects. `plan_reconcile` then emits one verdict per view:

| View state | Verdict |
| --- | --- |
| Occupied, one sidebar is claimed by a live renderer | `CloseDuplicates` for the rest |
| Occupied, no sidebar at all | `Add` |
| Occupied, sidebars exist but none is claimed | `Replace`: add first, close the old ones only after the new pane mounts |
| Orphan (no working pane, no daemon host) | `CloseDuplicates` for every sidebar, so a wedged renderer collapses with its view |
| The daemon view | Left alone |

`SidebarLiveness` carries the claims: `claimed_panes` from fresh renderer heartbeats, plus `young_panes` inside the first-heartbeat grace window so a pane that just started is never reaped. `has_unlocated` marks a live renderer whose pane could not be placed, which keeps the planner conservative.

Replacement is add-before-close on purpose. `wait_for_sidebar_heartbeat` blocks up to six seconds for the new pane to publish a heartbeat naming the expected build before the old pane is closed, so a failed add leaves the user with the sidebar they had. `prove_sidebar_mount` requires the current build, so a stale binary's pane never counts as the repair.

`SidebarRecovery` tallies the pass (`recovered`, `closed`, `failed`, `deferred`, `redocked`, `misdocked`) and the executor stops at the first failure, counting the remaining verdicts as failed. One best-effort pass: a view whose add fails is logged and skipped, never retried, never escalated to a session rebirth.

### Width

[`width.rs`](../../crates/rimz/src/mux/width.rs) resolves one room target from configured policy and live geometry, then snaps its share of the view to the backend's reachable rung.

The room-runtime record always contains `WidthPermille`, tenths of a percent of the full view, plus a pin flag. An unpinned target follows `theme.display.width_percent` and applies `theme.display.max_cols` whenever live view geometry is known. An `a`/`d` keypress or mouse drag pins the resulting share verbatim, so the explicit choice may exceed the configured cap and keeps its proportion when the terminal changes size. A genuinely new session clears the record and returns to configured policy.

Resolution produces `SidebarTarget`: one share, the configured cap, and whether the user pinned it. That resolved answer crosses the backend seam; `SidebarWidth` policy does not. Each repair path renders columns against the view geometry it already measured, clamping an unpinned default to the cap, while Zellij layouts spell the same share as a whole percentage. A geometry-free resolve preserves an existing share rather than blindly re-evaluating the width-keyed default; with no record it returns the narrow policy fallback for that call without persisting it. Columns without geometry use the bare cap because a detached layout's eventual view width is not known yet.

Every target is pinned to the backend's reachable share rung before publication. tmux keeps the permille value unchanged; its 1‰ resolution is finer than its exact two-column key step at ordinary terminal widths. Zellij rounds to the nearest 5% rung, independent of view geometry, so every tab born or key-stepped from that share can land together. An `a`/`d` keypress moves exactly one backend column step, converts the result to a share, atomically pins it, and broadcasts `WidthTargetChanged`; every renderer resolves that share for its own live view and converges with at most one mux resize in flight.

- **tmux** applies the absolute target in one command, so a narrower intent clamps to the 24-column floor.
- **Zellij** derives a step of 5% of the view from fresh presence topology and issues one relative step per resize-feedback event or one-second backstop. It rejects a narrower intent whose next reachable width would cross the floor. Missing topology rejects either direction because a fabricated view would pin the wrong room-wide share.

The renderer-local controller and reconcile-time repair use the same crossing contract. The backend step seeds a symmetric half-step stop band before the first nudge, and observed feedback refines that estimate. Within one convergence pass, a crossing that lands nearer to the target, or equally near, parks where it landed; one that lands farther earns exactly one reverse step and then parks unconditionally. The renderer keeps that park across events in its local `idle_at`; reconcile is stateless across invocations, so a later birth or attach repair may re-walk the same bounded steps when the nearest reachable width sits outside the nominal band. No progress and bounded step budgets remain terminal outcomes.

The one-second settled-resize pass arms only when the measured pane sits outside the current half-native-step band, then adopts only with positive evidence: backend geometry must exist, the pane observation must be newer than the resize, and neither the full view width nor sibling count may have changed. A drag inside the band leaves the shared share untouched because snapping would select the same rung. Missing or stale evidence converges and re-arms classification; a view or sibling change converges without adopting. Only the remaining case is an adopted mouse drag: RimZ converts the measured width to a share, snaps it to the backend rung, pins it room-wide, and broadcasts it once. A dragged Zellij pane already within half a native step (at most 2.5% of its view) keeps its measured width even when the snapped share resolves a few columns away. Lattice-resident panes reach the snapped target exactly; a room displaying widths such as 25/25/23/25 across tabs is therefore at the platform floor, not carrying divergent targets.

## Session lifecycle

[`RoomContext`](../../crates/rimz/src/room/mod.rs) sits above the trait and owns the shared parts: managed identity, config derivation, birth, reset, pre-attach health, heartbeat purge, and presence-load ordering.

### The pre-attach health gate

`open_sidebar` is best-effort and can be skipped or fail, so it cannot be the only thing standing between the user and a resurrecting attach. The room birth transition runs `ensure_clean_session` as the authoritative gate, immediately before presence load and attach preparation.

| Session state | Gate action | Verdict |
| --- | --- | --- |
| Live | Attach as-is, without inspecting panes | `Healthy` |
| Absent | Birth from the layout | `Reborn` |
| Exited (Zellij resurrection record) | Delete, then birth from the layout | `Reborn` |
| Still not live after a rebirth | Nothing further | `Stuck` |

tmux has no resurrection, so its gate is a no-op `Healthy`.

A Zellij IPC socket-path overflow is a separate environment precondition rather than a health verdict: it classifies as `SocketPathTooLong`, reset is not offered, and `rimz doctor` prints the shorter-directory fix.

### Reset

A `Stuck` room needs destructive reset. [`RoomContext::reset`](../../crates/rimz/src/room/mod.rs) gives explicit `rimz reset` and attended stuck recovery the same teardown plus store-reset runtime, while the CLI keeps confirmation and report rendering. Both paths purge the serialized-session cache, reap stale sidebar runtime files, sweep orphaned servers and leaked daemons, then rebirth.

The dangerous step is the process sweep in [`recovery.rs`](../../crates/rimz/src/mux/recovery.rs), which signals processes by heuristic. It is scoped four ways: the real uid, the exact path-derived session name in the command line, an explicit exclusion of this process and its ancestors, and the inherited environment domain. [`ProcessDomain`](../../crates/rimz/src/mux/domain.rs) is that last guard. A process in a foreign domain (a `cargo xtask sandbox`, another runtime root) is not RimZ's to signal, and an unreadable process environment is spared.

Without a terminal, `rimz start` fails fast with the `rimz reset` fix rather than destroying a session unattended.

### Sidebar orphan reaping

Two paths can kill a sidebar process, and both apply the same evidence boundary.

The destructive orphan watchdog reads `cached_pane_roster` first as a latency hint. On escalation, one sidebar wins a workspace/session single-flight lock, performs a listing with `RequireAuthoritative`, and atomically publishes mux kind, session, observation stamp, and pane ids so peers consume that exact fresh observation. A sidebar counts at most one strike per observation, resets on presence, preserves strikes on unknown evidence, and terminates only after three distinct authoritative absences. Contention, timeout, stale or mismatched cache, parse failure, and mux failure all stay unknown and cannot trigger a local destructive fallback.

Reload and repair reap paneless sidebar processes under the same rule. A fresh cache can prove a pane present and avoid a mux command; omission only nominates a process, and two `RequireAuthoritative` rosters separated by a short delay must both omit its pane before RimZ signals it. A candidate must also prove through its inherited environment that it lives in the invoker's state and mux-socket namespace. Either roster failing aborts the whole reap. A cache omission refuted by either roster records `pane_cache_divergence`; every signalled victim records `sidebar_orphan_reaped` with both observation stamps and the SIGKILL outcome.

## Zellij backend

One constraint shapes everything here: **a Zellij layout applies only at session birth, and a layout is the only way to place a left, sized pane at creation.** So RimZ owns the birth layout and treats everything after it as convergence: close stray sidebars by id, add a missing sidebar in place, move it left, and converge its width toward the current tab's live target.

`ensure_session` is a no-op because Zellij creates sessions lazily. The sidebar launch owns first birth through `attach --create-background` with a generated layout.

### The birth layout

Every tab is shaped the same way: a left `rimz-sidebar` pane and a focused terminal, above a one-row compact-bar plugin. A `new_tab_template` plus explicit birth tabs carry that shape forward.

Several details in [`layout.rs`](../../crates/rimz/src/mux/zellij/layout.rs) are load-bearing:

- The sidebar command names the workspace's stable room-bin path rather than one sweepable build generation, so the immutable `new_tab_template` keeps spawning working sidebars after reloads.
- The sidebar pane is borderless and `close_on_exit`, so work-pane frames can be styled while sidebar hit-testing starts at row 0 and the pane disappears when its process exits.
- Every sidebar width is spelled as a percentage, because Zellij resize-pins fixed-size layout panes. With known geometry the spelling names the resolved grid rung exactly; detached birth tabs and the template retain configured percentage policy until a live view exists.
- Every tab is born with an explicit focused terminal rather than a `children` placeholder, which nested in a split is never auto-filled and would strand focus on the sidebar alone.
- The layout file outlives the create call. Zellij parses `--default-layout` asynchronously, so the temp file stays on disk until the panes materialize.

Birth branches on the session's liveness, as reported by `zellij list-sessions`:

| Liveness | Branch |
| --- | --- |
| Live | The session already carries its sidebar and owns every resize and split since. A trusted sidebar heartbeat makes `open_sidebar` a no-op; a stale heartbeat rebuilds only an inspected live room, and an uninspectable one is left untouched. |
| `Exited` (`EXITED - attach to resurrect`) | Clean rebirth: delete, then create from the layout. With serialization off this state stops being minted, but the branch stays as defence for sessions serialized before the flag landed. |
| Absent | First birth: create from the layout. |

### Room options and the CLI XOR problem

`<room-options>` combines the RimZ-owned invariants with the optional `[zellij]` keys the user sets in RimZ config. Each maps onto a Zellij `options` flag ([reference → options catalog](../externals/mux-adapter/zellij-reference.md#options-catalog)), and newer flags are version-gated so an older host degrades to its default rather than aborting.

Mouse options need a second mechanism. Zellij XORs boolean CLI options against values already set in the user's `config.kdl`, so a CLI flag cannot express an absolute room invariant for every user. Birth and attach still pass `--mouse-click-through true` and `--focus-follows-mouse false` as a birth-window hint, but the presence plugin applies RimZ's resolved values through `reconfigure(..., false)`, whose KDL path merges onto the live config absolutely and never writes the user's file. With the defaults, Zellij 0.44 sends the first click through: a single click both focuses the sidebar pane and reaches the renderer, so a jump lands on the first click rather than the second.

RimZ leaves `advanced_mouse_actions`, `mouse_hover_effects`, `mouse_mode`, and global `pane_frames` to `config.kdl` unless the user sets those keys in RimZ config.

**Serialization off.** RimZ disables Zellij session serialization on every birth and attach. Resurrection is worse than useless for a room of agents: agents and scripts cannot restore their running state, so a resurrected room comes back as a wall of held command panes with a dead mouse. With serialization off, a crashed server's session simply vanishes and the next start births a clean, running room; RimZ owns rebirth instead ([resume on rebirth](./sidebar/sidebar.md#resume-on-rebirth)). The birth layout carries `session_serialization false` because Zellij 0.44 drops `options` flags from `attach --create-background` before detached-server initialization; attach still passes the flag and first purges the room's resurrection cache so a corrupt layout cannot block a live session.

**Session metadata off.** RimZ embeds `disable_session_metadata true` in the birth layout and passes it on every birth and attach. That stops Zellij's periodic `session-metadata.kdl` rewrite and its command-discovery `ps` loop, which at roughly 100 panes on 0.44.3 costs a visible share of the Zellij server CPU. `Absent` and `Exited` still converge through the same clean-rebirth gate, so this changes CPU cost rather than room semantics.

### In-place repair

Live reinjection resolves a stable tab id from an existing work pane and runs `new-pane --tab-id` with placement unspecified. From there the backend proves rather than assumes, because Zellij's answer arrives before the pane mounts:

1. Mounted-pane discovery verifies the intended tab; action stdout is only a hint.
2. Repair requires a fresh current-build heartbeat before an add commits or a replaced pane closes.
3. A wrong-tab mount is cleaned up and aborts the pass.
4. Structural move, stack, retry, and verification reads use direct `list-panes --all --json` geometry after completed actions, with fresh presence topology as the recovery fallback.
5. Each targeted left move crosses one adjacent pane, the current tiled-pane count bounds those swaps, and every step must strictly decrease the sidebar's `pane_x`.
6. Width convergence starts only after current geometry verifies the full-height left dock.

A timed-out authoritative read aborts the pass instead of falling back to the topology cache, so stale truth never drives a close or a spawn; the next elder or toggle pass retries.

RimZ passes `auto_layout=false` and `stacked_resize=true`, so `Alt+n` uses Zellij's native focused-pane split along the terminal's real cell-ratio-favorable edge, and closing a pane returns the freed space to the sibling it split from. The birth tree pins the sidebar and compact bar as tree siblings. When an add nests the new sidebar into one row, the same transaction stacks every surviving work pane into the right column; repair of a pre-existing arbitrary multi-column layout stays report-only.

The producer's shrink-confirmation path bypasses `pane-topology.json` entirely and reads `zellij action list-panes --all --json`, merging cached foreground command and cwd only as enrichment. If that server query fails, the backend falls back to the topology cache with a debug log. tmux already lists directly from the server, so its authoritative flag is a no-op.

### The daemon view and resumed births

`rimz start` always carries the `rimzd` runtime view, so `open_sidebar` births a two-tab layout: the runtime dashboard first, then the focused working tab. The order is fixed at birth because Zellij has no CLI to reorder tabs afterwards.

The runtime view is `sidebar | content | runtime`, and what fills those columns, how its panes are identified, and how repair rebuilds them are in [rimzd.md](./rimzd.md). What the backends contribute is the placement: tmux births multiple content or runtime panes as equal-height rows, with at most one row of integer-rounding drift.

Repair identity differs by backend: every managed Zellij pane carries its joined launch argv as an explicit pane name, so identity survives a supervisor or remote-control host putting a child in the foreground; tmux uses `pane_start_command`, with foreground command and title as fallbacks.

Scheduled loop runs split against the loop panel with Zellij's native stack, anchored through the panel's CLI pane context with `--near-current-pane`, so attached-client focus and the active tab stay untouched; tmux degrades the stack to a tiled row. If the view survives but the panel is gone, the same placement engine recreates the panel first; a missing view or a failed split falls back to a new tab. What decides that a run fires at all is in [loops.md](./harness/loops.md#where-a-scheduled-run-lands).

A reborn session re-seeds its remembered agents: the birth layout spells one `sidebar | agents…` tab per worktree, each agent a command pane running its resume CLI in that worktree, focus on the freshest. Born in a fresh layout they start running rather than suspended, which is the same reason serialization is off. One renderer handles plain, daemon, and resumed births.

### Tab-switch focus repair

Zellij can restore focus to the sidebar when the user switches tabs, which would strand them on chrome. Repair is a plugin observation plus a host verdict, and the split is deliberate.

On a `Some(old) → Some(new)` tab switch the presence plugin waits for the settle window, serializes one `list_clients()` observation, and publishes `switch-settled` with the active tab, a generation, and the full client views. It publishes no verdict.

The host classifies that observation against the accepted topology. A unique live work view in the active tab is healthy. A plugin view, the active tab's sidebar, or a live terminal in another tab is stranded, but only when the active tab has exactly one sidebar owner and a work sibling. Missing, detached, dead, superseded, foreign, and distinct-pane observations abstain. The accepted client sample independently drives host-side unique-live focus projection.

The renderer keeps the owner, TTL, client-ambiguity, and focus-intent guards, so automatic repair never overrides an explicit cross-tab jump. The renderer's selection model treats the resulting `from-pane → sidebar → target` transition correctly; see [sidebar.md → selection](./sidebar/sidebar.md#selection-and-jump).

### Zellij backend caveats

These are the upstream quirks the backend works around. The upstream surfaces themselves are in the [reference](../externals/mux-adapter/zellij-reference.md).

- **Minimum version is 0.44.0**, the floor `rimz doctor` reports as `meets_min_version`. Below it RimZ refuses the Zellij room and points at upgrading Zellij or using tmux. `stack-panes` and `advanced_mouse_actions` are inside the supported floor; `mouse_click_through` and `mouse_hover_effects` stay version-gated for future compatibility.
- **Pane IDs are positional, not stable.** Zellij exposes no stable per-pane CLI handle, and ids are reused as panes close and reopen. Pane stamps carry `pane_process_start` so reconciliation can refuse a stale match.
- **`new-pane` answers before the pane mounts, and action stdout can cross clients.** The printed pane id is allocated before the screen thread mounts the pane, and a detached session drops the mount entirely. Reconcile treats the id as a hint, discovers the mounted pane through plugin topology, and cleans up only a pane a fresh topology snapshot proves is a newly-created `rimz-sidebar`.
- **`new-pane` can mount into a nested row.** A stable-tab add inherits Zellij's tab-local split tree, so the sidebar can report `x=0` with a work pane spanning beneath it. Every add verifies the full-height left-column band and can stack the work panes it just displaced without replacing their processes.
- **Named-session actions can print a session-not-found banner with exit 0.** A `--session <name>` action against an absent, exited, or still-registering session prints `Session '<name>' not found...` plus an active-session list across stdout or stderr. RimZ classifies it as `MuxErr::SessionNotFound`; best-effort sidebar reconcile and daemon view launch defer quietly while the pre-attach gate owns rebirth.
- **A detached server can drop pane lifecycle processing until the next attach**, notably a last-pane exit and the relayout after a sibling closes. Reconcile defers adds on detached sessions, and the renderer's tab-empty self-close rides its data-tick backstop rather than resize delivery alone.
- **A configless server births a setup wizard that silently drops `new-pane` mounts** (layout-born panes mount normally). The test harness seeds a config at the home-relative path Zellij prefers; in production a first-ever user dismisses the wizard once and reconcile retries the dropped mount.
- **Plugin keybinds pause briefly on 0.44.x.** Zellij's upstream `KeybindPipe` completion path can freeze the UI for about a second before a plugin keybind acts. The focus-key jump still lands.
- **Session names are short and path-unique** (`rimz-<basename-slug>-<hash6>`), which keeps the room human-scannable while staying under Zellij's macOS AF_UNIX socket budget and distinguishing same-basename roots. When the recorded and current derived names diverge, `rimz start` retires the stale session before rebirth.
- **The presence plugin reports identity and geometry, not live process state.** RimZ derives cwd, pid, and process start through the process backend and treats the spawn command as identity.
- **Per-test server isolation in CI.** Tests construct the backend with a private runtime dir, since Zellij locates its server socket under `XDG_RUNTIME_DIR`. This is the parity counterpart of tmux's `with_socket`, and every command flows through the single `ZellijBackend::cmd` chokepoint so one field threads isolation everywhere.

## The Zellij presence plugin

tmux hands out a control-mode stream that any process can attach to. Zellij has no such thing: the only way to learn about pane changes as they happen is to run inside the Zellij server as a plugin. `crates/rimz-presence-zellij/` is that plugin, a headless wasm32-wasip1 binary loaded into every Zellij session RimZ manages. It renders nothing.

Its contract is [`crates/rimz-presence-zellij/AGENTS.md`](../../crates/rimz-presence-zellij/AGENTS.md).

### The boundary: observations, never verdicts

The plugin publishes Zellij facts. The host derives every meaning.

| The plugin owns | The host owns |
| --- | --- |
| Merged topology snapshots from Zellij's pane and tab manifests | Pane roles: which pane is a sidebar, which is an agent card |
| Attached-client observations, including the settled sample after a tab switch | Focus-repair decisions and the `SidebarEvent` taxonomy |
| Poke timing that Zellij's event model requires | Launch-chrome filtering and topology-writer authority |
| Capabilities that need plugin-only APIs: the runtime focus keybind, mouse `reconfigure`, hiding or closing itself | Durable cache publication |

The payoff is release cadence. A product-policy change (what counts as chrome, when a focus is stranded, how an event maps) ships in the `rimz` crate alone and needs no plugin release. Two corollaries follow: carry only facts that originate in Zellij's server state, since a fact derivable from the OS routes host-side through `pane_pid` (the host owns `/proc`, the plugin owns the event stream), and add a wake shape only for a fact that cannot be derived from an accepted snapshot diff.

One session holds one plugin. Splitting control features across plugins would multiply lifecycle, permission, and writer-coordination complexity.

### Crate shape

The crate splits along the wasm boundary, which is what makes it testable.

| Module | Role |
| --- | --- |
| [`main.rs`](../../crates/rimz-presence-zellij/src/main.rs) | The wasm shell. Projects Zellij events into the engine, gathers runtime telemetry, and executes returned effects. Compiled only for wasm; host targets build a stub so `--workspace` builds and lints stay green without the wasm toolchain. |
| [`engine.rs`](../../crates/rimz-presence-zellij/src/engine.rs) | The decision engine: room state, poke timing, focus correction, permission gating, topology publication. Returns `Vec<Effect>`. |
| [`policy.rs`](../../crates/rimz-presence-zellij/src/policy.rs) | Pure helpers and timing state machines: the stable-field hash, poke policy, foreground overlay. Time is injected as Unix milliseconds. |
| [`wire.rs`](../../crates/rimz-presence-zellij/src/wire.rs) | Every argv and KDL payload the shell sends to the host. |

`engine`, `policy`, and `wire` contain no `zellij-tile` type, so they compile and unit-test on the host target inside the ordinary workspace test run. `zellij-tile` is a wasm-only dependency, since its shims call extern host functions that exist only inside Zellij's plugin host.

The engine returns effects rather than performing them: `RunCommand`, `HideSelf`, `Reconfigure`, `CloseSelf`, `Unsubscribe`, `Resubscribe`, `SetTimeout`, `ListClients`. Every decision is therefore a pure function from event to effect list, and the shell stays a projection layer.

Inside the engine, one canonical pane map is the single source of truth. Reducers retain partial manifests, patch event enrichment in place, and publish panes in deterministic tab and key order.

### What it publishes

The plugin subscribes to ten Zellij events: `PaneUpdate`, `TabUpdate`, `CommandChanged`, `CwdChanged`, `PaneClosed`, `Timer`, `PermissionRequestResult`, `RunCommandResult`, `SessionUpdate`, and `ListClients`.

Everything it publishes travels one way: a fire-and-forget `run_command` fork of `rimz sidebar wake`. Four wake shapes exist:

| Shape | Meaning |
| --- | --- |
| Announced snapshot | A room change worth an event broadcast. |
| Silent snapshot | Keepalive and explicit dumps: refresh the cache without broadcasting. |
| `clients` sample | Raw attached-client observations, as `{ views: [{ client_id, pane_id }] }`. |
| `switch-settled` | The generation-bearing observation after a tab switch settles. |

Each wake after the first manifest may carry the live roster as repeated `--topology` values, bounded to 64 KiB each and omitted entirely above 1 MiB, while stamp and telemetry delivery continues. `rimz sidebar wake` concatenates the chunks in order and normalizes the boundary payload.

The first manifest after load names every pre-existing pane, so the host accepts it as a baseline and an announced baseline emits only one topology nudge.

The host derives attached-client count, terminal views, and unique-live focus from `clients`. A legacy `focused_pane` field is accepted only as a fallback when `clients` is absent, and a legacy payload without `clients` keeps producer-side `client_view` fallback active.

Three named pipes reach the plugin from the host:

| Pipe | Effect |
| --- | --- |
| `rimz:dump_topology` | Publish one immediate `alive` wake, bypassing the poke floor. Revives and resubscribes a retired same-id clone for that publish. |
| `rimz:focus_sidebar` | Run the focus-sidebar fork; this is what the focus keybind messages. |
| `rimz:retire` | Retire this instance if the payload's generation outranks it. |

### Poke discipline

Left unthrottled, a busy room would fork `rimz` on every keystroke-driven event. [`policy.rs`](../../crates/rimz-presence-zellij/src/policy.rs) holds the timing that keeps the channel quiet.

| Rule | Value | Purpose |
| --- | --- | --- |
| Immediate first poke | 0ms | The first change after quiet is never delayed. |
| Poke floor | 100ms | Duplicates inside the window collapse into one. |
| Settle poke | 250ms | Each accepted change schedules one, so a command change cannot strand the pre-change command. |
| Focus settle | 250ms | The window a tab switch waits before sampling clients. |
| Keepalive | 60s | Holds the presence stamp fresh while idle and requests a client-list self-heal. |

Title-only events stay filtered out entirely.

Client sampling has its own coordinator. Every `PaneUpdate` queues a coalesced general client query before topology deduplication, so a focus-only upstream update refreshes attached truth without storing a focus signature. One untagged `ListClients` request is in flight at a time; the coordinator retains the newest general or switch-settled purpose, expires a missing reply on the keepalive deadline, treats a reply after expiry as a general sample, and re-arms the superseded purpose.

Every host fork runs from `/`, which decouples the session-lifetime plugin from the cwd of the CLI that loaded it.

### Loading and permissions

Loading is RimZ-owned and never the user's `config.kdl`, because a layout cannot load plugins.

The load verb is the idempotent `zellij … action pipe --plugin --skip-plugin-cache`, the one verb that works on a clientless session and carries the cache-bypass bit in Zellij 0.44. Only owner flows use it: room birth and `rimz reload` upgrade and repair. Generic pane and topology readers broadcast the name-only `rimz:dump_topology` pipe instead and never launch a plugin.

Load-time configuration pins the workspace, the session, the room's `rimz` pointer (`workspaces/<id>/rimz`), runtime mouse options, the background launch scope, the lazy-once embedded-wasm digest, and a hash of the configuration itself. Every desired identity is instantiated only through this pipe, so an identity-matching writer is background by construction and receives global pane and tab updates. Changing an identity launches another background writer; the host accepts its proof and retires the old identity.

The canonical artifact path stays stable across upgrades, while Zellij's compiled-module cache is keyed by that path rather than the wasm bytes. Every plugin-addressed pipe therefore skips the cache; a live identity treats the flag as a no-op, and a missing identity compiles the bytes currently installed at the path. The `launch_scope=background` configuration key gives existing sessions a one-time identity bump, so the same convergence flow repairs writers created through the removed tab-scoped action fallback.

RimZ seeds Zellij's `permissions.kdl` cache for its own embedded plugin so the first attach is not interrupted by a prompt, even in a clientless session:

| Permission | What it buys |
| --- | --- |
| `ReadApplicationState` | The pane, tab, session, and client manifests. |
| `RunCommands` | The `rimz sidebar wake` fork. |
| `Reconfigure` | Runtime mouse options and the optional focus keybind, applied without writing `config.kdl`. |

The plugin artifact path is canonicalized because Zellij keys the grant on the exact string. The security boundary is in [security.md](../guide/security.md#the-zellij-presence-plugin).

A Zellij room requires Zellij 0.44 or newer and a loadable plugin. An older host, a missing artifact, or a denied permission makes the selected Zellij backend fail its precondition, and `rimz doctor` names the first failing fix plus tmux as the alternative backend.

### Build identity and embedding

The plugin is embedded into every RimZ build. Release binaries embed a fresh `cargo xtask build-plugin` artifact; the crates.io crate embeds the vendored `crates/rimz/presence/` wasm. `cargo xtask plugin-refresh` builds that artifact with canonical path remaps and commits provenance beside it: the source-tree digest, wasm digest, and producing rustc version. Repository invariants bind both digests to the current tree and blob, every vendored embed verifies the wasm digest, and `cargo xtask checks` rebuilds with the recorded toolchain and requires byte-for-byte equality.

The digest of that wasm is the plugin's build identity, which intentionally makes each build a distinct Zellij plugin identity so an owner can upgrade a clientless session.

The workspace record carries the staged `rimz_bin` plus its `rimz_build` digest as one verified room target. Only room owner claim and reload update the pair: `rimz start`, cwd-based `rimz attach`, and `rimz reload`. Named attach by session preserves the recorded owner, and generic CLI re-records preserve both values. Because the plugin configuration names the stable `rimz` pointer rather than the staged path, a worktree build that asks for topology leaves the configuration string unchanged.

Owner flows materialize or refresh the shared embedded wasm artifact. Read-only topology refreshes use only an existing artifact or the beside-executable development fallback. The shared artifact therefore tracks the last owner build, and another session can run those bytes until its own owner refreshes them. The writer gate below makes that benign.

### Writer fencing

Zellij 0.44 runs one wasm instance per connected client and can retain ghosts for departed clients, so a single plugin id can have both the blessed current clone and an older same-id clone. Overlapping writers are therefore normal, and the host arbitrates.

Every topology payload carries its plugin build and configuration plus the fallback generation `(loaded_at_ms, plugin_id)`. Owner launches atomically publish the desired identity in `presence-desired.json`.

[`sidebar::presence`](../../crates/rimz/src/sidebar/presence.rs) holds the workspace-runtime `topology-writer.lock` for at most one second, across the desired-record and cache reads, the writer-rank comparison, cache replacement, and conflict update. Lock or write failure rejects the command rather than falling back unlocked.

Ranking: a writer matching both desired fields outranks every non-matching writer, then load time and plugin id break ties. Without a desired record, ranking degenerates to generation ordering, and legacy payloads without writer identity rank at the zero generation.

The gate accepts a poke when the cache is absent, the existing same-session cache is stale, or the incoming rank is at least the cached rank. A sole non-matching writer therefore keeps refreshing its own cache, while a desired writer deterministically wins an overlap.

An accepted write commits before writer-change diagnostics, conflict clearing, presence stamps, telemetry, and event broadcast. A rejected poke skips all of them: no presence stamp, no plugin-presence sample, no topology write, no sidebar event. Rejections update `topology-writer-conflict.json` under the same lock and emit a rate-limited `topology_write_rejected` diagnostic. The reject count restarts whenever either writer changes, while the diagnostic's rate limit spans incidents. An accepted writer with a strictly higher rank removes the superseded sidecar, and doctor ignores an orphaned sidecar once the live cache carries a newer generation. Accepted writer changes emit `topology_writer_changed`.

The rejected plugin learns about it too. A rejected publish exits with the private stale-writer status (73), and three consecutive rejections retire the losing plugin in place by muting it and unsubscribing. `close_self()` would unload every clone sharing that plugin id, including the blessed one, so a same-id clone mutes instead of closing. Only a successful topology publish resets the streak.

### Retirement

`rimz reload` converges the plugin only when it must. It reads the fresh topology cache and, when the writer echoes a `build` equal to the embedded-wasm digest and a `config` equal to the desired configuration hash, confirms that the live plugin roster contains only that writer id before counting the session plugin-current. Extra or missing ids run the retire-and-sweep path without reloading the accepted writer, and a failed live listing falls back to full convergence. Any identity mismatch, missing field, or stale cache also converges and reports the upgrade.

Retirement requires proof. Reload waits for topology at or after the flow's freshness floor from the expected build and configuration, while session birth uses any matching proof already published by the boot pipe rather than adding a startup wait. The retire broadcast then carries that complete writer identity as JSON, and each instance decides for itself:

| Instance | Response to a retire broadcast |
| --- | --- |
| Different build or configuration | `CloseSelf`, regardless of load time |
| Same identity, outranks the generation | Ignore |
| Same identity, different plugin id, outranked | `CloseSelf` |
| Same identity, same plugin id, outranked | Mute and unsubscribe, revivable through `rimz:dump_topology` |

After the broadcast, the host lists every pane under a bounded deadline and closes each `rimz-presence-zellij` plugin id except the accepted writer, unconditionally unloading later old-wasm instances and command-path zombies while preserving every blessed same-id clone. RimZ then sends the boot pipe again, healing the legacy case where an old path-based retire closed the whole plugin id. A failed or timed-out listing degrades to the cooperative retire alone, leaving manual session restart as the fallback. When a detached or degraded session cannot prove the replacement is alive, RimZ skips retire and retries on a later owner flow.

`rimz reload` without `--repair` nudges its sidebars, which converge worker-first from the durable workspace record, and touches the presence plugin only when its echoed identity no longer matches the running build. It never changes pane structure. `rimz reload --repair` ensures the plugin first and then requires a post-ensure topology publication before any topology-dependent work; a Zellij session that misses the bounded health proof reports no live presence channel and skips repair, while runtime cleanup still runs.

### Telemetry and failure reporting

The plugin subscribes to `RunCommandResult` and drains every reply to its command forks. Each reply carries the host's exit code and stderr, so the plugin retains the newest failure as an exit code, the first non-empty stderr line bounded to 200 bytes, and a timestamp. That retained failure is the one channel by which a wake's cause reaches `rimz doctor`.

`fold_failure` decides what survives, and the division is the boundary again: the plugin ships observations, the host judges them.

| Outcome | Effect on the retained failure |
| --- | --- |
| Topology or other failure | Replaces it, stamped with the time |
| Stale-writer rejection | Left alone; that exit is the fence working, and reporting it would bury the failure the reader is chasing |
| Success | Left alone, so the evidence outlives the recovery |

A success deliberately does not clear the record. Wakes run far more often than telemetry is sampled, so clearing on success dropped the cause of an intermittent failure before any sample could carry it: the host counted the failure in its window and had nothing to say about it. The host instead takes its cause from the window its counters measure, dropping a stamp older than the window's first sample rather than passing it off as current. The stamp is optional on the wire, so a cause from a plugin loaded before it existed stays usable rather than being dated to the epoch and hidden.

Three consecutive fork failures clear the configured `rimz_bin`, retry one `alive` poke through `rimz` on PATH, and reset after a successful fork.

The keepalive carries WASM memory pages, uptime, per-bucket command counts (completed, succeeded, stale-writer rejections, topology failures, other failures), the retained failure, and the Zellij version into the rotating `plugin-presence.log.jsonl`. That file is the leak-investigation surface, because it separates plugin linear-memory growth from Zellij-native RSS growth.

## tmux backend

### The managed server endpoint

RimZ owns one tmux server per runtime domain, at `<runtime-root>/rimz/tmux/server`, holding one path-derived session per workspace. Every managed command runs `tmux -S <socket> …` through the single [`TmuxBackend::cmd`](../../crates/rimz/src/mux/tmux.rs) chokepoint. The socket is always set, so no command can reach the user's default server, and `cargo xtask invariants` rejects a bare `tmux` argv.

Because the endpoint derives from the resolved runtime root alone, any caller reconstructs it without a workspace or `RuntimePaths` argument. Attach, ttyd, presence, pane I/O, list, reload, GC, sidebar, and doctor all address the same constant. A disposable `XDG_RUNTIME_DIR` yields a different socket and therefore a private server, which is exactly what gives sandboxes and tests their isolation.

Each managed session is stamped with the concrete `HOME` and `XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`XDG_CACHE_HOME`/`XDG_STATE_HOME`/`XDG_RUNTIME_DIR` of the resolved domain, at birth and on every ensure, so a pane resolves the same store and the same endpoint as the client that created it. Socket identity and stamped environment are two projections of one runtime domain, derived together in [`store::paths`](../../crates/rimz/src/store/paths.rs). A server whose sessions disagreed with the socket addressing them would be unreachable in exactly the way this design removes.

Routing stays explicit. The ambient default server serves only recordless external sessions and the exact endpoint a process inherits through `$TMUX`; managed commands clear `$TMUX` so an ambient session cannot capture them. `ProcessDomain` resolves a process's endpoint from `$TMUX` when present and from the managed socket otherwise, so the orphan sweep recognizes managed processes and spares anything on the user's own server.

Before birth or attach, a read-only `has-session` probe of the legacy default socket reports a same-named session left there by an older release, with the one command that retires it (`tmux -S <default-socket> kill-session -t <session>`). `has-session` cannot start a server, so probing never resurrects a default daemon, and unrelated sessions there stay untouched.

Workspace reset and cleanup use `kill-session`. `kill-server` is reserved for explicit recovery of the whole RimZ tmux fleet, and is safe to suggest because it is scoped to the RimZ socket. Server-global options and root key bindings are shared across RimZ workspaces, the pre-existing behaviour now confined to RimZ's own server. They die with the last session, so `ensure_session` re-asserts them on every ensure rather than once at first birth.

### Every managed client runs from `/`

A tmux server inherits its working directory from the client that births it, and tmux's `spawn.c` performs a pane's `chdir(cwd)` only while `getcwd()` on the server succeeds. A server born in a directory that is later deleted (a disposable worktree, a swept tempdir) silently strands every later pane in that deleted directory, even when RimZ passes an absolute `-c`.

`/` cannot be deleted or unmounted, so birth and rebirth always start from a readable directory. The Zellij plugin host forks from `/` for the same reason.

Session birth proves the property rather than assuming it: it reads the birth pane's `pane_current_path` back and refuses a mismatch with a socket-scoped `kill-server`. Reading the pane back tests what actually matters and works on every host, where inspecting the daemon's live working directory is only a proxy and is unavailable on some platforms.

### Room options

`ensure_session` applies the per-machine `[tmux]` room options in one batched client call, and the `after-new-window` hook replays window options before docking the sidebar so later windows match the birth window. Session and window options stay scoped to the RimZ session; server-scoped options (clipboard, rich-key handling, focus events) are runtime-global because tmux has no per-session equivalent.

The batch also does four things the option list alone does not express:

- writes `*:sync` at the fixed `terminal-features[240]` index for atomic redraws in pixel pets and TUIs, purging exact `*:sync`/`*:extkeys` entries leaked at other indices by the former append path,
- writes `*:extkeys` at `terminal-features[241]` whenever extended keys are enabled and unsets that index when they are disabled,
- registers root-table `S-Enter` and `M-Enter` bindings that inject the configured modified-Enter sequence, so agents receive soft newlines even when they do not request modifyOtherKeys,
- names `ESC[27u` as `user-keys[240]` and binds it to Escape, because tmux passes that modifier-less form into panes verbatim.

On tmux 3.5.x the same extended-key mode trades clean multiline clipboard paste; tmux 3.6 preserves paste bytes while modified keys still reach agents as CSI-u. Per-option semantics and RimZ's values are in the [reference → options](../externals/mux-adapter/tmux-reference.md#options); the config model is in [configuration.md](../guide/configuration.md#multiplexer-room-options).

An attach launched by RimZ runs with alternate scroll disabled on its terminal and restores the saved prior mode when the client exits. tmux unconditionally disables outer mouse reporting in `tty_start_tty` before its first attached-client repaint restores the requested mouse mode, and terminals with alternate scroll enabled translate wheel ticks in that gap into arrow keys. The CLI uses XTSAVE/XTRESTORE around the client process because a tmux `client-detached` hook runs after teardown has cleared the departed client's tty name; Ghostty 1.3.1 implements those operations for mode 1007, and a terminal that ignores them safely remains with alternate scroll off. A one-shot SSH attach keeps one bracket on the local terminal and marks the remote launch through an environment variable as already bracketed because terminal mode save slots are not a stack; an older remote ignores the marker and performs no inner bracket. Reconnect supervision has no local bracket, so the remote RimZ owns it. The same lifecycle safely covers Zellij attaches.

The waiting RimZ parent mirrors a client's `SIGTSTP` stop and resumes the child with its foreground job, preserving tmux's stock `prefix` + `C-z` suspend behavior. Normal child exit codes pass through unchanged after terminal restoration.

When RimZ owns `pane-border-status`, it also writes a `pane-border-format` that floods the `rimz-sidebar` pane's border row with spaces, so work panes carry titled frames while the sidebar reads frameless: the tmux analog of Zellij's borderless sidebar. tmux borders are inter-pane separators plus an optional top or bottom status row, and tmux does not draw the outer window edge, so a closed four-edge pane frame stays Zellij-only.

### The sidebar and the `after-new-window` hook

tmux has no tab template, so a hook supplies Zellij parity. `open_sidebar` splits a left sidebar into the initial window at the launch seed and installs a session-scoped `after-new-window` hook that re-runs the split in every later window.

The hook reads an absolute-column session option initialized from the resolved room share. Keypresses, adopted mouse drags, view changes, and reconcile passes refresh it, so future windows start at the share rendered for the current view.

Two prompt-cleanliness details ride along, both specific to tmux because Zellij births terminals from the layout template at their final size:

- Plain default-shell windows have an empty `pane_start_command`, so after the hook split establishes the final width, the hook respawns only that work pane as the user's shell, avoiding zsh's `PROMPT_SP` end-of-line marker.
- Pristine birth installs a one-shot `client-attached` hook for the first work shell. The detached session can draw zsh's first prompt before the attaching client applies its final size, and a resize during that draw strands the `PROMPT_SP` `%` marker above the prompt. The hook skips control-mode clients, respawns the birth work pane after the first real client attach, then removes itself. A room born without a probed terminal is healed when the first later attach normalizes its detached geometry and records `default-size`.

A quick tmux kill-and-restart still enters the pristine birth path: once the room transition proves the session absent, it purges sidebar heartbeat files and clears the width target from the prior incarnation before creating the replacement session. A fresh-but-dead heartbeat therefore cannot route the new shell through the later reconcile split that strands the `%` marker.

Reconcile converges widths only against an attached sized client's geometry or, while detached, the attaching terminal's probe. The detached path first aligns the session `default-size` and every window to that probe so the subsequent attach preserves the geometry, while a daemon reload with neither basis re-asserts structure without changing panes or the recorded width. When `open_tab` temporarily expands a freshly-born window to the widest attached client, it re-asserts the sidebar at that live target before splitting agent columns, then restores tmux autosizing. Layouts compile to tmux command sequences from the same layout IR Zellij uses.

The pane itself is best-effort: a fresh sidebar heartbeat suppresses producer relaunch, while a missing, stale, unreadable, or protocol-mismatched heartbeat lets `rimz start` or `attach` open a new pane. tmux has no resurrection, so `ensure_clean_session` is a no-op and the managed pane is tmux's only renderer. For supervised agent panes the producer derives the wrapper spawn command from the process backend, paralleling its `pane_process_start` derivation, so lazy-registering agents bind and panes group by worktree as on Zellij.

### The control-mode presence watch

The producer holds one size-excluded control-mode client, [`PresenceWatch`](../../crates/rimz/src/mux/tmux/presence.rs), with a single `refresh-client -B` subscription.

[`TmuxPresenceState`](../../crates/rimz/src/sidebar/presence/tmux.rs) retains only the native stream state needed to normalize out-of-order lines (panes, current windows, pending inactive panes, floating status, seeding) and feeds pane observations, focus, view switches, and incomplete-layout nudges through the shared host projector. The projector emits `PaneOpened`, `CommandChanged`, and `FocusChanged` under the same launch and sidebar policy as Zellij:

| tmux notification | Emitted event |
| --- | --- |
| `%window-pane-changed` | `FocusChanged`, naming the new active pane immediately. Deliberate in-window sidebar focus stays `FocusChanged`, so clicking or keying onto the sidebar is not bounced. |
| `%session-window-changed` | `FocusStranded` when the destination sidebar has a working sibling, otherwise `FocusChanged`. |
| Identity-free lines | `PanesChanged`. |
| Window close or layout shrinkage | `PaneClosed`. |

Each overlay reaches every fresh sidebar immediately, the producer verifies structural changes with a fresh frame, and the watch refreshes the presence stamp on attach and on each classified line. That puts tmux in the same event-mode pane TTL as Zellij while the stream is alive.

The watch rides the control-mode contracts in the [reference → control mode](../externals/mux-adapter/tmux-reference.md#control-mode). It attaches with `ignore-size,no-output`, holds stdin open because closing the pipe detaches the client, writes only `refresh-client -B` from its command allowlist, drains notifications promptly because tmux force-exits a stale reader, and drops `$TMUX` from the child env so a nested attach is not refused. The writable attach keeps tmux 3.7 `send-keys` usable when a headless session's presence watch is the sole attached client.

A dead, refused, or idle watcher degrades to the tmux poll, and the producer respawns with backoff. That is the parity rule in action: each backend owns an authoritative roster mechanism, while overlays stay latency hints.

### Notification passthrough

Desktop notifications are terminal-local. The sidebar renderer writes OSC 777 and BEL bytes into its pane, SSH carries them to the local terminal, and the terminal decides whether to show a banner or play a sound.

tmux forwards the OSC path when `allow-passthrough` is on, which is RimZ's default, wrapped as DCS passthrough. The sidebar raises its own pane to `all`, so its notification and graphics bytes also pass while its window is hidden.

Zellij currently drops notification OSCs, so `[notifications].desktop = "auto"` disables OSC there and notification handlers stay the portable channel. The full contract is in [notifications.md](./sidebar/notifications.md).

### tmux backend caveats

- **Minimum version is 3.5.0**, set by the room options `ensure_session` applies across supported hosts (`extended-keys-format` landed in 3.5), and the batched sequence fails at the first unknown option. The command surface alone needs only 3.2. `extended-keys`, `*:extkeys`, the `S-Enter` and `M-Enter` root bindings, and the bare-Esc normalization activate across supported versions. `rimz doctor` reports floor compliance.
- **Server-less `list_sessions` is empty, not an error.** tmux exits non-zero with `no server running` before the daemon starts; the backend swallows that shape and returns an empty `Vec`, matching the Zellij contract.
- **A server exits when its last session ends.** The endpoint is a stable path rather than a long-lived daemon, so a leftover socket file is normal and the next command births a replacement. Pane and window ids restart from `%0` and `@0` across that boundary; see [pane metadata](#pane-metadata).
- **The sidebar self-closes** the same way it does on Zellij, through the normalized pane listing, so a lone sidebar removes itself when its window's last working pane exits. The `after-new-window` hook runs `split-window` and, for plain default-shell tabs, `respawn-pane`, so it never recurses through `new-window`.
- **Resumed agents open as windows**, one `new-window` per remembered channel, named `#<channel>` and born `sidebar | agents…` as the hook docks the sidebar. `new-window -n` turns off automatic-rename, so the name is a stable idempotency key and a re-run never doubles a channel.
- **Per-test server isolation in CI.** Tests point the backend at a private runtime root, so `with_socket` receives the same derived path production would use one domain over. A test that pairs a live server with `rimz` subprocesses builds both from one runtime root, because the subprocess resolves its own endpoint rather than inheriting `$TMUX`.

## What both backends guarantee

- **Detach and reattach are multiplexer features.** RimZ does not reimplement them.
- **Runtime correctness needs no visible sidebar.** Hooks, `rimz message`, and supervised runs work headless.
- **The renderer is interchangeable and optional.** The native pane is the default on both backends, and correctness never depends on which renderer, or none, is attached.
- **The store survives host restart; processes do not**, unless a host supervisor is wired (tmux-resurrect, Zellij resurrect, systemd).

### What `rimz doctor` reports

The selected backend, versions and floor compliance, PATH-visible backend binaries, backend server-log issues, feature availability, sidebar liveness, RimZ runtime socket headroom, the managed tmux server socket path, any same-named session stranded on the legacy default tmux server with the command that retires it, Zellij IPC socket headroom when Zellij is selected, and any degraded modes.

The server-log scan in [`logtail.rs`](../../crates/rimz/src/mux/logtail.rs) reads a bounded tail, assembles logical multi-line records, stamps each from the backend's own line format, and drops everything at or before the `rimz doctor --clear` watermark. Each backend's classifier names an issue and places it: an ordinary lifecycle record (a client leaving, a closed pane's pty, a late action acknowledgement) is `expected` and folds into a counted line, and everything else stays `investigate` and earns its own. A record wrapped in a generic header is named by its `Caused by:` chain, so two unrelated failures under one wrapper stay two issues.

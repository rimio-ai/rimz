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

**Parity is the rule, and a push channel is a latency hint.** A backend-only capability always sits over shared truth. The Zellij presence plugin and the tmux control-mode watch both push topology, but they carry different weight. The tmux watch is optional: with it dead, the producer polls `tmux list-panes`. On Zellij the plugin's topology cache is what an ordinary `Cached` pane read returns, so with the plugin dead those reads fail until it loads again, and only callers that ask for an authoritative read reach Zellij's own listing ([reading the room](#reading-the-room)). Either way, a destructive decision never rests on a push channel.

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
| [`tab_name.rs`](../../crates/rimz/src/mux/tab_name.rs) | Pure pane-derived tab labels, name ownership, and `TabNameIntent`. |
| [`focus_anchor.rs`](../../crates/rimz/src/mux/focus_anchor.rs) | The two-phase focus intent, a rename-atomic runtime record, behind RimZ-initiated focus actions ([state.md → focus intent](./sidebar/state.md#focus-intent)). |
| [`focus_key.rs`](../../crates/rimz/src/mux/focus_key.rs) | Parsing and rendering the `[sidebar] focus_key` and `zoom_key` chords. |
| [`recovery.rs`](../../crates/rimz/src/mux/recovery.rs) | The guarded process sweep behind room teardown. |
| [`domain.rs`](../../crates/rimz/src/mux/domain.rs) | `ProcessDomain`: the guard every heuristic process kill passes. |
| [`capabilities.rs`](../../crates/rimz/src/mux/capabilities.rs) | Static backend facts, such as whether a view is a tab or a window. |
| [`binaries.rs`](../../crates/rimz/src/mux/binaries.rs) | PATH and live-server binary probes for `rimz doctor`. |
| [`winsize.rs`](../../crates/rimz/src/mux/winsize.rs) | The pre-provider repair of a pane tty born at 0x0 ([below](#the-daemon-view-and-resumed-births)). |

Zellij, [`zellij.rs`](../../crates/rimz/src/mux/zellij.rs) plus `crates/rimz/src/mux/zellij/`:

| Path | Owns |
| --- | --- |
| [`backend.rs`](../../crates/rimz/src/mux/zellij/backend.rs) | The `MuxBackend` implementation. |
| [`layout.rs`](../../crates/rimz/src/mux/zellij/layout.rs) | KDL layout rendering for birth, the daemon view, resumed agents, and background tabs. Pure `&options → String`. |
| [`background_plugins.rs`](../../crates/rimz/src/mux/zellij/background_plugins.rs) | Read-only resolution of the user's background plugins that the birth layout must keep ([loading](#loading-and-permissions)). |
| [`sidebar.rs`](../../crates/rimz/src/mux/zellij/sidebar.rs) | Sidebar birth, in-place recovery, and geometry convergence. |
| [`presence.rs`](../../crates/rimz/src/mux/zellij/presence.rs) | Plugin materialization, identity, load, retire and upgrade pipes, and the reload upgrade outcome. |
| [`pane_topology.rs`](../../crates/rimz/src/mux/zellij/pane_topology.rs) | The topology cache the plugin publishes: schema, path, freshness window, and the desired-presence record beside it. |
| [`tab_owner.rs`](../../crates/rimz/src/mux/zellij/tab_owner.rs) | Locked session-scoped tab ownership, keyed by stable tab id. |
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
| [`layout.rs`](../../crates/rimz/src/mux/tmux/layout.rs) | Pure tmux layout trees: reads of the checksummed layout string and its JSON form, layout-string writes, and the lift that places a directional neighbour beside a stack ([collapsed stacks](#collapsed-stacks)). |
| [`options.rs`](../../crates/rimz/src/mux/tmux/options.rs) | Room options, key bindings, hooks, and sidebar-pane classification. |
| [`presence.rs`](../../crates/rimz/src/mux/tmux/presence.rs) | The control-mode presence watch. |
| [`parse.rs`](../../crates/rimz/src/mux/tmux/parse.rs) | Command-output parsers. |

Both backends take named keys and bracketed-paste markers from [`pane::keys`](../../crates/rimz/src/pane/keys.rs). The presence plugin is its own crate ([below](#the-zellij-presence-plugin)).

Three modules outside `mux/` complete the picture. [`room/`](../../crates/rimz/src/room/mod.rs) sits above the trait and owns managed identity, admission, birth and reset, the pre-attach health gate, and presence-load ordering ([session lifecycle](#session-lifecycle)). [`sidebar/presence/`](../../crates/rimz/src/sidebar/presence/projector.rs) turns normalized transitions from either backend into typed `SidebarEvent`s. [`wakeup/`](../../crates/rimz/src/wakeup/mod.rs) sits below both and is the wire that carries those events into the room host from other processes (Zellij's `rimz sidebar wake`, CLI and hook producers): the renderer heartbeat record, the versioned envelope, and the best-effort datagram send, which this seam uses without importing the sidebar. The host's own tmux watch hands its events to the data plane in memory.

## Choosing a backend

[`auto_detect_backend`](../../crates/rimz/src/mux/selection.rs) takes the first match:

1. the `--mux <name>` flag,
2. the active environment (`ZELLIJ` or `ZELLIJ_PANE_ID`, then `TMUX` or `TMUX_PANE`),
3. `[mux] default` from per-machine config, skipped when it names an uninstalled backend (birth records a `mux.default` warning through `room/judgment.rs`),
4. the installed binary, tmux preferred when both are present.

The flag and the environment decide before config loads, so a command run inside a live room always addresses that room.

Room identity is path-derived and shared across backends, so every worktree of one repository resolves to the same recorded session name on either backend. A new session takes the state directory name, and a live session keeps its recorded name until its next birth ([the workspace record](./store.md#the-workspace-record)).

Room entry without `--mux` does not trust selection alone, because the room may live on the backend selection would not pick. [`pick_mux_for_session`](../../crates/rimz/src/room/session.rs) lists Zellij, then tmux, and takes the first that lists the recorded session; only when neither does it fall back to `auto_detect_backend`. Each listing retries up to `LIST_SESSIONS_ATTEMPTS` (3) times, `LIST_SESSIONS_RETRY_DELAY` (250 ms) apart, except that a timeout or a denied socket skips that backend at once with a notice. Start attaches to the owner it finds. Reset tears the room down there, then rebirths on the resolved default, and its socket preflight judges that default, so an unbirthable rebirth refuses before teardown. Web hands the session to the writable browser daemon, whose shim attaches through the owning backend.

Selection admits nothing: whether a birth may proceed is [room admission](#room-admission).

## The `MuxBackend` trait

[`MuxBackend`](../../crates/rimz/src/mux/mod.rs) is the backend-neutral command seam. Native readers also live in the adapters: tmux's shared width memo samples session geometry, the tmux [control-mode watch](#the-control-mode-presence-watch) spawns its own long-lived client, and `rimz uninstall` asks the ambient `$TMUX` server which session it runs in (`crates/rimz/src/cli/uninstall.rs::current_tmux_session`).

| Group | Methods | Notes |
| --- | --- | --- |
| Session lifecycle | `name`, `ensure_session`, `attach_command`, `attach_existing_command`, `attach_readonly_command`, `attach_may_drop_early`, `detach`, `kill_session`, `list_sessions`, `list_sessions_within`, `session_liveness`, `version` | The attach methods return a `CommandSpec` for the CLI attach runner instead of running it. |
| Pane inventory | `list_panes`, `cached_pane_roster`, `client_view`, `view_geometry`, `pane_content_size`, `confirm_resume_tabs` | See [reading the room](#reading-the-room). `confirm_resume_tabs` reads tab structure only, never pane content. |
| Pane I/O | `require_pane_in_session`, `capture_pane`, `send_keys`, `send_key`, `paste_text` | `paste_text` writes one bracketed paste; the submit Enter is a separate `send_key`. Chunking, failure cleanup, and the write lock are in [messaging.md → writing to the pane](./harness/messaging.md#writing-to-the-pane). |
| Structure | `split_pane`, `append_companion_pane`, `open_tab`, `rename_tab`, `open_sidebar`, `open_background_view`, `close_pane`, `close_pane_on_exit`, `close_view_floating_panes` | Callers pass backend-neutral argv and layout geometry. |
| Focus and geometry | `focus_pane`, `toggle_fullscreen`, `nudge_sidebar_width`, `record_sidebar_width_default`, `register_room_key`, `lagging_views`, `lagging_view`, `fit_views` | The last three fit hidden tmux windows ([attach and the terminal](#attach-and-the-terminal)); other backends inherit no-op defaults. |
| Health | `probe_session_health`, `ensure_clean_session`, `reconcile_sidebars`, `purge_resurrection_cache`, `resurrection_cache_paths`, `session_accepts_agent_close` | Several default to a no-op because they answer a Zellij-only question. |
| Presence | `ensure_presence_plugin` | Zellij only; tmux keeps the no-op default because its control-mode watch already pushes. |

A method with a sensible cross-backend answer carries a default implementation, so a backend implements only what it does differently. `ensure_clean_session` and `purge_resurrection_cache` exist because Zellij resurrects sessions and tmux does not; tmux takes the no-op and calling code stays branch-free.

Everything correctness-critical stays above the trait and is identical across backends: the store, the run and wakeup sockets, the event schema, the trust gate, and the agent hooks.

### Structure calls

`append_companion_pane` reads the anchor's view before birth and adds unfocused to its single stack, founding one on a lone work pane. The sidebar may be the anchor but is never the split target: the backend picks the bottom-most work pane, excluding chrome and floating panes. A shared floor of 10 content rows for the expanded member after the add controls numbered overflow; a `Full` result guarantees no payload ran. Errors preserve the mux failure and identify its phase: `CompanionAppendErr::BeforeSpawn` guarantees no payload ran because preparation failed, while `Spawn` means the spawn command or reading its reply failed and a payload may be running. Preparation is the view read on both backends and, on tmux, the stack read plus the one batch that clears the members' `@rimz_stack_open` marks and resizes the target before `split-window`. A tmux `split-window` that prints no pane id is a `Spawn` error, and a stack mark or close-on-exit retention pin that fails after the spawn only warns, because the pane already runs its payload (`crates/rimz/src/mux/tmux/backend.rs::TmuxBackend::split_pane_phased`). Grids and manual splits are full, as are zoomed tmux windows and Zellij tabs holding a fullscreen or suppressed pane. Zellij below 0.45 always answers `Full` to preserve client focus. No balancing or geometry confirmation follows the spawn. tmux uses the shared spawn-argv classification described under [Pane metadata](#pane-metadata), alongside its title and current-command checks. The [pane-zone rules](./harness/subagents.md#pane-zones) describe uncertain errors and Zellij's accepted shrink-before-spawn edge.

`TabOptions::after` opens a new view immediately after the view containing an anchor pane; `None` appends. tmux resolves the pane's session and window id and runs `new-window -a` only when the anchor belongs to the target session; an unresolvable or outside-session anchor appends in the target session instead. Zellij appends, then runs `move-tab left --tab-id` once per neighbour to cross, without changing client focus (`ZellijBackend::move_new_tab_after`). Placement is best-effort on both backends and the launch continues on failure: tmux leaves the view appended, and Zellij leaves the tab wherever its last successful step put it, with no rollback.

`LayoutPanes::focused_pane` names the pane that holds focus inside the new view, counted in layout order; `0`, or an index past the last pane, keeps the leading pane. Zellij marks that pane `focus=true` in the tab layout. tmux splits every pane with `-d`, then runs `select-pane` on it. Team launches and resumes set it to the leader's pane.

### Command discipline

Every control command runs through [`CommandSpec`](../../crates/rimz/src/mux/command.rs) under a deadline. At the bound the child is SIGKILLed and the caller gets `MuxErr::Timeout`, which carries the bound as a `Duration` and prints it exactly: whole seconds, or milliseconds for a bound with a fractional second (`crates/rimz/src/utils/time.rs::format_duration_bound`). The start-path `note:` and refusal lines spell their bounds the same way.

| Bound | Value | Why |
| --- | --- | --- |
| `COMMAND_TIMEOUT` | 30 s | A `zellij action` whose server died busy-loops at 100% CPU and would otherwise hang the caller. |
| `LIST_SESSIONS_TIMEOUT` | 3 s | A read-only local query on hot paths. |
| `SESSION_BIRTH_TIMEOUT` | 60 s | One Zellij `attach --create-background` birth client, which forks and boots a server from a cold binary rather than answering from a live one. It normally returns in under a second; a wedged client is killed and `rimz start` refuses. |
| `TAB_RENAME_TIMEOUT` | 2 s | Tab status is disposable chrome, so a stuck backend drops the rename instead of holding a worker for the structural bound. |
| `SESSION_PROBE_TIMEOUT`, then `SESSION_PROBE_RETRY_TIMEOUT` | 1 s, then 3 s | The start-path responsiveness probe (`mux_responsive_preflight` in `cli/room/mod.rs`). A first timeout prints a `note:` line and retries once with the longer bound. |

A healthy command answers in milliseconds, and most callers treat a mux failure as best-effort, so a bound degrades the caller instead of blocking it. The start path is the exception: when the selected backend also times out on the retry, `rimz start` refuses with the backend's recovery command and the other backend as a fallback, and a Zellij birth client that outlives `SESSION_BIRTH_TIMEOUT` makes `rimz start` refuse with the command and the bound. Backend-selection and notice probes skip their enrichment and continue; birth admission uses the room lock, not a rival query.

Zellij adds two rules inside these bounds, a rerun of its pre-dispatch refusal and a socket check before trusting its session list ([session liveness and refusals](#session-liveness-and-refusals)).

## Identity

### Pane and view IDs

Raw IDs stay inside the backend adapter, where native commands expect them (Zellij `terminal_3`, `plugin_1`; tmux `%3`). Everywhere else they travel normalized as `zellij:<raw>` or `tmux:<raw>`: in env vars (`RIMZ_PANE_ID`), store events, snapshots, and CLI arguments. [`pane_from_env_value`](../../crates/rimz/src/mux/mod.rs) is the one env-to-ID mapping, and `ensure_pane_backend` rejects a pane addressed to the wrong backend before any command runs.

`ViewId` names the view holding a pane (a Zellij tab, a tmux window) by the identity RimZ observes on every fast path: Zellij tab position (`tab_1`), tmux window id (`@3`). `PaneRef.view_id` is the flat read-side field, and the producer lifts it into `TabFrame.view_id` so per-view sidebar bookkeeping runs over typed topology.

**A view id is never the view's on-screen label.** Zellij's default tab names are number-shaped (`Tab #16`), so matching a positional `tab_15` against a tab named "Tab #15" joins two unrelated id spaces and picks the wrong tab in any session that has closed one. The label is minted at tab creation and does not follow position; `PaneRef.view_name` carries it for display only. Resolve "which view holds this pane?" through the pane id.

### Tab names

`rename_tab` takes a normalized pane anchor, never a display name or a positional `ViewId`. tmux accepts the pane directly as a `rename-window` target; Zellij resolves the pane through an authoritative listing and passes the tab's stable id to `rename-tab-by-id`.

An ordinary launch label is pane-name tokens joined by `+`: `<profile-or-kind>` for agents, the executable basename for commands, and the shell basename for empty command cells. The label keeps the first three tokens and adds `+…` for overflow, with no directory suffix (`opus`, `nvim+claude`).

#### Who owns a tab name

Ownership comes from a per-view `ViewNaming` that the mux listing carries into `TabFrame.naming`, never from a resemblance between the tab name and pane titles:

| Owner | Condition after stripping the status suffix | Producer may write |
| --- | --- | --- |
| Automatic | No record, tmux `automatic-rename` on | Nothing, including no glyph |
| User | No record otherwise, or the observed base differs from the recorded base | Status suffix only |
| RimZ | Record present and its base matches | Status suffix, survivor label, or shell name |

A user rename leaves the record in place but reads as User, and naming the tab back to its recorded base resumes RimZ ownership. A frame without a `naming` field decodes as no record, so it reads as User: it keeps its base while status suffixes come and go. Zellij has no automatic tab naming, so on Zellij no record means User.

#### Rename intents

Each rename carries a `TabNameIntent`:

| Intent | Effect |
| --- | --- |
| `Claim { pane_name }` | Name the tab, pin the anchor pane's launch name, and record the base and anchor founder. |
| `Status { observed }` | Add the projected status glyph; no record or option changes. |
| `Rest { observed }` | Clear a stale glyph; no record or option changes. |
| `Rebuild { observed, base }` | Write the surviving agents' label and status glyph, recording the supplied base without re-deriving it from another theme. |
| `Release { observed }` | Rename to the shell's name and clear ownership. tmux restores inherited `automatic-rename` and clears every pane's `@rimz_title`. Zellij retains pane names and keeps the explicit shell name. |

Every intent except `Claim` applies only while the tab still bears the name it was projected from. The room host projects `Status`, `Rest`, `Rebuild`, and `Release` from the tab's contents, without waiting for an agent-exit hook, and applies them off-thread from a frame it observed earlier. Without the check, a projection from a frame that predates an in-place launch would overwrite that launch's `Claim`, and nothing would converge back. A miss is a silent `Ok`: the next frame projects again from the current name. `Claim` is unconditional.

The two backends enforce the check differently:

- **tmux** runs the whole intent in one command list. It stashes the observed and target names in `@rimz_rename_observed` and `@rimz_rename_target`, runs the intent's commands under `if-shell -F` on `#{==:#{window_name},#{@rimz_rename_observed}}` with the new name read back from `#{@rimz_rename_target}`, and unsets both options. No user-chosen name passes through format or command parsing.
- **Zellij** has no conditional rename. For owner-writing intents it holds the owners lock across the listing, the comparison, `rename-tab-by-id`, and record publication, so an external user rename can still slip between the check and the dispatch. `Status` and `Rest` stay lock-free.

The tmux pane listing replaces commas in window names with `_` and trims edge whitespace, so a window whose name has a comma or edge whitespace never matches its raw `#{window_name}`. Such a tab gets no projected glyph, and its user-chosen name is never rewritten. Every managed tmux client passes `-u`, so Unicode names read back intact whatever the caller's locale.

The status suffix is chrome, rebuilt from projected agent state, and never identity or durable truth. Name-based birth and resume checks strip built-in and configured status suffixes before comparing their idempotency keys.

#### Founders and the survivor label

A founder is a work pane the naming record credits with the tab's name. Every RimZ-named tab birth records its base and its founders, command panes included, and an in-place `Claim` records the anchor alone. tmux's unnamed first window stays automatic and unowned until claimed. A founder stays active only while it is present in the tab and hosts a live agent (an agent row or `hosted_agent_kind`); exiting into a kept shell counts as leaving.

While any founder hosts a live agent, the birth base stays. Once all founders leave, the base follows the surviving agents on every tick:

1. Order work-pane titles by `first_seen_at_ms` ascending, missing stamps last, with the canonical pane id as tie-breaker.
2. Join them through the same three-token label helper. An empty survivor label keeps the existing base, and status changes still apply.
3. Spell the base the way the view reads it back: tmux maps `:` and `.` to `-` and commas to `_`, and trims edge whitespace; Zellij keeps the raw label.
4. With no surviving agent, release to the shell name, or only clear a glyph when the base already equals it.

Floating agent panes count as survivors; sidebar chrome and daemon-host panes do not. `Rebuild` preserves the original founders. Scoped `#` and `team:` bases, including tmux's sanitized `team-`, receive status changes only, never a rebuild or release. A restart that replaces the founder pane enters follow mode and normally keeps the same profile label.

#### Where the record lives

tmux keeps the record in window options: the sanitized base in `@rimz_tab_base` and the space-separated raw founder ids in `@rimz_tab_founders`, both readable per pane through `list-panes -F`. On tmux 3.7c both survive `move-window` and `swap-window`. `break-pane` leaves the record on the original window, and `join-pane` makes the joined pane read the destination window's record. A named birth publishes its final name only after recording the initial founder, because window options chained after a detached `new-window` would address the previously active window.

Zellij keeps the record in the runtime lane cache `tab-owners.json`, owned by `mux/zellij/tab_owner.rs`, as `{version: 1, session_name, tabs: {<stable tab id>: {base, founders}}}`. The file follows these rules:

- Writes are locked read-modify-write, rename-atomic without fsync, and prune entries to a live tab listing taken inside the same lock. Listing and cache readers stay lock-free.
- Birth holds the lock from founder listing through record publication. When founder resolution fails at a confirmed new-tab birth, founders are empty, and pruning waits for a successful live listing.
- Session birth clears the file, because tab and pane ids restart on resurrection (observed on 0.45.1: both `1` became `0`). A session or version mismatch reads as empty.
- Cached and native listings join the file on `PaneTopologyPane.stable_tab_id`, while the public view key stays `tab_<position>`. The topology field `tab_position` also decodes from the key `tab_id`, which means position. A plugin that publishes no `stable_tab_id` reads as User until `rimz reload` upgrades it.

Zellij birth identifies its new named tab in topology as a namesake whose stable id no namesake held before the call. Stable ids are reused after the highest-id tab closes, so `new-tab` stdout serves only as a tie-breaker among new namesakes. Founders resolve from an authoritative listing that excludes plugins and sidebar chrome, and layout resume tabs resolve once by birth label after confirmation.

Zellij session birth holds its temporary layout until the server's [authoritative roster](#pane-metadata) (`action list-panes`) shows the sidebar and a second live terminal, within the bounded layout wait. A session with no client attached gets plugin topology only during startup, so birth cannot use that cache as proof, and a dead session's cached topology does not count.

### The identity pin

Session birth stamps the room's identity (`RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, through [`pin_env`](../../crates/rimz/src/workspace.rs)) and the registry's adapter-enrichment environment into the session. Every pane inherits it, and so does every agent and every in-pane hook child. The [room host](./sidebar/state.md#the-room-host-and-its-attachments) is pinned by its spawner from the verified workspace record, not by inheritance. A daemon-routed hook that misses the pin recovers it from the in-pane agent process ([adapter.md](./agents/adapter.md#hooks-resolve-the-room-they-live-in)).

Each backend pins at its own birth seam, and they differ in whether a live session can be updated:

- **tmux** sets identity and adapter env on `new-session` and re-asserts them idempotently on every ensure, so panes born later carry the current values. The same seam stamps `COLORTERM=truecolor` when the launching terminal advertises it, because tmux births panes under `tmux-256color` with an empty `COLORTERM`, and room apps read RGB capability from the session environment.
- **Zellij** carries the map on the spawning client's environment, from which the per-session server and every pane fork. There is no post-birth re-assert, so a session keeps the environment it was born with until rebirth. Zellij birth also stamps `TERM=xterm-256color` when the spawning environment has no `TERM`, so a non-PTY birth (remote-web prep, a headless launch) still yields panes with usable terminfo.

Server birth removes two sets of inherited variables through `CommandSpec`, so a room started from inside another room or another multiplexer gets its own identity:

| Removed at server birth | Names | Why |
| --- | --- | --- |
| Ambient mux identity (`CommandSpec::without_mux_context`) | `TMUX`, `TMUX_PANE`, `ZELLIJ`, `ZELLIJ_PANE_ID`, `ZELLIJ_SESSION_NAME` | The room's multiplexer supplies its own identity to every pane it forks, so a room started inside the other multiplexer resolves its own backend. |
| Remote client context (`mux::REMOTE_CLIENT_ENV`, seven names) | See [remote channel](./remote.md#the-command-rimz-runs-on-the-host) | A nested `rimz start` must not reuse the laptop's version, reconnect posture, or attach context. |

These are birth rules, not chokepoint scrubs: later in-room commands keep their pane context. The launching `rimz start` and the Zellij attach client also keep the remote context, because predecessor reaping needs the attach client's lineage. `TERM`, `COLORTERM`, the workspace pin, `TMUX_TMPDIR`, and `ZELLIJ_SOCKET_DIR` stay.

tmux needs one more step, because its server is shared and outlives any one session. Every session ensure marks both sets for removal in the managed server's global environment (`set-environment -g -r`), so a server whose global environment already holds them is repaired. The repair runs before `new-session` to protect its first pane (an absent server is expected and is not started), and again afterward on both the birth and the duplicate-session paths. Newly forked panes still get tmux's own `TMUX` and `TMUX_PANE`, and processes already running keep their environment. A live Zellij server keeps its birth environment until rebirth.

Both chokepoints (`ZellijBackend::cmd` and `tmux::tmux_cmd`) also give every mux child the user's `TMPDIR` back. A launch moves the agent's `TMPDIR` to its temp unit and saves the value it replaced as `RIMZ_USER_TMPDIR` ([sandbox.md](./sandbox.md#environment-pins)); without the restore, a room started from an agent's pane would hand that unit, which GC later removes, to every pane its server forks. `CommandSpec::restore_user_tmpdir` applies the one rule, which `child_process::user_temp_env` decides: a saved value becomes the child's `TMPDIR`, an empty save removes `TMPDIR`, and no save leaves the inherited value alone. With a save, the provider temp-root keys the launch listed in `RIMZ_TEMP_ROOT_KEYS` (such as `CLAUDE_CODE_TMPDIR`, which names the same unit) are removed too, so mux code never names a provider; a `CLAUDE_CODE_TMPDIR` the user set before the launch is dropped rather than restored, since nothing saves it. Neither save reaches the child, so a shell in that room launches agents that save afresh. An explicit spec `TMPDIR` (a test's private runtime dir) still wins, and `ZELLIJ_SOCKET_DIR` stays, since it already names the base the user's own environment resolves.

Rebirth re-pins on both backends. Panes also carry a pane-scope pin ([`pane_pin_env`](../../crates/rimz/src/workspace.rs)): `RIMZ=1`, `RIMZ_WORKTREE_PATH` set to the tab's cwd, and `RIMZ_CHANNEL` when the tab has a channel. A live tab applies it through `TabOptions.env` and a resume tab through `ResumeTab.env`, both rendered per pane (tmux `-e K=V`, Zellij an `env` argv prefix), so a re-seeded agent or a reborn `#channel` shell starts with the same identity a fresh launch gets. The harness builds `ResumeTab.env` from stored fields; the backends never derive a channel from a tab label.

`RIMZ_WORKTREE_PATH` therefore marks more than agent panes, and code reading it cannot take it as proof of an agent. The pane-scope pin goes to agent launches (fresh, forked, restarted, resumed, and supervised runs), `rimz pane split` shells, and reborn tabs. Two non-pane processes set the key alone, without `RIMZ=1` or a channel: a loop check, where it names the task's run directory on top of the session pin, and a worktree hook, where it names the worktree the hook fired for. The room's first shell carries only the session pin.

A pane's launch-name pin is separate from room identity. tmux stores it in the pane option `@rimz_title`, sanitized like window names (`:` and `.` become `-`), and removes it from every pane in a released window. Zellij uses the pane name and keeps it on release.

### Pane metadata

`list_panes` reports each pane's id, view, foreground command, optional spawn command, optional title, cwd, and floating flag. The title is the pinned launch name when present, otherwise the multiplexer's pane title; the published pane frame keeps it for tab-name reconciliation.

The sidebar uses the foreground command for display, the spawn command for identity only while the pane root still runs the spawn program (or the foreground is briefly unreported), and cwd for worktree grouping ([sidebar.md → presence model](./sidebar/sidebar.md#presence-model)). A foreground shell therefore demotes a pane's historical agent argv. Foreground, title, and cwd exist on both backends. Spawn is optional because Zellij omits it for panes created with `action new-pane`, while tmux exposes the static `pane_start_command`. **The parity floor for presence is command plus cwd**, which both backends meet.

Zellij adds two wrinkles. The foreground command reaches RimZ as a full argv string, so every Zellij source (the plugin cache on ingest and the authoritative listing) clears a `rimz agents <spec>` launcher command before a consumer reads it. A layout-named `rimz-sidebar` pane always reports `rimz-sidebar` as its foreground, so it filters as chrome even when Zellij omits the command fields.

tmux reports `rimz-sidebar` as the command when either the title matches or `pane_start_command` decodes to a `sidebar serve` argv after the executable. The shared `spawn_command_is_sidebar_serve` predicate in `mux/tmux/options.rs` unwraps tmux's whole-command quoting before inspecting argv. A hook-born sidebar is therefore chrome from its first listing, before the renderer publishes its title, just as a Zellij sidebar is chrome from its birth name.

tmux exposes `pane_floating_flag` from 3.7; older supported releases expand the unknown format empty and report every pane tiled. Floating agent panes stay addressable but out of the room-row projection, and a self-closing sidebar view closes same-view floating panes before its tiled anchor exits.

Neither backend reports a per-pane process start, so RimZ derives `pane_process_start` from the process backend ([produce/panes.rs](../../crates/rimz/src/sidebar/produce/panes.rs)) and uses it as the key that refuses a match against a reused id. Neither backend recycles an id within one server, but both start a new id space with a new server, so a durable record naming `terminal_3` or `%3` can outlive the pane it described:

- Zellij terminal and plugin ids come from increasing counters within one server ([upstream allocation](../externals/mux-adapter/zellij-reference.md#types)). A new server, a resurrected session included, numbers from 0 again.
- tmux `%id`s are unique within one server lifetime, but the RimZ server exits when its last session ends, and the next command births a replacement that numbers from `%0` again.

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

On Zellij, `list-clients` replies without the `CLIENT_ID` header are foreign, not evidence of detachment ([upstream format](../externals/mux-adapter/zellij-reference.md#cli-action-reply-association)). The backend retries them within the caller's command budget, then returns an error so callers take their existing error path. A header-only listing is a genuine zero-client view.

`ClientView::unique_live_focus` resolves a single focus: fresh attached-client evidence counts only when every observation names the same live pane. Detailed client rows outrank summarized pane ids, and dead summarized panes do not invalidate one distinct live pane.

`PaneFrame.focused_pane` is the session's focus register:

| Sample | Effect on `focused_pane` |
| --- | --- |
| Fresh, every attached view names one distinct live terminal, plugin sample not withheld | Set to that pane |
| Fresh, and empty, plugin-only, dead, or distinct, plugin sample not withheld | Cleared |
| Unavailable, plugin sample not withheld | May hold the prior live value only when the live pane-ID set is unchanged |
| Zellij plugin sample withheld (`clients_withheld: true`) | Admit the host sample only when it names a pane absent from the prior frame's pane-ID set, or the prior focused pane is no longer live; otherwise keep the live prior with `FocusOrigin::Prior`, even for an empty or unavailable sample |
| Realtime `FocusChanged` between pulls | Updated |

The attached-client sample is the only runtime authority for focus. Hidden tabs carry no RimZ focus state, and the view's `ViewState::baseline_pane` is only a local highlight and restoration hint. `PaneRef` and pane topology carry no focus bit: `rimz pane list` reports identity and process context without an active mark, hook recovery uses a fresh unique client view to choose among plural candidates, and `rimz sidebar focus --toggle` requires the same unambiguous view. Upstream roster focus marks never enter RimZ's model, diagnostics, binding, or repair decisions.

### Jumping to a pane

`focus_pane` is a one-way jump that crosses views on both backends: Zellij switches to the containing tab directly, and tmux selects the pane, then its window, so the presence watch never sees the destination's stale active pane.

A RimZ jump that moves an attached client is wrapped in the two-phase intent in [`focus_anchor.rs`](../../crates/rimz/src/mux/focus_anchor.rs), a rename-atomic runtime record. `Requested` is written and wakes every renderer before the command, and acceptance moves the same nonce to `Applied`. A requested intent is judged on freshness alone, while native client observations confirm, supersede, invalidate, or fence an applied one ([state.md → verdicts](./sidebar/state.md#verdicts)). The separation matters on Zellij, where `action focus-pane-id` can move the visible pane and routed input without a matching `ListClients` update. One jump carries no intent: after a Zellij focus-taking directional split, the jump to the new pane goes out bare, because no renderer's roster holds that pane yet ([focus-taking spawns](#versions-and-focus-taking-spawns)).

### Room keys

The sidebar's in-pane keys fire only when its pane is focused, so a room-scoped chord reaches it from any pane. The multiplexer intercepts the keystroke before it lands in the focused pane.

- **`[sidebar] focus_key`** (default `Alt+p`) runs `rimz sidebar focus --toggle`. It focuses this session's `rimz-sidebar` pane, or returns to a deterministic working sibling when the sidebar is already current, and acts only when one unique fresh client view proves which is current. An unavailable or distinct view returns a non-mutating ambiguity error.
- **`[sidebar] zoom_key`** (default `Alt+g`) runs `rimz pane zoom`. With one unique attached-client focus it toggles fullscreen for the focused work pane. If that pane is sidebar chrome, RimZ resolves a non-chrome sibling in the same view, records and applies the focus intent, then fullscreens the sibling. With no sibling the view is unchanged.

[`FocusChord`](../../crates/rimz/src/mux/focus_key.rs) parses both chords once (`Alt` or `Ctrl`, with `M-`/`C-` prefixes and `-`/`+` separators). `Alt` is the default because it passes through the terminal, Zellij's locked mode, and tmux's prefix; `off` or an empty value registers nothing. Registration is best-effort at session birth, so a convenience key never blocks a room.

The backends bind the chords differently:

- **tmux** binds a server-global root-table key that carries no room identity and resolves the pressing pane's session at keypress.
- **Zellij** routes through the presence plugin. RimZ passes both chords in the plugin's load configuration, and the plugin installs them in one runtime `Reconfigure`, each as a `MessagePluginId` action to its own instance. A client attach reloads its stock bindings, so the plugin replays already-applied runtime configuration once per observed increase in connected clients. The focus pipe runs the sidebar toggle and the zoom pipe runs `rimz pane zoom`. The user's `config.kdl` is unchanged, and the bindings end with the session.

## One sidebar per view

Every occupied view should hold exactly one live sidebar pane. `reconcile_sidebars` converges toward that in place, without disturbing working panes and without recreating the session.

The planner in [`reconcile.rs`](../../crates/rimz/src/mux/reconcile.rs) is pure. Each backend groups its native listing into `ViewSidebars` (the view's sidebar panes in mux order, plus whether the view holds working panes or daemon hosts) and supplies native add, close, and verification effects. A view is occupied when it holds a working pane or a daemon host, so the [daemon view](./rimzd.md) gets the same verdicts as a working view; reconcile never closes the hosts themselves. `plan_reconcile` emits at most one verdict per view:

| View state | Verdict |
| --- | --- |
| Occupied, one sidebar claimed by a live renderer | `CloseDuplicates` for the rest, or nothing when it is the only one |
| Occupied, no sidebar | `Add` |
| Occupied, sidebars exist but none is claimed | `Replace`: add first, close the old ones only after the new pane mounts and proves its heartbeat; Zellij 0.45+ splits a docked old sidebar rather than a work pane when the old sidebar is at least 10 columns wide |
| Orphan (no working pane, no daemon host) | `CloseDuplicates` for every sidebar, so a wedged renderer collapses with its view |
| Orphan, while a live renderer is unlocated | `CloseDuplicates` for all but one sidebar |

The renderer applies the same empty-view rule without waiting for reconcile: once the last working pane exits, the sidebar closes itself ([sidebar.md → self-close](./sidebar/sidebar.md#self-close)), and both multiplexers remove a view when its last pane closes. A companion subagent tab therefore collapses after its final child finishes, with no close-tab primitive.

`SidebarLiveness` carries the claims: `claimed_panes` from fresh renderer heartbeats, plus `young_panes` for pane-attributed sidebar processes inside the first-heartbeat grace window. `has_unlocated` marks a live renderer whose pane could not be placed, and keeps the planner conservative. A fixed `SidebarLiveness` also serves as a source for direct callers.

Each backend samples its `SidebarLivenessSource` exactly once, after the native pane listing its planner uses, and before planning, any sidebar add or close, and Zellij's pre-execution geometry repair. A sidebar born at any point before the listing returns is therefore judged on evidence read after its birth.

One gap remains on both backends: a pane listed before its child execs shows no `sidebar serve` process yet and can still be replaced. tmux inserts a new pane, forks, and returns before the child execs. Zellij spawns the child before it inserts the pane, which narrows the gap without closing it. The `pre_exec` closure in Zellij v0.45.1's [`handle_openpty`](https://github.com/zellij-org/zellij/blob/v0.45.1/zellij-server/src/os_input_output_unix.rs) closes every descriptor from 3 up, the close-on-exec socket `Command::spawn` waits on among them, so the spawn returns before the exec, and the exec races the pane's insertion.

Every Zellij repair pass reads the live pane list, and judges width only on tabs that a live `list-clients` client views, plugin-pane focus included. A failed client listing leaves structure repair enabled but defers width judgment. An ordinary attach therefore converges a viewed tab's off-spec width too. Repair leaves hidden tabs to their renderers, which can converge equal-width tabs with one attached client.

Attach and explicit repair share a per-workspace, per-`(mux, session)` advisory reconcile lock. The lock covers reading the executable build and the entire backend pass, including its post-listing heartbeat and young-process sample, so a waiter sees the sidebar its predecessor just mounted. The young-process clock is read at sample time, not when constructing the source. Attach waits at most two seconds and skips a busy pass; explicit repair waits at most sixty seconds and reports a busy session. Layout births, the tmux new-window hook, renderer self-close, and the two-authoritative-read orphan-process reap remain outside that lock.

Replacement is add-before-close. [`mount_proof.rs`](../../crates/rimz/src/mux/mount_proof.rs) reads heartbeats from the room runtime paths supplied in `SidebarPaneOptions`, not from ambient workspace resolution. It waits up to six seconds for the new pane to publish a heartbeat naming the expected build before the old pane closes. A failed add leaves the user with the sidebar they had, and a pane running a stale binary never counts as the repair. After the closes, tmux snaps kept and newly mounted sidebars to their per-window targets. Zellij verifies each completed replacement from the live pane list, even if a later view failed, under the attached-client gate and within its native stop band. Width handed over by a closed pane therefore does not become the replacement's target.

`SidebarRecovery` tallies the pass (`recovered`, `closed`, `failed`, `deferred`, `redocked`, `misdocked`) and carries `repairs`: each native view id with its successfully closed pane ids in close order and an optional verified added pane id. Only landed structural effects produce an entry, including effects before a transaction fails; `recovered` still counts only completed add or replacement transactions. The shared `reconcile_session` funnel emits one [`sidebar_view_repaired` diagnostic](./diagnostics.md#event-taxonomy) per entry after the backend returns its report; a pass without structural effects writes nothing. `execute_reconcile_plan` stops at the first failed verdict and counts it and every remaining verdict as `failed`. The pass is one best-effort attempt: nothing retries it, and a failure never escalates to a session rebirth. The next attach, `rimz sidebar repair`, or `rimz reload --repair` runs a fresh pass; no host tick reruns it.

### Width

[`width.rs`](../../crates/rimz/src/mux/width.rs) resolves one room target from configured policy and live geometry, and [`sidebar_pane/app/width_control.rs`](../../crates/rimz/src/sidebar_pane/app/width_control.rs) is the renderer-local controller that converges each pane toward it.

Zellij controllers on one room plane share the topology parse and its pane-keyed geometry table through `WidthMemo` in [`zellij/raw_pane.rs`](../../crates/rimz/src/mux/zellij/raw_pane.rs). The full file stamp, including device and inode, invalidates the memo on atomic replacement even at equal length and mtime. An unchanged stamp needs only a metadata check and table lookup, not another JSON read or tab-width derivation. Session identity, current freshness and each caller's observation floor are checked on every lookup, never memoized as a verdict. Each controller still supplies its own fd width, sibling count and observation stamp, and owns fullscreen hold and resize feedback.

tmux controllers share one session-wide `list-panes -s` attempt through `WidthMemo` in [`tmux.rs`](../../crates/rimz/src/mux/tmux.rs), under a plane-owned mutex separate from client views. The sample records its start before the command and maps each pane to its window width; failures are shared too. Every request rejects a sample started after the wall time read under that mutex, so a backward clock step cannot keep old geometry fresh. An idle backstop accepts an attempt less than `IDLE_RETRY` old at its own deadline. Event floors require a strictly newer successful sample (ties re-sample), so a baseline's unchanged floor never pins a failed attempt. Floorless requests require a start at or after their own request instant, including a sample started while waiting for the mutex. A pane absent from the sample falls back to a live per-pane window-width query.

**The record.** The room-runtime record in [`width_target.rs`](../../crates/rimz/src/mux/width_target.rs) holds a `WidthPermille` (tenths of a percent of the full view) and a pin flag. An unpinned target follows `theme.display.width_percent` (a set value clamps to 10 through `max_percent`; unset means a fixed 30% while pets are enabled, since the pet dashboard needs the wide default, and otherwise 30% for a view wider than 240 columns and 25% at or below) and applies `theme.display.max_cols` whenever live view geometry is known. Every resolved or pinned share is bounded by `theme.display.max_percent` (default 50, clamped to 10-90). An explicit choice may exceed `max_cols` but never `max_percent`, and keeps its proportion when the terminal resizes; the 24-column floor still wins on narrow views. Stored shares above the bound resolve at the cap without a write on read; `adopt` heals the record only when it differs. A genuinely new session clears the record and returns to configured policy.

**Resolution.** Resolving produces a `SidebarTarget`: one share, the configured cap, and whether the user pinned it. That answer and the `max_percent` rendering bound cross the backend seam, not the percentage policy. tmux renders columns against the view geometry it measured, rounding fractional shares up and clamping an unpinned default to the cap. Zellij layouts, renderers and repair all use `SidebarTarget::zellij_step`: the multiple of 5 whose ceiling-estimated columns are closest to that column target, ties narrower. Candidates must reach 24 columns and fit `max_percent`, rounded down to a multiple of 5; when those bounds conflict the smallest step reaching 24 wins. Without geometry the step is nearest the share itself, ties narrower, within the percentage bound. Resolution is read-only except in two cases: birth geometry from a real terminal probe, or a live backend viewport proven for the current event, may adopt and rewrite the room share. On Zellij one sampled client proves its own tab and equal-width tabs; two or more require `panes[].tab_viewed`, the plugin's observation that a client has viewed the tab since the session last had zero clients. The host remembers its last client-to-stable-tab sample across withheld or sample-less publishes. Structural reconcile instead assigns that flag from its live client-view snapshot and never steps hidden tabs. A resolve without geometry keeps an existing share; with no record it returns the narrow policy fallback for that call without persisting it. Columns without geometry use the bare cap, because a detached layout's eventual view width is unknown.

**Keypress steps.** Every view receives the same exact permille share. An `a`/`d` keypress moves one backend column step, converts the result to a share, pins it atomically, and broadcasts `WidthTargetChanged`; each renderer resolves the share for its own view and converges with at most one mux resize in flight. On Zellij a press is ruled against the topology and the room share read at the burst's open, so a key after an attach or resize does not wait for the controller's own re-proof. On tmux, where the geometry read is a subprocess, a press is ruled against the stored view, which the controller re-proves off the key path. The cold case is the exception: with no stored view width, `WidthController::adjust` asks the shared tmux memo for a fresh sample on the key path.

- **tmux** applies the absolute target in one command, and a narrower intent clamps at the 24-column floor (`MIN_ADJUSTABLE_WIDTH`).
- **Zellij** resizes only relatively, in steps of about 5% of the view. Every percentage RimZ writes is on the same 5% grid, and controllers target that grid's columns. A wider keypress uses the floor of the native step, so it never targets past the next reachable width; a narrower keypress uses the ceiling. The ceiling is also the stop-step width, whose settled band admits exactly one grid step at native spacing. A drag or a percentage Zellij reassigned after a neighbouring pane closed or repair split a work pane can retain an off-grid offset within that band. Each accepted intent issues one relative step per resize-feedback event or per `FEEDBACK_TIMEOUT` (1 second). Width keys stop at the first step reaching 24 columns and the last step permitted by `max_percent`. Missing topology rejects either direction, because a fabricated view would pin the wrong room-wide share.

**Convergence.** The renderer controller and reconcile-time repair share one settled band. For target `t` and backend stop step `s`, a width is settled when its distance from `t` is at most half of `max(s, 1)`, and a step is issued only while moving toward `t` can land strictly closer. tmux steps one column and lands exactly on `t`. Zellij reserves the ceiling of its relative step, and the renderer widens that estimate when observed feedback is larger. A step that increases the distance earns one reverse step: the renderer parks a reversal that restores at least the prior distance, and reconcile, which keeps no state between invocations, stops after its one reversal.

**Parking.** The renderer parks instead of spinning. A step whose feedback never arrives is retried once, then its no-progress cycle is retried once after `IDLE_RETRY` (5 seconds), which also checks the viewport and re-derives the target if the proven viewport changed. tmux backstops share an attempt for that interval, so an unreported viewport resize is caught within two idle retries (10 seconds), plus scheduling and command time. After `MAX_NO_PROGRESS_CYCLES` (2) unacknowledged cycles the renderer holds until its width moves, a roster change lands, or the target changes, and a convergence that spends `MAX_STEPS` (32) steps parks on its step budget. A transient resize failure, or an attach after a detached birth, therefore gets one delayed retry. Reconcile relies on its later passes instead.

**Structural changes.** Typed pane-open and pane-close events, and sibling-count changes, re-arm convergence and correct an off-spec pane immediately. Re-arming keeps a resize already in flight and its original feedback deadline, so a burst of roster inputs cannot multiply the pending step. A topology-change event still marks structural evidence but proves geometry only while the baseline proof is pending, without re-arming convergence. The proof floor is the observation of the topology the event was derived from, or send time when the sender had none. An event can prove against its own viewed topology even when that observation predates the renderer's start; the baseline probe without an event still requires a topology at least as new as the renderer. Continuously tracked sibling counts are the pulled-truth fallback when an event is missed. The structural marker also keeps a stalled correction from being read as a mouse drag.

**Mouse drags.** The settle pass arms only when the measured pane sits outside the settled band. A view change re-resolves from config. Adopting a drag needs positive evidence: backend geometry exists, an unchanged sibling count was observed before the resize, no structural marker falls within `STRUCTURAL_GUARD_MS` (2 seconds) of the resize, and the post-resize sibling observation is at least that much newer than the resize. While evidence is pending, including when geometry is missing, the controller waits without nudging, so it does not fight a real drag. A settled resize without the earlier count is rejected as unproven and converges back to the target. The first measured movement consumes any remembered unacknowledged step: movement toward that step's target is late feedback from RimZ's own nudge and is never adopted, while movement away goes through drag classification. A drag that ends inside the band leaves the share untouched. Otherwise RimZ pins the measured share up to `max_percent` and broadcasts it once. Zellij renders that share as the nearest bounded step: the dragged tab stays inside its band without a nudge, except when a bound excludes its nearest step, and peers converge to the step. A drag wider than the largest allowed step can still be stepped back when `max_percent` is not a multiple of 5. If the nearest step is below the 24-column floor, the room target is the smallest step reaching it: at a 200-column viewport a 24-column drag selects the 30-column target. The usual half-step band still applies to the dragged pane.

### The fullscreen hold

Zellij fullscreen holds width convergence; it is not geometry to repair. The presence topology carries each pane's fullscreen bit, and any fullscreen pane holds its whole tab: Zellij geometry repair (`off_spec_sidebars`) skips the tab, and the renderer parks width control without arming its idle retry. A later topology observation with fullscreen cleared releases both. RimZ never resizes or redocks against the fullscreen pane's override geometry or its hidden siblings' stale geometry. Structural verdicts (`Add`, `Replace`, `CloseDuplicates`) still apply to a fullscreen tab.

## Session lifecycle

[`RoomContext`](../../crates/rimz/src/room/mod.rs) sits above the trait and owns the shared parts: managed identity, config derivation, birth, reset, pre-attach health, heartbeat purge, and presence-load ordering.

| Path | Owns |
| --- | --- |
| [`room/mod.rs`](../../crates/rimz/src/room/mod.rs) | `RoomContext`: managed identity, config derivation, presence load, and room keys. |
| [`room/birth.rs`](../../crates/rimz/src/room/birth.rs) | Admission, birth, health recovery, and reset transitions. |
| [`room/session.rs`](../../crates/rimz/src/room/session.rs) | Live room inventory, session-record lookup, the room claim, and mux choice. |
| [`room/teardown.rs`](../../crates/rimz/src/room/teardown.rs) | Teardown steps and incompatible-room replacement. |
| [`room/judgment.rs`](../../crates/rimz/src/room/judgment.rs) | Configuration problems the room opens without, kept for the sidebar and `rimz doctor`. |
| [`room/recovery.rs`](../../crates/rimz/src/room/recovery.rs) | Parked recovery on entry to a running room. |
| [`room/standing.rs`](../../crates/rimz/src/room/standing.rs) | Account standing for each provider kind at one project root. |

### Room admission

One mux session per project holds the room, and the room claim decides whether a birth may proceed. The claim is a `RoomLock` in [`disk/lock.rs`](../../crates/rimz/src/disk/lock.rs) at `RuntimePaths::room_lock`. It keeps a second backend or a renamed session from birthing a rival room. Backend selection decides nothing here, and no probe of the other backend does either.

`admit_birth` in [`room/birth.rs`](../../crates/rimz/src/room/birth.rs) runs on normal entry before hook detection, every startup prompt, and recovery inspection, before the `RoomContext` exists. It lists the selected backend's sessions and admits by the recorded session's state:

| Recorded session | Admission |
| --- | --- |
| Listing failed | Treated as live, with no claim |
| Listed, with a fresh sidebar heartbeat for this backend and session name, of any build | Reattach with no claim |
| Listed, with no heartbeat of its own | Take `recovery.lock` (`WorkspaceLock::acquire`, 30 s bound), then poll up to 2 s for that heartbeat or an exclusive claim, and refuse if neither appears |
| Absent | Take an exclusive claim within 2 s, and refuse on a live hold |

The poll in `crates/rimz/src/room/session.rs::claim_listed_room` waits out a concurrent birth without admitting a rival room's heartbeat, since only a heartbeat naming this backend and session counts. Every refusal uses the shared text in `claim_room`.

The admission travels with the room into `RoomContext::birth`:

- A claimed admission is held across the startup prompts and consumed by birth. A claimless one is decided again at birth, since the sidebar it trusted may have died during the prompts. Supervised birth runs the same admission when none is carried.
- Owner and login writes follow admission. A claimed birth holds `recovery.lock` until birth returns.
- The claim downgrades from exclusive to shared right after `ensure_session`, so pane supervisors can hold it too. On tmux that follows session creation. On Zellij `ensure_session` is a no-op, so the session is created later, by the sidebar launch, under the shared claim.
- After the final health recovery, birth waits up to 6 s for a fresh sidebar heartbeat matching its backend and session name, so a recovery rebirth's sidebar gets the whole bound. Expiry logs a warning naming the session and continues, and a failed sidebar launch skips the wait.
- The claim drops before birth returns, never across client attach.

After birth, the room stays held while a sidebar pane or a live supervisor lives. Every pane supervisor takes a shared hold before attaching and keeps it across host retries until exit. Once neither lives, the room is unheld, but reattaching it must claim before relaunching a sidebar, so another room's hold refuses it. A host restarting under live supervisors can refuse an attach while its heartbeat is stale; the attach succeeds once the host writes again.

Locks are CLOEXEC, so a birthing process cannot leave a hold in a mux server, and a supervisor re-exec drops and retakes its hold. Converting a lock between exclusive and shared releases and retakes the flock on Linux and macOS, which leaves a brief admission gap, as does a supervisor exec. Separate RimZ homes have separate locks.

Reset and replacement admit more strictly. Reset admits with zero wait before it prompts or tears down, through `claim_listed_room` for a listed session and `claim_room` for an unlisted one (`cli/reset.rs`). It holds any claim through teardown and drops it before rebirth. Incompatible-room replacement (`crates/rimz/src/room/teardown.rs::IncompatibleRoom`) admits only by exclusive claim, with no heartbeat bypass: start, attach from the cwd, and reset take a zero-wait exclusive claim before any teardown, re-check the layout under it, and hold it until both roots are removed. Reset takes that claim in place of its generic admission, before prompting.

### The pre-attach health gate

`open_sidebar` is best-effort and can be skipped or fail, so it cannot be the only thing between the user and a resurrecting attach. A normal managed entry probes a live session before setup side effects and threads that verdict through birth; when no reusable live verdict exists, birth runs `ensure_clean_session` to create or cleanly rebirth the session. Supervised birth calls `ensure_clean_session` directly. Both paths must return an attachable verdict before presence load and attach preparation.

| Session state | Gate action | Verdict |
| --- | --- | --- |
| Live and responsive | Keep the room after a bounded direct pane probe | `Healthy` |
| Live but unresponsive | Keep the room and refuse attach | `Unresponsive` |
| Absent | Birth from the layout | `Reborn` |
| Exited (Zellij resurrection record) | Delete, then birth from the layout | `Reborn` |
| Still not live after a rebirth | Nothing further | `Stuck` |
| A rebirth command outlives its deadline (a wedged birth client) | Kill it; no reset, no attach | None: `rimz start` refuses with the command and the bound |

tmux has no resurrection, so its gate is a no-op `Healthy`.

A Zellij IPC socket path that overflows is an environment precondition, not a health verdict: it classifies as `SocketPathTooLong`, reset is not offered, and `rimz doctor` prints the shorter-directory fix.

### Reset

A `Stuck` room needs a destructive reset. [`RoomContext::reset`](../../crates/rimz/src/room/mod.rs) gives explicit `rimz reset` and attended stuck recovery the same teardown and store reset, while the CLI keeps confirmation and report rendering. The teardown routine in [`room/teardown.rs`](../../crates/rimz/src/room/teardown.rs) kills the session and purges the serialized-session cache through the backend, reaps stale sidebar runtime files, and runs the mux process sweep for orphaned servers and leaked daemons, then the room is reborn. Each step is best-effort and independent, so one failure never blocks the others.

The process sweep in [`recovery.rs`](../../crates/rimz/src/mux/recovery.rs) is the dangerous step, because it signals processes by heuristic. It thaws every cgroup scope of the workspace first (`crates/rimz/src/proc/cgroup.rs::thaw_workspace`), and thaws each target's own scope again just before its `SIGTERM`. It selects through `select_sweep_targets`:

- Every target runs under the caller's real uid and is neither this process nor one of its ancestors.
- A server, sidebar, or daemon target names the recorded session as a whole command-line token, bounded by the start or end of the line, whitespace, or `/`. Whole-token matching keeps prefix-related state directory names from selecting each other's processes.
- At teardown only, a `rimz hooks feed`, `drain`, or `apply` process is a target when its inherited `RIMZ_WORKSPACE_ID` names this workspace, with no session token required, since hooks can outlive their pane. Reload's sweep never selects them.

The session's `rimz sidebar host` carries the same session and workspace tokens as its panes' serve processes, so teardown reaps it with them; it is not a serve process, and nothing that counts or attributes sidebar panes selects it. [`ProcessDomain`](../../crates/rimz/src/mux/domain.rs) is the last guard: a process in a foreign domain (a `cargo xtask sandbox`, another runtime root) is not RimZ's to signal, and a process whose environment cannot be read is spared.

Without a terminal, `rimz start` fails fast with the `rimz reset` fix instead of destroying a session unattended.

### Sidebar orphan reaping

Two paths kill a sidebar process whose pane is gone, and both require authoritative absence.

The supervisor's pane watchdog (`sidebar_pane/supervise.rs`) reads `cached_pane_roster` first as a latency hint. On escalation, one sidebar wins a workspace-and-session single-flight lock, lists with `RequireAuthoritative`, and atomically publishes mux kind, session, observation stamp, and pane ids, so peers consume that same observation. A peer takes the published observation only when it is newer than its own last one, so a sidebar never reads back the probe it published itself and lists again instead. A sidebar counts at most one strike per observation, resets on presence, keeps its strikes on unknown evidence, and exits only after `PANE_GONE_STRIKES` (3) distinct authoritative absences. A stale, mismatched, unreadable, or already-seen cache leads to a new authoritative listing. An unavailable lock, a wait that times out on a peer that never publishes, a failed cache write, and a mux failure all count as unknown and never trigger a local destructive fallback.

Reload and repair reap paneless sidebar processes ([`reload.rs`](../../crates/rimz/src/reload.rs)). A fresh cache can prove a pane present and skip the mux command, but a cache omission only nominates a process: two `RequireAuthoritative` rosters separated by a short delay must both omit the pane before RimZ signals it. The candidate must also prove, through its inherited environment, that it lives in the invoker's state and mux-socket namespace. If either roster fails, the whole reap aborts. A cache omission that either roster refutes records `pane_cache_divergence`, and every signalled process records `sidebar_orphan_reaped` with both observation stamps and the SIGKILL outcome.

## Zellij backend

One constraint shapes this backend: **a Zellij layout applies only at session birth, and a layout is the only way to create a pane already docked left at a set size.** RimZ therefore owns the birth layout and treats everything after it as convergence: close stray sidebars by id, add a missing sidebar in place, move it left, and converge its width toward the room target.

Zellij has no tab-width query, so RimZ infers the viewport from the rightmost of at least two tiled terminal extents, excluding the 50-column detached placeholder. A lone sidebar would measure itself. Renderer geometry must be current for the action it judges. With exactly one sampled client, that client's tab must resolve and pass the shape test; any tab with the same proven width may converge, regardless of `tab_viewed`. With two or more clients the plugin's viewed-tab latch gates each tab instead. Zero clients, an unresolved sole client, or no known sample proves nothing. `WidthMemo` in [`zellij/raw_pane.rs`](../../crates/rimz/src/mux/zellij/raw_pane.rs) remembers the last sample by session and stable tab id across withheld or sample-less publishes; an observed zero-client sample clears that proof. A new tab's percentage seed uses the first proven tab in the cached listing, without host memory, or the geometry-free step when none proves. Repair shares the shape test but `tab_view_cols` restricts it to its live client-view snapshot, never hidden tabs. Without proof the renderer retries and repair skips width adjustment; layout births use their read-only configured or recorded share.

`ensure_session` is a no-op because Zellij creates sessions lazily. The sidebar launch owns first birth through `attach --create-background` with a generated layout.

### Session liveness and refusals

Zellij's session list is not proof of liveness. It can omit a live socket when a connect is denied or the [client id reuse race](../externals/mux-adapter/zellij-reference.md#cli-action-reply-association) loses the probe's reply, and can show the cached session as exited instead. So RimZ checks a named session with one nonblocking socket connect before running `zellij list-sessions`: success or a full backlog (`WouldBlock`) means `Live` without a listing, and a missing, refused, or otherwise unaccepted socket leaves the listing's `Exited` or `Absent` verdict. After a batch listing, `ZellijBackend::check_unlisted_sockets` adds every unlisted socket that accepts or reports a full backlog, skipping directories and regular files. Socket paths come from `socket::spec_socket_path` and the exact child's environment.

A denied connect is never read as an absent room. `PermissionDenied` on the socket, an unreadable socket directory, and Zellij's `Error occurred: PermissionDenied` banner all become `MuxErr::SocketConnectDenied`, whose message names the cause and advises running outside the sandbox or asking to escalate. A delete also probes its named socket first, since Zellij's `delete-session --force` ignores a denied connect and still removes the resurrection cache.

RimZ's raw connect closes without sending `ConnStatus`, so Zellij removes that connection once ([upstream route loop](https://github.com/zellij-org/zellij/blob/v0.45.1/zellij-server/src/route.rs)). The probe adds no double removal of its own, though on Zellij 0.45.1 and earlier a stale removal from another client can still target the probe's reused client id.

Every Zellij command reruns one refusal inside its own deadline. Zellij's client probes each session socket before it dispatches anything and counts a server too busy to answer as absent (`assert_socket` in `zellij-utils`, observed on 0.45.1), so a live session can answer `There is no active session!` or the `Session '<name>' not found` banner for a few milliseconds. `socket::refused_live_session` recognises that nonzero exit only while the session's socket is still on disk, and `ZellijBackend::cmd` attaches it as the command's `RefusalRetry`: at most `PREDISPATCH_REFUSAL_RERUNS` (5) reruns, `PREDISPATCH_REFUSAL_DELAY` (100 ms) apart. Nothing was dispatched, so a rerun cannot repeat a side effect. A session with no socket is absent and its refusal returns at once, as does every other failure.

### The birth layout

Every tab has the same shape: a left `rimz-sidebar` pane and a focused terminal, above a one-row compact-bar plugin. A `new_tab_template` plus explicit birth tabs carry that shape.

These details in [`layout.rs`](../../crates/rimz/src/mux/zellij/layout.rs) are load-bearing:

- The sidebar command names the workspace's stable room-bin path, not one sweepable build generation, so the immutable `new_tab_template` keeps spawning working sidebars after reloads.
- The sidebar pane is borderless and `close_on_exit`, so work-pane frames can be styled while sidebar hit-testing starts at row 0, and the pane disappears when its process exits.
- Every sidebar percentage is a multiple of 5, because Zellij pins a fixed-size layout pane against resize and moves percentage panes in 5% steps. Known launch geometry chooses the step closest to the capped column target; without it the step is nearest the share. Detached birth tabs, resumed tabs, the daemon column and the immutable new-tab template share that seed. A native new-tab-key tab corrects once if the room step changed since birth or the room was born without measured geometry. An already-running session keeps its old template until rebirth, so its new tabs can retain an off-grid offset within half a step.
- Every tab is born with an explicit focused terminal. A `children` placeholder nested in a split is never filled and would leave focus on the sidebar alone.
- A tab layout file only has to exist while the `zellij action new-tab --layout` client runs: the client parses the file and sends the parsed tree with the action, so the server never reads it. `open_tab` drops it once the tab is confirmed. Birth is the opposite case: the `attach --create-background` client sends only the path of the `--default-layout` file, and the new server reads it while handling the first client's connection, which can come after that client has returned (Zellij v0.45.1, `start_server_detached` and `CliAssets::load_config_and_layout`). `create_session_with_sidebar` therefore holds the file through a bounded wait for the sidebar pane.
- The layout ends with the complete resolved session option list, including `auto_layout false`, `stacked_resize true`, and `stacked_pane_list false` ([room options](#room-options-and-the-cli-xor-problem)).

Birth branches on socket-based liveness, falling back to `zellij list-sessions` when the socket neither accepts nor reports a full backlog:

| Liveness | Branch |
| --- | --- |
| Live | Before attach, a bounded direct `list-panes` probe must show the control plane responds. A responsive session already has its sidebar and owns every resize and split since. Sidebar launch trusts a fresh heartbeat; after a stale heartbeat it rebuilds only a room whose presence topology can be inspected, and otherwise leaves the sidebar alone. |
| `Exited` (`EXITED - attach to resurrect`) | Clean rebirth: delete, then create from the layout. With serialization off (the default) Zellij does not mint this state, but a session serialized with it on still can. |
| Absent | First birth: create from the layout. |

A session can stay in Zellij's live roster while its per-session screen thread no longer answers control commands. RimZ retries native pane queries within one `HEALTH_PROBE_TIMEOUT` (8 seconds) budget. When none succeeds it classifies the session as unresponsive, separate from a rebuildable exited session: it refuses to attach before entering the alternate screen, keeps the room and its panes, and points the user at `rimz doctor` to inspect or `rimz reset` to rebuild. tmux needs no per-session probe, because its session roster and pane queries share one server event loop.

### Room options and the CLI XOR problem

RimZ sets only the `pane_frames` boolean; it leaves `pane_frame_style` to the user's `config.kdl` ([frame-style resolution and readback limits](../externals/mux-adapter/zellij-reference.md#configuration)).

[`crates/rimz/src/mux/zellij.rs::zellij_session_options`](../../crates/rimz/src/mux/zellij.rs) resolves one ordered, typed list: RimZ's fixed `auto_layout false`, `stacked_resize true`, and `stacked_pane_list false`, the four always-resolved `[zellij]` keys, and every configured optional key except client-only `mouse_mode`. The list uses three value types (boolean, integer, enum word), with channel-specific quoting. The three channels are:

- **Birth layout:** `crates/rimz/src/mux/zellij/layout.rs::render_session_layout` renders the complete session list in the KDL tail. Layout values merge absolutely; a detached birth discards CLI option flags. `create_session_with_sidebar` passes only `--default-cwd` and `--default-layout` after `options`.
- **Live reconfiguration:** `PresencePluginOptions::from_config` projects the same list. `crates/rimz/src/mux/zellij/presence.rs::presence_plugin_identity` sends generic `opt_<key>=<value>` entries and includes them in the configuration hash, so a changed option re-converges the plugin. The plugin collects entries without knowing option names and renders them ahead of keybinds in one `reconfigure(..., false)` payload. Supplied values win absolutely without writing `config.kdl`. Zellij ignores an option key it does not know, so neither KDL channel needs a version gate ([versions](#versions-and-focus-taking-spawns)).
- **Attaching client:** `zellij_client_options_args` carries only `--default-mode locked`, explicit `--mouse-mode false`, and configured `--support-kitty-keyboard-protocol <bool>`. On Zellij 0.45.1, `Setup::from_cli_args` applies attach flags through plain `merge`, then `start_client` applies them again through `merge_from_cli`. Each of the nine XOR booleans is XORed against itself, so either explicit value becomes `false` ([upstream mechanism](../externals/mux-adapter/zellij-reference.md#command-line-options)). RimZ never sends an XOR boolean as `true`.

The whole session list reaches reconfiguration, including start-only keys; RimZ keeps no separate live-key table. Zellij consumes `session_serialization`, `disable_session_metadata`, `scroll_buffer_size`, `on_force_close`, `osc8_hyperlinks`, `show_startup_tips`, and `show_release_notes` at room birth, and `mouse_mode` and Kitty keyboard support at client start. An absent reconfigure key keeps its live value, so removing an optional `[zellij]` key restores `config.kdl` control only at the next birth. `mouse_mode` is read in the client's own process: RimZ's `true` cannot override a `config.kdl` `false`, while RimZ's explicit `false` disables it.

**Serialization off by default.** `[zellij] session_serialization` defaults to `false`, and the birth layout supplies the configured value when Zellij consumes it. Resurrection does not help a room of agents: agents and scripts cannot restore their running state, so a resurrected room comes back as a wall of suspended command panes with a dead mouse. With serialization off, a crashed server's session vanishes, the next start births a clean running room, and RimZ owns rebirth ([resume on rebirth](./sidebar/sidebar.md#resume-on-rebirth)). Attach first purges the room's resurrection cache, so a corrupt serialized layout cannot block a live session.

**Session metadata off by default.** `[zellij] disable_session_metadata` defaults to `true` and takes effect at birth through the same list. It stops Zellij's periodic `session-metadata.kdl` rewrite and its command-discovery `ps` loop, which at roughly 100 panes on 0.44.3 costs a visible share of Zellij server CPU. `Absent` and `Exited` sessions still converge through the same clean-rebirth gate, so the setting changes CPU cost, not room semantics.

### In-place repair

Zellij repair keys every view by stable tab id, so closing an orphan tab cannot redirect a later Add. On Zellij 0.45+, a plain Add into a tab with exactly one live tiled work pane splits that pane with `--direction right --no-focus`, anchored by `ZELLIJ_PANE_ID`, then moves the sidebar left and converges its width. Other plain adds run `new-pane --tab-id` without a placement. A Replace on 0.45+ instead anchors on a docked old sidebar at least 10 columns wide; closing the old pane gives its width to the new neighbour without moving work panes. Without a qualifying anchor it retains add-before-close placement. Zellij answers before the pane mounts, so the backend verifies each step:

1. Mounted-pane discovery verifies the intended tab and, for any anchored split, that the new pane lies strictly inside the anchor's column band; the action's stdout is only a hint.
2. An add commits, or a replaced pane closes, only after a fresh current-build heartbeat.
3. A pane mounted in the wrong tab is cleaned up and aborts the pass.
4. Plan, mount, move, stack, width, retry, verification, and focus-restoration reads use direct `list-panes --all --json` after completed actions, never the presence roster as a fallback.
5. Each left move crosses one adjacent pane, the current tiled-pane count bounds the swaps, and every step must strictly decrease the sidebar's `pane_x`.
6. Width convergence starts only after current geometry verifies a full-height left dock.
7. A missed mount cleans up newly listed sidebars and the serve pair attributed to the stdout hint. If an earlier attempt mounted a working sidebar, it stays open and is reported as misdocked. Otherwise a lone anchored Add uses its next attempt, within the two-attempt bound, for a plain `new-pane --tab-id` split. That split hides shown floating panes before tiling the sidebar. An anchored Replace never falls back to a work-moving split: it returns the spawn error when available, or the mount error otherwise. Floating panes being shown can prevent an anchored mount.

An authoritative read never falls back to the topology cache, so stale truth never drives a close or a spawn. Before plan execution, a failed read returns an error. After execution, Zellij stops replacement verification at the first read failure, still attempts focus restoration, and returns the completed counts with one additional failure; the first post-execution error is logged. The next attach, `rimz sidebar repair`, or `rimz reload --repair` retries. Unknown or unreadable dock geometry never counts as a verified dock.

RimZ passes `auto_layout=false` and `stacked_resize=true`, so `Alt+n` uses Zellij's native focused-pane split along the edge that suits the terminal's cell ratio, and closing a pane returns its space to the sibling it split from. It also pins `stacked_pane_list=false`. Zellij 0.45's list mode keeps collapsed stack members in `list-panes` but marks them suppressed and reports the stack's full rectangle for every member; RimZ filters suppressed panes and relies on per-pane geometry, so the classic representation keeps every agent observable with its own rectangle. The birth tree makes the sidebar and compact bar tree siblings. When an add nests the new sidebar into one row and there are at least two live work panes, the same transaction stacks every surviving work pane into the right column; a plain lone-pane add on 0.45+ first tries an anchored split to avoid nesting. Repair of an arbitrary pre-existing multi-column layout only reports.

The producer's shrink-confirmation path bypasses `pane-topology.json` and reads `zellij action list-panes --all --json`, which carries Zellij's own live `pane_command` and `pane_cwd` for every terminal pane; the cached plugin copy fills only a field the listing left empty, and pid and geometry come from the cache the same way. If that query fails, the backend falls back to the topology cache with a debug log. tmux always lists from the server, so the authoritative flag changes nothing there.

### The daemon view and resumed births

`rimz start` always carries the `rimzd` runtime view, so `open_sidebar` births a two-tab layout: the runtime dashboard first, then the focused working tab. Writing that order into the birth layout avoids a visible post-birth tab move.

The runtime view is `sidebar | content | runtime`. What fills the columns, how its panes are identified by launch command, and how repair rebuilds them are in [rimzd.md](./rimzd.md#how-managed-panes-are-identified). Each managed Zellij pane carries its joined launch argv as an explicit pane name for that identity. tmux births multiple content or runtime panes as equal-height rows, with at most one row of rounding drift.

Explicit `rimz sidebar repair` reopens a wholly closed runtime view after sidebar reconciliation; the automatic tracker leaves it closed. A reopened tmux window leads the session, preserving the prior active window. Zellij late adds defer without an attached client; repair reports that deferral and asks the user to attach and retry. With a client, they append and keep its focus: `new-tab --no-focus` on 0.45+, otherwise the same captured-client restoration and lead-tab fallback as an unfocused `open_tab`. This deferral and focus rule also applies to a late add during `rimz start`.

Subagent panes use Zellij's native stack, anchored through the target's CLI pane context with `--near-current-pane` below 0.45 and `--no-focus` from 0.45, so attached-client focus and the active tab stay put. tmux uses [collapsed stacks](#collapsed-stacks) in the target column. Scheduled loop runs instead open [their own unfocused tabs](./harness/loops.md#where-a-scheduled-run-lands); the loop panel remains part of [daemon-view repair](./rimzd.md#who-repairs-and-when).

Zellij's background stacked insert can leave the new PTY at winsize 0x0 ([upstream defect](../externals/mux-adapter/zellij-reference.md#action-catalog)). Before any provider starts, the `rimz agents exec` wrapper calls [`repair_zero_winsize`](../../crates/rimz/src/mux/winsize.rs), which reads the live content rectangle through `MuxBackend::pane_content_size` within its remaining startup budget. It polls for at most two seconds, writes only nonzero dimensions, and leaves an already-sized tty untouched. A repair emits [`pane_winsize_repaired`](./diagnostics.md#event-taxonomy); unavailable geometry or an ioctl failure logs at debug and never fails the launch. The wrapper neither changes client focus nor asserts a frame style.

A reborn session re-seeds its remembered agents: the birth layout spells one `sidebar | agents…` tab per resume tab, each agent a command pane running its resume CLI in its own directory, with focus on the most recent. The resume planner groups agents by channel, and by working directory for agents without one (`crates/rimz/src/harness/resume.rs::resume_tab_identity`, [fleet.md → rebirth](./harness/fleet.md#rebirth)). Panes born from a fresh layout start running, not suspended, which is the same reason serialization is off. One renderer handles plain, daemon, and resumed births.

### Tab-switch focus repair

Zellij can restore focus to the sidebar when the user switches tabs, which leaves them on chrome. Repair is a plugin observation plus a host verdict.

On a `Some(old) → Some(new)` tab switch, the plugin waits the focus settle window, takes one `list_clients()` observation, and publishes `switch-settled` with the active tab, a generation, and the full client views. It publishes no verdict.

The host classifies the observation against the accepted topology. A unique live work view in the active tab is healthy. A plugin view, the active tab's sidebar, or a live terminal in another tab is stranded, but only when the active tab has exactly one sidebar owner and a work sibling. Missing, detached, dead, superseded, foreign, and distinct-pane observations abstain. The same accepted sample also feeds host-side unique-live focus.

The renderer keeps its owner, TTL, client-ambiguity, and focus-intent guards, so automatic repair never overrides an explicit cross-tab jump ([sidebar.md → selection and jump](./sidebar/sidebar.md#selection-and-jump)). Client ambiguity reads observed client views, never the fused pane cache or its focus-intent padding: on Zellij the strand's settled evidence, on tmux the pulled `snapshot.client_views`. Distinct panes defer repair; several clients viewing one pane agree, even when the fused cache still lists a pre-switch view.

The renderer acts on the strand against the live pre-action client sample rather than pane-cache agreement, re-sampling every 100 ms until the strand's TTL when the live sample disagrees. A same-writer topology older than the accepted one is not written, though a `switch-settled` wake carrying it still reports its strand against the accepted topology.

### Versions and focus-taking spawns

The Zellij features RimZ depends on, by the release that first ships them:

| Release | Feature | What depends on it |
| --- | --- | --- |
| 0.44.1 | `action focus-pane-id`, `new-pane --tab-id` | Jumping to a pane, and adding a sidebar in place |
| 0.44.2 | The `CommandChanged` event | The plugin's foreground command, and so the 0.44.2 floor |
| At the floor | `stack-panes`, `advanced_mouse_actions`, `mouse_click_through`, `mouse_hover_effects` | Used with no version check |
| 0.45.0 | `new-pane --no-focus`, `new-tab --no-focus` | Unfocused opens that never move an attached client's focus |

No option key carries a version gate: every one travels as KDL, in the birth layout tail or the `reconfigure` payload, and Zellij ignores an option key it does not know instead of rejecting the document. That was measured on 0.45.1 in both KDL channels, not on a floor host. The `--no-focus` flags share one gate because they are action flags, not options.

On 0.44.x a spawn can take focus for a moment. RimZ restores the single pre-spawn client pane when it resolves one; without a single view, tabs fall back to the lead tab and directional splits to their target pane.

A focus-taking `new-pane --direction` ignores the pane context and `--tab-id`, and splits the focused pane of the last-active client. Only `--no-focus` splits the pane named in `ZELLIJ_PANE_ID`. So on 0.45+ a focus-taking directional split of a named session pane (`rimz agents resume '#lane'`, `rimz agents restart`) runs in two steps:

1. Spawn with `--no-focus` against the exact anchor.
2. Run `focus-pane-id` on the pane id the spawn printed. A missing id or a failed jump costs the focus, never the placement or the spawn's result.

That jump carries no focus intent, because the new pane is in no renderer's roster yet and the intent would be invalidated. On 0.44.x the same split still lands beside the client's focused pane in the anchor's tab. A `focus-pane-id` issued from outside a pane moves the last-active client only, and a client that has sent no input since attaching is not moved.

### Zellij backend caveats

These are the upstream quirks the backend works around. The upstream surfaces are in the [reference](../externals/mux-adapter/zellij-reference.md).

- **Minimum version is 0.44.2** (`MIN_ZELLIJ_VERSION`), which `rimz doctor` reports as `meets_min_version`. Below it RimZ refuses the Zellij room and points at upgrading Zellij or using tmux. The floor is what the backend cannot work without, as the [version table](#versions-and-focus-taking-spawns) shows.
- **`new-pane` answers before the pane mounts, and action stdout can cross clients.** The printed id is allocated before the screen thread mounts the pane, and a detached session can drop the mount entirely. Reconcile treats the id as a hint and discovers the mounted pane through the live pane list. Cleanup closes only a live-listed new `rimz-sidebar`; a stdout-only hint can authorize killing its attributed serve pair, never closing a pane.
- **Every action reply is a hint.** Client-id reuse through Zellij 0.45.1 can misroute concurrent action replies; upstream 55d077a5 retires ids ([source and mechanism](../externals/mux-adapter/zellij-reference.md#cli-action-reply-association)). RimZ retries foreign listing replies and zero-byte `dump-screen` replies within the caller's command budget, then errors if no answer arrives. A blank pane's dump contains at least a newline and is not retried. Malformed array elements and session-not-found banners are rejected immediately; dumps check stderr only because stdout is pane content. A new tab is a namesake whose stable id no namesake held in the pre-call topology. Zellij numbers a new tab after the highest live id, so a tab closed mid-call can hand its id to the new one, and an id that was held under another name still counts as new. Stdout only breaks ties among those candidates. Ambiguous new namesakes fail without cleanup. Only a topology-proven new tab can be closed after failed materialization. Stable tab ids are present at the 0.44.2 floor, so no stdout-only fallback is needed.
- **`new-pane` can mount into a nested row.** A stable-tab add inherits the tab's split tree, so the sidebar can report `x=0` with a work pane spanning beneath it. Every add verifies the full-height left column and can stack two or more displaced live work panes without replacing their processes. On 0.45+, a plain add beside one live work pane first uses an anchored right split. If that split misses, the plain fallback hides shown floating panes and tiles the sidebar; at a tall view it can remain misdocked and is reported, as a lone-pane add can on 0.44.x. A kept sidebar already nested beside a lone work pane is not replaced: close that sidebar once with floating panes hidden, then repair or attach to add a docked one on 0.45+.
- **Named-session actions can print a session-not-found banner and exit 0.** A `--session <name>` action against an absent, exited, or still-registering session prints `Session '<name>' not found...` and an active-session list on stdout or stderr. RimZ's listing readers classify either stream as `MuxErr::SessionNotFound`; dump readers classify stderr only, preserving stdout as pane content. Sidebar reconcile and daemon view launch defer quietly while the pre-attach gate owns rebirth.
- **An action that names no session cannot prove it acted.** Without `--session` or `ZELLIJ_SESSION_NAME`, Zellij acts on the sole live session. With several live, it prints `Please specify the session name to send actions to` and the session list, and exits 0. A `--pane-id` the session does not hold also exits 0. So every action RimZ builds names its session, through the one builder `ZellijBackend::zellij_action`. A caller that holds no name (a split beside its own pane, `rimz pane detach`) gets the session its pane sits in, from `ZELLIJ_SESSION_NAME`, and with none the action fails with `MuxErr::NoSessionToAddress` before Zellij is spawned.
- **A pane write must prove the pane is in the room.** The `MuxBackend` pane I/O methods and `PaneWriter::open` require the room's session, which covers `rimz message`, `rimz answer`, the budget parker, and the three internal pane reads as well as `rimz pane send` and `rimz pane capture`. Only `pane send` and `pane capture` also check the room's authoritative listing for the pane first (`MuxBackend::require_pane_in_session`), because a raw pane id reaches them with no lookup. tmux shares the check, because its pane ids reach every room on the server. The other writers take their target from a fresh listing of the same session.
- **A detached server can skip pane lifecycle processing until the next attach**, notably a last-pane exit and the relayout after a sibling closes. Reconcile defers adds on detached sessions, and the renderer's empty-tab self-close relies on the unwatched fold backstop (5 s by default), not resize delivery alone. Its watchdog waits that interval plus 2 s without a completion; confirming emptiness keeps the 2 s check.
- **A server with no config file births a setup wizard that drops `new-pane` mounts** (layout-born panes mount normally). The test harness seeds a config at the home-relative path Zellij prefers; in production a first-time user dismisses the wizard once and reconcile retries the mount.
- **Plugin keybinds pause briefly on 0.44.x.** Zellij's `KeybindPipe` completion path can freeze the UI for about a second before a plugin keybind acts. The focus-key jump still lands.
- **New sessions use the state directory name** (`<basename-slug>-<hex>`). New slugs cap at 20 characters, leaving room for the usual 4- or 6-digit suffix under both RimZ's runtime socket directory budget and Zellij's macOS AF_UNIX socket budget. Existing directories are adopted unchanged; `rimz start` and `rimz reset` preflight the socket path of the name birth will take, so an overflowing state directory name refuses before the room is touched. Live recorded names survive upgrades until the next birth. Since the web relay port hashes the session name, that birth may also change its local port.
- **The presence plugin reports identity, geometry, root pid, foreground command, and cwd.** It probes the root pid when a live non-floating terminal joins the manifest. The command follows `CommandChanged` and the cwd follows `CwdChanged`; while either is missing, RimZ derives it host-side from the root pid through the process backend ([pane admission](./sidebar/sidebar.md#the-binding-ladder)), excluding commands whose program label is `rimz`. RimZ derives process start host-side and treats the spawn command as identity.
- **Tests isolate servers per test.** Tests construct the backend with a private runtime dir, since Zellij locates its server socket under `XDG_RUNTIME_DIR`. This is the counterpart of tmux's `with_socket`, and every command goes through the single `ZellijBackend::cmd` chokepoint, so one field threads isolation everywhere.

## The Zellij presence plugin

tmux offers a control-mode stream any process can attach to. Zellij does not: the only way to learn about pane changes as they happen is to run inside the Zellij server as a plugin. `crates/rimz-presence-zellij/` is that plugin, a headless wasm32-wasip1 binary loaded into every Zellij session RimZ manages. It renders nothing. Its contract is [`crates/rimz-presence-zellij/AGENTS.md`](../../crates/rimz-presence-zellij/AGENTS.md).

`Engine::on_pane_manifest` suppresses its own plugin pane whenever the canonical roster shows it unsuppressed, even on the stable-hash fast path, in an inactive tab, or with the floating layer hidden. This is level-triggered: Zellij can defer pane creation until the first attach, and a hide sent before the pane materializes is dropped rather than retried. Every suppression reports another manifest, but an absent or already suppressed own pane emits no hide, ending the feedback loop. `Engine::on_permission_granted` also hides on every Granted result because a prompt pane may not yet appear in the roster. Grant evidence from other events does not hide. The [upstream lifecycle](../externals/mux-adapter/zellij-reference.md#commands-host-functions) explains the deferred creation and suppression reports.

### The boundary: observations, never verdicts

The plugin publishes Zellij facts, and the host derives every meaning.

| The plugin owns | The host owns |
| --- | --- |
| Merged topology snapshots from Zellij's pane and tab manifests | Pane roles: which pane is a sidebar, which is an agent card |
| Attached-client observations, including the settled sample after a tab switch | Focus-repair decisions and the `SidebarEvent` taxonomy |
| Poke timing that Zellij's event model requires | Launch-chrome filtering and topology-writer authority |
| Capabilities that need plugin-only APIs: runtime keybinds, session-option `reconfigure`, fullscreen toggles, hiding or closing itself | Durable cache publication |

The split keeps product policy out of plugin releases: a change to what counts as chrome, when focus is stranded, or how an event maps ships in the `rimz` crate alone. Two rules follow. The plugin carries only facts that originate in Zellij's server state; a fact derivable from the OS goes through `pane_pid` on the host, which owns `/proc`. And a new wake shape is added only for a fact that an accepted snapshot diff cannot produce.

One session holds one plugin. Splitting control features across plugins would multiply lifecycle, permission, and writer-coordination work.

### Crate shape

The crate splits along the wasm boundary, which makes it testable.

| Module | Role |
| --- | --- |
| [`main.rs`](../../crates/rimz-presence-zellij/src/main.rs) | The wasm shell. Projects Zellij events into the engine, gathers runtime telemetry, and executes returned effects. Compiled only for wasm; host targets build a stub so `--workspace` builds and lints pass without the wasm toolchain. |
| [`engine.rs`](../../crates/rimz-presence-zellij/src/engine.rs) | The decision engine: room state, poke timing, client sampling and the switch-settled observation, permission gating, topology publication. Returns `Vec<Effect>`. |
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

Each wake after the first manifest carries the live roster as repeated `--topology` values, at most 64 KiB each and omitted entirely above 1 MiB, while stamp and telemetry delivery continue. `rimz sidebar wake` concatenates the chunks in order and normalizes the payload. The topology payload includes the raw attached-client observations as `clients: { views: [{ client_id, pane_id }] }`, from which the host derives attached-client count, terminal views, and unique-live focus. Each pane also carries `tab_viewed: true` when its tab has been viewed; false is omitted and defaults to false on the host. Native pane listings retain this plugin observation by pane identity through `merge_topology_enrichment` in [`zellij/backend.rs`](../../crates/rimz/src/mux/zellij/backend.rs); structural reconcile overrides it with exactly the tabs its live client-view snapshot names. When `clients` is omitted, the current plugin payload also has no `focused_pane`, so the host has no pushed session focus and the producer samples `client_view` directly ([focus truth](#who-is-looking-at-what)).

The engine latches viewed tabs by stable `TabInfo.tab_id`, not shifting positions. A `TabUpdate` records a viewer when `active || !other_focused_clients.is_empty()`; switching away keeps the latch, and ids absent from the roster are pruned. A current-session `SessionUpdate` with `connected_clients == 0` clears every latch. A later positive count alone restores nothing: a tab must be viewed again. Any latch change schedules a publish independently of the manifest's stable-hash dedup. Projection at publish gives every pane of one tab the same value, including panes mounted after the observation.

The zero-client clear excludes Zellij's detached birth client, which briefly views the birth tab at 50x50. Its plugin instance survives the client's departure and can keep publishing, so the clear publish is load-bearing. Zellij reuses the lowest free client id, so the first attach usually rebinds that same cleared instance, which then latches at the attached geometry. There is a residual interval between the birth observation and the clear; the host refuses Zellij's 50-column detached placeholder as a viewport, so no width decision acts in that interval. A real client exactly 50 columns wide also gets no width convergence or divider-drag snapback. With one sampled client, hidden tabs whose proven width equals that client's tab can converge without a latch. After a terminal resize, or a switch between a terminal and the web view, a hidden tab of another width waits until revealed and steps once then. With two or more clients, including a terminal and web client together, convergence retains the viewed-since-last-detach latch rule. Structural repair still uses only tabs viewed now.

An announced topology with neither `clients` nor the `focused_pane` field of plugin builds that predate `clients` says nothing about focus and emits no focus transition. The next observed sample compares against the last focus observed from the same writer, not the clientless topology between them. An observed sample with no unique live focus still unfocuses the prior pane. At the start of a withheld span, [`sidebar::presence`](../../crates/rimz/src/sidebar/presence.rs) saves the cached observation once in the private runtime sidecar `topology-focus-observation.json`, using the existing topology schema under the writer lock. Observed wakes write no sidecar and compare with the canonical cache when it has an observation, otherwise with the same-session, same-writer sidecar. Only wake ingestion reads the sidecar; the pane-frame focus ladder reads the canonical cache, which remains clientless during withholding.

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

`crates/rimz-presence-zellij/src/engine.rs::FocusSync` invalidates retained clients when canonical pane membership or tab placement changes, including a pane close. A tab switch alone sets no mark and keeps its client sample publishable. Every wake publishes the roster with its existing timing, but omits `clients` while the sample is stale, and it never carries `focused_pane`. It carries `clients_withheld: true` while the mark is set; the additive field defaults to false and is omitted when false. The host samples its own clients meanwhile. A reply clears the roster mark only if its query was issued at or after the latest change and the mode is Stable after applying the reply, so a general reply during a switch does not clear it. Clearing the mark republishes once even if the sample is unchanged. Field-only manifests retain the existing timing.

Every host fork runs from `/`, so the session-lifetime plugin does not depend on the cwd of the CLI that loaded it.

### Loading and permissions

At session birth, RimZ loads the presence plugin through a top-level `load_plugins` block in the birth layout. That block replaces Zellij's background-plugin list, so RimZ first resolves the birth client's configuration and carries its entries over verbatim, then appends the presence entry. Without a user block, it preserves Zellij's built-in `zellij:link`. If the selected config cannot be read or parsed, RimZ omits the block, logs the path and error at debug level, and falls back to the boot pipe. The user's config is read-only.

The idempotent `zellij … action pipe --plugin --skip-plugin-cache` remains the route for room re-entry, reload upgrade and convergence, and post-retirement boots. Birth also keeps this pipe to publish the desired identity, deliver the boot payload, and cover hosts that did not load the layout entry. A clientless birth can still hold the pipe through its two-second deadline. Generic pane and topology readers broadcast the name-only `rimz:dump_topology` pipe instead and never launch a plugin.

Load-time configuration pins the workspace, the session, the room's `rimz` pointer (`ws/<workspace-dir>/rimz`), runtime mouse options, the focus and zoom chords, `launch_scope=background`, the embedded-wasm digest (computed once, lazily), and a hash of the configuration itself. One ordered configuration supplies both the layout entry and the pipe string, so the boot and re-entry pipes address the same suppressed background instance. Changing an identity uses the pipe route; the host accepts the replacement's proof and retires the old identity.

The canonical artifact path is the same across upgrades, but Zellij keys its compiled-module cache by that path, not the wasm bytes. Every plugin-addressed pipe therefore skips the cache: a live identity treats the flag as a no-op, and a missing identity compiles the bytes currently installed at the path. Background layout loads do not bypass that cache, so a birth immediately after a build swap can still load stale compiled bytes.

RimZ seeds Zellij's `permissions.kdl` cache for its embedded plugin before spawning the birth client, so the background loader sees the grant and the first attach shows no prompt. Pipe loads also repeat the idempotent seed:

| Permission | What it grants |
| --- | --- |
| `ReadApplicationState` | The pane, tab, session, and client manifests. |
| `RunCommands` | The `rimz sidebar wake`, `rimz sidebar focus`, and `rimz pane zoom` forks. |
| `Reconfigure` | The resolved session option list and optional focus and zoom keybinds, applied without writing `config.kdl`. |
| `ChangeApplicationState` | The fullscreen toggle for the host-selected pane id. |

The artifact path is canonicalized because Zellij keys the grant on the exact string. The security boundary is in [security.md](../guide/security.md#the-zellij-presence-plugin).

A Zellij room requires Zellij 0.44.2 or newer, and a fresh birth requires the plugin artifact: without it, birth fails with `rimz doctor` and the tmux backend as the fixes (`ZellijBackend::birth_presence_options`). A plugin that fails to load later does not refuse the room. A boot pipe still held at its deadline (a pending permission prompt, or no client attached yet) counts as delivered, `RoomContext::load_presence` logs any other load error at debug, and a denied permission only clears the plugin's own granted flag (`Engine::on_permission_denied`). Until a plugin publishes, the Zellij `Cached` pane reads described in [reading the room](#reading-the-room) fail.

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

`rimz reload` without `--repair` nudges its sidebars, which converge host-first from the durable workspace record, and touches the plugin only when its echoed identity no longer matches the running build; it never changes pane structure ([sidebar.md → reload and repair](./sidebar/sidebar.md#reload-and-repair)). Explicit repair ensures the plugin, reads the live pane list, then requests a publication at or after a floor taken after that live read, waiting at most five seconds. No publication (or an unreadable live list) is `Silent`. A publication containing an absent terminal id no higher than the largest live terminal id is `Stale`: terminal ids never repeat, so that pane was already gone when the live read ran. Higher ids and omitted live panes prove nothing about staleness. A stale verdict records `presence_roster_stale` with the ghost ids, both observation stamps, and the writer generation. Silent and stale plugins warn but never gate structural repair, which uses the live pane list independently; a reattach or restart reloads the plugin.

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

### Collapsed stacks

Stacked splits and stacked layout columns keep one member expanded; the others have one content row plus their separating border. Focus opens a member regardless of whether keys, mouse, or a RimZ jump moved it. Leaving the stack keeps its open member. This happens entirely in the tmux server, without a RimZ process or changes to the user's bindings.

Pane option `@rimz_stack` is the founding pane's raw id, opaque and never reassigned after its death. Pane option `@rimz_stack_open = 1` marks at most one open member per stack. Global indexed hooks `window-pane-changed[240]` and `window-layout-changed[240]` coexist with the user's global hooks. Room ensure and stack birth assert both hooks through `crates/rimz/src/mux/tmux/options.rs::stack_hook_commands`. Focus moves the open flag among members, and closing the open member while focus is elsewhere leaves the stack with none until focus enters a member; layout changes restore every open tiled member when any collapsed member survives. Zoom defers expansion until unzoom, and floating panes are excluded.

Placement determines whether a new pane belongs to the stack. `Stacked` and `Rejoin` add a member. Before that join, RimZ clears the stack's open flag and expands the target only when another member survives, so a collapsed target has space without resizing a foreign container around an unmarked target or a lone survivor. A join into an existing stack reads its membership again after the split, so a member that exits during the add is not marked back in. After marking the newcomer, it opens the focused member of that stack, otherwise the newcomer. A horizontal join with a lone survivor sets the open flag without resizing; a vertical join creates its own container and can expand safely.

`Directional` opens beside the whole run of members: right spans the run's rows, down spans its columns, and the newcomer takes half the extent while the stack keeps the larger half. It leaves the newcomer unmarked and preserves the stack's marks, order and open member, and every outside pane's geometry. No open flag is required: a downward shrink comes out of the flagged member, or otherwise the tallest member, which must remain taller than every other member. It skips the join's flag clearing and resize, splits the last member horizontally, then rewrites the window's layout. A shared row container gets an inner container for the run so later stack focus cannot resize its neighbours. The rewrite checks pane-list order against depth-first leaf order and applies only while the post-split layout still equals the read, with the comparison and apply in one tmux invocation. A lone member's stack is dissolved by unsetting both pane options before a plain directional split; neither pane is marked afterwards.

Layout birth opens the requested focus member of each stacked column, otherwise its last member. Companion append extends a lone work pane or a single existing stack while keeping the expanded member at least 10 content rows; other layouts take no new children.

Kept differences: when adding a member while another member has focus, tmux keeps the focused member open; Zellij opens the newcomer. Retaining that focus on Zellij would need presence-plugin coordination. Restart and lane resume use `Rejoin`: they remain in the stack on tmux but open beside it on Zellij. A directional neighbour preserves the existing open member on both backends.

When `[tmux] pane_border_status` is unset, stack windows get `pane-border-status top` and a format naming members by `@rimz_title`, falling back to their current command; every other pane's border row is blank. These blank rows also cost a row on non-members. Window marker `@rimz_stack_borders_owned` permits hand-back of both border options when the window's last member closes or leaves its stack. Session option `@rimz_stack_borders` records whether this default is enabled; explicit border configuration always wins, including `off`. Reapplying room options keeps stack-window borders while members survive.

Accepted edges: a directional neighbour falls back to a member join with a warning when the window has floating panes, an unreadable layout, mismatched pane-list and tree order, nonconsecutive or nested members, insufficient space, changed stack membership, movement or resizing outside the staging split, or a changed post-split layout or failed apply. A post-split fallback refreshes membership and places the newcomer after the last member rather than the requested member. A user's `after-split-window` hook causes refusal if it changes geometry outside the staging split or creates an extra pane. Once the new pane exists, a read, rewrite or marking failure never retries the launch elsewhere. A manual split off a member is not adopted and can be held at minimum height by the restore (zoom still reaches it). Dragged borders snap back on layout change. If one of several stacks becomes a lone member, its former container dissolves and restoring it can resize a foreign container while another stack still has collapsed members. A horizontal `Rejoin` split of a lone survivor preserves surrounding rows at birth, but focus or restore before the old pane closes can still expand the side-by-side members against a foreign container. Re-sourcing an unindexed assignment to either hook clears RimZ's indexed entry until room ensure or stack birth reasserts it.

### The managed server endpoint

RimZ owns one tmux server per runtime domain, at `<runtime-root>/rimz/tmux/server`, holding one session per workspace, named after its state directory at birth. Every managed command runs `tmux -S <socket> -u …` through the single [`TmuxBackend::cmd`](../../crates/rimz/src/mux/tmux.rs) chokepoint. The socket is always set, so no command can reach the user's default server, and `cargo xtask invariants` rejects a bare `tmux` argv. The control-mode watch is the one managed client outside the chokepoint: `PresenceWatch::attach` sets the same `-S <socket> -u` itself, removes `$TMUX`, and inherits the host's working directory.

The endpoint derives from the resolved runtime root alone, so any caller reconstructs it without a workspace or `disk::paths::RuntimePaths` argument. Attach, ttyd, presence, pane I/O, list, reload, GC, sidebar, and doctor all address the same path. A disposable `XDG_RUNTIME_DIR` yields a different socket and a private server, which is what isolates sandboxes and tests.

Each managed session is stamped with the concrete `HOME`, `RIMZ_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`, and `XDG_RUNTIME_DIR` of the resolved domain, at birth and on every ensure, so a pane resolves the same store and endpoint as the client that created it. Socket identity and stamped environment are two projections of one runtime domain, derived together in [`disk::paths`](../../crates/rimz/src/disk/paths.rs).

Routing stays explicit. The user's default server serves only recordless external sessions and the exact endpoint a process inherits through `$TMUX`; managed commands clear `$TMUX` so an ambient session cannot capture them, while [session birth](#the-identity-pin) strips the outer pane's mux identity. `ProcessDomain` resolves a process's endpoint from `$TMUX` when present and from the managed socket otherwise, so the orphan sweep recognizes managed processes and spares the user's own server.

Before birth or attach, a read-only `has-session` probe of the default socket reports a same-named session left there by a RimZ release that used the default server, with the command that retires it (`tmux -S <default-socket> kill-session -t <session>`). `has-session` cannot start a server, so the probe never starts a default daemon, and unrelated sessions stay untouched.

Workspace reset and cleanup use `kill-session`. `kill-server` is reserved for explicit recovery of every RimZ tmux room at once, and is safe to suggest because it is scoped to the RimZ socket. Server-global options and root key bindings are shared across RimZ workspaces on that server and die with the last session, so `ensure_session` re-asserts them on every ensure.

### Every managed client runs from `/`

A tmux server inherits its working directory from the client that births it. Since tmux 3.7, `spawn.c` applies a pane's `chdir(cwd)` only while `getcwd()` succeeds on the server. A server born in a directory that is later deleted (a disposable worktree, a swept tempdir) therefore places every later pane in that deleted directory, even when RimZ passes an absolute `-c`. Earlier releases honour `-c` regardless of the server's working directory.

`/` cannot be deleted or unmounted, so birth and rebirth always start from a readable directory. The Zellij plugin forks from `/` for the same reason. The one managed tmux client that does not is the [control-mode watch](#the-control-mode-presence-watch), which inherits the room host's working directory and attaches only to a live session.

Session birth checks the property by reading the birth pane's `pane_current_path` back. Before tmux 3.7 the pane enters the requested directory after the fork, so an early read can show the server's own directory. A matching or empty read passes immediately; on a non-empty mismatch, birth re-reads every 25 ms until a read matches, reads empty, or still mismatches after a 3 s bound counted from the first mismatch. A server that strands panes never converges, so a mismatch after the bound fails with `MuxErr::ServerCwdUnusable`, whose message gives the socket-scoped `tmux -S <socket> kill-server` fix. Reading the pane back tests what matters and works on every host; the daemon's own working directory is only a proxy and cannot be read on some platforms.

### Room options

`ensure_session` applies the per-machine `[tmux]` room options on every open or reattach. `crates/rimz/src/mux/tmux.rs::TmuxBackend::apply_room_options` sends the session and server settings as one batch, then `TmuxBackend::apply_window_options` reads stable window IDs, stack membership, window-only values, and the global values of inherited keys in one fixed-size batch. A second read, one `source-file -` script with one line per listed window, gets `allow-passthrough`, `pane-border-status`, `pane-border-lines`, and `pane-border-format` with `show-options -wAv -t @N`. Those keys can be pane-scoped, and a format would see the active pane's override before the window's value. Only differing keys are written, in one script with one line per changed window. Begin/end markers attribute output and completion to each line: a window that closes during the scoped read gets no write and does not count as applied, and one that closes before the write costs only its own line. A listed window counts as applied when its read completes and it already matches, or its write line completes. A failed or malformed listing, or a failed read script, fails the call. Past that point, the call fails only when windows were listed and none applied. An unchanged ensure sends no write script.

`crates/rimz/src/mux/tmux/options.rs::tmux_window_options` returns five owned keys as values or unsets: `allow-passthrough`, `aggressive-resize`, `pane-border-status`, `pane-border-format`, and `pane-border-lines`. `stack_window_options` chooses the stack-window border default and ownership marker from config and live membership. Missing optional border settings compare against the live global value and emit `set-window-option -u` only when the effective value differs; `pane-border-format` also restores inheritance when status is explicitly `off`. A local value equal to the global is retained until a global change makes it differ on a later ensure. The ownership marker compares against an empty value when unset, since RimZ never sets it globally. The `after-new-window` hook renders the base list before docking the sidebar; stack birth applies its border default once members exist. Per-window values, rather than global window writes, preserve the inherited settings.

Session and window options are scoped to the RimZ session; server-scoped options (clipboard, rich-key handling, focus events) are global to the RimZ server because tmux has no per-session equivalent.

That session and server batch also does what the option list alone cannot express:

- writes `*:sync` at the fixed `terminal-features[240]` index for atomic redraws in pixel pets and TUIs, removes exact `*:sync` and `*:extkeys` entries found at other indices, and collapses an exact `*:hyperlinks` entry inherited from the user's config,
- writes `*:extkeys` at `terminal-features[241]` when `extended_keys` is on and unsets that index when it is off,
- writes `*:hyperlinks` at `terminal-features[242]` so OSC 8 links from the sidebar and panes stay clickable,
- when `extended_keys` is on (the default), registers root-table `S-Enter` and `M-Enter` bindings that send the configured modified-Enter sequence, so agents receive soft newlines without requesting modifyOtherKeys, and names `ESC[27u` as `user-keys[240]` bound to Escape, because tmux passes that modifier-less form into panes verbatim.

On tmux 3.5.x, extended-key mode breaks clean multiline clipboard paste; tmux 3.6 preserves paste bytes while modified keys still reach agents as CSI-u. Per-option semantics and RimZ's values are in the [reference → options](../externals/mux-adapter/tmux-reference.md#options); the config model is in [configuration.md](../guide/configuration.md#multiplexer-room-options).

When `[tmux] pane_border_status` explicitly enables borders, RimZ also writes a `pane-border-format` that fills the `rimz-sidebar` pane's border row with spaces, so work panes carry titled frames while the sidebar reads frameless, the tmux counterpart of Zellij's borderless sidebar. The [stack-window default](#collapsed-stacks) titles only members instead. tmux borders are separators plus an optional top or bottom status row, and tmux does not draw the outer window edge, so a closed four-edge pane frame exists only on Zellij.

### Attach and the terminal

tmux defers client-driven resizes of hidden windows until selection. `MuxBackend::lagging_views` reads the attached session's current window size and lists differently sized hidden windows in MRU order, with never-visited windows last. Effective `window-size manual` windows are left pinned. An ignore-size control watch does not count as a sizing client. While detached, normalization covers only the current window and session `default-size`; hidden windows wait for the room host's hidden-view sweep, which a terminal client's attach (`%client-session-changed` for the watched session) kicks among other triggers ([state.md → the room host](./sidebar/state.md#the-room-host-and-its-attachments)). That attach or switch-in also projects a `PanesChanged` nudge, requesting fresh panes even at the same terminal size, without waiting for the unwatched pane-cache TTL. `lagging_view` makes the same fresh check for a jump destination in one batched read. Other backends inherit no-op defaults.

`MuxBackend::fit_views` takes an ordered slice of views and the sidebar's legit columns for the target view size, computed by the caller with width control's room-target resolution. One stdin script reads the pane inventories and local `window-size` values; another fits the views in input order, with per-line markers isolating failed windows and their output. Each outcome is fitted, skipped, or failed. A script that begins no line, fails to spawn, or outlives `COMMAND_TIMEOUT` fails the whole call (`crates/rimz/src/mux/tmux.rs::TmuxBackend::run_script`). In the same guarded command group as each hidden-window resize, it reasserts the sidebar width when the view has exactly one sidebar, at least one sibling, and no zoom. tmux coalesces the queued pane sizes to the final size when it differs from the starting size, so the sidebar receives new rows at its legit width without needing a second, throttled width nudge. Zoomed, solitary, and ambiguous sidebars keep native width handling. The fit restores the local sizing value (or unsets it when previously inherited), because `resize-window` itself installs local `manual`. The mutation checks that the window is still hidden, non-manual, has a sizing client, and differs from the target size; a declined guard returns skipped, avoiding a same-size resize that would unzoom the window. Incomplete lines or malformed or empty pane rows fail only that view. Otherwise the returned per-pane rows become the destination-paint expectation, with sidebar columns only when the reassertion printed its proof. A failed sidebar reassertion can therefore still return fitted rows, but keeps the destination's normal paint budget. A clamped width still matches, and stays safe to bypass on, because the caller requests the legit width and tmux can only narrow it. That exact size permits the destination to bypass the held-grow hold ([state.md](./sidebar/state.md#focus-intent)). A skipped or failed jump carries no expectation.

An attach launched by RimZ disables alternate scroll on the outer terminal and restores the saved prior mode when the client exits. tmux disables outer mouse reporting in `tty_start_tty` before its first repaint restores the requested mouse mode, and a terminal with alternate scroll enabled turns wheel ticks in that gap into arrow keys. The CLI brackets the client process with XTSAVE and XTRESTORE, because a tmux `client-detached` hook runs after the departed client's tty name is cleared. Ghostty 1.3.1 implements those operations for mode 1007, and a terminal that ignores them is left with alternate scroll off.

Terminal mode save slots are not a stack, so a one-shot SSH attach brackets only the local terminal and marks the remote launch as already bracketed through an environment variable; a remote build that does not read the marker does not bracket either. Reconnect supervision has no local bracket, and the remote RimZ owns it. Zellij attaches go through the same lifecycle.

The waiting RimZ parent mirrors a client's `SIGTSTP` stop and resumes the child with its foreground job, keeping tmux's `prefix` + `C-z` suspend. Child exit codes pass through unchanged after terminal restoration.

### The sidebar and the `after-new-window` hook

tmux has no tab template, so a hook provides it. `open_sidebar` splits a left sidebar into the initial window at the launch seed and installs a session-scoped `after-new-window` hook that repeats the split in every later window.

Every tmux sidebar starts in the session's directory (`#{session_path}`), including hook-docked sidebars and reconcile re-docks. Sidebar splits pass that format explicitly with `-c`: the tmux client runs from `/`, and `#{pane_current_path}` would resolve against the session's current window, not necessarily the new window targeted by the hook. This matches pristine birth, which retains the session's start directory. On Zellij, one sidebar KDL fragment with no cwd of its own serves every tab of the birth layout (`crates/rimz/src/mux/zellij/layout.rs::render_session_layout`), so those sidebars start in the session's default cwd, resumed tabs included, while each resumed work pane gets its own cwd. Existing sidebars keep their cwd until re-created; reconcile refreshes the hook for future windows without restarting live panes.

The hook reads an absolute-column session option initialized from the resolved room share. Keypresses, adopted mouse drags, view changes, and reconcile passes refresh it, so a new window starts at the share rendered for the current view.

Two details keep the first prompt clean. Both are tmux-only, because Zellij births terminals from the layout at their final size:

- A plain default-shell window has an empty `pane_start_command`, so after the hook split sets the final width, the hook respawns only that work pane as the user's shell, which avoids zsh's `PROMPT_SP` end-of-line marker.
- Pristine birth installs a one-shot `client-attached` hook for the first work shell. A detached session can draw zsh's first prompt before the attaching client applies its size, and a resize during that draw leaves the `PROMPT_SP` `%` marker above the prompt. The hook skips control-mode clients, respawns the birth work pane after the first real attach, then removes itself. A room born without a probed terminal is fixed when the first later attach normalizes its detached geometry and records `default-size`.

A quick tmux kill-and-restart still takes the pristine birth path: once the room transition proves the session absent, it purges sidebar heartbeat files and clears the prior width target before creating the replacement. A fresh heartbeat from a dead sidebar therefore cannot send the new shell through the reconcile split that leaves the `%` marker.

Reconcile converges widths only against an attached, sized client's geometry or, while detached, the attaching terminal's probe. `crates/rimz/src/mux/tmux/window.rs::TmuxBackend::normalize_detached_geometry` first reads the session `default-size`, status lines, and current window's geometry and local `window-size` in one fixed-size batch. It writes `default-size` only when it differs from the full terminal probe, and resizes only a non-manual current window whose size differs from the probe minus the session's status lines (at least one row). It restores that window's prior local `window-size`, or unsets it if inherited. Hidden windows keep their geometry and local policies for the hidden-view sweep. An unchanged reconnect therefore writes no geometry; a daemon reload with neither basis re-asserts structure without changing panes or the recorded width. `open_tab` still unsets the local `window-size` after its temporary resize, revealing the inherited autosizing policy. When `open_tab` temporarily expands a new window to the widest attached client, it re-asserts the sidebar at the live target before splitting agent columns. Layouts compile to tmux command sequences from the same layout IR Zellij uses: every column is born first as a full-height horizontal split chained off the last column created, then each column's rows as vertical splits inside it. Both runs pass absolute cell sizes, so every rectangle is even at creation and no resize pass follows; splitting rows first would leave each later column nested inside the first column's top row.

The pane itself is best-effort: a fresh sidebar heartbeat suppresses a relaunch, while a missing, stale, unreadable, or protocol-mismatched heartbeat lets `rimz start` or `attach` open a new pane. For supervised agent panes the producer derives the wrapper spawn command from the process backend, as it derives `pane_process_start`, so lazily registering agents bind and panes group by worktree as they do on Zellij.

### The control-mode presence watch

The host holds one control-mode client, [`PresenceWatch`](../../crates/rimz/src/mux/tmux/presence.rs), started from `sidebar_pane/app/tmux_watch.rs`, with a single `refresh-client -B` subscription.

[`TmuxPresenceState`](../../crates/rimz/src/sidebar/presence/tmux.rs) keeps only the stream state needed to normalize out-of-order lines (panes, current windows, pending inactive panes, floating status, seeding) and feeds pane observations, focus, view switches, and incomplete-layout nudges to the shared projector. Which notification becomes which event is in [state.md → what triggers a mux-derived event](./sidebar/state.md#what-triggers-a-mux-derived-event). The plane records each overlay once and immediately fans events out in memory to its subscribed panes. The producer verifies structural changes with a fresh frame, and the watch refreshes the presence stamp on attach and on each classified line, which puts tmux on the same event-mode pane TTL as Zellij while the stream is alive.

The watch follows the control-mode contracts in the [reference → control mode](../externals/mux-adapter/tmux-reference.md#control-mode). It attaches with `-C` and `ignore-size,no-output`, holds stdin open because closing the pipe detaches the client, sends only `refresh-client -B` and a one-shot `list-panes -s` seed, drains notifications promptly because tmux force-exits a slow reader, and removes `$TMUX` from the child environment so tmux does not refuse a nested attach. The pane seed carries pane and window ids, pane left position and width, window width, session id, and the window-active flag. The client is writable, which keeps tmux 3.7 `send-keys` working when the watch is a headless session's only attached client.

From tmux 3.7 a window's layout string repeats each floating pane, once inside the tiled tree and once in a trailing `<...>` suffix ([reference → layout strings](../externals/mux-adapter/tmux-reference.md#layout-strings)). The reader drops the suffix's ids and reports the tiled panes, so a window holding a floating pane keeps typed layout changes; the floating panes themselves arrive through the subscription. A zoomed window writes no suffix, so its floating panes still appear as leaves there; the consumer already skips panes it knows are floating.

A dead or refused watch degrades to the tmux poll, and `crates/rimz/src/sidebar_pane/app/tmux_watch.rs::watch_loop` reattaches after `RESPAWN_BACKOFF` (5 seconds) whenever an attach fails or the stream ends. An idle watch is not respawned: its read blocks with no deadline, and its presence stamp ages until the producer's poll takes over.

### Notification passthrough

Desktop notifications are terminal-local. The sidebar renderer writes OSC 777 and BEL bytes into its pane, SSH carries them to the local terminal, and the terminal decides whether to show a banner or play a sound.

tmux forwards OSC 777 wrapped as DCS passthrough when `allow-passthrough` is on, which is RimZ's default. When an attachment starts, the room host raises the sidebar pane's own passthrough from `on` to `all` (`escalate_own_pane_passthrough` in `sidebar_pane/pixel/probe.rs`), so kitty graphics reach the terminal while the window is hidden ([pets.md → capability probes](./sidebar/pets.md#capability-probes)). DCS-wrapped notification bytes pass under the same setting.

Zellij drops notification OSCs, so `[notifications].desktop = "auto"` disables OSC there and notification handlers are the portable channel. The full contract is in [notifications.md](./sidebar/notifications.md).

### tmux backend caveats

- **Minimum version is 3.5.0** (`MIN_TMUX_VERSION`), set by the room options `ensure_session` applies (`extended-keys-format` arrived in 3.5); the batched sequence fails at the first unknown option. The command surface alone needs only 3.2. `rimz doctor` reports floor compliance.
- **Only a missing server makes `list_sessions` empty.** tmux's `no server running on <path>` and `error connecting to <path> (No such file or directory)` return an empty `Vec`. `Permission denied` and `Operation not permitted` become `MuxErr::SocketConnectDenied`, carrying the socket path, OS reason, and advice to run outside the sandbox or ask to escalate. Other connect errors remain `MuxErr::Command`. The same classifier governs missing-server tolerance in session kill and pre-birth environment repair; a denied kill is never success.
- **A server exits when its last session ends.** The endpoint is a stable path, not a long-lived daemon, so a leftover socket file is normal and the next command births a replacement. Pane and window ids restart from `%0` and `@0` across that boundary ([pane metadata](#pane-metadata)).
- **The sidebar self-closes** as it does on Zellij, through the normalized pane listing, so a lone sidebar removes itself when its window's last working pane exits. At startup the supervisor pins its own pane to `remain-on-exit off` at pane scope, overriding the user's global, window, or pane retention without changing other panes' settings; startup refusals before the pin keep the user's setting. The creation commands cannot set it: three of the four sites that create a sidebar pane hold no id for the new pane, and a `set-option -p` after `split-window -d` addresses the still-current work pane. The `after-new-window` hook runs `split-window` and, for plain default-shell windows, `respawn-pane`, so it never recurses through `new-window`.
- **The [daemon view](./rimzd.md#repair) pins retention at window scope**, so its exited commands close under user retention and repair can restore them.
- **Resumed agents open as windows**, one `new-window -n <label>` per resume tab (`#<channel>` for a channel tab), born `sidebar | agents…` as the hook docks the sidebar. `new-window -n` turns off automatic-rename, so the name is a stable idempotency key and a re-run never doubles a tab.
- **Tests isolate servers per test.** Tests point the backend at a private runtime root, so `with_socket` receives the same derived path production would use one domain over. A test that pairs a live server with `rimz` subprocesses builds both from one runtime root, because the subprocess resolves its own endpoint instead of inheriting `$TMUX`.

## What both backends guarantee

- **Detach and reattach are multiplexer features.** RimZ does not reimplement them.
- **Runtime correctness needs no visible sidebar.** Hooks, `rimz message`, and supervised runs work headless.
- **The renderer is optional.** The native pane is the default on both backends, and correctness never depends on which renderer, or none, is attached.
- **The store survives host restart; processes do not**, unless a host supervisor is wired (tmux-resurrect, Zellij resurrect, systemd).

### What `rimz doctor` reports

The selected backend, versions and floor compliance, PATH-visible backend binaries, backend server-log issues, feature availability, sidebar liveness, RimZ runtime socket headroom, the managed tmux server socket path, any same-named session left on the default tmux server with the command that retires it, Zellij IPC socket headroom when Zellij is selected, Zellij 0.45's live kitty-graphics handshake, and any degraded modes.

Backend adapters own server-log locations. Doctor's collector in [`mux_log.rs`](../../crates/rimz/src/cli/doctor/mux_log.rs) reads a bounded tail, assembles multi-line records, stamps each from the backend's line format, and drops everything at or before the `rimz doctor --clear` watermark. Its classifier names each issue: ordinary lifecycle records (a client leaving, a closed pane's pty, a late action acknowledgement) are `expected` and fold into counted lines, and everything else is `investigate`. A record wrapped in a generic header is named by its `Caused by:` chain, so two unrelated failures under one wrapper stay two issues.

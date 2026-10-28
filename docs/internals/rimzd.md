# The rimzd view

`rimzd` is the background view RimZ owns in a managed room: a tmux window or Zellij tab that holds a sidebar and the long-lived processes the room depends on, kept out of the user's focus. RimZ specifies its panes, births them with the room, and rebuilds any that disappear. The view leads the tab order at birth. tmux moves it back to the front on every later `rimz start`, while a Zellij tab added to a running session is appended and stays where it lands ([multiplexers.md](./multiplexers.md#the-daemon-view-and-resumed-births)).

Two modules carry it. [`daemon_view.rs`](../../crates/rimz/src/daemon_view.rs) builds the pane specification, matches live panes against it, and repairs the difference. [`daemon_content.rs`](../../crates/rimz/src/daemon_content.rs) runs the supervisor that holds each middle-column command. Users shape the middle column through the `[daemon]` table ([configuration.md](../guide/configuration.md#daemon-view)).

## What the view contains

The view has three columns: the sidebar on the left, content in the middle, and a runtime column on the right whose panes stack top to bottom.

```text
┌─────────┬──────────────────┬─────────────────────┐
│ sidebar │ content slot 0   │ Codex broker        │
│         ├──────────────────┼─────────────────────┤
│         │ content slot 1   │ Claude host         │
│         │ …                ├─────────────────────┤
│         │                  │ loop panel          │
└─────────┴──────────────────┴─────────────────────┘
```

The sidebar is the room's ordinary renderer, kept to one per view by [sidebar reconcile](./multiplexers.md#one-sidebar-per-view). This module manages the other four kinds of pane, each launched from a fixed argv (`rimz` is the room's resolved RimZ binary):

| Pane | Column | Argv | Present when | Runs from |
| --- | --- | --- | --- | --- |
| Content slot | Middle | `rimz daemon content --slot <n> --worktree-root <path>` | Always: one per resolved `[[daemon.pane]]`, or one default | Worktree root |
| Codex app-server broker | Runtime | `rimz codex app-server serve --workspace-id <id> --session-name <name>` | The Codex binary resolves: a non-empty `RIMZ_CODEX_BIN`, else `codex`, looked up through `PATH` | Worktree root |
| Claude remote-control host | Runtime | `env CLAUDE_CONFIG_DIR=<home> claude remote-control --spawn worktree`, or `env -u CLAUDE_CONFIG_DIR claude remote-control --spawn worktree` when the room's Claude login sets no config directory | Remote control is on for Claude in this room and the readiness probe reports ready | Project root |
| Loop panel | Runtime | `rimz loop watch --hold` | Always | Worktree root |

The broker keeps one handshaked `codex app-server` warm for context enrichment ([adapter_codex.md](./agents/adapter_codex.md#app-server-enrichment)), whatever `[remote_control] codex` says. The Claude host runs from the project root so the worktrees `--spawn worktree` creates for phone sessions hang off the canonical repository, never the room's current checkout ([remote.md](../guide/remote.md#answer-asks-from-your-phone)). Its argv and readiness checks are in [adapter_claude.md](./agents/adapter_claude.md#readiness). The loop panel is a display, present even in a room that has never run a task. Scheduled runs open [their own tabs](./harness/loops.md#where-a-scheduled-run-lands).

## Building the specification

`daemon_view_spec` is a pure function from `DaemonViewSpecParams` to the `DaemonView` the mux consumes: content panes, host panes, and the loop panel. Every condition arrives resolved, so birth and every repair path build the same specification from the same facts, and the specification never derives policy itself.

| Input | Decides | Callers take it from |
| --- | --- | --- |
| `codex_present` | The broker pane | `agents::runtime_control::installed_broker_bin` is `Some` |
| `claude_host_argv` | The Claude host pane and its argv | `ReadinessSnapshot::claude_host_argv`, `Some` only when Claude remote control is on for the room and the probe returned ready |
| `daemon` | The content pane count | The length of `daemon_content::resolve_content` over the machine `[daemon]` table |
| `project_root` | The Claude host's working directory | The workspace record |
| `worktree_root` | Every other pane's working directory and the supervisors' `--worktree-root` | The workspace record, falling back to `project_root` |
| `rimz_bin`, `workspace_id`, `session_name` | Argv tokens | The room |

Hosts are ordered broker first, then Claude, and that order is the runtime column's placement order. A host is on for the room under `remote_control::room_config`: machine `[remote_control]` minus the hosts listed in the workspace record's `remote_control_off`. So a host that room birth waived gets no pane on any later repair, even though machine config enables it.

### Where start gets its readiness

Only `rimz start` and detached start request the view (`RoomBirth::Normal`'s `background_view`). Attach and web-session entry request none and inherit whatever view exists.

Room judgment ([`room/judgment.rs`](../../crates/rimz/src/room/judgment.rs) `judge`) produces the readiness the view is built from. It runs `remote_control::prepare_hosts`, probes `ReadinessSnapshot`, and for each installed host that `ReadinessSnapshot::start_gate` reports blocked, it warns, turns that host off, and probes again. A fresh room records the hosts it turned off in `remote_control_off`, so they stay off for this run without asking. A live room keeps its applied policy and only warns. `RoomContext::background_view` then builds the specification from the surviving readiness.

## How managed panes are identified

Managed panes are matched by their launch command. A pane id names a live pane, not its managed role, and the foreground command alone cannot serve either, because a content supervisor or the Claude host puts a child in the foreground. A pane listing offers three strings per pane, and `matching_managed_panes` accepts a pane in the `rimzd` view when any one of them matches a marker:

| String | Zellij | tmux |
| --- | --- | --- |
| Spawn command | The command Zellij launched the pane with | `pane_start_command` |
| Foreground command | The presence plugin's last observed command | `pane_current_command` |
| Title | The layout's pane name, set to the joined argv at birth and at repair | `@rimz_title`, set to the joined argv when repair spawns the pane |

The markers are the variants of `ManagedPaneMarker`. `command_marker` tries them in this order, so the first match classifies a command:

| Marker | Matches a command that | Column |
| --- | --- | --- |
| `ContentSlot(n)` | has the adjacent tokens `daemon content` and a `--slot` token followed by `n` | Content |
| `CodexAppServer` | contains the substring `app-server` | Runtime |
| `ClaudeRemoteControl` | has `remote-control` right after its first token whose file name is `claude` (`pane::command_is_claude_host`) | Runtime |
| `LoopPanel` | contains the substring `loop watch` | Runtime |

A content slot matches only its own `--slot` value, so slot 0 and slot 1 never collide. The Claude marker parses tokens, which keeps `nvim remote-control.md` out of the managed set. The broker and loop markers are substrings, and that is a trap: a user's pane in the view whose command contains `app-server` or `loop watch` counts as a managed duplicate, and reconciliation closes it when it is newer than the real one.

## How the rest of the room treats the view

Three broader predicates in [`pane.rs`](../../crates/rimz/src/pane.rs) classify panes outside this module, and they answer different questions:

| Predicate | True when | Used by |
| --- | --- | --- |
| `command_is_host(command)` | The command line contains `remote-control` or `app-server`, a substring test that ignores content supervisors and the loop panel | Zellij's `is_daemon_host_pane` (`mux/zellij/raw_pane.rs`), which also counts every pane in the `rimzd` tab and checks the spawn command for hosts that re-exec. Tab status (`sidebar/produce/tab_status.rs`), to drop hosts from a tab's work panes |
| `pane_runs_daemon_host(pane)` | The pane's spawn or foreground command passes `command_is_host` | Card admission (`pane_admits_card`) and the host reap (`store/snapshot/view/reap.rs`), to skip a daemon host wherever it sits |
| `pane_is_host(pane)` | The pane runs a daemon host, or it is in the `rimzd` view | The sidebar frame (`sidebar/frame.rs`), to tell a daemon view from a working one. tmux `reconcile_sidebars`, to mark a view occupied. `only_daemon_view`, to tell a room that has nothing but the dashboard left |

Card admission ([`store/snapshot/panes.rs`](../../crates/rimz/src/store/snapshot/panes.rs) `pane_admits_card`) reads the narrow predicate and adds one rule for this view: a `rimzd` pane admits a card only when an agent or a launched child is durably stamped on it. The dashboard's own infrastructure stays chrome, while an agent a user starts here remains addressable. Admission feeds rows, `rimz agents list`, and `agent_panes`, which is where message delivery binds a receiver's pane, so a pane dropped there is also a pane no message can reach.

Tab status and the sidebar bell stay view-level on purpose. `tab_status.rs` drops the whole `rimzd` tab, and a sidebar whose own view is this one returns `BellDecision::DaemonView`. A stamped agent here is addressable and rendered, but gets no tab-status glyph or bell from this view.

The sidebar's remote-control health reads one fact from this module: `claude_host_present` over the frame's panes (`sidebar/enrich.rs`), the same marker repair uses.

## The content supervisor

The mux never runs a configured command directly. A content pane runs `rimz daemon content --slot <n> --worktree-root <path>`, a hidden subcommand ([`cli/daemon.rs`](../../crates/rimz/src/cli/daemon.rs)) that holds one child and swaps it when the configuration changes. The pane's launch command names a slot instead of a command, so the child can change while the specification, the pane's identity, and repair all stay still.

`resolve_slot` answers what slot `n` runs now:

- The reserved command `stats` expands to `rimz stats --refresh --hold` ([stats.md](./stats.md)). Its range and reload keys work normally, but its hint omits quit and `q` and `Ctrl-C` do nothing.
- Any other command is split with `shlex` and run without a shell.
- An absolute `cwd` is used as written, a relative one is joined onto the worktree root, and an absent one means the worktree root.
- An empty or unparseable command is skipped with a warning. When every pane is skipped, or the table is empty, the result is the single default stats pane.
- A slot past the end of the resolved list runs the default stats pane.

The supervisor watches the directory that holds `config.toml` through the [shared mutation watcher](./lsp.md#freshness), which catches the atomic rename RimZ writes with. Only create, modify (rename included), and remove events naming that file wake it. Access events and sibling changes do not. After a change it waits out a 300 ms debounce (`CONFIG_RELOAD_DEBOUNCE`), rereads `[daemon]`, resolves its slot again, and compares argv and cwd. Equal means no action. Different means it spawns the replacement first and terminates the old child only once the new one is running, so a command that fails to spawn leaves the working child in place. A config file that cannot be read or parsed also keeps the current child. A missing file means the default table.

Termination is a ladder: `SIGTERM`, a 300 ms grace (`CHILD_SIGNAL_GRACE`), then `SIGKILL`. `SIGHUP` or `SIGTERM` to the supervisor terminates its child that way and exits. The supervisor ignores `SIGINT`, but its child shares the pane's foreground process group, so `Ctrl-C` reaches the child. The default stats pane ignores it (`--hold`). A configured command that exits on it ends the supervisor as below.

When the child exits, the supervisor exits with the child's exit code, or 1 when a signal killed it (`crates/rimz/src/cli/daemon.rs::run_content`). Managed panes close with their command on both backends, so the next repair pass respawns the slot while the view survives ([Repair](#repair)).

The pane count follows the specification, which only a birth or a repair rebuilds. A `[[daemon.pane]]` entry added to a running room creates a new slot in the next specification, and repair spawns it under the preceding slot: on the host tracker's next pass ([Who repairs, and when](#who-repairs-and-when)), or at the next `rimz start`. A removed entry leaves its slot pane outside the specification. Repair ignores unmatched panes, so the orphaned supervisor resolves past the end of the list and runs the stats pane until the room is reborn.

## Reconciliation

`managed_pane_reconciliation` diffs the specification against a pane listing and returns a spawn list and a close list. For each pane in the specification, it collects the live panes matching that marker, ordered by pane creation ordinal:

- No match: the pane goes on the spawn list.
- One match: nothing to do.
- Several matches: the oldest survives and the rest go on the close list.

Keeping the oldest makes repeated passes stable, so two racing passes converge on the same survivor. Panes that match no marker in the specification are left alone, so a shell a user opened in the view stays.

One rule sits outside that loop. When the specification carries no Claude host, every live Claude host pane goes on the close list, so turning remote control off removes the pane.

## Repair

`repair_daemon_view` applies the diff one pane at a time and returns `Converged` or `Retry`. It plans each step from an authoritative pane listing (`PaneReadConsistency::RequireAuthoritative`, with the 3-second `REPAIR_LIST_TIMEOUT`).

Two situations end the pass before it acts. A listing that fails or times out returns `Retry`, so cached topology never drives a close or a spawn. A listing with no pane in the `rimzd` view returns `Converged`: a closed view is treated as deliberate, leaves no anchor to rebuild against, and gets nothing until the next `rimz start`.

Otherwise closes run first, then spawns. Before it closes a surplus pane that hibernation froze, the pass wakes it (`harness::hibernate::wake_pane_at`). A failed wake or a failed close ends the pass with `Retry` before any spawn. Each spawn is then its own round trip: place one pane, wait for its marker to appear in a fresh listing (`SETTLE_ATTEMPTS`, 5 attempts, `SETTLE_POLL`, 100 ms apart), and plan the next placement from that listing. Each restored pane is an anchor for the next, which is how a wholly missing runtime column comes back as one column. A failed split or a pane that never settles ends the pass with `Retry`.

Placement follows specification order within a column:

1. A missing pane splits below the nearest preceding live member of its column.
2. With no preceding member, it splits below the first live member of its column.
3. With an empty column, the column is created to the right of its structural neighbour: content splits off the sidebar, and runtime splits off the first live content pane, else the sidebar.
4. With none of those alive, it splits right of any pane in the view.

Managed panes close with their command on both backends, whether born with the view or split by repair. On tmux, `open_background_view` pins the view window to `remain-on-exit off` immediately after birth, before further splits, and re-asserts the pin when `rimz start` finds an existing view. All panes in that window inherit it, including a shell the user opens there; other windows and the global setting stay unchanged. The `close_on_exit` option `split_managed_pane` sets also pins a repair split's printed pane id at pane scope. A failed pin only warns.

At `rimz start`, tmux reads one pane listing before removing any retained dead panes. When a live pane other than sidebar chrome remains, the dead panes are swept before repair restores the missing commands. Otherwise, including when only the docked sidebar is live, the replacement window is born before the old window is removed, preserving even a session whose only window is `rimzd`. A sidebar cannot hold the old view up: removing its dead siblings would let it self-close before repair restores them. A failed sweep only warns. A user's pane-level retention inside the view still wins over the window pin, so such an exited pane stays dead until the next start sweeps it. A command that exits between window birth and the pin can likewise leave a dead pane until the next start. If the whole view closes, repair treats it as deliberate on both backends and waits for the next start.

### The shape check

Once the markers are satisfied, repair reads the view's outer pane rectangles through `MuxBackend::view_geometry`, and `judge_daemon_shape` returns a `DaemonShape`:

| Verdict | When | What repair does |
| --- | --- | --- |
| `Healthy` | No column fault | Nothing |
| `Repairable` | Faults, and the loop panel is the [spanning loop](#the-spanning-loop-repair) | Restarts the loop panel |
| `Misshapen` | Faults of any other kind | Nothing: the shape is report-only and stays as the user left it |
| `Unjudged` | No geometry (a fullscreen, zoomed, suppressed, or stacked view), an empty read, or a managed pane missing from it | Nothing |

The faults are the variants of `DaemonShapeFault`. Content members must share one x position and width (else `ContentSplit`), and runtime members must share another (else `RuntimeSplit`). Each band must tile the view's full height, where a foreign pane such as a user shell may fill part of it (else `ContentNotFullHeight` or `RuntimeNotFullHeight`). The runtime band must sit right of the content band (else `RuntimeLeftOfContent`). A missing managed member is for marker repair, never a shape fault. Floating panes and plugins, including Zellij's compact bar, are outside the tiled region.

`inspect_daemon_shape` runs the same judge without waking or changing any pane. `rimz doctor` prints its verdict as the `daemon tab` row ([`cli/doctor/runtime.rs`](../../crates/rimz/src/cli/doctor/runtime.rs)).

### The spanning-loop repair

The one shape repair fixes is a loop panel that spans the bottom of the view beneath both content and a runtime host. `repairable_loop` names the oldest loop panel only when all of these hold:

1. The loop panel reaches the bottom of the view.
2. The panes directly above it tile its whole width and include both a content and a runtime member.
3. Extending those panes down over the loop's rows leaves no column fault.
4. On neither side do the edges of the adjoining panes include both the loop's top and its bottom edge. Zellij gives freed space to such a side neighbour, so the panes above would not take it. tmux's preceding-sibling fill produces the same columns for this family.

For that shape, the pass wakes the loop panel when hibernation froze it, re-lists, re-reads geometry, and closes the panel only if the fresh decision still names the same pane. A changed decision leaves every pane alone and returns `Converged`. After the close, the pass recreates the loop below its preceding runtime host through the normal placement path and waits for it to settle.

A pass restarts at most one loop panel, and content and broker panes are never restarted for shape. The restart cannot cycle across passes: one split divides one pane, so the replacement lies inside a single column and cannot span both again. A backend that does not place the split at the anchor can leave the replacement elsewhere, which the next pass reports as a misshapen view. The native shape tests run only on Zellij 0.45 and later (`daemon_shape_case`).

## Who repairs, and when

Three callers drive repair, all best-effort:

| Caller | When | Scope | On `Retry` |
| --- | --- | --- | --- |
| Room birth (`room/birth.rs`, `launch_background_view`) | `rimz start` finds the view already running (`BackgroundViewLaunch::AlreadyRunning`) | The whole view, from the specification start just built | Discarded |
| Host tracker (`DaemonRepairTracker`) | A pass of the sidebar host's cache refresher, at most once per 30 seconds (`DAEMON_VIEW_REPAIR_TTL` in `sidebar_pane/app/cache_refresh.rs`), after that pass's cache work. The refresher skips passes while no sidebar subscribes to the host. | The whole view | Kept pending for the next pass |
| Remote-control toggle (`remote_control::apply_runtime_toggle`) | `rimz config set remote_control.claude <bool>`, for every known workspace with a live session | The whole view, through `ensure_daemon_view_with_readiness`, so the Claude host appears or closes at once | Discarded |

The tracker and the toggle each run `remote_control::prepare_hosts` before `ReadinessSnapshot::probe`, as room judgment does. Probing first would read a precondition the pass can restore, and close a working host over a gap that lasts only until the pass refills it.

### The host tracker

The tracker makes the common tick free: it rebuilds the specification only when its `DaemonViewInputsStamp` changes. The shared data plane it runs on is in [state.md](./sidebar/state.md#the-room-data-plane).

| Stamp field | Source |
| --- | --- |
| `config_generation` | `MachineConfig::load_stamp_generation`, a hash over the stamped per-machine config files and `~/.rimz` fragments |
| `workspace` | The workspace record's `project_root`, `worktree_root`, and `remote_control_off`, leaving out `updated_at`, which ordinary CLI and hook traffic rewrites |
| `rimz_bin` | Stamped path of the RimZ executable |
| `claude_bin` | Stamped path of `claude` as resolved on `PATH` |
| `codex_bin` | Stamped path of the binary `installed_broker_bin` resolves |
| `claude_settings` | Stamped path of Claude's settings file under the room's Claude login |

A rebuilt specification always gets one authoritative repair. With a stable stamp and no repair outstanding, the tracker reads the host's retained pane frame instead of the backend. The tick ends with no work and no child processes when the frame:

- is for this session
- is fresh within `EVENT_PANE_TTL` (10 seconds)
- already satisfies the specification
- has the same set of `rimzd` pane ids as the last pass

Anything else escalates to `repair_daemon_view`, and a `Retry` outcome keeps the repair outstanding until a later tick converges. Moving or swapping panes without changing membership waits for a host start, another repair trigger, or the next `rimz start`. When the workspace's state paths or record cannot be read, the tick is skipped.

## Where the code lives

| File | What it holds |
| --- | --- |
| [`daemon_view.rs`](../../crates/rimz/src/daemon_view.rs) | Specification, markers and matching, reconciliation, shape inspection, repair and placement, the host tracker |
| [`daemon_view/tests.rs`](../../crates/rimz/src/daemon_view/tests.rs) | Specification, planner, reconciliation, identity, shape, post-wake decision, and tracker tests |
| [`daemon_content.rs`](../../crates/rimz/src/daemon_content.rs) | Slot resolution, the supervisor loop, config watching, child termination |
| [`cli/daemon.rs`](../../crates/rimz/src/cli/daemon.rs) | The hidden `rimz daemon content` entry point and its exit code |
| [`cli/doctor/runtime.rs`](../../crates/rimz/src/cli/doctor/runtime.rs) | Read-only doctor runtime and multiplexer probes, the `daemon tab` row among them |
| [`config/daemon.rs`](../../crates/rimz/src/config/daemon.rs) | `DaemonConfig` and `DaemonPane` |
| [`pane.rs`](../../crates/rimz/src/pane.rs) | `VIEW_NAME`, the host markers, `command_is_host`, `command_is_claude_host`, `pane_runs_daemon_host`, `pane_is_host` |
| [`remote_control.rs`](../../crates/rimz/src/remote_control.rs) | `room_config`, `ReadinessSnapshot`, `prepare_hosts`, `apply_runtime_toggle` |
| [`room/judgment.rs`](../../crates/rimz/src/room/judgment.rs) | Start-time readiness and the hosts a fresh room turns off |
| [`room/mod.rs`](../../crates/rimz/src/room/mod.rs), [`room/birth.rs`](../../crates/rimz/src/room/birth.rs) | The start-time specification and birth-time repair |
| [`sidebar_pane/app/cache_refresh.rs`](../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs) | The host pass that calls the tracker |
| [`mux/mod.rs`](../../crates/rimz/src/mux/mod.rs) | `DaemonView`, `HostPane`, `BackgroundViewOptions`, `PaneRect`, and `ViewGeometry`, the backend-facing types |
| [`mux/tmux/backend.rs`](../../crates/rimz/src/mux/tmux/backend.rs), [`mux/zellij/backend.rs`](../../crates/rimz/src/mux/zellij/backend.rs) | `open_background_view` on each backend |

## Tests

The planner, reconciliation, and shape tests in `daemon_view/tests.rs` build a specification, hand it synthetic listings and rectangles, and assert the next step. They cover a runtime column chaining back from empty, identity surviving foreground command churn, surplus panes closing down to the oldest, a closed view staying closed, gap-zero and gap-one healthy and repairable shapes, foreign rows, report-only shapes, the repair fixpoint, and a changed post-wake decision preventing a close. The tracker tests drive `maintain_with` directly with injected build and repair closures, so stamp invalidation, pane-id changes, and frame classification run without a mux. Slot resolution, reload comparison, and the config watch are unit tests in `daemon_content.rs`. The integration test `crates/rimz/tests/integration/daemon_content.rs` runs a real supervisor and proves a saved config swaps its child in place.

Run them with `cargo xtask test 'daemon_view::'` and `cargo xtask test 'daemon_content::'`, which matches both the unit and the integration module. The live-backend tests in `crates/rimz/tests/integration/backend/zellij/daemon.rs` and `crates/rimz/tests/integration/backend/tmux/layout.rs` prove healthy births and the spanning-loop repair against native geometry while the broker process survives, and the Zellij module also proves a report-only shape leaves every pane unchanged. Run them with `cargo xtask test -P live -E 'binary(integration) & test(/^backend::zellij::daemon::/)'` and the same filter on `backend::tmux::layout::`. Backend placement belongs to the live-backend tier ([multiplexers.md](./multiplexers.md)).

## See also

- [configuration.md](../guide/configuration.md#daemon-view): the `[daemon]` table as users write it.
- [multiplexers.md](./multiplexers.md#the-daemon-view-and-resumed-births): how each backend births the view and stacks its panes.
- [stats.md](./stats.md): the default content pane.
- [adapter_codex.md](./agents/adapter_codex.md#app-server-enrichment): the broker's place in Codex enrichment.
- [adapter_claude.md](./agents/adapter_claude.md#remote-control): the Claude host and its readiness.

# The rimzd view

`rimzd` is the background view RimZ owns in a managed room: a tmux window or Zellij tab that holds a sidebar and the long-lived processes the room depends on, kept out of the user's focus. RimZ specifies its panes, births them with the room, and rebuilds any that disappear. The view leads the tab order when the room is born with it; tmux moves it back to the front on every later `rimz start`, while a Zellij tab added to a running session is appended and stays where it lands ([multiplexers.md](./multiplexers.md#the-daemon-view-and-resumed-births)).

Two modules carry it. [`daemon_view.rs`](../../crates/rimz/src/daemon_view.rs) builds the pane specification, matches live panes against it, and repairs the difference. [`daemon_content.rs`](../../crates/rimz/src/daemon_content.rs) runs the supervisor that holds each middle-column command. Users shape the view through the `[daemon]` table in [configuration.md](../guide/configuration.md#daemon-view).

## What the view contains

The view has three columns, the sidebar on the left, content in the middle, and a runtime column on the right whose panes stack top to bottom:

```text
┌─────────┬──────────────────┬─────────────────────┐
│ sidebar │ content slot 0   │ Codex broker        │
│         ├──────────────────┼─────────────────────┤
│         │ content slot 1   │ Claude host         │
│         │ …                ├─────────────────────┤
│         │                  │ loop panel          │
└─────────┴──────────────────┴─────────────────────┘
```

The sidebar is the room's ordinary renderer, kept to one per view by [sidebar reconcile](./multiplexers.md#one-sidebar-per-view). The other four kinds of pane are managed by this module, each launched from a fixed argv (`rimz` below is the room's resolved RimZ binary):

| Pane | Column | Argv | Present when | Runs from |
| --- | --- | --- | --- | --- |
| Content slot | Middle | `rimz daemon content --slot <n> --worktree-root <path>` | Always: one per resolved `[[daemon.pane]]`, or one default | Worktree root |
| Codex app-server broker | Runtime | `rimz codex app-server serve --workspace-id <id> --session-name <name>` | `codex` resolves on `PATH` | Worktree root |
| Claude remote-control host | Runtime | The host argv from Claude's readiness probe (`claude remote-control --spawn worktree`, optionally behind `env CLAUDE_CONFIG_DIR=<home>`) | `[remote_control] claude = true` and the probe reports ready | Project root |
| Loop panel | Runtime | `rimz loop watch --hold` | Always | Worktree root |

The Claude host runs from the project root so a session started from the phone carves its own worktree off the canonical repository instead of the current checkout ([remote.md](../guide/remote.md#answer-asks-from-your-phone)); its readiness checks are in [adapter_claude.md](./agents/adapter_claude.md#readiness). The broker keeps one handshaked `codex app-server` warm for context enrichment ([adapter_codex.md](./agents/adapter_codex.md#app-server-enrichment)). The loop panel is in the specification even in a room that has never run a task, because scheduled runs need a stable pane to stack against ([the loop zone](#the-loop-zone)).

## Building the specification

`daemon_view_spec` takes `DaemonViewSpecParams` and returns the `DaemonView` the mux consumes: content panes, host panes, and the loop panel. It is a pure function of its inputs, so birth and every repair path build the same specification from the same facts.

Every condition arrives resolved in the inputs. `codex_present` decides the broker. `claude_host_argv` comes from `ReadinessSnapshot::claude_host_argv`, which is `Some` only when Claude remote control is enabled and the probe returned a ready result with a host argv, so the specification never re-derives remote-control policy. Hosts are ordered broker first, then Claude. The content pane count is the length of `daemon_content::resolve_content` over the current `[daemon]` table.

`rimz start` is the only entry point that births the view. It prepares and probes remote-control readiness before any session side effect, refuses to start when an enabled, installed host is blocked (`ReadinessSnapshot::start_gate`), and hands the snapshot to room birth. Attach and web-session entry carry no view request and inherit whatever view exists. Repair holds the same line: it restores missing panes into a view that exists and creates nothing when the view is gone.

## How managed panes are identified

Managed panes are matched by their launch command. A pane id cannot serve: Zellij pane ids are positional and get reused. The foreground command cannot serve either, because a content supervisor or the Claude host puts a child in the foreground. A pane listing offers three strings per pane, and `matching_managed_panes` accepts a pane in the `rimzd` view when any one of them matches a marker:

| String | Zellij | tmux |
| --- | --- | --- |
| Spawn command | The command Zellij launched the pane with | `pane_start_command` |
| Foreground command | The presence plugin's last observed command | `pane_current_command` |
| Title | The layout's pane name, set to the joined argv at birth and at repair | `@rimz_title`, set to the joined argv when repair spawns the pane |

The markers are the taxonomy of `ManagedPaneMarker`:

| Marker | Matches a command that | Column |
| --- | --- | --- |
| `ContentSlot(n)` | contains the tokens `daemon content` and `--slot n` | Content |
| `CodexAppServer` | contains `app-server` | Runtime |
| `ClaudeRemoteControl` | has a token whose file name is `claude`, immediately followed by `remote-control` | Runtime |
| `LoopPanel` | contains `loop watch` | Runtime |

A content slot matches its own `--slot` value, so slot 0 and slot 1 never collide. The Claude marker parses tokens (`pane::command_is_claude_host`) where the others test substrings, which keeps `nvim remote-control.md` out of the managed set.

Three broader predicates in [`pane.rs`](../../crates/rimz/src/pane.rs) classify panes outside this module, and they answer different questions:

| Predicate | True when | Used by |
| --- | --- | --- |
| `command_is_host(command)` | The command line contains `remote-control` or `app-server` (a substring test that ignores content supervisors and the loop panel) | Zellij's daemon-host pane classifier (`mux/zellij/raw_pane.rs`), which also checks the spawn command for hosts that re-exec; tab status, to drop hosts from a tab's work panes |
| `pane_runs_daemon_host(pane)` | The pane's spawn or foreground command passes `command_is_host` | Card admission and the host reap in `store/snapshot`, to skip a daemon host wherever it sits |
| `pane_is_host(pane)` | The pane runs a daemon host, or it is in the `rimzd` view | The sidebar frame, to tell a daemon view from a working one; tmux reconcile, to mark a view occupied; `only_daemon_view`, to tell a room that has nothing but the dashboard left |

The split is what lets a loop-zone run be a card. Admission ([`store/snapshot/panes.rs`](../../crates/rimz/src/store/snapshot/panes.rs) `pane_admits_card`) reads the narrow predicate and adds one rule for this view: a `rimzd` pane admits a card only when an agent is durably stamped on it, so the dashboard's own infrastructure stays chrome while a run pane does not. Admission feeds rows, `rimz agents`, and `agent_panes`, which is where message delivery binds a receiver's pane, so a pane dropped there is also a pane no message can reach.

## The content supervisor

The mux never runs a configured command directly. A content pane runs `rimz daemon content --slot <n> --worktree-root <path>`, a hidden subcommand ([`cli/daemon.rs`](../../crates/rimz/src/cli/daemon.rs)) that holds one child and swaps it when the configuration changes. The pane's launch command names a slot instead of a command, so the child can change while the specification, the pane's identity, and repair all stay still.

`resolve_slot` answers what slot `n` runs now:

- The reserved command `stats` expands to `rimz stats --refresh --hold` ([stats.md](./stats.md)).
- Any other command is split with `shlex` and run without a shell.
- An absolute `cwd` is used as written, a relative one is joined onto the worktree root, and an absent one means the worktree root.
- An empty or unparseable command is skipped with a warning. When every pane is skipped, or the table is empty, the result is the single default stats pane.
- A slot past the end of the resolved list also runs the default stats pane.

The supervisor watches the directory holding `config.toml`, which catches the atomic rename RimZ writes with. After a change it waits out a 300 ms debounce (`CONFIG_RELOAD_DEBOUNCE`), rereads `[daemon]`, resolves its slot again, and compares argv and cwd. Equal means no action. Different means it spawns the replacement first and terminates the old child only once the new one is running, so a command that fails to spawn leaves the working child in place. A config file that cannot be read or parsed also keeps the current child; a missing file means the default table.

Termination is a ladder: `SIGTERM`, a 300 ms grace (`CHILD_SIGNAL_GRACE`), then `SIGKILL`. `SIGHUP` and `SIGTERM` to the supervisor terminate its child that way and exit. `SIGINT` sets a flag nothing reads, so `Ctrl-C` in the pane leaves the supervisor running. When the child exits on its own, the supervisor exits with the child's status, the pane closes, and the next repair pass spawns the slot again.

The pane count follows the specification, which only a repair or a birth rebuilds. A `[[daemon.pane]]` entry added to a running room creates a new slot in the next specification, and repair spawns it under the preceding slot: within the elder's 30-second repair interval, or at the next `rimz start`. A removed entry leaves its slot pane outside the specification; repair ignores unmatched panes, and the orphaned supervisor resolves past the end of the list and runs the stats pane until the room is reborn.

## Reconciliation

`managed_pane_reconciliation` diffs the specification against a pane listing and returns a spawn list and a close list. For each pane in the specification, it collects the live panes matching that marker, ordered by pane creation ordinal:

- No match: the pane goes on the spawn list.
- One match: nothing to do.
- Several matches: the oldest survives and the rest go on the close list.

Keeping the oldest makes repeated passes stable, so two racing passes converge on the same survivor. Panes that match no marker in the specification are left alone, so a shell a user opened in the view stays.

One rule sits outside that loop. When the specification carries no Claude host, every live Claude host pane goes on the close list, so turning remote control off removes the pane.

## Repair

`repair_daemon_view` applies the diff one pane at a time, planning each step from an authoritative pane listing (`PaneReadConsistency::RequireAuthoritative`, 3-second `REPAIR_LIST_TIMEOUT`).

The pass does nothing in two situations. When the listing fails or times out, it returns `Retry` instead of acting on cached topology, so stale truth never drives a close or a spawn. When no live pane carries the `rimzd` view name, the view is closed; a closed view is treated as deliberate, leaves no anchor to rebuild against, and gets nothing until the next `rimz start`.

Otherwise closes run first, then spawns, and each spawn is its own round trip: place one pane, wait for its marker to appear in a fresh listing (`SETTLE_ATTEMPTS`, 5 attempts, `SETTLE_POLL`, 100 ms apart), and plan the next placement from that listing. Each restored pane is therefore an anchor for the next, which is how a wholly missing runtime column comes back as one column. A failed close, a failed split, or a pane that never settles ends the pass with `Retry`. A repaired pane carries close-on-exit like its layout-born twin, so a managed command that exits removes its pane for the next pass to respawn.

Placement follows specification order within a column:

1. A missing pane splits below the nearest preceding live member of its column.
2. With no preceding member, it splits below the first live member of its column.
3. With an empty column, the column is created to the right of its structural neighbour: content splits off the sidebar, and runtime splits off the first live content pane, else the sidebar.
4. With none of those alive, it splits right of any pane in the view.

Placement is decided when a pane is created and never revisited. A pane that satisfies the specification stays where it is, even if a different binary would have placed it elsewhere, until it closes or the view is reborn.

The outcome is `Converged` or `Retry`. Callers treat `Retry` as backpressure to try again later; neither outcome is an error.

## Who repairs, and when

Four callers drive repair, all best-effort:

| Caller | When | Scope |
| --- | --- | --- |
| Room birth (`room/birth.rs`, `launch_background_view`) | `rimz start` finds the view already running (`BackgroundViewLaunch::AlreadyRunning`) | The whole view, from the specification start just built |
| Elder tracker (`DaemonRepairTracker`) | The elected sidebar elder's cache-refresh tick, at most every 30 seconds (`DAEMON_VIEW_REPAIR_TTL` in `sidebar_pane/app/cache_refresh.rs`) | The whole view |
| Remote-control toggle (`remote_control::apply_runtime_toggle`) | `rimz config set remote_control.claude <bool>`, for every known workspace with a live session | The whole view, through `ensure_daemon_view_with_readiness`, so the Claude host appears or closes at once |
| Loop zone (`daemon_view::ensure_loop_panel`) | A scheduled run fires and the loop panel is missing | The loop panel alone ([the loop zone](#the-loop-zone)) |

The toggle, the elder, and the loop zone each run `remote_control::prepare_hosts` before `ReadinessSnapshot::probe`, so a host precondition the pass can restore is restored before readiness judges it.

The elder tracker is built to make the common tick free. Election is in [state.md](./sidebar/state.md#renderers-the-producer-and-consumers). The tracker holds a `DaemonViewInputsStamp` and rebuilds the specification only when the stamp changes:

| Stamp field | Source |
| --- | --- |
| `config_generation` | `MachineConfig::load_stamp_generation`, a hash over the stamped per-machine config files and `~/.rimz` fragments |
| `workspace` | The workspace record's `project_root` and `worktree_root`, leaving out `updated_at`, which ordinary CLI and hook traffic rewrites |
| `rimz_bin`, `claude_bin`, `codex_bin` | Stamped paths of the RimZ executable and of `claude` and `codex` as resolved on `PATH` |
| `claude_settings` | Stamped path of Claude's settings file under the room's Claude login |

A rebuilt specification always gets one authoritative repair. With a stable stamp and no repair outstanding, the tracker reads the sidebar's published pane frame instead of the backend: a frame for this session, fresh within `EVENT_PANE_TTL` (10 seconds), that already satisfies the specification ends the tick with no work and no child processes. A missing, stale, or unsatisfied frame escalates to `repair_daemon_view`, and a `Retry` outcome keeps the repair outstanding until a later tick converges. When the workspace's state paths or record cannot be read, the tick is skipped.

## The loop zone

A scheduled run lands in the runtime column. `split_into_loop_zone` ([`cli/supervised/pane.rs`](../../crates/rimz/src/cli/supervised/pane.rs)) asks `ensure_loop_panel` for the workspace's oldest live loop panel and splits the run pane against it with `SplitPlacement::Stacked`. Which fires land there, and the new-tab fallback, are in [loops.md](./harness/loops.md#where-a-scheduled-run-lands).

The run pane is the one real agent pane inside this view, and it is a card like any other: the exec wrapper stamps the agent on it at launch, so admission keeps it, the sidebar renders it, `rimz agents` lists it, and a queued wake or a steer binds its pane. The panel, the content slots, and the hosts beside it carry no stamp and stay chrome.

`ensure_loop_panel` repairs at fire time, outside the elder's tick:

1. Look for the panel in a listing that prefers authoritative truth, bounded by `LOOP_PANEL_LOOKUP_TIMEOUT` (500 ms). A failed lookup returns `None`, and the run opens a new tab.
2. When the panel is gone, build the effective specification the way the elder does, preparing hosts before probing readiness.
3. List again authoritatively; return the panel if another pass restored it meanwhile.
4. Place the loop panel with the same anchor rules and settle wait as a full repair, and return it. A closed view leaves no anchor, and a failed listing, split, or settle also returns `None`; the run then opens a new tab.

## Where the code lives

| File | What it holds |
| --- | --- |
| [`daemon_view.rs`](../../crates/rimz/src/daemon_view.rs) | Specification, markers and matching, reconciliation, repair and placement, `ensure_loop_panel`, the elder tracker |
| [`daemon_view/tests.rs`](../../crates/rimz/src/daemon_view/tests.rs) | Specification, planner, reconciliation, identity, and tracker tests |
| [`daemon_content.rs`](../../crates/rimz/src/daemon_content.rs) | Slot resolution, the supervisor loop, config watching, child termination |
| [`cli/daemon.rs`](../../crates/rimz/src/cli/daemon.rs) | The hidden `rimz daemon content` entry point |
| [`config/daemon.rs`](../../crates/rimz/src/config/daemon.rs) | `DaemonConfig` and `DaemonPane` |
| [`pane.rs`](../../crates/rimz/src/pane.rs) | `VIEW_NAME`, the host markers, `command_is_host`, `command_is_claude_host`, `pane_runs_daemon_host`, `pane_is_host` |
| [`remote_control.rs`](../../crates/rimz/src/remote_control.rs) | `ReadinessSnapshot`, `prepare_hosts`, `apply_runtime_toggle` |
| [`room/mod.rs`](../../crates/rimz/src/room/mod.rs), [`room/birth.rs`](../../crates/rimz/src/room/birth.rs) | The start-time specification and birth-time repair |
| [`sidebar_pane/app/cache_refresh.rs`](../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs) | The elder tick that calls the tracker |
| [`cli/supervised/pane.rs`](../../crates/rimz/src/cli/supervised/pane.rs) | `split_into_loop_zone` |
| [`mux/mod.rs`](../../crates/rimz/src/mux/mod.rs) | `DaemonView`, `HostPane`, and `BackgroundViewOptions`, the backend-facing types |
| [`mux/tmux/backend.rs`](../../crates/rimz/src/mux/tmux/backend.rs), [`mux/zellij/backend.rs`](../../crates/rimz/src/mux/zellij/backend.rs) | `open_background_view` on each backend |

## Tests

The planner and reconciliation tests in `daemon_view/tests.rs` are pure: they build a specification, hand it a synthetic pane listing, and assert the next step. They cover the cases hardest to reach live: a runtime column chaining back from empty, identity surviving foreground command churn, surplus panes closing down to the oldest, and a closed view staying closed. The tracker tests drive `maintain_with` directly with injected build and repair closures, so stamp invalidation and frame classification run without a mux. Slot resolution and reload comparison are unit tests in `daemon_content.rs`.

Run them with `cargo xtask test 'daemon_view::'` and `cargo xtask test 'daemon_content::'`. Backend placement belongs to the live-backend tier ([multiplexers.md](./multiplexers.md)).

## See also

- [configuration.md](../guide/configuration.md#daemon-view): the `[daemon]` table as users write it.
- [multiplexers.md](./multiplexers.md#the-daemon-view-and-resumed-births): how each backend births the view and stacks its panes.
- [stats.md](./stats.md): the default content pane.
- [adapter_codex.md](./agents/adapter_codex.md#app-server-enrichment): the broker's place in Codex enrichment.
- [adapter_claude.md](./agents/adapter_claude.md#remote-control): the Claude host and its readiness.

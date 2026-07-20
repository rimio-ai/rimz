# The rimzd view

`rimzd` is the background view RimZ owns in a managed room: a tmux window or Zellij tab, forced to the first position and out of the user's focus, holding the sidebar renderer and the long-lived processes the room depends on. RimZ specifies its panes, births them with the room, and rebuilds any that disappear.

Two modules carry it. [`daemon_view.rs`](../../crates/rimz/src/daemon_view.rs) builds the pane specification, classifies live panes against it, and repairs the difference. [`daemon_content.rs`](../../crates/rimz/src/daemon_content.rs) runs the middle column, where a small supervisor holds the configured command. The user-facing surface is the `[daemon]` table in [configuration.md](../guide/configuration.md#daemon-view).

## What the view contains

Three columns: the sidebar on the left, content in the middle, runtime on the right. A room born on tmux with `codex` on PATH and remote control off lists like this, with the room's staged binary path shortened to `rimz`:

```text
%3  0,0    72x24  rimz sidebar serve --mux tmux --workspace-id ws_37e27d… --session-name rimz-demo
%2  73,0   1x24   rimz daemon content --slot 0 --worktree-root /tmp/rimzd-demo
%4  75,0   5x11   rimz codex app-server serve --workspace-id ws_37e27d… --session-name rimz-demo
%5  75,12  5x12   rimz loop watch --hold
```

The runtime column stacks vertically (`%4` above `%5` at the same `x`), and the content column sits between it and the sidebar. Four kinds of managed pane can appear:

| Pane | Column | Present when | Runs from |
| --- | --- | --- | --- |
| Content slot | Middle | Always; one per resolved `[[daemon.pane]]`, or one default | Worktree root |
| Codex app-server broker | Right | `codex` resolves on PATH | Worktree root |
| Claude remote-control host | Right | Remote control is enabled and Claude probes ready | Project root |
| Loop panel | Right | Always | Worktree root |

The Claude host runs from the project root so a session started from the phone carves its own worktree off the canonical repo rather than the current checkout ([remote.md](../guide/remote.md)). The broker holds one warm, already-handshaked `codex app-server` so context enrichment skips the cold-spawn handshake ([performance.md](./performance.md)). The loop panel is always in the spec because scheduled runs need a stable anchor to stack against, even in a room that has never run a task.

## Identity is the command, not the pane id

Managed panes are matched by their launch command. Zellij pane ids are positional and get reused, and the supervisors and remote-control hosts here put children in the foreground, so neither the id nor the current foreground command survives as an identity. Each spawn therefore carries its joined argv as the pane title, and `matching_managed_panes` tests the spawn command, the foreground command, and the title against a marker, accepting a match on any of the three.

The markers are the taxonomy: `ContentSlot(n)`, `CodexAppServer`, `ClaudeRemoteControl`, and `LoopPanel`. A content slot matches on `daemon content` plus its `--slot` value, so slot 0 and slot 1 never collide. The Claude marker parses the command rather than substring-matching it: a token whose file name is `claude` must be followed by `remote-control`, which keeps `nvim remote-control.md` out of the managed set.

Two similarly named predicates answer different questions, and the difference matters at call sites. `command_is_host` asks whether a command line is a managed *host* — the broker or the Claude host only, never a content supervisor or the loop panel. `pane_is_host` asks whether a *pane* belongs to the dashboard at all, and any pane in the `rimzd` view qualifies. The sidebar uses the second to tell a daemon view from a working one; the Zellij pane classifier uses the first to recognize a host that re-execs after launch.

## Building the specification

`daemon_view_spec` takes `DaemonViewSpecParams` and returns the `DaemonView` the mux consumes: content panes, host panes, and the loop panel. It is a pure function of its inputs, so the birth path and the repair path build the same spec from the same facts.

Conditionality lives entirely in the inputs. `codex_present` decides the broker. `claude_host_argv` arrives already resolved from `ReadinessSnapshot::probe`, which is `Some` only when remote control is enabled for Claude and the probe reports ready, so the spec never re-derives the policy. Host order is broker first, then Claude.

`rimz start` is the only entry point that births the view. `rimz attach` and web-session entry pass `BackgroundViewBirth::Skip` and inherit whatever is already there. Repair holds the same line: it restores missing panes into a view that exists, and creates nothing when the view is gone.

## The content column and its supervisor

The mux never runs your configured command directly. It runs `rimz daemon content --slot <n> --worktree-root <path>`, a hidden subcommand whose whole job is to hold one child and swap it when the configuration changes ([`cli/daemon.rs`](../../crates/rimz/src/cli/daemon.rs)).

That indirection is what makes edits apply in place. The pane's launch command names a slot, not a command, so the child underneath can change without the view specification changing and without repair seeing anything move. `resolve_slot` reads the current `[daemon]` config and answers what slot `n` should be running: the reserved token `stats` expands to `rimz stats --refresh --hold` ([stats.md](./stats.md)), any other command is split with `shlex` and run without a shell, and a relative `cwd` is joined onto the worktree root. An unparseable or empty command is skipped with a warning, and a configuration where every pane is skipped falls back to the single default stats pane — the same result as no configuration at all.

The supervisor watches the parent directory of `config.toml` rather than the file, which catches the atomic rename RimZ writes with. Changes debounce for 300ms, then it re-resolves its slot and compares argv and cwd. Equal means no action. Different means it spawns the replacement first and terminates the old child only once the new one is up, so a command that fails to spawn leaves the working child in place. Teardown is a ladder: `SIGTERM`, a 300ms grace, then `SIGKILL`. `SIGINT` is registered to a flag the loop ignores, so `Ctrl-C` in the pane does not take the supervisor down.

Pane *count* is the one thing this cannot absorb. The number of content panes is fixed in the specification at birth, so adding or removing `[[daemon.pane]]` entries takes effect when the room restarts.

## Reconciliation

`managed_pane_reconciliation` diffs the spec against a pane listing and returns two lists. For each spec pane, it collects live panes matching that marker, ordered by pane creation ordinal:

- No match: the pane goes on the spawn list.
- One match: nothing to do.
- Several matches: the oldest survives and the rest go on the close list.

Keeping the oldest makes the outcome stable under repeated passes — a pass that races another pass converges on the same survivor rather than trading places. Panes matching no marker are untouched, so a shell you opened in the view stays.

One rule sits outside the loop: when the spec carries no Claude host, every live Claude host pane goes on the close list. Turning remote control off therefore removes the pane instead of orphaning it.

## Repair

`repair_daemon_view` applies that diff, one pane at a time, against authoritative pane listings.

The pass starts by refusing two situations. If no live pane carries the `rimzd` view name, the view is closed, and a closed view is treated as deliberate: there is no anchor to rebuild against and nothing is created until the next `rimz start`. If the authoritative listing fails or times out (3 seconds), the pass returns `Retry` rather than acting on cached topology, so stale truth never drives a close or a spawn.

Otherwise closes come first, then spawns, and each spawn is its own round trip: place one pane, wait for its marker to appear in a fresh listing (5 attempts, 100ms apart), and plan the next placement from that new listing. This is what lets a wholly missing runtime column rebuild as a column — the first restored pane becomes the anchor the second one places against. A pane that never settles ends the pass with `Retry`.

Placement follows the spec order within a column. A missing pane is placed *below* the nearest preceding live member of its own column, falling back to any live member. When the column has no members at all, it is created to the right of its structural neighbor: content splits off the sidebar, and runtime splits off the first live content pane, or the sidebar if content is gone too.

Placement is decided when a pane is created and never revisited. A pane the specification is satisfied with stays where it is, so panes an older binary placed differently keep their position until they are closed or the view is reborn.

The outcome is `Converged` or `Retry`, and callers use it as backpressure rather than an error.

## Who repairs, and when

Three callers drive repair, all best-effort.

Room birth repairs when `open_background_view` reports the view already running, which is the ordinary case for a `rimz start` against a live room.

The elected sidebar elder owns the steady state through `DaemonRepairTracker`, called on its cache-refresh tick with a 30-second floor. The tracker exists to make the common case free. It holds a `DaemonViewInputsStamp` — the config generation, the workspace roots, and stamped paths for the `rimz`, `claude`, and `codex` binaries plus Claude's settings file — and rebuilds the specification only when that stamp changes. Workspace freshness metadata is deliberately excluded, because ordinary CLI and hook traffic rewrites `updated_at` constantly and none of it changes the view.

With a stable stamp and no repair outstanding, the tracker reads the sidebar's own published pane frame instead of the backend. A frame for this session, fresh within the pane TTL, that already satisfies the specification ends the tick with no work and no child processes. A missing, stale, or unsatisfied frame escalates to the authoritative path, and a `Retry` outcome latches until a later tick clears it.

The third caller is the loop zone, which repairs the panel alone.

## The loop zone

A scheduled `--agent` fire stacks its transient run pane under the loop panel, so run output lands in the runtime column instead of splitting the sidebar or a working tab ([loops.md](../guide/loops.md#what-a-task-does-on-your-machine)). `split_into_loop_zone` finds the oldest live panel and splits against it with `SplitPlacement::Stacked`.

When the panel is gone but the view survives, `ensure_loop_panel` recreates just the panel through the same placement and settle path a full repair uses, then stacks the run under it. When the view itself is gone, or the split fails, the run falls back to its own tab. Manual `rimz loop fire` never enters the zone; it splits beside the caller so its foreground stream stays local.

## Where the code lives

| File | What it holds |
| --- | --- |
| [`daemon_view.rs`](../../crates/rimz/src/daemon_view.rs) | Spec construction, markers and matching, reconciliation, the repair step machine, the elder tracker |
| [`daemon_view/tests.rs`](../../crates/rimz/src/daemon_view/tests.rs) | Planner, reconciliation, identity, and tracker contracts |
| [`daemon_content.rs`](../../crates/rimz/src/daemon_content.rs) | Slot resolution, the supervisor loop, config watching, child teardown |
| [`cli/daemon.rs`](../../crates/rimz/src/cli/daemon.rs) | The hidden `rimz daemon content` entry point |
| [`config/daemon.rs`](../../crates/rimz/src/config/daemon.rs) | `DaemonConfig` and `DaemonPane` |
| [`mux/mod.rs`](../../crates/rimz/src/mux/mod.rs) | `DaemonView`, `HostPane`, and `BackgroundViewOptions`, the backend-facing types |

## Tests

The planner and reconciliation tests are pure: they build a spec, hand it a synthetic pane listing, and assert the next step. That covers the cases hardest to reach live — a runtime column chaining back from empty, identity surviving foreground command churn, surplus panes closing down to the oldest, and a closed view staying closed. The tracker tests drive `maintain_with` directly with injected build and repair closures, so stamp invalidation and frame classification are checked without a mux.

Run them with `cargo xtask test daemon`. Backend placement itself belongs to the live-backend tier ([multiplexers.md](./multiplexers.md)).

## See also

- [configuration.md](../guide/configuration.md#daemon-view) — the `[daemon]` table as users write it.
- [multiplexers.md](./multiplexers.md) — how each backend births the view, names panes, and degrades stacking.
- [stats.md](./stats.md) — the default content pane.
- [performance.md](./performance.md) — the Codex broker's place in the enrichment path.

# Performance

> Mechanisms live in their own docs — this one is the performance model over them. The render loop and snapshot are detailed in [sidebar.md](./sidebar.md), durability and wakeups in [ledger.md](./ledger.md), the `list-panes` round-trip in [multiplexers.md](./multiplexers.md). This doc says where the milliseconds go, which optimizations bound them, and the rules the next change follows.

Perceived latency lives on one thread: the sidebar's render/input loop. Correctness lives in the ledger, off that thread entirely. So every performance change has one job — keep the render thread free — and one guardrail — never trade a correctness invariant for it. The UI may show stale-by-a-tick data; it may never block, drop a keystroke, or freeze the spinner.

## The hot path

`rimz sidebar serve` runs a single event loop that blocks only in `recv` on its wakeup socket (`app::serve`). Three things drive it: the animation tick (advance the spinner on the current snapshot), input (apply in place), and a finished background fetch (fold the new snapshot). Nothing on this thread forks a subprocess or fsyncs.

The expensive work is offloaded:

- The snapshot fetch — a `rimz sidebar snapshot` child that resolves the workspace, calls `list-panes`, and runs git — happens on a background **fetch worker** thread. The worker posts a `snapshot` wakeup datagram when a result is ready, so the loop folds it without polling.
- A jump (`rimz pane focus`, a process spawn plus mux IPC) runs on a **detached thread**. The highlight is already redrawn, so the jump is fire-and-forget.

Two kinds of update reach the loop: a **push** (a `ledger_delta` datagram after any ledger write, or now a context-sidecar write) collapses to one refetch; a **poll** (the ~2s tick, ~500ms while an agent animates) is the backstop that catches drift no write announced — pane and git changes the multiplexer never signals.

## Principles

The rules every performance change here follows. They are ordered: an earlier rule outranks a later one when they conflict.

1. **The render thread never blocks.** No subprocess fork, no fsync, no synchronous `list-panes` on the loop. Offload to a worker and wake the loop when it finishes. A single in-flight fetch is the unit of work; the loop stays interactive while it runs.
2. **Push over poll.** A change that a writer knows about posts a wakeup; the loop folds it within one wakeup. Polling is the missed-wakeup backstop, never the primary channel. When a datasource feeds the UI but is not the ledger (the statusline sidecar), give it a wakeup of its own rather than waiting for the next tick.
3. **Cache the disposable; fsync the durable.** Crash-durability is for the event log alone (per-record `sync_data` — the correctness contract). Runtime caches — the snapshot cache, diff-stats cache, agent-context sidecar — are rebuilt next tick, so they rename atomically *without* fsync (`write_temp_then_rename_cache`). A torn read is impossible either way; only "survives a power cut" is traded, and for a next-tick-rebuilt file that buys nothing while costing two fsyncs on a path the UI waits on.
4. **Single-flight, then coalesce.** One outstanding fetch at a time. A burst of deltas collapses to one fetch (`in_flight`); a delta that races an in-flight fetch defers exactly one follow-up (`refetch_pending`), never a queue. The same single-flight guard sits on the cache producer (`SNAPSHOT_CACHE_TTL`, 750ms) so concurrent sidebars across a fleet share one rebuild instead of stampeding.
5. **Pay the round-trip once per window.** `list-panes` and the git probes are the snapshot's cost; bound them with a short TTL cache and reuse the last good result. A degraded read (an empty body, a live pane missing its command) holds the last good pane list rather than flashing a corrupt frame — see [sidebar.md → Degraded reads](./sidebar.md#presence-model).
6. **Cheapest correct read.** The snapshot rebuild is O(active-events + items), never O(history); archives are touched only at rotation. Skip work that cannot matter — an idle room with no agents skips both sidecar directory scans entirely.

## Cost map

Where the milliseconds are, and what bounds each. Treat the figures as orders of magnitude, not promises.

| Operation | Rough cost | Bound |
| --- | --- | --- |
| `list-panes` (Zellij/tmux IPC) | 200–680ms, occasionally degraded mid-tick | snapshot cache (750ms TTL, single-flight); last-good hold (`PANE_HOLD_MAX`, 4s) |
| git diff-stats per worktree | 4 sequential `git` forks (trunk ref → merge-base → branch → numstat) | diff-stats cache (`DIFF_STATS_TTL`, 2s), keyed on worktree + session |
| snapshot rebuild | O(active-events + items) | event-log rotation caps the active log; carryover preserves the rollup |
| `rimz pane focus` (a jump) | process spawn + mux IPC, tens–hundreds ms | off the render thread (detached); fire-and-forget |
| durable file write | temp + 2 fsyncs (file, parent dir) | reserved for the event log and durable state |
| disposable cache write | temp + atomic rename, 0 fsync | `write_temp_then_rename_cache` |
| frame redraw | sub-millisecond, in-process | animation tick gated to `ANIMATION_FRAME` (120ms) |

## Key optimizations in place

The 2026-05 performance pass. Symptom → fix → where.

| Symptom | Fix | Where |
| --- | --- | --- |
| Click/Enter "feels slow" | Run `rimz pane focus` on a detached thread; the keypress returns after the highlight redraw | `app::spawn_pane_focus` |
| Animation freezes; keystrokes land in a late burst | Move the snapshot subprocess to a background fetch worker; the loop blocks only in `recv` and folds results via a `snapshot` wakeup | `app::serve`, `spawn_fetch_worker`, `request_fetch`, `apply_fetch_outcome` |
| `$`/token figure lags, updates in ~2s steps | Statusline sidecar write posts a sidebar wakeup so the cost pushes a repaint; while an agent animates, refetch every `ACTIVE_REFRESH` (500ms) instead of suppressing for a full tick | `cli::statusline::persist_context`, `ledger::wakeup::wake_sidebars_for_context`, `app::serve` |
| Per-frame env read on the hottest path | Cache the `NO_COLOR` lookup in a `OnceLock` (immutable for the process) | `render::theme::Theme::from_env` |
| Two fsyncs per disposable-cache write | `write_temp_then_rename_cache` (atomic rename, no fsync) for the snapshot/diff-stats caches and the agent-context sidecar | `ledger::atomic`, `cli::sidebar`, `ledger::agent_context` |
| Idle room re-scans the activity dir every tick | Gate `agent_activity::read_all` behind `!agents.is_empty()`, beside the context read | `cli::sidebar::run` |
| Throwaway allocations per pane per tick | Move `command`/`cwd` out of the owned `RawPane` (`take_*`) instead of cloning | `mux::zellij::list_panes` |

## Deferred candidates

Real wins the pass identified but did not take, because each changes a contract or crosses a backend-parity boundary and deserves its own change with tests. Ranked by expected payoff.

1. **Serve the snapshot read from `latest.json`.** The read path replays the active event log on every fetch; `snapshots/latest.json` already holds the rebuilt rollup. Reading it directly is O(1) versus O(active-events). Risk: read-after-write freshness — a fetch racing a just-appended event must fall back to a rebuild, so the fast path needs a staleness guard.
2. **Parallelize the per-worktree git probes.** The four `git` forks per worktree run sequentially; they are independent and could join. Risk: a process-spawn burst across a many-worktree fleet — bound the fan-out.
3. **Drop the eager `latest.json` rebuild on every mutation.** Rebuilding on every write pays O(active-events) on the write path; a debounced or lazy rebuild would cut it. Risk: it changes the read contract (1) depends on — sequence the two.
4. **Event-triggered pane updates.** Subscribe to multiplexer events instead of polling `list-panes`, so a pane open/close pushes a refresh. Zellij exposes pane events only inside a plugin, so this is the rail's path, not the CLI's — and it must keep tmux at parity (see [multiplexers.md](./multiplexers.md)).
5. **Opt-in async / group-commit event-log fsync.** The per-record `sync_data` is the durability floor; a workspace that tolerates bounded event loss could batch it for throughput. Risk: correctness — this stays opt-in and documented, never the default (principle 3).
6. **Persistent Codex app-server connection.** Each Codex datapoint currently spawns; a held connection amortizes the handshake. Risk: connection lifecycle and ownership.
7. **Remove the dead Zellij `pipe` broadcast.** The post-write `zellij pipe --name rimz::feed` (a per-ledger-write subprocess on Zellij) has no consumer — the native pane wakes over the socket. Removing it deletes a fork per write and shrinks the surface. Confirm no rail build depends on it first (see [ledger.md → Wakeups](./ledger.md#wakeups)).
8. **DRY the status and hit-test helpers.** The `AgentStatus` rank/glyph/attention logic and the mouse-hit-test geometry are duplicated across the renderer and `app`. Quality, not throughput — fold into one authority on a focused pass.

## Adding a performance change

1. Name the thread the cost lands on. If it is the render/input loop, the change is wrong until the work moves off it.
2. Prefer a push (a wakeup the writer already posts) over shortening a poll interval. A tighter poll burns cycles in the idle case; a wakeup costs nothing until something changes.
3. Decide durability explicitly: durable state fsyncs, a next-tick-rebuilt cache does not. Do not reach for `write_temp_then_rename` on a disposable file.
4. Keep single-flight: a new fetch trigger routes through `request_fetch`, never a bare spawn, so it coalesces with the rest.
5. Measure the idle case too — an optimization that speeds the busy path by adding idle work is usually a loss; the fleet is idle most of the time.
6. The gate is the proof. `cargo xtask ci` stays green — including the invariant that the sidebar never imports a ledger-writer module and never forks `pane capture`/`send` on the render path.

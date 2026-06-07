# Sidebar State And Timing

This doc owns the sidebar data plane: the node model every renderer runs, the per-lane files the elected producer publishes, the push channels that wake the nodes, and the timing that binds them. Product commitments live in [DESIGN.md](../../DESIGN.md), presence/ranking/recovery live in [sidebar.md](./sidebar.md), and render-thread budgets live in [performance.md](./performance.md).

## The Node Model

Every sidebar renderer is a node holding two-part in-memory runtime state: **pulled truth** — the event-fresh ledger rollup folded over the producer's published pane frame on the fetch worker's warm `RollupCursor` — plus a **typed realtime event store**. Every paint reads `fuse(pulled, events, now)` and renders the resulting `SidebarSnapshot`, and fusion is pure, so a node's frame is a function of its two stores and one clock value.

Durable truth feeds every node identically and bypasses the producer: the rollup is read event-fresh in process (`latest.json` plus the unfolded log tail — [ledger.md](./ledger.md#runtime-projection)), and the per-session sidecars (agent context, subagent context, activity) are read fresh from disk on every fold behind stat-gated parse caches.

One elected producer per node set — the eldest fresh heartbeat per workspace — owns every consistent-cadence external pull (panes, git, `/proc`, spending, accounts) and publishes each lane as its own single-writer cache file under the workspace runtime directory, with temp-file-plus-rename writes. Every other node consumes those files in process and never pulls for freshness on its own; the producer consumes its own published fast lane before paying a producing refresh. Realtime events never patch a published file — pulled truth on disk is written only by producer pulls.

Any process may broadcast typed wakeup events to every fresh node: ledger writers, context-sidecar writers, `rimz reload`, the Zellij presence plugin through the CLI, and the elder renderer's own watch threads (the tmux control-mode watcher, the [transcript watcher](#push-channels)). Events are latency hints layered over the pulls; a missed datagram costs staleness bounded by the next pull, never correctness. A dead producer is handled the same way every degradation is — by the heartbeat election ([Failure Modes](#failure-modes)).

## Published Files

One file per lane, one writer per lane. This table is the inventory — names, ownership, and what the stamp means; the cadence values live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs) and each file's mechanics in the module that writes it.

The pane frame is a typed mux topology: `PaneFrame` contains tabs/windows, each `TabFrame` contains one structural `active_pane`, and each `PaneState` carries the pane's current process record, optional previous process record, child pids, and sampled resource metrics. The view-model fold still projects rows from `PaneRef`s; the frame is the producer/consumer cache shape that preserves view structure and process rotation.

Consumers never produce for freshness on their own. They fold the published pane frame over an event-fresh ledger rollup in process, then read the producer's published enrichment caches.

| File (workspace runtime dir) | Writer | Readers | Freshness semantics |
| --- | --- | --- | --- |
| `snapshot.json` | producer ([`sidebar::produce::panes`](../../crates/rimz/src/sidebar/produce/panes.rs)) | every node's consumer fold | the pane frame alone — panes, command/cwd, metrics figures; `produced_at_ms` is the fusion supersession baseline; two-mode TTL, poll vs presence-stamp event mode |
| `presence.stamp` | `rimz sidebar wake` (Zellij presence plugin) | producer | mtime stamp; fresh stretches the pane TTL to event mode |
| `diff-stats.json` | producer ([`sidebar::produce::git`](../../crates/rimz/src/sidebar/produce/git.rs)), single-flighted on `diff-stats.lock` | every node | per-root stamps on activity-tiered TTLs; carries the cached group-root enumeration |
| `metrics-sample.json` | producer ([`sidebar::produce::metrics`](../../crates/rimz/src/sidebar/produce/metrics.rs)) | producer only | sample stamp plus the pane→root-pid bindings; the displayed values reach consumers on the pane frame |
| `provider-spending.json` | producer ([`sidebar::produce::spending`](../../crates/rimz/src/sidebar/produce/spending.rs)) | every node | walk stamp, fleet totals, and the cockpit overlay's live-session baselines |
| `pricing-cache.json` | producer's spending walk (TTL-gated remote refresh inside it — [pricing.md](./pricing.md)) | producer's spending walk | remote-refresh layer over the embedded snapshot, daily TTL with failure backoff |
| `accounts.json` | producer account probe | every node | success/retry TTL stamps |
| `rate_limits.json` | producer and the detached `rimz codex refresh-rate-limits` helper | every node | account-scoped budget windows, throttled per target |
| `agent_context/`, `subagent_context/`, `agent-activity/` | CLI hook and statusline producers; the Codex transcript refresh from any of its [triggers](#push-channels) | every node | latest-wins per session, TTL-bound, stat-gated parse caches on the read side |
| `heartbeat/sidebar.<instance>.json` | each node | election, launch gate, wakeup fanout | written at startup, then throttled below the liveness TTL |
| `sock/sidebar.<instance>.sock` | bound by each node | wakeup senders | the per-instance datagram socket of record |

The ledger's own caches (`snapshots/latest.json`, `snapshots/rollup.json`) are state-dir files owned by the ledger write tail; [ledger.md](./ledger.md) owns them.

## Event Store

Datagrams carry `SidebarEventEnvelope` from [`schema/sidebar_event.rs`](../../crates/rimz/src/schema/sidebar_event.rs). The envelope names the schema version, workspace id, scope, sender timestamp, and typed event body.

The envelope's `session_name` is the scope: `Some` targets the one mux session whose pane ids the event names, and `None` is workspace-scoped — ledger deltas, reloads, and pane-frame publications apply to every renderer of the workspace.

The renderer rejects events outside its workspace or session before appending them. Appended events store both `sent_at_ms` for supersession and `received_at_ms` for TTL, so clock skew cannot make an event immortal.

The store keeps overlay events only: `PaneClosed`, `CommandChanged`, `FocusChanged`, and `PaneOpened` when it carries a command. Nudge-only events drive fetches and do not occupy the store.

## Event Taxonomy

| Event | Payload | Fusion Action | Emitter |
| --- | --- | --- | --- |
| `PaneClosed` | `pane_id` | Delete every rendered row bound to the pane | Zellij plugin through `rimz sidebar wake` |
| `CommandChanged` | `pane_id`, `command` | Overlay command and reset the pane's row shape until the pull verifies it | Zellij plugin through `rimz sidebar wake` |
| `FocusChanged` | focused and unfocused pane ids, possibly spanning views | Mirror per-view focus bits onto every row; retarget the own-view baseline only for one of the view's own working panes | Zellij plugin through `rimz sidebar wake` |
| `PaneOpened` | `pane_id`, optional `command` | Synthesize a placeholder row when command is present; otherwise nudge a producer verification pull | Zellij plugin for exact opens |
| `PanesChanged` | none | Nudge a producer verification pull — topology moved, identity unknown | tmux control-mode watcher, the Zellij plugin's manifest fold, any sparse poke |
| `LedgerDelta` | optional event method and agent event name | Refetch the ledger rollup; session start/end also request fresh panes | Ledger writers and context sidecar writers |
| `PaneFramePublished` | none | Fold the just-published producer pane frame from cache | Producer after a pane-frame publish |
| `Reload` | none | Re-exec or hard-refresh the renderer | `rimz reload` |

## Push Channels

Each push channel exists so a change a writer already knows about reaches every node within one wakeup instead of a poll window; the producer's pull stays the structural backstop behind all of them.

- **Ledger and sidecar writers** post a `LedgerDelta` after every durable write or context-sidecar merge — status, tokens, and cost repaint within one wakeup.
- **The Zellij presence plugin** pushes exact pane events and stamps `presence.stamp`; **the elder's tmux control-mode watcher** broadcasts `PanesChanged` per topology notification ([multiplexers.md](./multiplexers.md)).
- **The elder's transcript watcher** ([`transcript_watch.rs`](../../crates/rimz/src/sidebar_renderer/app/transcript_watch.rs)) holds a filesystem watch on each live Codex session's rollout JSONL and runs the stat-gated context refresh on the write, covering the mid-turn gap between hook pushes — Codex hooks fire only at progress events, so a long generation otherwise goes quiet until the next tick. The refresh merges the sidecar and posts the same `LedgerDelta` the hook path does; only the elected elder watches, demotion drops the watch, and a watcher that never starts costs nothing because the producer-tick refresh stays unconditional.

## Fusion Rules

Fusion is pure over pulled truth, event store, and `now_ms`; it performs no IO, subprocess work, or clock reads.

`SidebarSnapshot::panes_produced_at_ms` is the supersession baseline. An event whose `sent_at_ms` is not newer than the pane frame is skipped because the pull already observed later pane truth.

`PaneClosed` has highest precedence and deletes rows before other overlays run. `PaneOpened` with a command then synthesizes a placeholder row using the same durable pane-id row key the pull uses. `CommandChanged` overlays the command for non-deleted panes. The newest `FocusChanged` event lands last: row bits mirror every per-view mark in the patch, and the own-view baseline retargets only onto one of the view's own working panes (`SidebarOwnView::working_pane_ids`) — a focus move in another tab is that view's mark, never this renderer's selection baseline.

Expired events disappear by receiver-clock TTL. A wrong visual verdict caused by a missed event or clock skew is bounded by the next producer pull.

## Pull-Tick Table

The table names staleness-budget semantics. Exact values and rationale comments live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs), and the registry is `PULL_CADENCES`.

| Lane | Cadence | Where Felt |
| --- | --- | --- |
| Pane frame | `SNAPSHOT_CACHE_TTL` in poll mode; `EVENT_PANE_TTL` while the presence stamp is fresh | Pane open/close and cwd/command regrouping when no exact event arrived |
| Presence stamp | `PRESENCE_STAMP_FRESH` | Switches the producer between poll and event-mode pane TTLs |
| Git diff stats | `DIFF_STATS_TTL` for hot worktrees; `DIFF_STATS_IDLE_TTL` for idle worktrees | Worktree header churn, ahead/behind counts, landed markers |
| Worktree root enumeration | `WORKTREE_ROOTS_TTL` | Grouping for checkouts added without a session boundary |
| `/proc` metrics | `METRICS_SAMPLE_TTL` | `PaneState` child pids plus process-row CPU, memory, and IO figures |
| Spending walk | `SPENDING_TTL` | Fleet ledger and the walked floor under the live cockpit spend overlay |
| Accounts | `ACCOUNTS_TTL` success, `ACCOUNTS_RETRY_TTL` failure | Provider dashboard login, plan, and account state |
| Codex rate limits | `CODEX_RATE_LIMIT_REFRESH_INTERVAL` | Provider dashboard budget windows |

## Render Cadences

`[sidebar] refresh_ms` is the base render grid and defaults to `DEFAULT_REFRESH_MS`. It rides `snapshot.sidebar`, so the renderer uses the default until the first fold and picks up config changes on later folds without reading config itself.

Money rolls sample on `refresh_ms * CLICK_PHASES`, matching the odometer phase counter. Cosmetic attention breath keeps its absolute `SLOW_ANIMATION_FRAME` floor and clamps to at least the configured base. Input paints synchronously off-grid; an overlay event fuses on arrival and paints on the spot, and a burst of events still coalesces to one paint per base frame.

The data backstop remains `rimz sidebar serve --tick-seconds`. Changing `refresh_ms` changes paint cadence, not pull cadence.

## Failure Modes

A missed event is acceptable because events are latency hints. The producer's next pull is the structural backstop.

A dead producer is handled by heartbeat election. Once the stale heartbeat ages out, the next eldest renderer becomes producer; status keeps flowing through consumer rollup folds while pane presence waits for the handoff.

Clock skew cannot make events immortal because TTL uses receiver time. A skewed sender timestamp can produce a short visual mis-ordering, and the verifying pull corrects it.

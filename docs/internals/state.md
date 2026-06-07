# Sidebar State And Timing

This doc owns sidebar timing and state-flow: how each renderer maintains two-part in-memory state and fuses it into each frame. Product commitments live in [DESIGN.md](../../DESIGN.md), presence/ranking/recovery live in [sidebar.md](./sidebar.md), and render-thread budgets live in [performance.md](./performance.md).

## Two-Part State

Each sidebar renderer keeps in-memory state as pulled truth plus a realtime event store.

Pulled truth is the producer's per-data-type view of panes, git, `/proc`, spending, accounts, and provider sidecars, stamped with provenance and folded through the shared enrichment spine into `SidebarSnapshot`.

The realtime event store is per renderer process, in memory, strongly typed, receiver-clock TTL-bound by `EVENT_STORE_TTL`, and deduped latest-wins per pane event key plus one focus key.

Every paint reads `fuse(pulled, events, now)` and renders the resulting `SidebarSnapshot`.

## Pulled Truth

One elected producer per workspace owns consistent-cadence pulls. It publishes the pane frame and enrichment caches under the workspace runtime directory with temp-file-plus-rename cache writes.

Consumers never produce for freshness on their own. They fold the published pane frame over an event-fresh ledger rollup in process, then read the producer's published enrichment caches.

Pulled truth is written only by producer pulls. Realtime events never patch the published pane frame on disk.

The exact cadence values live in [`crates/rimz/src/sidebar/timing.rs`](../../crates/rimz/src/sidebar/timing.rs).

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
| `FocusChanged` | focused and unfocused pane ids | Override row focus bits and the own-view active pane baseline | Zellij plugin through `rimz sidebar wake` |
| `PaneOpened` | `pane_id`, optional `command` | Synthesize a placeholder row when command is present; otherwise nudge a producer verification pull | Zellij plugin for exact opens |
| `PanesChanged` | none | Nudge a producer verification pull — topology moved, identity unknown | tmux control-mode watcher, the Zellij plugin's manifest fold, any sparse poke |
| `LedgerDelta` | optional event method and agent event name | Refetch the ledger rollup; session start/end also request fresh panes | Ledger writers and context sidecar writers |
| `PaneFramePublished` | none | Fold the just-published producer pane frame from cache | Producer after a pane-frame publish |
| `Reload` | none | Re-exec or hard-refresh the renderer | `rimz reload` |

## Fusion Rules

Fusion is pure over pulled truth, event store, and `now_ms`; it performs no IO, subprocess work, or clock reads.

`SidebarSnapshot::panes_produced_at_ms` is the supersession baseline. An event whose `sent_at_ms` is not newer than the pane frame is skipped because the pull already observed later pane truth.

`PaneClosed` has highest precedence and deletes rows before other overlays run. `PaneOpened` with a command then synthesizes a placeholder row using the same durable pane-id row key the pull uses. `CommandChanged` overlays the command for non-deleted panes. The newest `FocusChanged` event overrides the focus baseline last.

Expired events disappear by receiver-clock TTL. A wrong visual verdict caused by a missed event or clock skew is bounded by the next producer pull.

## Pull-Tick Table

The table names staleness-budget semantics. Exact values and rationale comments live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs), and the registry is `PULL_CADENCES`.

| Lane | Cadence | Where Felt |
| --- | --- | --- |
| Pane frame | `SNAPSHOT_CACHE_TTL` in poll mode; `EVENT_PANE_TTL` while the presence stamp is fresh | Pane open/close and cwd/command regrouping when no exact event arrived |
| Presence stamp | `PRESENCE_STAMP_FRESH` | Switches the producer between poll and event-mode pane TTLs |
| Git diff stats | `DIFF_STATS_TTL` for hot worktrees; `DIFF_STATS_IDLE_TTL` for idle worktrees | Worktree header churn, ahead/behind counts, landed markers |
| Worktree root enumeration | `WORKTREE_ROOTS_TTL` | Grouping for checkouts added without a session boundary |
| `/proc` metrics | `METRICS_SAMPLE_TTL` | Process-row CPU, memory, and IO figures |
| Spending walk | `SPENDING_TTL` | Fleet ledger and the walked floor under the live cockpit spend overlay |
| Accounts | `ACCOUNTS_TTL` success, `ACCOUNTS_RETRY_TTL` failure | Provider dashboard login, plan, and account state |
| Codex rate limits | `CODEX_RATE_LIMIT_REFRESH_INTERVAL` | Provider dashboard budget windows |

## Process Roles

The eldest fresh sidebar instance is the producer. It owns every consistent-cadence pull and publishes cache-class artifacts for the workspace.

Every renderer is a consumer of pulled truth. A producer also consumes its own published fast lane before paying a producing refresh.

Any node may broadcast typed events to every fresh sidebar heartbeat: ledger writers, `rimz reload`, the Zellij presence plugin through the CLI, and the tmux control-mode watcher.

## Render Cadences

`[sidebar] refresh_ms` is the base render grid and defaults to `DEFAULT_REFRESH_MS`. It rides `snapshot.sidebar`, so the renderer uses the default until the first fold and picks up config changes on later folds without reading config itself.

Money rolls sample on `refresh_ms * CLICK_PHASES`, matching the odometer phase counter. Cosmetic attention breath keeps its absolute `SLOW_ANIMATION_FRAME` floor and clamps to at least the configured base. Input paints synchronously off-grid; an overlay event fuses on arrival and paints on the spot, and a burst of events still coalesces to one paint per base frame.

The data backstop remains `rimz sidebar serve --tick-seconds`. Changing `refresh_ms` changes paint cadence, not pull cadence.

## Failure Modes

A missed event is acceptable because events are latency hints. The producer's next pull is the structural backstop.

A dead producer is handled by heartbeat election. Once the stale heartbeat ages out, the next eldest renderer becomes producer; status keeps flowing through consumer rollup folds while pane presence waits for the handoff.

Clock skew cannot make events immortal because TTL uses receiver time. A skewed sender timestamp can produce a short visual mis-ordering, and the verifying pull corrects it.

# Sidebar State And Timing

This doc owns the sidebar **data plane**: the node model every renderer runs, the per-lane caches the elected producer publishes, the typed events that wake the nodes, and the timing that binds them. Product commitments live in [DESIGN.md](../../../DESIGN.md); presence, ranking, and pane binding live in [sidebar.md](./sidebar.md); render-thread budgets live in [performance.md](../health/performance.md); the live-state inspection workflow lives in [diagnostics.md](../health/diagnostics.md#inspecting-live-card-state).

## At a glance

Every sidebar renderer is a **node**. A node paints from two in-memory stores — *pulled truth* and a *typed event store* — fused on every frame by a pure function. One node per workspace is elected **producer** and does all the expensive external work once; every other node consumes what it publishes.

```text
  ledger rollup (durable truth)          elected producer ── pulls ──▶ per-lane caches
  read event-fresh in every node          (panes + roots on fetch · git/spend/accounts on refresher)
            │                                                    │
            └────────────────────┬───────────────────────────────┘
                                 ▼
   every node:   pulled truth   +   event store (wakeup overlays)
                                 │
                                 ▼
              fuse(pulled, events, now)  ──▶  SidebarSnapshot  ──▶  paint
                    (pure: no IO, no subprocess, no clock read past `now`)
```

Durable truth is the ledger; everything the producer publishes is a cache that a pull rebuilds. Wakeup events are latency hints — a dropped datagram costs staleness bounded by the next pull, never correctness.

## The Node Model

A node holds two-part runtime state. **Pulled truth** is the event-fresh ledger rollup folded over the producer's published pane frame and enrichment caches. The **event store** is a small set of typed realtime events received since the last fold. Every paint computes `fuse(pulled, events, now)` and renders the resulting `SidebarSnapshot`, so a node's frame is a function of its two stores and one clock value.

The pane frame admits the cards; the ledger, sidecars, and events only enrich admitted cards. A frameless fold (CLI consumers that want rollup metadata, not a roster) sets `SidebarSnapshot::panes_produced_at_ms` to null and leaves `worktree_groups` empty.

Durable truth feeds every node identically and bypasses the producer: each node reads the rollup in process (`latest.json` plus the unfolded log tail — [ledger.md](./ledger.md#runtime-projection)) and reads the per-session sidecars fresh from disk behind stat-gated parse caches.

One **producer** — the eldest fresh heartbeat per workspace — owns every consistent-cadence external pull and publishes each lane as its own single-writer cache. Its fetch worker publishes pane truth and roots, then projects heavy caches; its elder-gated cache refresher publishes git diff-stats, spending, accounts, usage, credits, auto-continue side effects, and loop-task firing on the same data cadence. Every other node consumes those caches in process and never pulls for freshness on its own; the producer consumes its own published fast lane before paying for a refresh. Realtime events never patch a published cache — pulled truth on disk is written only by producer pulls. A dead producer is a degradation like any other, handled by the heartbeat election ([Failure Modes](#failure-modes)).

## Published Caches

One file per lane, one writer per lane, each written temp-file-plus-rename. Exact freshness values live in [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs); per-file mechanics (locks, single-flighting, repair) live in the module that writes the lane. Account-global data lanes live under `$XDG_STATE_HOME/rimz/shared/` so relaunches open warm; their `*.lock` election files live under `$XDG_RUNTIME_DIR/rimz/shared/`. The rest are room-local under the workspace runtime directory.

The **pane frame** is the topology everything else enriches: `PaneFrame` carries tabs and panes, each pane holding its current (and rotated-out previous) process record, child pids, sampled resource metrics, plugin-resolved active panes, and focus-contention flags for unresolved fallback cases. `observed_at_ms` records when the pane source saw the topology and is the fusion supersession baseline (legacy frames fall back to `produced_at_ms`). The frame is both the card-admission boundary and the producer/consumer cache shape that preserves view structure and process rotation; the read-side repair and carry-forward guards that keep a momentary mux glitch off-screen live in [sidebar.md](./sidebar.md#honest-reads-across-a-mux-hiccup).

| Lane | Writer | Readers | Carries |
| --- | --- | --- | --- |
| `snapshot.json` | producer ([`produce::panes`](../../../crates/rimz/src/sidebar/produce/panes.rs)) | every node's fold | the typed pane frame:<br>- panes with foreground/spawn command and cwd repaired from `/proc` when the mux races<br>- metrics, carried panes, focus-contention flags, `viewed_panes` global focus, and client presence<br>- observation stamp plus producer `build` id; `observed_at_ms` is the supersession baseline, with legacy `produced_at_ms` fallback<br>- poll-mode freshness by default, presence-stamp event TTL while `presence.stamp` is fresh |
| `pane-topology.json` | Zellij presence plugin via the host CLI | Zellij producer pull | a pre-producer Zellij hint — live panes, tab names, authoritative per-tab active panes usable with topology-served or CLI-served rosters, raw focus candidates, geometry |
| `presence.stamp` | Zellij plugin, tmux control-mode watch | producer | an mtime liveness mark; while fresh, the pane lane runs on the shorter event-mode TTL |

Producer **enrichment lanes** fold onto the admitted cards. The fetch worker handles `/proc` and group roots with pane production; the cache refresher handles git, spending, accounts, usage, credits, auto-continue, and loop-task firing so the worker's fast lane projects their last published values instead of waiting behind them. The figures reach consumers through the cache (or, for metrics, restamped onto the pane frame).

| Lane | Scope | Carries |
| --- | --- | --- |
| `diff-stats.json` | room | per-worktree git facts split into edit-sensitive stats (`added`/`removed`, dirty/untracked state, branch, merge/rebase state) and commit/PR-shaped stats (ahead/behind counts, landed markers, did-work marker), each with its own stamp, plus the group-root set |
| `pr-state.json` | room | producer-only `gh`/`tea` pull-request state by worktree path, absent when no PR or unsupported forge |
| `metrics-sample.json` | room (producer-only) | per-pane resource samples and pane→root-pid bindings; figures publish on the pane frame |
| `workspace-spending.<hash>.json` | room | the room's cockpit spend tally |
| `live-spend-baselines.json` | room | per-row cost baselines for the room-local live count-up |
| `link-stats.json` | room | the latest remote-SSH probe stats for the footer link badge ([remote.md](../reach/remote.md)) |
| `provider-spending.json` | account-global | user-global fleet/provider spend totals and the walk stamp |
| `spending.json` | account-global | the incremental transcript parse cache behind the spend walk |
| `pricing-cache.json` | account-global | the remote token-price refresh over the embedded snapshot ([provider.md](../agents/provider.md#token-pricing)) |
| `accounts.json` | account-global | per-provider login, plan, and account state |
| `rate_limits.json` | account-global | per-account budget windows |
| `credits.json` | account-global | provider-reported paid/extra usage |

Per-session **sidecars** (`agent_context/`, `subagent_context/`, `agent-activity/`) are the exception to producer ownership: CLI hook and statusline runs write them latest-wins, and the elder's transcript watcher refreshes Codex context between hooks ([Push Channels](#push-channels)). Every node reads them fresh behind stat-gated parse caches.

The remaining files are **coordination and receipts**, terse by design: `heartbeat/sidebar.<instance>.json` (election and wakeup fanout — the eldest fresh heartbeat is the producer), `sock/sidebar.<instance>.sock` (the node's wakeup datagram socket), `loop-fire.json` (elder loop-task arm/fire stamps for this room), `unread.json` and `read-marks/…` (open unread episodes and per-row read receipts that every fold reads), `binding.log.jsonl` (append-only pane-bind decisions; [sidebar.md](./sidebar.md)), and `diag.log.jsonl` (typed anomaly records; [diagnostics.md](../health/diagnostics.md)). The ledger's own `snapshots/latest.json` and `snapshots/rollup.json` are state-dir files owned by the ledger write tail — [ledger.md](./ledger.md) owns them.

## Realtime Events

Wakeup datagrams carry `SidebarEventEnvelope` ([`schema/sidebar_event.rs`](../../../crates/rimz/src/schema/sidebar_event.rs)): a schema version, the workspace id, an optional session scope, a sender timestamp, and the typed event. `session_name` is the scope — `Some` targets the one mux session whose pane ids the event names, `None` is workspace-wide (ledger deltas, reloads, and pane-frame publications reach every renderer of the workspace).

The receive path drops an event for another workspace or session before it reaches the store. The store keeps each event under a receiver-clock TTL and records both `sent_at_ms` (for supersession) and the receive time (for expiry), so a skewed sender clock can mis-order an overlay briefly but never pin it. Only **overlay** events live in the store — `PaneClosed`, `CommandChanged`, `FocusChanged`, and a `PaneOpened` that carries a command; the rest are consumed as renderer actions or producer-verification nudges.

### Event Taxonomy

| Event | Payload | Fusion action | Emitter |
| --- | --- | --- | --- |
| `PaneClosed` | `pane_id` | Delete every row bound to the pane (highest precedence) | Zellij plugin, tmux control-mode watch |
| `CommandChanged` | `pane_id`, `command` | Overlay the command until a pull verifies the pane's row shape | Zellij plugin, tmux watch |
| `FocusChanged` | focused / unfocused pane ids | Mirror focus bits onto every row; retarget the own-view baseline only onto one of the view's own working panes | Zellij plugin, tmux watch, renderer jumps |
| `FocusStranded` | sidebar `pane_id` | Renderer action only: the matching sidebar pane refocuses its held or own-view working sibling | Zellij plugin |
| `PaneOpened` | `pane_id`, optional `command` | Nudge a producer verification pull; never admits a card on its own | Zellij plugin, tmux watch |
| `PanesChanged` | none | Nudge a producer pull — topology moved, identity unknown | tmux watch fallback, the Zellij manifest fold |
| `LedgerDelta` | optional event method and lifecycle signal | Refetch the rollup; a session start/end also requests fresh panes | ledger and context-sidecar writers |
| `PaneFramePublished` | none | Fold the just-published producer pane frame from cache | producer |
| `Notify` | `title`, `body`, target panes, `recheck_unread`, kind | Renderer action only: raise the configured desktop/bell/command notification, gated on row-unread when `recheck_unread`; never fused into rows ([notifications.md](./notifications.md)) | the notification path |
| `Reload` | none | Re-exec or hard-refresh the renderer | `rimz reload` |

### Push Channels

Each push channel exists so a change a writer already knows about reaches every node within one wakeup instead of a poll window; the producer's pull stays the structural backstop behind all of them.

- **Ledger and sidecar writers** post a `LedgerDelta` after every durable write or context-sidecar merge, so status, tokens, and cost repaint within one wakeup.
- **The Zellij presence plugin** pushes exact pane events, stamps `presence.stamp`, and publishes `pane-topology.json` through the host CLI; **the tmux control-mode watcher** pushes typed pane overlays from its `refresh-client -B` subscription, stamps `presence.stamp`, and falls back to `PanesChanged` for identity-free topology notices ([multiplexers.md](./multiplexers.md)).
- **The elder's transcript watcher** ([`transcript_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/transcript_watch.rs)) watches each live Codex session's rollout JSONL and runs the stat-gated context refresh on write, covering the mid-turn gap where Codex hooks fire only at progress events. Only the elder watches; demotion drops the watch, and a watcher that never starts costs nothing because the producer-tick refresh stays unconditional.
- **The elder's cache refresher** ([`cache_refresh.rs`](../../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs)) ticks on the data cadence, re-checks the heartbeat election each pass, refreshes heavy caches from the last published pane frame, and fires due loop tasks for this room. Demotion turns it into a sleeper; a panic resets its rollup cursor and the next tick retries from cache truth.

Focus drives a dynamic fast tick for the work the user is viewing. The producer folds `PaneFrame.viewed_panes` into `SidebarSnapshot::viewed_panes`; git edit-sensitive facts for the viewed worktree and `/proc` metrics for the viewed pane run on the focused tier, while commit-shaped git facts and every background worktree/pane stay on their cheaper cadences.

Client presence rides the same producer sample as viewed panes. The mux `client_view` read returns attached human clients plus the panes they view; tmux also returns the freshest `client_activity` epoch, so `SidebarPresence::classify` marks `Idle` once input is quiet for `AFK_IDLE_THRESHOLD_MS` (15 minutes) and `Detached` when no human client remains. Zellij exposes attach state but no per-client input-idle timestamp, so an attached Zellij room stays `Active` until every terminal client detaches. A topology-cache hit carries the prior presence from `snapshot.json` with the prior viewed panes rather than forking `list-clients`.

## Fusion Rules

Fusion is pure over pulled truth, the event store, and `now_ms`: no IO, no subprocess, no clock read past `now`.

`SidebarSnapshot::panes_observed_at_ms.or(panes_produced_at_ms)` is the supersession baseline. An event no newer than the pane observation is skipped — the pull already saw later pane truth. A viewed work tab anchors `active_pane` to `client_view` focus and is not focus-contested; an unviewed plugin-resolved work tab trusts `active_panes` over raw `is_focused` churn on both topology-served and CLI-served roster paths. Only unviewed fallback tabs reach the contested fallback, and a fallback frame whose own view remains focus-contested abstains from superseding `FocusChanged` events that name panes in that contested view.

The overlays apply in precedence order. `PaneClosed` runs first and deletes rows. `PaneOpened` creates nothing; it asks the producer for a verified frame. `CommandChanged` overlays the command for non-deleted, already-admitted panes. The newest `FocusChanged` lands last: row bits mirror every listed focus mark, and the own-view baseline retargets only onto one of the view's own working panes (`SidebarOwnView::working_pane_ids`) — a focus move in another tab is that view's mark, never this renderer's selection. When the event names an own working pane, fusion also marks that pane viewed, so a locally delivered focus move clears unread before the next pull.

Expired events disappear by receiver-clock TTL, and any wrong verdict from a missed event or clock skew is bounded by the next producer pull.

## Pull-Tick Table

The table names staleness-budget semantics; exact values and rationale live in [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs) under `PULL_CADENCES`.

| Lane | Cadence | Where felt |
| --- | --- | --- |
| Pane frame | `SNAPSHOT_CACHE_TTL` in poll mode; `EVENT_PANE_TTL` while the presence stamp is fresh | Pane open/close and cwd/command regrouping with no exact event |
| Zellij topology cache | `PRESENCE_STAMP_FRESH` | Zellij pre-producer pane listing |
| Presence stamp | `PRESENCE_STAMP_FRESH` | Switches the producer between poll and event-mode pane TTLs |
| Git diff stats | focused: `DIFF_STATS_FOCUSED_LOCAL_TTL` local/edit facts and `DIFF_STATS_FOCUSED_COMMIT_TTL` commit/PR facts; background: `DIFF_STATS_TTL` hot and `DIFF_STATS_IDLE_TTL` idle | Worktree header churn, ahead/behind counts, landed markers, trunk-sync classification |
| PR state | `PR_STATE_TTL` success; `PR_STATE_RETRY_TTL` failure | Worktree header PR glyphs after diverged stats |
| Worktree root enumeration | `WORKTREE_ROOTS_TTL` | Grouping for checkouts added without a session boundary |
| `/proc` metrics | `METRICS_FOCUSED_SAMPLE_TTL` viewed; `METRICS_BACKGROUND_SAMPLE_TTL` background | Child pids plus per-row CPU, memory, IO, and process-state figures |
| Spending walk | `SPENDING_TTL` | Provider dashboard, fleet ledger, and the floor under the live cockpit spend |
| Accounts | `ACCOUNTS_TTL` success; `ACCOUNTS_RETRY_TTL` failure | Provider dashboard login, plan, and account state |
| Codex rate limits | `CODEX_RATE_LIMIT_REFRESH_INTERVAL` | Provider dashboard budget windows |
| Account credits | `CREDITS_TTL` success; `CREDITS_RETRY_TTL` failure | Provider dashboard paid/extra usage row |
| Remote link stats | `LINK_STATS_STALE`, expiring at `LINK_STATS_EXPIRE` | Footer link badge for `rimz remote connect` rooms |

## Render Cadences

`[theme.display] refresh_ms` is the base render grid, defaulting to `DEFAULT_REFRESH_MS`. It rides `snapshot.theme.display`, so the renderer uses the default until the first fold and picks up config changes on later folds without reading config itself.

Money rolls sample on `refresh_ms * CLICK_PHASES`, matching the odometer phase counter; row animations sample on `BREATH_ANIMATION_FRAME`, clamped to at least the base. Input paints synchronously off-grid, an overlay event fuses on arrival and paints on the spot, and a burst of events still coalesces to one paint per base frame. The data backstop remains `rimz sidebar serve --tick-seconds`: changing `refresh_ms` changes paint cadence, not pull cadence.

## Failure Modes

A **missed event** is acceptable because events are latency hints; the producer's next pull is the structural backstop.

A **dead producer** is handled by heartbeat election. Once the stale heartbeat ages out, the next eldest renderer becomes producer; status keeps flowing through consumer rollup folds while pane presence waits for the handoff.

**Clock skew** cannot make events immortal because TTL uses receiver time. A skewed sender timestamp can briefly mis-order an overlay, and the verifying pull corrects it.

Every accepted anomaly path writes a typed diagnostic before it falls back, holds, suppresses, or exits. A recurrence of flicker, duplicate rows, or a phantom external group maps to a record in `diag.log.jsonl`; `rimz doctor` shows the recent tail and [diagnostics.md](../health/diagnostics.md) names the taxonomy.

# Sidebar state and timing

This doc owns the sidebar **data plane**: the node model every renderer runs, the per-lane caches the elected producer publishes, the typed events that wake the nodes, and the timing that binds them. Product commitments live in [DESIGN.md](../../../DESIGN.md); presence, ranking, and pane binding live in [sidebar.md](./sidebar.md); render-thread budgets live in [performance.md](../performance.md); the live-state inspection workflow lives in [diagnostics.md](../diagnostics.md#inspecting-live-card-state).

## At a glance

Every sidebar renderer is a **node**. A node paints from two in-memory stores — *pulled truth* and a *typed event store* — fused on every frame by a pure function. One node per workspace is elected **producer** and does all the expensive external work once; every other node consumes what it publishes.

```text
  store rollup (durable truth)          elected producer ── pulls ──▶ per-lane caches
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

Durable truth is the store; everything the producer publishes is a cache that a pull rebuilds. Wakeup events are latency hints — a dropped datagram costs staleness bounded by the next pull, never correctness.

## The node model

A node holds two-part runtime state. **Pulled truth** is the event-fresh store rollup folded over the producer's published pane frame and enrichment caches. The **event store** is a small set of typed realtime events received since the last fold. Every paint computes `fuse(pulled, events, now)` and renders the resulting `SidebarSnapshot`, so a node's frame is a function of its two stores and one clock value.

The pane frame admits the cards; the store, sidecars, and events only enrich admitted cards. A frameless fold (CLI consumers that want rollup metadata, not a roster) sets `SidebarSnapshot::panes_produced_at_ms` to null and leaves `worktree_groups` empty.

Durable truth feeds every node identically and bypasses the producer: each node reads the rollup in process (`latest.json` plus the unfolded log tail — [store.md](../store.md#runtime-projection)) and reads the per-session sidecars fresh from disk behind stat-gated parse caches.

One **producer** — the eldest fresh heartbeat per workspace — owns every consistent-cadence external pull and publishes each lane as its own single-writer cache. Two producer threads split that work. The fetch worker publishes pane truth and group roots each data tick, then projects the heavy caches' last published values into its snapshot. The elder-gated cache refresher owns the TTL-gated refreshes behind those caches — git diff-stats, PR state, spending, accounts, usage, credits, and auto-continue side effects — on the same data cadence, so a status flip never waits behind a `git` fork or a provider probe. Every other node consumes the published caches in process and never pulls for freshness on its own; the producer itself consumes its own published fast lane before paying for a refresh. Realtime events never patch a published cache — pulled truth on disk is written only by producer pulls. A dead producer is a degradation like any other, handled by the heartbeat election ([Failure modes](#failure-modes)).

## Published caches

One file per lane, one writer per lane, each written temp-file-plus-rename. Exact freshness values live in [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs); per-file mechanics (locks, single-flighting, repair) live in the module that writes the lane. Account-global data lanes live under `$XDG_STATE_HOME/rimz/shared/` so relaunches open warm; their `*.lock` election files live under `$XDG_RUNTIME_DIR/rimz/shared/`. The rest are room-local under the workspace runtime directory.

The **pane frame** is the topology everything else enriches: `PaneFrame` carries tabs and panes, each pane holding its current (and rotated-out previous) process record, child pids, sampled resource metrics, `viewed_panes`, the session `focused_pane` register, and client presence. `observed_at_ms` records when the pane source saw the topology and is the fusion supersession baseline. The frame is both the card-admission boundary and the producer/consumer cache shape that preserves view structure and process rotation; the read-side repair and carry-forward guards that keep a momentary mux glitch off-screen live in [sidebar.md](./sidebar.md#honest-reads-across-a-mux-hiccup).

| Lane | Writer | Readers | Carries |
| --- | --- | --- | --- |
| `snapshot.json` | producer ([`produce::panes`](../../../crates/rimz/src/sidebar/produce/panes.rs)) | every node's fold | the typed pane frame:<br>- panes with foreground/spawn command and cwd repaired from the process backend when the mux races<br>- metrics, carried panes, `viewed_panes`, `focused_pane`, and client presence<br>- observation stamp plus producer `build` id; `observed_at_ms` is the supersession baseline<br>- backend roster freshness by default, presence-stamp event TTL while `presence.stamp` is fresh |
| `pane-topology.json` | Zellij presence plugin via the host CLI | Zellij producer pull | Zellij's pane roster — live panes, tab names, the plugin-resolved session focused pane, attached-client count, viewed panes, raw focus candidates, foreground commands, and geometry |
| `presence.stamp` | Zellij plugin, tmux control-mode watch | producer | an mtime liveness mark; while fresh, the pane lane relaxes to the longer event-mode TTL because typed events cover the latency |

Producer **enrichment lanes** fold onto the admitted cards. The fetch worker handles process metrics and group roots with pane production; the cache refresher handles git, PR state, spending, accounts, usage, credits, and auto-continue so the worker's fast lane projects their last published values instead of waiting behind them. The figures reach consumers through the cache (or, for metrics, restamped onto the pane frame).

| Lane | Scope | Carries |
| --- | --- | --- |
| `diff-stats.json` | room | per-worktree git facts split into edit-sensitive stats (`added`/`removed`, dirty/untracked state, branch, merge/rebase state) and commit/PR-shaped stats (ahead/behind counts, landed markers, did-work marker), each with its own stamp, plus the group-root set |
| `pr-state.json` | room | producer-only `gh`/`tea` pull-request state by worktree path, plus per-repo probe stamps, path-to-repo metadata, and last-seen HEAD SHAs; absent when no PR or unsupported forge |
| `metrics-sample.json` | room (producer-only) | per-pane resource samples and pane→root-pid bindings; figures publish on the pane frame |
| `workspace-spending.<hash>.json` | room | the room's cockpit spend tally, headline cutoff, and live-card session keys excluded from walked headline USD |
| `link-stats.json` | room | the latest remote-SSH probe stats for the footer link badge ([remote.md](../remote.md)) |
| `provider-spending.json` | account-global | user-global fleet/provider spend totals and the walk stamp |
| `spending.json` | account-global | the incremental transcript parse cache behind the spend walk |
| `pricing-cache.json` | account-global | the remote token-price refresh over the embedded snapshot ([providers.md](../agents/providers.md#token-pricing)) |
| `accounts.json` | account-global | per-provider login, plan, and account state |
| `rate_limits.json` | account-global | per-account budget windows |
| `credits.json` | account-global | provider-reported paid/extra usage |

Per-session **sidecars** (`agent_context/`, `subagent_context/`, `agent-activity/`) are the exception to producer ownership: CLI hook and statusline runs write them latest-wins, and the elder's transcript watcher refreshes Codex context between hooks ([Push channels](#push-channels)). Every node reads them fresh behind stat-gated parse caches.

The remaining files are **coordination and receipts**, terse by design: `heartbeat/sidebar.<instance>.json` (election and wakeup fanout — the eldest fresh heartbeat is the producer), `sock/sidebar.<instance>.sock` (the node's wakeup datagram socket), `loop-fire.json` (elder loop-task arm/fire stamps for this room), `unread.json` and `read-marks/…` (open unread episodes and per-row read receipts that every fold reads), `focus-anchor.json` (a TTL-gated jump viewport hint that every renderer reads on focus adoption), `binding.log.jsonl` (append-only pane-bind decisions; [sidebar.md](./sidebar.md)), and `diag.log.jsonl` (typed anomaly records; [diagnostics.md](../diagnostics.md)). The store's own `snapshots/latest.json` and `snapshots/rollup.json` are state-dir files owned by the store write tail — [store.md](../store.md) owns them.

Heartbeat lifecycle is bounded by TTL between session boundaries and by purge at rebirth. The renderer writes and restamps its own heartbeat, the launch gate and producer election trust fresh heartbeats while the session lives, and a birth that has proven the session absent purges heartbeat files before creating the replacement session.

## Realtime events

Wakeup datagrams carry `SidebarEventEnvelope` ([`sidebar/events.rs`](../../../crates/rimz/src/sidebar/events.rs)): a schema version, the workspace id, an optional session scope, a sender timestamp, and the typed event. `session_name` is the scope — `Some` targets the one mux session whose pane ids the event names, `None` is workspace-wide (store deltas, reloads, and pane-frame publications reach every renderer of the workspace).

The receive path drops an event for another workspace or session before it reaches the store. Only **overlay** events live in the store — `PaneClosed`, `CommandChanged`, `FocusChanged`, and a `PaneOpened` that carries a command; the rest are consumed as renderer actions or producer-verification nudges. The store keeps one slot per key (per pane per event kind, plus a single focus slot), latest stamp wins, capped at `MAX_EVENTS`. Each entry expires under a receiver-clock TTL and records both `sent_at_ms` (for supersession) and the receive time (for expiry), so a skewed sender clock can mis-order an overlay briefly but never pin it.

### Event taxonomy

| Event | Payload | Fusion action | Emitter |
| --- | --- | --- | --- |
| `PaneClosed` | `pane_id` | Delete every row bound to the pane (highest precedence) | Zellij plugin, tmux control-mode watch |
| `CommandChanged` | `pane_id`, `command` | Overlay the command until a pull verifies the pane's row shape | Zellij plugin, tmux watch |
| `FocusChanged` | focused / unfocused pane ids | Mirror focus bits onto every row; a single focused pane updates `SidebarSnapshot::focused_pane` and appends it to `viewed_panes`; an unfocus naming the register clears it | Zellij plugin pane-focus and tab-switch-to-work events, tmux watch (pane-active and non-stranded window switch), renderer jumps |
| `FocusStranded` | sidebar `pane_id` | Renderer action only, dropped past the short `FOCUS_STRANDED_EVENT_TTL` so late delivery cannot yank focus: the matching sidebar pane refocuses its held or own-view working sibling; when attached clients are viewing distinct panes, the renderer leaves focus alone because `focus-pane-id` is session-global | Zellij plugin, tmux watch on a window switch onto a stranded sidebar |
| `PaneOpened` | `pane_id`, optional `command` | Nudge a producer verification pull; never admits a card on its own | Zellij plugin, tmux watch |
| `PanesChanged` | none | Nudge a producer pull — topology moved, identity unknown | tmux watch fallback, the Zellij manifest fold |
| `StoreDelta` | optional event method and lifecycle signal | Refetch the rollup; a session start/end also requests fresh panes | store and context-sidecar writers |
| `PaneFramePublished` | none | Fold the just-published producer pane frame from cache | producer |
| `Notify` | `title`, `body`, target panes, `recheck_unread`, kind | Renderer action only: raise the configured desktop/bell/command notification, gated on row-unread when `recheck_unread`; never fused into rows ([notifications.md](./notifications.md)) | the notification path |
| `Reload` | none | Re-exec or hard-refresh the renderer | `rimz reload` |

### Push channels

Each push channel exists so a change a writer already knows about reaches every node within one wakeup instead of a poll window; the producer's pull stays the structural backstop behind all of them.

- **Store and sidecar writers** post a `StoreDelta` after every durable write or context-sidecar merge, so status, tokens, and cost repaint within one wakeup.
- **The Zellij presence plugin** pushes exact pane events, stamps `presence.stamp`, and publishes `pane-topology.json` through the host CLI; it includes attached-client count and viewed terminal panes when Zellij has returned a client sample, skips title-only manifest churn before projection, floors repeated same-pane command shortcuts to one immediate patch plus the settled producer pull, and emits `FocusChanged` when a tab switch lands on a working pane. **The tmux control-mode watcher** pushes typed pane overlays from its `refresh-client -B` subscription, emits `FocusChanged` on window switches except the stranded-sidebar case that emits `FocusStranded`, stamps `presence.stamp`, and falls back to `PanesChanged` for identity-free topology notices ([multiplexers.md](../multiplexers.md)).
- **The elder's transcript watcher** ([`transcript_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/transcript_watch.rs)) watches each live Codex session's rollout JSONL and runs the stat-gated context refresh on write, covering the mid-turn gap where Codex hooks fire only at progress events. Only the elder watches; demotion drops the watch, and a watcher that never starts costs nothing because the producer-tick refresh stays unconditional.
- **The elder's cache refresher** ([`cache_refresh.rs`](../../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs)) ticks on the data cadence, re-checks the heartbeat election each pass, refreshes heavy caches from the last published pane frame, fires due loop tasks for this room, and wakes due scheduled messages. Demotion turns it into a sleeper; a panic resets its rollup cursor and spending walker, and the next tick retries from cache truth.

Focus drives a dynamic fast tick for the work the user is viewing. The producer folds `PaneFrame.viewed_panes` into `SidebarSnapshot::viewed_panes`; git edit-sensitive facts for the viewed worktree and process metrics for the viewed pane run on the focused tier, while commit-shaped git facts and every background worktree/pane stay on their cheaper cadences.

Client presence is classified by the reader's `now_ms`. Zellij pushes attached human clients plus the terminal panes they view in `pane-topology.json`; tmux samples the same shape through `client_view` and also returns the freshest `client_activity` epoch, so `SidebarPresence::classify` marks `Idle` once input is quiet for the configured `[sidebar] afk_after_secs` window (15 minutes by default) and `Detached` when no human client remains. Zellij exposes attach state but no per-client input-idle timestamp, so an attached Zellij room stays `Active` until every terminal client detaches.

Remote tmux honors `afk_after_secs`: host `client_activity` advances only on input crossing SSH, which makes it a faithful idle proxy. The link-stats sidecar drives only the remote-link badge. A Zellij topology-cache hit carries viewed panes without a `list-clients` fork; legacy topology and tmux fall back to the backend `client_view` sample. Idle-capable tmux presence re-samples on the fast presence cadence while an attached client remains. If a fallback producer focus probe fails, the producer carries the prior presence sample from `snapshot.json` with the prior viewed panes.

## Fusion rules

Fusion is pure over pulled truth, the event store, and `now_ms`: no IO, no subprocess, no clock read past `now`.

`SidebarSnapshot::panes_observed_at_ms.or(panes_produced_at_ms)` is the supersession baseline: an event no newer than the pane observation is skipped, because the pull already saw later pane truth. A `PaneClosed` naming a *carried* pane applies at any age — the frame held that pane on process evidence rather than seeing it, so the frame proves nothing the close could be superseded by, and the close also retires the pane's carried-truth notice.

The overlays apply in precedence order. `PaneClosed` runs first and deletes rows; if it names `focused_pane`, the register clears and the renderer holds its last highlight. `PaneOpened` creates nothing; it asks the producer for a verified frame. `CommandChanged` overlays the command for non-deleted, already-admitted panes. The newest `FocusChanged` lands last: row bits mirror every listed focus mark, a single focused pane sets the session register and marks the pane viewed, and a multi-pane level dump mirrors row bits only.

Expired events disappear by receiver-clock TTL, and any wrong verdict from a missed event or clock skew is bounded by the next producer pull.

## Pull-tick table

The table names staleness-budget semantics; exact values and rationale live as named constants in [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs).

| Lane | Cadence | Where felt |
| --- | --- | --- |
| Pane frame | `SNAPSHOT_CACHE_TTL` by default; `EVENT_PANE_TTL` while the presence stamp is fresh | Pane open/close and cwd/command regrouping with no exact event |
| Unwatched consumer fold | ≤ `UNWATCHED_FOLD_CLAMP` for identity-free nudges; watched renderers and the producer are immediate | Off-screen `StoreDelta` and topology nudges in active rooms |
| Zellij topology cache | `PRESENCE_STAMP_FRESH`; explicit topology floors only for structural repair | Zellij pre-producer pane listing and pushed client view |
| Presence stamp | `PRESENCE_STAMP_FRESH` | Switches the producer between default and event-mode pane TTLs |
| Presence sample | Zellij on client-list events, active-tab switches, and keepalive self-heal; `PRESENCE_SAMPLE_TTL` while tmux reports attached clients and input-idle timestamps | Zellij attach/detach and viewed-pane gating; tmux AFK badge clear after fresh input |
| Git diff stats | focused: `DIFF_STATS_FOCUSED_LOCAL_TTL` local/edit facts and `DIFF_STATS_FOCUSED_COMMIT_TTL` commit/PR facts; background: `DIFF_STATS_TTL` hot and `DIFF_STATS_IDLE_TTL` idle | Worktree header churn, ahead/behind counts, landed markers, trunk-sync classification |
| PR state | `PR_STATE_HOT_TTL` for hot/focused repos, `PR_STATE_TTL` for idle repos; escalating failure backoff starts at `PR_STATE_RETRY_TTL` and caps at the repo tier; HEAD changes bypass the TTL | Worktree header PR glyphs after diverged stats; each due repo enumerates open PRs once, and failed repos keep last-known-good state |
| Worktree root enumeration | `WORKTREE_ROOTS_TTL` | Grouping for checkouts added without a session boundary |
| process metrics | `METRICS_FOCUSED_SAMPLE_TTL` viewed; `METRICS_BACKGROUND_SAMPLE_TTL` background | Child pids plus per-row CPU, memory, IO, and process-state figures |
| Spending walk | `SPENDING_TTL` | Provider dashboard, fleet store, and the floor under the live cockpit spend |
| Accounts | `ACCOUNTS_TTL` success; `ACCOUNTS_RETRY_TTL` failure | Provider dashboard login, plan, and account state |
| Live-session context | `SESSION_REFRESH_INTERVAL` | Provider dashboard budget windows and session sidecars |
| Account credits | `OAUTH_USAGE_TTL` for provider reads; `CREDITS_DISPLAY_MAX_AGE` for display | Provider dashboard paid/extra usage row and Codex reset marker |
| Remote link stats | `LINK_STATS_STALE`, expiring at `LINK_STATS_EXPIRE` | Footer link badge for `rimz remote connect` rooms |

## Render cadences

`[theme.display] refresh_ms` is the base render grid, defaulting to `DEFAULT_REFRESH_MS`. It rides `snapshot.theme.display`, so the renderer uses the default until the first fold and picks up config changes on later folds without reading config itself.

Money rolls sample on `refresh_ms * CLICK_PHASES`, matching the odometer phase counter; row animations sample on `BREATH_ANIMATION_FRAME`, clamped to at least the base. Input paints synchronously off-grid, an overlay event fuses on arrival and paints on the spot, and a burst of events still coalesces to one paint per base frame. The data backstop remains `rimz sidebar serve --tick-seconds`: changing `refresh_ms` changes paint cadence, not pull cadence.

The serve loop also wakes at the renderer-local order-hold expiry to fire the releasing fold that lets rows and groups settle back to live rank after the user goes idle.

The jump scroll anchor is a display-only runtime file, TTL-gated by `FOCUS_ANCHOR_FRESH`, carried renderer-to-renderer on the existing `FocusChanged` wakeup.

An attached sidebar in an unviewed tab keeps animation suspended and repaints only when its glanceable roster/status/unread projection changes, throttled by `BACKGROUND_PAINT_MIN_INTERVAL`; turn phase, gauges, process metrics, spend, git facts, and animation phase stay off the hidden paint trigger.

## Failure modes

A **missed event** is acceptable because events are latency hints; the producer's next pull is the structural backstop.

A **dead producer** is handled by heartbeat election. Once the stale heartbeat ages out, the next eldest renderer becomes producer; status keeps flowing through consumer rollup folds while pane presence waits for the handoff.

**Clock skew** cannot make events immortal because TTL uses receiver time. A skewed sender timestamp can briefly mis-order an overlay, and the verifying pull corrects it.

Every accepted anomaly path writes a typed diagnostic before it falls back, holds, suppresses, or exits. A recurrence of flicker, duplicate rows, or a phantom external group maps to a record in `diag.log.jsonl`; `rimz doctor` shows the recent tail and [diagnostics.md](../diagnostics.md) names the taxonomy.

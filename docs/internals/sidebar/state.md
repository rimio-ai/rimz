# The sidebar data plane

> This page owns how the sidebar gets its data: which renderer reads what, what it publishes where, how realtime events overlay it, and how stale any value can be. [sidebar.md](./sidebar.md) owns what the sidebar does with the data (binding, grouping, ranking, the serve loop, recovery). [store.md](../store.md) owns the durable truth underneath and the runtime directory the caches share. [multiplexers.md](../multiplexers.md) owns the backend push channels, [providers.md](../agents/providers.md) the account caches and window fusion, [performance.md](../performance.md) what the plane costs, and [diagnostics.md](../diagnostics.md#inspecting-live-card-state) how to inspect a live card.

## The shape of the problem

A room holds one store and one sidebar pane per tab. One room host per mux session paints every one of those panes, several times a second, so each session's data plane has exactly one owner ([room admission](../multiplexers.md#room-admission)).

Half the inputs are cheap. The store rollup is `cache/snapshots/latest.json` when its recorded log offset equals the live log's length and its `SNAPSHOT_VERSION` matches (`crates/rimz/src/store/snapshot/assemble.rs::read_fresh_latest`, [store.md → truth and cache](../store.md#truth-and-cache)), and otherwise an incremental fold of the log through the host's warm rollup cursor (`crates/rimz/src/sidebar/consumer.rs::rollup_snapshot`). The runtime sidecars and lane caches are small JSON files behind stat gates.

Every producer and consumer enrichment fold also probes the wrapper-held hook-drainer outage leases through `crates/rimz/src/harness/drainer_outage.rs::read`. The snapshot's optional `hook_drainer_outage` carries the earliest start as a `jiff::Timestamp` in `since`, converted once from the runtime record's `since_ms`, and the agents in start order; free, unreadable, malformed, and unknown-version leases contribute nothing. Changes to that set are content, never a clock-only update. Diagnostic logs are not an input.

The other half are expensive. The pane roster costs a multiplexer IPC round trip or a plugin hop, git costs forks per worktree, and provider accounts, usage, and pricing cost subprocesses and network. Paying those once per tab per tick would saturate the mux server and the machine, and a room sits idle most of the time anyway.

Three rules resolve that, and the rest of this page follows from them.

1. The store is truth, and the sidebar's lanes are cache. Each lane rebuilds from the store plus a fresh read, is written temp-file-plus-rename without fsync, and can be deleted at any moment ([store.md → write classes](../store.md#write-classes)). The one exception is `records/live-roster.json`, which shares the cache write class but rebuilds from nothing: rebirth recovery reads it ([coordination and receipts](#coordination-and-receipts)). The `cargo xtask invariants` check `ensure_sidebar_library_boundaries` keeps store writers, the run-wake sender, and the broker out of the import graph of the data plane and the shared wakeup wire, so the sidebar is read-only on the store by construction.
2. One host per session pays the shared reads. Its data plane folds once for all subscribed panes, does the external work once, and publishes runtime caches for CLI readers. New attachments seed from its last delivered fold in memory.
3. Realtime events carry latency, never truth. A wakeup datagram lets a change paint now instead of at the next poll. A dropped datagram costs staleness bounded by the next producer pull, never a wrong verdict.

```text
     store rollup                         expensive external reads
     latest.json + log                    mux panes · git · providers
           │                                     │ host production
           │                          retained panes + agent projection
           │                          heavy-lane caches (also CLI refresh)
           └─────────────────┬───────────────────┘
                             ▼
                    host fetch worker
                    ordered workspace fold (enrich)
                             │
                    per-attachment projection
                             │
    realtime events ────────▶│◀────────────── focus intent
    room-shared overlay     ▼
                fuse_owned(pulled, events, intent, now)
                   pure: no IO, subprocess, or clock read past `now`
                             │
                       SidebarSnapshot ──▶ paint
```

## Where the code lives

`crates/rimz/src/sidebar/` is the data plane: it reads, folds, and publishes, and never writes store truth. `crates/rimz/src/sidebar_pane/` holds the pane supervisors and the painting host. `crates/rimz/src/wakeup/` holds the wire the data plane shares with every other sender, and `crates/rimz/src/mux/` owns the runtime files the multiplexer writes or dispatches on.

The data plane, in `crates/rimz/src/sidebar/`:

| Module | What it owns |
| --- | --- |
| [`mod.rs`](../../../crates/rimz/src/sidebar/mod.rs) | Heartbeat-based launch gating and pane liveness. |
| [`consumer.rs`](../../../crates/rimz/src/sidebar/consumer.rs) | Passive publication reads and the enrichment core shared with the host's retained-input fold. |
| [`enrich.rs`](../../../crates/rimz/src/sidebar/enrich.rs) | The ordered fold spine shared by production and published-cache reads. |
| [`frame.rs`](../../../crates/rimz/src/sidebar/frame.rs), [`cache.rs`](../../../crates/rimz/src/sidebar/cache.rs) | `PaneFrame`, the published pane topology, with its cache read, freshness verdict, presence stamp, and the authoritative pane probe. |
| [`produce/`](../../../crates/rimz/src/sidebar/produce/mod.rs) | Retained host production and private CLI reads: `panes.rs` shares repair, confirmation, carry, and acceptance, `metrics.rs` samples supplied history and backfills Zellij pids, `git.rs` enumerates worktree roots, `tab_status.rs` projects tab names. |
| [`refresh/`](../../../crates/rimz/src/sidebar/refresh/mod.rs) | The heavy lanes, each gated on its own TTL except the every-pass pipeline and keep-warm lanes: `git_stats.rs`, `trunk.rs`, `pr.rs`, `accounts.rs`, `usage.rs`, `credits.rs`, `rate_limits.rs`, `sessions.rs`, `live_spend.rs`, `cohort_spend.rs`, `pipeline.rs`, `keep_warm.rs`, `daemon_reap.rs`. `git_refs.rs` reads ref files to skip a `git` fork, and `trace.rs` is the opt-in account-refresh timing trace. Bounded fan-out comes from `crates/rimz/src/utils/parallel.rs`. |
| [`agent_projection.rs`](../../../crates/rimz/src/sidebar/agent_projection.rs) | Published adapter wiring and provider-local session discovery. |
| [`event_store.rs`](../../../crates/rimz/src/sidebar/event_store.rs) | The in-memory overlay store: which events overlay, how they supersede, when they expire. |
| [`presence.rs`](../../../crates/rimz/src/sidebar/presence.rs) | Zellij presence-wake ingestion and the topology writer gate. `presence/projector.rs` is the host policy both backends feed, `presence/tmux.rs` the control-mode normalizer. |
| [`fuse.rs`](../../../crates/rimz/src/sidebar/fuse.rs) | Pure fusion of pulled truth, overlay events, and focus intent. |
| [`body_filter.rs`](../../../crates/rimz/src/sidebar/body_filter.rs) | The room-runtime cockpit lens every renderer adopts. |
| [`unread.rs`](../../../crates/rimz/src/sidebar/unread.rs), [`read_marks.rs`](../../../crates/rimz/src/sidebar/read_marks.rs), [`notify.rs`](../../../crates/rimz/src/sidebar/notify.rs) | Unread episodes, the read receipts that clear them, and notification policy over new episodes ([notifications.md](./notifications.md)). |
| [`observe.rs`](../../../crates/rimz/src/sidebar/observe.rs), [`meter.rs`](../../../crates/rimz/src/sidebar/meter.rs) | The frame-stream anomaly observer and the producer tick-budget meter ([diagnostics.md](../diagnostics.md)). |
| [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs) | Every sidebar cadence and TTL as a named constant with its reasoning. A bound that defines another module's behaviour lives with that module. |

The shared wire and mux files:

| Module | What it owns |
| --- | --- |
| [`wakeup/mod.rs`](../../../crates/rimz/src/wakeup/mod.rs) | Sender-side heartbeat freshness, envelope encoding, and nonblocking datagram fanout. |
| [`wakeup/events.rs`](../../../crates/rimz/src/wakeup/events.rs) | The versioned wakeup envelope and the event taxonomy. |
| [`wakeup/heartbeat.rs`](../../../crates/rimz/src/wakeup/heartbeat.rs) | The per-renderer liveness file, its TTL, and the freshness scans launch gates and wakeup fanout read. |
| [`mux/focus_anchor.rs`](../../../crates/rimz/src/mux/focus_anchor.rs) | The two-phase intent behind every RimZ-initiated focus action. |
| [`mux/width_target.rs`](../../../crates/rimz/src/mux/width_target.rs) | The room-runtime sidebar share and whether a user action pinned it. |
| [`mux/zellij/pane_topology.rs`](../../../crates/rimz/src/mux/zellij/pane_topology.rs) | The Zellij topology cache, its freshness window, and the desired-presence record. |
| [`mux/recovery.rs`](../../../crates/rimz/src/mux/recovery.rs) | The scoped orphan-process sweep used by reload and teardown. |

The renderer threads, in `crates/rimz/src/sidebar_pane/`:

| Module | What it owns |
| --- | --- |
| [`host.rs`](../../../crates/rimz/src/sidebar_pane/host.rs), [`attach.rs`](../../../crates/rimz/src/sidebar_pane/attach.rs) | The room host (the attach listener, one attachment thread per pane, lifetime and reload) and the attach wire it shares with the supervisor. |
| [`supervise.rs`](../../../crates/rimz/src/sidebar_pane/supervise.rs) | The pane's process shell: its `SupervisorConfig` from argv and the pane environment (no machine config), terminal modes, the event reader, notice and retry, self-close confirmation, watchdog, and build promotion. |
| [`app.rs`](../../../crates/rimz/src/sidebar_pane/app.rs) | The host-built `RoomConfig` and `ServeConfig`, `SidebarAppErr`, `install_panic_diagnostic_hook`, and runtime-file cleanup. |
| [`app/attachment.rs`](../../../crates/rimz/src/sidebar_pane/app/attachment.rs), [`app/backend.rs`](../../../crates/rimz/src/sidebar_pane/app/backend.rs) | One pane: runtime-file guards, the fixed-timestep loop, and the terminal backend bound to the pane's output fd. |
| [`app/plane.rs`](../../../crates/rimz/src/sidebar_pane/app/plane.rs) | The data plane one process runs for all its panes, and each pane's subscription to it. |
| [`app/fetch.rs`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs) | Request coalescing, and the fetch worker's produce cadence and result publication. |
| [`app/views.rs`](../../../crates/rimz/src/sidebar_pane/app/views.rs), [`app/view_state.rs`](../../../crates/rimz/src/sidebar_pane/app/view_state.rs) | The host's client views: publication slots, replay cursors, membership and retention, and each view's reconciliation state. |
| [`app/loop_state.rs`](../../../crates/rimz/src/sidebar_pane/app/loop_state.rs) | Renderer state transitions, focus repair, maintenance deadlines, and paint eligibility. |
| [`app/inbox.rs`](../../../crates/rimz/src/sidebar_pane/app/inbox.rs) | The bounded input and room lanes of each attachment. |
| [`app/focus_push.rs`](../../../crates/rimz/src/sidebar_pane/app/focus_push.rs) | Click-ordered jump leases, the priority focus push, and its verdict trace. |
| [`app/cache_refresh.rs`](../../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs) | The heavy-lane refresher. |
| [`app/hidden_views.rs`](../../../crates/rimz/src/sidebar_pane/app/hidden_views.rs) | Debounced tmux hidden-view geometry repair. |
| [`app/tmux_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/tmux_watch.rs), [`app/transcript_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/transcript_watch.rs) | The two host-run push channels. |
| [`app/gate.rs`](../../../crates/rimz/src/sidebar_pane/app/gate.rs), [`app/health.rs`](../../../crates/rimz/src/sidebar_pane/app/health.rs) | The last-known-good commit gate and the debounced degraded-health verdict. |

Start at `enrich.rs` to learn what a snapshot is made of, at `app/fetch.rs` to learn when it is built, and at `timing.rs` to learn how often.

## The room data plane

Every tab's sidebar pane runs one `rimz sidebar serve` supervisor, and one `rimz sidebar host` per mux session paints all of them. The panes of a session share one process, one data plane, and the host's room socket for external events. Each pane still has its own heartbeat, bounded wake channel, and frame loop, and none waits on another to paint.

### The room host and its attachments

The supervisor keeps the pane's tty and hands the host only the pane's output. It holds raw mode and mouse capture for its whole lifetime, including first-start failures and the gaps between attach rounds. While attached it reads keys, clicks, and resizes and forwards them as raw input words. The host ([`host.rs`](../../../crates/rimz/src/sidebar_pane/host.rs)) is detached from every terminal and runs one attachment per pane ([`app/attachment.rs`](../../../crates/rimz/src/sidebar_pane/app/attachment.rs)), each on its own thread with its own `LoopState`, geometry, width control, and pixel lease. The host resolves each key against the routed [client view](#client-views)'s UI mode and the current snapshot's `sidebar.keys` navigation map ([motion keys](./sidebar.md#selection-and-jump)).

| Owned by | What |
| --- | --- |
| Supervisor, one per pane | Terminal modes, the input and resize reader, the attach connection, self-close confirmation against the mux, the pane-liveness probe, its own re-exec onto a new build. |
| Attachment, one per pane, in the host | The heartbeat (`instance_id` the supervisor's, `pane_id` the pane's, `build` the host's, `size` read from the pane fd), bounded wake channel, frame loop, geometry, own-tab marking, width control, self-close, render caches and last-painted hit maps. |
| Host, one per session | The room event socket and the data plane ([`app/plane.rs`](../../../crates/rimz/src/sidebar_pane/app/plane.rs)): one fetch worker, one cache refresher, one tmux watch and hidden-view sweep, one transcript watch, one observer writer, and the client-view registry under one mutex. |

An attachment has no terminal state of its own to touch. Geometry comes from `tcgetwinsize` on the pane fd and is read again on every `resize` word. A pane the mux has not laid out yet reports `0x0`: its heartbeat carries no `size`, and nothing is drawn until the first resize sizes it. A write error on the fd, which is how a closed pane first shows, closes the attachment exactly as the supervisor's end of stream does. A panic in one attachment closes that attachment alone.

The host's environment is the session's minus pane-scoped attribution, lane, and supervisor instance identity ([`supervise/host_link.rs`](../../../crates/rimz/src/sidebar_pane/supervise/host_link.rs)). Its spawner sets the room pin from the argv workspace's record, using the canonical root that `workspace::verify_pin` returns, never the supervisor's inherited pin. Two failures stop the spawn:

- A supervisor with no readable workspace record stops before spawning or writing diagnostics, with one stderr line naming the workspace and `rimz start`.
- A readable record that cannot verify the room pin starts no host. The supervisor appends a timestamped line with the workspace and reason to the host log (its own log sink is off while it renders), then shows a notice naming that log and retries.

One fold serves every pane. The fetch worker publishes the renderer-independent workspace into the host's views, and each attachment projects it for its own pane when it reduces. Tab names, the live roster, unread reconciliation, and notifications are decided once on the room fold. Each attachment still decides whether a notification rings in its own tab.

Projections share the fold's agent-scaled data through `Arc`: the room rosters (`agents`, `fenced_sessions`, `resume_outcomes`), pane bindings, the group vector, group rows, and each agent card. Pane-local fields and worktree roots stay owned. Pane-side code calls `Arc::make_mut` only after a read shows that the write changes that collection or card, so unchanged data stays shared.

A new attachment paints room content before its first fetch completes. Under one lock it subscribes and captures `Views.fold` with the current publication revision, and it projects that seed for its own pane and refresh override. A scratch borrow (a pane neither resolved to a client nor visible) applies the seed through the normal fusion and gate path as an interim cached snapshot, before draining wakeups. A shared client view drops it once the host has published, which a captured fold implies (`crates/rimz/src/sidebar_pane/app/loop_state.rs::LoopState::apply_latest_snapshot_with_seed`). That borrow's catch-up in `crates/rimz/src/sidebar_pane/app/loop_state/shared.rs::ViewAccess::with` instead replays the publications the view has not reduced, traced `seed: false`, and a view already current paints the content it holds. A visible new sidebar on a warm host takes this path. A seed completes no fetch. The attachment's first request still corrects it, since every request runs a worker cycle even when the shared worker has nothing new to fold. A warm host seeds with no age bound, stamp pairing, or incarnation guard. A cold host paints the placeholder until its first publication. Both backends share this rule.

The own view gates selection. A frame that predates this attachment's pane yields `own_view: None`, and `crates/rimz/src/sidebar_pane/app/state.rs::session_focus_baseline` then withholds focus. The snapshot's `focused_pane` stays as folded for fusion and watched-state. Neither the seed nor a later cached fold of that frame derives selection or clears unread from the withheld focus, so a selection established on this pane holds its last baseline; an arriving view follows the [client-view rule](#client-views). Even with its own view present, `session_focus_baseline` requires the focused pane to be a row before it can seat a new baseline. A missing own view is expected briefly at room or tab birth. `fold_decided.own_view` in the [focus trace](../diagnostics.md#the-focus-trace) shows the transition. An attachment that never sees its own pane cannot follow focus.

The supervisor carries a selection baseline across host replacement. At its first sized composition an attachment can seed the baseline its supervisor carried, but only into a view that has none, and a hidden pane cannot seed a different pane's view. Reattachment to a live host keeps the full view. Host replacement loses full views and keeps only these pane-keyed baseline hints. The [first-frame live check](../../contributing/sidebar-live-check.md#first-frame-of-a-new-tab) records the remaining seat lag and the attribution recipe. Selecting the previous tab's pane is a regression, not an accepted first frame.

### The attach wire

The host listens on a stream socket in the runtime `sock/` directory, named by a digest of the mux and session name. One connection is one pane, and end of stream from either side detaches it.

1. Hello. The supervisor sends one JSON line and, riding the same message as `SCM_RIGHTS`, its stdout. The fields are `protocol` (`rimz.sidebar-attach.v1`), `instance_id`, the required `pane_id` (the supervisor's own pane; a hello without it is malformed), `supervisor_build`, the optional `baseline_pane` (a pane id such as `"tmux:%6"`), the pane's `tick_seconds` cadence (also required), and its `refresh_ms` launch override when set.
2. Reply. The host answers `{"accept":{"build":"…"}}` or `{"reject":{"reason":"…"}}`. The named reasons are `protocol` (another wire version), `build` (the host is leaving for a newer build), and `capacity` (the host already holds `MAX_ATTACHMENTS`, 512). Any other text is a host-side failure.
3. Stream. After an accept, the host writes JSON control lines and the supervisor writes bare newline-terminated input words, starting with `resize`.

| Host control | Sent when | Supervisor effect |
| --- | --- | --- |
| `{"control":"self-close"}` | The pane's tab looks empty. | Confirms against the mux before closing the pane. |
| `{"control":"reload"}` | The host is leaving for another build. | Detaches and reattaches to the next host. |
| `{"control":{"baseline":"tmux:%6"}}` | The selection baseline changes (`null` forgets it). | Remembers it for the next hello and carries it through re-exec. The attachment continues. |

The host seeds the baseline before the first fold and compares it once per loop turn, counting the hello's value as already reported. Old hosts ignore the optional hello field, and old supervisors ignore an unknown control.

The input words are `resize`, `press:<mods>:<code>`, `mouse:left:<col>:<row>`, `scroll:up`, and `scroll:down`. The host ignores unknown lines and sends decoded input to the attachment's channel. EOF or a read error detaches. A supervisor write times out after the forwarder's 250 ms stop poll and drops that word.

A connection that closes without a hello is dropped silently, which is what the socket GC's liveness probe looks like.

### Host unavailable

The host is the only painter, so the supervisor refuses early when it could never reach one. First, `crates/rimz/src/cli/sidebar.rs::serve` refuses outside a mux pane: `rimz sidebar serve` runs only in the sidebar pane the room spawns. Inside a pane, after the no-room check, `crates/rimz/src/sidebar_pane/supervise.rs::run` refuses non-terminal stdout with `error: rimz sidebar serve: stdout is not a terminal; the sidebar pane command must run in a pane` on stderr and a non-zero exit. An unavailable `RuntimePaths` likewise exits non-zero with its `PathErr` message. These checks precede socket or terminal-mode work. The supervisor then initializes the runtime directories before starting a host, and the detached command uses the shared runtime root as its working directory.

Every other failure to reach the host follows one notice-and-retry path: a failed start, an unanswered socket (`HOST_WAIT`, 2 seconds), a rejected hello, an unreadable or late reply (`REPLY_WAIT`, 5 seconds), and a lost attachment. The supervisor clears the screen and homes the cursor with plain stdio control bytes, then writes two uncolored lines in raw mode with `\r\n`: `sidebar: <cause>` and `retrying in <N>s`. It writes only after a failed round, never before the first attempt or while attached, and the next attachment paints over the notice. N is the upcoming backoff in whole seconds, at least 1. Delays double from 1 second to 60 seconds and reset after an attachment has run for a stable minute. A workspace-record change interrupts the wait.

| Cause shown | Diagnostic `cause` |
| --- | --- |
| `host did not start (<error>); see <host log path>` | `start_failed` |
| `no host answered` | `no_answer` |
| `host rejected this pane: <reason verbatim from Reply::Reject>` | `rejected` |
| `host reply unreadable` | `reply_unreadable` |
| `host went away` | `lost` |

Each failed round emits the identity-rate-limited `sidebar_host_unavailable` record with the cause, reason, attached duration when lost, and upcoming retry delay ([diagnostic fields](../diagnostics.md#supervisor-kinds)). A lost host costs even a mature attachment a one-second notice before retrying.

Terminal modes survive every exit except a confirmed self-close. Self-close restores them. SIGTERM, pane-gone cleanup, and build exec preserve them for the pane handoff. An attachment's close removes its runtime files, and a stopping supervisor also removes its per-instance files.

### Host lifetime and reload

The supervisor that finds no host takes the session's spawn lock and starts one. It runs the room's verified recorded build, or its own executable when no verified target is available. The host's stderr is appended to `log/sidebar-host.<session key>.log` under the room's state directory, so a host that fails at startup leaves its error there.

At startup the host calls `setsid` and raises its open-file soft limit toward the hard limit (`crates/rimz/src/sidebar_pane/host.rs::raise_open_file_limit`). The target is 65,536, retried at macOS's 10,240, and a refusal warns in the host log. The sizing reason is on `OPEN_FILE_TARGET`. The host then holds the session's host lock for life. A second host for the same session waits out its predecessor (`SUCCESSION_WAIT`, 12 seconds) and exits if that one stays.

The host leaves in three ways:

- Idle. With no connection for ten seconds it exits.
- Reload. When the workspace record names a verified build other than its own, or a batch of reload requests finds a byte-different target, it removes its sockets, sends `reload` on every stream, and waits until its attachments have let go or `DRAIN_GRACE` (10 seconds) passes, whichever comes first. Supervisors then find no host and start the recorded build's.
- Teardown. The room teardown sweep kills it like any other process carrying the session's tokens.

A supervisor re-execs itself onto the recorded build once its attachment has run the stable window under a host of that build.

Reload requests reach the host inbox directly: room reload words and envelopes, and a pane's `r` action after UI-mode resolution. The host drains all queued requests into one comparison per turn. An identical or missing target forces one hard refresh without blanking panes.

### Shared lanes

The host starts its lanes when its first pane subscribes. The fetch worker skips an empty subscriber set, and the cache refresher skips a pass with no pane. Heartbeats remain per-pane liveness and wakeup hints, never ownership votes.

| Thread | Owns |
| --- | --- |
| Fetch worker | Pane and workspace publication, group roots, notifications, unread reconciliation, and tab-status names. |
| Cache refresher | TTL-gated git, PR, accounts, usage, credits, cohort effort, unattended recovery, due tasks and messages, then daemon-view repair ([rimzd.md](../rimzd.md#who-repairs-and-when)). |
| tmux control-mode watch | One tmux presence stream for the session. |
| Transcript watch | Filesystem watches on live sessions whose adapters declare transcript-tail context. |
| Observer writer | Frame-anomaly emission and real-world cross-checks when diagnostics are enabled. |

Host replacement is supervised, not elected. Account-global spending keeps its separate lifetime lock and socket ([spending.md](../agents/spending.md#one-walk-per-namespace)).

Three host-wide samples serve every attachment:

- Hidden-view fitting (tmux only). The tmux watch kicks it when its own watch attaches, a terminal client attaches to or switches into the watched session, a window is born, or the current window's layout changes. Client changes into other sessions on the server and hidden layout changes do not kick it. After 500 ms without a kick, one fresh geometry read lists stale hidden windows in MRU order, and one `MuxBackend::fit_views` call fits them all through stdin scripts. A kick received during a fit schedules another sweep 500 ms after that kick, or at once when the fit outlasted the settle. A user jump freshly checks and fits its one still-stale destination before the paint handshake; the in-tmux size guard skips an already fitted window without unzooming it. Manual windows and sessions without a sizing client are excluded, and each fit preserves the window-local sizing policy ([mux geometry](../multiplexers.md#attach-and-the-terminal)). Zellij runs neither this worker nor its geometry subprocesses.
- Graphics capability (tmux only). Probes share a plane-owned ten-second sample of the rendering clients, process ancestry, version, and per-pane passthrough options. A session-wide pane listing keeps each pane's permission. A pane born after the sample, or an unavailable listing, uses a targeted query. Passthrough escalation and fd-derived cell aspect stay pane-local.
- Diagnostics observation. With diagnostics enabled, the fetch worker extracts one shared observation signature before local projection, and the writer detects common anomalies once. Attachments detect their own committed roster and clicked-card displacement after paints. With no writer, commit signatures are skipped. Attribution and evidence scope are in [diagnostics](../diagnostics.md#the-commit-point).

## One fetch cycle

An attachment's loop reads no data itself. It blocks in `recv_timeout` on its bounded wake channel, using the timeout as its frame clock, and requests folds for pane-local observations. Shared folding and side effects run on the worker. Local projection and the clock-only decision run when a view reduces.

### Requests

A `FetchRequest` carries a mode and an optional pane-cache floor:

| Field | Meaning |
| --- | --- |
| `Normal` mode | The ordinary wakeup. Production is attempted at most once per data tick. |
| `FreshPanes` mode | The producer produces regardless of frame age. Raised by agent birth and death. |
| `HardRefresh` mode | The host produces regardless of frame age. Raised by reload and manual recovery. |
| `min_pane_cache_ms` | A floor that rejects any pane cache older than the signal that asked for it, which is how birth and death beat the pane TTL. |

The plane requests at most one fold per room event as it records it (`RoomEvents::dispatch` in [`app/plane.rs`](../../../crates/rimz/src/sidebar_pane/app/plane.rs)): fresh panes for topology and agent birth or death, none for width, filter, notification, focus actions, or other overlays, and ordinary otherwise.

Panes send attach, own reveal or stranded focus, resize, and local read marks. Gate escape, tab-read dwell, freeze expiry, and self-close deadlines send only while no pane request awaits completion.

The worker merges waiting requests (strongest mode, latest pane-cache floor) in `drive` ([`app/fetch.rs`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs)), so a storm costs the running cycle plus one more. It rests for `FOLD_FLOOR` (250 ms) after each cycle returns, and requests that arrive while a cycle runs or rests merge into the next batch. No mode bypasses the rest, and a request after at least 250 ms idle starts at once. Disconnecting all senders still lets a batch in hand finish.

The worker owns the data backstop. When no request arrives within one data tick of the last cycle's return, it runs an ordinary cycle itself, using the shortest `rimz sidebar serve --tick-seconds` among subscribed panes (default 1, floored at 1 s by `tick_for`). The backstop starts after the first cycle, and with no subscribers the worker blocks for a request.

### The cycle

One cycle runs these steps on the worker ([`FetchWorker::run_cycle`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs)):

1. Fast fold. Fold the event-fresh rollup over the retained panes and agent projection, plus sidecars and heavy-lane files. The fast fold reads neither `snapshot.json` nor `agent-projection.json`.
2. Production decision. `ProducerCadence::start_attempt_if_due` starts ordinary production when no attempt started in the current data tick and the retained frame is at least one tick old or unavailable. `FreshPanes` and `HardRefresh` bypass that cadence.
3. Publish the fast fold. It is final when no production follows, and interim when one does.
4. Produce. When due, `PaneProducer` reconciles panes and publishes the produced fold as final. A failure publishes a failed outcome.
5. On every final fold, before publication: reconcile unread and evaluate notifications and reminders.
6. Publish, then wake subscribers. After that, a produced final writes `records/live-roster.json`, and the worker delivers the evaluated notifications: it runs any notification command handlers itself, then fans `Notify` out to the attachments.

`PaneProducer` owns its prior frame, metrics history, and presence-attempt clock, and seeds the frame and history once at host start. A fresh prior avoids mux listing, while a stale one runs candidate, confirmation and carry, and acceptance. Only fresh acceptance publishes topology, and a held prior stays unpublished. Metrics and presence keep their own cadences. Tab-status renames follow the ownership rules in [multiplexers.md](../multiplexers.md#tab-names), and a failed mux rename stays best-effort.

Every published fold becomes `Views.fold`, the seed for new attachments, and a failure keeps the previous one. Accepted producer inputs reach shared memory before publication, even when a later fold fails.

A produce runs behind a panic guard. An unwind costs one failed outcome, the loop holds its last good frame, and the next cycle refolds cold from a fresh rollup cursor instead of trusting a base the panic may have torn.

### Publication and replay

The host's views keep three publication slots for the room: the newest final success, the newest interim, and the newest failure ([`Views`](../../../crates/rimz/src/sidebar_pane/app/views.rs)). A successful final clears the interim and failure slots, and an interim never displaces an unread final. Under the views lock, publication advances the revision, and a final also sets completion latches for the subscribers present at cycle start. Wakes follow after unlocking. A subscriber that joined mid-cycle can reduce that publication but gets no completion or wake from it. Publication never waits for tty output.

A cursor replays what its consumer has not seen: the newest unseen final success, otherwise the newest unseen interim, then the newest unseen failure. Wakes are hints. Frame maintenance reads the views even when a full wake channel dropped one.

Replay compares before it applies. `Cursor::replay` projects the fold through the seed's routine for the reducing attachment's own pane and refresh override, then compares the result with its last successful projected baseline. Equality includes the shared focus anchor and body lens, and ignores only the snapshot clock, generation time, pane observation and production stamps, and the input sampling instant. Equal content with live permission applies clock-only: it advances clocks, stamps, the self-close watchdog, and paint scheduling, and skips full fusion, gate and health commits, receipts and dwell, selection, filter, order, and freeze reconciliation, spend rolls, self-close, anchor observations, and focus-repair retry. Idle presence ages with that clock. Every request still runs a worker cycle, and final-fold notification evaluation, unread persistence and pruning, and link recovery still run.

Clock-only permission is read live at replay and is blocked by any of: a failure, a gate episode, an active overlay, a freeze, a read dwell, an empty-tab confirmation, a pending focus repair, or an input obligation. Completion is independent of content reuse. A rejected final requests nothing, and the gate asks again once its deadline is due.

Each shared view has its own cursor and baseline. A pane that reduces with no routed view uses its own Scratch scope, with a separate cursor and baseline. Seeds do not prime the Scratch baseline, and a shared drain clears it, so the next Scratch reduction is full once. Local input invalidates the Scratch baseline, and identity-free nudges do not.

Before publication, the worker compares shared collections once against the preceding fold and reuses equal Arcs. Replay checks their identity before small metadata, and pane-local exclusions or older baselines fall back to structural equality. Shared views project and compare under the views lock, and the worker never compares a consumer's baseline. A co-viewer whose view is current does not reduce again.

### Hidden attachments and reveal

A settled hidden attachment leaves its cursor unread until reveal, with no time bound. A deferred drain builds no projection and advances neither cursor nor baseline. It consumes completion and refreshes the self-close watchdog only when completion is present. `FetchUpdate::preview_for` reads the selected unseen success, not the seed slot, which may hold an interim behind an unread final.

These conditions apply a hidden publication at once instead of deferring it:

- an unseen failure
- a changed own view
- a pending reveal
- optimistic watching
- a bounded obligation: exit, a gate episode, a health failure, a tab-read dwell, an empty-tab confirmation, focus repair, or pending input focus

Freeze and overlay baselines do not prevent deferral. Input flushes before acting, and hidden resize does not. Revealing focus, owned notification targets, own stranded focus, and owned focus intents also flush, and the plane records overlays before that decision. Width and body-filter events keep their local effects without draining. Unread reminder timing belongs to the worker.

A reveal catches up the routed client view before composing its first frame in the destination's geometry. An unpainted view, or a view changing panes, counts as an arrival when the view has painted before or the pane has already made an advancing projection into its current views registry. That includes a never-painted view's first reveal after a hidden fold, whether observed or evidenced by input. A never-painted view on a pane's first advancing projection (visible from birth) arms nothing, and non-advancing borrows do not count.

An arrival arms the tab-read dwell and holds the last painted order, not a hidden fold's order. Without a painted order it holds its first frame's order. Replaying an older hidden fold cannot cancel a newer arrival's dwell or viewing latch, and an existing hold keeps its deadline. Spend accrued while the view was unpainted settles at reveal, which keeps the spend ratchet. A continuously painted view moving tabs keeps its roll, and joining incumbent viewers neither restarts their roll nor re-arms their hold.

### Client views

A client view is one human client's presentation state, kept by the host so that it follows the client between tabs. `crates/rimz/src/sidebar_pane/app/views.rs::Views` owns fetch content, the seed fold, shared replay baselines, membership, and retention. `crates/rimz/src/sidebar_pane/render/ui_state.rs::ViewState` holds the portable presentation, and `crates/rimz/src/sidebar_pane/app/view_state.rs::ViewContext` its reconciliation state. Selection, scroll, relative row anchors, open cards and toggles, freeze, spend rolls, receipts, and tab-read dwell travel together. Full views live in memory only: the attach wire, snapshots, and the room filter format carry none of it.

On its first advancing borrow, a view arriving from another pane leaves its selection behind if the newest published fold's frame does not list the destination yet: the baseline, browse pick, selected pane and index, drawn selection, and cards held open by selection. Everything else still travels. The pane also takes the frame's missing own view at that arrival, before catch-up replay or event fusion. A frameless arrival cannot distinguish a new pane from a jump and keeps its selection. A listed jump destination keeps the selection it carries. Same-pane re-arrivals and non-arrival borrows preserve an established pane's hold across a transient missing own view.

The pane loop reaches a view only through `crates/rimz/src/sidebar_pane/app/loop_state/shared.rs::ViewAccess`, which lends it explicitly to `crates/rimz/src/sidebar_pane/app/loop_state.rs::PaneState`. Each borrow takes its overlay cut before locking the plane's one mutex. Only the painter consumes one-shots or writes back geometry-derived placement, and scratch renders cannot. Terminal writes, mux commands, read-mark I/O, asset loads, and other module locks stay outside this guard. After unlocked preparation (asset loads between a paint's two borrows, or the fold a stale paint loads), the painter re-reads routing and the latest fold before composing.

A resolved client sample changes membership by four rules:

| Sample shows | Result |
| --- | --- |
| A client still on the pane it viewed | It keeps its view, and clients arriving on that pane join it. |
| A client arriving on a pane no one views | Its view comes with it. |
| Co-viewers splitting to several panes | The view forks once per destination. A whole-group move keeps one view. |
| A same-sample swap | No client joins a view another client just vacated. |

tmux clients are always co-located, while Zellij clients may view different tabs. When co-viewers disagree on working-pane focus, selection holds its last baseline rather than following an unrelated client.

Between samples, an attributable `FocusChanged` advances selection and parked state in the same fold. Every tmux event is attributable, because focus is session-wide. On Zellij, the sampled clients on the event pane's tab must all belong to that view, otherwise sampled focus stands. Before the first resolved sample, native focus supplies the baseline.

A departed client's view is retained for `[sidebar] afk_after_secs` (15 minutes by default), timed on a monotonic clock from the fresh sample that dropped it. If co-viewers remain, the departed client keeps a frozen copy. A return under the same client id before expiry resumes it. A new client joins incumbent viewers, or takes the most recently active unexpired live, held, or anonymous view. With none, it starts at scroll zero with no selection and no carried hold, and native working-pane focus can then derive a new baseline. When its pane has folded before, as on a reattach after expiry, its first paint is an arrival. Activity advances only on routed input or pane arrival, never on folds, paints, or repeated samples. An unresolved sample (retained Zellij rows or an empty tmux map) changes no membership and starts no departure.

Input and observed visibility can establish a view before the client map catches up. A painting pane with no resolved viewer routes in this order ([`Views::route`](../../../crates/rimz/src/sidebar_pane/app/views.rs)):

1. The sole attached client's view, when that client has no pane assignment yet.
2. The most recently active view no other painting pane holds. A view a departed client still owns is forked rather than taken.
3. A copy of the most recent view.
4. A fresh default.

An anonymous view stays live while painted, and its retention window starts when its last painter and client leave. Resolved departure cancels the old pane's input and focus optimism, so a hidden paint cannot create a newer anonymous view that overrides the client's next arrival. New input remains viewing evidence, and a fold without a frame observes nothing. A view that evidence established after the last resolved sample, and that no sampled client held, survives the next sample: a client arriving on its pane joins it, even when the view the client left carries newer input from a co-viewer.

Hidden views do no work. Hidden resize alone changes no view and neither grants nor revokes watching optimism. A hidden, held, or anonymous unpainted view performs no reductions or receipts. Each painted view owns one tab-read dwell, cancelled on departure. No view mutation wakes a pane that does not paint it.

### The fold spine

Production and published-cache reads run one ordered spine in [`enrich.rs`](../../../crates/rimz/src/sidebar/enrich.rs), so the two paths cannot drift. It forks no subprocess and writes no cache file: it projects what is already on disk.

`enrich_workspace` is the renderer-independent half. Its order matters in three places:

- Adapter wiring is set before any pane-backed projection, because wiring gates idle-agent synthesis.
- Activity sidecars land before the pane overlay, so row age, ranking, waiting guards, and the stall window see the per-tool timestamp instead of the coarser turn-grained event time. The same record carries the open run of identical tool-name-and-argument signatures. A different tool or any other progress event clears it, and adapters without structured tool input leave detection off.
- The pane overlay admits the cards. Everything after it (process metrics, provider panels, git and PR facts, spend tallies, group labels, the presentation sort) enriches only panes the frame already holds.

The same half stamps two group fields that the CLI and the sidebar both render. `SidebarWorktreeGroup.team` is the unique non-empty team among the group's rows (`cohort_team`): rows without a team are ignored, and two distinct teams leave it empty. `SidebarWorktreeGroup.label_qualifier` is set, after cached git facts finalize branch labels, only on groups whose labels collide, as the shortest trailing path suffix that tells them apart (`disambiguate_group_labels`).

`project_local` is the renderer-local half: classify session presence against this renderer's clock, resolve this renderer's own view, and drop its own pane from the roster. Snapshot clones share agents, fenced sessions, pane bindings, resume outcomes, the group vector, and group rows through copy-on-write handles. Sidebar chrome is absent from the roster, so normal local projections copy no shared collections. Exclusions, overlays, and unread write-back detach the group vector only when needed, and then detach only changed row collections. Presentation sorting leaves already-sorted groups and rows shared. An armed layout freeze may detach the group vector and every group's rows to keep the user's frozen ordering. Sharing changes neither the JSON representation nor the renderer's snapshot interface.

A frameless fold, which a cold fast lane or a CLI caller wanting rollup metadata gets, leaves `panes_produced_at_ms` null and `worktree_groups` empty while store metadata still paints.

### Workspace enrichment

Provider panels are keyed by `LoginKey`, with `SidebarProviderPanel.account` telling a kind's logins apart. The native `default` account is omitted on serialization.

`AgentCard.cache` carries the card's `CacheClock`, stamped by `enrich_core` so that rendering reads neither prices nor files:

| Field | Stamped from |
| --- | --- |
| `ceiling_secs` | The provider's prompt-cache TTL, on every agent row whose kind has one. |
| `last_request_at` | The age pin's un-held anchor when later than the agent's own activity, so a keepalive ping refreshes the clock without counting as activity. |
| `warm_until`, `held_since` | Set when `lanes/keep-warm.json` names a horizon for the agent and `cache_keepalive::holds` says keep-warm is holding it at snapshot time. `held_since` is `turn_ended_at` ([messaging.md](../harness/messaging.md#prompt-cache-keepalive)). |
| `rewarm_cost_usd` | The card's current context priced as a cache write through the memoized price book, using `context.model_id` before the carried `model`, and the 1h bucket for TTLs of at least an hour. Missing tokens or an unpriced model leave it absent. |

The card's age pin and context meter both read it through `crates/rimz/src/sidebar_pane/render/sections/agent_card/gauge.rs::cache_age`. An unknown TTL keeps the age pin's hour fallback but never expires the meter.

The pipeline lane reads one board per eligible staged-team group on every refresher pass, with no TTL or event-log scan. It takes team definitions from the seat policy the refresher loads once per pass whenever an agent carries a team or profile (`crates/rimz/src/harness/cache_keepalive.rs::SeatPolicy::load`): trusted repo-local teams through `config::effective::load` when a project root is available, otherwise machine teams. A group needs a single team, a staged definition, exactly one distinct non-empty lexically normalized worktree path among that team's rows, and a readable board with a `Stage:` line. With no eligible group it reads no board but still clears stale cache entries.

The board's ledger becomes stage visits. The parser folds parseable ledger entries (`opened` included). A same-stage re-flip changes no visit, and leaving `Done` clears visits for the new run. A negative interval adds zero seconds. When the ledger's open stage differs from the board stage, the open-visit stamp is absent. The record's fields are in [the pipeline lane row](#enrichment-lanes).

`enrich_core` projects the cache by group key onto `SidebarWorktreeGroup.pipeline`, and absent entries become `None`. Rendering uses only the current snapshot time:

- `SidebarPipeline::span_secs` adds prior seconds to the open visit's age, with no fallback to the run start. It is `None` at `Done`, without an open visit, or before its start.
- `total_secs` measures from the run start to snapshot time, or to the stop at `Done`, capped at snapshot time.
- `clock_running` requires either start stamp and a position outside `Done`.

Every shared fast fold reads the pipeline cache, so a changed board publication reaches all panes without an adoption or skip memo.

## Published lanes

Each lane is one file with one writer, written temp-file-plus-rename. Exact freshness values are in [Cadences](#cadences). Locks, single-flighting, and repair live in the module that writes the lane.

Room-local lanes live in the workspace runtime directory beside the store's runtime files ([store.md → the per-room runtime tier](../store.md#the-per-room-runtime-tier)). Account-global data caches live under `~/.rimz/cache/providers/` so relaunches open warm, and their `*.lock` election files under `$XDG_RUNTIME_DIR/rimz/shared/`.

### The pane frame

`lanes/snapshot.json` publishes the topology everything else enriches. [`PaneFrame`](../../../crates/rimz/src/sidebar/frame.rs) carries tabs and panes, each pane with its current and rotated-out previous process record, child pids, and sampled resource metrics. It also carries `viewed_panes`, the session `focused_pane` register, and client presence. Only the host writes it ([`produce/panes.rs`](../../../crates/rimz/src/sidebar/produce/panes.rs)), together with `lanes/metrics-sample.json`. The host folds its retained frame, and passive readers read the publication. One-shot inspection and recipient resolution always run a private cached-consistency mux listing and the same repair and acceptance ladder, sampling into throwaway history without publishing panes, metrics history, or agent projection.

Three properties matter downstream:

- It is the card-admission boundary. A pane absent from the frame renders nothing, whatever the store says about it.
- `observed_at_ms` is the fusion supersession baseline. It records when the pane source saw the topology, not when the producer wrote the file.
- It carries one monotonic topology stamp. A topology publication bumps it, and metrics and presence publications keep it. The fetch worker's tab-rename memo reads it to tell a new topology from a repeat.

The producer repairs raced-null processes, missing cwds, and briefly omitted panes before publishing. Those guards are [sidebar.md → Honest reads across a mux hiccup](./sidebar.md#honest-reads-across-a-mux-hiccup).

### Projections

| Lane | Writer | Carries |
| --- | --- | --- |
| `lanes/agent-projection.json` | Producer fetch worker | For one session: normalized admitted agent kinds, default launch models, the sorted admitted `(kind, absolute workspace)` inputs, and provider-validated local-session observations. |

The host probes wiring behind exact input stamps and batches account-scoped discovery per represented kind. It compares the fresh agent projection against its last successful publication in memory and writes the file only on change. A failed write stays retryable. The producing fold already carries the new projection to panes, so publication sends no extra wake.

Readers bind observations through the intersection of published and current inputs. The host filters its retained projection the same way. Passive readers parse the file once and validate the session, so a newly added input waits for the next publication and a removed one disappears at once. A missing, malformed, or wrong-session file fails closed. CLI inspection probes privately and never publishes the file.

### Presence hints

These files are the multiplexer's side of the channel, written outside the sidebar's fold. [`mux/zellij/pane_topology.rs`](../../../crates/rimz/src/mux/zellij/pane_topology.rs) owns the path, schema, and freshness window of the two Zellij files, whichever process writes them.

| Lane | Writer | Carries |
| --- | --- | --- |
| `lanes/pane-topology.json` | Zellij presence plugin, through `rimz sidebar wake` | The pane roster (live panes, tab names, foreground commands, geometry) and full client views, from which the host derives attached-client count, terminal `viewed_panes`, and unique-live focus. |
| `lanes/presence-desired.json` | Zellij room-owner flows (birth, reload repair and upgrade, web sharing) | The requested plugin build and configuration identity that ranks competing writers ([multiplexers.md → Writer fencing](../multiplexers.md#writer-fencing)). |
| `live/presence.stamp` | Zellij plugin, tmux control-mode watch | A liveness mark carrying its own `written_at_ms`, so `rimz doctor` reads the same age the producer does. While it is fresh, the pane lane relaxes to the longer event-mode TTL. |

### Enrichment lanes

The fetch worker publishes process metrics and group roots alongside pane production. The cache refresher owns the rest, and asks the separately elected spending service to refresh spend.

The project-root trunk lane publishes `lanes/trunk-state.json` separately from per-worktree diff stats and emits through a detached helper. Its baseline and single-flight contract live in [Trunk and worktree signals](../harness/loops.md#trunk-and-worktree-signals).

| Lane | Scope | Carries |
| --- | --- | --- |
| `lanes/group-roots.json` | Room | The fetch worker's enumerated checkout roots and each root's exact RimZ marker name, cached under `WORKTREE_ROOTS_TTL` with the session-boundary freshness floor. |
| `lanes/diff-stats.json` | Room | Per-worktree git facts in two stamped halves. Edit-sensitive: added and removed lines, dirty and untracked state, branch, merge or rebase in progress. Commit-shaped: ahead and behind, landed marker, did-work marker, `--from-pr` provenance. Also the head and merge-base the line counts were measured at, so a clean tree at the same pair skips the `git diff`. The landed and did-work proofs are [worktrees.md → What the sidebar asks](../harness/worktrees.md#what-the-sidebar-asks). |
| `lanes/pr-state.json` | Room | Pull-request links and CI verdicts by worktree path and demanded branch ([PR state](#pr-state)). |
| `lanes/metrics-sample.json` | Room | Per-pane resource samples and pane-to-root-pid bindings. The figures publish on the pane frame. |
| `lanes/codex-daemon-reap.json` | Room | Codex daemon-mode reap inputs: live daemon PIDs and the app-server's loaded-thread list, stamped `produced_at_ms`. The producer re-probes every `CODEX_DAEMON_REAP_TTL` (30 seconds), and readers accept a record for `CODEX_DAEMON_REAP_STALE` (90 seconds), so a replacement probe overlaps the previous evidence. An absent, unreadable, or stale record reaps nothing and reports Codex daemon health unknown. |
| `lanes/workspace-spending.<hash>.json` | Room | The cockpit spend tally, headline cutoff, and the live-card session keys excluded from walked headline USD. A consumer reads the file named by its own scope hash and requires a matching `scope_hash`. When that misses because its cached roots lag the producer's, it serves the one remaining publication, since the producer prunes every other. Two candidates or none leave the tally absent. |
| `lanes/cohort-spend.json` | Room | Lifetime dollars, token split, and retained active time for each finished group with an agent, singletons included, plus each seat's lifetime dollars and tokens keyed by agent-row id. A seat folds in the pane-backed children it launched through `attribution::slot_groups`, so an orphaned launched child counts as its own seat. Refreshed for collapsible groups only, every `COHORT_SPEND_TTL` (60 seconds), through the shared cross-session effort aggregator, and projected onto `SidebarWorktreeGroup::cohort_effort`. Renderers never parse transcripts. Only records born in their checkout's current lifetime count ([attribution.md](../agents/attribution.md#selecting-records)). An unreadable checkout contributes no seat totals and is logged. |
| `lanes/link-stats.json` | Room | The latest remote-SSH probe stats behind the footer link badge ([remote.md](../remote.md)). |
| `lanes/pipeline.json` | Room | Version 2, by group key: stage list, board stage and owner, the current stage's open-visit stamp (`stage_started_at`), earlier closed visits in whole seconds (`stage_prior_secs`), the entered-stage set (`visited`), and the run's `started_at` and `done_at`. Published by `refresh/pipeline.rs` every refresher pass only when content changes, through `write_temp_then_rename_cache`. A version mismatch reads as empty. Version-2 caches without the visit fields read them as zero and an empty set, and an empty `visited` is omitted on write. Projected onto `SidebarWorktreeGroup.pipeline` ([workspace enrichment](#workspace-enrichment)). |
| `lanes/keep-warm.json` | Room | Version 1: TTL- and stage-condition-admitted keep-warm horizon seconds by `<kind>:<agent_id>`. Published by `refresh/keep_warm.rs` every heavy pass only when content changes, through `write_temp_then_rename_cache`. A version mismatch reads as empty. Projected onto `AgentCard.cache`. |
| `provider-spending.json` | Account-global | Fleet and provider spend totals plus the walk stamp ([spending.md](../agents/spending.md)). |
| `spending.json` | Account-global | The incremental transcript parse cache behind the spend walk ([spending.md](../agents/spending.md#the-incremental-cache)). |
| `pricing-cache.json` | Account-global | The remote token-price refresh over the embedded snapshot ([spending.md](../agents/spending.md#token-pricing)). |
| `accounts.json` | Account-global | Per-login account state ([providers.md → The shared caches](../agents/providers.md#the-shared-caches)). |
| `rate_limits.json` | Account-global | Fused budget windows per login. |
| `credits.json` | Account-global | Provider-reported paid and extra usage, and the direct-query claim. |

`rate_limits.json` is the one fused source for every sidebar and `rimz accounts` reader, and every writer, out-of-band usage reads included, fuses before publishing. The rules live in providers.md: how readings from different sources and times fuse and when a refill is confirmed ([Window fusion](../agents/providers.md#window-fusion)), how an entry displays after its reset, including named quotas and model sub-caps that read unknown until reported again, and which account a live reading may join ([Persistence across idle sessions](../agents/providers.md#persistence-across-idle-sessions)), and when a fusion verdict pulls the next direct read forward ([Refresh cadences](../agents/providers.md#refresh-cadences)). Only the producer confirms a refill or requests that early read, and consumers hold the persisted value. The sub-cap tick is drawn in [the interface reference](../../interface/sidebar.md).

### PR state

`lanes/pr-state.json` answers, for each worktree the room shows or a [loop or workflow demands](../harness/loops.md#condition-planning), which pull request belongs to it and what CI says. The record and reader are `forge::pr_state`. The producer-only probe that writes it is [`sidebar/refresh/pr.rs`](../../../crates/rimz/src/sidebar/refresh/pr.rs), which shells out to `gh` or `tea`. A checkout whose forge is unsupported gets no link, and the file may still carry its bookkeeping and probe stamps.

Demand reaches past the room's panes. Live loop conditions, and active workflow runs with forge-reading condition cases or default-scoped `ci`/`pr` wakes, keep their scope checkouts probed without panes, and workflow demand lasts through steps. Conditions can also demand branch rows in `branches[repo_key][branch]`, resolved through the scope checkout's `path_repos` entry, without a checkout of the selected branch. A branch row holds an optional PR link and optional head CI. Branch rows use the runtime-cache publication path, keep the prior row on a failed group probe, and are pruned when demand ends. Signals remain checkout-only.

#### Which PR a checkout owns

A link belongs to one checkout incarnation. Each link is stamped with its PR head branch and the RimZ worktree marker's creation time (`TargetStamp`), so a branch switch or a recreated managed worktree drops the old link and resolves again.

| Candidate | Attaches when |
| --- | --- |
| Open PR | Its branch matches, since a live push updates it. |
| Merged or closed PR, managed worktree | It was created no earlier than the marker, within `TERMINAL_PR_CLOCK_SKEW` (5 minutes). A missing marker or forge timestamp accepts it. |
| `--from-pr` checkout | Its named PR, always. |
| Any PR, trunk checkout | Never, so a same-named fork branch cannot attach a false badge. |

An open link also carries a display-only `stack`, projected as the worktree group's `pr_stack`. Both forges supply a repository-wide open set: GitHub adds one `open` alias to the first batch, counting against its 100-alias budget, and Tea adds `base` to its existing list fields without another request. Cross-repository PRs never join the stack. From the linked PR, the walk follows base-to-head downward, choosing the highest PR number for duplicate heads, and head-to-base upward. It stops below at the diff-stats trunk (an `origin/`-prefixed name included), and above at a fork, keeping all fork members in ascending PR-number order as the last level. Each direction stops at eight levels or a repeated PR number. Below is stored bottom-first and above nearest-first, and neither includes the own PR. Member URLs use the repository's normal PR URL form. Non-open links have empty stacks, and failed open-set reads keep prior stacks. The stack never takes part in CI or transition decisions. The cockpit's open-PR count counts its members, each distinct `(url, number)` once. GitHub's first 100 open PRs and Tea's list and server page limits can leave a shorter stack on busy repositories.

#### CI verdicts

The CI verdict comes from the most specific commit:

| Target | Verdict source |
| --- | --- |
| Open PR | The PR head's check rollup. |
| Merged PR | The merge commit's checks, falling back to the PR head's when the merge commit has none. GitHub rereads it on every due probe, so a CI rerun moves the verdict. Tea stops rereading an owned merged link once its verdict settles. |
| Path without a PR, trunk included | The exact local HEAD commit, stored in the `branch_ci` map. |

#### When a repository is probed

A repository is due when its tier TTL has passed, when a target's HEAD differs from the last seen HEAD, or when a target has never been probed. The tier is `PR_STATE_HOT_TTL` (20 seconds) when any of its worktrees is hot, focused, has a pending verdict, or has an open PR in the merge queue, and `PR_STATE_TTL` (5 minutes) otherwise. Branch rows join the uncached and pending or queued hot tiers without synthetic paths reaching disk. A failed probe backs off from `PR_STATE_RETRY_TTL` (30 seconds), doubling up to the tier TTL, and keeps the last-known-good links.

GitHub resolves a due repository with one `gh api graphql` request that aliases every branch lookup and every distinct HEAD commit. It splits into further requests only past `GH_BULK_MAX_ALIASES` (100). The count is two per checkout PR lookup, three per demanded branch (PR, facts, head ref), and one each for a distinct HEAD and the optional repository open set. Each branch lookup includes an open-only facts alias in that same request.

Tea lists open PRs once per repository and reads combined commit status per open branch. It runs per-PR reads only for a path that drops off the open list while its prior link is open, merged without a settled verdict, or no longer owned. Demanded branch targets also share one all-state list per repository pass, from first sight. Terminal candidates use the detail API, and head CI uses the branch ref.

#### Transitions become signals

Each publication is diffed against the previous one ([`refresh/pr/transitions.rs`](../../../crates/rimz/src/sidebar/refresh/pr/transitions.rs)):

| Change | Signal |
| --- | --- |
| A link leaves the open state | `pr.merged` or `pr.closed` |
| A continuous target has a successful prior repository probe and gains an owned open PR without a prior owned open link of the same number | `pr.opened` |
| An open GitHub PR is behind its live base (`behindBy > 0`), on entry or a changed forge head | `pr.behind` |
| An open GitHub PR has settled conflicting mergeability, on entry or a changed settled-at head | `pr.conflicted` |
| An open GitHub PR is in the merge queue, on entry or a later `enqueuedAt` | `pr.queued` |
| An open GitHub PR was removed from the merge queue for a reason other than `merged`, on entry or a later removal | `pr.dequeued` |
| A PR's CI verdict, or a PR-less path's verdict for a commit the branch added (trunk exempt), settles to passing or failing | `ci.passed` or `ci.failed` |

A transition counts only when the target stamp is continuous and the repository probe succeeded, so a branch switch or a failed probe emits nothing. PR links must belong to that stamp, and terminal PR transitions still require a prior open link. A PR-less path emits only for a commit the branch added: a known HEAD matching the worktree marker's base commit, or the cached merge base (an ancestor of the resolved trunk), is inherited and emits nothing. Trunk targets are exempt, and unknown facts keep emission on. The badge still shows the inherited verdict.

The payload carries `path`, `branch`, and `repo`, plus `number`, `url`, and `state` when a link exists, and the observed `head` with a `checks_url` built from the target's `RemoteRepo` (GitHub's `/commit/<head>/checks` page, Gitea's commit page). The producer stays read-only: it spawns a detached `rimz events emit --source forge`, which appends the event durably and fires subscribed loop and wait tasks ([loops.md](../harness/loops.md#the-signal-vocabulary)).

#### GitHub open-PR facts

Open GitHub links cache the forge head, base name, behind count, and last settled mergeability with its settled-at head. The facts alias requests `headRefOid`, `mergeable`, `isCrossRepository`, and `baseRef { name compare(headRef: <branch>) { behindBy } }` only for open PRs. Non-open links carry no facts, and Tea supplies none, so Tea emits none of the four GitHub-only signals.

`pr.behind` and `pr.conflicted` key on a per-head value: the forge head when `behind_by > 0`, or the settled-at head when mergeability is conflicting. A nonempty next key different from the prior key fires. A missing, unowned, non-open, or fact-free prior link has no key, so first sight on a continuous target fires too. Repeated readings do not fire, and leaving and re-entering the state can fire again for the same head. Both payloads carry the PR context with `state: "open"`, `pr_head` (the key), and `base` when known. Only `pr.behind` adds `behind_by`. `head` stays the local worktree HEAD, not the forge head.

`UNKNOWN` mergeability carries the prior settled value forward only for an owned link with the same PR number, even after a push changes the forge head. A later settled conflict at that new head can then fire once.

The behind distance is scoped to its own PR. Null `baseRef` or `compare` means unknown distance and never fires `pr.behind`. Fork PRs never fire `pr.behind`, because the comparison resolves the head branch name in the base repository, while their mergeability still applies. Errors under a requested `facts<i>` alias leave that PR's distance unknown without failing the repository probe, even when `gh` exits nonzero. Other GraphQL errors, missing data, and command timeouts fail the probe.

#### Merge-queue facts

An open GitHub link also caches one merge-queue fact, `queue`, read inside the same facts alias through `mergeQueueEntry { enqueuedAt }` and the latest `RemovedFromMergeQueueEvent` (`createdAt`, `reason`, `beforeCommit { oid }`):

```json
"queue": { "state": "queued", "at": "2026-10-03T12:28:46Z" }
"queue": { "state": "dequeued", "at": "2026-10-03T12:46:03Z", "reason": "failed_checks", "commit": "3ba93ec…" }
```

This fact is the only trace of a queue failure. The queue runs CI on its own commit, which neither the PR head rollup nor a local HEAD rollup sees. A present queue entry is `queued`, stamped with its `enqueuedAt`. With no entry, the latest removal is `dequeued`, stamped with the event's `createdAt`. `reason` is GitHub's string verbatim when supplied, and `commit` is the queue's own commit, absent when GitHub names none. A removal whose reason is `merged` is no fact: the PR is about to read merged, and `pr.merged` is its only signal whichever of the two the poll sees first. `dequeued` is a standing state that survives new pushes and holds until the PR is queued again, merged, or closed. A poll that returns no facts node drops the fact rather than carrying it.

`pr.queued` and `pr.dequeued` key on the fact's timestamp under the same continuity, ownership, and successful-probe rules as the head-keyed pair, so a first reading on a continuous target fires and a repeated reading does not. A fresh room starts from an empty cache, so its first poll only stamps the target, and a standing `queued` or `dequeued` is never announced there. `pr.queue` is how it is read. Their payload keeps `pr_head` as the PR head and puts the timestamp in `queued_at` or `dequeued_at`. `pr.dequeued` adds `reason` when supplied and, when the fact has a commit and the remote builds one, `queue_checks_url`.

Queue errors fail the whole repository probe. A GraphQL error whose path reaches `mergeQueueEntry` or `timelineItems` fails it, since a nulled entry would otherwise read as a stale removal. The prior links carry and nothing emits, while a `compare` error stays scoped and keeps the queue fact. A server whose schema lacks merge-queue fields rejects the whole query, so the repository probe fails there.

### Sidecars

Per-session sidecars (`agent_context/`, `subagent_context/`, `agent-activity/`, `active-time/`) are the one exception to producer ownership. Hook and statusline runs write the context and activity records, hooks update the active-time accumulators under per-record locks, and the host's transcript watcher refreshes transcript-tail context between hooks ([push channels](#push-channels)). The shared host fold reads them fresh behind stat-gated parse caches.

### Coordination and receipts

Unless marked, these live in the workspace runtime directory.

| File | Purpose |
| --- | --- |
| `live/heartbeat/sidebar.<instance>.json` | Liveness for launch gating and wakeup fanout, named by the full instance id, plus the pane size that `rimz sidebar frame` draws at. |
| `sock/host.<digest>.wake.sock` | The host's room datagram socket, named by a digest of mux and session to fit the AF_UNIX path limit. Every attachment's heartbeat names this same address. |
| `lanes/focus-anchor.json` | The durable jump intent, optional card placement, and frozen order every renderer reads on fusion ([focus intent](#focus-intent)). |
| `lanes/unread.json`, `live/read-marks/marks.json` | Open unread episodes and the room's locked, max-updated read marks. |
| `lanes/loop-fire.json` | The host's loop-task arm and fire stamps for this room. |
| `lanes/authoritative-pane-probe.json` | One single-flight winner's authoritative mux pane observation, shared by every sidebar's liveness watchdog. |
| `lanes/sidebar-width.json` | The room-runtime sidebar width the renderers settled on. |
| `lanes/sidebar-filter.json` | The room-runtime body lens: independent status, unread, or PR pick and the committed text query, adopted by every renderer. |
| `audit/binding.log.jsonl` (state directory) | Append-only pane-bind decisions ([sidebar.md](./sidebar.md#the-binding-ladder)). |
| `records/live-roster.json` (state directory) | The producer's pane-backed live root-agent set, read by rebirth recovery and written best-effort after each produced final fold ([sidebar.md → Resume-on-rebirth](./sidebar.md#resume-on-rebirth), [store.md → session death](../store.md#session-death)). |
| `audit/diag.log.jsonl` (state directory) | Typed anomaly records ([diagnostics.md](../diagnostics.md)). |

`cache/snapshots/latest.json` and `cache/snapshots/rollup.json` are not sidebar files: the store's write tail publishes them into the state directory ([store.md → the read path](../store.md#the-read-path)).

The body lens codec is `BodyLens` in [`body_filter.rs`](../../../crates/rimz/src/sidebar/body_filter.rs), for example `{"filter":{"kind":"status","status":"waiting"},"query":"auth"}`. Both fields are optional, and writing an empty lens removes the file. An absent or malformed file, including the older single-filter shape, loads as an empty lens. A typing draft belongs to its client view and is never written here.

Heartbeats are bounded by TTL while the session lives and by purge at rebirth: a birth that has proven the session absent deletes heartbeat files before creating the replacement session.

## Realtime events

Wakeup datagrams carry `SidebarEventEnvelope` ([`wakeup/events.rs`](../../../crates/rimz/src/wakeup/events.rs)): a schema version, the workspace id, an optional session scope, a sender timestamp, and the typed event.

`session_name` is the scope. `Some` targets the one mux session whose pane ids the event names. `None` reaches every room host of the workspace, which is how store deltas and reloads travel. The host drops an event for another workspace or session once. Reload words and envelopes reach the host inbox directly. The plane records every other envelope once, then fans it out to subscribers' bounded wake channels for pane-local reactions and producer verification. The host's tmux watch and body-filter saves use the same in-memory ingress without a datagram.

Events divide by what the receiver does with them:

- Overlay events land in the in-memory event store and change what the next fuse paints. Exactly four qualify: `PaneClosed`, `CommandChanged`, `FocusChanged`, and a `PaneOpened` that carries a command.
- Nudges and actions are consumed on arrival. Some ask the producer for a verifying pull, some drive a renderer action, and none touch the overlay store.

The plane owns one overlay store ([`event_store.rs`](../../../crates/rimz/src/sidebar/event_store.rs)), shared by its attachments and diagnostic observer. Each pane fuses its own projected baseline against it. The store keeps one slot per key (per pane per event kind, plus one focus slot), latest stamp wins, capped at 256 entries. Each entry records `sent_at_ms`, used for supersession against the pane frame, and the host's receipt time, used for expiry after `EVENT_STORE_TTL` (12 seconds, inclusive). A late pane cannot extend that lifetime or reinsert an expired overlay. The overlay cut a [view borrow](#client-views) takes is a clone of the store that shares its entry vector by reference count, so a cut copies no events. An append, prune, or cap that changes the store detaches the vector and leaves every outstanding cut as it was, and an append identical to the stored entry changes nothing.

A pane keeps its baseline only while a fold applies an overlay or focus intent, or while the gate holds a rejected pull. The next pull or synthetic fold that finds neither overlay nor intent releases the baseline. Event expiry alone does not trigger a fold or repaint.

### Event taxonomy

| Event | Payload | What the receiver does | Emitter |
| --- | --- | --- | --- |
| `PaneClosed` | `pane_id` | Delete every row bound to the pane. Highest precedence in fusion. | Host presence projector |
| `CommandChanged` | `pane_id`, `command` | Overlay the command until a pull verifies the pane's row shape. | Host presence projector |
| `FocusChanged` | `focused` and `unfocused` pane id lists | Set `SidebarSnapshot::focused_pane` to the current pane and mark it viewed. A transition naming only the prior pane clears the register. | Host presence projector, from client-derived Zellij focus or tmux pane and view focus |
| `PaneOpened` | `pane_id`, optional `command` | Nudge a producer verification pull. Admits no card on its own. | Host presence projector |
| `PanesChanged` | none | Nudge a producer pull: topology moved, identity unknown. | Projector fallback, or an incomplete tmux layout |
| `StoreDelta` | optional event method and lifecycle signal | Refetch the rollup. A session start or end also requests fresh panes. | Store and context-sidecar writers ([store.md → wakeups](../store.md#wakeups)) |
| `FocusIntent` | target `pane_id`, nonce | Fold the durable focus anchor at once, unless the same nonce was already folded through the host push. | Renderer jumps |
| `FocusStranded` | owning sidebar `pane_id`, generation, client views | Focus repair in the matching renderer (see below). Other renderers ignore it. | Host presence projector, from a settled Zellij switch or a tmux window switch |
| `WidthTargetChanged` | none | Re-read the room-runtime width share, resolve it against this renderer's view, and converge only its own pane. | The resolver or renderer that published a new target |
| `BodyFilterChanged` | none | Re-read the whole body lens and adopt pick and committed query without a producer fetch, leaving any local draft intact. | A renderer that changed the lens or auto-cleared its empty pick |
| `Notify` | `title`, `body`, target panes, `recheck_unread`, kind | Raise the desktop notification and the tab bell. `recheck_unread` gates the bell on row-unread, not the desktop notification. Never fused into rows ([notifications.md](./notifications.md)). | The fetch worker, after it has run any notification command handlers itself |
| `Reload` | none | Observe the durable workspace target: a changed host build drains attachments for reattach, and an unchanged build refetches in place. Supervisors poll the record independently. | `rimz reload` |

`FocusStranded` repair keeps the renderer's baseline if that is a live visible work sibling, and otherwise picks the leftmost sibling. Distinct observed client views leave focus alone, because `focus-pane-id` is session-global. The repair reads the strand's sample on Zellij and the pulled `client_views` on tmux, never fused focus or intents. It is dropped after `FOCUS_STRANDED_EVENT_TTL` (2 seconds), so late delivery cannot yank focus.

On tmux, a direct pane-focus transition never lists sidebar chrome in `unfocused`, so focus leaving a sidebar arrives as `unfocused: []`. A window switch is different: it names the previous window's active pane as the prior pane whatever its role, so a sidebar can appear in `unfocused` there (`crates/rimz/src/sidebar/presence/tmux.rs::TmuxPresenceState::switch_window`).

`Reload` also travels as a bare control word, so it reaches a renderer whose event schema predates the current one. Pane rows carry no focus bit: focus lives only in the session register.

### What triggers a mux-derived event

Neither backend constructs a `SidebarEvent`. Both normalize what they see into presence transitions, and the shared projector ([`presence/projector.rs`](../../../crates/rimz/src/sidebar/presence/projector.rs)) decides the event.

| Room change | Event | How each backend sees it |
| --- | --- | --- |
| A pane joins the room | `PaneOpened` | Zellij diffs a new terminal id out of the announced manifest. tmux takes the first non-seeding subscription line naming an unknown pane. |
| A pane's command changes | `CommandChanged` | Zellij compares the pane command across two live manifests. tmux reads the changed command field of a subscription line. |
| A pane leaves the room | `PaneClosed` | Zellij finds the terminal id absent from the new manifest. tmux takes a window-close line, or a pane missing from the layout roster. |
| Focus moves between panes | `FocusChanged` | Zellij projects session focus from client observations. tmux takes a newly active subscription pane or a pane-changed line only in a session's current window, seeded at attach from the pane seed's session id and window-active flag (falling back to any window only if no current-window seed arrived). A hidden window's active pane is window-local state, kept for a later view switch. |
| A view switch lands away from a sidebar | `FocusChanged` | Zellij settles the switch generation. tmux takes the window-changed line. |
| A view switch lands on a sidebar that has a working sibling | `FocusStranded` | The same inputs, classified by the landed pane's role. |
| The layout cannot be read whole | `PanesChanged` | tmux only: a window holding a floating pane, or a layout line that fails to parse. |
| Topology moved but no typed event resulted | `PanesChanged` | The projector's fallback, from either backend. |
| A divider drag resizes panes, roster unchanged | `PanesChanged` on Zellij, nothing on tmux | The Zellij plugin hashes pane position and column count, so a drag republishes an announced manifest with no roster, command, or focus diff. The tmux subscription carries no geometry. |

The last row has a consequence: a renderer that reads `PanesChanged` as structural evidence reads a Zellij divider drag as structure moving.

Each observation also carries event eligibility by pane role, which keeps each backend's established stream:

| Pane role | Zellij | tmux |
| --- | --- | --- |
| Work pane | All events | All events |
| Sidebar | No open. Close, command, and direct focus. | Direct focus only |
| Launch chrome | All events (an open carries no command) | None |

View switches follow their own rule on both backends: landing on launch chrome emits `FocusChanged`, and landing on a sidebar with a working sibling emits `FocusStranded`.

### Push channels

A push channel lets a change a writer already knows about reach every renderer within one wakeup instead of a poll window. The producer's pull stays the structural backstop behind all of them.

- Store and sidecar writers post a `StoreDelta` after every durable write or context-sidecar merge, so status, tokens, and cost repaint within one wakeup.
- The Zellij presence plugin publishes topology snapshots and client observations through `rimz sidebar wake`. That CLI process accepts one writer under the topology lock, derives focus, runs the projector, publishes `lanes/pane-topology.json`, and stamps `live/presence.stamp`. Its events reach the host over the room socket. What the plugin sends, how often, and how competing writers are fenced and retired is [multiplexers.md → The Zellij presence plugin](../multiplexers.md#the-zellij-presence-plugin).
- The tmux control-mode watch, run by the host, keeps only out-of-order stream state, maps subscription and focus lines into the same transitions, stamps `live/presence.stamp`, and feeds the same projector. The plane records its events once and fans them out in memory to subscribed panes ([multiplexers.md → The control-mode presence watch](../multiplexers.md#the-control-mode-presence-watch)).
- The host's transcript watcher ([`transcript_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/transcript_watch.rs)) watches each live session whose adapter declares `transcript_tail_context`, Codex and Copilot included, and runs the stat-gated refresh on write to cover mid-turn gaps between progress hooks. Every five seconds it folds its cursor and stats the sidecar directory before deciding whether to rescan records. An unchanged log extent and directory mtime skip the roster rebuild for up to 60 seconds, which bounds coarse-mtime misses. A rescan reads context sidecars only for the rollup rows that have not ended, so an orphan sidecar costs nothing. A failed watch registration leaves that gate open for the next five-second retry. A watcher that never starts costs nothing, because the producer-tick refresh stays unconditional.
- The host's cache refresher ([`cache_refresh.rs`](../../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs)) ticks on the data cadence. It refreshes heavy lanes from retained host inputs (or, in a room with no previous publication, from a frameless rollup before the first produce), fires due loop tasks (including the watch-lost backstop for a dead `rimz wait` watcher), wakes due scheduled messages, and emits [forge signals](#pr-state) and [trunk signals](../harness/loops.md#trunk-and-worktree-signals). Daemon-view repair receives the same retained frame. A panic resets only its rollup cursor, and the next tick retries from retained inputs and heavy-lane caches.

### What presence data drives

Focus buys a fast tick for the work the user is watching. The producer folds `PaneFrame.viewed_panes` into the snapshot. Git edit-sensitive facts for the viewed worktree and process metrics for the viewed pane run on the focused tier, while commit-shaped git facts and every background worktree and pane stay on their slower cadences. The other side effects gated on `viewed_panes` are listed in [multiplexers.md → Who is looking at what](../multiplexers.md#who-is-looking-at-what).

Client presence is classified against the reader's `now_ms` by `SidebarPresence::classify`: `Detached` when no human client remains, `Idle` once input has been quiet for `[sidebar] afk_after_secs` (15 minutes by default), and `Active` otherwise. Only tmux reports an input clock (`client_activity`), so an attached Zellij room stays `Active` until every client detaches. The tmux clock advances only on real input, including input over SSH, so remote tmux rooms honour the setting too. On both backends the same setting also bounds [departed-view retention](#client-views).

tmux presence is sampled, not pushed. While a client is attached, the producer re-samples every `PRESENCE_SAMPLE_TTL` (1 second). It records `presence_probed_at_ms` in memory before sampling, so failed attempts also suppress bursts, and a restart loses at most that throttle window. An unchanged sample does not rewrite `lanes/snapshot.json`, and pane publications send no self-wake. A failed fallback focus probe carries the prior presence and viewed panes from the retained frame.

## Fusion rules

Fusion is pure over pulled truth, the event store, and `now_ms`. It runs on the render thread on every paint that has an overlay or intent to apply. An unmodified pull goes straight to presentation.

Supersession comes first. The baseline is `panes_observed_at_ms.or(panes_produced_at_ms)`, and an event no newer than the pane observation is skipped, because the pull already saw later truth. The exception is a `PaneClosed` naming a carried pane, which applies at any age: the frame held that pane on process evidence without seeing it, so nothing in the frame supersedes the close. The close also retires the carried-pane notice.

The surviving overlays then apply in precedence order:

1. `PaneClosed` deletes rows and clears a listed child's matching pane binding, returning its entry click to the parent. If it names `focused_pane`, the register clears and the renderer keeps its last highlight.
2. `CommandChanged` overlays the command on panes that survived step 1 and were already admitted.
3. The newest `FocusChanged` sets or clears the session register, as in the taxonomy. With one observed client, a singleton focus replaces its old `viewed_panes` until the next pull, so the departed tab stops painting at once. Unknown or multiple clients keep the conservative union, and client-view membership changes only on a resolved sample. With one observed client, a singleton focus away from this sidebar also cancels its optimistic resize and focus watching.
4. A requested focus intent lands last and outranks both the pulled register and `FocusChanged` for `FOCUS_ANCHOR_FRESH` (2.5 seconds), provided its pane is an admitted row's own pane or a listed child's. This presentation overlay adds to `viewed_panes`, not the observed `client_views`, and cannot make a strand's client-ambiguity guard defer repair.

`PaneOpened` creates nothing: it asks the producer for a verified frame. A wrong verdict from a missed event lasts until the next producer pull. A skewed sender clock can stamp an event later than the following pane observations, so it can outlive several pulls, but expiry runs on the receiver's clock, so it never lasts past `EVENT_STORE_TTL` (12 seconds) after receipt.

## Focus intent

A RimZ-initiated focus action, a user jump or an automatic repair, is a two-phase intent in `lanes/focus-anchor.json` ([`mux/focus_anchor.rs`](../../../crates/rimz/src/mux/focus_anchor.rs)), a rename-atomic runtime record written without fsync. The one exception, a bare jump after a Zellij focus-taking directional split, is in [multiplexers.md → jumping to a pane](../multiplexers.md#jumping-to-a-pane). The two phases are:

- `Requested` is written and broadcast with `FocusIntent` before the one-way mux focus command.
- `Applied` records that the mux accepted the command, without reseeding presentation. Native confirmation and fencing read this phase.

### The anchor record

The file is workspace-wide and client-scoped. It carries a nonce, the session, the target pane, the origin, the exact pre-action client map, an optional card placement, and the frozen row order from the source frame. The version is `rimz.focus-anchor.v4`. Its optional `placement {row_id, viewport_row, held}` is the card's own first line relative to the source scroll-zone top, signed when clipped above, together with the ids open by hold or selection in that source frame. A missing `held` defaults to an empty set, and an empty set is omitted. Readers also accept v3, ignoring its line offset and yielding no placement. Other versions are ignored.

The fetch worker reads the anchor once per fold cycle and shares that observation with every projection, alongside the room body filter. The painter resolves focus against its view's clients and applies jump placement only in the owning or initiating view. Confirmation, read intent, and gate and health reduction travel with that view. Read sweeps load the current room mark record through the stat-keyed parse cache. Explicit actions and event-only folds read live state. Retiring an observed intent always compares its nonce with the live file, so a newer intent written after the shared read cannot be cleared by an older fold. A retirement lands in the view whose borrow queued it (`crates/rimz/src/sidebar_pane/app/loop_state/shared.rs::ViewBorrow`, its `target`): a hidden pane's Scratch fold retires in Scratch, never in a shared view an earlier read routed to.

### Verdicts

Native client observations resolve the intent, with the pre-action client map as the fence. [`observation_outcome_from`](../../../crates/rimz/src/mux/focus_anchor.rs) checks these rows in order, and the first match wins. Fresh means within `FOCUS_ANCHOR_FRESH` (2.5 seconds) of the request or, for an applied intent, of acceptance.

| Order | Condition | Verdict | Effect |
| --- | --- | --- | --- |
| 1 | The observation names another session or no session, or the target left the roster. | `Invalidated` | Drop the overlay. A sidebar filters out another session's anchor before judging it, so it never presents, invalidates, or retires that anchor. |
| 2 | Requested, and fresh. | `Present` | Keep presenting the target. |
| 3 | Requested, and stale. | `Invalidated` | The request was abandoned. Drop the overlay. |
| 4 | Applied, client presence unknown. | `Present` while fresh, then `Fence` | Yield unknown instead of restoring stale evidence. |
| 5 | Applied, and no client remains or the client id set changed. | `Invalidated` | Detach or replacement. Drop the overlay. |
| 6 | Applied, and every observed client views the target pane. | `Confirmed` | The action landed. Retire the intent. |
| 7 | Applied, and the client map moved somewhere other than the target. | `Superseded` | Something else took focus. Drop the overlay. |
| 8 | Applied, and the pre-action map is unchanged. | `Present` while fresh, then `Fence` | Keep presenting, then yield unknown. |

A requested intent ignores the client map: only an applied one is judged against it.

### Placement and the click guard

A jump carries the clicked card's painted position so the destination can land it on the same row. The sidebar captures a visible clicked card's own first line relative to its scroll-zone top, and a pane-backed child uses its parent card. The placement carries the opening set it was measured under: held expansions plus the last drawn selection. The destination composes its landing frame from that set instead of its own holds and previous selection. This one-paint carry is a geometry fact about the source frame.

- A same-host jump lands in the initiating client's view without reinstalling order or restarting its hold. Carried order is a fallback only for external or recovered anchors without a live source view.
- An off-screen repair and non-sidebar focus carry no placement.
- With a placement, a fresh request queues positioning for the next draw using the destination's zone top. Without one, it leaves the viewport anchored.
- Only a focus requested by this view's own input may reveal an off-screen card, and acceptance cannot apply the same request stamp twice.

Portable anchors stay relative to the scroll zone. Each pane keeps its signed absolute painted placements, correction-clamp facts, and banner visibility for local observation.

Clicks resolve against the painted rows, never a newer roster. Each draw writes the projected roster's row ids beside its hit map, and a row click resolves the painted id in the current roster rather than a newer roster's ordinal. A departed id is swallowed without changing selection, ending search, or re-arming the freeze. A watched non-input paint that moves a previously painted card keeps the preceding hit frame for 400 ms. Further moves keep that first frame and deadline, clicks at exactly 400 ms use the current frame, and an input paint clears the guard. Unwatched paints clear the guard and never arm it. Resize delivery shares the redraw path but is not user input: while watched, it keeps an active guard and can arm one when its paint moves a card. The loop owns the monotonic deadline, and rendering only writes geometry back.

The shared input cut is stamped before its reads. A pane's later focus-anchor observation takes precedence over older cuts. An actual retirement fences their pre-jump focus unless a later native focus event supersedes it, and ordinary anchor absence does not fence native focus. A later local filter observation or write makes older filter sync non-authoritative. A later cut still converges missed events, and the complete snapshot still passes through fusion, gate, health, and local application. Receipt lookup reads the current room record, so an old cut cannot renew a clear for unchanged activity.

### The cross-tab handshake

A user jump to another tab in the same host waits for the destination to paint before it focuses, so the user never sees the old frame in the new tab. Same-tab jumps and automatic repairs skip all of this.

1. Push. The source writes the `Requested` anchor and also pushes it through the host's attachment inboxes ([`app/focus_push.rs`](../../../crates/rimz/src/sidebar_pane/app/focus_push.rs)). Peers fold its target, optional card placement, and frozen order once per nonce across push and datagram.
2. Fit (tmux only). Before fan-out, tmux freshly probes the destination and fits it if stale. Zellij adds no geometry probe or fit.
3. Wait. The source waits off the render loop until the destination's frame is written, every peer has released, or the deadline at click + 500 ms passes. Request work and any fit spend that same budget. A fit never resets the deadline, and a fit that uses it up leaves no paint wait.
4. Dispatch. The source sends the mux focus command with the existing nonce check.

A push has priority over ordinary inbox wakeups and carries the anchor value as an arrival observation without reading the file. When folded, older shared cuts yield to it and newer reads supersede it. The owner tries the existing gated paint inside the push handler, ahead of maintenance. Each peer answers with one verdict ([`crates/rimz/src/diag/focus_trace.rs::PushVerdict`](../../../crates/rimz/src/diag/focus_trace.rs)):

| Verdict | When |
| --- | --- |
| `painted` | The destination wrote a frame with the owning card selected, its placement consumed, and its card on the requested row. |
| `displaced` | The destination consumed the placement but the card painted on another row or not at all. |
| `not_destination` | The peer does not own the target, in another session or not its row. |
| `exiting` | The attachment is closing. |
| `expired` | The push's deadline passed before a qualifying paint. |
| `superseded` | A newer click replaced this push. |

`painted` and `displaced` release the source's paint wait. A peer whose current snapshot already has the target row reports `not_destination` before projection or folding only when its own view is known and the selected unseen success in the views previews no own-view change. A hidden non-owner then also skips the ordinary `FocusIntent` event. A missing target, unknown own view, or pending own-view change keeps the drain-and-fold path before the ownership check.

A fitted push also carries the expected rows per destination pane. Both cached-frame and parked acknowledgements then require the last painted height to match the attachment's own expected rows. A missing expectation, including an absent own-pane entry, keeps the ordinary predicate. Acknowledgement does not check columns, because sidebar width control may change them after a fit. When the fit actually reasserted the sidebar's legit width, the own-pane expectation also carries the columns tmux laid out. A resize landing at exactly that rows-by-columns size bypasses the held-grow paint hold, and a push arriving after the resize releases an engaged hold only at that exact size. Rows-only expectations and mismatching sizes keep the ordinary hold path.

A resize paint hold, empty-tab confirmation, or exit can prevent acknowledgement, and the wait cap still permits dispatch. If all handles drop, including a failed destination delivery, dispatch can proceed before the cap. While a lease is active, a loop ignores early or obsolete focus datagrams until the current pushed nonce is registered, and ordinary paints also wait for that nonce to fold. The push carries the durable anchor, so this avoids painting an older request before the current request thread reaches its store.

Every handshake records its source outcome and per-peer stages in [`audit/focus.log.jsonl`](../diagnostics.md#push-handshakes), after dispatch and bounded late-report collection. A `FocusIntent` missed outside this push path waits for the next event or pull while the file stays unresolved. If it was already retired, the first native-focus fold moves the highlight without revealing the card.

Automatic repairs append to the account-global `focus-repairs.log.jsonl` at command acceptance (`AcceptedUnconfirmed`) or failure, and again when observation confirms, supersedes, or invalidates the action. Dispatch records carry the final live fence sample in `observed` and the request count in `attempts`. Abandonment and later resolution records use zero attempts and no fence sample. Records without these fields read as an empty sample and one attempt.

### Jump leases

The host's jump lease orders cross-tab requests. It is armed on the source loop before the synchronous click paint, with a monotonically increasing click sequence, the target pane, and the click + 500 ms deadline. It fences obsolete pushes and paints and does not suspend room work. Each loop consumes its priority push before ordinary inbox work and maintenance. Input has a separate bounded lane of 64 wakeups, which keeps input order even when the room inbox is full. Release wakes every attachment once.

| Focus source | Lease effect |
| --- | --- |
| A cross-tab focus click or key | Arms a newer lease and push. |
| Same-tab focus input | Advances the sequence and cancels the active jump and its pending pushes, without arming a lease or push of its own. |
| Automatic repairs, CLI focus, the hibernate writer | No lease, and no effect on an active one. |

Input still catches up the routed view, and resize updates pane geometry without changing hidden views. Only user focus input supersedes an active lease.

Click sequence, not detached-thread arrival order, fences in-host requests. The current-sequence check and the request store are serialized against other in-host jump requests. A newer click also cancels an older paint wait. Checkpoints after fitting, and under the focus-anchor lock before each mux dispatch attempt, stop obsolete work, including supersession during the lock wait or retry delay. The paint consumer retires obsolete parked pushes even when the newer target is elsewhere. Request failure releases before rollback is enqueued, dispatch releases on return, and a drop guard covers early returns and unwinding. Releasing an old sequence cannot clear a newer one.

## Cadences

Each row is a staleness budget. The constants and the reasoning behind each live beside the code they govern: sidebar cadences in [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs), presence-stamp freshness in [`mux/mod.rs`](../../../crates/rimz/src/mux/mod.rs), the heartbeat TTL in [`wakeup/heartbeat.rs`](../../../crates/rimz/src/wakeup/heartbeat.rs), the focus-anchor window in [`mux/focus_anchor.rs`](../../../crates/rimz/src/mux/focus_anchor.rs), and the paint grid in [`config.rs`](../../../crates/rimz/src/config.rs).

| Lane | Cadence | Where staleness shows |
| --- | --- | --- |
| Pane frame | `SNAPSHOT_CACHE_TTL` (750 ms). `EVENT_PANE_TTL` (10 s) while presence is fresh or the retained prior has no viewed panes. An absent prior counts as watched, and carried panes are capped at the poll TTL. | Pane open, close, and cwd or command regrouping with no exact event |
| Zellij topology cache | `PRESENCE_STAMP_FRESH` (150 s). Explicit freshness floors for diagnostic probes and renderer geometry, never structural repair. | Pre-producer pane listing and pushed client views |
| Presence stamp | `PRESENCE_STAMP_FRESH` (150 s) | Switches the pane frame between poll-mode and event-mode TTLs |
| Presence sample | Zellij on client-list events, tab switches, and keepalive. tmux every `PRESENCE_SAMPLE_TTL` (1 s) while clients are attached. | Attach, detach, and viewed-pane gating. The tmux AFK badge clearing after input. |
| Git diff stats | Focused: `DIFF_STATS_FOCUSED_LOCAL_TTL` (3 s) edit facts, `DIFF_STATS_FOCUSED_COMMIT_TTL` (10 s) commit facts. Background: `DIFF_STATS_TTL` (5 s) hot, `DIFF_STATS_SETTLED_TTL` (5 min) for an unviewed, not-hot worktree whose cached entry is clean and landed, `DIFF_STATS_IDLE_TTL` (60 s) for the rest. A host pass refreshes every due focused or hot worktree, and fills up to `MAX_HOST_PASS_ENTRIES` (8) in total with other due worktrees, oldest first. The one-shot CLI refreshes all. | Worktree header churn, ahead and behind counts, landed markers, trunk sync |
| PR state | `PR_STATE_HOT_TTL` (20 s) hot, `PR_STATE_TTL` (5 min) idle. Failures back off from `PR_STATE_RETRY_TTL` (30 s). A HEAD change bypasses the TTL, seen up to `DIFF_STATS_SETTLED_TTL` late in a settled checkout. | Worktree header PR and CI glyphs |
| Worktree roots | `WORKTREE_ROOTS_TTL` (60 s) | Grouping for checkouts added without a session boundary |
| Process metrics | `METRICS_FOCUSED_SAMPLE_TTL` (1 s) viewed, `METRICS_BACKGROUND_SAMPLE_TTL` (3 s) background | Child pids and per-row CPU, memory, IO, and process state |
| Spending service | `SPENDING_TTL` (15 s) | Provider dashboard, fleet totals, and the floor under live cockpit spend |
| Agent projection | Every producer data tick, paying metadata checks only while inputs are unchanged. `LOCAL_SESSION_DISCOVERY_BACKSTOP` (30 s) forces full validation. | Hookless identity binding and launch admission |
| Accounts | Per login (`LoginKey`): `ACCOUNTS_TTL` (10 min) on settled success, `ACCOUNTS_RETRY_TTL` (10 s) on provisional success or failure, keeping last-known-good data on failure. Changed adapter input stamps (Pi auth and session directories) re-probe after the same 10 s floor from the previous probe; absent stamps on settled accounts stay TTL-only. | Provider dashboard login, plan, and account state |
| Account usage and credits | `OAUTH_USAGE_TTL` (5 min) for reads, with longer floors after a settled authentication failure and for idle accounts ([providers.md](../agents/providers.md#refresh-cadences)). `CREDITS_DISPLAY_MAX_AGE` (24 h) for display. A recorded entitlement lapse persists until a direct read settles it ([entitlement](../agents/providers.md#entitlement)). | Budget bars, the paid and extra usage row, the Codex reset marker. A lapse suppresses the plan and balances. |
| Live-session context | Every refresher pass, stat-gated local reads. A Codex turn death that needs pane confirmation also captures 60 pane lines, retried for up to 10 minutes. | Session sidecars. Account windows use the per-login usage lane. |
| Finished-cohort effort | `COHORT_SPEND_TTL` (60 s) | Finished group receipts |
| Team pipeline | Every refresher pass, no TTL. Content-identical writes are suppressed. | Board stage, owner, open-visit stamp, prior visit seconds, visited stages, and run start and stop |
| Codex daemon reap | `CODEX_DAEMON_REAP_TTL` (30 s), stale after `CODEX_DAEMON_REAP_STALE` (90 s) | Codex daemon session reaping |
| Remote link stats | Stale after `LINK_STATS_STALE` (10 s), expired after `LINK_STATS_EXPIRE` (120 s) | Footer link badge for `rimz remote connect` rooms |
| Daemon-view repair | 30 s. A fresh, marker-satisfied frame with unchanged inputs and the same `rimzd` pane-id set as the last pass skips the authoritative check ([rimzd.md](../rimzd.md#who-repairs-and-when)). | Managed `rimzd` pane recovery, loop-watch shape repair, and configuration changes |
| Renderer heartbeat | Written every `HEARTBEAT_WRITE_INTERVAL` (2 s), trusted for `SIDEBAR_HEARTBEAT_TTL` (5 s) | Launch freshness, wakeup fanout, and runtime cleanup |

### The paint clock

Data cadence and paint cadence are separate clocks, which is why the sidebar stays responsive while the data layer runs slow. The fetch worker's 250 ms post-cycle rest bounds data folds even under continuous requests and adds up to 250 ms of card-state latency. The production data-tick gate is separate. Input, resize, and overlay paints do not wait for the worker. The data backstop is the fetch worker's idle tick ([requests](#requests)), so `refresh_ms` changes paint cadence, never pull cadence.

The attachment's `LoopClock` owns transition time and effect policy. Production uses wall time, while the testkit scenario player drives the same loop with virtual time and external effects disabled.

`[theme.display] refresh_ms` sets the base render grid, `DEFAULT_REFRESH_MS` (100 ms) by default, clamped to 16 through 1000 ms. It rides `snapshot.theme.display`, so the renderer uses the default until the first fold and picks up config changes on later folds without reading config itself. Money rolls sample every `refresh_ms * CLICK_PHASES` (two base frames), matching the odometer's phase counter. Row animations sample on `BREATH_ANIMATION_FRAME` (120 ms), never faster than the base grid.

Input paints synchronously off the grid. Keys, clicks, and resize keep their per-wakeup paint, and non-wheel user viewport changes snap. A destination focus push also tries its gated paint on arrival, before maintenance, without bypassing resize holds, empty-tab confirmation, exit, or watched and frame-deadline checks. A relevant overlay event fuses and paints on arrival, and a burst of events coalesces to one paint per base frame.

The wheel glides. Consecutive queued wheel ticks apply in arrival order and coalesce into one paint, and a non-wheel wakeup ends the run and is processed next. Each tick clamps the target to the last drawn range, which keeps reversals at either edge without recomputing geometry between ticks. The visible viewport eases toward that target over 100 ms on a 16 ms grid independent of `refresh_ms`, then returns to the base grid while the scrollbar fades. Position derives from elapsed milliseconds since the loop's monotonic base, stamped beside the animation phase, so a late frame skips ahead rather than stretching the glide. Another wheel run accumulates on the target and restarts from the currently visible row. The input paint pulls the next grid boundary within one current animation interval. Card-anchor corrections snap too, but translate the remaining wheel flight without restarting it or waking the auto scrollbar.

A sidebar in an unviewed tab suspends animation and emits no frame bytes for plain data changes, including when the session is detached. Its publications wait in the views as described in [hidden attachments and reveal](#hidden-attachments-and-reveal). Freeze expiry does not cap a hidden pane's timeout or request a fold, and an unpainted view cancels its tab-read dwell. Watched freeze expiry still wakes and requests a fold to restore live rank. Hidden resize updates geometry but neither reveals the pane nor changes a view, and the grow-resize self-close hold still gates visible resize paints. Input before the sampled client map catches up routes through the evidence rule in [client views](#client-views), so an older retained sample cannot undo it. An unknown own view still counts as watched. Hidden capture semantics are owned by [sidebar.md](./sidebar.md#from-store-to-screen).

The focus-marker dwell also caps `LoopState::frame_timing` while watched, even when otherwise idle. At its deadline the dwell itself makes a paint due, independently of a later ordinary frame boundary. Once ready, the marker holds no expired timer, and the next focus move re-arms it.

## Failure modes

Each degradation has one owner, and none leaves a wrong verdict that outlives the next pull, except an event a skewed sender stamped in the future, which is bounded by the event TTL instead.

| Failure | Why it is survivable | Recovery |
| --- | --- | --- |
| Missed event | Events are latency hints. | The producer's next pull. |
| Dead host | Supervisors keep the room holds and each pane's tty. | The spawn lock admits one replacement host, and each pane reattaches. |
| Clock skew | Event expiry uses receiver time, so no event lives forever. | A future-stamped event can mis-order an overlay across several pulls, and expires `EVENT_STORE_TTL` (12 s) after receipt. |
| Cold host: no fold yet | Memory starts empty with each host. | The placeholder remains until the first fold. |
| Panicking produce | The panic guard catches the unwind and discards the fold cursor. | One failed outcome. The next cycle refolds cold. |
| Unreadable store or panes | The serve loop holds its last committed frame. | A sustained failure raises the sticky health alert. The attachment stays open and the worker's data backstop keeps folding until completed production recovers ([sidebar.md → Degraded reads](./sidebar.md#degraded-reads)). |
| Hook drainer unreachable for 10 seconds | The wrapper's runtime lease keeps the affected cards identifiable without reading diagnostics. | A separate drainer alert keeps the rest of the sidebar usable; after a 3-second recovery hold it lingers until dismissed. |

`crates/rimz/src/sidebar_pane/render/ui_state.rs::ViewState::hook_drainer_alert` is separate from refresh health. Every committed producer or cached fold activates it from `hook_drainer_outage`, or marks an active episode recovered at that fold's snapshot clock when the fact is absent; later absent folds preserve the recovered stamp. Failed reads do not recover it. `x` clears only a recovered drainer episode, while retaining its existing refresh-alert behavior. A view hidden for the entire lease lifetime misses the line; one revealed after its lease clears marks it recovered on reveal. The [recovery hold](../harness/fleet.md) gives visible views time to observe a crossed outage. Host restart loses lingering view state, but doctor retains the durable evidence.

Every accepted anomaly path writes a typed diagnostic before it falls back, holds, suppresses, or exits. Flicker, duplicate rows, or a phantom external group each map to a record in `diag.log.jsonl`. `rimz doctor` shows the recent tail, and [diagnostics.md](../diagnostics.md) names the taxonomy.

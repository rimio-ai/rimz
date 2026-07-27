# The sidebar data plane

> This doc owns how the sidebar gets its data: which process reads what, what gets published where, how realtime events overlay it, and how stale any value can be. [sidebar.md](./sidebar.md) owns what the sidebar does with that data (presence, ranking, layout, recovery). [store.md](../store.md) owns the durable truth beneath it and the runtime directory these caches live in. [performance.md](../performance.md) owns what all of it costs, [DESIGN.md](../../../DESIGN.md) the product commitments, and [diagnostics.md](../diagnostics.md#inspecting-live-card-state) the workflow for inspecting live card state.

## The shape of the problem

A room holds one store and one sidebar per tab, and every sidebar paints the whole fleet live. The same picture gets assembled several times a second, in several processes at once.

Half the inputs are cheap. The store rollup is a lock-free read of `snapshots/latest.json` plus an incremental fold over any log tail it does not yet cover; the runtime sidecars and lane caches are small JSON files behind stat gates.

The other half are expensive. The pane roster costs a multiplexer IPC round trip or a plugin hop. Git costs forks per worktree. Provider accounts, usage, and pricing cost subprocesses and network. Paying those once per tab per tick would saturate both the mux server and the machine, and a room sits idle most of the time anyway.

Three rules resolve that, and everything below is a consequence of them.

1. **The store is truth; every file the sidebar writes is cache.** A cache file rebuilds from the store plus a fresh read, is written temp-file-plus-rename with no fsync, and can be deleted at any moment without losing anything ([store.md → write classes](../store.md#write-classes)). A `cargo xtask invariants` grep keeps store-writer, run-wake, and broker imports out of `crates/rimz/src/sidebar/`, so the read-only boundary holds by construction.
2. **One renderer per workspace pays the expensive reads.** The eldest live renderer is elected **producer**, does the external work once, and publishes the result. Every other renderer folds those published files in process and never pulls for freshness on its own.
3. **Realtime events carry latency, never truth.** A wakeup datagram lets a change paint now instead of at the next poll. A dropped datagram costs staleness bounded by the next producer pull, never a wrong verdict.

```text
        durable truth                          expensive external reads
   ┌──────────────────────┐              ┌──────────────────────────────┐
   │ store rollup         │              │ mux panes · git · providers  │
   │ latest.json + log    │              └───────────────┬──────────────┘
   └──────────┬───────────┘                              │ producer only
              │ every renderer,                          ▼
              │ read event-fresh                published lane caches
              │                            snapshot.json · diff-stats.json · …
              │                                          │
              └────────────────────┬─────────────────────┘
                                   ▼
                    enrich   the ordered fold spine, pure over files
                                   │
    realtime events ──────────────▶│◀────────────── focus intent
    in-memory overlay store        ▼
                          fuse(pulled, events, intent, now)
                          pure: no IO, no subprocess, no clock read past `now`
                                   │
                                   ▼
                            SidebarSnapshot ──▶ paint
```

## Where the code lives

Two module trees split the work by side of the boundary. `crates/rimz/src/sidebar/` is the data plane: it reads, folds, and publishes, and it never writes store truth. `crates/rimz/src/sidebar_pane/` is the renderer process that drives it.

Election and liveness:

| Module | What it owns |
| --- | --- |
| [`mod.rs`](../../../crates/rimz/src/sidebar/mod.rs) | Launch gating, producer election (`ProducerElectionTracker`), and the orphan sweep. |
| [`heartbeat.rs`](../../../crates/rimz/src/sidebar/heartbeat.rs) | The per-renderer liveness file every election and launch gate reads. |

Reading and folding:

| Module | What it owns |
| --- | --- |
| [`consumer.rs`](../../../crates/rimz/src/sidebar/consumer.rs) | The consumer read: event-fresh rollup over the published pane frame, projection adoption, and the fold-skip input stamps. |
| [`enrich.rs`](../../../crates/rimz/src/sidebar/enrich.rs) | The ordered fold spine both producer and consumer run, so the two paths cannot drift. |
| [`frame.rs`](../../../crates/rimz/src/sidebar/frame.rs) | `PaneFrame`, the typed pane topology the producer publishes. |
| [`cache.rs`](../../../crates/rimz/src/sidebar/cache.rs) | The pane-frame cache read, its freshness verdict, and the presence and topology hint files. |

Producing and publishing, all producer-only:

| Module | What it owns |
| --- | --- |
| [`produce/`](../../../crates/rimz/src/sidebar/produce/mod.rs) | The producer read. `panes.rs` assembles and publishes the pane frame behind a single flight, `metrics.rs` samples per-pane `/proc`, `git.rs` enumerates worktree roots. |
| [`refresh/`](../../../crates/rimz/src/sidebar/refresh/mod.rs) | The heavy lanes, each self-gated on its own TTL: `git_stats.rs`, `pr.rs`, `accounts.rs`, `usage.rs`, `credits.rs`, `rate_limits.rs`, `sessions.rs`, `live_spend.rs`, `cohort_spend.rs`, `daemon_reap.rs`. `git_refs.rs` reads ref files in process to skip a `git` fork, and `trace.rs` is the opt-in account-refresh timing trace. |
| [`workspace_projection.rs`](../../../crates/rimz/src/sidebar/workspace_projection.rs) | The renderer-independent fold publication and the consumer's adoption check. |
| [`agent_projection.rs`](../../../crates/rimz/src/sidebar/agent_projection.rs) | Published adapter wiring and provider-local session discovery. |

Realtime and presence:

| Module | What it owns |
| --- | --- |
| [`events.rs`](../../../crates/rimz/src/sidebar/events.rs) | The wakeup envelope, the event taxonomy, and the in-memory overlay store. |
| [`presence.rs`](../../../crates/rimz/src/sidebar/presence.rs) | Zellij presence-wake ingestion and the topology writer gate. `presence/projector.rs` is the shared host policy both backends feed; `presence/tmux.rs` is the control-mode side. |
| [`fuse.rs`](../../../crates/rimz/src/sidebar/fuse.rs) | Pure fusion of pulled truth, overlay events, and pending focus intent. |
| [`focus_anchor.rs`](../../../crates/rimz/src/sidebar/focus_anchor.rs) | The two-phase intent behind every RimZ-initiated focus action. |

Attention, instrumentation, and constants:

| Module | What it owns |
| --- | --- |
| [`unread.rs`](../../../crates/rimz/src/sidebar/unread.rs), [`read_marks.rs`](../../../crates/rimz/src/sidebar/read_marks.rs) | Durable unread episodes and the read receipts that clear them. |
| [`notify.rs`](../../../crates/rimz/src/sidebar/notify.rs) | Notification policy over newly opened unread episodes ([notifications.md](./notifications.md)). |
| [`observe.rs`](../../../crates/rimz/src/sidebar/observe.rs), [`meter.rs`](../../../crates/rimz/src/sidebar/meter.rs) | The frame-stream anomaly observer and the producer tick-budget meter ([diagnostics.md](../diagnostics.md)). |
| [`width_target.rs`](../../../crates/rimz/src/sidebar/width_target.rs) | The room-runtime sidebar share and whether a user action pinned it. |
| [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs) | Every sidebar cadence and TTL, as named constants with the reasoning on each. |

On the renderer side, `render/` paints and `app/` runs the loop and the threads this doc describes:

| Module | What it owns |
| --- | --- |
| [`app.rs`](../../../crates/rimz/src/sidebar_pane/app.rs) | The fixed-timestep serve loop, its thread wiring, and the fuse-and-paint path. |
| [`app/fetch.rs`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs) | The fetch worker: request coalescing, role observation, the two-speed cycle, and result publication. |
| [`app/cache_refresh.rs`](../../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs) | The elder-gated heavy-lane refresher and the elder's timers. |
| [`app/tmux_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/tmux_watch.rs), [`app/transcript_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/transcript_watch.rs) | The two elder-gated push channels. |
| [`app/gate.rs`](../../../crates/rimz/src/sidebar_pane/app/gate.rs), [`app/health.rs`](../../../crates/rimz/src/sidebar_pane/app/health.rs) | The last-known-good commit gate and the debounced degraded-health verdict. |

Start reading at `enrich.rs` if you want to know what a snapshot is made of, at `app/fetch.rs` if you want to know when it happens, and at `timing.rs` if you want to know how often.

## Renderers, the producer, and consumers

Every tab runs one `rimz sidebar serve` process. Each writes its own heartbeat, binds its own wakeup socket, and paints its own frame; none of them ever goes dark waiting on another.

Sidebar instance ids are UUIDv7, so they sort by birth. The renderer that finds no fresher heartbeat below its own id is the producer; every other renderer is a consumer. Election trusts the same `SIDEBAR_HEARTBEAT_TTL` the launch gate does, restamped every `HEARTBEAT_WRITE_INTERVAL`, so a killed producer is honoured for at most one TTL before the next-eldest takes over.

Every long-lived thread in one renderer shares a process-local `ProducerElectionTracker`. A known consumer returns its cached elder with no filesystem work until that heartbeat's mtime-derived expiry, then validates only that one typed heartbeat; an invalid or expired elder falls back to a single directory scan. A cached producer rescans on `HEARTBEAT_WRITE_INTERVAL`, which lets a resumed older renderer demote it promptly. The tracker accelerates heartbeat truth rather than replacing it: launch gating, reload and build convergence, wakeup fanout, rebirth purge, and orphan sweeping keep their uncached full scans.

Four threads inside the renderer gate on that election.

| Thread | Owns while elected | On demotion |
| --- | --- | --- |
| Fetch worker | The pane frame, worktree group roots, the agent projection, and the workspace projection, on the data tick. | Folds published caches only. |
| Cache refresher | Git diff stats, PR state, accounts, usage, credits, finished-cohort effort, auto-continue, budget enforcement, due loop tasks and scheduled messages, and daemon-view repair. | Sleeps on the election poll. |
| tmux control-mode watch | The tmux presence stream for this session (tmux rooms only). | Drops the control client. |
| Transcript watch | Filesystem watches on every live session whose adapter declares `transcript_tail_context`. | Drops the watches. |

Two ownerships sit outside that election. Durable truth bypasses the producer entirely: every renderer reads the rollup event-fresh in process, so a status flip repaints in a consumer tab without waiting for a producer pull. Account-global spending is elected separately, by a lifetime lock and socket versioned on every wire-visible schema and on the persistent/discovery namespace, so its warm walker survives producer demotion here; one-shot snapshot producers connect to an existing owner or use a bounded direct fallback without taking that lock.

Producer death is a degradation like any other, handled by the corresponding election. Status keeps flowing through consumer folds while pane presence waits for the handoff.

## One fetch cycle

The serve loop reads no data itself. It blocks on its wakeup socket, hands work to the fetch worker, and folds the result when the worker nudges it back. Everything below happens on the worker.

A request carries a mode and three flags. `Normal` is the ordinary wakeup; `ProducerFreshPanes` and `HardRefresh` carry a `min_pane_cache_ms` floor that rejects any pane cache older than the signal that asked for it, which is how agent birth and death beat the pane TTL. The `published_frame_hint` flag marks a request raised by a `PaneFramePublished` wake, and `force_fold` makes a renderer-local timer fold from current caches even when nothing on disk changed. The dispatcher merges queued requests (strongest mode, latest floor, flags OR'd), so a delta storm collapses to one run plus at most one deferred follow-up.

One cycle runs four steps.

1. **Observe the role.** One tracker lookup decides producer or consumer for this cycle, and a role change emits a diagnostic.
2. **Try to skip.** An ordinary consumer request stamps the files its fold would read and compares them against the memo from last time. An unchanged stamp posts an `Unchanged` outcome that clears single-flight state without replacing the snapshot or dirtying the frame.
3. **Fast fold.** The producer folds the rollup, pane frame, and sidecars, then publishes the result as `workspace-projection.json`. A consumer instead tries to adopt that publication, and falls back to the same full in-process fold when it cannot.
4. **Produce, if due.** Only the producer, and only when the published pane frame is past its TTL and no attempt has started inside this data tick, pays the reconciling produce: resolve panes, refresh group roots, publish, and fold again with the fresh frame.

A produce runs behind a panic guard. An unwind costs one degraded outcome, the loop holds its last good frame, and the next cycle refolds cold from a fresh cursor rather than trusting a base a panic may have torn.

### The fold spine

Producer and consumer run one ordered spine in [`enrich.rs`](../../../crates/rimz/src/sidebar/enrich.rs), so the two paths cannot drift. It forks no subprocess and writes no cache file; it only projects what is already on disk.

`enrich_workspace` is the renderer-independent half. Its order is load-bearing in three places, and the rest follows from those:

- Adapter wiring is set before any pane-backed projection, because wiring is what gates idle-agent synthesis.
- Activity sidecars land before the pane overlay, so row age, ranking, waiting guards, and the stall window all see the per-tool timestamp rather than the coarser turn-grained event time. The same record carries the open run of consecutive identical tool-name-and-argument signatures; a differing tool or any other progress event clears it, and adapters without structured tool input leave detection off.
- The pane overlay admits the cards, and everything after it (process metrics, provider panels, git and PR facts, spend tallies, the presentation sort) enriches only panes the frame already holds.

Worktree-group construction stamps `SidebarWorktreeGroup.team` in this renderer-independent fold.
The derivation selects the unique non-empty team carried by agent rows in the group: rows without a team are tolerated, while two distinct team names yield no group label.
CLI and sidebar renderers consume that one projected field, so active headers, finished receipts, and fleet tables stay aligned.
After cached git facts finalize branch labels, the same fold stamps `SidebarWorktreeGroup.label_qualifier` only for colliding checkout labels, using the shortest distinguishing trailing path suffix. Snapshot schema version 14 carries that optional field; older persisted projections are rejected by the existing version gate.

`project_local` is the renderer-local half: classify session presence against this reader's clock, resolve this renderer's own view, and drop this renderer's own pane from the roster. Splitting there is what makes the producer's fold shareable at all.

A frameless fold, which is what a cold consumer or a CLI caller wanting rollup metadata gets, leaves `panes_produced_at_ms` null and `worktree_groups` empty while store metadata still paints.

### Adoption and fallback

A consumer adopts `workspace-projection.json` only when it proves the publication describes the same world this renderer is looking at: matching schema version, matching session name, and an exact match on the source tuple of rollup generation, rollup offset, pane-frame topology stamp, pane-frame metrics stamp, and config generation. On a match it clones the parse-cached projection and applies its own `project_local`. On any miss, including an absent, corrupt, legacy, or mixed-build file, it runs the full local fold, which costs no mux read and no git fork.

The producer serializes the projection once per fold and republishes only when the bytes change, so a quiet room writes nothing while time-window verdicts still land when they flip.

### The skip memo

Two input sets back the unchanged check. After a successful adoption the memo holds a slim five-input stamp: the event log, `latest.json`, `snapshot.json`, `workspace-projection.json`, and the config generation. After a fallback it restores the conservative full set, which adds the rollup and carryover caches, the workspace record, the runtime lane caches, the agent projection, the sidecar and message directories, and the filtered per-room spending, budget, and auto-continue files.

Producer cycles, forced folds, fresh-pane requests, hard refreshes, and failed folds all clear the memo, and a 30 second backstop forces a real fold regardless. So the skip is an optimization the correctness path never depends on.

## Published lanes

One file per lane, one writer per lane, each written temp-file-plus-rename. Exact freshness values live in [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs), and per-file mechanics (locks, single-flighting, repair) live in the module that writes the lane.

Room-local lanes live in the workspace runtime directory beside the store's own runtime files ([store.md → the per-room runtime tier](../store.md#the-per-room-runtime-tier)). Account-global data caches live under `$XDG_STATE_HOME/rimz/shared/` so relaunches open warm, while their `*.lock` election files live under `$XDG_RUNTIME_DIR/rimz/shared/`.

### The pane frame

`snapshot.json` is the topology everything else enriches, and it is the load-bearing file of this whole plane. [`PaneFrame`](../../../crates/rimz/src/sidebar/frame.rs) carries tabs and panes, each pane holding its current (and rotated-out previous) process record, child pids, sampled resource metrics, `viewed_panes`, the session `focused_pane` register, and client presence.

Three properties matter downstream:

- **It is the card-admission boundary.** A pane absent from the frame renders nothing, whatever the store says about it.
- **`observed_at_ms` is the fusion supersession baseline.** It records when the pane source saw the topology, not when the producer wrote the file.
- **It carries two monotonic section stamps.** A topology publication bumps both; a metrics publication bumps only metrics; a presence publication preserves both. Those stamps are what let a consumer prove a workspace projection still applies, and a mixed-build frame missing them forces projection fallback.

The producer repairs the frame before publishing: a fresh raced-null read joins to the last published process for that exact pane id, a missing cwd backfills from the process backend, and a pane the source omitted carries forward while liveness still proves it. Those read-side guards are [sidebar.md → Honest reads across a mux hiccup](./sidebar.md#honest-reads-across-a-mux-hiccup).

Written by the producer ([`produce/panes.rs`](../../../crates/rimz/src/sidebar/produce/panes.rs)); read by every renderer's fold.

### Projections

| Lane | Writer | Carries |
| --- | --- | --- |
| `workspace-projection.json` | Producer fetch worker | The renderer-independent half of the fold, plus the exact source tuple a consumer validates it against. Serialized once per fold and republished only when the content changes. |
| `agent-projection.json` | Producer fetch worker | One exact-session publication of normalized admitted agent kinds, default launch models, the sorted admitted `(kind, absolute workspace)` inputs, and provider-validated local-session observations. One producer pass batches discovery per represented kind and probes wiring behind exact input stamps; one semantic comparison performs at most one atomic write and wake. Consumers parse once, validate the session once, and bind observations through the safe published-and-current input intersection, so a newly added input waits for publication and a removed one disappears immediately. Missing, malformed, and wrong-session reads fail closed. |

### Presence hints

These are the multiplexer's side of the channel, written outside the sidebar's own fold.

| Lane | Writer | Carries |
| --- | --- | --- |
| `pane-topology.json` | Zellij presence plugin, through the host CLI | Zellij's pane roster: live panes, tab names, foreground commands, geometry, and full client views. The host derives attached-client count, terminal `viewed_panes`, and unique-live focus; a legacy `focused_pane` is accepted only when `clients` is absent and pane entries carry no focus marks. |
| `presence-desired.json` | Zellij room-owner flows | The latest requested plugin build and configuration identity. A matching writer outranks a non-matching generation during upgrade convergence. |
| `presence.stamp` | Zellij plugin, tmux control-mode watch | A liveness mark carrying its own `written_at_ms`, so `rimz doctor` reads the same age the producer's verdict does. While it is fresh, the pane lane relaxes to the longer event-mode TTL because typed events cover the latency. |
| `client-presence-probe.stamp` | tmux producer | The latest `list-clients` attempt time. Success and failure both suppress another attempt for `PRESENCE_SAMPLE_TTL`. It is neither pane truth nor a consumer-fold input. |

### Enrichment lanes

The fetch worker publishes process metrics and group roots alongside pane production. The cache refresher owns the rest and asks the separately elected spending service to refresh spend.

| Lane | Scope | Carries |
| --- | --- | --- |
| `diff-stats.json` | Room | Per-worktree git facts, split into edit-sensitive stats (added/removed, dirty and untracked state, branch, merge or rebase state) and commit-shaped stats (ahead/behind, landed markers, did-work marker, `--from-pr` provenance), each with its own stamp, plus the group-root set. |
| `pr-state.json` | Room | Producer-only `gh`/`tea` pull-request state, number, and best-effort CI verdict by worktree path, plus a `branch_ci` map for paths without a PR link, with each link stamped by its PR head branch and RimZ worktree-marker creation time, plus per-repo probe stamps, path-to-repo metadata, merge SHAs, and last-seen HEAD SHAs. A branch switch or recreated managed worktree invalidates the path's old link and resolves the current incarnation. For managed worktrees, merged and closed candidates must have been created with the current incarnation (allowing bounded clock skew); `--from-pr` checkouts keep their named PR, and missing marker or forge timestamps fail open. Open PRs remain branch-matched because a live branch push updates them. GitHub resolves each due repository with one GraphQL request in the normal room: aliased branch PR lookups share the response with deduplicated local-HEAD commit rollups, with bounded extra requests only past the alias limit. Every due probe re-verifies merged PR rollups so CI reruns move between passing and failing; Tea keeps its per-PR probes. Open PRs use the head-check rollup; merged PRs use the forge-reported merge commit and fall back to the PR head verdict when merge checks are absent; paths without a PR, including trunk, use the exact local HEAD SHA. Trunk bypasses PR matching so a same-named fork branch cannot attach a false badge. A pending PR or branch verdict keeps the repo on the hot TTL. Absent when the forge is unsupported or the HEAD commit has no forge verdict. |
| `metrics-sample.json` | Room | Per-pane resource samples and pane-to-root-pid bindings. The figures themselves publish on the pane frame. |
| `workspace-spending.<hash>.json` | Room | The room cockpit spend tally, headline cutoff, and the live-card session keys excluded from walked headline USD. Consumers accept a hash-matching publication only and leave the tally absent until one exists. |
| `cohort-spend.json` | Room | Lifetime dollars, token split, and retained active time for each finished multi-agent group, plus each durable seat's lifetime dollars and tokens keyed by agent-row id. The producer refreshes only collapsible groups on a 60-second TTL, folds the audit rollup through the shared cross-session effort aggregator, and projects the cached result onto `SidebarWorktreeGroup::cohort_effort`; renderers never parse transcripts. |
| `link-stats.json` | Room | The latest remote-SSH probe stats behind the footer link badge ([remote.md](../remote.md)). |
| `provider-spending.json` | Account-global | User-global fleet and provider spend totals plus the walk stamp. |
| `spending.json` | Account-global | The incremental transcript parse cache behind the spend walk. |
| `pricing-cache.json` | Account-global | The remote token-price refresh over the embedded snapshot ([providers.md](../agents/providers.md#token-pricing)). |
| `accounts.json` | Account-global | Per-provider login, plan, and account state. |
| `rate_limits.json` | Account-global | Per-account budget windows. |
| `credits.json` | Account-global | Provider-reported paid and extra usage. |

### Sidecars

Per-session sidecars (`agent_context/`, `subagent_context/`, `agent-activity/`, `active-time/`) are the one exception to producer ownership: CLI hook and statusline runs write the context and activity records, the hook updates active-time accumulators under per-record locks, and the elder's transcript watcher refreshes transcript-tail context between hooks ([push channels](#push-channels)). Every renderer reads them fresh behind stat-gated parse caches.

### Coordination and receipts

The remaining files are terse by design.

| File | Purpose |
| --- | --- |
| `heartbeat/sidebar.<instance>.json` | Liveness for election, launch gating, and wakeup fanout. The eldest fresh heartbeat is the producer. |
| `sock/sidebar.<instance>.sock` | The renderer's wakeup datagram socket. |
| `focus-anchor.json` | The TTL-gated durable jump intent, viewport offset, and frozen order every renderer reads on fusion ([focus intent](#focus-intent)). |
| `unread.json`, `read-marks/…` | Open unread episodes and the per-row read receipts every fold merges. |
| `live-roster.json` | The producer's current pane-backed live root-agent set, consumed by rebirth recovery ([sidebar.md → Resume-on-rebirth](./sidebar.md#resume-on-rebirth), [store.md → session death](../store.md#session-death)). |
| `loop-fire.json` | The elder's loop-task arm and fire stamps for this room. |
| `authoritative-pane-probe.json` | One single-flight winner's authoritative mux pane observation, shared by every sidebar's liveness watchdog. |
| `sidebar-width.json` | The room-runtime sidebar width the renderer settled on. |
| `sidebar-filter.json` | The room-runtime cockpit lens every renderer adopts. |
| `binding.log.jsonl` | Append-only pane-bind decisions ([sidebar.md](./sidebar.md)). |
| `diag.log.jsonl` | Typed anomaly records ([diagnostics.md](../diagnostics.md)). |

`snapshots/latest.json` and `snapshots/rollup.json` look adjacent but are not sidebar files: they are state-directory caches the store's own write tail publishes ([store.md → the read path](../store.md#the-read-path)).

Heartbeat lifetime is bounded by TTL between session boundaries and by purge at rebirth. A renderer writes and restamps its own heartbeat, the launch gate and the election trust fresh heartbeats while the session lives, and a birth that has proven the session absent purges heartbeat files before creating the replacement session.

### Daemon-view maintenance

The cache refresher keeps the managed `rimzd` view converged, and it treats a fresh same-session pane frame as evidence it can skip the expensive check. Stable input stamps plus a frame whose managed reconciliation reads `Done` make the 30 second healthy pass stamp-and-parse only. Changed inputs rebuild and preflight the desired view. An absent, stale, wrong-session, or unhealthy frame falls back to the backend's authoritative pane listing and repairs from there. A view deliberately configured absent stays absent.

## Realtime events

Wakeup datagrams carry `SidebarEventEnvelope` ([`events.rs`](../../../crates/rimz/src/sidebar/events.rs)): a schema version, the workspace id, an optional session scope, a sender timestamp, and the typed event.

`session_name` is the scope. `Some` targets the one mux session whose pane ids the event names; `None` is workspace-wide, which is how store deltas, reloads, and pane-frame publications reach every renderer of the workspace. The receive path drops an event for another workspace or session before it reaches the store.

Events divide into two kinds by what the receiver does with them:

- **Overlay events** land in the in-memory event store and change what the next fuse paints. Exactly four qualify: `PaneClosed`, `CommandChanged`, `FocusChanged`, and a `PaneOpened` that carries a command.
- **Nudges and actions** are consumed on arrival. Some ask the producer for a verifying pull, some drive a renderer action, and none of them touch the overlay store.

The store keeps one slot per key: per pane per event kind, plus a single focus slot, latest stamp wins, capped at `MAX_EVENTS` (256). Each entry records both `sent_at_ms`, used for supersession against the pane frame, and its receive time, used for expiry under `EVENT_STORE_TTL` on the receiver's own clock. So a skewed sender clock can mis-order an overlay briefly and can never pin one.

### Event taxonomy

| Event | Payload | What the receiver does | Emitter |
| --- | --- | --- | --- |
| `PaneClosed` | `pane_id` | Delete every row bound to the pane. Highest precedence in fusion. | Shared host presence projector |
| `CommandChanged` | `pane_id`, `command` | Overlay the command until a pull verifies the pane's row shape. | Shared host presence projector |
| `FocusChanged` | `focused` and `unfocused` pane id lists | Set `SidebarSnapshot::focused_pane` from the current pane and mark it viewed; a transition naming only the prior pane clears the register. Rows carry no mirrored focus bit. | Shared host presence projector, from client-derived Zellij focus or tmux pane and view focus |
| `PaneOpened` | `pane_id`, optional `command` | Nudge a producer verification pull. Admits no card on its own. | Shared host presence projector |
| `PanesChanged` | none | Nudge a producer pull: topology moved, identity unknown. | Presence projector fallback, or an incomplete-layout input |
| `StoreDelta` | optional event method and lifecycle signal | Refetch the rollup. A session start or end also requests fresh panes. | Store and context-sidecar writers ([store.md → wakeups](../store.md#wakeups)) |
| `PaneFramePublished` | publication kind (topology, metrics, or presence) | Fold the just-published pane frame from cache. The kind sets how long a hidden consumer may coalesce before folding. | Producer |
| `FocusIntent` | target `pane_id`, nonce | Store-less nudge: fold the durable focus anchor now, so hidden peer tabs repaint before the mux switch reveals them. | Renderer jumps |
| `FocusStranded` | owning sidebar `pane_id`, generation, client views | Renderer focus-repair action only. The matching renderer validates its baseline as a live visible work sibling or picks the deterministic leftmost sibling; distinct client views leave focus alone because `focus-pane-id` is session-global. Dropped past `FOCUS_STRANDED_EVENT_TTL` so late delivery cannot yank focus. | Shared host presence projector, from a Zellij settled generation or a tmux window switch |
| `WidthTargetChanged` | none | Re-read the room-runtime width share, resolve it against this renderer's view, and converge only its own pane. | A resolver or renderer that published a new target |
| `BodyFilterChanged` | none | Re-read the room-runtime cockpit lens and adopt it without a producer fetch. | A renderer that changed or auto-cleared the lens |
| `Notify` | `title`, `body`, target panes, `recheck_unread`, kind | Renderer action only: raise the configured desktop, bell, or command notification, gated on row-unread when `recheck_unread`. Never fused into rows ([notifications.md](./notifications.md)). | The notification path |
| `Reload` | none | Accelerate the supervisor's durable workspace-record poll; the worker hands off or hard-refreshes. | `rimz reload` |

Reload also travels as a bare control word rather than a typed envelope, so it still reaches a renderer whose event schema predates the current one.

Normalized pane observations carry event eligibility, so the shared projector preserves each backend's established stream. tmux suppresses pane open, close, and command overlays for sidebar and launch chrome, suppresses direct focus for launch chrome, and keeps direct sidebar focus. Zellij suppresses only sidebar opens, emits launch-chrome opens without a command, closes every removed terminal, permits live-sidebar command changes, and emits direct focus for every live terminal. View switches keep their own contract on both backends: launch chrome emits `FocusChanged`, while a sidebar with a working sibling emits `FocusStranded`.

### Push channels

Each push channel exists so a change a writer already knows about reaches every renderer within one wakeup instead of a poll window. The producer's pull stays the structural backstop behind all of them.

- **Store and sidecar writers** post a `StoreDelta` after every durable write or context-sidecar merge, so status, tokens, and cost repaint within one wakeup.
- **Mux presence channels** normalize into one host projector. The Zellij plugin publishes announced or silent topology snapshots, stamps `presence.stamp`, and sends raw client observations with a settled generation after each tab switch; the host accepts a writer under the topology lock, sanitizes launch chrome, derives unique-live focus, maps accepted changes and settled evidence into normalized transitions, runs the projector, and publishes `pane-topology.json`. Topology travels as repeated `--topology` arguments of at most 64 KiB and is omitted above 1 MiB while the wake still carries its stamp and telemetry. Only a successful topology publish resets the stale-writer rejection streak. The tmux control-mode watcher retains only out-of-order native stream state, maps its subscription and focus lines into the same normalized transitions, stamps `presence.stamp`, and feeds the same projector rather than constructing events itself. Backend detail is [multiplexers.md](../multiplexers.md).
- **The elder's transcript watcher** ([`transcript_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/transcript_watch.rs)) watches each live session whose adapter declares `transcript_tail_context`, including Codex and Copilot, and runs the stat-gated refresh on write to cover mid-turn gaps between progress hooks. Only the elder watches, demotion drops the watch, and a watcher that never starts costs nothing because the producer-tick refresh stays unconditional.
- **The elder's cache refresher** ([`cache_refresh.rs`](../../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs)) ticks on the data cadence, rechecks the election each pass, refreshes the heavy caches from the last published pane frame, fires due loop tasks for this room, and wakes due scheduled messages. Demotion turns it into a sleeper and drops no account-global spending state, because the separately elected service owns the sole warm walker. A panic resets only the refresher's rollup cursor, and the next tick retries from cache truth.

### The Zellij writer gate

Only one plugin instance may write topology for a session, and the gate is what picks it. Ranking runs desired-identity match first, then load time, then plugin id, and a retire broadcast closes every identity-mismatched instance regardless of its load time. Once a matching writer is proven, the owner lists plugin panes under a bounded deadline and force-closes every stale `rimz-presence-zellij` id except the accepted writer, unloading old instances that cannot cooperate.

Generic topology reads broadcast `rimz:dump_topology` to existing instances and degrade when none runs. Plugin launch itself belongs to room birth, reload repair and upgrade, and web sharing, each of which records the desired build and configuration in `presence-desired.json`.

The plugin includes every attached client in its full view map, and the host derives distinct client ids and terminal panes from that. Every `PaneUpdate` coalesces a client query independently of topology deduplication; a tab switch issues one query at the settle deadline, and only the reply whose generation matches can publish the settled observation. Stale and superseded replies drop, while missing, detached, dead, foreign, and distinct observations reach host-side classification. Foreground and shell commands follow `CommandChanged`, cwd follows `CwdChanged`, and one in-memory `get_pane_pid` lookup per live terminal pane gives the producer an exact root for targeted process enrichment.

### What presence data drives

Focus buys a fast tick for the work the user is watching. The producer folds `PaneFrame.viewed_panes` into the snapshot; git edit-sensitive facts for the viewed worktree and process metrics for the viewed pane run on the focused tier, while commit-shaped git facts and every background worktree and pane stay on their cheaper cadences.

Client presence is classified by the reader's `now_ms`. Zellij pushes attached human clients plus the terminal panes they view; tmux samples the same shape through `client_view` and also returns the freshest `client_activity` epoch, so `SidebarPresence::classify` marks `Idle` once input has been quiet for `[sidebar] afk_after_secs` (15 minutes by default) and `Detached` when no human client remains. Zellij exposes attach state but no per-client input-idle timestamp, so an attached Zellij room stays `Active` until every terminal client detaches.

Remote tmux honours `afk_after_secs` because host `client_activity` advances only on input crossing SSH, which makes it a faithful idle proxy. Idle-capable tmux presence re-samples on the fast presence cadence while a client remains attached, with `client-presence-probe.stamp` written before the external attempt so event-driven fetches cannot burst probes inside one `PRESENCE_SAMPLE_TTL`. A successful unchanged observation updates the producer's returned sample and leaves `snapshot.json` and its wake untouched; client-count or activity changes publish normally. When a fallback producer focus probe fails, the producer carries the prior presence sample and viewed panes forward from `snapshot.json`.

## Fusion rules

Fusion is pure over pulled truth, the event store, and `now_ms`: no IO, no subprocess, no clock read past `now`. It runs on the render thread on every paint that has an overlay or intent to apply, and an unmodified pull moves straight into presentation.

**Supersession comes first.** `panes_observed_at_ms.or(panes_produced_at_ms)` is the baseline: an event no newer than the pane observation is skipped, because the pull already saw later pane truth. One exception: a `PaneClosed` naming a *carried* pane applies at any age. The frame held that pane on process evidence rather than seeing it, so it proves nothing the close could be superseded by, and the close also retires the pane's carried-truth notice.

**Then the overlays apply in precedence order.**

1. `PaneClosed` deletes rows. If it names `focused_pane`, the register clears and the renderer holds its last highlight.
2. `CommandChanged` overlays the command for panes that survived step 1 and were already admitted.
3. The newest `FocusChanged` lands after the observation overlays: one current pane sets the session register and marks that pane viewed, while a transition naming only the prior pane clears the register.
4. A requested focus intent lands last and outranks both the pulled register and `FocusChanged` for `FOCUS_ANCHOR_FRESH`, provided its pane still has an admitted row.

`PaneOpened` creates nothing; it asks the producer for a verified frame. Pane rows carry no focus state at all, in any step.

Expired events disappear by receiver-clock TTL, and any wrong verdict from a missed event or clock skew is bounded by the next producer pull.

## Focus intent

Every RimZ-initiated focus action, whether a user jump or an automatic repair, is a durable two-phase intent in `focus-anchor.json` ([`focus_anchor.rs`](../../../crates/rimz/src/sidebar/focus_anchor.rs)). The file is workspace-wide and client-scoped, and it carries a nonce, the session, the target pane, the origin, the exact pre-action client map, the viewport offset, and the frozen row order from the source frame.

The intent has two states:

- **`Requested`** is written and wakes every renderer with a store-less `FocusIntent` before the one-way mux focus command. It supplies a bounded presentation overlay, so peer tabs adopt the target, viewport offset, and frozen order while the destination is still hidden.
- **`Applied`** records command acceptance without reseeding presentation. It is the honest accepted phase used by native confirmation and fencing.

Command acceptance and later native client observations resolve an intent. The pre-action client map is the fence, and [`observation_outcome`](../../../crates/rimz/src/sidebar/focus_anchor.rs) returns one of five verdicts:

| Verdict | Condition | Effect |
| --- | --- | --- |
| `Confirmed` | Every observed client views the target pane. | The action landed; retire the intent. |
| `Superseded` | The client map moved somewhere other than the target. | Something else took focus; drop the overlay. |
| `Invalidated` | The session changed, the target pane left the roster, no client remains, the client id set changed, or a requested anchor went stale before acceptance. | Detach, replacement, pane closure, or abandoned request; drop the overlay. |
| `Present` | A requested anchor is fresh, or an applied anchor is fresh with its pre-action map unchanged. | Keep presenting the target. |
| `Fence` | The pre-action map is unchanged past `FOCUS_ANCHOR_FRESH`. | Yield unknown rather than restore stale evidence. |

A missed `FocusIntent` wakeup delays the fold until another event or pull arrives, because the file stays authoritative until observation resolves it. Automatic repairs append to the account-global `focus-repairs.log.jsonl` diagnostics log at command acceptance (`AcceptedUnconfirmed`) or failure, and again when observation later confirms, supersedes, or invalidates the action.

## Cadences

The table names staleness-budget semantics. Exact values and the reasoning behind each live as named constants in [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs).

| Lane | Cadence | Where it is felt |
| --- | --- | --- |
| Pane frame | `SNAPSHOT_CACHE_TTL` by default; `EVENT_PANE_TTL` while the presence stamp is fresh, or while the published frame has no viewed panes | Pane open, close, and cwd or command regrouping with no exact event |
| Workspace projection | Recomputed on every producer fold; content-identical writes are suppressed. Hidden consumers usually adopt after their 1s or 3s coalescing clamp, and the 30 second backstop bounds quiet-room time-window transitions | Shared enrichment for consumer tabs; a source mismatch falls back without delaying status truth |
| Unwatched consumer fold | At most `UNWATCHED_FOLD_CLAMP` for identity-free nudges, `UNWATCHED_METRICS_FOLD_CLAMP` for metrics-only publications; watched renderers and the producer stay immediate | Off-screen store deltas and topology nudges in active rooms |
| Zellij topology cache | `PRESENCE_STAMP_FRESH`; explicit topology floors only for structural repair | Zellij pre-producer pane listing and pushed client view |
| Presence stamp | `PRESENCE_STAMP_FRESH` | Switches the producer between poll-mode and event-mode pane TTLs |
| Presence sample | Zellij on client-list events, active-tab switches, and keepalive self-heal; `PRESENCE_SAMPLE_TTL` while tmux reports attached clients and input-idle timestamps | Zellij attach/detach and viewed-pane gating; the tmux AFK badge clearing after fresh input |
| Git diff stats | Focused: `DIFF_STATS_FOCUSED_LOCAL_TTL` for edit facts, `DIFF_STATS_FOCUSED_COMMIT_TTL` for commit facts. Background: `DIFF_STATS_TTL` hot, `DIFF_STATS_IDLE_TTL` idle | Worktree header churn, ahead/behind counts, landed markers, trunk-sync classification |
| PR state | `PR_STATE_HOT_TTL` for hot and focused repos, `PR_STATE_TTL` for idle repos; failure backoff starts at `PR_STATE_RETRY_TTL` and caps at the repo tier; a HEAD change bypasses the TTL | Worktree header PR glyphs. Each due repo enumerates open PRs once, and a failed repo keeps last-known-good state |
| Worktree root enumeration | `WORKTREE_ROOTS_TTL` | Grouping for checkouts added without a session boundary |
| Process metrics | `METRICS_FOCUSED_SAMPLE_TTL` viewed, `METRICS_BACKGROUND_SAMPLE_TTL` background | Child pids plus per-row CPU, memory, IO, and process-state figures |
| Spending service | The spending domain's `SPENDING_TTL` | Provider dashboard, fleet store, and the floor under the live cockpit spend |
| Agent projection | Producer data tick; unchanged wiring and provider snapshots pay metadata checks only, and a 30 second backstop forces bounded catalog and candidate validation | Hookless identity binding and launch admission, without per-renderer provider-store or config reads |
| Accounts | Per provider: `ACCOUNTS_TTL` on success, `ACCOUNTS_RETRY_TTL` on failure with last-known-good data | Provider dashboard login, plan, and account state |
| Live-session context | `SESSION_REFRESH_INTERVAL` | Provider dashboard budget windows and session sidecars |
| Account credits | `OAUTH_USAGE_TTL` for provider reads, `CREDITS_DISPLAY_MAX_AGE` for display | Provider dashboard paid and extra usage row, and the Codex reset marker |
| Remote link stats | `LINK_STATS_STALE`, expiring at `LINK_STATS_EXPIRE` | Footer link badge for `rimz remote connect` rooms |
| Daemon-view repair | 30 seconds; a fresh frame with unchanged input stamps skips the authoritative work | Managed `rimzd` pane recovery and configuration changes |

### The paint clock

Data cadence and paint cadence are separate clocks, and keeping them separate is the reason the sidebar stays responsive while the data layer runs slow.

`[theme.display] refresh_ms` is the base render grid, defaulting to `DEFAULT_REFRESH_MS` (100ms). It rides `snapshot.theme.display`, so the renderer uses the default until the first fold and picks up config changes on later folds without reading config itself. Money rolls sample on `refresh_ms * CLICK_PHASES`, matching the odometer phase counter; row animations sample on `BREATH_ANIMATION_FRAME`, clamped to at least the base grid.

Input paints synchronously off-grid, an overlay event fuses on arrival and paints on the spot, and a burst of events still coalesces to one paint per base frame. The data backstop stays `rimz sidebar serve --tick-seconds`: changing `refresh_ms` changes paint cadence, not pull cadence.

An attached sidebar in an unviewed tab suspends animation and repaints only when its glanceable roster, status, or unread projection changes, throttled by `BACKGROUND_PAINT_MIN_INTERVAL`. Turn phase, gauges, process metrics, spend, git facts, and animation phase stay off the hidden paint trigger. The serve loop also wakes at the renderer-local order-hold expiry to fire the releasing fold that lets rows and groups settle back to live rank once the user goes idle.

## Failure modes

Each degradation has one owner, and none of them can produce a wrong verdict that outlives the next pull.

| Failure | Why it is survivable | Recovery |
| --- | --- | --- |
| Missed event | Events are latency hints, never truth. | The producer's next pull is the structural backstop. |
| Dead producer | Consumers keep folding the rollup and the last published caches. | Heartbeat election promotes the next-eldest renderer once the stale heartbeat ages out; pane presence waits for the handoff. |
| Clock skew | The event TTL uses receiver time, so no event can become immortal. | A skewed sender timestamp can briefly mis-order an overlay; the verifying pull corrects it. |
| Corrupt or stale projection | Adoption is a live-truth verdict, not a trust decision. | The consumer falls back to the full in-process fold, which needs no mux and no git. |
| Panicking produce | The panic guard catches the unwind and discards the possibly-torn fold cursor. | One degraded outcome; the next cycle refolds cold. |
| Unreadable store | The serve loop holds its last committed frame. | Sustained failure raises the sticky health alert, and a renderer degraded past `GIVE_UP_AFTER_DEGRADED` exits for supervisor respawn ([sidebar.md → Degraded reads and give-up](./sidebar.md#degraded-reads-and-give-up)). |

Every accepted anomaly path writes a typed diagnostic before it falls back, holds, suppresses, or exits. A recurrence of flicker, duplicate rows, or a phantom external group maps to a record in `diag.log.jsonl`; `rimz doctor` shows the recent tail and [diagnostics.md](../diagnostics.md) names the taxonomy.

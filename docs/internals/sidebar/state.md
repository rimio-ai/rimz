# The sidebar data plane

> This page owns how the sidebar gets its data: which renderer reads what, what it publishes where, how realtime events overlay it, and how stale any value can be. [sidebar.md](./sidebar.md) owns what the sidebar does with the data (binding, grouping, ranking, the serve loop, recovery). [store.md](../store.md) owns the durable truth underneath and the runtime directory the caches share. [multiplexers.md](../multiplexers.md) owns the backend push channels, [providers.md](../agents/providers.md) the account caches and window fusion, [performance.md](../performance.md) what the plane costs, and [diagnostics.md](../diagnostics.md#inspecting-live-card-state) how to inspect a live card.

## The shape of the problem

A room holds one store and one sidebar per tab, and every sidebar paints the whole fleet live, several times a second, in several processes at once.

Half the inputs are cheap. The store rollup is a lock-free read of `snapshots/latest.json` plus an incremental fold over the log tail it does not yet cover, and the runtime sidecars and lane caches are small JSON files behind stat gates.

The other half are expensive. The pane roster costs a multiplexer IPC round trip or a plugin hop, git costs forks per worktree, and provider accounts, usage, and pricing cost subprocesses and network. Paying those once per tab per tick would saturate the mux server and the machine, and a room sits idle most of the time anyway.

Three rules resolve that, and the rest of this page follows from them.

1. **The store is truth, and every file the sidebar writes is cache.** A cache file rebuilds from the store plus a fresh read, is written temp-file-plus-rename without fsync, and can be deleted at any moment ([store.md → write classes](../store.md#write-classes)). The `cargo xtask invariants` check `ensure_sidebar_library_boundaries` keeps store writers, the run-wake sender, and the broker out of the import graph of the data plane and the shared wakeup wire, so the sidebar is read-only on the store by construction.
2. **One renderer per workspace pays the expensive reads.** The eldest live renderer is elected **producer**, does the external work once, and publishes the result. Every other renderer is a **consumer**: it folds the published files in process and never pulls on its own.
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

`crates/rimz/src/sidebar/` is the data plane: it reads, folds, and publishes, and never writes store truth. `crates/rimz/src/sidebar_pane/` is the renderer process that drives it. `crates/rimz/src/wakeup/` holds the wire the data plane shares with every other sender, and `crates/rimz/src/mux/` owns the runtime files the multiplexer writes or dispatches on.

The data plane, in `crates/rimz/src/sidebar/`:

| Module | What it owns |
| --- | --- |
| [`mod.rs`](../../../crates/rimz/src/sidebar/mod.rs) | Launch gating, producer election (`ProducerElectionTracker`), and the orphan sweep, all decided over heartbeat records. |
| [`consumer.rs`](../../../crates/rimz/src/sidebar/consumer.rs) | The consumer read: event-fresh rollup over the published pane frame, projection adoption, the skip memo and its input stamps. |
| [`enrich.rs`](../../../crates/rimz/src/sidebar/enrich.rs) | The ordered fold spine that producer and consumer both run. |
| [`frame.rs`](../../../crates/rimz/src/sidebar/frame.rs), [`cache.rs`](../../../crates/rimz/src/sidebar/cache.rs) | `PaneFrame`, the published pane topology; its cache read, freshness verdict, presence stamp, and the authoritative pane probe. |
| [`produce/`](../../../crates/rimz/src/sidebar/produce/mod.rs) | The producer read: `panes.rs` assembles and publishes the pane frame behind a single flight, `metrics.rs` samples per-pane `/proc` and backfills Zellij pids, `git.rs` enumerates worktree roots, `tab_status.rs` projects tab names. |
| [`refresh/`](../../../crates/rimz/src/sidebar/refresh/mod.rs) | The heavy lanes, gated on their own TTLs except the every-pass pipeline lane: `git_stats.rs`, `pr.rs`, `accounts.rs`, `usage.rs`, `credits.rs`, `rate_limits.rs`, `sessions.rs`, `live_spend.rs`, `cohort_spend.rs`, `pipeline.rs`, `daemon_reap.rs`. `runner.rs` holds the bounded worker mechanics, `git_refs.rs` reads ref files to skip a `git` fork, and `trace.rs` is the opt-in account-refresh timing trace. |
| [`workspace_projection.rs`](../../../crates/rimz/src/sidebar/workspace_projection.rs) | Publication of the renderer-independent fold and the consumer's adoption check. |
| [`agent_projection.rs`](../../../crates/rimz/src/sidebar/agent_projection.rs) | Published adapter wiring and provider-local session discovery. |
| [`event_store.rs`](../../../crates/rimz/src/sidebar/event_store.rs) | The in-memory overlay store: which events overlay, how they supersede, when they expire. |
| [`presence.rs`](../../../crates/rimz/src/sidebar/presence.rs) | Zellij presence-wake ingestion and the topology writer gate; `presence/projector.rs` is the host policy both backends feed, `presence/tmux.rs` the control-mode normalizer. |
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
| [`wakeup/heartbeat.rs`](../../../crates/rimz/src/wakeup/heartbeat.rs) | The per-renderer liveness file, its TTL, and the freshness scans every election and launch gate reads. |
| [`mux/focus_anchor.rs`](../../../crates/rimz/src/mux/focus_anchor.rs) | The two-phase intent behind every RimZ-initiated focus action. |
| [`mux/width_target.rs`](../../../crates/rimz/src/mux/width_target.rs) | The room-runtime sidebar share and whether a user action pinned it. |
| [`mux/zellij/pane_topology.rs`](../../../crates/rimz/src/mux/zellij/pane_topology.rs) | The Zellij topology cache, its freshness window, and the desired-presence record. |

The renderer threads, in `crates/rimz/src/sidebar_pane/`:

| Module | What it owns |
| --- | --- |
| [`app.rs`](../../../crates/rimz/src/sidebar_pane/app.rs) | The process shell: terminal setup, runtime-file guards, worker wiring, the fixed-timestep serve loop, and exit handling. |
| [`app/loop_state.rs`](../../../crates/rimz/src/sidebar_pane/app/loop_state.rs) | Renderer state transitions, focus repair, maintenance deadlines, and paint eligibility. |
| [`app/fetch.rs`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs) | Request coalescing, and the fetch worker's role observation, produce cadence, and result publication. |
| [`app/cache_refresh.rs`](../../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs) | The election-gated heavy-lane refresher. |
| [`app/tmux_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/tmux_watch.rs), [`app/transcript_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/transcript_watch.rs) | The two election-gated push channels. |
| [`app/gate.rs`](../../../crates/rimz/src/sidebar_pane/app/gate.rs), [`app/health.rs`](../../../crates/rimz/src/sidebar_pane/app/health.rs) | The last-known-good commit gate and the debounced degraded-health verdict. |

Start at `enrich.rs` to learn what a snapshot is made of, at `app/fetch.rs` to learn when it is built, and at `timing.rs` to learn how often.

## Renderers, the producer, and consumers

Every tab runs one `rimz sidebar serve` process. Each writes its own heartbeat, binds its own wakeup socket, and paints its own frame, and none waits on another to paint.

Sidebar instance ids are UUIDv7, so they sort by birth. The renderer that finds no fresh heartbeat older than its own is the producer, and every other renderer is a consumer. Election trusts `SIDEBAR_HEARTBEAT_TTL` (5 seconds), the same TTL the launch gate uses, and each renderer restamps its heartbeat every `HEARTBEAT_WRITE_INTERVAL` (2 seconds). A killed producer therefore holds the role for at most one TTL before the next-eldest renderer takes over.

The long-lived threads of one renderer share a process-local `ProducerElectionTracker`. A consumer returns its cached elder with no filesystem work until that heartbeat's mtime-derived expiry, then validates only that one heartbeat; an invalid or expired elder falls back to one directory scan. A cached producer rescans every `HEARTBEAT_WRITE_INTERVAL`, so a resumed older renderer demotes it promptly. The tracker only accelerates the election: launch gating, reload and build convergence, wakeup fanout, rebirth purge, and the orphan sweep keep their uncached full scans.

These threads gate on the election:

| Thread | Owns while elected | On demotion |
| --- | --- | --- |
| Fetch worker | The pane frame, worktree group roots, the agent projection, the workspace projection, and best-effort tab-status names, on the data tick. | Folds published caches only and never renames tabs. |
| Cache refresher | Git diff stats, PR state, accounts, usage, credits, finished-cohort effort, auto-continue, budget enforcement (`harness::budget::enforce`), due loop tasks and scheduled messages, and daemon-view repair ([rimzd.md](../rimzd.md#who-repairs-and-when)). | Sleeps on the election poll. |
| tmux control-mode watch | The tmux presence stream for this session (tmux rooms only). | Drops the control client. |
| Transcript watch | Filesystem watches on every live session whose adapter declares `transcript_tail_context`. | Drops the watches. |
| Observer writer | Real-world cross-checks behind frame-anomaly diagnostics (only when diagnostics are enabled). | Skips the cross-checks. |

Two ownerships sit outside this election. Durable truth bypasses the producer: every renderer reads the rollup event-fresh in process, so a status flip repaints in a consumer tab without waiting for a pull. Account-global spending has its own election, a lifetime lock and socket, so its warm walker survives a producer handoff here ([spending.md](../agents/spending.md#one-walk-per-namespace)).

A dead producer is an ordinary degradation. Status keeps flowing through consumer folds, and pane presence waits for the handoff.

## One fetch cycle

The serve loop reads no data itself. It blocks on its wakeup socket, hands work to the fetch worker, and folds the result when the worker nudges it back. Everything below runs on the worker.

A request carries a mode, an optional pane-cache floor, and two flags:

| Field | Meaning |
| --- | --- |
| `Normal` mode | The ordinary wakeup. Only the producer may produce, and only once per data tick. |
| `ProducerFreshPanes` mode | The producer produces regardless of frame age. Raised by agent birth and death. |
| `HardRefresh` mode | Any renderer produces, consumers included. Raised by reload and manual recovery. |
| `min_pane_cache_ms` | A floor that rejects any pane cache older than the signal that asked for it, which is how birth and death beat the pane TTL. |
| `published_frame_hint` | The request came from a `PaneFramePublished` wake. |
| `force_fold` | A renderer-local timer wants a fold from current caches even when nothing on disk changed. |

The dispatcher merges queued requests (strongest mode, latest floor, flags OR'd), so a storm of deltas collapses to one run plus at most one deferred follow-up.

One cycle runs four steps.

1. **Observe the role.** One tracker lookup decides producer or consumer for this cycle, and a role change emits a diagnostic.
2. **Try to skip.** An ordinary consumer request stamps the files its fold would read and compares them with the [skip memo](#the-skip-memo). An unchanged stamp posts an `Unchanged` outcome, which clears single-flight state without replacing the snapshot. The app restamps the held snapshot's `now` and marks the frame dirty only when a pipeline clock is live; a quiet room without one gains no repaint.
3. **Fast fold.** The producer folds the rollup, pane frame, and sidecars, then publishes the result as `workspace-projection.json`. A consumer tries to [adopt](#adoption-and-fallback) that publication and falls back to the same full fold in process.
4. **Produce, if due.** `ProducerCadence::start_attempt_if_due` decides by mode: a `Normal` request produces when this renderer is the producer, no attempt started in the current data tick, and the published frame is at least one tick old; `ProducerFreshPanes` always produces on the producer; `HardRefresh` always produces. The produce resolves panes (a pane cache younger than the [pane TTL](#cadences) skips the mux read), refreshes group roots, publishes, and folds again with the fresh frame. The producer then projects each tab's name from live-agent status and the pane frame and forwards a rename only when the name differs; the release rule for pane-named tabs is [multiplexers.md → Tab names](../multiplexers.md#tab-names). A failed mux write is logged, retried at most once per pane observation, and never fails the snapshot.

A produce runs behind a panic guard. An unwind costs one degraded outcome, the loop holds its last good frame, and the next cycle refolds cold from a fresh cursor instead of trusting a base the panic may have torn.

### The fold spine

Producer and consumer run one ordered spine in [`enrich.rs`](../../../crates/rimz/src/sidebar/enrich.rs), so the two paths cannot drift. It forks no subprocess and writes no cache file; it projects what is already on disk.

`enrich_workspace` is the renderer-independent half. Its order matters in three places:

- Adapter wiring is set before any pane-backed projection, because wiring gates idle-agent synthesis.
- Activity sidecars land before the pane overlay, so row age, ranking, waiting guards, and the stall window see the per-tool timestamp instead of the coarser turn-grained event time. The same record carries the open run of identical tool-name-and-argument signatures; a different tool or any other progress event clears it, and adapters without structured tool input leave detection off.
- The pane overlay admits the cards. Everything after it (process metrics, provider panels, git and PR facts, spend tallies, group labels, the presentation sort) enriches only panes the frame already holds.

The same half stamps two group fields that the CLI and the sidebar both render. `SidebarWorktreeGroup.team` is the unique non-empty team among the group's rows (`cohort_team`): rows without a team are ignored, and two distinct teams leave it empty. `SidebarWorktreeGroup.label_qualifier` is set, after cached git facts finalize branch labels, only on groups whose labels collide, as the shortest trailing path suffix that tells them apart (`disambiguate_group_labels`).

`project_local` is the renderer-local half: classify session presence against this renderer's clock, resolve this renderer's own view, and drop its own pane from the roster. Splitting the spine there is what makes the producer's fold shareable.

A frameless fold, which a cold consumer or a CLI caller wanting rollup metadata gets, leaves `panes_produced_at_ms` null and `worktree_groups` empty while store metadata still paints.

### Adoption and fallback

A consumer adopts `workspace-projection.json` only when the publication describes the same world it sees: matching projection schema version (`WORKSPACE_PROJECTION_SCHEMA_VERSION`), matching session name, and an exact match on the source tuple of rollup generation, rollup offset, pane-frame topology stamp, pane-frame metrics stamp, and config generation; a `latest.json` written at another `SNAPSHOT_VERSION` also forces the fallback, because the consumer reads its rollup extent — and so builds a source tuple to compare — only at the matching version. On a match it clones the parse-cached projection and applies its own `project_local`. On any miss, including an absent, corrupt, or mixed-build file, it runs the full local fold, which costs no mux read and no git fork.

The miss rate rises with write load by construction. The consumer's rollup offset is the live log length at its read, while the producer stamps its publication with the extent of its own last fold, and every commit sends its `StoreDelta` wakeup before the debounced checkpoint publish. During a burst a consumer wakes ahead of the producer's republish, so it falls back. A stale projection is never adopted to avoid that; the fallback is the freshness path, and it stays cheap because the consumer's cursor folds only the appended frames over a carryover parsed once per file identity.

The producer serializes the projection once per fold and republishes only when the bytes change, so a quiet room writes nothing while time-window verdicts still land when they flip.

The pipeline lane reads one board per eligible staged-team group every refresher pass, with no TTL or event-log scan. It resolves trusted repo-local teams once through `config::effective::load` when a project root is available, falling back to machine teams on error or without a root. A group needs a single team, a staged definition, exactly one distinct non-empty lexically normalized worktree path among that team's rows, and a readable board with a `Stage:` line. With no team groups, it skips team resolution and board reads but still clears stale cache entries. The version-2 record carries `stage_started_at` alongside the run's `started_at` and `done_at`. The stage stamp comes from the latest parseable ledger entry into the current stage from a different stage (an `opened` entry counts); same-stage re-flips do not reset it. `SidebarPipeline::span_secs` uses stage age outside `Done`, with no run-start fallback, and the run start/stop at `Done`, capped at snapshot time. `clock_running` requires a stage stamp and a position outside `Done`. `enrich_core` projects the cache by group key onto `SidebarWorktreeGroup.pipeline`; absent entries become `None`. `pipeline.json` participates in both consumer input stamps, so a changed board publication invalidates the skip memo rather than waiting for its backstop. Rendering uses only snapshot time; the app's `Unchanged` restamp keeps a consumer's running clock moving without a new fold.

### The skip memo

The unchanged check compares one of two input stamps. After a successful adoption the memo holds a slim six-input stamp: the event log, `latest.json`, `snapshot.json`, `workspace-projection.json`, `pipeline.json`, and the config generation. After a fallback it holds the full set (`filtered_runtime_inputs` in `consumer.rs`), which adds the rollup and carryover caches, the workspace record, the runtime lane caches (`unread.json`, link stats, `metrics-sample.json`, `codex-daemon-reap.json`, and the rest), the agent projection, the sidecar and message directories including `messages/queue.json` and `read-marks/`, the per-room spending, budget, and auto-continue files, and the account-global `budget.account.*.json`.

Producer cycles, forced folds, fresh-pane requests, hard refreshes, and failed folds all clear the memo, and `CONSUMER_UNCHANGED_BACKSTOP_MS` (30 seconds) forces a real fold regardless. Correctness never depends on the skip.

## Published lanes

Each lane is one file with one writer, written temp-file-plus-rename. Exact freshness values are in [Cadences](#cadences); locks, single-flighting, and repair live in the module that writes the lane.

Room-local lanes live in the workspace runtime directory beside the store's runtime files ([store.md → the per-room runtime tier](../store.md#the-per-room-runtime-tier)). Account-global data caches live under `~/.rimz/cache/providers/` so relaunches open warm, and their `*.lock` election files under `$XDG_RUNTIME_DIR/rimz/shared/`.

### The pane frame

`snapshot.json` is the topology everything else enriches, and the most important file in the plane. [`PaneFrame`](../../../crates/rimz/src/sidebar/frame.rs) carries tabs and panes, each pane with its current and rotated-out previous process record, child pids, sampled resource metrics, plus `viewed_panes`, the session `focused_pane` register, and client presence. The producer writes it ([`produce/panes.rs`](../../../crates/rimz/src/sidebar/produce/panes.rs)), and every renderer's fold reads it.

Three properties matter downstream:

- **It is the card-admission boundary.** A pane absent from the frame renders nothing, whatever the store says about it.
- **`observed_at_ms` is the fusion supersession baseline.** It records when the pane source saw the topology, not when the producer wrote the file.
- **It carries two monotonic section stamps.** A topology publication bumps both, a metrics publication bumps only the metrics stamp, and a presence publication preserves both. The stamps let a consumer prove a workspace projection still applies, and a mixed-build frame without them forces fallback.

The producer repairs raced-null processes, missing cwds, and briefly omitted panes before publishing; those guards are [sidebar.md → Honest reads across a mux hiccup](./sidebar.md#honest-reads-across-a-mux-hiccup).

### Projections

| Lane | Writer | Carries |
| --- | --- | --- |
| `workspace-projection.json` | Producer fetch worker | The renderer-independent half of the fold plus the source tuple a consumer validates it against. |
| `agent-projection.json` | Producer fetch worker | For one session: normalized admitted agent kinds, default launch models, the sorted admitted `(kind, absolute workspace)` inputs, and provider-validated local-session observations. |

The producer builds `agent-projection.json` in one pass that batches discovery per represented kind and probes wiring behind exact input stamps, then writes and wakes at most once, and only when the content differs. Consumers parse it once, validate the session, and bind observations through the intersection of published and current inputs, so a newly added input waits for the next publication and a removed one disappears at once. A missing, malformed, or wrong-session file fails closed.

### Presence hints

These files are the multiplexer's side of the channel, written outside the sidebar's fold. [`mux/zellij/pane_topology.rs`](../../../crates/rimz/src/mux/zellij/pane_topology.rs) owns the path, schema, and freshness window of the two Zellij files, whichever process writes them.

| Lane | Writer | Carries |
| --- | --- | --- |
| `pane-topology.json` | Zellij presence plugin, through `rimz sidebar wake` | The pane roster (live panes, tab names, foreground commands, geometry) and full client views, from which the host derives attached-client count, terminal `viewed_panes`, and unique-live focus. |
| `presence-desired.json` | Zellij room-owner flows (birth, reload repair and upgrade, web sharing) | The requested plugin build and configuration identity that ranks competing writers ([multiplexers.md → Writer fencing](../multiplexers.md#writer-fencing)). |
| `presence.stamp` | Zellij plugin, tmux control-mode watch | A liveness mark carrying its own `written_at_ms`, so `rimz doctor` reads the same age the producer does. While it is fresh, the pane lane relaxes to the longer event-mode TTL. |
| `client-presence-probe.stamp` | tmux producer | The latest `list-clients` attempt. Success and failure both suppress another attempt for `PRESENCE_SAMPLE_TTL`. Neither pane truth nor a fold input. |

### Enrichment lanes

The fetch worker publishes process metrics and group roots alongside pane production. The cache refresher owns the rest, and asks the separately elected spending service to refresh spend.

| Lane | Scope | Carries |
| --- | --- | --- |
| `diff-stats.json` | Room | Per-worktree git facts in two stamped halves: edit-sensitive (added and removed lines, dirty and untracked state, branch, merge or rebase in progress) and commit-shaped (ahead and behind, landed marker, did-work marker, `--from-pr` provenance), plus the group-root set. The landed and did-work proofs are [worktrees.md → What the sidebar asks](../harness/worktrees.md#what-the-sidebar-asks). |
| `pr-state.json` | Room | Pull-request links and CI verdicts by worktree path ([PR state](#pr-state)). |
| `metrics-sample.json` | Room | Per-pane resource samples and pane-to-root-pid bindings. The figures publish on the pane frame. |
| `codex-daemon-reap.json` | Room | Codex daemon-mode reap inputs: live daemon PIDs and the app-server's loaded-thread list, stamped `produced_at_ms`. The producer re-probes every `CODEX_DAEMON_REAP_TTL` (30 seconds); readers accept a record for `CODEX_DAEMON_REAP_STALE` (90 seconds) so a replacement probe overlaps the previous evidence. An absent, unreadable, or stale record reaps nothing and reports Codex daemon health unknown. |
| `workspace-spending.<hash>.json` | Room | The cockpit spend tally, headline cutoff, and live-card session keys excluded from walked headline USD. A consumer reads the file named by its own scope hash and requires a matching `scope_hash`. When that misses because its cached roots lag the producer's, it serves the one remaining publication, since the producer prunes every other; two candidates or none leave the tally absent. |
| `cohort-spend.json` | Room | Lifetime dollars, token split, and retained active time for each finished multi-agent group, plus each seat's lifetime dollars and tokens keyed by agent-row id. Refreshed for collapsible groups only, every `COHORT_SPEND_TTL` (60 seconds), through the shared cross-session effort aggregator, and projected onto `SidebarWorktreeGroup::cohort_effort`; renderers never parse transcripts. Only records born in their checkout's current lifetime count ([attribution.md](../agents/attribution.md#selecting-records)). An unreadable checkout contributes no seat totals and is logged. |
| `link-stats.json` | Room | The latest remote-SSH probe stats behind the footer link badge ([remote.md](../remote.md)). |
| `pipeline.json` | Room | Version 2: stage lists, board stage and owner, stage-entry timestamp (`stage_started_at`), and run start/stop timestamps (`started_at`, `done_at`) by group key. Published by `refresh/pipeline.rs` every refresher pass only when content changes, using `write_temp_then_rename_cache`; a version mismatch reads as empty. Projected onto `SidebarWorktreeGroup.pipeline`. |
| `provider-spending.json` | Account-global | Fleet and provider spend totals plus the walk stamp ([spending.md](../agents/spending.md)). |
| `spending.json` | Account-global | The incremental transcript parse cache behind the spend walk ([spending.md](../agents/spending.md#the-incremental-cache)). |
| `pricing-cache.json` | Account-global | The remote token-price refresh over the embedded snapshot ([spending.md](../agents/spending.md#token-pricing)). |
| `accounts.json` | Account-global | Per-login account state ([providers.md → The shared caches](../agents/providers.md#the-shared-caches)). |
| `rate_limits.json` | Account-global | Fused budget windows per login. |
| `credits.json` | Account-global | Provider-reported paid and extra usage, and the direct-query claim. |

`rate_limits.json` is the one fused source for every sidebar and `rimz providers` reader, and every writer, out-of-band usage reads included, fuses before publishing. The rules live in providers.md: how readings from different sources and times fuse and when a refill is confirmed ([Window fusion](../agents/providers.md#window-fusion)), how an entry displays after its reset, including named quotas and model sub-caps that read unknown until reported again, and which account a live reading may join ([Persistence across idle sessions](../agents/providers.md#persistence-across-idle-sessions)), and when a fusion verdict pulls the next direct read forward ([Refresh cadences](../agents/providers.md#refresh-cadences)). Only the producer confirms a refill or requests that early read; consumers hold the persisted value. The sub-cap tick is drawn in [the interface reference](../../interface/sidebar.md).

### PR state

`pr-state.json` answers, for each worktree the room shows, which pull request belongs to it and what CI says. The record and reader are `forge::pr_state`; the producer-only probe that writes it is [`sidebar/refresh/pr.rs`](../../../crates/rimz/src/sidebar/refresh/pr.rs), which shells out to `gh` or `tea`. The file is absent when the forge is unsupported.

A link belongs to one checkout incarnation. Each link is stamped with its PR head branch and the RimZ worktree marker's creation time (`TargetStamp`), so a branch switch or a recreated managed worktree drops the old link and resolves again. An open PR matches by branch alone, because a live push updates it. A merged or closed PR attaches to a managed worktree only when it was created no earlier than the marker, within `TERMINAL_PR_CLOCK_SKEW` (5 minutes); a `--from-pr` checkout keeps its named PR, and a missing marker or forge timestamp accepts the candidate. The trunk checkout never matches a PR, so a same-named fork branch cannot attach a false badge.

The CI verdict comes from the most specific commit:

| Target | Verdict source |
| --- | --- |
| Open PR | The PR head's check rollup. |
| Merged PR | The merge commit's checks, falling back to the PR head's when the merge commit has none. Re-read on every due probe, so a CI rerun moves the verdict. |
| Path without a PR, trunk included | The exact local HEAD commit, stored in the `branch_ci` map. |

A repository is due when its tier TTL has passed, when a target's HEAD differs from the last seen HEAD, or when a target has never been probed. The tier is `PR_STATE_HOT_TTL` (20 seconds) when any of its worktrees is hot, focused, or has a pending verdict, and `PR_STATE_TTL` (5 minutes) otherwise. A failed probe backs off from `PR_STATE_RETRY_TTL` (30 seconds), doubling up to the tier TTL, and keeps the last-known-good links. GitHub resolves a due repository with one `gh api graphql` request that aliases every branch lookup and every distinct HEAD commit, splitting into further requests only past `GH_BULK_MAX_ALIASES` (100). Tea lists open PRs once per repository, reads combined commit status per open branch, and runs per-PR reads only for a path that drops off the open list while its prior link is open, merged without a settled verdict, or no longer owned.

**Transitions become signals.** Each publication is diffed against the previous one ([`refresh/pr/transitions.rs`](../../../crates/rimz/src/sidebar/refresh/pr/transitions.rs)):

| Change | Signal |
| --- | --- |
| A link leaves the open state | `pr.merged` or `pr.closed` |
| A PR's or tracked branch's CI verdict settles to passing or failing | `ci.passed` or `ci.failed` |

A transition counts only when the same target stamp owns both readings and the repository probe succeeded, so a branch switch or a failed probe emits nothing. The payload carries `path`, `branch`, and `repo`, plus `number`, `url`, and `state` when a link exists, and the observed `head` with a `checks_url` built from the target's `RemoteRepo` (GitHub's `/commit/<head>/checks` page, Gitea's commit page). The producer stays read-only: it spawns a detached `rimz events emit --source forge`, which appends the event durably and fires subscribed loop and wait tasks ([loops.md](../harness/loops.md#the-signal-vocabulary)).

### Sidecars

Per-session sidecars (`agent_context/`, `subagent_context/`, `agent-activity/`, `active-time/`) are the one exception to producer ownership. Hook and statusline runs write the context and activity records, hooks update the active-time accumulators under per-record locks, and the elder's transcript watcher refreshes transcript-tail context between hooks ([push channels](#push-channels)). Every renderer reads them fresh behind stat-gated parse caches.

### Coordination and receipts

Unless marked, these live in the workspace runtime directory.

| File | Purpose |
| --- | --- |
| `heartbeat/sidebar.<instance>.json` | Liveness for election, launch gating, and wakeup fanout, named by the full instance id. The eldest fresh heartbeat is the producer. |
| `sock/sidebar.<short-id>.sock` | The renderer's wakeup datagram socket, named by the short instance id to fit the AF_UNIX path limit. |
| `focus-anchor.json` | The durable jump intent, viewport offset, and frozen order every renderer reads on fusion ([focus intent](#focus-intent)). |
| `unread.json`, `read-marks/sidebar.<instance>.json` | Open unread episodes and the per-renderer read receipts every fold merges. |
| `loop-fire.json` | The elder's loop-task arm and fire stamps for this room. |
| `authoritative-pane-probe.json` | One single-flight winner's authoritative mux pane observation, shared by every sidebar's liveness watchdog. |
| `sidebar-width.json` | The room-runtime sidebar width the renderers settled on. |
| `sidebar-filter.json` | The room-runtime cockpit lens every renderer adopts. |
| `binding.log.jsonl` | Append-only pane-bind decisions ([sidebar.md](./sidebar.md#the-binding-ladder)). |
| `live-roster.json` (state directory) | The producer's pane-backed live root-agent set, read by rebirth recovery ([sidebar.md → Resume-on-rebirth](./sidebar.md#resume-on-rebirth), [store.md → session death](../store.md#session-death)). |
| `diag.log.jsonl` (state directory) | Typed anomaly records ([diagnostics.md](../diagnostics.md)). |

`snapshots/latest.json` and `snapshots/rollup.json` are not sidebar files: the store's write tail publishes them into the state directory ([store.md → the read path](../store.md#the-read-path)).

Heartbeats are bounded by TTL while the session lives and by purge at rebirth: a birth that has proven the session absent deletes heartbeat files before creating the replacement session.

## Realtime events

Wakeup datagrams carry `SidebarEventEnvelope` ([`wakeup/events.rs`](../../../crates/rimz/src/wakeup/events.rs)): a schema version, the workspace id, an optional session scope, a sender timestamp, and the typed event.

`session_name` is the scope. `Some` targets the one mux session whose pane ids the event names; `None` reaches every renderer of the workspace, which is how store deltas, reloads, and pane-frame publications travel. The receive path drops an event for another workspace or session before it reaches the overlay store.

Events divide by what the receiver does with them:

- **Overlay events** land in the in-memory event store and change what the next fuse paints. Exactly four qualify: `PaneClosed`, `CommandChanged`, `FocusChanged`, and a `PaneOpened` that carries a command.
- **Nudges and actions** are consumed on arrival. Some ask the producer for a verifying pull, some drive a renderer action, and none touch the overlay store.

The overlay store ([`event_store.rs`](../../../crates/rimz/src/sidebar/event_store.rs)) keeps one slot per key (per pane per event kind, plus one focus slot), latest stamp wins, capped at 256 entries. Each entry records `sent_at_ms`, used for supersession against the pane frame, and its receive time, used for expiry after `EVENT_STORE_TTL` (12 seconds) on the receiver's clock. A skewed sender clock can briefly mis-order an overlay but can never pin one.

### Event taxonomy

| Event | Payload | What the receiver does | Emitter |
| --- | --- | --- | --- |
| `PaneClosed` | `pane_id` | Delete every row bound to the pane. Highest precedence in fusion. | Host presence projector |
| `CommandChanged` | `pane_id`, `command` | Overlay the command until a pull verifies the pane's row shape. | Host presence projector |
| `FocusChanged` | `focused` and `unfocused` pane id lists | Set `SidebarSnapshot::focused_pane` to the current pane and mark it viewed; a transition naming only the prior pane clears the register. | Host presence projector, from client-derived Zellij focus or tmux pane and view focus |
| `PaneOpened` | `pane_id`, optional `command` | Nudge a producer verification pull. Admits no card on its own. | Host presence projector |
| `PanesChanged` | none | Nudge a producer pull: topology moved, identity unknown. | Projector fallback, or an incomplete tmux layout |
| `StoreDelta` | optional event method and lifecycle signal | Refetch the rollup. A session start or end also requests fresh panes. | Store and context-sidecar writers ([store.md → wakeups](../store.md#wakeups)) |
| `PaneFramePublished` | publication kind (topology, metrics, or presence) | Fold the just-published pane frame from cache. The kind sets how long a hidden consumer may coalesce first. | Producer |
| `FocusIntent` | target `pane_id`, nonce | Fold the durable focus anchor now, so hidden peer tabs repaint before the mux switch reveals them. | Renderer jumps |
| `FocusStranded` | owning sidebar `pane_id`, generation, client views | Focus repair in the matching renderer: keep its baseline if that is a live visible work sibling, otherwise pick the leftmost sibling. Distinct client views leave focus alone, because `focus-pane-id` is session-global. Dropped after `FOCUS_STRANDED_EVENT_TTL` (2 seconds) so late delivery cannot yank focus. | Host presence projector, from a settled Zellij switch or a tmux window switch |
| `WidthTargetChanged` | none | Re-read the room-runtime width share, resolve it against this renderer's view, and converge only its own pane. | The resolver or renderer that published a new target |
| `BodyFilterChanged` | none | Re-read the cockpit lens and adopt it without a producer fetch. | A renderer that changed or auto-cleared the lens |
| `Notify` | `title`, `body`, target panes, `recheck_unread`, kind | Raise the configured desktop, bell, or command notification, gated on row-unread when `recheck_unread` is set. Never fused into rows ([notifications.md](./notifications.md)). | The notification path |
| `Reload` | none | Accelerate the supervisor's poll of the durable workspace record; the worker hands off or hard-refreshes. | `rimz reload` |

`Reload` also travels as a bare control word, so it reaches a renderer whose event schema predates the current one. Pane rows carry no focus bit; focus lives only in the session register.

### What triggers a mux-derived event

Neither backend constructs a `SidebarEvent`. Both normalize what they see into presence transitions, and the shared projector ([`presence/projector.rs`](../../../crates/rimz/src/sidebar/presence/projector.rs)) decides the event.

| Room change | Event | How each backend sees it |
| --- | --- | --- |
| A pane joins the room | `PaneOpened` | Zellij diffs a new terminal id out of the announced manifest. tmux takes the first non-seeding subscription line naming an unknown pane. |
| A pane's command changes | `CommandChanged` | Zellij compares the pane command across two live manifests. tmux reads the changed command field of a subscription line. |
| A pane leaves the room | `PaneClosed` | Zellij finds the terminal id absent from the new manifest. tmux takes a window-close line, or a pane missing from the layout roster. |
| Focus moves between panes | `FocusChanged` | Zellij projects session focus from client observations. tmux takes the newly active pane or a pane-changed line. |
| A view switch lands away from a sidebar | `FocusChanged` | Zellij settles the switch generation. tmux takes the window-changed line. |
| A view switch lands on a sidebar that has a working sibling | `FocusStranded` | The same inputs, classified by the landed pane's role. |
| The layout cannot be read whole | `PanesChanged` | tmux only: a window holding a floating pane, or a layout line that fails to parse. |
| Topology moved but no typed event resulted | `PanesChanged` | The projector's fallback, from either backend. |
| A divider drag resizes panes, roster unchanged | `PanesChanged` on Zellij, nothing on tmux | The Zellij plugin hashes pane position and column count, so a drag republishes an announced manifest with no roster, command, or focus diff. The tmux subscription carries no geometry. |

The last row has a consequence: a consumer that reads `PanesChanged` as structural evidence reads a Zellij divider drag as structure moving.

Each observation also carries event eligibility by pane role, which keeps each backend's established stream:

| Pane role | Zellij | tmux |
| --- | --- | --- |
| Work pane | All events | All events |
| Sidebar | No open; close, command, and direct focus | Direct focus only |
| Launch chrome | All events (an open carries no command) | None |

View switches follow their own rule on both backends: landing on launch chrome emits `FocusChanged`, and landing on a sidebar with a working sibling emits `FocusStranded`.

### Push channels

A push channel lets a change a writer already knows about reach every renderer within one wakeup instead of a poll window. The producer's pull stays the structural backstop behind all of them.

- **Store and sidecar writers** post a `StoreDelta` after every durable write or context-sidecar merge, so status, tokens, and cost repaint within one wakeup.
- **The Zellij presence plugin** publishes topology snapshots and client observations through `rimz sidebar wake`. The host accepts one writer under the topology lock, derives focus, runs the projector, publishes `pane-topology.json`, and stamps `presence.stamp`. What the plugin sends, how often, and how competing writers are fenced and retired is [multiplexers.md → The Zellij presence plugin](../multiplexers.md#the-zellij-presence-plugin).
- **The tmux control-mode watch**, run by the elder, keeps only out-of-order stream state, maps subscription and focus lines into the same transitions, stamps `presence.stamp`, and feeds the same projector ([multiplexers.md → The control-mode presence watch](../multiplexers.md#the-control-mode-presence-watch)).
- **The elder's transcript watcher** ([`transcript_watch.rs`](../../../crates/rimz/src/sidebar_pane/app/transcript_watch.rs)) watches each live session whose adapter declares `transcript_tail_context`, Codex and Copilot included, and runs the stat-gated refresh on write to cover mid-turn gaps between progress hooks. A watcher that never starts costs nothing, because the producer-tick refresh stays unconditional.
- **The elder's cache refresher** ([`cache_refresh.rs`](../../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs)) ticks on the data cadence, rechecks the election each pass, refreshes the heavy lanes from the last published pane frame, fires due loop tasks (including the watch-lost backstop for a dead `rimz wait` watcher), wakes due scheduled messages, and emits [forge signals](#pr-state). A panic resets only its rollup cursor, and the next tick retries from cache.

### What presence data drives

Focus buys a fast tick for the work the user is watching. The producer folds `PaneFrame.viewed_panes` into the snapshot: git edit-sensitive facts for the viewed worktree and process metrics for the viewed pane run on the focused tier, while commit-shaped git facts and every background worktree and pane stay on their slower cadences. The other side effects gated on `viewed_panes` are listed in [multiplexers.md → Who is looking at what](../multiplexers.md#who-is-looking-at-what).

Client presence is classified against the reader's `now_ms` by `SidebarPresence::classify`: `Detached` when no human client remains, `Idle` once input has been quiet for `[sidebar] afk_after_secs` (15 minutes by default), and `Active` otherwise. Only tmux reports an input clock (`client_activity`), so an attached Zellij room stays `Active` until every client detaches. The tmux clock advances only on real input, including input over SSH, so remote tmux rooms honour the setting too.

tmux presence is sampled, not pushed. While a client is attached the producer re-samples every `PRESENCE_SAMPLE_TTL` (1 second), writing `client-presence-probe.stamp` before the attempt so event-driven fetches cannot burst probes. An unchanged sample leaves `snapshot.json` and its wake untouched, and a failed fallback focus probe carries the prior presence and viewed panes forward from `snapshot.json`.

## Fusion rules

Fusion is pure over pulled truth, the event store, and `now_ms`. It runs on the render thread on every paint that has an overlay or intent to apply; an unmodified pull goes straight to presentation.

**Supersession comes first.** The baseline is `panes_observed_at_ms.or(panes_produced_at_ms)`: an event no newer than the pane observation is skipped, because the pull already saw later truth. The exception is a `PaneClosed` naming a *carried* pane, which applies at any age: the frame held that pane on process evidence without seeing it, so nothing in the frame supersedes the close. The close also retires the carried-pane notice.

**Then the overlays apply in precedence order:**

1. `PaneClosed` deletes rows. If it names `focused_pane`, the register clears and the renderer keeps its last highlight.
2. `CommandChanged` overlays the command on panes that survived step 1 and were already admitted.
3. The newest `FocusChanged` sets or clears the session register, as in the taxonomy.
4. A requested focus intent lands last and outranks both the pulled register and `FocusChanged` for `FOCUS_ANCHOR_FRESH` (2.5 seconds), provided its pane still has an admitted row.

`PaneOpened` creates nothing; it asks the producer for a verified frame. Expired events drop by receiver-clock TTL, and any wrong verdict from a missed event or clock skew lasts only until the next producer pull.

## Focus intent

Every RimZ-initiated focus action, a user jump or an automatic repair, is a durable two-phase intent in `focus-anchor.json` ([`mux/focus_anchor.rs`](../../../crates/rimz/src/mux/focus_anchor.rs)). The file is workspace-wide and client-scoped. It carries a nonce, the session, the target pane, the origin, the exact pre-action client map, the viewport offset, and the frozen row order from the source frame.

The intent has two states:

- **`Requested`** is written, and wakes every renderer with `FocusIntent`, before the one-way mux focus command. It supplies a bounded presentation overlay, so peer tabs adopt the target, viewport offset, and frozen order while the destination is still hidden.
- **`Applied`** records that the mux accepted the command, without reseeding presentation. Native confirmation and fencing read this phase.

Native client observations resolve the intent, with the pre-action client map as the fence. [`observation_outcome`](../../../crates/rimz/src/mux/focus_anchor.rs) returns one of five verdicts:

| Verdict | Condition | Effect |
| --- | --- | --- |
| `Confirmed` | Every observed client views the target pane. | The action landed; retire the intent. |
| `Superseded` | The client map moved somewhere other than the target. | Something else took focus; drop the overlay. |
| `Invalidated` | The session changed, the target left the roster, no client remains, the client id set changed, or a requested anchor went stale before acceptance. | Detach, replacement, closure, or abandoned request; drop the overlay. |
| `Present` | A requested anchor is fresh, or an applied anchor is fresh with its pre-action map unchanged. | Keep presenting the target. |
| `Fence` | The pre-action map is unchanged past `FOCUS_ANCHOR_FRESH`. | Yield unknown instead of restoring stale evidence. |

A missed `FocusIntent` wakeup only delays the fold until the next event or pull, because the file stays authoritative until an observation resolves it. Automatic repairs append to the account-global `focus-repairs.log.jsonl` at command acceptance (`AcceptedUnconfirmed`) or failure, and again when observation confirms, supersedes, or invalidates the action.

## Cadences

Each row is a staleness budget. The constants and the reasoning behind each live beside the code they govern: sidebar cadences in [`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs), presence-stamp freshness in [`mux/mod.rs`](../../../crates/rimz/src/mux/mod.rs), the heartbeat TTL in [`wakeup/heartbeat.rs`](../../../crates/rimz/src/wakeup/heartbeat.rs), the focus-anchor window in [`mux/focus_anchor.rs`](../../../crates/rimz/src/mux/focus_anchor.rs), and the paint grid in [`config.rs`](../../../crates/rimz/src/config.rs).

| Lane | Cadence | Where staleness shows |
| --- | --- | --- |
| Pane frame | `SNAPSHOT_CACHE_TTL` (750 ms); `EVENT_PANE_TTL` (10 s) while the presence stamp is fresh or the published frame has no viewed panes | Pane open, close, and cwd or command regrouping with no exact event |
| Workspace projection | Every producer fold, content-identical writes suppressed; hidden consumers adopt after their coalescing clamp, and the 30 s skip backstop bounds quiet-room time-window flips | Shared enrichment in consumer tabs; a source mismatch falls back without delaying status |
| Unwatched consumer fold | At most `UNWATCHED_FOLD_CLAMP` (1 s) for identity-free nudges and `UNWATCHED_METRICS_FOLD_CLAMP` (3 s) for metrics-only publications; watched renderers and the producer fold at once | Off-screen store deltas and topology nudges |
| Zellij topology cache | `PRESENCE_STAMP_FRESH` (150 s); explicit freshness floors only for structural repair | Pre-producer pane listing and pushed client views |
| Presence stamp | `PRESENCE_STAMP_FRESH` (150 s) | Switches the pane frame between poll-mode and event-mode TTLs |
| Presence sample | Zellij on client-list events, tab switches, and keepalive; tmux every `PRESENCE_SAMPLE_TTL` (1 s) while clients are attached | Attach, detach, and viewed-pane gating; the tmux AFK badge clearing after input |
| Git diff stats | Focused: `DIFF_STATS_FOCUSED_LOCAL_TTL` (3 s) edit facts, `DIFF_STATS_FOCUSED_COMMIT_TTL` (10 s) commit facts. Background: `DIFF_STATS_TTL` (5 s) hot, `DIFF_STATS_IDLE_TTL` (60 s) idle | Worktree header churn, ahead and behind counts, landed markers, trunk sync |
| PR state | `PR_STATE_HOT_TTL` (20 s) hot, `PR_STATE_TTL` (5 min) idle; failures back off from `PR_STATE_RETRY_TTL` (30 s); a HEAD change bypasses the TTL | Worktree header PR and CI glyphs |
| Worktree roots | `WORKTREE_ROOTS_TTL` (60 s) | Grouping for checkouts added without a session boundary |
| Process metrics | `METRICS_FOCUSED_SAMPLE_TTL` (1 s) viewed, `METRICS_BACKGROUND_SAMPLE_TTL` (3 s) background | Child pids and per-row CPU, memory, IO, and process state |
| Spending service | `SPENDING_TTL` (15 s) | Provider dashboard, fleet totals, and the floor under live cockpit spend |
| Agent projection | Every producer data tick, paying metadata checks only while inputs are unchanged; `LOCAL_SESSION_DISCOVERY_BACKSTOP` (30 s) forces full validation | Hookless identity binding and launch admission |
| Accounts | Per provider: `ACCOUNTS_TTL` (10 min) on success, `ACCOUNTS_RETRY_TTL` (10 s) on failure, keeping last-known-good data | Provider dashboard login, plan, and account state |
| Account usage and credits | `OAUTH_USAGE_TTL` (5 min) for reads ([providers.md](../agents/providers.md#refresh-cadences)); `CREDITS_DISPLAY_MAX_AGE` (24 h) for display | Budget bars, the paid and extra usage row, the Codex reset marker |
| Live-session context | `SESSION_REFRESH_INTERVAL` (60 s) | Budget windows and session sidecars |
| Finished-cohort effort | `COHORT_SPEND_TTL` (60 s) | Finished group receipts |
| Team pipeline | Every refresher pass, no TTL; content-identical writes suppressed | Board stage, owner, stage-entry stamp, and run start/stop |
| Codex daemon reap | `CODEX_DAEMON_REAP_TTL` (30 s), stale after `CODEX_DAEMON_REAP_STALE` (90 s) | Codex daemon session reaping |
| Remote link stats | Stale after `LINK_STATS_STALE` (10 s), expired after `LINK_STATS_EXPIRE` (120 s) | Footer link badge for `rimz remote connect` rooms |
| Daemon-view repair | 30 s; a fresh frame with unchanged inputs skips the authoritative check ([rimzd.md](../rimzd.md#who-repairs-and-when)) | Managed `rimzd` pane recovery and configuration changes |
| Renderer heartbeat | Written every `HEARTBEAT_WRITE_INTERVAL` (2 s), trusted for `SIDEBAR_HEARTBEAT_TTL` (5 s) | Producer handoff after a renderer dies |

### The paint clock

Data cadence and paint cadence are separate clocks, which is why the sidebar stays responsive while the data layer runs slow.

`[theme.display] refresh_ms` sets the base render grid, `DEFAULT_REFRESH_MS` (100 ms) by default, clamped to 16 through 1000 ms. It rides `snapshot.theme.display`, so the renderer uses the default until the first fold and picks up config changes on later folds without reading config itself. Money rolls sample every `refresh_ms * CLICK_PHASES` (two base frames), matching the odometer's phase counter, and row animations sample on `BREATH_ANIMATION_FRAME` (120 ms), never faster than the base grid.

Input paints synchronously off the grid, an overlay event fuses and paints on arrival, and a burst of events coalesces to one paint per base frame. The data backstop is `rimz sidebar serve --tick-seconds` (default 1): `refresh_ms` changes paint cadence, never pull cadence.

A sidebar in an unviewed tab suspends animation and repaints only when its roster, status, or unread projection changes, at most once per `BACKGROUND_PAINT_MIN_INTERVAL` (1 second). Turn phase, gauges, process metrics, spend, git facts, and animation phase do not trigger a hidden paint. The serve loop also wakes when the order hold expires, to fire the fold that lets rows and groups settle back to live rank once the user goes idle.

## Failure modes

Each degradation has one owner, and none leaves a wrong verdict that outlives the next pull.

| Failure | Why it is survivable | Recovery |
| --- | --- | --- |
| Missed event | Events are latency hints. | The producer's next pull. |
| Dead producer | Consumers keep folding the rollup and the last published caches. | The next-eldest renderer takes over once the stale heartbeat ages out; pane presence waits for the handoff. |
| Clock skew | Event expiry uses receiver time, so no event lives forever. | A skewed sender can briefly mis-order an overlay; the verifying pull corrects it. |
| Corrupt or stale projection | Adoption is checked against the source tuple on every read. | The consumer runs the full fold in process, with no mux read and no git. |
| Panicking produce | The panic guard catches the unwind and discards the fold cursor. | One degraded outcome; the next cycle refolds cold. |
| Unreadable store or panes | The serve loop holds its last committed frame. | A sustained failure raises the health alert and, past `GIVE_UP_AFTER_DEGRADED`, respawns the worker ([sidebar.md → Degraded reads and give-up](./sidebar.md#degraded-reads-and-give-up)). |

Every accepted anomaly path writes a typed diagnostic before it falls back, holds, suppresses, or exits. Flicker, duplicate rows, or a phantom external group each map to a record in `diag.log.jsonl`; `rimz doctor` shows the recent tail, and [diagnostics.md](../diagnostics.md) names the taxonomy.

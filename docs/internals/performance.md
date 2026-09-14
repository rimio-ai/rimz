# Performance

> This page is the cost model that sits over the mechanisms: what the workload asks for, where each cost lands, which bound holds it down, how the bounds are guarded, and the rules a performance change follows. The mechanisms themselves live on their own pages: the data plane, election, and every cadence value in [state.md](./sidebar/state.md), the write and read paths in [store.md](./store.md), the backend roster in [multiplexers.md](./multiplexers.md), and the spend walker in [spending.md](./agents/spending.md). To measure a running fleet, use [profiling.md](./profiling.md); to read what RimZ recorded about its own faults, use [diagnostics.md](./diagnostics.md).

## The workload

RimZ watches a fleet of agents for one human, and that job has three traits every cost is judged against.

Tens of writers share one reader. Every agent hook appends to one workspace store, often at the same moment, while one sidebar per tab reads the whole room.

The room is idle most of the time. A fleet emits in bursts and then waits on a model or a human, so idle is the common case, and work added to the idle path is paid all day.

One tab is watched. A room runs one renderer per tab, and the human looks at one of them.

Those traits give three goals that pull against each other, so each lives in a different place:

| Goal | Where it lives | What it demands |
| --- | --- | --- |
| Low perceived latency | The render/input loop | Never block, drop a keystroke, or freeze the spinner. Showing data one tick stale is allowed. |
| No write contention | The store write path | Tens of concurrent commits cost no felt latency: the lock covers microsecond holds, and durability runs off the lock. |
| Near-zero idle work | Every lane | No polling spin, no per-frame fork, no directory rescan. |

One rule sits under all three: correctness lives in the store, never on the render thread. A performance change may leave the UI stale by a tick. It may never make the UI wrong, and it never trades a durability or CAS invariant for latency.

## Where the work runs

The design keeps the three goals apart by putting them on separate threads, separate processes, and separate clocks.

### The render thread

Each tab runs one `rimz sidebar serve` supervisor and worker pair ([`sidebar_pane::app::serve`](../../crates/rimz/src/sidebar_pane/app.rs)). The supervisor owns reload convergence and crash capture. The worker runs up to seven long-lived threads, and only the render/input loop faces the human.

The render loop spins the animation, applies input, fuses overlay events, and folds a snapshot the fetch worker finished. It blocks only in `recv` on its own wakeup socket, which receives store and pane wakeups as well as the event waker thread's terminal input and resize nudges. It never forks, never fsyncs, and never reads the pane roster. A mux action the loop starts (a jump, a width nudge) runs on a short-lived detached thread.

The other long-lived threads are the event waker, the fetch worker, the cache refresher, the observer writer, the tmux control-mode watch, and the transcript watch. What each owns, and which ones stop working when their renderer is not the producer, is the thread table in [state.md](./sidebar/state.md#renderers-the-producer-and-consumers).

### One producer, many consumers

The external reads a room needs (the pane roster, git, the forge, provider accounts) cost the same whether one tab asks or twenty do, so exactly one renderer per workspace pays them.

The eldest live renderer is the **producer**: it runs the reads and publishes the results as runtime caches. Every other renderer is a **consumer**: it folds those caches in process through [`PublishedSnapshotReader`](../../crates/rimz/src/sidebar/consumer.rs) and applies only what is local to it (its own pane exclusion, its own view, presence). A consumer forks nothing.

N tabs therefore cost one set of external reads plus N-1 in-process folds. This is the largest lever in the design: a 20-tab room costs about what a 2-tab room costs. The election mechanics live in [state.md](./sidebar/state.md#renderers-the-producer-and-consumers), and the account-global spend walker has its own election in [spending.md](./agents/spending.md#one-walk-per-namespace).

### Why the election is safe

The election needs no consensus protocol, because the lock is what actually bounds the cost and the eldest rule only avoids contending for it.

Every shared external read single-flights through [`disk::single_flight::coalesce`](../../crates/rimz/src/disk/single_flight.rs). Two renderers that both believe they are the producer still collapse to one pane-roster read per TTL window, so the room is correct with zero, one, or many self-declared producers.

The eldest rule saves the contention. Renderer ids are UUIDv7 and sort by birth, so a younger renderer skips production without touching the lock. A wrong pick costs one lock wait.

Liveness rides the heartbeat. When the producer dies, the next eldest takes the role within one `SIDEBAR_HEARTBEAT_TTL` (5 s) and produces on its next cycle. Pane discovery lags by those seconds, while agent status keeps flowing, because every renderer reads the store rollup itself and needs no producer for it.

### Two clocks

Smoothness and freshness run on separate clocks, so tuning one never taxes the other.

The frame clock is a fixed grid at `[theme.display] refresh_ms` (100 ms by default). A frame redraws from the cached snapshot and does no IO. The data clock is event-driven, with a one-second backstop tick (`rimz sidebar serve --tick-seconds`), and it folds new truth when a wakeup says there is some. The clamps, animation cadences, and hidden-tab paint rules are [state.md → The paint clock](./sidebar/state.md#the-paint-clock).

Durable truth and the pane roster are split the same way. Every fold reads the store rollup event-fresh, so a status flip repaints within one wakeup. The pane roster sits behind a TTL cache, so discovery stays cheap. A writer that knows about a change posts a typed wakeup, and polling only backs up a missed one.

### The three end-to-end budgets

Every row of the cost map rolls up into one of three paths, each named with its dominant term so a regression shows at review.

| Path | Budget | What happens |
| --- | --- | --- |
| Keypress to pixel | One in-process paint | Input applies synchronously and paints. Nothing on the path locks, forks, or reads the store. |
| Pane event to pixel | One fuse plus one off-grid paint | A typed overlay event (`PaneClosed`, `FocusChanged`) lands in the in-memory event store, re-fuses the held frame, and paints at once. The producer's next pull confirms it ([state.md](./sidebar/state.md#fusion-rules)). |
| Write to pixel | The frame grid | The commit holds the lock for microseconds, the wakeup datagram takes microseconds, the consumer folds the published frame in process (O(1) cached, O(delta) on a race), and the next dirty frame paints. The 100 ms grid dominates. |

## Principles

A performance change follows these rules. An earlier rule outranks a later one when they conflict.

1. **The render thread never blocks.** No fork, fsync, or roster read on the loop. Move the work to a worker and wake the loop when it finishes.
2. **The frame never drags the fetch.** Responsiveness is a redraw from the cached snapshot. A smoothness change shortens the frame interval and pays in-process paints; a freshness change belongs to the data layer. A frame that triggers a roster-plus-git fetch spends a fork to move a spinner.
3. **Push, then poll.** A writer that knows about a change posts a wakeup, and polling is the backstop. A data source that feeds the UI without being the store (a context sidecar, a transcript) gets a wakeup of its own.
4. **Fsync truth, rename caches.** Durable records fsync. Everything derived (rollup caches, the snapshot cache, diff stats, context sidecars) is written with `write_temp_then_rename_cache`, which renames atomically without fsync, because it rebuilds from truth on the next read. Neither form allows a torn read; the cache form gives up only surviving a power cut ([store.md → Write classes](./store.md#write-classes)).
5. **Single-flight, then coalesce.** One fetch runs at a time. A burst of requests collapses to one fetch, and a request racing an in-flight fetch leaves exactly one follow-up, never a queue.
6. **Pay the external read once per window.** A short TTL bounds pane discovery, git, and forge probes, and the last good result is reused. A failed probe backs off and keeps its last known good value. A degraded roster read backfills missing fields per pane instead of painting a corrupt frame.
7. **Take the cheapest correct read.** Catch-up is O(delta bytes) from the persisted fold base, never O(history). Skip work that cannot matter: unchanged inputs cost a stat, and a room with no agents skips the sidecar scans.
8. **One producer per workspace, one renderer per tab.** Production is capped, renderer count is not: every tab keeps its own renderer so no pane goes dark. Recovering from a stale producer belongs to the election, never to consumers.

## The cost map

Each lane's cost and the bound that holds it down. Reproducible figures come from `cargo xtask perf` ([Benchmarks](#benchmarks)); external IPC rows give ranges seen in production. Every cadence constant named below has its value and staleness budget in [state.md → Cadences](./sidebar/state.md#cadences); a value this page gives is one that page does not own.

### The render loop

| Operation | Cost | Bound |
| --- | --- | --- |
| Frame redraw | Sub-millisecond in process; 413 µs at 40 agents | The fixed `refresh_ms` grid; off-screen animation relaxes to the data backstop; a frame never starts a fetch |
| Overlay fuse | 75 µs owned, 100 µs with an active overlay, at 40 agents | Pure: no IO, no subprocess, no clock read |
| Jump to a pane | One mux client fork, tens to hundreds of ms | Detached thread; the focus intent it writes reaches the frame through the fold ([state.md](./sidebar/state.md#focus-intent)) |

### The fetch worker

| Operation | Cost | Bound |
| --- | --- | --- |
| Snapshot rollup | O(1) from `snapshots/latest.json`; O(delta bytes) when writes outran the cache | The `(generation, offset)` freshness stamp; a long-lived `RollupCursor` holds the parsed base, and the rotation carryover beneath it is parsed once per file identity per thread and shared, never copied, by every fold |
| Event-log fold | Warm cursor: one stat of the log and one of the carryover, plus the appended frames; the projection clones only the rows it keeps. Unchanged log: shared handles only | Guard tests `delta_fold_is_o_new_bytes` and `warm_fold_parses_unchanged_carryover_zero_times` |
| Consumer unchanged check | Metadata stamps on five inputs after an adoption; the full input set after a fallback | A matching stamp skips the fold; `CONSUMER_UNCHANGED_BACKSTOP_MS` forces one anyway ([state.md](./sidebar/state.md#the-skip-memo)) |
| Workspace projection | Producer: one enrichment and one serialize per changed fold. Consumer: one parse-cached clone plus its local projection | Adoption requires an exact source match; any mismatch falls back to a full local fold with no mux read ([state.md](./sidebar/state.md#adoption-and-fallback)) |
| Pane roster (producer) | Zellij: one topology read plus a `zellij pipe` nudge. tmux: one `list-panes` call | `SNAPSHOT_CACHE_TTL` while a client drives it, `EVENT_PANE_TTL` while the presence stamp is fresh; one attempt per data tick |
| Process metrics (producer) | A due sample reads one metrics record per process | Per-pane focused and background stamps in `metrics-sample.json`; the full process-table walk runs only on pane churn |
| Agent projection (producer) | Unchanged tick: metadata stamps only, no directory enumeration | One batched discovery call per kind and one wiring probe per data tick |

### The cache refresher (producer only)

| Operation | Cost | Bound |
| --- | --- | --- |
| Git diff stats per root | Unchanged HEAD: about 2 `git` forks. Fallback or ancestry change: about 6 | `DIFF_STATS_TTL` hot, `DIFF_STATS_IDLE_TTL` idle, per root; the sweep runs at most `MAX_PARALLEL_GIT` (8) roots at once |
| Worktree roots | One `git worktree list` fork plus one marker read per root | `WORKTREE_ROOTS_TTL`; no child scans in a directory room |
| PR state per repo | One `gh` or `tea` open-set call per due repo, plus a per-branch read only when a PR leaves the open set | `PR_STATE_HOT_TTL` and `PR_STATE_TTL`; failures back off and keep the last known map; closed links, and merged links with a settled CI verdict, are pinned ([state.md](./sidebar/state.md#pr-state)) |
| Provider accounts | Cold providers probe in waves of at most four account-then-version chains, each subprocess capped at 3 s | `ACCOUNTS_TTL` per provider; contention serves the stale cache instead of starting a second wave |
| Fleet spend walk | Within `SPENDING_TTL`: one read of the published aggregate, no transcript IO. A due walk: frontier stats plus O(appended bytes) per changed file | One warm walker per namespace owns the index; consumers never open `spending.json` ([spending.md](./agents/spending.md#one-walk-per-namespace)) |
| Finished-cohort effort | Transcript stat and parse for collapsible groups only; unchanged files reuse a process-local parse memo | `COHORT_SPEND_TTL`; the producer publishes `cohort-spend.json` and renderers only read it |
| Codex daemon reap | Zero unless Codex remote control is on or a daemon-hooked session is live; when due, one process scan plus one WebSocket handshake | `CODEX_DAEMON_REAP_TTL`; success and failure share the stamp |

### The store write path

The choreography and write classes are [store.md → The write path](./store.md#the-write-path).

| Operation | Cost | Bound |
| --- | --- | --- |
| Critical section | One event-log `write()`, zero fsyncs, one frame of at most 1 KiB | The lock covers truth mutation only; `store_fsync.rs` and `store_bytes.rs` pin both numbers |
| Durability | One group `fdatasync` per second per workspace (`LOG_SYNC_INTERVAL`) | Runs on the off-lock tail |
| Snapshot publish | One cache rename per second per workspace | Single-flighted; due after `PUBLISH_INTERVAL` (1 s) or `PUBLISH_BYTE_BUDGET` (64 KiB) of unpublished tail |
| Wakeup fanout | One heartbeat directory scan plus one datagram per live sidebar | N is live sidebars; the reads are page-cache hot and cost less than the fsync floor |
| Durable file write | Temp file plus two fsyncs (file, parent directory) | Cold paths only: trust grants, the workspace record, hook installs |

### Everything else

| Operation | Cost | Bound |
| --- | --- | --- |
| Sidebar observer | One O(rows) signature pass per committed fold, microseconds | Pure detection at the fold's commit point; the elder adds one cross-check pass per `OBSERVE_CROSSCHECK_TTL` (5 s) ([diagnostics.md](./diagnostics.md#the-frame-stream-observer)) |
| Tick meter | Six relaxed counter loads and two clock reads per metered tick | Healthy ticks do no file IO and spawn nothing |
| Sidebar heartbeat | Temp file plus atomic rename | Every `HEARTBEAT_WRITE_INTERVAL`, inside the liveness TTL |
| Merged read receipts | Unchanged generation: two metadata stamps and one shared in-memory merge | Keyed on the `generation.json` inode plus the directory stamp |
| Reload poll | One `stat()` of the durable reload record per sidebar per second | Executable hashing runs only after that record's metadata changes; the embedded presence-plugin digest resolves once, lazily ([sidebar.md](./sidebar/sidebar.md#build-promotion)) |
| Remote link probe | One JSON probe over the existing SSH ControlMaster every 2 s | Supervised remote attach only; `RIMZ_REMOTE_PROBE_MS=0` disables it |
| `loop watch` repaint | Catalog, pause, run-log, and terminal reads once per second | Workspace identity resolves once before the loop |

`WORKTREE_ROW_CAP` (6) caps the idle and process tail in the renderer, never the snapshot. Active, paused, blocked, finished, focused, and unread rows render past the cap, so jump targets and unread convergence stay visible. The observer's pass scales with the full roster and the render and selection walks with the visible set; both stay bounded by live pane count across the 20 to 100 agent target. If that bound loosens, the fix is row virtualization in the renderer, never hiding rows from the snapshot.

## Guarding it

Three layers hold the cost map in place: a live meter in every renderer, exact counters in CI, and benchmarks.

### The tick budget

The fetch worker and the cache refresher each meter their ticks against budgets in [`sidebar/meter.rs`](../../crates/rimz/src/sidebar/meter.rs):

| Budget | Limit |
| --- | --- |
| In-process wall time (`wall_ms - mux_wait_ms`) | One configured data tick |
| Mux subprocess wait | 5 s (`TICK_MUX_WAIT_BUDGET_MS`) |
| Fold bytes | 256 KiB (`TICK_FOLD_BYTES_BUDGET`) |
| Spawns | 32 (`TICK_SPAWN_BUDGET`) |

Byte and spawn limits are absolute because they describe the shape of a storm, not a cadence. Fork and byte counts are attributed to the loop that caused them, so the two metered loops do not charge each other.

Five consecutive over-budget ticks (`TICK_BUDGET_BREACH_TICKS`) write a `tick_budget_breach` diagnostic ([diagnostics.md](./diagnostics.md)) and emit one `warn!` through the observability bridge. The same streak window filters recovery, so one cheap tick inside a saturated episode does not flap the record. The meter only observes; the tick's work proceeds unchanged.

The budgets mirror the cost map. A change to the cost map revisits them in the same PR.

### CI counter gates

The deterministic gates pin exact integers. They live in `crates/rimz/tests/integration/performance/` and run in `cargo xtask ci`.

| Gate | What it pins |
| --- | --- |
| `store_fsync.rs` | The warm write path's fsync count |
| `store_bytes.rs` | A lifecycle frame at 1 KiB or less |
| `produce_budget.rs` | Zero subprocess spawns for a warm produce with fresh inputs, and for a produce over stale or missing heavy caches; a warm produce at fleet scale under 50 ms |
| `fold_incremental.rs` | Warm folds and produces read O(new bytes); a warm or unchanged fold parses zero carryover bytes, and a replaced carryover re-parses once |
| `consumer_enrichment.rs`, `enrichment_cadence.rs`, `spending_incremental.rs` | Unchanged-room consumer cost, per-enrichment TTL stamps, and O(delta) spend IO |

The `cargo xtask invariants` check `ensure_sidebar_library_boundaries` keeps store writers, the run-wake sender, and the broker out of the sidebar data plane's imports, so the producer cannot grow write-side machinery unnoticed.

### Benchmarks

`cargo xtask perf` runs the non-gating divan benches in `crates/rimz/benches/` over synthetic stores and pane frames, through the same entry points the sidebar uses. It launches no agents and spends no tokens. Wall-clock and allocation figures stay out of `ci`, so a busy runner never fails a build on timing.

Allocation per operation is the steadier regression signal; medians move with host and load. The baseline was captured on `xlab-term`, a loaded LXC container on an AMD Ryzen 9 9950X with 28 online CPUs and the `performance` governor.

| Bench | Median | Alloc/op | Captured |
| --- | ---: | ---: | --- |
| `fleet::produce_cold` 20 / 50 / 100 agents | 4.65 ms / 7.61 ms / 13.45 ms | 4.52 MB / 9.26 MB / 17.36 MB | 2026-07-05 |
| `fleet::produce_warm` 20 / 50 / 100 agents | 520 µs / 792 µs / 1.32 ms | 1.53 MB / 2.09 MB / 2.98 MB | 2026-07-05 |
| `hotpath::fuse` 40 agents | 99.9 µs | 80.9 KB | 2026-07-05 |
| `hotpath::fuse_owned_no_overlay` 40 agents | 74.7 µs | 819 B | not recorded |
| `hotpath::rollup_fold_warm` 40 agents | 129 µs | 637.1 KB | 2026-07-05 |
| `hotpath::rollup_fold_warm` 40 agents, 0 / 1,500 carryover rows, re-parsing carryover | 537 µs / 24.19 ms | 973.4 KB / 89.6 MB | 2026-09-14 |
| `hotpath::rollup_fold_warm` 40 agents, 0 / 1,500 carryover rows, shared carryover parse | 930 µs / 2.46 ms | 645.6 KB / 646.2 KB | 2026-09-14 |
| `hotpath::rollup_fold_unchanged` 40 agents, 0 / 1,500 carryover rows, cloning the held rollup | 591 µs / 5.47 ms | 120.4 KB / 15.96 MB | 2026-09-14 |
| `hotpath::rollup_fold_unchanged` 40 agents, 0 / 1,500 carryover rows, layered rollup | 674 µs / 2.88 ms | 712 B / 1.38 KB | 2026-09-14 |
| `hotpath::enrich_cached` 40 agents | 610 µs | 1.21 MB | 2026-07-05 |
| `hotpath::consumer_adopt_parse_cached` 40 agents | 116 µs | 125.2 KB | 2026-07-18 |
| `hotpath::consumer_adopt_changed_file` 40 agents | 329 µs | 226.9 KB | 2026-07-18 |
| `hotpath::render_fixed` 40 agents | 413 µs | 1.09 MB | 2026-07-05 |
| `hotpath::spending_walk_cold` 20k entries | 19.59 ms | 24.43 MB | 2026-07-15 |
| `hotpath::spending_walk_warm_no_change` 20k entries | 7.95 ms | 10.78 MB | 2026-07-15 |
| `hotpath::spending_live_scale_cold_hydrate` 6k files / 102k entries | 252.3 ms | 122.6 MB | 2026-07-16 |
| `hotpath::spending_live_scale_cold_discovery_inclusive` 6k files / 102k entries | 173.9 ms | 101.9 MB | 2026-07-18 |
| `hotpath::spending_live_scale_warm_global_refresh` 6k files / 102k entries | 93.5 ms | 50.25 MB | 2026-07-20 |
| `hotpath::spending_live_scale_warm_discovery_only` 6k files | 19.55 ms | 770.9 KB | 2026-07-18 |
| `hotpath::spending_live_scale_warm_discovery_inclusive` 6k files / 102k entries | 140.2 ms | 99.59 MB | 2026-07-18 |
| `hotpath::spending_live_scale_additional_workspace_scope` 6k files / 102k entries | 116.6 ms | 58.59 MB | 2026-07-20 |

## Overhead at fleet scale

RimZ is sized against a single agent, not against the fleet: watching a hundred agents should cost a small and nearly flat fraction of running one of them.

Cost attaches to three units, and only the cheapest grows with agent count.

Per workspace, RimZ pays once. The producer pays the roster and metrics on its fetch worker and git and accounts on its cache refresher, then publishes caches the other tabs fold in process. Every workspace that shares a state root and provider-discovery environment asks the same warm spend walker, so a producer handoff adds no second parsed cursor.

Per worktree, cost follows activity. The git input set scales with distinct group roots, not agents; a root drops to its idle TTL once its agents go quiet, and the sweep runs at most 8 roots at once. PR probes scale with origin repositories, each enumerating open PRs once when due. A hundred agents sharing a few checkouts pay for a few hot roots.

Per agent, cost is event-driven. An agent reports through a short-lived `rimz hooks feed` child that appends when something happens and exits. Nothing resident wraps a running agent, so an agent blocked on a question holds no RimZ process.

Two costs stay flat in agent count: one group `fdatasync` per second per workspace however many agents append, and one snapshot rename per second per workspace. Both scale with rooms. The hot runtime caches live in `$XDG_RUNTIME_DIR` (tmpfs), so their churn is memory traffic, not disk IO.

Totals across a fleet of 2 to 5 rooms:

| Resource | 20 agents | 50 agents | 100 agents | What sets it |
| --- | --- | --- | --- | --- |
| CPU, idle | ~0 | ~0 | ~0 | Loops block in `recv`; off-screen animation pauses |
| CPU, busy | <0.3 core | ~0.3 to 0.8 core | ~0.5 to 1.5 core | One producer per room, bursting toward the 8-root git cap, never on the render thread |
| RAM, resident | ~80 to 150 MiB | ~100 to 180 MiB | ~120 to 220 MiB | Renderers plus one spend walker, room-local rollups, prepared cell-pet grids |
| Durable write | ~1 to 3 KiB/s | ~2 to 4 KiB/s | ~2 to 5 KiB/s | Lifecycle frames pinned at 1 KiB or less, summed across rooms |
| fsync rate | ~rooms/s | ~rooms/s | ~rooms/s | One group `fdatasync` per second per workspace |
| State on disk | Tens of MiB | Tens of MiB | ~100s of MiB | Rotation-capped event log plus ~5 KiB of snapshot per agent, per workspace |
| Network | Weekly pricing fetch, 5-minute OAuth usage probes per metered provider, forge open-set probes per due repo | Same | Same | Pricing is fleet-shared and single-flighted; local datagrams carry the rest |

Against the agents it tracks, that overhead is a rounding error. One developer's week of Claude and Codex sessions produced 1.23 GiB of transcript JSONL (about 177 MiB a day), with each agent process resident at 250 to 340 MiB (Claude) or 50 to 65 MiB (Codex). RimZ watched the same fleet with tens of MiB of durable state, a resident set about the size of one agent process, one fsync a second per room, and one pricing refresh a week.

Remote render-stream bytes sit outside this budget: SSH carries whatever the visible full-screen TUIs repaint. Idle RimZ surfaces send close to nothing, and a busy agent TUI commonly sends tens of KB/s. `rimz pane bandwidth` reports each pane's producer write rate beside the room's SSH socket payload (`WIRE(ssh)`), which is usually far below the per-pane sum ([reference](../reference/cli/pane.md#bandwidth)).

## What's optimized

Each mechanism that holds a cost down, named once with its code home. Where another page owns the mechanism, this section says what it saves and links there.

### The frame is decoupled from the fetch

The fetch worker builds snapshots; the render loop blocks in `recv` and folds a result when the worker's `snapshot` wakeup arrives. The animation tick redraws from the cached snapshot and never fetches, so a missed push degrades to the backstop tick instead of a poll storm. [`animation_cadence`](../../crates/rimz/src/sidebar_pane/render/animation.rs) classifies motion: fast work stays on the base grid, row animations redraw on `BREATH_ANIMATION_FRAME`, and a dirty data fold pulls the next frame back to the base grid so freshness never waits on cosmetic motion.

### Only the watched tab animates

Each renderer knows its own pane and the latest focus view, so it can tell whether a client is looking at its tab. A watched tab keeps the normal grid. An unwatched or detached tab treats motion as idle, wakes on the data backstop, and clamps its folds to `UNWATCHED_FOLD_CLAMP` for store and topology nudges and `UNWATCHED_METRICS_FOLD_CLAMP` for metrics-only publications. Deferred requests merge the strongest freshness requirement and the earliest deadline, and an immediate fold absorbs any deferred one, so a hidden tab folds once per clamp. A dirty fold still paints once, keeping the hidden buffer current for the next tab switch. When ownership is unknown the renderer counts as watched, so tests, demos, and cold starts keep the responsive path. The hidden paint rules are [state.md → The paint clock](./sidebar/state.md#the-paint-clock).

### One producer per workspace

The producer builds the snapshot in process on its fetch worker ([`produce_workspace_snapshot`](../../crates/rimz/src/sidebar/produce/mod.rs)), with no `rimz sidebar snapshot` fork per tick. The worker's cadence stamp allows one attempt per data tick, and only against a stale published frame, so unrelated wakeups that see the same stale frame cannot multiply production.

The fold spine splits into renderer-independent [`enrich_workspace`](../../crates/rimz/src/sidebar/enrich.rs) and renderer-local `project_local`. The producer publishes the first half as `workspace-projection.json`, so a consumer's fold is a parse-cached clone plus its own local half. When nothing a consumer reads has changed, a stamp check skips the fold entirely. Adoption, fallback, and the skip memo are [state.md → One fetch cycle](./sidebar/state.md#one-fetch-cycle).

### Truth arrives by event, the frame by coalescing

Every `snapshot.json` publication broadcasts `PaneFramePublished` with the kind of input that changed (topology, metrics, or presence), so hidden consumers coalesce topology and metrics on their clamps while presence stays immediate. A publication from an older build that carries no kind decodes as topology, the conservative bound. The rollup is read event-fresh from `latest.json` on every fold, so a status change repaints within one wakeup while pane discovery stays coalesced.

Every writer that knows about a change pushes: store and sidecar writers post a `StoreDelta`, both backends' presence streams feed one projector that emits typed pane events, and the elder's transcript watcher refreshes context mid-turn for every adapter that declares transcript-tail context ([state.md → Push channels](./sidebar/state.md#push-channels)).

### No reader pays for history

The rollup persists a raw fold base with its `(generation, offset)` stamp, and catch-up seeks to the offset and folds only new frames ([`fold.rs`](../../crates/rimz/src/store/snapshot/fold.rs), [store.md → The read path](./store.md#the-read-path)). Runtime projection, resume outcomes, and smart-compact dedupe ride the same fold instead of rescanning `events.log.jsonl`. A `(path, mtime, len)` parse cache on `snapshot.json`, `latest.json`, and `rollup.json`, and a full-identity `(len, mtime, dev, ino)` one on `agents.carryover.json`, return `Arc<T>` handles, so an unchanged file costs neither a re-parse nor a deep clone ([`disk/parse_cache.rs`](../../crates/rimz/src/disk/parse_cache.rs)). The carryover's cached form is fold-ready (identities backfilled, rows key-sorted), and a cursor holds the merged rollup as that shared carryover beneath the live rows that win the merge, so a fold thread retains one copy of history however many times it folds. A corrupt carryover is never cached: every fold reports it until the file is replaced.

The spend walk is incremental in three layers: the walker's directory index stats only active frontiers and reconciles fully every 15 minutes, the disk cache keeps a cursor per file so a grown file parses only its appended suffix, and the one elected walker holds the only parsed cache, so workspace requests borrow instead of cloning. Aggregation uses hash collections for session uniqueness and keeps deterministic order only in the published maps. The cache layout is [spending.md → The incremental cache](./agents/spending.md#the-incremental-cache).

### Per-enrichment cadences

Every display figure is display-only by invariant ([DESIGN.md](../../DESIGN.md#triage-at-a-glance)), so each enrichment runs on its own cadence behind stamps stored in the cache file it already writes. Because the stamps live in shared files, every process agrees on freshness across a producer handoff. Account probes carry one stamp per provider, so a failure retries alone. Process sampling keeps per-pane focused and background stamps, independent of the roster clock. Git probes take activity-tiered TTLs whose hotness comes from store activity, with no filesystem watching, and in-process `.git` ref reads skip the ancestry forks when the cached HEAD and trunk pair and the clean verdict are unchanged. The values are [state.md → Cadences](./sidebar/state.md#cadences).

### The write path holds no fsync under the lock

The workspace lock covers durable truth only, so a hold lasts microseconds. The off-lock tail issues one group `fdatasync` per second, which makes every writer's appends in that interval durable at once. Length-plus-CRC32 framing turns a lost suffix into deterministic corruption that repair truncates. Every fsync goes through `disk/atomic.rs`, which the `cargo xtask invariants` check `ensure_store_durability` enforces for `sync_all` and `sync_data` calls. The contract is [store.md → Write classes](./store.md#write-classes).

### Helper processes stay cheap

The build id is computed once per process from the linker build identity in the first 1 MiB of the image, and the whole image is hashed only when that identity is missing; `main` warms it off-thread at startup ([`build_id.rs`](../../crates/rimz/src/build_id.rs)). Self-spawns resolve through one helper, `rimz_exe`, which honors `RIMZ_BIN` and repairs Linux `current_exe()` paths ending in ` (deleted)` after an atomic reinstall. The Zellij presence plugin hashes the stable manifest fields before projecting pane fields, so title-only `PaneUpdate` storms skip projection ([`policy.rs`](../../crates/rimz-presence-zellij/src/policy.rs)). Zellij rooms start with `disable_session_metadata true` by default, which stops the server rewriting `session-metadata.kdl` and running command-discovery `ps` for the room.

### Codex context stays warm

Codex app-server enrichment connects to the warmest server available, starting with a per-session broker in the `rimzd` tab that holds one handshaked `codex app-server`, and falls back to a cold spawn ([adapter_codex.md → App-server enrichment](./agents/adapter_codex.md#app-server-enrichment)). The elder's transcript watcher debounces file events to one flush per 300 ms for the workspace and refreshes each changed session in that flush. Both are latency hints over the unconditional producer tick: a broker or watcher that never starts costs nothing.

## Anti-patterns

Each of these has cost RimZ a real regression. They are grouped by the principle they break, because the principle is what transfers.

**Cosmetics must not drag data (principle 2).** A fixed refetch every 500 ms to keep a working agent's cost figures moving forks a subprocess per frame to move a spinner, and on Zellij its roster read resets unrelated panes' cursor blink. Context-sidecar pushes cover the same need for free.

**Recovery belongs to the election (principle 8).** When every consumer produces on producer staleness, the single-flight losers time out into their own uncached produces: an N-way fork storm on exactly the tick the room is already degraded. Per-tab roster reads pin the mux server with N round trips. Capping renderers instead of production blacks out every tab but one.

**Shared state has one owner (principle 5).** Per-producer spend walkers duplicate the account-global parsed cursor in every promoted workspace and keep it after demotion. Consumers that derive workspace spend from the global cursor make every renderer parse `spending.json`. Per-thread heartbeat scans make each thread decode every renderer heartbeat. The fix in each case is one elected or process-local owner that the others read.

**Unchanged inputs cost nothing (principle 7).** A consumer that refolds every second reloads config, clones published JSON, enriches, and renders with every input unchanged. A pane-frame publication with no input kind makes each background metrics sample force every hidden renderer through a full fold. Scanning a provider-global session tree (`~/.codex/sessions`) once per worktree traverses the same directories many times in one fold.

**Every retry is bounded (principle 6).** A deterministic `gh` or `tea` failure retried on the hot TTL, with no command deadline and the cached map erased each time, turns a forge outage into a permanent per-worktree fork loop. An auth probe whose result is recorded under a different scope than the one requested reads as a new login on the next fold and spawns another helper. Both need backoff that keeps the last known good value.

**Cheap-looking work multiplies by fleet size.** Deriving the merge base separately for every group root costs about 240 `git` execs on a 20-worktree fixture. A warning emitted once per fold per renderer, with Sentry's `attach_stacktrace` on, makes every consumer symbolize a backtrace before the rate limiter can drop it (about 200 MiB per fold against 18 MiB clean). Streaming serde output straight into a socket turns each JSON token into a syscall.

**An atomic reinstall leaves deleted inodes behind.** A long-lived renderer's `current_exe()` resolves to `rimz (deleted)`, so a naive self-spawn fails `execve`, and a supervisor parent holds the deleted inode until session end while its children orphan. `rimz_exe` repairs the suffix, supervisor-owned reload convergence re-execs parents onto the new binary, and supervisor-side reaping clears the orphans.

## Deferred and rejected

Wins identified but not taken, because each changes a contract or crosses a backend-parity boundary, ranked by expected payoff:

1. **tmux `list-panes` over the held control client.** The elder's `tmux -C` client already writes commands and parses reply blocks, so sending `list-panes` over it would remove the producer's per-window fork and connect. The saving is 10 to 30 ms on the already-cheap backend, and the poll must still cover a dead watcher.
2. **Delta-bearing wakeup datagrams.** `StoreDelta` could carry the appended frames, so a warm consumer folds from the datagram with no file IO. The warm cursor fold is already one stat plus a page-cache-hot read, so the win is microseconds against a second delivery path for state that must never become truth. Build it only above sustained hundreds of events per second.
3. **A faster workspace-projection codec.** JSON keeps the projection inspectable and tolerant of mixed builds. If a live consumer profile shows projection parsing dominating, replace only this disposable file's codec with postcard behind the same identity, publication, and fallback mechanics.

Two costs belong upstream, and RimZ only bounds them:

- **Zellij topology freshness depends on the presence plugin.** A missing, denied, or wedged plugin leaves a Zellij room holding its last good frame while `rimz doctor` names the failing precondition. A watchdog belongs in the presence channel, not in a CLI fallback.
- **Zellij compact-bar and scrollback footprint.** A 100-pane room carries one compact-bar wasm instance per tab and large server-side scrollback. RimZ can recommend configuration and avoid adding pressure; the costs belong to Zellij's plugin and scrollback model.

Evaluated and rejected, recorded so the next pass does not reopen them:

- **Lock-free `O_APPEND` event appends.** Recovery assumes the lock makes the log single-writer, so only the trailing frame can tear. Concurrent appenders' dirty pages can write back out of order and leave a zeroed frame in the middle, which rebuild correctly treats as a hard error. Making that safe needs per-frame magic for resync, all to shave a lock hold that is already microseconds.
- **Binary snapshot format.** The parse cache removes re-parsing on delta storms and the `RollupCursor` holds the parsed base in memory, so a binary checkpoint would speed up a parse that does not happen. JSON keeps `rimz sidebar snapshot --json` inspectable.
- **Caching the wakeup heartbeat scan.** N is live sidebars, the reads are page-cache hot, and the fanout runs after the lock releases, below the write's fsync floor. A cache would have to live across processes and re-validate exactly what the TTL and re-stat already check.
- **A resident writer daemon behind `rimz hooks feed`.** The agent's hook contract spawns a process per event regardless, so a daemon would remove only RimZ's startup: a page-cache-hot exec, workspace resolution, store open, and a microsecond append. That is single-digit milliseconds per event, roughly 0.1 to 0.3 core at 30 chatty agents and under a core at 100, off every human-facing budget. The spawned child also carries two contracts a daemon would have to re-earn: its synchronous append lands truth inside the agent's hook-execution guarantee, and its stdout is the decision channel. A daemon path would need an ack protocol, a request and response surface, and a direct-append fallback for a dead daemon, which is the current design. Revisit only on measured hook-spawn CPU at fleet scale.

## Making a performance change

1. **Name the thread the cost lands on.** If it is the render/input loop, the change is wrong until the work moves off it.
2. **Prefer a push to a shorter poll.** A tighter poll burns cycles while the room is idle; a wakeup costs nothing until something changes.
3. **Decide durability explicitly.** Durable state goes through `write_temp_then_rename`, which fsyncs; a cache rebuilt on the next tick goes through `write_temp_then_rename_cache`.
4. **Keep single-flight.** A new fetch trigger goes through `FetchDispatcher` in [`app/fetch.rs`](../../crates/rimz/src/sidebar_pane/app/fetch.rs) (`request`, `request_or_defer`, `defer_until`), never a bare send or spawn, so it merges with the rest.
5. **Measure the idle case.** A change that speeds the busy path by adding idle work is usually a net loss.
6. **Prove it.** `cargo xtask ci` stays green, the tick budgets and the cost map move with the change, and `cargo xtask perf` refreshes the figures when the change touches a measured path.

### Perceived response time

Responsiveness is what the user feels: a frame that paints now with slightly stale data beats a fresh frame that arrives late. Reach for these levers in order.

1. **Acknowledge before you finish, unless one source of truth is worth the wait.** Where the local intent is the truth, paint the instant input lands: a browse pick and the help overlay paint synchronously in `LoopState::apply_input`. The jump is the deliberate exception. It changes no local selection; its detached thread writes a focus intent that every renderer, this one included, presents through the fold ([state.md → Focus intent](./sidebar/state.md#focus-intent)), so a stale frame cannot roll the highlight back.
2. **Animate on the wall clock, never on IO.** The spinner advances from `wall_clock_phase` on a fixed interval whether or not a fetch is in flight. Motion that stalls while data is slow reads as a hang.
3. **Separate smoothness from freshness.** When a "make it feel snappier" request lands, first decide whether it is a frame-rate problem or a data-latency problem. They are fixed in different layers, and conflating them turns a cosmetic tweak into a CPU regression.
4. **Keep session-wide effects off the frame path.** A redraw writes only to the sidebar's own pane and is safe at any rate. A mux action touches the whole session and can reset an unrelated pane's cursor blink, so it belongs on the event-driven data layer.

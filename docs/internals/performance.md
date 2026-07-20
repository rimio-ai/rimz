# Performance

> The performance model that sits over the mechanisms. This doc says where the milliseconds go, what a whole fleet costs RimZ, which bounds hold each cost down, and the rules the next change follows. The mechanisms live elsewhere: sidebar data flow and cadences in [state.md](./sidebar/state.md), presence and ranking in [sidebar.md](./sidebar/sidebar.md), the durability contract in [store.md](./store.md#write-classes), the backend pane roster in [multiplexers.md](./multiplexers.md). To measure a running fleet, use [profiling.md](./profiling.md); to read what RimZ recorded about its own faults, [diagnostics.md](./diagnostics.md).

## The workload

RimZ watches a fleet for one human, and that job has a characteristic shape:

- **Tens of writers, one reader.** Every agent hook appends to one workspace store concurrently, while one sidebar reads the whole room.
- **The room is idle most of the time.** A fleet emits in bursts and then waits on a model or a human, so idle is the common case every cost is measured against.
- **One pane is watched; the rest are not.** A room has one renderer per tab, and a human looks at one tab.

Good performance here is three goals in three different places, and they pull against each other:

- **The read path wants low perceived latency.** The sidebar's render/input loop is the one thread a human feels. It may show data that is stale by a tick. It may never block, drop a keystroke, or freeze the spinner.
- **The write path wants no contention.** Tens of hooks commit at once, and a concurrent append must cost no felt latency. The critical section is a queue of microsecond holds, with durability riding off the lock.
- **Idle wants near-zero work.** No polling spin, no per-frame fork, no directory rescan. An optimization that speeds the busy path by adding idle work is usually a net loss.

One guardrail sits under all three: **correctness lives in the store, never on the render thread.** A performance change may leave the UI stale by a tick. It may never make it wrong, and it never trades a durability or CAS invariant for latency.

## The shape of the solution

The design keeps the three goals from fighting by putting them on independent threads and independent clocks.

### Processes and threads

A room runs one `rimz sidebar serve` supervisor and worker pair per tab ([`sidebar_pane::app::serve`](../../crates/rimz/src/sidebar_pane/app.rs)). The supervisor owns reload convergence and crash capture; the worker does the fold, render, and heartbeat work, spreading it across up to seven long-lived threads. Only the first faces the human.

| Thread | Work | Faces the human |
| --- | --- | --- |
| render/input loop | Spinner, input, overlay fuse, folding a finished fetch. Blocks only in `recv`. | Yes |
| event waker | Receives wakeup datagrams and pokes the loop | No |
| fetch worker ([`app::fetch`](../../crates/rimz/src/sidebar_pane/app/fetch.rs)) | Cadence, election, the store fold, and on the producer the pane produce | No |
| cache refresher ([`app::cache_refresh`](../../crates/rimz/src/sidebar_pane/app/cache_refresh.rs)) | Producer-only heavy lanes: git, accounts, spend, diff-stats | No |
| observer writer ([`observe::writer`](../../crates/rimz/src/sidebar/observe/writer.rs)) | Anomaly records and elder cross-checks | No |
| tmux watch | tmux control-mode topology nudges, producer only | No |
| transcript watch | Codex rollout file watch, producer only | No |

The render loop never forks, never fsyncs, and never reads the pane roster synchronously. Everything expensive is somebody else's thread.

### One producer, many consumers

The external reads a room needs (the pane roster, git, forge, provider accounts) cost the same whether one tab asks or twenty do. So exactly one renderer per workspace pays them.

The eldest live renderer is elected **producer** and pays them all, publishing the result as runtime caches. Every younger renderer is a **consumer**: it folds those caches in process through one [`PublishedSnapshotReader`](../../crates/rimz/src/sidebar/consumer.rs) and applies only what is local to it (pane exclusion, own-view, presence). A consumer forks nothing.

That turns N tabs into one external-read cost plus N-1 in-process folds, which is the single largest lever in the whole design: it is what keeps a 20-tab room costing about what a 2-tab room costs. The election mechanics, the per-thread ownership table, and the separately elected spending service live in [state.md](./sidebar/state.md#renderers-the-producer-and-consumers).

### Why the election is safe

The eldest-UUIDv7 rule looks like a distributed-consensus problem and is not one, because **the flock is the real election and the eldest rule is only an optimization.**

- **The flock decides.** Every shared external read single-flights through [`store::single_flight::coalesce`](../../crates/rimz/src/store/single_flight.rs). Two renderers that both believe they are the producer still collapse to one pane-roster read per TTL window. The system is correct with zero, one, or many self-declared producers.
- **The eldest rule saves the contention.** Sorting by birth lets a younger renderer skip production before ever touching the lock, so the common case never contends at all. A wrong pick costs nothing, because the flock bounds it.
- **The heartbeat TTL carries liveness.** On a producer death the next eldest flips within one `SIDEBAR_HEARTBEAT_TTL` (5s) and produces on its next cycle. Pane discovery lags a few seconds; agent status keeps flowing the entire time through the rollup fast lane, which needs no producer.

### Two clocks

Freshness and smoothness ride separate clocks, so tuning one never taxes the other.

The **frame clock** is a fixed timestep at `[theme.display] refresh_ms` (100ms default). It redraws from the cached snapshot: no IO, no clock read, no allocation of consequence. The **data clock** is event-driven, one tick per second by default, and folds new truth when it arrives.

Durable truth and the pane roster also split. The store rollup is read event-fresh on every fold, so a status flip repaints within one wakeup. The backend pane roster sits behind a TTL cache and is folded under it, so pane discovery stays cheap. A change a writer knows about posts a typed wakeup; polling is only the missed-wakeup backstop.

### The three end-to-end budgets

Everything in the cost map rolls up into these, each with its dominant term named so a regression shows at review.

- **Keypress to pixel: one in-process paint.** Input applies synchronously and marks the frame dirty. Nothing on this path locks, forks, or reads the store.
- **Pane event to pixel: one fuse plus one off-grid paint.** A typed overlay event (`PaneClosed`, `FocusChanged`) appends to the in-memory event store, re-fuses the held frame, and paints immediately. The producer's next pull squares truth behind it ([state.md](./sidebar/state.md#fusion-rules)).
- **Write to pixel: bounded by the frame bucket.** Commit under the flock (µs), wakeup datagram (µs), consumer folds the published frame in process (O(1) cached, O(delta) on a race), next dirty frame paints. The 100ms bucket dominates; everything upstream is orders of magnitude smaller.

## Principles

The rules a performance change follows, ordered. An earlier rule outranks a later one when they conflict.

1. **The render thread never blocks.** No fork, no fsync, no synchronous roster read on the loop. Offload to a worker and wake the loop when it finishes.
2. **Decouple the frame from the fetch.** Responsiveness is a redraw from the *cached* snapshot, not a data refresh. A smoothness change shortens the frame interval and costs in-process paints; a freshness change rides the data layer. Never invert it: letting a frame drag a roster-plus-git fetch behind it spends a fork to move a spinner.
3. **Push over poll.** A writer that knows about a change posts a wakeup. Polling is the backstop, never the primary channel. A datasource that feeds the UI without being the store (a statusline or transcript sidecar) gets a wakeup of its own.
4. **Cache the disposable; fsync the durable.** Crash durability is for truth alone. Everything derived (rollup caches, the snapshot cache, diff-stats, context sidecars) renames atomically without fsync through `write_temp_then_rename_cache`, because it rebuilds from truth on the next read. A torn read is impossible either way, so the only thing traded is surviving a power cut, which buys nothing for a rebuildable file.
5. **Single-flight, then coalesce.** One outstanding fetch at a time. A burst of deltas collapses to one fetch, and a delta racing an in-flight fetch defers exactly one follow-up, never a queue.
6. **Pay the roster read once per window.** Bound pane discovery and git probes with a short TTL and reuse the last good result. A degraded read backfills missing fields per pane rather than flashing a corrupt frame.
7. **Cheapest correct read.** Snapshot catch-up is O(delta bytes) from the persisted fold base, O(active events + items) at worst, never O(history). Skip work that cannot matter: an idle room with no agents skips the sidecar scans entirely.
8. **One producer per workspace, one renderer per tab.** Production is what gets capped, not renderer count. Every tab keeps its own renderer so no pane goes dark, and staleness recovery belongs to the election, never to consumers.

## The cost map

What each lane costs and what holds it down. Reproducible figures come from `cargo xtask perf`; external IPC rows name measured production ranges. Cadence constants live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs), and the staleness each lane may show is budgeted in [state.md](./sidebar/state.md#cadences).

### The render loop

| Operation | Cost | Bound |
| --- | --- | --- |
| frame redraw | sub-millisecond, in process; 413µs at 40 agents | fixed `refresh_ms` grid; off-screen animation relaxes to the backstop; never forks a fetch |
| overlay fuse | 75µs owned, 100µs with an active overlay at 40 agents | pure: no IO, no subprocess, no clock read |
| jump to a pane | one mux-client fork, tens to hundreds of ms | detached thread, fire and forget; no roster re-validation |

### The fetch worker

| Operation | Cost | Bound |
| --- | --- | --- |
| snapshot rollup | O(1) from `snapshots/latest.json`; O(delta bytes) when writes outran the cache | the `(generation, offset)` freshness stamp; a long-lived `RollupCursor` holds the parsed base |
| event-log fold | warm cursor: one stat plus the appended frames | perf guard `delta_fold_is_o_new_bytes` |
| consumer unchanged check | metadata stamps on five inputs after adoption; the conservative full set after a fallback | a matching stamp skips the fold; a 30s backstop forces one anyway |
| workspace projection | producer: one enrichment plus one serialize per changed fold. consumer: one parse-cached clone plus local pane/view/presence | adoption needs exact schema, session, log extent, section stamps, and config generation; any mismatch falls back to a full local fold |
| pane roster (producer) | Zellij: one JSON topology read plus a `zellij pipe` nudge. tmux: one `list-panes` IPC call | two TTL windows, responsive while a client watches and stretched while presence is fresh; one attempt per data tick |
| process metrics (producer) | focused panes ~1s, background ~3s; a due sample reads one metrics record per process | per-pane stamps in `metrics-sample.json`; the full process-table walk runs only on pane churn |
| agent projection (producer) | unchanged tick: metadata stamps only, zero directory enumeration | one batched discovery call per kind, one wiring probe per data tick |

### The cache refresher (producer only)

| Operation | Cost | Bound |
| --- | --- | --- |
| git diff-stats per root | unchanged HEAD: ~2 `git` forks per root. fallback or ancestry change: ~6 | `DIFF_STATS_TTL` 5s hot / `DIFF_STATS_IDLE_TTL` 60s idle, keyed per root; the whole sweep capped at `MAX_PARALLEL_GIT` (8) |
| worktree roots | 1 `git worktree list` fork plus one marker read per root | `WORKTREE_ROOTS_TTL` 60s; zero child scans in a directory room |
| PR state per repo | one `gh`/`tea` open-set call per due repo, plus a per-branch read only on an open-to-terminal transition | `PR_STATE_HOT_TTL` 20s / `PR_STATE_TTL` 5min; failure backoff retains the last known good map; terminal `merged` states are pinned |
| provider accounts | cold providers run in waves of at most four account-then-version chains, each subprocess capped at 3s | `ACCOUNTS_TTL` 10min per provider; contention serves stale cache rather than duplicating the wave |
| fleet spend walk | within `SPENDING_TTL` (15s): one read of the published aggregate, zero transcript IO. due walk: frontier stats plus O(appended bytes) per changed file | one warm walker per state/discovery namespace owns the index; consumers never open `spending.json` |
| Codex daemon reap | zero without a daemon-hooked session; when due, one process scan plus one WebSocket handshake | `CODEX_DAEMON_REAP_TTL` 30s; success and failure share the stamp |

### The store write path

| Operation | Cost | Bound |
| --- | --- | --- |
| critical section | one event-log `write()`, zero fsyncs, one frame under 1 KiB | the flock covers truth mutation only; `store_fsync.rs` and `store_bytes.rs` pin both numbers |
| durability | one group `fdatasync`, debounced to at most 1/s per workspace | runs on the off-lock write tail |
| snapshot publish | one debounced cache rename per second per workspace | single-flighted; 1/s or 64 KiB of unpublished tail |
| wakeup fanout | heartbeat-dir scan plus datagram sends | N is live sidebars, page-cache-hot, below the write's fsync floor |
| durable file write | temp plus 2 fsyncs (file, parent dir) | cold paths only: trust grants, workspace identity, hook installs |

### Everything else

| Operation | Cost | Bound |
| --- | --- | --- |
| sidebar observer | one O(rows) signature pass per committed fold, µs-scale | inline pure detection at the fold chokepoint; the elder adds one cross-check pass per `OBSERVE_CROSSCHECK_TTL` (5s) ([diagnostics.md](./diagnostics.md#the-frame-stream-observer)) |
| tick meter | two relaxed counter loads plus one `Instant` per tick | healthy ticks do no file IO and spawn nothing |
| sidebar heartbeat | temp plus atomic rename | `HEARTBEAT_WRITE_INTERVAL` 2s, below the 5s liveness TTL |
| merged read receipts | unchanged generation: two metadata stamps and one shared in-memory merge | keyed on the atomically published `generation.json` inode plus the directory stamp |
| reload poll | one durable-record `stat()` per sidebar per second | executable hashing runs only after that record's metadata changes, and the embedded presence-wasm digest resolves lazily once ([sidebar.md](./sidebar/sidebar.md#build-promotion)) |
| remote link probe | one JSON probe over the existing SSH ControlMaster every ~2s | supervised remote attach only; `RIMZ_REMOTE_PROBE_MS=0` disables it |
| `loop watch` repaint | catalog, pause, run-log, and terminal reads once per second | workspace identity resolves once before the loop |

`WORKTREE_ROW_CAP` (6) caps the idle and process tail in the renderer, not the snapshot. Active, paused, blocked, finished, focused, and unread rows stay renderable past it so jump targets and unread convergence remain visible. The observer's O(rows) pass scales with the full roster while the render and selection walks scale with the visible set; both stay bounded by live pane count in the 20 to 100 agent target. If that bound loosens, the fix is row virtualization in the renderer, not hiding rows from snapshots.

## Guarding it

Three layers hold the cost map in place: a live meter, exact CI counters, and benchmarks.

### The tick budget

The sidebar meters producer ticks against budgets declared in [`sidebar/meter.rs`](../../crates/rimz/src/sidebar/meter.rs): in-process wall time (`wall_ms - mux_wait_ms`) for one configured data tick, 5s of mux subprocess wait, 256 KiB of fold bytes, and 32 spawns. Byte and spawn bounds stay absolute because they track storm shape rather than cadence.

Five consecutive over-budget ticks write a `tick_budget_breach` diagnostic ([diagnostics.md](./diagnostics.md)) and emit one `warn!` through the observability bridge. The same streak window filters recovery, so one cheap tick inside a saturated episode does not flap the record. The meter observes only; producer work proceeds unchanged.

The budgets mirror the cost map and move with it. A cost-map change revisits the tick budgets in the same PR.

### CI counter gates

The deterministic gates own exact integers and run in `cargo xtask ci`:

- `store_fsync.rs` pins the warm write path's fsync count.
- `store_bytes.rs` pins a lifecycle frame below 1 KiB.
- `produce_budget.rs` pins zero subprocess spawns for a warm produce with fresh inputs.
- The incremental fold guards pin O(new bytes).
- `cargo xtask invariants` bans store-writer, run-wake, and broker imports under `crates/rimz/src/sidebar/`, so the in-process producer cannot grow write-side machinery unnoticed.

### Benchmarks

`cargo xtask perf` runs the non-gating divan tier over synthetic stores and pane frames, through the same public entry points the sidebar uses. It launches no agents and spends no tokens. Wall-clock and allocation figures stay out of `ci` so a busy runner never fails a build on timing.

Allocation per op is the steadier regression signal; medians are host- and load-sensitive. The baseline below was captured on `xlab-term`, a loaded LXC on an AMD Ryzen 9 9950X with 28 online CPUs, governor `performance`. The 20k spending rows date from July 15, 2026, the live-scale rows from July 16, and the discovery and projection rows from July 18; everything else from July 5.

| Bench | Median | Alloc/op |
| --- | ---: | ---: |
| `fleet::produce_cold` 20 / 50 / 100 agents | 4.65ms / 7.61ms / 13.45ms | 4.52MB / 9.26MB / 17.36MB |
| `fleet::produce_warm` 20 / 50 / 100 agents | 520µs / 792µs / 1.32ms | 1.53MB / 2.09MB / 2.98MB |
| `hotpath::fuse` 40 agents | 99.9µs | 80.9KB |
| `hotpath::fuse_owned_no_overlay` 40 agents | 74.7µs | 819B |
| `hotpath::rollup_fold_warm` 40 agents | 129µs | 637.1KB |
| `hotpath::enrich_cached` 40 agents | 610µs | 1.21MB |
| `hotpath::consumer_adopt_parse_cached` 40 agents | 116µs | 125.2KB |
| `hotpath::consumer_adopt_changed_file` 40 agents | 329µs | 226.9KB |
| `hotpath::render_fixed` 40 agents | 413µs | 1.09MB |
| `hotpath::spending_walk_cold` 20k entries | 19.59ms | 24.43MB |
| `hotpath::spending_walk_warm_no_change` 20k entries | 7.95ms | 10.78MB |
| `hotpath::spending_live_scale_cold_hydrate` 6k files / 102k entries | 252.3ms | 122.6MB |
| `hotpath::spending_live_scale_cold_discovery_inclusive` 6k files / 102k entries | 173.9ms | 101.9MB |
| `hotpath::spending_live_scale_warm_global_refresh` 6k files / 102k entries | 119.1ms | 52.54MB |
| `hotpath::spending_live_scale_warm_discovery_only` 6k files | 19.55ms | 770.9KB |
| `hotpath::spending_live_scale_warm_discovery_inclusive` 6k files / 102k entries | 140.2ms | 99.59MB |
| `hotpath::spending_live_scale_additional_workspace_scope` 6k files / 102k entries | 161.0ms | 59.43MB |

## Overhead at fleet scale

RimZ is the layer that watches a fleet, so its footprint is sized against a single agent rather than against the fleet. Observing a hundred agents should stay a small, near-flat fraction of running one of them, and that ratio is the target as much as any absolute number.

The cost attaches to three units, and only the cheapest grows with agent count:

- **Per workspace, once.** Each room elects one producer, which pays the roster and metrics on its fetch worker and git and accounts on its cache refresher, then publishes caches the other tabs fold in process. Every workspace sharing a persistent cache and provider-discovery environment requests spend from the same warm walker, so promotion and demotion add no parsed cursor copy.
- **Per worktree, activity-tiered.** The git input set scales with distinct group roots, not agents. A root drops to the 60s idle TTL the moment its agents go quiet, and the sweep is capped at 8 fork chains. PR probes scale with origin repos: each due repo enumerates open PRs once. A hundred agents sharing a few checkouts pay a few hot roots.
- **Per agent, cheap and event-driven.** An agent reports through a short-lived `rimz hooks feed` child that appends when something happens and exits. Nothing resident wraps a running agent, and even an agent blocked on a question holds no RimZ process.

Two costs stay flat in agent count by design: one group `fdatasync` per second per workspace however many agents append, and one debounced snapshot rename per second per workspace. Both scale with rooms, not agents. The hot runtime caches land in `$XDG_RUNTIME_DIR` (tmpfs), so their churn is memory traffic rather than disk IO.

The table totals across a 2 to 5 room fleet:

| Resource | 20 agents | 50 agents | 100 agents | What sets it |
| --- | --- | --- | --- | --- |
| CPU, idle | ~0 | ~0 | ~0 | loops block in `recv`; off-screen animation pauses |
| CPU, busy | <0.3 core | ~0.3-0.8 core | ~0.5-1.5 core | one producer per room, bursting toward the 8-fork cap, never on the render thread |
| RAM, resident | ~80-150 MiB | ~100-180 MiB | ~120-220 MiB | renderers plus one spending owner, room-local rollups, prepared cell-pet grids |
| Durable write | ~1-3 KiB/s | ~2-4 KiB/s | ~2-5 KiB/s | lifecycle frames counter-pinned below 1 KiB, summed across rooms |
| fsync rate | ~rooms/s | ~rooms/s | ~rooms/s | one group `fdatasync` per second per workspace |
| State on disk | tens of MiB | tens of MiB | ~100s of MiB | rotation-capped event log plus ~5 KiB/agent snapshot, per workspace |
| Network | weekly pricing fetch; 5min OAuth usage probes per metered provider; forge open-set probes per due repo | same | same | pricing is fleet-shared and single-flighted; local datagrams carry the rest |

Set against the agents it tracks, the overhead reads as a rounding error, and the gap widens as the fleet grows. One developer's week of Claude and Codex sessions came to 1.23 GiB of transcript JSONL, about 177 MiB a day, with agent processes resident at 250-340 MiB (Claude) and 50-65 MiB (Codex) each. RimZ watched the same fleet for tens of MiB of durable state, a resident set on the order of one of those agent processes, one fsync a second per room, and one pricing refresh a week.

Remote render-stream bytes sit outside this budget: SSH carries whatever visible full-screen TUIs repaint. Idle RimZ surfaces are near zero; busy agent TUIs commonly run tens of KB/s. `rimz remote bandwidth` attributes that write rate per pane.

## What's optimized

The mechanisms in place, each named once with its code home. The chronology lives in git.

### Decouple the frame from the fetch

The snapshot fetch runs on the background worker; the loop blocks in `recv` and folds results via a `snapshot` wakeup. The animation tick redraws the spinner from the cached snapshot and never fetches, so a missed push degrades to the backstop tick rather than a poll storm. Animation cadence is classified in [`render::animation_cadence`](../../crates/rimz/src/sidebar_pane/render/mod.rs): fast work stays on the base grid, row animations redraw at `BREATH_ANIMATION_FRAME` (120ms), and a dirty data fold clamps back to the base budget so freshness never waits on cosmetic motion.

### Animate only what a human watches

Each renderer knows its own pane and the latest same-tab focus view, so the loop can tell whether an attached client is viewing its tab. A watched tab keeps the normal grid. An unwatched or detached tab treats cosmetic motion as idle and wakes on the data backstop, clamping store and topology work to `UNWATCHED_FOLD_CLAMP` (1s) and metrics-only publications to `UNWATCHED_METRICS_FOLD_CLAMP` (3s). Deferred requests merge the strongest freshness requirement and the earliest deadline, and every immediate fold absorbs pending work, so a hidden tab folds once instead of leaving a deferred echo. Dirty data folds still paint once, keeping the off-screen buffer current for the next tab switch. Unknown ownership reads as watched, so demos, tests, and cold starts keep the responsive path.

### One producer per workspace

The producer runs the produce **in process** on its fetch worker ([`produce_snapshot`](../../crates/rimz/src/sidebar/produce/mod.rs)) rather than forking `rimz sidebar snapshot` per tick. A monotonic attempt stamp owned by that worker requires both a stale durable topology frame and no attempt within the data tick, so unrelated wakeups observing the same stale frame cannot multiply production; forced refreshes bypass and advance the stamp.

The enrichment spine splits into renderer-independent [`enrich_workspace`](../../crates/rimz/src/sidebar/enrich.rs) and renderer-local `project_local`. The producer publishes the workspace result as `workspace-projection.json`; each consumer applies only its own pane exclusion, own-view, and presence. Section stamps plus the exact rollup extent and config generation make adoption a live-truth verdict, and legacy, corrupt, or stale files fall back to the full fold.

Consumers shed idle folds with a source-sensitive input stamp ([`consumer.rs`](../../crates/rimz/src/sidebar/consumer.rs)). A successful adoption records a slim five-input set; a fallback restores the conservative full set. When the stamp matches, the worker posts an unchanged outcome that clears single-flight state without replacing the snapshot or dirtying the frame. Producer cycles, force folds, hard refreshes, and failed folds invalidate the memo, and a 30s backstop forces a real fold regardless.

### Event-fresh truth over a coalesced frame

Every atomic `snapshot.json` write broadcasts `PaneFramePublished` carrying the input kind that changed, so hidden consumers can coalesce topology at 1s and metrics at 3s while presence stays immediate. Legacy unit publications decode as topology, so a mixed-build room keeps the conservative bound. The rollup is read event-fresh from `latest.json` on every dispatched fold, so a status change repaints within one wakeup while pane discovery stays coalesced.

Every writer that knows about a change pushes: store and sidecar writers post a `StoreDelta`; accepted Zellij topology snapshots are diffed into typed pane overlays; the tmux control-mode subscriber emits the same taxonomy directly; the elder's transcript watcher covers Codex's mid-turn gap ([state.md](./sidebar/state.md#push-channels)).

### Incremental everything

No reader pays O(history). The rollup persists a raw fold base plus its `(generation, offset)` stamp, and catch-up seeks to the offset and folds only new frames ([`fold.rs`](../../crates/rimz/src/store/snapshot/fold.rs)). Runtime projection, resume outcomes, and smart-compact dedupe ride the same fold instead of rescanning `events.log.jsonl`. A `(path, mtime, len)` parse cache on `snapshot.json`, `latest.json`, and `rollup.json` returns `Arc<T>` handles, so unchanged files skip both re-parse and cache-hit deep clone ([`parse_cache.rs`](../../crates/rimz/src/store/parse_cache.rs)).

The fleet spend walk is incremental in three layers: the walker-owned directory index stats only active frontiers and reconciles fully every 15 minutes; the disk cache stores `(mtime, len, cursor)` per file so a grown file parses only its appended suffix; and the elected walker holds the sole parsed cache plus a generation-keyed slice of dedup winner locations, so serialized workspace requests borrow rather than clone. Rows older than 8 days compact into per-day rollups inside the same stream.

### Per-enrichment cadences

Every display figure is display-only by invariant ([DESIGN.md](../../DESIGN.md#triage-at-a-glance)), so each enrichment gets its own cadence behind process-safe stamps in the cache file it already writes. Account probes carry one stamp per provider so a failure retries alone. Process sampling rides per-pane hot/idle stamps decoupled from the pane-read clock. Git probes take activity-tiered TTLs whose hotness comes from store agent activity rather than filesystem watching, and in-process `.git` ref reads skip ancestry forks when the cached HEAD/trunk pair and clean verdict are unchanged. Stamps live in cache files, so every process agrees on freshness across failover.

### The zero-fsync write path

The critical section covers durable truth only, and the flock hold drops to microseconds. The off-lock tail issues a group fdatasync debounced to at most 1/s, so one writer per interval makes the whole fleet's appends durable. Length-plus-CRC32 framing makes a lost suffix deterministic corruption that the next write tail self-heals. The full contract, including the CI grep that funnels every fsync through `store/atomic.rs`, lives in [store.md](./store.md#write-classes).

### Keep helper processes cheap

Short-lived helpers reuse a cached build id keyed by executable path, mtime, and length instead of hashing the running binary on every spawn ([`build_id.rs`](../../crates/rimz/src/build_id.rs)). Self-spawns resolve through one helper that honors `RIMZ_BIN` and repairs Linux `current_exe()` paths ending in ` (deleted)` after an atomic reinstall. The Zellij presence plugin hashes raw stable manifest fields before allocating projected pane fields, so title-only `PaneUpdate` storms skip projection entirely ([`policy.rs`](../../crates/rimz-presence-zellij/src/policy.rs)). Zellij rooms start with `disable_session_metadata true`, which stops the server rewriting `session-metadata.kdl` and running command-discovery `ps` on the room's behalf.

### Warm context, no cold spawns

Codex enrichment skips the cold-spawn handshake: a per-session broker holds one warm, already-handshaked `codex app-server` and serves it over a unix socket ([codex.md](./agents/codex.md#context-and-transcript)). It runs as a pane in the `rimzd` daemon tab, respawns a dead child once, and always leaves a cold-spawn fallback, so enrichment never depends on it. The elder's transcript watcher closes Codex's mid-turn freshness gap, debounced to one flush per 300ms per session. Both are latency hints over the unconditional producer tick: a watcher that never starts costs nothing.

### Lessons from removed anti-patterns

Each of these looked reasonable when it landed. They are grouped by the rule they violate, because the rule is what transfers.

**Do not let cosmetics drag data (principle 2).** `ACTIVE_REFRESH` refetched every 500ms purely to keep a spinning agent's cost figures current: a subprocess fork per frame to move a spinner, and on Zellij its periodic roster read even reset unrelated panes' cursor blink. Context-sidecar pushes cover the same need for free.

**Recovery belongs to the election, not to consumers (principle 8).** Letting every consumer produce on producer staleness meant the single-flight loser wait (~200ms) sat under the Zellij roster floor, so every loser timed out into its own uncached produce: an N-way fork storm on exactly the tick the room was already degraded. Separately, per-tab roster reads pinned the mux server with N round-trips, and the first fix over-corrected into a per-workspace renderer-exit election that blacked out every tab but one.

**One owner for shared state (principle 5).** Per-producer spending walkers duplicated the account-global parsed cursor in every promoted workspace and retained it after demotion. Consumers deriving workspace spend from the global cursor made every renderer parse and aggregate `spending.json`. Per-thread heartbeat scans made five threads each decode every renderer heartbeat. Each is now one elected or process-local owner that others read.

**Unchanged inputs must cost nothing (principle 7).** Per-second consumer refolds reloaded config, cloned published JSON, enriched, and rendered even with every input file unchanged. Unit pane-frame publications made topology, metrics, and presence indistinguishable, so each background metrics sample forced every hidden renderer through a full fold. Consumer checkout rediscovery statted every group path and reopened `.git` for diff and PR projection independently. Per-worktree scans of the provider-global `~/.codex/sessions` tree made every Codex workspace traverse the same directories in one fold. Warm spending rematerialization re-collected, sorted, and deduplicated the same active paths for every request after discovery was already warm.

**Bound every retry (principle 6).** A deterministic `gh`/`tea` failure retried on the hot TTL with no command deadline and erased the cached open-PR map each time, turning a forge outage into a permanent per-worktree fork loop. A Pi no-credentials probe completed under the kind-wide default after preflight requested an exact sub-provider scope, so the next fold read the mismatch as a new login and spawned another helper despite the settled-auth TTL. Both now back off while retaining last-known-good.

**Cheap-looking work multiplies by fleet size.** A cold producer snapshot forked `git` per group root end to end (about 240 execs on a 20-worktree fixture) because every root re-derived its merge base. A deterministic projection outcome warned once per fold per renderer, and Sentry's `attach_stacktrace` made every consumer symbolize a backtrace before the limiter could drop it: about 200 MiB per fold against 18 MiB clean. Serde fragments streamed directly into spending-service sockets turned each JSON token into its own syscall.

**An atomic reinstall leaves deleted inodes behind.** Long-lived renderers resolved `current_exe()` to `rimz (deleted)`, so self-spawn helpers failed `execve` and supervisor parents held the deleted inode until session end while orphaning stray children. The `rimz_exe` resolver repairs the suffix, supervisor-owned reload convergence re-execs parents onto the new binary, and supervisor-side reaping clears the orphans.

## Deferred and rejected

Real wins identified but not taken, because each changes a contract or crosses a backend-parity boundary. Ranked by expected payoff.

1. **Zellij topology freshness depends on the presence plugin.** With the slow JSON pane-list path gone from product runtime, a missing, denied, or wedged plugin leaves Zellij rooms holding the last good frame while `rimz doctor` names the failing precondition. Deeper watchdog behavior belongs in the presence channel rather than a CLI fallback.
2. **Zellij compact-bar and scrollback footprint.** A 100-pane room carries one compact-bar wasm instance per tab and large server-side scrollback RSS. RimZ can recommend configuration and avoid adding pressure, but those costs belong to Zellij's plugin and scrollback model.
3. **tmux `list-panes` over the held control client.** The elder's `tmux -C` client already writes its stdin and parses reply blocks, so issuing `list-panes` over it would remove the producer's per-window fork and connect. The saving is 10 to 30ms on the already-cheap backend, and the poll must still back a dead watcher.
4. **Delta-bearing wakeup datagrams.** `StoreDelta` could carry the appended frames, letting a warm consumer fold from the datagram with zero file IO. With the warm cursor fold already one stat plus a page-cache-hot read, the residual win is microseconds against a second delivery path for state that must never become truth. Build only above sustained hundreds of events per second.
5. **A faster workspace-projection codec.** JSON keeps the projection inspectable and mixed-build tolerant. If a live consumer profile makes projection parsing dominant, replace only this disposable file's codec with postcard behind the same identity, publication, and fallback mechanics.

Evaluated and **rejected**, recorded so the next pass does not re-litigate them:

- **Lock-free `O_APPEND` event appends.** Recovery assumes the flock makes the log single-writer-at-a-time, so only the trailing frame can tear. Concurrent appenders' dirty pages can write back out of order, leaving a zeroed *middle* frame, which rebuild correctly treats as a hard error. Making it safe needs per-frame magic for resync, all to shave a lock tail that is already a queue of microsecond holds.
- **Binary snapshot format.** The parse cache removed the re-parse on delta storms and the `RollupCursor` holds the parsed base in memory, so a binary checkpoint would accelerate a parse that no longer happens. JSON keeps `rimz sidebar snapshot --json` inspectable.
- **Caching the wakeup heartbeat scan.** N is live sidebars, the reads are page-cache-hot, and the fanout runs after the lock releases, below the write's fsync floor. A cache would have to live cross-process and re-validate exactly what the TTL and re-stat already validate.
- **A resident writer daemon behind `rimz hooks feed`.** The agent's hook contract spawns a process per event regardless, so a daemon removes only rimz's startup: a page-cache-hot exec plus workspace resolve, store open, and a µs-scale append. That is single-digit milliseconds each, roughly 0.1 to 0.3 core at 30 chatty agents and under a core at 100, off every human-facing budget. Meanwhile the spawned child carries two contracts a daemon would have to re-earn: its synchronous direct append is what lands truth under the agent's hook-execution guarantee, and its stdout is the decision channel. A daemon path needs an ack protocol, a request/response surface, and a direct-append fallback for a dead daemon, which is the current design. Revisit only on measured hook-spawn CPU at fleet scale.

## Making a performance change

1. **Name the thread the cost lands on.** If it is the render/input loop, the change is wrong until the work moves off it.
2. **Prefer a push over a shorter poll.** A tighter poll burns cycles in the idle case; a wakeup costs nothing until something changes.
3. **Decide durability explicitly.** Durable state fsyncs; a next-tick-rebuilt cache does not. Do not reach for `write_temp_then_rename` on a disposable file.
4. **Keep single-flight.** A new fetch trigger routes through `request_fetch`, never a bare spawn, so it coalesces with the rest.
5. **Measure the idle case too.** An optimization that speeds the busy path by adding idle work is usually a loss, because the fleet is idle most of the time.
6. **Prove it with the gate.** `cargo xtask ci` stays green, and `cargo xtask perf` refreshes the figures when the change moves a measured path.

### Perceived response time

Responsiveness is what the user feels, not what the profiler measures: a frame that paints now with slightly stale data beats a fresh frame that arrives late. The levers, in the order to reach for them.

1. **Acknowledge before you finish, except where one source of truth is worth the wait.** Where the local intent *is* the truth, redraw the instant the input lands: a browse pick and the help overlay paint synchronously in `app::apply_input`. The jump is the deliberate counter-example. It fires the focus command on a detached thread, changes no local state, and lets the derived selection catch up on the next fold, so a stale frame can never roll the highlight back.
2. **Animate on wall clock, never on IO.** The spinner advances from `wall_clock_phase` on a fixed interval whether or not a fetch is in flight. Motion that stalls when data is slow reads as hung even when nothing is wrong.
3. **Tune smoothness and freshness on separate dials.** When a "make it feel snappier" request lands, ask first whether it is a frame-rate problem or a data-latency problem. They are fixed in different layers, and conflating them is how a cosmetic tweak becomes a CPU regression.
4. **Keep session-wide effects off the frame path.** A redraw writes only to the sidebar's own pane and is safe at any rate. A mux *action* touches the whole session and can reset an unrelated pane's cursor blink, so it belongs on the event-driven data layer.

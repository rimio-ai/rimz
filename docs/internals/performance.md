# Performance

> This page is the cost model that sits over the mechanisms: what the workload asks for, where each cost lands, which bound holds it down, how the bounds are guarded, and the rules a performance change follows. The mechanisms themselves live on their own pages: the data plane and every cadence value in [state.md](./sidebar/state.md), the write and read paths in [store.md](./store.md), the backend roster in [multiplexers.md](./multiplexers.md), and the spend walker in [spending.md](./agents/spending.md). To measure a running fleet, use [profiling.md](./profiling.md); to read what RimZ recorded about its own faults, use [diagnostics.md](./diagnostics.md).

## The workload

RimZ watches a fleet of agents for one human, and that job has three traits every cost is judged against.

Tens of writers share one reader. Every agent hook appends to one workspace store, often at the same moment, while one sidebar per tab reads the whole room.

The room is idle most of the time. A fleet emits in bursts and then waits on a model or a human, so idle is the common case, and work added to the idle path is paid all day.

One tab is watched. A room runs one renderer per tab, and the human looks at one of them.

Those traits give three goals that pull against each other, so each lives in a different place:

| Goal | Where it lives | What it demands |
| --- | --- | --- |
| Low perceived latency | The render/input loop | Never block, drop a keystroke, or freeze the spinner. Showing data one tick stale is allowed. |
| No write contention | The store write path | Hooks share a blocking ingress lock before exit and defer lifecycle work to an elected drainer. Workspace-lock waiters use short retries for brief releases and backoff for long holds. |
| Near-zero idle work | Every lane | No polling spin, no per-frame fork, no directory rescan. |

One rule sits under all three: correctness lives in the store, never on the render thread. A performance change may leave the UI stale by a tick. It may never make the UI wrong, and it never trades a durability or CAS invariant for latency.

## Where the work runs

The design keeps the three goals apart by putting them on separate threads, separate processes, and separate clocks.

### The render thread

Each tab's sidebar pane runs one `rimz sidebar serve` supervisor, and one `rimz sidebar host` per mux session paints every pane of the room ([`sidebar_pane::host`](../../crates/rimz/src/sidebar_pane/host.rs)). The supervisor keeps the pane's terminal modes and input, and owns reload convergence. The host runs one render/input loop per pane, each on its own thread, over one set of data-plane threads, so a room pays the per-process cost of a renderer once rather than once per tab. Each pane holds the shared fold's agent-scaled collections, group vector, and rows by reference. `project_local` copies local metadata; a group edit detaches the vector and only changed row collections. Ordinary snapshot cloning has no term in agent, group, or history count. The host is the only painter; an unavailable host costs a supervisor notice and bounded retry, never another data plane ([state.md → Host unavailable](./sidebar/state.md#host-unavailable)).

A render loop spins the animation, applies input, fuses overlay events, and projects a fold the fetch worker finished. It blocks only in `recv_timeout` on its bounded channel, which receives host-routed room events, fetch hints, and supervisor input; the timeout is its frame clock. It never forks, never fsyncs, and never reads the pane roster. A mux action the loop starts (a jump, a width nudge) runs on a short-lived detached thread.

The data-plane threads, one set per host, are the fetch worker, the cache refresher, the observer writer, the tmux control-mode watch, and the transcript watch; the terminal event reader lives in each pane's supervisor. Their responsibilities are in the thread table in [state.md](./sidebar/state.md#the-room-data-plane).

### One host, many panes

The external reads a room needs (the pane roster, git, the forge, provider accounts) cost the same whether one tab asks or twenty do, so one room host pays them for the fleet.

The room host runs one data plane for every pane, retaining panes, agent projection, and metrics history rather than reading its own publications back each cycle. Passive CLI readers fold the published caches through [`PublishedSnapshotReader`](../../crates/rimz/src/sidebar/consumer.rs) without probing mux, git, or providers. Default inspection and recipient resolution invoke private mux reads; inspection also refreshes heavy lanes, but neither publishes the host-owned inputs.

N tabs therefore cost one set of external reads, and each room's host folds once for all its panes. A wake reaches every pane and each pane asks the shared fetch worker for a fold; a fold answers every request whose cause a pane observed before the fold started, and both the worker and the panes drop a request the last fold answered ([`sidebar_pane::app::fetch`](../../crates/rimz/src/sidebar_pane/app/fetch.rs) `Coverage`). A burst whose wakes all land within one fold costs at most two folds for any tab count; a wake that lands later is indistinguishable from a new change and costs one more. This is the largest lever in the design: in external reads, a 20-tab room costs about what a 2-tab room costs. The shared data plane lives in [state.md](./sidebar/state.md#the-room-data-plane), and the account-global spend walker has its own election in [spending.md](./agents/spending.md#one-walk-per-namespace).

Heavy-lane external reads still single-flight through [`disk::single_flight::coalesce`](../../crates/rimz/src/disk/single_flight.rs), so a CLI refresh racing the host shares that lane's TTL window. Pane-frame production has no cross-process election: the host alone publishes, while CLI callers compute privately.

### Two clocks

Smoothness and freshness run on separate clocks, so tuning one never taxes the other.

The base frame grid is set by `[theme.display] refresh_ms` (100 ms by default); a wheel glide briefly uses a finer grid. A frame redraws from the cached snapshot and does no IO. The data clock is event-driven, with a one-second backstop tick (`rimz sidebar serve --tick-seconds`), and it folds new truth when a wakeup says there is some. The clamps, animation cadences, and hidden-tab paint rules are [state.md → The paint clock](./sidebar/state.md#the-paint-clock).

Durable truth and the pane roster are split the same way. Every fold reads the store rollup event-fresh, so a status flip repaints within one wakeup. The pane roster sits behind a TTL cache, so discovery stays cheap. A writer that knows about a change posts a typed wakeup, and polling only backs up a missed one.

### The three end-to-end budgets

Every row of the cost map rolls up into one of three paths, each named with its dominant term so a regression shows at review.

| Path | Budget | What happens |
| --- | --- | --- |
| Keypress to pixel | One in-process paint; at most eight for a wheel glide | Input applies synchronously and paints; consecutive queued wheel ticks share the first paint, followed by time-based eased frames for at most 100 ms ([state.md](./sidebar/state.md#the-paint-clock)). Nothing on the path locks, forks, or reads the store. |
| Pane event to pixel | One fuse plus one off-grid paint | A typed overlay event (`PaneClosed`, `FocusChanged`) lands in the in-memory event store, re-fuses the held frame, and paints at once. The producer's next pull confirms it ([state.md](./sidebar/state.md#fusion-rules)). |
| Write to pixel | The frame grid after apply | Applied events wake the panes, which request a shared fold and paint on the next dirty frame. Hook ingress must first pass through the drainer; the 100 ms grid does not bound that queue. |

## Principles

A performance change follows these rules. An earlier rule outranks a later one when they conflict.

1. **The render thread never blocks.** No fork, fsync, or roster read on the loop. Move the work to a worker and wake the loop when it finishes.
2. **The frame never drags the fetch.** Responsiveness is a redraw from the cached snapshot. A smoothness change shortens the frame interval and pays in-process paints; a freshness change belongs to the data layer. A frame that triggers a roster-plus-git fetch spends a fork to move a spinner.
3. **Push, then poll.** A writer that knows about a change posts a wakeup, and polling is the backstop. A data source that feeds the UI without being the store (a context sidecar, a transcript) gets a wakeup of its own.
4. **Fsync truth, rename caches.** Durable records fsync. Everything derived (rollup caches, the snapshot cache, diff stats, context sidecars) is written with `write_temp_then_rename_cache`, which renames atomically without fsync, because it rebuilds from truth on the next read. Neither form allows a torn read; the cache form gives up only surviving a power cut ([store.md → Write classes](./store.md#write-classes)).
5. **Single-flight, then coalesce.** One fetch runs at a time. A burst of requests collapses to one fetch, and a request racing an in-flight fetch leaves exactly one follow-up, never a queue.
6. **Pay the external read once per window.** A short TTL bounds pane discovery, git, and forge probes, and the last good result is reused. A failed probe backs off and keeps its last known good value. A degraded roster read backfills missing fields per pane instead of painting a corrupt frame.
7. **Take the cheapest correct read.** Catch-up is O(delta bytes) from the thread's held fold or the persisted fold base, never O(history). Skip work that cannot matter: unchanged inputs cost a stat, and a room with no agents skips the sidecar scans.
8. **One host per room, one render loop per pane.** The data plane and room event socket are shared, while every pane keeps its loop, heartbeat, and bounded wake channel. Supervisors replace a lost host and reattach without an ownership election.

## The cost map

Each lane's cost and the bound that holds it down. Reproducible figures come from `cargo xtask perf` ([Benchmarks](#benchmarks)); external IPC rows give ranges seen in production. Every cadence constant named below has its value and staleness budget in [state.md → Cadences](./sidebar/state.md#cadences); a value this page gives is one that page does not own.

### The render loop

| Operation | Cost | Bound |
| --- | --- | --- |
| Frame redraw | Sub-millisecond in process; 413 µs at 40 agents | The base `refresh_ms` grid, briefly finer for a wheel glide; off-screen animation relaxes to the data backstop; a frame never starts a fetch |
| Overlay fuse | 75 µs with no active overlay, at 40 agents | Pure: no IO, no subprocess, no clock read |
| Jump to a pane | One mux client fork, tens to hundreds of ms | Detached thread; the focus intent it writes reaches the frame through the fold ([state.md](./sidebar/state.md#focus-intent)) |
| Tmux graphics capabilities | One room-wide client/process/version sample and one pane-option listing | Ten-second plane cadence; a missing pane or unavailable listing uses a targeted permission query; escalation remains per pane |
| Zellij width observation | One topology parse and pane geometry table per changed file stamp; a metadata check and lookup thereafter | Shared per plane; freshness and each pane's observation floor stay live ([multiplexers.md](./multiplexers.md#width)) |

### The fetch worker

| Operation | Cost | Bound |
| --- | --- | --- |
| Snapshot rollup | O(1) from `cache/snapshots/latest.json`; O(delta bytes) when writes outran the cache | The `(generation, offset)` freshness stamp; a `RollupCursor` holds the parsed base (one held per thread for store reads, one owned by the sidebar fetch worker), and the rotation carryover beneath it is parsed once per file identity per thread, from the stamped trimmed copy when current, and shared, never copied, by every fold |
| Event-log fold | Warm cursor: one stat of the log and one of the carryover, plus the appended frames; the projection clones only the rows it keeps. Unchanged log: shared handles only | Guard tests `delta_fold_is_o_new_bytes` and `warm_fold_parses_unchanged_carryover_zero_times` |
| Workspace fold | One enrichment; pane projections share agent-scaled collections, the group vector, and rows, with no seed serialization | The host retains its last delivered fold for new-attachment seeds; only changed collections detach |
| Fold observations | One focus-anchor read and one body-filter read per real cycle, shared by every attachment | Pane-local application keeps selection, confirmation and read intent; live nonce comparison guards retirement ([state.md](./sidebar/state.md#focus-intent)) |
| Pane roster (producer) | Zellij: one topology read plus a `zellij pipe` nudge. tmux: one `list-panes` call | `SNAPSHOT_CACHE_TTL` while a client drives it, `EVENT_PANE_TTL` while the presence stamp is fresh; one attempt per data tick |
| Process metrics (producer) | A due sample reads one metrics record per process | Per-pane focused and background stamps in `lanes/metrics-sample.json`; the full process-table walk runs only on pane churn |
| Agent projection (producer) | Unchanged tick: metadata stamps only, no directory enumeration | One batched discovery call per kind and one wiring probe per data tick |

### The host cache refresher

| Operation | Cost | Bound |
| --- | --- | --- |
| Live-session context | Stat-gated local reads, no context-helper subprocess | Every refresher pass; rich session metadata refreshes at hook boundaries or on explicit refresh, account windows through the per-login usage lane |
| Git diff stats per root | Unchanged HEAD: about 2 `git` forks. Fallback or ancestry change: about 6 | `DIFF_STATS_TTL` hot, `DIFF_STATS_IDLE_TTL` idle, per root; the sweep runs at most `MAX_PARALLEL_GIT` (8) roots at once |
| Worktree roots | One `git worktree list` fork plus one marker read per root | `WORKTREE_ROOTS_TTL`; no child scans in a directory room |
| Owned-worktree condition scopes and timers | One repo check and one `git worktree list` per root on a cache miss; ownership markers read fresh each call | Shared process-local rows invalidate on the common `worktrees` directory's stat identity or after 60 s, bounding external worktree moves; CLI discovery stays uncached |
| PR target assembly | In-process HEAD reads, including checkout subdirectories; unresolved ref formats fall back to `git rev-parse --abbrev-ref HEAD`; one successful `git remote get-url origin` per common Git directory per config version | Host-retained memo keyed by the common config's stat identity; missing origins are cached, transient failures retry. Detached HEAD needs no branch subprocess. One-shot CLI refresh starts fresh. Global URL rewrites and per-worktree config changes alone take effect after a common config change or host restart |
| PR state per repo | GitHub batches branch PRs and distinct HEAD rollups, plus one repository open-set alias in the first batch when a non-trunk target exists, within the 100-alias budget. Merge-queue state rides the existing open-facts alias and costs no alias or request, but a queued PR keeps its repository on the hot tier until it leaves the queue. Tea reads base branches from its one open-list call, so stacks cost no extra request; Tea adds a per-branch read only when a PR leaves the open set | `PR_STATE_HOT_TTL` and `PR_STATE_TTL`; failures back off and keep the last known map; Tea closed links, and merged links with a settled CI verdict, are pinned ([state.md](./sidebar/state.md#pr-state)) |
| Provider accounts | Cold providers probe in waves of at most four account-then-version chains, each subprocess capped at 3 s | `ACCOUNTS_TTL` per provider; contention serves the stale cache instead of starting a second wave |
| Fleet spend walk | Within `SPENDING_TTL`: one read of the published aggregate, no transcript IO. A due walk: frontier stats plus O(appended bytes) per changed file | One warm walker per namespace owns the index; consumers never open `spending.json` ([spending.md](./agents/spending.md#one-walk-per-namespace)) |
| Finished-cohort effort | Transcript stat and parse for collapsible groups only; unchanged files reuse a process-local parse memo | `COHORT_SPEND_TTL`; the producer publishes `lanes/cohort-spend.json` and renderers only read it |
| Team pipeline | One board read per eligible staged-team group plus one effective-team resolution per pass; no subprocess or event-log scan | Every refresher pass, no TTL; publishes `lanes/pipeline.json` only when content changes. With no team groups, skips resolution and board reads but clears stale entries |
| Codex daemon reap | Zero unless Codex remote control is on or a daemon-hooked session is live; when due, one process scan plus one WebSocket handshake | `CODEX_DAEMON_REAP_TTL`; success and failure share the stamp |

### The store write path

The choreography and write classes are [store.md → The write path](./store.md#the-write-path).

| Operation | Cost | Bound |
| --- | --- | --- |
| Hook ingress | One framed append of raw payload and captured environment under its shared blocking lock, no lifecycle fold | Appenders do not exclude each other; payload-sized, not the derived frame's 1 KiB limit; first creation syncs the parent directory |
| Derived lifecycle append | One event-log `write()`, zero steady-state fsyncs, one frame of at most 1 KiB | `store_fsync.rs` and `store_bytes.rs` pin the derived append; the drainer's lifecycle batch also folds under the workspace lock |
| Drain cursor | Two durable cursor publications per applied frame, plus synced truncation and cursor reset when the queue empties | Each publication is a temp file and two fsyncs under the workspace lock; the elected worker applies frames serially in environment-scoped children |
| Event-log durability | One group `fdatasync` per second per workspace (`LOG_SYNC_INTERVAL`) | Runs on the off-lock tail; cursor and truncation barriers are additional |
| Snapshot publish | One cache rename per second per workspace | Single-flighted; due after `PUBLISH_INTERVAL` (1 s) or `PUBLISH_BYTE_BUDGET` (64 KiB) of unpublished tail |
| Wakeup fanout | One heartbeat directory scan plus one datagram per live sidebar | N is live sidebars; the reads are page-cache hot and cost less than the fsync floor |
| Durable file write | Temp file plus two fsyncs (file, parent directory) | Trust grants, the workspace record, hook installs, and the per-frame drain cursor |

### Everything else

| Operation | Cost | Bound |
| --- | --- | --- |
| Sidebar observer | One O(rows) signature pass per committed fold, microseconds | Pure detection at every renderer's fold commit point; the host writes observer records and adds one cross-check pass per `OBSERVE_CROSSCHECK_TTL` (5 s) ([diagnostics.md](./diagnostics.md#the-frame-stream-observer)) |
| Tick meter | Six relaxed counter loads and two clock reads per metered tick | Healthy ticks do no file IO and spawn nothing |
| Sidebar heartbeat | Temp file plus atomic rename | Every `HEARTBEAT_WRITE_INTERVAL`, inside the liveness TTL |
| Room read marks | One stat-keyed parse per thread per record change | Full atomic-file identity of `live/read-marks/marks.json`; pane sweeps read the current record, not an older shared cut |
| Codex session index | One `stat()` of `session_index.jsonl` per live Codex session per refresh; one full parse per index change per process | Full file identity (length, mtime, device, inode): appends grow the file and thread deletes rewrite it ([adapter_codex.md](./agents/adapter_codex.md)) |
| Reload poll | One `stat()` of the durable reload record per sidebar per second | Executable hashing runs only after that record's metadata changes; the embedded presence-plugin digest resolves once, lazily ([sidebar.md](./sidebar/sidebar.md#build-promotion)) |
| Remote link probe | One JSON probe over the existing SSH ControlMaster every 2 s | Supervised remote attach only; `RIMZ_REMOTE_PROBE_MS=0` disables it |
| `loop watch` repaint | Catalog, pause, and terminal reads once per second; run-log cost is one `stat` per generation per room per repaint, reading appended bytes only; plus one `locks/` listing per task root that opens and try-locks every `loop-run-*.lock` file there, free ones included | Workspace identity resolves once before the loop; the run-log fold fully re-reads on rotation, a new local day, or a time-zone change; the lock cost grows with stale run-lock files until `rimz gc` removes the free ones |

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
| `produce_budget.rs` | Zero subprocess spawns for a warm fleet-scale produce with fresh inputs, and for a produce over stale or missing heavy caches; a retained refresh cycle over mixed Codex/Claude sessions and linked worktrees also spawns zero, after one cold origin lookup and no branch subprocess; produce wall-clock cost is the `fleet::produce_warm` bench below |
| Detached-helper and Codex transport unit tests | Spawn attempts include detached helpers, scope retries, and cold app-servers, so refresh budgets count these children too |
| `fold_incremental.rs` | Warm folds and produces read O(new bytes); a warm or unchanged fold parses zero carryover bytes, and a replaced carryover re-parses once |
| `consumer_enrichment.rs`, `enrichment_cadence.rs`, `spending_incremental.rs` | Unchanged-room consumer cost, per-enrichment TTL stamps, and O(delta) spend IO |

The `cargo xtask invariants` check `ensure_sidebar_library_boundaries` keeps store writers, the run-wake sender, and the broker out of the sidebar data plane's imports, so the producer cannot grow write-side machinery unnoticed.

### Live client-byte guard

`crates/rimz/tests/integration/journey/deep.rs::tmux_cell_only_sidebar_bounds_client_bytes` runs the real sidebar beside an agent driven by installed Codex hooks, with a PTY-attached tmux client. A cell-only frame costs that client at most 1 KiB: over a measured window, the guard allows one 1024-byte diff per configured frame interval plus 4 KiB for the status line. It also requires the attached client's screen to change during the window, so a renderer that paints nothing cannot pass. The pane has at least 4000 cells, making the old full repaint exceed the budget. The guard bites from tmux 3.7b: 3.7 and 3.7a could skip the redraw at the end of a synchronized update ([tmux reference](../externals/mux-adapter/tmux-reference.md)), so a full repaint per frame is not guaranteed to show on those releases. This is a live-tmux gate, run with `cargo xtask test -E 'binary(integration) & test(journey::deep::tmux)'`, not a wall-clock assertion.

### Benchmarks

`cargo xtask perf` runs divan benches in `crates/rimz/benches/` over synthetic stores and pane frames, through the same entry points the sidebar uses. Explicit regression ceilings, including `hook_stop_budget`, fail this command. It launches no agents and spends no tokens. These benches stay out of `ci`, so a busy CI runner never fails a build on their timing.

Allocation per operation is the steadier regression signal; medians move with host and load. The baseline was captured on `xlab-term`, a loaded LXC container on an AMD Ryzen 9 9950X with 28 online CPUs and the `performance` governor.

Run `cargo xtask perf --bench fleet -- publish` for shared publication at (51 agents, 24 panes, 4,600 ended sessions) and (200, 100, 5,000). `crates/rimz/src/testkit/mod.rs::fleet::seed_scale_room` takes state paths, runtime paths, and `ScaleRoom { agents, panes, ended_sessions, orphan_contexts }`, publishes fresh producer inputs, and returns a frame for `produce_snapshot` or `enrich_workspace`. Orphan context sidecars have keys absent from the room's agents. It binds the first live agents to the requested panes and leaves the remaining agents unbound. The carryover generator uses the existing representative row sizes but ends every history session, keeping the three dimensions independent; `seed_ended_carryover` retains its original 98% ended distribution for other benchmarks.

`crates/rimz/benches/fleet.rs::publish` measures one enrichment followed by all local projections, retaining the delivered snapshots together until the end of publication. Normal sidebar IDs are absent from the frame; `exclude_agent` instead excludes a live agent pane from each projection to measure detaching the group vector, its row group, and pane bindings. `local_only` moves the fold outside timing to isolate per-pane allocations. Each bound agent has its own synthetic branch, so the two room sizes have 24 and 100 groups. `local_fixed_room` holds the larger room fixed while varying subscribers between 24 and 100, separating subscriber scaling from group-count scaling. All cases use ten samples, one iteration per sample, with fixture setup and teardown outside timing. These cases measure publication, not renderer loops or live host RSS.

Captured 2026-10-12; paired values below are for 24 / 100 subscribers. Before is trunk `8fc7fe9cd`, measured in the reset checkout with only the fixture, benchmark, and public `project_local` seam added, before the sharing changes. After adds shared group vectors, rows, and pane bindings with guarded writers. Both runs use the same fixture and benchmark. Alloc/op is Divan's median `alloc` byte count, excluding its separately reported grow bytes.

| Publish case | Before median | After median | Before alloc/op | After alloc/op |
| --- | ---: | ---: | ---: | ---: |
| `fold_and_project`, respective room sizes | 2.431 ms / 23.26 ms | 1.800 ms / 4.330 ms | 2.664 MB / 24.98 MB | 1.035 MB / 2.780 MB |
| `exclude_agent`, respective room sizes | 2.487 ms / 18.81 ms | 2.417 ms / 12.65 ms | 2.683 MB / 25.37 MB | 1.589 MB / 12.27 MB |
| `local_only`, respective room sizes | 999.5 µs / 15.94 ms | 58.34 µs / 188.2 µs | 1.690 MB / 22.53 MB | 88.36 KB / 368.2 KB |
| `local_fixed_room`, 200 agents, 100 groups, 5,000 ended sessions | 4.403 ms / 18.79 ms | 62.84 µs / 206.4 µs | 5.407 MB / 22.53 MB | 88.36 KB / 368.2 KB |

Local allocations are flat at about 3.68 KB per subscriber, both at fixed room size and across the two room sizes: groups, rows, and history do not multiply with subscribers. Sharing reduces the larger local publication from trunk's 22.53 MB to 368.2 KB (about 98.4% less allocation); local configuration, worktree roots, and pane metadata remain owned. A real exclusion still pays for a detached group vector and changed collections, as the `exclude_agent` case shows.

#### Hidden-pane fanout at 50 agents and 57 subscribers

`crates/rimz/benches/fleet.rs::idle_cycle_hidden_fanout` places each subscriber in a distinct tab, watches one, and paints every attachment once before timing. Every measured cycle changes row content, delivers a real retained worker fold, and steps each renderer's frame timing twice. The 56 hidden panes coalesce content within the data tick; the watched pane applies every change. The allocation ceiling is 72,000 requests per cycle, about three times the measured 23,776 and below the uncoalesced 116,624.

Captured 2026-10-14 with `cargo xtask perf --bench fleet -- idle_cycle` and `cargo xtask perf --bench fleet -- publish`, ten samples per case. Before is `fb140708c`, after is `6c22a4b01`, on the same machine with the arena cap unchanged. Bytes include allocation and reallocation requests from the counting allocator, rather than Divan's allocation-only column. The other cases retain their 200-agent/100-subscriber fixtures.

| Case | Orphans | Before → after median, ms | Before → after allocation requests | Before → after requested bytes |
| --- | ---: | ---: | ---: | ---: |
| Painted hidden fanout | 0 | 7.689 → 5.115 | 116,624 → 23,776 | 10,846,587 → 2,920,860 |
| End-to-end idle | 0 | 7.013 → 5.924 | 51,200 → 51,200 | 6,566,776 → 6,483,552 |
| End-to-end idle | 3,000 | 6.407 → 7.637 | 51,200 → 51,200 | 6,566,776 → 6,483,552 |
| Worker-only idle | 0 | 6.142 → 16.74 | 51,103 → 51,103 | 6,559,720 → 6,476,496 |
| Worker-only idle | 3,000 | 6.280 → 16.37 | 51,103 → 51,103 | 6,559,720 → 6,476,496 |
| Publish | 0 | 3.952 → 3.327 | 37,996 → 37,996 | 3,807,708 → 3,724,484 |
| Publish | 3,000 | 4.857 → 7.687 | 37,996 → 37,996 | 3,807,708 → 3,724,484 |

Hidden fanout requests 79.6% fewer allocations and 73.1% fewer bytes. The wall times were taken at a load average near 45 and are not comparable: two back-to-back reruns of the unchanged worker-only arm read 6.4 ms and 12.2 ms. Allocation counts are deterministic and carry the comparison; no wall-time change is claimed for the other arms. This pair compares the complete implementation, without isolating the animation short-circuit or measuring live-host CPU.

#### Change-driven delivery at 200 agents and 100 subscribers

`crates/rimz/benches/fleet.rs::idle_cycle` measures a real retained worker fold and delivery followed by all 100 subscribers applying through `LoopState::on_snapshot`. Painting is excluded: content and clock-only deliveries both mark the frame dirty. `idle_cycle_worker` retains the worker-only measurement. Both use 5,000 ended sessions and either zero or 3,000 orphan context sidecars; fixture creation and the initial content cycle are outside timing. The idle-consumer tests assert that every renderer's snapshot clock advances; the shared benchmark fixture permits unchanged clocks when hidden content is deferred.

The earlier keyed-read measurements used `cargo xtask perf --bench fleet -- publish` and `cargo xtask perf --bench fleet -- idle_cycle` (the latter was worker-only then). Ten samples per case, median wall time; requested bytes include allocations and reallocations counted by `crates/rimz/benches/fleet.rs::counted`, not Divan's separate allocation column:

| Case | Orphans | Before keyed reads, ms / bytes | After keyed reads, ms / bytes |
| --- | ---: | ---: | ---: |
| Publish | 0 | 3.045 / 3,528,539 | 3.744 / 3,773,532 |
| Publish | 3,000 | 22.59 / 25,098,655 | 3.703 / 3,773,532 |
| Worker-only idle | 0 | 4.579 / 6,257,968 | 8.041 / 6,502,961 |
| Worker-only idle | 3,000 | 21.70 / 27,824,532 | 5.779 / 6,502,961 |

The renderer-apply comparison below was captured on 2026-10-12. Before is the keyed-read change, after adds delivery gating, each with the same renderer fixture. Native bench executables ran from the checkout root in three before/after alternations, ten samples per run: `--bench idle_cycle` and `--bench publish::scale_with_orphans`. Times summarize the three medians by their median; counts are per warm cycle. The idle comparison was repeated after adding the renderer clock assertion; that final series is reported here. These runs were on a loaded host, not measurements of live-host CPU.

| Case | Orphans | Before ms | After ms | Before → after allocation requests | Before → after requested bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| Publish | 0 | 3.875 | 3.881 | 37,674 → 37,674 | 3,753,128 → 3,753,128 |
| Publish | 3,000 | 3.907 | 3.735 | 37,674 → 37,674 | 3,753,128 → 3,753,128 |
| End-to-end idle | 0 | 27.64 | 12.25 | 300,454 → 50,877 to 51,438 | 22,498,013 → 6,502,167 to 6,531,291 |
| End-to-end idle | 3,000 | 24.24 | 10.77 | 300,454 → 50,877 | 22,498,013 → 6,502,167 |
| Worker-only idle | 0 | 7.228 | 11.39 | 50,657 → 50,780 | 6,482,557 → 6,495,111 |
| Worker-only idle | 3,000 | 7.280 | 8.234 | 50,657 → 50,780 | 6,482,557 → 6,495,111 |

End-to-end saving is 15.39 ms per cycle without orphans and 13.47 ms with 3,000 orphans. Worker-only medians increased 4.162 / 0.954 ms respectively; the after medians span 9.786 to 12.90 / 7.182 to 19.05 ms. The worker-only target is not established within noise, and these deltas do not isolate comparison cost from scheduling noise. Three further alternating no-orphan runs against the tree before keyed reads gave worker medians 5.397 → 6.317 ms, with 47,405 → 50,780 requests and 6,237,564 → 6,495,111 bytes. All three after medians were higher. The earlier 4.579 → 8.041 ms rise was exaggerated by noise, but the no-orphan overhead is not all noise; this comparison includes both keyed reads and delivery gating.

Scale ceilings fail `cargo xtask perf --bench fleet`: publication at 200/100 may request at most 11,500,000 bytes per cycle (about 3× the measured 3,753,128 bytes), and the upper median end-to-end idle cycle with 3,000 orphans may take at most 33 ms (about 3× the measured 10.77 ms). Each failure prints the measured value and ceiling. The time bound uses the median rather than a single scheduler stall; these benches remain outside CI.

| Bench | Median | Alloc/op | Captured |
| --- | ---: | ---: | --- |
| `fleet::produce_cold` 20 / 50 / 100 agents | 4.65 ms / 7.61 ms / 13.45 ms | 4.52 MB / 9.26 MB / 17.36 MB | 2026-07-05 |
| `fleet::produce_warm` 20 / 50 / 100 agents | 520 µs / 792 µs / 1.32 ms | 1.53 MB / 2.09 MB / 2.98 MB | 2026-07-05 |
| `fleet::idle_cycle_hidden_fanout` 50 agents / 57 subscribers, one watched | 5.115 ms | 2.92 MB requested | 2026-10-14 |
| `hotpath::fuse_owned_no_overlay` 40 agents | 74.7 µs | 819 B | not recorded |
| `hotpath::rollup_fold_warm` 40 agents | 129 µs | 637.1 KB | 2026-07-05 |
| `hotpath::rollup_fold_warm` 40 agents, 0 / 1,500 carryover rows, re-parsing carryover | 70.8 µs / 16.52 ms | 989 KB / 90.11 MB | 2026-09-14 |
| `hotpath::rollup_fold_warm` 40 agents, 0 / 1,500 carryover rows, shared carryover parse | 56.9 µs / 79.8 µs | 655.6 KB / 655.6 KB | 2026-09-14 |
| `hotpath::rollup_fold_unchanged` 40 agents, 0 / 1,500 carryover rows, cloning the held rollup | 10.95 µs / 1.92 ms | 121.6 KB / 16.03 MB | 2026-09-14 |
| `hotpath::rollup_fold_unchanged` 40 agents, 0 / 1,500 carryover rows, layered rollup | 2.24 µs / 2.99 µs | 0 B / 0 B | 2026-09-14 |
| `hotpath::carryover_fold_cold` 3,300 / 13,200 rows | 58.97 ms / 299.4 ms | 107.9 MB / 420.5 MB | 2026-10-07 |
| `hotpath::carryover_fold_rebirth_warm` 3,300 / 13,200 rows, one registration delta | 429.8 µs / 4.347 ms | 1.725 MB / 6.746 MB | 2026-10-07 |
| `hotpath::enrich_cached` 40 agents | 610 µs | 1.21 MB | 2026-07-05 |
| `hotpath::render_fixed` 40 agents | 413 µs | 1.09 MB | 2026-07-05 |
| `hotpath::spending_walk_cold` 20k entries | 19.59 ms | 24.43 MB | 2026-07-15 |
| `hotpath::spending_walk_warm_no_change` 20k entries | 7.95 ms | 10.78 MB | 2026-07-15 |
| `hotpath::spending_live_scale_cold_hydrate` 6k files / 102k entries | 252.3 ms | 122.6 MB | 2026-07-16 |
| `hotpath::spending_live_scale_cold_discovery_inclusive` 6k files / 102k entries | 173.9 ms | 101.9 MB | 2026-07-18 |
| `hotpath::spending_live_scale_warm_global_refresh` 6k files / 102k entries | 93.5 ms | 50.25 MB | 2026-07-20 |
| `hotpath::spending_live_scale_warm_discovery_only` 6k files | 19.55 ms | 770.9 KB | 2026-07-18 |
| `hotpath::spending_live_scale_warm_discovery_inclusive` 6k files / 102k entries | 140.2 ms | 99.59 MB | 2026-07-18 |
| `hotpath::spending_live_scale_additional_workspace_scope` 6k files / 102k entries | 116.6 ms | 58.59 MB | 2026-07-20 |

### Carryover fold measurements

Run `cargo xtask perf --bench hotpath -- carryover_fold` for these three benches. `crates/rimz/src/testkit/mod.rs::fleet::seed_ended_carryover` supplies unique identities, 98% ended rows, and 39% pane-bearing rows. Compact row JSON is 4,800 bytes at p50, about 13,200 at p90, and 35,900 at maximum; the 1x and 4x files are 32,943,350 and 131,784,597 bytes. Each bench takes 20 samples with fixture setup and teardown outside timing. The cold benches parse inside timing: `carryover_fold_cold` reads raw, while `carryover_fold_cold_derived` reads the stamped trimmed copy published on a separate thread outside timing; the warm bench primes a rebirth cursor and appends one `Registered` frame outside timing before measuring the delta fold. Alloc/op is Divan's `alloc` byte count; cold allocation includes transient raw-file text and decoding, not just the retained projection.

The 2026-10-07 capture ran from 19:20 to 19:26 UTC on the host above, with load averages `29.58 39.46 40.04` before and `59.70 54.95 47.02` after. These are non-gating observations, not latency bounds.

For process RSS, set `RIMZ_CARRYOVER_FIXTURE_DIR` to a scratch directory when running the benches. They export `carryover-3300.json`, `carryover-13200.json`, and the matching `rebirth-<rows>.log.jsonl` files outside timing. Build with `cargo build --release -p rimz --bin rimz` and resolve this checkout's binary as `$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)/release/rimz`. From a managed room, retain its `RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`; create a scratch `RIMZ_HOME` with only the room's copied `ws/<directory>/workspace.json`. For each size, put the exported carryover under that workspace's `records/agents-carryover.json` and its rebirth log under `log/events.log.jsonl`, with no snapshot checkpoints. Time invocations with `env RIMZ_HOME="$carryover_rss_home" /usr/bin/time -f '%M %e %U %S' "$carryover_binary" agents list --json > /dev/null`. For a before/after comparison, verify binary versions and alternate three runs per binary and size in one session on the same scratch home and fixtures. If the installed binary is not the intended base, build that revision in a separate scratch worktree and target directory, then remove the worktree.

The 2026-10-07 release-profile comparison began at 19:31 UTC with load averages `40.04 48.07 46.28`, ending at `44.51 48.70 46.50`. Before is the merge base `g805007bff1bf`; after is this change. Each entry reports minimum wall time and maximum peak RSS across three alternating runs per binary and size. An earlier untimed `strace -f -e trace=openat,execve` control at 1x confirmed the then-current branch release executable, one scratch carryover read, and two active-log reads (the snapshot and audit folds). The CLI retains the existing room's live-mux and runtime-cache overhead. Peak RSS includes transient raw parsing and allocator pages, so it is not a retained-heap measurement.

| Binary | Rows | Peak RSS (KiB) | Min wall (s) |
| --- | ---: | ---: | ---: |
| Before | 3,300 | 142,308 | 0.33 |
| After | 3,300 | 82,332 | 0.25 |
| Before | 13,200 | 513,260 | 0.87 |
| After | 13,200 | 302,124 | 0.51 |

## Overhead at fleet scale

RimZ is sized against a single agent, not against the fleet: watching a hundred agents should cost a small and nearly flat fraction of running one of them.

Cost attaches to three units. Hook application scales with event volume; moving it out of the foreground does not remove its fold or IO cost.

Per workspace, RimZ pays once. The host pays the roster and metrics on its fetch worker and git and accounts on its cache refresher, then folds once for all its panes. Every workspace that shares a state root and provider-discovery environment asks the same warm spend walker, so a host restart adds no second parsed cursor. One elected hook drainer owns the ingress queue and exits after 60 s without work or connections. Each spawn runs the room's recorded RimZ binary, with `RIMZ_BIN` as an explicit override, rather than the spawning hook or wrapper's build. When quiet, `crates/rimz/src/harness/hook_drain.rs::run` blocks in poll(2) on its listener and client sockets; a lease prevents idle exit without introducing periodic wakeups. After each wakeup the drainer exits when its socket path no longer names its listener, without unlinking that path. Idle exit unlinks only its own socket and closes the listener before the final drain, so a later hook elects a successor instead of nudging an exiting worker.

Per worktree, cost follows activity. The git input set scales with distinct group roots, not agents; a root drops to its idle TTL once its agents go quiet, and the sweep runs at most 8 roots at once. PR probes scale with origin repositories, each enumerating open PRs once when due. A hundred agents sharing a few checkouts pay for a few hot roots.

Per agent, cost is event-driven. An agent reports through a short-lived `rimz hooks feed` child that appends when something happens and exits. Every interactive launch retains its wrapper. A Unix wrapper re-execs into a parked image with no store and an 8 MiB private (anonymous) memory budget, guarded by the live `backend::tmux::agent_lifecycle::parked_wrapper_holds_under_8_mib` test and the thin-image invariant. Executable text is shared across wrappers; the test bounds `Pss_Anon` and prints total `Pss` alongside it. Fold-dependent duties run in helper children: strand repair is resident only for its parked nonterminal phase, while receipt, parent confirmation and card evidence are one-shot. A `--keep` subagent's settled linger is outside this budget ([the exec wrapper](./harness/fleet.md#resident-wrapper)).

Two debounced costs stay flat in agent count: event-log group `fdatasync` and snapshot publication. Both scale with rooms. Drain cursor publications are separate, per-frame durable writes, not part of either debounce. The hot runtime caches live in `$XDG_RUNTIME_DIR` (tmpfs), so their churn is memory traffic, not disk IO.

Display-plane CPU and RAM estimates across a fleet of 2 to 5 rooms follow. They exclude the elected hook drainer and its serial apply child, whose cost follows hook volume rather than fleet size.

| Resource | 20 agents | 50 agents | 100 agents | What sets it |
| --- | --- | --- | --- | --- |
| CPU, idle | ~0 | ~0 | ~0 | Loops block in `recv`; off-screen animation pauses |
| CPU, busy | <0.3 core | ~0.3 to 0.8 core | ~0.5 to 1.5 core | One producer per room, bursting toward the 8-root git cap, never on the render thread |
| RAM, resident | ~80 to 150 MiB | ~100 to 180 MiB | ~120 to 220 MiB | One host per room plus a small supervisor and local snapshot metadata per tab, shared rosters and rows, one spend walker, room-local rollups, prepared cell-pet grids |
| Durable write | Event-volume dependent | Event-volume dependent | Event-volume dependent | Derived lifecycle frames at 1 KiB or less, raw ingress payload and environment, and cursor publications |
| fsync rate | Event-volume dependent | Event-volume dependent | Event-volume dependent | Group event-log barrier plus four cursor fsyncs per applied frame and three barriers per drained-queue reset |
| State on disk | Tens of MiB | Tens of MiB | ~100s of MiB | Rotation-capped event log, ~5 KiB of snapshot per agent, and the pending ingress queue, truncated after drain |
| Network | Weekly pricing fetch, 5-minute OAuth usage probes per metered provider, forge open-set probes per due repo | Same | Same | Pricing is fleet-shared and single-flighted; local datagrams carry the rest |

Against the agents it tracks, that overhead is a rounding error. One developer's week of Claude and Codex sessions produced 1.23 GiB of transcript JSONL (about 177 MiB a day), with each agent process resident at 250 to 340 MiB (Claude) or 50 to 65 MiB (Codex). RimZ watched the same fleet with tens of MiB of durable state, a resident set about the size of one agent process, one fsync a second per room, and one pricing refresh a week.

Remote render-stream bytes sit outside this CPU and memory budget: SSH carries whatever the visible full-screen TUIs repaint. The sidebar has a separate [client-byte guard](#live-client-byte-guard): a cell-only frame costs the attached tmux client at most 1 KiB, rather than the roughly 17 KB full repaint its unconditional synchronized-output bracket caused on a 60x69 pane (tmux 3.7c, 2026-10-07; about 165 KB/s). A same-day guard re-measure on a 50x119 pane received 496667 bytes in 10.066 seconds before this change, versus 19963 bytes in 10.114 seconds with graphics-only brackets, about 49 KB/s versus 2 KB/s. These are attached-client bytes, not the renderer's producer write rate. Idle RimZ surfaces send close to nothing, and a busy agent TUI commonly sends tens of KB/s. `rimz pane bandwidth` reports each pane's producer write rate beside the room's SSH socket payload (`WIRE(ssh)`), which is usually far below the per-pane sum ([reference](../reference/cli/pane.md#bandwidth)).

## What's optimized

Each mechanism that holds a cost down, named once with its code home. Where another page owns the mechanism, this section says what it saves and links there.

### The frame is decoupled from the fetch

The fetch worker builds snapshots; the render loop blocks in `recv` and folds a result when the worker's `snapshot` wakeup arrives. The animation tick redraws from the cached snapshot and never fetches, so a missed push degrades to the backstop tick instead of a poll storm. [`animation_cadence`](../../crates/rimz/src/sidebar_pane/render/animation.rs) classifies row motion: fast work stays on the base grid, row animations redraw on `BREATH_ANIMATION_FRAME`, and a dirty data fold pulls the next frame back to the base grid so freshness never waits on cosmetic motion. A wheel glide temporarily takes the finer grid ([state.md](./sidebar/state.md#the-paint-clock)).

### Only the watched tab animates

Each renderer knows its own pane and the latest focus view, so it can tell whether a client is looking at its tab. A watched tab keeps the normal grid. An unwatched or detached tab skips the animation roster scan and wakes on the data backstop. Every pane requests event-driven folds immediately; the shared worker coalesces their requests. A settled hidden attachment coalesces content folds to at most once per data tick, plus on reveal, own-view changes, and outward actions. It retains its dirty frame without rendering cells, diffing, or writing it. Reveal resumes painting within one base frame. When ownership is unknown the renderer counts as watched, so tests, demos, and cold starts keep the responsive path. The mailbox exceptions and hidden paint rules are [state.md → The paint clock](./sidebar/state.md#the-paint-clock).

### One shared fold per room

The producer builds the snapshot in process on its fetch worker ([`produce_workspace_snapshot`](../../crates/rimz/src/sidebar/produce/mod.rs)), with no `rimz sidebar snapshot` fork per tick. The worker's cadence stamp allows one attempt per data tick, and only against a stale retained frame, so unrelated wakeups cannot multiply production.

The fold spine splits into renderer-independent [`enrich_workspace`](../../crates/rimz/src/sidebar/enrich.rs) and renderer-local `project_local`. The host folds the first half once and projects it for every subscribed pane. It remembers the last delivered fold for new-attachment seeds. Both lanes are [state.md → One fetch cycle](./sidebar/state.md#one-fetch-cycle).

### Truth arrives by event, the frame by coalescing

The host delivers produced folds directly. The rollup is read event-fresh from `cache/snapshots/latest.json` on every fold, so a status change repaints within one wakeup while the host retains and shares its pane inputs.

Every writer that knows about a change pushes: store and sidecar writers post a `StoreDelta`, both backends' presence streams feed one projector that emits typed pane events, and the host's transcript watcher refreshes context mid-turn for every adapter that declares transcript-tail context ([state.md → Push channels](./sidebar/state.md#push-channels)).

### No reader pays for history

The rollup persists a raw fold base with its `(generation, offset)` stamp, and catch-up seeks to the offset and folds only new frames ([`fold.rs`](../../crates/rimz/src/store/snapshot/fold.rs), [store.md → The read path](./store.md#the-read-path)). Runtime projection, resume outcomes, and smart-compact dedupe ride the same fold instead of rescanning `log/events.log.jsonl`. A `(path, mtime, len)` parse cache on `lanes/snapshot.json`, `cache/snapshots/latest.json`, and `cache/snapshots/rollup.json`, and a full-identity `(len, mtime, dev, ino)` one on `records/agents-carryover.json`, return `Arc<T>` handles, so an unchanged file costs neither a re-parse nor a deep clone ([`disk/parse_cache.rs`](../../crates/rimz/src/disk/parse_cache.rs)). The carryover's cached form is fold-ready (identities backfilled, rows key-sorted) and retains only a bounded first-line prompt label on ended rows, unlike the unchanged durable record. Its reborn base is cached once per parse, and every fold layers live rows over the shared base without copying all history. The [read-path contract](./store.md#the-read-path) owns the prefix bound, hydration, and the explicit `Store::load_full_agent` read. A corrupt replacement is never cached: every fold reports it until the file is replaced. In-place corruption that preserves the source stamp can remain masked by the derived copy; the [read-path contract](./store.md#the-read-path) describes this accepted edge.

The spend walk is incremental in three layers: the walker's directory index stats only active frontiers and reconciles fully every 15 minutes, the disk cache keeps a cursor per file so a grown file parses only its appended suffix, and the one elected walker holds the only parsed cache, so workspace requests borrow instead of cloning. Aggregation uses hash collections for session uniqueness and keeps deterministic order only in the published maps. The cache layout is [spending.md → The incremental cache](./agents/spending.md#the-incremental-cache).

### Per-enrichment cadences

Every display figure is display-only by invariant ([DESIGN.md](../../DESIGN.md#triage-at-a-glance)), so each enrichment runs on its own cadence behind stamps stored in the cache file it already writes. Because the stamps live in shared files, every process agrees on freshness across a producer handoff. Account probes carry one stamp per provider, so a failure retries alone. Process sampling keeps per-pane focused and background stamps, independent of the roster clock. Git probes take activity-tiered TTLs whose hotness comes from store activity, with no filesystem watching, and in-process `.git` ref reads skip the ancestry forks when the cached HEAD and trunk pair and the clean verdict are unchanged. The values are [state.md → Cadences](./sidebar/state.md#cadences).

### Append and drain critical sections

The foreground hook appends under a shared, blocking `locks/hook-ingress.lock`, independently of the workspace lock used for lifecycle batches and durable cursor publication. Appenders do not exclude each other or poll in sleep quanta. A due `PostToolUse` deadline rung can still take the workspace lock. The drainer holds ingress exclusively for its repair read, releases it before application, and takes workspace then exclusive ingress for repair, truncation and store reset. Not every workspace-lock hold is a microsecond append or fsync-free. Workspace-lock waiters retry at 1 ms for the first 25 ms, then exponentially back off to 50 ms; the acquisition deadline and inode-reopening rules are unchanged. The event-log off-lock tail retains its group `fdatasync` barrier. Length-plus-CRC32 framing detects corruption; the event log truncates a lost suffix, while ingress skips corrupt terminated rows and truncates an unterminated tail with diagnostic evidence. Every fsync goes through `disk/atomic.rs`, which the `cargo xtask invariants` check `ensure_store_durability` enforces for `sync_all` and `sync_data` calls. The contract is [store.md → Write classes](./store.md#write-classes).

### Helper processes stay cheap

The build id is computed once per process from the linker build identity in the first 1 MiB of the image, and the whole image is hashed only when that identity is missing; `main` warms it off-thread at startup ([`build_id.rs`](../../crates/rimz/src/build_id.rs)). Self-spawns resolve through one helper, `rimz_exe`, which honors `RIMZ_BIN` and repairs Linux `current_exe()` paths ending in ` (deleted)` after an atomic reinstall. The Zellij presence plugin hashes the stable manifest fields before projecting pane fields, so title-only `PaneUpdate` storms skip projection ([`policy.rs`](../../crates/rimz-presence-zellij/src/policy.rs)). Zellij rooms start with `disable_session_metadata true` by default, which stops the server rewriting `session-metadata.kdl` and running command-discovery `ps` for the room.

### Codex context stays warm

Codex app-server enrichment for the room-default login starts with a broker in the `rimzd` tab that holds one handshaked `codex app-server`, and falls back to a cold spawn. Other logins skip the room broker, and periodic usage refresh uses only their direct probe ([adapter_codex.md → App-server enrichment](./agents/adapter_codex.md#app-server-enrichment)). The host's transcript watcher debounces file events to one flush per 300 ms for the workspace and refreshes each changed session in that flush. Periodic session passes back up the watcher with local reads only; they never spawn app-servers.

## Anti-patterns

Each of these has cost RimZ a real regression. They are grouped by the principle they break, because the principle is what transfers.

**Cosmetics must not drag data (principle 2).** A fixed refetch every 500 ms to keep a working agent's cost figures moving forks a subprocess per frame to move a spinner, and on Zellij its roster read resets unrelated panes' cursor blink. Context-sidecar pushes cover the same need for free.

**Recovery belongs to the host and its supervisors (principle 8).** Letting each attachment run its own production on stale data creates an N-way storm on exactly the tick the room is already degraded. Per-tab roster reads pin the mux server with N round trips. Capping renderers instead of production blacks out every tab but one.

**Shared state has one owner (principle 5).** Per-host spend walkers duplicate the account-global parsed cursor. Readers that derive workspace spend from the global cursor make every renderer parse `spending.json`. Per-thread heartbeat scans make each thread decode every renderer heartbeat. The fix in each case is one shared service or process-local owner that the others read.

**Unchanged inputs cost nothing (principle 7).** Independent attachment folds multiply config reads, enrichment, and published JSON clones with every input unchanged. Background metrics publications must share one fold across attachments, not force a full fold per hidden pane. Scanning a provider-global session tree (`~/.codex/sessions`) once per worktree traverses the same directories many times in one fold.

**Every retry is bounded (principle 6).** A deterministic `gh` or `tea` failure retried on the hot TTL, with no command deadline and the cached map erased each time, turns a forge outage into a permanent per-worktree fork loop. An auth probe whose result is recorded under a different scope than the one requested reads as a new login on the next fold and spawns another helper. Both need backoff that keeps the last known good value.

**Cheap-looking work multiplies by fleet size.** Deriving the merge base separately for every group root costs about 240 `git` execs on a 20-worktree fixture. A warning emitted once per fold per renderer, with Sentry's `attach_stacktrace` on, makes every consumer symbolize a backtrace before the rate limiter can drop it (about 200 MiB per fold against 18 MiB clean). Streaming serde output straight into a socket turns each JSON token into a syscall.

**An atomic reinstall leaves deleted inodes behind.** A long-lived renderer's `current_exe()` resolves to `rimz (deleted)`, so a naive self-spawn fails `execve`, and a supervisor parent holds the deleted inode until session end while its children orphan. `rimz_exe` repairs the suffix, supervisor-owned reload convergence re-execs parents onto the new binary, and supervisor-side reaping clears the orphans.

**A 2-arena cap serializes malloc in a many-threaded long-lived process.** With glibc capped to two malloc arenas, the sidebar host's 123 threads queued on the arena locks: a profile of a 57-window room put 48% of its on-CPU samples under `__lll_lock_wait_private`. `crates/rimz/src/main.rs::malloc_arena_cap` gives the host 16 arenas and keeps 2 everywhere else. On a 57-window tmux sandbox room with 5 `store_delta` events per second and only stub agents, fresh hosts measured, over 3 × 10 s windows each:

| Host arena cap | Hosts | Mean CPU (run means) | USS after about 1 min |
| --- | ---: | --- | --- |
| 2 (old) | 3 | 27.7% (26.1 to 30.3) | 51 MiB |
| 8 | 6 | 19.1% (18.6 to 20.1) | 53 MiB |
| 16 (adopted) | 6 | 16.9% (15.2 to 18.4) | 56 MiB |
| no cap (8 × cores) | 6 | 15.0% (13.7 to 16.6) | 62 MiB |

The 2-arena row comes from an earlier session of the same harness, where cap 8 averaged 18.1%. Cap 4 averaged 22.8%. Every cap-8 run averaged above every uncapped run, while cap 16 overlapped them. The host stays capped because glibc's default, 8 arenas per core, bounds nothing on a many-core machine over hours of uptime. Lift another process's cap only after a profile of that process shows the same lock wait.

## Adopted hook drainer

The rejected resident-writer premise was wrong: the foreground hook folded the room and ran lifecycle effects, not merely a microsecond append. `crates/rimz/src/cli/hooks.rs::run_feed` now appends raw ingress before return and nudges an elected, idle-exiting worker. Verified pins avoid git resolution; path discovery and typed validation reuse the same atomically stamped workspace-record bytes. A failed connect spawns without waiting for the spawn lock or listener. Host drain-through callers keep polling the socket and lifetime lock until their deadline: a socket connection requests the elected drainer's result, while a free lock applies inline immediately, including after a failed spawn. Sandbox callers use their wrapper-held lease and apply in the caller's view without appending when the host drainer is unreachable or their launch predates the `RIMZ_SANDBOX_HOST_PATHS` mount pin. A missing pin also skips the connect. Hook stdout remains the provider's decision channel and helper children have fresh stdio. Reply-bearing events wait for enrichment, not all hooks.

Release replay used the same frozen room copy (3.7 MB cache, 84 MB log, 9.2 MB records), pinned Codex `Stop`, ten interleaved hook/`--version` pairs and a separate 24-stop burst. Before is `g957778e33d7a`; after is `19c2341e5`, including the elected worker, independent ingress lock, bounded lock retries and workspace-record reuse. The first hook starts without a drainer; steady-state samples follow one hook and drain-through. These are separate measurement windows, not an alternating before/after experiment.

| Release replay (ms) | Before min / mean / max | After min / mean / max |
| --- | ---: | ---: |
| Sequential hook | 205.804 / 277.554 / 362.888 | 2.051 / 2.984 / 5.183 |
| Interleaved `--version` | 1.985 / 2.662 / 3.566 | 1.694 / 2.097 / 2.501 |
| Python-coordinated 24 concurrent hooks (diagnostic) | 708.053 / 1824.012 / 2992.600 | 14.749 / 32.949 / 38.937 |

The after replay's cold hooks returned in 5.238 and 4.485 ms. Drain-through after the measured returns took another 2.865 s sequentially and 7.176 s after the burst: the serial environment-scoped apply child moves fold and effects off the foreground, not out of the system. Cursor claim, applied and reset publications are additional durability work, not the debounced event-log barrier.

`crates/rimz/tests/integration/performance/concurrent_stops.rs::every_concurrent_stop_lands_and_folds` feeds 24 concurrent Stops through `rimz hooks feed` against a live `rimz hooks drain` worker, once on an empty store and once on a 40-agent seeded store. It proves every Stop's frame is in ingress or the event log before its hook returns, that drain-through derives exactly one lifecycle event per Stop, and that the fold sees all 24 agents. It asserts no timing: per the performance tier rule in [rust-conventions.md](../contributing/rust-conventions.md), the timing guard for this path is `crates/rimz/benches/hotpath.rs::hook_stop_budget` alone. That guard fails `cargo xtask perf`, not CI.

`crates/rimz/benches/hotpath.rs::hook_stop_budget` holds the release hook wall to max(4 ms, 2x the `--version` wall) for sequential and 24-way ingress, each case at its own statistic. The sequential half compares medians over 100 interleaved pairs: a single spawn stalls at random, so a high percentile of a few dozen samples is the slowest one and fails on host noise, while a regression on the foreground path (fold or lifecycle effects, an extra fsync, git resolution) shifts every sample and still fails the median. The 24-way half compares p99 over 240 children per side, where tail contention (lock retries, drainer contention) would surface. Its Rust coordinator prepares commands before one barrier releases 24 threads, each timing spawn through the standard child wait. One unmeasured warm-up burst per side precedes ten hook bursts alternating with ten version bursts; both write and close the same piped stdin, and output collection is outside timing. Sequential samples alternate 100 pairs. Both cases measure a warmed, live draining worker; first-creation directory sync and worker startup remain in the separately measured cold hook.

| Rust release coordinator, shared ingress lock (ms) | Statistic | Hook wall | Version wall | Ceiling |
| --- | --- | ---: | ---: | ---: |
| Seeded, sequential | median of 100 | 2.197 | 1.738 | 4.000 |
| Seeded, 24-way | p99 of 240 | 7.834 | 6.907 | 13.814 |
| Live-sized copy, sequential | p99 (max of 48, pre-change) | 3.389 | 5.414 | 10.828 |
| Live-sized copy, 24-way | p99 of 240 | 8.164 | 7.369 | 14.738 |

The seeded sequential row is the slowest hook median of five quiet runs after the switch to medians (hook medians 1.887 to 2.197 ms, maxima 2.877 to 3.743 ms); three runs beside 14 CPU hogs held hook medians of 2.555 to 2.746 ms against ceilings of 4.169 to 4.454 ms, one with a 9.934 ms hook maximum, above twice the 3.788 ms `--version` maximum: the spike a max-based check fails on. A scratch 4 ms sleep added to every hook sample fails the median at 6.220 ms. The other rows predate the switch.

Burst p99 is over 240 children per side; per-burst wall summaries are printed for replay. Set `RIMZ_HOOK_BUDGET_HOME` to a frozen home containing `.rimz` and exactly one recorded room to run the same bench on a disposable copy; the source stays untouched. Build with `cargo build -p rimz --release --features testkit --bins`, then run `cargo xtask perf -- hook`, with that environment variable for the live-sized case. Cleanup drains the measured frames and warm-up outside the budget with a 120 s wait sized for serial apply. After-return drain took 2.668 s on the fixture and 80.468 s on the live-sized copy.

Both final sequential hook values are below 4 ms, and both 24-way cases satisfy the relative ceiling. Across the native runs the 24-way exec baseline itself was about 7 to 11 ms (6.907/7.369 ms in this final run): 4 ms is the sequential target, not a fixed 24-way wall-time promise. Before shared appenders and parallel warm-up, the fixture missed the same rule at 24.206 ms hook versus 7.909 ms version. Exclusive ingress locking polled with 1 ms sleeps made appenders queue behind each other; sharing a kernel-blocking lock removes that appender contention while excluding length snapshots and truncation. The earlier Python ThreadPoolExecutor burst's 38.937 ms and a separate subsequent version burst's 8.647 ms were not a fair paired comparison and remain diagnostic only. Earlier pinned traces show one workspace-record open, one ingress-lock open, one append and one socket connect, with no workspace-lock, snapshot, rollup, event-log or git access.

The review fixes publish each staged lifecycle batch with one ordered-batch append under the workspace lock, using the existing framed-log write path. The measurements above predate the review fixes; they remain historical evidence, not measurements of the corrected drainer's throughput. The hook's append-and-nudge budget is unchanged.

## Deferred and rejected

Wins identified but not taken, because each changes a contract or crosses a backend-parity boundary, ranked by expected payoff:

1. **tmux `list-panes` over the held control client.** The host's `tmux -C` client already writes commands and parses reply blocks, so sending `list-panes` over it would remove the producer's per-window fork and connect. The saving is 10 to 30 ms on the already-cheap backend, and the poll must still cover a dead watcher.
2. **Delta-bearing wakeup datagrams.** `StoreDelta` could carry the appended frames, so a warm consumer folds from the datagram with no file IO. The warm cursor fold is already one stat plus a page-cache-hot read, so the win is microseconds against a second delivery path for state that must never become truth. Build it only above sustained hundreds of events per second.

Two costs belong upstream, and RimZ only bounds them:

- **Zellij topology freshness depends on the presence plugin.** A missing, denied, or wedged plugin leaves a Zellij room holding its last good frame while `rimz doctor` names the failing precondition. A watchdog belongs in the presence channel, not in a CLI fallback.
- **Zellij compact-bar and scrollback footprint.** A 100-pane room carries one compact-bar wasm instance per tab and large server-side scrollback. RimZ can recommend configuration and avoid adding pressure; the costs belong to Zellij's plugin and scrollback model.

Evaluated and rejected, recorded so the next pass does not reopen them:

- **Lock-free `O_APPEND` event appends.** Recovery assumes the lock makes the log single-writer, so only the trailing frame can tear. Concurrent appenders' dirty pages can write back out of order and leave a zeroed frame in the middle, which rebuild correctly treats as a hard error. Making that safe needs per-frame magic for resync, all to shave a lock hold that is already microseconds.
- **Binary snapshot format.** The parse cache removes re-parsing on delta storms and the `RollupCursor` holds the parsed base in memory, so a binary checkpoint would speed up a parse that does not happen. JSON keeps `rimz sidebar snapshot --json` inspectable.
- **Caching the wakeup heartbeat scan.** N is live sidebars, the reads are page-cache hot, and the fanout runs after the lock releases, below the write's fsync floor. A cache would have to live across processes and re-validate exactly what the TTL and re-stat already check.

## Making a performance change

1. **Name the thread the cost lands on.** If it is the render/input loop, the change is wrong until the work moves off it.
2. **Prefer a push to a shorter poll.** A tighter poll burns cycles while the room is idle; a wakeup costs nothing until something changes.
3. **Decide durability explicitly.** Durable state goes through `write_temp_then_rename`, which fsyncs; a cache rebuilt on the next tick goes through `write_temp_then_rename_cache`.
4. **Keep single-flight.** A new fetch trigger goes through `FetchDispatcher` in [`app/fetch.rs`](../../crates/rimz/src/sidebar_pane/app/fetch.rs) (`request`, `request_or_defer`, `defer_until`), never a bare send or spawn, so it merges with the rest.
5. **Measure the idle case.** A change that speeds the busy path by adding idle work is usually a net loss.
6. **Prove it.** `cargo xtask ci` stays green, the tick budgets and the cost map move with the change, and `cargo xtask perf` refreshes the figures when the change touches a measured path. A before/after comparison rests on the bench's allocation counts; quote a wall-time delta only from runs on an idle host, since medians move with load ([Benchmarks](#benchmarks)).
7. **Measure the file you fixed.** For an I/O fix, `strace -f -e trace=openat` filtered on the target path across several ticks shows whether that file is still re-read; `/proc/<pid>/io` `rchar` is the process total, mixing every other read, so report it beside the per-path count, never alone.

### Perceived response time

Responsiveness is what the user feels: a frame that paints now with slightly stale data beats a fresh frame that arrives late. Reach for these levers in order.

1. **Acknowledge before you finish, unless one source of truth is worth the wait.** Where the local intent is the truth, paint the instant input lands: a browse pick and the help overlay paint synchronously in `LoopState::apply_input`. The jump is the deliberate exception. It changes no local selection; its detached thread writes a focus intent that every renderer, this one included, presents through the fold ([state.md → Focus intent](./sidebar/state.md#focus-intent)), so a stale frame cannot roll the highlight back.
2. **Animate on the wall clock, never on IO.** The spinner advances from the loop's frame clock (`frame_clock`) on a fixed interval whether or not a fetch is in flight. Motion that stalls while data is slow reads as a hang.
3. **Separate smoothness from freshness.** When a "make it feel snappier" request lands, first decide whether it is a frame-rate problem or a data-latency problem. They are fixed in different layers, and conflating them turns a cosmetic tweak into a CPU regression.
4. **Keep session-wide effects off the frame path.** A redraw writes only to the sidebar's own pane and is safe at any rate. A mux action touches the whole session and can reset an unrelated pane's cursor blink, so it belongs on the event-driven data layer.

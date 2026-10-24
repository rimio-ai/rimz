# Performance

> This page is the cost model that sits over the mechanisms: what the workload asks for, where each cost lands, which bound holds it down, how the bounds are guarded, and the rules a performance change follows. The mechanisms themselves live on their own pages: the data plane and every cadence value in [state.md](./sidebar/state.md), the write and read paths in [store.md](./store.md), the backend roster in [multiplexers.md](./multiplexers.md), and the spend walker in [spending.md](./agents/spending.md). To measure a running fleet, use [profiling.md](./profiling.md). To read what RimZ recorded about its own faults, use [diagnostics.md](./diagnostics.md).

## The workload

RimZ watches a fleet of agents for one human, and that job has three traits every cost is judged against.

Tens of writers share one reader. Every agent hook appends to one workspace store, often at the same moment, while one sidebar per tab reads the whole room.

The room is idle most of the time. A fleet emits in bursts and then waits on a model or a human, so idle is the common case, and work added to the idle path is paid all day.

One tab is watched. A room runs one renderer per tab, and the human looks at one of them.

Those traits give three goals that pull against each other, so each lives in a different place:

| Goal | Where it lives | What it demands |
| --- | --- | --- |
| Low perceived latency | The render/input loop | Never block, drop a keystroke, or freeze the spinner. Showing data one tick stale is allowed. |
| No write contention | The store write path | Hooks append raw ingress under a shared lock and leave lifecycle work to an elected drainer. Workspace-lock waiters use short retries for brief releases and backoff for long holds. |
| Near-zero idle work | Every lane | No polling spin, no per-frame fork, no directory rescan. |

One rule sits under all three: correctness lives in the store, never on the render thread. A performance change may leave the UI stale by a tick. It may never make the UI wrong, and it never trades a durability or CAS invariant for latency.

## Where the work runs

The design keeps the three goals apart by putting them on separate threads, separate processes, and separate clocks.

### The render thread

Each tab's sidebar pane runs one `rimz sidebar serve` supervisor, and one `rimz sidebar host` per mux session paints every pane of the room ([`sidebar_pane::host`](../../crates/rimz/src/sidebar_pane/host.rs)). The supervisor keeps the pane's terminal modes and input, and owns reload convergence. The host runs one render/input loop per pane, each on its own thread, over one set of data-plane threads, so a room pays the per-process cost of a renderer once rather than once per tab. The host is the only painter. An unavailable host costs a supervisor notice and a bounded retry, never another data plane ([state.md → Host unavailable](./sidebar/state.md#host-unavailable)).

Panes share the fold by reference. Each pane holds the shared fold's agent-scaled collections, group vector, and rows through shared handles, and `project_local` copies only the pane's local metadata. A group edit detaches the vector and only the row collections it changes. Ordinary snapshot cloning therefore has no term in agent, group, or history count.

A render loop spins the animation, applies input, fuses overlay events, and projects a fold the fetch worker finished. Each pane has two bounded lanes of 64 wakeups, one for room events and fetch results and one for supervisor input ([`WakeupInbox`](../../crates/rimz/src/sidebar_pane/app/inbox.rs)). The loop drains pending input first and otherwise waits in `recv_timeout` on the room lane, and that timeout is its frame clock. It never fsyncs and never reads the event log or a snapshot. A mux action the loop starts (a jump, a width nudge) runs on a short-lived detached thread.

The loop forks in one place: the tmux graphics-capability probe. `LoopState::run_maintenance` calls `RoomCaps::detect` ([`sidebar_pane/pixel/probe.rs`](../../crates/rimz/src/sidebar_pane/pixel/probe.rs)) synchronously, under the host-wide `RoomCaps` mutex. A due sample runs tmux client, version, and pane-option queries, each with a 500 ms command timeout, at most once per `CAPS_REFRESH_INTERVAL` (10 s) for the whole room. The other panes wait on the mutex while it runs. Zellij rooms never take this path.

The data-plane threads, one set per host, are the fetch worker, the cache refresher, the observer writer, the tmux control-mode watch, and the transcript watch. The terminal event reader lives in each pane's supervisor. Their responsibilities are in the thread table in [state.md](./sidebar/state.md#the-room-data-plane).

### One host, many panes

The external reads a room needs (the pane roster, git, the forge, provider accounts) cost the same whether one tab asks or twenty do, so one room host pays them for the fleet.

The room host runs one data plane for every pane. It retains panes, agent projection, and metrics history in memory instead of reading its own publications back each cycle. Passive CLI readers fold the published caches through [`PublishedSnapshotReader`](../../crates/rimz/src/sidebar/consumer.rs) without probing mux, git, or providers. A producing inspection (`rimz sidebar snapshot` without `--no-produce`) and message recipient resolution read the mux privately, and the inspection also refreshes heavy lanes. Neither publishes the host-owned inputs.

N tabs therefore cost one set of external reads, and each room's host folds once for all its panes. A wake reaches every pane, and each pane asks the shared fetch worker for a fold. A fold answers every request whose cause a pane observed before the fold started, and both the worker and the panes drop a request the last fold answered ([`sidebar_pane::app::fetch`](../../crates/rimz/src/sidebar_pane/app/fetch.rs) `Coverage`). A burst whose wakes all land within one fold costs at most two folds for any tab count. A wake that lands later is indistinguishable from a new change and costs one more. This is the largest lever in the design: in external reads, a 20-tab room costs about what a 2-tab room costs. The shared data plane lives in [state.md](./sidebar/state.md#the-room-data-plane), and the account-global spend walker has its own election in [spending.md](./agents/spending.md#one-walk-per-namespace).

Heavy-lane external reads also single-flight across processes through [`disk::single_flight::coalesce`](../../crates/rimz/src/disk/single_flight.rs), so a CLI refresh racing the host shares that lane's TTL window. Pane-frame production has no cross-process election: the host alone publishes, and CLI callers compute privately.

### Two clocks

Smoothness and freshness run on separate clocks, so tuning one never taxes the other.

The base frame grid is set by `[theme.display] refresh_ms` (100 ms by default), and a wheel glide briefly uses a finer grid. A frame redraws from the cached snapshot. The data clock is event-driven, with a one-second backstop tick (`rimz sidebar serve --tick-seconds`), and it folds new truth when a wakeup says there is some. The fetch worker rests `FOLD_FLOOR` (250 ms) after each cycle returns before it starts the next, whatever the request, which bounds its duty cycle. The clamps, animation cadences, and hidden-tab paint rules are [state.md → The paint clock](./sidebar/state.md#the-paint-clock).

Durable truth and the pane roster are split the same way. Every fold reads the store rollup event-fresh, so a status flip repaints within one wakeup. The pane roster sits behind a TTL cache, so discovery stays cheap. A writer that knows about a change posts a typed wakeup, and polling only backs up a missed one.

### The three end-to-end budgets

Every row of the cost map rolls up into one of three paths, each named with its dominant term so a regression shows at review.

| Path | Budget | What happens |
| --- | --- | --- |
| Keypress to pixel | One in-process paint; at most eight for a wheel glide | Input applies synchronously and paints. Consecutive queued wheel ticks share the first paint, followed by time-based eased frames for at most 100 ms ([state.md](./sidebar/state.md#the-paint-clock)). The path takes the shared view lock and reads three small runtime records (read marks through a stat-keyed cache, the body filter, and the focus anchor). It forks nothing and reads no event log or snapshot. |
| Pane event to pixel | One fuse plus one off-grid paint | A typed overlay event (`PaneClosed`, `FocusChanged`) lands in the in-memory event store, re-fuses the held frame, and paints at once. The producer's next pull confirms it ([state.md](./sidebar/state.md#fusion-rules)). |
| Write to pixel | Up to the worker's 250 ms rest, one fold, then the next dirty frame | Applied events wake the panes, which request a shared fold and paint on the next dirty frame. Hook ingress must first pass through the drainer, and no frame budget bounds that queue. |

## Principles

A performance change follows these rules. An earlier rule outranks a later one when they conflict.

1. **The render thread never blocks.** No fork, fsync, or roster read on the loop. Move the work to a worker and wake the loop when it finishes. The tmux graphics-capability probe ([The render thread](#the-render-thread)) is the one standing exception, and new code does not add a second.
2. **The frame never drags the fetch.** Responsiveness is a redraw from the cached snapshot. A smoothness change shortens the frame interval and pays in-process paints. A freshness change belongs to the data layer. A frame that triggers a roster-plus-git fetch spends a fork to move a spinner.
3. **Push, then poll.** A writer that knows about a change posts a wakeup, and polling is the backstop. A data source that feeds the UI without being the store (a context sidecar, a transcript) gets a wakeup of its own.
4. **Fsync truth, rename caches.** Durable records fsync. Everything derived (rollup caches, the snapshot cache, diff stats, context sidecars) is written with `write_temp_then_rename_cache`, which renames atomically without fsync, because it rebuilds from truth on the next read. Neither form allows a torn read. The cache form gives up only surviving a power cut ([store.md → Write classes](./store.md#write-classes)).
5. **Single-flight, then coalesce.** One fetch runs at a time, and a burst of requests collapses to one fetch. A request that races an in-flight fetch leaves at most one follow-up, and only when it carries new committed data (a store delta, or a deferred request it absorbed). Any other racing request is dropped, never queued.
6. **Pay the external read once per window.** A short TTL bounds pane discovery, git, and forge probes, and the last good result is reused. A failed probe backs off and keeps its last known good value. A degraded roster read backfills missing fields per pane instead of painting a corrupt frame.
7. **Take the cheapest correct read.** Catch-up is O(delta bytes) from the thread's held fold or the persisted fold base, never O(history). Skip work that cannot matter: unchanged inputs cost a stat, and a room with no agents skips the sidecar scans.
8. **One host per room, one render loop per pane.** The data plane and room event socket are shared, while every pane keeps its loop, heartbeat, and bounded wake lanes. Supervisors replace a lost host and reattach without an ownership election.

## The cost map

This section gives each lane's cost and the bound that holds it down. Reproducible figures come from `cargo xtask perf` ([Benchmarks](#benchmarks)). External IPC rows give ranges seen in production. Every cadence constant named below has its value and staleness budget in [state.md → Cadences](./sidebar/state.md#cadences). A value this page gives is one that page does not own.

### The render loop

| Operation | Cost | Bound |
| --- | --- | --- |
| Frame redraw | Sub-millisecond in process; 413 µs at 40 agents. Card painting scales with the viewport plus height-cache misses: one full paint per snapshot, clock, width, theme, focus, or card-configuration epoch, and the cards whose expansion, selection, or match changed within it | A cold epoch paints in one walk. A warm frame measures geometry from cached heights, then paints the window. The base `refresh_ms` grid is briefly finer for a wheel glide, and off-screen animation relaxes to the data backstop. Geometry still scans the roster, and a frame never starts a fetch |
| Overlay fuse | 75 µs with no active overlay, at 40 agents | Pure: no IO, no subprocess, no clock read |
| Jump to a pane | One mux client fork, tens to hundreds of ms | Detached thread. The focus intent it writes reaches the frame through the fold ([state.md](./sidebar/state.md#focus-intent)) |
| Tmux graphics capabilities | One room-wide client, process, and version sample and one pane-option listing, on the render loop | At most once per `CAPS_REFRESH_INTERVAL` (10 s) per room, under the shared `RoomCaps` mutex; each command times out at 500 ms. A missing pane or unavailable listing uses a targeted permission query, and escalation stays per pane |
| Zellij width observation | One topology parse and pane geometry table per changed file stamp; a metadata check and lookup thereafter | Shared per plane. Freshness and each pane's observation floor stay live ([multiplexers.md](./multiplexers.md#width)) |

### The fetch worker

| Operation | Cost | Bound |
| --- | --- | --- |
| Snapshot rollup | One stat while `cache/snapshots/latest.json` is unchanged and parse-cached; one whole-file parse after it is replaced; O(delta bytes) when writes outran it | The checkpoint's stamped `LogExtent`, whose offset a reader compares with the live log's length ([store.md → Truth and cache](./store.md#truth-and-cache)). A `RollupCursor` holds the parsed base: one per thread for store reads, and one owned by the sidebar fetch worker. The rotation carryover beneath it is parsed once per file identity per thread, from the stamped trimmed copy when that copy is current, and every fold shares it without copying |
| Event-log fold | Warm cursor: one stat of the log and one of the carryover, plus the appended frames; the projection clones only the rows it keeps. Unchanged log: shared handles only | Guard tests `delta_fold_is_o_new_bytes` and `warm_fold_parses_unchanged_carryover_zero_times` |
| Workspace fold | One enrichment per cycle; every pane projects from it by reference ([The render thread](#the-render-thread)), with no seed serialization | Host views retain the seed fold |
| Fold observations | One focus-anchor read and one body-filter read per real cycle, shared by every attachment | The painting view applies selection, confirmation, and read intent. A live nonce comparison guards retirement ([state.md](./sidebar/state.md#focus-intent)) |
| Pane roster (producer) | Zellij: one topology read plus a `zellij pipe` nudge. tmux: one session-wide `list-panes` call | `SNAPSHOT_CACHE_TTL` while a client drives it, `EVENT_PANE_TTL` while the presence stamp is fresh; one attempt per data tick |
| Process metrics (producer) | A due sample reads one metrics record per process | Per-pane focused and background stamps in `lanes/metrics-sample.json`. The full process-table walk runs only on pane churn |
| Agent projection (producer) | Unchanged tick: metadata stamps only, no directory enumeration | One batched discovery call per kind and one wiring probe per data tick |

### The host cache refresher

| Operation | Cost | Bound |
| --- | --- | --- |
| Live-session context | Stat-gated local reads, no context-helper subprocess | Every refresher pass. Rich session metadata refreshes at hook boundaries or on explicit refresh, and account windows through the per-login usage lane |
| Git diff stats per root | Unchanged HEAD on a clean tree: 1 `git` fork (`status`; the prior `+/-` carries forward). Unchanged HEAD on a dirty tree: about 2. Fallback or ancestry change: about 6 | `DIFF_STATS_TTL` hot, `DIFF_STATS_IDLE_TTL` idle, `DIFF_STATS_SETTLED_TTL` (5 min) for a clean, landed root nobody views, per root. A host pass refreshes every due focused and hot root, then fills up to `MAX_HOST_PASS_ENTRIES` (8) with the other due roots, oldest first (`sidebar/refresh/git_stats.rs::select_for_pass`). The one-shot CLI pass refreshes every due root. At most `MAX_PARALLEL_GIT` (8) run at once |
| Worktree roots | One `git worktree list` fork plus one marker read per root | `WORKTREE_ROOTS_TTL`; no child scans in a directory room |
| Owned-worktree condition scopes and timers | One repo check and one `git worktree list` per root on a cache miss; ownership markers read fresh each call | Shared process-local rows invalidate on the common `worktrees` directory's stat identity or after 60 s, which bounds external worktree moves. CLI discovery stays uncached |
| PR target assembly | In-process HEAD reads, including checkout subdirectories. Unresolved ref formats fall back to `git rev-parse --abbrev-ref HEAD`. One successful `git remote get-url origin` per common Git directory per config version | Host-retained memo keyed by the common config's stat identity. Missing origins are cached, and transient failures retry. Detached HEAD needs no branch subprocess. One-shot CLI refresh starts fresh. Global URL rewrites and per-worktree config changes alone take effect after a common config change or a host restart |
| PR state per repo | GitHub batches branch PRs and distinct HEAD rollups, plus one repository open-set alias in the first batch when a non-trunk target exists, within the 100-alias budget. Each condition-demanded branch adds a head-ref alias. Merge-queue state rides the existing open-facts alias and costs no alias or request, but a queued PR keeps its repository on the hot tier until it leaves the queue. Tea reads base branches from its one open-list call, so stacks cost no extra request. Checkout detail reads begin when a PR leaves the open set, while demanded branches share one all-state list and resolve terminal details from first sight | `PR_STATE_HOT_TTL` and `PR_STATE_TTL`. Failures back off and keep the last known map. Tea checkout closed links, and merged links with a settled CI verdict, are pinned ([state.md](./sidebar/state.md#pr-state)) |
| Provider accounts | Cold providers probe in waves of at most four account-then-version chains, each subprocess capped at 3 s | `ACCOUNTS_TTL` per login (`sidebar/refresh/accounts.rs::due_provider_logins`). Contention serves the stale cache instead of starting a second wave |
| Fleet spend walk | Within `SPENDING_TTL`: one read of the published aggregate, no transcript IO. A due walk: frontier stats plus O(appended bytes) per changed file | One warm walker per namespace owns the index. Consumers never open `spending.json` ([spending.md](./agents/spending.md#one-walk-per-namespace)) |
| Finished-cohort effort | Transcript stat and parse for collapsible groups only; unchanged files reuse a process-local parse memo | `COHORT_SPEND_TTL`. The producer publishes `lanes/cohort-spend.json`, and renderers only read it |
| Team pipeline | One board read per eligible staged-team group; no subprocess or event-log scan | Every refresher pass, no TTL. Writes `lanes/pipeline.json` only when content changes. The pass resolves team configuration (`SeatPolicy::load`) only when some agent carries a team or a profile, and shares that resolution with idle compaction |
| Codex daemon reap | Zero unless Codex remote control is on or a daemon-hooked session is live. When due, per in-use Codex login: one daemon process scan, plus a WebSocket handshake when that login's daemon is found | `CODEX_DAEMON_REAP_TTL`; success and failure share the stamp |

### The store write path

The choreography and write classes are [store.md → The write path](./store.md#the-write-path), and the ingress lock and drain cursor rules are [store.md → Hook ingress](./store.md#hook-ingress).

| Operation | Cost | Bound |
| --- | --- | --- |
| Hook ingress | One framed append of raw payload and captured environment under the shared ingress lock, no lifecycle fold | Appenders block in the kernel and never exclude each other. Cost is payload-sized. First creation syncs the parent directory |
| Derived lifecycle append | One ordered-batch event-log `write()` per applied frame, zero steady-state fsyncs | `store_fsync.rs` pins the zero fsyncs. `store_bytes.rs` pins one representative lifecycle turn at 1 KiB or less; the event log itself enforces no frame size |
| Drain cursor | Two durable cursor publications per applied frame, plus synced truncation and cursor reset when the queue empties | Each publication is a temp file and two fsyncs under the workspace lock. The elected drainer applies frames serially in environment-scoped children |
| Event-log durability | One group `fdatasync` per second per workspace (`LOG_SYNC_INTERVAL`) | Runs on the off-lock tail. Cursor and truncation barriers are additional |
| Snapshot publish | Rename-only rewrites of the rollup cache and `latest.json`, plus the carryover's stamped trimmed copy once per carryover file identity per process | Single-flighted on the off-lock tail and debounced to once per `PUBLISH_INTERVAL` (1 s). Due sooner when the unpublished tail reaches `PUBLISH_BYTE_BUDGET` (64 KiB) or the log shrinks (`store/writer/publish.rs::should_publish`) |
| Wakeup fanout | One heartbeat directory scan plus one datagram per distinct room socket | The scan reads one heartbeat per live sidebar. Every attachment of a room host names the same socket, so the datagram count is the live room host count. The reads are page-cache hot and cost less than the fsync floor |
| Durable file write | Temp file plus two fsyncs (file, parent directory) | Trust grants, the workspace record, hook installs, and the per-frame drain cursor |

### Everything else

| Operation | Cost | Bound |
| --- | --- | --- |
| Sidebar observer | One O(rows) signature pass per committed fold, microseconds | Pure detection at every renderer's fold commit point. The host writes observer records and adds one cross-check pass per `OBSERVE_CROSSCHECK_TTL` (5 s) ([diagnostics.md](./diagnostics.md#the-frame-stream-observer)) |
| Tick meter | Six relaxed counter loads and two clock reads per metered tick | Healthy ticks do no file IO and spawn nothing |
| Sidebar heartbeat | Temp file plus atomic rename | Every `HEARTBEAT_WRITE_INTERVAL`, inside the liveness TTL |
| Room read marks | One stat-keyed parse per thread per record change | Full atomic-file identity of `live/read-marks/marks.json`. Pane sweeps read the current record, never an older shared cut |
| Codex session index | One `stat()` of `session_index.jsonl` per live Codex session per refresh; one full parse per index change per process | Full file identity (length, mtime, device, inode): appends grow the file and thread deletes rewrite it ([adapter_codex.md](./agents/adapter_codex.md)) |
| Reload poll | One `stat()` of the durable reload record per sidebar per second | Executable hashing runs only after that record's metadata changes. The embedded presence-plugin digest resolves once, lazily ([sidebar.md](./sidebar/sidebar.md#build-promotion)) |
| Remote link probe | One JSON probe over the existing SSH ControlMaster every 2 s | Supervised remote attach only; `RIMZ_REMOTE_PROBE_MS=0` disables it |
| `loop watch` repaint | One room's catalog, pause, and terminal reads once per second. Run-log cost is one `stat` per generation per repaint, reading appended bytes only. Plus one `locks/` listing for that root, which opens and try-locks every `loop-run-*.lock` file there under the directory's lease gate, free ones included | Workspace identity resolves once before the loop. The entry check reads the verified room pin and sidebar heartbeat once, without another workspace probe. The run-log fold fully re-reads on rotation, a new local day, or a time-zone change. The lock cost grows with stale run-lock files until `rimz gc` removes the free ones |

`WORKTREE_ROW_CAP` (6) caps the idle and process tail in the renderer, never the snapshot. Active, paused, blocked, finished, focused, and unread rows render past the cap, so jump targets and unread convergence stay visible. The observer's pass scales with the full roster, and the render and selection walks scale with the visible set. Both stay bounded by live pane count across the 20 to 100 agent target. If that bound loosens, the fix is row virtualization in the renderer, never hiding rows from the snapshot.

## Overhead at fleet scale

RimZ is sized against a single agent: watching a hundred agents should cost a small and nearly flat fraction of running one of them.

Cost attaches to three units: the workspace, the worktree, and the agent. Hook application scales with event volume on top of all three. Moving it out of the foreground does not remove its fold or IO cost.

Per workspace, RimZ pays once. The host pays the roster and metrics on its fetch worker and git and accounts on its cache refresher, then folds once for all its panes. Every workspace that shares a state root and provider-discovery environment asks the same warm spend walker, so a host restart adds no second parsed cursor. One elected hook drainer per room owns the ingress queue. When quiet it blocks in poll(2) on its listener and client sockets (`crates/rimz/src/harness/hook_drain.rs::run`), so an idle drainer and the leases it holds open add no periodic wakeups. It exits after `DRAINER_IDLE` (60 s) without frames or connections. Its election, sockets, and shutdown order are [ARCHITECTURE.md → Runtime shape](../../ARCHITECTURE.md#runtime-shape), and its apply children are [adapter.md → The feed](./agents/adapter.md#the-feed).

Per worktree, cost follows activity. The git input set scales with distinct group roots, not agents. A root drops to its idle TTL once its agents go quiet, and to the 5-minute settled TTL once it is also clean and landed. A host pass always refreshes the due viewed and hot roots and gives the others what is left of its 8-root cap, so a cold start or a clump of expiring stamps spreads over several passes. Under sustained overload an idle root can outlast its TTL. PR probes scale with origin repositories, and each enumerates open PRs once when due. A hundred agents sharing a few checkouts pay for a few hot roots.

Per agent, cost is event-driven. An agent reports through a short-lived `rimz hooks feed` child that appends when something happens and exits. Every interactive launch retains its wrapper. A Unix wrapper re-execs into a parked image with no store and an 8 MiB private (anonymous) memory budget, guarded by the live `backend::tmux::agent_lifecycle::parked_wrapper_holds_under_8_mib` test and the thin-image invariant. Executable text is shared across wrappers, and the test bounds `Pss_Anon` and prints total `Pss` beside it. Fold-dependent duties run in helper children: strand repair is resident only for its parked nonterminal phase, while receipt, parent confirmation, and card evidence are one-shot. A `--keep` subagent's settled linger is outside this budget ([the exec wrapper](./harness/fleet.md#resident-wrapper)).

Two debounced costs stay flat in agent count: event-log group `fdatasync` and snapshot publication. Both scale with rooms. Drain cursor publications are separate, per-frame durable writes outside either debounce. The hot runtime caches live under `$XDG_RUNTIME_DIR`, normally a tmpfs, so their churn is memory traffic. Without that variable RimZ falls back to `/tmp/rimz-<uid>` (`disk/paths.rs::runtime_home_from`), whose churn reaches whatever filesystem holds `/tmp`.

Display-plane CPU and RAM estimates across a fleet of 2 to 5 rooms follow. They exclude the elected hook drainer and its serial apply child, whose cost follows hook volume rather than fleet size.

| Resource | 20 agents | 50 agents | 100 agents | What sets it |
| --- | --- | --- | --- | --- |
| CPU, idle | ~0 | ~0 | ~0 | Loops block in `recv`; off-screen animation pauses |
| CPU, busy | <0.3 core | ~0.3 to 0.8 core | ~0.5 to 1.5 core | One producer per room, bursting on its git pass, never on the render thread |
| RAM, resident | ~80 to 150 MiB | ~100 to 180 MiB | ~120 to 220 MiB | One host per room plus a small supervisor and local snapshot metadata per tab, shared rosters and rows, one spend walker, room-local rollups, prepared cell-pet grids |
| Durable write | Event-volume dependent | Event-volume dependent | Event-volume dependent | Derived lifecycle frames, about 1 KiB per turn; raw ingress payload and environment; cursor publications |
| fsync rate | Event-volume dependent | Event-volume dependent | Event-volume dependent | Group event-log barrier plus four cursor fsyncs per applied frame and three barriers per drained-queue reset |
| State on disk | Tens of MiB | Tens of MiB | ~100s of MiB | Rotation-capped event log, ~5 KiB of snapshot per agent, and the pending ingress queue, truncated after drain |
| Network | Weekly pricing fetch, OAuth usage probes per metered login, forge open-set probes per due repo | Same | Same | Pricing is fleet-shared and single-flighted. A login's usage probe runs every 5 minutes, every hour after a settled auth failure, and no more often than every 3 hours for an idle account ([providers.md → Refresh cadences](./agents/providers.md#refresh-cadences)). Local datagrams carry the rest |

Against the agents it tracks, that overhead is a rounding error. One developer's week of Claude and Codex sessions produced 1.23 GiB of transcript JSONL (about 177 MiB a day), with each agent process resident at 250 to 340 MiB (Claude) or 50 to 65 MiB (Codex). RimZ watched the same fleet with tens of MiB of durable state, a resident set about the size of one agent process, one fsync a second per room, and one pricing refresh a week.

Remote render-stream bytes sit outside this CPU and memory budget: SSH carries whatever the visible full-screen TUIs repaint. The sidebar's own share is held by the [client-byte guard](#live-client-byte-guard): a cell-only frame costs the attached tmux client at most 1 KiB. A synchronized-output bracket around every frame makes tmux repaint the whole pane instead, about 17 KB per frame on a 60x69 pane (tmux 3.7c, 2026-10-07). On a 50x119 pane, that bracket on every frame delivered 496,667 bytes in 10.066 s, against 19,963 bytes in 10.114 s with graphics-only brackets (about 49 KB/s against 2 KB/s). These are attached-client bytes, not the renderer's producer write rate. Idle RimZ surfaces send close to nothing, and a busy agent TUI commonly sends tens of KB/s. `rimz pane bandwidth` reports each pane's producer write rate beside the room's SSH socket payload (`WIRE(ssh)`), which is usually far below the per-pane sum ([reference](../reference/cli/pane.md#bandwidth)).

## What's optimized

Each mechanism that holds a cost down, named once with its code home. Where another page owns the mechanism, this section says what it saves and links there.

### The frame is decoupled from the fetch

The fetch worker publishes room folds into views. The render loop waits on its room lane and reduces when the worker's `snapshot` wakeup arrives. The animation tick redraws from the cached snapshot and never fetches, so a missed push degrades to the backstop tick instead of a poll storm. [`animation_cadence`](../../crates/rimz/src/sidebar_pane/render/animation.rs) classifies row motion: fast work stays on the base grid, row animations redraw on `BREATH_ANIMATION_FRAME`, and a dirty data fold pulls the next frame back to the base grid so freshness never waits on cosmetic motion. A wheel glide temporarily takes the finer grid ([state.md](./sidebar/state.md#the-paint-clock)).

### Only the watched tab animates

Each renderer knows its own pane and the latest client map, so it can tell whether a client is looking at its tab. A watched tab keeps the normal grid. An unwatched or detached tab skips the animation roster scan and wakes on the data backstop. Every pane requests event-driven shared folds, and the worker coalesces their requests. A settled hidden attachment leaves publications unread in views until reveal, without a time bound. Pane-local reconciliation obligations still apply, but hidden, held, and anonymous unpainted views do no reductions. Freeze expiry sleeps while hidden, and view mutations wake no unrelated panes. Arrival catches up the routed client view before its first paint, preserving its last painted order and any existing hold deadline. Hidden resize changes geometry only. When ownership is unknown the renderer counts as watched, so tests, demos, and cold starts keep the responsive path. Deferral exceptions and hidden paint rules are [state.md → The paint clock](./sidebar/state.md#the-paint-clock).

### One shared fold per room

The producer builds the snapshot in process on its fetch worker ([`produce_workspace_snapshot`](../../crates/rimz/src/sidebar/produce/mod.rs)), with no `rimz sidebar snapshot` fork per tick. The worker's cadence stamp allows one attempt per data tick, and only against a stale retained frame, so unrelated wakeups cannot multiply production.

The fold spine splits into renderer-independent [`enrich_workspace`](../../crates/rimz/src/sidebar/enrich.rs) and renderer-local `project_local`. The worker folds the first half into host views, and each attachment projects at reduction. The retained fold seeds new attachments. Both lanes are [state.md → One fetch cycle](./sidebar/state.md#one-fetch-cycle).

### Truth arrives by event, the frame by coalescing

The host delivers produced folds directly. The rollup is read event-fresh from `cache/snapshots/latest.json` on every fold, so a status change repaints within one wakeup while the host retains and shares its pane inputs.

Every writer that knows about a change pushes. Store and sidecar writers post a `StoreDelta`. Both backends' presence streams feed one projector that emits typed pane events. The host's transcript watcher refreshes context mid-turn for every adapter that declares transcript-tail context ([state.md → Push channels](./sidebar/state.md#push-channels)).

### No reader pays for history

The rollup persists a raw fold base stamped with its log extent, and catch-up seeks to the stamped offset and folds only new frames ([`fold.rs`](../../crates/rimz/src/store/snapshot/fold.rs), [store.md → The read path](./store.md#the-read-path)). Runtime projection, resume outcomes, and smart-compact dedupe ride the same fold instead of rescanning `log/events.log.jsonl`.

A per-thread parse cache returns `Arc<T>` handles, so an unchanged file costs neither a re-parse nor a deep clone ([`disk/parse_cache.rs`](../../crates/rimz/src/disk/parse_cache.rs)). `lanes/snapshot.json` and `cache/snapshots/rollup.json` are keyed on `(path, mtime, len)`. `cache/snapshots/latest.json` and `records/agents-carryover.json` are keyed on the full `(len, mtime, dev, ino)` identity, which tells same-size replacements apart.

The carryover's cached form is fold-ready: identities backfilled, rows key-sorted, and only a bounded first-line prompt label kept on ended rows, while the durable record stays unchanged. Its reborn base is cached once per parse, and every fold layers live rows over the shared base without copying all history. A corrupt replacement is never cached, so every fold reports it until the file is replaced. The [read-path contract](./store.md#the-read-path) owns the prefix bound, hydration, the explicit `Store::load_full_agent` read, and the accepted edge where in-place corruption that keeps the source stamp stays masked by the derived copy.

The spend walk is incremental in three layers. The walker's directory index stats only active frontiers and reconciles fully every 15 minutes. The disk cache keeps a cursor per file, so a grown file parses only its appended suffix. The one elected walker holds the only parsed cache, so workspace requests borrow instead of cloning. Aggregation uses hash collections for session uniqueness and keeps deterministic order only in the published maps. The cache layout is [spending.md → The incremental cache](./agents/spending.md#the-incremental-cache).

### Per-enrichment cadences

Every display figure is display-only by invariant ([DESIGN.md](../../DESIGN.md#triage-at-a-glance)), so each enrichment runs on its own cadence behind stamps stored in the cache file it already writes. Because the stamps live in shared files, every process agrees on freshness across a producer handoff. Account probes carry one stamp per login, so a failure retries alone. Process sampling keeps per-pane focused and background stamps, independent of the roster clock. Git probes take activity-tiered TTLs whose hotness comes from store activity, with no filesystem watching. In-process `.git` ref reads skip the ancestry forks when the cached HEAD and trunk pair and the clean verdict are unchanged. The values are [state.md → Cadences](./sidebar/state.md#cadences).

### Append and drain critical sections

The foreground hook never waits on the workspace lock for an ordinary append. Its ingress lock is shared and kernel-blocking, independent of the workspace lock that lifecycle batches and cursor publications take, so appenders never queue behind each other or behind a fold. A due `PostToolUse` deadline rung can still take the workspace lock. Workspace-lock waiters retry at 1 ms for the first 25 ms, then back off exponentially to 50 ms. Not every workspace-lock hold is a microsecond append or fsync-free. Every fsync goes through `disk/atomic.rs`, which the `cargo xtask invariants` check `ensure_store_durability` enforces for `sync_all` and `sync_data` calls. The lock order, repair, and framing are [store.md → Hook ingress](./store.md#hook-ingress), and the durability classes are [store.md → Write classes](./store.md#write-classes).

### Helper processes stay cheap

The build id is computed once per process from the linker build identity in the first 1 MiB of the image, and the whole image is hashed only when that identity is missing. `main` warms it off-thread at startup ([`build_id.rs`](../../crates/rimz/src/build_id.rs)). Self-spawns resolve through one helper, `rimz_exe`, which honors `RIMZ_BIN` and repairs Linux `current_exe()` paths ending in ` (deleted)` after an atomic reinstall. The Zellij presence plugin hashes the stable manifest fields before projecting pane fields, so title-only `PaneUpdate` storms skip projection ([`policy.rs`](../../crates/rimz-presence-zellij/src/policy.rs)). Zellij rooms start with `disable_session_metadata true` by default, which stops the server rewriting `session-metadata.kdl` and running command-discovery `ps` for the room.

### Codex context stays warm

Codex app-server enrichment for the room-default login starts with a broker in the `rimzd` tab that holds one handshaked `codex app-server`, and falls back to a cold spawn. The broker's answers skip the per-datapoint child handshake only while it serves the login. A broker that does not confirm the login (an older binary, or a child on another home) costs one `initialize` round-trip before the daemon and cold-spawn attempts. Other logins skip the room broker, and periodic usage refresh uses only their direct probe ([adapter_codex.md → App-server enrichment](./agents/adapter_codex.md#app-server-enrichment)). The host's transcript watcher debounces file events to one flush per 300 ms for the workspace and refreshes each changed session in that flush. Periodic session passes back up the watcher with local reads only, and they never spawn app-servers.

## Anti-patterns

Each of these has cost RimZ a real regression. They are grouped by the principle they break, because the principle is what transfers.

**Cosmetics must not drag data (principle 2).** A fixed refetch every 500 ms to keep a working agent's cost figures moving forks a subprocess per frame to move a spinner, and on Zellij its roster read resets unrelated panes' cursor blink. Context-sidecar pushes cover the same need for free.

**Recovery belongs to the host and its supervisors (principle 8).** Letting each attachment run its own production on stale data creates an N-way storm on exactly the tick the room is already degraded. Per-tab roster reads pin the mux server with N round trips. Capping renderers instead of production blacks out every tab but one.

**Shared state has one owner (principle 5).** Per-host spend walkers duplicate the account-global parsed cursor. Readers that derive workspace spend from the global cursor make every renderer parse `spending.json`. Per-thread heartbeat scans make each thread decode every renderer heartbeat. The fix in each case is one shared service or process-local owner that the others read.

**Unchanged inputs cost nothing (principle 7).** Independent attachment folds multiply config reads, enrichment, and published JSON clones with every input unchanged. Background metrics publications must share one fold across attachments, never force a full fold per hidden pane. Scanning a provider-global session tree (`~/.codex/sessions`) once per worktree traverses the same directories many times in one fold.

**Every retry is bounded (principle 6).** A deterministic `gh` or `tea` failure retried on the hot TTL, with no command deadline and the cached map erased each time, turns a forge outage into a permanent per-worktree fork loop. An auth probe whose result is recorded under a different scope than the one requested reads as a new login on the next fold and spawns another helper. Both need backoff that keeps the last known good value.

**The hook is on the agent's turn.** A foreground hook that folds the room and runs lifecycle effects holds the agent for the whole fold. On a frozen live-sized room copy (3.7 MB cache, 84 MB log), a sequential Codex `Stop` that did so took 278 ms on average and up to 363 ms, and 24 concurrent Stops averaged 1.8 s. Appending raw ingress and nudging the drainer returns in about 3 ms on the same copy. The fold and effects still run, in the drainer: draining ten sequential hooks took another 2.9 s. The ingress lock is shared for the same reason. With an exclusive ingress lock polled in 1 ms sleeps and a serial warm-up, appenders queued behind each other: 24 concurrent hooks reached 24.2 ms against 7.9 ms for `--version` and missed the [hook budget](#the-hook-budget).

**Cheap-looking work multiplies by fleet size.** Deriving the merge base separately for every group root costs about 240 `git` execs on a 20-worktree fixture. A warning emitted once per fold per renderer, with Sentry's `attach_stacktrace` on, makes every consumer symbolize a backtrace before the rate limiter can drop it (about 200 MiB per fold against 18 MiB clean). Streaming serde output straight into a socket turns each JSON token into a syscall.

**An atomic reinstall leaves deleted inodes behind.** A long-lived renderer's `current_exe()` resolves to `rimz (deleted)`, so a naive self-spawn fails `execve`, and a supervisor parent holds the deleted inode until session end while its children orphan. `rimz_exe` repairs the suffix, supervisor-owned reload convergence re-execs parents onto the new binary, and supervisor-side reaping clears the orphans.

**A 2-arena cap serializes malloc in a many-threaded long-lived process.** With glibc capped to two malloc arenas, the sidebar host's 123 threads queued on the arena locks: a profile of a 57-window room put 48% of its on-CPU samples under `__lll_lock_wait_private`. `crates/rimz/src/main.rs::malloc_arena_cap` gives the host 16 arenas and keeps 2 everywhere else. On a 57-window tmux sandbox room with 5 `store_delta` events per second and only stub agents, fresh hosts measured, over 3 × 10 s windows each:

| Host arena cap | Hosts | Mean CPU (run means) | USS after about 1 min |
| --- | ---: | --- | --- |
| 2 | 3 | 27.7% (26.1 to 30.3) | 51 MiB |
| 8 | 6 | 19.1% (18.6 to 20.1) | 53 MiB |
| 16 (shipped) | 6 | 16.9% (15.2 to 18.4) | 56 MiB |
| no cap (8 × cores) | 6 | 15.0% (13.7 to 16.6) | 62 MiB |

The 2-arena row comes from an earlier session of the same harness, where cap 8 averaged 18.1%. Cap 4 averaged 22.8%. Every cap-8 run averaged above every uncapped run, while cap 16 overlapped them. The host stays capped because glibc's default, 8 arenas per core, bounds nothing on a many-core machine over hours of uptime. Lift another process's cap only after a profile of that process shows the same lock wait.

## Deferred and rejected

Wins identified but not taken, because each changes a contract or crosses a backend-parity boundary, ranked by expected payoff:

1. **tmux `list-panes` over the held control client.** The host's `tmux -C` client already writes commands and parses reply blocks, so sending `list-panes` over it would remove the producer's `list-panes` fork and connect. The saving is 10 to 30 ms on the already-cheap backend, and the poll must still cover a dead watcher.
2. **Delta-bearing wakeup datagrams.** `StoreDelta` could carry the appended frames, so a warm consumer folds from the datagram with no file IO. The warm cursor fold is already one stat plus a page-cache-hot read, so the win is microseconds against a second delivery path for state that must never become truth. Build it only above sustained hundreds of events per second.

Two costs belong upstream, and RimZ only bounds them:

- **Zellij topology freshness depends on the presence plugin.** A missing, denied, or wedged plugin leaves a Zellij room holding its last good frame while `rimz doctor` names the failing precondition. A watchdog belongs in the presence channel, not in a CLI fallback.
- **Zellij compact-bar and scrollback footprint.** A 100-pane room carries one compact-bar wasm instance per tab and large server-side scrollback. RimZ can recommend configuration and avoid adding pressure. The costs belong to Zellij's plugin and scrollback model.

Evaluated and rejected, recorded so the next pass does not reopen them:

- **Lock-free `O_APPEND` event appends.** Recovery assumes the lock makes the log single-writer, so only the trailing frame can tear. Concurrent appenders' dirty pages can write back out of order and leave a zeroed frame in the middle, which rebuild correctly treats as a hard error. Making that safe needs per-frame magic for resync, all to shave a lock hold that is already microseconds.
- **Binary snapshot format.** The parse cache removes re-parsing on delta storms and the `RollupCursor` holds the parsed base in memory, so a binary checkpoint would speed up a parse that does not happen. JSON keeps `rimz sidebar snapshot --json` inspectable.
- **Caching the wakeup heartbeat scan.** N is live sidebars, the reads are page-cache hot, and the fanout runs after the lock releases, below the write's fsync floor. A cache would have to live across processes and re-validate exactly what the TTL and re-stat already check.

## Guarding it

Three layers hold the cost map in place: a live meter in every renderer, exact counters in the test suite, and benchmarks.

### The tick budget

The fetch worker and the cache refresher each meter their ticks against budgets in [`sidebar/meter.rs`](../../crates/rimz/src/sidebar/meter.rs):

| Budget | Limit |
| --- | --- |
| In-process wall time (`wall_ms - mux_wait_ms`) | One configured data tick |
| Mux subprocess wait | 5 s (`TICK_MUX_WAIT_BUDGET_MS`) |
| Fold bytes | 256 KiB (`TICK_FOLD_BYTES_BUDGET`) |
| Spawns | 32 (`TICK_SPAWN_BUDGET`) |

Byte and spawn limits are absolute because they describe the shape of a storm, not a cadence. Fork and byte counts are attributed to the loop that caused them, so the two metered loops do not charge each other.

Five consecutive over-budget ticks (`TICK_BUDGET_BREACH_TICKS`) write a `tick_budget_breach` diagnostic ([diagnostics.md](./diagnostics.md)) and emit one `warn!` through the observability bridge. The same streak window filters recovery, so one cheap tick inside a saturated episode does not flap the record. A cache-refresh record also names which refresh step the spawns came from (`spawns_by_step`), so a spawn breach points at a lane instead of at the pass. The meter only observes, and the tick's work proceeds unchanged.

The budgets mirror the cost map. A change to the cost map revisits them in the same PR.

### Counter gates

The deterministic gates pin exact integers, never time. They live in `crates/rimz/tests/integration/performance/`, which the `gate` nextest profile includes, so `cargo xtask gate` and `cargo xtask ci` both run them.

| Gate | What it pins |
| --- | --- |
| `store_fsync.rs` | Zero fsyncs on a warm append, and one group sync per interval for a write burst |
| `store_bytes.rs` | One representative lifecycle turn at 1 KiB or less of event-log bytes |
| `concurrent_writers.rs` | A write burst's event-log bytes read and written stay independent of archived history |
| `concurrent_stops.rs` | 24 concurrent Stops through `rimz hooks feed` against a live `rimz hooks drain`, on an empty and a 40-agent store: once all 24 hooks have returned, every frame is in ingress or the event log, drain-through derives exactly one lifecycle event per Stop, and the fold sees all 24 agents. Its timing guard is the [hook budget](#the-hook-budget) |
| `produce_budget.rs` | Zero subprocess spawns for a warm fleet-scale produce with fresh inputs, and for a produce over stale or missing heavy caches. A retained refresh cycle over mixed Codex and Claude sessions and linked worktrees also spawns zero, after one cold origin lookup and no branch subprocess |
| `fold_incremental.rs` | Warm folds and produces read O(new bytes). A warm or unchanged fold parses zero carryover bytes, and a replaced carryover re-parses once |
| `sidebar_diff_stats.rs` | Per-worktree git probes single-flight across CLI snapshot processes, counted through the `git-trace` shim |
| `consumer_enrichment.rs`, `enrichment_cadence.rs`, `spending_incremental.rs` | Unchanged-room consumer cost, per-enrichment TTL stamps, and O(delta) spend IO |
| Detached-helper and Codex transport unit tests | Spawn attempts include detached helpers, scope retries, and cold app-servers, so refresh budgets count these children too |

The `cargo xtask invariants` check `ensure_sidebar_library_boundaries` keeps store writers, the run-wake sender, and the broker out of the sidebar data plane's imports, so the producer cannot grow write-side machinery unnoticed.

### Live client-byte guard

`crates/rimz/tests/integration/journey/deep.rs::tmux_cell_only_sidebar_bounds_client_bytes` runs the real sidebar beside an agent driven by installed Codex hooks, with a PTY-attached tmux client. A cell-only frame costs that client at most 1 KiB: over a measured window, the guard allows one 1024-byte diff per configured frame interval plus 4 KiB for the status line. It also requires the attached client's screen to change during the window, so a renderer that paints nothing cannot pass. The pane has at least 4000 cells, so a full repaint per frame exceeds the budget. The guard bites from tmux 3.7b. On 3.7 and 3.7a, tmux could skip the redraw at the end of a synchronized update ([tmux reference](../externals/mux-adapter/tmux-reference.md)), so a full repaint per frame is not guaranteed to show on those releases. It is a live-tmux gate in the `live` profile, run with `cargo xtask test -E 'binary(integration) & test(journey::deep::tmux)'`, and it asserts bytes, not wall time.

### Benchmarks

`cargo xtask perf` runs the divan benches in `crates/rimz/benches/` over synthetic stores and pane frames, through the same entry points the sidebar uses. It launches no agents and spends no tokens. The benches stay out of `gate` and `ci`, so a busy CI runner never fails a build on their timing, but the regression ceilings below fail `cargo xtask perf` itself. Each failure prints the measured value and the ceiling.

Allocation counts are the steady regression signal, and medians move with host and load. The figures here were captured on `xlab-term`, a loaded LXC container on an AMD Ryzen 9 9950X with 28 online CPUs and the `performance` governor. Requested bytes and allocation requests come from the bench's counting allocator (`crates/rimz/benches/fleet.rs::counted`), which counts allocation and reallocation requests. Alloc/op is Divan's median `alloc` byte count, which excludes the grow bytes Divan reports separately.

#### Regression ceilings

| Bench | Ceiling | Basis |
| --- | --- | --- |
| `fleet::publish::scale_with_orphans`, 200 agents / 100 subscribers, 0 and 3,000 orphans | 11,500,000 requested bytes per cycle | About 3× the measured 3.75 MB |
| `fleet::idle_cycle`, 3,000 orphans | 33 ms, median cycle | About 3× the measured 10.77 ms; the median ignores a single scheduler stall |
| `fleet::idle_cycle_hidden_fanout` | 38,000 allocation requests per cycle | About 1.5× the 25,315 with hidden panes skipping, below the 91,565 of hidden panes folding |
| `hotpath::hook_stop_budget` | Hook wall at most max(4 ms, 2× the `--version` wall) | [The hook budget](#the-hook-budget) |

#### The hook budget

`crates/rimz/benches/hotpath.rs::hook_stop_budget` holds the release `rimz hooks feed` wall for a Codex `Stop` to max(4 ms, 2× the `rimz --version` wall), sequentially and 24 at a time. The `--version` spawn is the baseline because it pays process start and nothing else, so the ceiling tracks the host's exec cost.

Each half uses the statistic its failure mode moves. The sequential half compares medians over 100 interleaved hook and version pairs. A single spawn stalls at random, so a high percentile of a few dozen samples is the slowest one and fails on host noise. A regression on the foreground path (a fold or lifecycle effect, an extra fsync, git resolution) shifts every sample and still fails the median. The 24-way half compares p99 over 240 children per side, where tail contention (lock retries, drainer contention) shows. Its coordinator prepares the commands, then one barrier releases 24 threads, and each thread times spawn through the standard child wait. One unmeasured warm-up burst per side precedes ten hook bursts alternating with ten version bursts. Both sides write and close the same piped stdin, and output collection is outside timing.

Both halves measure a warmed, live draining worker. First-creation directory sync and drainer startup stay outside them. Cleanup drains the measured frames and the warm-up outside the budget, with a 120 s wait sized for serial apply.

To run it, build with `cargo build -p rimz --release --features testkit --bins`, then run `cargo xtask perf -- hook`. To run it against a disposable copy of a real room, set `RIMZ_HOOK_BUDGET_HOME` to a frozen home that contains `.rimz` and exactly one recorded room. The bench copies it and leaves the source untouched.

| Room (release, ms) | Statistic | Hook wall | Version wall | Ceiling |
| --- | --- | ---: | ---: | ---: |
| Seeded, sequential | median of 100 | 2.197 | 1.738 | 4.000 |
| Seeded, 24-way | p99 of 240 | 7.834 | 6.907 | 13.814 |
| Live-sized copy, 24-way | p99 of 240 | 8.164 | 7.369 | 14.738 |

The seeded sequential row is the slowest hook median of five quiet runs (medians 1.887 to 2.197 ms). Three runs beside 14 CPU hogs held hook medians of 2.555 to 2.746 ms against ceilings of 4.169 to 4.454 ms, and one of them hit a 9.934 ms hook maximum: the spike a max-based check would fail on. A scratch 4 ms sleep added to every hook sample fails the median at 6.220 ms. The 24-way exec baseline itself runs about 7 to 11 ms, so 4 ms is the sequential target, not a 24-way wall-time promise. After-return drain took 2.668 s on the seeded fixture and 80.468 s on the live-sized copy.

#### Fixtures

`crates/rimz/src/testkit/mod.rs::fleet::seed_scale_room` builds the fleet benches' rooms. It takes state paths, runtime paths, and `ScaleRoom { agents, panes, ended_sessions, orphan_contexts }`, publishes fresh producer inputs, and returns a frame for `produce_snapshot` or `enrich_workspace`. It binds the first live agents to the requested panes and leaves the remaining agents unbound, and each bound agent has its own synthetic branch. Orphan context sidecars have keys absent from the room's agents. Its carryover uses representative row sizes but ends every history session, so the agent, pane, and history dimensions stay independent. `crates/rimz/src/testkit/mod.rs::fleet::seed_ended_carryover` keeps a 98% ended distribution for the carryover benches.

The `fleet::publish` module measures shared publication at (51 agents, 24 panes, 4,600 ended sessions) and (200, 100, 5,000), so the two room sizes have 24 and 100 groups. `fold_and_project` times one enrichment followed by every local projection, retaining the delivered snapshots until the end. `exclude_agent` excludes a live agent pane from each projection, which measures detaching the group vector, its row group, and pane bindings. `local_only` moves the fold outside timing to isolate per-pane allocation. `local_fixed_room` holds the larger room fixed while it varies subscribers between 24 and 100, which separates subscriber scaling from group-count scaling. These cases measure publication, not renderer loops or live host RSS.

`fleet::idle_cycle` times a retained worker fold and real renderer application for 100 subscribers sharing one plane with explicit client membership. `fleet::idle_cycle_worker` replays one cursor per subscriber, including pane projection and content comparison, without renderer application. Neither paints. Both run at 200 agents and 5,000 ended sessions with zero or 3,000 orphan context sidecars. Settled hidden subscribers may defer content, so renderer clocks never regress but may stay unchanged.

`fleet::idle_cycle_hidden_fanout` places each of 57 subscribers over 50 agents in its own tab, with explicit client membership, and watches and paints one of them. It waits two seconds, past the renderers' one-second data tick, before timing. Every measured cycle changes row content, delivers a real retained worker fold, and steps each renderer's frame timing twice. The 56 hidden panes retain content until reveal without reducing a view, and the watched pane applies every change.

`hotpath::carryover_fold_cold` parses the raw carryover inside timing, and `hotpath::carryover_fold_cold_derived` parses the stamped trimmed copy, published on a separate thread outside timing. `hotpath::carryover_fold_rebirth_warm` primes a rebirth cursor and appends one `Registered` frame outside timing, then measures the delta fold. Their compact row JSON is 4,800 bytes at p50, about 13,200 at p90, and 35,900 at maximum, and the 3,300-row and 13,200-row files are 32,943,350 and 131,784,597 bytes. Cold allocation includes transient raw-file text and decoding, not only the retained projection. To measure a CLI read's peak RSS on these fixtures, follow [profiling.md → Peak RSS of a CLI read](./profiling.md#peak-rss-of-a-cli-read).

#### Reference figures

The latest capture of each bench. Run one group with a name filter, such as `cargo xtask perf --bench fleet -- publish` or `cargo xtask perf --bench hotpath -- carryover_fold`. Paired values are for the bench's listed arguments in order.

| Bench | Median | Allocation | Captured |
| --- | ---: | ---: | --- |
| `fleet::produce_cold` 20 / 50 / 100 agents | 4.65 / 7.61 / 13.45 ms | 4.52 / 9.26 / 17.36 MB alloc/op | 2026-07-05 |
| `fleet::produce_warm` 20 / 50 / 100 agents | 520 µs / 792 µs / 1.32 ms | 1.53 / 2.09 / 2.98 MB alloc/op | 2026-07-05 |
| `fleet::publish::fold_and_project` 24 / 100 subscribers | 1.800 / 4.330 ms | 1.035 / 2.780 MB alloc/op | 2026-10-12 |
| `fleet::publish::exclude_agent` 24 / 100 subscribers | 2.417 / 12.65 ms | 1.589 / 12.27 MB alloc/op | 2026-10-12 |
| `fleet::publish::local_only` 24 / 100 subscribers | 58.34 / 188.2 µs | 88.36 / 368.2 KB alloc/op | 2026-10-12 |
| `fleet::publish::local_fixed_room` 24 / 100 subscribers | 62.84 / 206.4 µs | 88.36 / 368.2 KB alloc/op | 2026-10-12 |
| `fleet::publish::scale_with_orphans` 0 / 3,000 orphans | not quoted | 37,996 requests, 3,724,484 bytes requested, both arms | 2026-10-14 |
| `fleet::idle_cycle` 0 / 3,000 orphans | 12.25 / 10.77 ms | 51,200 requests, 6,483,552 bytes requested, both arms | 2026-10-14; medians 2026-10-12 |
| `fleet::idle_cycle_worker` 0 / 3,000 orphans | not quoted | 51,103 requests, 6,476,496 bytes requested, both arms | 2026-10-14 |
| `fleet::idle_cycle_hidden_fanout` | 2.402 ms | 25,315 requests, 2,956,692 bytes requested | 2026-10-21 |
| `hotpath::fuse_owned_no_overlay` 40 agents | 74.7 µs | 819 B alloc/op | not recorded |
| `hotpath::rollup_fold_warm` 40 agents, 0 / 1,500 carryover rows | 56.9 / 79.8 µs | 655.6 / 655.6 KB alloc/op | 2026-09-14 |
| `hotpath::rollup_fold_unchanged` 40 agents, 0 / 1,500 carryover rows | 2.24 / 2.99 µs | 0 / 0 B alloc/op | 2026-09-14 |
| `hotpath::carryover_fold_cold` 3,300 / 13,200 rows | 58.97 / 299.4 ms | 107.9 / 420.5 MB alloc/op | 2026-10-07 |
| `hotpath::carryover_fold_rebirth_warm` 3,300 / 13,200 rows | 429.8 µs / 4.347 ms | 1.725 / 6.746 MB alloc/op | 2026-10-07 |
| `hotpath::enrich_cached` 40 agents | 610 µs | 1.21 MB alloc/op | 2026-07-05 |
| `hotpath::render_fixed` 40 agents | 413 µs | 1.09 MB alloc/op | 2026-07-05 |
| `hotpath::spending_walk_cold` 20k entries | 19.59 ms | 24.43 MB alloc/op | 2026-07-15 |
| `hotpath::spending_walk_warm_no_change` 20k entries | 7.95 ms | 10.78 MB alloc/op | 2026-07-15 |
| `hotpath::spending_live_scale_cold_hydrate` 6k files / 102k entries | 252.3 ms | 122.6 MB alloc/op | 2026-07-16 |
| `hotpath::spending_live_scale_cold_discovery_inclusive` 6k files / 102k entries | 173.9 ms | 101.9 MB alloc/op | 2026-07-18 |
| `hotpath::spending_live_scale_warm_global_refresh` 6k files / 102k entries | 93.5 ms | 50.25 MB alloc/op | 2026-07-20 |
| `hotpath::spending_live_scale_warm_discovery_only` 6k files | 19.55 ms | 770.9 KB alloc/op | 2026-07-18 |
| `hotpath::spending_live_scale_warm_discovery_inclusive` 6k files / 102k entries | 140.2 ms | 99.59 MB alloc/op | 2026-07-18 |
| `hotpath::spending_live_scale_additional_workspace_scope` 6k files / 102k entries | 116.6 ms | 58.59 MB alloc/op | 2026-07-20 |

The 2026-10-14 capture ran at a load average near 45, where two back-to-back runs of the same build read 6.4 ms and 12.2 ms for one arm, so this table quotes no median from it. `hotpath::carryover_fold_cold_derived` and the `kimi`, `grok`, and `droid` changed-session refresh benches have no recorded capture.

At fixed room size and across both room sizes, local publication allocates about 3.68 KB per subscriber: groups, rows, and history do not multiply with subscribers. Local configuration, worktree roots, and pane metadata stay owned per pane, and a real exclusion still pays for a detached group vector and the changed collections, as `exclude_agent` shows.

## Making a performance change

1. **Name the thread the cost lands on.** If it is the render/input loop, the change is wrong until the work moves off it.
2. **Prefer a push to a shorter poll.** A tighter poll burns cycles while the room is idle. A wakeup costs nothing until something changes.
3. **Decide durability explicitly.** Durable state goes through `write_temp_then_rename`, which fsyncs. A cache rebuilt on the next tick goes through `write_temp_then_rename_cache`.
4. **Keep single-flight.** A new fetch trigger goes through `FetchDispatcher` in [`app/fetch.rs`](../../crates/rimz/src/sidebar_pane/app/fetch.rs) (`request` for now, `defer_until` for later, `fire_due` from maintenance), never a bare send or spawn, so it merges with the rest.
5. **Measure the idle case.** A change that speeds the busy path by adding idle work is usually a net loss.
6. **Prove it.** `cargo xtask ci` stays green, the tick budgets and the cost map move with the change, and `cargo xtask perf` refreshes the figures when the change touches a measured path. A before/after comparison rests on the bench's allocation counts. Quote a wall-time delta only from runs on an idle host, since medians move with load ([Benchmarks](#benchmarks)). When comparing two builds' wall times, warm both, then run them interleaved (base, branch, base, branch) and compare paired ratios, because a sequential pair measures host load. A change to a bench fixture compares against base rerun with the same fixture, since a comparison across fixtures is not evidence.
7. **Measure the file you fixed.** For an I/O fix, `strace -f -e trace=openat` filtered on the target path across several ticks shows whether that file is still re-read. `/proc/<pid>/io` `rchar` is the process total, mixing every other read, so report it beside the per-path count, never alone.

### Perceived response time

Responsiveness is what the user feels: a frame that paints now with slightly stale data beats a fresh frame that arrives late. Reach for these levers in order.

1. **Acknowledge before you finish, unless one source of truth is worth the wait.** Where the view's intent is the truth, paint the instant input lands: a browse pick and the help overlay paint synchronously in `LoopState::apply_input`. The jump is the deliberate exception. A key jump changes no browse pick. Its detached thread writes a focus intent that the owning view presents through the fold ([state.md → Focus intent](./sidebar/state.md#focus-intent)), so a stale frame cannot roll the highlight back.
2. **Animate on the wall clock, never on IO.** The spinner advances from the loop's frame clock (`frame_clock`) on a fixed interval whether or not a fetch is in flight. Motion that stalls while data is slow reads as a hang.
3. **Separate smoothness from freshness.** When a "make it feel snappier" request lands, first decide whether it is a frame-rate problem or a data-latency problem. They are fixed in different layers, and conflating them turns a cosmetic tweak into a CPU regression.
4. **Keep session-wide effects off the frame path.** A redraw writes only to the sidebar's own pane and is safe at any rate. A mux action touches the whole session and can reset an unrelated pane's cursor blink, so it belongs on the event-driven data layer.

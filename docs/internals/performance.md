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

The loop forks for tmux graphics capabilities and width geometry. `LoopState::run_maintenance` calls `RoomCaps::detect` ([`sidebar_pane/pixel/probe.rs`](../../crates/rimz/src/sidebar_pane/pixel/probe.rs)) synchronously, under the host-wide `RoomCaps` mutex. A due sample runs tmux client, version, and pane-option queries, each with a 500 ms command timeout, at most once per `CAPS_REFRESH_INTERVAL` (10 s) for the whole room. Width controllers share one session-wide geometry attempt under a separate plane-owned mutex: idle backstops reuse it for `IDLE_RETRY` (5 s), while event floors and floorless requests require fresh geometry ([width](./multiplexers.md#width)). Other panes can wait on either sample, never while holding the client-view mutex. Zellij rooms use neither subprocess path.

The data-plane threads, one set per host, are the fetch worker, the cache refresher, the observer writer, the tmux control-mode watch, and the transcript watch. The terminal event reader lives in each pane's supervisor. Their responsibilities are in the thread table in [state.md](./sidebar/state.md#the-room-data-plane).

### One host, many panes

The external reads a room needs (the pane roster, git, the forge, provider accounts) cost the same whether one tab asks or twenty do, so one room host pays them for the fleet.

The room host runs one data plane for every pane. It retains panes, agent projection, and metrics history in memory instead of reading its own publications back each cycle. Passive CLI readers fold the published caches through [`PublishedSnapshotReader`](../../crates/rimz/src/sidebar/consumer.rs) without probing mux, git, or providers. A producing inspection (`rimz sidebar snapshot` without `--no-produce`) and message recipient resolution read the mux privately, and the inspection also refreshes heavy lanes. Neither publishes the host-owned inputs.

N tabs therefore cost one set of external reads, and each room's host folds once for all its panes. A fold-bearing room event costs one fold request at the plane plus one wake per pane. Panes request only what they alone observe. A burst of events that lands within one cycle costs at most two folds for any tab count: every request waiting when the worker drains its queue joins the next cycle, and those that arrive while it runs join one more. An event that lands later is indistinguishable from a new change and costs one more. This is the largest lever in the design: in external reads, a 20-tab room costs about what a 2-tab room costs. The shared data plane lives in [state.md](./sidebar/state.md#the-room-data-plane), and the account-global spend walker has its own election in [spending.md](./agents/spending.md#one-walk-per-namespace).

Heavy-lane external reads also single-flight across processes through [`disk::single_flight::coalesce`](../../crates/rimz/src/disk/single_flight.rs), so a CLI refresh racing the host shares that lane's TTL window. Pane-frame production has no cross-process election: the host alone publishes, and CLI callers compute privately.

### Two clocks

Smoothness and freshness run on separate clocks, so tuning one never taxes the other.

The base frame grid is set by `[theme.display] refresh_ms` (100 ms by default), and a wheel glide briefly uses a finer grid. A frame redraws from the cached snapshot. The data clock is event-driven, and it folds new truth when a wakeup says there is some. Its backstop is the fetch worker's: an ordinary cycle when no request arrives within the shortest subscriber's data tick (`rimz sidebar serve --tick-seconds`, 1 s by default) after a cycle returns ([state.md → Requests](./sidebar/state.md#requests)). The fetch worker rests `FOLD_FLOOR` (250 ms) after each cycle returns before it starts the next, whatever the request, which bounds its duty cycle. The cache refresher consumes that worker's fold rather than folding again. It uses a sample strictly younger than its own data tick; otherwise it requests one ordinary cycle and waits at most one tick for a post-request sample. Failure or timeout skips heavy lanes for that pass, not due tasks, messages or daemon-view repair. The clamps, animation cadences, and hidden-tab paint rules are [state.md → The paint clock](./sidebar/state.md#the-paint-clock).

Durable truth and the pane roster are split the same way. Every fold reads the store rollup event-fresh, so a status flip repaints within one wakeup. The pane roster sits behind a TTL cache, so discovery stays cheap. A writer that knows about a change posts a typed wakeup, and polling only backs up a missed one.

### The three end-to-end budgets

Every row of the cost map rolls up into one of three paths, each named with its dominant term so a regression shows at review.

| Path | Budget | What happens |
| --- | --- | --- |
| Keypress to pixel | One in-process paint; at most eight for a wheel glide | Input applies synchronously and paints. Consecutive queued wheel ticks share the first paint, followed by time-based eased frames for at most 100 ms ([state.md](./sidebar/state.md#the-paint-clock)). The path takes the shared view lock and reads three small runtime records (read marks through a stat-keyed cache, the body filter, and the focus anchor). It forks nothing and reads no event log or snapshot. |
| Pane event to pixel | One fuse plus one off-grid paint | A typed overlay event (`PaneClosed`, `FocusChanged`) lands in the in-memory event store, re-fuses the held frame, and paints at once. The producer's next pull confirms it ([state.md](./sidebar/state.md#fusion-rules)). |
| Write to pixel | Up to the worker's 250 ms rest, one fold, then the next dirty frame | The plane requests a shared fold for applied events and wakes the panes, which paint on the next dirty frame. Hook ingress must first pass through the drainer, and no frame budget bounds that queue. |

## Principles

A performance change follows these rules. An earlier rule outranks a later one when they conflict.

1. **The render thread never blocks.** No fork, fsync, or roster read on the loop. Move the work to a worker and wake the loop when it finishes. The tmux graphics-capability probe ([The render thread](#the-render-thread)) is the one standing exception, and new code does not add a second.
2. **The frame never drags the fetch.** Responsiveness is a redraw from the cached snapshot. A smoothness change shortens the frame interval and pays in-process paints. A freshness change belongs to the data layer. A frame that triggers a roster-plus-git fetch spends a fork to move a spinner.
3. **Push, then poll.** A writer that knows about a change posts a wakeup, and polling is the backstop. A data source that feeds the UI without being the store (a context sidecar, a transcript) gets a wakeup of its own.
4. **Fsync truth, rename caches.** Durable records fsync. Everything derived (rollup caches, the snapshot cache, diff stats, context sidecars) is written with `write_temp_then_rename_cache`, which renames atomically without fsync, because it rebuilds from truth on the next read. Neither form allows a torn read. The cache form gives up only surviving a power cut ([store.md → Write classes](./store.md#write-classes)).
5. **Single-flight, then coalesce.** One fetch runs at a time, and a burst of requests collapses to one fetch. Requests arriving during a fetch merge into exactly one next fetch, never a queue.
6. **Pay the external read once per window.** A short TTL bounds pane discovery, git, and forge probes, and the last good result is reused. A failed probe backs off and keeps its last known good value. A degraded roster read backfills missing fields per pane instead of painting a corrupt frame.
7. **Take the cheapest correct read.** Catch-up is O(delta bytes) from the thread's held fold or the persisted fold base, never O(history). Skip work that cannot matter: projection reads of unchanged budget, park, spending and message files cost a stat, including missing or malformed files; a room with no agents skips the sidecar scans. Locked read-modify-write paths still read fresh bytes.
8. **One host per room, one render loop per pane.** The data plane and room event socket are shared, while every pane keeps its loop, heartbeat, and bounded wake lanes. Supervisors replace a lost host and reattach without an ownership election.

## The cost map

This section gives each lane's cost and the bound that holds it down. Reproducible figures come from `cargo xtask perf` ([Benchmarks](#benchmarks)). External IPC rows give ranges seen in production. Every cadence constant named below has its value and staleness budget in [state.md → Cadences](./sidebar/state.md#cadences). A value this page gives is one that page does not own.

### The render loop

| Operation | Cost | Bound |
| --- | --- | --- |
| Frame redraw | Sub-millisecond in process; 413 µs at 40 agents. Card painting scales with the viewport plus height-cache misses: one full paint per snapshot, clock, width, theme, focus, or card-configuration epoch, and the cards whose expansion, selection, or match changed within it | A cold epoch paints in one walk. A warm frame measures geometry from cached heights, then paints the window. The base `refresh_ms` grid is briefly finer for a wheel glide, and off-screen animation relaxes to the pane's idle tick. Geometry still scans the roster, and a frame never starts a fetch |
| Overlay fuse | 75 µs with no active overlay, at 40 agents | Pure: no IO, no subprocess, no clock read |
| Jump to a pane | One mux client fork, tens to hundreds of ms | Detached thread. The focus intent it writes reaches the frame through the fold ([state.md](./sidebar/state.md#focus-intent)) |
| Tmux graphics capabilities | One room-wide client, process, and version sample and one pane-option listing, on the render loop | At most once per `CAPS_REFRESH_INTERVAL` (10 s) per room, under the shared `RoomCaps` mutex; each command times out at 500 ms. A missing pane or unavailable listing uses a targeted permission query, and escalation stays per pane |
| Zellij width observation | One topology parse and pane geometry table per changed file stamp; a metadata check and lookup thereafter | Shared per plane. Freshness and each pane's observation floor stay live ([multiplexers.md](./multiplexers.md#width)) |
| Tmux width observation | One session-wide `list-panes -s` attempt, with a live per-pane fallback for a missing pane | Shared per plane, including failures. Idle backstops within `IDLE_RETRY` (5 s) share the attempt; event floors and floorless requests demand fresh geometry. An unreported resize is caught within two idle retries plus scheduling and command time ([multiplexers.md](./multiplexers.md#width)) |

### The fetch worker

| Operation | Cost | Bound |
| --- | --- | --- |
| Snapshot rollup | Bounded generation read and metadata checks while runtime `snapshots/latest.json` is unchanged and parse-cached; one whole-file parse after it is replaced; O(delta bytes) when writes outran it | The checkpoint's stamped `LogExtent`, whose generation and offset a reader compares with the durable record and live log ([store.md → Truth and cache](./store.md#truth-and-cache)). A `RollupCursor` holds the parsed base, also validated against the log's device/inode identity: one per thread for store reads, and one owned by the sidebar fetch worker. The durable rotation carryover beneath it is parsed once per file identity per thread, and every fold shares its fold-ready rows without copying |
| Event-log fold | Warm cursor: one stat of the log and one of the carryover, plus the appended frames; the projection clones only the rows it keeps. Unchanged log: shared handles only | Guard tests `delta_fold_is_o_new_bytes` and `warm_fold_parses_unchanged_carryover_zero_times` |
| Workspace fold | One enrichment per cycle; every pane projects from it by reference ([The render thread](#the-render-thread)), with no seed serialization | Host views retain the seed fold |
| Fold observations | One focus-anchor read and one body-filter read per real cycle, shared by every attachment | The painting view applies selection, confirmation, and read intent. A live nonce comparison guards retirement ([state.md](./sidebar/state.md#focus-intent)) |
| Pane roster (producer) | Zellij: one topology read plus a `zellij pipe` nudge. tmux: one session-wide `list-panes` call | `SNAPSHOT_CACHE_TTL` while a client drives it, `EVENT_PANE_TTL` while the presence stamp is fresh; one attempt per data tick |
| Process metrics (producer) | A bound agent pane reads only its root stat, preserving the starttime guard; agent cards consume no tree metrics. Process rows sample their full trees, including IO and process state. A single-threaded process reads one `children` file without scanning its task directory; multi-threaded processes retain the per-thread union | Focused and background sample stamps in `lanes/metrics-sample.json`; the producer holds each pane's shared [hibernated check](./sidebar/state.md#cadences) in memory. A full process-table walk is needed only when a due root binding is unknown |
| Pane process repair | Due panes read process commands, identity, cwd and hosted-agent evidence; resume discovery adds no process-file reads for settled hosted-process identities (pid, start and cmdline) or resolved ids whose hosted pid and start are unchanged; held panes retain the prior process fields without those reads. One production-local process-file memo shares process-file reads across repair, its composite probes and metrics, including at most one boot-time read; confirmation candidates start fresh | The same per-pane [hibernated check](./sidebar/state.md#cadences) as metrics; changed or unknown native tenants repair fully. Fresh-frame metrics passes have their own process-file memo, discarded after the pass; settled no-id answers and resolved ids are retained only for live panes and their respective hosted identity |
| Agent projection (producer) | Unchanged tick: metadata stamps only, no directory enumeration | One batched discovery call per kind and one wiring probe per data tick |

### The host cache refresher

The heavy-lane table covers passes that acquire a worker fold. A failed acquisition postpones these lanes, while due tasks, messages and daemon-view repair keep their own cadence.

| Operation | Cost | Bound |
| --- | --- | --- |
| Live-session context | Stat-gated local reads, no context-helper subprocess | Every refresher pass, except roots on the [hibernated cadence](./sidebar/state.md#cadences). Rich session metadata refreshes at hook boundaries or on explicit refresh, and account windows through the per-(login, scope) usage lane |
| Git diff stats per root | Unchanged HEAD on a clean tree: 1 `git` fork (`status`; the prior `+/-` carries forward). Unchanged HEAD on a dirty tree: about 2. Fallback or ancestry change: about 6 | Focused, then `DIFF_STATS_TTL` hot, `DIFF_STATS_HIBERNATED_TTL` (30 min) for qualifying hibernated roots, `DIFF_STATS_SETTLED_TTL` (5 min) clean and landed, else `DIFF_STATS_IDLE_TTL` idle ([qualification](./sidebar/state.md#cadences)). Condition-demanded roots keep their normal tier. A host pass refreshes every due focused, hot and just-thawed root, even beyond `MAX_HOST_PASS_ENTRIES` (8), then fills remaining slots with other due roots, oldest first (`crates/rimz/src/sidebar/refresh/git_stats.rs::select_for_pass`). The one-shot CLI pass refreshes every due root. At most `MAX_PARALLEL_GIT` (8) run at once |
| Worktree roots | One `git worktree list` fork plus one marker stat per root; marker bytes read only on change or memo miss | `WORKTREE_ROOTS_TTL`; no child scans in a directory room |
| Owned-worktree condition scopes and timers | One repo check and one `git worktree list` per root on a cache miss; ownership markers revalidated by stat each call | Shared process-local rows invalidate on the common `worktrees` directory's stat identity or after 60 s, which bounds external worktree moves. CLI Git enumeration stays uncached |
| PR target assembly | In-process HEAD reads, including checkout subdirectories. Unresolved ref formats fall back to `git rev-parse --abbrev-ref HEAD`. One successful `git remote get-url origin` per common Git directory per config version | Host-retained memo keyed by the common config's stat identity. Missing origins are cached, and transient failures retry. Detached HEAD needs no branch subprocess. One-shot CLI refresh starts fresh. Global URL rewrites and per-worktree config changes alone take effect after a common config change or a host restart |
| PR state per repo | GitHub batches branch PRs and distinct HEAD rollups, plus one repository open-set alias in the first batch when a non-trunk target exists, within the 100-alias budget. Each condition-demanded branch adds a head-ref alias. Merge-queue state rides the existing open-facts alias and costs no alias or request, but a queued PR keeps its repository on the hot tier until it leaves the queue. Tea reads base branches from its one open-list call, so stacks cost no extra request. Checkout detail reads begin when a PR leaves the open set, while demanded branches share one all-state list and resolve terminal details from first sight | `PR_STATE_HOT_TTL` and `PR_STATE_TTL`. Failures back off and keep the last known map. Tea checkout closed links, and merged links with a settled CI verdict, are pinned ([state.md](./sidebar/state.md#pr-state)) |
| Provider accounts | Each pass stats stamping adapters' inputs: Pi's auth file, session roots, and their direct child directories. Due providers probe in waves of at most four account-then-version chains, each subprocess capped at 3 s | `ACCOUNTS_TTL` per settled login (`crates/rimz/src/sidebar/refresh/accounts.rs::due_provider_logins`). Failures back off per login from 10 s, doubling to `ACCOUNTS_TTL` (10 min). An input-stamp change bypasses backoff after `ACCOUNTS_RETRY_TTL` (10 s); provisional answers also retry on that floor. Pi reads at most 32 candidate session tails, each through at most one parent. A provisional re-probe reuses a stored version younger than `ACCOUNTS_TTL` instead of spawning `pi --version`. Contention serves the stale cache instead of starting a second wave |
| Fleet spend walk | Within `SPENDING_TTL`: one read of the published aggregate, no transcript IO. A missing or stale room sidecar: scan memo locations with one scope test per discovered file, then fold only that room's counted rows and raw sessions. A due walk: frontier stats plus O(appended bytes) per changed file | One warm walker per namespace owns the index. Consumers never open `spending.json` ([spending.md](./agents/spending.md#one-walk-per-namespace)) |
| Finished-cohort effort | Transcript stat and parse for collapsible groups only; unchanged files reuse a process-local parse memo | `COHORT_SPEND_TTL`. The producer publishes `lanes/cohort-spend.json`, and renderers only read it |
| Team pipeline | One board read per eligible staged-team group; no subprocess or event-log scan | Every refresher pass, no TTL. Writes `lanes/pipeline.json` only when content changes. The pass resolves team configuration (`SeatPolicy::load`) only when some agent carries a team or a profile, and shares that resolution with idle compaction |
| Codex daemon reap | Zero unless Codex remote control is on or a daemon-hooked session is live. When due, per in-use Codex login: one daemon process scan, plus a WebSocket handshake when that login's daemon is found | `CODEX_DAEMON_REAP_TTL`; success and failure share the stamp |

### The store write path

The choreography and write classes are [store.md → The write path](./store.md#the-write-path), and the ingress lock and drain cursor rules are [store.md → Hook ingress](./store.md#hook-ingress).

| Operation | Cost | Bound |
| --- | --- | --- |
| Hook ingress | One framed append of raw payload and captured environment under the shared ingress lock, no lifecycle fold | Appenders block in the kernel and never exclude each other. Cost is payload-sized. First creation syncs the parent directory |
| Derived lifecycle append | One ordered-batch event-log `write()` per applied frame, zero steady-state fsyncs | `store_fsync.rs` pins the zero fsyncs. `store_bytes.rs` pins one representative lifecycle turn at 1 KiB or less; the event log itself enforces no frame size |
| Lifecycle attention config | Two core-only reads per ordinary apply, without definitions, theme, loop, or the machine-config memo; a quiet core file is parsed once, and the second read is a hit in the core-only memo the dispatch default and account reads share | `AttentionConfig::load_lenient` shares the full load's torn-read guard: a 50 ms quiet window and an unchanged stamp across the read, up to three attempts, then the last quiet value cached for that file, or a final read when none is cached. Quiet files incur no sleep |
| Drain cursor | Two cache-class publications per known same-boot frame; two durable publications for unstamped frames or an unknown drainer boot | Durable publications cost two fsyncs each; skips, earlier-boot drops and resets are cache-class, with one publication per consecutive earlier-boot run within a read. Emptying the queue retains one truncation fdatasync. The elected drainer applies frames serially in environment-scoped children |
| Event-log durability | One group `fdatasync` per second per workspace (`LOG_SYNC_INTERVAL`) | Runs on the off-lock tail. Cursor and truncation barriers are additional |
| Snapshot publish | Rename-only rewrites of the rollup cache and `latest.json`; the durable generation changes only at log-replacing boundaries | Single-flighted on the off-lock tail and debounced to once per `PUBLISH_INTERVAL` (1 s). Due sooner when the unpublished tail reaches `PUBLISH_BYTE_BUDGET` (64 KiB) or the log shrinks (`crates/rimz/src/store/writer/publish.rs::should_publish`) |
| Wakeup fanout | One heartbeat directory scan plus one datagram per distinct room socket | The scan reads one heartbeat per live sidebar and checks freshness by mtime. Every attachment of a room host names the same socket, so the datagram count is the live room host count. The reads are page-cache hot and cost less than the fsync floor |
| Durable file write | Temp file plus two fsyncs (file, parent directory) | Trust grants, the workspace record, hook installs, and drain claims/applications without a known matching boot |

### Everything else

| Operation | Cost | Bound |
| --- | --- | --- |
| Sidebar observer | One O(rows) signature pass per committed fold, microseconds | Pure detection at every renderer's fold commit point. The host writes observer records and adds one cross-check pass per `OBSERVE_CROSSCHECK_TTL` (5 s), with per-row process probes on the [hibernated cadence](./sidebar/state.md#cadences) ([diagnostics.md](./diagnostics.md#the-frame-stream-observer)) |
| Tick meter | Six relaxed counter loads and two clock reads per metered tick | Healthy ticks do no file IO and spawn nothing |
| Sidebar heartbeat | Temp file plus atomic rename on creation or claim change; pathname mtime update otherwise | Each attachment renews every `HEARTBEAT_WRITE_INTERVAL` (2 s), inside the 5 s liveness TTL. A deleted record is republished in full; other failures retry without advancing the success stamp |
| Room read marks | One stat-keyed parse per thread per record change | Full atomic-file identity of `live/read-marks/marks.json`. Pane sweeps read the current record, never an older shared cut |
| Codex session index | One `stat()` of `session_index.jsonl` per live Codex session per refresh; one full parse per index change per process | Full file identity (length, mtime, device, inode): appends grow the file and thread deletes rewrite it ([adapter_codex.md](./agents/adapter_codex.md)) |
| Reload poll | One `stat()` of the durable reload record per sidebar per second | Executable hashing runs only after that record's metadata changes. The embedded presence-plugin digest resolves once, lazily ([sidebar.md](./sidebar/sidebar.md#build-promotion)) |
| Tmux hidden-view fit | One `list-windows` read per settled burst of kicks. When hidden windows lag the current window's size, one read script and one fit script, each a single `tmux source-file -`, cover all of them whatever their count | A tmux-only host thread (`crates/rimz/src/sidebar_pane/app/hidden_views.rs::spawn`) reads 500 ms after the last kick (`SWEEP_SETTLE`). A jump to a stale destination adds one batched read and the same two scripts for that window. Zellij rooms run none of it ([state.md → Shared lanes](./sidebar/state.md#shared-lanes)) |
| Remote link probe | One JSON probe over the existing SSH ControlMaster every 2 s | Supervised remote attach only; `RIMZ_REMOTE_PROBE_MS=0` disables it |
| `loop watch` repaint | One room's catalog, pause, and terminal reads once per second. Run-log cost is one `stat` per generation per repaint, reading appended bytes only. Plus one `locks/` listing for that root, which opens and try-locks every `loop-run-*.lock` file there under the directory's lease gate, free ones included | Workspace identity resolves once before the loop. The entry check reads the verified room pin and sidebar heartbeat once, without another workspace probe. The run-log fold fully re-reads on rotation, a new local day, or a time-zone change. The lock cost grows with stale run-lock files until `rimz gc` removes the free ones |

`WORKTREE_ROW_CAP` (6) caps the idle and process tail in the renderer, never the snapshot. Active, paused, blocked, finished, focused, and unread rows render past the cap, so jump targets and unread convergence stay visible. The observer's pass scales with the full roster, and the render and selection walks scale with the visible set. Both stay bounded by live pane count across the 20 to 100 agent target. If that bound loosens, the fix is row virtualization in the renderer, never hiding rows from the snapshot.

## Overhead at fleet scale

RimZ is sized against a single agent: watching a hundred agents should cost a small and nearly flat fraction of running one of them.

Cost attaches to three units: the workspace, the worktree, and the agent. Hook application scales with event volume on top of all three. Moving it out of the foreground does not remove its fold or IO cost.

Within one hook frame, cached-tier store reads share one fold and snapshot assembly per `(log generation, log offset, staged event count)`. Own-row context reads copy only that row; context is read again at each use. A write changes the key, so a later decision sees the staged or committed event rather than the earlier snapshot. This reuse is frame-local, not a liveness cache for the room host ([store.md → The read path](./store.md#the-read-path)). Non-registration lifecycle reactions and signal retirement borrow durable audit rows; a firing team signal copies only its cohort with Audit molt marks, while registration retains the full marked projection.

Message dispatch reads the inherited `harness.smart_compact` threshold from the typed core file only when no explicit threshold is set. Session-login environment resolution reads account declarations through the same core-only reader, including parked-target hook preflight; attention reads share it too. The task catalog reads its machine tasks from the loop file alone. Each file has a single-slot per-process memo: repeated reads do no I/O within two seconds of the last stamp check, then revalidate the stamp and parse only if it changed. Like the full lenient load, these readers cache only a quiet, stable read and use built-in defaults for missing or broken files. Neither reads definitions or theme; account selection still uses the shared session-login resolver.

Per workspace, RimZ pays once. The host pays the roster and metrics on its fetch worker and git and accounts on its cache refresher. The worker folds once for all its panes and shares that fold with the refresher. Every workspace that shares a state root and provider-discovery environment asks the same warm spend walker, so a host restart adds no second parsed cursor. One elected hook drainer per room owns the ingress queue. When quiet it blocks in poll(2) on its listener and client sockets (`crates/rimz/src/harness/hook_drain.rs::run`), so an idle drainer and the leases it holds open add no periodic wakeups. It exits after `DRAINER_IDLE` (60 s) without frames or connections. Its election, sockets, and shutdown order are [ARCHITECTURE.md → Runtime shape](../../ARCHITECTURE.md#runtime-shape), and its apply children are [adapter.md → The feed](./agents/adapter.md#the-feed).

Per worktree, cost follows activity. The git input set scales with distinct group roots, not agents. A quiet root uses the idle TTL, or the 5-minute settled TTL when clean and landed. A qualifying all-hibernated root takes the 30-minute tier instead, unless focused, hot or condition-demanded ([qualification](./sidebar/state.md#cadences)). The first host pass after it leaves the hibernated set forces a refresh. A host pass always refreshes the due viewed, hot and just-thawed roots and gives the others what is left of its 8-root cap, so a cold start or a clump of expiring stamps spreads over several passes. Under sustained overload an idle or hibernated root can outlast its TTL. PR probes scale with origin repositories, and each enumerates open PRs once when due. A hundred agents sharing a few checkouts pay for a few hot roots.

Per agent, hook reporting is event-driven. An agent reports through a short-lived `rimz hooks feed` child that appends when something happens and exits. Recurring sidebar context, activity, budget-ledger and auto-continue park reads obey the [hibernated cadence](./sidebar/state.md#cadences), retaining the last raw values between checks; account and fleet scopes still project every pass. Unchanged ledger and park records reuse bounded, per-thread full-stamp parse caches. Enforcement reuses ledger parses for no-op checks, rereading fresh bytes before a ledger write or interrupt; auto-continue likewise reevaluates fresh park bytes before a write, removal, or helper spawn. Live-session refresh reads due context sidecars through their stat gate. Every interactive launch retains its wrapper. Only wrappers carrying a run poll its record: every 250 ms with self-cleanup, or every 5 s without it ([run-pane cleanup](./harness/scripting.md#reclaiming-the-run-pane)). Scoped wrappers also check their drainer lease every 200 ms ([resident wrapper](./harness/fleet.md#resident-wrapper)). A Unix wrapper re-execs into a parked image with no store and a private (anonymous) memory budget of 8 MiB outside huge pages, at most 16 MiB of huge pages, guarded by the live `backend::tmux::agent_lifecycle::parked_wrapper_holds_under_8_mib` test and the thin-image invariant. Executable text is shared across wrappers, and the test bounds `Pss_Anon - AnonHugePages` and `AnonHugePages` separately, printing total `Pss` beside them. Fold-dependent duties run in helper children: strand repair is resident only for its parked nonterminal phase, while receipt, parent confirmation, and card evidence are one-shot. A `--keep` subagent's settled linger is outside this budget ([the exec wrapper](./harness/fleet.md#resident-wrapper)).

Two costs are cadence-gated per room: event-log group `fdatasync` and snapshot publication. Publication still projects and serializes the roster, so its work grows with row count even though its cadence does not. Drain cursor publications are separate writes outside either debounce: known same-boot frames use cache-class rename; unstamped frames or an unknown drainer boot retain durable claim and applied writes. Skips, earlier-boot drops and resets are cache-class, with one publication per consecutive earlier-boot run within a read, while truncation retains its fdatasync. The hot runtime caches live under `$XDG_RUNTIME_DIR`, normally a tmpfs, so their churn is memory traffic. Without that variable RimZ falls back to `/tmp/rimz-<uid>` (`crates/rimz/src/disk/paths.rs::runtime_home_from`), whose churn reaches whatever filesystem holds `/tmp`.

These display-plane CPU and RAM estimates cover a fleet of 2 to 5 rooms. They exclude the elected hook drainer and its serial apply child, whose cost follows hook volume rather than fleet size. The measured CPU figures below cover one host, not a fleet.

| Resource | 20 agents | 50 agents | 100 agents | What sets it |
| --- | --- | --- | --- | --- |
| CPU, busy | <0.3 core | ~0.3 to 0.8 core | ~0.5 to 1.5 core | One producer per room, bursting on its git pass, never on the render thread |
| RAM, resident | ~80 to 150 MiB | ~100 to 180 MiB | ~120 to 220 MiB | One host per room plus a small supervisor and local snapshot metadata per tab, shared rosters and rows, one spend walker, room-local rollups, prepared cell-pet grids |
| Durable write | Event-volume dependent | Event-volume dependent | Event-volume dependent | Derived lifecycle frames, about 1 KiB per turn; raw ingress payload and environment; cursor publications |
| fsync rate | Event-volume dependent | Event-volume dependent | Event-volume dependent | Group event-log barrier, four cursor fsyncs only for unstamped or unknown-boot applications, and one fdatasync per drained queue |
| State on disk | Tens of MiB | Tens of MiB | ~100s of MiB | Rotation-capped event log, measured ~1.6 KiB per live snapshot row in the [200-agent fixture](#live-text-hook-comparison), and the pending ingress queue, truncated after drain |
| Network | Weekly pricing fetch, OAuth usage probes per metered (login, scope), forge open-set probes per due repo | Same | Same | Pricing is fleet-shared and single-flighted. Each credential's usage probe runs every 5 minutes, every hour after a settled auth failure, and no more often than every 3 hours for an idle account ([providers.md → Refresh cadences](./agents/providers.md#refresh-cadences)). Local datagrams carry the rest |

### Measured host-pass cost

Measured on 2026-10-28, Linux x86-64, EPYC 7502P, 58 available logical CPUs and tmux 3.8. Clean release testkit CLIs compare trunk `889af63ad` (build `ae36cd45f44f`) with that base through `perf(sidebar): renew v6 heartbeats by pathname mtime` (build `a9dc7bdf8c2e`). Both use the [scale yardstick](#scale-yardstick): 200 agents, 300 closed rows, sixteen threads per stub, and 20 or 80 sidebars in distinct worktrees. Idle means no incoming frames, with 25 agents initially working, not an all-finished room. Each host warms for 60 s, then measures idle and busy for 120 s each; busy sends 240 frames. Host CPU comes from its process-stat delta and excludes supervisors, helper children, tmux and stubs.

Cells are base / head. CPU is milliseconds per wall-clock minute; cycles are fetch cycles per minute, not total worker-plus-refresher folds. Busy fold time per cycle (the window's fold milliseconds over its cycles) fell from 734 and 740 ms on base to 80 and 90 ms on head, and base's busy cycle rate stayed flat across sidebar counts. Base idle and head busy at 200 / 80 cover about 60 s of diagnostic intervals rather than 120. Base's refresher also folded on its own thread, outside these counts; in the hidden-tab test rig the worker and refresher together folded 59 times in 30 s on base and 30 times on head. Load before/after the whole host bench was 8.49/13.26 on base and 13.26/23.84 on head. No load was induced; other builds were active. This is one base/head pair under shared load, not a fixed speedup or near-zero idle promise.

| Agents / sidebars | Idle host CPU | Busy host CPU | Idle fetch cycles | Busy fetch cycles |
| --- | ---: | ---: | ---: | ---: |
| 200 / 20 | 10,525 / 6,760 | 16,235 / 13,465 | 51.75 / 60.33 | 45.75 / 89.85 |
| 200 / 80 | 18,125 / 10,475 | 21,685 / 17,315 | 50.51 / 58.02 | 45.04 / 120.51 |

Separate traced idle rooms counted 167,405 / 94,270 host `/proc` open attempts per minute at 200/20, and 162,106 / 91,934 at 200/80. Their fetch rates were 44.37 / 56.74 and 37.18 / 52.83 respectively, so these are traced rates, not untraced per-pass counts. Per-agent ledger, park and context-file opens fell from 36,800, 10,200 and 10,200 per minute to zero at 200/20, and from 33,800, 9,500 and 9,500 to zero at 200/80. Provider-spending-cache opens fell from 389 and 359 to 29 per minute. Missing queue/history files caused no file opens on either revision; history-directory scans remain. The empty-commit git fixture captured no timed index replacements on either revision and does not measure the stale-index refresh optimization.

The `fleet` idle-cycle benches ran base, head, head, base. Applied-cycle allocation requests were 64,512 / 63,486 on both pairs; worker-only requests were 67,644 / 67,716. Hidden-fanout requests were 27,024 / 27,048, below the 38,000 ceiling in all four runs. The 3,000-orphan applied-cycle medians were 11.55, 11.06, 10.75 and 15.99 ms, all below the 33 ms ceiling. One-minute load before/after each run was 18.78/13.51, 13.51/9.77, 9.77/16.63 and 16.63/26.53; allocation counts, not these loaded wall times, ground the comparison.

One developer's week of Claude and Codex sessions produced 1.23 GiB of transcript JSONL (about 177 MiB a day), with each agent process resident at 250 to 340 MiB (Claude) or 50 to 65 MiB (Codex). RimZ watched the same fleet with tens of MiB of durable state, a resident set about the size of one agent process, one fsync a second per room, and one pricing refresh a week.

Remote render-stream bytes sit outside this CPU and memory budget: SSH carries whatever the visible full-screen TUIs repaint. The sidebar's own share is held by the [client-byte guard](#live-client-byte-guard): a cell-only frame costs the attached tmux client at most 1 KiB. A synchronized-output bracket around every frame makes tmux repaint the whole pane instead, about 17 KB per frame on a 60x69 pane (tmux 3.7c, 2026-10-07). On a 50x119 pane, that bracket on every frame delivered 496,667 bytes in 10.066 s, against 19,963 bytes in 10.114 s with graphics-only brackets (about 49 KB/s against 2 KB/s). These are attached-client bytes, not the renderer's producer write rate. Idle RimZ surfaces send close to nothing, and a busy agent TUI commonly sends tens of KB/s. `rimz pane bandwidth` reports each pane's producer write rate beside the room's SSH socket payload (`WIRE(ssh)`), which is usually far below the per-pane sum ([reference](../reference/cli/pane.md#bandwidth)).

## What's optimized

Each mechanism that holds a cost down, named once with its code home. Where another page owns the mechanism, this section says what it saves and links there.

### The frame is decoupled from the fetch

The fetch worker publishes room folds into views. The render loop waits on its room lane and reduces when the worker's `snapshot` wakeup arrives. The animation tick redraws from the cached snapshot and never fetches, so a missed push degrades to the backstop tick instead of a poll storm. [`animation_cadence`](../../crates/rimz/src/sidebar_pane/render/animation.rs) classifies row motion: fast work stays on the base grid, row animations redraw on `BREATH_ANIMATION_FRAME`, and a dirty data fold pulls the next frame back to the base grid so freshness never waits on cosmetic motion. A wheel glide temporarily takes the finer grid ([state.md](./sidebar/state.md#the-paint-clock)).

### Only the watched tab animates

Each renderer knows its own pane and the latest client map, so it can tell whether a client is looking at its tab. A watched tab keeps the normal grid. An unwatched or detached tab skips the animation roster scan and wakes only on its idle tick, events, and fetch results. The plane requests one shared fold per fold-bearing room event, and the worker merges waiting requests. A settled hidden attachment leaves publications unread in views until reveal, without a time bound. Its drain remembers the examined revision and memoizes its own-view preview by frame Arc and pane: repeated wakes clone no outcomes and scan no frame, and new content on the same frame reuses the preview. Pane-local reconciliation obligations still apply, but hidden, held, and anonymous unpainted views do no reductions. Freeze expiry sleeps while hidden, and view mutations wake no unrelated panes. Arrival catches up the routed client view before its first paint, preserving its last painted order and any existing hold deadline. Hidden resize changes geometry only. When ownership is unknown the renderer counts as watched, so tests, demos, and cold starts keep the responsive path. Deferral exceptions and hidden paint rules are [state.md → The paint clock](./sidebar/state.md#the-paint-clock).

### One shared fold per room

The producer builds the snapshot in process on its fetch worker ([`produce_workspace_snapshot`](../../crates/rimz/src/sidebar/produce/mod.rs)), with no `rimz sidebar snapshot` fork per tick. The worker's cadence stamp allows one attempt per data tick, and only against a stale retained frame, so unrelated wakeups cannot multiply production.

The fold spine splits into renderer-independent [`enrich_workspace`](../../crates/rimz/src/sidebar/enrich.rs) and renderer-local `project_local`. The worker folds the first half into host views, and each attachment projects at reduction. The retained fold seeds new attachments and supplies the cache refresher's workspace and matching frame. Both worker lanes are [state.md → One fetch cycle](./sidebar/state.md#one-fetch-cycle).

### Truth arrives by event, the frame by coalescing

The host delivers produced folds directly. The rollup is read event-fresh from `snapshots/latest.json` on every fold, so a status change repaints within one wakeup while the host retains and shares its pane inputs.

Every writer that knows about a change pushes. Store and sidecar writers post a `StoreDelta`. Both backends' presence streams feed one projector that emits typed pane events. The host's transcript watcher refreshes context mid-turn for every adapter that declares transcript-tail context ([state.md → Push channels](./sidebar/state.md#push-channels)).

### No reader pays for history

The rollup persists a raw fold base stamped with its log extent and file identity. Catch-up validates its version, generation, identity and offset before seeking to the stamped offset and folding only new frames ([`fold.rs`](../../crates/rimz/src/store/snapshot/fold.rs), [store.md → The read path](./store.md#the-read-path)). Runtime projection, resume outcomes, and smart-compact dedupe ride the same fold instead of rescanning `log/events.log.jsonl`.

A per-thread parse cache returns `Arc<T>` handles, so an unchanged file costs neither a re-parse nor a deep clone ([`disk/parse_cache.rs`](../../crates/rimz/src/disk/parse_cache.rs)). `lanes/snapshot.json` and `snapshots/rollup.json` are keyed on `(path, mtime, len)`. `snapshots/latest.json` and `records/agents-carryover.json` are keyed on the full `(len, mtime, dev, ino)` identity, which tells same-size replacements apart.

The keyed cache retains at most 1,024 paths per cache per thread, evicting oldest insertions under churn. Budget ledgers and scope state, parks, provider spending, and projection/enforcement reads of the queue and history use full file stamps; missing and malformed outcomes stay cached until that stamp changes. History appends invalidate by length, and a torn-tail warning fires only on a new parse. Other I/O failures are retried, not cached. Spending version, freshness and budget-window decisions are evaluated on every read, never memoized as verdicts. Context sidecar reads also include device and inode in their stamp; locked sidecar and queue transactions bypass all parse caches.

The carryover's cached form is fold-ready: identities backfilled, rows key-sorted, every row's prompt text reduced to bounded single-line labels, and ended rows stripped of pane-command text. Observations construct the labels once; the carryover reader normalizes legacy rows with the same rule, rotation writes the labels into durable carryover, and a rollup cache from before labels fails its version check and cold-rebuilds, so its reads normalize nothing. Its reborn base is cached once per parse. Carryover, rollup-cache rows, reducer seeds and merged layers share individual rows; a fold deep-copies only rows it changes, including root-lane and rebirth stamps. Deltas still walk retained keys and names, and a cold hook child still decodes every persisted rollup and carryover row once. A corrupt replacement is never cached, so every fold reports it until the file is replaced. The [read-path contract](./store.md#the-read-path) owns the prefix bound, full-text wake classification and revival from labels without text restoration.

The spend walk is incremental in three layers. The walker's directory index stats only active frontiers and reconciles fully every 15 minutes. The disk cache keeps a cursor per file, so a grown file parses only its appended suffix. The one elected walker holds the only parsed cache, so workspace requests borrow instead of cloning. Aggregation uses hash collections for session uniqueness and keeps deterministic order only in the published maps. The cache layout is [spending.md → The incremental cache](./agents/spending.md#the-incremental-cache).

### Per-enrichment cadences

Every display figure is display-only by invariant ([DESIGN.md](../../DESIGN.md#triage-at-a-glance)), so each enrichment runs on its own cadence. Normal cache freshness stamps live in shared files, agreeing across a producer handoff; per-key hibernated check stamps are process-local and start due on a new reader. Account probes carry one stamp per login, so a failure retries alone. Process sampling keeps per-pane focused and background stamps, independent of the roster clock, with the hibernated hold shared with process repair. Git probes take activity-tiered TTLs whose hotness comes from store activity, with no filesystem watching. In-process `.git` ref reads skip the ancestry forks when the cached HEAD and trunk pair and the clean verdict are unchanged. The values are [state.md → Cadences](./sidebar/state.md#cadences).

Git status normally disables optional index locks. The host enables them on the next due status only when the preceding status reported at least 128 stale entries: its trace2 `refresh/sum_scan` count minus the entries a refresh scans but cannot bring up to date, saturating at zero: those whose worktree status is modified (`M`), type-changed (`T`), intent-to-add (`A`), or a worktree rename or copy (`R`, `C`). Deletions fail `lstat` before the scan and refresh skips unmerged entries, so neither is subtracted, except that an unmerged `AA` or `UA` reads as intent-to-add, which errs toward not locking. Each root gets at most one such attempt per 60 seconds, measured from the locked status's start, including a lost lock race or a root temporarily leaving the room's needed set. A locked status can also refresh initialized submodules' indexes, bounded by the parent's threshold and window rather than each submodule's own staleness. Missing trace2 evidence means zero stale entries; a one-shot CLI never enables the lock. This reuses the status fork rather than adding one. Numstat diffs disable optional locks and set `diff.autoRefreshIndex=false`, so neither they nor their submodule status children refresh indexes.

### Append and drain critical sections

The foreground hook never waits on the workspace lock for an ordinary append. Its ingress lock is shared and kernel-blocking, independent of the workspace lock that lifecycle batches and cursor publications take, so appenders never queue behind each other or behind a fold. Known same-boot claims and applications are cache-class; unstamped frames or an unknown drainer boot retain durable publications. Earlier-boot frames drop before application or recovery, with one normally rate-limited record and cursor publication per maximal consecutive run within a read. Skips, drops and post-truncation cursor resets are cache-class; truncation keeps its fdatasync before reset. A due `PostToolUse` deadline rung can still take the workspace lock. Workspace-lock waiters retry at 1 ms for the first 25 ms, then back off exponentially to 50 ms. Not every workspace-lock hold is a microsecond append or fsync-free. Every fsync goes through `disk/atomic.rs`, which the `cargo xtask invariants` check `ensure_store_durability` enforces for `sync_all` and `sync_data` calls. The lock order, repair, and framing are [store.md → Hook ingress](./store.md#hook-ingress), and the durability classes are [store.md → Write classes](./store.md#write-classes).

### Helper processes stay cheap

The per-tab sidebar supervisor loads no machine config, so its retained state does not grow with agent, subagent, or team definitions. The room host owns the settings and definition-backed work.

The build id is computed once per process on first use from the linker build identity in the first 1 MiB of the image, and the whole image is hashed only when that identity is missing ([`build_id.rs`](../../crates/rimz/src/build_id.rs)). The long-lived sidebar host still warms it off-thread; short-lived commands do not. Hook feeds reuse the CLI parse needed to restore their owner's environment; statusline feeds skip that preliminary parse. Both feeds and `hooks apply` use a narrow clap tree built from the normal argument types, with the full tree retained for help and parse failures. Self-spawns resolve through one helper, `rimz_exe`, which honors `RIMZ_BIN` and repairs Linux `current_exe()` paths ending in ` (deleted)` after an atomic reinstall. The Zellij presence plugin hashes the stable manifest fields before projecting pane fields, so title-only `PaneUpdate` storms skip projection ([`policy.rs`](../../crates/rimz-presence-zellij/src/policy.rs)). Zellij rooms start with `disable_session_metadata true` by default, which stops the server rewriting `session-metadata.kdl` and running command-discovery `ps` for the room.

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

**Cheap-looking work multiplies by fleet size.** Deriving the merge base separately for every group root costs about 240 `git` execs on a 20-worktree fixture. A warning emitted once per fold per renderer, with Sentry's `attach_stacktrace` on, makes every consumer symbolize a backtrace before the rate limiter can drop it (about 200 MiB per fold against 18 MiB clean). Streaming serde output straight into a socket turns each JSON token into a syscall. The hook drain wire avoids this in `crates/rimz/src/harness/hook_drain.rs::write_line`: it serializes a whole JSON frame and its newline into a byte buffer, then calls `write_all` once. Serialization errors send no partial frame; short socket writes still take as many writes as needed.

**An atomic reinstall leaves deleted inodes behind.** A long-lived renderer's `current_exe()` resolves to `rimz (deleted)`, so a naive self-spawn fails `execve`, and a supervisor parent holds the deleted inode until session end while its children orphan. `rimz_exe` repairs the suffix, supervisor-owned reload convergence re-execs parents onto the new binary, and supervisor-side reaping clears the orphans.

**Timed allocator decay leaves dirty pages in idle processes.** On Linux x86-64 and aarch64, with glibc or musl, `crates/rimz/src/main.rs::ALLOC` selects jemalloc for Rust allocations, and the crate's `override_allocator_on_supported_platforms` feature routes C `malloc` to it too, bundled SQLite included. `crates/rimz/src/main.rs::ALLOCATOR_CONFIG` exports the process-start configuration `dirty_decay_ms:0,thp:never`, with jemalloc defaults otherwise. Timed decay advances through allocator activity, so an idle process can retain dirty pages indefinitely; immediate decay returns them without a RimZ-scheduled trim or background allocator threads. macOS and other Linux architectures keep their platform allocators. aarch64 Linux runs jemalloc with 64 KiB pages on every kernel, because jemalloc's bundled `configure` fixes that target's page size rather than detecting it. Memory on musl, aarch64, and other Linux architectures is unmeasured; no saving is claimed there.

`thp:never` marks jemalloc's extents `MADV_NOHUGEPAGE`, but its base metadata blocks remain eligible for huge pages on kernels whose transparent huge page (THP) mode is `always`. This cost is accepted: [CI run 40362145411](https://github.com/rimio-ai/rimz/actions/runs/40362145411), on trunk `9f7285e1a6` plus `fix(runtime): keep jemalloc mappings off transparent huge pages` under inferred THP `always`, measured the all-features test build's parked exec wrapper at 14,668 KiB private memory and its command wrapper at 12,524 KiB, while glibc main passed both tests below 8 MiB. On every kernel, the live wrapper budgets allow 8 MiB outside huge pages and at most 16 MiB of huge pages; the THP mode is only a diagnostic label. Disabling THP with `prctl(PR_SET_THP_DISABLE)` in `main` was rejected: the flag survives fork and exec, changing every child, and does not evict huge pages already faulted into pre-main base blocks. On `madvise` or `never` kernels, `thp:never` does not change the measured residency, so the re-check figures below stand.

jemalloc replaces the glibc arena cap of 2, or 16 for the sidebar host. Two arenas serialized the many-threaded host on malloc locks; jemalloc instead creates arenas lazily, up to its default of four times the CPU count. Selection runs rejected mimalloc and default jemalloc because both raised small-process memory. jemalloc's background threads added threads and retained more memory than immediate decay.

Built-binary re-check, 2026-10-26, on `xlab-term` (x86-64 Linux 7.0.14, THP `madvise`, 24 logical CPUs, 72 GiB RAM, Rust 1.97.1): glibc build of trunk `97b25672499d` versus the jemalloc build of `perf(runtime): replace glibc malloc with jemalloc on Linux glibc` on that trunk, both release builds with `testkit` and without `sentry`. Figures are median `Pss_Anon + SwapPss` in MiB at six minutes, except the walker at 500 seconds and the separately labelled two-minute row. Small-process samples came from two fresh rooms per build; the walker had two runs per build and the many-pane host had six, each in its own fresh room, interleaved across builds. No fleet total was measured.

| Process or workload | glibc | jemalloc, immediate decay |
| --- | ---: | ---: |
| Sidebar supervisor | 4.004 | 2.797 |
| Parked exec wrapper | 3.453 | 3.139 |
| Three-window sidebar host | 42.518 | 9.518 |
| Held tmux client | 7.695 | 4.258 |
| Loop watch | 3.430 | 2.717 |
| Stats daemon | 4.238 | 4.186 |
| Content daemon | 3.299 | 3.188 |
| Codex broker | 3.281 | 3.078 |
| Spending walker | 310.496 | 208.455 |
| 57-window sidebar host | 94.613 | 53.039 |
| Stats daemon at two minutes | 4.205 | 4.439 |

The stats daemon's two-minute ranges did not overlap: 4.195 to 4.215 MiB for glibc, 4.406 to 4.473 MiB for jemalloc. Its six-minute ranges overlapped (4.230 to 4.246 versus 4.070 to 4.301 MiB), so these measurements do not establish a saving for that process.

| Timing measure | glibc | jemalloc, immediate decay |
| --- | ---: | ---: |
| Many-pane host mean CPU, percent of one core (range of six host means) | 10.83 (9.73 to 11.80) | 10.28 (9.23 to 12.07) |
| Walker CPU seconds, median over 500 seconds | 10.335 | 9.680 |
| First walker request seconds, median (range) | 2.524 (2.207 to 2.842) | 1.657 (1.530 to 1.784) |

Each many-pane room had 56 stub agents and 57 attached sidebar panes, receiving five synthetic `store_delta` events per second. CPU windows followed a 60-second attachment settle. Across six fresh hosts per build, jemalloc's mean of host means was below glibc's, meeting the host CPU bar; shared-host load varied, so this is not a causal speedup claim. First-request timing runs from spending-service owner election to its first completed request.

Immediate decay has a process-start CPU price on short-lived commands: about 0.1 to 0.3 ms per invocation, or 2 to 6 percent of a 5 to 7 ms process. In 1,000 interleaved pairs per command, jemalloc-minus-glibc CPU medians were +317.5 µs for `--version` and +268 µs for `hooks feed --source codex`; a three-arm follow-up measured +124.5 µs and −0.5 µs respectively (hook interval −111 to +95 µs). Against jemalloc's default decay, immediate decay alone cost the hook +250.5 µs, about 100 extra `madvise` calls and 198 extra minor faults. Wall time stayed within shared-load noise, and `crates/rimz/benches/hotpath.rs::hook_stop_budget` passed on both builds.

Any later allocator change must meet the many-pane host CPU bar: a 57-window tmux sandbox room with stub agents and five `store_delta` events per second, at least six fresh hosts per build interleaved across configurations, each sampled over three 10-second CPU windows. Compare resident memory plus swap for each process kind, not RSS alone, and claim savings only where measured. A change to the allocator or its configuration also runs the live wrapper budget tests with `MALLOC_CONF=thp:always,metadata_thp:always` added, which gives jemalloc's extents and base blocks the huge pages that a THP `always` kernel would. The bench executables also use jemalloc on the same targets, but carry no exported configuration; their allocation counts do not depend on decay.

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

The fetch worker and the cache refresher each meter their ticks against budgets in [`sidebar/meter.rs`](../../crates/rimz/src/sidebar/meter.rs). The refresher meters its heavy-lane refresh alone: the wait for the worker's fold is the worker's own metered cost, and a pass that acquires no fold records no tick. Due tasks, messages and daemon-view repair run unmetered after it:

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
| `fold_incremental.rs` | Warm folds and produces read O(new bytes). A warm or unchanged fold parses zero carryover bytes, and a replaced carryover re-parses once. A warm delta over 3,300 carryover rows copies at most the one fold row it touches |
| `work_log.rs` | Cross-process work records from direct and drainer-spawned apply children; cold Prompt/Read/Stop frames on a fixed small room use 2/2/2 assemblies, 9/8/8 row copies, 0/0/0 machine-config loads and one rollup parse, and the same fold and outside-fold row copies at 12 and at 120 closed rows; a team-member Stop with a pending message also pins zero machine-config loads |
| `sidebar_diff_stats.rs` | Per-worktree git probes single-flight across CLI snapshot processes, counted through the `git-trace` shim |
| `consumer_enrichment.rs`, `enrichment_cadence.rs`, `spending_incremental.rs` | Unchanged-room consumer cost, per-enrichment TTL stamps, and O(delta) spend IO |
| Detached-helper and Codex transport unit tests | Spawn attempts include detached helpers, scope retries, and cold app-servers, so refresh budgets count these children too |

The `cargo xtask invariants` check `ensure_sidebar_library_boundaries` keeps store writers, the run-wake sender, and the broker out of the sidebar data plane's imports, so the producer cannot grow write-side machinery unnoticed.

The work-counter index below distinguishes thread-local reads from process totals and file channels. A counter in the bench coordinator never sees a separate apply child; the work log is the bridge. These are work counts, not allocation or disk-I/O totals.

| Counter / channel | Scope | What it counts; what it omits |
| --- | --- | --- |
| `crates/rimz/src/proc/mod.rs::testkit::spawn_count` | Process | Attempts through the counted subprocess funnels; no endpoint attribution, raw spawns elsewhere or child work |
| `crates/rimz/src/disk/atomic.rs::testkit::fsync_count` | Process | File/directory sync calls; no child syncs or duration |
| `crates/rimz/src/store/event_log.rs::testkit` (`bytes_read`, `bytes_written`, `bytes_read_under`) | Process | Bytes scanned/appended, optionally reads under a path; not carryover, snapshots, physical disk traffic or children |
| `crates/rimz/src/store/snapshot/fold.rs::testkit` (`carryover_bytes_parsed`, `rollup_rows_materialized`, `fold_row_copies`) | Process | Raw carryover parses; row copies outside the fold from flattening, kept runtime rows, dispatch's retained rows and explicit resuming child, own-row context reads, live launch members read with context for Prompt sender attribution, marked team-signal cohorts and shared context vectors; deep `AgentState` copies inside catch-up, pending folds, rebirth bases and carryover preparation after parsing. Pointer shares count nothing. Checkpoint parses are counted separately |
| `crates/rimz/src/store/sidecar.rs::testkit::read_counts` | Thread | Cumulative sidecar stats, enumerations and parses; not other threads or children, and absent from work records |
| `crates/rimz/src/testkit/host_pass.rs::thread_counters` | Thread and process `TOTAL` | Host reads, wake sources, drain clones/previews, views acquisitions, new record-generation loads and attachment event-store takes; not child work or mutex wait duration |
| `crates/rimz/src/agents/pricing/mod.rs::testkit::book_loads` | Process | Price-book memo misses; not memo hits or a cross-process total, and absent from work records |
| `crates/rimz/src/testkit/mod.rs::WorkCounters` (`snapshot_assemblies`, `snapshot_publishes`, `rollup_bytes_parsed`, `latest_bytes_parsed`, `machine_config_loads`, `marker_reads`, `spending_fleet_folds`) | Process | Assembly/rebuild calls, rollup/latest parser-input bytes, machine-config disk loads and worktree-marker byte reads. Parse/load/read fields omit memo hits; marker stats and missing files count nothing. No arbitrary JSON parses or sentry-only core-config reads. The spending field counts fleet folds including walk checkpoints, not scope-only derivations or other processes, and is absent from the work log |
| `RIMZ_TEST_MACHINE_CONFIG_LOAD_LOG` | Cross-process file | One strict/lenient line per machine-config disk build, including reloads; no memo hits, pid or sentry-only core read |
| `RIMZ_TEST_HOOK_DRAIN_TIMING=1` | Cross-process diagnostic file | Per-frame drainer claim/application timing in the room's drainer log for the elected drainer, or the draining process's own stderr for an inline drain, such as `hooks drain --once` or a hook's fallback drain; no apply-child CPU or file-parse counts |
| `RIMZ_TEST_WORK_LOG` | Cross-process file | CPU/resources and work counters when `main` returns; no `std::process::exit` paths (including clap exits), signal-killed processes or thread-local sidecar counters. [Scale yardstick](#scale-yardstick) defines the records and attribution |

### Live client-byte guard

`crates/rimz/tests/integration/journey/deep.rs::tmux_cell_only_sidebar_bounds_client_bytes` runs the real sidebar beside an agent driven by installed Codex hooks, with a PTY-attached tmux client. A cell-only frame costs that client at most 1 KiB: over a measured window, the guard allows one 1024-byte diff per configured frame interval plus 4 KiB for the status line. It also requires the attached client's screen to change during the window, so a renderer that paints nothing cannot pass. The pane has at least 4000 cells, so a full repaint per frame exceeds the budget. The guard bites from tmux 3.7b. On 3.7 and 3.7a, tmux could skip the redraw at the end of a synchronized update ([tmux reference](../externals/mux-adapter/tmux-reference.md)), so a full repaint per frame is not guaranteed to show on those releases. It is a live-tmux gate in the `live` profile, run with `cargo xtask test -E 'binary(integration) & test(journey::deep::tmux)'`, and it asserts bytes, not wall time.

### Benchmarks

`cargo xtask perf` runs the divan benches in `crates/rimz/benches/` over synthetic stores and pane frames, through the same entry points the sidebar uses. It starts no providers and spends no tokens; the ignored host-scale bench runs a real private tmux room with parked agent stand-ins. The benches stay out of `gate` and `ci`, so a busy CI runner never fails a build on their timing, but the regression ceilings below fail `cargo xtask perf` itself. Each failure prints the measured value and the ceiling. A bench that takes its input by value (`bench_values`, `bench_local_values`) times the input's drop, so a fixture whose teardown is costly (a tempdir, a large cache) is benched with `bench_refs` or `bench_local_refs`, or returned from the closure so divan drops it outside the sample.

Allocation counts are the steady regression signal, and medians move with host and load. Unless noted otherwise, the figures here were captured on `xlab-term`, a loaded LXC container on an AMD Ryzen 9 9950X with 28 online CPUs and the `performance` governor. Requested bytes and allocation requests come from the bench's counting allocator (`crates/rimz/benches/fleet.rs::counted`), which counts allocation and reallocation requests. Alloc/op is Divan's median `alloc` byte count, which excludes the grow bytes Divan reports separately.

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

The seeded sequential row is the slowest hook median of five quiet runs (medians 1.887 to 2.197 ms). Three runs beside 14 CPU hogs held hook medians of 2.555 to 2.746 ms against ceilings of 4.169 to 4.454 ms, and one of them hit a 9.934 ms hook maximum: the spike a max-based check would fail on. A scratch 4 ms sleep added to every hook sample fails the median at 6.220 ms. The 24-way exec baseline itself runs about 7 to 11 ms, so 4 ms is the sequential target, not a 24-way wall-time promise. After-return drain took 2.668 s on the seeded fixture and 80.468 s on the live-sized copy. These measurements predate boot-stamped cursor writes. Claims and applications now require additional durability work only for unstamped frames or an unknown drainer boot; other cursor writes are cache-class, and each emptied queue still pays a truncation fdatasync separately from the debounced event-log barrier.

#### Fixtures

`crates/rimz/benches/hotpath.rs::latency::Fixture` takes live and closed counts, holds a real owner process for every live root, and registers each through SessionStart. Each live row then observes a 2,400-byte prompt and a pane stamp whose spawn and foreground commands come from the production team-member exec encoder, using the fixture's runtime paths. Setup cancels those turns back to idle before measurement. Its measurements use real feed, drainer and apply processes. `drainer_latency` and `drainer_backlog` retain 300 live roots and 1,500 closed rows; [Scale yardstick](#scale-yardstick) varies those dimensions for `apply_scale`.

`crates/rimz/src/testkit/mod.rs::fleet::seed_scale_room` builds the fleet benches' rooms. It takes state paths, runtime paths, and `ScaleRoom { agents, panes, ended_sessions, orphan_contexts }`, publishes fresh producer inputs, and returns a frame for `produce_snapshot` or `enrich_workspace`. It binds the first live agents to the requested panes and leaves the remaining agents unbound, and each bound agent has its own synthetic branch. The same live-text seeder gives every agent an observed prompt and every bound pane production-encoded launch text, then cancels setup turns to idle. Orphan context sidecars have keys absent from the room's agents. Its carryover uses representative row sizes but ends every history session, so the agent, pane, and history dimensions stay independent. `crates/rimz/src/testkit/mod.rs::fleet::seed_ended_carryover` keeps a 98% ended distribution for the carryover benches.

The `fleet::publish` module measures shared publication at (51 agents, 24 panes, 4,600 ended sessions) and (200, 100, 5,000), so the two room sizes have 24 and 100 groups. `fold_and_project` times one enrichment followed by every local projection, retaining the delivered snapshots until the end. `exclude_agent` excludes a live agent pane from each projection, which measures detaching the group vector, its row group, and pane bindings. `local_only` moves the fold outside timing to isolate per-pane allocation. `local_fixed_room` holds the larger room fixed while it varies subscribers between 24 and 100, which separates subscriber scaling from group-count scaling. These cases measure publication, not renderer loops or live host RSS.

`fleet::idle_cycle` times a retained worker fold and real renderer application for 100 subscribers sharing one plane with explicit client membership. `fleet::idle_cycle_worker` replays one cursor per subscriber, including pane projection and content comparison, without renderer application. Neither paints. Both run at 200 agents and 5,000 ended sessions with zero or 3,000 orphan context sidecars. Settled hidden subscribers may defer content, so renderer clocks never regress but may stay unchanged.

`fleet::idle_cycle_hidden_fanout` places each of 57 subscribers over 50 agents in its own tab, with explicit client membership, and watches and paints one of them. It waits two seconds, past the renderers' one-second data tick, before timing. Every measured cycle changes row content, delivers a real retained worker fold, and steps each renderer's frame timing twice. The 56 hidden panes retain content until reveal without reducing a view, and the watched pane applies every change.

`crates/rimz/benches/fleet.rs::concurrent_hidden_fanout` runs 100 attachment loops on real threads over 200 agents, with one watched pane and 99 hidden. After warming through hidden backstop ticks, a publisher runs 100 content cycles, resting for the worker's 250 ms fold floor after each cycle. Its stderr line reports attachment CPU per publication, split into hidden and watched threads, publication-to-paint p50/p99/max, and lateness beyond the later of publication and the scheduled frame deadline. CPU and lock/take counts include backstops and one final non-revealing room nudge, but exclude the publisher and the subsequent reveal. `reveal_ms` measures publication to the newly revealed pane's first paint. This bench has no timing ceiling; the allocation ceiling above belongs to the sequential fixture.

`hotpath::carryover_fold_cold` parses the durable carryover inside timing. `hotpath::carryover_fold_rebirth_warm` primes a rebirth cursor and appends one `Registered` frame outside timing, then measures the delta fold. Their legacy full-text fixture rows are 4,800 bytes at p50, about 13,200 at p90, and 35,900 at maximum, and the 3,300-row and 13,200-row files are 32,943,350 and 131,784,597 bytes. Cold allocation includes transient raw-file text and decoding, not only the retained projection; production rotations now write already-trimmed ended rows. `hotpath::carryover_fold_cold_trimmed` folds the same history cold in that trimmed shape (`crates/rimz/src/testkit/mod.rs::fleet::seed_trimmed_carryover`), so it times the steady state, while `carryover_fold_cold` times the first cold fold after an upgrade. To measure a CLI read's peak RSS on these fixtures, follow [profiling.md → Peak RSS of a CLI read](./profiling.md#peak-rss-of-a-cli-read).

#### Reference figures

The latest capture of each bench. Run one group with a name filter, such as `cargo xtask perf --bench fleet -- publish` or `cargo xtask perf --bench hotpath -- carryover_fold`. Paired values are for the bench's listed arguments in order. A change that replaces a row here also measures that row's bench at its merge base and cites both figures, so a reader can split trunk drift from the change's own effect.

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
| `fleet::idle_cycle_hidden_fanout` | 5.328 ms | 29,973 requests, 3,467,780 bytes requested | 2026-10-29 |
| `fleet::concurrent_hidden_fanout` 200 agents / 100 panes | 1.750 ms publication-to-paint | not quoted | 2026-10-29 |
| `hotpath::fuse_owned_no_overlay` 40 agents | 74.7 µs | 819 B alloc/op | not recorded |
| `hotpath::rollup_fold_warm` 40 agents, 0 / 1,500 carryover rows | 56.9 / 79.8 µs | 655.6 / 655.6 KB alloc/op | 2026-09-14 |
| `hotpath::rollup_fold_unchanged` 40 agents, 0 / 1,500 carryover rows | 2.24 / 2.99 µs | 0 / 0 B alloc/op | 2026-09-14 |
| `hotpath::carryover_fold_cold` 3,300 / 13,200 rows | 58.97 / 299.4 ms | 107.9 / 420.5 MB alloc/op | 2026-10-07 |
| `hotpath::carryover_fold_cold_trimmed` 3,300 / 13,200 rows | 64.66 / 287.5 ms | 52.86 / 202.4 MB alloc/op | 2026-10-27 |
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
| `hotpath::spending_live_scale_additional_workspace_scope` 6k files / 102k entries, all in scope | 100.1 ms | 33.75 MB alloc/op | 2026-10-29 |
| `hotpath::spending_live_scale_additional_workspace_partial_scope` 6k files / 102k entries, one-third in scope | 28.72 ms | 15.28 MB alloc/op | 2026-10-29 |

The 2026-10-29 scope rows quote the second head run of a base/head/head/base check (three samples per bench), on Linux x86-64 with an EPYC 7502P, 58 available CPUs and Rust 1.97.1 under ambient build load. Both benches use `bench_local_refs`; the medians exclude fixture teardown, including the 6,000-file tempdir removal and walker cache/memo cleanup. Both trees use trunk `d602478a2` plus `test(spending): pin the room spend slice before splitting its fold`; head adds `perf(spending): derive a room's spend slice without the fleet fold`.

Allocation medians were stable across each build's two runs: all-in-scope used 98.28 MB alloc/op on base versus 33.75 MB on head; one-third-in-scope used 80.35 MB versus 15.28 MB. One-minute load before/after each whole command (including compilation and fixture setup) was 30.56/32.21, 32.21/79.63, 79.63/74.96 and 74.96/39.19 in base/head/head/base order. These loaded timing medians are not an isolated speedup measurement.

The 2026-10-14 capture ran at a load average near 45, where two back-to-back runs of the same build read 6.4 ms and 12.2 ms for one arm, so this table quotes no median from it. The `kimi`, `grok`, and `droid` changed-session refresh benches have no recorded capture.

At fixed room size and across both room sizes, local publication allocates about 3.68 KB per subscriber: groups, rows, and history do not multiply with subscribers. Local configuration, worktree roots, and pane metadata stay owned per pane, and a real exclusion still pays for a detached group vector and the changed collections, as `exclude_agent` shows.

##### Concurrent hidden attachment fanout

On 2026-10-29, baseline A (`test(sidebar): instrument concurrent attachment fanout`) and head (`perf(sidebar): keep settled hidden passes off the views lock`) were measured on trunk `bc6bf69254e`. Both ran their own same-tree fleet bench, A in a temporary detached worktree, serially sharing a target directory. Order was A1, H1, H2, A2, each running `cargo xtask perf --bench fleet -- idle_cycle_hidden_fanout` then `cargo xtask perf --bench fleet -- concurrent_hidden_fanout`. The Linux x86-64 EPYC 7502P host had 58 available logical CPUs, uncontrolled ambient load and no induced load.

At 100 panes (one watched, 99 hidden), each concurrent run measured 100 content publications. Paint triples are nearest-rank p50 / p99 / max in milliseconds, from publication to the watched paint. Reported `paint_late_ms_*` equals `paint_ms_*` in all four runs. Reveal is publication to the newly revealed pane's first paint, outside the counting window.

| Run | Paint ms | Reveal ms | Attachment CPU µs/publication, hidden / watched / total |
| --- | --- | ---: | --- |
| A1 | 3.606 / 4.849 / 4.851 | 5.638 | 6,347.4 / 1,855.2 / 8,202.7 |
| H1 | 1.772 / 4.130 / 5.736 | 3.795 | 3,357.2 / 1,884.0 / 5,241.2 |
| H2 | 1.750 / 4.159 / 4.547 | 2.881 | 3,413.3 / 1,853.3 / 5,266.6 |
| A2 | 3.716 / 4.795 / 4.848 | 6.481 | 6,459.6 / 1,905.5 / 8,365.1 |

Both A runs took 141,075 hidden views locks and 143,199 total views locks; both head runs took zero and 1,407 respectively. Hidden EventStore takes stayed at 30,294 per window. Head's aggregate hidden CPU remains about 1.8 times the watched thread's, not small beside it. These counts do not isolate EventStore time from other remaining work; generation-based EventStore/JumpHost publication remains an unmeasured follow-up, not part of this change.

The recorded paint medians, p99s and reveal times were lower in both head runs, but the H1 maximum was higher. Ambient load was uncontrolled, so these are observations, not an attributed wall-time speedup or a live-provider latency guarantee. The sequential fixture stayed below its 38,000-request ceiling: 29,967 requests at the merge base and at A, and 29,973 at head. Its median ms in the same order were 5.175 / 8.254 / 5.328 / 5.323, so this capture shows no consistent sequential timing improvement.

#### Per-frame drainer latency under build load

On 2026-10-24, `crates/rimz/benches/hotpath.rs::drainer_latency` measured the four layer-1 changes together: the publication-bound generation mirror, selective context attachment, boot-aware cursor write classes, and weighted drainer scope. Before (B) is the tree of the measurement-only probe commit, `test(hooks): measure drainer claim-to-applied latency`; after (A) and the single shared coordinator are the tree of `test(perf): measure drainer latency at receipts and prompt replies`, the change's last code commit. Both trees sit on trunk `ed303994fe` (`refactor(time): share exact bound formatting with room probes`). The branch's measured code has not changed since: the later rebase brought in trunk commits that change no drainer, ingress, cursor or snapshot production code; the review round's drop-record aggregation runs only for frames stamped by another boot, which this bench never sends; and the scope-argv refactor builds the same `systemd-run` arguments. Both CLIs were built with `cargo build -p rimz --release --features testkit --bins --locked` into independent temporary target directories. Their build ids are `c1aba0f57552` and `4991060d5e9e`; SHA-256 digests and compiler logs are retained with the results.

Every run generated 300 live Claude root sessions with distinct held owner processes and pane identities, 300 context sidecars, and 1,500 retained history rows. This is not live-room replay. Actual fixture sizes, in bytes:

| Run | Rollup | Durable carryover | Derived carryover | Context sidecars |
| --- | ---: | ---: | ---: | ---: |
| Quiet A | 9,338,539 | 9,428,117 | 9,075,688 | 44,256 |
| Quiet B | 9,342,677 | 9,428,141 | 9,075,713 | 44,259 |
| Loaded B1 | 9,345,427 | 9,428,171 | 9,075,743 | 44,261 |
| Loaded A1 | 9,346,743 | 9,428,144 | 9,075,716 | 44,249 |
| Loaded A2 | 9,342,643 | 9,428,120 | 9,075,692 | 44,251 |
| Loaded B2 | 9,344,747 | 9,428,156 | 9,075,728 | 44,256 |

Each run measured 300 serial Stops and the first prompt of each of the 300 sessions. Claim-start to applied uses the testkit probe in `crates/rimz/src/harness/hook_drain.rs::Drainer::drain`: immediately before the claim workspace-lock acquisition through applied cursor publication, including generation lookup, both cursor writes and the apply child. Append-start to receipt includes the append and a covering receipt matched by ingress id, including any truncation before that reply. Prompt wall includes spawning `hooks feed --source claude`, collecting stdout and waiting for exit. A reply requires successful JSON with `hookSpecificOutput.hookEventName == "UserPromptSubmit"` and nonempty `additionalContext`; neutral stdout counts as a miss. The production 1.2 s reply wait was not overridden. Cold startup is separate. The following triples are nearest-rank p50 / p99 / max, in milliseconds; misses are out of 300 prompts per row.

| Run | Claim-start → applied | Append-start → receipt | Prompt wall | Misses |
| --- | ---: | ---: | ---: | ---: |
| Quiet A | 69.158 / 91.049 / 141.715 | 72.135 / 113.757 / 152.646 | 80.508 / 178.022 / 236.812 | 0 |
| Quiet B | 138.128 / 299.037 / 440.107 | 147.557 / 306.611 / 451.588 | 140.211 / 382.711 / 490.933 | 0 |
| Loaded B1 | 105.592 / 218.705 / 249.348 | 112.652 / 225.137 / 264.026 | 127.709 / 339.982 / 380.167 | 0 |
| Loaded A1 | 163.449 / 441.461 / 603.694 | 171.140 / 458.683 / 609.867 | 193.789 / 366.779 / 375.457 | 0 |
| Loaded A2 | 134.287 / 230.763 / 251.142 | 144.834 / 282.910 / 328.419 | 214.082 / 335.349 / 377.768 | 0 |
| Loaded B2 | 105.446 / 279.281 / 431.395 | 112.807 / 291.136 / 513.693 | 175.565 / 518.046 / 743.629 | 0 |

The host was Linux on a Ryzen 9 9950X with 28 online logical CPUs and ZFS temporary storage. Quiet means no induced load, not an idle shared host; its pair ran A then B to check weighted placement first. Loaded order was B, A, A, B. Each case ran inside `systemd-run --user --scope --quiet --collect --unit rimz-proof-<case>-eg1vxx6y.scope -- bash <case-script>`, sharing that non-agent scope with a sustained loop of `CARGO_TARGET_DIR=<fresh-target> cargo build -p rimz --release -j "$(nproc)" --locked` from the probe tree. Every cold target was on ZFS and removed after its build or termination. All four builds remained in their first iteration, with active rustc at measurement start and end. One-second process snapshots give the following load CPU-time lower bounds (the sum of each observed PID's maximum CPU seconds); complete shell/waited-child times are also retained.

| Loaded run | Active rustc, start / end | Sampled load CPU, at least seconds | Host one-minute load, start / end |
| --- | ---: | ---: | ---: |
| B1 | 14 / 1 | 372 | 28.09 / 22.00 |
| A1 | 7 / 1 | 393 | 17.03 / 53.68 |
| A2 | 13 / 1 | 233 | 51.53 / 57.41 |
| B2 | 6 / 1 | 453 | 62.11 / 35.72 |

The drainer was reached only through production `hooks feed` spawning, never a direct `hooks drain` launch. The isolated fixture runtime linked its `systemd` directory to the caller's user-manager socket directory; passing a D-Bus address alone did not provide that connection. All cgroups below are under `/user.slice/user-1000.slice/user@1000.service/app.slice/`. Before drainers shared the runner/load scope; after drainers used distinct weighted helper scopes. Every row sampled 601 apply children, all matching its drainer's cgroup and weight. `null` records an absent `cpu.weight` file, not an observed weight of 100.

| Run | Drainer PID | Cgroup suffix | cpu.weight | Cold hook / ready, ms |
| --- | ---: | --- | ---: | ---: |
| Quiet A | 1129109 | `rimz-helper-01a16df0447b74949231c7a26602fe1e.scope` | 1000 | 55.726 / 114.201 |
| Quiet B | 1406715 | `rimz-proof-quiet-before-eg1vxx6y.scope` | null | 11.177 / 126.155 |
| Loaded B1 | 1489239 | `rimz-proof-loaded-before-1-eg1vxx6y.scope` | null | 10.394 / 156.826 |
| Loaded A1 | 1544688 | `rimz-helper-01a16dfb308f7757b4fea54bd2c08e2a.scope` | 1000 | 157.175 / 598.138 |
| Loaded A2 | 1617472 | `rimz-helper-01a16dfe1a0571b99af6c47977997ce8.scope` | 1000 | 59.162 / 211.827 |
| Loaded B2 | 1684747 | `rimz-proof-loaded-before-2-eg1vxx6y.scope` | null | 8.412 / 164.511 |

Both loaded after-runs meet the acceptance target: claim-start to applied p99 below 1 s and zero prompt misses. Their largest prompt walls, 375.457 and 377.768 ms, also stay below 1.2 s. The baselines meet these targets too, so this fixture did not reproduce the incident's seconds-to-minutes lag. Paired A/B claim median ratios are 1.548 and 1.274 (slower after); claim p99 ratios are 2.019 and 0.826, and prompt p99 ratios 1.079 and 0.647. Ambient load varied substantially. These runs establish target compliance and successful placement, not individual fixes' contributions or queue stability under concurrent arrivals; round 3 below supplies the loaded A/B speedup verdict.

During loaded B1, the supplied fsync probe measured 300 alternating durable/cache-class replacements on the same ZFS temporary filesystem:

| Replacement, ms | p50 | p99 | max |
| --- | ---: | ---: | ---: |
| Write + file fsync + rename + directory fsync | 6.99 | 69.27 | 121.17 |
| Write + rename, no fsync | 0.22 | 4.05 | 6.48 |

Raw samples, ingress ids and receipts, every sampled apply placement, fixture sizes, binary digests, build/command logs, case scripts, the summary table and `analysis.json` were retained outside the repository, on the measuring host. JSON rows are selected from each Divan log with `jq -Rc 'fromjson? | select(.drainer_latency)'`. The detached probe worktree and temporary build targets are removed after the experiment; earlier measurements above remain historical.

**Git discovery was deliberately retained.** Verified pins avoid git resolution in the foreground hook, not the apply child's cwd-specific worktree and branch discovery. The earlier 200-run ambient-load probe of `git rev-parse --show-toplevel --git-common-dir --abbrev-ref HEAD` measured median 3.061 ms and p99 8.053 ms at one-minute load 36.41 on 28 CPUs (`fix2-reference.md` in the artifacts). That is gross subprocess cost, not a measured net saving or a controlled build-load comparison. Pins do not carry worktree/branch facts; an equivalent in-process reader needs environment, ownership, config-include and ref-ambiguity rules. Fix 2 was dropped, leaving that resolver work for PR 2 rather than expanding this change.

##### Weighted-scope isolation

A second round on 2026-10-24 used the same fixture, coordinator and cold-build recipe, in order S, P, B, B, P, S. S and P used the identical head binary (built from round 1's after tree): S used a helper scope at weight 1000; P forced the existing plain fallback by giving the coordinator an empty parent `XDG_RUNTIME_DIR`, preventing its manager-socket link. P and probe-commit B shared their runner/load scopes, with `cpu.weight` absent. All six runs passed placement validation, each with 601 matching apply-child samples, 300 Stops and 300 first prompts. No Rust code changed; quiet and fsync cases were not repeated. Times below are milliseconds except main-thread runqueue wait, which is microseconds per frame.

| Run | Claim p50 / p99 / max | Prompt p50 / p99 / max | Misses | Host load, start / end | rustc, start / end | Runqueue wait, µs/frame |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| S1 | 81.447 / 124.041 / 725.867 | 94.755 / 253.985 / 290.333 | 0 | 28.25 / 22.14 | 10 / 1 | 24.256 |
| P1 | 81.997 / 173.601 / 235.205 | 87.248 / 496.090 / 556.471 | 0 | 21.16 / 23.47 | 14 / 1 | 376.887 |
| B1 | 86.369 / 154.928 / 176.650 | 127.843 / 260.033 / 639.134 | 0 | 28.30 / 19.42 | 5 / 1 | 118.307 |
| B2 | 93.124 / 182.661 / 388.964 | 132.029 / 329.930 / 397.326 | 0 | 18.37 / 14.87 | 5 / 1 | 94.582 |
| P2 | 65.438 / 145.183 / 176.046 | 85.844 / 189.827 / 234.936 | 0 | 13.68 / 12.98 | 12 / 1 | 49.899 |
| S2 | 67.906 / 91.462 / 93.571 | 89.433 / 143.950 / 158.388 | 0 | 11.45 / 13.10 | 15 / 1 | 3.267 |

Scope attribution remains unresolved: paired S/P claim-median ratios are 0.993 and 1.038, straddling 1, while claim-p99 ratios 0.715 and 0.630 favor S. S/P prompt medians are higher at 1.086 and 1.042, but prompt-p99 ratios favor S at 0.512 and 0.758. P/B claim-median ratios are 0.949 and 0.703; S/B ratios are 0.943 and 0.729. Neither head arm reproduces round 1's claim-median slowdown relative to B. The recorded load endpoints, 11.45 to 28.30, were lighter than round 1's 17.03 to 62.11, so this round does not settle the higher-load cause. The main-thread runqueue wait is lower in S than in P in both pairs (24.256 against 376.887, and 3.267 against 49.899 µs per frame), so the weight does reach the scheduler. The scope policy is unchanged.

External snapshots of `/proc/<drainer>/schedstat` at the cold row and final prompt supplied the second-field runqueue-wait delta, divided by 600 frames above. This measures the main drainer thread, not apply-child wait. End-of-run cgroup `cpu.pressure`, raw samples, per-50-Stop upper medians, paired ratios, build/digest/command logs and scheduler timestamps were retained outside the repository, on the measuring host. P/B pressure includes the competing build, unlike S's helper-scope pressure. The benchmark itself was unchanged.

##### Loaded A/B rerun with ambient CPU

Round 3 on 2026-10-24 (UTC) rebuilt both trees on trunk `a38168dc38`: B from `test(hooks): measure drainer claim-to-applied latency`, A and the shared coordinator from `test(perf): measure drainer latency at receipts and prompt replies`. The release testkit build recipe and fixture were unchanged; build ids were `d5e1c6e0a384` (B) and `11b111749ed0` (A). Eight loaded cases ran B1, A1, A2, B2, B3, A3, A4, B4, paired consecutively. No quiet or plain-fallback arm ran. All eight passed validation on the first attempt: 300 Stops, 300 first prompts, 601 matching apply-child placements, and active rustc at both ends. Every A drainer used a distinct helper scope at weight 1000; every B drainer shared its runner/load scope.

At the cold row and final prompt, the external observer also read cgroup v2 `cpu.stat` `usage_usec` for the host, case scope and drainer scope, timestamping each reading. Ambient CPU is Δhost minus Δcase, minus Δdrainer only when the drainer scope differs from the case scope. Average ambient CPUs divide that CPU time by the host samples' wall-time delta. These sequential snapshots are not atomic; they cover the observation window, as do the main-thread schedstat samples. No run was excluded for ambient load. Raw readings, CPU deltas, scope identities, binary digests, build logs and analysis were retained outside the repository, on the measuring host.

Timing triples remain nearest-rank p50 / p99 / max, in milliseconds. Receipt means Stop append-start to its covering receipt.

| Run | Claim p50 / p99 / max | Receipt p50 / p99 / max | Prompt p50 / p99 / max | Misses |
| --- | ---: | ---: | ---: | ---: |
| B1 | 87.420 / 178.569 / 186.963 | 90.519 / 190.310 / 199.687 | 136.124 / 309.445 / 344.271 | 0 |
| A1 | 78.098 / 115.292 / 125.361 | 81.813 / 117.769 / 127.283 | 95.019 / 148.636 / 167.858 | 0 |
| A2 | 72.831 / 100.977 / 110.688 | 78.752 / 112.647 / 130.675 | 88.313 / 148.919 / 254.912 | 0 |
| B2 | 89.425 / 206.087 / 294.345 | 92.816 / 211.565 / 297.474 | 117.256 / 308.403 / 340.882 | 0 |
| B3 | 96.299 / 142.045 / 245.120 | 108.356 / 147.648 / 260.511 | 154.099 / 275.366 / 410.461 | 0 |
| A3 | 80.304 / 136.549 / 170.350 | 87.636 / 140.664 / 173.575 | 109.021 / 357.156 / 541.514 | 0 |
| A4 | 112.330 / 218.433 / 248.400 | 136.719 / 321.267 / 364.630 | 131.279 / 351.918 / 396.852 | 0 |
| B4 | 125.505 / 447.455 / 719.277 | 129.733 / 462.699 / 726.586 | 131.548 / 274.793 / 399.034 | 0 |

| Run | Ambient CPUs | Host load, start / end | rustc, start / end | Runqueue wait, µs/frame | Target |
| --- | ---: | ---: | ---: | ---: | --- |
| B1 | 6.407 | 27.50 / 20.70 | 6 / 1 | 327.443 | pass |
| A1 | 6.121 | 20.70 / 15.66 | 10 / 1 | 10.646 | pass |
| A2 | 4.791 | 15.66 / 13.51 | 6 / 1 | 6.176 | pass |
| B2 | 5.814 | 13.51 / 17.42 | 7 / 1 | 69.081 | pass |
| B3 | 8.049 | 17.42 / 19.57 | 7 / 1 | 234.181 | pass |
| A3 | 9.721 | 19.57 / 22.86 | 6 / 1 | 299.586 | pass |
| A4 | 14.256 | 22.86 / 28.65 | 9 / 1 | 494.477 | pass |
| B4 | 13.327 | 28.65 / 22.65 | 9 / 1 | 1368.202 | pass |

Before the runs, the rule was registered: claim "faster under load" only if A's claim median is below B's in all four consecutive pairs. Ratios below are A/B, not pooled across runs.

| Pair | Claim p50 ratio | Claim p99 ratio | Prompt p99 ratio |
| --- | ---: | ---: | ---: |
| A1/B1 | 0.893 | 0.646 | 0.480 |
| A2/B2 | 0.814 | 0.490 | 0.483 |
| A3/B3 | 0.834 | 0.961 | 1.297 |
| A4/B4 | 0.895 | 0.488 | 1.281 |

The rule holds: A was faster by claim median in four of four pairs, with ratios 0.814 to 0.895 and ambient load of 4.791 to 14.256 CPUs. All eight runs met the target (claim p99 below 1 s, zero prompt misses). A's claim p99 ranged from 100.977 to 218.433 ms; its largest prompt wall was 541.514 ms. Prompt p99 improved in only two pairs, so the claim-median improvement is not a uniform prompt-tail improvement. A's main-thread runqueue wait was lower than B's in three pairs and higher in A3/B3 (299.586 against 234.181 µs per frame). This establishes a loaded A/B speedup for this recipe, not the cause of round 1's slowdown or the contribution of each fix. The bench sends hooks one at a time, so it [does not reproduce the incident's backlog](#drainer-backlog-under-arrival-load). The before binary also met the target.

#### Drainer backlog under arrival load

The Linux-only `crates/rimz/benches/hotpath.rs::drainer_backlog` is an open-loop sibling of the serial latency bench, ignored by default so an unfiltered performance run does not start the sweep. Run it with `cargo xtask perf --bench hotpath -- drainer_backlog --ignored`; `RIMZ_HOOK_BUDGET_BIN` selects the CLI under test. Select its JSON rows with `jq -Rc 'fromjson? | select(.drainer_backlog)'`. It asserts run validity, not a rate or latency gate.

Each run keeps the existing 300 live-root fixture, context sidecars and 1,500 retained history rows. Of those roots, 46 send round-robin, each walking a staggered 20-frame turn: one UserPromptSubmit, 18 non-mutating Read PostToolUse frames and one Stop. One serial warm-up prompt per sender spends the runtime-environment first-prompt claim before measurement. Rates are 1, 2, 4, 8 and 16 frames/s, 60 s each, with a complete drain between steps, bounded at 10 minutes. Each arrival is due at step start plus its index divided by rate. Late sends catch up without dropping frames or moving the schedule. Prompts wait on separate threads for a covering receipt within 1.2 s; other frames nudge and close. The sender never waits for a receipt, and a held lease keeps the drainer alive.

`claim_to_applied_us` is the testkit interval in `crates/rimz/src/harness/hook_drain.rs::Drainer::drain`, from before the claim lock through applied cursor publication, excluding time queued before claim. `append_to_applied_us` starts immediately before append and ends when `crates/rimz/benches/hotpath.rs::latency::TimingReader::start` reads that frame's timing record. This is an upper observation bound on cursor publication, including the drainer's post-publication response work, nominal 10 ms polling and any scheduling delay, not a hard 10 ms error bound. Every sweep frame must have exactly one matching successful timing record; cold and warm-up records do not count. A late prompt lacks a covering receipt within the reply thread's 1.2 s wait; empty context on a repeat prompt is not a miss. Receipted-prompt latency starts before append, unlike its wait deadline.

Five-second `window` rows report cumulative sent/applied counts, observed depth (sent minus applied), applied-in-window latency distributions and host load. Windows restart at each send/drain phase; a phase's last window may be short, an empty drain still emits a window, empty distributions are null, and `t_ms` marks the end. Each `step` row includes its drain tail in the latency distributions. Applied/s counts frames observed applied by send end divided by the actual send-phase duration, which extends past 60 s if sends run late. Depth end is at that boundary, depth max spans the step, and drain time ends at its last applied observation. Triples below are nearest-rank p50 / p99 / max in milliseconds; drain is milliseconds too.

On 2026-10-24 (UTC), loaded runs L1 and L2 used the same CLI built from trunk `1f7ce3556b` (`docs(store): hold replaced files open in forged-identity tests`) in a detached worktree with a fresh target: `cargo build -p rimz --release --features testkit --bins --locked`. Both fixture rows report build id `2b08d9eb3478`. The coordinator was the tree of `test(perf): add an open-loop drainer backlog sweep` on that base. Each case shared a non-agent `systemd-run --user --scope` with a sustained loop of `CARGO_TARGET_DIR=<fresh-target> cargo build -p rimz --release -j "$(nproc)" --locked` from the merge base. Both drainers used distinct helper scopes at CPU weight 1000.

The host was the same Ryzen 9 9950X, with 28 online logical CPUs and ZFS temporary storage. Ambient CPU uses [round 3's cgroup method](#loaded-ab-rerun-with-ambient-cpu), sampled at the config row and final step row so it covers the send and drain phases but excludes fixture setup and warm-up. Readings are sequential, not atomic. Rustc counts are live compilers in the injected build's process session; one-second process snapshots and build-loop logs retain the intervening load phases.

| Run | Ambient CPUs | Host load, start / end | Window load, min / max | rustc, start / end |
| --- | ---: | ---: | ---: | ---: |
| L1 | 6.054 | 42.29 / 34.43 | 16.06 / 42.96 | 1 / 1 |
| L2 | 14.358 | 27.12 / 29.22 | 20.56 / 109.37 | 1 / 1 |

| Run | Arrivals/s | Applied/s | Depth end | Depth max | Drain ms | Append → observed applied, ms | Claim → applied, ms | Prompts late / sent |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| L1 | 1 | 1.000 | 0 | 1 | 0 | 86.295 / 228.639 / 228.639 | 77.029 / 213.605 / 213.605 | 0/3 |
| L1 | 2 | 2.000 | 0 | 1 | 0 | 138.681 / 363.457 / 433.407 | 131.945 / 312.523 / 344.764 | 0/6 |
| L1 | 4 | 4.000 | 0 | 3 | 0 | 97.615 / 529.125 / 539.707 | 87.235 / 328.833 / 330.568 | 0/10 |
| L1 | 8 | 8.000 | 0 | 5 | 0 | 90.258 / 504.260 / 517.346 | 78.966 / 167.291 / 187.562 | 0/26 |
| L1 | 16 | 8.500 | 450 | 451 | 55897 | 25337.308 / 55882.738 / 55960.259 | 94.332 / 290.836 / 405.949 | 45/48 |
| L2 | 1 | 1.000 | 0 | 1 | 0 | 113.885 / 293.024 / 293.024 | 99.424 / 279.250 / 279.250 | 0/3 |
| L2 | 2 | 2.000 | 0 | 2 | 0 | 230.984 / 382.245 / 620.906 | 219.708 / 329.424 / 458.504 | 0/6 |
| L2 | 4 | 4.000 | 0 | 4 | 0 | 201.566 / 545.541 / 796.305 | 188.439 / 372.452 / 450.558 | 0/10 |
| L2 | 8 | 5.100 | 174 | 175 | 29656 | 18764.399 / 29930.010 / 29961.909 | 187.222 / 318.731 / 409.743 | 25/26 |
| L2 | 16 | 10.150 | 351 | 352 | 37114 | 12792.829 / 36873.573 / 37176.587 | 88.916 / 203.533 / 235.928 | 45/48 |

Both runs applied all 1,860 scheduled frames; maximum send lag was 77.952 ms in L1 and 199.408 ms in L2. L1's first growing step was 16/s, after holding at 8/s; L2's was 8/s, after holding at 4/s. Their five-second send-window depths grew from 20 to 450 and from 18 to 174 respectively. These are observed brackets, not one fixed capacity. L2's 8/s step ran at that run's load peak, with window one-minute load between 91.35 and 109.37 against 39.19 to 70.6 at 16/s. It applied 5.1/s there, against 10.15/s at 16/s, so L2's 8/s growth reflects a load phase, the case two runs were taken to expose. Both 16/s steps grew, so the ladder did not need a 32/s extension.

At the incident-rate steps of 1 and 2 frames/s, trunk holds on this fixture in both runs, with no late prompts. Claim medians here were 77.029 to 219.708 ms, against the incident's observed 0.47 to 3.4 s per frame. The cause of that per-frame cost gap remains unmeasured. This is a loaded arrival-capacity baseline, not live-room replay; SessionStart and mutating-tool work are not represented.

Raw Divan rows, row-validation results, one-second build-process snapshots, CPU readings and deltas, scope identities, scripts, binary digest and compiler logs were retained outside the repository on the measuring host. The detached merge-base worktree and all fresh build targets were removed after measurement.

#### Scale yardstick

The Linux-only ignored `crates/rimz/benches/hotpath.rs::apply_scale` and `crates/rimz/benches/hotpath.rs::host_scale` benches measure cold hook application and a whole real tmux room. They report today's work, not a target ceiling. Build one release testkit CLI, then run the benches serially, without another build or test in this checkout:

```sh
cargo build -p rimz --release --features testkit --bins --locked
RIMZ_XTASK_TIMEOUT=90m cargo xtask perf --bench hotpath -- apply_scale --ignored
RIMZ_SCALE_COMPARE_SUPERVISOR=1 RIMZ_XTASK_TIMEOUT=180m cargo xtask perf --bench hotpath -- host_scale --ignored
```

Run long commands through `rimz wait --run` and retain their output under a unique directory in `$TMPDIR`. `RIMZ_HOOK_BUDGET_BIN` selects another release testkit CLI. `RIMZ_SCALE_ROOMS=50x300,200x1500` selects apply rooms as live agents × closed rows; `RIMZ_SCALE_ROOMS=50x20,200x80` selects host rooms as agents × sidebars. Unset, the selectors run the full grids below. The timed host is started by the bench, not by a sidebar supervisor; `RIMZ_SCALE_COMPARE_SUPERVISOR=1` adds 120 s `parity_idle` windows for both start modes on the first selected room (50/20 here: 5,470 / 6,070 host CPU ms/min, 58.42 / 58.17 fetch cycles/min and 237 / 236.5 host git `status` calls/min, bench-started / supervisor-started). Neither fixture reads the real home or starts a provider. The host uses logged-out Claude stand-ins, missing Codex probes, offline account usage and no transcripts or queued messages. Provider API/auth costs, transcript discovery and the stuck message-sweep-stamp defect are not represented.

Each stdout JSON line is keyed by `apply_scale` or `host_scale`; extract it with `jq -Rc 'fromjson? | select(.apply_scale)'` or the host equivalent. Distributions carry nearest-rank `p50`, `p99`, and `max`. Apply rows are `fixture` (shape, build/version, file sizes, setup SessionStart wall/CPU), `frame` (50 direct cold children each for human Prompt, Read, Stop and AgentPrompt, wall/CPU and every work-counter distribution), and `drain` (100 Read frames from 25 working roots, one nudge, throughput and claim-to-applied distribution). AgentPrompt is a UserPromptSubmit delivering a fresh batch-sent message from another live agent; it runs after the drain so the existing frame and drain fixtures stay unchanged. The fixture's `latest_live_row_bytes` and `rollup_live_row_bytes` report mean and maximum compact serialized bytes per live row from the persisted `agents` and `raw_agents` arrays, excluding ended rows and array delimiters. `load_1m_before` in the fixture and `load_1m_after` in the drain row bracket setup and the original frames and drain, excluding compilation and AgentPrompt. The burst appends ingress under the workspace lock rather than spawning 100 feed producers, so it measures serial drain capacity, not feed launch capacity. When citing a measurement, give the CLI build ID that the apply `fixture` row reports (`crates/rimz/src/build_id.rs::of_file`, read from the binary the bench times), not the ID of a CLI built before the bench command; the two can differ, as they did for the [shared-row comparison](#shared-row-hook-comparison).

Where `perf` is unavailable, attribute a cold frame with temporary testkit `Instant` timers exported as work-log keys, timing each container rebuild (collect, `into_values`, clone) apart from JSON decode and inserting keys iteratively rather than in one `json!` expansion, whose recursion limit bounds its size. Agent sandboxes run with `NoNewPrivs: 1` and `perf_event_paranoid` 4, so `perf` runs only from the user's own shell. The host's per-pane attachment loops and input readers carry `rimz-attach`; the yardstick attributes their CPU through per-task `/proc` samples instead.

Host rows are `room` (shape, total/hot worktrees, build/version, traced flag), `window` (untraced idle/busy or optional parity process deltas and spawns), `trace` (separate traced idle room, or a skip reason), and `work` (untraced host lifetime counters after exit, including warm-up). Each window has its own checkout: window 0 uses the main checkout and every other window a linked worktree. The 20-sidebar rooms have 20 hot and zero cold worktrees; the 80-sidebar room has 25 hot and 55 cold. All checkouts share one empty commit, so git child CPU is a floor; use git spawn counts for before/after comparisons of the git lane, not these figures as a real-repository CPU baseline. Each stub holds sixteen threads and completes a turn before measurement. Each host warms for 60 s, measures idle and busy for 120 s each, and feeds 240 frames during busy, two frames/s in staggered 20-frame turns over 25 agents. Warm-up requires host-parented git in every checkout. All agent bindings and sidebar attachments are asserted before and after every window; teardown asserts every room process is gone.

"Per minute" covers the whole room, not one synchronized pass: fetch, refresher, watchers and renderers run independently. Host, serve-sum and room tmux-server CPU come from `/proc/<pid>/stat`; host-reaped children use `cutime + cstime`, and rimz-child CPU uses work records completed in the window, excluding `hooks feed`. Those child CPU figures can overlap, and omit killed or still-outstanding children. Process samples are sequential, not atomic room-wide cuts. Wakeups are voluntary plus involuntary task context-switch deltas, not timer firings; tasks that exit before the end are omitted and counted in `departed_tasks`. Reads are process `syscr` and `rchar`, not disk bytes, and need the per-path trace beside them. Fetch rates normalize diagnostic intervals ending in the window by their own covered seconds; those intervals may cross a phase boundary.

`cpu_ms_per_minute.host_attach` and `wakeups_per_minute.host_attach` sum the host tasks whose comm is `rimz-attach` at both boundaries, using task `stat` user/system ticks and status context switches. `host_attach_threads` counts those tasks: one attachment loop and one input reader per attached pane (two per pane). Room-wide workers inherit `rimz-plane` and are excluded. Unlike `serve`, which measures pane supervisors, these fields measure attachment work inside the host; exited tasks are excluded, as `wakeups_scope` states.

Spawn rows group parent (`host`, `serve`, `other`), endpoint (`tmux`, `git`, `rimz`) and subcommand. PATH shims log tmux/git attempts with JSON argv and exec the real binary; rimz records count completed commands, not every spawn attempt. Other executables are not counted. PATH shims add their own logging/exec cost, so keep the fixture unchanged for before/after. The separate trace uses `strace -ff --seccomp-bpf -e trace=openat,clone,clone3` for a 120 s idle window after warm-up. It follows the host's `CLONE_THREAD` lineage, including new and transient threads but excluding forked helpers, and asserts coverage of every boundary-live host thread. `host_thread_files` counts the host-lineage trace files read so far, including startup, not concurrent threads. Open attempts, including failures, are filtered to the measurement interval; process/session/temporary IDs are normalized in non-`/proc` path classes. Trace CPU is discarded. Compare traced and untraced idle fetch rates before reading opens/minute as an untraced rate; opens/fetch-cycle is a ratio of rates from differently bounded intervals, not a direct per-cycle count, and cannot remove tracing's other effects.

`RIMZ_TEST_WORK_LOG` names the cross-process JSONL channel (`crates/rimz/src/cli/work_log.rs::WorkLog::append`). Under `testkit`, each rimz process appends one record with one O_APPEND write when `main` returns, on success or error: pid/ppid, parsed subcommand path without payloads or positional values, start/end Unix milliseconds, `getrusage` CPU, peak RSS, minor faults and process work counters. It propagates through hook ingress into apply children and through inherited environments into helpers. Commands that leave through `std::process::exit` (including clap help, version and argument-error exits) and signal-killed processes write none. It has no sidecar thread-local counters or price-book loads, and no timing within a long-lived host. Unset costs one environment read; non-testkit binaries have no channel. `crates/rimz/tests/integration/performance/work_log.rs::cold_frame` pins two assemblies per Prompt/Read/Stop, nine/eight/eight row copies, zero full machine-config loads and one rollup parse per frame on its small fixed fixture, not a host ceiling. `crates/rimz/tests/integration/performance/work_log.rs::cold_team_stop_with_pending_message_avoids_machine_config_loads` also pins zero full loads on the team-member lifecycle and queued-dispatch path. The larger bench fixture enables runtime Environment context and reports its own counts below.

##### Live-text hook comparison

Base is trunk `1834508ff` plus `test(perf): price live text and fold-row materialization`. Head adds `perf(launch): keep exec requests out of pane argv`, `perf(store): bound prompt labels on every agent row`, `perf(store): reuse cached reads within each hook frame`, `perf(store): borrow durable audit rows for hook reactions`, and `perf(config): narrow dispatch defaults and session accounts`. This compares the combined changes, not their individual contributions. Clean release testkit CLI build IDs are `6cc737c33671` (base) and `9eb51ea26fc5` (head).

Both sides ran their own same-tree bench and CLI, with base in a standalone clone and separate target directory. After warming each at 200 live / 300 closed, the measured order was B1, H1, H2, B2 on 2026-10-28, 08:44:07 to 08:53:04 UTC, using `RIMZ_SCALE_ROOMS=200x300,200x1500`. The Linux x86-64 EPYC 7502P host had 58 available logical CPUs, the `performance` governor and ZFS temporary storage. No load was induced; ambient activity was uncontrolled. Per-cell one-minute load brackets appear with the drain figures below. Timings describe these runs, not an isolated or general wall-time speedup.

Each fixture has production-encoded team-member pane argv and a 2,400-byte observed prompt per live row. Latest and raw rollup live-row bytes are equal in each fixture; the shared column reports both, as mean / maximum compact serialized bytes. Totals include their other fields and retained history. The small byte differences between repeats include generated paths and timestamps.

| Live / closed | Run | Latest and rollup live-row bytes, mean / max | Latest bytes | Rollup bytes |
| --- | --- | --- | --- | --- |
| 200 / 300 | B1 | 8,376.505 / 8,386 | 1,683,796 | 1,946,308 |
| 200 / 300 | H1 | 1,661.525 / 1,668 | 340,800 | 603,333 |
| 200 / 300 | H2 | 1,661.560 / 1,668 | 340,807 | 603,352 |
| 200 / 300 | B2 | 8,376.545 / 8,386 | 1,683,804 | 1,946,304 |
| 200 / 1,500 | B1 | 8,377.660 / 8,387 | 1,714,527 | 3,003,898 |
| 200 / 1,500 | H1 | 1,662.660 / 1,667 | 371,527 | 1,660,993 |
| 200 / 1,500 | H2 | 1,662.710 / 1,670 | 371,537 | 1,661,147 |
| 200 / 1,500 | B2 | 8,377.445 / 8,385 | 1,714,484 | 3,003,909 |

Each event has 50 direct cold apply children per cell and run. Prompt is UserPromptSubmit; Read is PostToolUse(Read). Timing pairs are the two runs of that side, B1 / B2 or H1 / H2, in milliseconds.

| Live / closed | Frame | Base wall p50 | Head wall p50 | Base CPU p50 | Head CPU p50 |
| --- | --- | --- | --- | --- | --- |
| 200 / 300 | Prompt | 112.348 / 111.759 | 58.963 / 58.892 | 98.871 / 98.759 | 48.202 / 47.883 |
| 200 / 300 | Read | 92.111 / 92.588 | 53.515 / 52.671 | 83.727 / 84.071 | 47.522 / 46.662 |
| 200 / 300 | Stop | 94.885 / 96.214 | 53.938 / 53.570 | 86.039 / 87.420 | 47.786 / 47.613 |
| 200 / 1,500 | Prompt | 221.556 / 221.618 | 148.582 / 148.652 | 199.871 / 200.725 | 131.247 / 131.192 |
| 200 / 1,500 | Read | 199.957 / 202.868 | 142.068 / 139.666 | 184.557 / 187.390 | 130.255 / 128.373 |
| 200 / 1,500 | Stop | 206.976 / 207.272 | 143.603 / 142.036 | 190.581 / 192.064 | 131.656 / 129.616 |

Work-count p50 / maximum pairs were identical in both repeats. Head's row copies and assemblies do not grow between these two retained-history sizes; its CPU time still does. These measured builds loaded the full machine config zero times for Prompt/Read and once for Stop, whose lifecycle signal read the task catalog; they precede the memoized narrow loop-file reader and the core-only time-zone reader that remove the full loads from Stop and team-member lifecycle signals. In those builds, sender-address resolution on an agent-sent Prompt read every runtime row's context sidecar and, when any context was present, copied the shared runtime-row vector. Those measurements did not cover that path.

| Live / closed | Frame | Base row copies | Head row copies | Base assemblies | Head assemblies |
| --- | --- | --- | --- | --- | --- |
| 200 / 300 | Prompt | 1,900 / 1,900 | 404 / 404 | 7 / 7 | 2 / 2 |
| 200 / 300 | Read, Stop | 1,500 / 1,500 | 402 / 402 | 5 / 5 | 2 / 2 |
| 200 / 1,500 | Prompt | 3,100 / 4,800 | 404 / 404 | 7 / 7 | 2 / 2 |
| 200 / 1,500 | Read, Stop | 2,700 / 2,700 | 402 / 402 | 5 / 5 | 2 / 2 |

Each burst drains 100 Read frames from 25 working roots. Claim-to-applied triples are p50 / p99 / maximum in milliseconds. Load brackets each cell, including fixture setup but excluding compilation.

| Live / closed | Run | Seconds | Frames/s | Claim to applied ms | Load before / after |
| --- | --- | --- | --- | --- | --- |
| 200 / 300 | B1 | 9.560 | 10.461 | 94.268 / 105.862 / 108.486 | 9.75 / 8.54 |
| 200 / 300 | H1 | 5.455 | 18.331 | 53.394 / 61.693 / 61.872 | 6.65 / 6.93 |
| 200 / 300 | H2 | 5.434 | 18.403 | 53.283 / 61.522 / 62.109 | 7.90 / 7.63 |
| 200 / 300 | B2 | 9.959 | 10.041 | 98.032 / 114.169 / 119.496 | 7.58 / 9.40 |
| 200 / 1,500 | B1 | 20.626 | 4.848 | 204.850 / 222.551 / 230.595 | 8.54 / 6.65 |
| 200 / 1,500 | H1 | 14.508 | 6.893 | 142.178 / 165.987 / 173.637 | 6.93 / 7.90 |
| 200 / 1,500 | H2 | 14.549 | 6.873 | 144.010 / 163.127 / 166.290 | 7.63 / 7.71 |
| 200 / 1,500 | B2 | 21.345 | 4.685 | 207.853 / 295.776 / 301.517 | 9.40 / 10.28 |

##### Shared-row hook comparison

The preceding 6.873 to 6.893 frames/s result prompted investigation of a resident apply worker. The decision is **no resident worker**: share fold rows and narrow agent-sender context reads instead. Head now drains 13.141 to 13.351 frames/s at 200 / 1,500, above the 12 frames/s follow-up threshold. That threshold was twice an inferred six frames/s, not a measured arrival rate. A worker around the current fold would remove cold parsing but not the retained-key and name walks, and would need explicit per-frame environment and room-keyed process caches to preserve the account-home isolation that cold children supply ([runtime shape](../../ARCHITECTURE.md#runtime-shape)).

Base is trunk `df40bb7a7` (`fix(loop): place a new loop task after a template's trailing comments`) plus `test(perf): bench agent-sent Prompt frames`, cherry-picked alone. Head adds `perf(store): share fold rows across catch-up` and `perf(hooks): resolve agent senders over launch members only`. Each side used its own standalone clone, target directory, same-tree bench and release testkit CLI, built with the Scale yardstick commands above. Measured CLI build IDs are `9c5153484d21` (base) and `d395de5bdae7` (head), identical across repeats and without `.dirty`. These are the fixture-reported IDs after the first bench command rebuilt each CLI, not the preceding standalone build's identity.

Commands ran in B1, H1, H2, B2 order on 2026-10-28, 18:46:07 to 19:01:34 UTC, with `RIMZ_SCALE_ROOMS=200x300,200x1500`. The Linux x86-64 EPYC 7502P host had 58 available logical CPUs, the `performance` governor and ZFS temporary storage. No load was induced; ambient activity was uncontrolled. The per-cell load brackets below exclude compilation and AgentPrompt, which runs after the drain. These are paired observations under shared load, not an isolated speedup or a sustainable-arrival guarantee.

The fixture is unchanged between sides: 200 production-shaped live rows with observed prompt text and team-member pane argv, plus closed rows present in both carryover and the raw rollup. That double retention overstates cold decode cost relative to a room where each closed row usually occupies only one of those files. Setup sizes are bytes; small repeat differences include generated paths and timestamps. Compact live-row bytes are equal in latest and rollup within each fixture, with means 1,699.620 to 1,700.710 and maxima 1,707 to 1,708.

| Live / closed | Run | Rollup bytes | Carryover bytes | Latest bytes | Event-log bytes |
| --- | --- | --- | --- | --- | --- |
| 200 / 300 | B1 | 610,951 | 313,176 | 348,453 | 1,180,914 |
| 200 / 300 | H1 | 610,913 | 313,200 | 348,419 | 1,180,893 |
| 200 / 300 | H2 | 611,028 | 313,191 | 348,471 | 1,180,938 |
| 200 / 300 | B2 | 610,887 | 313,155 | 348,441 | 1,180,897 |
| 200 / 1,500 | B1 | 1,668,549 | 1,571,108 | 379,128 | 1,636,572 |
| 200 / 1,500 | H1 | 1,668,600 | 1,571,120 | 379,121 | 1,636,589 |
| 200 / 1,500 | H2 | 1,668,534 | 1,571,078 | 379,129 | 1,636,569 |
| 200 / 1,500 | B2 | 1,668,645 | 1,571,153 | 379,137 | 1,636,605 |

Each frame has 50 direct cold apply children per cell and run. Prompt is human UserPromptSubmit; AgentPrompt delivers a fresh batch-sent agent message; Read is PostToolUse(Read). Timing pairs are B1 / B2 or H1 / H2, in milliseconds. Base's small-cell Stop and AgentPrompt vary substantially between repeats; neither observation is discarded.

| Live / closed | Frame | Base wall p50 | Head wall p50 | Base CPU p50 | Head CPU p50 |
| --- | --- | --- | --- | --- | --- |
| 200 / 300 | Prompt | 61.146 / 61.096 | 42.582 / 42.753 | 48.903 / 49.092 | 31.800 / 31.662 |
| 200 / 300 | AgentPrompt | 77.913 / 119.908 | 45.231 / 44.575 | 69.926 / 104.124 | 38.486 / 37.936 |
| 200 / 300 | Read | 54.950 / 56.372 | 37.541 / 36.618 | 47.474 / 48.952 | 31.225 / 30.375 |
| 200 / 300 | Stop | 55.992 / 82.915 | 38.592 / 37.163 | 48.341 / 69.837 | 31.802 / 30.845 |
| 200 / 1,500 | Prompt | 152.313 / 151.536 | 80.778 / 79.644 | 133.305 / 132.783 | 64.350 / 63.544 |
| 200 / 1,500 | AgentPrompt | 190.451 / 192.428 | 83.773 / 83.293 | 176.965 / 177.414 | 72.550 / 72.393 |
| 200 / 1,500 | Read | 144.575 / 142.752 | 72.849 / 73.727 | 131.495 / 130.101 | 62.350 / 63.036 |
| 200 / 1,500 | Stop | 145.721 / 142.445 | 73.622 / 74.360 | 131.909 / 129.643 | 62.733 / 63.495 |

Head's `fold_row_copies` is zero at p50 and maximum for all 800 measured cold children, at both history sizes. Base's former `rollup_base_copies` is 1 / 1 throughout, but counts whole-base copies rather than individual rows and is not comparable. The outside-fold row counter and assembly counts below are p50 / maximum, identical across history sizes and repeats except B2 Read at 200 / 1,500, whose row-copy maximum is 2,102. Full machine-config loads are zero at p50 and maximum throughout both sides.

| Frame | Base outside-fold row copies | Head outside-fold row copies | Assemblies, both sides |
| --- | --- | --- | --- |
| Prompt | 404 / 404 | 404 / 404 | 2 / 2 |
| AgentPrompt | 804 / 804 | 605 / 605 | 3 / 3 |
| Read, Stop | 402 / 402 | 402 / 402 | 2 / 2 |

The 199 fewer AgentPrompt row copies match replacing the full 200-row context-vector copy with one sender-launch member (`crates/rimz/src/address.rs::launch_rows`, `crates/rimz/src/store/mod.rs::Store::rows_with_live_context`). AgentPrompt still performs a third assembly. A cold child still decodes every persisted rollup and carryover row once; head Prompt's median parsed rollup grows from about 0.608 MB to 1.667 MB, and carryover from 0.313 MB to 1.571 MB. Sharing removes deep row copies, not retained-key container work and identity-name walks (`crates/rimz/src/store/snapshot/fold.rs::catch_up_from`, `crates/rimz/src/store/snapshot/project/identity.rs::CardIdentityAllocator::from_map_and_state`). The remaining CPU growth was not phase-timed in these runs.

Each burst drains 100 Read frames from 25 working roots. Claim-to-applied triples are p50 / p99 / maximum in milliseconds. Load brackets each cell's setup, original frames and drain, as in the preceding comparison.

| Live / closed | Run | Seconds | Frames/s | Claim to applied ms | Load before / after |
| --- | --- | --- | --- | --- | --- |
| 200 / 300 | B1 | 5.610 | 17.825 | 55.021 / 64.526 / 68.608 | 27.15 / 19.40 |
| 200 / 300 | H1 | 3.876 | 25.802 | 38.061 / 43.206 / 43.578 | 26.94 / 23.34 |
| 200 / 300 | H2 | 3.818 | 26.190 | 37.411 / 47.660 / 52.506 | 16.77 / 16.31 |
| 200 / 300 | B2 | 7.142 | 14.002 | 68.836 / 88.333 / 95.311 | 15.06 / 25.45 |
| 200 / 1,500 | B1 | 14.393 | 6.948 | 142.173 / 159.157 / 161.476 | 18.33 / 12.81 |
| 200 / 1,500 | H1 | 7.490 | 13.351 | 73.389 / 91.282 / 93.004 | 23.34 / 17.01 |
| 200 / 1,500 | H2 | 7.610 | 13.141 | 74.251 / 88.250 / 105.987 | 16.37 / 15.15 |
| 200 / 1,500 | B2 | 14.630 | 6.835 | 144.202 / 169.910 / 173.378 | 27.17 / 25.70 |

The retained-log arrival sample from the busiest room, covering 2026-10-23 to 28 UTC, counted distinct ingress IDs on event envelopes: peak 2.4 frames/s over one fixed UTC minute and 4.8 over five seconds. The peak minute, Oct 27 at 16:35 UTC, had 16 emitting sessions and 47 to 56 registered, not-ended sessions. These rates are lower bounds: ordinary nonblocking PreToolUse often appends no durable event. Adding one pre-tool frame per completed tool in that minute gives about 4.5 frames/s, a sensitivity estimate, not a measured correction. The persisted rollup held 711 rows, 660 ended, with no carryover. Thus neither the actual arrival peak nor that room's drain capacity is established by this larger double-retention fixture. Revisit a worker only when complete ingress-arrival measurements approach post-change capacity at real room shapes.

Raw per-run frame distributions, all work counters, fixture sizes, build identities and command/load metadata were retained outside the repository on the measuring host. Other live-fleet sizes, the room-host bench and a loaded open-loop arrival sweep were not rerun. The focused-pane binding recovery's same-kind context-vector copy remains a follow-up (`crates/rimz/src/cli/hooks/binding.rs::recover_focused_pane_binding`).

##### Baseline identity and load

The following full-grid apply and host figures are historical baselines, separate from the same-fixture comparison above.

Clean release testkit CLIs on trunk merge base `b46dfb9dd` (`docs(agents): find running builds by process name, not pgrep -af`) plus `test(perf): measure real room-host work across fleet sizes`: apply build `8c93aa3c07b6` measured on 2026-10-27 UTC, and host build `4f3902cd6306` measured on 2026-10-27 to 28 UTC with supervisor comparison enabled. The apply figures retain the original run; the host figures use the corrected git/worktree fixture. Every fixture/room version is free of `.dirty`. The shared Linux x86-64 host had an AMD EPYC 7502P, 58 online logical CPUs, the `performance` governor and ZFS temporary storage. No load was induced; ambient builds/tests were active. Load is the one-minute average before/after each whole bench, including compilation and fixture setup, not a timed-window average. These are single-run baselines under shared load, not a paired speedup claim.

| Bench | Load before / after |
| --- | --- |
| `apply_scale` | 22.97 / 10.85 |
| `host_scale` | 28.00 / 30.32 |

##### Apply baseline

These historical apply tables predate production-shaped live text and the expanded row-copy counter. Use a same-fixture rerun, not these figures, as the before side of a comparison with the current bench.

Setup sizes are bytes. Time triples are p50 / p99 / max in milliseconds. SessionStart counts equal each fixture's live count; each measured frame event has 50 samples.

| Live / closed | Rollup | Raw carryover | Latest | Event log | SessionStart wall ms | SessionStart CPU ms |
| --- | --- | --- | --- | --- | --- | --- |
| 50 / 300 | 1,855,242 | 1,884,603 | 53,695 | 152,170 | 50.159 / 58.458 / 58.458 | 42.879 / 51.130 / 51.130 |
| 200 / 300 | 1,957,803 | 1,884,546 | 192,807 | 268,703 | 63.066 / 89.583 / 97.856 | 55.346 / 80.162 / 88.586 |
| 400 / 300 | 2,100,446 | 1,884,588 | 385,993 | 424,276 | 84.650 / 125.755 / 141.852 | 76.136 / 116.690 / 131.586 |
| 200 / 1,500 | 9,278,336 | 9,428,159 | 260,639 | 724,341 | 197.266 / 241.509 / 242.532 | 176.943 / 220.671 / 222.031 |
| 200 / 3,000 | 18,426,433 | 18,863,003 | 335,660 | 1,295,655 | 423.027 / 626.711 / 652.545 | 379.649 / 552.794 / 593.424 |

Prompt means UserPromptSubmit; Read means non-mutating PostToolUse. The three parse/read byte columns are medians in decimal MB, not timing distributions.

| Live / closed | Frame | Wall ms | CPU ms | Rollup parsed MB | Latest parsed MB | Trimmed carryover read MB |
| --- | --- | --- | --- | --- | --- | --- |
| 50 / 300 | Prompt | 66.649 / 172.159 / 172.159 | 54.237 / 150.367 / 150.367 | 1.866 | 0.068 | 1.814 |
| 50 / 300 | Read | 74.767 / 98.667 / 98.667 | 64.210 / 78.523 / 78.523 | 1.874 | 0.078 | 1.814 |
| 50 / 300 | Stop | 57.065 / 75.644 / 75.644 | 49.358 / 66.040 / 66.040 | 1.875 | 0.080 | 1.814 |
| 200 / 300 | Prompt | 105.417 / 114.710 / 114.710 | 92.164 / 101.450 / 101.450 | 1.970 | 0.208 | 1.814 |
| 200 / 300 | Read | 83.850 / 94.399 / 94.399 | 75.850 / 86.439 / 86.439 | 1.976 | 0.217 | 1.814 |
| 200 / 300 | Stop | 91.781 / 195.859 / 195.859 | 82.291 / 126.548 / 126.548 | 1.977 | 0.218 | 1.814 |
| 400 / 300 | Prompt | 167.885 / 183.607 / 183.607 | 153.788 / 168.855 / 168.855 | 2.106 | 0.393 | 1.814 |
| 400 / 300 | Read | 134.177 / 145.820 / 145.820 | 125.001 / 136.710 / 136.710 | 2.113 | 0.403 | 1.814 |
| 400 / 300 | Stop | 134.821 / 150.137 / 150.137 | 125.626 / 140.780 / 140.780 | 2.115 | 0.405 | 1.814 |
| 200 / 1,500 | Prompt | 240.478 / 268.068 / 268.068 | 210.445 / 240.165 / 240.165 | 9.283 | 0.268 | 9.076 |
| 200 / 1,500 | Read | 228.118 / 259.409 / 259.409 | 205.744 / 238.494 / 238.494 | 9.291 | 0.276 | 9.076 |
| 200 / 1,500 | Stop | 230.024 / 281.513 / 281.513 | 208.311 / 257.597 / 257.597 | 9.292 | 0.279 | 9.076 |
| 200 / 3,000 | Prompt | 419.842 / 501.071 / 501.071 | 373.387 / 460.443 / 460.443 | 18.432 | 0.343 | 18.158 |
| 200 / 3,000 | Read | 420.872 / 473.386 / 473.386 | 382.102 / 436.854 / 436.854 | 18.439 | 0.351 | 18.158 |
| 200 / 3,000 | Stop | 399.886 / 604.927 / 604.927 | 359.891 / 530.552 / 530.552 | 18.440 | 0.353 | 18.158 |

Across 750 measured cold children, machine-config loads had median/max 1/1 in every event/shape. Assembly median/max was 7/7 for Prompt and 5/5 for Read and Stop in these runtime-context-enabled rooms; the fixed small gate fixture's 6/5/5 is a different environment. Snapshot rebuilds had median 0 and maximum 1 per child. Rollup parse bytes track one checkpoint file per cold child, not one file per assembly; the checkpoint grows slightly between frames. Raw carryover parse bytes were zero, but median trimmed-carryover reads ranged from 1.814 to 18.158 MB. Median materialized rollup rows were live + closed (350, 500, 700, 1,700, 3,200), with an observed maximum of 6,400; shared-base copies had median/max 1/1 throughout. This does not support a claim that every snapshot assembly re-parses the rollup.

Each drain row applies 100 Read frames from 25 working roots after warm-up. Seconds run from first append to the reader's last applied timing observation; frames/s is burst drain throughput, not an open-loop sustainable-arrival guarantee.

| Live / closed | Seconds | Frames/s | Claim to applied ms |
| --- | --- | --- | --- |
| 50 / 300 | 5.710 | 17.513 | 56.098 / 62.004 / 63.184 |
| 200 / 300 | 11.078 | 9.027 | 104.745 / 145.247 / 150.373 |
| 400 / 300 | 13.496 | 7.410 | 135.526 / 147.464 / 148.570 |
| 200 / 1,500 | 26.332 | 3.798 | 245.452 / 368.594 / 369.466 |
| 200 / 3,000 | 41.686 | 2.399 | 400.340 / 514.256 / 519.410 |

##### Host baseline

Each room has 300 closed rows, and each untraced phase lasts 120 s. Idle means no incoming frames, not an all-finished roster: setup marks 25 agents working. Every busy window sent 240 frames; the maximum send lag across rooms was 1.214 ms. CPU is milliseconds per wall-clock minute; serve is the sum of all sidebar supervisors. Do not add reaped-child CPU to rimz-child CPU, because they may overlap.

| Agents / sidebars | Phase | Host threads | Host CPU | Serve CPU | Tmux CPU | Host-reaped CPU | Rimz-child CPU |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 50 / 20 | idle | 52 | 6,210 | 180 | 875 | 2,780 | 0 |
| 50 / 20 | busy | 53 | 7,720 | 190 | 820 | 2,770 | 6,194 |
| 200 / 20 | idle | 52 | 10,230 | 185 | 1,645 | 2,505 | 0 |
| 200 / 20 | busy | 52 | 17,175 | 175 | 2,180 | 2,835 | 9,667 |
| 400 / 20 | idle | 52 | 17,045 | 155 | 2,155 | 2,675 | 0 |
| 400 / 20 | busy | 52 | 21,890 | 190 | 2,400 | 2,785 | 15,947 |
| 200 / 80 | idle | 172 | 18,350 | 580 | 3,375 | 6,300 | 0 |
| 200 / 80 | busy | 173 | 23,440 | 675 | 4,055 | 6,475 | 10,142 |

Wakeups, read syscalls and logical read MB below are per minute, each cell host / serve. Every departed-task count was zero in this run.

| Agents / sidebars | Phase | Wakeups, host / serve | Read calls, host / serve | rchar MB, host / serve |
| --- | --- | --- | --- | --- |
| 50 / 20 | idle | 61,177.5 / 5,994 | 322,489.8 / 77 | 187.706 / 0.064 |
| 50 / 20 | busy | 70,908 / 5,981.5 | 359,777.8 / 77 | 191.626 / 0.064 |
| 200 / 20 | idle | 87,127.9 / 5,977.5 | 487,890.7 / 79 | 268.265 / 0.142 |
| 200 / 20 | busy | 210,196.4 / 5,987 | 1,048,148.9 / 81 | 581.405 / 0.145 |
| 400 / 20 | idle | 148,078.9 / 6,000.5 | 793,745.1 / 74 | 432.814 / 0.239 |
| 400 / 20 | busy | 245,265.4 / 6,008.5 | 1,302,945.3 / 134 | 703.176 / 0.428 |
| 200 / 80 | idle | 145,813.9 / 23,919 | 721,818.6 / 110.5 | 319.872 / 0.257 |
| 200 / 80 | busy | 300,708.8 / 23,958 | 1,073,813.4 / 229 | 599.753 / 0.534 |

Fetch diagnostics retain their own covered duration; all other columns here are per minute.

| Agents / sidebars | Phase | Cycles | fold_ms | Content reductions | Unchanged reductions | Covered s |
| --- | --- | --- | --- | --- | --- | --- |
| 50 / 20 | idle | 50.4 | 9,564.9 | 0 | 100.7 | 121.5 |
| 50 / 20 | busy | 86.1 | 11,475.6 | 81.6 | 48.3 | 120.6 |
| 200 / 20 | idle | 52.6 | 8,469.1 | 58.5 | 96.3 | 120.9 |
| 200 / 20 | busy | 45.9 | 33,056 | 40.5 | 51.4 | 121.5 |
| 400 / 20 | idle | 49.5 | 15,226.5 | 26.7 | 89.1 | 121.2 |
| 400 / 20 | busy | 33 | 43,515.6 | 22.8 | 43.2 | 123.6 |
| 200 / 80 | idle | 50.5 | 9,215.7 | 0 | 101 | 60.6 |
| 200 / 80 | busy | 45.7 | 34,199.4 | 40.8 | 50.1 | 122.1 |

Spawn rates per minute below preserve every observed tracked bucket. Host git commands are `status` and `worktree`, in that order: status scales with due worktrees, while worktree enumeration is room-wide. Host tmux commands are `<options>` (options-only), `display-message`, `list-clients`, `list-panes`, in that order; serve is tmux `list-panes`. Other columns are git `rev-parse` and rimz `hooks apply`.

| Agents / sidebars | Phase | Host git: status / worktree | Host tmux: options / display / clients / panes | Serve panes | Other git rev-parse | Other hooks apply |
| --- | --- | --- | --- | --- | --- | --- |
| 50 / 20 | idle | 227.5 / 1 | 6 / 240 / 54 / 54 | 1 | 0 | 0 |
| 50 / 20 | busy | 227 / 1 | 6 / 240 / 46 / 46 | 1 | 124 | 120 |
| 200 / 20 | idle | 217 / 1 | 6 / 240 / 53 / 18 | 1 | 0 | 0 |
| 200 / 20 | busy | 236 / 1 | 6 / 239.5 / 56.5 / 56.5 | 1 | 121 | 120 |
| 400 / 20 | idle | 235.5 / 1 | 6 / 231 / 50.5 / 17 | 0.5 | 0 | 0 |
| 400 / 20 | busy | 225.5 / 1 | 6 / 240 / 34 / 34 | 1 | 121 | 120 |
| 200 / 80 | idle | 305.5 / 1 | 6 / 960 / 53 / 18.5 | 0.5 | 0 | 0 |
| 200 / 80 | busy | 293.5 / 1 | 6 / 960 / 54.5 / 54.5 | 1 | 122.5 | 120 |

Additional tracked buckets, also per minute: 50/20 busy had host rimz `gc` 0.5, other git `for-each-ref` 0.5 and `worktree` 1, and other tmux `list-panes` 0.5. The 200/80 busy window had other git `worktree` 0.5 and other tmux `list-panes` 0.5. No other tracked bucket was observed.

All four separately traced rooms passed without skips. Traced and untraced idle fetch rates sit together below; `/proc` opens/cycle divides opens/minute by the traced rate, with the interval caveat above. File opens exclude `/proc`. Host thread-file counts cover startup and transient threads, not just the concurrent set.

| Agents / sidebars | Idle cycles/min | Traced cycles/min | Host thread files | /proc opens/min | /proc opens/cycle | File opens/min |
| --- | --- | --- | --- | --- | --- | --- |
| 50 / 20 | 50.35 | 53.94 | 728 | 69,316 | 1,285.1 | 28,990 |
| 200 / 20 | 52.59 | 43.84 | 447 | 189,296 | 4,318.2 | 66,082 |
| 400 / 20 | 49.50 | 31.21 | 593 | 318,790 | 10,216 | 97,811 |
| 200 / 80 | 50.51 | 36.75 | 824 | 183,700 | 4,998.2 | 57,267 |

Each `/proc` kind cell is opens/minute / opens per traced fetch cycle. These include failed open attempts.

| Agents / sidebars | children | stat | io | status | task | other |
| --- | --- | --- | --- | --- | --- | --- |
| 50 / 20 | 26,744 / 495.8 | 21,986 / 407.6 | 884 / 16.4 | 6,307 / 116.9 | 1,934 / 35.9 | 11,461 / 212.5 |
| 200 / 20 | 85,684 / 1,954.6 | 62,139 / 1,417.5 | 3,229 / 73.7 | 8,791 / 200.5 | 5,539 / 126.4 | 23,914 / 545.5 |
| 400 / 20 | 154,437 / 4,949.1 | 98,841 / 3,167.5 | 6,432 / 206.1 | 12,427 / 398.2 | 9,792 / 313.8 | 36,861 / 1,181.3 |
| 200 / 80 | 79,745 / 2,169.7 | 56,773 / 1,544.7 | 2,918 / 79.4 | 12,071 / 328.4 | 5,684 / 154.7 | 26,507 / 721.2 |

The leading non-`/proc` path classes were the same in every room (attempts/minute). Full `trace` rows retain the remaining normalized paths; an open attempt is not proof of a successful JSON parse.

| Normalized path class | 50 / 20 | 200 / 20 | 400 / 20 | 200 / 80 |
| --- | --- | --- | --- | --- |
| `<runtime>/rimz/ws/<workspace>/lanes/budget.<id>.json` | 10,300 | 35,300 | 55,200 | 27,900 |
| `<runtime>/rimz/ws/<workspace>/live/agent_context/ctx.<id>.json` | 2,600 | 9,400 | 16,000 | 7,500 |
| `<runtime>/rimz/ws/<workspace>/lanes/auto-continue.<id>.json` | 2,600 | 9,400 | 16,000 | 7,500 |

Finally, the prestarted hosts' `work` rows cover their whole lifetime, including startup, warm-up, the first room's parity window and shutdown, not one phase or pass. Rebuild counts are calls to the snapshot publisher.

| Agents / sidebars | Lifetime s | Assemblies | Rebuilds | Rollup parsed MB | Latest parsed MB | Config loads |
| --- | --- | --- | --- | --- | --- | --- |
| 50 / 20 | 430.9 | 383 | 0 | 7.501 | 12.636 | 1 |
| 200 / 20 | 311.2 | 219 | 0 | 8.070 | 42.483 | 1 |
| 400 / 20 | 311.2 | 149 | 0 | 8.832 | 75.585 | 1 |
| 200 / 80 | 311.8 | 649 | 0 | 6.054 | 42.247 | 1 |

These host figures establish no regression ceiling: they include scheduling and ambient process-population effects, independent cadences and a deliberately incomplete provider workload. The fixed-fixture counter gates remain the deterministic ratchets.

###### Settled hidden attachment comparison

On 2026-10-29, the 200/20 and 200/80 yardstick ran at A (`test(sidebar): instrument concurrent attachment fanout`) then head (`perf(sidebar): keep settled hidden passes off the views lock`), both based on trunk `bc6bf69254e`. Each used its same-tree release testkit CLI and bench; CLI build IDs were `ac3a80608978` and `756e826a7311`. Host, fixture and 120 s idle/busy windows were as above, on the EPYC host described in the [concurrent capture](#concurrent-hidden-attachment-fanout). The commands were the release build above followed by `RIMZ_SCALE_ROOMS=200x20,200x80 RIMZ_XTASK_TIMEOUT=180m cargo xtask perf --bench hotpath -- host_scale --ignored`, serially under `rimz wait --run`.

CPU is ms/minute; wakeups are task context switches/minute, not delivered notifications. Attachment tasks are the `rimz-attach` loops and input readers, two per pane, included in the host totals.

| Agents / sidebars | Phase | Tree | Attachment tasks | CPU, host / attachment | Wakeups, host / attachment | Fetch cycles/min |
| --- | --- | --- | ---: | --- | --- | ---: |
| 200 / 20 | idle | A | 40 | 6,505 / 1,985 | 49,102.5 / 14,802.5 | 58.0 |
| 200 / 20 | idle | head | 40 | 7,070 / 1,835 | 38,509.0 / 4,184.5 | 58.3 |
| 200 / 20 | busy | A | 40 | 9,140 / 2,290 | 64,364.5 / 28,588.0 | 90.0 |
| 200 / 20 | busy | head | 40 | 8,935 / 2,080 | 43,448.0 / 6,905.5 | 89.9 |
| 200 / 80 | idle | A | 160 | 9,565 / 3,455 | 109,807.9 / 75,205.5 | 57.6 |
| 200 / 80 | idle | head | 160 | 8,750 / 2,635 | 56,567.5 / 17,139.0 | 57.3 |
| 200 / 80 | busy | A | 160 | 13,275 / 5,010 | 186,226.4 / 150,644.4 | 89.5 |
| 200 / 80 | busy | head | 160 | 11,350 / 3,050 | 63,058.5 / 25,474.5 | 85.8 |

No boundary task departed. Every busy window sent 240 frames; the largest send lag was 5.459 ms. Idle fetch diagnostics cover about 60 s of each 120 s window. Attachment CPU and context switches were lower in all four head windows, but whole-host idle CPU at 200/20 was higher. These single host runs do not isolate the cause of each difference; the concurrent fixture supplies the per-publication CPU and lock counts.

## Making a performance change

1. **Name the thread the cost lands on.** If it is the render/input loop, the change is wrong until the work moves off it.
2. **Prefer a push to a shorter poll.** A tighter poll burns cycles while the room is idle. A wakeup costs nothing until something changes.
3. **Decide durability explicitly.** Durable state goes through `write_temp_then_rename`, which fsyncs. A cache rebuilt on the next tick goes through `write_temp_then_rename_cache`.
4. **Keep single-flight.** A new fetch trigger is a room event through `RoomEvents::dispatch` ([`app/plane.rs`](../../crates/rimz/src/sidebar_pane/app/plane.rs)) or a pane observation through its `FetchHandle` ([`app/fetch.rs`](../../crates/rimz/src/sidebar_pane/app/fetch.rs)), never a bare send or spawn, so the worker merges it with the rest.
5. **Measure the idle case.** A change that speeds the busy path by adding idle work is usually a net loss.
6. **Prove it.** `cargo xtask ci` stays green, the tick budgets and the cost map move with the change, and `cargo xtask perf` refreshes the figures when the change touches a measured path. A before/after comparison rests on the bench's allocation counts. Ordinarily quote a wall-time delta only from runs on an idle host, since medians move with load ([Benchmarks](#benchmarks)). A controlled loaded experiment may instead report paired, interleaved runs with the same fixture and load command, observed placement, sustained-load evidence, raw distributions and misses, as in [the drainer experiment](#per-frame-drainer-latency-under-build-load); conflicting pairs do not establish a speedup. Such an experiment records, for each run, the CPU the host spent outside the run's own scopes over the timed window, and states its speedup rule before the first run. A candidate in a stack of commits is measured against its own parent commit, since a merge-base comparison credits it with the earlier commits' gains. When comparing two builds' wall times, warm both, then run them interleaved (base, branch, base, branch) and compare paired ratios, because a sequential pair measures host load. A change to a bench fixture compares against base rerun with the same fixture, since a comparison across fixtures is not evidence. A bench that times publication to paint publishes at the producer's production cadence (the fetch worker rests 250 ms after each cycle), never on the consumer's acknowledgement, or it measures the frame grid.
7. **Measure the file you fixed.** For an I/O fix, `strace -f --seccomp-bpf -e trace=openat` filtered on the target path across several ticks shows whether that file is still re-read; `--seccomp-bpf` stops the target only at the traced call, where plain `strace` stops it at every syscall and slows a busy host. Even so, a traced host can cycle slower than an untraced one (the [host-scale traced room](#scale-yardstick) at 400 agents ran 31.2 fetch cycles a minute against 49.5 untraced), so compare its cycle rate before reading an open count as a rate. `/proc/<pid>/io` `rchar` is the process total, mixing every other read, so report it beside the per-path count, never alone. With `kernel.yama.ptrace_scope=1`, an unprivileged tracer normally must start its target or trace a descendant, rather than attach to an unrelated room host. `perf_event_paranoid >= 2` disallows kernel profiling; stricter settings can also block userspace events (this measurement host's value 4 denied `perf stat -e task-clock -- true`). The [host-scale traced room](#scale-yardstick) starts its host under strace instead of attaching to an existing one.

### Perceived response time

Responsiveness is what the user feels: a frame that paints now with slightly stale data beats a fresh frame that arrives late. Reach for these levers in order.

1. **Acknowledge before you finish, unless one source of truth is worth the wait.** Where the view's intent is the truth, paint the instant input lands: a browse pick and the help overlay paint synchronously in `LoopState::apply_input`. The jump is the deliberate exception. A key jump changes no browse pick. Its detached thread writes a focus intent that the owning view presents through the fold ([state.md → Focus intent](./sidebar/state.md#focus-intent)), so a stale frame cannot roll the highlight back.
2. **Animate on the wall clock, never on IO.** The spinner advances from the loop's frame clock (`frame_clock`) on a fixed interval whether or not a fetch is in flight. Motion that stalls while data is slow reads as a hang.
3. **Separate smoothness from freshness.** When a "make it feel snappier" request lands, first decide whether it is a frame-rate problem or a data-latency problem. They are fixed in different layers, and conflating them turns a cosmetic tweak into a CPU regression.
4. **Keep session-wide effects off the frame path.** A redraw writes only to the sidebar's own pane and is safe at any rate. A mux action touches the whole session and can reset an unrelated pane's cursor blink, so it belongs on the event-driven data layer.

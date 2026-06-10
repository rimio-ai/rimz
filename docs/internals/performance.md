# Performance

> This is the performance model over the mechanisms, not the mechanisms themselves. Sidebar data flow, cadences, and the published-file inventory live in [state.md](./state.md); presence, ranking, and recovery in [sidebar.md](./sidebar.md); the durability contract and write classes in [ledger.md](./ledger.md#durable-state); the `list-panes` round-trip and the Zellij presence channel in [multiplexers.md](./multiplexers.md). This doc says where the milliseconds go, what the whole fleet costs Rimz, which optimizations bound them, and the rules the next change follows.

## The performance model

Read this first; everything else is detail under it.

Rimz watches a fleet for one human. The workload has a characteristic shape: tens of agents append to one flat-file ledger concurrently, a sidebar renders the room and must stay interactive while they do, and **the room is idle most of the time** — a fleet emits in bursts and then waits on a model or a human. Good performance here is three goals at once, in three different places:

- **The read/render path — perceived latency.** The sidebar's render/input loop is the one thread a human feels. Its goal is that the UI never blocks, drops a keystroke, or freezes the spinner. It may show data that is stale by a tick; it may never stall. This is where "responsiveness" lives.
- **The write path — contention.** Tens of agent hooks commit to one workspace at once. Its goal is that a concurrent append costs no felt latency — the critical section is a queue of microsecond holds, and durability rides off the lock.
- **Idle steady state — near-zero work.** When nothing changes, the room should cost almost nothing: no polling spin, no per-frame fork, no directory rescan. An optimization that speeds the busy path by adding idle work is usually a net loss, because idle is the common case.

Underneath all three sits one guardrail: **correctness lives in the ledger, never on the render thread.** A performance change may leave the UI stale by a tick; it may never make it wrong, and it never trades a durability or CAS invariant for latency.

The whole design keeps these from fighting each other by **decoupling them onto independent clocks and threads**. The core abstractions, each named once with its code home:

- **The serve loop and the fetch worker.** `rimz sidebar serve` runs one event loop that blocks only in `recv` on its wakeup socket ([`sidebar_pane::app::serve`](../../crates/rimz/src/sidebar_pane/app.rs)). All expensive work is offloaded to a background **fetch worker** thread ([`app::fetch::spawn_fetch_worker`](../../crates/rimz/src/sidebar_pane/app/fetch.rs)). The loop never forks or fsyncs.
- **One producer, many consumers.** The eldest live renderer per workspace is elected **producer** and pays the external reads (`list-panes`, git, spend, accounts) once for the room; every younger renderer is a **consumer** that folds the producer's published caches in process ([`sidebar::consumer::read_published_snapshot`](../../crates/rimz/src/sidebar/consumer.rs)). Election is by UUIDv7 birth order ([`sidebar::elder_sidebar_present`](../../crates/rimz/src/sidebar/mod.rs)); correctness rides a flock-based single-flight underneath it ([`ledger::single_flight::coalesce`](../../crates/rimz/src/ledger/single_flight.rs)).
- **Event-fresh truth over a coalesced frame.** Durable truth (the ledger rollup) is read event-fresh on every fold; the expensive pane topology is coalesced behind a TTL cache and folded under it. They have separate clocks, so a status flip repaints now while `list-panes` stays cheap.
- **Push over poll.** A change a writer knows about posts a typed wakeup; the loop folds it within one wakeup. The poll is the missed-wakeup backstop, never the primary channel.
- **A zero-fsync critical section.** The workspace flock covers truth mutation only; durability is a group fdatasync on an off-lock write tail. The contract lives in [ledger.md → Durable state](./ledger.md#durable-state); this doc bounds its hot-path cost.

## Where the milliseconds go

Four threads carry all the cost, and only one of them faces the human.

- **The render/input loop** does sub-millisecond, in-process work: advance the spinner from the cached snapshot, apply an input in place, append an overlay event and re-fuse, or fold a finished fetch. The fuse is pure — no IO, no subprocess, no clock read — and perf-guarded at fleet scale. A keystroke or a fused pane event paints off-grid on the spot; everything else coalesces to one paint per `[sidebar] refresh_ms` boundary.
- **The fetch worker** runs two speeds per cycle ([`app::fetch::run_fetch_cycle`](../../crates/rimz/src/sidebar_pane/app/fetch.rs)). It first folds the event-fresh ledger rollup over the published pane frame **entirely in process** — no `list-panes`, no git — and posts it, so a status or cost change paints in single-digit milliseconds. Only on the producer, and only when a cache is due, does it pay the reconciling **produce** as the cycle's second post.
- **The producer's produce** ([`sidebar::produce::produce_snapshot`](../../crates/rimz/src/sidebar/produce/mod.rs)) is where the subprocesses live: the TTL-gated `list-panes`, the git probes, the account probe. It runs in process on the worker's warm rollup cursor and publishes the shared caches the whole room reads.
- **The ledger critical section** is the write path's hot spot: a feed rename plus one event-log `write()`, zero fsyncs, microseconds under the flock. Everything heavier — the group fdatasync, the snapshot publish, the wakeup fanout — runs after the lock releases.

The three end-to-end budgets the cost map rolls up into, each with its dominant term named so a regression is visible at review:

- **Keypress → pixel: one in-process paint.** Input applies synchronously and marks the frame dirty. Nothing on this path takes a lock, forks, or reads the ledger.
- **Pane event → pixel: one fuse plus one off-grid paint.** A typed overlay event (`PaneClosed`, `FocusChanged`, …) appends to the in-memory store, re-fuses the held frame, and paints immediately; the producer's verifying pull squares truth behind it ([state.md → Fusion Rules](./state.md#fusion-rules)).
- **Write → pixel: bounded by the base frame bucket** (`[sidebar] refresh_ms`, 100ms default). Commit under the flock (µs) → wakeup datagram (µs) → consumer folds the published frame in process (O(1) cached, O(delta) on a race) → next dirty frame paints. The frame bucket dominates; everything upstream of it is orders of magnitude smaller.

## The cost map

Where the milliseconds are, and what bounds each. Figures are orders of magnitude, not promises; exact cadence constants live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs) and the staleness each lane may show is budgeted in [state.md → Pull-Tick Table](./state.md#pull-tick-table).

| Operation | Rough cost | Bound |
| --- | --- | --- |
| `list-panes` (Zellij/tmux IPC) | 200–680ms, occasionally degraded mid-tick | one producer per workspace; snapshot cache, single-flight, **two-mode TTL** — poll cadence, stretched ~13× while the Zellij presence stamp is fresh ([multiplexers.md → Zellij presence channel](./multiplexers.md#zellij-presence-channel)); per-pane process rotation repairs raced-null fields; render-side last-known-good gate |
| git diff-stats per group root | ~7 `git` forks per group root, plus a byte-budgeted untracked read; each root's chain is sequential, roots run bounded-parallel (`MAX_PARALLEL_GIT`) | activity-tiered TTLs (hot vs idle) keyed on root, single-flighted (`diff-stats.lock`); a non-repo room's root pod costs zero forks. Input set scales with the room — a 50-child-repo room pays a cold idle-TTL window on the fetch worker, never the render thread |
| group-root enumeration | 1 `git worktree list` fork per `WORKTREE_ROOTS_TTL` in a repo room; 1 `read_dir` in a directory room | cached in the diff-stats cache; a session-boundary `--min-pane-cache-ms` floor re-enumerates a new checkout immediately |
| snapshot rollup | O(1) from `snapshots/latest.json` lock-free; O(delta bytes) when writes outran the cache | the `(generation, offset)` freshness stamp; a miss folds only the unfolded log tail; rotation caps the active log ([ledger.md](./ledger.md#durable-state)) |
| event-log reader fold | warm cursor: one stat + the appended frames (O(new bytes)); cold: one checkpoint parse + a bounded tail fold | the extent stamp and a long-lived `RollupCursor` per fetch worker; perf guard `delta_fold_is_o_new_bytes` |
| ledger write critical section | feed rename + one event-log `write()`, zero fsyncs, ~µs | the flock covers truth mutation only; durability and publish run off-lock ([ledger.md](./ledger.md#durable-state)); perf tier `ledger_fsync.rs` pins zero fsyncs on the warm path |
| dead-owner abandon sweep | O(pending) scan + per-dead-item writes | debounced to ~once per 2s (one stamp stat); read-side expel hides a dead-owner item instantly; `rimz gc` is the operator trigger |
| pending enumeration | O(pending feed files) | terminal items relocate to `feed/terminal/` on transition; decision-path scans list only the pending dir |
| sidebar heartbeat | temp + atomic rename | written at startup, then throttled below the liveness TTL, so a delta storm does not churn heartbeats |
| sidebar wakeup fanout | heartbeat-dir scan + datagram sends after ledger writes | one reused datagram sender; N is bounded by live sidebars (one per tab), page-cache-hot, below the write's fsync floor |
| jump (focus the bound pane) | one mux-client fork, tens–hundreds ms | off the render thread (detached); in-process focus, no `rimz pane focus` child, no per-click `list-panes` re-validation; fire-and-forget |
| durable file write | temp + 2 fsyncs (file, parent dir) | cold paths only — trust grants, workspace identity, hook installs, rotation carryover; nothing a hook or the UI waits on pays it |
| disposable cache write | temp + atomic rename, 0 fsync | `write_temp_then_rename_cache` |
| frame redraw | sub-millisecond, in-process | fixed `[sidebar] refresh_ms` grid for dirty folds and fast motion; slow `?`/`!` breath on a 300ms floor; idle relaxes to the backstop; never forks a fetch; perf guard `compose_budget` |
| truecolor effects pass | µs-scale, O(affected cells) | a color-only post-pass inside the same draw, gated by `Theme::effects_enabled`; targets resolve from the hit-test `line_map`, so cost scales with attention rows, never screen size |
| fleet spending walk (producer) | within `SPENDING_TTL`: one read of shared `provider-spending.json`, zero transcript IO; on the due walk, one stat per file + O(appended bytes) per grown file | the shared cache stamp gates the whole walk, single-flighted across rooms (`spending.lock`); the incremental `(mtime, len, cursor)` parse keeps it history-independent ([transcript.md](./transcript.md), guard `spending_walk_io_is_history_independent`) |
| Codex local transcript context refresh | steady-state: one stat on the prior rollout path; changed file: one bounded 64 KiB tail parse, no app-server subprocess | three stat-gated triggers — hook, the elder's transcript watcher, and the producer backstop ([state.md → Push Channels](./state.md#push-channels)); app-server fields stay detached and throttled |
| `/proc` pane metrics (producer) | active panes sample ~1s, idle panes ~5s; within a pane's window the stored values and pane→root-pid binding carry forward, zero `/proc` IO | per-pane stamps in `metrics-sample.json`; the full process-table walk runs only on pane churn or a foreground change — exactly when `list-panes` already refreshed |
| sidebar observer (every renderer) | one O(rows) signature pass per committed fold, µs-scale; the elder's cross-check pass adds one stat-gated cache read plus at most one `/proc` stat per row per `OBSERVE_CROSSCHECK_TTL` | inline pure detection at the fold chokepoint, zero snapshot clones; anomaly drafts cross a bounded channel to one writer thread; per-kind cooldown, the sink rate limit, and size-capped rotation bound the shared `diag.log.jsonl` ([observe.md](./observe.md), [diagnostics.md](./diagnostics.md)) |

## The overhead, at fleet scale

Rimz is the layer that watches a fleet for one human, so its own footprint is sized against a single agent rather than the fleet: the cost of observing twenty or a hundred agents stays a small, near-flat fraction of running one of them, and that ratio is the performance target as much as the measurement. The figures below are measured on a real fleet and projected to 20, 50, and 100 concurrent agents spread across a handful of rooms — two to five workspaces, each a repo or a directory of worktrees, which is how a developer's agents actually divide. Like the cost map they are orders of magnitude, and the constants that bound them live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs).

The cost attaches to three units, and only the cheapest grows with the agent count:

- **Per workspace, once.** Each room elects its eldest renderer as the sole producer, which pays every external read for that room — `list-panes`, the git probes, `/proc`, the spend walk, the account probe — then publishes caches the other tabs fold in process. One round-trip per tick covers a room whether it holds two agents or a hundred, so the expensive work is bounded by the workspace count, not by the agent count: a few rooms means a few producers, each flat in the agents under it.
- **Per worktree, activity-tiered.** The git diff-stats input set scales with distinct group roots, not agents; a root drops to the 60s idle TTL the moment its agents go quiet, and the whole sweep is capped at `MAX_PARALLEL_GIT` (8) fork chains. A hundred agents sharing a few checkouts pay a few hot roots; a hundred-worktree room pays the cap and the idle tier.
- **Per agent, cheap and event-driven.** An agent reports through a short-lived `rimz hooks feed` child that appends to the ledger only when something happens — a turn boundary, a question, a resolution — and exits. Nothing resident wraps a running agent. The default path writes a question's feed item and hands the ask back to the agent's own UI, so even a waiting agent holds no Rimz process; only an enrolled resolver's bridge parks one ~6 MiB child while it answers, for at most the hook cap.

Two costs stay flat in the agent count by design: durability is one group `fdatasync` per second per workspace however many agents append into it, and the snapshot publish is one debounced cache rename per second per workspace — both scale with rooms, not agents. The hot runtime caches — the published snapshot, heartbeats, diff-stats, `/proc` samples — land in `$XDG_RUNTIME_DIR` (tmpfs), so their churn is memory traffic, never disk IO.

The table totals across a 2–5 room fleet and names the per-workspace rate where it matters:

| Resource | 20 agents | 50 agents | 100 agents | What sets it |
| --- | --- | --- | --- | --- |
| CPU, idle | ~0 | ~0 | ~0 | loops block in `recv`; no poll spin, no per-frame fork |
| CPU, busy | <0.3 core | ~0.3–0.8 core | ~0.5–1.5 core | one producer per room runs git / `/proc` / spend on its fetch worker, bursting toward the per-room 8-fork cap, never on the render thread |
| RAM, resident | ~80–150 MiB | ~100–180 MiB | ~120–220 MiB | one ~30 MiB renderer per open room + per-room producer caches + a thin per-Codex-session broker; scales with rooms, flat in agents |
| Durable write | ~1–3 KiB/s | ~2–4 KiB/s | ~2–5 KiB/s | event frames (~0.5 KiB) per turn; a ~7.6 KiB feed item only on a question; summed across rooms |
| fsync rate | ~rooms/s | ~rooms/s | ~rooms/s | one group `fdatasync` per second per workspace (≈2–5/s for the fleet) |
| State on disk | tens of MiB | tens of MiB | ~100s of MiB | rotation-capped event log + ~5 KiB/agent snapshot + relocating feed items, per workspace |
| Network | 1 pricing fetch/day | 1/day | 1/day | fleet-shared, single-flighted; local datagrams and unix sockets otherwise; no core egress |

Set against the agents it tracks, the overhead reads as a rounding error, and the gap widens as the fleet grows. One developer's week of Claude and Codex sessions came to 1.23 GiB of transcript JSONL — ~177 MiB a day — with runtime processes resident at 250–340 MiB (Claude) and 50–65 MiB (Codex) each; Rimz watched the same fleet for tens of MiB of durable state total, a resident set on the order of a single one of those agent processes, a fsync a second per room, and one pricing refresh a day over the network. The agents' transcript and process cost climbs with the agent count; Rimz's climbs only with the room and worktree count, so a denser fleet widens the ratio rather than narrowing it. The scaling terms the model flags first — git enrichment across many worktrees and transcript-discovery stat volume as fleet history grows — are producer-side and activity-gated, so they surface in fetch-worker latency long before they reach the human's frame.

## Principles

The rules every performance change here follows, ordered — an earlier rule outranks a later one when they conflict.

1. **The render thread never blocks.** No subprocess fork, no fsync, no synchronous `list-panes` on the loop. Offload to the worker and wake the loop when it finishes; a single in-flight fetch is the unit of work, and the loop stays interactive while it runs.
2. **Decouple the frame from the fetch.** Responsiveness — the spinner advancing, the cursor holding, a click highlighting — is a redraw from the *cached* snapshot, not a data refresh. The render layer is a fixed-timestep loop on its own clock; the data layer arrives on a slower, event-driven cadence and folds in when ready. The two never share a clock: a smoothness change tightens the frame interval and costs only in-process paints, while a freshness change rides the data layer (principle 3). Never invert it — letting a frame drag a `list-panes`+git fetch behind it spends a fork to move a spinner.
3. **Push over poll.** A change a writer knows about posts a wakeup; the loop folds it within one wakeup. Polling is the missed-wakeup backstop, never the primary channel. A datasource that feeds the UI but is not the ledger (a statusline or local transcript sidecar) gets a wakeup of its own rather than waiting for the next tick.
4. **Cache the disposable; fsync the durable.** Crash-durability is for truth alone — the event log and the cold-path records (the write-class contract in [ledger.md](./ledger.md#durable-state)). Everything derived — the rollup caches, the snapshot cache, diff-stats, the context sidecars — renames atomically *without* fsync (`write_temp_then_rename_cache`), because it rebuilds from truth on the next read. A torn read is impossible either way; only "survives a power cut" is traded, and for a rebuildable file that buys nothing while costing two fsyncs on a path the UI waits on.
5. **Single-flight, then coalesce.** One outstanding fetch at a time; a burst of deltas collapses to one fetch, and a delta racing an in-flight fetch defers exactly one follow-up, never a queue. The same lock+poll election ([`ledger::single_flight`](../../crates/rimz/src/ledger/single_flight.rs)) sits on every shared external read, so concurrent sidebars and rooms share one producer instead of stampeding.
6. **Pay the round-trip once per window.** `list-panes` and the git probes are the snapshot's cost; bound them with a short TTL cache and reuse the last good result. A degraded read backfills missing fields per pane from the last good read rather than flashing a corrupt frame, and the renderer holds the last good frame rather than commit a regression ([sidebar.md → Presence model](./sidebar.md#presence-model)).
7. **Cheapest correct read.** The snapshot catch-up is O(delta bytes) from the persisted fold base on the common path, O(active-events + items) at worst, never O(history); archives are touched only at rotation. Skip work that cannot matter — an idle room with no agents skips the sidecar directory scans entirely.
8. **One producer per workspace, one renderer per tab.** The external-read cost is per *production*, so production stays at one while every tab keeps its own renderer. The eldest live instance is elected producer and publishes the shared caches; every younger renderer reads them in process and never exits, so a per-tab pane never goes dark. This caps the mux server to one round-trip per workspace per tick — production, not renderer count, is what is bounded — and staleness recovery belongs to the election, never to the consumers (see [The election, assessed](#the-election-assessed)).

## What's optimized

The mechanisms in place, by the structure each creates — each described once, with its code home. The chronology lives in git; the lessons that prevent re-introduction are below in [Anti-patterns we removed](#anti-patterns-we-removed).

### Decouple the frame from the fetch

The snapshot fetch runs on the background worker; the loop blocks only in `recv` and folds results via a `snapshot` wakeup ([`app::serve`](../../crates/rimz/src/sidebar_pane/app.rs), `spawn_fetch_worker`, `request_fetch`, `apply_fetch_outcome`). The animation tick redraws the spinner from the *cached* snapshot and never fetches, so a missed push degrades only to the backstop tick, not a per-frame poll storm. Animation cadence is classified ([`render::animation_cadence`](../../crates/rimz/src/sidebar_pane/render/mod.rs), `app::frame_interval`): fast work stays on the base grid, slow attention motion redraws at a 300ms floor, and a dirty data fold clamps back to the base budget so freshness never waits on the cosmetic cadence. Smaller cuts on the same hot path: the `NO_COLOR` lookup caches in a `OnceLock`, and pane `command`/`cwd` move out of the owned `RawPane` instead of cloning per pane per tick.

### One producer per workspace

The eldest renderer is elected producer ([`sidebar::elder_sidebar_present`](../../crates/rimz/src/sidebar/mod.rs)) and publishes the shared caches; younger renderers read the published frame in process ([`sidebar::consumer::read_published_snapshot`](../../crates/rimz/src/sidebar/consumer.rs)) — never their own `list-panes`/git, never exiting. The producer itself runs the produce **in process** on its fetch worker ([`sidebar::produce::produce_snapshot`](../../crates/rimz/src/sidebar/produce/mod.rs)) rather than forking `rimz sidebar snapshot` per tick; the CLI `snapshot` arm stays as a thin delegate over the same pipeline for inspection, scripting, and the plugin rail. The producer and consumer share one ordered enrichment fold ([`sidebar::enrich::enrich`](../../crates/rimz/src/sidebar/enrich.rs), `EnrichMode::{Cached, Producing}`): `Cached` reads only published caches and sidecars, `Producing` inserts the daemon reap, account probe, and git refresh at named points, and goldens prove the two modes byte-identical. A `cargo xtask invariants` grep bans ledger-writer, feed-store, bridge, and broker imports under `crates/rimz/src/sidebar/`, so the in-process producer can never grow write-side machinery unnoticed.

### Event-fresh truth over a coalesced frame

The published `snapshot.json` carries only the typed pane topology; the rollup is read event-fresh from `latest.json` on every fold and folded over the coalesced panes ([`consumer::{read_published_snapshot, rollup_snapshot}`](../../crates/rimz/src/sidebar/consumer.rs)). A status change or a new agent in an existing pane repaints within one wakeup, while `list-panes` stays coalesced for genuine open/close. Every change a writer knows about pushes a wakeup so it skips the poll window: ledger and sidecar writers post a `LedgerDelta`; the Zellij presence plugin and the tmux control-mode watcher push pane events; the elder's transcript watcher covers Codex's mid-turn gap ([state.md → Push Channels](./state.md#push-channels)). On Zellij the presence channel stretches the producer's pane TTL while its stamp is fresh, dropping the steady-state `list-panes` fork rate ~13× ([multiplexers.md → Zellij presence channel](./multiplexers.md#zellij-presence-channel)). Agent birth/death that must beat the cache carries a producer-only pane-freshness floor (`min_pane_cache_ms`) so the producer pulls fresh, publishes, and broadcasts `PaneFramePublished` for consumers to refold.

### Incremental everything

No reader pays O(history). The rollup persists a raw fold base plus its `(generation, offset)` stamp; catch-up seeks to the offset and folds only new frames ([`snapshot::{RollupCache, catch_up_rollup}`](../../crates/rimz/src/ledger/snapshot/fold.rs)). A long-lived `RollupCursor` per fetch worker holds the parsed base in memory, so a warm fold is one stat plus the appended frames; a `(path, mtime, len)` parse cache on `snapshot.json`, `latest.json`, and `rollup.json` returns a clone instead of re-parsing 100–500 KB of JSON on an unchanged file ([`ledger::parse_cache`](../../crates/rimz/src/ledger/parse_cache.rs)). The fleet spend walk is incremental the same way: the cache stores `(mtime, len, cursor)` per file, a grown file parses only its appended suffix, and only a truncated or rewritten file re-parses cold. Allocation cuts ride along: `reap_stale_sessions` marks superseded sessions with a parallel `Vec<bool>` instead of a `BTreeSet` of cloned tuples, and `reduce_agent_states` deserializes lifecycle fields straight from the borrowed `&Value`.

### Per-enrichment cadences

Every display figure is display-only by invariant ([DESIGN.md → Attention at a glance](../../DESIGN.md#attention-at-a-glance)), so each enrichment gets its own natural cadence behind a process-safe stamp in the cache file it already writes — the `AccountsCache::is_fresh` pattern. The fleet spend walk gates the whole walk on `SPENDING_TTL` ([`produce::spending`](../../crates/rimz/src/sidebar/produce/spending.rs)); `/proc` sampling rides per-pane hot/idle stamps decoupled from the pane-read clock ([`produce::metrics`](../../crates/rimz/src/sidebar/produce/metrics.rs)); the git probes use activity-tiered TTLs whose hotness comes from ledger agent activity, not filesystem watching ([`enrich::hot_worktree_paths`](../../crates/rimz/src/sidebar/enrich.rs), `produce::git`); and `git worktree list` rides its own `WORKTREE_ROOTS_TTL`. Stamps live in the cache files, never process memory, so the in-process producer, the CLI inspection produce, and every consumer agree on freshness across processes. Guards `idle_room_produce_runs_no_enrichment_io` and `spending_walk_skips_entirely_within_ttl` pin the idle case.

### The zero-fsync write path

The critical section covers durable truth only — feed write plus one event-log `write()` — and the flock hold drops to microseconds; the off-lock write tail issues a group fdatasync debounced to ≤1/s, so one writer per interval makes the whole fleet's appends durable. Length-plus-CRC32 framing makes a lost suffix deterministic corruption that the next write tail self-heals. The snapshot publish runs after the lock releases, single-flighted and debounced to ≤1/s or 64 KiB of unpublished tail, and rollup reads are lock-free. The full contract — write classes, recovery, the CI grep that funnels every fsync through `ledger/atomic.rs`, and the perf tier that pins exact syscall counts — lives in [ledger.md → Durable state](./ledger.md#durable-state). The mux seam took the same fork-shedding treatment: an event-driven wait replaces the per-command poll tax ([`mux::command::CommandSpec::run_bounded`](../../crates/rimz/src/mux/command.rs)), tmux room birth batches its option sets into one client invocation, and version probes memoize.

### Warm context, no cold spawns

Codex enrichment skips the cold-spawn handshake: a per-session broker holds one warm, already-handshaked `codex app-server` and serves it over a unix socket ([transcript.md → Appendix Codex](./transcript.md#appendix--codex)). It runs as a pane in the `rimzd` daemon tab, respawns a dead child once, and always leaves a cold-spawn fallback, so enrichment never depends on it. The elder's transcript watcher closes Codex's mid-turn freshness gap — it holds a filesystem watch on each live root Codex rollout JSONL and runs the existing stat-gated refresh on the write, debounced to one flush per 300ms per session, posting the same `LedgerDelta` the hook path does ([state.md → Push Channels](./state.md#push-channels)). Both are latency hints over the unconditional producer tick: a watcher that never starts costs nothing.

### Anti-patterns we removed

The mistakes worth not re-introducing, because each looked reasonable:

- **`ACTIVE_REFRESH`** refetched every 500ms purely to keep a spinning agent's `$`/tokens current — a subprocess fork per frame to move a spinner, and on Zellij its periodic `list-panes` even reset unrelated panes' cursor blink. The cost it bought is now covered by context-sidecar pushes (principle 3).
- **The consumer self-heal** let every consumer produce on producer staleness; the single-flight loser wait (~200ms) sat under the `list-panes` floor (200–680ms), so every loser timed out into its own uncached produce — an N-way fork storm on exactly the tick the room was already degraded. Recovery now belongs to the election alone.
- **Per-tab `list-panes`** pinned the mux server with N× round-trips; the first fix over-corrected to a per-workspace renderer-exit election, which blacked out every tab but one. The standing answer is one producer, N renderers — production capped, renderers not.
- **A consumerless `zellij pipe`** was spawned per ledger write per Zellij session to feed a rail that does not yet exist; it is dropped, with `MuxBackend::wake_sidebar` kept as the dormant primitive the rail will re-arm.

## Bottlenecks and deferred work

Real wins identified but not taken, because each changes a contract or crosses a backend-parity boundary and deserves its own change with tests. Ranked by expected payoff.

1. **Dir-mtime-gated transcript discovery.** The fleet spend walk readdirs every provider's session tree per due walk to discover files. With the whole walk now gated to once per `SPENDING_TTL`, the warm readdir is producer-side noise — caching the directory set behind dir mtimes buys little for the bookkeeping it adds. Revisit only if fleet history outgrows the readdir itself.
2. **tmux `list-panes` over the held control client.** The elder already holds one `tmux -C` control client for presence; it could issue `list-panes` over that client's stdin and parse the wrapped reply, eliminating the per-window fork+connect. The saving is ~10–30ms on tmux — already the cheap backend, since the 200–680ms floor is Zellij's — and the poll must still back a dead watcher. Revisit if tmux pane reads surface in producer-tick latency.
3. **Delta-bearing wakeup datagrams.** The `LedgerDelta` event could carry the appended frames themselves, so a warm consumer folds straight from the datagram with zero file IO. With the warm cursor fold already one stat plus a page-cache-hot read, the residual win is microseconds; the cost is a second delivery path for state that must never *become* truth (datagrams are lossy and unordered). Build only when sustained fleet rates reach hundreds of events per second.
4. **Pricing-cache pruning.** Pruning `pricing-cache.json` to models the fleet has actually used would shrink the stale-arm parse, but with the spend walk now shared and gated it is below the noise floor.

Evaluated and **rejected** — recorded so the next pass does not re-litigate them:

- **Lock-free `O_APPEND` event appends.** The recovery model assumes the flock makes the log single-writer-at-a-time, so only the trailing frame can tear; concurrent appenders' independent dirty pages can write back out of order, leaving a zeroed *middle* frame, which rebuild correctly treats as a hard error. Making it safe needs per-frame magic for resync — the CRC validates a frame but cannot relocate the next boundary past a torn middle — all to shave a lock tail that is now a queue of microsecond holds. See [ledger.md](./ledger.md#durable-state).
- **Binary snapshot format.** The parse cache already removes the re-parse on delta storms, the `RollupCursor` holds the parsed base in memory, and JSON keeps `rimz sidebar snapshot --json` inspectable. A binary checkpoint would accelerate a parse that no longer happens.
- **Caching the wakeup heartbeat scan.** N is live sidebars, the reads are page-cache-hot, and the fanout already runs after the lock releases, below the write's fsync floor. A cache would have to live cross-process and re-validate exactly what the TTL + re-stat already validate.

## Maintaining performance

### Adding a performance change

1. Name the thread the cost lands on. If it is the render/input loop, the change is wrong until the work moves off it.
2. Prefer a push (a wakeup the writer already posts) over shortening a poll interval. A tighter poll burns cycles in the idle case; a wakeup costs nothing until something changes.
3. Decide durability explicitly: durable state fsyncs, a next-tick-rebuilt cache does not. Do not reach for `write_temp_then_rename` on a disposable file.
4. Keep single-flight: a new fetch trigger routes through `request_fetch`, never a bare spawn, so it coalesces with the rest.
5. Measure the idle case too — an optimization that speeds the busy path by adding idle work is usually a loss; the fleet is idle most of the time.
6. The gate is the proof. `cargo xtask ci` stays green, including the invariant that the sidebar render path imports no durable ledger-writer module and forks no `pane capture`/`send`.

### Optimizing perceived response time

Responsiveness is what the user feels, not what the profiler measures: a frame that paints now with slightly-stale data beats a fresh frame that arrives late. The UI/UX levers, in the order to reach for them.

1. **Acknowledge before you finish — except where one source of truth is worth the wait.** Where the local intent *is* the truth, redraw it the instant the input lands: a browse pick and the help overlay paint synchronously in `app::apply_input`. The jump is the deliberate counter-example — it fires the focus command on a detached thread (`app::spawn_pane_focus`), changes no local state, and lets the **derived** selection catch up on the next fold, so a stale frame can never roll the highlight back. Reach for the optimistic redraw before making an action faster; reach for the derived-only model when the echo would need its own protocol to stay honest.
2. **Animate on wall-clock, never on I/O.** The spinner and cursor advance from `wall_clock_phase` on a fixed interval, independent of whether a fetch is in flight. Motion that stalls when data is slow reads as hung even when nothing is wrong; a steady tick from cache reads as alive. Drive any future animation off the same monotonic base so a refetch or delta can never reset its phase.
3. **Tune smoothness and freshness on separate dials.** A smoother UI shortens the frame interval — cheap, in-process paints (principle 2). Fresher data adds a push, not a tighter poll (principle 3). Conflating the two is how a cosmetic tweak becomes a CPU regression — the `ACTIVE_REFRESH` mistake. If a "make it feel snappier" request lands, ask first whether it is a frame-rate problem or a data-latency problem; they are fixed in different layers.
4. **Keep session-wide effects off the frame path.** A redraw writes only to the sidebar's own pane and is safe at any rate. A mux *action* — `list-panes`, a `pipe` broadcast — touches the whole session and can reset an unrelated pane's cursor blink, so it belongs on the slow, event-driven data layer, never on the animation tick.

### The election, assessed

Is the eldest-UUIDv7 + flock + staleness design robust enough? **Yes — the flock is the real election; the eldest rule is a "don't even try" optimization; the TTL carries liveness.**

- **The flock is the actual election.** Correctness never depends on who believes it is the elder: every shared external read single-flights through [`ledger::single_flight::coalesce`](../../crates/rimz/src/ledger/single_flight.rs), so two renderers that both produce still collapse to one `list-panes` per TTL window. The system is safe with zero, one, or many self-declared producers.
- **The eldest rule is the efficiency layer.** UUIDv7 ids sort by birth, so `elder_sidebar_present` lets every younger renderer skip production before ever contending the lock — N tabs become one producer plus N−1 in-process cache reads. It is justified precisely because it is *only* an optimization: a wrong pick is bounded by the flock and healed by the TTL.
- **Takeover latency, as the user perceives it.** On a producer death the next-eldest's scan flips within one heartbeat TTL and it produces on its next cycle — fresh panes in well under ten seconds, while **status flows the entire time** through the rollup fast lane, which needs no producer. A window open/close lagging a few seconds during a rare producer crash sits comfortably inside principle 1's staleness tolerance.

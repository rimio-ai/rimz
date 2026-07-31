# The store

> The store is `crates/rimz/src/store/`, and this doc is its map: the on-disk shape, the event log, the write and read paths, and the maintenance that keeps them bounded. The sidebar's own runtime caches and how a renderer consumes this store are [state.md](./sidebar/state.md). The cost model over these mechanisms is [performance.md](./performance.md), and the product commitments behind them are [DESIGN.md](../../DESIGN.md).

Every durable fact RimZ knows about a room lives in one directory of flat files. There is no daemon and no database. A hook fires, a CLI command runs, an agent finishes a turn: each is a short-lived process that takes an advisory lock, appends a framed record, and exits. Readers, every sidebar renderer included, fold those records back into state without taking a lock at all.

Three properties follow, and the rest of RimZ leans on all three. Writers cannot interleave, because the lock serializes them. Readers never block a writer, because reading is lock-free. And a process killed at any instant leaves a log the next reader can still parse, because the only frame that can be in flight is the last one.

## Truth and cache

One rule organizes everything below.

**`events.log.jsonl` and the durable record files beside it are the truth. Everything else RimZ writes is a cache, and any reader may rebuild it from the log.**

A reader that finds a cache stale, corrupt, or absent folds the log itself and moves on. So a writer that dies before publishing costs the next reader a bounded fold, never a wrong answer. The same rule is why the sidebar can be read-only on the store: it never has to write to be correct, only to be fast. A `cargo xtask invariants` grep keeps store-writer modules out of the sidebar's import graph so that stays true.

Two habits follow from the rule and are worth knowing before you add anything here.

Cache the parse, never a verdict. [`parse_cache.rs`](../../crates/rimz/src/store/parse_cache.rs) memoizes one thread's last deserialization of a JSON cache file, keyed by `(mtime, len)` plus the device and inode pair. That identity is deliberately not airtight: two republishes inside one mtime tick at equal length can serve the older parse. Every caller therefore re-validates against live truth, so a stale serve costs a re-read and never a wrong result.

Freshness is an extent, not a timestamp. A derived rollup records the `LogExtent` it reflects, a `(generation, offset)` pair naming the rotation generation and the byte offset after the last folded frame. Comparing that against the live log is one `stat`, with none of mtime's granularity or write-ordering hazards.

## Where the code lives

`Store` is a cloneable handle around an `Arc`. [`mod.rs`](../../crates/rimz/src/store/mod.rs) holds the handle, the types, and the lock-free reads; every mutator lives under [`writer.rs`](../../crates/rimz/src/store/writer.rs). There is no in-process actor: cross-process serialization is the workspace lock's whole job.

| Module | What it owns |
| --- | --- |
| [`paths.rs`](../../crates/rimz/src/store/paths.rs) | `StatePaths` and `RuntimePaths`: every filename in this doc, and the XDG resolution behind them. |
| [`atomic.rs`](../../crates/rimz/src/store/atomic.rs) | The write primitives. Every fsync syscall in the project is in this file, enforced by a CI grep. |
| [`lock.rs`](../../crates/rimz/src/store/lock.rs) | The workspace advisory lock, bounded at 30 seconds and naming the holder-hunting command on timeout. |
| [`event.rs`](../../crates/rimz/src/store/event.rs) | `EventEnvelope`, the typed `EventKind` decode, and the schema version. |
| [`event_log.rs`](../../crates/rimz/src/store/event_log.rs) | The framed append log: `frame.rs` codec, `recovery.rs` repair, `rotation.rs` archive and retention. |
| [`writer.rs`](../../crates/rimz/src/store/writer.rs) | The `commit` primitive and the off-lock tail. `writer/` splits it into `debounce`, `lifecycle`, `publish`, `queue`, `reap`, and `reset`. |
| [`snapshot/`](../../crates/rimz/src/store/snapshot/mod.rs) | The read side: `fold.rs` resumable rollup, `project.rs` lifecycle reducer, then pane binding and the view-model projection ([sidebar.md](./sidebar/sidebar.md#where-the-code-lives)). |
| [`runtime.rs`](../../crates/rimz/src/store/runtime.rs) | The runtime-versus-audit read scope. |
| [`message_store.rs`](../../crates/rimz/src/store/message_store.rs), [`run_store.rs`](../../crates/rimz/src/store/run_store.rs) | The live message queue and the supervised-run records. |
| [`sidecar.rs`](../../crates/rimz/src/store/sidecar.rs) | The shared latest-wins enrichment sidecar store behind `agent_context/` and `subagent_context/`. |
| [`active_time.rs`](../../crates/rimz/src/store/active_time.rs) | The per-session estimated active-time accumulator, serialized by per-record flocks. |
| [`wakeup.rs`](../../crates/rimz/src/store/wakeup.rs) | The best-effort datagrams a commit posts to live consumers. |
| [`gc.rs`](../../crates/rimz/src/store/gc.rs) | Maintenance: stale runtime hints, orphan write temps, dead workspaces. |
| [`single_flight.rs`](../../crates/rimz/src/store/single_flight.rs) | Cross-process producer election, imported by the sidebar and so free of every writer module. |

Start at `writer.rs` for how a fact gets in, at `snapshot/fold.rs` for how it comes back out, and at `atomic.rs` for what durability actually costs.

## What is on disk

Three tiers, distinguished by what survives a reboot and who may delete them.

### The workspace store

Under `${XDG_STATE_HOME:-~/.local/state}/rimz/workspaces/<workspace_id>/`. This tier is durable truth plus the reboot-surviving caches derived from it.

```text
events.log.jsonl                              the framed event log: crash-recoverable truth
events.log.archive/events.<uuidv7>.jsonl      rotated logs, chronologically sortable by name
agents.carryover.json                         the agent rollup carried across rotation
snapshots/latest.json                         published view-model checkpoint (cache)
snapshots/rollup.json                         resumable fold base (cache)
messages/messages.jsonl                       the live message queue
messages/history.jsonl                        terminal message records, with their text
transcript/<YYYY-MM-DD>.jsonl                 the append-only chat transcript, one file per day
runs/<run_id>.json                            supervised-run records
workspace.json                                project root, root class, session name, room executable
channels.json                                 named channel lanes
rimz                                          stable executable path for deferred pane spawns
boot.json                                     the host boot id last seen, for reboot detection
live-roster.json                              the producer's last pane-backed live agent set
last-death.json                               the last incident, for `rimz list --all`
doctor-cleared.json                           the watermark `rimz doctor` clears incidents against
crashes/<utc-ts>/                             mux forensics from a crash birth, newest five kept
diag.log.jsonl, diag-frames/                  typed anomaly records and captured frames
locks/workspace.lock, locks/publish.lock      the write and publish flocks
locks/{publish,log-sync,dead-reap}.stamp      debounce stamps for the off-lock tail
locks/auto-rotate.stamp                       rotation debounce, taken under the write lock
```

`<workspace_id>` is `ws_` plus the first 24 hex characters of the SHA-256 of the canonical root path. Every root class (repo, marker, bare directory) derives it the same way, so adding a class never re-keys an existing store.

This doc explains the log, the caches derived from it, and the workspace record. For the rest: messages, the transcript, and channels are [messaging.md](./harness/messaging.md), run records are [scripting.md](./harness/scripting.md#the-record), the diagnostics files are [diagnostics.md](./diagnostics.md), and the roster and crash files are [Session death](#session-death) below.

`workspace.json` is the index maintenance commands read after a project root moves or vanishes. It records the project root and its class, the active worktree root, the mux session name, and the executable the room serves as a `(rimz_bin, rimz_build)` path-and-digest pair. Only `rimz start`, cwd-based `rimz attach`, and `rimz reload` set that pair; every other command preserves what it finds, so a CLI call from another worktree cannot retarget a live room. A reader executes the target only after re-digesting it, so a missing or altered file leaves the running process serving. Staging and promotion themselves belong to reload ([sidebar.md → Build promotion](./sidebar/sidebar.md#build-promotion)), which stages generations under `~/.local/state/rimz/builds/<build_id>/`.

Launch reads this record before overwriting it. When the derived session name diverges from a recorded session that is still alive, launch rebirths the workspace under the new name rather than stranding the old one ([`workspace_record.rs`](../../crates/rimz/src/store/workspace_record.rs)).

### The per-room runtime tier

Under `${XDG_RUNTIME_DIR}/rimz/<workspace_id>/`, or `/tmp/rimz-<uid>/rimz/<workspace_id>/` at mode `0700` when `XDG_RUNTIME_DIR` is unset. Everything here is disposable: it speeds the next read and dies with the session.

```text
sock/run.<short_run_id>.sock          per-run wakeup socket, bound by a supervised-run waiter
sock/sidebar.<short_instance_id>.sock per-instance wakeup socket, bound by each live sidebar
sock/codex-app-server.sock            the per-session Codex broker socket
heartbeat/sidebar.*.json              renderer liveness timestamps
read-marks/{sidebar.<id>,manual}.json read receipts that clear unread rows
agent_context/, subagent_context/     latest-wins per-session enrichment sidecars
agent-activity/                       per-agent activity hints
active-time/                          per-root-session active-time accumulators
agent-telemetry/copilot-otel.jsonl    room-scoped metadata-only Copilot export
```

The Copilot export is an adapter concern; what RimZ does with it is [adapter_copilot.md](./agents/adapter_copilot.md). The sidebar also publishes its own caches into this directory, and that catalog belongs to the module that writes them: [state.md → Published lanes](./sidebar/state.md#published-lanes).

Liveness hints live apart from the store for one concrete reason: `AF_UNIX` socket paths are short, 108 bytes on Linux and 104 on macOS including the terminator, and a path under the deep state tree would overrun the budget. The sockets set the location, and the heartbeats and receipts follow them.

The runtime root is mode `0700` and `ensure_private_runtime_dir` refuses to proceed if it finds a symlink, a foreign owner, or group and other permissions it cannot strip. That check is what makes `agent-telemetry/` private through its ancestry.

Freshness here gates behaviour, so these files are scoped to one mux session incarnation. When a birth proves the previous session absent, RimZ purges sidebar heartbeats before creating the replacement, so a fresh-but-dead liveness claim cannot steer launch or reconcile decisions in a reborn room.

### Account-global caches

Under `~/.local/state/rimz/shared/` for the data (`accounts.json`, `rate_limits.json`, `credits.json`, `provider-spending.json`, `spending.json`, `pricing-cache.json`) and `$XDG_RUNTIME_DIR/rimz/shared/` for the election locks and the spending service socket. Data persists so the provider dashboard opens warm after a reboot; locks are runtime because they mean nothing once the process holding them is gone. What each file carries is [state.md → Published lanes](./sidebar/state.md#published-lanes) and [providers.md](./agents/providers.md).

## The event log

`events.log.jsonl` is the canonical history of everything that happened in the workspace.

### Framing

Each record is one line: `<payload length> <crc32, 8 lowercase hex> <json payload>`, newline-terminated. The CRC covers the payload alone, and the length is validated structurally on read. Pre-CRC frames still decode, because a JSON payload always opens with `{` and so cannot be mistaken for an 8-character hex token ([`event_log/frame.rs`](../../crates/rimz/src/store/event_log/frame.rs)).

The payload is an `EventEnvelope`: schema version, event id, workspace id, session name, mux name, source and source kind, method, timestamp, and a method-specific `params` blob kept as raw JSON so a reducer parses only the events it folds.

### What is in it

`method` is the discriminator, and [`EventKind`](../../crates/rimz/src/store/event.rs) is its typed decode.

| Method | Carries | Folded into |
| --- | --- | --- |
| `agent.lifecycle` | One lifecycle signal plus the observation around it: session id, pane stamp, status, turn, context, tokens, subagents. | The agent rollup ([model.md](./agents/model.md)) |
| `agent.attached` | Resume identity and placement: provider session id, stable RimZ launch id, pane stamp, and live runtime owner. | Re-stamps identity and placement; an identified discovered resume seeds its row before the provider starts |
| `agent.launched` | The launch RimZ itself performed: identity, profile, role, team, worktree, permission mode, and a `Starting`/`Bound`/`Failed` state. | Launch admission and resume posture |
| `message.*` | One of eleven terminal or transitional message outcomes: `queued`, `edited`, `after_met`, `when_met`, `sent`, `delivered`, `timed_out`, `errored`, `canceled`, `abandoned`, `archived`. | The message audit trail ([messaging.md](./harness/messaging.md)) |
| `session.rebirth` | Nothing. It is a boundary marker. | Clears every pane stamp recorded before it |
| `session.death` | `cause` (`reboot` or `crash`) and the agents lost with the previous incarnation. | [Session death](#session-death) |
| anything else | Raw params. Older `feed.*` frames and methods from a newer binary land here. | Nothing, by design |

The unknown-method arm matters more than it looks. A record written by a newer RimZ decodes as `Other` and survives every fold, so a downgrade reads an intact log rather than a corrupt one.

`session.rebirth` unstamps; it never ends a session. A reborn mux session renumbers panes from zero, so every stamp recorded before the boundary names a pane that no longer exists. The fold clears them all at that point in the log, which is what keeps a prior incarnation's session off a reused pane id. Each resumed wrapper then appends `agent.attached` to re-establish its stable launch identity, precise placement, and live runtime owner. A provider-store session RimZ had not recorded before can start with this identified attach; legacy attach events without an identity still cannot mint a row.

Lifecycle records carry forward rather than restamping. High-cadence progress events omit `transcript_path`, worktree, pane identity, role, team, channel, profile, and the smart-compact stamp, inheriting them from the prior rollup instead. Missing optional keys decode as absent, and `runtime_owner` is reconstructed from the agent's process identity. That is what keeps the hot log compact under a busy fleet.

### Crash recovery

Only the trailing frame can be in flight when a process dies, because appends are one `write()` under the workspace lock.

A reader that hits an undecodable frame *at the end* of the log stops in front of it and reports an extent that does not claim those bytes. An unterminated tail is an in-flight append the next wakeup will cover, logged at debug. A terminated but torn tail is a power-cut corpse, logged at warn and skipped.

A bad frame *behind* a good one is different: that is real corruption, and the read fails loudly rather than silently dropping everything behind it. [`repair`](../../crates/rimz/src/store/event_log/recovery.rs) is the deliberate recovery, truncating from the first invalid frame to end of file and reporting how many frames it kept and how many bytes it cut. The publish tail calls it automatically when a fold hits corruption; `rimz gc` calls it on demand.

Lock-free `O_APPEND` would drop the lock cost but let writeback reorder and tear a frame in the *middle* of the file. Repair can only cut from that frame onward, so a mid-file tear costs every good record behind it. [performance.md → Deferred and rejected](./performance.md#deferred-and-rejected) records why that trade stays rejected.

### Rotation and carryover

Rotation renames the active log into `events.log.archive/events.<uuidv7>.jsonl` and starts a fresh one. UUIDv7 names sort chronologically, so the archive needs no index.

The store writer claims rotation automatically after a lifecycle append crosses 64 MiB, debounced through `locks/auto-rotate.stamp`, then the lifecycle CLI spawns a detached helper that runs the normal path. `rimz workspace rotate-events` is the manual entry point, with `--max-bytes` overriding the threshold and `--archive-older-than` pruning archives past the 14 day default.

Rotation preserves identity. Before the rename it merges every agent in the rotating log's audit rollup into `agents.carryover.json`, including ended sessions and sessions whose runtime owner has exited, then prunes records older than the retention window and reseeds the fold base. So rotation is storage mechanics: it bounds the log and never changes what the audit rollup remembers. The first post-rotation event for a continuing agent reduces against its carryover row, so the reducer's normal lifetime-field rules preserve launch identity, parentage, and enrichment exactly as if the log had not rotated; `last_seen` still resolves carried-only and live rows in the merged audit view ([`writer/lifecycle.rs`](../../crates/rimz/src/store/writer/lifecycle.rs), [`event_log/rotation.rs`](../../crates/rimz/src/store/event_log/rotation.rs)).

## The write path

Every mutation runs the same choreography, through `Store::commit` in [`writer.rs`](../../crates/rimz/src/store/writer.rs).

Under the workspace lock:

1. Read whatever state the decision needs.
2. Write the durable record files this mutation touches.
3. Append the event frames, one ordered batch per logical change.

Then the lock releases, and the tail runs off-lock:

4. Post a `StoreDelta` wakeup to every fresh sidebar, one per appended event, and ping a completing run's waiter socket.
5. Group-sync the log with a single `fdatasync`, at most once per second.
6. Publish the snapshot checkpoint, when due.
7. Reap provably dead sessions, when due.

Steps 5 through 7 are each gated by a stamp file beside the lock: `log-sync.stamp`, `publish.stamp`, `dead-reap.stamp`. A fourth, `auto-rotate.stamp`, gates the rotation claim a lifecycle append makes inside the lock. A missing, unreadable, or future-dated stamp reads as due, so clock and I/O uncertainty costs one redundant run rather than a skipped one.

Those stamps are what keep the write path O(1) over log history. A busy fleet appending hundreds of events a second still pays one fsync and one checkpoint per second between them, however long the log has grown.

Wakeups fire *before* the publish, deliberately. Consumers fold the log tail from their own cursor, so checkpoint cadence tunes cold-start latency and never gates freshness.

### Write classes

Every disk write falls into one of four classes, and one line classifies them: **durable records and cold metadata fsync; hot appends and disposable caches do not.** A cache rebuilds, and a group sync or an audit tolerance bounds what an append can lose.

| Class | Files | Discipline | After a power cut |
| --- | --- | --- | --- |
| Event log | `events.log.jsonl` | One CRC-framed `write()` per record or ordered batch. The off-lock tail issues a group `fdatasync` about once a second; rotation syncs before the rename. | Intact through the last group sync. The trailing window can be lost, and the frame CRC turns any torn suffix into deterministic corruption that repair truncates. |
| Audit appends | `messages/history.jsonl`, `transcript/*.jsonl` | `O_APPEND` under the workspace lock, no per-record fsync. | Trailing records can be lost. The cost is history completeness, never queue correctness. |
| Cache | `snapshots/*.json`, `live-roster.json`, heartbeats, sidecars, the sidebar's published lanes | Temp file plus atomic rename, no fsync. | Rebuilt from the log or refreshed from live producers on the next read. |
| Durable records | `messages/messages.jsonl`, `runs/<run_id>.json`, `workspace.json`, `agents.carryover.json`, `channels.json`, trust grants, notification handlers, hook installs | Temp file, fsync, rename, parent-directory sync. | Survives. |

Every fsync syscall funnels through [`atomic.rs`](../../crates/rimz/src/store/atomic.rs), checked by a CI grep, so the discipline is enforced rather than reviewed. No module hand-rolls its own atomic dance.

### Wakeups

After a commit the writer walks the runtime heartbeat directory and sends a typed `store_delta` datagram to each sidebar whose heartbeat is fresh within about 5 seconds, re-stat-ing each file just before the send to close the window where a renderer exited between read and write. A completing supervised run additionally pings its waiter's [run socket](./harness/scripting.md#the-wake-socket).

Sends are non-blocking, so a full receiver queue drops the datagram and the write moves on. Per-target failures are absorbed; only a failure to read the heartbeat directory propagates.

A wakeup carries latency, never truth. The consumer folds the log tail from its own cursor, treats the published checkpoint as an accelerator it may skip, and closes any missed wakeup at its next tick (`rimz sidebar serve --tick-seconds`, default 1s). The envelope and its full event taxonomy belong to the receiving side, in [state.md → Realtime events](./sidebar/state.md#realtime-events).

## The read path

Reads take no lock. `snapshot::rebuild` and the `assemble.rs` entry points fold the log into the agent rollup, then project that into the sidebar view-model ([sidebar.md](./sidebar/sidebar.md#from-store-to-screen) owns the projection).

The fold is resumable. `snapshots/rollup.json` holds a fold base and the `LogExtent` it reflects; `snapshots/latest.json` holds the published view-model and the extent it was built from. A reader trusts either checkpoint exactly when its extent matches the live log, and folds the missing tail itself otherwise. So a cold reader pays one full fold, a warm reader pays only the new bytes, and neither can be wrong ([`snapshot/fold.rs`](../../crates/rimz/src/store/snapshot/fold.rs), [`writer/publish.rs`](../../crates/rimz/src/store/writer/publish.rs)).

The publish gate decides when the writer refreshes those files. A checkpoint is due when the stamp is a second old, when the unpublished tail crosses 64 KiB, or when the log shrank underneath the stamp, which is how rotation, an identity rewrite, and a repair all force a fresh publication. Concurrent publishers serialize on `locks/publish.lock` and group-commit.

### Runtime and audit

The same durable rollup answers two questions, and [`runtime.rs`](../../crates/rimz/src/store/runtime.rs) is the filter between them.

**Runtime** is read-time filtering, and it backs the default views: `rimz sidebar snapshot`, the plain `rimz doctor` agent summary. It hides rows carrying `ended_at` and keeps the agent rollups whose `runtime_owner` is still the live process that wrote them; ended and dead-owner-expelled identities ride the published snapshot as `fenced_sessions` to fence provider-local session rebinding. `runtime_owner` records the owner kind, a stable subject id, the pid, and on Linux the process-start token, so a reused pid does not read as the original owner. Records with no owner at all abstain and stay visible; known-dead owners and process-start mismatches are suppressed while the write path converges them to a real end stamp.

**Audit** bypasses the filter and reads durable history as written. `rimz doctor --audit` is the surface, and explicit resume is the reason ended rows are retained at all.

## Session death

Two mechanisms end a session in the store. One converges sessions RimZ can prove are gone; the other records the loss of a whole room.

### The reap

The off-lock tail runs a debounced scan, at most once a minute, over the audit rollup. It appends an `agent.lifecycle` `Ended` observation for each root session it can prove is finished, and the event name it uses is the reason:

| Event name | Condition |
| --- | --- |
| `ReapedSuperseded` | A newer session took the same pane: a genuine relaunch, or a `/clear` that started a fresh conversation in place. |
| `ReapedInterrupted` | The adapter probe confirms the older conversation was interrupted at rest and a newer one superseded it. |
| `ReapedDead` | The recorded owner process is provably dead. |
| `ReapedStale` | The session never captured a pid, or its owner is the shared daemon, and it has been quiet past the three-hour ghost TTL. |

An agent named in `live-roster.json` is exempt from the last two until rebirth planning consumes the roster, so crash-recovery candidates survive the scan. Already-ended rows are skipped and cannot supersede an active replacement.

The reducer stamps `ended_at`, which hides the row from runtime views immediately while the audit rollup keeps its resumable identity until rotation prunes it at the retention boundary. The stamp is suppression, not a terminal state: any later lifecycle event under the same `(kind, session id)` clears it, so a session that reports in again returns to the runtime view under its own identity and every fence keyed on the end stamp releases with it. Runtime expel and snapshot view reaping apply the same predicates as latency shims, so the screen and the log agree before the durable append lands.

### The session.death record

When a room comes back after its mux session died, the rebirth path records `session.death` for the incarnation that was lost. The event carries `cause` and the agents that went with it. Reboot wins when the host boot id in `boot.json` changed; a same-boot crash additionally requires `live-roster.json` to name agents worth recovering.

`live-roster.json` is the sidebar producer's last pane-backed live root-agent set, the agents that mux session would lose if it died. It is written cache-class and intersected with the audit rollup at birth, so cleanly ended agents and paneless ghosts stay out of recovery. `session.rebirth` clears it after planning, so a fast second birth cannot reuse stale evidence.

Beside it, `last-death.json` records the incident for cheap `rimz list --all` display, and the reborn room writes back how many lost agents it seeded. A lost session that recovery leaves behind gets a durable `Ended` trace instead: `rimz.recovery-declined` when the user chose a fresh start, `rimz.not-resumed` for a leftover recovery could not seed. Either trace drops the session out of the next recovery set while keeping it resumable by hand. A crash birth also archives mux forensics under `crashes/<utc-ts>/`, best-effort and never blocking launch, retaining the newest five. `rimz doctor` surfaces the last incident with its cause, time, lost and recovered counts, and archive path.

The recovery flow itself, from roster to repopulated panes, is [sidebar.md → Resume-on-rebirth](./sidebar/sidebar.md#resume-on-rebirth) and [fleet.md](./harness/fleet.md).

## Maintenance

`rimz reset` is a room boundary. It cancels active runs and terminal-wakes their waiters, force-rotates the log, clears diagnostics, and removes the runtime directory. A soft reset applies the same carryover contract as rotation, so agent identity including ended sessions survives within retention even after pane teardown. `--hard` is the explicit forget boundary: it drops the carryover and the derived caches. Provider-owned session files live outside this store either way ([`writer/reset.rs`](../../crates/rimz/src/store/writer/reset.rs)).

`rimz gc` is the global collector. Inside the runtime directory it removes expired heartbeats, the wakeup sockets those heartbeats named, stale context, activity, active-time, and telemetry sidecars, read receipts whose owning sidebar has expired, and stale provider probe markers. It leaves `run.*.sock` alone, because a live supervised-run waiter may still own one, and keeps `read-marks/manual.json` with the room runtime.

Across the state tree it archives orphaned message records, prunes carryover agents past the retention window, sweeps orphaned atomic-write temp files, and prunes workspaces it can prove are dead: a recorded project root that no longer exists, or an abandoned `rimz start` scaffold with no history. A directory whose record is unreadable but which still holds history is kept and reported, never deleted ([`gc/collect.rs`](../../crates/rimz/src/store/gc/collect.rs), [`gc/prune.rs`](../../crates/rimz/src/store/gc/prune.rs)).

Path setup also sweeps pre-migration copies of the account-global data caches out of the runtime `shared/` directory, releasing tmpfs those files used to pin.

## What survives what

| Event | Store | Live sockets and heartbeats | Multiplexer session |
| --- | --- | --- | --- |
| Detach | yes | yes, the mux server stays alive | yes |
| Sidebar reload | yes | socket rebound on attach | yes |
| Multiplexer server crash | yes | no | no |
| Host reboot | yes | no | no, needs a host supervisor (tmux-resurrect, Zellij resurrect, systemd) |
| Host power cut | yes, through the last group `fdatasync`; the trailing window can be lost and repair truncates any torn suffix ([write classes](#write-classes)) | no | no, needs a host supervisor |

RimZ guarantees the store across all of these. At a power cut that guarantee runs through the last group sync, with repair bounding the damage to the final window. The session and its processes survive only what the multiplexer server and the host supervisor keep alive.

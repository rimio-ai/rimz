# The store

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes, and [performance.md](./performance.md) for the cost model over these mechanisms.

The store is the workspace's durable state engine: a directory of flat files that every writer appends to and every renderer reads. Correctness lives here — the harness, the message queue, the sidebar, and the agent UIs all own their semantics one layer up and keep their truth in the store's records.

This doc owns the durability contract — the on-disk shape and the write classes. The write- and read-path *choreography* lives beside the code in [`store/AGENTS.md`](../../crates/rimz/src/store/AGENTS.md), and each file's mechanics live in the module linked from the rule that names it. The supervised-run wake that rides the store's write path is [harness.md → The run wake](./harness/harness.md#supervised-runs).

## Durable state

Under `${XDG_STATE_HOME:-~/.local/state}/rimz/workspaces/<workspace_id>/`:

```text
workspace.json                                  project root, root class, session name, verified room build target
rimz                                            stable executable path for deferred room spawns
events.log.jsonl                                the framed event log — crash-recoverable truth
events.log.archive/events.<uuidv7>.jsonl        rotated logs, chronologically sortable
agents.carryover.json                           agent rollup carried across rotation
live-roster.json                                producer-written live agent roster for rebirth recovery
snapshots/latest.json                           published view-model checkpoint (cache)
snapshots/rollup.json                           resumable agent-rollup fold base (cache)
messages/messages.jsonl                         the live message queue ([messaging.md](./harness/messaging.md))
messages/history.jsonl                          terminal message records with text
transcript/<bucket-start>.jsonl                 the append-only chat transcript log
runs/<run_id>.json                              supervised-run records ([harness.md](./harness/harness.md))
locks/workspace.lock                            the single-writer flock
locks/{publish,log-sync,auto-rotate,dead-reap}.stamp debounce stamps for the off-lock write tail
```

`<workspace_id>` is `ws_` plus the first 24 hex of the SHA-256 of the canonical root path — the same derivation for every root class (repo, marker, directory), so introducing a class never re-keys a store.

User-scoped immutable executable generations live under `${XDG_STATE_HOME:-~/.local/state}/rimz/builds/<build_id>/rimz`. Room start and reload copy the invoking executable there with a durable temp-file-plus-rename write and mode `0755`, then publish its path and digest together in each live workspace record. Each publish also atomically refreshes `workspaces/<workspace_id>/rimz`, the stable room-bin path used by deferred pane spawns and session-lifetime helpers; it is a hardlink to the staged generation when the filesystem permits, so sweeping the generation directory cannot orphan the spawn path, with an executable byte copy as the fallback. Staging reuses a matching digest and sweeps generations referenced by no known workspace record, while retaining the generation being staged and any generation younger than the one-minute stage-to-record race lease.

The split of truth from cache is the organizing rule: **`events.log.jsonl` and the durable record files beside it are the crash-recoverable truth; everything under `snapshots/` is a reconstructible cache.** A reader rebuilds the cache from the log on any mismatch or parse failure, so a writer that crashes before publishing costs the next reader a bounded fold, never staleness.

- `workspace.json` records the project root, root class, session name, and optional verified executable target as the `rimz_bin` and `rimz_build` pair. A record predating `root_class` decodes as `repo`, and a record predating the target pair falls back to the running process's executable until an owner flow self-heals it. Generic re-records preserve both fields; only `rimz start`, cwd-based `rimz attach`, and `rimz reload` update them, while named attach by session preserves the recorded owner, so routine CLI commands from another worktree do not change a live room's executable authority. Readers execute the target only after its current digest matches `rimz_build`; a missing file or mismatch leaves the old process serving. Launch reads the record before overwriting: when the derived session name diverges from a still-live recorded session, launch rebirths the workspace under the new name rather than stranding the session ([`workspace_record.rs`](../../crates/rimz/src/store/workspace_record.rs)).
- `snapshots/latest.json` is the published view-model and `snapshots/rollup.json` the resumable fold base; writers publish both off-lock, debounced through stamps, stamped with the log `(generation, offset)` they reflect. A reader trusts a checkpoint exactly when its stamp matches the live log and folds the missing tail itself otherwise ([`snapshot/fold.rs`](../../crates/rimz/src/store/snapshot/fold.rs), [`writer/publish.rs`](../../crates/rimz/src/store/writer/publish.rs)).
- `rimz workspace rotate-events` archives the active log once it crosses a byte threshold and prunes archives older than the retention window. After a lifecycle append succeeds, the store writer claims automatic rotation in the same workspace-lock transaction at the default 64MiB threshold, debounced through `locks/auto-rotate.stamp`; the lifecycle CLI then spawns the detached helper that executes the normal rotation command. Manual rotation uses the same threshold unless `--max-bytes` overrides it, and archive pruning defaults to 14d. Rotation first merges every agent in the rotating log's audit rollup into `agents.carryover.json`, including ended sessions and sessions whose runtime owner has exited, then prunes dead records older than the retention window and reseeds the rollup base. Rotation is therefore a storage mechanism: it preserves resumable identity within retention and never changes what the audit rollup remembers ([`writer/lifecycle.rs`](../../crates/rimz/src/store/writer/lifecycle.rs), [`event_log/rotation.rs`](../../crates/rimz/src/store/event_log/rotation.rs)).
- The write path converges store-provable session death between rotations: after publishing commits, a debounced `locks/dead-reap.stamp` gate scans the audit rollup and appends an `agent.lifecycle` `Ended` observation for root sessions with a dead recorded owner, pidless or daemon-owned sessions past the ghost TTL, relaunch-superseded sessions, and `/clear`-superseded conversations. The reducer stamps `ended_at`; runtime views hide that row immediately while the audit rollup retains its resumable identity until rotation prunes it at the retention boundary. A key present in `live-roster.json` stays protected until rebirth planning consumes the recovery roster. Already-ended rows are skipped and cannot supersede an active replacement. Runtime expel and snapshot view reaping use the same rules as latency shims; the live daemon loaded-thread probe remains snapshot-side.
- `agent.lifecycle` records use carry-forward fields to keep the hot log compact. Missing optional keys decode as absent, `runtime_owner` is reconstructed from the agent process identity, and high-cadence progress events carry `transcript_path`, worktree, pane identity, role, team, channel, profile, and the smart-compact stamp from the prior rollup instead of restamping them on every tool event. The rotation merge backfills those same enrichment fields from carryover when the first trimmed post-rotation event wins by `last_seen`.
- `rimz reset` is a room boundary in the store: it cancels active runs, force-rotates the log, clears diagnostic files, and removes the runtime directory. Soft reset applies the same carryover contract and preserves the audit rollup's agent identity, including ended sessions, within retention even after pane teardown. `--hard` is the explicit RimZ identity-forget boundary and drops the carryover; provider-owned session files remain outside this store ([`writer/reset.rs`](../../crates/rimz/src/store/writer/reset.rs)).

### Write classes

Every disk write belongs to one of four classes. The classification rule is one line: **durable records and cold metadata fsync; the hot appends and the disposable caches do not** — a cache rebuilds, and a group sync or audit tolerance bounds an append's loss. Every fsync syscall funnels through [`atomic.rs`](../../crates/rimz/src/store/atomic.rs) (CI grep), so the discipline is enforced, not reviewed.

| Class | Files | Write discipline | After a power cut |
| --- | --- | --- | --- |
| Event log | `events.log.jsonl` | one CRC-framed `write()` per record; the off-lock tail issues a group `fdatasync` (~1/s), and rotation syncs before the rename | intact through the last group sync. The trailing window can be lost; the frame CRC turns any torn suffix into deterministic corruption that repair truncates |
| Audit appends | `messages/history.jsonl`, `transcript/*.jsonl` | `O_APPEND` under the workspace lock, no per-record fsync | trailing records can be lost; the cost is history completeness, never queue correctness |
| Cache | `snapshots/*.json`, `live-roster.json`, heartbeats, sidecars | temp file + atomic rename, no fsync | rebuilt or refreshed from live producers on the next read |
| Durable records | `messages/messages.jsonl`, `runs/<run_id>.json`, `workspace.json`, `agents.carryover.json`, trust grants, notification handlers, hook installs | temp file, fsync, rename, parent-dir sync | survives |

Crash recovery rests on the framing and the flock. Each record is framed `<len> <crc32> <json>`, the CRC over the payload, and pre-CRC frames still decode ([`event_log/frame.rs`](../../crates/rimz/src/store/event_log/frame.rs)). The workspace flock makes the log single-writer-at-a-time; acquisition is bounded to about 30s and identifies the lock path and holder-hunting command on timeout. Only the *trailing* frame can be in flight at a crash: a torn suffix is truncated and logged, while a bad frame *behind* a good one is real corruption that fails the read loudly rather than silently dropping the events behind it ([`event_log/recovery.rs`](../../crates/rimz/src/store/event_log/recovery.rs)). Lock-free `O_APPEND` would let writeback reorder and tear a *middle* frame; [performance.md](./performance.md#bottlenecks-and-deferred-work) records why that trade is rejected.

## Runtime state

Liveness hints live apart from the store, under `${XDG_RUNTIME_DIR}/rimz/<workspace_id>/` (or `/tmp/rimz-<uid>/rimz/<workspace_id>/` at mode `0700` when `XDG_RUNTIME_DIR` is unset):

```text
sock/run.<short_run_id>.sock            per-run wakeup socket; a supervised-run waiter binds and tears it down
sock/sidebar.<short_instance_id>.sock   per-instance wakeup socket; each live sidebar binds one
heartbeat/sidebar.*.json                liveness timestamps
read-marks/{sidebar.<id>,manual}.json   renderer and room-runtime read receipts
agent_context/                          latest per-session normalized enrichment
agent-activity/                         per-agent activity hints
agent-telemetry/copilot-otel.jsonl      room-scoped metadata-only Copilot OTel cache
```

Sockets, heartbeats, and read receipts are liveness hints — rebuilt or rebound as processes come and go, and the [survival table](#what-survives-what) treats them as expendable. They live apart from the store because `AF_UNIX` socket paths are short (108 bytes on Linux, 104 on macOS, terminator included), so a deep state path would overrun them.

The runtime root is mode `0700`, so `agent-telemetry/copilot-otel.jsonl` stays private through its ancestry. Copilot appends metadata-only OTel records while the room lives; RimZ pins message-content capture off for this managed file, scans only a bounded tail, and filters by exact conversation ID. The file is cache-class data: reset removes it with the room runtime and GC removes it after the room heartbeat expires and the runtime retention threshold passes. A fresh room heartbeat protects the exporter file and its parent from GC because Copilot's reopen and flush behavior is not a pinned contract.

Runtime files whose freshness gates behaviour are scoped to one mux session incarnation. When a birth proves the session absent, RimZ purges sidebar heartbeat files before creating the replacement session, so a fresh-but-dead liveness claim cannot choose launch, reconcile, or session-record behaviour for a reborn room. Display hints and sockets keep their existing TTL or connect self-validation.

`rimz gc` collects this directory: it removes expired heartbeats, the wakeup sockets they named, stale agent context/activity/telemetry files, read receipts whose owning sidebar has expired, and stale provider probe-throttle markers in the runtime shared dir, keeping `read-marks/manual.json` with the room runtime and leaving `run.*.sock` alone because a live supervised-run waiter may still own one. Startup path setup (`RuntimePaths::ensure_dirs`) also sweeps pre-migration data-cache copies from the runtime `shared/` dir, releasing old tmpfs files after shared data moved under state-home. As the global collector it also archives orphaned message records, prunes stale carryover agents on the store retention window, prunes provably-dead workspaces — a vanished project root, or an abandoned scaffold with no history — and sweeps orphaned atomic-write temps with `atomic::sweep_orphan_temps_under` across the state and runtime trees, while keeping and reporting any workspace that still holds history ([`gc/collect.rs`](../../crates/rimz/src/store/gc/collect.rs), [`gc/prune.rs`](../../crates/rimz/src/store/gc/prune.rs), [`atomic.rs`](../../crates/rimz/src/store/atomic.rs)).

## Runtime projection

History and runtime are separate views over the one durable store ([`runtime.rs`](../../crates/rimz/src/store/runtime.rs)).

- **Expel** is read-time filtering. Default runtime views (`rimz sidebar snapshot`, the default `rimz doctor` agent summary) hide rows with `ended_at` and keep the remaining agent rollups whose `runtime_owner` is still the live process that wrote them. Ownerless legacy records abstain and remain visible; known-dead owners and Linux PID-start mismatches are suppressed while the write-path session reap converges them to an end stamp.
- **Audit** is durable history. `rimz doctor --audit` reads the full rollup, including ended rows retained for explicit resume.

`runtime_owner` records the owner kind, a stable subject id, pid, and the Linux process-start token when available.

## Session death records

`session.death` records the previous room incarnation's death before a genuine birth replaces it. The event carries `cause` (`reboot` or `crash`) and `lost_agents`; reboot wins when the boot marker changed, while same-boot crash recovery requires the producer's persisted `live-roster.json` to contain recoverable agents. The roster is the sidebar's last live root-agent set, written through cache-class temp-file-plus-rename and intersected with the audit rollup at birth, so cleanly ended agents and older ghosts stay out of recovery.

Rebirth materialization also writes `last-death.json` beside the workspace store for cheap `rimz list --all` display. The reborn room writes back `recovered` after the recovery plan is finalized, so the marker records how many lost agents were seeded again. Crash births archive mux forensics under `crashes/<utc-ts>/mux-cache/` and write `roster.json` with the recovered agents' rollup rows; retention keeps the newest five archives. The archive is best-effort and never blocks launch. `session.rebirth` clears `live-roster.json` after planning so a fast second birth cannot reuse stale evidence before the new producer publishes. `rimz doctor` surfaces the last incident with cause, time, lost agents, recovered count, and the crash archive path when one exists.

## Wakeups

After every write, the writer wakes live consumers off-lock: it walks fresh sidebar heartbeats (TTL ~5s) and sends each a `store_delta` wakeup datagram, and a completing run pings its waiter's [run socket](./harness/harness.md#supervised-runs). The send is non-blocking, so a full receiver queue drops the datagram. The envelope and its event taxonomy live in [state.md → event taxonomy](./sidebar/state.md#event-taxonomy) and [`sidebar/events.rs`](../../crates/rimz/src/sidebar/events.rs).

A wakeup carries latency, not truth: the consumer folds the log tail from its own cursor, and the published checkpoint is a catch-up accelerator it can skip. A missed wakeup is closed by the next sidebar tick (`--tick-seconds`, default 1s) ([`wakeup.rs`](../../crates/rimz/src/store/wakeup.rs)).

## What survives what

| Event | Store | Live sockets and heartbeats | Multiplexer session |
| --- | --- | --- | --- |
| Detach | yes | yes (mux server stays alive) | yes |
| Sidebar reload | yes | sidebar socket rebound on attach | yes |
| Multiplexer server crash | yes | no | no |
| Host reboot | yes | no | no — needs a host supervisor (tmux-resurrect, Zellij resurrect, systemd) |
| Host power cut | yes — through the last group `fdatasync`; the trailing window can be lost, and repair truncates any torn suffix (see [write classes](#write-classes)) | no | no — needs a host supervisor |

RimZ guarantees the store across all of these: at a power cut, through the last group sync, with repair bounding the damage to the final window. The session and its processes survive only what the multiplexer server and the host supervisor keep alive.

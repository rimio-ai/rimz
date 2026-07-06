# Ledger and bridge

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes, and [performance.md](../health/performance.md) for the cost model over these mechanisms.

The ledger is the workspace's durable source of truth: a directory of flat files that every writer appends to and every renderer reads. Correctness lives here — the sidebar, notifications, and the agent UIs are all views over it. The bridge is the blocking path layered on top: it carries a completion wake frame to a supervised run's waiter over the run's own socket.

This doc owns the durability contract — the on-disk shape, the write classes, and the run bridge. The write- and read-path *choreography* lives beside the code in [`ledger/AGENTS.md`](../../../crates/rimz/src/ledger/AGENTS.md), and each file's mechanics live in the module linked from the rule that names it.

## Durable state

Under `${XDG_STATE_HOME:-~/.local/state}/rimz/workspaces/<workspace_id>/`:

```text
workspace.json                                  project root, root class, session name
events.log.jsonl                                the framed event log — crash-recoverable truth
events.log.archive/events.<uuidv7>.jsonl        rotated logs, chronologically sortable
agents.carryover.json                           agent rollup carried across rotation
snapshots/latest.json                           published view-model checkpoint (cache)
snapshots/rollup.json                           resumable agent-rollup fold base (cache)
messages/messages.jsonl                         the live message queue ([message.md](../agents/message.md))
messages/history.jsonl                          terminal message records with text
transcript/<bucket-start>.jsonl                 the append-only chat transcript log
runs/<run_id>.json                              supervised-run records ([harness.md](../agents/harness.md))
locks/workspace.lock                            the single-writer flock
locks/{publish,log-sync,auto-rotate}.stamp      debounce stamps for the off-lock write tail
```

`<workspace_id>` is `ws_` plus the first 24 hex of the SHA-256 of the canonical root path — the same derivation for every root class (repo, marker, directory), so introducing a class never re-keys a ledger.

The split of truth from cache is the organizing rule: **`events.log.jsonl` and the durable record files beside it are the crash-recoverable truth; everything under `snapshots/` is a reconstructible cache.** A reader rebuilds the cache from the log on any mismatch or parse failure, so a writer that crashes before publishing costs the next reader a bounded fold, never staleness.

- `workspace.json` records the project root, root class, and session name for maintenance commands. A record predating `root_class` decodes as `repo` and self-heals on the next start. Launch reads it before overwriting: when the derived session name diverges from a still-live recorded session, launch rebirths the workspace under the new name rather than stranding the session ([`workspace_record.rs`](../../../crates/rimz/src/ledger/workspace_record.rs)).
- `snapshots/latest.json` is the published view-model and `snapshots/rollup.json` the resumable fold base; writers publish both off-lock, debounced through stamps, stamped with the log `(generation, offset)` they reflect. A reader trusts a checkpoint exactly when its stamp matches the live log and folds the missing tail itself otherwise ([`snapshot/fold.rs`](../../../crates/rimz/src/ledger/snapshot/fold.rs), [`writer/publish.rs`](../../../crates/rimz/src/ledger/writer/publish.rs)).
- `rimz workspace rotate-events` archives the active log once it crosses a byte threshold and prunes archives older than the retention window. Lifecycle hooks trigger the same rotation path automatically at the default 64MiB threshold, debounced through `locks/auto-rotate.stamp`; manual rotation uses the same threshold unless `--max-bytes` overrides it, and archive pruning defaults to 14d. Rotation first merges the rotating log's agent rollup into `agents.carryover.json`, prunes carryover agents that are older than the retention window and have no live recorded owner, and reseeds the rollup base so the sidebar's agent panel stays correct across rotations without rescanning archives ([`event_log/rotation.rs`](../../../crates/rimz/src/ledger/event_log/rotation.rs)).
- `agent.lifecycle` records use carry-forward fields to keep the hot log compact. Missing optional keys decode as absent, `runtime_owner` is reconstructed from the agent process identity, and high-cadence progress events carry `transcript_path`, worktree, pane identity, role, team, channel, profile, and the smart-compact stamp from the prior rollup instead of restamping them on every tool event. The rotation merge backfills those same enrichment fields from carryover when the first trimmed post-rotation event wins by `last_seen`.
- `rimz reset` is a room boundary in the ledger: it cancels active runs, force-rotates the log, clears diagnostic files, and removes the runtime directory. Soft reset keeps `agents.carryover.json` for audit; `--hard` also drops it ([`writer/reset.rs`](../../../crates/rimz/src/ledger/writer/reset.rs)).

### Write classes

Every disk write belongs to one of four classes. The classification rule is one line: **durable records and cold metadata fsync; the hot appends and the disposable caches do not** — a cache rebuilds, and a group sync or audit tolerance bounds an append's loss. Every fsync syscall funnels through [`atomic.rs`](../../../crates/rimz/src/ledger/atomic.rs) (CI grep), so the discipline is enforced, not reviewed.

| Class | Files | Write discipline | After a power cut |
| --- | --- | --- | --- |
| Event log | `events.log.jsonl` | one CRC-framed `write()` per record; the off-lock tail issues a group `fdatasync` (~1/s), and rotation syncs before the rename | intact through the last group sync. The trailing window can be lost; the frame CRC turns any torn suffix into deterministic corruption that repair truncates |
| Audit appends | `messages/history.jsonl`, `transcript/*.jsonl` | `O_APPEND` under the workspace lock, no per-record fsync | trailing records can be lost; the cost is history completeness, never queue correctness |
| Cache | `snapshots/*.json`, heartbeats, sidecars | temp file + atomic rename, no fsync | rebuilt from the log on the next read |
| Durable records | `messages/messages.jsonl`, `runs/<run_id>.json`, `workspace.json`, `agents.carryover.json`, trust grants, notification handlers, hook installs | temp file, fsync, rename, parent-dir sync | survives |

Crash recovery rests on the framing and the flock. Each record is framed `<len> <crc32> <json>`, the CRC over the payload, and pre-CRC frames still decode ([`event_log/frame.rs`](../../../crates/rimz/src/ledger/event_log/frame.rs)). The workspace flock makes the log single-writer-at-a-time, so only the *trailing* frame can be in flight at a crash: a torn suffix is truncated and logged, while a bad frame *behind* a good one is real corruption that fails the read loudly rather than silently dropping the events behind it ([`event_log/recovery.rs`](../../../crates/rimz/src/ledger/event_log/recovery.rs)). Lock-free `O_APPEND` would let writeback reorder and tear a *middle* frame; [performance.md](../health/performance.md#bottlenecks-and-deferred-work) records why that trade is rejected.

## Runtime state

Liveness hints live apart from the ledger, under `${XDG_RUNTIME_DIR}/rimz/<workspace_id>/` (or `/tmp/rimz-<uid>/rimz/<workspace_id>/` at mode `0700` when `XDG_RUNTIME_DIR` is unset):

```text
sock/run.<short_run_id>.sock            per-run wakeup socket; a supervised-run waiter binds and tears it down
sock/sidebar.<short_instance_id>.sock   per-instance wakeup socket; each live sidebar binds one
heartbeat/sidebar.*.json                liveness timestamps
read-marks/{sidebar.<id>,manual}.json   renderer and room-runtime read receipts
```

Sockets, heartbeats, and read receipts are liveness hints — rebuilt or rebound as processes come and go, and the [survival table](#what-survives-what) treats them as expendable. They live apart from the ledger because `AF_UNIX` socket paths are short (108 bytes on Linux, 104 on macOS, terminator included), so a deep state path would overrun them.

`rimz gc` collects this directory: it removes expired heartbeats, the wakeup sockets they named, read receipts whose owning sidebar has expired, and stale provider probe-throttle markers in the runtime shared dir, keeping `read-marks/manual.json` with the room runtime and leaving `run.*.sock` alone because a live supervised-run waiter may still own one. Startup path setup (`RuntimePaths::ensure_dirs`) also sweeps pre-migration data-cache copies from the runtime `shared/` dir, releasing old tmpfs files after shared data moved under state-home. As the global collector it also archives orphaned message records, prunes stale carryover agents on the ledger retention window, prunes provably-dead workspaces — a vanished project root, or an abandoned scaffold with no history — and sweeps orphaned atomic-write temps with `atomic::sweep_orphan_temps_under` across the state and runtime trees, while keeping and reporting any workspace that still holds history ([`gc/collect.rs`](../../../crates/rimz/src/ledger/gc/collect.rs), [`gc/prune.rs`](../../../crates/rimz/src/ledger/gc/prune.rs), [`atomic.rs`](../../../crates/rimz/src/ledger/atomic.rs)).

## The run bridge

A blocking supervised run (`rimz agents <kind> -p`) waits on its own socket for the completion wake. The waiter binds `sock/run.<short_run_id>.sock` before the pane opens, the run record lands at `runs/<run_id>.json`, and the lifecycle write that completes the run sends a `RunCompleted` frame. The waiter validates every frame by `(workspace_id, run_id)` and drops a mismatch, so a frame can never complete a stranger's run ([`bridge.rs`](../../../crates/rimz/src/bridge.rs)).

The socket is latency, never truth: the run's terminal state is the durable record, and the frame only tells the waiter to read it. The run lifecycle — record fields, completion sources, streaming, and cleanup — is [harness.md → Supervised runs](../agents/harness.md#supervised-runs).

## Runtime projection

History and runtime are separate views over the one durable ledger ([`runtime.rs`](../../../crates/rimz/src/ledger/runtime.rs)).

- **Expel** is read-time filtering. Default runtime views (`rimz sidebar snapshot`, the default `rimz doctor` agent summary) keep only agent rollups whose `runtime_owner` is still the live process that wrote them. Ownerless legacy records, dead owners, and Linux PID-start mismatches are audit-only.
- **Audit** is durable history. `rimz doctor --audit` reads the full rollup.

`runtime_owner` records the owner kind, a stable subject id, pid, and the Linux process-start token when available.

## Session death records

`session.death` records the previous room incarnation's death before a genuine birth replaces it. The event carries `cause` (`reboot` or `crash`) and `lost_agents`; reboot wins when the boot marker changed, while same-boot crash recovery requires positive `agent-lost` markers from the exec wrappers. The fold keeps the `lost` set only until the next `session.rebirth`, so a later incarnation never sees stale crash evidence.

The coroner also writes `last-death.json` beside the workspace ledger for cheap `rimz list --all` display. Crash births archive mux forensics under `crashes/<utc-ts>/mux-cache/` and write `roster.json` with the lost agents' rollup rows; retention keeps the newest five archives. The archive is best-effort and never blocks launch.

## Wakeups

After every write, the writer wakes live consumers off-lock: it walks fresh sidebar heartbeats (TTL ~5s) and sends each a `ledger_delta` wakeup datagram, and a completing run pings its waiter's [run socket](#the-run-bridge). The envelope and its event taxonomy live in [state.md → event taxonomy](./state.md#event-taxonomy) and [`sidebar/events.rs`](../../../crates/rimz/src/sidebar/events.rs).

A wakeup carries latency, not truth: the consumer folds the log tail from its own cursor, and the published checkpoint is a catch-up accelerator it can skip. A missed wakeup is closed by the next sidebar tick (`--tick-seconds`, default 1s) ([`wakeup.rs`](../../../crates/rimz/src/ledger/wakeup.rs)).

## What survives what

| Event | Ledger | Live sockets and heartbeats | Multiplexer session |
| --- | --- | --- | --- |
| Detach | yes | yes (mux server stays alive) | yes |
| Sidebar reload | yes | sidebar socket rebound on attach | yes |
| Multiplexer server crash | yes | no | no |
| Host reboot | yes | no | no — needs a host supervisor (tmux-resurrect, Zellij resurrect, systemd) |
| Host power cut | yes — through the last group `fdatasync`; the trailing window can be lost, and repair truncates any torn suffix (see [write classes](#write-classes)) | no | no — needs a host supervisor |

Rimz guarantees the ledger across all of these: at a power cut, through the last group sync, with repair bounding the damage to the final window. The session and its processes survive only what the multiplexer server and the host supervisor keep alive.

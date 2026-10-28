# The store

> The store is `crates/rimz/src/store/`, and this page maps it: the files on disk, the event log, hook ingress, the write and read paths, session death, and the maintenance that keeps it all bounded. The sidebar's runtime caches and how a renderer consumes the store are [state.md](./sidebar/state.md). What these mechanisms cost is [performance.md](./performance.md), and the product commitments behind them are [DESIGN.md](../../DESIGN.md).

Every durable fact RimZ knows about a room lives in one directory of flat files, with no database or permanent daemon. Short-lived CLI writers and an elected, idle-exiting hook drainer serialize store mutations with `locks/workspace.lock`. Hooks stage their raw payloads under a separate lock, so a hook never waits on a fold, and the drainer later applies them ([Hook ingress](#hook-ingress)). Readers, every sidebar renderer included, fold the event log back into state without taking a lock.

## Truth and cache

**`log/events.log.jsonl` and the durable record files are the truth. Derived snapshots are caches that any reader may rebuild from the log. Audit evidence and owned scratch are not rebuildable caches.**

A reader that finds a cache stale, corrupt, or absent folds the log itself. A writer that dies before publishing therefore costs the next reader a bounded fold, never a wrong answer. The same rule lets the sidebar be read-only on the store: it never has to write to be correct, only to be fast. The `cargo xtask invariants` check `ensure_sidebar_library_boundaries` keeps store writers out of the sidebar's import graph.

Two habits follow from the rule, and new code here keeps both.

Cache the parse, never a verdict. [`disk/parse_cache.rs`](../../crates/rimz/src/disk/parse_cache.rs) memoizes one thread's last deserialization of a JSON file under one of two keys. `ParseCache::get`, which the snapshot readers use, keys on `(path, mtime, len)`, so two republishes inside one mtime tick at equal length can serve the older parse. `ParseCache::get_stamped` adds the device and inode pair and tells those replacements apart. Every caller re-validates against live truth, so a stale serve costs a re-read and never a wrong result.

Measure freshness by log extent. A derived checkpoint records the `LogExtent` it reflects: the rotation generation and the byte offset after the last folded frame. A reader compares the stamped offset with the live log's length, without mtime's granularity or write-ordering hazards. The live log carries no generation, so an offset alone could alias into a log that was rotated or cut and then regrew. Boundary writers advance the durable generation when replacing the log, then retract and rebuild their checkpoints under the workspace and publish locks ([The write path](#the-write-path)).

Run listings follow the same habits with a per-thread, per-directory parse cache and a shared sorted slice. A listing stats the directory first. It reuses the slice, with no enumeration or per-record stat, only when two conditions hold: the full `(len, mtime, device, inode)` stamp matches the stamp read before the last scan, and the directory mtime predates that scan's start by at least two seconds. Every record write publishes a temporary sibling by atomic rename and GC deletes entries, so both change the directory stamp, and the two-second margin covers coarse filesystem timestamps. Otherwise the listing scans and stats each JSON record and reparses only a file whose stamp changed, since atomic rename gives even an equal-length, equal-timestamp replacement a new inode. Terminal status is not a freshness key, because a run can reopen.

A listing drops deleted records, ignores temporary siblings, and skips files lost to a concurrent GC. Any other read error or a corrupt record fails the listing without updating its cache. `RunListing::generation` starts at one and changes only when the record set or a file stamp changes, which gives same-thread consumers a cheap change signal.

## Where the code lives

`Store` ([`mod.rs`](../../crates/rimz/src/store/mod.rs)) is a cloneable handle around an `Arc` holding the workspace's `StatePaths` and `RuntimePaths`. `mod.rs` carries the handle, the core errors, and the lock-free reads. The mutations live under [`writer.rs`](../../crates/rimz/src/store/writer.rs) and take the workspace lock. Ingress appends take a caller-owned ingress lock, and cursor and run writes take the caller-owned workspace lock. A mutation that needs both takes the workspace lock first and the ingress lock second, never the reverse.

Four boundaries decide where new code goes:

- The store folds the agent model, so `store` imports `agents` and nothing under `agents` imports `store`.
- `store::message` owns the durable message record, its codec, and FIFO/claim selection. Delivery in `message/` imports them ([messaging.md](./harness/messaging.md)).
- `store::event` owns the persisted signal vocabulary (`SignalName`, `SignalSource`, `SignalEventPayload`). `Store::append_signal` takes that payload, and the harness converts its runtime `Signal` on the way down ([loops.md](./harness/loops.md#the-signal-vocabulary)).
- `store::run` owns the supervised-run schema, its temp-file-plus-rename codec, and the terminal wake sender. `harness::run` owns creation and the status transitions, which take the workspace lock ([scripting.md](./harness/scripting.md#the-record)). Store reset is the one place the store writes a run record itself.

| Store module | What it owns |
| --- | --- |
| [`writer.rs`](../../crates/rimz/src/store/writer.rs) | Public mutation intents and outcomes, the `commit` and `commit_boundary` primitives, and the off-lock tail. `writer/` splits the implementation into `debounce`, `lifecycle`, `publish`, `queue`, `reap`, and `reset`. |
| [`event.rs`](../../crates/rimz/src/store/event.rs) | `EventEnvelope`, the typed `EventKind` decode, `EVENT_SCHEMA_VERSION`, and the persisted signal vocabulary. |
| [`event_log.rs`](../../crates/rimz/src/store/event_log.rs) | The framed append log: `frame.rs` codec, `recovery.rs` repair, `rotation.rs` archive publication and retention. |
| [`ingress.rs`](../../crates/rimz/src/store/ingress.rs) | Raw hook frames over the event log's frame codec, the boot-aware drain cursor, repair, and locked truncate-and-reset ([Hook ingress](#hook-ingress)). |
| [`follow.rs`](../../crates/rimz/src/store/follow.rs) | The read-only lifecycle and signal follower over the event log. |
| [`snapshot/`](../../crates/rimz/src/store/snapshot/mod.rs) | The canonical snapshot schema and read side: `fold.rs` resumable rollup and carryover, `project.rs` lifecycle reducer, `assemble.rs` read entry points, then pane binding and the view-model projection ([sidebar.md](./sidebar/sidebar.md#where-the-code-lives)). |
| [`runtime.rs`](../../crates/rimz/src/store/runtime.rs) | The runtime-versus-audit read scope. |
| [`session_death.rs`](../../crates/rimz/src/store/session_death.rs) | Supersession and pidless-ghost rules shared by the durable reap and the view reap, and `GHOST_SESSION_TTL_SECS`. |
| [`live_roster.rs`](../../crates/rimz/src/store/live_roster.rs) | The `records/live-roster.json` codec for rebirth recovery. |
| [`pending_recovery.rs`](../../crates/rimz/src/store/pending_recovery.rs) | The `records/pending-recovery.json` codec: lost agents parked at a rebirth boundary until a settlement resumes or ends them. |
| [`idle_stop.rs`](../../crates/rimz/src/store/idle_stop.rs) | The `records/idle-stop.json` codec: pending `agents stop --when-idle` requests, written under the workspace lock. An absent or unreadable record reads as no requests. |
| [`hibernate.rs`](../../crates/rimz/src/store/hibernate.rs) | `records/hibernation.json`, version 1: one entry per `(kind, agent_id)` with `pane_id`, `scope`, `provider_pid`, `frozen_at`, `anon_at_freeze`, `step`, `wake_error`, and cumulative `reclaimed_bytes`. Absent numeric fields read as zero and an absent `wake_error` as none. Reads are lock-free, and a missing, unreadable, or unknown-version record reads empty. Replacement and withdrawal take the workspace lock and publish durably, and reset deletes the record. The [hibernation helper](./harness/hibernate.md) records confirmed freezes and reclaim steps. |
| [`message.rs`](../../crates/rimz/src/store/message.rs), [`message/codec.rs`](../../crates/rimz/src/store/message/codec.rs) | The durable message record, FIFO/claim/batch selection, and the live-queue and history codec. |
| [`run.rs`](../../crates/rimz/src/store/run.rs) | `RunRecord` and `RunStatus`, the durable codec, and `wake_run`. |
| [`workflow.rs`](../../crates/rimz/src/store/workflow.rs) | Workflow record and append-only journal codecs. The harness owns the locked transitions ([workflow.md](./harness/workflow.md)). |
| [`sidecar.rs`](../../crates/rimz/src/store/sidecar.rs) | The stat-gated latest-wins sidecar store behind [`agent_context.rs`](../../crates/rimz/src/store/agent_context.rs) and [`subagent_context.rs`](../../crates/rimz/src/store/subagent_context.rs). |
| [`active_time.rs`](../../crates/rimz/src/store/active_time.rs) | The per-session estimated active-time accumulator, serialized by per-record flocks. |
| [`gc.rs`](../../crates/rimz/src/store/gc.rs) | Room and machine maintenance: `gc/classes.rs` the per-class state sweeps, `gc/collect.rs` runtime hints, `gc/tmp.rs` the temp-unit inventory and owner holds, `gc/temp_sweep.rs` orphan write temps, `gc/prune.rs` machine-only dead workspaces. |

The file primitives sit one level down, in `disk/`, which imports nothing from the store.

| Disk module | What it owns |
| --- | --- |
| [`disk/paths.rs`](../../crates/rimz/src/disk/paths.rs) | `StatePaths` and `RuntimePaths`: the store-owned filenames on this page, the XDG resolution, the layout check, and the private runtime-directory check. |
| [`disk/atomic.rs`](../../crates/rimz/src/disk/atomic.rs) | Whole-file publication and sync discipline, cache-local temp cleanup, and the cache-class read (missing or corrupt reads as default). Every fsync call is in this file ([write classes](#write-classes)). |
| [`disk/lock.rs`](../../crates/rimz/src/disk/lock.rs) | `WorkspaceLock`, the advisory flock, bounded by `LOCK_TIMEOUT` (30 seconds). The timeout error names `fuser` to find the holder. `SharedAppendLock` is the shared, blocking guard for ingress appends. |
| [`disk/lease.rs`](../../crates/rimz/src/disk/lease.rs) | Single-flight leases (the loop watch and run locks, and wrapper hook-drainer outage records). Claim, probe, and GC removal each run under the directory's `lease-gate.lock`, held for one attempt and its body write, read, or unlink, so a probe or a sweep never defeats a claim. The gate wait is bounded at five seconds, and a timeout is an error. |
| [`disk/retention.rs`](../../crates/rimz/src/disk/retention.rs) | The retention figures that state writers and the class sweeps share: archive retention, the rotation threshold, the owned and temp-unit graces, the audit age and size caps, and the transcript bucket width. |
| [`disk/buckets.rs`](../../crates/rimz/src/disk/buckets.rs) | Date-bucket file names and listing for the append-only `audit/transcript/` and `audit/messages/` logs. |
| [`disk/rotating.rs`](../../crates/rimz/src/disk/rotating.rs) | Best-effort rotating JSONL append and decoded visits. |
| [`disk/parse_cache.rs`](../../crates/rimz/src/disk/parse_cache.rs) | The per-thread parse memo ([Truth and cache](#truth-and-cache)). |
| [`disk/single_flight.rs`](../../crates/rimz/src/disk/single_flight.rs) | Cross-process cache election: shared reuse, exclusive production, or local-only fallback. Sidebar heavy lanes and launch gating use it, and host-owned pane production does not. It stays free of every writer module, and callers own freshness and publication. |
| [`disk/usage.rs`](../../crates/rimz/src/disk/usage.rs), [`disk/summary.rs`](../../crates/rimz/src/disk/summary.rs) | Symlink-safe, hardlink-aware byte walks for `doctor --audit`, `uninstall`, and `gc`, and text-file measurement. |

Start at `writer.rs` for how a fact gets in, at `snapshot/fold.rs` for how it comes back out, and at `disk/atomic.rs` for what durability costs.

## What is on disk

Room files have one lifetime class per directory, and only `disk/paths.rs` constructs their paths. Each path is built under its class directory, never straight off a room root, and `cargo xtask invariants` rejects a `self.root.join(` there outside tests. State survives reboot, and runtime does not. The root `workspace.json` and `rimz` executable are room identity, outside the classes. The figures below come from `disk/retention.rs`.

| Class directory | Tier | Reclamation rule |
| --- | --- | --- |
| `log/` | State | Event log: rotate at 64 MiB, with 14-day retention for archives and carryover. Hook ingress: truncate only after it is fully drained, with no age sweep or rotation. |
| `records/` | State | Standing records, with no age sweep. |
| `audit/` | State | Except `transcript/` and `messages/`, remove files older than 30 days by mtime, then oldest first until at most 64 MiB remains per room. Those two subdirectories follow `gc.transcript_retention` (default `forever`) and report together as `transcript`. |
| `cache/` | State | Rebuildable. Cleared on reset, with no age sweep. |
| `owned/` | State | Agent unit: 7 days after the latest session using its handle ends, never while its process owner is live or its directory or a direct child is newer than 7 days. Unknown handles use the same mtime grace. Runs: terminal and record mtime older than 7 days. Workflow homes: see [Garbage collection](#garbage-collection). Restorability does not extend the grace. |
| `tmp/` | State | Agent temp unit `tmp/<handle>/`: remove after `gc.tmp_grace` (default 1 day) has passed both its newest owner's end and the mtime of the directory and each direct child. Owners include launched children sharing the parent's unit, but not provider-native subagents. Any live process owner, or any owner without an end, keeps the unit. Unknown handles use the same mtime grace. [Pressure eviction](../reference/cli/maintenance.md#automatic-sweeps) bypasses the grace for ended and rowless units, never for holds. An error skips one unit, not the class. `tmp/_unnamed/`, shared by handleless launches, stays until the room directory is pruned. |
| `<home>/empty/<workspace-dir>/` | State (outside the room store) | Keep each agent directory while any retained row records it as `worktree_path`, even after that session ends. Otherwise remove it once its directory mtime is older than the 7-day owned grace. Keep the room-level directory while its store exists. Machine GC sweeps the trees of removed stores under the same per-agent grace, then removes an empty room-level directory. Unknown entries and symlink targets are untouched. |
| `shared/` | State | Room-wide files agents hand each other, with no sweep. Hard reset removes it. Otherwise it goes when the room directory is pruned. |
| `team-notes/` | State | Per-run team archives. `gc.team_notes_retention` defaults to `forever`. A duration removes a whole run only after its newest recursive entry mtime expires, without following symlinks. Soft reset keeps it, while hard reset and whole-room deletion remove it. |
| `out/` | State | RimZ-written results under `out/<reader>/`. `out/` and each reader directory are created at mode `0700`. A response file goes once it is older than 7 days and no run record names its agent (the stem before the first `.`). A `wait-` file goes once it is older than 7 days and its watcher is not live. An empty reader directory goes. |
| `locks/` | State | Try-lock each file and unlink it while held under its directory's gate. Keep the `lease-gate.lock` itself, busy files (including shared supervisor holds on `room.lock`), and every subdirectory, since a reopening waiter recreates only the file. Dry runs count files as would-check, existing gates included, without locking. Every acquirer checks descriptor and path inode identity after the flock and retries on replacement. Reset and teardown never remove this class. |
| `sock/` | Runtime | Remove sockets whose connect probe is refused, after the sidebar heartbeat TTL startup grace. |
| `live/` | Runtime | Mtime TTL (`gc.older_than`, default 7 days). Renderer-instance claims also expire by heartbeat liveness. Two exceptions: agent telemetry stays while its room is live, because the external exporter holds its inode open, and `hibernate/admission.json` stays until room teardown, since only birth republishes it. |
| `lanes/` | Runtime | No age sweep. Reset, teardown, room death, and reboot reclaim this coordination state, including quiet schedules and user choices. |
| `snapshots/` | Runtime | Rebuildable store checkpoints and publish pacing. No age sweep. Soft reset and teardown keep the class; hard reset drops its checkpoint files. Whole-room runtime removal reclaims it. Only runtime setup creates the directory; checkpoint writers skip a missing directory. |

### The workspace store

The workspace store is `<home>/ws/<workspace-dir>/`, where `<home>` is `$RIMZ_HOME` or `~/.rimz`. It holds durable truth plus the caches derived from it that should survive a reboot.

The workspace id is `ws_` plus the first 24 hex characters of the SHA-256 of the canonical project root (`WorkspaceId::from_project_root`). Every root class (repo, marker, bare directory) derives it the same way, so adding a class never re-keys an existing store.

The directory is named by a `WorkspaceDirName`, never by the id. The name is `<basename>-<hex>`: the root's sanitized basename plus a hex prefix of the id, four digits long unless another workspace already holds that prefix, then two more digits at a time. `StatePaths::for_project_root` finds an existing directory or mints the name, and creates nothing. Lookup scans `ws/` for names whose hex part is a prefix of the id, and a candidate's `workspace.json` decides, so a record naming another id is skipped. Only the root lookup adopts an unrecorded directory, matching the root's slug or the full fallback name, and several such candidates are an error. A site that holds only an id (`StatePaths::for_workspace`) resolves to the directory whose record names the id, otherwise to `ws-<24hex>`, and never adopts an unrecorded directory. The runtime directory uses the same name (`RuntimePaths::for_state`), so both trees agree.

`Store::open` creates the state and runtime trees, and only room-choosing flows call it. Participants, writers included, take the non-creating handle from `Store::open_existing`: a room exists only when its state root holds a readable `workspace.json`. A missing, corrupt, or incompatible record returns `None`, and a directory holding only locks is not a room. In the CLI, `open_store` creates the trees and records identity, while `open_existing_store` and `Ctx` never create directories or re-record it. Identity-only and read-only maintenance commands can resolve a workspace without a room.

```text
log/events.log.jsonl                          framed event log
log/hook-ingress.log.jsonl                    pending raw hook frames
log/hook-drainer.log                          drainer diagnostics
log/archive/events.<uuidv7>.jsonl             rotated logs
records/hook-drain.json                       applied/claimed cursor with boot-aware write classes, frame ids, claim extent
records/agents-carryover.json                 agent rollup carried across rotation
records/log-generation.json                  durable active-log generation
records/messages/messages.jsonl               live message queue
records/loop-instances.json                   loop and wait rows
records/loop-launches.json                    resident loop launches by task and checkout
records/loop-workflow-launches.json           condition workflow launches by task and checkout, version 1
records/boot.json                             last host boot id
records/live-roster.json                      producer's last pane-backed live agent set
records/pending-recovery.json                 lost agents awaiting the user's recovery decision
records/idle-stop.json                        pending `agents stop --when-idle` requests, one per session
records/hibernation.json                      frozen agents and their reclaim steps
records/last-death.json                       last incident
records/budget.fleet.json                     standing fleet override, raise, or disable
records/lsp-usage.jsonl                       counted language-server query history
audit/transcript/<bucket-start>.jsonl         chat transcript, 7-day buckets
audit/messages/<bucket-start>.jsonl           terminal messages, same bucket rule
audit/crashes/<utc-ts>/                       mux forensics
audit/diag.log.jsonl, audit/diag-frames/      anomalies and captured frames
audit/notify.log.jsonl                        notification diagnostics
audit/plugin-presence.log.jsonl               presence diagnostics
audit/binding.log.jsonl                       binding diagnostics
team-notes/<run>/                             team board, notes, and run.json manifest
cache/doctor-cleared.json                     cleared-incident watermark
cache/auto-gc.json                            daily sweep stamp
cache/{log-sync,dead-reap}.stamp              durable-work debounce
cache/auto-rotate.stamp                       rotation debounce
cache/skills/<sha256>/                        immutable rewritten skills, shared by the room
cache/ssh-config/<sha256>/                    shared system SSH Include-graph snapshots
owned/agents/<handle>/scratch/                per-handle scratch
owned/runs/<run_id>.json                      supervised-run records
owned/workflows/<run-id>/                     workflow record, sealed spec, journal and work directory
tmp/<handle>/, tmp/_unnamed/                  per-agent temp units
shared/                                       room-wide shared files
out/<reader>/<name>[.<n>|.<run_id>].output    child and peer responses
out/<reader>/<wait-name>.output               watched-command output
locks/*.lock                                  room, workspace, publish, subagent-zone, loop-instances,
                                              loop-run-<name>[-<checkout id>], loop-watch-<name>, lease-gate,
                                              message-sweep, sidebar-launch, snapshot,
                                              topology-writer, authoritative-pane-probe, focus-anchor,
                                              pr-state, diff-stats, budget.fleet, hook-drainer,
                                              hook-drainer-spawn, hook-ingress, lsp-usage, and sidecar locks
workspace.json                                room record, layout: 2
rimz                                          stable room executable
```

This page owns the log, the caches derived from it, hook ingress, and the workspace record. The other files have their own homes:

| Files | Owner |
| --- | --- |
| `records/messages/`, `audit/messages/` | [messaging.md](./harness/messaging.md#storage-and-audit) |
| `audit/transcript/` | [transcript.md](./harness/transcript.md#the-log) |
| `owned/runs/` | [scripting.md](./harness/scripting.md#the-record) |
| `owned/workflows/` | [workflow.md](./harness/workflow.md#storage-and-isolation-boundary) |
| `records/loop-instances.json`, `out/<reader>/wait-*.output` | [loops.md](./harness/loops.md#where-tasks-live) |
| `records/loop-launches.json`, `records/loop-workflow-launches.json` | [loops.md](./harness/loops.md#the-launch-ledger) |
| `records/idle-stop.json` | [loops.md](./harness/loops.md#recovery-the-host-runs) |
| `records/hibernation.json` | [hibernate.md](./harness/hibernate.md) |
| `records/lsp-usage.jsonl` | [lsp.md](./lsp.md#query-usage) |
| `tmp/`, `shared/`, `owned/agents/` | [sandbox.md](./sandbox.md#temp-units) |
| Audit diagnostics | [diagnostics.md](./diagnostics.md) |
| Rebirth records and crash archives | [Session death](#session-death) below |

### The workspace record

`workspace.json` is the index maintenance commands read after a project root moves or vanishes ([`workspace/record.rs`](../../crates/rimz/src/workspace/record.rs), `WorkspaceRecord`). It records the project root and its class, the active worktree root, the mux session name, the executable the room serves, and the room's provider accounts. Only room creators re-record identity, and a participant open leaves its bytes and modification time unchanged. Explicit owner and account updates write their own fields and preserve the others.

Every writer stamps `layout: 2`, and an absent stamp reads as layout 1. There is no upgrade from one layout to the next: an incompatible layout requires room replacement. `rimz start` and `rimz reset` tear down a layout-1 room and replace it with a fresh room, without carrying over history. While another running room holds its `room.lock`, both refuse with the held-room error. `Store::open` refuses a layout-1 room and names those two commands as the fix. Record scanners skip it, and machine GC prunes it with its runtime tree ([Garbage collection](#garbage-collection)).

The executable is a `(rimz_bin, rimz_build)` path-and-digest pair. Only `rimz start`, cwd-based `rimz attach`, and `rimz reload` set it, so a CLI call from another worktree cannot retarget a live room. Build promotion re-digests the target before executing it, so a missing or altered file leaves the running process serving. Staging and promotion belong to reload ([sidebar.md → Build promotion](./sidebar/sidebar.md#build-promotion)).

`session_name` is the session the room was last born under. Resolution reads it from the record and falls back to the state directory name when the record is absent or unreadable. Creator resolution warns on a read error, while participants report the absent room at the store boundary. State-path errors propagate. Scanning may canonicalize `project_root` but never rewrites the session name. [`RoomContext::birth`](../../crates/rimz/src/room/birth.rs) joins a live recorded session unchanged. Otherwise it chooses the state directory name and records it through `Store::record_workspace`, preserving the owner and account fields. A failed session-list probe keeps the recorded name and the non-destructive birth policy.

`skipped` carries the latest room-entry configuration judgment as an ordered array of `{setting, message}`. It defaults to empty on read and is omitted when empty. The locked `Store::set_skipped_settings` writer replaces the entire list. Generic and owner re-records preserve it, `reset_records` clears it, and `reset_records_keeping_logins` preserves it. Sidebar and doctor readers use the record path without opening a writer.

`remote_control_off` is an array of agent kinds skipped at room birth. It is omitted when empty and decodes as empty when absent. `Store::set_room_birth_settings` writes it at every stopped birth, in the same locked commit as `agent_shell`. Generic and owner re-records preserve it, `rimz reset` clears it, and `reset_records_keeping_logins` keeps it. Every per-room reader of remote-control enablement applies it through `crate::remote_control::room_config`.

`agent_shell` is the optional `{name, path}` lookup for a bare `[agents] shell` name. A new session's birth resolves it once on that process's PATH and writes the absolute hit as found, without canonicalizing, before creating the session or resuming panes. A scheduled loop birth (`rimz loop run`, including a hand-run invocation) instead clears the field even when lookup succeeds; a manual `rimz loop fire` still saves it. Stopped-room recovery planning and judgment still use that same pending lookup rather than the previous life's record. Unset, absolute, failed, or non-UTF-8 birth lookups clear it; an absent field decodes as no lookup and is omitted on write. Live re-entry, reload, and generic or owner re-records preserve it. `rimz reset` clears it, while `reset_records_keeping_logins` keeps it. A launch with no matching saved name, or whose saved path is no longer a file, falls back to its own PATH.

#### Account pins

`pins` is a `<kind>: <name>` map of explicit account pins for future launches, omitted when empty. An absent kind inherits live: the trusted project's selection, then the machine's `[accounts.use]`, then `default`, through `crates/rimz/src/agents/login.rs::room_accounts`. A record that carries `logins` and no `pins` reads its named entries as pins and its `default` entries as inheriting. An explicit `pins` map wins, including a pin to `default`. Writers emit only `pins`. An unreadable record is an error, never an empty pin set.

Three writers change pins, all under the workspace lock, and generic record writes preserve them:

- Birth writes requested pins only through `crates/rimz/src/store/writer.rs::Store::pin_room_logins`. A differing existing pin refuses the whole write with `RoomLoginsFrozen`, which directs the user to the explicit switch.
- `Store::switch_room_login` inserts a pin.
- `Store::unpin_room_login` removes one and returns the prior pin, or `None` for an inheriting kind.

`rimz reset` clears pins, and `reset_records_keeping_logins` preserves them. Switching emits no wakeup: the next sidebar heavy pass reads the new account.

Each launch carries an account stamp. A launch batch resolves `LaunchLogin::RoomDefault` live under the workspace lock and stamps each identity, or preserves an explicitly pinned launch name. Only an error for a requested kind fails the batch. Exec resolves the stamp and never rereads the room's account. Reopening a session requires its stamp and the room's current account to share a history pool, and the reopened session is stamped with the current account. Ingress inherits a known parent's stamp, preserves an adopted batch stamp, and resolves live only for hook-first roots. A refusal for that kind warns and stamps `None`, which is native `default`.

### The per-room runtime tier

The runtime tier is `${XDG_RUNTIME_DIR}/rimz/ws/<workspace-dir>/`, or `/tmp/rimz-<uid>/rimz/ws/<workspace-dir>/` when `XDG_RUNTIME_DIR` is unset. Everything here is disposable: it speeds the next read and dies with the session. The store's own entries are:

```text
sock/run.<short_run_id>.sock                  per-run wakeup socket, bound by a supervised-run waiter
sock/host.<digest>.wake.sock                  room event socket, bound by each sidebar host
sock/codex-app-server.sock                    the per-session Codex broker socket
snapshots/{latest,rollup}.json                 view-model checkpoint and fold base
snapshots/publish.stamp                       checkpoint publish pacing
live/heartbeat/sidebar.*.json                 renderer liveness
live/read-marks/marks.json                    room read marks
live/{agent_context,subagent_context}/        enrichment
live/agent-activity/                          activity hints
live/active-time/                             active-time accumulators
live/prompt/{task,sys}.<digest>.md            launch artifacts
live/agent-telemetry/copilot-otel.jsonl       metadata-only Copilot export
live/idle-compact/<digest>.json               idle-compaction fire records
live/idle-stop/<digest>.json                  idle-stop helper pacing records
live/presence.stamp                           mux/presence liveness hint
lanes/{snapshot,agent-projection}.json        projections
lanes/{sidebar-width,sidebar-filter,unread}.json renderer state
lanes/{pane-topology,presence-desired,topology-writer-conflict}.json topology
lanes/{authoritative-pane-probe,focus-anchor}.json focus and pane truth
lanes/{group-roots,diff-stats,cohort-spend,pipeline,pr-state,metrics-sample}.json enrichment
lanes/{loop-fire,message-wake,codex-daemon-reap,link-stats}.json coordination
lanes/{budget.scopes,budget.fleet-park,budget.<digest>,auto-continue.<digest>}.json session state
lanes/workspace-spending.<prefix>.json        room spend projection
lanes/hook-drainer-outages/<pid>              wrapper-held hook-drainer outage leases
```

Other subsystems publish their own latency files into the same directory, and each catalog belongs to the module that writes it:

| Files | Owner |
| --- | --- |
| Loop fire lane and run/watch locks | [loops.md](./harness/loops.md) |
| Budget records and lanes | [budget.md](./harness/budget.md#the-ledgers-on-disk) |
| Auto-continue lanes | [providers.md](./agents/providers.md#auto-continue) |
| Message wake lane and sweep lock | [messaging.md](./harness/messaging.md) |
| Topology lanes | [multiplexers.md](./multiplexers.md) |
| Sidebar lanes | [state.md](./sidebar/state.md#published-lanes) |
| Agent telemetry | [adapter_copilot.md](./agents/adapter_copilot.md) |

Liveness hints live apart from the store because `AF_UNIX` socket paths are short. `AF_UNIX_PATH_LIMIT` is 108 bytes on Linux and 104 on macOS, terminator included, and a path under the deep state tree would overrun it. The sockets set the location, and the heartbeats and receipts follow them. `RuntimePaths::for_workspace` validates the socket budget before any session side effect.

The runtime root is private. `ensure_private_runtime_dir` refuses a symlink, a foreign owner, or group and other permissions it cannot strip, and that check keeps `agent-telemetry/` private through its ancestry.

Freshness here gates behaviour, so these files are scoped to one mux session incarnation. When a birth proves the previous session absent, it purges sidebar heartbeats before creating the replacement. A fresh-looking heartbeat from a dead renderer therefore cannot steer launch or reconcile decisions in the reborn room.

### Account-global caches

Account-global provider caches live under `~/.rimz/cache/providers/`: `accounts.json`, `rate_limits.json`, `credits.json`, `provider-spending.json`, `spending.json`, and `pricing-cache.json`. They persist so the provider dashboard opens warm after a reboot, and they rebuild from the providers' own files (a re-probe, or one full spending walk for the cursor). Their election locks and the spending service socket live under `$XDG_RUNTIME_DIR/rimz/shared/`, because a lock means nothing once its holder is gone. What each file carries is [state.md → Published lanes](./sidebar/state.md#published-lanes), [providers.md](./agents/providers.md), and [spending.md](./agents/spending.md).

The same two directories hold ledgers that are not rebuildable caches. Persistent `cache/providers/` also holds `budget.account.<kind>@<account>.json` standing account choices and park state, and the `auto_redeem.<key>.json` and `auto_redeem_rate.<key>.json` redemption state. Runtime `shared/` also holds `rate_limits_trace.jsonl`, provider probe markers, `board-write/` locks, account budget locks, and auto-redeem election locks. `rimz gc` removes an account budget ledger that no history pool reads ([Garbage collection](#garbage-collection)). These paths sit outside the room classes. The room-scoped spend projection is `lanes/workspace-spending.<prefix>.json`.

## The event log

`log/events.log.jsonl` is the canonical history of everything that happened in the workspace.

### Framing

Each record is one newline-terminated line: `<payload length> <crc32, 8 lowercase hex> <json payload>`. The CRC covers the payload alone, and the reader checks the length against the payload it finds ([`event_log/frame.rs`](../../crates/rimz/src/store/event_log/frame.rs), `decode_line`). The length prefix is mandatory, while the CRC is optional: a `<payload length> <json payload>` line also decodes, because a payload always opens with `{` and cannot be mistaken for the hex token. The prefix also means a bare `jq` over the file fails, so strip it first to read frames by hand:

```sh
sed -E 's/^[0-9]+ [0-9a-f]{8} //' log/events.log.jsonl | jq 'select(.method == "agent.lifecycle")'
```

A full envelope is long. To follow one agent, run, or message through the log, match its id and project the three fields that say what happened and when:

```sh
rg -a <id> log/events.log.jsonl | sed -E 's/^[0-9]+ [0-9a-f]{8} //' | jq -r '[.timestamp, .method, .params.event_name // ""] | @tsv'
```

The payload is an `EventEnvelope`: schema version, event id, workspace id, session name, mux name, source and source kind, method, timestamp, and a method-specific `params` blob kept as raw JSON, so a reducer parses only the events it folds.

### What is in it

`method` is the discriminator, and [`EventKind`](../../crates/rimz/src/store/event.rs) is its typed decode.

| Method | Carries | Folded into |
| --- | --- | --- |
| `agent.lifecycle` | One lifecycle signal plus the observation around it: session id, pane stamp, status, turn, context, tokens, subagents. | The agent rollup ([model.md](./agents/model.md)). |
| `agent.attached` | Resume identity and placement: provider session id, stable RimZ launch id, pane stamp, runtime owner, isolation, the process `login` (`default` spelled out), and optional launch `record`, `tier`, and `mode`. A spawning wrapper writes two: its own owner before the spawn, the provider's pid after it. | Re-stamps identity, placement, and present isolation. A present `login` replaces the row's account stamp (`default` clears it), and an event without the field leaves the stamp alone. With a record, replaces record and tier (an absent tier clears it) and sets a present mode. Without a record, it leaves all three unchanged. An identified attach for an unseen session seeds its row before the provider starts. |
| `agent.launch_warnings` | Warnings the exec wrapper printed for one launch, keyed by kind and agent id as `agent.attached` is, with the launch id when known. | Replaces the existing row's `launch_warnings`, and an empty list clears it. Never creates a row. |
| `agent.launched` | The launch RimZ performed: identity, profile, role, team, worktree, permission mode, optional `record` (verbatim model, effort, agent base), optional `tier` stamp (requested tier, listed model, upward `used_tier`, usage `skipped` reasons, and `unbuildable` entries with `model` and full build `error`), and a `Starting`, `Bound`, or `Failed` state. | Launch admission and resume posture. Record and tier survive model observations and hot lifecycle events. |
| `message.*` | One of twelve message outcomes: `queued`, `edited`, `after_met`, `when_met`, `sent`, `delivered`, `timed_out`, `errored`, `canceled`, `abandoned`, `expired`, `archived`. `message.removed` decodes as `canceled`. | The message audit trail ([messaging.md](./harness/messaging.md)). |
| `signal.emit` | One `SignalEventPayload`: name, top-level payload object, and source (`cli`, `watch`, `lifecycle`, `forge`, `git`, `worktree`, or `team`). | Nothing in the rollup. It is the durable trace of an event ingress, replayed by `rimz events follow` ([loops.md](./harness/loops.md#the-signal-vocabulary)). |
| `session.rebirth` | Nothing: it is a boundary marker. | Clears every pane stamp recorded before it. |
| `session.death` | `cause` (`reboot` or `crash`) and the agents lost with the previous incarnation. | [Session death](#session-death). |
| anything else | Raw params: `feed.*` and `event.emit` frames, methods from a newer binary, and any known method whose params fail to decode. | Nothing, by design. |

The catch-all `Other` arm is what makes a downgrade safe. A record written by a newer RimZ decodes as `Other` and survives every fold, so an older binary reads an intact log instead of a corrupt one. The price is that a params type grows only by optional, defaulted fields: a new enum variant or a required field turns the whole event into `Other` for an older binary, which then drops it from every fold. Worktrees of a project share one workspace store, so mixed binaries are the ordinary case. The tier stamp shows the rule: `unbuildable` is an additive list, empty and omitted when unused and ignored by older readers, where a new member of the closed `skipped` reason enum would make an older reader drop the whole launch or attach event. Both lists describe passes ahead of the chosen entry, never later static exclusions.

Message records get the same tolerance outside the log. A record's `sender` can carry a `Harness` notice that a newer binary minted, and `HarnessNotice::Other` keeps that string verbatim, so an older binary's queue rewrites and history retention pass it through. Without it, one such record in `audit/messages/<bucket-start>.jsonl` would fail every history-touching command of the older binary (`message cancel`, `message list`, retention), and one in `records/messages/messages.jsonl` would fail every queue transaction. Nothing dispatches on an unknown notice: it takes generic harness delivery and renders its own name as the message `Type` ([messaging.md → The message header](./harness/messaging.md#the-message-header)).

`session.rebirth` clears pane stamps and never ends a session. A reborn mux session renumbers panes from zero, so every stamp recorded before the boundary names a pane that no longer exists, and clearing them keeps a prior incarnation's session off a reused pane id. Each resumed wrapper then appends `agent.attached` to re-establish its launch identity, placement, and owning process. An attach event without a launch identity cannot create a row.

### Fields the fold carries forward

Lifecycle records inherit from the rollup instead of repeating themselves. High-cadence progress events omit `transcript_path`, worktree, pane identity, role, team, channel, profile, and the smart-compact stamp, and the reducer carries each forward from the prior row. Missing optional keys decode as absent. `runtime_owner` is serialized on a lifecycle record when present, and the reducer carries a prior owner forward when it is absent. That keeps the hot log compact under a busy fleet.

Some row fields come from launch parameters or the workspace record rather than from lifecycle events. Each of them is a field the persisted snapshot and the rollup cache carry, so adding one bumps `SNAPSHOT_VERSION` (`store/snapshot/view.rs`) and `ROLLUP_CACHE_VERSION` (`store/snapshot/fold.rs`), and caches written before it rebuild instead of hiding it.

- `AgentState.root_lane` derives from the workspace record: a row is on the root lane when its channel is unstamped and its worktree path equals the project root. Bound `PaneAgent` rows copy it, and sessionless panes derive it from the snapshot's project root. This display fact never comes from a lifecycle event and does not change routing channel values.
- `LaunchParams.checkout` carries an empty-dir launch's original checkout, projected to `AgentState.checkout` and `SidebarRow.checkout`. Lane routing, root-lane stamps, and sidebar groups use the checkout before the process `worktree_path`, so a hook that updates the cwd does not lose the lane. A record without `checkout` keeps the plain worktree-path semantics.
- `LaunchParams.loop_task` is stamped on a resident loop launch's prompt leader before its pane opens. The reducer carries it into `AgentState.loop_task` through provisional adoption, later observations, and event-log rotation, as it does the team identity. Registration reads that durable task name to arm subscriptions, never the launch ledger.
- `LaunchParams.molted_from` holds a molt successor's predecessor launch id (the session id for a row without one). The fold carries it onto the registered session and through sparse observations and session rotation.

Molt succession is the one carried field with a rule attached. `agents::state::standing_successor` recognizes an unended successor unless it is `Failed` with a still-provisional id: a startup failure yields the place back, but a registered session's failed turn does not. In both scopes, the runtime projection stamps `AgentState.molt_superseded` over the whole retained rollup before dropping ended rows, so a standing successor farther down the chain still supersedes its predecessor. The stamp also precedes the snapshot's reap and the live read's drops, so a successor reaped while still unended keeps its predecessor superseded until its durable end stamp lands. The stamp overwrites any incoming flag, and the fold never sets it.

`Store::begin_agent_launch_batch` checks that predicate under the workspace lock, against the caught-up durable fold, before it allocates or appends anything. A standing successor refuses another molt reservation and names its handle and stop command. A failed startup frees the slot through the same predicate, with no separate release record or CLI precheck. Stopping a pane-less molt reservation appends `Failed` under the same lock, and `Store::bind_agent_launch` rejects a late binding for a failed or ended provisional molt, and the wrapper refuses to start the provider when binding fails. When a binding wins the race with stop, stop refreshes the row and closes the bound pane instead. Ordinary launches keep their own binding policy.

Rebirth and rotation keep handles stable. Every fold on a thread shares one unstamped carryover base per file identity, and the fold stamps `root_lane` onto its rows once per workspace identity instead of copying them on every fold. At a rebirth it hydrates continuing carried rows into the active fold: observed live rows take the low ordinals, and continuing rows fit around them. Ended carried-only rows keep their names but have no pane or ordinal, and an absent ordinal is omitted on disk, never written as null. Backfilled identities the reducer has not assigned stay provisional in the rollup cache, so a later delta produces the same handles as one cold fold, even when the prefix updated only warnings. Rotation commits the projected ordinal high-water mark for the next generation.

### Crash recovery

Only the trailing frame can be in flight when a process dies. The workspace lock serializes appenders, and each append hands the kernel one encoded buffer through `write_all` (`crates/rimz/src/disk/atomic.rs::append_record_bytes`), so a crash or a short write can leave at most a partial last frame.

A reader that hits an unterminated row has met end of input, so it stops in front of that row even if the rest of the append lands during the read. It reports an extent that does not claim those bytes and logs the stop at debug, and the next wakeup covers that in-flight append. Only a terminated undecodable frame is judged by what follows it. A power cut leaves such a frame; an in-flight append never does, since its partial frame lacks the closing `\n`. With no bytes behind it, it is a torn tail, logged at warn and skipped, and the read stops in front of it too.

A terminated bad frame followed by more bytes is real corruption, and the read fails loudly instead of silently dropping everything after it. [`repair`](../../crates/rimz/src/store/event_log/recovery.rs) is the deliberate recovery: it truncates from the first invalid frame to end of file and reports the frames kept and bytes cut. The publish tail calls it when its fold hits corruption, and `rimz gc` calls it on demand.

Lock-free `O_APPEND` would remove the lock cost but let writeback reorder and tear a frame in the middle of the file, where repair can only cut from that frame onward and lose every good record behind it. [performance.md → Deferred and rejected](./performance.md#deferred-and-rejected) records why that trade stays rejected.

### Rotation and carryover

Rotation syncs the active log, renames it to `log/archive/events.<uuidv7>.jsonl`, syncs the directory, and lets the next append start a fresh log. UUIDv7 names sort chronologically, so the archive needs no index ([`event_log/rotation.rs`](../../crates/rimz/src/store/event_log/rotation.rs)).

Rotation starts two ways. A lifecycle append that leaves the log at or above `DEFAULT_EVENT_LOG_ROTATE_BYTES` (64 MiB) claims rotation inside the write lock, debounced by `cache/auto-rotate.stamp` (`AUTO_ROTATE_DEBOUNCE`, 60 seconds), and the lifecycle CLI then spawns a detached `rimz workspace rotate-events`. The same command is the manual entry point: `--max-bytes` overrides the threshold, and `--archive-older-than` prunes archives past `DEFAULT_RETENTION` (14 days).

Rotation never changes what the audit rollup remembers. Before the rename, `Store::rotate_event_log` merges every agent in the rotating log's audit rollup into `records/agents-carryover.json`, ended sessions and exited owners included. It prunes carryover rows older than the retention window, then advances the durable generation and rebuilds the fold base. The first post-rotation event for a continuing agent reduces against its carryover row. The event schema's lifetime-field policy therefore keeps launch identity, parentage, and enrichment exactly as if the log had not rotated, and `last_seen` resolves across carried and live rows.

Staging folds against a fresh read of the durable carryover, using the ordinary backfilled merge base and the same ended-row text rule as every other fold. It writes ended rows with bounded prompt labels and without recent-prompt history or pane-command text ([The read path](#the-read-path)). Continuing rows stay whole.

## Hook ingress

`log/hook-ingress.log.jsonl` is durable pending work, not input to the snapshot fold. A hook appends its raw payload there and returns, and the elected drainer applies the frames to the event log and the durable records later. The drainer's process model (election, apply children, deadlines) is [adapter.md](./agents/adapter.md). This section owns the file, its cursor, and their durability.

Appenders share `locks/hook-ingress.lock` through `crates/rimz/src/disk/lock.rs::SharedAppendLock`, blocking in the kernel rather than polling. Each encoded frame goes to an `O_APPEND` file as one buffer, over the event log's frame codec. The ingress lock is independent of the workspace lock, so neither lifecycle folds nor other appenders delay a hook's lock acquisition.

Linux frames carry an optional `boot_id` from the cached native boot identity. When both boot ids are known, each maximal consecutive run of frames from other boots within one read is dropped before application or recovery. Under the workspace lock, after ownership validation, the drainer emits one normally rate-limited `hook_ingress_dropped` diagnostic with `dropped`, `first_ingress` and `last_ingress`, then makes one cache-class cursor publication with both offsets and ids at the run's last frame and responds once. A run spanning reads gets one record per read; a crash can repeat its evidence. Pending work from any earlier boot is discarded, including after a clean reboot. An unstamped frame, or a drainer without a boot id, retains the existing application and recovery path.

Shared appenders can tear a line: a killed or short append leaves a partial frame ahead of later frames. Before applying pending ingress, the drainer takes the workspace lock and then the ingress lock exclusively for its repair read (`store::ingress::repair_and_read`). It truncates an unterminated tail and resynchronizes at the next newline after a corrupt terminated record. Good frames after corruption still apply, including a whole frame that a later appender wrote onto the same line as a torn one. The drainer advances past skipped records and leaves typed repair evidence in the [main diagnostic log](./diagnostics.md#hook-drain-evidence). It releases the ingress lock before it applies frames.

`records/hook-drain.json` is the drain cursor. It records `applied` and `claimed` byte offsets, their frame ids, and the event-log extent at claim time, for crash recovery. Cursor writes take only the workspace lock.

- An absent cursor starts at zero.
- A nonzero offset must end at a row carrying the recorded id, and skipped corrupt rows carry no id.
- An invalid offset or a mismatched id resets the cursor to zero. That covers a lost truncation reset followed by log regrowth. A malformed cursor still errors.
- Validation seeks backward to the rows ending at the two offsets, reading only those rows plus bounded boundary-search blocks, so a cursor check costs O(frame) whatever the ingress history.
- Only `applied == claimed == len` permits in-place truncation, under the workspace lock and then the exclusive ingress lock. Truncation retains its fdatasync and finishes before the cache-class cursor reset.

Claim and applied publications are durable only for unstamped frames or when the drainer cannot identify its boot. Same-boot claims and applications, corrupt-row skips, other-boot drops and resets use atomic rename without fsync. This assumes replace-on-rename leaves the complete old or new cursor after power loss (as on ZFS and ext4); elsewhere a torn cursor errors as before. Lost cursor writes cannot reapply stamped frames after reboot because surviving older-boot frames drop.

The file and its cursor outlive ordinary maintenance. GC and event-log rotation leave both alone, and the lock sweep keeps a held shared ingress lock. Reset and teardown first stop the drainer under its spawn and lifetime guards. Reset then removes ingress and cursor under the workspace lock and then the ingress lock. Reload uses the same stop after it records the new room build, releases both guards at once, and nudges a successor without deleting ingress or cursor. At either stop the drainer answers satisfied clients, cached replies included, before it exits, and unsatisfied clients retry against the successor or drain inline.

The drainer's apply child stages a frame's events in its locked lifecycle phase and publishes them with one ordered-batch append under the workspace lock, through `crates/rimz/src/store/event_log.rs::append_batch`. The staged records are encoded into one buffer and appended with one write, without replacing the file or syncing each commit. Transaction-local reads fold the staged events, while other readers see only committed bytes. Derived envelopes carry an optional `ingress` id, the source frame's id, without changing the event schema version.

A frame can apply again after a process crash, so each effect declares how it replays:

- Ingress lifecycle payloads keep the recorded transition and ping edge for redo. A short write can leave a leading part of a lifecycle batch, which redo then treats as applied.
- Keyed native answers, conversation entries, root-session active-time updates, locally priced costs, and standing subscription arming replay.
- Lifecycle replay keeps signal fires and unkeyed effects at-most-once.
- A frame with no lifecycle event re-runs its whole apply on redo.
- Conversation entries and assistant responses use `(entry kind, ingress id)` as their transcript key. Duplicate scans run only on crash retries, never on a first attempt, through `crates/rimz/src/cli/hooks/lifecycle/transcript.rs::record_conversation` and `crates/rimz/src/cli/hooks/lifecycle/transcript.rs::record_assistant_response`.

Derived event timestamps, activity heartbeats, and root-session active-time records use ingress time. That time need not increase in log order, because another writer can commit between a frame's capture and its apply. The post-batch dead-session reaper runs as a plain transaction through `crates/rimz/src/store/writer/reap.rs::Store::reap_dead_sessions_if_due`: another session's `Ended` carries wall-clock reap time and no ingress id, so frame recovery excludes it.

## The write path

Ordinary mutations run one choreography through `Store::commit` in [`writer.rs`](../../crates/rimz/src/store/writer.rs).

Under the workspace lock:

1. Read whatever state the decision needs.
2. Write the durable record files the mutation touches.
3. Append the event frames, one ordered batch per logical change.

Then the lock releases. When the mutation appended an event or forced a publish, the tail runs off-lock:

4. Send a `store_delta` wakeup to every fresh sidebar, one per appended event.
5. Group-sync the log with one `fdatasync`, debounced to once per `LOG_SYNC_INTERVAL` (1 second).
6. Publish the snapshot checkpoint, when due.
7. Reap provably dead sessions, debounced to once per `REAP_INTERVAL` (60 seconds).

Steps 5 and 7 use state stamps `cache/log-sync.stamp` and `cache/dead-reap.stamp`; step 6 uses runtime `snapshots/publish.stamp`. A missing, unreadable, or future-dated stamp reads as due, so clock and I/O uncertainty costs one redundant run instead of a skipped one. The gates are best-effort, not ceilings: the stamp check and the stamp touch are separate steps (`crates/rimz/src/store/writer/debounce.rs::claim`, `sync_log_debounced`), so two concurrent tails can both pass. One `fdatasync` flushes the inode's dirty pages whoever wrote them, so in steady state one writer per interval makes the whole fleet's appends durable.

Wakeups go out before the publish on purpose. Consumers fold the log tail from their own cursor, so checkpoint cadence tunes cold-start latency and never gates freshness.

Mutations that replace, cut, or forget the active log run `Store::commit_boundary` instead. It holds the workspace lock and then the publish lock and runs the mutation. When the mutation reports a `RollupInvalidation`, it deletes `snapshots/latest.json` and the publish stamp before it touches the rollup cache. A log replacement first advances the durable generation and removes the legacy state `cache/snapshots/` directory under the same locks. A crash in between leaves readers folding for themselves instead of trusting an offset that could alias into a regrown log.

| Mutation | Policy | Rollup cache | Rebuild |
| --- | --- | --- | --- |
| Rotation, identity rewrite, soft reset that archived a log | `Replace` | deleted; durable generation adds one | yes |
| Repair that cut bytes | `Drop` | deleted | yes |
| Soft reset of an empty log | `Drop` | deleted | yes |
| Hard reset | `Forget` | deleted; durable generation adds one only if a log rotated | no |

`Store::prune_carryover` takes the same two locks and rebuilds when it removed rows. It retracts nothing, because the log's extent did not change.

### Write classes

Every disk write falls into one of ten classes, and one line sorts them: **durable records and cold metadata fsync; hot appends and disposable caches do not; the drain cursor fsyncs only the writes for a frame a reboot would not drop.** A cache rebuilds, and a group sync or an audit tolerance bounds what an append can lose.

| Class | Files | Discipline | After a power cut |
| --- | --- | --- | --- |
| Durable appends | `cache/providers/session-roles/<UTC-year>.jsonl` | One `append_record_bytes` followed by `sync_file_data`, with parent sync on creation and no lock. The [session role index](./agents/spending.md#session-role-provenance) owns retention and decoding. | Completed writes survive, and malformed trailing records are skipped. |
| Durable effort appends | `cache/providers/session-effort/<UTC-date>.jsonl` | Read-free, lock-free `append_record_bytes` followed by `sync_file_data`, with parent sync on creation. A leading newline separates each append from a torn tail. The [effort ledger](./agents/spending.md#session-effort-ledger) records active credit after its sidecar update and messages after their transcript append. | Completed appends survive, and malformed records are skipped. A process crash between the source effect and the append can lose that effort record. |
| Durable effort compaction | Closed `cache/providers/session-effort/<UTC-date>.jsonl` segments | The elected spending engine folds segments older than 8 days into per-session UTC-day totals through `write_bytes_atomically` (temp file, fsync, rename, directory sync), and removes segments older than 366 days with a directory sync. Hooks only append to the current wall-clock segment. | Either the raw segment or its equivalent totals survive. An interrupted prune can leave an expired segment for the next pass. |
| Durable workflow appends | `owned/workflows/<run-id>/journal.jsonl` | Data-synced append under the workspace lock, before the durable record replace. New directories sync their ancestors. | Survives. A crash before record publication can repeat a sequence. Torn or unknown rows are skipped when inspecting history. |
| Framed logs | `log/events.log.jsonl`, `log/hook-ingress.log.jsonl` | One CRC-framed buffer per record or ordered batch, including each staged hook lifecycle batch, with no per-record fsync. The event log's off-lock tail issues a debounced group `fdatasync` about once a second, and rotation syncs before the rename. The ingress codec has no group-sync tail. | Intact through the last sync. The trailing window can be lost, and the frame CRC detects corruption. Event-log repair truncates it, and the drainer truncates an unterminated ingress tail and skips corrupt terminated rows with diagnostic evidence. |
| Audit appends | `audit/messages/<bucket-start>.jsonl`, `audit/transcript/*.jsonl`, `out/<reader>/<wait-name>.output` | `O_APPEND`, no per-record fsync. History and transcript append under the workspace lock. A queue transaction commits `messages.jsonl` before it appends history and event frames, so a history append failure warns and never undoes the queue transition ([messaging.md → Storage and audit](./harness/messaging.md#storage-and-audit)). A wait log takes no store lock: `rimz wait` creates it at arm time, and the one watcher holding that wait's `locks/loop-watch-<name>.lock` is its only writer after that. A check watcher truncates and rewrites it for each run, keeping only the latest output ([loops.md → Watched commands](./harness/loops.md#watched-commands)). | Trailing records can be lost. The cost is history completeness, never queue correctness. For wait output, the run record keeps the last 4 KiB, and the delivered message carries the file path, estimated tokens, and line count. A file pattern match also includes a matched-line preview. |
| Cache write | Runtime `snapshots/*.json`, `records/live-roster.json`, heartbeats, sidecars, the sidebar's published lanes | Temp file plus atomic rename, no fsync. Checkpoints never create a directory. The roster is the named best-effort exception among records: it is not rebuildable history. | Caches rebuild or refresh. Losing the roster's latest write can narrow recovery. |
| Usage appends | `records/lsp-usage.jsonl` | One `O_APPEND` write under a shared append lock, no fsync. Compaction tries the exclusive lock without waiting and durably replaces counted rows. | Trailing appends can be lost, and compacted counts survive. Malformed rows are skipped. |
| Drain cursor | `records/hook-drain.json` | Claim and applied writes for unstamped frames or an unknown drainer boot use temp file, fsync, rename and directory sync. Same-boot claim/applied writes, corrupt-row skips, other-boot drops and post-truncation resets use cache-class rename without fsync. Truncation retains fdatasync before reset. | Surviving stamped frames from an older boot drop before application, regardless of the surviving cursor. Unstamped recovery stays unchanged. |
| Durable records | `records/messages/messages.jsonl`, `owned/runs/<run_id>.json`, `out/<reader>/<name>[.<n>\|.<run_id>].output` child and peer responses, `workspace.json`, `records/agents-carryover.json`, `records/log-generation.json`, `records/loop-instances.json`, `records/idle-stop.json`, `records/pending-recovery.json`, trust grants, notification handlers, hook installs | Temp file, fsync, rename, parent-directory sync. A child or peer response goes through `crates/rimz/src/harness/run.rs::publish_response`. | Survives. |

Every fsync call funnels through [`disk/atomic.rs`](../../crates/rimz/src/disk/atomic.rs), and no module hand-rolls its own temp-file dance. The `cargo xtask invariants` check `ensure_store_durability` rejects a `sync_all` or `sync_data` method call anywhere else. It matches those two std methods only, so a raw `libc` or `nix` fsync would pass the grep and has to be caught in review.

A rename over a path replaces a symlink there with a regular file. To write through a link, resolve it first.

### Wakeups

After a commit, the writer calls `wake_store_delta` in [`wakeup/mod.rs`](../../crates/rimz/src/wakeup/mod.rs), the shared leaf wire below this module. It walks the runtime heartbeat directory and sends a typed `store_delta` datagram once per distinct `wakeup_socket` among sidebars whose heartbeats are no older than `SIDEBAR_HEARTBEAT_TTL` (5 seconds). It re-stats each heartbeat before the send, which closes the window where a renderer exits between read and write. Each host's attachments name the same room socket.

Sends are non-blocking, so a full receiver queue drops the datagram and the write moves on. Per-target failures are absorbed. Only a failure to read the heartbeat directory reaches the writer, which logs it. A consumer closes any missed wakeup at its next tick (`rimz sidebar serve --tick-seconds`, default 1 second). The envelope and its event taxonomy belong to the receiving side, [state.md → Realtime events](./sidebar/state.md#realtime-events).

Supervised-run waiters are woken separately. `wake_run` in [`run.rs`](../../crates/rimz/src/store/run.rs) sends a terminal datagram to the run's [wake socket](./harness/scripting.md#the-wake-socket). `harness::run` and the CLI call it after they write a terminal run record, and store reset calls it for the runs it cancels.

## The read path

Reads take no lock. `snapshot::rebuild` and the [`snapshot/assemble.rs`](../../crates/rimz/src/store/snapshot/assemble.rs) entry points fold the log into the agent rollup, then project that into the sidebar view-model ([sidebar.md](./sidebar/sidebar.md#from-store-to-screen) owns the projection).

The fold is resumable from two runtime checkpoints ([`snapshot/fold.rs`](../../crates/rimz/src/store/snapshot/fold.rs)). `snapshots/rollup.json` holds a fold base and the `LogExtent` it reflects. A reader resumes only when the version, durable generation and active log file identity match, and its offset does not exceed the log's length, folding only the missing tail. The generation is read before the log stat; that stat supplies the identity before bytes are folded. An absent identity matches an absent log; targets without device/inode identity retain the generation and offset guards. `snapshots/latest.json` holds the published view-model and the extent it was built from. `read_fresh_latest` serves it only when its generation matches, its stamped offset equals the live log's length and its `snapshot_version` is current. A cold reader pays one full fold and a warm reader pays only the new bytes; boundary writers retract and rebuild their own runtime domain's checkpoints after changing the log under the publish lock. Independent runtime domains validate against the shared durable record and log rather than trusting another domain to retract their copy.

`StatePaths` binds checkpoints to one `RuntimePaths` domain. Ambient constructors bind the ambient runtime; explicit-home constructors stay unbound. Store opens rebind to their supplied runtime, including read-only existing-store opens. An unbound path always misses and skips checkpoint writes. Runtime setup alone creates `snapshots/` beneath the validated private room root; publishing into a missing directory skips without failing a commit. Soft reset rebuilds there while removing sock, live and lanes; hard reset leaves no checkpoint or publish stamp. Losing the runtime checkpoint never loses the durable generation or carryover.

The legacy state `cache/snapshots/` directory remains available to older binaries until a log-replacing boundary or a reset clears it. A no-op rotation check and an in-place repair leave it alone; GC does not age-sweep it. Scanners no longer consult checkpoints: list recency uses the active log and workspace record, and GC counts only the active log or archives as history. A legacy `latest.json` alone is debris, not a reason to keep an otherwise abandoned room.

Every store read on a thread goes through that thread's one held `RollupCursor`: `Store::snapshot`, `Store::runtime_projection`, the writer's identity and lifecycle reads, and `snapshot::rebuild`. A read after an append folds only the new frames. An unchanged log and carryover need a bounded generation read and metadata checks, without reopening the log or carryover or copying a row. The slot is keyed by the workspace's log, rollup, and carryover paths, so a read of another workspace replaces it and a thread holds at most one rollup. A long-lived reader that must not share that slot (the sidebar fetch worker) owns its own cursor instead. Callers share the held rows, and only a caller that changes or keeps a row copies it.

Every generation reader, including the parked wrapper, reads `records/log-generation.json`: `{"generation": <u64>}`. Only `Store::commit_boundary` writes it, durably after the mutation and before rebuilding, under the workspace lock. A rotation or identity rewrite adds one; a hard reset adds one only when it rotated a log; repair keeps the generation. Losing a checkpoint or rebooting does not change it. The read is bounded to 1 KiB, never caches or writes, and treats a missing, undecodable or oversized record as zero. Additional record fields must be optional and defaulted. There is no migration from older binaries' cache generation: the first new reader may see one backward move to zero.

The carryover beneath every fold is parsed once per file identity per thread. Rotation and prune replace it by atomic rename, which changes that identity, and the maintenance writers themselves always read the durable file. A missing durable carryover reads as empty; a corrupt replacement fails every fold until replaced. The fold-ready parse backfills identities, sorts rows by key, and applies the ended-row text rule even to legacy carryover that still holds full text. Readers never persist that projection.

Every ended row, whether carried or reduced from the active log, keeps `first_prompt` and `prompt` as labels: collapse whitespace and drop control characters with `single_line_description`, take the first 160 characters, then drop trailing whitespace from the cut. Blank labels become absent. `AgentState::trim_ended_text` owns this idempotent rule and clears `recent_prompts`, `pane.foreground_cmdline`, and `pane.spawn_command`, preserving pane identity and `task`. The reducer applies it after the ending event's text producers, and the carryover reader applies the same rule. Continuing rows stay whole; a later lifecycle event or launch registration revives an ended row from its labels, without restoring discarded text.

Explicit agent reads return the row they resolve, including its bounded labels. An ended child retained in the runtime projection keeps a present latest-prompt label, so it is not mistaken for a never-prompted Fresh card. Description and task precedence stay unchanged. The session-name prefix filter compares against the retained text, so a name longer than that prefix is not suppressed merely for prefixing the original prompt. Snapshot JSON omits ended rows' recent-prompt history and pane-command keys. These labels are presentation text, not structured wake headers; the wake-header parser remains newline-sensitive.

The publish gate in [`writer/publish.rs`](../../crates/rimz/src/store/writer/publish.rs) decides when the write tail refreshes the checkpoints. A checkpoint is due when the stamp is `PUBLISH_INTERVAL` (1 second) old, when the unpublished tail reaches `PUBLISH_BYTE_BUDGET` (64 KiB), or when the log is shorter than the stamp's offset. Concurrent publishers serialize on `locks/publish.lock`, so they group-commit.

`Store::snapshot_cached` serves `latest.json` when it is current and re-projects otherwise, then attaches the agent context sidecars for the projected agents. That gives every address resolver the same provider rest evidence (a provider error, or a completed or interrupted settle) that pane ownership uses. It delegates to `Store::snapshot_with_context`, which keeps the same rows and membership but attaches only the selected `(kind, agent_id)` keys; an empty selection does not read sidecars or evict cached parses. Hook reads of lifecycle-only fields select none, and own-session context reads select that session. Address resolution, pane ownership and reaping retain full attachment. The pure snapshot reducer reads the store alone, and a missing sidecar leaves the raw lifecycle status authoritative.

### Runtime and audit

The same durable rollup answers two questions, and [`runtime.rs`](../../crates/rimz/src/store/runtime.rs) is the filter between them.

**Runtime** scope filters at read time and backs the default views: `rimz sidebar snapshot` and the plain `rimz doctor` agent summary. It hides rows carrying `ended_at` and rows whose `runtime_owner` is no longer the live process that wrote them, and it keeps a launched child visible while its parent is. Ended rows still count toward the molt supersession the projection stamps. `runtime_owner` records the owner kind, a stable subject id, the pid, and on Linux the process-start token, so a reused pid does not read as the original owner. A row with no owner stays visible. A row with a known-dead owner or a process-start mismatch is hidden while the reap converges it to a real end stamp.

Ended and expelled identities ride the published snapshot as `fenced_sessions`, which stops provider-local session rebinding from reviving them. The fence holds every retained ended or expelled identity, so its size follows retained history: a row leaves it when rotation or `rimz gc` prunes it from the carryover at the 14-day retention window, unless its owner is still live. The fence is deliberately not narrowed further. Only the oldest live pane's clock could say which ended sessions no pane can still claim, and the snapshot is assembled store-side with no pane list. A pane lives as long as the user keeps it open, so any age cutoff would let a pane older than the cutoff rebind an ended session.

**Audit** scope bypasses the filter and reads durable history as written. `rimz doctor --audit`, explicit resume, launch name allocation, and message dispatch read it, and explicit resume is the reason ended rows are retained at all.

Pick the scope by the question, because the wrong one is silent. `Store::snapshot_cached` and `Store::runtime_projection(RuntimeScope::Runtime)` answer "who is here now". `Store::runtime_projection(RuntimeScope::Audit)` answers every question where an ended session is itself the evidence: a liveness gate, a retirement predicate, a resume check. Asking the runtime scope whether a session has ended returns a clean empty answer, since the row that carries the proof is the one that scope removed.

## Session death

Two mechanisms end sessions in the store. The reap converges individual sessions RimZ can prove are gone, and the `session.death` record captures the loss of a whole room.

### The reap

The write tail's debounced reap (`writer/reap.rs`) scans the audit rollup and appends an `agent.lifecycle` `Ended` observation for each root session it can prove is finished. The event name records the reason, checked in this order:

| Event name | Condition |
| --- | --- |
| `ReapedSuperseded` | A different session took the same slot (`session_death::supersedes`): same-instance on-screen evidence for a non-asking `FollowLatest` owner, or the fallback proofs (relaunch, fresh replacement, in-place switch, forked retry, compaction continuation). On-screen evidence uses its own stamp, not newer activity, and never retires an open ask. |
| `ReapedDead` | The recorded owner process is provably dead. |
| `ReapedStale` | The session never captured an agent or script pid, or its owner is the shared daemon, and it has been quiet past `GHOST_SESSION_TTL_SECS` (3 hours). |

Before it evaluates supersession, the reaper attaches the context sidecar of each running or waiting root, so a current provider error or a completed or interrupted settle can certify the owner rested. A missing sidecar leaves the raw lifecycle status authoritative.

Two records shield agents from the reap. An agent named in `records/live-roster.json` is exempt from `ReapedDead` and `ReapedStale`, but `ReapedSuperseded` still applies. A root agent named in `records/pending-recovery.json` is exempt from every reap reason and cannot supersede another agent, so a parked candidate survives the scan until a settlement resumes or ends it, however long the user stays away. Pending launched children have no such shield: recovery never resumes them, so the ordinary reasons and supersession apply. The exec wrapper infers no end for a pending agent ([fleet.md → Reclaiming a pane](./harness/fleet.md#reclaiming-a-pane)). Already-ended rows are skipped and cannot supersede an active replacement.

Outside the reap, `Store::retire_worktree_sessions` appends the same `Ended` observation as `WorktreeRemoved` for the non-live sessions of a removed worktree.

The reducer stamps `ended_at`, which hides the row from runtime views at once, while the audit rollup keeps its resumable identity until rotation prunes it at the retention boundary. The stamp suppresses the row without making it terminal. Any later lifecycle event under the same `(kind, session id)` clears it, so a session that reports in again returns to the runtime view under its own identity, and every fence keyed on the end stamp releases with it.

The screen runs ahead of the durable append through two read-side filters. Runtime expel ([Runtime and audit](#runtime-and-audit)) hides a row whose owner is dead. The snapshot view reap, `crates/rimz/src/store/snapshot/view/reap.rs::SidebarSnapshot::reap_stale_sessions`, drops superseded rows and pidless rows past the ghost TTL, through the same `session_death` predicates. Neither applies the roster or pending-recovery shields, and the view reap attaches no sidecar rest evidence, so the screen can drop a row that the durable reap keeps.

### The session.death record

When a room comes back after its mux session died, the rebirth path appends `session.death` for the lost incarnation, carrying `cause` and the agents that went with it. The cause is `reboot` when the host boot id in `records/boot.json` changed. A same-boot `crash` also requires `records/live-roster.json` to name agents worth recovering.

`records/live-roster.json` is the sidebar producer's last pane-backed live agent set, children included: the agents that mux session would lose if it died. It is the one records-class file the producer writes best-effort (`store/live_roster.rs`): nothing rebuilds it, but a lost write only narrows one recovery. The producer removes an agent from it only while the mux still lists its session, so a sidebar that outlives the session cannot empty it ([sidebar.md → Resume-on-rebirth](./sidebar/sidebar.md#resume-on-rebirth)). Birth intersects it with the audit rollup, so cleanly ended agents and paneless ghosts stay out of recovery.

The roster means only "the current incarnation's live set". Every birth whose session did not exist (inspected, inspection failed, or supervised) parks the roster's agents in `records/pending-recovery.json` before it creates the session, then deletes the roster at the boundary. A failed park fails the birth before the session exists and leaves the roster intact for a retry, so the new producer cannot overwrite an unparked lost set. The reap reads the roster before pending, so it cannot miss an agent moving between them.

`records/pending-recovery.json` (`store/pending_recovery.rs`) carries `version: 1`, an `agents` set of `(kind, session id)`, and a `seeded` subset awaiting tab confirmation. `seeded` defaults to empty on read and is omitted when empty, and a reader that knows only `agents` ignores it. Boundary and settlement are read-modify-write from separate CLI processes, so both take the workspace lock and publish durably. An absent or unreadable record reads as empty.

A settlement plans over the record plus the roster at a birth, and over the record alone in a live room, keeping the agents that are neither ended nor live. Its write removes settled keys and replaces `seeded` with its resumed keys intersected with the remaining `agents`, which clears stale seeds from a birth that died before confirmation. An agent leaves the record in three ways:

- The tab that resumes it is confirmed open.
- Its exec wrapper attaches it to a pane while it is not seeded ([fleet.md → Reclaiming a pane](./harness/fleet.md#reclaiming-a-pane)).
- Its ended trace is durable.

Confirmation removes confirmed resumed keys from both sets, and unconfirmed keys from `seeded` only, which returns them to parked for a later rebirth or an explicit resume. Every write keeps `seeded` a subset of `agents`, and every external reader reads `agents`, so a seeded agent keeps the pending shield. Reset and class GC keep the record wherever they keep the roster.

`records/last-death.json` records the incident for cheap `rimz list --all` display, and the reborn room writes back how many lost agents came back in a tab confirmed open. `rimz doctor` shows the last incident with its cause, time, lost and recovered counts, and archive path. A crash birth also archives mux forensics under `audit/crashes/<utc-ts>/`, best-effort and never blocking launch, bounded by the audit class sweep rather than a write-time ring.

Each lost session that recovery does not bring back gets a durable `Ended` trace. The trace drops the session from the next recovery set and keeps it resumable by hand. A user decision at an interactive start writes one of three:

- `rimz.recovery-declined` when the user declined recovery: an answered no, `--no-resume`, or `resume.on_rebirth = false`.
- `rimz.not-resumed` when the user dropped the agents a recovery could not resume.
- `rimz.worktree-gone` when the dropped agent's checkout is missing.

A settlement writes two traces unattended. `rimz.child-not-resumed` ends a non-live child that recovery never resumes, after canceling its newest open run. `rimz.seat-refilled` ends a parked session when a live member already holds its matched team seat, or when the tab of its planned Fresh replacement is confirmed open. Each child end emits an informational `recovery_child_ended` diagnostic. The ended stamp is durable before the key leaves pending, so a failed append or an unconfirmed replacement tab keeps the old session parked.

The recovery flow from roster to repopulated panes is [fleet.md → Resume and rebirth](./harness/fleet.md#resume-and-rebirth) and [sidebar.md → Resume-on-rebirth](./sidebar/sidebar.md#resume-on-rebirth).

## Maintenance

Three things reclaim store space: `rimz reset` on demand, `rimz gc` on demand, and the sidebar host's daily automatic sweep. What each command prints is [maintenance.md](../reference/cli/maintenance.md), and this section owns what each one does on disk.

### Reset

For a layout-2 room, soft or hard, `rimz reset`:

- Cancels active supervised runs. Their records stay loadable for waiters until terminal-run GC reclaims them after its 7-day grace.
- Clears the room account selection.
- Removes `records/idle-stop.json`. A soft reset leaves its agents resumable, and a surviving request would stop the resumed session.
- Stops the hook drainer and removes ingress and its cursor ([Hook ingress](#hook-ingress)).
- Stages carryover on a soft reset, and rotates the log.
- Clears `cache/`, skill copies included, and the runtime `sock/`, `live/`, and `lanes/`.

Every directory a reset removes is detached first, so a late writer cannot refill the tree being deleted. Each runtime directory is renamed before its recursive deletion. Each state directory is renamed under the workspace lock to the sibling `<dir>.reset` before it is counted and deleted. An entry written to the canonical path after the detach survives the reset, and the next reset of that directory removes, uncounted, any `<dir>.reset` an interrupted reset left behind. The `.reset` suffix is reserved: hard reset treats any `owned/` child directory ending in `.reset` as debris and removes it in place, uncounted, instead of detaching it again.

The rename cannot detach a process inside a scoped view, whose `/tmp` is a bind mount of the temp unit directory itself and follows it to `tmp.reset/`. A hard reset therefore ends, after teardown and before it opens the store, every process that has one of the room's temp units as its `/tmp`. When one outlives the kill, reset refuses and names each survivor's pid. The store is untouched and a rerun completes ([temp units](./sandbox.md#temp-units)).

A soft reset keeps `audit/`, `owned/`, `tmp/`, `shared/`, `team-notes/`, `out/`, the log archive, and every record it does not name above, standing fleet budget choices included. `--hard` also removes the active log, carryover, `audit/`, `owned/` except `owned/runs/`, `tmp/`, `shared/`, `team-notes/`, and `out/`, and keeps the log archive it just wrote. Neither removes state `locks/`. Provider-owned sessions stay outside this boundary.

Teardown removes the runtime `sock/`, `live/`, and `lanes/` classes and no state class, so `tmp/`, `shared/`, and `out/` wait for GC. Allocating a handle to a new agent row removes any `tmp/<handle>/` and `out/<handle>/` an earlier holder of that name left behind. That removal runs after the launch commits, outside the workspace lock, and a failure logs a warning instead of failing the launch.

### Garbage collection

`rimz gc` applies the [class table](#what-is-on-disk) in one of two scopes. Room scope sweeps the current room through `store::gc::collect_room`. Machine scope (`--all`, or the elected unattended sweep) first prunes dead workspaces, then sweeps every remaining known room through `collect_classes`, so a removed store gets no class block. Both scopes resolve the room once through `WorkspaceResolver::resolve_participant` and use `open_existing_store`, so an absent state root gets no class block or scaffold. Text and JSON output carry per-room, per-class counts and bytes as `rooms[].classes[]`, with a top-level `scope`.

Layout-1 rooms differ by scope. Room scope refuses a layout-1 current room with the layout error, which names `rimz start` or `rimz reset` as the fix. Machine scope prunes every layout-1 room and its runtime tree as `incompatible_layout`, the current room included, before any class sweep (`crates/rimz/src/store/gc/prune.rs::classify_workspace`). `rimz list` omits layout-1 rooms.

`--dry-run` plans removals without deleting files, and skips the mutating store maintenance below. Locks report `locks_would_check` instead, with no acquisition and no claimed removal. `--older-than` sets the TTL for `live/` and for orphan atomic-write temps. It never changes the temp-unit grace, owned or audit retention, or either history retention setting.

The two scopes differ in reach:

| Step | Room scope | Machine scope |
| --- | --- | --- |
| Orphan atomic-write temps | Under the room's state and runtime roots | Across both whole roots |
| Loop instance rows | The room's own, through `TaskCatalog::reap_dead_deliveries_for` | Every root's, plus machine delivery tasks |
| Dead workspaces and stale provider probes | Not touched | Pruned |
| Orphan `<home>/empty/` trees | Not touched | Swept |
| Landed worktrees | Current repository only | Current repository only |
| Event-log repair, orphan message archive, stale sent-message reconcile, carryover prune past 14 days | Current workspace | Current workspace |

The dead-workspace prune isolates errors per workspace. A workspace it cannot remove becomes a `workspaces.failed` row, counts as a problem, and contributes no reclaimed bytes. Its state and runtime removals are attempted independently, and the row carries the first error. The state removal deletes `workspace.json` last, so a partially removed state directory still classifies as dead and is retried next run instead of being kept for its history. A runtime-only failure after a successful state removal leaves no state entry for the next prune to retry.

The orphan empty-dir sweep scans `<home>/empty/` for valid room directory names whose stores were pruned in this run or are absent. It removes agent directories past the 7-day owned grace through the same predicate as the per-room sweep, and removes an orphan room directory once it is empty. `workspaces.empty_dirs` reports every visited tree's files, bytes, and the agent directories kept within the grace, and those bytes count in the prune total. A dry run uses the pruned names as well as store absence, so its preview matches an applied sweep. A tree the sweep cannot read or delete is skipped with a warning and costs neither the other trees nor the workspace prune report.

Both scopes write two shared paths outside the room:

- Loop overlays. Both scopes run `TaskCatalog::load(Some(root)).prune_orphan_overlays()`, which prunes keys for the root's known scopes in the shared `loops_dir()` files, and overlay storage stays outside the room. When loading the catalog fails and loop-task sources are unreadable, GC keeps those overlays, skips its worktree step, and continues the other maintenance. Worktree protection reads every workspace's task sources strictly, and [worktree protection](./harness/worktrees.md#protection-facts) owns the holder rules.
- Account budget ledgers. Both scopes call `harness::budget::sweep_orphan_account_ledgers`. It removes from `cache/providers/` each `budget.account.<component>.json` that no history pool reads: a kind-only name, or the own key of a declared account whose pool is another key. The matching lock in runtime `shared/` goes with it. An `@`-named ledger stays when the accounts config fails to load, and so does the ledger of an undeclared name or of a standalone account. The sweep never waits on a ledger lock: it try-locks, unlinks the ledger and then the lock while it holds it, and leaves both for the next run when another process holds the lock. The ledger is a cache-class write, so the unlink takes no fsync. An error on one ledger is a debug line and keeps that ledger. A dry run counts without locking.

Three report rows follow their own retention settings:

- `transcript` is not a class directory. It covers `audit/transcript/` and `audit/messages/`, which the audit sweep's traversal, mtime rule, and size cap exclude. `gc.transcript_retention` keeps both forever by default, or applies a per-file mtime TTL and prunes empty subdirectories.
- `team-notes` follows `gc.team_notes_retention`: forever by default, otherwise whole-run expiry by the newest recursive `lstat` mtime, directories and symlinks included, without traversing link targets. A checkout's continued existence does not protect a stale run, and GC never edits checkout links.
- `audit`, `transcript`, and `team-notes` report remaining file bytes as optional `bytes_retained` (projected in dry runs). Other rows omit that field.

The owned-class sweep removes a terminal workflow's whole home once its record mtime exceeds the 7-day owned grace, under the workspace lock. Active and parked workflow records are never candidates. A workflow home without its record, which only a crash during creation leaves, goes once the home directory's mtime exceeds the same grace.

### Automatic sweeps

The sidebar host is the sole automatic trigger: once a day per room, after a five-minute settle, it runs `rimz gc --unattended`. The helper normally runs room scope. `harness::auto_gc::claim_machine_sweep` elects one machine sweep per 24 hours across the machine, using a stamp check, a nonblocking lock, and a locked stamp recheck, and it holds the guard through the sweep and the stamp write. `disk::paths::machine_auto_gc_stamp` names `cache_dir()/auto-gc.json`, and `RuntimePaths::shared_auto_gc_lock` names the shared runtime `auto-gc.lock`.

Room and machine stamps both use `{ "swept_at": <Timestamp> }` and record failed attempts too, so a persistent failure retries daily. A closed room waits for the machine sweep. Assist records carry `scope` and the helper's own room's `class_bytes`, while the other totals cover the whole selected scope. Automatic sweeps require a readable core config, so an unreadable opt-out never becomes an enabled default. See [automatic sweeps](../reference/cli/maintenance.md#automatic-sweeps) for the settings.

## What survives what

Reset, teardown, and reboot each draw a fixed line through the classes, and age-based GC runs independently of all three. A soft reset drops `cache/`, the runtime `sock/`, `live/`, and `lanes/`, and the active log after archiving it. A hard reset also drops carryover, `audit/`, `owned/` except `owned/runs/`, `tmp/`, `shared/`, `team-notes/`, and `out/` ([Reset](#reset)). Workflow homes under `owned/workflows/` therefore survive a soft reset but not a hard one. Both resets keep state lock inodes. Teardown drops those three runtime classes and keeps `snapshots/` and state; reboot drops all of runtime and keeps state. Whole-room deletion and `uninstall --state` or `--all` remove team archives with the room, and a default uninstall keeps room state.

| Event | Store | Live sockets and heartbeats | Multiplexer session |
| --- | --- | --- | --- |
| Detach | yes | yes, the mux server stays alive | yes |
| Sidebar reload | yes | socket rebound on attach | yes |
| Multiplexer server crash | yes | no | no |
| Host reboot | yes | no | no, needs a host supervisor (tmux-resurrect, Zellij resurrect, systemd) |
| Host power cut | yes, through the last group `fdatasync`, and repair truncates any torn suffix ([write classes](#write-classes)) | no | no, needs a host supervisor |

RimZ guarantees the store across all of these, and at a power cut that guarantee runs through the last group sync. The session and its processes survive only what the multiplexer server and the host supervisor keep alive.

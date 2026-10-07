# The store

> The store is `crates/rimz/src/store/`, and this page maps it: the files on disk, the event log, the write and read paths, session death, and the maintenance that keeps it all bounded. The sidebar's runtime caches and how a renderer consumes the store are [state.md](./sidebar/state.md). What these mechanisms cost is [performance.md](./performance.md), and the product commitments behind them are [DESIGN.md](../../DESIGN.md).

Every durable fact RimZ knows about a room lives in one directory of flat files, with no daemon and no database. A hook fires, a CLI command runs, an agent finishes a turn: each is a short-lived process that takes an advisory lock, appends a framed record, and exits. Readers, every sidebar renderer included, fold those records back into state without taking a lock.

Three properties follow, and the rest of RimZ relies on all three. Writers cannot interleave, because the lock serializes them. Readers never block a writer, because reading takes no lock. A process killed at any instant leaves a log the next reader can still parse, because the only frame that can be in flight is the last one.

## Truth and cache

**`log/events.log.jsonl` and the durable record files are the truth. Derived snapshots are caches that any reader may rebuild from the log; audit evidence and owned scratch are not rebuildable caches.**

A reader that finds a cache stale, corrupt, or absent folds the log itself. A writer that dies before publishing therefore costs the next reader a bounded fold, never a wrong answer. The same rule lets the sidebar be read-only on the store: it never has to write to be correct, only to be fast. The `cargo xtask invariants` check `ensure_sidebar_library_boundaries` keeps store writers out of the sidebar's import graph.

The fold derives `AgentState.root_lane` from the workspace record: an unstamped channel whose worktree path equals the project root. Bound `PaneAgent` rows copy it; sessionless panes derive it from the snapshot's project root. This display fact never comes from a lifecycle event and does not change routing channel values. Carryover rows retain their shared fold-ready layer, stamped once per workspace identity, rather than being copied on every fold.

Two habits follow from the rule, and new code here keeps both.

Cache the parse, never a verdict. [`disk/parse_cache.rs`](../../crates/rimz/src/disk/parse_cache.rs) memoizes one thread's last deserialization of a JSON file under one of two keys. `ParseCache::get`, which the snapshot readers use, keys on `(path, mtime, len)`, so two republishes inside one mtime tick at equal length can serve the older parse. `ParseCache::get_stamped` adds the device and inode pair and tells those replacements apart. Every caller re-validates against live truth, so a stale serve costs a re-read and never a wrong result.

Measure freshness by log extent. A derived rollup records the `LogExtent` it reflects: the rotation generation and the byte offset after the last folded frame. Comparing that pair against the live log is one `stat`, with none of mtime's granularity or write-ordering hazards.

## Where the code lives

`Store` ([`mod.rs`](../../crates/rimz/src/store/mod.rs)) is a cloneable handle around an `Arc` holding the workspace's `StatePaths` and `RuntimePaths`. `mod.rs` carries the handle, the core errors, and the lock-free reads; every mutation lives under [`writer.rs`](../../crates/rimz/src/store/writer.rs) and takes the workspace lock. There is no in-process actor, because cross-process serialization is the workspace lock's whole job.

Four boundaries decide where new code goes:

- The store folds the agent model, so `store` imports `agents` and nothing under `agents` imports `store`.
- `store::message` owns the durable message record, its codec, and FIFO/claim selection; delivery in `message/` imports them ([messaging.md](./harness/messaging.md)).
- `store::event` owns the persisted signal vocabulary (`SignalName`, `SignalSource`, `SignalEventPayload`). `Store::append_signal` takes that payload, and the harness converts its runtime `Signal` on the way down ([loops.md](./harness/loops.md#the-signal-vocabulary)).
- `store::run` owns the supervised-run schema, its temp-file-plus-rename codec, and the terminal wake sender. `harness::run` owns creation and the status transitions, which take the workspace lock ([scripting.md](./harness/scripting.md#the-record)). Store reset is the one place the store writes a run record itself.

| Store module | What it owns |
| --- | --- |
| [`writer.rs`](../../crates/rimz/src/store/writer.rs) | Public mutation intents and outcomes, the `commit` and `commit_boundary` primitives, and the off-lock tail. `writer/` splits the implementation into `debounce`, `lifecycle`, `publish`, `queue`, `reap`, and `reset`. |
| [`event.rs`](../../crates/rimz/src/store/event.rs) | `EventEnvelope`, the typed `EventKind` decode, the schema version, and the persisted signal vocabulary. |
| [`event_log.rs`](../../crates/rimz/src/store/event_log.rs) | The framed append log: `frame.rs` codec, `recovery.rs` repair, `rotation.rs` archive publication and retention. |
| [`follow.rs`](../../crates/rimz/src/store/follow.rs) | The read-only lifecycle and signal follower over the event log. |
| [`snapshot/`](../../crates/rimz/src/store/snapshot/mod.rs) | The canonical snapshot schema and read side: `fold.rs` resumable rollup and carryover, `project.rs` lifecycle reducer, `assemble.rs` read entry points, then pane binding and the view-model projection ([sidebar.md](./sidebar/sidebar.md#where-the-code-lives)). |
| [`runtime.rs`](../../crates/rimz/src/store/runtime.rs) | The runtime-versus-audit read scope. |
| [`session_death.rs`](../../crates/rimz/src/store/session_death.rs) | Supersession and pidless-ghost rules shared by the durable reap and the view reap, and `GHOST_SESSION_TTL_SECS`. |
| [`live_roster.rs`](../../crates/rimz/src/store/live_roster.rs) | The `records/live-roster.json` codec for rebirth recovery. |
| [`idle_stop.rs`](../../crates/rimz/src/store/idle_stop.rs) | The `records/idle-stop.json` codec: pending `agents stop --when-idle` requests, written under the workspace lock; an absent or unreadable record reads as no requests. |
| [`message.rs`](../../crates/rimz/src/store/message.rs), [`message/codec.rs`](../../crates/rimz/src/store/message/codec.rs) | The durable message record, FIFO/claim/batch selection, and the live-queue and history codec. |
| [`run.rs`](../../crates/rimz/src/store/run.rs) | `RunRecord` and `RunStatus`, the durable codec, and `wake_run`. |
| [`sidecar.rs`](../../crates/rimz/src/store/sidecar.rs) | The stat-gated latest-wins sidecar store behind [`agent_context.rs`](../../crates/rimz/src/store/agent_context.rs) and [`subagent_context.rs`](../../crates/rimz/src/store/subagent_context.rs). |
| [`active_time.rs`](../../crates/rimz/src/store/active_time.rs) | The per-session estimated active-time accumulator, serialized by per-record flocks. |
| [`gc.rs`](../../crates/rimz/src/store/gc.rs) | Room and machine maintenance: `gc/collect.rs` runtime hints, `gc/temp_sweep.rs` orphan write temps, `gc/prune.rs` machine-only dead workspaces. |

The file primitives sit one level down, in `disk/`, which imports nothing from the store.

| Disk module | What it owns |
| --- | --- |
| [`disk/paths.rs`](../../crates/rimz/src/disk/paths.rs) | `StatePaths` and `RuntimePaths`: the store-owned filenames on this page, the XDG resolution, and the private runtime-directory check. |
| [`disk/atomic.rs`](../../crates/rimz/src/disk/atomic.rs) | Whole-file publication and sync discipline, cache-local temp cleanup, and the cache-class read (missing or corrupt reads as default). Every fsync call is in this file ([write classes](#write-classes)). |
| [`disk/lock.rs`](../../crates/rimz/src/disk/lock.rs) | `WorkspaceLock`, the advisory flock, bounded by `LOCK_TIMEOUT` (30 seconds); the timeout error names `fuser` to find the holder. |
| [`disk/rotating.rs`](../../crates/rimz/src/disk/rotating.rs) | Best-effort rotating JSONL append and decoded visits. |
| [`disk/parse_cache.rs`](../../crates/rimz/src/disk/parse_cache.rs) | The per-thread parse memo ([Truth and cache](#truth-and-cache)). |
| [`disk/single_flight.rs`](../../crates/rimz/src/disk/single_flight.rs) | Cross-process producer election: shared reuse, exclusive production, or local-only fallback. The sidebar imports it, so it stays free of every writer module; callers own freshness and publication. |
| [`disk/usage.rs`](../../crates/rimz/src/disk/usage.rs), [`disk/summary.rs`](../../crates/rimz/src/disk/summary.rs) | Symlink-safe, hardlink-aware byte walks for `doctor`, `uninstall`, and `gc`, and text-file measurement. |

Start at `writer.rs` for how a fact gets in, at `snapshot/fold.rs` for how it comes back out, and at `disk/atomic.rs` for what durability costs.

## What is on disk

Room files have one lifetime class per directory, constructed by `disk/paths.rs`. State survives reboot; runtime does not. The root `workspace.json` and `rimz` executable are room identity, outside the classes. Retention figures live in `disk/retention.rs`.

| Class directory | Tier | Reclamation rule |
| --- | --- | --- |
| `log/` | State | Rotate at 64 MiB; archives and carryover have 14-day retention. |
| `records/` | State | Standing records; no age sweep. |
| `audit/` | State | Remove files older than 30 days by mtime, then oldest first until at most 64 MiB remains per room. |
| `cache/` | State | Rebuildable; cleared on reset, no age sweep. |
| `owned/` | State | Agent unit: 7 days after the latest session using its handle ends, never while its process owner is live or its directory or a direct child is newer than 7 days. Unknown handles use the same mtime grace. Runs: terminal and record mtime older than 7 days. Restorability does not extend the grace. |
| `tmp/` | State | Agent temp unit `tmp/<handle>/`: the `owned/` agent-unit rule. `tmp/_unnamed/`, shared by handleless launches, stays until the room directory is pruned. |
| `shared/` | State | Room-wide files agents hand each other; no sweep. Hard reset removes it; otherwise it goes when the room directory is pruned. |
| `out/` | State | RimZ-written results under `out/<reader>/`; `out/` and each reader directory are created at mode `0700`. A response file goes once it is older than 7 days and no run record names its agent (the stem before the first `.`); a `wait-` file once it is older than 7 days and its watcher is not live. An empty reader directory goes. |
| `locks/` | State | Try-lock and unlink while held; keep busy files and every subdirectory, since a reopening waiter recreates only the file. Dry runs only count files as would-check, without locking. Every acquirer checks descriptor/path inode identity after flock and retries on replacement. Reset and teardown never remove this class. |
| `sock/` | Runtime | Remove sockets whose connect probe is refused, after the sidebar heartbeat TTL startup grace. |
| `live/` | Runtime | Mtime TTL (`gc.older_than`, default 7 days); renderer-instance claims also expire by heartbeat liveness. Exception: keep agent telemetry while its room is live, because the external exporter holds its inode open. |
| `lanes/` | Runtime | No age sweep; reset, teardown, room death and reboot reclaim coordination state, including quiet schedules and user choices. |

### The workspace store

The workspace store is `<home>/ws/<workspace-dir>/`, where `<home>` is `$RIMZ_HOME` or `~/.rimz`: durable truth plus the caches derived from it that should survive a reboot.

```text
log/events.log.jsonl                          framed event log
log/archive/events.<uuidv7>.jsonl              rotated logs
records/agents-carryover.json                 agent rollup carried across rotation
records/messages/messages.jsonl               live message queue
records/loop-instances.json                   loop and wait rows
records/loop-launches.json                    resident loop launches by task and checkout
records/boot.json                             last host boot id
records/live-roster.json                      producer's last pane-backed live agent set
records/pending-recovery.json                 lost agents awaiting the user's recovery decision
records/idle-stop.json                        pending `agents stop --when-idle` requests, one per session
records/last-death.json                       last incident
records/budget.fleet.json                     standing fleet override, raise, or disable
audit/transcript/<bucket-start>.jsonl         chat transcript, 7-day buckets
audit/messages/<bucket-start>.jsonl           terminal messages, same bucket rule
audit/crashes/<utc-ts>/                       mux forensics
audit/diag.log.jsonl, audit/diag-frames/       anomalies and captured frames
audit/notify.log.jsonl                        notification diagnostics
audit/plugin-presence.log.jsonl               presence diagnostics
audit/binding.log.jsonl                       binding diagnostics
cache/snapshots/{latest,rollup}.json           view-model checkpoint and fold base
cache/doctor-cleared.json                     cleared-incident watermark
cache/auto-gc.json                            daily sweep stamp
cache/{publish,log-sync,dead-reap}.stamp       off-lock tail debounce
cache/auto-rotate.stamp                        rotation debounce
cache/skills/<sha256>/                        handleless rewritten skills
owned/agents/<handle>/scratch/                per-handle scratch
owned/agents/<handle>/skills/<sha256>/         immutable rewritten skills
owned/runs/<run_id>.json                      supervised-run records
tmp/<handle>/, tmp/_unnamed/                  per-agent temp units
shared/                                       room-wide shared files
out/<reader>/<name>[.<n>|.<run_id>].output    child and peer responses
out/<reader>/<wait-name>.output               watched-command output
locks/*.lock                                 workspace, publish, subagent-zone, loop-instances,
                                             loop-run-<name>[-<checkout id>], loop-watch-<name>,
                                             message-sweep, sidebar-launch, snapshot,
                                             topology-writer, authoritative-pane-probe, focus-anchor,
                                             pr-state, diff-stats, budget.fleet, and sidecar locks
workspace.json                               room record, layout: 2
rimz                                         stable room executable
```

The workspace id is `ws_` plus the first 24 hex characters of the SHA-256 of the canonical project root (`WorkspaceId::from_project_root`). Every root class (repo, marker, bare directory) derives it the same way, so adding a class never re-keys an existing store.

The directory is named by a `WorkspaceDirName`, `<basename>-<hex>`, never by the id: the root's sanitized basename plus a hex prefix of the id, four digits unless a prefix is already taken by another workspace, then two more at a time. `StatePaths::for_project_root` finds an existing directory or mints the name and creates nothing; lookup scans `ws/` for names whose hex prefixes the id, and a candidate's `workspace.json` decides, so a record naming another id is skipped. Only the root lookup adopts an unrecorded directory, matching the root's slug or the full fallback name; several such candidates are an error. A site holding only an id (`StatePaths::for_workspace`) resolves to the directory whose record names the id, otherwise to `ws-<24hex>`, never adopting an unrecorded directory. The runtime directory uses the same name (`RuntimePaths::for_state`), so both trees agree.

`Store::open` creates the state and runtime trees for commands that write, while reading commands take the non-creating handle from `Store::open_existing` (`store/mod.rs`), which returns `None` when no store exists.

This page owns the log, the caches derived from it, and the workspace record. The other files have their own homes:

| Files | Owner |
| --- | --- |
| `records/messages/`, `audit/messages/` | [messaging.md](./harness/messaging.md#storage-and-audit) |
| `audit/transcript/` | [transcript.md](./harness/transcript.md#the-log) |
| `owned/runs/` | [scripting.md](./harness/scripting.md#the-record) |
| `records/loop-instances.json`, `out/<reader>/wait-*.output` | [loops.md](./harness/loops.md#where-tasks-live) |
| `records/idle-stop.json` | [loops.md](./harness/loops.md#recovery-the-elder-runs) |
| `tmp/`, `shared/`, `owned/agents/` | [sandbox.md](./sandbox.md#temp-units) |
| Audit diagnostics | [diagnostics.md](./diagnostics.md) |
| Rebirth records and crash archives | [Session death](#session-death) below |

### The workspace record

Every writer stamps `layout: 2`; an absent stamp reads as layout 1. `rimz start` and `rimz reset` tear down a layout-1 room and replace it with a fresh room, without carrying over history. `Store::open` refuses it with those commands as the fix. Record scanners skip it, and `rimz gc --all` prunes it with its runtime tree.

`workspace.json` is the index maintenance commands read after a project root moves or vanishes ([`workspace/record.rs`](../../crates/rimz/src/workspace/record.rs)). It records the project root and its class, the active worktree root, the mux session name, the executable the room serves, and the room's provider accounts. Most commands re-record it freely, but two fields belong to owner flows and every other write preserves them.

The executable is a `(rimz_bin, rimz_build)` path-and-digest pair. Only `rimz start`, cwd-based `rimz attach`, and `rimz reload` set it, so a CLI call from another worktree cannot retarget a live room. A reader re-digests the target before executing it, so a missing or altered file leaves the running process serving. Staging and promotion belong to reload ([sidebar.md → Build promotion](./sidebar/sidebar.md#build-promotion)).

`logins` is a `<kind>: <name>` map of defaults for future launches, written at birth (`Store::record_room_logins`) and moved per kind by `Store::switch_room_login` through `rimz accounts use`. Both writers use the workspace lock; generic record writes preserve the map. An absent field means an unselected room, and every kind defaults to `default`; `rimz reset` clears the field. Birth still refuses a different recorded selection with `RoomLoginsFrozen`, directing the user to the explicit switch. A launch batch stamps each identity from its `LaunchLogin`: the current room default or an explicitly pinned name. Exec resolves that stamp, never rereading the room's default. Reopening a session whose stamp differs from the room's current default is refused (rebirth skips it), so a reopened session's stamp is always the current default. Ingress inherits a known parent's stamp, preserves an adopted batch stamp, and uses the room default only for hook-first roots. A record that exists but cannot be parsed fails the write instead of reading as absent. Switching emits no wakeup; the next sidebar heavy pass reads the new default.

`session_name` is the session the room was last born under. Resolution reads it from the record, falling back to the state directory name for an absent or unreadable record (with a warning for a read error); state-path errors still propagate. Scanning may canonicalize `project_root`, but never rewrites the session name. [`RoomContext::birth`](../../crates/rimz/src/room/birth.rs) joins a live recorded session unchanged. Otherwise it chooses the state directory name and records it through `Store::record_workspace`, preserving the owner and account fields. A failed session-list probe keeps the recorded name and the existing non-destructive birth policy. The schema is unchanged.

### The per-room runtime tier

The runtime tier is `${XDG_RUNTIME_DIR}/rimz/ws/<workspace-dir>/`, or `/tmp/rimz-<uid>/rimz/ws/<workspace-dir>/` when `XDG_RUNTIME_DIR` is unset. Everything here is disposable: it speeds the next read and dies with the session. The store's own entries are:

```text
sock/run.<short_run_id>.sock          per-run wakeup socket, bound by a supervised-run waiter
sock/sidebar.<short_instance_id>.sock per-instance wakeup socket, bound by each live sidebar
sock/codex-app-server.sock            the per-session Codex broker socket
live/heartbeat/sidebar.*.json         renderer liveness
live/read-marks/{sidebar.<id>,manual}.json read receipts
live/{agent_context,subagent_context}/ enrichment
live/agent-activity/                  activity hints
live/active-time/                     active-time accumulators
live/prompt/{task,sys}.<digest>.md     launch artifacts
live/agent-telemetry/copilot-otel.jsonl metadata-only Copilot export
live/idle-compact/<digest>.json       idle-compaction fire records
live/idle-stop/<digest>.json          idle-stop helper pacing records
live/{presence,client-presence-probe}.stamp probe freshness
lanes/{snapshot,agent-projection,workspace-projection}.json projections
lanes/{sidebar-width,sidebar-filter,unread}.json renderer state
lanes/{pane-topology,presence-desired,topology-writer-conflict}.json topology
lanes/{authoritative-pane-probe,focus-anchor}.json focus and pane truth
lanes/{diff-stats,cohort-spend,pipeline,pr-state,metrics-sample}.json enrichment
lanes/{loop-fire,message-wake,codex-daemon-reap,link-stats}.json coordination
lanes/{budget.scopes,budget.fleet-park,budget.<digest>,auto-continue.<digest>}.json session state
lanes/workspace-spending.<prefix>.json room spend projection
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

Liveness hints live apart from the store because `AF_UNIX` socket paths are short: `AF_UNIX_PATH_LIMIT` is 108 bytes on Linux and 104 on macOS, terminator included, and a path under the deep state tree would overrun it. The sockets set the location, and the heartbeats and receipts follow them. `RuntimePaths::for_workspace` validates the socket budget before any session side effect.

The runtime root is private. `ensure_private_runtime_dir` refuses to proceed on a symlink, a foreign owner, or group and other permissions it cannot strip, and that check is what keeps `agent-telemetry/` private through its ancestry.

Freshness here gates behaviour, so these files are scoped to one mux session incarnation. When a birth proves the previous session absent, it purges sidebar heartbeats before creating the replacement, so a fresh-looking heartbeat from a dead renderer cannot steer launch or reconcile decisions in the reborn room.

### Account-global caches

These machine-shared paths are outside room classes. Runtime `shared/` additionally holds `rate_limits_trace.jsonl`, provider probe markers, `board-write/` locks, account budget locks, and auto-redeem election locks. Persistent `cache/providers/` also holds `budget.account.<kind>@<account>.json` standing account choices and park state, and `auto_redeem.<key>.json` / `auto_redeem_rate.<key>.json` redemption state. These ledgers are not all rebuildable provider caches. `rimz gc` removes an account budget ledger no history pool reads ([Maintenance](#maintenance)). Room-scoped spending projections are instead `lanes/workspace-spending.<prefix>.json`.

Account-global provider caches live under `~/.rimz/cache/providers/` (`accounts.json`, `rate_limits.json`, `credits.json`, `provider-spending.json`, `spending.json`, `pricing-cache.json`), and their election locks and the spending service socket under `$XDG_RUNTIME_DIR/rimz/shared/`. The caches persist so the provider dashboard opens warm after a reboot, and those provider caches rebuild from the providers' own files (a re-probe, or one full spending walk for the cursor); the locks are runtime because they mean nothing once their holder is gone. `RuntimePaths::ensure_dirs` removes stray copies of those data files from the runtime `shared/` directory so they stop pinning tmpfs. What each file carries is [state.md → Published lanes](./sidebar/state.md#published-lanes), [providers.md](./agents/providers.md), and [spending.md](./agents/spending.md).

## The event log

`log/events.log.jsonl` is the canonical history of everything that happened in the workspace.

### Framing

Each record is one newline-terminated line: `<payload length> <crc32, 8 lowercase hex> <json payload>`. The CRC covers the payload alone, and the length is validated structurally on read. A bare JSON line without the length and CRC also decodes, because a payload always opens with `{` and cannot be mistaken for the hex token ([`event_log/frame.rs`](../../crates/rimz/src/store/event_log/frame.rs)). That prefix also means a bare `jq` over the file fails; strip it first to read frames by hand:

```sh
sed -E 's/^[0-9]+ [0-9a-f]{8} //' log/events.log.jsonl | jq 'select(.method == "agent.lifecycle")'
```

A full envelope is long, so to follow one agent, run, or message through the log, match its id and project the three fields that say what happened and when:

```sh
rg -a <id> log/events.log.jsonl | sed -E 's/^[0-9]+ [0-9a-f]{8} //' | jq -r '[.timestamp, .method, .params.event_name // ""] | @tsv'
```

The payload is an `EventEnvelope`: schema version, event id, workspace id, session name, mux name, source and source kind, method, timestamp, and a method-specific `params` blob kept as raw JSON, so a reducer parses only the events it folds.

### What is in it

`method` is the discriminator, and [`EventKind`](../../crates/rimz/src/store/event.rs) is its typed decode.

| Method | Carries | Folded into |
| --- | --- | --- |
| `agent.lifecycle` | One lifecycle signal plus the observation around it: session id, pane stamp, status, turn, context, tokens, subagents. | The agent rollup ([model.md](./agents/model.md)). |
| `agent.attached` | Resume identity and placement: provider session id, stable RimZ launch id, pane stamp, runtime owner, isolation, the process `login` (`default` spelled out), and optional launch `record`, `tier`, and `mode`. A spawning wrapper writes two: its own owner before the spawn, the provider's pid after it. | Re-stamps identity, placement, and present isolation. A present `login` replaces the row's account stamp (`default` clears it); an event written before the field leaves the stamp alone. With a record, replaces record and tier (absent tier clears it), and sets present mode; without a record, leaves all three unchanged. An identified attach for an unseen session seeds its row before the provider starts. |
| `agent.launch_warnings` | Warnings the exec wrapper printed for one launch, keyed by kind and agent id as `agent.attached`, with the launch id when known. | Replaces the existing row's `launch_warnings`; an empty list clears it. Never creates a row. |
| `agent.launched` | The launch RimZ performed: identity, profile, role, team, worktree, permission mode, optional `record` (verbatim model, effort, agent base), optional `tier` stamp (requested tier, listed model, upward `used_tier`, usage `skipped` reasons), and a `Starting`, `Bound`, or `Failed` state. | Launch admission and resume posture. Record and tier survive model observations and hot lifecycle events; old events omit them. |
| `message.*` | One of eleven message outcomes: `queued`, `edited`, `after_met`, `when_met`, `sent`, `delivered`, `timed_out`, `errored`, `canceled`, `abandoned`, `archived`. `message.removed` decodes as `canceled`. | The message audit trail ([messaging.md](./harness/messaging.md)). |
| `signal.emit` | One `SignalEventPayload`: name, top-level payload object, and source (`cli`, `watch`, `lifecycle`, `forge`, or `team`). | Nothing in the rollup. It is the durable trace of an event ingress, replayed by `rimz events follow` ([loops.md](./harness/loops.md#the-signal-vocabulary)). |
| `session.rebirth` | Nothing: it is a boundary marker. | Clears every pane stamp recorded before it. |
| `session.death` | `cause` (`reboot` or `crash`) and the agents lost with the previous incarnation. | [Session death](#session-death). |
| anything else | Raw params: `feed.*` and `event.emit` frames, methods from a newer binary, and any known method whose params fail to decode. | Nothing, by design. |

The catch-all `Other` arm is what makes a downgrade safe. A record written by a newer RimZ decodes as `Other` and survives every fold, so an older binary reads an intact log instead of a corrupt one.

Message records get the same tolerance outside the log. A record's `sender` can carry a `Harness` notice that a newer binary minted, and `HarnessNotice::Other` keeps that string verbatim, so an older binary's queue rewrites and history retention pass it through. Without it, one such record in `audit/messages/<bucket-start>.jsonl` would fail every history-touching command of the older binary (`message cancel`, `message list`, retention), and one in `records/messages/messages.jsonl` every queue transaction. Worktrees of a project share one workspace store, so mixed binaries are the ordinary case. Nothing dispatches on an unknown notice: it takes generic harness delivery and renders its own name as the message `Type` ([messaging.md → The message header](./harness/messaging.md#the-message-header)).

`session.rebirth` clears pane stamps and never ends a session. A reborn mux session renumbers panes from zero, so every stamp recorded before the boundary names a pane that no longer exists, and clearing them keeps a prior incarnation's session off a reused pane id. Each resumed wrapper then appends `agent.attached` to re-establish its launch identity, placement, and owning process. An attach event without a launch identity cannot create a row.

The fold shares one unstamped carryover base per file identity. At rebirth it hydrates continuing carried rows into the active fold, assigning observed live rows the low ordinals before fitting continuing rows around them. Ended carried-only rows keep their names but have no pane or ordinal; an absent ordinal is omitted on disk, not written as null. Backfilled identities that the reducer has not assigned remain provisional in the v27 rollup cache, so a later delta produces the same handles as one cold fold, even if the prefix only updated warnings. Rotation commits the projected ordinal high-water mark for the next generation.

Lifecycle records inherit from the rollup instead of repeating themselves. High-cadence progress events omit `transcript_path`, worktree, pane identity, role, team, channel, profile, and the smart-compact stamp, and the reducer carries them forward from the prior row. Missing optional keys decode as absent, and `runtime_owner` is serialized on lifecycle records when present; the reducer carries a prior owner forward when it is absent. That keeps the hot log compact under a busy fleet.

Resident loop launches stamp `LaunchParams.loop_task` on the prompt leader before opening its pane. The reducer carries it into `AgentState.loop_task`, through provisional adoption, subsequent observations and event-log rotation, like the team identity. Registration reads that durable task name to arm subscriptions, not the launch ledger. The additive field defaults to absent in old records; snapshot version 32 and rollup-cache version 26 rebuild pre-field caches.

### Crash recovery

Only the trailing frame can be in flight when a process dies, because each append is one `write()` under the workspace lock.

A reader that hits an undecodable frame at the end of the log stops in front of it and reports an extent that does not claim those bytes. An unterminated tail is an in-flight append that the next wakeup will cover, logged at debug. A terminated but torn tail is left by a power cut, logged at warn, and skipped.

A bad frame behind a good one is real corruption, and the read fails loudly instead of silently dropping everything after it. [`repair`](../../crates/rimz/src/store/event_log/recovery.rs) is the deliberate recovery: it truncates from the first invalid frame to end of file and reports the frames kept and bytes cut. The publish tail calls it when its fold hits corruption, and `rimz gc` calls it on demand.

Lock-free `O_APPEND` would remove the lock cost but let writeback reorder and tear a frame in the middle of the file, where repair can only cut from that frame onward and lose every good record behind it. [performance.md → Deferred and rejected](./performance.md#deferred-and-rejected) records why that trade stays rejected.

### Rotation and carryover

Rotation syncs the active log, renames it to `log/archive/events.<uuidv7>.jsonl`, syncs the directory, and lets the next append start a fresh log. UUIDv7 names sort chronologically, so the archive needs no index ([`event_log/rotation.rs`](../../crates/rimz/src/store/event_log/rotation.rs)).

Rotation starts two ways. A lifecycle append that leaves the log at or above `DEFAULT_EVENT_LOG_ROTATE_BYTES` (64 MiB) claims rotation inside the write lock, debounced by `cache/auto-rotate.stamp` (`AUTO_ROTATE_DEBOUNCE`, 60 seconds), and the lifecycle CLI then spawns a detached `rimz workspace rotate-events`. The same command is the manual entry point: `--max-bytes` overrides the threshold and `--archive-older-than` prunes archives past `DEFAULT_RETENTION` (14 days).

Rotation never changes what the audit rollup remembers. Before the rename, `Store::rotate_event_log` merges every agent in the rotating log's audit rollup into `records/agents-carryover.json`, ended sessions and exited owners included. It then prunes carryover rows older than the retention window and reseeds the fold base. The first post-rotation event for a continuing agent reduces against its carryover row, so the event schema's lifetime-field policy keeps launch identity, parentage, and enrichment exactly as if the log had not rotated, and `last_seen` resolves across carried and live rows.

Staging folds against a fresh, untrimmed read of the raw carryover, never the parse cache's text-trimmed projection. It uses the ordinary backfilled merge base while preserving prompt and pane-command text. The in-memory representation is not the record; trimming changes neither the carryover schema nor its prompt and pane-command text.

## The write path

Ordinary mutations run one choreography through `Store::commit` in [`writer.rs`](../../crates/rimz/src/store/writer.rs).

Under the workspace lock:

1. Read whatever state the decision needs.
2. Write the durable record files the mutation touches.
3. Append the event frames, one ordered batch per logical change.

Then the lock releases. When the mutation appended an event or forced a publish, the tail runs off-lock:

4. Send a `store_delta` wakeup to every fresh sidebar, one per appended event.
5. Group-sync the log with one `fdatasync`, at most once per `LOG_SYNC_INTERVAL` (1 second).
6. Publish the snapshot checkpoint, when due.
7. Reap provably dead sessions, at most once per `REAP_INTERVAL` (60 seconds).

Steps 5 through 7 are gated by stamp files beside the lock: `log-sync.stamp`, `publish.stamp`, `dead-reap.stamp`. A missing, unreadable, or future-dated stamp reads as due, so clock and I/O uncertainty costs one redundant run instead of a skipped one. The stamps keep the write path O(1) over log history: a fleet appending hundreds of events a second still pays one fsync and at most one checkpoint per interval, however long the log has grown.

Wakeups go out before the publish on purpose. Consumers fold the log tail from their own cursor, so checkpoint cadence tunes cold-start latency and never gates freshness.

Mutations that replace, cut, or forget the active log run `Store::commit_boundary` instead. It holds the workspace lock and then the publish lock, runs the mutation, and when the mutation reports a `RollupInvalidation`, deletes `cache/snapshots/latest.json` and the publish stamp before touching the rollup cache. A crash in between leaves readers folding for themselves instead of trusting an offset that could alias into a regrown log.

| Mutation | Policy | Rollup cache | Rebuild |
| --- | --- | --- | --- |
| Rotation, identity rewrite, soft reset that archived a log | `Reseed` | reseeded as a new generation | yes |
| Repair that cut bytes | `Drop` | deleted | yes |
| Soft reset of an empty log | `Keep` | kept | yes |
| Hard reset | `Forget` | deleted | no |

`Store::prune_carryover` takes the same two locks and rebuilds when it removed rows, without retracting anything, because the log's extent did not change.

### Write classes

Every disk write falls into one of four classes, and one line sorts them: **durable records and cold metadata fsync; hot appends and disposable caches do not.** A cache rebuilds, and a group sync or an audit tolerance bounds what an append can lose.

| Class | Files | Discipline | After a power cut |
| --- | --- | --- | --- |
| Event log | `log/events.log.jsonl` | One CRC-framed `write()` per record or ordered batch. The off-lock tail issues a group `fdatasync` at most once a second, and rotation syncs before the rename. | Intact through the last group sync. The trailing window can be lost, and the frame CRC turns a torn suffix into deterministic corruption that repair truncates. |
| Audit appends | `audit/messages/<bucket-start>.jsonl`, `audit/transcript/*.jsonl`, `out/<reader>/<name>.output` | `O_APPEND`, no per-record fsync. History and transcript append under the workspace lock. A queue transaction commits `messages.jsonl` before it appends history and event frames, so a history append failure warns and never undoes the queue transition ([messaging.md → Storage and audit](./harness/messaging.md#storage-and-audit)). A wait log takes no store lock: `rimz wait` creates it at arm time, and the one watcher holding that wait's `locks/loop-watch-<name>.lock` is its only writer after that. A check watcher truncates and rewrites it for each run, keeping only the latest output ([loops.md → Watched commands](./harness/loops.md#watched-commands)). | Trailing records can be lost. The cost is history completeness, never queue correctness. For wait output, the run record keeps the last 4 KiB, and the delivered message carries the file path, estimated tokens, and line count; a file pattern match also includes a matched-line preview. |
| Cache write | `cache/snapshots/*.json`, `records/live-roster.json`, heartbeats, sidecars, the sidebar's published lanes | Temp file plus atomic rename, no fsync. The roster is the named best-effort records exception, not rebuildable history. | Caches rebuild or refresh; loss of the roster's latest write can narrow recovery. |
| Durable records | `records/messages/messages.jsonl`, `owned/runs/<run_id>.json`, `workspace.json`, `records/agents-carryover.json`, `records/loop-instances.json`, `records/idle-stop.json`, trust grants, notification handlers, hook installs | Temp file, fsync, rename, parent-directory sync. | Survives. |

Every fsync call funnels through [`disk/atomic.rs`](../../crates/rimz/src/disk/atomic.rs), and no module hand-rolls its own temp-file dance. The `cargo xtask invariants` check `ensure_store_durability` rejects a `sync_all` or `sync_data` method call anywhere else; it matches those two std methods only, so a raw `libc` or `nix` fsync would pass the grep and has to be caught in review.

A rename over a path replaces a symlink there with a regular file; resolve the link first to write through it.

### Wakeups

After a commit, the writer calls `wake_store_delta` in [`wakeup/mod.rs`](../../crates/rimz/src/wakeup/mod.rs), the shared leaf wire below this module. It walks the runtime heartbeat directory and sends a typed `store_delta` datagram to each sidebar whose heartbeat is no older than `SIDEBAR_HEARTBEAT_TTL` (5 seconds), re-statting each heartbeat just before the send to close the window where a renderer exits between read and write.

Sends are non-blocking, so a full receiver queue drops the datagram and the write moves on. Per-target failures are absorbed; only a failure to read the heartbeat directory reaches the writer, which logs it. A consumer closes any missed wakeup at its next tick (`rimz sidebar serve --tick-seconds`, default 1 second). The envelope and its event taxonomy belong to the receiving side, [state.md → Realtime events](./sidebar/state.md#realtime-events).

Supervised-run waiters are woken separately. `wake_run` in [`run.rs`](../../crates/rimz/src/store/run.rs) sends a terminal datagram to the run's [wake socket](./harness/scripting.md#the-wake-socket); `harness::run` and the CLI call it after they write a terminal run record, and store reset calls it for the runs it cancels.

## The read path

Reads take no lock. `snapshot::rebuild` and the [`snapshot/assemble.rs`](../../crates/rimz/src/store/snapshot/assemble.rs) entry points fold the log into the agent rollup, then project that into the sidebar view-model ([sidebar.md](./sidebar/sidebar.md#from-store-to-screen) owns the projection).

The fold is resumable. `cache/snapshots/rollup.json` holds a fold base and the `LogExtent` it reflects; `cache/snapshots/latest.json` holds the published view-model and the extent it was built from. A reader trusts either checkpoint exactly when its extent matches the live log, and otherwise folds the missing tail itself. A cold reader pays one full fold, a warm reader pays only the new bytes, and neither can be wrong ([`snapshot/fold.rs`](../../crates/rimz/src/store/snapshot/fold.rs)). The rotation carryover beneath every fold is parsed once per file identity per thread: rotation and prune replace it by atomic rename, which changes that identity, while the maintenance writers themselves always read the raw file.

The cached carryover keeps the first non-blank line of `first_prompt`, capped at 160 characters without splitting a character, and clears `prompt`, `recent_prompts`, `pane.foreground_cmdline`, and `pane.spawn_command` only on rows whose `ended_at` is present. Pane identity stays intact; continuing carried rows and rows reduced from the active log stay whole. An ended carried child retained in the runtime projection keeps this bounded fallback label after the usual description and task precedence. The session-name prefix filter compares against the retained text: a name longer than the prefix is no longer filtered out as merely its prefix. A delta targeting an ended carried row, its compaction predecessor, or a compact-command receiver restores that row's text from one raw carryover read per fold.

`crates/rimz/src/store/mod.rs::Store::load_full_agent` is the explicit lock-free read for full historical text. It restores only those five fields onto an ended carried observation with matching `(kind, agent_id)`, `ended_at`, and `last_seen`, provided the active log contains no same-key agent event. The log check distinguishes new observations even when their timestamps are reused. It reads raw carryover at most once and leaves other input rows unchanged, without resurrecting a cleared pane. `crates/rimz/src/cli/agents_cmd/mod.rs::resolve_live_or_audit` uses it for live hits as well as the audit fallback on the explicit show, history, fork, and explain paths; a non-ended hit reads no carryover, and audit enumeration keeps the lightweight projection.

The publish gate in [`writer/publish.rs`](../../crates/rimz/src/store/writer/publish.rs) decides when the write tail refreshes those files. A checkpoint is due when the stamp is `PUBLISH_INTERVAL` (1 second) old, when the unpublished tail reaches `PUBLISH_BYTE_BUDGET` (64 KiB), or when the log is shorter than the stamp's offset. Concurrent publishers serialize on `locks/publish.lock`, so they group-commit.

`Store::snapshot_cached` serves `latest.json` when its extent is current and re-projects otherwise, then attaches the agent context sidecars for the projected agents. That gives every address resolver the same provider rest evidence (a provider error or a completed or interrupted settle) that pane ownership uses. The pure snapshot reducer reads the store alone, and a missing sidecar leaves the raw lifecycle status authoritative.

### Runtime and audit

The same durable rollup answers two questions, and [`runtime.rs`](../../crates/rimz/src/store/runtime.rs) is the filter between them.

**Runtime** scope filters at read time and backs the default views: `rimz sidebar snapshot` and the plain `rimz doctor` agent summary. It hides rows carrying `ended_at` and rows whose `runtime_owner` is no longer the live process that wrote them, and keeps a launched child visible while its parent is. Ended and expelled identities ride the published snapshot as `fenced_sessions`, which stops provider-local session rebinding from reviving them. `runtime_owner` records the owner kind, a stable subject id, the pid, and on Linux the process-start token, so a reused pid does not read as the original owner. A row with no owner stays visible; a known-dead owner or a process-start mismatch is hidden while the reap converges it to a real end stamp.

**Audit** scope bypasses the filter and reads durable history as written. `rimz doctor --audit`, explicit resume, launch name allocation, and message dispatch read it, and explicit resume is the reason ended rows are retained at all.

Pick the scope by the question, because the wrong one is silent. `Store::snapshot_cached` and `Store::runtime_projection(RuntimeScope::Runtime)` answer "who is here now". `Store::runtime_projection(RuntimeScope::Audit)` answers every question where an ended session is itself the evidence — a liveness gate, a retirement predicate, a resume check. Asking the runtime scope whether a session has ended returns a clean empty answer, since the row that carries the proof is the one that scope removed.

## Session death

Two mechanisms end sessions in the store. The reap converges individual sessions RimZ can prove are gone; the `session.death` record captures the loss of a whole room.

### The reap

The write tail's debounced reap (`writer/reap.rs`) scans the audit rollup and appends an `agent.lifecycle` `Ended` observation for each root session it can prove is finished. The event name records the reason, checked in this order:

| Event name | Condition |
| --- | --- |
| `ReapedSuperseded` | A newer session took the same slot (`session_death::supersedes`): a relaunch, a fresh replacement, or a rested owner replaced in place. |
| `ReapedDead` | The recorded owner process is provably dead. |
| `ReapedStale` | The session never captured an agent or script pid, or its owner is the shared daemon, and it has been quiet past `GHOST_SESSION_TTL_SECS` (3 hours). |

Before evaluating supersession, the reaper attaches the context sidecar of each running or waiting root, so a current provider error or a completed or interrupted settle can certify the owner rested. A missing sidecar leaves the raw lifecycle status authoritative.

An agent named in `records/live-roster.json` is exempt from `ReapedDead` and `ReapedStale`, but `ReapedSuperseded` still applies. A root agent named in `records/pending-recovery.json` is exempt from every reap reason and cannot supersede another agent, so a parked candidate survives the scan until a settlement resumes or ends it, however long the user stays away. Pending launched children have no such shield: recovery never resumes them, so ordinary reap reasons and supersession still apply. The old exec wrapper infers no end for a pending agent ([fleet.md → Reclaiming a pane](./harness/fleet.md#reclaiming-a-pane)). Already-ended rows are skipped and cannot supersede an active replacement. Outside the reap, `Store::retire_worktree_sessions` appends the same `Ended` observation as `WorktreeRemoved` for non-live sessions of a removed worktree.

The reducer stamps `ended_at`, which hides the row from runtime views at once while the audit rollup keeps its resumable identity until rotation prunes it at the retention boundary. The stamp suppresses the row without making it terminal: any later lifecycle event under the same `(kind, session id)` clears it, so a session that reports in again returns to the runtime view under its own identity and every fence keyed on the end stamp releases with it. Runtime expel and the snapshot view reap apply the same predicates immediately, so the screen agrees with the log before the durable append lands.

### The session.death record

When a room comes back after its mux session died, the rebirth path appends `session.death` for the lost incarnation, carrying `cause` and the agents that went with it. The cause is `reboot` when the host boot id in `records/boot.json` changed. A same-boot `crash` additionally requires `records/live-roster.json` to name agents worth recovering.

`records/live-roster.json` is the sidebar producer's last pane-backed live agent set, including children: the agents that mux session would lose if it died. It is the one records-class file written best-effort by the producer (`store/live_roster.rs`): nothing rebuilds it, but a lost write only narrows one recovery. The producer removes an agent from it only while the mux still lists its session, so a sidebar that outlives the session cannot empty it ([sidebar.md → Resume-on-rebirth](./sidebar/sidebar.md#resume-on-rebirth)). It is intersected with the audit rollup at birth, so cleanly ended agents and paneless ghosts stay out of recovery. It means only "the current incarnation's live set": every birth whose session did not exist (inspected, inspection failed, or supervised) parks the roster's agents in `records/pending-recovery.json` before creating the session, then deletes the roster at the boundary. A failed park fails the birth before the session exists, leaving the roster intact for a retry; the new producer cannot overwrite an unparked lost set. The reap reads the roster before pending, so it cannot miss an agent moving between them.

`records/pending-recovery.json` (`store/pending_recovery.rs`) is a version and a set of `(kind, session id)`: the lost agents nobody has decided about. Boundary and settlement are read-modify-write from separate CLI processes, so both take the workspace lock and publish durably; an absent or unreadable record reads as empty. A settlement plans over the record plus the roster at a birth, and over the record alone in a live room, keeping the agents that are neither ended nor live. An agent leaves the record once the tab that resumes it is confirmed open, its exec wrapper attaches it to a pane ([fleet.md → Reclaiming a pane](./harness/fleet.md#reclaiming-a-pane)), or its ended trace is durable; a resumed agent whose tab did not open stays in it, and a settlement also drops entries ended by other means. Reset and class GC keep the record wherever they keep the roster.

`records/last-death.json` records the incident for cheap `rimz list --all` display, and the reborn room writes back how many lost agents came back in a tab confirmed open. A user decision at an interactive start writes a lost session's durable `Ended` trace: `rimz.recovery-declined` when the user declined recovery (an answered no, `--no-resume`, or `resume.on_rebirth = false`), `rimz.not-resumed` when the user dropped the agents a recovery could not resume (`rimz.worktree-gone` for a missing checkout). Each trace drops the session from the next recovery set and keeps it resumable by hand. A settlement writes two ended traces unattended: `rimz.child-not-resumed` for a non-live child that recovery never resumes, after canceling its newest open run, and `rimz.seat-refilled` when a live member already holds the parked session's matched team seat or its planned Fresh replacement's tab is confirmed open. Each child end emits an informational `recovery_child_ended` diagnostic. The ended stamp is durable before the key leaves pending; a failed append or an unconfirmed replacement tab keeps its old session parked. A crash birth also archives mux forensics under `audit/crashes/<utc-ts>/`, best-effort and never blocking launch, bounded by the audit class sweep rather than a write-time ring. `rimz doctor` shows the last incident with its cause, time, lost and recovered counts, and archive path.

The recovery flow from roster to repopulated panes is [fleet.md → Resume and rebirth](./harness/fleet.md#resume-and-rebirth) and [sidebar.md → Resume-on-rebirth](./sidebar/sidebar.md#resume-on-rebirth).

## Maintenance

For layout-2 rooms, `rimz reset` cancels active runs, clears the room account selection, removes `records/idle-stop.json` (a soft reset leaves its agents resumable, and a surviving request would stop the resumed session), stages carryover on a soft reset, and rotates the log. It clears `cache/` (including handleless skill copies) and runtime `sock/`, `live/`, `lanes/`; each runtime directory is renamed before recursive deletion so late writers cannot refill the detached tree. State directories constructed by RimZ that a reset removes are detached the same way under the workspace lock, renamed to the sibling `<dir>.reset` before they are counted and deleted: an entry written to the canonical path after the detach survives the reset, and a `<dir>.reset` an interrupted reset left behind is removed, uncounted, by the next reset of that directory. The rename does not detach a process inside a sandbox view, whose `/tmp` is a bind mount of the temp unit directory itself and follows it to `tmp.reset/`. So a hard reset, after teardown and before it opens the store, ends every process that still has one of the room's temp units as its `/tmp`, and refuses with each survivor's pid when one outlives the kill; the store is untouched and a rerun completes ([temp units](./sandbox.md#temp-units)). The `.reset` suffix is reserved: hard reset treats any `owned/` child directory ending in `.reset` as leftover debris, removing it in place and uncounted rather than detaching it again. It never removes state `locks/`. Soft reset keeps audit, owned state, `tmp/`, `shared/`, `out/`, and every other record, including standing fleet budget choices. `--hard` additionally removes the active log, carryover, `audit/`, `owned/` except `owned/runs/`, `tmp/`, `shared/`, and `out/`; it keeps the log archive just written. Canceled run records stay loadable for waiters until terminal-run GC reclaims them after its seven-day grace. Provider-owned sessions remain outside this boundary. Teardown removes disposable runtime classes and no state class: `tmp/`, `shared/`, and `out/` are left for GC. Allocating a handle to a new agent row removes any `tmp/<handle>/` and `out/<handle>/` an earlier holder of that name left behind; the removal runs after the launch commits, outside the workspace lock, and a failure logs a warning rather than failing the launch.

`rimz gc` applies the class table to the current room through `store::gc::collect_room`; `--all` uses `collect_classes` for every known room, after pruning dead workspaces so removed stores get no class block. Text and JSON `rooms[].classes[]` carry per-room/per-class counts and bytes, with top-level `scope`. Both scopes resolve once through `WorkspaceResolver::resolve_participant` and use `open_existing_store`; an absent state root gets no class block or scaffold. A layout-1 current room refuses either scope with the layout error, which names `rimz start` or `rimz reset` as the fix. `--all` removes other layout-1 rooms and their runtime trees as `incompatible_layout`, and `rimz list` omits them. `--dry-run` plans removals without deleting files; locks instead report `locks_would_check`, without acquiring them or claiming removal. `--older-than` controls live and orphan atomic-write temp TTL, not owned or audit retention.

Room scope sweeps orphan atomic-write temps only under its state and runtime roots, and reaps only its instance rows through `TaskCatalog::reap_dead_deliveries_for`. Machine scope also prunes provably dead workspaces, shared stale provider probes, temps across both whole roots, machine delivery tasks and every root's instance rows. Both scopes prune landed worktrees only in the current repository. In the current workspace they repair the event log, archive orphaned messages, reconcile stale sent messages, and prune carryover past 14 days; these mutating maintenance operations are skipped for a dry run. Overlay cleanup is the deliberate shared-path exception: both scopes keep `TaskCatalog::load(Some(root)).prune_orphan_overlays()`, pruning keys for the root's known scopes in shared `loops_dir()` files, not moving overlay storage into the room. The account budget ledgers are a second shared-path exception: both scopes call `harness::budget::sweep_orphan_account_ledgers`, which removes from `cache/providers/` each `budget.account.<component>.json` no history pool reads, a kind-only name or the own key of a declared account whose pool is another key, with its lock in runtime `shared/`. An `@`-named ledger stays when the accounts config fails to load, and so does the ledger of an undeclared name or of a standalone account. It never waits on a ledger lock: it try-locks, unlinks the ledger and then, whatever became of the ledger, the lock while it holds it, and leaves both for the next run when another process holds the lock. The ledger is a cache-class write, so the unlink takes no fsync; an error on one ledger is a debug line and keeps that ledger. A dry run counts without locking.

The elected sidebar producer remains the sole automatic trigger, once daily per room after a five-minute settle. The helper normally runs room scope; `harness::auto_gc::claim_machine_sweep` elects one machine sweep per 24 hours using a stamp check, a nonblocking lock, and a locked stamp recheck. It holds the guard through the sweep and stamp write. `disk::paths::machine_auto_gc_stamp` names `cache_dir()/auto-gc.json`; `RuntimePaths::shared_auto_gc_lock` names the shared runtime `auto-gc.lock`. Room and machine stamps use `{ "swept_at": <Timestamp> }` and record failed attempts too. Closed rooms wait for the machine sweep. Assist records include `scope` and the helper's own room's `class_bytes`; other totals cover the full selected scope. See [automatic sweeps](../reference/cli/maintenance.md#automatic-sweeps).

## What survives what

Soft reset keeps `records/`, `audit/`, `owned/`, `tmp/`, `shared/`, `out/` and archived logs; hard reset also drops audit, owned state except `owned/runs/`, `tmp/`, `shared/`, `out/`, and carryover. Both keep state lock inodes. Reboot drops runtime, not state. Teardown drops runtime, not state. Age-based GC is independent of these boundaries.

| Event | Store | Live sockets and heartbeats | Multiplexer session |
| --- | --- | --- | --- |
| Detach | yes | yes, the mux server stays alive | yes |
| Sidebar reload | yes | socket rebound on attach | yes |
| Multiplexer server crash | yes | no | no |
| Host reboot | yes | no | no, needs a host supervisor (tmux-resurrect, Zellij resurrect, systemd) |
| Host power cut | yes, through the last group `fdatasync`; repair truncates any torn suffix ([write classes](#write-classes)) | no | no, needs a host supervisor |

RimZ guarantees the store across all of these, and at a power cut that guarantee runs through the last group sync. The session and its processes survive only what the multiplexer server and the host supervisor keep alive.

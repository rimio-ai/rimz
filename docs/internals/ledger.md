# Ledger and bridge

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

The ledger is the source of truth. The bridge is the optional blocking path that carries decisions back to a waiting hook or script. Correctness lives here; everything else (sidebar, notifications, agent UIs) reads through it.

## The three paths at a glance

```text
Default path                Bridge path                 Script path
(no fresh resolver)         (resolver enrolled)         (rimz feed ask)

agent hook fires            agent hook fires            script calls feed ask
        │                            │                            │
        ▼                            ▼                            ▼
write feed item             write feed item             write feed item
surface = native_ui         surface = bridge            surface = script
        │                            │                            │
        ▼                            ▼                            ▼
wake sidebars               bind per-request socket     bind per-request socket
return neutral no-op        wait up to hook cap         wait up to --timeout
exit                                 │                            │
        │                            ▼                            ▼
        ▼                  resolver answers (CAS)      human/resolver answers
agent's own UI asks         hook prints agent JSON     script unblocks
human focuses pane          (or times out →            (or timeout → expires)
                             native fallback)
```

The wire-level signal that distinguishes the three is the `surface` field on the feed item. Sidebar rendering, feed-verb gating, and resolver matching all read it.

## Surfaces

The `native_ui` / `bridge` / `script` vocabulary is defined in [DESIGN.md → The three operating paths](../../DESIGN.md#the-three-operating-paths). This doc describes how each path moves through the ledger; the table of which path holds the hook open and where the answer comes from lives there.

`rimz feed resolve` is valid only for `bridge` and `script`. `rimz feed dismiss` is the local acknowledgement path for `native_ui` — it never reaches the agent.

## Durable state

Under `${XDG_STATE_HOME:-~/.local/state}/rimz/workspaces/<workspace_id>/`:

```text
workspace.json
events.log.jsonl
events.log.archive/events.<uuidv7>.jsonl
agents.carryover.json
snapshots/latest.json
snapshots/rollup.json
feed/<request_id>.json
feed/terminal/<request_id>.json
locks/workspace.lock
locks/publish.lock
locks/abandon-sweep.stamp
locks/log-sync.stamp
locks/publish.stamp
```

### Write classes

Every disk write belongs to one of four classes, and the classification rule is stated once: fsync leaves the hot path; cold paths keep it even where the same-host argument would allow relaxing, because removing it there buys nothing. Every fsync syscall funnels through `ledger/atomic.rs` (CI grep), so the contract is enforced, not reviewed.

| Class | Files | Write discipline | After a power cut |
| --- | --- | --- | --- |
| Event log | `events.log.jsonl` | one `write()` per CRC-framed record, no per-record fsync; the off-lock write tail issues a group fdatasync debounced to at most one per second (`locks/log-sync.stamp`), and rotation syncs the file before the rename | intact through the last group sync. The loss window is up to one debounce interval of trailing events under sustained load — decision events (`feed.resolve`) included, not just observational ones — and a final pre-quiescence tail additionally rides kernel writeback (~30s default), with the frame CRC turning any lost writeback into deterministic corruption that repair truncates. A lost resolution is benign by construction: the power cut killed its waiter too, so the resurrected pending ask is expelled at read time and durably abandoned by the sweep |
| Coordination | `feed/*.json`, `feed/terminal/*.json` | temp file + atomic rename, no fsync | a lost item file costs at most the audit-completeness of its final window; the dead-owner expel abandons any ask whose waiter died with the machine |
| Cache | `snapshots/latest.json`, `snapshots/rollup.json`, heartbeats, sidecars | temp file + atomic rename, no fsync (`write_temp_then_rename_cache`) | rebuilt from the log on the next read |
| Cold path | `workspace.json`, `agents.carryover.json`, trust grants, resolver allowlists, hook installs | temp file, fsync, rename, parent-dir sync (`write_temp_then_rename`) | survives |

Rules:

- `<workspace_id>` is `ws_` plus the first 24 hex characters of the SHA-256 of the canonical root path — the same derivation for every root class (repo, marker, directory), so introducing a class never re-keys a ledger.
- `workspace.json` records the last known project root, root class, and session name for maintenance commands; feed files and the event log remain the request source of truth. A record predating the `root_class` field decodes as `repo` and self-heals on the next start/attach re-record. Launch reads the prior record before overwriting it: when the derived session name diverges from the recorded one and a session still answers to the recorded name, launch retires that session and rebirths the workspace under the new name, so a changed derivation never strands a live session or its sidebar.
- Feed files are CAS coordination state: the CAS re-checks under the workspace lock, same-host writers always read their own renames, and the event log carries the durable audit trail, so rename atomicity is the whole durability requirement (coordination class).
- Resolutions take the workspace lock, then CAS on `status = pending`. First valid writer wins.
- A feed item that reaches a terminal status (`resolved`, `timed_out`, `abandoned`) relocates into `feed/terminal/` with an atomic rename under the same lock, so decision-path scans stay O(pending). Audit reads (`feed list --audit`, `feed show`) span both directories.
- `events.log.jsonl` uses length-plus-CRC framing (`<len> <crc32> <json>`), the CRC computed over the JSON payload; pre-CRC frames still decode, so old logs fold cleanly. The event log and the feed files are the crash-recoverable truth; everything under `snapshots/` is a reconstructible cache.
- A torn trailing record at SIGKILL is skipped on rebuild and logged. A corrupt frame *behind* later frames hard-errors every read; the next write tail (or `rimz gc`) repairs by truncating at the first invalid frame and republishing from the surviving prefix.
- The workspace flock makes the log single-writer-at-a-time, and recovery is built on that: only the *trailing* frame can ever be in flight, so a torn frame anywhere earlier is corruption and rebuild fails loudly rather than silently dropping the events behind it. This is load-bearing — lock-free appends (`O_APPEND` from concurrent writers) would let writeback reordering tear a *middle* frame after a crash, and are unsafe until the framing grows per-frame magic for resync (the CRC validates a frame; it cannot relocate the next boundary past a torn middle). See the rejected-candidates list in [performance.md](./performance.md#deferred-candidates).
- Rollup reads are lock-free. The snapshot resumes from the persisted fold base in `snapshots/rollup.json` and folds only the log bytes appended since, so a writer's half-written trailing frame is simply not folded until it completes — it can never drop a previously-folded event. The write that completes the frame posts the wakeup that folds it.
- `rimz workspace rotate-events` archives the active log into `events.log.archive/events.<uuidv7>.jsonl` once it exceeds the operator-supplied byte threshold (default `64MiB`); UUIDv7 filenames sort chronologically. The same command prunes archives older than `--archive-older-than`.
- Before rename, the agent rollup of the rotating log is merged into `agents.carryover.json`. The snapshot reducer loads carryover and lets newer in-log observations override; this keeps the sidebar's agent panel correct across rotations without rescanning archives. Rotation also bumps the rollup generation and reseeds `snapshots/rollup.json` from the carryover, so incremental readers detect the boundary and never fold across it.
- Every feed file carries `workspace_id`, `request_id`, nonce, resolver id, and timestamps.
- `snapshots/latest.json` is a derived view, published by writers after the workspace lock releases — serialized through `locks/publish.lock`, atomic rename, no fsync, and debounced through `locks/publish.stamp` to at most one checkpoint per second, or sooner once the unpublished tail crosses 64 KiB. Wakeups fire before the publish and consumers fold the log tail from their own cursor, so the checkpoint is a catch-up accelerator, never the freshness path — a skipped publish costs a cold reader a bounded fold, not staleness. Queued publishers group-commit: each holder folds to the log's current end. It is stamped with the log generation and byte offset it reflects; a reader trusts it exactly when the stamp matches the live log, folds the missing tail itself when writes outran it (O(delta bytes)), and rebuilds from scratch on any mismatch or parse failure.
- `snapshots/rollup.json` is the resumable agent-rollup fold base — the raw pre-projection state plus the `(generation, offset)` stamp — cache-class like `latest.json`. A writer that crashes between releasing the lock and publishing costs nothing: the next reader folds the delta from the durable log itself.

## Runtime projection

History and runtime are separate views over the same durable ledger.

- **Expel** is read-time filtering. Default runtime views (`rimz sidebar snapshot`, `rimz feed list`, and the default agent summary in `rimz doctor`) include only feed items and agent rollups whose `runtime_owner` points at the same live process that created them. Ownerless legacy records, dead owners, and Linux PID-start mismatches are audit-only.
- **Audit** is durable history. `rimz feed show <request-id>` is exact, `rimz feed list --audit` lists all feed items, and `rimz doctor --audit` reads the full agent rollup history.
- **Abandon** is a durable terminal transition. A periodic sweep — the next ledger write past a ~2s debounce, or `rimz gc` — finds a pending item with a recorded but dead owner process, writes `status = abandoned`, records reason `owner_process_exited`, and appends a `feed.abandon` audit event. Expel hides a dead-owner item from runtime views the instant it dies; the sweep makes that durable within the debounce window, and the write path itself stays O(1) — one stamp stat, never a history scan.

`runtime_owner` records `kind = agent | script`, a stable subject id, `pid`, and the Linux process-start token when available. Agent hooks publish the detected agent process and session id; blocking script asks publish the running `rimz feed ask` waiter. Short-lived `feed push` and `feed ask --no-block` records are still written for audit, but once their CLI process exits they leave default runtime views.

## Runtime state

Under `${XDG_RUNTIME_DIR}/rimz/<workspace_id>/`, or `/tmp/rimz-<uid>/<workspace_id>/` at mode `0700` when `XDG_RUNTIME_DIR` is unset (common inside containers and on minimal hosts):

```text
sock/feed.<short_id>.sock           per-request decision socket; bound by the
                                    waiting hook subprocess, torn down on exit
sock/sidebar.<instance_id>.sock     per-instance wakeup socket; bound by each
                                    live sidebar
heartbeat/sidebar.<instance_id>.json
heartbeat/resolver.<resolver_id>.json
```

Sockets and heartbeats are liveness hints, not durable state. They're split from the ledger directory because Linux's `AF_UNIX` path-length limit (108 bytes) makes deeply nested state paths fragile.

`rimz gc --older-than <duration>` removes stale resolver/sidebar heartbeat files and stale sidebar wakeup sockets named by those heartbeat files. It does not remove `feed.*.sock` files because a long-running `rimz feed ask` may still own one. It also abandons pending feed items whose recorded owner process has exited. As the global garbage collector it additionally prunes provably-dead durable workspaces — a recorded project root that no longer exists, or an abandoned `rimz start` scaffold with no history. A workspace whose `workspace.json` is unreadable but that still holds history is kept and reported, never deleted.

## Default path

No fresh enrolled resolver heartbeat at hook fire time:

1. Hook reads agent payload from stdin.
2. Hook writes a feed item with `surface = native_ui`.
3. Hook wakes any live sidebars.
4. Hook returns the event-specific neutral no-op.
5. Hook exits within milliseconds.
6. Agent's own UI asks the human.

No per-request socket is bound, and the human answers in the agent's own UI — Rimz never learns the decision, so the item never reaches `resolved`. Instead the next ledger event that proves the session moved on expires it: a fresh ask supersedes it before being pushed, a turn-boundary lifecycle event (a fresh prompt or a turn's end) clears it, and a session-end event clears every surface the session left pending (the adapter marks these events via `moves_on` / `ends_session` — see [hooks.md](./hooks.md)). Each match moves to `abandoned` with an `agent_moved_on` (or `agent_session_ended`) reason, so a session can never stack more than one native_ui row. The broad per-tool hook is silent on the ledger, so it is *not* a trigger; the read-side snapshot also collapses a session's pending asks to one row as a backstop.

## Bridge path

Fresh enrolled resolver heartbeat at hook fire time:

1. Hook writes a feed item with `surface = bridge` and binds a per-request socket; the socket path is written into the feed file.
2. Hook re-stats the resolver heartbeat directory (TOCTOU guard) — if the resolver died between the initial stat and the bind, the hook downgrades to `native_ui` and exits.
3. Resolver calls `rimz feed resolve`.
4. CAS validates `status = pending`, active chain step, `workspace_id`, `request_id`, and nonce.
5. The waiting hook unblocks and prints exactly one agent-native decision JSON.

On hook-cap timeout (the per-agent ceiling — see [hooks.md](./hooks.md) for the value each adapter ships), the hook returns neutral, the feed item moves to `timed_out`, and the sidebar labels it **"Delegated to native prompt"** — the agent's own UI takes over, exactly as in the default path.

## Script path

`rimz feed ask` always blocks. Resolver presence is irrelevant — the script chose Rimz as its decision surface. The wait has no agent-imposed cap; the caller's `--timeout` is the only ceiling. Resolution can come from any shell (`rimz feed resolve`), the sidebar UI, or an enrolled resolver client.

## Wakeups

After every ledger write the CLI or hook subprocess:

1. Walks fresh `heartbeat/sidebar.*.json` entries on the current sidebar protocol version (TTL ~5s).
2. Sends a small wakeup datagram (`{ "kind": "ledger_delta", "request_id": "...", "workspace_id": "...", "protocol_version": "rimz.plugin.v4" }`) to each `sock/sidebar.<instance_id>.sock`.

The socket datagram is the only wakeup the walk fires, on both backends — one per fresh instance. The `MuxBackend::wake_sidebar` pipe primitive (`zellij --session <name> pipe --name rimz::feed`) is dormant: it has no consumer until the opt-in Zellij plugin rail is built, so the walk spawns no `zellij` subprocess per write. When that rail lands it re-arms the pipe, gated on rail presence (see [multiplexers.md](./multiplexers.md)).

The native sidebar's response to a wakeup is an in-process fetch cycle: it first folds the event-fresh rollup over the published pane frame and, if it is the producer, reconciles through `rimz::sidebar::produce`. A missed wakeup is closed by the next tick (~2s).

## Late answers

A `feed resolve` arriving after the item is `timed_out` is accepted by CAS and appended to `events.log.jsonl` as:

```text
effective = false
late = true
reason = "hook_already_returned_neutral"
```

It does not change agent behaviour, never surfaces as a sidebar attention item, and is never typed into a pane by Rimz core. The record exists for audit.

## What survives what

| Event | Ledger | Live sockets and heartbeats | Multiplexer session |
| --- | --- | --- | --- |
| Detach | yes | yes (mux server stays alive) | yes |
| Sidebar reload | yes | sidebar socket rebound on attach | yes |
| Multiplexer server crash | yes | no | no |
| Host reboot | yes | no | no — needs host supervisor (tmux-resurrect, Zellij resurrect, systemd) |
| Host power cut | yes — through the last group fdatasync; up to ~1s of trailing events (decisions included) can be lost, and repair truncates any torn suffix (see [write classes](#write-classes)) | no | no — needs host supervisor |

Rimz guarantees the ledger across all of these — at a power cut, through the last group sync, with repair bounding the damage to the final window. The session and processes survive only what the host supervisor and the multiplexer server keep alive.

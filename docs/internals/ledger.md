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
print neutral payload       wait up to hook cap         wait up to --timeout
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
feed/<request_id>.json
locks/workspace.lock
```

Rules:

- `workspace.json` records the last known project root and session name for maintenance commands; feed files and the event log remain the request source of truth. Launch reads the prior record before overwriting it: when the derived session name diverges from the recorded one and a session still answers to the recorded name, launch retires that session and rebirths the workspace under the new name, so a changed derivation never strands a live session or its sidebar.
- Feed files are written temp-file + rename.
- Resolutions take the workspace lock, then CAS on `status = pending`. First valid writer wins.
- `events.log.jsonl` uses length-prefixed framing with `fsync` per record.
- A torn trailing record at SIGKILL is skipped on rebuild and logged.
- Rollup reads serialize against writers: `runtime_projection` (behind `rimz sidebar snapshot`, `feed list`, and `doctor`) takes the workspace lock for its read, so a writer's half-written trailing frame is never observed as a torn record and dropped. The torn-trailing skip then fires only for a genuine crash corpse, never a live concurrent append — which would otherwise blink an agent out of the rollup for one tick and flash its live pane as a bare `process` row.
- `rimz workspace rotate-events` archives the active log into `events.log.archive/events.<uuidv7>.jsonl` once it exceeds the operator-supplied byte threshold (default `64MiB`); UUIDv7 filenames sort chronologically. The same command prunes archives older than `--archive-older-than`.
- Before rename, the agent rollup of the rotating log is merged into `agents.carryover.json`. The snapshot reducer loads carryover and lets newer in-log observations override; this keeps the sidebar's agent panel correct across rotations without rescanning archives.
- Every feed file carries `workspace_id`, `request_id`, nonce, resolver id, and timestamps.
- `snapshots/latest.json` is rebuilt from the active event log, the agent carryover, and the feed dir on every ledger mutation. Cost is O(active-events + items); archives are read only at rotation time.

## Runtime projection

History and runtime are separate views over the same durable ledger.

- **Expel** is read-time filtering. Default runtime views (`rimz sidebar snapshot`, `rimz feed list`, and the default agent summary in `rimz doctor`) include only feed items and agent rollups whose `runtime_owner` points at the same live process that created them. Ownerless legacy records, dead owners, and Linux PID-start mismatches are audit-only.
- **Audit** is durable history. `rimz feed show <request-id>` is exact, `rimz feed list --audit` lists all feed items, and `rimz doctor --audit` reads the full agent rollup history.
- **Abandon** is a durable terminal transition. When a ledger writer or `rimz gc` sees a pending item with a recorded but dead owner process, it writes `status = abandoned`, records reason `owner_process_exited`, and appends a `feed.abandon` audit event.

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

`rimz gc --older-than <duration>` removes stale resolver/sidebar heartbeat files and stale sidebar wakeup sockets named by those heartbeat files. It does not remove `feed.*.sock` files because a long-running `rimz feed ask` may still own one. It also abandons pending feed items whose recorded owner process has exited. As the global garbage collector it additionally prunes provably-dead durable workspaces — a recorded project root that no longer exists, or an abandoned `rimz start` scaffold with no history — the same rule `rimz workspace prune` applies. A workspace whose `workspace.json` is unreadable but that still holds history is kept and reported, never deleted.

## Default path

No fresh enrolled resolver heartbeat at hook fire time:

1. Hook reads agent payload from stdin.
2. Hook writes a feed item with `surface = native_ui`.
3. Hook wakes any live sidebars.
4. Hook prints the event-specific neutral payload.
5. Hook exits within milliseconds.
6. Agent's own UI asks the human.

No per-request socket is bound, and the human answers in the agent's own UI — Rimz never learns the decision, so the item never reaches `resolved`. Instead the next ledger event that proves the session moved on expires it: a fresh ask supersedes it before being pushed, a `Stop`/`UserPromptSubmit` clears it at the turn boundary, and `SessionEnd` clears every surface the session left pending. Each match moves to `abandoned` with an `agent_moved_on` (or `agent_session_ended`) reason, so a session can never stack more than one native_ui row. The broad `PostToolUse` hook is silent on the ledger, so it is *not* a trigger; the read-side snapshot also collapses a session's pending asks to one row as a backstop.

## Bridge path

Fresh enrolled resolver heartbeat at hook fire time:

1. Hook writes a feed item with `surface = bridge` and binds a per-request socket; the socket path is written into the feed file.
2. Hook re-stats the resolver heartbeat directory (TOCTOU guard) — if the resolver died between the initial stat and the bind, the hook downgrades to `native_ui` and exits.
3. Resolver calls `rimz feed resolve`.
4. CAS validates `status = pending`, active chain step, `workspace_id`, `request_id`, and nonce.
5. The waiting hook unblocks and prints exactly one agent-native decision JSON.

On hook-cap timeout (Claude 120s, Codex shorter — see [agent.md](./agent.md) for the exact value each adapter ships), the hook prints the neutral payload, the feed item moves to `timed_out`, and the sidebar labels it **"Delegated to native prompt"** — the agent's own UI takes over, exactly as in the default path.

## Script path

`rimz feed ask` always blocks. Resolver presence is irrelevant — the script chose Rimz as its decision surface. The wait has no agent-imposed cap; the caller's `--timeout` is the only ceiling. Resolution can come from any shell (`rimz feed resolve`), the sidebar UI, or an enrolled resolver client.

## Wakeups

After every ledger write the CLI or hook subprocess:

1. Walks fresh `heartbeat/sidebar.*.json` entries on the current sidebar protocol version (TTL ~5s).
2. Sends a small wakeup datagram (`{ "kind": "ledger_delta", "request_id": "...", "workspace_id": "...", "protocol_version": "rimz.plugin.v2" }`) to each `sock/sidebar.<instance_id>.sock`.
3. **On the Zellij backend only**, additionally issues a broadcast `zellij --session <name> pipe --name rimz::feed -- <envelope>` as a latency optimization for pipe-aware Zellij clients. The native sidebar pane still uses the socket above as the wakeup channel of record.

The sidebar's response to a wakeup is always to refetch via `rimz sidebar snapshot`. A missed wakeup is closed by the next tick (~2s).

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

Rimz guarantees the ledger across all of these. The session and processes survive only what the host supervisor and the multiplexer server keep alive.

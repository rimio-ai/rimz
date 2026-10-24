# Shared language servers

A room can run one language server per checkout and configured server name, shared by agent queries and attached editors, in place of a private server per client. The feature is off until a server is configured, and it is enrichment: an agent without a server navigates with grep.

This page owns the process model in [`lsp/`](../../crates/rimz/src/lsp/mod.rs) and the CLI glue that drives it. The [CLI reference](../reference/cli/lsp.md) owns commands, flags, output, exit codes, and configuration fields.

| Path under `crates/rimz/src/` | Owns |
| --- | --- |
| `lsp/admission.rs` | Memory accounting, launch and query admission, eviction, the required-server queue, lost-row revival |
| `lsp/broker.rs` | The `rimz lsp serve` main thread: server lifetimes, housekeeping, client capabilities, shared settings |
| `lsp/broker/socket.rs` | The newline-JSON socket: one thread per connection, query waits, the attach upgrade |
| `lsp/broker/transport.rs` | Content-Length JSON-RPC over the server's stdio, request IDs, server-initiated requests |
| `lsp/broker/router.rs`, `lsp/broker/clients.rs` | Editor traffic: the single writer and the pure routing state machine |
| `lsp/broker/adaptation.rs` | Reply shapes adapted to each editor's capabilities |
| `lsp/broker/lifecycle.rs` | Lease grace, idle and recycle decisions, progress-driven readiness |
| `lsp/broker/watch.rs` | Saved-file watching |
| `lsp/broker/watchdog.rs` | Memory-pressure election and victim stops |
| `lsp/broker/probe.rs` | `rimz doctor` startup probes |
| `lsp/registry.rs` | Per-key runtime entries, the admission lock, sweeps, stop requests |
| `lsp/history.rs` | Learned peak memory per project, server, and settings |
| `lsp/usage.rs` | Room query counts |
| `lsp/memory.rs`, `lsp/installed.rs`, `lsp/retire.rs` | Process-tree memory, installed-build identity, outdated-broker retirement |
| `lsp/server.rs`, `lsp/protocol.rs` | The server process and its stderr tail, framing and the read-only request vocabulary |
| `lsp/query.rs`, `lsp/query/client.rs` | Symbol resolution, query composition, compact output |
| `lsp/check.rs`, `lsp/check/` | Notes anchor checks, `--fix`, and `rimz lsp show` |
| `lsp/attach.rs` | The editor stdio bridge |
| `cli/lsp.rs`, `cli/lsp_admission.rs` | The `rimz lsp` verbs, and pre-pane admission at launch |

## Why RimZ owns this

A language server is expensive to hold, and a harness-native LSP tool starts one per agent session. One `rust-analyzer` over this repository holds several gigabytes once indexed (the default `memory-estimate` is `8G`). A three-seat team plus its subagents would hold three or more copies of the same index over one checkout, idle or not. RimZ denies the native tool where the provider allows it ([What agents see](#what-agents-see)) and serves one server per checkout instead.

RimZ resolves every launch's checkout before any pane opens ([fleet.md § One launch, end to end](./harness/fleet.md#one-launch-end-to-end)), so it can register the server before the agents exist. Wrapper process leases supply liveness without coupling the machine-level broker to a room's store or multiplexer.

## The rules that shape it

Enrichment is the default, never a precondition. A configuration error or a memory refusal for an optional server, or a server that dies later, leaves agents on grep. `admission::admit_launch` warns once per launch. Untrusted project entries warn and stay inert, even when declared required, and same-named machine entries stay in effect. A required server's configuration error refuses the launch. An invalid machine-wide `[lsp]` policy refuses only when a required server's root markers match the checkout, and otherwise warns once and skips every server. Each warning is visible to the human ([Visibility](#visibility)).

A server is registered at launch and started on demand. An optional launch creates a dormant broker with no server process. The first agent query or editor request runs memory admission and starts the server. A required launch admits and starts eagerly. Launch reminders name dormant and running servers alike, so an agent's instructions do not change when a server stops or restarts.

There is one server per key, and agents only read. The key is the canonical checkout root plus the server name. Attached editors own buffers ([Editors](#editors)). Disk is the truth for every file no editor holds, and the broker keeps the server's view of saved files current ([Freshness](#freshness)).

Leases, not rooms, bound the broker's life. Agent wrappers and attached editors hold process leases, and the broker exits after the last one goes ([Leases and shutdown](#leases-and-shutdown)). Leases also outlive their broker: a broker that dies while a holder lives leaves its row behind, and the next query revives it ([Lost rows and revival](#lost-rows-and-revival)).

Memory is the only admission budget. There is no concurrency cap. A server starts only when its estimate fits under free memory with the reserve intact ([Admission](#admission)).

## Lifecycle

### States and stop reasons

`registry::State` is `starting`, `indexing`, `ready`, `dormant` (with an optional `registry::StopReason`), or terminal `stopped`. A dormant entry with no reason has never run. Stop reasons serialize as human-readable strings in the registry, socket, and history records.

| Reason | Set by | Ends |
| --- | --- | --- |
| `idle` | Housekeeping: a ready server unused for `idle-timeout` | Server |
| `recycled` | Housekeeping: a ready server over its footprint ceiling ([Recycling](#recycling)) | Server |
| `memory pressure` | The watchdog ([Memory pressure](#memory-pressure)) | Server, alarm |
| `evicted` | Query-time admission making room (`admission::admit_query`) | Server |
| `crashed` | A server exit or a lifetime error in the broker | Server, alarm |
| `stopped by hand` | `rimz lsp stop` (`crates/rimz/src/cli/lsp.rs::stop`) | Server |
| `team done` | `team_stage::open_stage` at a board's terminal flip | Server |
| `released` | Housekeeping: 60 seconds after the last lease went | Broker |
| `never leased` | Housekeeping: five minutes after broker start with no lease ever taken | Broker |
| `checkout removed` | Housekeeping's root check, or worktree removal (`registry::stop_checkout`) | Broker |

A reason that ends the server leaves the broker dormant, and the next agent query or editor request can restart it. The last three are terminal (`StopReason::is_terminal`): the broker exits. `State::is_alarm` marks crashed and memory-pressure dormancy. List shows alarm rows by default and sorts them after running rows, and a hand stop acknowledges them. Memory-pressure dormancy still renders in the neutral role, and only a crash renders as a failure.

### Which launches register a broker

`crates/rimz/src/cli/lsp_admission.rs::admit` registers brokers for one checkout. Each launch path calls it after the worktree exists (step 5 of the launch) and before the exec wrappers compile (step 7), once per distinct checkout its panes open in:

| Caller | Checkouts |
| --- | --- |
| `crates/rimz/src/cli/agents_cmd/launch.rs::launch_resolved` (`rimz agents`, `rimz teams`) | The launch's cwd |
| `crates/rimz/src/cli/agents_cmd/launch.rs::launch_resume_layout` (cohort resume) | Each resumed empty-dir agent's recorded checkout, then the cohort's cwd |
| `crates/rimz/src/cli/agents_cmd/resume.rs::resume_selected` | Each resumed tab's cwd |
| `crates/rimz/src/cli/agents_cmd/resume.rs::resume_lane` | Each distinct target cwd |
| `crates/rimz/src/cli/supervised/run.rs::prepare_supervised` (`-p` runs) | The launch's cwd, after provider preflights, skipped for a subagent |
| `crates/rimz/src/cli/room/mod.rs::preflight_recovery` (rebirth) | `RebirthPlan::checkout_roots`, warning and continuing on any admission error |

A subagent launch never admits. It joins whatever broker its checkout already has, dormant or running, which by default is its parent's ([subagents.md § The child works where its parent works](./harness/subagents.md#the-child-works-where-its-parent-works)). A child sent with `--cwd` into a checkout with no broker gets exit 3 from its queries.

Admission for a live key joins it without starting another server. The wrapper takes its lease separately, after launch-plan application ([Leases and shutdown](#leases-and-shutdown)). Admission reuses the launcher's lenient machine config and skips effective config loading when neither machine nor project declares servers. Recovery is the one caller that never fails on admission: a config, registry, or memory-sampling error only warns there. Every other path keeps its strict effective-config checks.

### Admission

Admission reads free memory and the machine-wide registry under one lock, `admission.lock` in the runtime directory ([State](#state)), so admission decisions are serialized across rooms. An optional launch skips memory admission, and the broker's main thread runs it on the first query and on every restart. A required launch admits its first lifetime before it spawns the eager broker.

```text
available  = MemAvailable, capped by the cgroup's memory.max headroom when RimZ runs under one
swap       = SwapFree, capped by the cgroup's memory.swap.max headroom (0 when a swap limit cannot be read)
reserve    = max(reserve-percent × MemTotal / 100, reserve-min)
admit when estimate + reserve ≤ available + swap (checked addition, overflow refuses)
```

Running servers get no allowance for future growth. Several indexing servers can each pass admission and later cross the reserve together, and the watchdog then stops some of them. `estimate` is read again for each lifetime: the learned peak for this project, server, and settings, or the configured `memory-estimate` before any history exists ([Learned cost](#learned-cost)). The free figure that waits, timeouts, and refusals print is available plus swap minus the reserve, saturating at zero. Holders list running servers and their tree RSS.

A refusal depends on the server's `policy`:

| Policy | Refused admission |
| --- | --- |
| `optional` (default) | The launch does not check memory. A query-time refusal leaves the broker dormant, returns exit 4 with the memory figures, and records the shortfall. A later query tries again. |
| `required` | The launch waits in first-come order among required waiters, before panes open. Admission polls every five seconds, and the launcher's terminal prints the wait when the queue position changes. No sidebar row exists before launch. At `wait-timeout` the launch fails with the fix, naming the memory needed, the memory free, and the holders. Optional launches do not queue behind required ones. |

`required` gates the first launch admission only. Joining a live broker, dormant or not, skips eager admission. After any stop, both policies restart lazily, and a query-time refusal returns exit 4. RimZ never stops agents to honour required policy. Editor attachment exits 3 on an admission timeout or refusal.

`admission::admit_query` can evict other servers to make room. A candidate must be ready, have a live broker, have been ready for at least five minutes, and have had no request in the last 120 seconds, measured from the later of readiness and last request. Starting servers and the requesting broker are protected. Candidates follow `registry::kill_order`. Admission stops one at a time with reason `evicted`, waits up to three seconds for token-guarded server death without a kill fallback, then samples again and stops evicting once the request fits. Required launch admission does not evict.

### Readiness

Launch admission waits up to five seconds, under the admission lock, for the broker to publish its entry. It does not wait for the index. If the entry does not appear, an optional server records a refusal and the launch proceeds, and a required server fails the launch with the server name and a command or configuration fix.

After `initialize` and `initialized`, an open progress token means `indexing`. The server is `ready` when every token has closed, at least one has ended, and two seconds have passed since the last progress event. Without any progress the fallback is ten seconds after initialization. The settle window keeps a short initial token from claiming readiness before indexing begins. Later progress can return the server to `indexing`. Each lifetime resets readiness and the start and ready timestamps, and broker readiness also waits for the router's editor replay ([Ownership and replay](#ownership-and-replay)).

A query waits at most 110 seconds for startup and readiness. The client's socket timeout is 115 seconds, so exit 4 lands before an agent shell's 120-second timeout. On timeout the query reports the elapsed time for this server lifetime, never an empty result from an unfinished index.

The first Ready transition in `crates/rimz/src/lsp/broker.rs::run` samples RSS plus swap as the lifetime's ready footprint. It does so under the model lock that stamps `ready_at_ms`, before waiting queries wake. A zero reading leaves the baseline unset and a later Ready transition tries again. `crates/rimz/src/lsp/broker.rs::prepare_start` clears the baseline for each new lifetime.

### Leases and shutdown

`crates/rimz/src/cli/agents_cmd/exec.rs::run_exec` registers a lease with every live broker at its checkout (`registry::register_lease`) after `launch_plan::apply`, and `settle_after_exit` releases them on resident-wrapper paths. A lease carries an optional launch id, the wrapper pid, and its process start token. A session recovered without a launch id still takes a lease, and the wire accepts absent and null ids. A direct `exec` keeps the wrapper pid. An attached editor holds a lease without a launch id, and a fork takes its own.

Housekeeping runs every five seconds, dormant or running. It reaps leases whose owner is dead, rejecting pid reuse through the start token, and it never reads store or pane state. It also checks that the checkout root exists, as the backstop for worktree removal, which calls `registry::stop_checkout` best-effort. When the last lease goes, a 60-second grace starts, covering the gap of an agent restart, and a new lease cancels it. A broker that never had a lease exits five minutes after it started.

A query starts a dormant server whether or not the broker holds a lease at that moment, so a query during the release grace or the never-leased window can start one. Required-queue tickets also carry an owner pid and start token: abandoned tickets are removed, and dropping the launcher's `WaitQueue` removes its own.

A hand stop on an alarm entry only relabels it `stopped by hand`, the user's acknowledgment of the recorded reason. The server is already down, so `since_ms` and the stop epoch stay put and no waiting query fails. The relabel happens only after `serve` has cleared the transport, the lock under which it reads the reason for the history record and the crash diagnostic. A hand stop during that teardown is a no-op, so the record keeps the real cause. A hand stop leaves any other dormant reason as it is. After each successful stop request, `crates/rimz/src/cli/lsp.rs::stop` reads the broker's `status` and fails an entry still in an alarm, with a retry hint.

A terminal stop ends the broker. It publishes `Stopped`, closes editor connections, drains owed replies (giving up after five seconds with a warning), removes its directory, and exits. The drain begins with an admission cutoff: connections enrolled before it finish their replies, with incomplete requests answered as protocol errors, and later connections close unread. A client holding a pre-stop registry entry can therefore report `crashed` instead of `stopped` if its liveness probe arrives during the drain.

`rimz reset --hard` of a scoped-isolation room also ends a broker that one of the room's scoped agents spawned, whatever leases or editors it still has. Such a broker runs inside that agent's view, and the reset ends every process there ([sandbox.md § Temp units](./sandbox.md#temp-units)).

### Lost rows and revival

A row whose broker is dead while any lease holder lives is a lost row. `registry::sweep` leaves its files on disk but does not return it, so `rimz lsp list` and `status` do not show it. A row with neither a live broker nor a live holder is removed. Holder liveness uses pid plus start token, so pid reuse cannot keep a row.

The next query in that checkout revives it: `crates/rimz/src/cli/lsp.rs::query_context` calls `admission::revive`, which spawns a dormant broker for each lost row whose server is still configured and whose root markers still match. The new broker adopts the row's leases, and the query then starts it through memory admission like any dormant query. A query never spawns a broker for a checkout without a lost row.

A replacement broker takes over in the same way however it was spawned. Before binding, it reads its own key's entry. If the previous broker's pid and start token are dead, it removes the stale socket, adopts the whole lease list, and records `revived`. Housekeeping then reaps adopted owners that have died, with the normal release grace after. Launch and attach admission share this takeover, so neither needs lost-row logic of its own.

### Recycling

The footprint ceiling is `max(recycle-factor × ready footprint, recycle-min)`, in KiB, with saturating arithmetic. `recycle-factor = 0` disables recycling, and `1` is accepted. Housekeeping reuses its one RSS, swap, and peak reading and, for a Ready server only, applies `crates/rimz/src/lsp/broker/lifecycle.rs::recycle_due`. The server recycles when its footprint is strictly above the ceiling, a baseline is present, no agent query is in flight, no editor request is being forwarded, and the transport has no outstanding request. A lifetime with no baseline never recycles.

The request-state reads follow the sample immediately, before `Model::stop` and `notify_all`, all under the model lock. `crates/rimz/src/lsp/broker/transport.rs::Transport::has_outstanding` reads the pending map that agent and editor requests share. Under the same model lock, the router reserves an editor forward before it releases the lock to write. The reservation lasts until the request is in the pending map, so a recycle cannot land between accepting and forwarding a request. Editor frames that arrive after a non-terminal stop wait for the router's lifetime-end event, then enter the inactive client state machine and never reach the old transport.

The broker parses its idle and recycle policy once, at startup. A recycle is neutral: the server stays dormant until a query or editor request restarts it through admission, with a fresh baseline and the usual re-index wait. Attached editors receive the serverStatus warning naming `recycled`, with no cancelled requests and no popup. There is no recycle backoff. Each lifetime has its own baseline, and repeated growth shows in history, diagnostics, and the restart count in `status`.

### Build retirement

`rimz reload` retires brokers whose build differs from the installed one, without interrupting a running server. It reads live registry rows and compares each broker's `/proc/<pid>/exe` build id with the first `rimz` on the reload process's `PATH`, through the installed-build resolver the watchdog also uses ([The watchdog](#the-watchdog)). Current brokers get no operation and no diagnostic, and unreadable broker images are skipped. With live rows but no readable installed build, no broker is checked. Retirement runs once, before room enumeration, even with no live room.

The nonce-checked `{"op":"retire"}` socket operation marks the broker in memory. It replies `{"ok":true,"exiting":true}` when the broker is dormant with no start requested and no editor attached, and `{"ok":true,"exiting":false}` otherwise, each with the nonce. A marked broker keeps serving until its server next stops for any non-terminal reason. A query that sets `start_requested` first wins that lifetime, and a dormant broker whose last editor detaches leaves within one housekeeping pass. Repeated marks are idempotent, and a broker already closing answers `{"ok":true,"exiting":true}`.

A retiring broker closes the router, drains owed replies, and removes its socket, but it keeps its key directory and publishes no further row. Its last Dormant entry and leases stay as a lost row, which a query, launch, or attach revives while a holder lives ([Lost rows and revival](#lost-rows-and-revival)). `broker_exited` records `reason: "retired"`. That is a diagnostic reason only, never a registry state or a `StopReason` variant, so a binary from another build can still read the row.

A broker that rejects `retire`, answers anything else, or cannot be reached falls back to a guarded kill. Reload takes `registry::lock()` and reads the row again. If the broker has exited or its row is gone, the outcome is `gone`, without a warning. Otherwise it signals only the same live broker (nonce, pid, and start token), Dormant with no server pid, no editor attached, and no child processes. Every dormant start takes that lock, and the child check excludes a server still shutting down after Dormant was published. The token is checked again immediately before SIGKILL, followed by a bounded wait for death. A query already at such a broker's socket can fail once, and the next query revives the kept row. A broker that is not safe to kill keeps running, with its own build's rules, until a later reload finds it safe.

Reload appends one `retired` diagnostic per outdated live broker, carrying `broker_pid`, `build` (the broker's), `installed`, and `outcome`: `exiting`, `marked`, `killed`, `kept`, or `gone`. A `gone` outcome counts toward none of the retired, marked, or kept totals in the [reload report](../reference/cli/maintenance.md#reload-running-sidebars).

## Memory pressure

### The watchdog

Every live broker samples free memory (available RAM plus free swap, each capped by cgroup headroom) on its housekeeping pass. Below the admission reserve, the broker that wins the machine lock picks victims and samples again after each stop, until free memory reaches the reserve.

A stop goes to the victim's broker over the socket with reason `memory pressure`, and the request and the wait for the server to die share one two-second budget. A server still alive after it is killed with a token-guarded SIGKILL on its process group. When the elected broker picks itself, it kills its own server group directly instead of waiting on its own housekeeping loop. Each server starts as a process-group leader, so the kill also ends its cargo and proc-macro children. Servers get `oom_score_adj = 800`, so the kernel picks them before agents if it acts first.

Only a broker whose build id matches the first `rimz` on its inherited `PATH` may take the pressure-relief lock and stop servers. A staged copy of that build qualifies, and a branch build does not. The installed identity is resolved only under pressure or an admission shortfall, and cached while the resolved path's stat identity is unchanged. An absent `rimz` on `PATH`, an unreadable installed image, or an unreadable own build id grants no stop authority. Query-time eviction uses the same gate: an ineligible broker still admits a start that fits, but refuses a short start without taking victims.

The broker keeps the machine policy captured at spawn until it is replaced. Idle and recycle stops only make its server dormant, so they do not refresh it.

### Who is killed first

`registry::kill_order` is least-recently-used order: last request, falling back to start time, then start time, checkout root, and server name. The watchdog considers starting, indexing, and ready servers, never dormant ones, and ignores the age and idle floors that eviction applies. Both paths share `watchdog::stop_victim`, and neither publishes the victim's entry or writes its history. The owning broker records the lifetime's end and becomes dormant.

### The agent's view of a kill

Agents get no message. The next query against a dormant key requests a restart, answers normally if the server is ready within the wait, and exits 4 if it is still indexing or memory admission refuses. Memory pressure that interrupts an in-flight query also exits 4, and any other stop that interrupts one exits 3. The next call can try again without relaunching the agent.

## Learned cost

`peak_rss_kb` keeps its wire name but measures peak footprint. It is the running maximum of sampled tree RSS plus swap, floored by the summed kernel `VmHWM` over the process tree (`crates/rimz/src/proc/mod.rs::tree_memory_kb`, read through `crates/rimz/src/lsp/memory.rs::tree`). The same status walk sums `VmSwap`, without adding status reads to `proc::tree_totals`. Broker housekeeping refreshes the peak, and the watchdog samples the victim's peak for its diagnostic without publishing the entry. `VmHWM` catches resident spikes shorter than the five-second sampling interval. Summed high-water marks can overcount processes that peaked at different times and omit children that have exited, and the sampled footprint keeps what was observed. Off Linux only sampled RSS is available and swap reads zero.

The same meaning holds in history, list JSON, and the `killed`, `evicted`, `swept`, and `crashed` diagnostics. `victim_rss_kb` stays resident-only. Records written by builds before the footprint measure carry resident-only peaks until newer lifetimes replace them in the window.

The owning broker appends one `crates/rimz/src/lsp/history.rs::Record` per server lifetime to `lsp-history.jsonl` under the RimZ home. A record carries the project, checkout, server, lifetime peak footprint, spawn-to-first-ready time (`ready_ms`), settings hash, stop reason, and `dormant_ms` (the preceding stop-to-start gap, absent for a first start). The entry's own peak stays monotonic across lifetimes, while each history record carries that lifetime's peak.

Admission takes the maximum of the last five records for the same project, server, and settings hash that reached Ready at least once (`ready_ms` present and non-null). Without such a lifetime it falls back to `memory-estimate`. A partial lifetime stays recorded but takes no slot in the window, and a later return to Indexing does not disqualify a lifetime that reached Ready. Records without a project are skipped. The watchdog remains the backstop for an estimate that is too low.

The project is the workspace's `launch_repo_root()`, resolved by `crates/rimz/src/cli/lsp_admission.rs::admit`, so every checkout of one project shares learned costs while the registry key stays checkout and server. The settings hash covers the command argv and the canonical initialization options ([Configuration](#configuration)).

## The broker

A language server speaks to exactly one client over stdio, so each server gets one broker: a hidden `rimz lsp serve` process that owns the server's stdio, listens on a per-key Unix socket, and speaks the `rimz lsp` query protocol to callers. It is a per-key process bounded by leases, not a room-wide service ([fleet.md](./harness/fleet.md#the-rules-that-shape-it)).

### Threads and locks

`broker::serve` loops over server lifetimes and stays dormant between them while leases live. Each lifetime creates a new child, transport, and saved-file watcher. The main thread owns admission and server lifetimes, and it never holds the model mutex while it waits on the machine lock, a socket, or process death. The acceptor blocks in `accept`. Each socket connection has its own thread, which only signals starts and stops and never spawns or kills a process. Each reply is one write of a complete line, and terminal shutdown waits for those threads' owed replies before it takes the registry lock.

Both supervisor loops wait on the model condvar until a model change, a watch event, a server exit, the readiness settle deadline, or the next housekeeping pass. Only the startup handshake polls, every 50 ms. Only the owning broker publishes its entry. The broker answers `hello` before publishing its first entry and without taking `admission.lock`, since launch admission holds that lock while it waits for the publication.

### The socket protocol

The socket is mode 0600 and speaks newline JSON. It accepts `hello`, `lease`, `release`, `query`, `stop`, `status`, `attach`, and `retire` ([Build retirement](#build-retirement)). Every response carries the broker's nonce, which stays the same across server lifetimes.

A query carries `method`, `params`, and `wait_ms`, and returns the raw LSP `result`, elapsed `indexing`, `refused` with a `Shortfall`, or `error`. A query to a dormant broker signals a start request and waits for readiness, a terminal stop, a new refusal, or its deadline. An RAII in-flight counter covers the whole query, that wait included, and keeps the server from an idle stop. Agent queries and editor requests both stamp recent use, and agent query stamps feed the LAST column, idle stop, and admission. Only editor requests increment `request_count` (editor LSP requests since the broker started), and they do not hold the in-flight counter. The CLI owns query verb semantics, and the broker forwards a method and its parameters.

### The server process

`crates/rimz/src/lsp/server.rs::spawn` owns the configured argv, the checkout as cwd, the process group, and piped stdio. Stderr is drained for the whole lifetime into a bounded tail: the last 8 KiB, cut at a line boundary and decoded as lossy UTF-8. On a crash, `Server::crash` reads the exit status before it kills the group, allowing 100 ms for a server that exits after EOF, then finishes the stderr capture. A broker-side failure while the server is still alive has no exit code or signal.

`registry::Entry.last_crash` keeps `at_ms`, `exit_code`, `signal`, `stderr_tail`, and `error` while the broker is dormant. `broker::prepare_start` clears it on the next Starting transition, and idle, recycle, hand stops, and memory-pressure kills never write it. The `crashed` diagnostic carries the same cause beside its `reason` and `peak_rss_kb`, so the cause survives removal of the runtime entry. Crashed query errors point to `rimz lsp status`.

`transport.rs` handles Content-Length framing, and request IDs match concurrent replies. The transport reader answers configuration, workspace-folder, progress-creation, and capability-registration requests itself, and acknowledges both capability registration and unregistration with `null` ([Settings, capabilities, and bounds](#settings-capabilities-and-bounds)).

### Spawning and its traps

`admission.rs` spawns the broker through `child_process::spawn_detached_rimz`, and the broker calls `setsid`. Three traps follow:

- A scoped launcher. An agent may run `rimz teams` from inside its bubblewrap view, and a broker spawned there inherits that mount namespace, where `/tmp` is the agent's temp unit. The sandbox rebinds the runtime home, the RimZ home, and every checkout at their host paths ([sandbox.md § Reachable host paths](./sandbox.md#reachable-host-paths)), so the broker names only such paths. Its socket and registry live under `disk::paths::runtime_rimz_root` (`$XDG_RUNTIME_DIR`, else `/tmp/rimz-<uid>`), never under the launcher's temp unit.
- A host launcher's temp unit. The detached spawn gives the broker the user's `TMPDIR` back and drops the listed provider temp-root keys ([sandbox.md § Environment pins](./sandbox.md#environment-pins)). A shared broker and its servers therefore never hold the first launcher's temp unit, which GC removes after that agent's sessions end. A sandboxed launcher's broker keeps `/tmp`, since no saved variable reaches the view. A broker keeps its inherited environment for its whole life: `rimz lsp stop` makes only its server dormant.
- The server's own build. The `rust-analyzer` template in the [reference](../reference/cli/lsp.md#configuration) sets `checkOnSave = false`. The read-only query surface exposes no diagnostics, and a check would contend for the checkout's `target/` lock with the agents' own builds. The broker does not inject this option into a custom definition.

### Registry compatibility

Every `rimz` binary on the host reads the registry, older builds in other worktrees included, so its schema changes only additively. `registry::Entry` does not deny unknown fields, so an older binary ignores a new field. An enum variant it does not know, such as a new `StopReason`, fails that row's parse. `registry::read_entries_at` skips any row it cannot read (a parse, key, or read error) and records `unreadable_entry`, so the row is invisible to that binary and never swept by it, and every other row still reads. Missing `attached` defaults to an empty list, missing `kind` and `last_crash` to absent.

The broker's read of its own key and admission's startup wait stay strict, so an older broker fails before it binds instead of unlinking a newer broker's live socket.

### Checking a broker change live

Query a broker that runs the worktree's build. `rimz lsp stop` leaves the old broker process serving, and a scratch `RIMZ_HOME` alone still reaches the room's socket, because the socket and registry resolve from `XDG_RUNTIME_DIR` (`disk::paths::runtime_home`), not from the RimZ home.

1. Copy `~/.rimz/config.toml` into a scratch `RIMZ_HOME`, so the server definitions carry over.
2. Point `XDG_RUNTIME_DIR` at a scratch directory with mode 700.
3. Make a FIFO: `mkfifo "$XDG_RUNTIME_DIR/attach.in"`.
4. From the checkout, run `target/debug/rimz lsp attach --server <NAME> < "$XDG_RUNTIME_DIR/attach.in" &`.
5. Hold the FIFO open with `exec 3> "$XDG_RUNTIME_DIR/attach.in"`, and write a framed LSP `initialize` request to descriptor 3.

`target/debug/rimz lsp` queries under the same two variables then reach that broker. To end the check, close the descriptor (`exec 3>&-`): `attach` exits on EOF and releases its lease, and the broker exits after the 60-second release grace.

## Freshness

`lsp/broker/watch.rs` batches `notify` events into `workspace/didChangeWatchedFiles`, coalescing saved-file events in a 50 ms window from the first undelivered event. Freshness depends on the server handling these notifications. Unsaved editor buffers reach the server only through the owning editor ([Editors](#editors)), and watch traffic never increments request counters.

Non-recursive watches cover individual directories. Excluded directories are dropped before registration: `.git`, `target`, `node_modules`, `.venv`, `venv`, `__pycache__`, `.tox`, `.nox`, `.mypy_cache`, `.pytest_cache`, and `.ruff_cache`, for every server kind. A package install into an excluded environment needs a stop and a lazy restart. Created or moved-in directories gain watches, and a scan also forwards files saved before the watch was registered.

Files with a configured extension pass, and so do file names in `root-markers`, such as a `Cargo.toml` edit. Each Python kind adds its configuration files:

| Kind | Extra watched names |
| --- | --- |
| ty | `ty.toml`, `pyproject.toml` |
| Pyright, basedpyright | `pyrightconfig.json`, `pyproject.toml` |
| Ruff | `pyproject.toml`, `ruff.toml`, `.ruff.toml` |

Create, change, delete, and rename events map to LSP event types. Removed directories and rename sources are forwarded as Deleted, so the server drops the old tree. Notifications are not suppressed for open files, and preferring an open buffer is the server's job. The broker acknowledges dynamic watcher registrations but does not install their globs.

A directory that cannot be watched, and a notify error event, produce a `watch_error` diagnostic and leave the server running with degraded freshness. An error that ends a server lifetime goes into its `crashed` diagnostic, and one that ends the broker's run into `broker_exited`.

## Editors

`crates/rimz/src/lsp/broker/socket.rs::handle` upgrades a connection with a newline-JSON `attach` operation carrying `pid` and `start_token`. Success returns `ok`, `nonce`, and `client_id`, and from then on both directions use Content-Length framing. One buffered reader spans the handshake and the frames, so pipelined bytes survive, and both socket timeouts are cleared after the reply. Attachment registers a process lease without a launch id. A closing or stopped broker, a stale process token, and the client limit refuse with `-32003` before the upgrade.

EOF and read errors detach the editor. Dormancy keeps connections open. Terminal shutdown drains queued replies and stop notifications with a one-second socket write timeout, then closes both directions of every attached socket. Its last frame is a `rimz/stopped` notification carrying the stop reason, which `crates/rimz/src/lsp/attach.rs::relay` consumes instead of forwarding, and reports as `stopped: <reason>` at EOF. The reason travels on the connection because `serve` removes the registry directory, `entry.json` included, right after closing connections, so an attach that read the registry at EOF could find nothing. `serve` still publishes `stopped` before it closes connections, and the `stop` operation publishes under the lock that sets it, so registry readers see the stop before any waiter wakes.

`crates/rimz/src/lsp/broker/router.rs::Router` is the single writer of editor-originated server traffic. It drives the pure `crates/rimz/src/lsp/broker/clients.rs::Clients` state machine, and socket readers only enqueue events. The transport allocates server request IDs, and the router maps replies and cancellations back to each client's original ID, keeping server error objects, `data` included. Lifetime generations discard stale replies and notifications. No model lock spans a socket or transport write, and the agent query path stays separate.

Every client frame is checked under the model lock. Between a non-terminal stop and the router's lifetime-end event, frames wait in arrival order, document notifications included, and frames from detached connections are dropped. Comparing the model's transport with the router's keeps this boundary even when an agent query has already started the next lifetime. After the lifetime-end event, the held frames enter `Clients` while it is inactive, so document changes are cached before queued requests replay.

### Ownership and replay

`Clients::document` keeps holders in open order by normalized `query::DocumentKey`, each with its own URI spelling, text, version, language id, and dirty marker. A file URI normalizes by converting the parsed URL to a file path and back. A parsed URL without a file path keeps its parsed spelling, and an unparseable URI keeps its raw spelling. Equivalent percent encodings therefore share one document, without resolving symlinks or folding path case. Document notifications match this identity even when an editor changes its spelling after opening.

The first opener owns the server buffer. Other holders' changes stay cached, and their requests run against the owner's text, as agent queries do. The cached initialization reply advertises full-text synchronization (`change = 1`), keeps the server's save and will-save settings, and disables workspace-folder change notifications. A change must hold one full-text item without a range, and incremental changes are ignored.

`Clients::sync` reconciles ownership with the server's view:

- A new document identity gets one `didOpen`.
- An owner version advance gets a whole-document `didChange`.
- The last holder leaving gets `didClose`.
- Closing or detaching the owner moves ownership to the next holder: `didClose` under the old spelling, then `didOpen` under the new holder's spelling with its cached text and version.
- Only the owner's `didSave` reaches the server, rewritten to the view's spelling.

The view keeps the owner's opening URI spelling for every server document notification. Other forwarded requests and notifications rewrite `uri`, `targetUri`, and `changes` map keys to the view's spelling, and replies rewrite those fields to the receiving editor's spelling. Unheld URIs stay as they are, and so do diagnostic `relatedInformation` URIs. A lifetime end clears the server view but not the held buffers. After the next server `initialized`, owned documents replay before queued editor requests.

Editors' `initialize`, `initialized`, `shutdown`, and `exit` never reach the server. `initialize` starts a dormant lifetime and answers from the cached server result, or waits for the first one. `shutdown` returns `null` and makes later requests from that editor fail with `-32600`, and neither restarts a dormant server or refreshes idle accounting. `exit` detaches only that editor. Requests to a dormant server queue and request a restart. Accepted editor requests refresh idle accounting but cannot pin the server through a hung request.

On a lifetime end or an admission refusal, every request already forwarded or queued in `Clients` fails with `-32802` (ServerCancelled). Frames the router holds after a non-terminal stop enter `Clients` only afterward, so they survive that end, for idle stops as well as recycles. Each client that receives at least one such failure gets exactly one `window/showMessage`, ready or not: a Warning for a stop, an Error for a refusal. An editor with nothing outstanding gets no popup, only the readiness-gated server-status broadcast. Every starting epoch ends once, including a stop before spawn or initialization, and failing its queued `initialize` does not retry a broken server.

### Readiness and diagnostics

An editor is ready only after its own `initialized`. Before that it receives responses to its own requests and the broker's failure message that goes with them, but no broadcasts or fan-out requests. `Clients::server_message` caches `experimental/serverStatus` and replays the latest value when the editor becomes ready. A lifetime end caches and broadcasts a warning status naming the stop reason, with a next-request restart hint only for a non-terminal stop.

After the status, a late-ready editor receives the open server-created progress tokens in creation order: a fresh `window/workDoneProgress/create` request, the begin frame if one arrived, and the latest report if one arrived. Ended tokens and tokens from a previous lifetime are not replayed, and editors already ready receive only live frames. Editor-originated `workDoneToken` progress is broadcast but not tracked for replay. Progress feeds broker readiness independently of this cache.

`Clients::diagnostics` caches a publication by normalized document identity for an open server document, with the text and server-document incarnation it describes, and the frame keeps the publication's version. Incarnations advance on each server-side open, transfers and lifetime replay included. Every delivery to a holder uses that holder's opening URI spelling, clears and replay included, and a non-holder receives the server's spelling. Delivery to ready editors follows these rules:

- The owner receives the publication with its version unchanged. A publication whose version differs from the current server view reaches only the owner and is not cached.
- Another holder receives it only when its text equals the snapshot text, with the version rewritten to that holder's version. An editor not holding the URI receives it without a version.
- An empty list clears every ready editor, strips the version for non-owners, and removes the snapshot. A non-owner holder that diverges from text whose diagnostics it received gets one empty, unversioned clear. Owner changes wait for the server's next publication.
- A ready opener, or a holder becoming ready, receives a valid cached snapshot under the same text and version rule. Owner changes, ownership transfer, last close, and lifetime start or end invalidate snapshots before they can be replayed.

Known limit: a pre-transfer publication that arrives after the transfer, when both owners used the same numeric version, is indistinguishable from a current one. The broker treats it as current and does not remap document versions on the server side.

### Settings, capabilities, and bounds

The broker owns server settings. It ignores editor initialization options, `workspace/didChangeConfiguration`, workspace-folder changes, watched-file changes, and trace settings, and its saved-file watcher is the only watched-file notifier. The opt-in rust-analyzer `editor-check-on-save` setting overrides only top-level `checkOnSave`, while any editor is attached, from handshake to detach. `crates/rimz/src/lsp/broker.rs::Shared.settings` supplies both the initialization options and the configuration answers. On a first attach or last detach in a live lifetime, the router updates that value before sending `workspace/didChangeConfiguration {"settings": null}`. A dormant broker applies the change at the next initialization, and a transition during initialization is reconciled before editor replay. With no editor attached, the original options, absent options included, are restored.

`crates/rimz/src/lsp/broker.rs::client_capabilities` sends a fixed set per resolved server kind, never the union of what editors advertise. It includes completion, hover, navigation, formatting, rename, semantic tokens, inlay hints, and hierarchies. Every kind advertises `linkSupport` for definition, type definition, implementation, and declaration, and completion snippets. Only rust-analyzer gets the `experimental` block, with `commands`, `hoverActions`, `codeActionGroup`, and `snippetTextEdit`. Left unadvertised: position-encoding negotiation, apply-edit requests, insert/replace edits, pull diagnostics, show-message action items, local docs, open-server-logs, test explorer, and dynamic registration other than watched files.

The pure `lsp/broker/adaptation.rs` adapts each reply to the receiving editor's initialization capabilities and the pending request's method. An unsupported navigation link becomes a location at `targetSelectionRange`, rust-analyzer's `experimental/parentModule` and `experimental/childModules` replies included, which follow the editor's definition `linkSupport`. Unsupported completion snippets become plain insertions, and unsupported snippet text edits are converted recursively, workspace edits included. The plain text keeps recursive placeholder and variable defaults, takes the first choice, removes tabstops and variables without defaults, and unescapes snippet escapes. `crates/rimz/src/lsp/broker/socket.rs::query` returns the server result unchanged, so agent JSON keeps LocationLinks and their full ranges.

`LspServerConfig::resolved_kind` takes an explicit kind, or detects the first recognized command-token basename before its first `@`, or falls back to generic. The broker resolves it at startup and shares the kind and options through a locked settings handle between initialization and the transport reader. `initializationOptions` is sent verbatim apart from the check-on-save override. `workspace/configuration` walks dotted section names, and rust-analyzer's prefix stripping applies to that kind alone. For ty, the requested section `ty` walks to `null`. Pyright and basedpyright get section-keyed settings, and Ruff gets its `{settings}` or `{globalSettings, settings: [...]}` initialization shape. Admission rejects unread top-level keys for those four kinds, whose accepted shapes the [reference](../reference/cli/lsp.md#configuration) lists.

`Transport::read` answers configuration, workspace-folder, and capability-registration requests itself. Progress creation and semantic-token, code-lens, inlay-hint, and diagnostic refresh requests get `null` at once and fan out to ready editors under broker-owned IDs, and the editors' replies are discarded. That includes rust-analyzer's diagnostic refresh. Any other server request gets `-32601`. Progress, show-message, and log-message notifications broadcast to ready editors, and telemetry, log-trace, and unknown notifications are dropped. A server that emits no work-done progress reaches ready through the ten-second fallback ([Readiness](#readiness)).

| Bound | Limit | Over the limit |
| --- | --- | --- |
| Attached clients | 8 | Attach refused with `-32003` |
| Queued requests per client | 256 | `-32803` |
| Outbound frames per client | 1024 | The client is detached |
| Message size | 64 MiB (`protocol.rs` framing cap) | A protocol error on the reading side |

Editor request counters reach the registry on housekeeping or another publication, not on every request.

### Registry and unsaved output

`registry::Entry::attached` lists each editor's pid, optional name, attachment time, and open URIs with `owner` and `dirty`. Each `OpenDocument.uri` keeps that editor's opening spelling. New entries publish the resolved kind for `status`. Attach, detach, open, close, dirty flips, and housekeeping publish the view, and ordinary buffer changes do not.

`dirty` is per holder: false on open, true after a change, false after save, and kept across an ownership transfer. It means changed since open or save, never a comparison with disk. A buffer already unsaved at open is not detected, and editing back to the disk text does not clear it. `crates/rimz/src/cli/lsp.rs::run` passes the dirty owned URIs (`crates/rimz/src/lsp/query.rs::dirty_documents`) to `query::render`, which compares normalized document identities and replaces the disk source suffix for `def`, `refs`, and `impl` with `(unsaved in editor)` without reading those lines. Definition spans still print before the marker, and JSON is unchanged. The marker is advisory, races between registry selection and rendering included.

## State

| Record | Where | Lifetime | Truth |
| --- | --- | --- | --- |
| Registry entry per key: root and server, broker and server pids and tokens, nonce, state, start and ready times, estimate and settings hash, editor request count, last request time, peak footprint, restarts, leases, attached editors and their open-buffer markers | Machine-level, under `disk::paths::runtime_rimz_root`, atomic writes | Lease lifetime plus shutdown grace, gone at reboot | The matching broker process start token holds ownership, and a nonce-checked socket serves requests |
| Cost history | `lsp-history.jsonl` under the RimZ home, rotating at 4 MiB | Survives restarts | Best-effort append without fsync |
| Query usage | The caller's room, `records/lsp-usage.jsonl` | Room lifetime, independent of registry rows | Shared append, exclusive compaction ([Query usage](#query-usage)) |
| Diagnostics | `~/.rimz/logs/lsp.log.jsonl`, rotating at 4 MiB ([diagnostics.md](./diagnostics.md)) | Survives restarts | Best-effort append without fsync |

The registry is machine-level because admission budgets one machine's memory across every room on it.

`disk::paths::lsp_runtime_dir()` holds `admission.lock`, owner-tagged `queue/` tickets, and one directory per key, named by the first 16 hex characters of the canonical checkout's SHA-256 plus the server name. Each key directory has `entry.json`, `sock`, and a publication lock. There is no shared registry JSON file. Socket paths are checked against the Unix path budget.

`registry::sweep`, which `list`, `status`, `stop`, and attach use, takes the admission lock, and admission calls `registry::sweep_locked` while it holds that lock across its work. Both keep an entry while its broker process token is live, even when `hello` fails, since an unavailable socket must not orphan a live server. Dead brokers become lost rows or are removed ([Lost rows and revival](#lost-rows-and-revival)).

## Query usage

`records/lsp-usage.jsonl` in the caller's room keeps query counts independently of broker lifetimes. The room is resolved as a participant, a pane's pinned room included, and counts belong to that room even when the queried checkout belongs to another repository. A room without a readable workspace record receives no writes. Reading or constructing a usage handle creates nothing.

Each JSONL row carries Unix `at_ms`, checkout `root`, `server`, query `kind`, and `outcome`, and an absent `count` means one. Readers skip malformed lines and count unknown outcomes as neither answered nor failed. Kinds and outcomes stay strings, so newer values survive an older compactor.

The query client records one result per verb target after server selection succeeds, before rendering or exiting. The kind is the verb's serialized name (`def`, `refs`, `hover`, `impl`, `callers`, `callees`, `symbols`, `find`), or `show` and `check` for each file outline they ask a server for. An outline cached within the command is not another question, and a request failure replayed for a second target or file is. Not recorded: selection failures (replayed or not), an unknown `--server`, missing-column helper outlines, and test-classification outlines. Human calls inside the room count. Recording errors are debug-only and change neither output nor exit status.

| Raw query result | Outcome | Count |
| --- | --- | --- |
| An answer other than null or an empty array, or an ambiguous lookup (exit 6) | `results` | answered |
| A null or empty-array answer, or a name not found (exit 5) | `empty` | answered |
| Indexing | `indexing` | failed |
| Memory short or memory pressure | `memory` | failed |
| Other query error | `error` | failed |

Emptiness is judged before scope and `--no-tests` filtering, so a filtered answer still counts as answered.

An append takes a blocking shared `locks/lsp-usage.lock` around one `O_APPEND` write, without fsync. The workspace record is checked at recording time, again after taking the append lock, and before a compaction publishes. Once the file passes 1 MiB, the appender releases its shared hold and tries the exclusive lock without waiting, and skips compaction when another process holds it. Under the exclusive hold it folds rows by `(root, server, kind, outcome)`, sums counts, keeps the latest timestamp, and durably replaces the log. Roots whose directories no longer exist fold to `null`, and their counts stay in the room total after their registry rows disappear. A checkout recreated at the same path before compaction inherits its counts. A power cut can lose trailing appends, never counts already compacted.

List and status each read one snapshot of the participant room's log. Rows show checkout and server counts, and the list header shows the whole-room total, folded roots and keys absent from the registry included. JSON adds only `queries: {answered, failed}` to each entry, and recorded kinds stay in the log.

## Configuration

A room with no `[lsp.servers.*]` table has the feature off. Server definitions may live in machine or project config, and a trusted project entry replaces the same-named machine entry whole, as profiles do. The `[lsp]` memory policy (`reserve-percent`, `reserve-min`, `idle-timeout`, `recycle-factor`, `recycle-min`) is machine-only, because a repository cannot set how much of this machine it may take. The [reference](../reference/cli/lsp.md#configuration) owns every field, its default, and the shipped `rust-analyzer` template with the reason for each option.

`command`, `init-options`, and `editor-check-on-save` run or configure a process, so they join the executable surface ([trust.md § Adding a command-running field](./harness/trust.md#adding-a-command-running-field)), with hash-coverage cases. An absent `editor-check-on-save` and an empty server list are left out of the trust projection, so neither changes the trust hash. `kind` changes neither the trust hash nor the history key. Enabling `editor-check-on-save` changes the learned-memory history key, since cargo checks can raise peak memory.

## What agents see

The launch reminder. `launch_plan::compile` reads `registry::live_server_names` without writing. [`launch_reminders.rs`](../../crates/rimz/src/harness/launch_reminders.rs) lists language names only, in an `- lsp:` bullet under `### Environment`, and points to `Skill(rimz-lsp)`, for children too. The bullet appears even when `env_reminder` is off, and is omitted when no server is registered.

The skill. `rimz-lsp` lives in the user's skill library beside `rimz-subagents`, outside this repository. It teaches the query verbs, the exit codes, and the grep fallback. Profile skill lists apply at every isolation level, through the scoped view or the provider's host-mode switch, and a host provider without a switch warns and runs unrestricted ([sandbox.md § Profile skill views](./sandbox.md#profile-skill-views)). The skill is visible whether or not a server runs, and the CLI's no-server exit covers that case.

The native tool. Any effective server configuration applies `LaunchCapability::disable_native_lsp_args`, even after an optional refusal. Claude merges `LSP` into one `--disallowedTools` flag ([adapter_claude.md § Native LSP denial](./agents/adapter_claude.md#native-lsp-denial)). OpenCode and Grok have no verified switch, and their adapter pages record that gap. `rimz agents validate` reads machine config only and warns about declared `LSP` tools, and `LoadedDefinitions.tools` keeps that metadata apart from the rendered argv.

## Query surface

The [reference](../reference/cli/lsp.md#queries) owns the target grammar, the output, and the exit table. This section owns how `cli/lsp.rs` and `lsp/query.rs` reach those results.

| Verb | LSP request |
| --- | --- |
| `def` | `textDocument/definition` |
| `refs` | `textDocument/references` |
| `hover` | `textDocument/hover` |
| `impl` | `textDocument/implementation` |
| `symbols` | `textDocument/documentSymbol` |
| `find` | `workspace/symbol` |
| `check` | `textDocument/documentSymbol`, once per file with symbol anchors |
| `callers`, `callees` | Call hierarchy incoming and outgoing |

### Targets and exit codes

`rimz lsp` resolves the checkout from cwd (or `--root`) and registry entries. `crates/rimz/src/cli/lsp.rs::run` validates an explicit `--server` against configuration and the checkout's registry rows before any target runs, and an unknown name exits 2. It then runs targets in order and selects a server per target. An uncovered extension fails that target with exit 1, and several matching servers also exit 1, without a configuration-error prefix.

A position-taking verb accepts `path:line:col`, an exact symbol name resolved through `workspace/symbol`, or a file-qualified `path::Name`. The head of `path::Name` must pass the file-head test `check` applies to anchors (`query::is_file_head`). Its path names files the way `check` resolves an anchor's path, through the shared `query::checkout_relative`: the exact file when one exists, else every suffix match, decided by one stat and no file listing. The name then resolves among the candidates in a file the path names, for the direct lookup and the member fallback's qualifier alike. Navigation uses a workspace-symbol location's start position as the target position.

`query::Output::exit_code` and `query::QueryErr::exit_code` own query outcomes and error codes, and `check::Report::exit_code` owns anchor-check outcomes. `QueryErr::server_error` supplies the shared exit 3 and 4 message for queries, checks, and source reads, memory refusals and memory-pressure stops included. Several targets fold by severity: 3 > 4 > 1 > 7 > 6 > 5 > 0. The exit codes are a contract with the `rimz-lsp` skill, which teaches agents to branch on them.

### Name resolution

A symbol name goes through these stages:

1. Lookup. `workspace/symbol` returns candidates, and the workspace matcher keeps exact-name matches (subsequence and named-file rules). With no exact match, every subsequence match stays.
2. Enrichment. Ambiguous matches gain outline ancestors in `container_name`. Each ancestor loses generic arguments and reduces to its last whitespace token, so an impl header contributes its self type. Enrichment is all-or-nothing within the attempted set: more than 20 distinct files, a flat outline, or any missing node leaves the set unchanged and skips preference.
3. Preference. Matches directly under the last written container win. A file head without qualifiers prefers top-level items, and a bare name gets no preference. Candidate names then carry inline modules as well as types, while not-found suggestions and `find` keep workspace-symbol names.
4. Recovery of a missing qualified name. When `workspace/symbol` lacks the qualified name, its exact-name candidates first narrow to the named files, or to files matching a leading qualifier prefix that ends at the file-derived module. Module narrowing omits server containers. Each candidate is judged on its own, so overlapping module paths such as `a.rs` and `a/tests.rs` stay eligible together. The narrowed set is enriched as in stage 2 and rematched with its outline containers restored, so unrelated files cannot cancel a narrowed rematch. Failed enrichment or no match retries the full exact-name set under the 20-file cap, reusing outlines already fetched.
5. Member fallback. As a last step, the qualifier resolves by workspace lookup alone (no definition collapse, no recursion), and its `documentSymbol` children are matched. More than 20 qualifier matches skip the fallback. Each matching qualifier's outline node is the one whose `selectionRange.start` equals its indexed workspace-symbol position. Failing that, as for ty, whose workspace-symbol range spans the whole declaration, it is the earliest same-named node whose `selectionRange.start` falls inside that range. Only its direct children are matched, and the child's selection position feeds navigation. Several members stay ambiguous, no member keeps the original not-found candidates, and flat outlines supply none.
6. Definition collapse. Not-found and ambiguous sets are ranked with the qualifier-score, name, and position order of `query::render_outcome`, and the first `COLLAPSE_CAP = 30` resolve through `textDocument/definition`. A unique path match still makes one collapse request. Results group by definition URI and start position: exactly one returned location becomes the candidate's identity, and an empty or multiple answer keeps its own location. One group resolves uniquely at its definition only when no candidate is left unresolved. Distinct groups keep the candidates' names and locations, so each printed name is accepted input, preferring the definition's own indexed symbol over a re-export.

Counts include resolved groups plus unresolved candidates, shown as `up to` in text and `total` and `truncated` in candidate JSON. Raw workspace-symbol candidates are kept for not-found suggestions. Request errors propagate in input order.

### Request budget

Outline, collapse, and call-hierarchy fan-outs run through one order-preserving pool of at most four workers. Each fan-out keeps input order and returns the first error in input order with its error type intact.

| Request | Most per symbol |
| --- | --- |
| `workspace/symbol` searches | 3 |
| Collapse definitions | 30 |
| Enrichment outlines (ambiguous matches, or a missing qualified name's candidates) | 20 |
| Member-fallback outlines | 20 |
| Navigation request | 1 |
| Call-hierarchy expansion | 1 per item `prepareCallHierarchy` returns (usually one, not capped) |

A not-found result can spend both the 20 enrichment outlines and the 20 fallback outlines. Test-last and `--no-tests` rendering add their own per-file outlines (below). `lsp/check.rs` keeps its own sequential per-file outline loop.

### List rendering

The five list verbs (`refs`, `impl`, `callers`, `callees`, `find`) show results inside the checkout by default, and `--external` includes the rest. `query::render` decides per verb, and `def`, `hover`, and `symbols` ignore scope. The list arms share an inside-checkout predicate and a hidden-count footer with an `--external` hint. Counts follow each verb's deduplication, and `find` does not deduplicate. JSON stays the raw, unfiltered LSP answer. `find --external` changes only the output: the `workspace/symbol` request is the same.

`refs`, `callers`, and `find` list production entries before test entries, separated by one `tests:` line. Within each group, checkout results come before external ones. `find` keeps its ranked order, and references and call items group by file, then position, with call items breaking ties by name. A file with both kinds repeats under the test boundary. `impl`, `callees`, and flat `symbols` answers are not classified.

`query::ListOptions` carries the scope and a nonzero optional limit, where `None` means `--all`. Text lists cap at `LIST_CAP = 50` locations after filtering and ordering, before reading snippets. The CLI rejects an explicit `--limit` or `--all` with `--json`. Single-answer verbs ignore the limit. Snippets are trimmed to 100 Unicode characters, and `render` caches source lines per normalized document URI, failed reads included, reading source only for displayed, clean locations.

Test classification reuses the path and module-name rules of `--no-tests`, caching one outline per normalized document URI. It walks the output order until the production count reaches the limit, and `--all` and `--no-tests` classify every entry. `--no-tests` drops tests before the cap. Classification outlines use a 5 s readiness wait and a 10 s socket timeout, apart from the main query's 110 s and 115 s. The first outline request or decode failure stops further outline requests, and that entry and every later one fall back to the path rule instead of failing the query, `--no-tests` included.

The header depends on how the walk went:

| Walk | Header |
| --- | --- |
| Complete, no outline failure, tests found | `, N in tests` after the total and `, tests last` after the shown count |
| Incomplete, or an outline failed | `, tests last` only, since module counts are unknown |
| Complete, no tests, or `--no-tests` | Plain |

The `--no-tests` hidden count stays exact, since it counts dropped entries, so the hidden-test footer still prints. Truncation adds the total and shown header and the cap footer before the hidden-external footer.

## Anchor checks and source reads

`lsp/check.rs` extracts inline-code anchors with the Markdown parser, and repo-qualified anchors short-circuit as `external` before path resolution. `crates/rimz/src/lsp/check.rs::resolve` funnels check, show, and `--fix` through one path rule:

1. An exact checkout-relative path that is a regular file on disk resolves, ignored by Git or not.
2. Otherwise a unique component-boundary suffix over Git's tracked and untracked, non-ignored files resolves. Check reports it `short-path`, and `--fix` completes it without an outline or editor check.
3. A deleted file whose removal is not yet staged stays `missing-path`.

Symbol matching uses ordered ancestor subsequences, removes generic arguments, and also compares each container's last whitespace-delimited token, so container headers match without any Rust keyword in the matcher, for flat and hierarchical outlines alike. `crates/rimz/src/lsp/check.rs::symbol_hits` prefers matches directly under the last written container, or top-level matches under a file head without qualifiers. Without an exact match every subsequence match counts. Check, show, and hint fixes share this preference before hints are considered, and `crates/rimz/src/lsp/check.rs::named` breaks a type-versus-impl tie.

In check, a line-only anchor reads disk without a server. A symbol anchor with no configured server for its language is `unchecked`, and a configured server's failure aborts the whole report through the query error path. The [reference](../reference/cli/lsp.md#check-anchors-in-a-notes-file) owns the grammar and the report format.

`crates/rimz/src/lsp/check.rs::Context` shares Git listing, path resolution, server selection, and a per-file outline cache between notes checks and source reads. Candidates keep full and selection ranges plus direct-parent indices. `lsp/check/show.rs` selects items and zooms large bodies without truncation. A bare line (`path:N` or `path:~N`) selects the innermost extent covering any character of the line, an indented first line included and an end line at character zero excluded. It falls back to the disk line, with the owning-editor dirty marker, when no extent covers it or the outline is unavailable or fails. An explicit line range reads disk without an outline. A failed show argument leaves later arguments running, unlike a notes check.

## List, status, and doctor

`rimz lsp list` and `stop --all` select the current room's keys by matching each entry's recorded `project` against the resolved workspace's `project_root` and `launch_repo_root` (`crates/rimz/src/cli/lsp.rs::room_entries`). The machine-wide sweep runs first. List shows running and alarm rows by default, `--all` adds the rows holding no memory, and JSON always returns every room key. Other repositories with running entries are grouped by project, or by root for rows with no project, outside the room table.

List checkout labels come from `crates/rimz/src/worktree.rs::classify_checkout`: the room's project root is `#main`, and a linked worktree of that project uses its directory name. Classification reads the checkout's own `.git` metadata and `commondir` without spawning Git, and does not borrow a parent checkout's identity for a nested directory. Another repository, or missing metadata, falls back to the home-relative path. Doctor, stop receipts, status's checkout row, and other-room lines print paths.

List and doctor use `crates/rimz/src/lsp/memory.rs::for_entry` to reject a gone or reused server process before sampling. List samples each running room row once for its table and total, and dormant rows show their recorded peak. `crates/rimz/src/lsp/history.rs::Needs` reads history once per command and applies the last-five rule of [Learned cost](#learned-cost), falling back to the entry's estimate. `crates/rimz/src/lsp/admission.rs::Headroom` supplies the fit calculation admission uses.

Status predicts the next query from state, needs, and headroom, and measures ages from each state's own stamp. It also shows editor request counts since broker start and restart counts excluding the first lazy start. List and status use today's machine reserve policy, which can differ from a broker's spawn-time policy until it respawns. For `--server` validation, status and stop read machine and project configuration best-effort, and a read failure skips the check. Status also reads machine configuration, best-effort, for its memory prediction.

`rimz lsp list` and `rimz doctor` share `crates/rimz/src/cli/render/status.rs::lsp` for state roles, `crates/rimz/src/cli/render/mod.rs::lsp_state_label` for state words, and `crates/rimz/src/cli/render/status.rs::lsp_order` for row order. Doctor shows list's STATE, CHECKOUT, SERVER, RSS, SWAP, TOTAL, and LEASES columns and the last refusal. Doctor JSON adds `swap_bytes` beside `rss_bytes`, while list JSON adds room query counts to the registry fields without live memory.

Doctor also calls `lsp::broker::probe::check_startup` for the resolved checkout's effective configuration, with admission's validation and root-marker selection. A starting, indexing, or ready entry whose broker process is alive reports `running` without a second process, and a stale entry left by a killed broker is probed. A configuration or checkout-resolution error lands in `checks_error` and leaves the machine-wide table in place. Other selected servers are probed concurrently through the broker's spawn and `initialize` parameters, with a ten-second deadline and bounded stderr capture. Probes never send `initialized`, take the registry lock, or publish an entry. They send `shutdown` and `exit` after a successful reply and kill the process group at the end. They skip memory admission because they request no workspace indexing, and they run in doctor's environment, which can differ from a broker launched outside a sandbox.

The `lsp.ready.checks` JSON array is sorted by server name:

| Check | Fields | Counts as |
| --- | --- | --- |
| `running` | `state` | Healthy |
| `started` | nullable `version` | Healthy |
| `failed`, `timed_out` | `cause` (the registry crash shape), `fix` | Problem (`timed_out` only warns) |
| `invalid` | `error`, `fix` | Problem |
| `untrusted` | `fix` | Problem |

Untrusted project servers are never executed. Rustup's missing-component stderr yields a component-install command, and other failures point to the configured command and checkout. A dormant-crashed row shows the recorded exit and the last five stderr lines whatever the probe finds.

## Visibility

Automation here is internal repair, so it keeps diagnostic records, never assist records ([diagnostics.md](./diagnostics.md)). Each record in `lsp.log.jsonl` carries `at`, `root`, `server`, `event`, and `details`:

| Event | Written by | Details |
| --- | --- | --- |
| `refused` | Admission: an optional or untrusted server's configuration error, an invalid policy with no matching required server, an optional server that did not publish within five seconds, a query-time memory shortfall | `reason`, or the `Shortfall` figures |
| `queue_timeout` | A required launch reaching `wait-timeout` | The `Shortfall` figures |
| `killed` | The watchdog | `reason`, `peak_rss_kb`, `free_before_bytes`, `free_after_bytes`, `swap_free_before_bytes`, `swap_free_after_bytes`, `victim_rss_kb`, `reserve_bytes`, `pressure_some_avg10`, `pressure_full_avg10` |
| `evicted` | Query-time admission | The `killed` fields plus the requester's `estimate_bytes` and `for: {root, server}` |
| `recycled` | Housekeeping | `rss_kb`, `swap_kb`, `ready_footprint_kb`, `ceiling_kb` |
| `crashed` | The owning broker at a crashed lifetime's end | The `last_crash` cause, `reason`, `peak_rss_kb` |
| `watch_error` | The saved-file watcher | `path`, `error` |
| `broker_exited` | Every broker return and main-thread panic, setup failures and terminal stops while dormant included | Exactly one of `reason` or `error`, `broker_pid`, lease count |
| `revived` | A replacement broker taking over a dead one's row | The previous `broker_pid`, `state`, `last_crash`, adopted lease count, `has_live_lease_holder` |
| `swept` | `registry::sweep` removing a row | `broker_pid`, `state`, lease count, `peak_rss_kb`, `last_crash` |
| `unreadable_entry` | `registry::read_entries_at`, at most once per row per process | `root` is the row's directory, `server` is empty, `details.error` |
| `retired` | `rimz reload` ([Build retirement](#build-retirement)) | `broker_pid`, `build`, `installed`, `outcome` |

A required server's configuration error, or an invalid policy with a matching required server, refuses the launch and writes no record. A shortfall's `available_bytes` is RAM only, and `swap_free_bytes` beside it is free swap (read as 0 where absent). Their sum is the figure admission compared. `pressure_some_avg10` and `pressure_full_avg10` are the ten-second memory pressure-stall averages, null where `/proc/pressure/memory` is unreadable. They are logged only, and no decision reads them. `free_before_bytes` and `free_after_bytes` are RAM only. A self-retirement records `broker_exited` with `reason: "retired"`, and an external kill cannot record its own exit.

Diagnostics add no server lifetimes to cost history. Idle stops and restarts write no diagnostic, since history carries them. Lost rows do not show in `rimz lsp list` or `status`, and their kept `entry.json` and these records are the evidence. Query memory-short errors come from the broker's refusal, never from reading diagnostics.

## Open questions

- An exact hard cap. Recycling bounds growth only while a server is Ready and quiet, not while it indexes or holds a hung request. On Linux with systemd, a per-server cgroup with `MemoryMax` near 1.3 times the learned peak would give exact accounting and a kill that can land only on the server. The watchdog and `oom_score_adj` stay the backstop until then.

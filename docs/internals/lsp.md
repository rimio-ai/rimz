# Shared language servers

The [CLI reference](../reference/cli/lsp.md) owns commands, flags, exit codes, and configuration. This page owns the process model implemented in [`lsp/`](../../crates/rimz/src/lsp/mod.rs).

A room can run one language server per (checkout, configured server name), shared by agent queries and attached editors, instead of each client starting a private one. The feature is off until a room configures a server, and it is enrichment: an agent without a server works exactly as agents work today, navigating with grep.

## Why RimZ owns this

A language server is expensive to hold. Measured on this repository, one `rust-analyzer` reaches 4.4 GB after its first index, 6.3 GB once `workspaceSymbol` and `findReferences` have run, and its startup `cargo check` briefly takes the process tree to 8.1 GB. A harness-native LSP tool starts one server per agent session and keeps it for the session's life, so a three-seat team plus its subagents holds three or more copies of the same index over the same checkout, idle or not.

RimZ resolves every launch's checkout before any pane opens ([fleet.md § One launch, end to end](./harness/fleet.md#one-launch-end-to-end)). Wrapper process leases supply liveness without coupling the machine-level broker to a room's store or multiplexer. A sandboxed launcher can leave the broker in its inherited mount view; its paths must remain reachable there.

## The rules that shape it

**Enrichment, never a precondition by default.** Memory refusal of an optional server, or a server that dies later, leaves agents on grep. The fail-fast invariant still governs configuration: a configured server whose binary is missing or whose command is untrusted refuses at the launch entry point with the fix. Memory refusal is visible to the human ([Visibility](#visibility)).

**Registered at launch, started on demand.** Optional launches create a dormant broker, not a server process. The first agent query or editor request triggers memory admission and startup; required launches admit and start eagerly. Launch reminders name dormant and running servers alike, so no tool change is needed when a server restarts.

**One server per key, shared buffers.** The key is (canonical checkout root, server name). Agents only query; attached editors own buffers ([Editors](#editors)). Disk is the truth for files no editor holds, and the broker keeps the server's view of saved files current ([Freshness](#freshness)).

**Agents and editors own the broker lifetime.** Wrapper and editor process leases keep the broker alive; the last release or detach starts the 60-second release grace. Idle timeout, memory pressure, eviction, a hand stop, a crash, or a team's Done flip stops its server back to dormant; the next agent query or editor request can restart it. Checkout removal and lease expiry end the broker.

**Memory is the only admission budget.** No concurrency cap. A server starts only when its estimate fits under free memory with the reserve intact.

## Lifecycle

### Which launches start a server

Every fleet launch entry point except a subagent launch registers brokers for its checkout before opening panes: `rimz agents`, `rimz teams`, cohort and lane resume, rebirth recovery, and supervised `-p` runs. It runs after the worktree exists (step 5 of the launch) and before the exec wrappers compile (step 7), once per launch rather than once per cell. A subagent works in its parent's checkout ([subagents.md § The child works where its parent works](./harness/subagents.md#the-child-works-where-its-parent-works)), so it joins its parent's broker, dormant or running; its queries can wake that server too.

Admission for a live key joins it without starting another server. The wrapper registers its lease separately after launch-plan application. `cli/lsp_admission.rs::admit` is the one helper at the five admission sites; rebirth obtains distinct roots through `RebirthPlan::checkout_roots`.

### Admission

Admission reads free memory and the machine-wide registry under one lock (`disk::lock`, at a machine-level path under `disk::paths::runtime_rimz_root`), so simultaneous starts cannot both pass on the same free gigabytes. Optional launch skips memory admission; the broker's main thread runs it on the first query and every restart. Required launch admits its first lifetime before spawning the eager broker.

```text
available  = MemAvailable, capped by the cgroup's memory.max headroom when RimZ runs under one
committed  = Σ over running servers of max(0, entry.estimate_bytes − current_tree_rss)
reserve    = max(reserve-percent × MemTotal / 100, reserve-min)
admit when available − committed − estimate ≥ reserve
```

`committed` is what makes the check honest: a server at 4.4 GB that has reached 6.3 GB before will grow again, and that growth is already spoken for. Dormant entries reserve nothing. `estimate` is re-read for each lifetime: the learned peak for this (project, server, settings) triple, or the configured `memory-estimate` before any history exists ([Learned cost](#learned-cost)).

What happens on a refusal depends on the server's `policy`:

| Policy | Refused admission |
| --- | --- |
| `optional` (default) | Launch does not check memory. A query-time refusal leaves the broker dormant, returns exit 3, and records the shortfall diagnostically. A later query retries admission. |
| `required` | The launch waits in first-come order among required waiters before panes open. Admission polls every five seconds; the launcher's terminal prints the wait when the queue position changes. There is no pre-launch sidebar row. At `wait-timeout` the launch fails with the fix, naming memory needed, memory free, and the holders. Optional launches do not queue behind required ones. |

`required` gates initial launch admission only. Joining a live broker, including a dormant one, does not repeat eager admission. After a stop, either policy restarts lazily and a memory refusal returns exit 3; RimZ never stops agents to honour required policy.

`admission::admit_query` can evict other servers to make room. Candidates must be ready, have a live broker, have been ready for at least five minutes, and have no request in the last 120 seconds (measured from the later of readiness and last request). Starting servers and the requesting broker are protected. Candidates follow `registry::kill_order`; admission stops one at a time with reason `evicted`, waits up to three seconds for token-guarded server death without a kill fallback, then re-samples and stops as soon as the request fits. Required launch admission does not evict.

Admission reuses the launcher's lenient machine config and skips effective config loading when neither machine nor project declares servers. Recovery warns and skips admission on any admission error, so a config, registry, or memory-sampling failure never blocks Recover; other launch paths retain their strict effective-config checks. Supervised launches finish provider preflights before admission starts a server.

### Leases and shutdown

`cli/agents_cmd/exec.rs::run_exec` registers leases after `launch_plan::apply`; `settle_after_exit` explicitly releases them on resident-wrapper paths. Each lease carries an optional launch id, wrapper pid, and process start token. Recovery of older sessions without launch ids still takes a lease; absent and null ids are accepted on the wire. The broker reaps dead owners every five seconds, rejecting pid reuse; direct `exec` preserves the wrapper pid. It does not read store or pane state. Worktree removal calls `registry::stop_checkout` best-effort, and the broker checks root existence every five seconds as a backstop.

The last release arms a 60-second grace, canceled by a new lease, to cover restart's gap, including while dormant. A fork takes its own lease. A broker never leased exits after five minutes. Queue tickets likewise carry an owner pid/start token; abandoned tickets are removed, and dropping the launcher's `WaitQueue` removes its ticket.

Housekeeping runs every five seconds whether dormant or running. A ready server with no query in flight stops with reason `idle` once `idle-timeout` has elapsed since the later of readiness and last request. Starting and indexing servers are protected. `team_stage::open_stage` best-effort stops the board's checkout with reason `team done` at the terminal return, under the board lock; errors only log at debug level and never fail the flip. Worktree removal uses `checkout removed`. That reason, `released`, and `never leased` are terminal: the broker publishes `Stopped`, closes requests, removes its directory, and exits.

### Readiness

Launch admission waits up to five seconds under the admission lock for the broker's entry, not its index. After `initialize`/`initialized`, open progress tokens mean `indexing`. The server becomes `ready` when all tokens close, at least one has ended, and two seconds have passed since the last progress event; without any progress, the fallback is ten seconds after initialization. The settle window prevents a short initial token from claiming readiness before indexing begins. Later progress can return it to `indexing`. Each lifetime resets readiness and start/ready timestamps. A query waits at most 30 seconds for startup and readiness, then reports elapsed time for this server lifetime rather than an empty result for an unfinished index.

## Memory pressure

### The watchdog

Every live broker samples free memory on its five-second housekeeping cadence. Below `kill-floor-percent` of total memory, the machine-lock winner selects victims and re-samples between stops. It first sends a socket stop with reason `memory pressure`, allowing another broker two seconds to shut down after acknowledgment. A server still alive is killed with token-guarded SIGKILL on its process group; when the elected broker selects itself, it kills its server group directly rather than waiting on its own housekeeping loop. Each server starts as a process-group leader, so shutdown also kills its cargo and proc-macro children. The watchdog records free bytes before and after the stop, victim tree RSS in KiB, and peak RSS in the `killed` diagnostic. Servers receive `oom_score_adj = 800` so the kernel favors them over agents if it acts first.

### Who is killed first

`registry::kill_order` is plain least-recently-used order: last request, falling back to start time, then start time, checkout root, and server name. The watchdog considers starting, indexing, and ready servers, never dormant ones. Unlike admission eviction, real memory pressure ignores age and idle floors. Both paths share `watchdog::stop_victim`; neither publishes the victim's entry or writes its history. The owning broker records the lifetime's end and becomes dormant.

### The agent's view of a kill

Agents are not messaged. The next query against a dormant key requests a restart. It answers normally if ready within the wait, exits 4 if still indexing, or exits 3 if memory admission refuses. A query interrupted by shutdown can fail; the next call can retry without relaunching the agent.

## Learned cost

`peak_rss_kb` is the monotonic maximum of summed kernel `VmHWM` over the process tree (`proc::tree_peak_rss_kb`), floored by sampled tree RSS through `lsp::memory::tree_peak_kb`. Broker housekeeping refreshes it; the watchdog also samples the victim's peak for its diagnostic without publishing the entry. This catches spikes shorter than the five-second sampling interval. Summed high-water marks can overcount processes that peaked at different times and omit children that have exited; the sampled floor preserves previously observed usage. Off Linux, only the sampled floor is available.

The owning broker appends one `lsp/history.rs::Record` per server lifetime to rotating `lsp-history.jsonl` under the RimZ home: project, checkout, server, lifetime peak tree RSS, spawn-to-first-ready time, settings hash, stop reason, and `dormant_ms` (the preceding stop-to-start gap, absent for a first start and defaulted for old records). Entry peak RSS remains monotonic across lifetimes, but history uses each lifetime's peak. Admission takes the maximum of the last five records for the same (project, server, settings) triple, falling back to `memory-estimate` without history. Several short lifetimes can therefore age out an older high peak; the watchdog remains the backstop. The project is the workspace's `launch_repo_root()`, resolved by `cli/lsp_admission.rs::admit`, so checkouts of one project share learned costs. The registry key remains checkout/server. Records without a project are skipped when estimating, without migration. The settings hash covers command argv and canonical initialization options. This is an admission input, not a diagnostic log.

## Freshness

`lsp/broker/watch.rs` batches `notify` events into `workspace/didChangeWatchedFiles`. Non-recursive watches cover individual directories, excluding `.git`, `target`, and `node_modules` before registration. Created or moved-in directories gain watches; a scan also forwards files saved before registration. Configured extensions and file names in `root-markers` pass, including manifest edits such as `Cargo.toml`. Removed directories and rename source paths are forwarded as Deleted so the server drops the old tree. Create, change, delete, and rename events map to LSP event types. A directory that cannot be watched and notify error events produce `watch_error` diagnostics rather than stopping the server; freshness is degraded for missed events. Errors that end the broker's run are included in its `crashed` diagnostic. Freshness depends on the server handling these notifications, not on unsaved editor buffers. Watch traffic never increments request counters.

## The broker

A language server speaks to exactly one client over stdio. Each server therefore gets one broker: a hidden `rimz lsp serve` process that owns the server's stdio, listens on a per-key Unix socket, serializes nothing it does not have to, and speaks the `rimz lsp` query protocol to callers.

It is spawned through `child_process::spawn_detached_rimz` and calls `setsid`. `broker::serve` loops over server lifetimes, remaining dormant between them while leases live. Each lifetime recreates the child, transport, and saved-file watcher. It is a per-key process bounded by leases, not a room-wide service ([fleet.md](./harness/fleet.md#the-rules-that-shape-it)). Standard threads own socket handling and Content-Length transport; request IDs match concurrent replies. The reader answers configuration, workspace-folder, progress-creation, and capability-registration requests.

The 0600 newline-JSON socket accepts `hello`, `lease`, `release`, `query`, `stop`, `status`, and `attach`; responses carry the broker's nonce across lifetimes. Queries carry `method`, `params`, and `wait_ms`, returning raw LSP `result`, elapsed `indexing`, `refused` with a `Shortfall`, or `error`. A dormant query signals a start request and waits for readiness, terminal stop, a new refusal, or its deadline. An RAII in-flight counter covers the whole query, including that wait, protecting it from idle stop. Agent queries and editor requests increment request counters; editor requests do not hold the in-flight counter. The CLI owns query verb semantics, not the broker.

The broker answers `hello` before publishing its first entry, without taking `admission.lock`: launch admission holds that lock while waiting for publication. Server pid/token are absent while dormant or stopped. Only the owning broker publishes its entry. The main thread owns admission and server lifetimes and never holds the model mutex while waiting on the machine lock, a socket, or process death. The socket thread only signals starts and stops; it never spawns or kills processes.

If the initial entry is not published within five seconds, optional admission returns a refusal and records it diagnostically while the launch proceeds. Required admission fails with the server name and a command/configuration fix. Stop reasons use `registry::StopReason`, serialized as the existing human-readable strings in registry, socket, and history records. The broker acknowledges both capability registration and unregistration with `null`.

Two traps follow from spawning:

- **A sandboxed launcher.** An agent may run `rimz teams` from inside its bubblewrap view. A broker spawned from there inherits that mount namespace, where `/tmp` is room tmp. Because the sandbox rebinds the runtime home, the RimZ home, and every checkout at their host paths ([sandbox.md § Reachable host paths](./sandbox.md#reachable-host-paths)), the broker must name only such paths: its socket and registry live under `disk::paths::runtime_rimz_root`, never under `/tmp`.
- **The server's own build.** `rust-analyzer` runs `cargo check` on startup and save by default. The configuration template turns it off (`checkOnSave = false`): the read-only query surface exposes no diagnostics, the check cost 57 CPU seconds and a 3.7 GB spike on this repository, and it would contend for the checkout's `target/` lock with the agents' own builds. The broker does not inject this option for custom definitions.

## Editors

`broker/socket.rs::handle` upgrades a connection with a newline-JSON `attach` operation carrying `pid` and `start_token`. Success returns `ok`, `nonce`, and `client_id`, then both directions use Content-Length framing. One buffered reader spans the handshake and frames so pipelined bytes survive; both socket timeouts are cleared after the reply. Attachment registers a process lease without a launch id. Closing or stopped brokers, a stale process token, and the client limit refuse with `-32003` before upgrading. EOF and read errors detach; terminal shutdown drains queued replies and stop notifications with a one-second socket write timeout before closing both directions of every attached socket, while dormancy keeps connections open.

`broker/router.rs::Router` is the single writer of editor-originated server traffic. It drives the pure `broker/clients.rs::Clients` state machine; socket readers only enqueue events. The transport allocates server request IDs, and the router maps replies and cancellation back to each client's original ID, preserving server error objects including `data`. Lifetime generations discard stale replies and notifications. No model lock spans socket or transport writes; the agent query path remains separate.

### Ownership and replay

`Clients::document` keeps each URI's holders in open order, each with its own text, version, language id, and dirty marker. The first opener owns the server buffer. Other holders' changes stay cached, and their requests run against the owner's text, as agent queries do. The cached initialization reply advertises full-text synchronization (`change = 1`), preserves the server's save and will-save settings, and disables workspace-folder change notifications. A change must contain one full-text item without a range; incremental changes are ignored.

`Clients::sync` reconciles ownership with the server view: one `didOpen` for a new URI, a whole-document `didChange` when its owner's version advances, and `didClose` when the last holder leaves. Closing or detaching the owner transfers to the next holder with `didClose` then `didOpen`, using that holder's cached text and version. Only the owner's `didSave` reaches the server. A lifetime end clears the server view, not the held buffers; after the next server `initialized`, owned documents replay before queued editor requests. Broker readiness waits for the router's replay acknowledgement as well as the indexing rules above.

Editors' `initialize`, `initialized`, `shutdown`, and `exit` never reach the server. `initialize` starts a dormant lifetime and answers from the cached server result, or waits for the first result. `shutdown` returns `null` and makes later requests from that editor fail with `-32600`; neither restarts a dormant server or refreshes idle accounting. `exit` detaches only that editor. Dormant requests queue and request a restart. Accepted editor requests refresh idle accounting but cannot pin the server through a hung request. On lifetime end or admission refusal, every held request, forwarded or queued, fails with `-32802` (ServerCancelled). Each client receiving at least one such answer gets exactly one `window/showMessage`, whether ready or not: Warning for a stop, Error for refusal. An editor with nothing outstanding gets no popup, only the readiness-gated server-status broadcast on lifetime end. Every starting epoch ends once, including a stop before spawn or initialization; failing its queued `initialize` does not automatically retry a broken server.

### Readiness and diagnostics

An editor is ready only after its own `initialized`. Before that it receives responses to its requests and the broker's accompanying failure message, but no broadcasts or fan-out requests. `Clients::server_message` caches `experimental/serverStatus` and replays the latest value when the editor becomes ready. Lifetime end caches and broadcasts a warning status naming the stop reason, with a next-request restart hint only for non-terminal stops. Progress still feeds broker readiness; late editors do not receive a replay of in-flight progress creation.

`Clients::diagnostics` caches a publication for an open server document with the text and server-document incarnation it describes; the frame retains the publication's version. Incarnations advance on each server-side open, including transfers and lifetime replay. Delivery to ready editors follows these rules:

- The owner receives the publication verbatim. A publication with a version different from the current server view reaches only the owner and is not cached.
- Another holder receives it only when its text equals the snapshot text, with the version rewritten to that holder's version. An editor not holding the URI receives it without a version.
- An empty list clears every ready editor, strips the version for non-owners, and removes the snapshot. A non-owner holder diverging from text whose diagnostics it received gets one empty, unversioned clear; owner changes await the server's next publication.
- A ready opener, or a holder becoming ready, receives a valid cached snapshot under the same text/version rule. Owner changes, ownership transfer, last close, and lifetime start/end invalidate snapshots before they can be replayed.

Known limit: a pre-transfer publication arriving after transfer when both owners used the same numeric version is indistinguishable from a current publication. The broker treats it as current; it does not remap document versions on the server side.

### Settings, capabilities, and bounds

The broker owns server settings. Editor initialization options and `workspace/didChangeConfiguration` are ignored, as are editor workspace-folder changes, watched-file changes, and trace settings. The broker's saved-file watcher remains the only watched-file notifier. Attaching an editor does not toggle `checkOnSave`.

`broker.rs::client_capabilities` sends one fixed capability set, not the union of editors' advertised features. It includes completion, hover, navigation, formatting, rename, semantic tokens, inlay hints, and hierarchies. Intersection-safe choices omit snippet text edits, position-encoding negotiation, apply-edit requests, and dynamic registration except watched files. `linkSupport` stays off because every editor renders `Location`, not because agent queries cannot parse `LocationLink` (they can). The deliberate Rust exception advertises `experimental.commands`, `hoverActions`, and `codeActionGroup` to preserve rust-analyzer's VS Code lenses and hover actions; snippet edits, open-server-logs, and test-explorer capabilities remain off.

`Transport::read` answers configuration, workspace-folder, and capability-registration requests locally. Progress creation and semantic-token, code-lens, inlay-hint, and diagnostic refresh requests receive `null` immediately and fan out to ready editors under broker-owned IDs; editor replies are discarded. This includes rust-analyzer's ungated diagnostic refresh. Other server requests receive `-32601`. Progress, show-message, and log-message notifications broadcast to ready editors; telemetry, log-trace, and unknown notifications are dropped.

Bounds are eight attached clients, 256 queued requests per client (`-32803` on overflow), 1024 outbound frames per client (overflow detaches), and the framing layer's 64 MiB message cap. Editor request counters reach the registry on housekeeping or another publication, not on every request.

### Registry and unsaved output

`registry::Entry::attached` lists each editor's pid, optional name, attachment time, and open URIs with `owner` and `dirty`. Missing `attached` defaults to an empty list for older entries. Attach, detach, open, close, dirty flips, and housekeeping publish the view; ordinary buffer changes do not publish it repeatedly.

`dirty` is per holder: false on open, true after a change, false after save, and preserved on ownership transfer. It means changed since open or save, not compared with disk. A buffer already unsaved at open is not detected; editing back to disk text does not clear it. `cli/lsp.rs::run` passes dirty owned URIs to `query::render`, which replaces disk-source suffixes for `def`, `refs`, and `impl` with `(unsaved in editor)` without reading those source lines. JSON stays unchanged. The marker is advisory, including races between registry selection and rendering.

## State

| Record | Where | Lifetime | Truth |
| --- | --- | --- | --- |
| Registry entry per key: root/server, broker and server pids/tokens, nonce, state (`dormant`, `starting`, `indexing`, `ready`, or terminal `stopped`), lifetime start/ready times, estimate/settings hash, request count, last request time, peak RSS, restarts, leases, attached editors and open-buffer markers | machine-level, under `disk::paths::runtime_rimz_root`, atomic writes | through lease lifetime and shutdown grace; runtime files do not survive reboot | matching broker process start token retains ownership; a nonce-checked socket serves requests |
| Cost history | machine-level under the RimZ home, rotating JSONL | durable | append-only |
| Kills, evictions, refusals, and queue timeouts | `diag/` diagnostic records | durable | append-only |

The registry is machine-level rather than per-room because admission budgets one machine's memory across every room on it.

`disk::paths::lsp_runtime_dir()` holds `admission.lock`, owner-tagged `queue/` tickets, and one directory per key: the first 16 hex characters of the canonical checkout SHA-256 plus server name. Each directory has `entry.json`, `sock`, and a publication lock. There is no shared registry JSON file. `registry::sweep_locked`, used by admission, `list`, and `stop`, retains entries while the broker process token is live even when hello fails; an unavailable socket must not orphan a live server. Socket paths are validated against the Unix path budget.

## Configuration

A room with no `[lsp.servers.*]` table has the feature off. Server definitions may live in machine or project config, with project entries overlaid on machine ones as profiles are; the memory policy is machine-only, because a repository cannot set how much of this machine it may take.

```toml
[lsp]                                  # machine config only
reserve-percent = 10
reserve-min = "8G"
kill-floor-percent = 5
idle-timeout = "10m"

[lsp.servers.rust]
command = ["rust-analyzer"]            # trust-hashed in project config
extensions = ["rs"]
root-markers = ["Cargo.toml"]
init-options = { checkOnSave = false, workspace = { symbol = { search = { kind = "all_symbols", limit = 10000 } } } }
policy = "optional"                    # or "required"
wait-timeout = "10m"                   # required only
memory-estimate = "8G"                 # until learned
```

`command` and `init-options` run or configure a process, so both join the executable surface ([trust.md § Adding a command-running field](./harness/trust.md#adding-a-command-running-field)), with a hash-coverage case each.

Trusted project entries replace machine entries whole by name. The empty trust projection is omitted so existing hashes hold. Sizes are decimal (`8G`); `8GiB` is binary. The [reference](../reference/cli/lsp.md#configuration) owns field defaults. The Rust tuning includes functions and raises rust-analyzer's default 128-result workspace search cap, which otherwise hides symbol-name navigation targets.

## What agents see

**The launch reminder.** `launch_plan::compile` reads `registry::live_server_names` without writing. [`launch_reminders.rs`](../../crates/rimz/src/harness/launch_reminders.rs) names dormant and running servers after the subagent paragraph and points to `Skill(rimz-lsp)`, including for children. It explains first-query startup and the possible wait. Without a broker there is no paragraph.

**The skill.** `rimz-lsp` lives in the user's skill library beside `rimz-subagents`, not in this repository. It teaches the query verbs, the exit codes, and the grep fallback. Under host isolation the skill stays visible even when no server runs, because profile skill lists apply only under sandbox isolation ([sandbox.md § Profile skill views](./sandbox.md#profile-skill-views)); the CLI's no-server exit covers that case.

**The native tool is switched off for Claude.** Any effective server configuration applies `LaunchCapability::disable_native_lsp_args`, even after optional refusal. Claude merges `LSP` into one `--disallowedTools` flag before subagent lockdown adds `Agent`. OpenCode and Grok have no verified native-server shutdown switch here; the gap is prose in their adapter pages, not a new coverage concern. `agents validate` consults machine config only and warns about declared `LSP` tools; `LoadedDefinitions.tools` preserves that metadata separately from rendered argv.

## Query surface

Ambiguous workspace symbols are resolved through `textDocument/definition` for each candidate and grouped by definition URI and start position. Exactly one returned location becomes the candidate's identity; empty or multiple answers keep its own location. One group resolves uniquely, while distinct groups list definition positions without guessing. Request errors propagate.

Callers and callees text output defaults to items inside the checkout. The renderer deduplicates before counting hidden outside items and appends the hidden count with a `--external` hint. That flag includes outside items; JSON remains the raw, unfiltered LSP answer.

`rimz lsp` resolves the checkout from cwd (or `--root`) and registry entries. Position-taking verbs accept `path:line:col` or exact symbol names resolved through `workspace/symbol`; `symbols` takes a file and `find` a search string. Output is compact text; `--json` returns structured results. The rust-analyzer smoke confirmed workspace symbol locations point at names, so their start positions feed navigation directly.

| Verb | LSP request |
| --- | --- |
| `def` | `textDocument/definition` |
| `refs` | `textDocument/references` |
| `hover` | `textDocument/hover` |
| `impl` | `textDocument/implementation` |
| `symbols` | `textDocument/documentSymbol` |
| `find` | `workspace/symbol` |
| `callers`, `callees` | call hierarchy incoming and outgoing |

An ambiguous symbol name lists its candidates with positions instead of guessing. The exit codes are a contract with the `rimz-lsp` skill, which teaches agents to branch on them:

| Exit | Meaning |
| --- | --- |
| 0 | Answered; `no results` is a real answer. |
| 3 | No server for this checkout, memory admission refused, or terminal shutdown; the one line names the reason. |
| 4 | Still indexing after the readiness bound; the line gives the elapsed time. |

`rimz lsp list` shows every machine key with state, tree RSS, the maximum of recorded and live tree peak for running servers, requests, last request, restarts, and leases; dormant and stopped entries show no live RSS or PEAK. A first lazy start is not a restart. `rimz lsp stop` makes a server dormant until the next query. The [reference](../reference/cli/lsp.md) owns flags.

The external skill's model-invocation switches are ready for a separate release action. This implementation does not change them.

## Visibility

Automation here is an internal repair, not a user assist, so it keeps diagnostic records rather than assist records: every refused admission, queue timeout, kill, and eviction appends one with the numbers behind it. `evicted` carries the watchdog's kill details plus `for: {root, server}` identifying the requester. Idle stops and restarts need no diagnostic; history carries them. `rimz lsp list` and `rimz doctor` surface running and dormant servers and the last refusal; doctor treats dormant as neutral. Query memory-short errors come from the broker's refusal, never an inference from diagnostics.

## Open questions

- **A hard cap.** On Linux with systemd, a per-server cgroup with `MemoryMax` near 1.3 × the learned peak would give exact accounting and a kill that can only land on the server. The watchdog and `oom_score_adj` are the first version; the cgroup is a later refinement.

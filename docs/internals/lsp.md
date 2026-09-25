# Shared language servers

The [CLI reference](../reference/cli/lsp.md) owns commands, flags, exit codes, and configuration. This page owns the process model implemented in [`lsp/`](../../crates/rimz/src/lsp/mod.rs).

A room can run one language server per (checkout, configured server name) and share it read-only among every agent working in that checkout, instead of each agent's harness starting a private one. The feature is off until a room configures a server, and it is enrichment: an agent without a server works exactly as agents work today, navigating with grep.

## Why RimZ owns this

A language server is expensive to hold. Measured on this repository, one `rust-analyzer` reaches 4.4 GB after its first index, 6.3 GB once `workspaceSymbol` and `findReferences` have run, and its startup `cargo check` briefly takes the process tree to 8.1 GB. A harness-native LSP tool starts one server per agent session and keeps it for the session's life, so a three-seat team plus its subagents holds three or more copies of the same index over the same checkout, idle or not.

RimZ resolves every launch's checkout before any pane opens ([fleet.md § One launch, end to end](./harness/fleet.md#one-launch-end-to-end)). Wrapper process leases supply liveness without coupling the machine-level broker to a room's store or multiplexer. A sandboxed launcher can leave the broker in its inherited mount view; its paths must remain reachable there.

## The rules that shape it

**Enrichment, never a precondition by default.** Memory refusal of an optional server, or a server that dies later, leaves agents on grep. The fail-fast invariant still governs configuration: a configured server whose binary is missing or whose command is untrusted refuses at the launch entry point with the fix. Memory refusal is visible to the human ([Visibility](#visibility)).

**Decided before the agent exists.** A server starts only at a fleet launch entry point, before panes open. RimZ never starts one mid-session. So every agent learns at launch whether a server serves its checkout, through its launch reminder, and nothing about its skills or tools changes afterward.

**One server per key, shared read-only.** The key is (canonical checkout root, server name). Agents only query; none sends document edits. The disk is the one truth, and the broker keeps the server's view of it current ([Freshness](#freshness)).

**Agents own the lifetime.** Leases keep a server alive without an idle timeout. Memory pressure, a hand stop, checkout removal, or a crash can end it early; queries do not restart it.

**Memory is the only admission budget.** No concurrency cap. A server starts only when its estimate fits under free memory with the reserve intact.

## Lifecycle

### Which launches start a server

Every fleet launch entry point except a subagent launch runs admission for its checkout before opening panes: `rimz agents`, `rimz teams`, cohort and lane resume, rebirth recovery, and supervised `-p` runs. It runs after the worktree exists (step 5 of the launch) and before the exec wrappers compile (step 7), once per launch rather than once per cell. A subagent works in its parent's checkout ([subagents.md § The child works where its parent works](./harness/subagents.md#the-child-works-where-its-parent-works)), so it joins its parent's server when one runs and never starts one.

Admission for a live key joins it without starting another server. The wrapper registers its lease separately after launch-plan application. `cli/lsp_admission.rs::admit` is the one helper at the five admission sites; rebirth obtains distinct roots through `RebirthPlan::checkout_roots`.

### Admission

Admission reads free memory and the machine-wide registry under one lock (`disk::lock`, at a machine-level path under `disk::paths::runtime_rimz_root`), so two teams launching at once cannot both pass on the same free gigabytes.

```text
available  = MemAvailable, capped by the cgroup's memory.max headroom when RimZ runs under one
committed  = Σ over running servers of max(0, entry.estimate_bytes − current_tree_rss)
reserve    = max(reserve-percent × MemTotal / 100, reserve-min)
admit when available − committed − estimate ≥ reserve
```

`committed` is what makes the check honest: a server at 4.4 GB that has reached 6.3 GB before will grow again, and that growth is already spoken for. `estimate` is the learned peak for this (project, server), or the configured `memory-estimate` before any history exists ([Learned cost](#learned-cost)).

What happens on a refusal depends on the server's `policy`:

| Policy | Refused admission |
| --- | --- |
| `optional` (default) | The launch proceeds without that server. The launch reminder omits its name, and a diagnostic record names the shortfall. |
| `required` | The launch waits in first-come order among required waiters before panes open. The launcher's terminal prints the wait and queue position every five seconds; there is no pre-launch sidebar row. At `wait-timeout` the launch fails with the fix, naming memory needed, memory free, and the holders. Optional launches do not queue behind required ones. |

`required` gates admission only. A required server killed later for memory degrades like an optional one; RimZ never stops agents to honour it.

### Leases and shutdown

`cli/agents_cmd/exec.rs::run_exec` registers leases after `launch_plan::apply`; `settle_after_exit` explicitly releases them on resident-wrapper paths. Each lease carries launch id, wrapper pid, and process start token. The broker reaps dead owners every five seconds, rejecting pid reuse; direct `exec` preserves the wrapper pid. It does not read store or pane state. Worktree removal calls `registry::stop_checkout` best-effort, and the broker checks root existence every five seconds as a backstop.

The last release arms a 60-second grace, canceled by a new lease, to cover restart's gap. A fork takes its own lease. A broker never leased exits after five minutes. There is no idle timeout. Queue tickets likewise carry an owner pid/start token; abandoned tickets are removed, and dropping the launcher's `WaitQueue` removes its ticket.

### Readiness

Admission waits up to five seconds under the admission lock for the broker's entry, not its index. After `initialize`/`initialized`, open progress tokens mean `indexing`. The server becomes `ready` when all tokens close and at least one has ended, or after ten seconds without progress. Later progress can return it to `indexing`. A query waits at most 30 seconds for readiness, then reports elapsed indexing time rather than an empty result for an unfinished index.

## Memory pressure

### The watchdog

Every live broker samples free memory on its five-second housekeeping cadence. Below `kill-floor-percent` of total memory, the machine-lock winner selects victims and re-samples between stops. It first sends a socket stop with reason `memory pressure`; without acknowledgment within two seconds, it uses token-guarded SIGKILL on the server pid and publishes the tombstone itself. Servers receive `oom_score_adj = 800` so the kernel favors them over agents if it acts first.

### Who is killed first

The order preserves the servers agents actually use:

1. Servers with zero requests, longest-running first. A team that has not queried its server in a long time probably has a task that does not need one, and the newest zero-request server is the one most likely still to be used.
2. Servers with requests, longest since the last request first, breaking ties by start time.

A killed server is not restarted. Its registry entry becomes a tombstone carrying the reason, and it stays until the key's last lease is released.

### The agent's view of a kill

Agents are not messaged. The next `rimz lsp` query against a tombstoned key exits with a distinct exit code and one line: the server stopped, why, and to use grep. An agent that never queried again never pays for the notice; one that had been using the server adapts on its next call.

## Learned cost

`lsp/history.rs` appends checkout, server, peak tree RSS, time to first ready, settings hash, and stop reason to rotating `lsp-history.jsonl` under the RimZ home. Admission takes the maximum of the last five records for the same checkout/server/settings triple, falling back to `memory-estimate` without history. The settings hash covers command argv and canonical initialization options. This is an admission input, not a diagnostic log.

## Freshness

`lsp/broker/watch.rs` batches `notify` events into `workspace/didChangeWatchedFiles`. Only configured extensions pass; `.git`, `target`, and `node_modules` components are excluded. Create, change, delete, and rename events map to LSP event types. Freshness depends on the server handling these notifications, not on unsaved editor buffers. Watch traffic never increments request counters.

## The broker

A language server speaks to exactly one client over stdio. Each server therefore gets one broker: a hidden `rimz lsp serve` process that owns the server's stdio, listens on a per-key Unix socket, serializes nothing it does not have to, and speaks the `rimz lsp` query protocol to callers.

It is spawned through `child_process::spawn_detached_rimz` and calls `setsid`. The broker remains after server shutdown to answer from a tombstone while leases live. It is a per-server process bounded by leases, not a room-wide service ([fleet.md](./harness/fleet.md#the-rules-that-shape-it)). Standard threads own socket handling and Content-Length transport; request IDs match concurrent replies. The reader answers configuration, workspace-folder, progress-creation, and capability-registration requests.

The 0600 newline-JSON socket accepts `hello`, `lease`, `release`, `query`, `stop`, and `status`; responses carry the nonce. Queries carry `method`, `params`, and `wait_ms`, returning raw LSP `result`, elapsed `indexing`, or `error`. Only queries increment counters. The CLI owns verb semantics, not the broker.

The broker answers `hello` before publishing its first entry, without taking `admission.lock`: admission holds that lock while waiting for publication. Server pid/token remain nullable until spawn. Per-key publication locking prevents a racing refresh from overwriting a stop.

Two traps follow from spawning:

- **A sandboxed launcher.** An agent may run `rimz teams` from inside its bubblewrap view. A broker spawned from there inherits that mount namespace, where `/tmp` is room tmp. Because the sandbox rebinds the runtime home, the RimZ home, and every checkout at their host paths ([sandbox.md § Reachable host paths](./sandbox.md#reachable-host-paths)), the broker must name only such paths: its socket and registry live under `disk::paths::runtime_rimz_root`, never under `/tmp`.
- **The server's own build.** `rust-analyzer` runs `cargo check` on startup and save by default. The configuration template turns it off (`checkOnSave = false`): the read-only query surface exposes no diagnostics, the check cost 57 CPU seconds and a 3.7 GB spike on this repository, and it would contend for the checkout's `target/` lock with the agents' own builds. The broker does not inject this option for custom definitions.

## State

| Record | Where | Lifetime | Truth |
| --- | --- | --- | --- |
| Registry entry per key: root/server, broker and server pids/tokens, nonce, state (`starting`, `indexing`, `ready`, or a `stopped` tombstone), start/ready times, estimate/settings hash, request count, last request time, peak RSS, leases | machine-level, under `disk::paths::runtime_rimz_root`, atomic writes | through lease lifetime and shutdown grace; runtime files do not survive reboot | a live socket answering with the entry's nonce; a pid alone proves nothing |
| Cost history | machine-level under the RimZ home, rotating JSONL | durable | append-only |
| Kills, refusals, and queue timeouts | `diag/` diagnostic records | durable | append-only |

The registry is machine-level rather than per-room because admission budgets one machine's memory across every room on it.

`disk::paths::lsp_runtime_dir()` holds `admission.lock`, owner-tagged `queue/` tickets, and one directory per key: the first 16 hex characters of the canonical checkout SHA-256 plus server name. Each directory has `entry.json`, `sock`, and a publication lock. There is no shared registry JSON file. `registry::sweep_locked`, used by admission and `list`, removes dead entries using process tokens and nonce-checked hello; live tombstones remain. Socket paths are validated against the Unix path budget.

## Configuration

A room with no `[lsp.servers.*]` table has the feature off. Server definitions may live in machine or project config, with project entries overlaid on machine ones as profiles are; the memory policy is machine-only, because a repository cannot set how much of this machine it may take.

```toml
[lsp]                                  # machine config only
reserve-percent = 10
reserve-min = "8G"
kill-floor-percent = 5

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

**The launch reminder.** `launch_plan::compile` reads `registry::live_server_names` without writing. [`launch_reminders.rs`](../../crates/rimz/src/harness/launch_reminders.rs) names the servers after the subagent paragraph and points to `Skill(rimz-lsp)`, including for children. Without a server there is no paragraph.

**The skill.** `rimz-lsp` lives in the user's skill library beside `rimz-subagents`, not in this repository. It teaches the query verbs, the exit codes, and the grep fallback. Under host isolation the skill stays visible even when no server runs, because profile skill lists apply only under sandbox isolation ([sandbox.md § Profile skill views](./sandbox.md#profile-skill-views)); the CLI's no-server exit covers that case.

**The native tool is switched off for Claude.** Any effective server configuration applies `LaunchCapability::disable_native_lsp_args`, even after optional refusal. Claude merges `LSP` into one `--disallowedTools` flag before subagent lockdown adds `Agent`. OpenCode and Grok have no verified native-server shutdown switch here; the gap is prose in their adapter pages, not a new coverage concern. `agents validate` consults machine config only and warns about declared `LSP` tools; `LoadedDefinitions.tools` preserves that metadata separately from rendered argv.

## Query surface

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
| 3 | No server for this checkout, or a tombstone; the one line names the reason. |
| 4 | Still indexing after the readiness bound; the line gives the elapsed time. |

`rimz lsp list` shows every machine key with state, tree RSS, observed peak, requests, last request, and leases; `rimz lsp stop` stops one by hand. The [reference](../reference/cli/lsp.md) owns flags.

The external skill's model-invocation switches are ready for a separate release action. This implementation does not change them.

## Visibility

Automation here is an internal repair, not a user assist, so it keeps diagnostic records rather than assist records: every refused admission, queue timeout, and kill appends one with the numbers behind it. `rimz lsp list` and `rimz doctor` surface current servers, tombstones, and the last refusal, so a human can see why an agent was on grep.

## Open questions

- **Admission eviction.** Admission never kills today. A `required` launch queued behind zero-request servers that have run for a long time may deserve the right to evict them.
- **A hard cap.** On Linux with systemd, a per-server cgroup with `MemoryMax` near 1.3 × the learned peak would give exact accounting and a kill that can only land on the server. The watchdog and `oom_score_adj` are the first version; the cgroup is a later refinement.

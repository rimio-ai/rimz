# Shared language servers

> **Status: planned.** Nothing on this page ships yet. It records the agreed design so the implementation lands against a fixed target. Paths and symbols named as existing are anchors into today's code; everything under `lsp/` and `rimz lsp` is new.

A room can run one language server per (checkout, language) and share it read-only among every agent working in that checkout, instead of each agent's harness starting a private one. The feature is off until a room configures a server, and it is enrichment: an agent without a server works exactly as agents work today, navigating with grep.

## Why RimZ owns this

A language server is expensive to hold. Measured on this repository, one `rust-analyzer` reaches 4.4 GB after its first index, 6.3 GB once `workspaceSymbol` and `findReferences` have run, and its startup `cargo check` briefly takes the process tree to 8.1 GB. A harness-native LSP tool starts one server per agent session and keeps it for the session's life, so a three-seat team plus its subagents holds three or more copies of the same index over the same checkout, idle or not.

Sharing needs the one party that knows which agents work where and when they finish. RimZ already knows both: it resolves every launch's checkout before any pane opens ([fleet.md § One launch, end to end](./harness/fleet.md#one-launch-end-to-end)), and the store records when each agent ends. It also launches from the host side of the sandbox, so a server it starts is not trapped inside one agent's mount view.

## The rules that shape it

**Enrichment, never a precondition by default.** A server that cannot start, or dies, leaves agents on grep. The fail-fast invariant still governs configuration: a configured server whose binary is missing or whose command is untrusted refuses at the launch entry point with the fix. Only memory shortage degrades silently to the agent, and it is never silent to the human ([Visibility](#visibility)).

**Decided before the agent exists.** A server starts only at a fleet launch entry point, before panes open. RimZ never starts one mid-session. So every agent learns at launch whether a server serves its checkout, through its launch reminder, and nothing about its skills or tools changes afterward.

**One server per key, shared read-only.** The key is (canonical checkout root, server name). Agents only query; none sends document edits. The disk is the one truth, and the broker keeps the server's view of it current ([Freshness](#freshness)).

**Agents own the lifetime.** A server lives while any agent holding a lease on its key lives. There is no idle timeout: a server never restarts mid-session, so stopping it for idleness would remove it for the rest of the team's run. Only memory pressure ends a server early.

**Memory is the only admission budget.** No concurrency cap. A server starts only when its learned peak fits under free memory with the reserve intact.

## Lifecycle

### Which launches start a server

Every fleet launch entry point except a subagent launch runs admission for its checkout before opening panes: `rimz agents`, `rimz teams`, cohort and lane resume, rebirth recovery, and supervised `-p` runs. It runs after the worktree exists (step 5 of the launch) and before the exec wrappers compile (step 7), once per launch rather than once per cell. A subagent works in its parent's checkout ([subagents.md § The child works where its parent works](./harness/subagents.md#the-child-works-where-its-parent-works)), so it joins its parent's server when one runs and never starts one.

Admission for a key that already has a live server is a lease, not a start.

### Admission

Admission reads free memory and the machine-wide registry under one lock (`disk::lock`, at a machine-level path under `disk::paths::runtime_rimz_root`), so two teams launching at once cannot both pass on the same free gigabytes.

```text
available  = MemAvailable, capped by the cgroup's memory.max headroom when RimZ runs under one
committed  = Σ over running servers of max(0, learned_peak − current_tree_rss)
reserve    = max(reserve-percent × MemTotal, reserve-min)
admit when available − committed − estimate ≥ reserve
```

`committed` is what makes the check honest: a server at 4.4 GB that has reached 6.3 GB before will grow again, and that growth is already spoken for. `estimate` is the learned peak for this (project, server), or the configured `memory-estimate` before any history exists ([Learned cost](#learned-cost)).

What happens on a refusal depends on the server's `policy`:

| Policy | Refused admission |
| --- | --- |
| `optional` (default) | The launch proceeds without a server. The launch reminder omits the LSP paragraph, and a diagnostic record names the shortfall. |
| `required` | The launch waits in a first-come queue before any pane opens, so a team is never half-started. `rimz teams` and the sidebar show the wait and the queue position. At `wait-timeout` the launch fails with the fix, naming the memory needed, the memory free, and the servers holding it. |

`required` gates admission only. A required server killed later for memory degrades like an optional one; RimZ never stops agents to honour it.

### Leases and shutdown

Each exec wrapper whose checkout has a live server registers a lease keyed by its launch id. The broker reconciles leases against the store on a short cadence: a lease whose agent has ended, or whose pane is gone, is released. When the last lease is released the broker shuts the server down. A checkout that no longer exists (worktree removal) shuts it down at once, and the worktree removal path ([worktrees.md § Removing a worktree](./harness/worktrees.md#removing-a-worktree)) signals the broker directly rather than waiting for the cadence.

A restart, fork, or resume of an agent keeps its launch identity, so its lease survives the bounce and the server does not churn.

### Readiness

Admission spawns the server and returns without waiting for its index; the first index of this repository takes about 20 seconds of wall time, which a team spends reading its prompt. A query that arrives while the server is still indexing blocks up to a bound and then answers, or reports `indexing` with the elapsed time. It never returns an empty result for an unfinished index, because an agent reads "no references" as a fact.

## Memory pressure

### The watchdog

Every live broker samples free memory. When it falls below `kill-floor`, the broker that wins the machine-level lock picks one victim, kills it, re-samples, and repeats until free memory is back above the floor. Each server also runs with a raised `oom_score_adj`, so if the kernel acts first it takes a server rather than an agent mid-turn.

### Who is killed first

The order preserves the servers agents actually use:

1. Servers with zero requests, longest-running first. A team that has not queried its server in a long time probably has a task that does not need one, and the newest zero-request server is the one most likely still to be used.
2. Servers with requests, longest since the last request first.

A killed server is not restarted. Its registry entry becomes a tombstone carrying the reason, and it stays until the key's last lease is released.

### The agent's view of a kill

Agents are not messaged. The next `rimz lsp` query against a tombstoned key exits with a distinct exit code and one line: the server stopped, why, and to use grep. An agent that never queried again never pays for the notice; one that had been using the server adapts on its next call.

## Learned cost

After each server stops, its broker appends one record to a machine-level history (rotating JSONL through `disk::rotating`): the project, the server name, the peak process-tree RSS, the time to first ready, and the effective settings that change the peak (for `rust-analyzer`, whether `checkOnSave` was on). Admission's `estimate` is the maximum over the last N records for the same (project, server, settings), so a server that sometimes serves `workspaceSymbol` and sometimes does not is budgeted at its worst. A settings change starts a fresh history rather than trusting a peak measured under different settings.

## Freshness

Agents edit files while the server runs, and no client sends `didChange`. The broker therefore watches the checkout itself (the `notify` crate, already a dependency), ignoring `.git/` and build output such as `target/`, and forwards changes to the server as `workspace/didChangeWatchedFiles`. This works for every server; relying on a server's own watcher (`rust-analyzer`'s `files.watcher = "server"`) would not. File-watch activity never counts as a request, so it cannot hold a server off the kill order.

## The broker

A language server speaks to exactly one client over stdio. Each server therefore gets one broker: a hidden `rimz lsp serve` process that owns the server's stdio, listens on a per-key Unix socket, serializes nothing it does not have to, and speaks the `rimz lsp` query protocol to callers.

It is spawned detached through `child_process::spawn_detached_rimz`, the same way RimZ starts its other bounded background processes, and it exits with its server. That keeps the harness rule in [fleet.md § The rules that shape it](./harness/fleet.md#the-rules-that-shape-it): there is no room-wide resident service, only a per-server process whose lifetime is its leases.

Two traps follow from spawning:

- **A sandboxed launcher.** An agent may run `rimz teams` from inside its bubblewrap view. A broker spawned from there inherits that mount namespace, where `/tmp` is room tmp. Because the sandbox rebinds the runtime home, the RimZ home, and every checkout at their host paths ([sandbox.md § Reachable host paths](./sandbox.md#reachable-host-paths)), the broker must name only such paths: its socket and registry live under `disk::paths::runtime_rimz_root`, never under `/tmp`.
- **The server's own build.** `rust-analyzer` runs `cargo check` on startup and save by default. The shared default turns it off (`checkOnSave = false`): the read-only query surface exposes no diagnostics, the check cost 57 CPU seconds and a 3.7 GB spike on this repository, and it would contend for the checkout's `target/` lock with the agents' own builds.

## State

| Record | Where | Lifetime | Truth |
| --- | --- | --- | --- |
| Registry entry per key: key, broker and server pids, nonce, socket, state (`starting`, `indexing`, `ready`, or a `stopped` tombstone with its reason), start time, request count, last request time, leases | machine-level, under `disk::paths::runtime_rimz_root`, atomic writes | until the last lease is released; runtime files do not survive reboot, and neither do servers | a live socket answering with the entry's nonce; a pid alone proves nothing |
| Cost history | machine-level under the RimZ home, rotating JSONL | durable | append-only |
| Kills, refusals, and queue timeouts | `diag/` diagnostic records | durable | append-only |

The registry is machine-level rather than per-room because admission budgets one machine's memory across every room on it.

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
init-options = { checkOnSave = false }
policy = "optional"                    # or "required"
wait-timeout = "10m"                   # required only
memory-estimate = "8G"                 # until learned
```

`command` and `init-options` run or configure a process, so both join the executable surface ([trust.md § Adding a command-running field](./harness/trust.md#adding-a-command-running-field)), with a hash-coverage case each.

## What agents see

**The launch reminder.** When a server holds a lease for the launch's checkout, [`launch_reminders.rs`](../../crates/rimz/src/harness/launch_reminders.rs) renders one more paragraph, after the subagent paragraph: the servers that serve this checkout and that navigation and exploration go through `Skill(rimz-lsp)`. Without a server there is no paragraph, and the agent behaves as today.

**The skill.** `rimz-lsp` lives in the user's skill library beside `rimz-subagents`, not in this repository. It teaches the query verbs, the exit codes, and the grep fallback. Under host isolation the skill stays visible even when no server runs, because profile skill lists apply only under sandbox isolation ([sandbox.md § Profile skill views](./sandbox.md#profile-skill-views)); the CLI's no-server exit covers that case.

**The native tool is switched off.** In a room with any server configured, a Claude launch gets `--disallowedTools LSP`, through a new adapter capability shaped like `lockdown_subagent_args` in [`agents/capabilities.rs`](../../crates/rimz/src/agents/capabilities.rs), whose default leaves argv unchanged. Otherwise a profile's `tools` list naming `LSP` starts a private server per session and the saving never happens. Other adapters with a native language-server integration get the same switch where a verified one exists; where none exists, the gap is declared in the adapter's coverage. `rimz agents validate` warns about a `tools` entry the room will strip.

## Query surface

`rimz lsp` resolves the caller's checkout from its working directory and the key's registry entry, then asks that key's broker. Positions are `path:line:col`, and every verb also accepts a symbol name, resolved through `workspaceSymbol`, so an agent need not compute columns. Output is compact text; `--json` is the structured form.

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

`rimz lsp list` shows every key on the machine with its state, tree RSS, learned peak, requests, and leases; `rimz lsp stop` stops one by hand. The reference page owns flags once they ship.

The skill ships user-invoked only (`disable-model-invocation: true`, and `allow_implicit_invocation: false` for Codex) until `rimz lsp` exists; the change that ships the command flips both.

## Visibility

Automation here is an internal repair, not a user assist, so it keeps diagnostic records rather than assist records: every refused admission, queue timeout, and kill appends one with the numbers behind it. `rimz lsp list` and `rimz doctor` surface current servers, tombstones, and the last refusal, so a human can see why an agent was on grep.

## Design changes this requires

These land with the implementation, not with this page:

- [DESIGN.md](../../DESIGN.md) gains the language-server line: shared servers are enrichment, off by default, started only at launch, and ended by their agents or by memory pressure.
- [fleet.md § The rules that shape it](./harness/fleet.md#the-rules-that-shape-it) names the broker beside the "No daemon" rule as a per-server process bounded by its leases.
- [ARCHITECTURE.md](../../ARCHITECTURE.md) adds the broker process and the machine-level registry to the runtime shape and on-disk state; the root code map adds `lsp/`.

## Open questions

- **Admission eviction.** Admission never kills today. A `required` launch queued behind zero-request servers that have run for a long time may deserve the right to evict them.
- **A hard cap.** On Linux with systemd, a per-server cgroup with `MemoryMax` near 1.3 × the learned peak would give exact accounting and a kill that can only land on the server. The watchdog and `oom_score_adj` are the first version; the cgroup is a later refinement.
- **The readiness bound.** How long a query blocks on an indexing server before answering `indexing`.

# Profiling a live fleet

> The field guide for measuring a running RimZ room: read what it already published, find the process that matters, then attach a profiler only when the cheap answers run out. The model this measures against, including the cost map and the budgets, lives in [performance.md](./performance.md). The evidence RimZ records about its own faults lives in [diagnostics.md](./diagnostics.md).

Work through the sections in order. Each step costs less than the one after it, and most questions are answered before you attach anything.

## Start with what RimZ already published

The host writes its caches with freshness stamps in them, so a stale or looping lane is visible without a profiler. Read these first. The cadence each stamp should follow is in [state.md → Cadences](./sidebar/state.md#cadences).

| File | Question it answers |
| --- | --- |
| `$XDG_RUNTIME_DIR/rimz/ws/<name>/lanes/pr-state.json` | Is the forge probe looping? Each origin repository has a `repos` entry. One with `"ok": false`, a rising `consecutive_failures`, and an advancing `refreshed_at_ms` is climbing its failure backoff. Which links are pinned instead of re-probed is [state.md → PR state](./sidebar/state.md#pr-state). |
| `$XDG_RUNTIME_DIR/rimz/ws/<name>/lanes/diff-stats.json` | How busy is the git sweep? Each worktree root carries `refreshed_at_ms` for its local facts and `commit_refreshed_at_ms` for its commit facts. Compare their age against the root's tier. The file keeps entries for roots the sweep no longer reads, so its root count is an upper bound on the sweep's input. |
| `~/.rimz/cache/providers/credits.json` | Is an account usage probe failing? Each (login, scope) entry's `oauth_read_at_ms` is the last direct usage read, and `oauth_read_hint` is the published account hint it was claimed under. Kind-wide entries live in `logins`; sub-provider entries prefer `scoped`, with a matching-scope legacy `logins` fallback. First compare that hint with the credential in `accounts.json`: a difference makes the next pass read, while an unchanged hint waits the cadence even if the reading's own identity differs. If `oauth_read_hint` is absent, compare the entry's own `scope` and `credentials_stamp` with that credential instead. If hints or `scoped` disappear after a pass, look for an older build rewriting the file. `refreshed_at_ms` and `observed_at_ms` also move on realtime publication, so they do not show the probe cadence. With an unchanged hint, an entry at `"ok": false` with `auth_settled: false` retries on `OAUTH_USAGE_TTL` (5 minutes). With `auth_settled: true` it waits `OAUTH_USAGE_SETTLED_TTL` (1 hour) ([providers.md → Refresh cadences](./agents/providers.md#refresh-cadences)). |
| `~/.rimz/cache/providers/pricing-cache.json` | Is the pricing fetch or the unpriced-model chase busy? `fetched_at_secs` shows the weekly baseline. `unknown_backoff_secs` and `unknown_seen` show the chase. |
| `~/.rimz/ws/<name>/audit/diag.log.jsonl` | Did RimZ already notice? A `tick_budget_breach` record separates `mux_wait_ms` from in-process time. On a cache-refresh breach, its `spawns_by_step` names the refresh step that forked ([diagnostics.md](./diagnostics.md)). |

`rimz sidebar snapshot --json --no-produce` prints the live folded view and the room's shape. With `--no-produce` it reads the host's publications through `PublishedSnapshotReader` and forks neither the multiplexer nor git, so the inspection does not perturb the room. This holds only when the command resolves the room's session: run it from inside the room, or pass `--session-name`. Without a session, `crates/rimz/src/cli/sidebar.rs::try_emit_consumer_snapshot` returns false and the command produces a fresh snapshot, forks included.

```console
$ rimz sidebar snapshot --json --no-produce | jq '{agents:(.agents|length), panes:(.agent_panes|length), roots:(.worktree_roots|length), groups:(.worktree_groups|length)}'
{
  "agents": 185,
  "panes": 157,
  "roots": 160,
  "groups": 125
}
```

## Find the host

One room host per mux session owns the shared reads and paints every sidebar pane ([performance.md → One host, many panes](./performance.md#one-host-many-panes)). Profile that process for rendering and data-plane work.

Find it by its subcommand. Each sidebar pane runs a `rimz sidebar serve` supervisor, which holds the pane's tty and paints nothing. The `rimz sidebar host` with the same `--workspace-id` and `--session-name` folds, paints, and writes every attachment's heartbeat:

```sh
ps -C rimz -o pid,ppid,rss,args
```

Match the subcommand in `args`, never a substring that could come from an agent's prompt. Room processes run from the staged build under `~/.rimz/builds/<build>/rimz`, so the path in `args` is not `~/.cargo/bin/rimz`.

A large heartbeat count in a many-tab room is expected. Each heartbeat identifies one attachment, and the host has no pane or sidebar-instance environment pin of its own, so a hundred heartbeats still mean one painter. Every attachment shares that host's external-read cost.

## Measure the process

Take CPU, memory, and IO from `/proc` first. These readings need no tool beyond the shell and are the numbers the rest of the page compares against.

From an agent pane in scoped isolation, `/proc/<pid>/stat` of the room host reads normally, but `/proc/<pid>/smaps_rollup` and `/proc/<pid>/io` fail with `Permission denied`: the host runs outside the sandbox's user namespace. Take memory and IO from a user shell.

### CPU

Measure CPU from the kernel's own accounting: take `utime + stime` (fields 14 and 15 of `/proc/<pid>/stat`) at both ends of a fixed window, and divide the delta by `getconf CLK_TCK` and the window length. The figure needs no profiler attached. This capture took the host and one of its supervisors:

```console
$ cat cpu.sh
pids="1815887 2081995"; secs=12; hz=$(getconf CLK_TCK)
declare -A before
for pid in $pids; do before[$pid]=$(awk '{print $14 + $15}' /proc/$pid/stat); done
sleep $secs
for pid in $pids; do
  after=$(awk '{print $14 + $15}' /proc/$pid/stat)
  awk -v d=$(( after - before[$pid] )) -v hz=$hz -v s=$secs -v p=$pid \
    'BEGIN { printf "pid %s  %.3f core\n", p, d / hz / s }'
done
$ bash cpu.sh
pid 1815887  0.386 core
pid 2081995  0.000 core
```

The host carries the room's whole cost, and a supervisor reads near zero. Compare one host against another host, never against a supervisor.

`pidstat -t` splits the host's CPU by thread, and one of its rows misleads. The row with TID `-` is the process total. The thread row whose TID equals the PID is the main thread alone: one host capture read 71.71% on the process row and 0.00% on that thread row. Run it as `LC_ALL=C pidstat -u -t -p <pid> 5 1`, or parse its columns by header name, because a 12-hour locale inserts an AM/PM column and shifts the rest.

### Memory

Read memory from `/proc/<pid>/smaps_rollup`: report `Pss`, and compute USS as `Private_Clean + Private_Dirty + Private_Hugetlb`. Summing RSS across the host and its supervisors double-counts shared mappings. Compare only newly started hosts with the same attachment count, and only after at least two host refresh cycles, because allocator fragmentation and retained state can preserve earlier memory peaks. A rising resident set has its own procedure under [memory growth against allocator high-water](#memory-growth-against-allocator-high-water).

### IO

Take the delta of `read_bytes` and `write_bytes` in `/proc/<pid>/io` over a fixed window. The hot runtime caches live under `$XDG_RUNTIME_DIR`, normally a tmpfs, so their churn is memory traffic. Sustained disk bytes point at durable state or transcripts.

### Comparing hosts across a setting

To compare an allocator or other startup setting, give each configuration at least six fresh hosts, interleave the configurations so machine load drifts across all of them, and compare per-run mean CPU against the [many-pane host bar](./performance.md#anti-patterns). On Linux x86-64 and aarch64 (glibc or musl), `MALLOC_CONF` in the relaunched process's environment overrides the built-in jemalloc configuration at startup. A different allocator takes a build: `LD_PRELOAD` cannot displace the executable's exported `malloc`. Give each build its own fresh rooms, started by that build: a room records the build that started it, and `crates/rimz/src/sidebar_pane/host.rs::Host::serve` drains a host from any other build at its first poll. A hand-relaunch swaps only the startup environment, never the binary. A running host cannot change its startup configuration.

A hand-relaunched host (supervisors paused with `SIGSTOP`, the host replaced, the supervisors resumed) takes the panes only if it is already listening on the room's `host.<key>.sock` (`crates/rimz/src/disk/paths.rs::RuntimePaths::sidebar_host_socket_path`) when the supervisors resume; `ss -xlp` shows the socket under its PID. A supervisor that finds no listener starts a host of its own, which takes every pane, and a replacement slowed by a profiler loses that race. The relaunched host must stay up at least 60 s after its supervisors attach. A supervisor resets its respawn backoff only after a host outlives `RESPAWN_STABLE_RUN` (60 s) in `crates/rimz/src/sidebar_pane/supervise.rs`, and each shorter run doubles the backoff. Once the backoff exceeds the host's `UNATTACHED_GRACE` (10 s) in `crates/rimz/src/sidebar_pane/host.rs`, the next host exits cleanly before any pane attaches. That looks like a crash of the configuration under test.

### Peak RSS of a CLI read

A one-shot CLI fold over a large carryover is measured on the carryover bench fixtures ([performance.md → Fixtures](./performance.md#fixtures)), in a scratch home that holds nothing else.

1. Export the fixtures. Set `RIMZ_CARRYOVER_FIXTURE_DIR` to a scratch directory and run `cargo xtask perf --bench hotpath -- carryover_fold`. Outside timing, the benches write `carryover-3300.json`, `carryover-13200.json`, and the matching `rebirth-<rows>.log.jsonl` files there.
2. Build with `cargo build --release -p rimz --bin rimz`, and set `carryover_binary="$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)/release/rimz"`.
3. From a shell inside a managed room, keep its `RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`. Set `carryover_rss_home` to a scratch directory, and copy the room's `ws/<directory>/workspace.json` into the same relative path under it. The scratch home holds nothing else.
4. For each size, put the exported carryover at that workspace's `records/agents-carryover.json` and its rebirth log at `log/events.log.jsonl`, with no snapshot checkpoints.
5. Time each invocation with `env RIMZ_HOME="$carryover_rss_home" /usr/bin/time -f '%M %e %U %S' "$carryover_binary" agents list --json > /dev/null`.

To compare two builds, verify both binaries' versions, then alternate three runs per binary and size in one session on the same scratch home and fixtures. Report minimum wall time and maximum peak RSS. If the installed binary is not the intended base, build that revision in a separate scratch worktree and target directory, then remove the worktree. Peak RSS includes transient raw parsing and allocator pages, so it is not a retained-heap measurement, and the CLI still pays the room's live-mux and runtime-cache overhead.

On 2026-10-07 a release build peaked at 82,332 KiB in 0.25 s at 3,300 rows and 302,124 KiB in 0.51 s at 13,200 rows, on the loaded host named in [performance.md → Benchmarks](./performance.md#benchmarks).

## Build a profilable binary

Profile a binary built with the `profiling` Cargo profile. `perf` and `samply` need frame pointers and line tables to unwind and attribute samples, and the release profile strips symbols.

```sh
cargo xtask profile-build
profiling_binary="$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)/profiling/rimz"
"$profiling_binary" --version
```

`profile-build` prints the artifact's path. It sits under the Cargo target directory, which `CARGO_TARGET_DIR` may move out of the checkout, so resolve it through `cargo metadata` rather than `target/`. The profile inherits `release` and adds `debug = "line-tables-only"` and `strip = "none"`. xtask adds frame pointers and v0 symbol mangling through `RUSTFLAGS` (`xtask/src/build.rs::PROFILING_RUSTFLAGS`). `cargo xtask install-dev` installs the same profile, with the `sentry` feature, to `~/.cargo/bin`, so a dogfooding binary is directly profilable ([rust-conventions.md](../contributing/rust-conventions.md)).

### Heap-profiling build

On Linux x86-64 and aarch64 with glibc, the binary exports jemalloc's `malloc` family, which binds ahead of any preloaded library, and Rust frees go through jemalloc's `sdallocx`. A heap profiler that interposes `malloc`, such as `heaptrack` or DHAT, therefore records nothing from a normal build: `heaptrack` hangs after the process exits, its output file empty, and DHAT's run dies with `SIGSEGV` in jemalloc's free path. The `rimz_system_malloc` cfg drops jemalloc and keeps glibc malloc. Build it in its own target directory, since a `RUSTFLAGS` change rebuilds every crate. The `testkit` feature lets `cargo xtask sandbox room` run the binary:

```sh
RUSTFLAGS='--cfg rimz_system_malloc' cargo build --profile profiling -p rimz --bin rimz --features testkit --target-dir "$TMPDIR/heap-target"
heaptrack --output "$TMPDIR/rimz-heap" "$TMPDIR/heap-target/profiling/rimz" agents list --json
```

Allocation sites and outstanding allocations do not depend on the allocator. Resident memory does, so measure it on a normal build.

Check which build is running before trusting absolute numbers. Frames such as `core::ub_checks::*` or `precondition_check` mark a debug build, whose absolute CPU does not compare with a release build. Ratios and rates (forks per second, folds per second, shared data work against per-pane rendering) carry across builds.

## Pick the tool by the question

Start with `strace -f -c -p <pid>` when a process is busy for no clear reason. It shows syscall shape, fork and exec storms, lock contention, and IO without rebuilding or restarting anything. A fork storm, a probe retry loop, and failed execs all show in syscall counts, and a CPU profile confirms the shape once `strace` has found it.

| Question | Tool |
| --- | --- |
| Which binaries fork, how often, and do they succeed? | `strace -f -e trace=execve -p <pid>`. A repeating `ENOENT` on a RimZ binary path is an exec of a binary that no longer exists ([symbols from a replaced binary](#symbols-from-a-replaced-binary)). |
| Where is CPU going by function? | `samply record -p <pid>` for a Firefox Profiler call tree, or `perf record --call-graph fp -p <pid> -- sleep 10` then `perf report --stdio` for flat self-time. |
| Which allocations churn? | `heaptrack` or DHAT on a [heap-profiling build](#heap-profiling-build) for ownership. The share of allocator frames (`malloc`, `sdallocx`, jemalloc's `je_*` slow paths), `memmove`, and `clone` in a `perf` profile is the first-pass signal. |
| What goes out on the network? | `strace -f -e trace=%network -p <pid>` for the host's own calls, `ss -tanp` for established peers, and the published cache stamps for cadence. |
| How much does each pane render, and what does SSH carry? | `rimz pane bandwidth`, run on the host serving the room. Its per-pane rows are each pane's write rate. Its `WIRE(ssh↑)` and `WIRE(ssh↓)` rows are the payload each way on the room's SSH socket, usually far below the per-pane sum, and are absent for a local room ([reference](../reference/cli/pane.md#bandwidth)). |
| What does an account refresh cost? | Set `RIMZ_ACCOUNT_REFRESH_TRACE` to a file path, or to `1`, `true`, or an empty value for `account_refresh_trace.jsonl` in `~/.rimz/cache/providers/`. The trace (`crates/rimz/src/sidebar/refresh/trace.rs`) appends one JSON line per provider probe, probe batch, cache contention, claim, helper spawn, and usage-helper run, with outcomes and durations and no commands, paths, identities, tokens, URLs, or bodies. It rotates at 1 MiB. |

To profile one command instead of a running process, run the profiling binary on the path under test: `samply record "$profiling_binary" sidebar snapshot --json`, with `--no-produce` added when the question is the read-only path. `samply record -- cargo xtask perf` covers the benchmarks.

Check host policy before attaching, because it limits these tools more than RimZ does. Read `kernel.perf_event_paranoid` and `kernel.yama.ptrace_scope`: `perf` needs `sudo` or a lower `perf_event_paranoid` when the value is 3 or higher. Hardware LBR call graphs may be unavailable under virtualization, so use `--call-graph fp`. Keep `strace` windows to a few seconds, because syscall tracing slows the process it observes. A failed attach is a failed measurement: record it instead of silently substituting another tool. Under a `ptrace_scope` of 1, a sandbox room's host can still be traced from its start ([recipe](../contributing/sidebar-live-check.md#room-host)). A live room's host that a user shell started is out of reach from a scoped agent pane. The agent sandbox sets no-new-privileges, so `sudo` fails, and a `ptrace_scope` of 1 refuses `strace -p` and `heaptrack -p` on any process that is not the agent's own descendant. Tracing that host is a step for the user's shell.

Treat captures as sensitive. Traces and argv contain project paths and command text, so set `umask 077`, keep the capture directory outside the repository, copy the measurements that matter into the change report, and delete the directory.

## What to look at, in order

Check these signals in order. The earlier ones find more for less effort.

1. Fork and exec rate, and outcome. At idle the host still runs git on each root's tier ([state.md → Cadences](./sidebar/state.md#cadences)): every 5 minutes for an idle root, clean and landed included, and every 10 minutes for one whose rows are all hibernated or ended agents, at least one hibernated. An off-screen hot root follows agent activity with a 5-second floor and a 60-second quiet fallback; unknown or unhooked agent kinds keep the 5-second timer. An exec rate above those tiers at idle is a retry loop or a cache that never goes fresh, and failed execs matter as much as the rate. `strace -e trace=execve` together with the published caches locates it.
2. Refold rate against event rate. Compare the store's events per second with the host fetch worker's fold rate. Pane requests coalesce through the shared worker, so the fold rate must not grow with tab count.
3. The host's thread split. Sum thread-level CPU: fetch and refresher cost points at shared reads, and attachment-heavy cost points at rendering multiplied by pane count.
4. Allocation share. A large share of allocator (`malloc`, `sdallocx`, `je_*`), `memmove`, and `clone` frames in fold or enrich paths means deep clones on a hot path, the cost the `Arc` handles from `crates/rimz/src/disk/parse_cache.rs` remove.
5. Multiplexer server cost. Attribute the multiplexer's own children and resident set separately. Zellij's compact-bar instances and scrollback footprint are upstream costs that RimZ bounds but does not own ([performance.md → Deferred and rejected](./performance.md#deferred-and-rejected)).

## Two hard cases

### Symbols from a replaced binary

A process whose binary file was removed keeps running from the deleted inode. `/proc/<pid>/exe` then reads `<path> (deleted)`, and `perf report` resolves no RimZ symbols because no file exists at the recorded path. Two paths reach this state: an atomic reinstall replaces `~/.cargo/bin/rimz` under a process that runs it, and `crates/rimz/src/reload.rs::sweep_unreferenced_builds` removes a staged build under `~/.rimz/builds/` once no workspace record names it. Recover the symbols from the inode, with `pid` set to the process you profiled:

```sh
exe=$(readlink "/proc/$pid/exe")
mkdir -p "/tmp/prof/symfs$(dirname "$exe")"
cp "/proc/$pid/exe" "/tmp/prof/symfs$exe"
perf report --symfs /tmp/prof/symfs --stdio | rustfilt
```

The symfs tree mirrors the original path, and the file name keeps the literal ` (deleted)` suffix because that is the mapping name `perf` recorded. Pipe reports through `rustfilt` whenever v0-mangled `_R…` names survive perf's own demangler.

### Memory growth against allocator high-water

Classify a rising resident set before changing allocation policy: it is usually retained state or allocator high-water, and seldom a leak.

1. Sample one host at a fixed cadence. A resident set that keeps rising while the room is idle is a leak candidate. A flat plateau after an attachment change is retained state or high-water.
2. Compare attachment counts, pets disabled, and cell pets enabled. A step tied to one attachment or feature names the owner even when the allocator keeps freed pages resident.
3. Reproduce the growth in a room the [heap-profiling build](#heap-profiling-build) started, since a heap-profiling host relaunched into a room of the shipped build drains at its first poll. Hold a disposable room with `RIMZ_BIN="$TMPDIR/heap-target/profiling/rimz" cargo xtask sandbox room --mux tmux`, or, when the growth needs real providers, start a [host room](../contributing/sidebar-live-check.md#check-against-a-real-provider-login) from that binary. Then relaunch that room's host by the [hand-relaunch procedure](#comparing-hosts-across-a-setting), its `sidebar host` command line run under `heaptrack --raw`. Without `--raw`, heaptrack interprets as it records, and on a loaded machine the host blocks on that pipe before it binds its socket. Once the host exits, heaptrack prints the command that interprets the raw file. Attaching with `heaptrack -p` needs ptrace rights over a process that is not your child, which Yama's default `ptrace_scope` of 1 denies. Keep taking step 1's resident figures from the shipped build. Outstanding Rust allocations that grow cycle over cycle are a leak. Bounded freed pages in the one owner are high-water.
4. Fix the owning feature before tuning malloc. Linux x86-64 and aarch64 builds (glibc or musl) use jemalloc with `dirty_decay_ms:0,thp:never` and defaults otherwise, unless the user overrides its configuration through `MALLOC_CONF` or `/etc/malloc.conf`. Remove duplicate owners or compact the retained representation first, and revisit allocator policy only when a single correct owner is still a material cost.

## Turn a finding into a guard

Pin every fix with a deterministic test, because a wall-clock profile proves it only once. Subprocess counts go through `proc::testkit::spawn_count` and the `git-trace` shim tests, syscall and size budgets through the fsync and byte counter gates, cache behaviour through the parse-cache and TTL-stamp tests, and wall-clock and allocation medians through `cargo xtask perf` ([performance.md → Guarding it](./performance.md#guarding-it)).

Record what the capture taught in [performance.md](./performance.md): a changed cost updates the cost map, a mistake joins the [anti-patterns](./performance.md#anti-patterns), and a win not taken is ranked under [deferred and rejected](./performance.md#deferred-and-rejected).

## The release baseline

Reproduce this capture with the steps above and compare against it before each release. Machine-specific figures (syscall totals, build ids, workspace ids) belong in a change's report. This section holds the shape a healthy room keeps.

On 2026-10-23, a live tmux room on `xlab-term` running `rimz 0.4.3+g500e4eb8f61c` held 185 agent cards (77 live), 157 agent panes, 160 worktree roots, and 125 groups, with 140 sidebar attachments on one host of 291 threads. Over three 12-second windows the host used 0.386, 0.730, and 0.590 core, and a supervisor 0.000 core in each. That room is larger than the 100-agent column of [performance.md → Overhead at fleet scale](./performance.md#overhead-at-fleet-scale), and its host sat inside that column's 0.5 to 1.5 core busy estimate or below it.

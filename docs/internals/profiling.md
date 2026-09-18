# Profiling a live fleet

> The field guide for measuring a running RimZ room: read what it already published, find the process that matters, then attach a profiler only when the cheap answers run out. The model this measures against, including the cost map and the budgets, lives in [performance.md](./performance.md). The evidence RimZ records about its own faults lives in [diagnostics.md](./diagnostics.md).

Work through the sections in order. Each step costs less than the one after it, and most questions are answered before you attach anything.

## Start with what RimZ already published

The producer writes its caches with freshness stamps in them, so a stale or looping lane is visible without a profiler. Read these first.

| File | Question it answers |
| --- | --- |
| `$XDG_RUNTIME_DIR/rimz/ws/<name>/pr-state.json` | Is the forge probe looping? Each origin repository has a `repos` entry; one with `"ok": false`, a rising `consecutive_failures`, and an advancing `refreshed_at_ms` is climbing its failure backoff. Which links are pinned instead of re-probed is [state.md → PR state](./sidebar/state.md#pr-state). |
| `$XDG_RUNTIME_DIR/rimz/ws/<name>/diff-stats.json` | How big is the git sweep? The root count is its input size, and the per-root `refreshed_at_ms` stamps show which roots are on the hot TTL and which on the idle one. |
| `~/.rimz/cache/providers/credits.json` | Is an account probe failing? The file-level `refreshed_at_ms` and each login entry's `observed_at_ms` give the usage cadence. An entry at `"ok": false` with `auth_settled: false` retries on `OAUTH_USAGE_TTL` (5 minutes); with `auth_settled: true` it waits `OAUTH_USAGE_SETTLED_TTL` (1 hour) and is not looping ([providers.md → Refresh cadences](./agents/providers.md#refresh-cadences)). |
| `~/.rimz/cache/providers/pricing-cache.json` | Is the pricing fetch or the unpriced-model chase busy? `fetched_at_secs` shows the weekly baseline; `unknown_backoff_secs` and `unknown_seen` show the chase. |
| `~/.rimz/ws/<name>/diag.log.jsonl` | Did RimZ already notice? A `tick_budget_breach` record separates `mux_wait_ms` from in-process time ([diagnostics.md](./diagnostics.md)). |

`rimz sidebar snapshot --json --no-produce` prints the live folded view and the room's shape. `--no-produce` reads the published frame and forks neither the multiplexer nor git, so the inspection does not perturb the room:

```console
$ rimz sidebar snapshot --json --no-produce | jq '{agents:(.agents|length), panes:(.agent_panes|length), roots:(.worktree_roots|length), groups:(.worktree_groups|length)}'
{
  "agents": 12,
  "panes": 12,
  "roots": 15,
  "groups": 13
}
```

## Find the producer

Establish which renderer is the producer before measuring anything. One renderer per workspace pays every external read, and the rest fold its published caches in process ([performance.md](./performance.md#one-producer-many-consumers)), so a capture of a consumer says little about the room's external cost.

The producer is the renderer with the lexically smallest instance id among fresh heartbeats. Ids are `sb_` plus a UUIDv7, so the smallest is the eldest. The election (`ProducerElection::full_scan` in `sidebar/mod.rs`) counts a heartbeat as fresh when its file mtime is within `SIDEBAR_HEARTBEAT_TTL` (5 s) and its protocol version is current; the script below approximates mtime with the `last_seen` field the same write stamps:

```console
$ rt=$(rimz paths --json | jq -r .runtime_dir)
$ jq -rs --argjson ttl 5 '
    (now - $ttl) as $cut
    | map(select((.last_seen | sub("\\.[0-9]+Z$"; "Z") | fromdate) > $cut))
    | sort_by(.instance_id) | to_entries[]
    | "\(if .key == 0 then "producer" else "consumer" end)\t\(.value.instance_id)\t\(.value.pane_id)"
  ' "$rt"/heartbeat/sidebar.*.json | column -t
producer  sb_019f7646c9c6772389da7c8a72ea01f1  zellij:terminal_0
consumer  sb_019f7646c9f47b93a712941e4a0979c0  zellij:terminal_5
consumer  sb_019f7df9052477e2a65be240d9d4d554  zellij:terminal_345
consumer  sb_019f7dfe7d767303ae175630ed402576  zellij:terminal_347
...
```

A large heartbeat count in a many-tab room is expected: every tab runs its own renderer.

Map an instance id to a pid through the process environment. Each tab runs a `rimz sidebar serve` supervisor and a worker, and only the worker, which folds, renders, and writes the heartbeat, is spawned with `RIMZ_SIDEBAR_WORKER` set (`spawn_worker` in `sidebar_pane/supervise.rs`):

```bash
for pid in $(pgrep -x rimz); do
  env_lines=$(tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null) || continue
  grep -q '^RIMZ_SIDEBAR_WORKER=' <<< "$env_lines" || continue
  instance=$(sed -n 's/^RIMZ_SIDEBAR_INSTANCE_ID=//p' <<< "$env_lines" | head -1)
  workspace=$(sed -n 's/^RIMZ_WORKSPACE_ID=//p' <<< "$env_lines" | head -1)
  printf '%s\t%s\t%s\n' "$pid" "$workspace" "$instance"
done | sort -k3
```

Select on the environment instead of grepping command lines for `rimz sidebar serve`: the supervisor carries the same arguments, and an agent's prompt can contain the string.

## Measure the process

Take CPU, memory, and IO from `/proc` first. These readings need no tool beyond the shell and are the numbers the rest of the page compares against.

### CPU

Measure CPU from the scheduler's own counters: take `utime + stime` (fields 14 and 15 of `/proc/<pid>/stat`) at both ends of a fixed window, and divide the delta by `getconf CLK_TCK` and the window length. Sampling tools cannot skew this figure:

```console
$ cat cpu.sh
pids="2662924 2662907"; secs=12; hz=$(getconf CLK_TCK)
declare -A before
for pid in $pids; do before[$pid]=$(awk '{print $14 + $15}' /proc/$pid/stat); done
sleep $secs
for pid in $pids; do
  after=$(awk '{print $14 + $15}' /proc/$pid/stat)
  awk -v d=$(( after - before[$pid] )) -v hz=$hz -v s=$secs -v p=$pid \
    'BEGIN { printf "pid %s  %.3f core\n", p, d / hz / s }'
done
$ bash cpu.sh
pid 2662924  0.043 core
pid 2662907  0.008 core
```

The first pid is the producer and the second a consumer, and that split is the healthy shape: the producer carries the room's external reads and consumers cost close to nothing. A consumer near the producer's figure means the election or the adoption path ([state.md → Adoption and fallback](./sidebar/state.md#adoption-and-fallback)) is broken.

`pidstat` needs `-t` to be useful here. Its per-process row reports the main thread only and understates a multi-threaded renderer: one producer read 2.4% as a process row and 38% summed over its `-t` thread rows. Parse its columns by header name, because a 12-hour locale inserts an AM/PM column and shifts the rest.

### Memory

Read memory from `/proc/<pid>/smaps_rollup`: report `Pss`, and compute USS as `Private_Clean + Private_Dirty + Private_Hugetlb`. Summing RSS across the renderer family double-counts shared mappings. Compare only newly started workers, and only after at least two producer refresh cycles, because glibc arenas keep earlier high-water allocations resident. A rising resident set has its own procedure under [memory growth against allocator high-water](#memory-growth-against-allocator-high-water).

### IO

Take the delta of `read_bytes` and `write_bytes` in `/proc/<pid>/io` over a fixed window. The hot runtime caches live in `$XDG_RUNTIME_DIR`, which is tmpfs, so their churn is memory traffic; sustained disk bytes point at durable state or transcripts.

## Build a profilable binary

Profile a binary built with the `profiling` Cargo profile. `perf` and `samply` need frame pointers and line tables, and the release profile strips symbols.

```sh
cargo xtask profile-build     # writes target/profiling/rimz
target/profiling/rimz --version
```

The profile inherits `release` and adds `debug = "line-tables-only"` and `strip = "none"`; xtask adds frame pointers and v0 symbol mangling through `RUSTFLAGS`. `cargo xtask install-dev` installs the same profile, with the `sentry` feature, to `~/.cargo/bin`, so a dogfooding binary is directly profilable ([rust-conventions.md](../contributing/rust-conventions.md)).

Check which build is running before trusting absolute numbers. Frames such as `core::ub_checks::*` or `precondition_check` mark a debug build, whose absolute CPU overstates release by roughly 5x. Ratios and rates (forks per second, folds per second, the producer and consumer split) stay valid across builds.

## Pick the tool by the question

Start with `strace -f -c -p <pid>` when a process is busy for no clear reason. It shows syscall shape, fork and exec storms, lock contention, and IO without rebuilding or restarting anything. The costliest faults this subsystem has had (a `git` fork storm, a forge-probe retry loop, exec failures against a replaced binary) all show in syscall counts; a CPU profile confirms the shape once `strace` has found it.

| Question | Tool |
| --- | --- |
| Which binaries fork, how often, and do they succeed? | `strace -f -e trace=execve -p <pid>`. A repeating `ENOENT` on a RimZ path is a stale self-exec after a reinstall ([symbols from a replaced binary](#symbols-from-a-replaced-binary)). |
| Where is CPU going by function? | `samply record -p <pid>` for a Firefox Profiler call tree, or `perf record --call-graph fp -p <pid> -- sleep 10` then `perf report --stdio` for flat self-time. |
| Which allocations churn? | `heaptrack` or DHAT for ownership. The `malloc`, `memmove`, and `clone` share of a `perf` profile is the first-pass signal. |
| What goes out on the network? | `strace -f -e trace=%network -p <pid>` for the producer's own calls, `ss -tanp` for established peers, and the published cache stamps for cadence. |
| How much does each pane render, and what does SSH carry? | `rimz pane bandwidth`, run on the host serving the room. Its per-pane rows are each pane's write rate; its `WIRE(ssh)` row is the payload on the room's SSH socket, usually far below the per-pane sum, and is absent for a local room. |
| What does an account refresh cost? | Set `RIMZ_ACCOUNT_REFRESH_TRACE` to a file path, or to `1` or `true` for `account_refresh_trace.jsonl` in `~/.rimz/cache/providers/`. The trace (`sidebar/refresh/trace.rs`) appends one JSON line per provider probe, probe batch, cache contention, claim, helper spawn, and usage-helper run, with outcomes and durations and no commands, paths, identities, tokens, URLs, or bodies. It rotates at 1 MiB. |

To profile without a live room, run the profiling binary on the path under test: `samply record target/profiling/rimz sidebar snapshot --json`, with `--no-produce` added when the question is the read-only path. `samply record -- cargo xtask perf` covers the benchmarks.

Check host policy before attaching, because it limits these tools more than RimZ does. Read `kernel.perf_event_paranoid` and `kernel.yama.ptrace_scope`: `perf` needs `sudo` or a lower `perf_event_paranoid` when the value is 3 or higher. Hardware LBR call graphs may be unavailable under virtualization, so use `--call-graph fp`. Keep `strace` windows to a few seconds, because syscall tracing slows the process it observes. A failed attach is a failed measurement: record it instead of silently substituting another tool.

Treat captures as sensitive. Traces and argv contain project paths and command text, so set `umask 077`, keep the capture directory outside the repository, copy the measurements that matter into the change report, and delete the directory.

## What to look at, in order

Check these signals in order; the earlier ones find more for less effort.

1. Fork and exec rate, and outcome. A steady exec rate at idle is a retry loop or a cache that never goes fresh, and failed execs matter as much as the rate. `strace -e trace=execve` together with the published caches locates it.
2. Refold rate against event rate. Compare the store's events per second with each renderer's fold rate. Every renderer folding at the writer's full event rate multiplies the room's cost by its tab count; only the watched tab and the producer need the full rate.
3. The producer and consumer split. Sum thread-level CPU per process across the workspace. Producer-heavy cost points at the external-read lanes; consumer-heavy cost points at fold and render work multiplied by renderer count.
4. Allocation share. `malloc`, `memmove`, and `clone` frames above 10 to 15% of samples in fold or enrich paths mean deep clones on a hot path, the cost the `Arc` handles from `disk/parse_cache.rs` remove.
5. Multiplexer server cost. Attribute the multiplexer's own children and resident set separately. Zellij's server-side `ps` runs and scrollback footprint are upstream costs that RimZ bounds but does not own ([performance.md](./performance.md#deferred-and-rejected)).

## Two hard cases

### Symbols from a replaced binary

An atomic reinstall leaves every long-lived process executing a deleted inode. `/proc/<pid>/exe` then reads `.../rimz (deleted)`, and `perf report` resolves no RimZ symbols because no file exists at the recorded path. Recover them from the inode:

```sh
cp /proc/<pid>/exe /tmp/prof/rimz-old
mkdir -p /tmp/prof/symfs/home/<user>/.cargo/bin
cp /tmp/prof/rimz-old "/tmp/prof/symfs/home/<user>/.cargo/bin/rimz (deleted)"
perf report --symfs /tmp/prof/symfs --stdio | rustfilt
```

The symfs tree mirrors the original install path, and the file name keeps the literal ` (deleted)` suffix because that is the mapping name `perf` recorded; `perf buildid-cache -a` alone does not resolve it. Pipe reports through `rustfilt` whenever v0-mangled `_R…` names survive perf's own demangler.

### Memory growth against allocator high-water

Classify a rising resident set before changing allocation policy: it is usually retained state or allocator high-water, and seldom a leak.

1. Sample one role at a fixed cadence. A resident set that keeps rising while the room is idle is a leak candidate; a flat plateau after a role change is retained state or high-water.
2. Compare roles: an ordinary consumer, the current producer, a demoted former producer, pets disabled, and cell pets enabled. A step the size of one role names the owner even when the allocator keeps freed pages resident.
3. Attach `heaptrack`. Outstanding Rust allocations that grow cycle over cycle are a leak; bounded freed pages in the one owner are high-water.
4. Fix the owning feature before tuning malloc. Remove duplicate owners or compact the retained representation first, and revisit allocator policy only when a single correct owner is still a material cost.

## Turn a finding into a guard

Pin every fix with a deterministic test, because a wall-clock profile proves it only once. Subprocess counts go through `proc::testkit::spawn_count` and the `git-trace` shim tests; syscall and size budgets through the fsync and byte counter gates; cache behaviour through the parse-cache and TTL-stamp tests; wall-clock and allocation medians through `cargo xtask perf` ([performance.md → Guarding it](./performance.md#guarding-it)).

Record what the capture taught in [performance.md](./performance.md): a changed cost updates the cost map, a mistake joins the [anti-patterns](./performance.md#anti-patterns), and a win not taken is ranked under [deferred and rejected](./performance.md#deferred-and-rejected).

## The release baseline

Reproduce this capture with the steps above and compare against it before each release. Machine-specific figures (absolute core counts, syscall totals, build ids, workspace ids) belong in a change's report; this section holds the shape a healthy room keeps.

On July 20, 2026, a live Zellij room on `xlab-term` held 12 agents across 12 panes, 15 worktree roots, 13 groups, and 14 renderers in one workspace. Scheduler CPU over a 12-second window put the elected producer at 0.043 core and a representative consumer at 0.008 core, well inside the `<0.3 core` busy target for a whole room ([performance.md → Overhead at fleet scale](./performance.md#overhead-at-fleet-scale)). The ratio is the figure to compare: consumers stay near zero because they fold published caches instead of reading the world.

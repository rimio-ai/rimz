# Profiling a live fleet

> The field guide for measuring a running RimZ room: read what it already published, find the process that matters, then attach a profiler only when the cheap answers run out. The model this measures against, including the cost map and the budgets, lives in [performance.md](./performance.md). The evidence RimZ records about its own faults lives in [diagnostics.md](./diagnostics.md).

Work in the order below: each step costs less than the one after it, and most questions are answered before you attach anything.

## Start with what RimZ already published

The producer writes its caches to disk with freshness stamps on them, so a stale or looping lane is visible without a profiler. Read these first.

| File | Question it answers |
| --- | --- |
| `$XDG_RUNTIME_DIR/rimz/<ws>/pr-state.json` | Is the forge probe looping? An entry with `"ok": false` and an advancing `refreshed_at_ms` is climbing its failure backoff. Terminal `merged` states are pinned rather than re-probed. |
| `$XDG_RUNTIME_DIR/rimz/<ws>/diff-stats.json` | How big is the git sweep? The root count is its input size, and per-root stamps show the hot/idle tiering in effect. |
| `~/.local/state/rimz/shared/credits.json` | Is an account probe failing? Cache-level `refreshed_at_ms` plus per-provider `observed_at_ms` give the usage cadence; a provider stuck at `"ok": false` is retrying on the short TTL. |
| `~/.local/state/rimz/shared/pricing-cache.json` | `fetched_at_secs` shows the weekly baseline; `unknown_backoff_secs` and `unknown_seen` show the unpriced-model chase. |
| `~/.local/state/rimz/workspaces/<ws>/diag.log.jsonl` | Did RimZ already notice? `tick_budget_breach` separates `mux_wait_ms` from in-process time ([diagnostics.md](./diagnostics.md)). |

`rimz sidebar snapshot --json --no-produce` prints the live folded view and the room's shape. `--no-produce` keeps the read passive, so inspection never perturbs the room:

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

One renderer per workspace pays every external read; the rest fold its published caches in process ([performance.md](./performance.md#one-producer-many-consumers)). Profiling a consumer when you meant the producer is the most common wasted capture, so establish the roles before anything else.

The producer is the eldest live renderer, and instance ids are UUIDv7, so the lexically smallest fresh heartbeat wins:

```console
$ ws=$(rimz workspace resolve . | jq -r .workspace_id)
$ jq -rs --argjson ttl 5 '
    (now - $ttl) as $cut
    | map(select((.last_seen | sub("\\.[0-9]+Z$"; "Z") | fromdate) > $cut))
    | sort_by(.instance_id) | to_entries[]
    | "\(if .key == 0 then "producer" else "consumer" end)\t\(.value.instance_id)\t\(.value.pane_id)"
  ' "${XDG_RUNTIME_DIR:-/run/user/$UID}"/rimz/"$ws"/heartbeat/sidebar.*.json | column -t
producer  sb_019f7646c9c6772389da7c8a72ea01f1  zellij:terminal_0
consumer  sb_019f7646c9f47b93a712941e4a0979c0  zellij:terminal_5
consumer  sb_019f7df9052477e2a65be240d9d4d554  zellij:terminal_345
consumer  sb_019f7dfe7d767303ae175630ed402576  zellij:terminal_347
...
```

Map an instance to a pid through the worker's environment. Each tab runs a supervisor and a worker; only the worker carries `RIMZ_SIDEBAR_WORKER` and does the fold, render, and heartbeat work:

```bash
for pid in $(pgrep -x rimz); do
  env_lines=$(tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null) || continue
  grep -q '^RIMZ_SIDEBAR_WORKER=' <<< "$env_lines" || continue
  instance=$(sed -n 's/^RIMZ_SIDEBAR_INSTANCE_ID=//p' <<< "$env_lines" | head -1)
  workspace=$(sed -n 's/^RIMZ_WORKSPACE_ID=//p' <<< "$env_lines" | head -1)
  printf '%s\t%s\t%s\n' "$pid" "$workspace" "$instance"
done | sort -k3
```

Match the three argv slots rather than grepping the whole command line: an agent prompt can contain the string `rimz sidebar serve`. A large heartbeat count in a many-tab room is the design working, not a leak; one supervisor and worker pair per tab is expected.

## Measure the process

**CPU, from the scheduler.** Delta `utime + stime` (fields 14 and 15 of `/proc/<pid>/stat`) over a fixed window and divide by `getconf CLK_TCK` and elapsed seconds. This is scheduler truth, immune to sampling-tool quirks:

```console
$ producer=2662924; consumer=2662907; secs=12; hz=$(getconf CLK_TCK)
$ for pid in $producer $consumer; do awk '{print $14, $15}' /proc/$pid/stat > /tmp/$pid.before; done
$ sleep $secs
$ for pid in $producer $consumer; do
    awk '{print $14, $15}' /proc/$pid/stat > /tmp/$pid.after
    read b1 b2 < /tmp/$pid.before; read a1 a2 < /tmp/$pid.after
    awk -v d=$(( (a1+a2) - (b1+b2) )) -v hz=$hz -v s=$secs -v p=$pid \
      'BEGIN{printf "pid %s  %.3f core\n", p, d/hz/s}'
  done
pid 2662924  0.043 core
pid 2662907  0.008 core
```

That split is the expected shape: the producer carries the room's external reads, and consumers cost close to nothing. A consumer near the producer's figure means the election or the adoption path is broken.

If you reach for `pidstat`, it needs `-t`. Per-process rows report the main thread only and badly understate a multi-threaded renderer: one producer read 2.4% as a process row and 38% summed over `-t` thread rows. Parse its columns by header name, because a 12-hour locale inserts an AM/PM field and shifts every column.

**Memory.** Use `/proc/<pid>/smaps_rollup`: report `Pss`, and compute USS as `Private_Clean + Private_Dirty + Private_Hugetlb`. Never sum RSS across the family, because it double-counts shared mappings. Compare only newly started workers, and only after at least two producer refresh cycles, because glibc arenas retain historical high-water allocations.

**IO.** Delta `/proc/<pid>/io` read and write bytes over a fixed window. The hot runtime caches live in `$XDG_RUNTIME_DIR` (tmpfs), so their churn is memory traffic; sustained disk bytes point at durable state or transcripts.

## Build a profilable binary

`perf` and `samply` need frame pointers and line tables, which the shipped release binary does not carry.

```sh
cargo xtask profile-build     # writes target/profiling/rimz
target/profiling/rimz --version
```

`cargo xtask install-dev` installs the same profiling profile with the dev-only `sentry` feature, so a dogfooding session is optimized and directly profilable.

Verify which build is actually running before trusting absolute numbers. Frames like `core::ub_checks::*` or `precondition_check` are the tell for a debug build: absolute CPU overstates release by roughly 5x, while ratios and rates (forks/s, folds/s, the producer/consumer split) stay valid.

## Pick the tool by the question

Start with `strace -f -c -p <pid>` when a process is busy for unclear reasons. It shows syscall shape, fork and exec storms, lock contention, and IO without rebuilding or restarting anything. Every capture so far started there, and the biggest findings (the git fork storm, the PR-probe retry loop, the deleted-binary exec failures) were visible in syscall counts before any CPU profile ran. `perf` confirmed shapes; it did not discover them.

| Question | Tool |
| --- | --- |
| Which binaries fork, how often, and do they succeed? | `strace -f -e trace=execve -p <pid>`. A repeating `ENOENT` on a RimZ path is a stale self-exec after a reinstall ([symbols from a replaced binary](#symbols-from-a-replaced-binary)). |
| Where is CPU going by function? | `samply record -p <pid>` for a Firefox-Profiler call tree, or `perf record --call-graph fp -p <pid> -- sleep 10` plus `perf report --stdio` for flat self-time. |
| Which allocations churn? | `heaptrack` or DHAT for ownership; the `malloc`/`memmove`/`clone` share of a `perf` profile as the first-pass signal. |
| What goes out on the network? | `strace -f -e trace=%network -p <pid>` for the producer's own calls, `ss -tanp` for established peers, and the runtime cache stamps for cadence. `rimz remote bandwidth` attributes the SSH render stream per pane. |
| What does an account refresh cost? | Set `RIMZ_ACCOUNT_REFRESH_TRACE` to a file path (or `1` for a default name beside the shared caches). It records per-provider probe outcomes and durations, claim and helper-spawn decisions, and usage-helper timings, with commands, paths, identities, tokens, URLs, and bodies excluded. |

For a reproducible profile without a live room, run the profiling binary against the path under test: `samply record target/profiling/rimz sidebar snapshot --json`, or add `--no-produce` when the question is read-only process shape. `samply record -- cargo xtask perf` covers the microbench tier.

Host policy shapes these more than RimZ does. `perf` needs `sudo` or a lower `kernel.perf_event_paranoid` when the value is 3 or higher; hardware LBR call graphs may be unavailable under virtualization, so use `--call-graph fp`; and `strace` windows stay short, a few seconds, because syscall tracing perturbs the process it observes. Check `kernel.perf_event_paranoid` and `kernel.yama.ptrace_scope` before attaching. A failed attach is a failed measurement: record it rather than silently substituting another tool.

Traces and argv can contain project paths and command text. Set `umask 077`, keep the capture directory uncommitted, distill the relevant measurements into the change report, then delete the directory.

## What to look at, in order

Ranked by what has actually paid off.

1. **Fork and exec rate, and outcome.** A steady per-second exec rate at idle is a retry loop or a cache that never goes fresh, and failures matter as much as rate. `strace -e trace=execve` plus the published caches localize it in minutes.
2. **Refold rate against event rate.** Compare event-log events/s against per-renderer fold rate. Every renderer folding at the writer's full event rate multiplies one room's cost by its tab count; only the watched tab and the producer need full rate.
3. **The producer and consumer split.** Sum thread-level CPU per process across the workspace. Producer-heavy cost points at the external-read lanes; consumer-heavy cost points at fold and render work multiplied by renderer count.
4. **Allocation share.** `malloc`, `memmove`, and `clone` frames above 10 to 15% of samples in fold or enrich paths mean deep clones on a hot path. The parse-cache `Arc` returns came from exactly this signal.
5. **Mux server cost.** Attribute the multiplexer's own children and RSS separately. Zellij's server-side `ps` storm per `list-panes` and its scrollback footprint are upstream costs RimZ bounds but does not own ([performance.md](./performance.md#deferred-and-rejected)).

## Two hard cases

### Symbols from a replaced binary

An atomic reinstall leaves every long-lived process executing a deleted inode. `/proc/<pid>/exe` readlinks to `.../rimz (deleted)`, and `perf` resolves no RimZ symbols because the on-disk binary no longer matches. Recover them from the inode itself:

```sh
cp /proc/<pid>/exe /tmp/prof/rimz-old
mkdir -p /tmp/prof/symfs/home/<user>/.cargo/bin
cp /tmp/prof/rimz-old "/tmp/prof/symfs/home/<user>/.cargo/bin/rimz (deleted)"
perf report --symfs /tmp/prof/symfs --stdio | rustfilt
```

The symfs tree mirrors the original install path and the filename keeps the literal ` (deleted)` suffix, because that string is the map name `perf` recorded; `perf buildid-cache -a` alone does not resolve it. Pipe reports through `rustfilt` whenever v0-mangled `_R…` names survive perf's own demangler.

### Memory growth against allocator high-water

A rising resident set is usually retained state or a freed arena, not a leak. Classify the shape before changing allocation policy.

1. **Sample one role at a fixed cadence.** A rising idle plateau is a leak candidate; a flat plateau after a role transition is retained state or allocator high-water.
2. **Compare roles.** Ordinary consumer, current producer, demoted former producer, pets disabled, cell pets enabled. A role-sized step names the owner even when the allocator keeps freed pages resident.
3. **Attach heaptrack.** Outstanding Rust allocations that grow cycle over cycle are a leak. Bounded freed pages in the sole owner are high-water.
4. **Trace the owning feature before tuning malloc.** Remove duplicate owners or compact the retained representation first. Revisit allocator policy only when one correct owner remains a material cost.

## Turn a finding into a guard

A wall-clock profile proves a fix once; a deterministic guard keeps it fixed. Pin subprocess counts with `proc::testkit::spawn_count` and the `git-trace` shim tests, syscall and size budgets with the fsync and byte counter gates, cache behavior with parse-cache and TTL-stamp tests, and wall-clock and allocation medians with `cargo xtask perf` ([performance.md](./performance.md#guarding-it)).

Conclusions flow back into [performance.md](./performance.md): a fixed cost updates the cost map, a removed mistake joins the lessons, and a real deferred win gets ranked in deferred work.

## The release baseline

Reproduce this with the steps above and compare before each release. Machine-specific figures (absolute core counts, syscall totals, build ids, workspace ids) belong in a change's implementation report; what lands here is the shape a healthy room holds.

**July 20, 2026.** Live Zellij room on `xlab-term`: 12 agents across 12 panes, 15 worktree roots, 13 groups, 14 renderers in one workspace. Scheduler-truth CPU over a 12-second window put the elected producer at 0.043 core and a representative consumer at 0.008 core, well inside the `<0.3 core` busy target for the whole room. The producer/consumer ratio is the number that matters: consumers stay near zero because they fold published caches rather than reading the world.

Earlier dated captures are in git history. Their conclusions already live in [performance.md](./performance.md): every optimization they produced is described under [what's optimized](./performance.md#whats-optimized), and every mistake they found is under [lessons from removed anti-patterns](./performance.md#lessons-from-removed-anti-patterns).

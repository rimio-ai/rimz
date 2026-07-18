# Profiling

> The field guide for profiling a live RimZ fleet: pick the tool by the question, attach to the right process, recover symbols, read the runtime state, and turn what you find into a deterministic guard. The performance model, cost map, and budgets this guide measures against live in [performance.md](./performance.md); every dated live capture lands in the [capture log](#capture-log) here.

## Pick the tool by the question

Start with `strace -f -c -p <pid>` when a process is busy for unclear reasons: it shows syscall shape, fork/exec storms, lock contention, and IO without rebuilding or restarting a live process. Every capture so far started there, and the biggest findings — the git fork storm, the PR-probe retry loop, the deleted-binary exec failures — were visible in syscall counts before any CPU profile ran; `perf` confirmed shapes, it did not discover them.

- **Fork/exec rate and outcome** — `strace -f -e trace=execve -p <pid>`: which binaries, how often, and whether they succeed. A repeating `ENOENT` on a RimZ path is a stale self-exec after a reinstall ([symbols from a replaced binary](#symbols-from-a-replaced-binary) explains the inode state behind it).
- **CPU by function** — `samply record -p <pid>` or `perf record --call-graph fp -p <pid> -- sleep 10` against the profiling build. `samply` opens a Firefox-Profiler call tree that reads Rust stacks well; `perf report --stdio` gives flat self-time and callees.
- **CPU by process and thread** — `pidstat -h -u -t -p <pid> 1 5`, or raw `/proc/<pid>/stat` deltas ([attach to the right process](#attach-to-the-right-process) lists the pitfalls).
- **RSS and allocation churn** — `/usr/bin/time -v` for peak RSS, `heaptrack` or DHAT when an allocation regression needs ownership, and the `malloc`/`memmove`/`clone` share of a `perf` profile as the first-pass churn signal.
- **IO** — `/proc/<pid>/io` read/write byte deltas over a fixed window. The hot runtime caches live in `$XDG_RUNTIME_DIR` (tmpfs), so their churn is memory traffic; sustained disk bytes point at durable state or transcripts.
- **Network egress by process and lane** — `strace -f -e trace=%network -p <pid>` catches the producer's direct `connect`/`sendto`/`recvfrom` calls with syscall byte counts, while spawned `gh`/`tea` and agent CLIs own many sockets themselves. `ss -tanp` snapshots established peers and is often empty at idle because the account, pricing, and forge calls are short-lived; runtime cache stamps (`pr-state.json`, shared `credits.json`, shared `pricing-cache.json`) are the fastest cadence profiler with nothing attached; `rimz remote bandwidth` attributes the SSH render-stream payload per pane, and its `WIRE(ssh)` rows are the TCP bytes. `nethogs` and `bpftrace` may be absent and `tcpdump` needs elevated privileges, so `strace`, `ss`, and cache stamps carry the usual question.

Host settings shape the tools more than RimZ: `perf` needs `sudo` or a lower `kernel.perf_event_paranoid` when the value is 3 or higher, hardware LBR call graphs may be unavailable under virtualization (use `--call-graph fp`), and `strace` windows stay short — a few seconds — because syscall tracing perturbs the process it observes.

For a reproducible command-shaped profile without a live room, run the profiling binary directly against the path under test — `samply record target/profiling/rimz sidebar snapshot --json`, or `--no-produce` over a synthetic seeded workspace when the question is read-only process shape. `samply record -- cargo xtask perf` and `/usr/bin/time -v cargo xtask perf` cover the microbench tier.

### Memory growth or retained high-water

Use `/proc/<pid>/smaps_rollup` PSS and `Private_Dirty`/`Private_Clean` as the resident truth, then classify the shape before changing allocation policy:

1. Sample the same role at fixed cadence. A rising idle plateau is a leak candidate; a flat plateau after a role transition is retained state or allocator high-water.
2. Compare ordinary consumers, the current producer/service owner, a demoted former producer, pets disabled, and cell pets enabled. A role-sized step names ownership even when the allocator keeps freed pages resident.
3. Attach heaptrack to distinguish outstanding Rust allocations from freed arena pages. Outstanding objects that grow cycle-over-cycle are a leak; bounded freed pages in the sole owner are allocator high-water.
4. Trace the owning feature before tuning malloc. Remove duplicate owners or compact retained representation first; revisit allocator policy only when one correct owner remains a material family cost.

The reproducible fixture accepts `--spending-files 6000 --spending-entries 100000`; run three workspace scopes with the printed `CODEX_HOME`, state, and runtime exports against the same shared cursor. Preserve the exact baseline binary and build id, sample promotion/demotion cycles, and keep host-specific PSS and heaptrack figures in the change report rather than this guide.

## Build and verify a profilable binary

```sh
cargo xtask profile-build
target/profiling/rimz --version
```

`cargo xtask profile-build` writes `target/profiling/rimz`: optimized like release, with line tables, frame pointers, and v0 symbol mangling for demangled call trees. `cargo xtask install-dev` installs the same profiling profile with the dev-only `sentry` feature, so dogfooding sessions are optimized and directly profilable; the shipped release binary keeps no frame-pointer or debug-info cost.

## Profile a cold account refresh safely

Measure account enrichment with fresh RimZ caches while retaining the real provider login surfaces. Keep `HOME`, `XDG_DATA_HOME`, `CODEX_HOME`, Pi/OpenCode data overrides, `PATH`, and keychain access inherited; give every sample new `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`, and `XDG_CONFIG_HOME` directories. Use a short `/tmp` runtime root so workspace socket paths stay below the AF_UNIX limit. This isolates `accounts.json`, `credits.json`, `rate_limits.json`, cache locks, and workspace snapshots without deleting or moving live state.

Create a deterministic empty pane fixture and preserve the exact binary under comparison. `RIMZ_BIN` pins detached usage helpers to that same artifact, and an explicit `RIMZ_ACCOUNT_REFRESH_TRACE` path keeps samples separate.

```sh
cargo xtask profile-build
cp target/profiling/rimz /tmp/rimz-account-before
printf '[]\n' > /tmp/rimz-empty-panes.json
state=$(mktemp -d /tmp/rimz-account-state.XXXXXX)
runtime=$(mktemp -d /tmp/rimz-account-runtime.XXXXXX)
config=$(mktemp -d /tmp/rimz-account-config.XXXXXX)
trace=$state/account-refresh.jsonl
XDG_STATE_HOME=$state XDG_RUNTIME_DIR=$runtime XDG_CONFIG_HOME=$config RIMZ_TEST_PANE_LIST=/tmp/rimz-empty-panes.json RIMZ_ACCOUNT_REFRESH_TRACE=$trace RIMZ_BIN=/tmp/rimz-account-before strace -f -tt -e trace=process,network -o "$state/strace.log" /tmp/rimz-account-before sidebar snapshot --json --workspace-id ws_0123456789abcdef01234567 --mux zellij --session-name account-refresh-profile > "$state/snapshot.json"
```

The opt-in trace is absent by default. Set `RIMZ_ACCOUNT_REFRESH_TRACE` to an explicit file, or to `1`/`true` for `account_refresh_trace.jsonl` beside the shared caches. Records use Unix `at_ms` only to order the snapshot process and detached helpers; durations come from monotonic clocks. `provider_probe` names kind, normalized outcome, and account/version/total milliseconds; `probe_batch` names due/worker/success counts and total milliseconds; `contention`, `claim`, and `helper_spawn` name only normalized decisions; `usage_helper` names kind, normalized outcome, realtime/direct/cache-publication/total milliseconds. The schema excludes commands, paths, account identities, claim nonces, tokens, URLs, request and response bodies, and plan labels.

Poll the isolated cache files with a finite deadline. Account publication completes when `accounts.json` exists; usage for each discovered metered provider with a supported usage surface completes when its `credits.json` entry has a nonzero `oauth_read_at_ms` and no `direct_query_claim`. Confirm the expected provider's authoritative windows in `rate_limits.json`; a settled or failed provider result is a valid terminal outcome, while a claim still present at the deadline is reported as lingering rather than replaced by hand. Count `execve` and network timestamps from the matching `strace.log` and read claim/spawn/publication timing from the trace.

Take at least three samples for each binary and report the logged-in provider set, median, and range for snapshot/account publication, per-provider usage publication, total convergence, subprocess count, and process/network timing. Compare identical isolated commands and poll criteria before and after the change; keep network-dependent wall time and all machine-specific measurements in the implementation report rather than CI assertions or this guide.

Verify which build is actually running before trusting absolute numbers. Frames like `core::ub_checks::*` or `precondition_check` in a profile are the tell for a debug build (opt-level 0 with debug assertions): absolute CPU overstates release by roughly 5×, while ratios and rates — forks/s, folds/s, the producer/consumer split — stay valid. The July 5 baseline ran on exactly such a build; its ratios held, its absolute figures did not.

## Attach to the right process

- **Count sidebar processes with a NUL-aware `/proc` scan.** Match the three argv slots rather than a whole-cmdline substring, because an agent prompt or unrelated argument can contain `rimz sidebar serve`. The worker-only environment marker separates the stable supervisor from the renderer that owns heartbeat, fold, and paint work.

  ```bash
  for f in /proc/[0-9]*/cmdline; do
    mapfile -d '' -t argv < "$f" 2>/dev/null || continue
    [[ ${argv[0]##*/} == rimz && ${argv[1]-} == sidebar && ${argv[2]-} == serve ]] || continue
    pid=${f#/proc/}; pid=${pid%/cmdline}
    if tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null | grep -q '^RIMZ_SIDEBAR_WORKER='; then role=worker; else role=supervisor; fi
    printf '%s %s\n' "$pid" "$role"
  done
  ```

- **Heartbeats count live workers.** One file per live worker under `$XDG_RUNTIME_DIR/rimz/ws_<id>/heartbeat/`, one supervisor/worker pair per tab — a large count in a many-tab room is the design working. Only the worker writes the heartbeat and performs fold/render work; the stable supervisor owns convergence across reloads. A leak hypothesis needs the argv scan above to distinguish the two before it holds.
- **Find the producer.** One renderer per workspace pays all external reads (the eldest by UUIDv7 birth order, [performance.md → One producer per workspace](./performance.md#one-producer-per-workspace)); it is the one forking `git`/`gh`/`tea` children and usually the top CPU consumer. The younger renderers are consumers whose cost is pure in-process folding.
- **`pidstat` needs `-t`.** Per-process rows report the main thread only and badly understate a multi-threaded renderer — the same producer read 2.4% as a process row and 38% summed over `-t` thread rows. Parse columns by header name rather than position: a 12-hour locale inserts an AM/PM field and shifts every column.
- **Cross-check with `/proc/<pid>/stat`.** Delta `utime + stime` (fields 14 and 15) over a fixed window and divide by `CLK_TCK` ticks/s for a scheduler-truth core count, immune to sampling-tool quirks.

## Capture one producer and one hidden consumer

Start with the whole RimZ process family, then drill into one producer and one demonstrably hidden consumer. Set private permissions before reading argv, environment, or traces: these can contain project paths and command text. Keep the capture directory uncommitted, distill only the relevant measurements into the implementation report, then remove the directory.

```bash
umask 077
export LC_ALL=C
for cmd in awk column cp date grep head jq mktemp perf pgrep readlink rg rimz rm rustfilt sed sha256sum sleep sort stat strace sysctl tail tee timeout tr; do
  command -v "$cmd" >/dev/null || { printf 'missing required command: %s\n' "$cmd" >&2; exit 1; }
done

capture=$(mktemp -d /tmp/rimz-fleet-profile.XXXXXX)
family=$capture/process-family.tsv
printf 'pid\trole\tthreads\tpss_kib\tuss_kib\targv\n' > "$family"

while read -r pid; do
  { mapfile -d '' -t argv < "/proc/$pid/cmdline"; } 2>/dev/null || continue
  [[ ${argv[0]##*/} == rimz ]] || continue
  env_lines=$({ tr '\0' '\n' < "/proc/$pid/environ"; } 2>/dev/null) || env_lines=
  if [[ ${argv[1]-} == sidebar && ${argv[2]-} == serve ]]; then
    if grep -q '^RIMZ_SIDEBAR_WORKER=' <<< "$env_lines"; then role=sidebar-worker; else role=sidebar-supervisor; fi
  elif [[ ${argv[1]-} == agents && ${argv[2]-} == exec ]]; then
    role=agents-exec
  elif [[ ${argv[1]-} == stats ]]; then
    role=stats
    for arg in "${argv[@]:2}"; do [[ $arg == --hold ]] && role=daemon-content; done
  elif [[ ${argv[1]-} == daemon && ${argv[2]-} == content ]]; then
    role=daemon-content
  elif [[ ${argv[1]-} == loop && ${argv[2]-} == watch ]]; then
    role=loop-watch
  elif [[ ${argv[1]-} == remote && ${argv[2]-} == link-stats ]]; then
    role=remote-link-stats
  else
    role=${argv[1]-rimz}
    [[ -n ${argv[2]-} ]] && role="$role ${argv[2]}"
  fi
  threads=$(awk '/^Threads:/{print $2}' "/proc/$pid/status" 2>/dev/null) || continue
  memory=$(awk '/^Pss:/{pss=$2} /^Private_(Clean|Dirty|Hugetlb):/{uss+=$2} END{print pss+0, uss+0}' "/proc/$pid/smaps_rollup" 2>/dev/null) || continue
  read -r pss uss <<< "$memory"
  printf -v command '%q ' "${argv[@]}"
  command=${command% }
  command=${command//$'\t'/ }
  command=${command//$'\n'/ }
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$pid" "$role" "$threads" "$pss" "$uss" "$command" >> "$family"
done < <(pgrep -x rimz)

{
  printf 'role\tprocesses\tpss_kib\tuss_kib\n'
  awk -F '\t' 'NR>1 {count[$2]++; pss[$2]+=$4; uss[$2]+=$5} END {for (role in count) print role, count[role], pss[role], uss[role]}' OFS=$'\t' "$family" | sort
} > "$capture/process-family-by-role.tsv"
column -ts $'\t' "$family"
column -ts $'\t' "$capture/process-family-by-role.tsv"
```

The family table is the fast budget check: it retains every readable exact-`rimz` process, classifies the known long-lived roles, and leaves any unfamiliar command under its first two argv words. It records thread count plus PSS and USS per PID and sums those proportional/private measures by role; summed RSS is deliberately absent because it double-counts shared mappings.

Build the sidebar inventory from the same capture. The full `RIMZ_SIDEBAR_INSTANCE_ID` sorts by UUIDv7 birth, and the lexically oldest fresh instance within each workspace/protocol is the elected producer. Heartbeat workspace/instance fields must agree with the environment and derived path before the row is admitted. Missing, stale, unreadable, mismatched, and unknown-watch rows stay out of the selection rather than becoming false hidden consumers.

```bash
raw=$capture/sidebar.raw.tsv
printf 'pid\tthreads\tworkspace\tprotocol\tinstance\tmux\tsession\tpane\twatch\tfocused\tbuild\texe\n' > "$raw"
now=$(date +%s)

while read -r pid; do
  { mapfile -d '' -t argv < "/proc/$pid/cmdline"; } 2>/dev/null || continue
  [[ ${argv[0]##*/} == rimz && ${argv[1]-} == sidebar && ${argv[2]-} == serve ]] || continue
  env_lines=$({ tr '\0' '\n' < "/proc/$pid/environ"; } 2>/dev/null) || continue
  grep -q '^RIMZ_SIDEBAR_WORKER=' <<< "$env_lines" || continue
  workspace=$(sed -n 's/^RIMZ_WORKSPACE_ID=//p' <<< "$env_lines" | head -1)
  instance=$(sed -n 's/^RIMZ_SIDEBAR_INSTANCE_ID=//p' <<< "$env_lines" | head -1)
  [[ -n $workspace && -n $instance ]] || continue
  runtime=$(sed -n 's/^XDG_RUNTIME_DIR=//p' <<< "$env_lines" | head -1)
  runtime=${runtime:-/run/user/$(id -u)}
  heartbeat=$runtime/rimz/$workspace/heartbeat/sidebar.$instance.json
  snapshot=$runtime/rimz/$workspace/snapshot.json
  [[ -r $heartbeat && -r $snapshot ]] || continue
  heartbeat_mtime=$(stat -c %Y "$heartbeat")
  (( now - heartbeat_mtime <= 5 )) || continue # SIDEBAR_HEARTBEAT_TTL
  fields=$(jq -er '[.workspace_id,.instance_id,.protocol_version,.mux,.session_name,(.pane_id // "-"),(.build // "-")] | @tsv' "$heartbeat" 2>/dev/null) || continue
  IFS=$'\t' read -r heartbeat_workspace heartbeat_instance protocol mux session pane build <<< "$fields"
  [[ $heartbeat_workspace == "$workspace" && $heartbeat_instance == "$instance" ]] || continue
  focused=$(jq -er '.focused_pane // "-"' "$snapshot" 2>/dev/null) || continue
  watch=unknown
  if [[ $pane != - ]]; then
    watch=$(jq -er --arg pane "$pane" '
      . as $frame
      | ([.tabs[]? | select(any(.panes[]?; .pane_id == $pane)) | .panes[]?.pane_id]) as $tab_panes
      | if ($tab_panes | length) == 0 then "unknown"
        elif any($frame.viewed_panes[]?; . as $viewed | $tab_panes | index($viewed)) then "watched"
        else "hidden" end
    ' "$snapshot" 2>/dev/null) || continue
  fi
  [[ $watch != unknown ]] || continue
  threads=$(awk '/^Threads:/{print $2}' "/proc/$pid/status")
  exe=$(readlink "/proc/$pid/exe" 2>/dev/null || printf '%s' unreadable)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$pid" "$threads" "$workspace" "$protocol" "$instance" "$mux" "$session" "$pane" "$watch" "$focused" "$build" "$exe" >> "$raw"
done < <(pgrep -x rimz)

{
  printf 'pid\tthreads\tworkspace\tprotocol\tinstance\trole\tmux\tsession\tpane\twatch\tfocused\tbuild\texe\n'
  tail -n +2 "$raw" | sort -t $'\t' -k3,3 -k4,4 -k5,5 | awk -F '\t' 'BEGIN{OFS=FS} {key=$3 FS $4; role=(key!=prior ? "producer" : "consumer"); prior=key; print $1,$2,$3,$4,$5,role,$6,$7,$8,$9,$10,$11,$12}'
} > "$capture/sidebar.tsv"
column -ts $'\t' "$capture/sidebar.tsv"
```

The `watch` derivation mirrors renderer policy: find the heartbeat pane's tab in published topology, then ask whether any attached client's `viewed_panes` entry belongs to that tab. Preserve `.focused_pane` as a consistency check. Select `producer` and `consumer,hidden` rows from the same workspace/protocol, set their PIDs below, and take the room fields from the producer row.

```bash
producer=PID_FROM_SIDEBAR_TSV
consumer=HIDDEN_PID_FROM_SIDEBAR_TSV
IFS=$'\t' read -r workspace mux session < <(awk -F '\t' -v pid="$producer" 'BEGIN{OFS=FS} NR>1 && $1==pid {print $3,$7,$8}' "$capture/sidebar.tsv")

rimz sidebar snapshot --json --no-produce --workspace-id "$workspace" --mux "$mux" --session-name "$session" > "$capture/sidebar-snapshot.json"
for role in producer consumer; do
  pid=${!role}
  cp --dereference "/proc/$pid/exe" "$capture/rimz-$role"
  sha256sum "$capture/rimz-$role"
done > "$capture/binaries.sha256"
```

Write the manifest before any long attach. Record date, fixed durations, backend, workspace/tab/agent counts, selected PIDs and thread counts, full instances and roles, watched/focused state, heartbeat/pane/metrics/store/sidecar timestamps, heartbeat protocol and pane-cache build ids, and whether each `/proc/<pid>/exe` readlink ends in ` (deleted)`. The selected-room command above prevents a shell in another checkout from silently inspecting the wrong workspace; the dereferenced copies preserve replaced/deleted inodes for the symfs workflow below.

Measure scheduler CPU and proportional memory over the same fixed window. Save `utime`/`stime` and `smaps_rollup` for both PIDs at each endpoint, sleep once, then sample both again; divide each tick delta by `getconf CLK_TCK` and elapsed seconds for cores. Report `Pss` from `smaps_rollup`, and compute USS as `Private_Clean + Private_Dirty + Private_Hugetlb`. Compare memory only on newly started workers after at least two producer refresh cycles because glibc arenas retain historical high-water allocations.

```bash
secs=12
for phase in before after; do
  for pid in "$producer" "$consumer"; do
    awk '{print $14, $15}' "/proc/$pid/stat" > "$capture/$pid.$phase.stat"
    cp "/proc/$pid/smaps_rollup" "$capture/$pid.$phase.smaps_rollup"
  done
  [[ $phase == before ]] && sleep "$secs"
done
for pid in "$producer" "$consumer"; do
  awk '/^Pss:/{pss=$2} /^Private_(Clean|Dirty|Hugetlb):/{uss+=$2} END{printf "pid=%s pss_kib=%d uss_kib=%d\n", pid, pss, uss}' pid="$pid" "$capture/$pid.after.smaps_rollup"
done
```

Correlate publications before opening a CPU profiler. Record timestamp sequences for `snapshot.json`, `metrics-sample.json`, the durable store checkpoint/log, and runtime sidecar directories, then compare them with the consumer's `PaneFramePublished` datagrams and snapshot reads. The datagram's topology, metrics, or presence intent names the changed lane: hidden topology and store work may dispatch once per one-second clamp, hidden metrics once per three-second background window, and presence dispatches immediately. Count actual snapshot opens or diagnostic fold records rather than wake datagrams when deriving fold rate because pending events merge at the earliest deadline.

Run every ptrace/perf client sequentially. Capture a producer summary and process/exec trace first, then the consumer summary and timestamped per-thread path trace; this preserves the diagnostic split between producer fork/exec/cache work and consumer fold/file work. The consumer path trace also exposes publication payloads. Check it explicitly for `.git`, `rimz-worktree.json`, provider configuration, `agent_context`/`subagent_context`, read marks, messages, budget/auto-continue sidecars, and failed `execve`.

```bash
sysctl kernel.perf_event_paranoid kernel.yama.ptrace_scope | tee "$capture/kernel-profile-policy.txt"
timeout "$secs" strace -f -c -p "$producer" -o "$capture/producer.syscalls.txt"
timeout 4s strace -ff -ttt -s 512 -e trace=process,execve -p "$producer" -o "$capture/producer.process"
timeout "$secs" strace -f -c -p "$consumer" -o "$capture/consumer.syscalls.txt"
timeout 4s strace -ff -ttt -s 512 -e trace=%file,process,recvfrom -p "$consumer" -o "$capture/consumer.file"
rg -n '\.git|rimz-worktree\.json|agent_context|subagent_context|read-marks|messages|budget|auto-continue|execve' "$capture"/consumer.file*

perf record --call-graph fp -p "$producer" -o "$capture/producer.perf.data" -- sleep "$secs"
perf record --call-graph fp -p "$consumer" -o "$capture/consumer.perf.data" -- sleep "$secs"
perf report -i "$capture/producer.perf.data" --stdio --no-children | rustfilt > "$capture/producer.perf.flat.txt"
perf report -i "$capture/producer.perf.data" --stdio | rustfilt > "$capture/producer.perf.callgraph.txt"
perf report -i "$capture/consumer.perf.data" --stdio --no-children | rustfilt > "$capture/consumer.perf.flat.txt"
perf report -i "$capture/consumer.perf.data" --stdio | rustfilt > "$capture/consumer.perf.callgraph.txt"
```

> **Capture pitfalls.** Check `kernel.perf_event_paranoid` and `kernel.yama.ptrace_scope` before attaching; ptrace and perf commands may require `sudo` under the operator's policy. A failed attach is a failed measurement, so record it and do not silently substitute another tool. Build with `cargo xtask profile-build` for frame-pointer call graphs. Copy deleted executable inodes and retain build ids. Read `pidstat -t` thread totals instead of the process-main-thread row. Aggregate PSS and computed USS instead of summed RSS. Keep shell counters out of pipeline/subshell scope, inspect generated files before trusting an aggregation, and keep raw captures out of git. After the implementation report contains the distilled results, remove the private directory with `rm -rf -- "$capture"`.

## Symbols from a replaced binary

An atomic reinstall leaves every long-lived process executing a deleted inode: `/proc/<pid>/exe` readlinks to `.../rimz (deleted)`, and `perf` resolves no RimZ symbols because the on-disk binary no longer matches. Recover the symbols from the inode itself:

```sh
cp /proc/<pid>/exe /tmp/prof/rimz-old
mkdir -p /tmp/prof/symfs/home/<user>/.cargo/bin
cp /tmp/prof/rimz-old "/tmp/prof/symfs/home/<user>/.cargo/bin/rimz (deleted)"
perf report --symfs /tmp/prof/symfs --stdio | rustfilt
```

The symfs tree mirrors the original install path and the filename keeps the literal ` (deleted)` suffix, because that string is the map name `perf` recorded; `perf buildid-cache -a` alone does not resolve it. Pipe reports through `rustfilt` (`cargo install rustfilt`) whenever v0-mangled `_R…` names survive `perf`'s own demangler.

## Read the runtime state

The producer's published caches and the diagnostics log answer many questions before any profiler attaches:

- `$XDG_RUNTIME_DIR/rimz/ws_<id>/pr-state.json` — a repo entry with `"ok": false` and an advancing `refreshed_at_ms` means the PR probe is climbing its failure backoff; `repos` shows per-repo stamps, `states` shows which per-path results survived, and terminal `merged` states are pinned rather than re-probed.
- `$XDG_RUNTIME_DIR/rimz/ws_<id>/diff-stats.json` — the root count is the git sweep's input size, and the per-root stamps show the hot/idle tiering in effect.
- `~/.local/state/rimz/shared/credits.json` — cache-level `refreshed_at_ms` plus per-provider `observed_at_ms` show the account-usage cadence; a provider stuck at `"ok": false` is a failing account-usage probe on the short retry TTL.
- `~/.local/state/rimz/shared/pricing-cache.json` — `fetched_at_secs` and `last_attempt_secs` show the weekly baseline refresh; `unknown_attempt_secs`, `unknown_backoff_secs`, and `unknown_seen` show the unpriced-model chase.
- `~/.local/state/rimz/workspaces/ws_<id>/diag.log.jsonl` — `tick_budget_breach` records separate `mux_wait_ms` from in-process wall time, and observer anomalies name render-side flaps ([diagnostics.md](./diagnostics.md)).
- `rimz sidebar snapshot --json` — the live folded view: agents, panes, roots, spend, and the session shape numbers a capture log entry needs.

## What to look at, in order

The first-look checklist, ranked by what has paid off:

1. **Fork/exec rate and outcome.** A steady per-second exec rate at idle is a retry loop or a cache that never goes fresh; failures matter as much as rate (a deterministic forge-CLI error, an `ENOENT` self-exec). `strace -e trace=execve` plus the runtime caches above localize it in minutes; cross-check cache stamps to name the lane (forge PR state per repo, account usage per provider) and confirm terminal PR states are not re-probing.
2. **Refold rate versus event rate.** Compare event-log events/s against per-renderer fold rate: every renderer folding at the writer's full event rate multiplies one room's cost by its tab count. Only the watched tab and the producer need full rate.
3. **Producer versus consumer split.** Sum thread-level CPU per process across the workspace. Producer-heavy cost points at the external-read lanes (panes, git, PR, spend); consumer-heavy cost points at fold/render work multiplied by renderer count.
4. **Allocation share.** `malloc`/`memmove`/clone frames above ~10–15% of samples in fold/enrich paths mean deep clones on a hot path; the parse-cache `Arc` returns came from exactly this signal.
5. **Mux server cost.** Attribute the multiplexer's own children and RSS separately — Zellij's server-side `ps` storm per `list-panes` and its scrollback footprint are upstream costs RimZ bounds but does not own ([performance.md → Bottlenecks](./performance.md#bottlenecks-and-deferred-work)).

## Turn a finding into a guard

A wall-clock profile proves a fix once; a deterministic guard keeps it fixed. Pin subprocess counts with `proc::testkit::spawn_count` and the `git-trace` shim tests, syscall and size budgets with the fsync/byte counter gates, cache behavior with parse-cache and TTL-stamp tests, and wall-clock/allocation medians with `cargo xtask perf` ([performance.md → Measuring](./performance.md#measuring)). A capture's conclusions flow back into [performance.md](./performance.md): a fixed cost updates the cost map, a removed mistake joins the anti-patterns list, and a real deferred win gets ranked in bottlenecks — the capture log below records each dated capture's finding and where its fix landed.

## Capture log

Dated live captures, newest last. Each entry names the session shape, the finding, and where the fix landed; per the rule above, raw machine-specific numbers — absolute core counts, syscall totals, build ids, workspace ids — stay in that change's implementation report, not here. The July 5 entry is the release baseline: reproduce it with the procedure above and compare before each release.

**July 5, 2026 — release baseline.** Live Zellij room, two active workspaces, ~75 agents across ~29 tabs, on a dogfood build — so absolute CPU and RSS read as shape while the `/proc` scheduler-truth CPU holds. Summing `utime+stime` deltas put the elected producer at ~0.06 core, every consumer at ~0 under the unwatched-fold clamp, and the whole two-room fleet at ~0.11 core, inside the `<0.3 core` busy target. `strace` showed the producer forking ~3.3 `git`/s for the hot diff-stats tier plus one PATH-probed `zellij list-panes`, and the window recorded temp-plus-rename cache writes with zero `fsync`/`fdatasync`. Published caches were healthy: `pr-state.json` ok with no consecutive failures, `diff-stats.json` on its hot/idle tiering, `credits.json` self-healing on expired OAuth, `pricing-cache.json` on its weekly cadence, and `spending.json` on the intentional 365-day window. Every optimization the earlier captures landed — the unwatched-fold clamp, PR failure backoff with last-known-good retention, `Arc` parse-cache returns, the 365-day spending-window eviction, `disable_session_metadata`, and the deleted-binary symbol resolver — is confirmed working in live data. No code change was warranted, so this is the baseline to re-measure against.

**July 13, 2026 — Sentry fold regression (tmux room).** With Sentry configured, a read-only fold peaked near 200 MiB RSS against ~18 MiB with it disabled: every renderer deterministically re-emitted subagent-lifecycle `warn!` events from persisted projection state, and Sentry's `attach_stacktrace` symbolized each backtrace before the `before_send` fingerprint limiter could drop it. Fix: demote those projection diagnostics in `store/snapshot/view/aggregate/subagents.rs`, guarded by a new xtask invariant keeping `warn!` and `error!` out of production `store/snapshot` code. The same capture found an immediate `PaneFramePublished` fold leaving a clamp-deferred store delta armed — a redundant second fold up to a second later — fixed by making every immediate fetch in `sidebar_pane/app/loop_state.rs` absorb deferred work, and `rimz agents exec` wrappers polling at 25 ms, now blocked on OS-driven child and signal wakeups.

**July 13, 2026 — consumer metadata-IO reduction (tmux room).** Unwatched consumers were repeating full heartbeat, mark, sidecar, worktree, and configuration reads on every scan. Fix: share one renderer-local election tracker, narrow and lazily construct consumer input stamps, use an unpublished attempt stamp for tmux presence, and skip the external probe when that attempt stamp cannot be written. A representative consumer's per-scan file operations fell by roughly half with the five-second failover and one-second presence bounds unchanged; pausing the producer aged its heartbeat out and promoted the next eldest within seconds, then resuming the elder demoted the younger back — confirming failover. Retained per-consumer private memory is unchanged and deferred to a separate allocation investigation.

**July 15, 2026 — Codex rollout-scan reduction (Zellij room).** Each Codex worktree independently traversed the provider-global `~/.codex/sessions` rollout tree. Fix, pinned by the multi-workspace Codex test that admits the requested roots and excludes an unrequested one from a bounded candidate pass: over matched scheduler-truth windows, tracked-worker CPU fell ~73%, a common consumer's syscalls ~85%, and its rollout-tree path operations ~86%, despite a slightly larger feature-side input set; a `perf stat` window corroborated the scheduler result. Publication freshness stayed live; the monotonic cadence boundary, failed-attempt throttle, forced bypass, and post-publication consumer memo are pinned by focused state-machine tests rather than inferred from traffic-dependent counts. Allocator-retained arenas are unchanged and remain separate follow-up work.

**July 15, 2026 — duplicated sidebar cost follow-up (mixed Zellij/tmux fleet).** Consumer call graphs still showed account-global spending aggregation and provider-local session discovery, while cache-refresh exec traces showed Claude preflight plus authoritative pane listings on every healthy daemon-maintenance cadence. Fix: consumers read only published spending aggregates, the spending walker retains dedup winner locations instead of cloned entries, the room producer publishes one exact-input local-session batch, and a long-lived daemon tracker classifies fresh frames before authoritative repair. Deterministic guards pin compact memo size and generation invalidation, published-only consumer inputs and exact observation binding, one adapter call per kind with parse-cache reuse and write-before-wake semantics, and zero-child stable daemon maintenance with failed-repair retry. The qualitative result is that the three measured costs move off consumers or disappear from healthy steady state while topology mismatches, producer failover, cache races, and daemon damage retain explicit recovery paths.

**July 16, 2026 — hidden-consumer fold amplification (mixed live fleet).** Background metrics writes used the same unit pane-frame wakeup as topology and presence, so every renderer folded at the metrics rate; consumer diff and PR projection independently reopened checkout metadata and repeated provider/runtime sidecar reads, while accepted fetches cloned full previous and incoming snapshots around allocation-heavy enrichment. Ordinary consumers were individually small but multiplied across the fleet, and the few producers retained much larger PSS high-water marks, so fresh-worker proportional memory remains the comparison that matters. Fix: pane publications carry topology/metrics/presence intent with one-second topology and three-second metrics coalescing for hidden consumers, earliest-deadline merging preserves stronger work, the producer publishes exact channel marker classifications for both git projections, adapter wiring is read once per fold, and accepted snapshots transfer ownership while diagnostics retain only the pane stamp. Presence, watched tabs, producers, focus resume, mixed-build topology fallback, cache upgrade, and rejection/failure gates keep their immediate or last-known-good behavior under deterministic protocol, scheduling, projection, and ownership tests.

**July 16, 2026 — role-triggered retained memory.** Fixed-cadence PSS stayed flat but stepped up when workspace producers first walked the account-global spending cursor and when cell renderers decoded a pet sheet; demoted producers retained their walker and cell pets retained RGBA after deriving terminal grids. Fix: one schema- and persistent/discovery-namespace-versioned service owns the warm walker across workspace election churn, while cell loaders publish only prepared grids and drop decoded RGBA. Only host-eligible long-lived callers take the lifetime lock; fresh concurrent requests bypass the walker and stale contention returns immediately. Durable spending publications, the mixed-build lock, direct one-shot fallback, pixel assets, and the exact eight-day raw/365-day rolled model remain unchanged. The qualitative verdict is duplicate-owner removal rather than allocator tuning: any free pages left in the sole service owner are bounded high-water unless repeated cycles resume growth.

**July 18, 2026 — spending-service socket write amplification.** An elected spending-service owner under a held `rimz stats --refresh --hold` streamed large response frames as tiny Unix-socket writes, making `sendto` dominate the owner's syscall count; small request frames followed the same fragmented path. Fix: `agents/spending/service.rs` buffers every `write_json_line` call through one fixed-capacity writer, with a deterministic write-count guard that preserves the exact newline-delimited JSON wire.

**July 18, 2026 — shared hidden-consumer enrichment.** A 94-agent Zellij room with 24 renderer workers put most room CPU and metadata IO in 22 hidden consumers repeating the same sidecar-heavy enrichment. Fix: the producer publishes one source-validated workspace projection, consumers apply only renderer-local exclusion/view/presence, and an adopted consumer carries a slim unchanged stamp while every mismatch retains the full fold. A disposable tmux room loaded from the same durable fleet state showed adoption-only hidden intervals after the projection settled; the optimized hidden-worker aggregate met the busy-room target and a traced hidden worker cut metadata calls substantially, while the parse-cached and changed-file benches pin the remaining clone/JSON costs. Killing consecutive producers and deleting the projection kept the event-fresh snapshot readable; each next eldest was elected and republished the cache. Fold-cause diagnostics now make adoption, fallback, memo skip, and trigger mix direct evidence instead of inference.

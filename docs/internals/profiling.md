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

A wall-clock profile proves a fix once; a deterministic guard keeps it fixed. Pin subprocess counts with `proc::testkit::spawn_count` and the `git-trace` shim tests, syscall and size budgets with the fsync/byte counter gates, cache behavior with parse-cache and TTL-stamp tests, and wall-clock/allocation medians with `cargo xtask perf` ([performance.md → Measuring](./performance.md#measuring)). A capture's conclusions flow back into [performance.md](./performance.md): a fixed cost updates the cost map, a removed mistake joins the anti-patterns list, and a real deferred win gets ranked in bottlenecks — the capture log below keeps the raw observations.

## Capture log

Dated live captures, newest last. Each entry names the session shape, the build, the findings, and where the fixes landed. The July 5 entry is the release baseline — reproduce it with the procedure above and compare before each release.

**July 5, 2026 — final pre-release baseline on live `rimz-rimz-f89e49`, Zellij, two active workspaces (`ws_f89e49` + `ws_79263b`), ~75 agents / ~29 tabs.** The running renderers were a dogfood build (diag build id `b4c50f3fa7a0`, differing from worktree HEAD), so absolute CPU and RSS are treated as shape while the `/proc` scheduler-truth CPU holds: summing `utime+stime` deltas over a 5s window put the elected producer (the 146–162 MiB renderer) at ~0.064 core, every consumer at ~0 with the unwatched-fold clamp holding, and the whole two-room fleet at ~0.11 core — inside the `<0.3 core` busy target. `strace -f -e trace=execve` showed the producer forking ~3.3 `/usr/bin/git`/s for the hot diff-stats tier plus one PATH-probed `zellij list-panes`, with ~2 `rimz agents refresh-usage` helpers per minute (only `opencode`/`pi`, then throttled by the legacy per-kind marker; `claude`/`codex` were covered by live realtime sessions); `strace -f -c` saw 26 temp-plus-rename cache writes and zero `fsync`/`fdatasync` over the window. The published caches were healthy: `pr-state.json` `ok:true` with zero consecutive failures, `diff-stats.json` at 73 roots on the hot/idle tiering (~20h median idle age), `credits.json` with `opencode`/`pi` at `ok:false` on the benign 60s retry (expired OAuth token / no ChatGPT quota surface, self-healing on re-auth), `pricing-cache.json` on its weekly cadence, and `spending.json` at 8 MiB / 5208 file records — the intentional 365-day window, with `cold_parse_out_of_window` pruning only files past 367 days. Over a 27.5h diag window the only anomaly volume was Zellij `list-panes` presence flap absorbed by carry-forward (`short_lived_row` 155, `pane_count_drop` 94, `pane_carry_forward` 111 — [bottleneck #1](./performance.md#bottlenecks-and-deferred-work), no user-visible flicker) plus dogfood-only `mixed_build_writers` (272) and `producer_demoted` (23) that fall to ~0 in a stable single-build release. Every optimization the earlier captures landed — the unwatched-fold clamp, PR failure backoff with last-known-good retention, `Arc` parse-cache returns, the 365-day spending-window eviction, `disable_session_metadata`, and the `rimz_exe` deleted-binary resolver — is confirmed working in live data; no code change was warranted, so this is the release baseline to re-measure against.

**July 13, 2026 — Sentry regression capture on a live tmux room, 10 sidebar supervisors + 10 workers.** The installed binary held each supervisor near 2 MiB private while workers held 194–393 MiB each, ~2.35 GiB private in total. A controlled read-only fold peaked at 200,208 KiB RSS in 0.78 s with Sentry configured, versus 18,424 KiB in ~0 s with the Sentry config disabled. The fold deterministically repeated subagent lifecycle `warn!` events from persisted projection state in every renderer; Sentry's `attach_stacktrace` captured and symbolized each backtrace before the `before_send` fingerprint limiter could drop it. The fix demotes those projection diagnostics in `store/snapshot/view/aggregate/subagents.rs` and adds an xtask invariant keeping `warn!` and `error!` out of production `store/snapshot` code. A typical unwatched worker made 9,062 syscalls in five seconds: 4,199 `statx`, 1,980 reads, and 1,489 opens. Inspection found that an immediate `PaneFramePublished` fold left an already clamp-deferred store delta armed, causing a redundant second fold up to one second later; the fix makes every immediate fetch in `sidebar_pane/app/loop_state.rs` absorb deferred work. The same capture found 24 `rimz agents exec` wrappers polling at 25 ms for ~1% aggregate CPU; wrappers and daemon content supervisors now block on OS-driven child and signal wakeups instead.

**July 13, 2026 — repeated sidebar fleet scans on live tmux workspace `ws_f89e49906df0621ad2765112`.** Before the fix, build `f51127663518` ran 18 supervisors, 18 workers, and 18 fresh heartbeats; a 20-second scheduler-truth sample put the workers at ~0.34 core, with the producer at ~10.3% of one core, ordinary consumers at ~1.2–1.3% each, and tmux at another ~5%. Eight-second `strace -f -c` windows counted 253,819 producer syscalls and ~20–26k per consumer, with repeated full heartbeat, mark, sidecar, worktree, and configuration reads. The landed fix shares one renderer-local election tracker, narrows and lazily constructs consumer input stamps, and uses an unpublished attempt stamp for tmux presence. After install and reload, all heartbeats and `/proc/<worker>/exe` converged to measured build `5e789ef43ce9`; the room grew from 9 to 11 supervisor/worker pairs before the fixed post window, whose worker fleet used 0.2055 core, the producer 8.65%, established consumers 1.00–1.05% each, one newly started worker 2.60%, and tmux 6.05%. The representative consumer fell to 13,253 syscalls over eight seconds (6,594 `statx`, 2,718 reads, and 2,189 opens), while the producer stayed flat at 255,363 because its external git/mux lanes dominate that process. A ten-second consumer file trace read only its one cached elder twice at heartbeat expiry and performed no full heartbeat-directory open; a 15-second exec trace observed 14 actual `tmux list-clients` attempts with no within-second burst. Pausing the producer aged its heartbeat out and raised the next eldest from 0.7% consumer CPU to 3.75% producer CPU; resuming the elder restamped its heartbeat and demoted the younger to 0.75% within three seconds. A final guard that skips the external probe when its attempt stamp cannot be written produced build `9a6d1d8ac9ac`; a second reload converged all 13 then-current workers and heartbeats to that build without changing the measured common path. The fleet is below the `<0.3 core` busy target and consumer metadata IO fell materially without changing the five-second failover or one-second presence bounds. The earlier retained-private-memory observation — about 63 MiB per consumer and 223 MiB for the producer in that capture — remains deferred for a separate allocation investigation; this change claims no RSS improvement.

**July 15, 2026 — sidebar refresh and loop-watch follow-up on live Zellij workspace `ws_f89e49906df0621ad2765112`.** Installed build `6d4a167e` supplied discovery evidence only: its then-16-worker room used about 0.44 core over 20 seconds, with about 330,000 producer and 19,500 consumer syscalls in ten seconds. That build predates the feature branch base and is non-comparable. A first `7ebf531d` feature-build sample is also non-comparable: its 16 workers used 2.928 cores over 20 seconds, its consumer and producer made 455,033 and 1,630,907 syscalls in ten seconds, and a two-second file trace saw 15,246 paths below `~/.codex/sessions`; this exposed that each Codex worktree independently traversed the provider-global rollout tree, but supports no before/after CPU, syscall, or memory claim.

The paired run installed exact base `22fdbc23` (ELF build `09ff626b9920`) and completed feature `f1414a95` (ELF build `891a28a80cc3`), reloaded the fleet, and verified every measured heartbeat build before each window. The base stabilized with 19 worker instances; the same 19 instances were tracked after the feature reload, although one additional pane and worker appeared before the feature window, so the feature carried a slightly larger input set and the topology pairing is conservative rather than exact. Over matched 20-second scheduler-truth windows, those 19 workers fell from 6.5971 to 1.7718 cores (73.1%); the elected producer fell from 1.0863 to 0.2605 core, and the same mature consumer fell from 0.2665 to 0.1530 core. Parallel 12-second `strace -f -c` windows fell from 700,975 to 106,888 syscalls for that consumer (84.8%) and from 2,011,170 to 1,100,112 for the producer (45.3%). Three-second consumer file traces fell from 26,148 to 3,624 path operations below active `~/.codex/sessions` (86.1%) and from 48 to 6 below the archive. Matching `perf stat` windows corroborated the scheduler result: consumer task-clock fell from 2,494.64 to 585.54 ms and producer task-clock from 12,610.54 to 3,838.65 ms over ten seconds. The observation is lower CPU and filesystem work despite the extra feature-side pane; the code-level cause is separately pinned by the multi-workspace Codex test, which admits two requested roots and excludes an unrequested root from one bounded candidate pass.

Twenty-second polling observed 21 base `snapshot.json` replacements with two `produced_at_ms` changes, versus 41 replacements, 37 production changes, and 12 observation changes during the busier feature window. Those counts prove publication freshness remained live but do not isolate normal attempts from forced and structural requests; the monotonic cadence boundary, failed-attempt throttle, forced bypass, and post-publication consumer memo are pinned by focused state-machine tests rather than inferred from this traffic-dependent count. A live base `loop watch` ran each of the three workspace Git probes on all 16 observed repaints, while the feature ran each probe once at startup across six repaints; its PTY integration guard also edits the task file after startup and observes the new task on the next frame.

Allocator retention remains material and does not justify an inline allocator workaround. Across the same 19 tracked instances, `/proc` RSS moved from 2,425,588 to 2,197,536 KiB and private memory from 2,134,544 to 1,898,180 KiB, but process age and the extra feature-side pane make those host totals descriptive rather than causal. Read-only `mallinfo2()` found producer free arena space of 201,839,984 bytes at base and 209,013,936 after the feature, while the mature consumer held 100,245,424 and 100,934,752 bytes; their RSS also stayed roughly flat at 300–307 MiB and 117–119 MiB. The repeat-work fixes materially reduce CPU and filesystem churn without removing allocator-retained arenas, so allocator policy remains separate follow-up work.

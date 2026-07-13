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

Verify which build is actually running before trusting absolute numbers. Frames like `core::ub_checks::*` or `precondition_check` in a profile are the tell for a debug build (opt-level 0 with debug assertions): absolute CPU overstates release by roughly 5×, while ratios and rates — forks/s, folds/s, the producer/consumer split — stay valid. The July 5 baseline ran on exactly such a build; its ratios held, its absolute figures did not.

## Attach to the right process

- **Count processes with a `/proc` cmdline scan.** `ps` truncates long argument lists and undercounts: a `ps -eo args | grep -c` pass reported 3 renderers in a room where the `/proc` scan found 59.

  ```sh
  for f in /proc/[0-9]*/cmdline; do tr '\0' ' ' < "$f" | grep -q 'rimz sidebar serve' && echo "${f%/cmdline}"; done
  ```

- **Heartbeats count live renderers.** One file per live renderer under `$XDG_RUNTIME_DIR/rimz/ws_<id>/heartbeats/`, one renderer per tab — a large count in a many-tab room is the design working, and a leak hypothesis needs the cmdline scan above to confirm each heartbeat's process is dead before it holds.
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

**July 5, 2026 — final pre-release baseline on live `rimz-rimz-f89e49`, Zellij, two active workspaces (`ws_f89e49` + `ws_79263b`), ~75 agents / ~29 tabs.** The running renderers were a dogfood build (diag build id `b4c50f3fa7a0`, differing from worktree HEAD), so absolute CPU and RSS are treated as shape while the `/proc` scheduler-truth CPU holds: summing `utime+stime` deltas over a 5s window put the elected producer (the 146–162 MiB renderer) at ~0.064 core, every consumer at ~0 with the unwatched-fold clamp holding, and the whole two-room fleet at ~0.11 core — inside the `<0.3 core` busy target. `strace -f -e trace=execve` showed the producer forking ~3.3 `/usr/bin/git`/s for the hot diff-stats tier plus one PATH-probed `zellij list-panes`, with ~2 `rimz agents refresh-usage` helpers per minute (only `opencode`/`pi`, throttled to the 60s `usage-probe.{kind}` marker; `claude`/`codex` were covered by live realtime sessions); `strace -f -c` saw 26 temp-plus-rename cache writes and zero `fsync`/`fdatasync` over the window. The published caches were healthy: `pr-state.json` `ok:true` with zero consecutive failures, `diff-stats.json` at 73 roots on the hot/idle tiering (~20h median idle age), `credits.json` with `opencode`/`pi` at `ok:false` on the benign 60s retry (expired OAuth token / no ChatGPT quota surface, self-healing on re-auth), `pricing-cache.json` on its weekly cadence, and `spending.json` at 8 MiB / 5208 file records — the intentional 365-day window, with `cold_parse_out_of_window` pruning only files past 367 days. Over a 27.5h diag window the only anomaly volume was Zellij `list-panes` presence flap absorbed by carry-forward (`short_lived_row` 155, `pane_count_drop` 94, `pane_carry_forward` 111 — [bottleneck #1](./performance.md#bottlenecks-and-deferred-work), no user-visible flicker) plus dogfood-only `mixed_build_writers` (272) and `producer_demoted` (23) that fall to ~0 in a stable single-build release. Every optimization the earlier captures landed — the unwatched-fold clamp, PR failure backoff with last-known-good retention, `Arc` parse-cache returns, the 365-day spending-window eviction, `disable_session_metadata`, and the `rimz_exe` deleted-binary resolver — is confirmed working in live data; no code change was warranted, so this is the release baseline to re-measure against.

**July 13, 2026 — Sentry regression capture on a live tmux room, 10 sidebar supervisors + 10 workers.** The installed binary held each supervisor near 2 MiB private while workers held 194–393 MiB each, ~2.35 GiB private in total. A controlled read-only fold peaked at 200,208 KiB RSS in 0.78 s with Sentry configured, versus 18,424 KiB in ~0 s with the Sentry config disabled. The fold deterministically repeated subagent lifecycle `warn!` events from persisted projection state in every renderer; Sentry's `attach_stacktrace` captured and symbolized each backtrace before the `before_send` fingerprint limiter could drop it. The fix demotes those projection diagnostics in `store/snapshot/view/aggregate/subagents.rs` and adds an xtask invariant keeping `warn!` and `error!` out of production `store/snapshot` code. The same capture found 24 `rimz agents exec` wrappers polling at 25 ms for ~1% aggregate CPU; ptrace policy blocked the syscall trace needed to justify that separate follow-up, so wrapper polling remains deferred.

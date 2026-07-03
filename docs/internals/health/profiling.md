# Profiling

> The field guide for profiling a live Rimz fleet: pick the tool by the question, attach to the right process, recover symbols, read the runtime state, and turn what you find into a deterministic guard. The performance model, cost map, and budgets this guide measures against live in [performance.md](./performance.md); every dated live capture lands in the [capture log](#capture-log) here.

## Pick the tool by the question

Start with `strace -f -c -p <pid>` when a process is busy for unclear reasons: it shows syscall shape, fork/exec storms, lock contention, and IO without rebuilding or restarting a live process. Every capture so far started there, and the biggest findings — the git fork storm, the PR-probe retry loop, the deleted-binary exec failures — were visible in syscall counts before any CPU profile ran; `perf` confirmed shapes, it did not discover them.

- **Fork/exec rate and outcome** — `strace -f -e trace=execve -p <pid>`: which binaries, how often, and whether they succeed. A repeating `ENOENT` on a Rimz path is a stale self-exec after a reinstall ([symbols from a replaced binary](#symbols-from-a-replaced-binary) explains the inode state behind it).
- **CPU by function** — `samply record -p <pid>` or `perf record --call-graph fp -p <pid> -- sleep 10` against the profiling build. `samply` opens a Firefox-Profiler call tree that reads Rust stacks well; `perf report --stdio` gives flat self-time and callees.
- **CPU by process and thread** — `pidstat -h -u -t -p <pid> 1 5`, or raw `/proc/<pid>/stat` deltas ([attach to the right process](#attach-to-the-right-process) lists the pitfalls).
- **RSS and allocation churn** — `/usr/bin/time -v` for peak RSS, `heaptrack` or DHAT when an allocation regression needs ownership, and the `malloc`/`memmove`/`clone` share of a `perf` profile as the first-pass churn signal.
- **IO** — `/proc/<pid>/io` read/write byte deltas over a fixed window. The hot runtime caches live in `$XDG_RUNTIME_DIR` (tmpfs), so their churn is memory traffic; sustained disk bytes point at durable state or transcripts.

Host settings shape the tools more than Rimz: `perf` needs `sudo` or a lower `kernel.perf_event_paranoid` when the value is 3 or higher, hardware LBR call graphs may be unavailable under virtualization (use `--call-graph fp`), and `strace` windows stay short — a few seconds — because syscall tracing perturbs the process it observes.

For a reproducible command-shaped profile without a live room, run the profiling binary directly against the path under test — `samply record target/profiling/rimz sidebar snapshot --json`, or `--no-produce` over a synthetic seeded workspace when the question is read-only process shape. `samply record -- cargo xtask perf` and `/usr/bin/time -v cargo xtask perf` cover the microbench tier.

## Build and verify a profilable binary

```sh
cargo xtask profile-build
target/profiling/rimz --version
```

`cargo xtask profile-build` writes `target/profiling/rimz`: optimized like release, with line tables, frame pointers, and v0 symbol mangling for demangled call trees. `cargo xtask install-dev` installs the same profiling profile with the dev-only `sentry` feature, so dogfooding sessions are optimized and directly profilable; the shipped release binary keeps no frame-pointer or debug-info cost.

Verify which build is actually running before trusting absolute numbers. Frames like `core::ub_checks::*` or `precondition_check` in a profile are the tell for a debug build (opt-level 0 with debug assertions): absolute CPU overstates release by roughly 5×, while ratios and rates — forks/s, folds/s, the producer/consumer split — stay valid. The July 3 capture ran on exactly such a build; its findings held, its absolute figures did not.

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

An atomic reinstall leaves every long-lived process executing a deleted inode: `/proc/<pid>/exe` readlinks to `.../rimz (deleted)`, and `perf` resolves no Rimz symbols because the on-disk binary no longer matches. Recover the symbols from the inode itself:

```sh
cp /proc/<pid>/exe /tmp/prof/rimz-old
mkdir -p /tmp/prof/symfs/home/<user>/.cargo/bin
cp /tmp/prof/rimz-old "/tmp/prof/symfs/home/<user>/.cargo/bin/rimz (deleted)"
perf report --symfs /tmp/prof/symfs --stdio | rustfilt
```

The symfs tree mirrors the original install path and the filename keeps the literal ` (deleted)` suffix, because that string is the map name `perf` recorded; `perf buildid-cache -a` alone does not resolve it. Pipe reports through `rustfilt` (`cargo install rustfilt`) whenever v0-mangled `_R…` names survive `perf`'s own demangler.

## Read the runtime state

The producer's published caches and the diagnostics log answer many questions before any profiler attaches:

- `$XDG_RUNTIME_DIR/rimz/ws_<id>/pr-state.json` — `"ok": false` with a `refreshed_at_ms` advancing on the short cadence means the PR probe is climbing its failure backoff; `states` shows which per-path results survived.
- `$XDG_RUNTIME_DIR/rimz/ws_<id>/diff-stats.json` — the root count is the git sweep's input size, and the per-root stamps show the hot/idle tiering in effect.
- `~/.local/state/rimz/workspaces/ws_<id>/diag.log.jsonl` — `tick_budget_breach` records separate `mux_wait_ms` from in-process wall time, and observer anomalies name render-side flaps ([diagnostics.md](./diagnostics.md)).
- `rimz sidebar snapshot --json` — the live folded view: agents, panes, roots, spend, and the session shape numbers a capture log entry needs.

## What to look at, in order

The first-look checklist, ranked by what has paid off:

1. **Fork/exec rate and outcome.** A steady per-second exec rate at idle is a retry loop or a cache that never goes fresh; failures matter as much as rate (a deterministic forge-CLI error, an `ENOENT` self-exec). `strace -e trace=execve` plus the runtime caches above localize it in minutes.
2. **Refold rate versus event rate.** Compare ledger events/s against per-renderer fold rate: every renderer folding at the writer's full event rate multiplies one room's cost by its tab count. Only the watched tab and the producer need full rate.
3. **Producer versus consumer split.** Sum thread-level CPU per process across the workspace. Producer-heavy cost points at the external-read lanes (panes, git, PR, spend); consumer-heavy cost points at fold/render work multiplied by renderer count.
4. **Allocation share.** `malloc`/`memmove`/clone frames above ~10–15% of samples in fold/enrich paths mean deep clones on a hot path; the parse-cache `Arc` returns came from exactly this signal.
5. **Mux server cost.** Attribute the multiplexer's own children and RSS separately — Zellij's server-side `ps` storm per `list-panes` and its scrollback footprint are upstream costs Rimz bounds but does not own ([performance.md → Bottlenecks](./performance.md#bottlenecks-and-deferred-work)).

## Turn a finding into a guard

A wall-clock profile proves a fix once; a deterministic guard keeps it fixed. Pin subprocess counts with `proc::testkit::spawn_count` and the `git-trace` shim tests, syscall and size budgets with the fsync/byte counter gates, cache behavior with parse-cache and TTL-stamp tests, and wall-clock/allocation medians with `cargo xtask perf` ([performance.md → Measuring](./performance.md#measuring)). A capture's conclusions flow back into [performance.md](./performance.md): a fixed cost updates the cost map, a removed mistake joins the anti-patterns list, and a real deferred win gets ranked in bottlenecks — the capture log below keeps the raw observations.

## Capture log

Dated live captures, newest last. Each entry names the session shape, the build, the findings, and where the fixes landed.

**July 1, 2026 — synthetic 100-agent workspace.** 200 read-only `rimz sidebar snapshot --json --no-produce` runs completed in 0.79s wall with 18MiB maximum RSS and one process `execve` per invocation; an attached `rimz sidebar serve` fixture idled at 0-1% CPU with 91MiB RSS, and attached syscall traces saw zero `clone`, `execve`, `fsync`, or `fdatasync` calls during idle and a 20-hook burst.

**July 1, 2026 — git-probe fold retry, 20-worktree seeded fixture with real pane `cwd`s.** A cold producer snapshot dropped from 2400 git `execve` attempts / 240 successful `/usr/bin/git` execs at `47af1718` to 180 attempts / 180 successful execs after `3f419d87`, while the `sidebar_diff_stats` performance guard stayed green. `cargo xtask profile-build` plus `perf report --call-graph fp` produced demangled stacks for `produce_snapshot`, `enrich`, spending, and rollup fold ownership; the observed CPU sat in producer work and git children, never a render-thread fork/fsync path.

**July 2, 2026 — live session `rimz-rimz-f89e49`, Zellij 0.44.3, 22 tabs / 101 panes.** Baseline `zellij action list-panes -j -a` cost 1.5–2.0s, degrading to 8–18s under host load, with the Zellij server forking roughly nine `ps -ao ppid,args` children per call. About twenty consumer renderers each refolded once per second while the ledger advanced at roughly 0.2 events/s, costing about 0.9 core combined; the producer sat around 15–17% CPU with git diff-stats around 7.6 forks/s; Zellij's session metadata loop rewrote `session-metadata.kdl` every few seconds and ran its own command-discovery `ps` loop. The matching `tick_budget_breach` records came from mux wait dominating tick wall time, so the meter now reports mux wait separately and treats the 1.5–2.0s steady Zellij floor differently from degraded 8–18s tails. Fixes landed as the consumer unchanged-stamp, `disable_session_metadata`, and the separated mux-wait budget.

**July 3, 2026 — same workspace, grown to 147 panes / 29 tabs / 75 agents / 43 repo worktrees / 193 diff-stat roots.** The running binary was a debug dev install, so absolute CPU overstated release by roughly 5×; ratios still exposed the bugs. A deterministic `tea pr list` failure kept PR-state refresh on the 30s retry loop across every worktree — roughly 2.8 `tea` and 9 `git` forks/s forever, with no command deadline and failures wiping the last known PR map. Active ledger traffic around 15 deltas/s woke every renderer, so 28 unwatched consumers refolded at full rate and spent about 3.5 debug cores combined, about 0.7 release-equivalent cores. Reinstalling `rimz` left long-lived processes resolving `current_exe()` to `rimz (deleted)`, and self-spawn helpers failed `execve` until restart. Allocation churn — `malloc`/`memmove` around 16% of producer samples — confirmed the parse-cache deep-clone item. Fixes landed as the PR failure backoff with last-known-good retention and bounded probes, `UNWATCHED_FOLD_CLAMP`, the shared `rimz_exe` resolver, `Arc` parse-cache returns, and the profiling-profile `install-dev`.

**July 3, 2026 — follow-up on `rimz-rimz-f89e49`, Zellij, 147 panes / ~29 tabs / 75 agents / 48 worktrees / ~200 diff-stat roots.** The room was running a mixed debug/profiling dogfood build, so absolute resident memory and CPU were treated as shape only; the PR probe confirmed the `336c8ed3` fix by self-healing to `ok:true` after the producer backoff expired and ran `tea pr list --repo rimio/rimz`. With the fleet idle the ledger advanced at roughly 0.1 events/s and renderers sat at 0% CPU, leaving three residual costs: stale sidebar supervisor parents held deleted binary inodes until session end, supervisor-parented stray children could become long-lived zombies, and `binding.log.jsonl` appended repeated identical lazy-pairing diagnostics under resumed cohorts with `pane_process_start: null`. Fixes landed as supervisor-owned reload convergence with stable sidebar instance ids, supervisor-side stray-child reaping, and producer-local lazy-pairing signature dedup.

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

A wall-clock profile proves a fix once; a deterministic guard keeps it fixed. Pin subprocess counts with `proc::testkit::spawn_count` and the `git-trace` shim tests, syscall and size budgets with the fsync/byte counter gates, cache behavior with parse-cache and TTL-stamp tests, and wall-clock/allocation medians with `cargo xtask perf` ([performance.md → Measuring](./performance.md#measuring)). A capture's conclusions flow back into [performance.md](./performance.md): a fixed cost updates the cost map, a removed mistake joins the anti-patterns list, and a real deferred win gets ranked in bottlenecks — the capture log below records each dated capture's finding and where its fix landed.

## Capture log

Dated live captures, newest last. Each entry names the session shape, the finding, and where the fix landed; per the rule above, raw machine-specific numbers — absolute core counts, syscall totals, build ids, workspace ids — stay in that change's implementation report, not here. The July 5 entry is the release baseline: reproduce it with the procedure above and compare before each release.

**July 5, 2026 — release baseline.** Live Zellij room, two active workspaces, ~75 agents across ~29 tabs, on a dogfood build — so absolute CPU and RSS read as shape while the `/proc` scheduler-truth CPU holds. Summing `utime+stime` deltas put the elected producer at ~0.06 core, every consumer at ~0 under the unwatched-fold clamp, and the whole two-room fleet at ~0.11 core, inside the `<0.3 core` busy target. `strace` showed the producer forking ~3.3 `git`/s for the hot diff-stats tier plus one PATH-probed `zellij list-panes`, and the window recorded temp-plus-rename cache writes with zero `fsync`/`fdatasync`. Published caches were healthy: `pr-state.json` ok with no consecutive failures, `diff-stats.json` on its hot/idle tiering, `credits.json` self-healing on expired OAuth, `pricing-cache.json` on its weekly cadence, and `spending.json` on the intentional 365-day window. Every optimization the earlier captures landed — the unwatched-fold clamp, PR failure backoff with last-known-good retention, `Arc` parse-cache returns, the 365-day spending-window eviction, `disable_session_metadata`, and the deleted-binary symbol resolver — is confirmed working in live data. No code change was warranted, so this is the baseline to re-measure against.

**July 13, 2026 — Sentry fold regression (tmux room).** With Sentry configured, a read-only fold peaked near 200 MiB RSS against ~18 MiB with it disabled: every renderer deterministically re-emitted subagent-lifecycle `warn!` events from persisted projection state, and Sentry's `attach_stacktrace` symbolized each backtrace before the `before_send` fingerprint limiter could drop it. Fix: demote those projection diagnostics in `store/snapshot/view/aggregate/subagents.rs`, guarded by a new xtask invariant keeping `warn!` and `error!` out of production `store/snapshot` code. The same capture found an immediate `PaneFramePublished` fold leaving a clamp-deferred store delta armed — a redundant second fold up to a second later — fixed by making every immediate fetch in `sidebar_pane/app/loop_state.rs` absorb deferred work, and `rimz agents exec` wrappers polling at 25 ms, now blocked on OS-driven child and signal wakeups.

**July 13, 2026 — consumer metadata-IO reduction (tmux room).** Unwatched consumers were repeating full heartbeat, mark, sidecar, worktree, and configuration reads on every scan. Fix: share one renderer-local election tracker, narrow and lazily construct consumer input stamps, use an unpublished attempt stamp for tmux presence, and skip the external probe when that attempt stamp cannot be written. A representative consumer's per-scan file operations fell by roughly half with the five-second failover and one-second presence bounds unchanged; pausing the producer aged its heartbeat out and promoted the next eldest within seconds, then resuming the elder demoted the younger back — confirming failover. Retained per-consumer private memory is unchanged and deferred to a separate allocation investigation.

**July 15, 2026 — Codex rollout-scan reduction (Zellij room).** Each Codex worktree independently traversed the provider-global `~/.codex/sessions` rollout tree. Fix, pinned by the multi-workspace Codex test that admits the requested roots and excludes an unrequested one from a bounded candidate pass: over matched scheduler-truth windows, tracked-worker CPU fell ~73%, a common consumer's syscalls ~85%, and its rollout-tree path operations ~86%, despite a slightly larger feature-side input set; a `perf stat` window corroborated the scheduler result. Publication freshness stayed live; the monotonic cadence boundary, failed-attempt throttle, forced bypass, and post-publication consumer memo are pinned by focused state-machine tests rather than inferred from traffic-dependent counts. Allocator-retained arenas are unchanged and remain separate follow-up work.

**July 15, 2026 — duplicated sidebar cost follow-up (mixed Zellij/tmux fleet).** Consumer call graphs still showed account-global spending aggregation and provider-local session discovery, while cache-refresh exec traces showed Claude preflight plus authoritative pane listings on every healthy daemon-maintenance cadence. Fix: consumers read only published spending aggregates, the spending walker retains dedup winner locations instead of cloned entries, the room producer publishes one exact-input local-session batch, and a long-lived daemon tracker classifies fresh frames before authoritative repair. Deterministic guards pin compact memo size and generation invalidation, published-only consumer inputs and exact observation binding, one adapter call per kind with parse-cache reuse and write-before-wake semantics, and zero-child stable daemon maintenance with failed-repair retry. The qualitative result is that the three measured costs move off consumers or disappear from healthy steady state while topology mismatches, producer failover, cache races, and daemon damage retain explicit recovery paths.

**July 16, 2026 — hidden-consumer fold amplification (mixed live fleet).** Background metrics writes used the same unit pane-frame wakeup as topology and presence, so every renderer folded at the metrics rate; consumer diff and PR projection independently reopened checkout metadata, while accepted fetches cloned full previous and incoming snapshots around allocation-heavy enrichment. Fix: pane publications carry topology/metrics/presence intent with one-second topology and three-second metrics coalescing for hidden consumers, earliest-deadline merging preserves stronger work, the producer publishes exact channel marker classifications for both git projections, adapter wiring is read once per fold, and accepted snapshots transfer ownership while diagnostics retain only the pane stamp. Presence, watched tabs, producers, focus resume, mixed-build topology fallback, and rejection/failure gates keep their immediate or last-known-good behavior under focused state-machine and protocol tests.

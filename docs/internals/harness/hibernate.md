# Agent hibernation

Hibernation freezes an idle agent's provider process with the cgroup v2 freezer, then asks the kernel to move its anonymous memory to swap in steps. Every RimZ path that writes to the pane or closes it thaws the agent first, so the provider never acts on a frozen pane. Hibernation runs on Linux only and is on by default. It is enrichment under the root contract's fail-fast rule: a host that lacks a capability starts its room as usual and skips hibernation.

The settings are `[agents.hibernate]` `enabled` (unset reads as on), `after` (default `30m`, the rest time before the freeze), and `deep_after` (default `120m`, the rest time at which reclaim reaches zero). The [configuration guide](../../guide/configuration.md#agent-hibernation) owns them for users.

Live checks: [Hibernate an agent](../../contributing/sidebar-live-check.md#hibernate-an-agent) covers freeze, deepen, and wake in a sandbox room with a stub agent. A check against a real provider's turns needs a host room the user starts ([recipe](../../contributing/sidebar-live-check.md#check-against-a-real-provider-login)).

## Where the code lives

| Path | Owns |
| --- | --- |
| `crates/rimz/src/harness/hibernate.rs` | Admission and doctor checks, the producer pass, `decide`, the runtime caches, record projection, `scope_argv`. It is in the sidebar import graph. |
| `crates/rimz/src/harness/hibernate/execute.rs` | The freeze helper: `run_request`, the freeze and deepen transactions, hold assists. |
| `crates/rimz/src/harness/hibernate/wake.rs` | Synchronous wake, the focus-wake helper and its dwell, lock-timeout recovery, the refault sampler, start reconciliation. |
| `crates/rimz/src/proc/cgroup.rs` | `ScopePath`, `scope_prefix`, freeze, thaw, reclaim, `memory_stat`, workspace scope listing. |
| `crates/rimz/src/config/agents.rs` | `HibernateConfig` and the validated `Schedule`. |
| `crates/rimz/src/store/hibernate.rs` | The durable record `records/hibernation.json`, whose schema [store.md](../store.md) owns. |
| `crates/rimz/src/diag/hibernate.rs` | The wake diagnostics in `audit/hibernate.log.jsonl`. |
| `crates/rimz/src/child_process.rs` | The helper-scope escape in `spawn_detached_reaped` and the weighted drainer scope in `spawn_hook_drainer`. |
| `crates/rimz/src/cli/agents_cmd/hibernate.rs` | The hidden `agents hibernate`, `agents wake`, and `agents wake-refault` helper commands. |

## Rules a change keeps

- Every function that opens a `PaneWriter` wakes the pane earlier in the same function. `cargo xtask invariants` enforces it ([Synchronous wake](#synchronous-wake)).
- One per-workspace lock, `hibernate.lock`, serializes the final freeze decision, the freeze, each reclaim, publication of the record, and wake. Wake bypasses it only after a timeout ([Lock-timeout recovery](#lock-timeout-recovery)).
- The record is published only after a confirmed freeze and a successful first reclaim. A failure between the freeze and publication thaws the scope. A deepen step never recreates an entry that a wake withdrew.
- Hibernation never changes an agent's lifecycle or displayed status ([What a frozen row shows](#what-a-frozen-row-shows)).
- A scope holds only the provider, its descendants, and its in-scope ancestor chain. Any other process vetoes the freeze.
- The producer in `harness/hibernate.rs` writes only runtime caches and spawns helpers. Cgroup writes and durable writes stay in the helper modules, outside the sidebar graph.
- The scopes are resource management, not a security boundary: any process of the same user can write the delegated cgroup files.

## Configuration and admission

`crates/rimz/src/config/agents.rs::HibernateConfig::schedule` validates the two durations at use. Each takes `s`, `m`, `h`, or `d`, may not exceed 24 hours, and `deep_after` may not be less than `after`. An invalid schedule makes admission unavailable with the error and its fix. RimZ never substitutes the default schedule. `rimz start` warns with the error, and `rimz doctor` shows it as an error row, the only error hibernation reports.

Off Linux, the config loader (`crates/rimz/src/config.rs::MachineConfig::disable_unavailable_hibernation`) sets `enabled = false`. It adds a start warning only when `enabled = true` was explicit.

`crates/rimz/src/harness/hibernate.rs::inspect` probes the host's capabilities for `rimz doctor`. Each failed check is a warning row, and the first failure becomes the reason hibernation is skipped:

| Doctor check | Passes when |
| --- | --- |
| `systemd-run` | `systemd-run` is on `PATH`. |
| `user manager` | `systemctl --user show-environment` succeeds within 3 seconds. |
| `memory controller` | The user manager's `app.slice` is writable and lists `memory` in `cgroup.controllers`. |
| `cgroup.freeze` | `app.slice/cgroup.freeze` is writable. |
| `memory.reclaim` | `app.slice/memory.reclaim` is writable (Linux 5.19 or newer). |
| `swap` | `/proc/meminfo` reports a positive `SwapTotal`. |
| `swap limit` | No visible ancestor of `app.slice` has a `memory.swap.max` that reads `0`, is malformed, or cannot be read. A value of `max`, a positive limit, or a missing file passes. |
| `free swap` | Advisory only: a warning when `SwapFree` is 0. It does not skip hibernation. |

The `app.slice` path is `user.slice/user-<uid>.slice/user@<uid>.service/app.slice`, built from the uid. The caller's own cgroup does not matter, so a login shell in `session-N.scope` can use it. A passing `swap limit` does not prove swap is available: a container's cgroup namespace can hide a tighter ancestor limit, and inside a container the virtualized `/proc/meminfo` is the authority on swap.

`crates/rimz/src/harness/hibernate.rs::admit` runs the schedule check and the capability probe, and returns an `Admission { available, reason }` without publishing it. `crates/rimz/src/room/birth.rs::RoomContext::birth` publishes it through `Admission::publish` to the runtime cache `live/hibernate/admission.json`, in normal and supervised rooms alike. It publishes twice: before session creation and resume seeding, and again after the health gate and any recovery reset. GC keeps this file regardless of age ([store.md](../store.md)).

Launch planning, the producer, and the freeze helper each require `enabled` and an `available` cached verdict. An absent or unreadable cache reads as unavailable with the reason `admission unavailable; run rimz start`. A fix to the host, such as adding swap or delegating the memory controller, therefore takes effect at the next room birth or reattach, even though `rimz doctor` probes live and reports it at once. `rimz start` warns about an unavailable verdict only when `enabled = true` is explicit (`crates/rimz/src/room/judgment.rs::hibernate_notice`).

## Scopes

### Provider scopes

The launch compiler (`crates/rimz/src/harness/launch_plan.rs`) wraps a ready provider argv on Linux when admission is available, through `crates/rimz/src/harness/hibernate.rs::scope_argv`. It wraps after the bubblewrap view is applied, so `systemd-run` is the outermost command:

```sh
systemd-run --user --scope --quiet --collect -p MemoryAccounting=yes \
  --unit rimz-agent-<workspace_id>-<uuid>.scope --description "RimZ agent <kind>" -- <argv>
```

Each compilation mints a fresh UUIDv7. `crates/rimz/src/proc/cgroup.rs::scope_prefix` replaces each workspace id character outside `[A-Za-z0-9_-]` with `_`, and the launch compiler, discovery, and reconciliation all use that one prefix. The exec wrapper stays outside the scope, so its supervision and leases keep running while the provider is frozen. A provider launched while admission was unavailable has no scope and holds as `NoScope` until it is relaunched.

Qwen's login-shell reentry (`AgentProcessStage::LoginShellReentry`, for a pending provider-account binding) stays unwrapped. The reentry launches a second exec wrapper, which compiles the finalized provider and wraps that. Wrapping the reentry itself would put an exec wrapper inside an agent scope.

### Helper scopes

A detached helper started from inside an agent scope, such as a hook or a `rimz` command the agent runs, would freeze with the agent and count as a foreign process. `crates/rimz/src/child_process.rs::spawn_detached_reaped` prevents this. When `/proc/self/cgroup` names a `rimz-agent-*.scope`, it prefixes the command with `systemd-run --user --scope --quiet --collect --unit rimz-helper-<uuid>.scope --`.

On Linux `crates/rimz/src/child_process.rs::spawn_hook_drainer` requests its own helper scope with `CPUWeight=1000` from every spawner, including a session or an existing helper scope. Apply children inherit that scope, and their ordinary detached helpers stay in it because they escape only agent scopes. It shares command construction with `spawn_detached_reaped`.

The caller supplies the program and a `configure` callback instead of a built `Command`. The callback can run twice, so a plain retry keeps the arguments, environment, working directory, and log descriptors. The parent holds the child for at most two seconds, until `/proc/<pid>/cgroup` shows the exact requested `rimz-helper-*.scope`, never accepting an inherited helper scope as the move. A successful early exit needs no retry. A failed exit or an unconfirmed move kills and reaps the wrapper, then retries without the scope and logs a debug line. Only a confirmed child passes to the shared reaper. Agent-owned background commands (`spawn_detached_reaped_as_agent`) keep the inherited agent scope.

A helper started inside a scoped agent runs `systemd-run` inside the bubblewrap view. The view binds the host root and keeps `XDG_RUNTIME_DIR`, so the user bus at `/run/user/<uid>/bus` stays reachable without an extra bind. The user manager must still be reachable on the host, which the `user manager` check covers.

### Scope identity and discovery

`crates/rimz/src/proc/cgroup.rs::ScopePath` is the validated scope identity that every cgroup operation takes. It is built only from a `0::` membership line that names a direct child of this uid's `app.slice` called `rimz-agent-*.scope`. Recorded paths are validated again before a thaw (`crates/rimz/src/harness/hibernate/wake.rs::recorded_scope`).

`crates/rimz/src/harness/hibernate.rs::ScopeView::probe` finds the scope from the pane's root pid. It resolves the provider process and takes its scope when the name carries this workspace's prefix. Outside that scope it follows the one child that could host an agent through `crates/rimz/src/proc/command.rs::sole_agent_child`, the step shared with resume-id recovery. That step skips RimZ helpers (every `rimz` process except a nested exec wrapper) and children with no command line, such as an unreaped zombie. It stops when no candidate or more than one remains, so a sibling agent cannot lend its process to the probe. The walk passes the exec wrapper, which is either unscoped or in another room's agent scope that it inherited from a multiplexer server started in that agent's pane. The probe then reads the scope's `cgroup.procs` (an unreadable file yields an empty list, as the process readers do elsewhere) and each member's parent chain. The provider lookup's Linux liveness check treats a frozen process in the sleeping or uninterruptible-sleep state as alive and excludes only zombie and dead states, so a probe of a frozen agent still finds its provider.

## Selection

### The producer pass

`crates/rimz/src/harness/hibernate.rs::hibernate_idle_agents` is the `hibernate` step of the sidebar's heavy refresh (`crates/rimz/src/sidebar/refresh/mod.rs::refresh_heavy_lanes`). It does not read read receipts. Off Linux it returns at once. On Linux each pass:

1. Updates the focus trail ([Grace stamps](#grace-stamps)).
2. Spawns a paced focus wake when the focused pane has a record ([Focus wakes](#focus-wakes)). Steps 1 and 2 run even when hibernation is disabled or unavailable, so an agent frozen before a setting change still wakes.
3. Stops unless hibernation is enabled and the cached admission is available, and stops with a warning when the schedule fails to validate.
4. For each agent with a pane and a pane pid, skips it when a recorded entry has no due step, or when an unrecorded agent fails the cheap holds (rows 1 to 6 below).
5. Skips the agent when its fire record shows a spawn in the last 30 seconds. An unrecorded agent whose focus stamp is newer than that fire is not skipped, so the first pass after focus grace can freeze it at once.
6. Writes the fire record, then spawns `rimz agents hibernate --request`. The record is written first because the helper may write its hold reason into the same record at once. A failed spawn clears the fire time.

### The decision

`crates/rimz/src/harness/hibernate.rs::decide` gives one `Verdict`: `Hibernate`, `Deepen { step }`, or `Hold(Hold)`. An agent that already has an entry skips every rest and debt check: its next step yields `Deepen` when due and `Hold::AlreadyHibernating` otherwise, including after the final step. An agent without an entry takes the first hold that applies:

| Order | Hold | Cause |
| --- | --- | --- |
| 1 | `Ended`, `ProviderSubagent`, `AwaitingInput`, `BudgetPark`, `Compacting`, `Busy` | `idle_stop::resting` refuses the agent: the session ended, it is a provider subagent, a native prompt is open, a budget park holds it, or it is compacting. `Busy` covers an effective status other than idle or success, a parked turn phase, and a running background shell. |
| 2 | `Clock` | The rest clock (`idle_stop::rested_since`) is younger than `after`. |
| 3 | `Focused` | The pane is focused. |
| 4 | `RemoteControl` | Remote control is on for the provider. |
| 5 | `KeepWarm` | A keep-warm hold applies, with the same horizon and ping eligibility as cache keepalive (`SeatPolicy::holding`). |
| 6 | `WakeClock` | A wake stamp's grace has not run out ([Grace stamps](#grace-stamps)). |
| 7 | `NoScope` | No scope with this workspace's prefix was found, or the provider is not in it. |
| 8 | `ForeignProcess { pid, comm }` | A member is neither on the provider's in-scope ancestor chain (such as `bwrap` and `sh`) nor able to reach the provider through its parent chain. Siblings under those ancestors are foreign, and so is a member whose chain breaks on an unreadable parent. `comm` is `unknown` when the member's own command cannot be read. |
| 9 | `Owed`, `Message`, `OpenRun` | `idle_stop::debt`: an owed wake, a queued message, or an open run. |

`decide` shares idle-stop's resting, rest-clock, and debt predicates. It does not take idle-stop's board, tree, or molt-successor vetoes. `Policy` resolves the schedule and the keep-warm definitions once, before any decision.

## Freeze and deepen

### The freeze helper

`rimz agents hibernate --request` (`crates/rimz/src/cli/agents_cmd/hibernate.rs::run`) reads an event-fresh published snapshot and calls `crates/rimz/src/harness/hibernate/execute.rs::run_request`. The helper checks again that the agent's pane and root pid match the request, probes the scope, and decides again. For the helper, focus also includes a fresh multiplexer focus intent (`mux::focus_anchor`), which holds the target while the published frame catches up. A `NoScope` or `ForeignProcess` hold appends a hold assist. Any other hold ends the helper without a record.

A `Hibernate` verdict runs the freeze transaction under `hibernate.lock`:

1. Re-read the published snapshot, probe again, and require the same scope, the same provider pid, and a `Hibernate` verdict. This gives the final decision a fresh rest clock, focus, and message and run debt. The raw store snapshot lacks focus, so the helper uses the enriched view.
2. Freeze: write `1` to `cgroup.freeze` and poll `cgroup.events` every 10 ms for `frozen 1`, for at most two seconds. A failed write returns its error at once. A read error or the timeout thaws the scope and fails, and the timeout's error is `FreezeTimeout`.
3. Read `memory_stat` and keep the anonymous bytes as `anon_at_freeze`.
4. Reclaim step 0, which targets half of `anon_at_freeze`.
5. Publish the entry with step 0, the reclaimed bytes, and `frozen_at` set to the time of publication. The deepen deadlines count from this time, after the first reclaim.

A failure in steps 3 to 5 thaws the scope and publishes nothing. On success the helper clears the fire record's hold and appends a [hibernate assist](./loops.md#the-assist-log) with `frozen = true` and the reclaimed bytes.

Holds that cost a probe (`NoScope`, `ForeignProcess`) and failed freeze or reclaim attempts append an assist only when the reason differs from the one in the fire record. A repeated hold therefore costs nothing in the assist log.

### Reclaim steps

`crates/rimz/src/config/agents.rs::Schedule` owns the step clock, shared by the producer, the helper, and `rimz agents show`. With `span = deep_after - after`, step `k` is due at `frozen_at + min(k * 30m, span)` and targets `anon_at_freeze * (span - min(k * 30m, span)) / (2 * span)` resident anonymous bytes. The final step is `ceil(span / 30m)`. A zero span targets zero at once, so the freeze reclaims everything. The default schedule:

| Step | Rest time | Target resident anonymous memory |
| --- | --- | --- |
| 0 (freeze) | 30 minutes | 50% |
| 1 | 60 minutes | about 33% |
| 2 | 90 minutes | about 17% |
| 3 (final) | 120 minutes | 0 |

Each step requests `max(0, anon_now - target)` bytes, never the original allocation or file-backed memory. A zero request still advances the step, and a partial reclaim does not retry a completed step. An overdue entry advances one step per transaction, so a pass that runs late does not jump to the target for the current rest time. `crates/rimz/src/proc/cgroup.rs::reclaim` measures `memory.current` before and after, writes `<bytes> swappiness=200` to `memory.reclaim`, retries without `swappiness` on `EINVAL`, and accepts `EAGAIN` as a partial reclaim. A zero-byte request skips the kernel write. The cgroup operations take an injected filesystem root, so tests run without a live freezer or swap.

### Deepening

A `Deepen` verdict runs the deepen transaction under `hibernate.lock`. It re-reads the entry and requires the same scope and `frozen_at`, so a stretch that a wake withdrew or a new freeze replaced is never recreated. It checks that the scope is still frozen, reclaims, and checks again before it publishes. A missing or thawed scope loses its entry at either check. A successful step updates `step` and adds to the cumulative `reclaimed_bytes`, and leaves `frozen_at` unchanged. A failed reclaim keeps the previous step, so the next due pass retries it. Each step appends an assist with that step's byte delta and `frozen = false`.

### Lock hold and wake latency

Each transaction holds `hibernate.lock` for one reclaim. Input that arrives during a reclaim waits for publication, then thaws and withdraws the entry before transport. Wake latency can therefore include the rest of the freeze confirmation (at most two seconds), the reclaim, and publication. Reclaim has no time bound of its own: the kernel decides how long it takes. The lock uses the standard 30-second timeout.

## What a frozen row shows

`crates/rimz/src/harness/hibernate.rs::project_hibernation` attaches a `Hibernation` (`frozen_at`, `step`, `reclaimed_bytes`, `wake_error`) to each open row that has an entry of the same kind and session id. Read-only folds reuse the atomic record's cached parse; locked record updates remain uncached. The projection drives the [hibernated sidebar cadence](../sidebar/state.md#cadences), not lifecycle: a frozen row keeps its displayed status and glyph, ranking, unread episodes, notifications, and turn-completion behaviour. A pending wait keeps its working or sleeping classification, and idle-stop reads the real state, so an armed wait still holds a frozen agent as sleeping. Cache keepalive skips rows with an entry; idle compaction wakes eligible frozen rows.

`rimz agents list --json` and `show --json` carry `hibernation` whenever the row carries the projection. An entry with no matching live row shows nowhere. `rimz agents show` renders `hibernating since <frozen_at>, step <n> of <final>, <bytes> reclaimed`, and appends `, wake failed: <error>` when `wake_error` is set. [stats.md](../stats.md) counts freezes and keeps reclaimed memory apart from GC disk recovery.

## Wake

### Synchronous wake

`crates/rimz/src/harness/hibernate/wake.rs::wake_pane` thaws a pane before its caller writes to it or closes it. `wake_pane_at` does the same for a caller that has state paths but no `Store`, and opens or creates none. The crate-private `wake_pane_in` also takes the cgroup root. All three return `NotHibernating` at once when the room has neither `hibernate.lock` nor a record, so ordinary pane input in such a room creates no lock or stamp. Under the lock, a synchronous wake:

1. Writes the pane's synchronous wake stamp before it reads the record, so a freeze helper that publishes later still sees the grace. A failed stamp write stops the wake before any thaw.
2. Reads the pane's entries. With none it returns `NotHibernating`.
3. For each entry, thaws its scope unless the scope is already gone, and appends a `wake` diagnostic.
4. On a thaw failure, saves the error as the entry's `wake_error`, keeps the entry, and returns the error, so the caller stops before transport.
5. On success, starts the refault sampler when `memory.stat` is readable, withdraws the entry, and returns `Thawed`.

A later successful wake withdraws the entry and its `wake_error` together, so a failed wake from a detached focus helper stays visible in `rimz agents show` without changing the sidebar status.

| Caller | Function | Then |
| --- | --- | --- |
| Message delivery | `crates/rimz/src/message/send.rs::send_batch_to_live_pane` | writes the batch |
| `rimz answer` | `crates/rimz/src/cli/answer.rs::run` | writes the answer keys |
| `rimz pane send` in a managed room | `crates/rimz/src/cli/pane.rs::send` | writes the keys |
| Budget park helper | `crates/rimz/src/cli/agents_cmd/budget_park.rs::run_budget_park` | sends Esc. A wake failure ends the helper. |
| Idle compaction helper | `crates/rimz/src/cli/agents_cmd/idle_compact.rs::run_idle_compact` | rechecks the cache window, then queues the compact command. A wake failure ends the helper. |
| `rimz pane focus` | `crates/rimz/src/cli/pane.rs::focus` | focuses the pane |
| `rimz agents focus`, `rimz teams focus` | `crates/rimz/src/cli/agents_cmd/show.rs::focus_resolved` | focuses the pane |
| A resume that finds the agent live | `crates/rimz/src/cli/agents_cmd/resume.rs::resume_lane`, `crates/rimz/src/launcher/reconcile.rs::focus_existing` | focuses the pane |
| `rimz agents restart` | `crates/rimz/src/cli/agents_cmd/restart.rs::restart_resolved` | closes the old pane |
| `rimz agents stop` | `crates/rimz/src/cli/agents_cmd/stop.rs::close_agent_pane` | closes the pane |
| Supervised run cleanup | `crates/rimz/src/cli/supervised/pane.rs::close_run_pane` | closes the run's pane |
| Orphaned subagent repair | `crates/rimz/src/cli/agents_cmd/orphan_subagent.rs::repair_orphan` | closes the pane |
| Daemon view | `crates/rimz/src/daemon_view.rs::close_surplus_panes`, `repair_daemon_shape` | closes the pane. A failed wake ends the pass with `Retry`. |
| Exec wrapper exit | `crates/rimz/src/cli/agents_cmd/exec.rs::close_own_pane` | closes its own pane. A wake failure is logged and the pane still closes. |
| Session retirement | `crates/rimz/src/harness/schedule/arm.rs::retire_session_in` | retires the session's wait rows. A wake failure is logged. |

`rimz pane send` outside a managed room is ordinary multiplexer input and wakes nothing. Typed keys that arrive before a thaw stay queued in the pty and reach the provider after it.

`xtask/src/invariants.rs::ensure_pane_writes_wake_first` guards the writers. Outside the multiplexer layer and test sources, each function that contains `PaneWriter::open(` must contain an earlier `hibernate::wake_pane(`, `hibernate::wake_pane_at(`, or `hibernate::wake_pane_in(` in the same function. The multiplexer layer is exempt so it takes no harness dependency. The check has no caller allowlist, and its regression test rejects an open placed before the wake. It does not see pane closes: a new close path must call the wake itself.

### Lock-timeout recovery

Only a timeout on `hibernate.lock` lets a wake go on without the lock. Every other lock error and every thaw failure still stops the wake. A focus helper first repeats its dwell check, and returns `NotHibernating` if the user left. Then `wake_workspace_unlocked`:

1. Writes the workspace-wide wake stamp, without the lock. A failed write stops the wake.
2. Thaws every scope with this workspace's prefix, including scopes the helper froze but has not yet recorded.
3. For each entry in the workspace, appends a `wake` diagnostic with trigger `lock_timeout` and withdraws the entry through the store's normal record-update lock.
4. Appends one `wake_lock_timeout` diagnostic.

It returns `Thawed` when the requested pane had an entry and `NotHibernating` otherwise, and starts no refault samplers. The room-wide reset is reserved for this case. The workspace stamp holds every pane for the full `after`, so agents that stay idle freeze again only after that grace. If a delayed helper later publishes an entry over a thawed scope, the next wake of that pane, deepen step, or start reconciliation withdraws it.

### Focus wakes

The sidebar import graph cannot write the store, so it never wakes synchronously. It spawns the hidden `rimz agents wake --request` helper (`crates/rimz/src/cli/agents_cmd/hibernate.rs::wake`), which runs `crates/rimz/src/harness/hibernate/wake.rs::wake_request`. Three paths spawn it:

- A room-host focus event from either backend: `crates/rimz/src/sidebar_pane/app/plane.rs` calls `wake_focused` for each `FocusChanged` event.
- A producer pass that sees a focused pane with an entry. This repairs a lost event, or focus that arrived before the entry was published.
- A sidebar card click (`crates/rimz/src/sidebar_pane/app/focus.rs::spawn_pane_focus`), before the focus request.

The first two paths share a per-pane pacing of two seconds in `live/hibernate/wake.json` and spawn only for a pane with an entry. Sidebar toggle and pane zoom are browsing actions: they wake nothing directly, and the focus change reaches the helper through the room host or the producer repair. A command that names its target wakes synchronously instead ([Synchronous wake](#synchronous-wake)).

The helper waits for a dwell so that a passing glance does not thaw an agent. A pane without an entry needs no dwell and writes no stamp. For a pane with an entry, `focus_dwell` measures time from the request's `requested_at_ms`, or from helper start when it is absent:

1. When one second has already passed, it samples focus and thaws only if the pane is focused (`dwell = elapsed`).
2. Otherwise it waits until 250 ms, then samples the pty's input queue as a baseline.
3. It polls the queue every 50 ms. Growth past the baseline samples focus: a focused pane thaws at once (`dwell = input`), and an unfocused pane aborts, because a focus-out report can itself grow the queue.
4. At one second it samples focus and thaws only if the pane is focused (`dwell = elapsed`).

A focus sample calls the backend's `client_view` with a 250 ms command timeout. Some attached client's focused pane must match. Detailed client rows take precedence, and only a view without them falls back to the summarized pane ids. A failed sample counts as focused. The helper samples focus again after it gets the lock, so time spent waiting for another transaction cannot turn a departure into a thaw. An entry published while the helper waited for the lock still gets a dwell. A glance that ends without a thaw writes no stamp, record change, or diagnostic.

The 250 ms settle absorbs a focus-in report that reaches the pty before the baseline sample. It does not cover a report that arrives later. A sidebar click spawns the helper before the click's focus request reaches the multiplexer, and a cross-tab jump can wait up to `PAINT_WAIT` (500 ms, `crates/rimz/src/sidebar_pane/app/focus_push.rs`) before focus moves. A focus-in report that lands after the baseline counts as input, so a focused pane can thaw before the one-second expiry without a key typed.

Keys typed in the first 250 ms can wait until the one-second expiry, and input followed by a departure stays queued until a later wake. Codex can read the Enter of a queued paste-like burst as a newline. Early input waking narrows that window but does not close it.

`crates/rimz/src/proc/mod.rs::input_tty` opens the pty for the queue samples. It reads the provider's controlling terminal from `tty_nr` in `/proc/<pid>/stat` and maps a Unix98 pts device number (majors 136 to 143) to `/dev/pts/N`. This works whatever the provider's dumpability and needs no backend tty lookup. It opens the device read-only with `O_NOCTTY | O_NONBLOCK | O_CLOEXEC`. A zero or non-pts device, an unreadable stat file, a failed open, or a platform other than Linux yields no handle, which removes early waking and keeps the one-second expiry. `FIONREAD` counts queued bytes without reading them, so the probe is not a pane read under the pane I/O invariant.

### Grace stamps

A wake stamp keeps a just-woken agent from freezing again at the next pass. `decide` and the producer share one predicate, `wake_stamp_holds`. The stamps and the other runtime caches live under the room's runtime `live/hibernate/` directory:

| File | Writer | Content | Effect |
| --- | --- | --- | --- |
| `woke-<pane digest>.json`, synchronous | Synchronous wake, under the lock, before it reads the record | a bare timestamp | holds the pane for the full `after` |
| `woke-<pane digest>.json`, focus | The focus helper, after it thaws a recorded scope (best-effort) | `{"focus": <timestamp>}` | holds the pane for 10 seconds after the later of the wake and the pane's last departure in `focus.json` |
| `woke-workspace.json` | Lock-timeout recovery, without the lock | a bare timestamp | holds every pane for the full `after` |
| `focus.json` | The producer only | the focused pane and recent departures | departure times for the focus stamp, pruned to 10 seconds |
| `wake.json` | Focus-wake spawns from host events and the producer | last spawn per pane | two-second focus-wake pacing |
| `<agent digest>.json` | The producer (`fired_at`) and the freeze helper (`hold`) | the fire record | 30-second spawn pacing and hold-assist deduplication |
| `admission.json` | Room birth | the admission verdict | [Configuration and admission](#configuration-and-admission) |

These are atomic cache writes without a durable fsync. A missing or unreadable stamp grants no grace. A failed focus-stamp write still withdraws the thawed entry, since the stamp only grants grace. The producer rewrites `focus.json` only when the focused pane changes, recording the previous pane's departure, and never edits a wake stamp, so it cannot shorten a synchronous wake's grace. Without a trail entry, the focus stamp counts from the wake. Focus itself still blocks a new freeze (hold 3). A new turn moves the rest clock and restores the full `after` on its own, and a new freeze starts the reclaim steps again from step 0.

### Shutdown paths

Paths that signal or close agents thaw before they act:

- The process sweep (`crates/rimz/src/mux/recovery.rs::sweep_orphan_processes`) thaws the workspace's scopes before it selects targets, and thaws each target's own scope before its SIGTERM.
- A supervised child (`crates/rimz/src/child_process.rs::SupervisedChild::signal_term`) thaws its own scope before SIGTERM.
- Pane closes wake through the callers in the [synchronous wake table](#synchronous-wake).

## Start reconciliation

`crates/rimz/src/harness/hibernate/wake.rs::reconcile_at` runs at every `rimz start` (`crates/rimz/src/cli/room/mod.rs::start`) and before a recovery (`preflight_recovery`). It returns at once when the room has neither a record nor a scope with its prefix. Otherwise, under `hibernate.lock`, it:

1. Withdraws each entry whose scope is missing or not frozen.
2. Thaws each scope with this workspace's prefix that has no entry and is frozen or has a freeze requested (`cgroup.freeze` reads `1`), whether or not `cgroup.events` has confirmed it.

Scopes of other workspaces are untouched. A missing, corrupt, or unknown-version record reads as empty ([store.md](../store.md)), so reconciliation then thaws every frozen scope of the workspace.

## Wake diagnostics

`crates/rimz/src/diag/hibernate.rs` appends best-effort records to the rotating log `audit/hibernate.log.jsonl` in the workspace state directory. They are diagnostic evidence only, never assists or inputs to a decision, and a failed append, counter read, or sampler spawn never delays input. Every record carries `at`, `workspace_id`, and `pane_id`. `wake` and `wake_refault` also carry `kind` and `agent_id`.

| `event` | Written | Fields |
| --- | --- | --- |
| `wake` | Once per entry at each wake exit, before withdrawal, including a failed locked thaw | `frozen_at`, `step`, `anon_at_freeze`, `reclaimed_bytes`, `trigger` (`sync`, `helper`, or `lock_timeout`), `thaw_ms`, `focus_to_thaw_ms`, `dwell`, `error` |
| `wake_refault` | Once by the refault sampler, after a successful locked thaw | `thawed_at`, and `samples`, each with `after_ms`, `refault_anon`, and `refault_file` |
| `wake_lock_timeout` | Once after a lock-timeout recovery | `lock_path`, `withdrawn` (the number of entries withdrawn) |

For a locked wake, `thaw_ms` runs from wake entry to the thaw write, lock acquisition included. For lock-timeout recovery it runs to the end of the workspace-wide thaw and is the same for every entry. `focus_to_thaw_ms` is present on helper wakes whose request carries `requested_at_ms`, and includes the dwell. `dwell` is `input` or `elapsed` on helper wakes and is omitted from the other triggers. `error` is null on success and carries the thaw failure otherwise, with the latency measured through the failed attempt.

The refault sampler is the hidden `rimz agents wake-refault --request` helper. A locked thaw that reads `memory.stat` passes it the `workingset_refault_anon` and `workingset_refault_file` counters as a baseline. It samples at about 1, 10, and 60 seconds after the thaw and records the cumulative deltas from that baseline. `after_ms` is the measured time since the thaw, and a counter that decreases gives a delta of zero. A missing or unreadable scope ends sampling and writes the samples already taken. A failed wake and lock-timeout recovery start no sampler.

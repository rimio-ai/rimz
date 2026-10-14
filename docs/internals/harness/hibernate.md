# Agent hibernation

Hibernation is an opt-in machine capability under `[agents.hibernate]`: `enabled = false`, `after = "10m"`. The duration is validated at use and cannot exceed 24 hours. Off Linux the loader disables an enabled setting and records a notice naming `agents.hibernate.enabled`.

## Admission

`crates/rimz/src/harness/hibernate.rs::preflight` enforces admission; `crates/rimz/src/harness/hibernate.rs::inspect` returns the same checks for doctor.

Start and recovery preflight require `systemd-run`, a reachable systemd user manager, and a writable user `app.slice` with the memory controller and writable `cgroup.freeze` and `memory.reclaim` files. Doctor reports each control separately. The user manager's `app.slice` is resolved from the uid, not the caller's cgroup membership, so a login in `session-N.scope` can use it. `/proc/meminfo` must report positive `SwapTotal`. `SwapFree = 0` produces a doctor warning, not a refusal. Visible ancestors with `memory.swap.max = 0` refuse admission; `max` does not prove swap is available. In a container, limits above the cgroup namespace may be invisible and virtualized meminfo is the swap authority.

## Provider scopes

The launch compiler wraps only a ready provider argv, outside bubblewrap, with a memory-accounted systemd user scope named `rimz-agent-<workspace_id>-<nonce>.scope`. Each compilation mints a fresh nonce. The exec wrapper retains its supervision and leases outside that scope.

Qwen's login-shell reentry stays unwrapped: it launches another exec wrapper. That wrapper compiles the finalized provider and adds its scope then. Scoping the reentry itself would put the wrapper inside an agent scope.

## Detached helpers

Helpers spawned from an agent scope use a separate `rimz-helper-<nonce>.scope`; agent-owned background commands keep their inherited scope. A failed scope spawn falls back to an ordinary helper with a debug diagnostic. Hibernation policy vetoes foreign processes that remain in an agent scope.

`crates/rimz/src/child_process.rs::spawn_detached_reaped` owns command construction. Callers supply the program and a repeatable configuration callback so both scoped and fallback launches retain arguments, environment, working directory and log descriptors. This avoids trying to reconstruct opaque stdio from an already configured `Command`. The parent retains the child while confirming its helper-scope membership for at most two seconds. A successful early exit needs no retry; a failed exit or unconfirmed move stops and reaps the wrapper before retrying plainly. Only then does a live helper pass to the shared reaper.

The sandbox starts with a host-root bind and preserves `XDG_RUNTIME_DIR`; it does not hide the normal `/run/user/<uid>/bus`. No additional bus bind is needed. A reachable user manager is still a host precondition, not something the mount view supplies.

## Cgroup operations

Scope discovery walks past an unscoped exec wrapper along its unambiguous child chain to the scoped provider. The shared liveness classifier recognizes wrappers too; their PID alone is not evidence of provider scope membership.

Freeze writes `1` and waits at most two seconds for `cgroup.events` to confirm `frozen 1`. Failure thaws the scope. Reclaim measures `memory.current` before and after, requests `swappiness=200`, retries without that parameter on `EINVAL`, and accepts `EAGAIN` as partial reclaim. These operations use injected filesystem roots in tests, without a live freezer or swap.

`crates/rimz/src/proc/cgroup.rs::ScopePath` is the validated scope identity passed to the cgroup operations. Process discovery returns an empty list on an unreadable scope, matching the process-reader convention. The decision requires the provider itself in that list, never accepting an empty list as proof that all members are descendants. Linux process liveness already admits frozen sleeping and uninterruptible-sleep states; only zombie and dead states are excluded.

## Selection and publication

`crates/rimz/src/harness/hibernate.rs::hibernate_idle_agents` runs on the sidebar's heavy refresh without consulting read receipts. Disabled or non-Linux configurations do nothing. A runtime cache under `live/hibernate` paces helper spawns to once per session per 30 seconds; it is published before spawn so a fast helper's hold reason is not overwritten.

`crates/rimz/src/harness/hibernate.rs::decide` shares idle-stop's resting, rest-clock, and debt predicates, not its board or launched-tree vetoes. It refuses ended sessions, provider subagents, native input, budget parks, compaction, active work, unfinished grace periods, focus, remote control, keep-warm holds, owed wakes, queued messages, open runs, and already-recorded sessions. The validated `Policy` resolves the configured duration and effective keep-warm definitions before deciding; `Hold::KeepWarm` uses the same horizon and ping eligibility as cache keepalive. Scope ownership must match this workspace, include the provider pid, and contain only that process, its descendants, and its own in-scope ancestor chain (such as bwrap and sh). Siblings under those ancestors remain foreign. Missing membership yields `NoScope`; a foreign or unreadable ancestry yields `ForeignProcess` with pid and command evidence. Idle-stop evaluates a frozen agent's real state, so an armed wait still holds it as sleeping.

The hidden `agents hibernate --request` helper reads an event-fresh published snapshot, probes the scope, and decides again. Its CLI-supplied reader refreshes that same enriched view inside the lock, rather than using the raw store snapshot that lacks focus. A fresh mux focus intent also holds the target while the published frame catches up. `crates/rimz/src/harness/hibernate/execute.rs::run_request` then freezes, reclaims, and publishes the [durable record](../store.md). Reclaim or publication failure attempts to thaw; a success appends a [hibernate assist](./loops.md#the-assist-log). Holds requiring a scope probe or failed freezer/reclaim attempt are recorded only when their reason changes. The helper mutation module is outside the sidebar's read-only producer file.

A per-workspace hibernation lock serializes the final decision, freeze, reclaim, and publication with synchronous wake and reconciliation. The final decision reads a fresh rest clock and message/run debt under that lock. Each freeze transaction holds it for one reclaim: input arriving during reclaim waits for publication, then thaws and withdraws the entry before transport. Wake latency therefore includes the remaining freeze confirmation (bounded at two seconds), reclaim, and publication; reclaim has no kernel-independent time bound. Lock acquisition uses the standard 30-second timeout; wake alone bypasses a timed-out hibernation lock as described below.

Enrichment attaches the record without changing lifecycle or displayed status. Frozen rows keep their real glyph, ranking, unread episodes, notifications, and turn-completion behavior; pending waits still make resting rows `sleeping`. Cache keepalive and idle compaction skip recorded frozen rows. List/show JSON carries hibernation metadata whenever a record exists, and show renders its freeze time and reclaimed bytes; [stats](../stats.md) keeps reclaimed memory separate from GC disk recovery.

## Wake and recovery

`crates/rimz/src/harness/hibernate/wake.rs::wake_pane` thaws every recorded scope for the pane before withdrawing its entry. A missing scope is already gone; other thaw failures retain the record and stop the input path. Scope paths must resolve beneath the current user's `app.slice` with an agent-unit name. `wake_pane_at` supplies the same operation to paths-only callers without opening or creating a store; raw pane sends outside managed rooms remain ordinary mux input.

On hibernation-lock timeout only, wake proceeds without that lock and thaws every scope of the workspace, including scopes the helper has frozen but not yet recorded. It withdraws all workspace hibernation entries through the store's normal record-update lock. Other lock errors and thaw failures still stop wake. The room-wide reset is reserved for this pathological timeout; the next producer pass can hibernate agents that remain idle. Wake reports `Thawed` when the requested pane had a record and `NotHibernating` otherwise. After the reset it appends one best-effort rotating diagnostic to `audit/hibernate.log.jsonl`: event `wake_lock_timeout`, timestamp `at`, `workspace_id`, `pane_id`, `lock_path`, and the `withdrawn` entry count. If a delayed helper publishes a stale entry over a thawed scope afterward, the next reconciliation withdraws it.

Message delivery, answers, pane sends, budget interrupts, CLI focus, and pane closes invoke wake before transport. The read-only sidebar graph instead spawns the hidden `agents wake --request` helper. Room-host focus events from either backend and producer passes observing a recorded, focused pane share the five-second wake pacing. Pulled focus repairs a lost event or focus that arrived before publication; sidebar clicks also spawn a helper. Session retirement attempts thaw and withdrawal before retiring its wait rows, logging failures. Typed keys received before thaw remain queued in the pty.

Wake records a per-pane timestamp under runtime `live/hibernate` while holding the hibernation lock, even if a queued helper has not yet published a record. Timeout reset records a workspace-wide timestamp without that lock. These are atomic cache writes without durable fsync; a missing stamp grants no extra grace. `decide` requires a full configured `after` since the latest applicable wake, preventing immediate refreeze before input lands or after a glance. Rooms with neither a hibernation lock nor a record create no lock or stamp on ordinary pane input.

Start and recovery reconcile frozen scopes against the durable record after preflight. A recorded scope that disappeared or is no longer frozen loses its entry; a recordless scope with this workspace's prefix is thawed when freeze was requested, even before `cgroup.events` confirms it. Other workspaces are untouched. Process sweeps thaw the workspace's scopes before signals; supervised children thaw their own scope before SIGTERM. Surplus daemon panes use synchronous wake before close. An exiting exec wrapper logs a wake failure but still closes its pane. The launch compiler and sweep use one scope-prefix definition.

Same-user processes can write delegated cgroup controls; the scopes are resource management, not a security boundary.

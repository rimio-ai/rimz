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

`crates/rimz/src/harness/hibernate.rs::hibernate_idle_agents` runs on the sidebar's heavy refresh. The sidebar supplies the seen verdict from its read receipts, requiring a receipt at or after the latest rest timestamp. Disabled or non-Linux configurations do nothing. A runtime cache under `live/hibernate` paces helper spawns to once per session per 30 seconds; it is published before spawn so a fast helper's hold reason is not overwritten.

`crates/rimz/src/harness/hibernate.rs::decide` shares idle-stop's resting, rest-clock, and debt predicates, not its board or launched-tree vetoes. It refuses ended sessions, provider subagents, native input, budget parks, compaction, active work, unfinished grace periods, unseen results, focus, remote control, owed wakes, queued messages, open runs, and already-recorded sessions. The validated `Policy` resolves the configured duration before deciding. Scope ownership must match this workspace, include the provider pid, and contain only that process, its descendants, and its own in-scope ancestor chain (such as bwrap and sh). Siblings under those ancestors remain foreign. Missing membership yields `NoScope`; a foreign or unreadable ancestry yields `ForeignProcess` with pid and command evidence. Hibernating agents remain resting for idle-stop decisions.

The hidden `agents hibernate --request` helper reads an event-fresh snapshot, probes the scope, and decides again. `crates/rimz/src/harness/hibernate/execute.rs::run_request` then freezes, reclaims, and publishes the [durable record](../store.md). Reclaim or publication failure attempts to thaw; a success appends a [hibernate assist](./loops.md#the-assist-log). Holds requiring a scope probe or failed freezer/reclaim attempt are recorded only when their reason changes. The helper mutation module is outside the sidebar's read-only producer file.

Enrichment attaches the record without changing lifecycle status. An idle or successful displayed result becomes `hibernating`, before pending waits can make it `sleeping`. Its glyph is `❄`, it ranks with idle, and it neither requests attention nor sends a notification. List/show carry the status and reclaimed-memory metadata; [stats](../stats.md) keeps reclaimed memory separate from GC disk recovery.

Wake and reconciliation wiring remains the next implementation stage: consumers must thaw before input or shutdown. Same-user processes can write delegated cgroup controls; the scopes are resource management, not a security boundary.

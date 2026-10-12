# Agent hibernation

Hibernation is an opt-in machine capability under `[agents.hibernate]`: `enabled = false`, `after = "10m"`. The duration is validated at use and cannot exceed 24 hours. Off Linux the loader disables an enabled setting and records a notice naming `agents.hibernate.enabled`.

## Admission

`crates/rimz/src/harness/hibernate.rs::preflight` enforces admission; `crates/rimz/src/harness/hibernate.rs::inspect` returns the same checks for doctor.

Start and recovery preflight require `systemd-run`, a reachable systemd user manager, and a writable user `app.slice` with the memory controller and writable `cgroup.freeze` and `memory.reclaim` files. Doctor reports each control separately. The user manager's `app.slice` is resolved from the uid, not the caller's cgroup membership, so a login in `session-N.scope` can use it. `/proc/meminfo` must report positive `SwapTotal`. `SwapFree = 0` produces a doctor warning, not a refusal. Visible ancestors with `memory.swap.max = 0` refuse admission; `max` does not prove swap is available. In a container, limits above the cgroup namespace may be invisible and virtualized meminfo is the swap authority.

## Provider scopes

The launch compiler wraps only a ready provider argv, outside bubblewrap, with a memory-accounted systemd user scope named `rimz-agent-<workspace_id>-<nonce>.scope`. Each compilation mints a fresh nonce. The exec wrapper retains its supervision and leases outside that scope.

Qwen's login-shell reentry stays unwrapped: it launches another exec wrapper. That wrapper compiles the finalized provider and adds its scope then. Scoping the reentry itself would put the wrapper inside an agent scope.

## Detached helpers

Helpers spawned from an agent scope use a separate `rimz-helper-<nonce>.scope`; agent-owned background commands keep their inherited scope. A failed scope spawn falls back to an ordinary helper with a debug diagnostic. Later hibernation policy must veto foreign processes that remain in an agent scope.

`crates/rimz/src/child_process.rs::spawn_detached_reaped` owns command construction. Callers supply the program and a repeatable configuration callback so both scoped and fallback launches retain arguments, environment, working directory and log descriptors. This avoids trying to reconstruct opaque stdio from an already configured `Command`. The parent retains the child while confirming its helper-scope membership for at most two seconds. A successful early exit needs no retry; a failed exit or unconfirmed move stops and reaps the wrapper before retrying plainly. Only then does a live helper pass to the shared reaper.

The sandbox starts with a host-root bind and preserves `XDG_RUNTIME_DIR`; it does not hide the normal `/run/user/<uid>/bus`. No additional bus bind is needed. A reachable user manager is still a host precondition, not something the mount view supplies.

## Cgroup operations

Freeze writes `1` and waits at most two seconds for `cgroup.events` to confirm `frozen 1`. Failure thaws the scope. Reclaim measures `memory.current` before and after, requests `swappiness=200`, retries without that parameter on `EINVAL`, and accepts `EAGAIN` as partial reclaim. These operations use injected filesystem roots in tests, without a live freezer or swap.

`crates/rimz/src/proc/cgroup.rs::ScopePath` is the validated scope identity passed to the cgroup operations. Process discovery returns an empty list on an unreadable scope, matching the process-reader convention. A future hibernation decision must require the provider itself in that list, not accept an empty list as proof that all members are descendants.

These foundations do not yet select idle agents, record hibernation, or wire wake paths. Those consumers must keep durable records authoritative and thaw before input or shutdown. Same-user processes can write delegated cgroup controls; the scopes are resource management, not a security boundary.

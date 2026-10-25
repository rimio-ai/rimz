# Agent scoped mount views

Scoped isolation runs each agent's provider process inside Linux bubblewrap with a rearranged filesystem view. It is machine policy, `agents.isolation = "scoped"` in `config.toml`, and the default is `host`. The view gives every agent a private `/tmp` and `/var/tmp` that its subagents share ([temp units](#temp-units)) and lets a profile decide which skills the model may call ([profile skill views](#profile-skill-views)). Raw command panes and the room's multiplexer stay on the host.

The view is not containment. The host root stays bound read-write except for the [protected paths](#protected-paths), and credentials stay visible. The PID, network, and IPC namespaces are shared, so a process an agent starts can outlive it and keep running inside the view. Trust decides what a repository may run, and a view does not make an untrusted command safe ([trust.md](./harness/trust.md#the-executable-surface)).

The code lives in `crates/rimz/src/sandbox/`:

| File | Owns |
| --- | --- |
| `mod.rs` | `plan` (mounts, environment pins, copies), `apply`, `bwrap_argv`, `preflight` and `preflight_launch`, `TmpView`, `SandboxErr` |
| `linux.rs` | The bubblewrap probe and the `rimz doctor` diagnosis. |
| `skills.rs` | The scoped skill view: the merged provider root and library, alias checks, omissions. |
| `rewrite.rs` | User-only skill copies and system SSH-config mirrors: capture, digest, rewrite, atomic apply. |
| `bind_guard.rs` | `BindGuard`, the [live repair](#live-protected-bind-repair) of protected binds that a host rename detached. |

`harness/launch_plan.rs` is the one caller that plans and applies a view. Two other places read a view back to host paths: `cli::caller_host_path` for paths a scoped caller passes to a launch ([paths a caller names](#paths-a-caller-names)), and `crates/rimz/src/cli/hooks.rs::map_sandbox_paths` for hook frames ([hook frames](#hook-frames)).

## Choosing the isolation

A launch resolves its level through `Isolation::resolve`: the recorded `--isolation` override wins, then the profile's `isolation: host|scoped|sandbox`, then the machine's `agents.isolation` as it reads now. A repository profile in either namespace that sets `isolation` is refused, so the level comes only from machine definitions, machine policy, and flags, none of which the trust hash covers. Team roles inherit the named definition's default and cannot declare their own.

The profile default travels beside `skills` as `isolation_default` through `ResolvedProfile`, `AgentCell`, `ResumeLaunchPosture`, and `ExecRequest`, and it never enters `LaunchParams.isolation`. Restart, fork, resume, and rebirth re-read the profile, so an edited default changes the next launch unless a recorded override wins.

Single-session resume (`resume @handle`, `--resume=<ID>`), the provider picker, and cohort resume on both `rimz agents` and `rimz teams` accept `--isolation host|scoped|sandbox`. An exact or cohort resume replaces the stored override when the flag is given and keeps it when the flag is omitted. A picked session keeps the picker's settings. Every path preflights on its resolved level before it opens or takes over a pane. Single-session resume gets the resolved level and skills presence back from the planner for its CLI preflight, and warns when the recorded effective-isolation stamp differs.

The exec wrapper stamps `effective_isolation` on `agent.attached` before the provider starts, independently of the recorded override. Observers read the stamp through `AgentState::runs_in(machine)`, which falls back to the override and then machine policy for rows without one. Relaunches resolve the current definition and never replay the stamp.

### The subagent cap

A subagent launched without its own `--isolation` inherits its parent's recorded override, not its profile default. Without one, it follows its own definition, then machine policy. `harness::plan::cap_child_isolation` then caps the choice at the parent's ambient isolation, ordered host < scoped < sandbox, at supervised child launch and at parent-message child resume:

| Source of a looser level | Result |
| --- | --- |
| Explicit `--isolation`, or a recorded override | Refused. |
| Profile default, or machine policy | Clamped to the parent's level, with a note naming both levels and the source. The clamped level is recorded as the override, so restart and resume keep it. |

A host parent, or a parent with no ambient isolation, imposes no cap. The cap keeps an agent tree consistent and is not containment: ordinary agent launches, teams, forks, and loops are outside it. Each scoped child opens its own pane through the multiplexer, builds its own view from its own profile, and shares the parent's temp unit.

The same planner caps the child's permission mode and notes a yolo child that resolves to scoped isolation. Both rules, and the launches outside either cap, are in [subagents.md](./harness/subagents.md#the-parent-caps-a-childs-posture).

## Provider command sandboxes

Each level also maps to provider flags, which the adapters own. The process compiler passes the resolved level to `LaunchCapability::configure_room_permissions` at every level. Only scoped isolation builds a mount plan, maps temp paths, or holds a host hook-drainer lease. Its host wrapper reconnects without pane output and records lease gaps ([resident wrapper](./harness/fleet.md#resident-wrapper)). Provider sandbox hooks use host paths.

Codex keeps its native command sandbox under host and scoped isolation and adds only room-root write grants where the selected permissions support them ([adapter launch](./agents/adapter_codex.md#launch)). Under `sandbox`, it appends `--sandbox workspace-write` only when the launch's arguments and config select no mode. A selected mode stays, full access and Yolo included. RimZ never adds `--sandbox danger-full-access`.

Claude's one merged `--settings` grants `sandbox.filesystem.allowWrite` for the room roots at every level, and adds routine `sandbox.excludedCommands` when `allow-routine-rimz` is on. Scoped launches add `enableWeakerNestedSandbox: true`. Sandbox launches add `enabled: true` and nothing else, so the user's other sandbox settings stay intact.

RimZ owns this flag mapping and nothing past it. It adds no dependency check, no `failIfUnavailable`, and no warning about a permission mode, so the provider's own behaviour applies when its sandbox cannot start. A provider with no sandbox switch gets a launch warning and no flag.

Muse alone implements `LaunchCapability::disable_native_sandbox_args`: it adds `--disable-sandbox` under scoped isolation, because its confinement check cannot reliably nest. Every other adapter keeps the no-op default. Cursor's `--sandbox disabled` comes only from its Yolo mode.

## Preflight

Every entry point that can start a scoped agent probes bubblewrap first and refuses with the fix. None falls back to host mode. `sandbox::preflight` runs `bwrap --bind / / --dev-bind /dev /dev --die-with-parent -- /usr/bin/true` with the `bwrap` found on `PATH`, and returns that binary's absolute path. Its errors are the `SandboxErr` variants `UnsupportedOs`, `MissingBwrap`, `ProbeFailed`, and `Io` (the probe could not run), each ending with what to change. Off Linux, the config loader changes a configured `scoped` setting to `host` with a notice before launch resolution, while an explicit `--isolation scoped` still refuses.

Two callers soften the refusal. Room judgment (`room/judgment.rs`) records a failed probe as a warning with fixes and opens the room, while scoped launches in it still refuse. Room recovery skips an unavailable candidate and leaves it resumable. The `Blocker` conversion in `sandbox/mod.rs` gives the cause fixes (install bubblewrap, check the user-namespace sysctls and LSM policy) as text only, and marks the `rimz config set agents.isolation host` fix as a security surface for the user to run.

| Entry point | When it probes |
| --- | --- |
| `rimz config set agents.isolation scoped` | Before writing the value. |
| `rimz start` and detached room ensure (`cli/room/mod.rs`) | When machine policy is scoped. |
| Room recovery | When a candidate resolves to scoped from its override, current profile default, or machine policy. An unavailable scoped view skips that candidate. |
| `rimz agents launch`, `restart`, `fork`, supervised runs | For the launch's effective isolation. |
| Cohort resume (`crates/rimz/src/launcher/mod.rs::launch_resume_layout`) | For each resumed agent's effective isolation. |
| Parent-message child resume (`cli/subagents/resume.rs`) | For the child's effective isolation after re-reading its profile. |
| The exec wrapper (`cli/agents_cmd/exec.rs`) | For the launch's effective isolation. A failure marks the launch failed and fails its run with the refusal as its reason. |
| `rimz agents explain` | For the explained launch's isolation. |
| `rimz doctor` | Always. Reports mode, binary path, version, probe verdict, and fix. In host mode the check is informational. |

`launch`, `restart`, `fork`, `molt`, supervised runs, subagent resume, room rebirth, the exec wrapper, and `explain` call `sandbox::preflight_launch`, which checks skills before it probes. Under scoped isolation it refuses a configured profile `skills` list when the adapter cannot mark skills user-only (`ManualSkillsUnsupported`), so that refusal needs no bubblewrap. Under `sandbox`, preflight probes nothing: the level only adds provider flags, and whether the provider's sandbox can start is the provider's behaviour. Saving `agents.isolation sandbox` probes nothing either.

## The launch plan

`sandbox::plan` reads the environment, paths, skill directories, and skill and system SSH-config bytes into a `SandboxPlan` and creates nothing. The plan holds the ordered `MountPlan`, the environment `pins`, the `skipped` skills, and the captured `copies` with their content-addressed targets. `sandbox::apply` writes the copies and ensures the private temp unit. Launch planning runs the two at different steps.

The exec wrapper drives the shared [launch plan](./harness/fleet.md#the-exec-wrapper). `launch_plan::compile` plans a view only for a ready provider process (`AgentProcessStage::Ready`), pins its environment into the compiled process, and lowers the mounts with `sandbox::bwrap_argv`. It creates nothing. `launch_plan::apply` creates the launch's temp unit and the room's `shared/`, both mode `0700`, at every level, then calls `sandbox::apply` on a scoped launch. Qwen's login-shell reentry stage runs on the host without a view, and its finalized exec is the ready stage that builds one.

[`rimz agents explain`](../reference/cli/agents.md#explain-a-launch) prints the mounts, pins, copy targets, and omissions without applying them. It plans from its own invoking environment plus the launch overrides, which can differ from the target pane's environment.

The wrapped process has four properties a contributor should expect:

- Each pane gets exactly one RimZ scoped view, never nested wrappers. The wrapper runs the absolute bubblewrap path its preflight probed, so a trusted provider `PATH` override does not change it.
- A scoped pane never plans another launch's view from its own. It never births a mux server (room birth refuses under ambient scoped isolation), and it never judges skill listings, since its unlisted skills appear there as RimZ's user-only copies. A launch it requests is planned and judged by the exec wrapper, which the host-born mux server spawns on the host.
- Bubblewrap forks the provider instead of becoming it. `--die-with-parent` makes the provider end when the supervised bubblewrap process ends.
- Bubblewrap sets `NoNewPrivs`, so `sudo` and setuid binaries cannot escalate inside the pane. Privileged work belongs in a host shell.

## Mount order

`sandbox::bwrap_argv` emits `bwrap --bind / / --dev-bind /dev /dev --die-with-parent`, then the plan's mounts in this order, then `--chdir <cwd> -- <provider argv>`:

1. The adapter's provider config home (`config_home`), bound at its own path, when it exists. Built-ins resolve it from the effective launch environment, where a room's named account carries its home override (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`), so the bind follows the room's account. A named account's home holds links into the provider's own home (its settings, and for a shared account its history too). They resolve through the root bind, or through the [reachable host paths](#reachable-host-paths) rebind when they lie under `/tmp`. The account reconciler runs on the host before the view is built. Plugins declare no config home.
2. The launch's temp unit (`StatePaths::temp_unit_dir`) bound at `/tmp`, then the same unit at `/var/tmp`. Bubblewrap resolves each bind source against the host root, so the second bind shows the unit, not the first mount.
3. Host paths beneath `/tmp` or `/var/tmp` that must stay reachable, rebound at their original paths ([reachable host paths](#reachable-host-paths)).
4. Requested read-only paths (a workflow judge's checkout and its Git directories), then each [protected path](#protected-paths) that exists, read-only, kept so by the [live guard](#live-protected-bind-repair).
5. The skill view, when one is needed: a tmpfs over the resolved skill root, one entry per skill, and read-only shadows of rewritten skills at their canonical paths ([building the view](#building-the-view)). A protected path that contains the skill root is bound writable at step 4, and both it and the skill root get `--remount-ro` after the view is assembled. A read-only bind of the parent placed after the view would hide it.
6. The [system SSH-config mirror](#system-ssh-config-mirror), when it has entries: one read-only bind of the rewritten system config at its canonical path. This follows every ancestor bind and skill-view remount so none can hide it.

The command has no `--unshare-*` flag, no replacement `/proc`, and no cleared environment. `/dev` takes `--dev-bind` because an ordinary bind breaks device files such as `/dev/null`.

### Protected paths

`crates/rimz/src/sandbox/mod.rs::protected_paths` builds the list of configuration paths a scoped agent cannot write: shell startup files, `.gitconfig`, `.ssh`, and the systemd user directory under HOME, the checkout's Git `hooks` and `config`, and the agent configuration entries (`.claude`, `.codex`, `.agents`, `.mcp.json`) at the checkout and the project root. The [security guide](../guide/security.md#scoped-isolation) owns the list and its operational costs. Five rules decide which entries become binds.

`launch_plan::compile` decides the checkout once, for both protection and the judge's read-only paths. `worktree::discover_checkout` finds the first ancestor whose `.git` resolves to an admin directory with a `HEAD` file, skipping empty or stray metadata. An unreadable `.git` file refuses a scoped launch with `SandboxErr::Io`. A cwd under `StatePaths::empty_dir_root` has no checkout, also when the root and the cwd use different symlink spellings. Without a checkout, configuration protection uses the cwd and the room project root and protects no Git directory.

`worktree::git_admin_dir_from_checkout_metadata` resolves the Git directory, including gitdir files for submodules and separate Git directories. A `commondir` file moves the Git protection to the shared directory.

A path that does not exist is skipped, and no placeholder is created for it.

An entry whose canonical path equals, lies inside, or contains the provider-home source, target, or default home is skipped silently, with an absent home normalized lexically. The exemption covers HOME startup files too, but never the requested read-only paths.

A bind follows a symlinked protected entry and makes its target read-only. The link itself stays replaceable by a rename over it in its writable parent directory.

A protected file is a single-file bind, and the kernel detaches such a bind in every running view when the host renames a new file over its path. `git config --global` writes `~/.gitconfig` that way, as do most editors and package managers. The [live guard](#live-protected-bind-repair) binds the new file read-only again, normally within milliseconds, and the view sees it writable only in that interval. Protected directories (`.ssh`, the Git `hooks`) are not detached this way. A new bind should still target a directory, or a file that is rarely replaced, and its doc should say which.

## Live protected-bind repair

A host rename over a protected entry detaches the old dentry's bind in other mount namespaces, which exposes the replacement through the writable root. `crates/rimz/src/sandbox/mod.rs::SandboxPlan::guarded_entries` keeps exactly the protected identity `RoBind` entries at canonical paths, deduplicated in plan order. Requested read-only paths, provider-home exemptions, skill entries, the SSH mirror, and writable overlay parents later remounted read-only are not guarded. The [security guide](../guide/security.md#scoped-isolation) owns the user-facing limits and costs.

`crates/rimz/src/sandbox/bind_guard.rs::BindGuard` arms before the provider spawns. Its single inotify instance watches the guarded entries' parents for matching `IN_CREATE` and `IN_MOVED_TO` names. One read coalesces its events into one batch, and `IN_Q_OVERFLOW` checks every entry. Unrelated names spawn nothing. An empty set and a non-Linux launch create no instance. A failure to arm refuses the launch with the OS error, and for `EMFILE` or `ENOSPC` names the inotify sysctl to raise.

The exec wrapper points the guard at the spawned provider and stops its worker for the park handoff. The fd, the entries, and the helper path travel in an additive, optional `ParkState.bind_guard` field, and the park version is unchanged. Adoption restores CLOEXEC, restarts the worker, and points it at the retained provider. Every startup retry re-targets the same instance, and providers never inherit it. The headless executor arms the same guard around its bounded execution. Dropping either executor's guard joins its worker and closes the fd before returning.

`crates/rimz/src/sandbox/bind_guard/linux.rs::assembled_view` walks the descendants of the pointed pid through `proc::children`. It selects the first mount namespace that differs from its own, once a child's executable differs from its parent's. With the [current argv](#mount-order), bubblewrap's monitor stays outside the new user and mount namespaces, and its child execs the provider only after assembly. A changed `/proc/<pid>/root` is not enough, because bubblewrap's first pivot precedes the mounts. These facts are pinned to `containers/bubblewrap@v0.11.1:bubblewrap.c::main` and `containers/bubblewrap@v0.11.1:bubblewrap.c::setup_newroot`. Adding `--dev`, a PID namespace, `--disable-userns`, lock files, or a sync fd means revisiting discovery. The view wait is bounded at 5 s. A dead pointed process repairs and records nothing, and a live process that never assembles reports failure.

Each batch starts one transient `rimz --scoped-bind-restore <view-pid> <absolute-path>...` helper, which never stays resident. `crates/rimz/src/main.rs::main` dispatches it before startup threads, since joining a user namespace requires a single-threaded caller. The helper joins the user namespace, then the mount namespace, and performs the mounts itself, because another exec would drop its capabilities. It skips absent and already read-only entries. For each other entry it binds the replacement onto itself and remounts it read-only with the existing `nosuid`, `nodev`, `noexec`, and atime flags from `statvfs`. Repeating the repair adds no stacked mount. Each failed entry writes an escaped path and errno to stderr, and the helper exits nonzero. Its 5 s deadline kills and reaps it.

The guard returns batch outcomes to its executor, which writes `scoped_bind_restored` (`info`, doctor recovered) or `scoped_bind_restore_failed` (`error`, doctor investigate) with the agent identity and paths, plus the failure text on errors ([diagnostic taxonomy](./diagnostics.md#event-taxonomy)). A failed repair leaves the agent running. The sink rate-limits by agent kind, optional name, and paths.

The interval between the rename and the repair stays writable, normally for milliseconds. A guarded entry deleted and then recreated is repaired on creation, but an entry absent at launch is never added. Watches follow the old parent inode, so a moved or replaced parent is not handled. Canonical paths protect symlink targets, not replaced links. A replaced overlay parent that contains the skill view stays writable until restart, since a plain bind would hide the overlay. A replaced root of the SSH mirror also needs a restart. These are mounts against mistakes, not containment.

## Reachable host paths

Replacing `/tmp` and `/var/tmp` would hide any RimZ state, socket, or project that lives under them on the host, so `sandbox::plan` rebinds those paths at their original locations. Paths stay identical inside and outside the view because process ownership comparisons and short Unix socket paths depend on it.

| Candidate | Source |
| --- | --- |
| `HOME`, `RIMZ_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`, `RIMZ_AGENTS_HOME` | Each non-empty value in the launch environment. |
| RimZ home, runtime home, Zellij socket base, tmux socket directory | `mux::domain::ProcessDomain::required_paths`. |
| Working directory, project root, worktree | The launch. |
| Provider config home | Mount step 1, when it exists. |
| Provider's own home | A launch on a named account: `ProviderLogin::default_home`, which its settings entries, and a shared account's history entries, link into. |

Each candidate is normalized lexically. A candidate equal to `/tmp` or `/var/tmp` refuses the launch with `SandboxErr::TmpCollision`, whose message names the path. A candidate below either that exists is rebound, and a candidate nested under one already rebound is skipped. Candidates elsewhere need no mount, since the root bind already shows them.

The tmux endpoint is the server an inherited `$TMUX` names, or RimZ's managed server when `$TMUX` is absent. An unrelated tmux server under `/tmp/tmux-<uid>` is not rebound, so a command inside the pane that targets that default socket directory sees the temp unit instead.

Bubblewrap creates missing mount-point directories beneath the tmp binds. Empty directories named after host paths (`rimz-<uid>` and the like) therefore appear in the temp unit. They are mount points, not leaked host data.

## Environment pins

The plan pins every environment variable it consulted, so shell startup files cannot move provider discovery away from the mounted view, plus fixed values the view itself requires. `CompiledAgentProcess::pin_env` applies the pins as the top layer of the launch environment ([env application](./harness/trust.md#env-application)), after shell startup and before the provider starts.

| Key | Pin |
| --- | --- |
| `HOME`, `RIMZ_HOME`, the five `XDG_*` roots above, `RIMZ_AGENTS_HOME` | Reapplied with the planned value, or removed with `env -u` when absent. |
| `TMUX`, `ZELLIJ_SOCKET_DIR` | Same. The launch sets `ZELLIJ_SOCKET_DIR` first (below), so the pin keeps the wrapper's socket base. |
| The adapter's native override keys (`config_home_env_keys`: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `QWEN_HOME`, `KIRO_HOME`, and others) | Same, so a room account's home key is among them. |
| `TMPDIR` | Always `/tmp`. |
| `RIMZ_SANDBOX_HOST_PATHS` | JSON array of the plan's identity-bound paths, which [hook mapping](#hook-frames) leaves alone. Its presence marks a process as inside a scoped view. |
| `RIMZ_USER_TMPDIR`, `RIMZ_TEMP_ROOT_KEYS` | Always removed, so a child started inside the view keeps `/tmp` (and the provider temp roots that name it), never a host path the view may lack. |
| `SCCACHE_CLIENT_SIDE` | Always `1`, so a shared sccache server never compiles in another view ([toolchain homes](#toolchain-homes)). |

A key present with an empty value is pinned to the empty value. To move a root, export it before launching RimZ or set it in trusted launch environment config: changing it only in the pane's startup files does not change the planned mounts. Finalized provider-account launches keep their raw argv and get the same pins without another shell.

Separately, every launch at every level sets these keys ([env application](./harness/trust.md#env-application), layer 5):

| Key | Value |
| --- | --- |
| `RIMZ_ISOLATION` | `host`, `scoped`, or `sandbox`, from the resolved launch level. |
| `TMPDIR` | The temp unit as the agent sees it: the unit's host path, except under scoped isolation, whose pin replaces it with `/tmp`. Claude's sandboxed commands use Claude's own temp directory. |
| `RIMZ_USER_TMPDIR` | The `TMPDIR` this launch replaced, which every child that outlives the agent's call gets back: the mux chokepoints ([multiplexers.md](./multiplexers.md#the-identity-pin)), every detached child through `child_process::spawn_detached_reaped` (the language-server broker, rimz helpers, ttyd, the Codex daemon, notify handlers), a synchronous loop run, and the browser opener. The wait watcher is the one exception: it runs the agent's own command, so it keeps the unit. The value is the ambient `RIMZ_USER_TMPDIR` when present (a launch inside an agent's tree, whose `TMPDIR` is already a unit), else the ambient `TMPDIR`, else empty, which means the user had none. The scoped pin removes it. |
| The adapter's temp-root keys (`temp_dir_env_keys`: Claude's `CLAUDE_CODE_TMPDIR`) | The same value as `TMPDIR` as the agent sees it, so the provider's own temp files land in the unit on every platform. `/tmp` under scoped isolation. |
| `RIMZ_TEMP_ROOT_KEYS` | The temp-root keys this launch pointed at its unit, space-separated and sorted: the adapter's own keys, empty when it has none. A key the ambient list carries that the adapter does not own names the parent's unit, so the launch unsets it (`rimz agents explain` prints `unset: <KEY>`) unless it sets that key itself. A codex child of a Claude agent therefore has no `CLAUDE_CODE_TMPDIR`. Every restore drops each listed key beside `TMPDIR`. The scoped pin removes it. |
| `RIMZ_SHARED` | The host path of the room's `shared/`. The state root is already reachable in the view, so it needs no mount. |
| `ZELLIJ_SOCKET_DIR` | The Zellij socket base resolved from the exec wrapper's own environment (`ProcessDomain::zellij_socket_base`). Where Zellij ignores `XDG_RUNTIME_DIR` (macOS) it falls back to `TMPDIR` for its socket base. Pinning the base the wrapper resolved keeps the agent on the room's server, and keeps the kill guard's endpoint comparison (`ProcessDomain::same_mux_endpoint_as_process`) true when only `TMPDIR` moved. |

`Isolation::ambient` gives `RIMZ_SANDBOX_HOST_PATHS` precedence over `RIMZ_ISOLATION`: the pin marks a bubblewrap view, and its meaning has not changed across isolation renames. Without the pin, it parses the `RIMZ_ISOLATION` stamp, including a pinless `scoped`, and returns no ambient isolation when the stamp is absent or invalid.

### Hook frames

A hook fired inside a scoped view carries the view's paths, and the host drainer applies it. `crates/rimz/src/cli/hooks.rs::map_sandbox_paths` maps the frame back to host paths when its environment shows ambient scoped isolation, and refuses a scoped frame without `RIMZ_SANDBOX_HOST_PATHS`. It maps the frame cwd, each absolute environment value (`PATH` and the comma lists `CLAUDE_CONFIG_DIR` and `PI_AGENT_DIR` entry by entry, `TMUX` never), and the payload's path fields (`cwd`, `worktree_path`, the transcript paths, and the workspace paths, in each spelling). A path under one of the pinned host-bound paths stays as it is. Any other `/tmp` or `/var/tmp` path maps into `tmp/<RIMZ_AGENT_NAME>/`, the unit named after the frame's own handle. A subagent's view shows its parent's unit, so its `/tmp` paths map to a directory that does not exist (a known defect).

## Temp units

Every agent gets one temp unit, `~/.rimz/ws/<workspace-dir>/tmp/<handle>/`, created at mode `0700` (`StatePaths::temp_unit_dir`, `StatePaths::ensure_temp_unit`). A launch without a handle (a bare `rimz agents exec`, a resume of a row from before launch ids) uses `tmp/_unnamed/`, a name no handle can take. A scoped pane sees its unit at both `/tmp` and `/var/tmp`, and a host agent's `TMPDIR` names it.

### Who owns a unit

A child launched with `parent_agent_id` uses its parent's unit at every level. `launch_plan::compile` resolves the owner once from the parent's row (`address::launch_row`), so a restarted child, which keeps `parent_agent_id` and drops `subagent`, keeps the unit too. When the parent row no longer resolves, the child falls back to its own unit and the launch warns. Its reminder still reads `You share it with your caller`, since a child launches no subagents. `launch_plan::agent_temp_unit` is the same resolution for `rimz agents show`. Peers never share a unit.

The room's `shared/` (`StatePaths.room_shared_dir`) is the one directory every agent in the room reaches, and `$RIMZ_SHARED` names it at its host path at every level. RimZ's result files live in the state `out/` class and print as host paths.

An applied `empty-dir` profile moves only the process cwd, to its directory under `~/.rimz/empty/`. The temp-unit owner and `RIMZ_SHARED` stay, since a child's unit comes from its parent's durable row and never from a process cwd. `RIMZ_WORKTREE_PATH` carries the recorded checkout for lane-aware commands, while provider argv, bubblewrap `--chdir`, hooks, and language-server leases use the process cwd.

### Paths a caller names

`cli::caller_host_path` turns a path a scoped caller names into the host path a launch needs, for an explicit launch `--cwd`, `--system-prompt-file`, or `--append-system-prompt-file`. The prompt files need it because the exec wrapper reads them in the new pane, outside the caller's view. The caller's ambient isolation selects the mapping, never machine policy, so a host shell keeps its paths. A relative path resolves against the caller's cwd before mapping and canonicalization.

The mapping is `sandbox::TmpView::host_path` for the caller's own handle: `/tmp/...` and `/var/tmp/...` become `tmp/<caller handle>/...`. Two cases map wrong. A scoped subagent sees its parent's unit at `/tmp`, but its paths map into a unit under its own handle, which does not exist. A host path rebound under `/tmp` ([reachable host paths](#reachable-host-paths)) also maps into the unit, since this path does not consult the host-bound roots that [hook mapping](#hook-frames) honours. Both are known defects.

Launch records and pane placement use the host path. Only the scoped provider's `--chdir` goes back through the launched agent's `TmpView::agent_path`. `TmpView` holds this one mapping, the owner's unit to `/tmp` and back, and nothing prints a result path through it.

A scoped child of a host parent gets the same `/tmp` reminder line as any child, so a bare `/tmp/x` in its report names the unit only as the child sees it. The host parent finds the file under the unit's host path.

### Lifecycle

A unit is separate from host `/tmp`, not hidden from the host. The state path stays reachable inside and outside the view, and host processes can read the directory. `rimz agents show` prints the agent's unit host path (its parent's for a child) when it exists. A unit is not a store record and carries no fsync guarantee.

| Event | What happens to the unit |
| --- | --- |
| Resumed restart | Kept. |
| Fresh restart | A new handle gets a new unit. A child launched before the restart keeps resolving the old row's unit. |
| Teardown, `rimz uninstall`, a plain session exit, soft reset | Kept. |
| Hard reset | The processes still inside the room's views end, then `tmp/` is deleted ([below](#hard-reset-ends-the-processes-inside-a-view)). |
| GC | Removed after the configurable temp-unit grace (default 1 day). A shared unit is kept while its parent or any launched child is live or has not ended ([store.md](./store.md#what-is-on-disk)). |
| Disk-pressure eviction | May reclaim an ended or rowless unit before the grace, never one with a live or open hold ([automatic sweeps](../reference/cli/maintenance.md#automatic-sweeps)). |

Team memory is a different mechanism, linked from the checkout into room state ([teams.md](./harness/teams.md#the-run-archive)).

### Hard reset ends the processes inside a view

Bubblewrap's `--die-with-parent` reaches only its direct child. A detached descendant of an agent survives the session kill with the unit still mounted at its `/tmp`, and it would refill the tree a hard reset is deleting. After teardown, `rimz reset --hard` therefore ends every holder that `mux::recovery::temp_unit_holders` selects: a process of the caller's uid, outside the caller's own ancestry, whose `/proc/<pid>/root/tmp` has the `(dev, ino)` of a unit directory under `tmp/`, or under the `tmp.reset/` an earlier failed reset left.

The match is the mount, never lineage or the workspace pin. A host-isolation agent's background process, and a process that left the view, are untouched, and a process whose root cannot be read is spared. The match takes in RimZ's own detached daemons that a scoped agent's `rimz` call spawned: a language-server broker and its server, a loop run, a notify handler. The scoped pin removed the saved `TMPDIR`, so such a daemon keeps `TMPDIR=/tmp` in the agent's mount namespace, where `/tmp` is the unit. It can be the writer that fails the reset, and a hard reset run from a host shell ends it. A broker ended this way leaves a registry entry that the next `registry::sweep` keeps while any lease holder lives and removes otherwise, and a replacement broker adopts the kept leases ([lsp.md § Lost rows and revival](./lsp.md#lost-rows-and-revival)).

Holders get the orphan sweep's SIGTERM, grace, SIGKILL, and exit barrier, and the reset enumerates again. A survivor refuses the reset before the store opens. Soft reset, attended auto-reset, `rimz uninstall`, and incompatible-room replacement skip this step.

## System SSH-config mirror

Bubblewrap's unprivileged user namespace maps only the caller's uid. Root-owned files consequently appear owned by the overflow uid, but OpenSSH requires every `Include` target, at any depth and in any `Host` or `Match` block, to be owned by root or the caller and not writable by group or others. The top-level system config is exempt; its usual `Include /etc/ssh/ssh_config.d/*.conf` otherwise fails with `Bad owner or permissions`. Fedora/RHEL nest another checked include: `50-redhat.conf` includes `/etc/crypto-policies/back-ends/openssh.config`. Mirroring the whole Include graph keeps system configuration available to SSH, git over SSH, and scp without overriding the user's SSH command or configuration.

`SandboxInputs.ssh_config_file` supplies the system config file to walk. `launch_plan::compile` passes `/etc/ssh/ssh_config` and `StatePaths.ssh_config_copies_dir`, constructed as `cache/ssh-config` by the disk path module. Planning captures a snapshot without writing; application uses the same `PlannedCopy` publisher as rewritten skills. The source path remains as supplied in `copies[].source`. The root and each reached regular file get a copy, and each canonical file is visited once so cycles terminate. Only the rewritten root is bound, read-only at its canonical path, as the last mount. Its Include lines name cache copies rather than host includes: binding each included file would detach that file's mount whenever a package upgrade renamed a replacement over it.

The Include reader matches the first token in any case, separated from arguments by whitespace or `=`. The `shlex` crate splits the arguments on whitespace and honours single and double quotes, and each argument is globbed. Relative arguments resolve against the input root's directory (`/etc/ssh` in production), not the including file's directory. Every Include line is followed without evaluating `Host` or `Match`. A line whose arguments `shlex` cannot split, such as one with an unbalanced quote, keeps its bytes and is not followed. This reader does not reproduce OpenSSH's tokenizer byte for byte; escaped input paths are not supported.

Each Include line in every copy becomes one `Include` line, with matches in glob order per argument. Mirrored matches name their absolute cache paths; unmirrored matches keep their absolute host paths. Paths are double-quoted, with glob metacharacters escaped, `"` written as `\"`, and a backslash as four backslashes for SSH's tokenizer and glob expansion. An argument matching nothing is dropped. An argument starting with `~` is not globbed or mirrored and keeps its spelling, but it is quoted and escaped like every other path, so a glob in it (`~/.ssh/config.d/*`) matches only a literal `*` (a known defect). A line left with no paths becomes a comment holding the original line. Non-Include lines keep their bytes, and cycles remain cycles among the copies.

| Input or Include match | Mirror behaviour |
| --- | --- |
| Input absent, non-regular, unreadable, or reaching no mirrorable file | No copy or mount. |
| Argument matching nothing | No copy; drop from the rewritten line. |
| Argument starting with `~` | No copy; keep its spelling, quoted and glob-escaped. |
| Regular file | Copy its bytes with Include lines rewritten and mode `& !0o022`. |
| Symlink to a regular file | Copy its target's bytes and masked mode as a regular file, with Include lines rewritten. |
| Dangling symlink | No copy; keep its host path. |
| Directory, FIFO, socket, or device, including through a symlink | No copy or read; keep its host path. |
| Unreadable regular file | No copy; keep its host path; other readable entries remain. |

Copies live at `cache/ssh-config/<sha256>/`, shared by all launches in the room, outside the writable temp unit. Each filename hashes its source's canonical path, avoiding basename collisions across directories. The digest covers entry names, masked modes, and bytes rewritten against a fixed placeholder directory, in canonical-source order under the `ssh-config-v1` domain. This includes kept host paths, so a new dangling glob match changes the digest. The files are then rewritten against the final cache target. `rewrite::apply` creates the private parent only when needed, builds a temporary sibling, and renames it into place; an existing target is reused. Both resets clear these cache-class copies, and GC never age-sweeps them. Their host paths remain writable, like skill-copy sources: the view is not containment.

Changes to included files reach an agent at its next launch or restart: a running view keeps its snapshot across in-place edits, package replacements, and new glob matches. The exception is the root system config itself. If a package or an admin tool renames a replacement over `/etc/ssh/ssh_config`, its bind detaches and SSH reads the host's root-owned includes again, failing with `Bad owner or permissions` until the agent restarts. Includes in the user's own `~/.ssh/config` are not walked. Skipped sources or entries add no launch warning or doctor row. `rimz agents explain` lists the copy and one root mount without publishing them. Host and provider `sandbox` isolation never build this mirror.

## Launch reminder

Every compiled launch names its temp unit and the room's shared dir in the Environment section of its launch reminder (`harness/launch_reminders.rs`, from `LaunchReminders.files`). The two lines follow the `lsp` bullet and precede a team's memory-file listing, where the reminder carries one:

> - tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.
> - shared: ~/.rimz/ws/rimz-f89e/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.

The value after `tmp:` is `/tmp` under scoped isolation and the unit's host path otherwise. A launch whose unit belongs to its parent reads `You share it with your caller` in place of `Your subagents share it`. `shared/` is room-scoped, since every worktree of a repo collapses into one workspace, which is why the line asks for a task-named subdirectory. The reminder names no `out/` path, because every wait message and subagent report carries its own, and it makes no promise about deletion.

A harness keeps its own temp files under the `tmp:` value too. Claude Code reads `CLAUDE_CODE_TMPDIR`, which the launch sets beside `TMPDIR` on every platform, and puts its per-session directory, the scratchpad's parent, at `<unit>/claude-<uid>/<project>/<session>/` (observed on Claude Code 2.1.309).

Claude's routine-permission settings carry the same two directories as `additionalDirectories` and in their `autoMode.environment` text ([adapter_claude.md](./agents/adapter_claude.md)). The reminder's position among the sections, and the providers that receive it, are owned by [fleet.md](./harness/fleet.md#launch-reminders).

## Toolchain homes

The view neither pins nor binds `CARGO_HOME` or `RUSTUP_HOME`. The agent keeps the host `HOME`, so the root bind shows the host toolchain at its host paths: an exported `CARGO_HOME` and `RUSTUP_HOME`, or `~/.cargo` and `~/.rustup` when they are unset. Both stay read-write on purpose. Cargo writes its registry, git checkouts, and installed binaries under `CARGO_HOME` during a build, and rustup writes toolchains and components under `RUSTUP_HOME` when it installs them. A scoped agent can therefore modify the host toolchain, as it can any other host file it can write.

An exported `CARGO_HOME` or `RUSTUP_HOME` below host `/tmp` is not itself a [reachable host path](#reachable-host-paths) candidate. The temp unit hides it unless a rebound candidate such as `HOME` or the worktree contains it. A default `~/.cargo` under a rebound `HOME` stays visible.

A compiler cache wrapper crosses views. Every sccache client forwards compiles to one server over its socket (`SCCACHE_SERVER_UDS`, or TCP port 4226), which the view shares with the host because `/run/user` is not rebound and the network is not unshared. A server-side compile runs rustc in the mount namespace of whichever process spawned the server. A scoped pane's `/tmp` paths then resolve against another view, or against a temp unit that is already gone, and the build fails with `error writing dependencies ... No such file or directory` or exit status 254.

The view therefore pins `SCCACHE_CLIENT_SIDE=1` ([environment pins](#environment-pins)). sccache 0.17 or later then runs rustc inside the client, in the pane's own view, and uses the shared server only for cache storage, so every view still shares one cache. Three conditions bring the failure back: an older sccache, which ignores the key, and a set `SCCACHE_ERROR_LOG` or a configured distributed-compilation scheduler, either of which turns client-side mode off. A client reuses a server that is already running, and a pre-0.17 server does not understand the client-side storage handshake, so run `sccache --stop-server` once after upgrading.

Host launches are not pinned, so a host shell's server-side compile can still land on a server a scoped pane spawned. Set `client_side_mode = true` in the sccache config, or export `SCCACHE_CLIENT_SIDE=1` machine-wide, to close that too. To reproduce the failure, build a fresh crate under `/tmp` with `CARGO_INCREMENTAL=0`, so that every compile goes through sccache.

## Profile skill views

A profile `skills` list decides which user skills the model may invoke on its own. Scoped isolation applies a filesystem view, and host and sandbox isolation use a provider switch where one exists. Config parsing rejects duplicate and invalid names at every level.

### What a list means

`skills = ["merge", "review"]` lists bare skill names, and listed skills stay model-callable. In the scoped view, unlisted skills that RimZ can prepare stay visible but become user-invoked only, and `skills = []` makes every available skill user-invoked only. On the host, the provider switch decides ([host mode](#host-mode)).

An omitted list inherits the parent profile's, and a child's list replaces its parent's instead of extending it. Once a profile in the chain configures a list, no descendant can return to unconfigured behaviour. When no profile in the chain configures one, invocation behaviour is native.

Listing a skill never lifts a native user-only marker. In the scoped view, listed skills are bound with their host metadata unparsed, so an author's own invocation restrictions still apply.

### Host mode

Claude uses one `--settings` value with `skillOverrides[<directory name>] = "user-invocable-only"` for unlisted skills, so explicit `/skill` invocation stays available. Codex uses `-c skills.config=[{name="<frontmatter name>",enabled=false},…]`, falling back to the directory name when the frontmatter has no name, and hides unlisted skills completely, explicit `$skill` invocation included. Other providers and plugins warn that the list is unenforced, then run unrestricted. The warning appears before the provider starts, in `explain`, and in `validate` for host definitions.

`crates/rimz/src/agents/skills.rs::enumerate` reads the launch environment's provider skill root and the RimZ library. The provider wins a directory-name collision, and broken symlinks are skipped. Definition loading and validation share this enumeration and the native marker check only where the list is enforced: scoped-resolved definitions of any kind, and host definitions with `HostSkills::Switch`. Resolution uses the inherited definition isolation default before machine policy. Host definitions without a switch skip both skill lookup and marker checks, and callers already inside a scoped view skip definition checks altogether.

A listed name missing from both roots refuses a switch-provider launch and names both roots. When two directories share one provider key, their policies must agree, or the launch refuses instead of disabling a listed skill. Entry-point preflights supply runtime paths when the resolved isolation is host, and run the same skill resolution and settings rendering as the exec wrapper before launch allocation or pane mutation. The host gate carries the artifact paths it needs, and default reminders do not imply host isolation. Preflight only plans settings artifacts and leaves the writes to launch-plan apply.

Claude keeps the last user `--settings` value, parsing inline JSON or a JSONC file relative to the provider's working directory. It preserves unrelated keys and listed-skill overrides and overlays the unlisted entries. When the user supplies settings, the single flag points to a content-addressed JSON file under `RuntimePaths::prompt_dir`, which launch-plan apply in the exec wrapper writes at mode `0600` in a directory enforced at `0700`. Compilation, `explain` included, only plans the file, and settings contents never enter provider or wrapped argv. Without user settings, the skill-only object stays inline. Unreadable or invalid settings refuse the launch.

Codex replaces CLI `-c` or `--config` overrides for `skills.config`. Entries from `[[skills.config]]` in the user's `config.toml` stay in effect, merged by Codex.

Directory names that a profile list cannot name, such as `odd name`, are still enumerated and always unlisted. Claude uses the raw directory name for their switch keys, and Codex still prefers the frontmatter name.

Neither provider has a wildcard, so skills installed after launch stay callable until restart. Project-chain skills, Codex's `$CODEX_HOME/skills`, and other native roots outside the two enumerated roots keep native behaviour. Host skill files are never rewritten.

### Skill roots and user-only markers

Each scoped launch overlays at most one user skill root, declared by the adapter's `skills_home`, and marks skills user-only with the adapter's `manual_skill`. Both appear in `agents/conformance.rs`.

| Provider | Skill root (`skills_home`) | User-only marker (`manual_skill`) |
| --- | --- | --- |
| Claude | First non-empty `CLAUDE_CONFIG_DIR` entry (default `$HOME/.claude`), plus `/skills` | `Frontmatter` |
| Qwen | `QWEN_HOME` (default `$HOME/.qwen`), plus `/skills` | `Frontmatter` |
| Qoder | `QODER_CONFIG_DIR` (default `$HOME/.qoder`), plus `/skills` | `Frontmatter` |
| OMP | `$HOME/.omp/agent/skills` | `Frontmatter` |
| Codex | `$HOME/.agents/skills` | `OpenAiPolicy` |
| Cursor, Copilot, Droid, Kimi, Pi | `$HOME/.agents/skills` | `Frontmatter` |
| Kiro | `KIRO_HOME` (default `$HOME/.kiro`), plus `/skills` | `Unsupported` |
| Hermes | `HERMES_HOME` (default `$HOME/.hermes`), plus `/skills` | `Unsupported` |
| Muse | `$XDG_CONFIG_HOME/muse/skills` (default `$HOME/.config/muse/skills`) | `Unsupported` |
| Antigravity, Amp, OpenCode, Grok | `$HOME/.agents/skills` | `Unsupported` |
| MiniMax | None | `Unsupported` |
| Plugins | None | `Unsupported` |

A provider marked `Unsupported`, or without a root, refuses every configured list under scoped isolation, whatever skills are installed. `Frontmatter` writes `disable-model-invocation: true` into `SKILL.md` frontmatter. `OpenAiPolicy` writes `policy.allow_implicit_invocation: false` into `agents/openai.yaml`.

The RimZ skill library at `agents_home()/skills` (by default `~/.rimz/skills/`, or `$RIMZ_HOME/skills/` when `RIMZ_HOME` relocates the home) is merged into the provider root, and a name already in the provider root shadows the library entry. Host-mode library links are provider-root symlinks, which the view preserves, so they shadow the library entry the same way. `RIMZ_AGENTS_HOME` is the narrower override that moves only the definition trees and the skill library together, and the library is then `$RIMZ_AGENTS_HOME/skills/`. Project-chain skills and Codex's `$CODEX_HOME/skills` are outside the view and keep their native behaviour.

Definition validation (`crates/rimz/src/config/definitions/agent.rs::skill_policy`) resolves each listed skill through the same two roots in the same order, the adapter's `skills_home` under the ambient environment and then the library, and checks the marker on the copy that wins. It cannot share `skills::plan`, since `config` sits below `sandbox`. A named account's login home is checked when that account launches.

### Building the view

`skills::plan` enumerates the provider root and the library on the host, before any overlay hides them. A skill is a directory that contains `SKILL.md`. Other entries, such as `_shared`, `AGENTS.md`, and `CLAUDE.md`, are carried into the view without copying or rewriting. Broken symlinks are skipped with a debug log.

The view preserves the provider root's shape:

- The tmpfs covers the resolved root, since the root itself may be a symlink. A root that does not exist can be created empty on the host when bubblewrap mounts over it.
- A symlink in the provider root is recreated with its literal target, so a skill script that resolves its own path still reaches its canonical parent and shared sibling modules. Its target is resolved before the overlay hides it.
- Every other entry, and every library entry, is a read-only bind. The host library path itself is shadowed only when a preserved provider-root symlink points into it.

Unlisted skills are rewritten once per canonical directory. A provider-root entry binds its rewritten copy directly. A skill reached through a preserved symlink instead gets one read-only shadow of the copy at its canonical path, which can lie outside the declared skill root. Aliases that resolve to the same skill directory must be all listed or all unlisted. A mix refuses the launch with `ConflictingSkillAliases`, naming both and the fix.

A view exists only when it changes something. With no library entries to merge and no copies to make, there is no tmpfs and no snapshot of the host directory. An unconfigured list never rewrites skills, and RimZ never rewrites or removes host skill files or symlinks.

### The user-only rewrite

The rewrite edits a full directory copy line by line and never touches the host source. `Frontmatter` sets the key in `SKILL.md` frontmatter, prepends a frontmatter block when the file has none, and leaves the document body untouched. `OpenAiPolicy` sets the key under `policy` in `agents/openai.yaml`, creating the file and its `agents/` directory when absent.

Both rewrites accept a limited YAML subset:

- block mappings and block sequences, without anchors, aliases, tags, or explicit `?` keys
- a flow collection (`[...]`, `{...}`) as a value, only when it opens and closes on one line with no anchor, alias, or tag
- quoted values on one line, with block scalars for multiline text
- one top-level indentation, in spaces, and spaces for the mapping that receives the key

Metadata outside the subset makes the skill unpreparable, and [omissions and refusals](#omissions-and-refusals) decide what happens to it. An unlisted skill is never bound unrewritten with implicit invocation still enabled. Tabs are checked only in those two indentations, so a tab-indented line nested deeper passes.

### Rewritten skill copies

Rewritten skills live in `owned/agents/<handle>/skills/<sha256>/` (`StatePaths::agent_skills_dir`). Launches without a handle use `cache/skills/<sha256>/`, which either reset clears and no age sweep touches. Copies sit outside the writable temp unit and are bound read-only at the skill destinations.

`rewrite::digest` hashes the rewrite kind and, for each entry below the skill directory, its relative path, mode, file-or-directory flag, and bytes. A change to any of those produces a new copy. A change to the skill directory's own mode, to timestamps, or to a broken symlink inside it does not. `rewrite::apply` builds a copy in a temporary sibling and renames it into place. A target that already exists is kept, which deduplicates concurrent launches. The `skills/` directory is created only when a copy is needed.

Copies deduplicate within a handle. GC removes the owned directory `owned/agents/<handle>/` once both its newest modification time (the directory and its immediate children) and its newest row's end are seven days old, and never while that row's runtime owner is live. A directory with no row needs only the modification age ([store.md](./store.md#garbage-collection)). Teardown and soft reset keep it, and hard reset drops it. Handleless copies live with the room. Storage reports count both under the workspace state root.

### Omissions and refusals

Whether a problem refuses the launch or drops one skill depends on whether a list is configured and what failed.

| Situation | Result |
| --- | --- |
| No list configured, and the view cannot be built (an unreadable root, an undecodable entry name) | Native discovery unchanged, with a debug diagnostic only. |
| List configured, and the provider has no skill root or an `Unsupported` marker | Launch refused (`SkillsNeedRoot`, `ManualSkillsUnsupported`). |
| List configured, and a listed name is in neither the provider root nor the library | Launch refused (`UnknownSkill`, naming the searched roots). |
| List configured, and aliases of one skill disagree | Launch refused (`ConflictingSkillAliases`). |
| List configured, and listing the provider root or the library fails: an unreadable root, an undecodable entry name, or an entry whose path cannot be resolved, such as a symlink loop (a broken symlink is skipped instead) | Launch refused. |
| List configured, and writing under `skills_dir` fails | Launch refused. |
| List configured, and an unlisted skill has an unreadable file or directory, a directory symlink cycle inside it, a non-file entry, or unsupported metadata | Skill omitted from this launch, and the others proceed. |

An omitted skill is hidden at its discovery path by the tmpfs. `SandboxPlan.skipped` carries a `SkippedSkill` for it, and the exec wrapper prints each one to the pane's stderr before the provider starts, as `rimz: starting without skill "<name>": RimZ cannot …`, naming the source path and the reason. The installed skill is untouched, and the remaining skills keep their invocation restrictions.

## Tests

Unit tests in `sandbox/mod.rs` and `sandbox/rewrite.rs` cover parsing, rewriting, and argv lowering. Filesystem and real-bubblewrap tests live in `crates/rimz/tests/integration/sandbox.rs`, and room messaging and supervised handoff through a view live in the journey tier (`crates/rimz/tests/integration/journey/sandbox.rs`).

Every real-bubblewrap test self-skips when `sandbox::preflight` fails, with a reason starting `bubblewrap unusable:`. `.config/allowed-test-skips.txt` admits that reason because the CI container denies the user namespace. A property of the view itself is therefore proven only on a local non-root run, so a change that relies on one also gets a bubblewrap-free test of `plan` and `apply` (the `sandbox_ssh_config_captures_regular_targets_without_writes` shape). The SSH-mirror tests that run real `ssh` also skip when it is not on `PATH`, and the unreadable-source tests skip as root.

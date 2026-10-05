# Agent sandbox mount views

Sandbox isolation runs each agent's provider process inside Linux bubblewrap with a rearranged filesystem view. It is machine policy, `agents.isolation = "sandbox"` in `config.toml`, and the default is `host`. The view gives every agent a private `/tmp` and `/var/tmp` that its subagents share ([temp units](#temp-units)) and lets a profile decide which skills the model may call ([profile skill views](#profile-skill-views)). Raw command panes and the room's multiplexer stay on the host.

The view is not containment. The host root stays bound read-write, credentials stay visible, the PID, network, and IPC namespaces are shared, so a process an agent starts can outlive it and keep running inside the view, and provider approval flags apply unchanged while provider command sandboxes are switched off, so an approval policy stops prompting for sandbox escalations ([provider command sandboxes](#provider-command-sandboxes)). Trust decides what a repository may run; a sandbox does not make an untrusted command safe ([trust.md](./harness/trust.md#the-executable-surface)).

The code lives in `crates/rimz/src/sandbox/`: `mod.rs` plans the mounts, pins, and bubblewrap argv, `linux.rs` probes bubblewrap, `skills.rs` builds the skill view, and `rewrite.rs` produces the user-only skill copies. `harness/launch_plan.rs` is the one caller that plans and applies a view for a launch.

## Choosing the isolation

A launch resolves isolation through `Isolation::resolve`: the recorded `--isolation` override wins, then the profile's `isolation: host|sandbox`, then current machine `agents.isolation`. Repository profiles in either namespace are refused if they set `isolation`; machine definitions and policy stay outside the trust hash. Team roles inherit the named definition's default and cannot declare their own.

The profile default travels beside `skills` as `isolation_default` through `ResolvedProfile`, `AgentCell`, `ResumeLaunchPosture`, and `ExecRequest`; it never enters `LaunchParams.isolation`. Restart, fork, resume, and rebirth re-read the profile, so editing its default changes the next launch unless a recorded override wins. Cohort resume accepts `--isolation host|sandbox` on both `rimz agents` and `rimz teams`: the flag replaces the stored override, and omission preserves it. Matched seeds are preflighted on that effective isolation.

The exec wrapper stamps `effective_isolation` on `agent.attached` before provider startup, independently of the recorded override. `AgentState::runs_in(machine)` reads this durable stamp; older rows fall back to override then machine policy. Observers read the stamp, while relaunches resolve the current definition rather than replaying the stamp.

A subagent launched without its own `--isolation` inherits its parent's recorded override, not its profile default. Otherwise it follows its own definition and then machine policy. `harness::plan::cap_child_isolation` caps this choice against the parent's effective `RIMZ_ISOLATION`, at supervised child launch and parent-message child resume. Under a sandboxed parent, an explicit or recorded host override refuses with the fix: launch from a host agent or host shell. A host profile default or host machine policy instead clamps to sandbox, prints a note naming the profile and source, and records `Some(Sandbox)` as the child's override so restart and resume preserve it. A host parent or absent ambient isolation imposes no cap. This is a consistency rule, not containment; ordinary agent launches, teams, forks, and loops are not capped. Subagents open their own panes through the multiplexer, so each builds its own view from its own profile and shares the parent's temp unit.

## Preflight

Every entry point that can start a sandboxed agent probes bubblewrap first and refuses with the fix; none falls back to host mode. `sandbox::preflight` runs `bwrap --bind / / --dev-bind /dev /dev --die-with-parent -- /usr/bin/true` from the `bwrap` found on `PATH` and returns its absolute path. The errors are the `SandboxErr` variants `UnsupportedOs`, `MissingBwrap`, and `ProbeFailed`, each ending with what to change.

| Entry point | When it probes |
| --- | --- |
| `rimz config set agents.isolation sandbox` | Before writing the value. |
| `rimz start` and detached room ensure (`cli/room/mod.rs`) | When machine policy is sandbox. |
| `rimz start` recovering a rebirth | When a planned launch resolves to sandbox from its override, current profile default, or machine policy (`RebirthPreview::requires_sandbox`). |
| `rimz agents launch`, `restart`, `fork`, supervised runs | For the launch's effective isolation. |
| Cohort resume (`launch_resume_layout` in `cli/agents_cmd/launch.rs`) | For each resumed agent's effective isolation. |
| Parent-message child resume (`cli/subagents/resume.rs`) | For the child's effective isolation after re-reading its profile. |
| The exec wrapper (`cli/agents_cmd/exec.rs`) | For the launch's effective isolation; a failure marks the launch failed and fails its run with the refusal as its reason. |
| `rimz agents explain` | For the explained launch's isolation. |
| `rimz doctor` | Always; reports mode, binary path, version, probe verdict, and fix. In host mode the check is informational. |

`launch`, `restart`, `fork`, supervised runs, subagent resume, the exec wrapper, and `explain` call `sandbox::preflight_launch` to check skills and then probe. It refuses a configured profile `skills` list under sandbox isolation when the adapter cannot mark skills user-only, so that refusal needs no bubblewrap at all.

## The launch plan

`sandbox::plan` reads the environment, paths, skill directories, and skill source bytes into a `SandboxPlan` without creating anything. The plan holds the ordered `MountPlan`, the environment `pins`, the `skipped` skills, and the rewritten `copies` with their content-addressed targets. `sandbox::apply` writes the copies and ensures the private temp unit; launch planning runs the two separately.

The exec wrapper uses the shared [launch plan](./harness/fleet.md#the-exec-wrapper). `launch_plan::compile` plans a view only for a ready provider process (`AgentProcessStage::Ready`), pins its environment into the compiled process, and lowers the mounts with `sandbox::bwrap_argv`. `launch_plan::apply` creates the launch's temp unit and the room's `shared/`, both mode `0700`, in both isolation modes, then calls `sandbox::apply` on a sandbox launch; `compile` creates nothing. Qwen's login-shell reentry stage runs on the host without a view; its finalized exec is the ready stage that builds one.

[`rimz agents explain`](../reference/cli/agents.md#explain-a-launch) prints the mounts, pins, copy targets, and omissions without applying them. It plans from its own invoking environment plus the launch overrides, which can differ from the target pane's environment.

The wrapped process has three properties a contributor should expect:

- Each pane gets exactly one RimZ sandbox, never nested wrappers. The wrapper runs the absolute bubblewrap path its preflight probed, so a trusted provider `PATH` override does not change it.
- A sandboxed pane never plans another launch's view from its own. It never births a mux server (room birth refuses under ambient sandbox isolation), and it never judges skill listings, since its unlisted skills appear there as RimZ's user-only copies. A launch it requests is planned and judged by the exec wrapper, which the host-born mux server spawns on the host.
- Bubblewrap forks the provider instead of becoming it. `--die-with-parent` ensures that terminating the supervised bubblewrap process also terminates the provider.
- Bubblewrap sets `NoNewPrivs`, so `sudo` and setuid binaries cannot escalate inside the pane. Privileged work belongs in a host shell.

### Provider command sandboxes

Under sandbox isolation the RimZ view is the agent's one sandbox. A provider that nests its own command sandbox inside it (Codex's `workspace-write` bubblewrap binds the root read-only and unshares the network) breaks work the pane itself can do: a build child cannot write `~/.cargo` or reach a local sccache server. The exec wrapper's process compiler therefore calls `LaunchCapability::disable_native_sandbox_args` on the provider's extra arguments for every sandboxed launch, resume, and fork, right after the subagent lockdown; `LaunchReminders.sandbox`, set from the launch plan's bubblewrap path, is the gate, and `rimz agents explain` renders the same argv. Host launches are unchanged, because there the provider's sandbox is the only one.

Codex replaces every `--sandbox`/`-s` flag and `sandbox_mode` override with `--sandbox danger-full-access`, which switches off the command sandbox and leaves the approval flags the mode chose; `--approve-for-me`, which clap refuses beside `--sandbox`, becomes its approval-only expansion. The flags keep their values but not all their effect: with nothing sandboxed, an `on-request` approval policy no longer prompts for commands that would have needed a sandbox escalation, such as network access or writes outside the workspace, and only commands it classifies as dangerous still prompt. Every other adapter, process plugins included, keeps the no-op default until a native switch is verified: Claude runs without a command sandbox by default, and Cursor's `--sandbox disabled` is passed only by its Yolo mode.

### Toolchain homes

The view neither pins nor binds `CARGO_HOME` or `RUSTUP_HOME`. The agent keeps the host `HOME`, so the root bind shows the host toolchain at its host paths: an exported `CARGO_HOME` and `RUSTUP_HOME`, or `~/.cargo` and `~/.rustup` when they are unset. Both stay read-write on purpose, because cargo writes its registry, git checkouts, and installed binaries under `CARGO_HOME` during a build, and rustup writes toolchains and components under `RUSTUP_HOME` when it installs them. The view is not containment here: a sandboxed agent can modify the host toolchain as it can any other host file it can write. An exported `CARGO_HOME` or `RUSTUP_HOME` below host `/tmp` is not itself a [reachable host path](#reachable-host-paths) candidate, so the temp unit hides it unless a rebound candidate such as HOME or the worktree contains it; a default `~/.cargo` under a rebound HOME stays visible.

A compiler cache wrapper crosses views. Every sccache client forwards compiles to one server over its socket (`SCCACHE_SERVER_UDS`, or TCP port 4226), which the view shares with the host because `/run/user` is not rebound and the network is not unshared. A server-side compile runs rustc in the mount namespace of whichever process spawned the server, so a sandboxed pane's `/tmp` paths resolve against another view, or against a temp unit that is already gone, and the build fails with `error writing dependencies ... No such file or directory` or exit status 254. The view therefore pins `SCCACHE_CLIENT_SIDE=1` ([environment pins](#environment-pins)): sccache 0.17 or later then runs rustc inside the client, in the pane's own view, and uses the shared server only for cache storage, so every view still shares one cache. Older sccache ignores the key and stays broken, and sccache turns client-side mode off whenever `SCCACHE_LOG` is set or a distributed-compilation scheduler is configured, so either brings the failure back. After upgrading, run `sccache --stop-server` once, because a client in client-side mode cannot talk to a pre-0.17 server still running. Host launches are not pinned, so a host shell's server-side compile can still land on a server a sandboxed pane spawned; set `client_side_mode = true` in the sccache config, or export `SCCACHE_CLIENT_SIDE=1` machine-wide, to close that too. Reproduce with `CARGO_INCREMENTAL=0` on a fresh crate under `/tmp`, since incremental compiles bypass the cache.

## Mount order

`sandbox::bwrap_argv` emits `bwrap --bind / / --dev-bind /dev /dev --die-with-parent`, then the plan's mounts in this order, then `--chdir <cwd> -- <provider argv>`:

1. The adapter's provider config home (`config_home`), bound at its own path, when it exists. Built-ins resolve it from the effective launch environment, and a room's named account carries its home override there (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`), so the bind follows the room's account. A named account's home holds links into the provider's own home (its settings, and for a shared account its history too), which resolve through the root bind or, outside it, through the [reachable host paths](#reachable-host-paths) rebind; the reconciler runs on the host before the view is built. Plugins declare no config home.
2. The launch's temp unit (`StatePaths::temp_unit_dir`) bound at `/tmp`, then the same unit at `/var/tmp`. Bubblewrap resolves each bind source against the host root, so the second bind shows the unit, not the first mount.
3. Host paths beneath `/tmp` or `/var/tmp` that must stay reachable, rebound at their original paths ([reachable host paths](#reachable-host-paths)).
4. The skill view, when one is needed: a tmpfs over the resolved skill root, one entry per skill, and read-only shadows of rewritten skills at their canonical paths ([building the view](#building-the-view)).

The command has no `--unshare-*` flag, no replacement `/proc`, and no cleared environment. `/dev` takes `--dev-bind` because an ordinary bind breaks device files such as `/dev/null`.

## Reachable host paths

Replacing `/tmp` and `/var/tmp` would hide any RimZ state, socket, or project that lives under them on the host, so `sandbox::plan` rebinds those paths at their original locations. Paths stay identical inside and outside the sandbox because process ownership comparisons and short Unix socket paths depend on it.

| Candidate | Source |
| --- | --- |
| `HOME`, `RIMZ_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`, `RIMZ_AGENTS_HOME` | Each non-empty value in the launch environment. |
| RimZ home, runtime home, Zellij socket base, tmux socket directory | `mux::domain::ProcessDomain::required_paths`. |
| Working directory, project root, worktree | The launch. |
| Provider config home | Mount step 1, when it exists. |
| Provider's own home | A launch on a named account: `ProviderLogin::default_home`, which its settings entries, and a shared account's history entries, link into. |

Each candidate is normalized lexically. A candidate equal to `/tmp` or `/var/tmp` refuses the launch with `SandboxErr::TmpCollision`, whose message names the path. A candidate below either that exists is rebound, and a candidate nested under one already rebound is skipped. Candidates elsewhere need no mount, since the root bind already shows them.

The tmux endpoint is the server named by an inherited `$TMUX`, or RimZ's managed server when `$TMUX` is absent. An unrelated tmux server under `/tmp/tmux-<uid>` is not rebound, so a command inside the pane that targets that default socket directory sees the temp unit instead.

Bubblewrap creates missing mount-point directories beneath the tmp binds. Empty directories named after host paths (`rimz-<uid>` and the like) therefore appear in the temp unit; they are mount points, not leaked host data.

## Environment pins

The plan pins every environment variable it consulted, so shell startup files cannot move provider discovery away from the mounted view, plus fixed values the view itself requires. `CompiledAgentProcess::pin_env` applies the pins as the top layer of the launch environment ([env application](./harness/trust.md#env-application)), after shell startup and before the provider starts.

| Key | Pin |
| --- | --- |
| `HOME`, `RIMZ_HOME`, the five `XDG_*` roots above, `RIMZ_AGENTS_HOME` | Reapplied with the planned value; removed with `env -u` when absent. |
| `TMUX`, `ZELLIJ_SOCKET_DIR` | Same; the launch sets `ZELLIJ_SOCKET_DIR` first (below), so the pin keeps the wrapper's socket base. |
| The adapter's native override keys (`config_home_env_keys`: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `QWEN_HOME`, `KIRO_HOME`, and others) | Same, so a room account's home key is among them. |
| `TMPDIR` | Always `/tmp`. |
| `RIMZ_USER_TMPDIR`, `RIMZ_TEMP_ROOT_KEYS` | Always removed, so a mux child started inside the view keeps `/tmp` (and the provider temp roots that name it) rather than a host path the view may lack. |
| `SCCACHE_CLIENT_SIDE` | Always `1`, so a shared sccache server never compiles in another view ([toolchain homes](#toolchain-homes)). |

Separately, every launch sets ([env application](./harness/trust.md#env-application), layer 5):

| Key | Value |
| --- | --- |
| `RIMZ_ISOLATION` | `sandbox` or `host`, so a process can tell which view it runs in without probing namespaces (`Isolation::ambient` is the one reader). |
| `TMPDIR` | The temp unit as the agent sees it: the unit's host path on a host launch; the sandbox pin replaces it with `/tmp`. |
| `RIMZ_USER_TMPDIR` | The `TMPDIR` this launch replaced, which every child that outlives the agent's call gets back: the mux chokepoints ([multiplexers.md](./multiplexers.md#the-identity-pin)), every detached child through `child_process::spawn_detached_reaped` (the language-server broker, rimz helpers, ttyd, the Codex daemon, notify handlers), a synchronous loop run, and the browser opener. The wait watcher is the one exception: it runs the agent's own command, so it keeps the unit. The value is the ambient `RIMZ_USER_TMPDIR` when present (a launch inside an agent's tree, whose `TMPDIR` is already a unit), else the ambient `TMPDIR`, else empty, which means the user had none. The sandbox pin removes it. |
| The adapter's temp-root keys (`temp_dir_env_keys`: Claude's `CLAUDE_CODE_TMPDIR`) | The same value as `TMPDIR` as the agent sees it, so the provider's own temp files land in the unit on every platform; `/tmp` under the sandbox. |
| `RIMZ_TEMP_ROOT_KEYS` | The temp-root keys this launch pointed at its unit, space-separated and sorted: the adapter's own keys, empty when it has none. A key the ambient list carries that the adapter does not own names the parent's unit, so the launch unsets it (`rimz agents explain` prints `unset: <KEY>`), unless the launch sets it itself; a codex child of a Claude agent therefore has no `CLAUDE_CODE_TMPDIR`. Every restore drops each listed key beside `TMPDIR`. The sandbox pin removes it. |
| `RIMZ_SHARED` | The host path of the room's `shared/`, in both modes. The state root is already reachable in the view, so it needs no mount. |
| `ZELLIJ_SOCKET_DIR` | The Zellij socket base resolved from the exec wrapper's own environment (`ProcessDomain::zellij_socket_base`). Zellij falls back to `TMPDIR` for its socket base where it ignores `XDG_RUNTIME_DIR` (macOS); pinning the base the wrapper resolved keeps the agent on the room's server and keeps the kill guard's endpoint comparison (`ProcessDomain::same_world_as_process`) true when only `TMPDIR` moved. |

A key present with an empty value is pinned to the empty value. To move a root, export it before launching RimZ or set it in trusted launch environment config; changing it only in the pane's startup files does not change the planned mounts. Finalized provider-account launches keep their raw argv and get the same pins without another shell.

## Temp units

Every agent gets one temp unit, `~/.rimz/ws/<workspace-dir>/tmp/<handle>/`, created at mode `0700`; a launch without a handle (a bare `rimz agents exec`, a pre-launch-id resume) uses `tmp/_unnamed/`, a name no handle can take. A sandboxed pane sees its unit at both `/tmp` and `/var/tmp`; a host agent's `TMPDIR` names it. A child launched with `parent_agent_id` uses its parent's unit in both modes: `launch_plan::compile` resolves the owner once from the parent's row (`address::launch_row`), so a restarted child, which keeps `parent_agent_id` and drops `subagent`, keeps it too. A child whose parent row no longer resolves falls back to its own unit and the launch warns; its reminder keeps the child wording (`You share it with your caller`), since a child launches no subagents. `launch_plan::agent_temp_unit` is the same resolution for `rimz agents show`. Peers never share a unit.

The room's `shared/` (`StatePaths.room_shared_dir`) is the one directory every agent in the room reaches, named by `$RIMZ_SHARED` at its host path in both modes. RimZ's result files live in the state `out/` class and are printed as host paths.

For an explicit launch `--cwd`, `--system-prompt-file`, or `--append-system-prompt-file`, `cli::caller_host_path` reverses the calling process's view: `/tmp/...` and `/var/tmp/...` map to the caller's own unit. The caller's `RIMZ_ISOLATION`, not machine policy, selects this mapping; a host shell keeps host paths. The prompt files need it because the exec wrapper reads them in the new pane, outside the caller's view. Relative paths resolve against the calling process's cwd before mapping and canonicalization. Launch records and pane placement use the host path; only the sandbox provider's `--chdir` is lowered back through the launched agent's `TmpView::agent_path`. `sandbox::TmpView` holds that one mapping, the owner's unit to `/tmp` and back, and nothing prints a result path through it.

A sandboxed child of a host parent gets the same `/tmp` reminder line as any child, so a bare `/tmp/x` in its report names the unit only as the child sees it; the host parent finds the file under the unit's host path.

The unit is separate from host `/tmp`, not hidden from the host: the host state path stays reachable inside and outside the sandbox, and host processes can read the directory directly. `rimz agents show` prints the agent's unit host path (its parent's for a child) when it exists. A unit survives a resumed restart; a fresh restart mints a new handle and a new unit, and a child launched before it keeps resolving the old row's unit. A unit is not a store record and carries no fsync guarantee. Teardown, `rimz uninstall`, a plain session exit and soft reset leave it in place; hard reset ends the processes still inside the room's views and then deletes `tmp/`, and GC removes a unit seven days after its owner's latest session ends, under the `owned/` agent-unit rule ([store.md](./store.md)). A child that outlives its parent by more than that grace, or whose parent's handle goes to a new agent after the row is pruned, can lose the unit.

Bubblewrap's `--die-with-parent` reaches only its direct child, so a detached descendant of an agent survives the session kill with the unit still mounted at its `/tmp`, and would refill the tree a hard reset is deleting. After teardown, `rimz reset --hard` therefore ends every such holder (`mux::recovery::temp_unit_holders`): a process of the caller's uid, outside the caller's own ancestry, whose `/proc/<pid>/root/tmp` has the `(dev, ino)` of a unit directory under `tmp/` or under the `tmp.reset/` an earlier failed reset left. The match is the mount, not lineage or the workspace pin, so a host-isolation agent's background process and one that left the view are untouched; a process whose root cannot be read is spared. Holders get the orphan sweep's SIGTERM, grace, SIGKILL, and exit barrier, and the reset enumerates again: a survivor refuses the reset before the store opens. Soft reset, attended auto-reset, `rimz uninstall`, and incompatible-room replacement do not run this step. Team scratch files are a different mechanism that lives in the worktree ([teams.md](./harness/teams.md#scratch-files)).

### Rewritten skill copies

Rewritten skills live in `owned/agents/<handle>/skills/<sha256>/`; launches without a handle use `cache/skills/<sha256>/`, cleared by either reset and never age-swept. Copies sit outside the writable temp unit and are bound read-only at the skill destinations. `rewrite::digest` hashes the rewrite kind and, for each source entry, its relative path, mode, file-or-directory flag, and bytes, so any change to a source produces a new copy. `rewrite::apply` builds a copy in a temporary sibling and renames it into place; a target that already exists is kept, which deduplicates concurrent launches. The `skills/` directory is created only when a copy is needed.

Copies deduplicate within a handle. GC removes the owned unit seven days after its latest session ends, never while its process owner is live; teardown and soft reset keep it, hard reset drops it. Handleless copies live with the room. Storage reports count both under the workspace state root.

## Launch reminder

Every compiled launch names its temp unit and the room's shared dir in the Environment section of its launch reminder (`harness/launch_reminders.rs`, from `LaunchReminders.files`), after the `lsp` bullet and before a team's memory-file listing, where the reminder carries one:

> - tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.
> - shared: ~/.rimz/ws/rimz-f89e/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.

The value after `tmp:` is `/tmp` under the sandbox and the unit's host path otherwise. A launch whose unit belongs to its parent reads `You share it with your caller` in place of `Your subagents share it`. `shared/` is room-scoped rather than worktree-scoped, since every worktree of a repo collapses into one workspace, which is why the line asks for a task-named subdirectory. The reminder names no `out/` path, because every wait message and subagent report carries its own, and makes no promise about deletion. A harness keeps its own temp files under this value too: Claude Code through `CLAUDE_CODE_TMPDIR`, which the launch sets beside `TMPDIR` on every platform, and on 2.1.287 its per-session directory (`<unit>/claude-<uid>/<project>/<session>/`, the scratchpad's parent) lands there.

Claude's routine-permission settings carry the same two directories as `additionalDirectories` and in their `autoMode.environment` text ([adapter_claude.md](./agents/adapter_claude.md)). The reminder's position among the sections and the providers that receive it are owned by [fleet.md](./harness/fleet.md#launch-reminders).

## Profile skill views

A profile `skills` list decides which user skills the model may invoke on its own. Sandbox isolation applies a filesystem view; host isolation uses a provider switch where available. Config parsing rejects duplicate and invalid names in both modes.

### Host mode

Claude uses one `--settings` value with `skillOverrides[<directory name>] = "user-invocable-only"` for unlisted skills; explicit `/skill` invocation remains available. Codex uses `-c skills.config=[{name="<frontmatter name>",enabled=false},…]`, falling back to the directory name when frontmatter has no name. Codex hides unlisted skills completely, including explicit `$skill` invocation. Other providers and plugins warn that the list is unenforced, then run unrestricted. The warning appears before the provider starts, in `explain`, and in `validate` for host definitions.

`agents/skills.rs::enumerate` reads the launch environment's provider skill root and RimZ library, with the provider winning a directory-name collision and broken symlinks skipped. Definition loading and validation share this enumeration and the native marker check only where the list is enforced: sandbox-resolved definitions of any kind, or host definitions with `HostSkills::Switch`. Resolution uses the inherited definition isolation default before machine policy. Host definitions without a switch skip both skill lookup and marker checks; callers already inside a sandbox skip definition checks altogether. A listed name missing from both roots refuses a switch-provider launch and names both roots. If two directories share one provider key, their policies must agree; otherwise the launch refuses rather than disabling a listed skill. Entry-point process preflights supply runtime paths when the resolved isolation is host, running the same skill resolution and settings rendering as the exec wrapper before launch allocation or pane mutation. The host gate carries the artifact paths it needs; default reminders do not imply host isolation. Preflight only plans settings artifacts, leaving writes to launch-plan apply.

Claude keeps the last user `--settings` value, parsing inline JSON or a JSONC file relative to the provider's working directory, preserves unrelated keys and listed-skill overrides, and overlays unlisted entries. When the user supplies settings, the single flag points to a content-addressed JSON file under `RuntimePaths::prompt_dir`, written at mode 0600 by launch-plan apply in the exec wrapper. The artifact directory is enforced at mode 0700. Compilation, including `explain`, only plans the file; settings contents never enter provider or wrapped argv. Without user settings, the skill-only object stays inline. Unreadable or invalid settings refuse the launch. Codex replaces CLI `-c` or `--config` overrides for `skills.config`; entries from `[[skills.config]]` in the user's `config.toml` stay in effect, merged by Codex.

Directory names that a profile list cannot name, such as `odd name`, are still enumerated and always unlisted. Claude uses the raw directory name for their switch keys; Codex still prefers the frontmatter name.

Neither provider has a wildcard. Skills installed after launch remain callable until restart. Project-chain skills, Codex's `$CODEX_HOME/skills`, and other native roots outside the two enumerated roots keep native behaviour. Host skill files are never rewritten.

### Skill roots and user-only markers

Each sandbox launch overlays at most one user skill root, declared by the adapter's `skills_home`, and marks skills user-only with the adapter's `manual_skill`. Both appear in `agents/conformance.rs`.

| Provider | Skill root (`skills_home`) | User-only marker (`manual_skill`) |
| --- | --- | --- |
| Claude | First non-empty `CLAUDE_CONFIG_DIR` entry (default `$HOME/.claude`), plus `/skills` | `Frontmatter` |
| Qwen | `QWEN_HOME` (default `$HOME/.qwen`), plus `/skills` | `Frontmatter` |
| Codex | `$HOME/.agents/skills` | `OpenAiPolicy` |
| Cursor, Copilot, Droid, Kimi, Pi | `$HOME/.agents/skills` | `Frontmatter` |
| Kiro | `KIRO_HOME` (default `$HOME/.kiro`), plus `/skills` | `Unsupported` |
| Antigravity, Amp, OpenCode, Grok | `$HOME/.agents/skills` | `Unsupported` |
| Plugins | None | `Unsupported` |

A provider marked `Unsupported`, or without a root, refuses every configured list under sandbox isolation, whatever skills are installed. `Frontmatter` writes `disable-model-invocation: true` into `SKILL.md` frontmatter. `OpenAiPolicy` writes `policy.allow_implicit_invocation: false` into `agents/openai.yaml`.

The RimZ skill library at `agents_home()/skills` (by default `~/.rimz/skills/`, or `$RIMZ_HOME/skills/` when `RIMZ_HOME` relocates the home) is merged into the provider root; a name already in the provider root shadows the library entry. Host-mode library links are preserved provider-root symlinks and likewise shadow the library entry. `RIMZ_AGENTS_HOME` is the narrower override that moves only the definition trees and the skill library together; the library is then `$RIMZ_AGENTS_HOME/skills/`. Project-chain skills and Codex's `$CODEX_HOME/skills` are outside the view and keep their native behaviour.

Definition validation (`config/definitions/agent.rs::skill_policy`) resolves each listed skill through the same two roots in the same order, the adapter's `skills_home` under the ambient env and then the library, and checks the marker on the copy that wins. It cannot share `skills::plan`, since `config` sits below `sandbox`; a named account's login home is gated at launch.

### What a list means

`skills = ["merge", "review"]` lists bare skill names. Listed skills stay model-callable. In the sandbox, unlisted skills that RimZ can prepare stay visible but become user-invoked only, and `skills = []` makes every available skill user-invoked only. Host behaviour follows the provider switch described above.

An omitted list inherits the parent profile's, and a child's list replaces its parent's instead of extending it. Once a profile in the chain configures a list, no descendant can return to unconfigured behaviour. When no profile in the chain configures one, invocation behaviour is native.

Listing a skill never lifts a native user-only marker. In the sandbox, listed skills are bound with their host metadata unparsed, so an author's own invocation restrictions still apply.

### Building the view

`skills::plan` enumerates the provider root and the library on the host, before any overlay hides them. A skill is a directory that contains `SKILL.md`. Other entries, such as `_shared`, `AGENTS.md`, and `CLAUDE.md`, are carried into the view without copying or rewriting. Broken symlinks are skipped with a debug log.

The view preserves the provider root's shape:

- The tmpfs covers the resolved root, since the root itself may be a symlink. A root that does not exist can be created empty on the host when bubblewrap mounts over it.
- A symlink in the provider root is recreated with its literal target, so a skill script that resolves its own path still reaches its canonical parent and shared sibling modules. Its target is resolved before the overlay hides it.
- Every other entry, and every library entry, is a read-only bind. The host library path itself is shadowed only when a preserved provider-root symlink points into it.

Unlisted skills are rewritten once per canonical directory. A provider-root entry binds its rewritten copy directly. A skill reached through a preserved symlink instead gets one read-only shadow of the copy at its canonical path, which can lie outside the declared skill root. Aliases that resolve to the same skill directory must be all listed or all unlisted; a mix refuses the launch with `ConflictingSkillAliases`, naming both and the fix.

A view exists only when it changes something. With no library entries to merge and no copies to make, there is no tmpfs and no snapshot of the host directory. An unconfigured list never rewrites skills, and RimZ never rewrites or removes host skill files or symlinks.

### The user-only rewrite

The rewrite edits a full directory copy line by line and never touches the host source. `Frontmatter` sets the key in `SKILL.md` frontmatter, prepends a frontmatter block when the file has none, and leaves the document body untouched. `OpenAiPolicy` sets the key under `policy` in `agents/openai.yaml`, creating the file and its `agents/` directory when absent.

Both rewrites accept a limited YAML subset:

- block mappings and block sequences, without anchors, aliases, tags, or explicit `?` keys;
- a flow collection (`[...]`, `{...}`) as a value only when it opens and closes on one line with no anchor, alias, or tag;
- quoted values on one line, with block scalars for multiline text;
- space indentation, consistent at the top level.

Metadata outside the subset makes the skill unpreparable, and the rules below decide what happens to it. An unlisted skill is never bound unrewritten with implicit invocation still enabled.

### Omissions and refusals

Whether a problem refuses the launch or drops one skill depends on whether a list is configured and what failed.

| Situation | Result |
| --- | --- |
| No list configured, and the view cannot be built (unreadable root, undecodable entry name) | Native discovery unchanged; a debug diagnostic only. |
| List configured, provider has no skill root or an `Unsupported` marker | Launch refused (`SkillsNeedRoot`, `ManualSkillsUnsupported`). |
| List configured, a listed name is in neither the provider root nor the library | Launch refused (`UnknownSkill`, naming the searched roots). |
| List configured, aliases of one skill disagree | Launch refused (`ConflictingSkillAliases`). |
| List configured, an unlisted skill has an unreadable source, a symlink cycle, a non-file entry, or unsupported metadata | Skill omitted from this launch; others proceed. |
| List configured, listing the root fails (including an undecodable entry name) or writing under `skills_dir` fails | Launch refused. |

An omitted skill is hidden at its discovery path by the tmpfs. `SandboxPlan.skipped` carries a `SkippedSkill` for it, and the exec wrapper prints each one to the pane's stderr before the provider starts, as `rimz: starting without skill "<name>": RimZ cannot …`, naming the source path and the reason. The installed skill is untouched, and the remaining skills keep their invocation restrictions.

# Agent sandbox mount views

Sandbox isolation runs each agent's provider process inside Linux bubblewrap with a rearranged filesystem view. It is machine policy, `agents.isolation = "sandbox"` in `config.toml`, and the default is `host`. The view gives every agent in a room a private shared `/tmp` ([room tmp](#room-tmp)) and lets a profile decide which skills the model may call ([profile skill views](#profile-skill-views)). Raw command panes and the room's multiplexer stay on the host.

The view is not containment. The host root stays bound read-write, credentials stay visible, the PID, network, and IPC namespaces are shared, and provider approval flags apply unchanged while provider command sandboxes are switched off, so an approval policy stops prompting for sandbox escalations ([provider command sandboxes](#provider-command-sandboxes)). Trust decides what a repository may run; a sandbox does not make an untrusted command safe ([trust.md](./harness/trust.md#the-executable-surface)).

The code lives in `crates/rimz/src/sandbox/`: `mod.rs` plans the mounts, pins, and bubblewrap argv, `linux.rs` probes bubblewrap, `skills.rs` builds the skill view, and `rewrite.rs` produces the user-only skill copies. `harness/launch_plan.rs` is the one caller that plans and applies a view for a launch.

## Choosing the isolation

A launch resolves isolation through `Isolation::resolve`: the recorded `--isolation` override wins, then the profile's `isolation: host|sandbox`, then current machine `agents.isolation`. Repository profiles in either namespace are refused if they set `isolation`; machine definitions and policy stay outside the trust hash. Team roles inherit the named definition's default and cannot declare their own.

The profile default travels beside `skills` as `isolation_default` through `ResolvedProfile`, `AgentCell`, `ResumeLaunchPosture`, and `ExecRequest`; it never enters `LaunchParams.isolation`. Restart, fork, resume, and rebirth re-read the profile, so editing its default changes the next launch unless a recorded override wins. Cohort resume accepts `--isolation host|sandbox` on both `rimz agents` and `rimz teams`: the flag replaces the stored override, and omission preserves it. Matched seeds are preflighted on that effective isolation.

The exec wrapper stamps `effective_isolation` on `agent.attached` before provider startup, independently of the recorded override. `AgentState::runs_in(machine)` reads this durable stamp; older rows fall back to override then machine policy. Observers read the stamp, while relaunches resolve the current definition rather than replaying the stamp.

A subagent launched without its own `--isolation` inherits its parent's recorded override, not its profile default. Otherwise it follows its own definition and then machine policy. Subagents open their own panes through the multiplexer, so each builds its own view from its own profile and shares the parent's room tmp.

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
| The exec wrapper (`cli/agents_cmd/exec.rs`) | For the launch's effective isolation; a failure marks the launch failed and fails its run. |
| `rimz agents explain` | For the explained launch's isolation. |
| `rimz doctor` | Always; reports mode, binary path, version, probe verdict, and fix. In host mode the check is informational. |

`launch`, `restart`, `fork`, supervised runs, the exec wrapper, and `explain` call `sandbox::preflight_skills` before the probe. It refuses a configured profile `skills` list under sandbox isolation when the adapter cannot mark skills user-only, so that refusal needs no bubblewrap at all.

## The launch plan

`sandbox::plan` reads the environment, paths, skill directories, and skill source bytes into a `SandboxPlan` without creating anything. The plan holds the ordered `MountPlan`, the environment `pins`, the `skipped` skills, and the rewritten `copies` with their content-addressed targets. `sandbox::apply` writes the copies and ensures the private tmp directory and the launch's scratch dir; `sandbox::prepare` runs both.

The exec wrapper uses the shared [launch plan](./harness/fleet.md#the-exec-wrapper). `launch_plan::compile` plans a view only for a ready provider process (`AgentProcessStage::Ready`), pins its environment into the compiled process, and lowers the mounts with `sandbox::bwrap_argv`. `launch_plan::apply` ensures the room tmp layout and the launch's scratch dir in both isolation modes, then calls `sandbox::apply` on a sandbox launch, so a launch in a room whose birth never created room tmp (a host-policy birth, for example) still gets it. Qwen's login-shell reentry stage runs on the host without a view; its finalized exec is the ready stage that builds one.

[`rimz agents explain`](../reference/cli/agents.md#explain-a-launch) prints the mounts, pins, copy targets, and omissions without applying them. It plans from its own invoking environment plus the launch overrides, which can differ from the target pane's environment.

The wrapped process has three properties a contributor should expect:

- Each pane gets exactly one RimZ sandbox, never nested wrappers. The wrapper runs the absolute bubblewrap path its preflight probed, so a trusted provider `PATH` override does not change it.
- A sandboxed pane never plans another launch's view from its own. It never births a mux server (room birth refuses under ambient sandbox isolation), and it never judges skill listings, since its unlisted skills appear there as RimZ's user-only copies. A launch it requests is planned and judged by the exec wrapper, which the host-born mux server spawns on the host.
- Bubblewrap forks the provider instead of becoming it. `--die-with-parent` ensures that terminating the supervised bubblewrap process also terminates the provider.
- Bubblewrap sets `NoNewPrivs`, so `sudo` and setuid binaries cannot escalate inside the pane. Privileged work belongs in a host shell.

### Provider command sandboxes

Under sandbox isolation the RimZ view is the agent's one sandbox. A provider that nests its own command sandbox inside it (Codex's `workspace-write` bubblewrap binds the root read-only and unshares the network) breaks work the pane itself can do: a build child cannot write `~/.cargo` or reach a local sccache server. The exec wrapper's process compiler therefore calls `LaunchCapability::disable_native_sandbox_args` on the provider's extra arguments for every sandboxed launch, resume, and fork, right after the subagent lockdown; `LaunchReminders.sandbox`, set from the launch plan's bubblewrap path, is the gate, and `rimz agents explain` renders the same argv. Host launches are unchanged, because there the provider's sandbox is the only one.

Codex replaces every `--sandbox`/`-s` flag and `sandbox_mode` override with `--sandbox danger-full-access`, which switches off the command sandbox and leaves the approval flags the mode chose; `--approve-for-me`, which clap refuses beside `--sandbox`, becomes its approval-only expansion. The flags keep their values but not all their effect: with nothing sandboxed, an `on-request` approval policy no longer prompts for commands that would have needed a sandbox escalation, such as network access or writes outside the workspace, and only commands it classifies as dangerous still prompt. Every other adapter, process plugins included, keeps the no-op default until a native switch is verified: Claude runs without a command sandbox by default, and Cursor's `--sandbox disabled` is passed only by its Yolo mode.

## Mount order

`sandbox::bwrap_argv` emits `bwrap --bind / / --dev-bind /dev /dev --die-with-parent`, then the plan's mounts in this order, then `--chdir <cwd> -- <provider argv>`:

1. The adapter's provider config home (`config_home`), bound at its own path, when it exists. Built-ins resolve it from the effective launch environment, and a room's named account carries its home override there (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`), so the bind follows the room's account. Plugins declare no config home.
2. `StatePaths.tmp_dir` bound at `/tmp`.
3. The launch's scratch dir (`StatePaths::scratch_dir`) bound at `/tmp/scratchpad`, over the room's own `scratchpad/`.
4. Host paths beneath `/tmp` that must stay reachable, rebound at their original paths ([reachable host paths](#reachable-host-paths)).
5. The skill view, when one is needed: a tmpfs over the resolved skill root, one entry per skill, and read-only shadows of rewritten skills at their canonical paths ([building the view](#building-the-view)).

The command has no `--unshare-*` flag, no replacement `/proc`, and no cleared environment. `/dev` takes `--dev-bind` because an ordinary bind breaks device files such as `/dev/null`.

## Reachable host paths

Replacing `/tmp` would hide any RimZ state, socket, or project that lives under host `/tmp`, so `sandbox::plan` rebinds those paths at their original locations. Paths stay identical inside and outside the sandbox because process ownership comparisons and short Unix socket paths depend on it.

| Candidate | Source |
| --- | --- |
| `HOME`, `RIMZ_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`, `RIMZ_AGENTS_HOME` | Each non-empty value in the launch environment. |
| RimZ home, runtime home, Zellij socket base, tmux socket directory | `mux::domain::ProcessDomain::required_paths`. |
| Working directory, project root, worktree | The launch. |
| Provider config home | Mount step 1, when it exists. |

Each candidate is normalized lexically. A candidate equal to `/tmp` refuses the launch with `SandboxErr::TmpCollision`. A candidate below `/tmp` that exists is rebound, and a candidate nested under one already rebound is skipped. Candidates elsewhere need no mount, since the root bind already shows them.

The tmux endpoint is the server named by an inherited `$TMUX`, or RimZ's managed server when `$TMUX` is absent. An unrelated tmux server under `/tmp/tmux-<uid>` is not rebound, so a command inside the pane that targets that default socket directory sees room tmp instead.

Bubblewrap creates missing mount-point directories beneath the tmp bind. Empty directories named after host paths therefore appear in room tmp; they are mount points, not leaked host data.

## Environment pins

The plan pins every environment variable it consulted, so shell startup files cannot move provider discovery away from the mounted view. `CompiledAgentProcess::pin_env` applies the pins as the top layer of the launch environment ([env application](./harness/trust.md#env-application)), after shell startup and before the provider starts.

| Key | Pin |
| --- | --- |
| `HOME`, `RIMZ_HOME`, the five `XDG_*` roots above, `RIMZ_AGENTS_HOME` | Reapplied with the planned value; removed with `env -u` when absent. |
| `TMUX`, `ZELLIJ_SOCKET_DIR` | Same. |
| The adapter's native override keys (`config_home_env_keys`: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `QWEN_HOME`, `KIRO_HOME`, and others) | Same, so a room account's home key is among them. |
| `TMPDIR` | Always `/tmp`. |
| `RIMZ_SCRATCH` | Always `/tmp/scratchpad`. |
| `RIMZ_SHARED` | Always `/tmp/shared`. |

Separately, every launch sets `RIMZ_ISOLATION` to `sandbox` or `host`, `RIMZ_SCRATCH` to the host path of its scratch dir, and `RIMZ_SHARED` to the host path of the room's shared dir ([env application](./harness/trust.md#env-application), layer 5), so a process can tell which view it runs in without probing namespaces and find both directories in either mode (`Isolation::ambient` is the one reader); the pins above replace the host paths under the sandbox.

A key present with an empty value is pinned to the empty value. To move a root, export it before launching RimZ or set it in trusted launch environment config; changing it only in the pane's startup files does not change the planned mounts. Finalized provider-account launches keep their raw argv and get the same pins without another shell.

## Room tmp

Room tmp is one directory per workspace, `~/.rimz/ws/<workspace-dir>/tmp/`, created at mode `0700`. Sandboxed panes see it at `/tmp`. `StatePaths::ensure_tmp_dir` builds its layout:

| Path | Holds |
| --- | --- |
| `agents/<handle>/` | One agent's private scratch files, bound over `/tmp/scratchpad` in that agent's view, as the [launch reminder](#launch-reminder) instructs. |
| `scratchpad/` | Scratch files of a launch without a handle (a bare `rimz agents exec`, a pre-launch-id resume). |
| `shared/` | Files agents in the room exchange on purpose; named to agents as `/tmp/shared` or `$RIMZ_SHARED`, as the [launch reminder](#launch-reminder) instructs. |
| `rimz-waits/` | Watched-command output ([loops.md](./harness/loops.md#watched-commands)). |
| `rimz-subagents/` | Settled child responses ([subagents.md](./harness/subagents.md)). |

Three callers ensure the layout. Room birth does so under sandbox policy, `launch_plan::apply` on every launch (with the launch's scratch dir), and the output writers (wait arming in `harness/schedule/arm.rs`, subagent reports in `cli/agents_cmd/subagent_report.rs`) on demand in either isolation mode. Host mode therefore has room tmp too, for RimZ's own output files.

`sandbox::TmpView` maps host paths to agent paths for output records and messages. Under sandbox isolation a path inside the recipient's own scratch dir becomes `/tmp/scratchpad/<relative path>`, and any other path inside room tmp becomes `/tmp/<relative path>`, so another agent's `agents/<handle>/f` stays `/tmp/agents/<handle>/f`; every other path, and every path under host isolation, stays a host path. `TmpView::current` takes an isolation, falling back to machine policy, and a handle, falling back to the shared `scratchpad/`. Subagent reports pass the parent's `runs_in(machine)` and handle. Wait watchers and signal firing pass neither, so a wait armed by an agent whose isolation differs from machine policy names its output path as machine policy maps it.

Room tmp is separate from host `/tmp`, not hidden from the host. The host state path stays reachable inside and outside the sandbox, and host processes can read the directory directly. `rimz agents show` prints the host path when the directory exists, and the agent's scratch dir host path when that exists.

Every sandboxed agent and subagent in the room sees the same `shared/`, `rimz-waits/`, and `rimz-subagents/`; `/tmp/scratchpad` is each agent's own `agents/<handle>/`, keyed by handle, so it survives restart with the handle. Room tmp survives agent restart but is not a store record and carries no fsync guarantee. `room::teardown::teardown_room` removes it after the process sweep, which covers `rimz reset`, the auto-reset in `rimz start`, and `rimz uninstall`; dead-workspace GC removes it with the state root. A plain session exit leaves it in place, and a reset followed by rebirth can create a fresh empty one straight away. Team scratch files are a different mechanism that lives in the worktree ([teams.md](./harness/teams.md#scratch-files)).

### Rewritten skill copies

Rewritten skills live beside room tmp in `StatePaths.skills_dir`, as `<workspace state root>/skills/<sha256>/`. `rewrite::digest` hashes the rewrite kind and, for each source entry, its relative path, mode, file-or-directory flag, and bytes, so any change to a source produces a new copy. `rewrite::apply` builds a copy in a temporary sibling and renames it into place; a target that already exists is kept, which deduplicates concurrent launches. The `skills/` directory is created only when a copy is needed.

Copies are immutable and stay for the room's lifetime, so a running mount never loses its files. The same teardown, reset, uninstall, and dead-workspace GC paths that remove room tmp remove them. Both directories sit under the workspace state root, so storage reports count them in that root's footprint.

## Launch reminder

A sandbox launch adds one paragraph to the agent's launch reminder. `launch_plan::compile` sets `LaunchReminders.sandbox` when the bubblewrap preflight succeeded, and `harness/launch_reminders.rs` renders `SANDBOX_REMINDER_BODY`:

> This pane runs in a bubblewrap sandbox. `/tmp` is the room's, separate from the host's `/tmp`, removed when the room closes; the host state path stays reachable. Every temporary file you make goes under `/tmp/scratchpad`, which is yours alone: every other agent and subagent has its own. A file another agent must read goes under `/tmp/shared`, in a subdirectory you name for the task. If your harness names a session-specific scratchpad and says to use `/tmp` only when asked, this is that ask: use `/tmp/scratchpad` in its place.

The paragraph gives the agent two paths and the rule that separates them: `/tmp/scratchpad` is per-handle, so a file a parent names for a child lands in a directory the child cannot see, and `/tmp/shared` is the one directory the whole room reaches. It is room-scoped rather than worktree-scoped, since every worktree of a repo collapses into one workspace, which is why the reminder asks for a task-named subdirectory rather than a bare filename. A harness such as Claude Code injects its own environment block that names a session-specific scratchpad and reserves `/tmp` for an explicit request. The reminder supplies that request and replaces the private path outright, so the agent never has to reconcile two rules from two sources.

The paragraph does not name `rimz-waits/` or `rimz-subagents/`, because every wait message and subagent report carries its own file path. Host launches carry the same two rules in its place, naming the variables rather than the paths:

> Your scratch directory is `$RIMZ_SCRATCH`, private to you and removed when the room closes; every temporary file you make goes there, and every other agent and subagent has its own. A file another agent must read goes under `$RIMZ_SHARED`, in a subdirectory you name for the task. If your harness names a session-specific scratchpad and says to use another location only when asked, this is that ask: use `$RIMZ_SCRATCH` in its place.

Every launch therefore carries a reminder. Its position among the reminder paragraphs and the providers that receive it (Claude, Qwen, Droid, and Codex, on every launch kind including subagents) are owned by [fleet.md](./harness/fleet.md#launch-reminders).

## Profile skill views

A profile `skills` list decides which user skills the model may invoke on its own. The list takes effect only under sandbox isolation. Host isolation ignores every list, including `[]` and lists for providers without skill-view support, without a warning, and native discovery and invocation stay unchanged. Config parsing rejects duplicate and invalid names in both modes.

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

`skills = ["merge", "review"]` lists bare skill names. Listed skills stay model-callable. Unlisted skills that RimZ can prepare stay visible but become user-invoked only. `skills = []` makes every available skill user-invoked only.

An omitted list inherits the parent profile's, and a child's list replaces its parent's instead of extending it. Once a profile in the chain configures a list, no descendant can return to unconfigured behaviour. When no profile in the chain configures one, invocation behaviour is native.

Listing a skill never lifts a native user-only marker. Listed skills are bound with their host metadata unparsed, so an author's own invocation restrictions still apply.

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

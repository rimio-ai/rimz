# Agent sandbox mount views

`agents.isolation = "sandbox"` is Linux-only machine policy in `agents.toml`; the default is `host`. It wraps agent panes in bubblewrap, not raw command panes or the room's multiplexer. This is a filesystem view for room scratch and profile skill discovery, not containment: the host root remains writable, credentials stay visible, and PID, network, and IPC namespaces are not unshared. Provider permission modes remain separate.

## Launch and preflight

`rimz config set agents.isolation sandbox` probes bubblewrap before writing. Start and launch preflights refuse non-Linux systems, missing `bwrap`, or a failed mount probe, with a fix rather than a host-mode fallback. `rimz doctor` reports the mode, binary/version, probe verdict, and fix; in host mode the diagnostic is informational.

The exec wrapper reads current machine policy and wraps only the ready provider process. Qwen's login-shell reentry runs first on the host; its finalized exec builds the one view. Each pane gets one RimZ sandbox, not nested wrappers. Subagents launch through the multiplexer and build their own view with their own profile, sharing room scratch. Restart and rebirth pick up the current isolation setting.

Bubblewrap forks instead of becoming the provider. The wrapper always passes `--die-with-parent` so terminating the supervised bubblewrap process also terminates its child. Bubblewrap sets `NoNewPrivs`: `sudo` and setuid privilege escalation do not work inside these panes. Use a host shell for privileged work.

## Mount order

The command starts with `bwrap --bind / / --dev-bind /dev /dev --die-with-parent`, then applies these mounts in order before `--chdir <cwd> -- <provider wrapper>`:

1. Bind the adapter-declared provider config home explicitly, source and target identical today. Built-ins resolve it from the effective launch environment; plugins need not declare one.
2. Bind `StatePaths.scratch_dir` at `/tmp`.
3. Rebind required host paths beneath `/tmp/` at their original absolute paths, so replacing host `/tmp` does not hide the room or its sockets.
4. Overlay each applicable skill root with a tmpfs directory and read-only binds for its visible entries, resolving symlink sources on the host before mounting.

There is no `--unshare-*`, replacement `/proc`, or cleared environment. `/dev` needs a device bind rather than an ordinary root bind for tools using device files such as `/dev/null`.

Host-reach candidates include `HOME`, the five XDG roots, RimZ runtime, project root, worktree, cwd, provider home, and mux endpoint directories resolved by the mux domain. Paths remain identical inside and outside: process ownership comparisons and short Unix socket paths depend on this. A required root equal to `/tmp` cannot coexist with the scratch replacement and is refused. Bubblewrap may create missing mount-point directories beneath the scratch bind; empty host-visible directories in room scratch are therefore expected, not leaked host data. Skill roots may themselves be symlinks: overlays target the resolved directories, while entry sources are resolved before any overlay hides them.

## Room scratch

`${XDG_STATE_HOME:-~/.local/state}/rimz/workspaces/<workspace_id>/tmp/` is ensured at mode `0700` before sandbox room birth and again during launch preparation. Host mode does not create it. `TMPDIR=/tmp` is stamped before process compilation so the login-shell wrapper reapplies it after shell startup files.

All sandboxed agents and subagents in the room see the same scratch files. Scratch survives agent restart and lives at a persistent location, but is not a durable store record and carries no fsync guarantee. Room teardown, including reset and uninstall, removes it after the process sweep; dead-workspace GC removes it with the state root. `rimz agents show` exposes the host path when it exists. Reset with rebirth may immediately create a new empty scratch directory.

## Profile skill views

The roots are the first `CLAUDE_CONFIG_DIR` entry (or `$HOME/.claude`) plus `/skills`, and literal `$HOME/.agents/skills`. `RIMZ_AGENTS_HOME` controls RimZ config fragments, not this skill root. Project-chain skills and Codex's deprecated `$CODEX_HOME/skills` are unscoped.

`skills = ["name[:mode]"]` accepts `auto` (the default) and `off`. Unlisted entries remain visible. A child list replaces its parent's list; an omitted list inherits and `[]` clears it. An empty effective list creates no skill overlay. Every named skill, including an `off` entry, must exist in at least one searched root or launch refuses with those roots. Duplicate names and `manual` mode are rejected; manual invocation needs a future rewritten-copy implementation.

For a non-empty list, existing roots are enumerated on the host before overlays are applied. Visible entries become read-only binds from canonical host sources; broken symlinks are skipped with a debug log. `off` entries are omitted. Host skill files and symlinks are never rewritten or removed. These views control ordinary discovery, not access through another host path: the writable root bind is still present. A non-empty skill list under host isolation refuses rather than silently ignoring the profile.

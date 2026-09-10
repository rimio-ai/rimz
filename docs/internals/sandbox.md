# Agent sandbox mount views

`agents.isolation = "sandbox"` is Linux-only machine policy in `agents.toml`; the default is `host`. It wraps agent panes in bubblewrap, not raw command panes or the room's multiplexer. This is a filesystem view for room tmp and profile skill discovery, not containment: the host root remains writable, credentials stay visible, and PID, network, and IPC namespaces are not unshared. Provider permission modes remain separate.

## Launch and preflight

`rimz config set agents.isolation sandbox` probes bubblewrap before writing. Start and launch preflights refuse non-Linux systems, missing `bwrap`, or a failed mount probe, with a fix rather than a host-mode fallback. `rimz doctor` reports the mode, binary/version, probe verdict, and fix; in host mode the diagnostic is informational.

The exec wrapper reads current machine policy and wraps only the ready provider process, using the absolute bubblewrap path that its preflight probed. A trusted provider `PATH` override does not change the wrapper binary. Qwen's login-shell reentry runs first on the host; its finalized exec builds the one view. Each pane gets one RimZ sandbox, not nested wrappers. Subagents launch through the multiplexer and build their own view with their own profile, sharing room tmp. Restart and rebirth pick up the current isolation setting.

Bubblewrap forks instead of becoming the provider. The wrapper always passes `--die-with-parent` so terminating the supervised bubblewrap process also terminates its child. Bubblewrap sets `NoNewPrivs`: `sudo` and setuid privilege escalation do not work inside these panes. Use a host shell for privileged work.

## Mount order

The command starts with `bwrap --bind / / --dev-bind /dev /dev --die-with-parent`, then applies these mounts in order before `--chdir <cwd> -- <provider wrapper>`:

1. Bind the adapter-declared provider config home explicitly, source and target identical today. Built-ins resolve it from the effective launch environment; plugins need not declare one.
2. Bind `StatePaths.tmp_dir` at `/tmp`.
3. Rebind required host paths beneath `/tmp/` at their original absolute paths, so replacing host `/tmp` does not hide the room or its sockets.
4. Overlay the provider skill root when its view differs from the host directory, using a tmpfs directory and read-only binds for its entries, resolving symlink sources on the host before mounting.

There is no `--unshare-*`, replacement `/proc`, or cleared environment. `/dev` needs a device bind rather than an ordinary root bind for tools using device files such as `/dev/null`.

Host-reach candidates include `HOME`, the five XDG roots, RimZ runtime, project root, worktree, cwd, provider home, and mux endpoint directories resolved by the mux domain. Paths remain identical inside and outside: process ownership comparisons and short Unix socket paths depend on this. A required root equal to `/tmp` cannot coexist with the tmp replacement and is refused. Bubblewrap may create missing mount-point directories beneath the tmp bind; empty host-visible directories in room tmp are therefore expected, not leaked host data. Skill roots may themselves be symlinks: overlays target the resolved directories, while entry sources are resolved before any overlay hides them. Overlaying a missing skill root may create an empty root directory on the host.

The tmux endpoint is the inherited `$TMUX` server, or the managed RimZ server when `$TMUX` is absent. An unrelated ambient server under `/tmp/tmux-<uid>` is not separately rebound; commands targeting that default socket directory see room tmp rather than the host directory.

The plan pins its environment inputs across shell startup: existing root and provider-override values are reapplied, while consulted keys that were absent are removed with `env -u`. Adapters declare their native override keys beside their home resolver. This prevents shell startup files from moving provider discovery away from the mounted view. Export root overrides before launching RimZ, or put them in trusted launch environment config; changing them only inside the pane's startup files does not change its planned mounts. `TMPDIR` is always pinned to `/tmp`. Finalized provider-account launches retain their raw argv and apply the same environment policy without another shell.

## Room tmp

`${XDG_STATE_HOME:-~/.local/state}/rimz/workspaces/<workspace_id>/tmp/` is ensured at mode `0700` before sandbox room birth and again during launch preparation. `StatePaths::ensure_tmp_dir` builds one layout: `scratchpad/` for agent scratch files, `rimz-wakes/` for watched-command output, and `rimz-subagents/` for settled child responses. Host mode creates this layout on demand for RimZ's own output files. The sandbox launch environment pins reapply `TMPDIR=/tmp` after shell startup files.

`sandbox::TmpView` owns the host-to-agent path mapping for output records and messages: paths under room tmp become `/tmp/<relative path>` under sandbox isolation and remain host paths otherwise. `TmpView::current` reads current machine policy for emitters without a launch config. Room tmp is separate from host `/tmp`, not hidden from host processes: the host state path remains accessible inside and outside the sandbox.

All sandboxed agents and subagents in the room see the same temporary files. Room tmp survives agent restart and lives at a persistent location, but is not a durable store record and carries no fsync guarantee. Room teardown, including reset and uninstall, removes it after the process sweep; dead-workspace GC removes it with the state root. `rimz agents show` exposes the host path when it exists. Reset with rebirth may immediately create a new empty tmp directory. Team `scratch-files` config is separate and unchanged.

Rewritten skill copies live beside `tmp/`, under `StatePaths.skills_dir` at `<workspace store>/skills/<sha256>/`. The digest covers the rewrite kind, relative paths, and file bytes. Copies are immutable and deduplicated across concurrent launches; changed sources produce new copies. The private `skills/` directory is created only when needed. Copies remain for the room's lifetime so running mounts retain their files, and teardown, reset, uninstall, and dead-workspace GC reclaim them with the room.

Storage reports include tmp and skill copies in the State root's on-disk footprint; the State category describes their location, not a durability guarantee.

## Launch reminder

The exec wrapper sets `LaunchReminders.sandbox` from its successful bubblewrap preflight and ensures the tmp layout before preparing the mounts, including on restart of an older room. The reminder renderer inserts this paragraph after the model line and before the catalog or child policy, inside the same `<system_reminder>` tag:

> This pane runs under a bubblewrap sandbox. `/tmp` belongs to this RimZ room: teammates and subagents in the room share it, it is separate from the host's `/tmp`, and it is removed when the room closes. The room's host state path remains accessible. Use it freely for temporary files, and use `/tmp/scratchpad` as your scratchpad directory. RimZ writes its own outputs there too: `rimz wake` command output under `/tmp/rimz-wakes/` and settled subagent responses under `/tmp/rimz-subagents/`.

It reaches Claude, Qwen, Droid, and Codex through their existing native append-system-text channels on every launch kind, including subagents. Host-mode launches omit it. Other providers gain no fallback; their child user-prompt fallback remains the no-delegation body only.

## Profile skill views

Host isolation ignores profile `skills` lists, including `[]` and lists for providers without skill-view support, without a warning. Native skill discovery and invocation remain unchanged. Config parsing still rejects duplicate and invalid names in every isolation mode.

Each sandbox launch overlays at most one user skill root, declared by its adapter through `skills_home`:

| Provider | Skill root |
| --- | --- |
| Claude | First `CLAUDE_CONFIG_DIR` entry (default `$HOME/.claude`), plus `/skills` |
| Qwen | Config home (`QWEN_HOME`, default `$HOME/.qwen`), plus `/skills` |
| Kiro | Config home (`KIRO_HOME`, default `$HOME/.kiro`), plus `/skills` |
| Other built-ins | `$HOME/.agents/skills` |
| Plugins | No declared skill root |

The RimZ library at `${XDG_CONFIG_HOME:-~/.config}/rimz/skills/` is merged into that root. A name already present in the provider root shadows the library entry. `RIMZ_AGENTS_HOME` controls RimZ config fragments, not the library location. Project-chain skills and Codex's deprecated `$CODEX_HOME/skills` are unscoped.

`skills = ["merge", "review"]` lists bare names: listed skills are model-callable, while every unlisted skill remains visible but is user-invoked only. `skills = []` makes every skill user-invoked only. An omitted list inherits; a child list replaces its parent's list rather than appending. Once a parent configures a list, a child cannot return to unconfigured behaviour. When no profile in the chain configures a list, native invocation behaviour is unchanged. With no library entries to merge and no copies to rewrite, there is no skill overlay or snapshot of the host directory.

Listing a skill never lifts a native user-only marker: listed skills bind their canonical sources, so an author's existing invocation restrictions still apply.

Without a configured list, an unavailable skill view (for example an unreadable root or an undecodable entry name) leaves native discovery unchanged and only logs a debug diagnostic. A configured list instead refuses those errors before provider execution.

In sandbox isolation, every configured list, including `[]`, requires a provider skill root and a user-only marker. Launch refuses when the provider declares no root, cannot mark skills user-only, or a listed name is absent from both the provider root and the library; the error names the fix. Antigravity, Amp, OpenCode, Kiro, Grok, and plugins refuse a configured list in sandbox mode regardless of installed skills.

Adapters declare the user-only rewrite through `manual_skill`. Claude, Cursor, Copilot, Droid, Kimi, Qwen, and Pi write `disable-model-invocation: true` in `SKILL.md` frontmatter. Codex writes `policy.allow_implicit_invocation: false` in `agents/openai.yaml`, creating that file when absent. These are line-structured edits in full directory copies, not edits to host sources; the frontmatter rewrite leaves the document body untouched. Metadata must use block mappings and block sequences without anchors, aliases, tags, or flow collections. Quoted values must fit on one line; use block scalars for multiline descriptions. Unsupported forms refuse with the source path and the fix rather than leave implicit invocation enabled.

Roots are enumerated on the host before overlays are applied; broken symlinks are skipped with a debug log. When an overlay is needed, listed entries bind canonical host sources read-only, while unlisted entries bind their rewritten copies read-only. Unconfigured views bind canonical sources without rewriting. Host skill files and symlinks are never rewritten or removed. These views control ordinary discovery, not access through another host path: the writable root bind is still present.

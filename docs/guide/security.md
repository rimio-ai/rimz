# Security and Trust

You already run agent CLIs that read your repository, write to it, and run commands as you. RimZ sits beside them at the same privilege: it runs under your user id, with no elevated privileges and no account of its own, and its durable state is a directory of plain files rather than a service. It cannot give an agent a permission you have not already given it.

The design commitments behind everything below are in [DESIGN.md](../../DESIGN.md).

## What RimZ changes on your machine

Five things, plus one file in a room running Copilot. Each one's removal is named with it; provider folder-trust decisions stay yours even after uninstalling RimZ.

**Reporting hooks and coordination rules in your agent CLIs' own configs.** To show an agent live in the sidebar, RimZ adds a hook to that agent's own configuration: a line in `~/.claude/settings.json`, a block in Codex's `$CODEX_HOME/config.toml` (`~/.codex/config.toml` by default), and for Copilot, Amp, Pi, and OMP, whose extension point is a file rather than a config entry, one RimZ-owned integration file. OpenCode gets two marked integration files in `plugins/rimz`, replacing a marked file from an older RimZ install. Run `rimz hooks install --dry-run` for the exact per-agent diff before anything is written. Hook installation preserves your own hooks, and the hooks only report events back to RimZ. Answering a prompt stays with you, in the agent's own UI. Codex also gets a managed `rules/rimz.rules` file for the [routine-command permissions](#what-can-run-a-command) below. `rimz hooks uninstall` removes exactly what was added and restores any statusline RimZ wrapped. The whole flow is walked in [set up your machine](./setup.md#install-agent-hooks).

**Skill links in each provider's skill directory.** Unless the agent launches into the [scoped view](#scoped-isolation), RimZ links the selected skills from `~/.rimz/skills/` and installed packs into that provider's declared skill root. Your own directories and links are never replaced, a name collision is reported at launch, and `rimz uninstall` removes RimZ's links and keeps the library and packs. The precedence and ownership rules are in [configuration → skills](./configuration.md#skills).

**Files under `~/.rimz/`.** Configuration and durable room state, plain files you can read and edit, with `RIMZ_HOME` relocating all of it. One subtree deserves care: `accounts/` holds the provider homes `rimz accounts add` creates, each with its own credentials and transcripts but [settings shared with the provider's own home](./accounts.md#declare-an-account). Treat it the way you treat `~/.ssh`, and keep it out of a dotfiles repository you publish. The layout is in [where RimZ keeps its files](./configuration.md#where-rimz-keeps-its-files).

[Packs](../reference/cli/packs.md) supply machine definitions and skills from Git or a linked checkout. Before activating a Git install, RimZ shows the packs, dependencies, item fates, and each added or changed launchable profile's resolved provider kind and effective mode, isolation, tools, and allowed-tools, including inherited grants and provider defaults. Widened fields are marked. A change of provider kind is printed but is not a widened field, so an update whose only change is a profile's kind is not held. The same fields can grant different capabilities under another provider (a Codex profile with `tools: [Read]` still edits the workspace through patches), so read the printed kind. A terminal asks for confirmation unless the source is unchanged; without a terminal, `--yes` is required. It skips the question, not the summary. Fetching and staging happen before consent, but the installation record and active entries are not changed until approval. A changed installation record aborts the commit and requires a fresh review.

`rimz packs update` skips linked and pinned sources. Other sources are reviewed one at a time: updates that do not widen effective profile grants apply without asking; widening updates ask on a terminal and are held when declined. Without a terminal, widening updates are held with exit 1 unless `--yes` approves them. The report names the packs, widened fields, and review command. Holds are not stored, and one failed or held source does not prevent other sources from updating. `--yes` skips only the question, not the review.

A linked `rimz packs install <path>` records and links every pack in that checkout without fetching or a summary prompt; later edits load without reinstalling. Neither Git nor linked installation sandboxes definitions: their grants, including `allowed-tools`, apply like your own home definitions. Review the source contents too; the profile summary does not parse skill frontmatter allowed-tools. `rimz packs remove <pack>` stops loading it and prunes obsolete owned skill links. Linked checkouts are never deleted; owned Git checkouts are collected by later pack mutations. Pack mutations are not routine-command grants; only `rimz packs list` and `rimz packs show` are.

**A permission grant for the Zellij presence plugin.** On Zellij, RimZ seeds a permission grant for the plugin it ships so your first attach is not interrupted by a prompt. Your `config.kdl` stays untouched, and the grant is yours to revoke. [The Zellij presence plugin](#the-zellij-presence-plugin) has it in full.

**Agent folder-trust decisions, when you approve them.** RimZ can write Codex, Claude, and Qoder's own trust settings after showing the proposed changes. Undo those decisions in the provider's configuration, not with `rimz trust revoke`; [folder trust for agents](#folder-trust-for-agents) explains what they enable.

A room running Copilot gets one more file. RimZ points Copilot's own telemetry exporter at `agent-telemetry/copilot-otel.jsonl` under the room's runtime directory on tmpfs, with message-content capture switched off, and reads the model and token counts for that conversation back out of it. Closing the room removes it, and an exporter you configured yourself is left alone.

`rimz uninstall` removes RimZ from the machine. It prints every root, room, hook, skill-link root, and binary it is about to touch, waits for a `y`, and then reports each removal; `--state` and `--config` add the durable stores and the per-machine config to that list. Your definition trees, skill library, installed `packs/`, and provider account homes under `~/.rimz/accounts/` survive every mode. Your credentials are in the account homes, so clearing them is yours to do by hand. The preview names each declared account home that exists and gives the removal command. The full table of what goes and what stays is in the [maintenance reference](../reference/cli/maintenance.md#uninstall-rimz).

Default-on [agent hibernation](./configuration.md#agent-hibernation) freezes provider processes and reclaims memory through delegated Linux cgroup controls where available. These scopes manage resources, not access: another process running as your user can write their freeze and reclaim controls. Hibernation does not add a security boundary to host or scoped isolation.

## What can run a command

Inside a workspace, plenty already runs as you: hooks, postinstall scripts, generated binaries, test runners, and the agents themselves. Same-user isolation is no real boundary there, so RimZ does not lean on it. Instead it makes command execution an explicit choice, and three things in your configuration can make that choice for you: a repository's config, which stays inert until you trust it, and your own notification handlers and worktree hooks, which a repository cannot supply.

RimZ also gives Claude and Codex a curated permission list for [routine coordination](./fleet.md#set-a-permission-mode), on by default: launch settings for Claude, and `$CODEX_HOME/rules/rimz.rules` installed by `rimz hooks install` for Codex. A matching Codex command runs unsandboxed, skipping Guardian and permission hooks unless a stricter user or managed rule applies. That list lets an agent widen what other agents may do, with no prompt:

- The `rimz agents` and `rimz subagents` rules cover launches as well as inspection. `rimz subagents claude "…" --yolo` is capped by the parent's recorded permission mode (unrecorded counts as auto), and higher child defaults are lowered. That consistency rule is not containment: an agent can still run `rimz agents claude --yolo …` to launch an independent peer, or `rimz subagents claude "…" -- --dangerously-skip-permissions`, since arguments after `--` pass through unfiltered. The launched agent then runs with no permission checks at all.
- The `rimz teams` rule covers the whole command group, not only inspection and board hand-offs. An agent can launch a team, and stop, restart, or resume a cohort. A team launch takes no `--yolo` and no passthrough arguments, so its members run as their definitions say, but `rimz teams resume <team> --yolo` saves a permission mode that skips the provider's prompts for every resumed member. A launch or resume also accepts `--isolation`, which replaces the machine's `agents.isolation` for that cohort. An agent can also run `rimz teams install <bundle>`, which downloads team and agent definitions from the RimZ repository into `~/.rimz/`: from the matching release by default, or from any tag or branch named with `--ref`. With `--force` it replaces the definitions already there, and later launches run what those files say.
- The `rimz answer` rule lets an agent answer another agent's permission prompt, approving it on your behalf.

To remove all of these rules, set `allow-routine-rimz = false` under `[agents]` in `~/.rimz/config.toml` ([routine RimZ commands](./configuration.md#routine-rimz-commands)). Restart Claude to drop its launch settings; the next `rimz start` consent or `rimz hooks install` removes Codex's managed rules file. This is a machine setting, so a repository cannot turn it back on. Codex also reads that user-level rules directory outside RimZ. Named accounts share it, including Codex's own “don't ask again” rules; an existing account-local `rules/` directory is moved aside with a warning when the shared link is reconciled.

Claude also receives exact `Skill(<name>)` permissions for profile-listed skills, plus installed `rimz-*` skills while the routine switch is on. A Skill permission lets that skill's `allowed-tools` apply unprompted for the invoking turn. Routine skill rules use the provider, library, and pack chain, not names discovered in the project. Profile-listed names receive their exact rules independently of discovery. Remove the profile's `skills:` list to remove its rules, and turn off `allow-routine-rimz` to remove the installed-`rimz-*` rules. If a name comes from both sources, remove both sources.

A Markdown definition's `allowed-tools` is a standing grant, not a skill's per-invocation permission. For example, `allowed-tools: ["Bash(git *)"]` lets that Claude profile run matching Git commands without a permission prompt, including reset and force-push. It applies to agents, subagents, and team roles in all isolation levels, even with `allow-routine-rimz` off. Inspect the configured rules with `rimz agents profiles`, `rimz subagents profiles`, or `rimz teams profiles`. Remove the list where it is defined, or override an inherited list with `allowed-tools: []`, then restart the agent to remove that grant. Other providers warn and ignore the list. Claude's managed policy and explicit ask/deny rules still apply, and auto mode drops some broad rules rather than honoring them; see the [rule syntax and auto-mode limits](../reference/definitions.md#tools-to-provider-arguments).

### Project trust

A cloned repository can ship a `.rimz/config.toml` that declares agents, profiles, teams, loop tasks, hooks, environment variables, and which provider account an unpinned room follows. Each of those can run a command or redirect your credentials, so RimZ keeps the whole file inert until you trust the workspace. On an untrusted clone nothing the file declares can launch; RimZ parses it only far enough to tell you what it is asking for.

A first interactive `rimz start` in an untrusted workspace offers the grant and names what the config declares. The interactive `rimz loop fire` and the project-task edit commands offer it inline when they meet the closed gate, with the field-level diff when a grant has gone stale. Declining leaves the workspace inert and suppresses only the first-start offer; the inline offers keep coming, and the first-start offer returns as soon as a command-running field in `.rimz/config.toml` changes.

`rimz trust grant` pins a single hash over every command-running field in that config. Every later read re-hashes the live file, so drift is caught at each read; `rimz trust status` and `rimz doctor` both re-hash on the spot.

Edits outside RimZ's own project-task commands, a direct file edit by an agent in the room included, make that hash drift. The workspace flips to `stale`, command execution turns back off, and `rimz trust status` prints a field-level diff of what changed before you re-grant. `rimz loop add`, `remove`, and `rename` are the exception: when they make the edit from an already-trusted workspace, or when they create the first config, they re-pin the grant themselves, whether you run them by hand, from a script, or from an agent. From an untrusted or stale workspace they leave the review to you.

`rimz trust revoke` drops the grant. The project config is inert again at the next read; panes already running keep what they launched with until they stop.

Room layout stays out of a repository's reach. A project config carrying a `[layout]` table is refused outright, and the error names what replaced the table: `placement` under `[agents]`, and a team definition for a whole room shape. The table goes rather than moves, because per-machine config has no `[layout]` either.

The hash covers every field that can cause a process to run:

- `[[agents]]`: `name`, `launch_command`, `env`
- `[profiles.<name>]`: `agent`, `skills`, `mode`, `model`, `effort`, `auto-compact`, `system-prompt-file`, `append-system-prompt-files`, `args`
- `[subagents.profiles.<name>]`: the same command-running profile fields, for supervised children
- `[agents.teams.<name>]`: `layout`, `consensus-file`, `append-system-prompt-files`, and each role's profile, signal bindings, and launch fields
- `[tasks.<name>]`: the loop `agent`, `prompt`, `prompt-file`, `check` (a shell command or the agent table's `agent`, `prompt`, `prompt-file`, `recheck`, `timeout`), `verify`, `system-prompt-file`, and the run and schedule options
- `[[hooks]]`: `event`, `command`
- `[env]`: every key and value
- `[lsp.servers.<name>]`: server name, executable `command`, `init-options`, and explicitly set `editor-check-on-save`; untrusted declarations warn at agent launch and stay inert, leaving same-named machine entries in effect, and the trust offer names their commands
- `[accounts]`: which provider account an unpinned room follows live

Every name in those tables is hashed too, so renaming a profile drifts the grant. Any field RimZ can execute has to enter this hash, and a unit test guards the projection by hashing one config per field, every field in the lists above included, and failing when two of them collide. The mechanics, the grant record, and the diff are in [the trust internals](../internals/harness/trust.md); the command is in [the trust reference](../reference/cli/hooks-trust.md#project-trust).

Per-machine loop schedules are a separate matter. A `check = "<shell>"` line in your own `~/.rimz/loop.toml` is your command, not a repository's. A clone can supply only project `[tasks]`, and those need both a trust grant for the config and a machine-local `rimz loop enable <name>` before they run unattended. Until trust, a same-named machine task remains the runnable definition; after trust, the project definition is visible but disabled here by default.

### Folder trust for agents

Codex, Claude, and Qoder can stop at a folder-trust screen before they report anything to RimZ. `rimz start` offers to record the decision for detected agents, under the accounts the room will use. `rimz doctor` and `rimz trust` show which agents remain undecided. Human interactive launches, including `--bg`, warn and let you answer in the agent's pane. Agent-started agent and team launches instead refuse with the grant command to ask you to run, or the option to relaunch from a directory the account already trusts.

A Codex folder saved as untrusted shows Folder access on every launch. RimZ leaves that decision alone: on a human launch, pick Open restricted in the pane, or trust the folder in Codex. Human agent launches and restarts warn; fresh agent-started launches, supervised runs, team commands, and resident loops refuse rather than wait indefinitely for that screen. A fresh agent-started launch asks you to trust the folder in Codex or relaunch from a directory the account already trusts; it opens no pane. Start prints the fix but never offers to grant a saved-untrusted folder. A folder without a project or saved decision needs no Codex consent.

A folder grant tells the provider to accept that folder, not just its RimZ configuration. For a repository, Codex can then load its `.codex/` configuration, hooks, and rules; Claude accepts the project for its normal interactive tooling; Qoder enables project settings and hook execution. RimZ shows the exact file changes before writing and asks with No as the default. `rimz trust grant --agents` and the machine-wide [`--empty-dir` grant](../reference/cli/hooks-trust.md#empty-directory-folder-trust) are explicit consent without a question, still with the changes displayed. Without a terminal or either flag, provider files stay untouched.

A Codex or Claude project-folder grant covers the main repository and its worktrees, in this room's selected account home. Codex grants in a shared `config.toml` apply to every account linked to that file; Claude's `.claude.json` stays per account. Qoder trusts the current worktree directory and its descendants, so a sibling worktree needs its own grant unless a trusted ancestor covers both. Run `rimz trust grant --agents qoder` there to preview and append that directory to Qoder's user trust list. A declined start offer stays quiet for those agent kinds until `rimz trust grant`; a newly detected undecided kind gets its own offer. To undo a grant, remove the relevant trust decision from Codex's `config.toml`, Claude's `.claude.json`, or Qoder's `permissions.trustDirectories` in `settings.json` at the path the grant printed. `rimz trust revoke` and uninstall do not change those provider-owned decisions. The [trust reference](../reference/cli/hooks-trust.md#agent-folder-trust) lists the flags and states.

### Notification handlers

Notification handlers run a command of your choosing when a card needs attention (`[[notifications.handler]]`, or the legacy `[notifications].command`). They live only in your per-machine `~/.rimz/config.toml`, so a clone can never supply one, and they sit outside the trust hash for the same reason. They run under your user id, spawned by the sidebar process, and often carry local push credentials. A handler that acts back on the room should treat pane text and transcripts as untrusted data: match a bounded prompt shape, and stay silent on anything else. Wiring is in [the notifications internals](../internals/sidebar/notifications.md).

### Worktree hooks

Worktree hooks run your commands when RimZ creates or removes a tree (`[agents.worktree.hooks]`). Like notification handlers, they live only in your per-machine `~/.rimz/config.toml`, outside the project trust hash; a clone cannot supply or override them. They run under your user id, spawned by whichever `rimz` process creates or removes the tree. When that process is inside an agent's scoped view, the hook inherits that view, including the room's `/tmp`; it is not a separate host-side service. Setup and teardown are in [prepare the tree with a hook](./worktrees.md#prepare-the-tree-with-a-hook).

### An unattended run

An unattended run changes who is watching, not what can execute. The run still decides permissions the agent's own way: either you keep every native prompt, so each one becomes a waiting card that routes to you, or you pass the agent's own bypass flag. The tradeoffs, and what an out-of-range agent version does to the prompt path, are in [permissions for an unattended run](./loops.md#permissions-for-an-unattended-run).

## Provider sandbox isolation

Claude Code and Codex each ship a command sandbox that you turn on with a flag or a setting. `rimz config set agents.isolation sandbox` adds that flag to every later launch, so one setting covers every agent in the room. RimZ adds the flag and does nothing else: the sandbox, its rules, and what happens when it cannot start are the provider's, the same as when you pass the flag yourself. This level adds no RimZ mount view.

| Provider | What RimZ adds at `sandbox` |
| --- | --- |
| Claude Code | `sandbox.enabled: true` in the launch's merged `--settings`. |
| Codex | `--sandbox workspace-write`, only when the launch's arguments and the [Codex config layers RimZ reads](../reference/agent-support.md#permission-modes) select no sandbox mode. A mode you select, including `danger-full-access`, a permission profile, or Yolo, stays, and RimZ adds nothing. |
| Every other provider | Nothing. The launch prints a warning and starts as it would at `host`. |

What these flags do is in the provider's own documentation: [Claude Code sandboxing](https://code.claude.com/docs/en/sandboxing) and [Codex sandboxing](https://learn.chatgpt.com/docs/sandboxing). RimZ does not check the provider's dependencies, override its fallback, or comment on your permission mode.

At every level, RimZ also adds flags for its own directories and commands. It grants write access to the room's state and runtime directories where the provider's permissions accept a grant (Claude's `sandbox.filesystem.allowWrite`, Codex's writable roots), and with `allow-routine-rimz` on it adds its routine coordination commands (Claude's `sandbox.excludedCommands`, Codex's `prefix_rule` entries). The [permission modes reference](../reference/agent-support.md#permission-modes) lists the exact arguments.

The child consistency cap orders `host` < `scoped` < `sandbox`: a looser explicit child override refuses, and a looser default clamps to the parent's level with a note. A child capped to `sandbox` whose provider has no sandbox switch warns and launches without one. The cap reads `RIMZ_ISOLATION` and the scoped view's `RIMZ_SANDBOX_HOST_PATHS` pin; an agent can strip both environment variables, so it prevents mistakes, not deliberate escape. Ordinary peer launches are outside that cap. Restore `agents.isolation host` and restart agents to stop requesting provider sandboxes; recorded launch overrides and profile defaults still win.

## Scoped isolation

An agent pane runs with everything your user account has: your whole home directory, the host's `/tmp` shared with every other process on the machine, and the provider's native skill discovery, subject to [host skill lists](./configuration.md#skills) on Claude and Codex. For a narrower view on Linux, `rimz config set agents.isolation scoped` gives each agent pane a bubblewrap mount view.

Inside the view the agent sees its own `/tmp` instead of the host's, and the RimZ library and pack skills merged into its provider's skill root in the [same precedence order](./configuration.md#skills). When the profile carries a skills list, only the skills you named stay callable by the model; the rest become skills you invoke by name. RimZ probes bubblewrap before writing the setting. When it is missing or unusable, room start warns and records the problem, while a scoped launch refuses with the fix. The warning prints `rimz config set agents.isolation host` as a security change you may choose to run yourself. RimZ never applies it for you or silently launches an agent without its requested scoped view.

This is a mount view, not containment. What it does not do:

- Most of the host filesystem stays writable and your credentials stay readable. The existing configuration paths listed below are read-only.
- Each agent's `/tmp` is a different directory, not a private one: any agent in the room can still read and write another's at its host path under the room's state.
- Processes, networking, and IPC are shared with the rest of the machine.
- Overlaid skills are bound read-only at their discovery paths, but their sources on the host are unchanged and may still be reachable by another path.
- No isolation level hides an agent's command line from a same-user `ps`. RimZ passes a composed system prompt by private file for that reason. Pi is the exception: its replacement surface is the `--system-prompt` flag, so a Pi prompt rides the command line, capped at 120 KiB.

Bubblewrap does add one protection: it turns off privilege escalation, so `sudo` and setuid programs cannot elevate inside these panes. Use a host shell for that work.

Codex keeps its own sandbox and network choices in both host and scoped isolation; RimZ adds only write access for the room's directories within a workspace-write sandbox or your own permission profile. Auto escalates other access to Guardian; interactive Ask asks you. The [routine-command rules](#what-can-run-a-command) bypass that sandbox for matching commands. Yolo bypasses both sandbox and approval checks. Claude also keeps your sandbox choice: RimZ grants its sandbox write access to room directories, excludes routine RimZ commands when that setting is on, and permits nesting under scoped isolation. Muse is the exception: RimZ passes `--disable-sandbox` inside the view because its own sandbox cannot reliably nest there.

Scoped isolation binds existing shell startup files (`~/.profile`, Bash's `.bashrc`, `.bash_profile`, `.bash_login`, `.bash_logout`, Zsh's `.zshenv`, `.zprofile`, `.zshrc`, `.zlogin`, `.zlogout`, and `~/.config/fish`), `~/.gitconfig`, `~/.ssh`, and `~/.config/systemd/user` read-only. It also protects the launch checkout's common Git directory's `hooks/` and `config`, including for linked worktrees, and `.claude`, `.codex`, `.agents`, and `.mcp.json` at both the launch checkout and room project roots. An agent started in an empty directory protects project configuration only at the room project root, without protecting an ancestor's Git directory. A symlinked entry protects its target, not the link itself: an editor's write-then-rename or `ln -sf` can still replace the link. An entry whose target is, lies inside, or contains the agent's own account home (or the provider home a named account links into) stays writable, so the provider can save its sessions and settings. This exception applies to startup files too. Custom `core.hooksPath` locations are not followed. Missing entries are skipped, not created, so an agent can still create a missing configuration path. While an agent runs, a host program that replaces a protected entry, such as git rewriting its config, an editor saving by rename, or a package manager, briefly leaves it writable. RimZ normally binds the replacement read-only again within milliseconds, and `rimz doctor` lists repairs and any repair that failed. An entry containing the agent's skill view stays writable after the whole directory is replaced until the agent restarts. A scoped launch refuses when the system cannot watch the protected files and names the limit to raise. These mounts prevent mistakes, not deliberate attacks through another path.

The view also reads the system SSH config through a launch snapshot of the files it includes, at any depth, using copies you own, so SSH and git over SSH can read the host's system configuration. A change to an included file, a package upgrade included, reaches an agent at its next launch or restart. If `/etc/ssh/ssh_config` itself is replaced while an agent runs, that agent's SSH fails with `Bad owner or permissions` until you restart it. An Include in your own `~/.ssh/config` naming a root-owned file is not covered.

Those protections have costs. `git config` (local or global), `git remote add`, and `gh auth setup-git` fail. `git push -u` and `git worktree add` from a remote base can succeed without saving tracking configuration. SSH cannot update `known_hosts`, and Claude cannot save “don't ask again for this project” in `.claude/settings.local.json`. Use a host shell for these changes. Each scoped agent also uses one inotify instance, counted against `fs.inotify.max_user_instances`.

The `/tmp` the view mounts, and `/var/tmp` with it, is one directory per agent, which its subagents share: a sibling agent's `/tmp` is a different directory, so files there do not collide or turn up by accident. That keeps agents apart, not locked out; as the list above says, a sibling can still reach yours at its host path. For files agents hand each other, every agent in the room reaches one shared directory, named by `$RIMZ_SHARED`. Both live under the room's state rather than the host's `/tmp`, and `rimz paths` prints where they sit ([reference](../reference/cli/paths.md)). RimZ can reclaim eligible temp units, but never sweeps the shared directory while the room exists; remove by hand what no agent still needs.

To stop using the view, run `rimz config set agents.isolation host` and restart the agents that follow machine policy. A profile with `isolation: scoped` still chooses the view; edit that default or override it for the launch. An agent launched with an explicit `--isolation scoped` keeps its own view until you replace it: stop it, then [`rimz agents <spec> --continue --isolation host`](../reference/cli/agents.md#continue-a-cohort) keeps its conversation and records host isolation for later relaunches and children. Profile `skills` lists still apply through Claude's or Codex's host switch; other host providers warn and run unrestricted.

## The Zellij presence plugin

On Zellij, RimZ loads a small presence plugin into each session so the sidebar learns pane layout by push, and a tab switch lands back on your work instead of the sidebar ([internals](../internals/multiplexers.md#the-zellij-presence-plugin)). Zellij normally prompts before a plugin gets permissions; RimZ seeds that grant ahead of load, keyed to the exact plugin path it materializes, so the first attach is not interrupted. The grant is four permissions:

- **Access Zellij state.** The plugin watches pane and tab shape.
- **Run commands.** It runs the RimZ-owned `rimz sidebar wake`, `rimz sidebar focus`, and `rimz pane zoom` calls, and nothing else.
- **Reconfigure.** It applies the room's mouse options and binds the configured [room keys](./configuration.md#sidebar-rendering), both at runtime, without writing your `config.kdl`.
- **Change application state.** It applies a host-selected pane's mechanical fullscreen toggle; sidebar-aware target selection stays in the host command.

The vendored plugin is reproducibly built from the checked-in source on the repository's pinned Rust toolchain. Its source digest, wasm digest, and producing toolchain are committed beside it; every vendored embed verifies the wasm checksum, and CI rebuilds the source with that toolchain and requires byte-for-byte equality.

The plugin's code, argv, and configuration are all RimZ-owned, never your `config.kdl`, and it ships no pane content anywhere. The grant lives in Zellij's own permission store, where its plugin manager can revoke it. Revoking stops pane discovery, and `rimz doctor` names the fix while discovery is down. It does not stay revoked: the next time RimZ opens or reattaches its session, it seeds the grant again.

## Browser listener

Browser access defaults to a loopback listener with named sign-in tokens. Each token authenticates the writable daemon rather than a room, so whoever holds one reaches every live RimZ room on the machine and has a shell as the serving user. Handle each like a machine-wide SSH key. `rimz web open` points at `rimz web token show` rather than printing a password; create and revoke named tokens with `rimz web token`.

RimZ's gate always stands in front of a private ttyd listener. In Basic mode it checks the supplied token; with an empty `trusted_proxies` list any peer may attempt sign-in, while a nonempty list limits peers to loopback and those networks. `[web] auth_header` instead delegates sign-in to an authenticating reverse proxy. In that mode every request passes four checks:

1. The source address is loopback, or inside a `[web] trusted_proxies` CIDR. Anything else is dropped without a reply; an empty list leaves only loopback.
2. The named identity header is present exactly once and is not empty. Zero copies, two copies, or an empty value are refused.
3. When `[web] auth_users` is set, the header's trimmed value matches an entry exactly and case-sensitively.
4. Any `Authorization` header the client sent is stripped, and a separate private upstream credential is injected before the request is forwarded.

The identity provider's application policy remains the primary access control; `auth_users` is defense in depth, using canonical username spellings. On its side, the proxy has to strip client-supplied copies of the identity header, inject its own value after authentication, and remain the only non-loopback source that can reach the gate. Pair the CIDRs with the host firewall, and account for the source address that container or host networking produces. Loopback bypasses only the source-address check: a trusted-header request from loopback still needs the header, and the private ttyd listener still needs Basic Auth. On a multi-user host, host-level user isolation is what keeps another local user from reading the serving user's credential or executing as that user.

The [web guide](./web.md#behind-a-reverse-proxy) configures Traefik and Authentik for this path.

## What leaves your machine

RimZ keeps your work local. Prompts, transcripts, pane text, and credentials stay on the box, and nothing is uploaded on a schedule. These are the calls it makes, each reusing a login you already hold or a host you named:

- **Provider usage.** To fill the cost and budget meters, RimZ reads each provider's usage endpoint with the OAuth login the agent already holds. Usage endpoints stay pinned to each provider's official host; a URL override is refused unless it targets that host or loopback.
- **Pull-request status.** The sidebar's PR marker runs your own `gh` (GitHub) or `tea` (Gitea, Forgejo, Codeberg) against your forge, on your existing login. It reads no RimZ secrets and adds no config field, so it stays off the trust hash.
- **Pets.** Enabling `[theme.pets]` fetches a sprite sheet over HTTPS from the host you name; a built-in pet reaches the public Codex pets CDN, and a local-path pet fetches nothing. `RIMZ_PETS_OFFLINE=1` makes the process tree cache-only, and `0`, `false`, or an empty value leaves fetching on. The request carries the asset URL and nothing else.
- **Release downloads.** `rimz update` asks GitHub for the latest tag, then fetches the archive and its `SHA256SUMS` and verifies the checksum before replacing the binary. It runs only when you run it; nothing checks for a release in the background.
- **Off-box error reporting.** Off by default and opt-in. See [Off-box error reporting](#off-box-error-reporting).

`rimz remote` and browser access connect where you point them. What rides an SSH link is in [remote](./remote.md), and the browser listener's boundary is the section above.

Hook payloads are the most sensitive thing RimZ handles, because they can carry prompts, tool inputs, file paths, command arguments, and errors. They land in RimZ's local state and nothing forwards them off the box, error reporting included.

### Off-box error reporting

Reporting is a non-default Cargo feature: code compiled without `--features sentry` contains no Sentry calls whatever the config says, which is how a stock install is built. Even compiled in, it stays off until you set `[sentry] dsn` (or `RIMZ_SENTRY_DSN`). The DSN is per-machine and never the committed project config, so a clone or a pull cannot redirect a contributor's telemetry.

When on, it sends RimZ's own `warn!` and `error!` events and the provider conditions RimZ observes (rate limits, overload), tagged with low-cardinality facts: the release and build id, the running command, a fault class, and the agent or session id when known. The hostname is stripped, Sentry's default personal-data collection is off, and hook payloads, prompts, and transcripts never ride along. An event's own message text does travel, so a warning that names a file path carries that path; a failed account-usage probe, for instance, reports the request's host and never its path or query. The full payload and the config knobs are in [the diagnostics internals](../internals/diagnostics.md#off-box-error-reporting) and [configuration.md](./configuration.md#off-box-error-reporting).

## Pane text is data, not instructions

`rimz pane capture` returns raw terminal text, exactly as the pane's program drew it. RimZ itself reads a pane in three places and no others: drawing the sidebar, an explicit `pane capture`, and confirming that a Codex turn has ended. None of them feeds the text back to an agent as instructions.

RimZ types into a pane only text you gave it: an answer you send with `rimz answer`, a message you send with `rimz message`, and the continuation line [auto-continue](./loops.md#auto-continue) sends when you switch it on.

If you script an answer through the pane primitives, match a bounded prompt shape and abstain when unsure. Captured text is agent output and can say anything at all, so feeding it into an LLM prompt as if it were a user message is the classic prompt-injection footgun. Keep it as data.

## Reporting a vulnerability

Report privately through GitHub Security Advisories on `github.com/rimio-ai/rimz`, or by email to `security@rimio.ai` if advisories are unavailable. Include the affected versions, the reproduction, the impact, and any mitigation you know of. Maintainers acknowledge reports privately before public issue tracking ([SECURITY.md](../../SECURITY.md)).

# Changelog

What changed in each RimZ release, written for the people who run it. Every release is a git tag: `v0.3` tags the `0.3.0` workspace version, `v0.2` tags `0.2.0`. Each heading links to that release's full diff.

RimZ is alpha software on the 0.x line. Commands, flags, config keys, and output formats can change between releases while the design settles, so read the "Changed" section of a release before upgrading. Entries describe what you can do differently; the reasoning behind a change lives in the linked guide.

## [0.4.0] (unreleased)

This section accrues as work lands, and ships as `v0.4`. Three themes so far: provider accounts and money become a queryable surface of their own, remote attach gains port forwarding and honest reconnect reporting, and RimZ updates itself.

### Added

- `rimz update` upgrades RimZ through the install path you already used. Homebrew delegates to `brew upgrade`, Cargo delegates to `cargo install --locked rimz`, and a prebuilt install downloads the release, verifies its checksum, smoke-tests it, and atomically replaces the running binary. When the binary changes, the new build reloads your running sidebars. `--version` selects a release tag, including the rolling `latest-main`. → [maintenance](./docs/reference/cli/maintenance.md)
- A one-line install script for verified prebuilt binaries on macOS and Linux, with no `sudo` required for a user-owned destination. Unsupported platforms report the exact `cargo install` fallback rather than failing silently. → [installation](./docs/guide/installation.md)
- `rimz providers` reports the account picture behind the provider dashboard: login status, plan, agent CLI version, included rate-limit windows, paid and reset credits, published spend, and daily-cap state. It runs inside or outside a room, `--json` emits a stable report array for scripts, and `--refresh` bypasses the caches for one call. → [providers CLI](./docs/reference/cli/providers.md)
- Grok Build is a first-class agent: passive hooks, rewind-aware session history, exact completed-turn dollars, and local account identity. Permission decisions stay in Grok's own TUI. → [agent support](./docs/reference/agent-support.md)
- Codex redeems reset credits on its own when blocked time or an approaching expiry justifies it, and rescues credits that would otherwise expire unused.
- Copilot reports monthly account quotas, usage history, and subagent telemetry.
- Qwen gates managed runs on the exact account quota rather than an estimate.
- Remote attach forwards ports. A listener that starts on the host after you attach, owned by your remote user on port 1024 or above, is forwarded to `127.0.0.1` on your machine. Forwards survive a link recovery, close when you detach, and skip a local port that is already busy. `--no-auto-forward` opts out for one connection or for a saved alias. → [remote](./docs/guide/remote.md)
- Remote reconnect runs behind a recovery panel that paces attempts and reports what it is actually waiting on, rather than showing a countdown it cannot honour. A wifi, route, or address change triggers an immediate retry.
- `rimz stats` makes automation savings and usage direction visible: Assists now covers auto-ping, auto-continue, pre-delivery auto-compaction, credit redemption, and crash/reboot recovery, while the insight lines add Week/Month spend trends, cost per session, and daily average. `--assists` renders the durable history and `--json` publishes the same assist rollup and events. → [insight](./docs/guide/insight.md) · [loop internals](./docs/internals/harness/loops.md#the-assist-log)
- Scheduled auto-ping keeps a provider's budget window running: opt in, and RimZ synthesizes ping tasks, refreshes provider usage before each one, and retries an authoritatively down window hourly. → [loops](./docs/guide/loops.md)
- The sidebar carries pull-request CI verdicts from GitHub and Gitea through the shared PR cache, so lanes, the cockpit summary, and `rimz agents` all show the same state. CI tracking follows a pull request after it merges.
- `rimz doctor` is evidence-driven. It preserves runtime evidence between runs, coordinates authoritative probes, and folds backend logs and diagnostic records into scoped incidents that each name a fix. It also lists Zellij presence-plugin generations and distinguishes an absent agent from a missing hook.
- `rimz gc` explains every worktree it keeps instead of silently retaining it.
- Attach warns about version drift: a stale local room build, or a version gap between your client and a remote host.
- Inside a team's channel, a bare role handle resolves against that team. From a pane in `#forge`, `rimz agents reviewer` means `rimz agents forge.reviewer`, and the new agent joins the lane it resolved from. A role that would launch a different agent than a same-named profile refuses and names both forms. Scheduled loop tasks still resolve their specs literally. → [teams](./docs/guide/teams.md)

### Changed

- RimZ runs its own tmux server at `<runtime-root>/rimz/tmux/server`, one per runtime domain, holding one session per workspace. Your default tmux server is no longer touched: RimZ's server-global options and root key bindings stay confined to it, and a server born in a directory you later delete can no longer strand new panes there. `rimz doctor` reports a same-named session left on the legacy default server and gives you the command that retires it.
- Remote version skew is tiered by how far apart the two builds are. A patch difference warns, a minor difference requires an explicit one-shot bypass, and a major difference refuses the attach.
- The sidebar defaults to the wide dashboard width when pets are enabled, including on narrow views and when launch geometry is unknown. Explicit width configuration and room overrides keep precedence.
- An explicit `--model` overrides a profile's declared model silently, instead of warning about the conflict.
- `rimz worktree remove` refuses a worktree an agent or an open pane is still working in, naming what holds it, and `--force` warns before overriding. It previously checked only Git state, so it could delete a checkout out from under a running agent. A stale session record from a crashed agent still does not block removal. → [worktrees](./docs/guide/worktrees.md#cleanup-once-work-lands)
- Turning on `remote_control.claude` starts the Claude host unattended. Claude asks `Enable Remote Control? (y/n)` once per machine, and RimZ now records that answer for you before the host starts, so the pane serves instead of waiting on a prompt in a room you left. Setting Claude's `remoteDialogSeen` to `false` by hand still refuses at start with the fix. → [remote](./docs/guide/remote.md)
- The `⇅ rc` flag and `rimz doctor` report whether the Claude host is actually serving, read from Claude's own record of the process behind each project root. A host whose server stopped now reads as down instead of staying healthy on a pane that outlived it.

### Fixed

- Sidebar: a cleanly finished turn with a background shell still running shows `✓` immediately alongside `⋯ bg`.
- Remote: the connect panel stays visible through the attach handoff and reports it as still in flight; terminal state is restored after an SSH session and verified across reconnects; supervised SSH children stay in the foreground so password, two-factor, and host-key prompts reach you; the confirmed master socket survives a reconnect; listener reports no longer depend on link timing.
- Sidebar: the active body filter applies in every tab, and cross-tab jumps publish their selection and viewport before the switch so the destination no longer blinks; filters and viewport still hold across card jumps and group toggles; a lone finished card stays visible and its elapsed clock heats; renderers whose panes disappeared are reaped, and panes are verified before an orphan sweep; tmux focus resolves from client views; account usage is fetched when rate-limit bars read unknown; and the sidebar no longer floods Zellij with pane probes.
- The cockpit holds its spend figures across a workspace-spending republication. A reader whose worktree roots lagged the producer scored the published tally as unreachable and read the room as empty, so the headline spend and session count blanked for several seconds at a time.
- Zellij presence: stale plugins are retired on the current reload and force-closed when they linger, topology publication waits for redock geometry, pane kind survives cache enrichment, and tab focus repairs from attached client views. Opening an unfocused tab returns focus to the leading tab only when a client is attached to receive it, so a detached room stops writing a Zellij error per tab.
- `rimz doctor` names the cause of a failing presence wake using evidence from the window it reports on. The plugin used to discard a failure as soon as the next wake succeeded, which on a busy room erased almost every intermittent cause before it could be sampled, leaving the count with generic reload advice beside it. Doctor also grades two Zellij log records below alarm: an action addressing a pane that had already closed, and a pane whose configured directory is gone, which Zellij starts in the inherited directory and names in the row. → [troubleshooting](./docs/guide/troubleshooting.md)
- Agents: Claude usage is read from the correct record, compaction clears a dismissed ask, prompt descriptions have the `user_query` envelope peeled off, provider CLI versions are normalized, and Kimi and Qwen card context is stable. Cursor follows conversation switches after a clear, Amp keeps interactive mode for an empty prompt, and Grok's lifecycle and native permission status are restored.
- Worktrees and store: post-removal cleanup attempts every step instead of stopping at the first failure, temporary refs are cleaned on every exit, landed snapshots are detected after a history rewrite, pull-request checkouts are forge-authoritative, sessions retire when their worktree is removed, and ended sessions are retained for an explicit resume.
- Resume proves a session is redeemable before resuming it, and a resume replays the posture its profile declared.
- Loops ping only windows confirmed fresh, and `rimz loop` reserves a watch overflow row so the display fits.
- Storage accounting no longer leaks hardlinks or overcounts disk size.
- `rimz workspace` refuses an unresolved root instead of hashing an empty path, and normalizes resolved git roots.

### Performance

- The sidebar reuses matching consumer fold stamps, shares producer workspace projections, memoizes stable untracked line counts, and attributes fetch-fold causes, cutting repeated work on every frame.
- The spending service buffers its socket writes, and historical store discovery is bounded.

## [0.3.0] (2026-07-16)

Subagents become visible across the experimental agent set, and the cockpit starts counting open pull requests.

### Added

- Subagent rows for Antigravity, Copilot, Cursor, Kimi, and Pi, each correlated to its parent through that agent's own records. Pi's subagent coverage is complete.
- Copilot reports an estimated session cost.
- Cursor surfaces native subagents and pane-only asks, including plan-approval waits that can only be answered in the pane.
- The cockpit counts open agent pull requests, paired with a branch glyph.

### Changed

- Per-user agent profile fragments move from `~/.agents/agents` to `~/.agents/profiles`, so RimZ stops colliding with the `~/.agents/agents` convention that agent tools use for their own subagent definitions. Move your fragments to keep them discovered. The sibling `~/.agents/teams` directory is unchanged.

### Fixed

- Antigravity adopts subagents after a late transcript flush, accepts inherited subagent workspaces, and validates captured child workspaces.
- Cursor derives subagents from local chat records and accepts completed child transcript stops.
- Kimi confirms the `Stop` wire boundary before suppressing an event; Pi preserves subagent parent lineage; Qwen prefers live statusline context occupancy and requires complete cost estimates.
- OpenCode follows in-pane conversation switches and preserves usage across an aborted turn.
- Remote reaps stale Zellij clients before reattaching, and a remote birth seeds its width from local terminal geometry.
- tmux sidebars converge from honest view geometry, and the store isolates child IDs from provisional roots.
- Homebrew formulas derive their own versions, and Intel Homebrew versions are pinned.

## [0.2.0] (2026-07-16)

First published release, carrying the project's history since 2026-05-22.

RimZ routes attention across a fleet of coding agents. It runs as a single binary inside the Zellij or tmux you already use, watches the agents through their own hooks, transcripts, and APIs, and renders one sidebar that tells you which agent needs you and takes you to its pane. The agents run their stock CLIs; their official web, desktop, and mobile apps keep working.

What shipped:

- The room: one workspace, one multiplexer session, and a sidebar that survives detach, reattach, and reboot. Zellij and tmux are both first-class, with parity key chords and a themed status bar. → [setup](./docs/guide/setup.md) · [multiplexer](./docs/guide/multiplexer.md)
- The sidebar: a cockpit line that reads the whole fleet at a glance, agent cards carrying working state, model and effort, context health, live token and dollar figures, and the subagent tree, ranked so whoever needs you arrives at the top. → [sidebar](./docs/guide/sidebar.md)
- Agents: twelve supported coding CLIs (Claude Code, Codex, Pi, OpenCode, Antigravity, Copilot, Droid, Cursor, Amp, Kiro, Qwen Code, Kimi) behind one adapter contract, launched by name with permission-mode suffixes, profiles, and a layout grammar. → [fleet](./docs/guide/fleet.md) · [agent support](./docs/reference/agent-support.md)
- Messaging: every agent answers to a handle. Park a message for the next safe turn boundary, steer the live turn, schedule for later, or ask and print the reply. Agents talk to each other and to you in channels. → [messaging](./docs/guide/messaging.md)
- Teams and worktrees: name a team of roles across models in `agents.toml` and launch it as one unit, optionally isolated in a RimZ-owned git worktree with supervised cleanup. → [teams](./docs/guide/teams.md) · [worktrees](./docs/guide/worktrees.md)
- Scripting: `rimz agents -p` is `claude -p` for every agent that exposes it, with exit codes, JSON and streaming output, background runs, and waits. → [scripting](./docs/guide/scripting.md)
- Loops: supervised turns on a calendar, interval, or cron schedule, plus check-guarded watchdogs that run a command and wake an agent on the result. → [loops](./docs/guide/loops.md)
- Recovery while you are away: a rate-limit pause resumes the moment the budget window resets, transient API overload retries on a backoff ramp, and smart compaction sends `/compact` ahead of your text once context passes a threshold.
- Money: token and dollar insight for today, this week, and this month, with plan and rate-limit bars for providers that expose them, and enforced daily dollar caps across four scopes. → [insight](./docs/guide/insight.md) · [budget](./docs/guide/budget.md)
- Remote and web: attach to a room on another host over SSH with a self-healing link that survives sleep and network loss, or open a Zellij room in the browser. → [remote](./docs/guide/remote.md) · [web](./docs/guide/web.md)
- Notifications: desktop banners, unread nudges, and handlers that run your own command, including the remote-control toggles that put an agent's question in the official Claude and ChatGPT mobile apps. → [notifications](./docs/guide/notifications.md)
- Theming and pets: bundled palettes, color-depth and slot overrides, custom themes, provider branding, and an animated companion on the provider dashboard. → [theme](./docs/guide/theme.md) · [pets](./docs/guide/pets.md)
- `rimz doctor`, `rimz setup`, hook install and uninstall, project trust, and a documented reset and GC path. → [troubleshooting](./docs/guide/troubleshooting.md) · [security](./docs/guide/security.md)

[0.4.0]: https://github.com/rimio-ai/rimz/compare/v0.3...HEAD
[0.3.0]: https://github.com/rimio-ai/rimz/compare/v0.2...v0.3
[0.2.0]: https://github.com/rimio-ai/rimz/releases/tag/v0.2

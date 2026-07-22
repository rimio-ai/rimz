# Changelog

What changed in each RimZ release, written for the people who run it. Every release is a git tag: `v0.4.1` tags the `0.4.1` workspace version, `v0.4` tags `0.4.0`, `v0.3` tags `0.3.0`. Each heading links to that release's full diff.

RimZ is alpha software on the 0.x line. Commands, flags, config keys, and output formats can change between releases while the design settles, so read the "Changed" section of a release before upgrading. Entries describe what you can do differently; the reasoning behind a change lives in the linked guide.

## Unreleased

### Added

- `rimz web share` serves an explicitly allowlisted live room through a second no-auth, input-blocked ttyd daemon; `unshare` revokes live viewers, tmux attaches ignore viewer size, and status reports both browser surfaces. → [web](./docs/guide/web.md#share-a-read-only-broadcast)
- The web daemon's base URL opens a themed live-session manager with cockpit-style repository cards, per-provider attention and usage totals, and mouse or keyboard selection; reconnects and refreshes stay with the attached room, while detaching makes the list the reconnect target again. → [web](./docs/guide/web.md#session-manager)
- Layout cells accept any executable on PATH as a raw command pane, while configured commands and profiles keep precedence over same-named binaries. → [fleet](./docs/guide/fleet.md#compose-a-layout)
- `rimz events follow` streams versioned agent lifecycle transitions as JSON Lines, with current-generation replay and gap-free handoff across event-log rotation. → [events](./docs/reference/cli/events.md)

### Changed

- Trusted-header browser access accepts an optional `[web] auth_users` identity allowlist and rejects duplicated identity headers; entries match the proxy's trimmed canonical username exactly and case-sensitively. → [web](./docs/guide/web.md#behind-a-reverse-proxy)
- Default launch tab titles list the first three cells in layout order instead of only the first agent kind. → [fleet](./docs/guide/fleet.md#compose-a-layout)
- Nerd Font worktree headers use the Powerline branch glyph. → [theme](./docs/guide/theme.md#glyphs)

### Fixed

- Cursor binary discovery verifies the generic `agent` executable before claiming it, launches only a verified path or the provider-unique `cursor-agent` alias, and leaves ambiguous process basenames unattributed until a native hook binds the pane. → [Cursor adapter](./docs/internals/agents/cursor.md#launch-and-resume)
- Browser authentication now keeps ttyd behind machine-wide Basic Auth while the public gate validates trusted proxy headers and injects that credential upstream; remote `--web` tunnels work through either auth mode, `rimz web restart` applies the current browser profile, and `rimz reload` refreshes an online web daemon after upgrades. → [web](./docs/guide/web.md)
- Browser panes opened with macOS Option chords no longer begin with the dead-key character those chords would normally compose. → [web](./docs/guide/web.md#browser-appearance-and-input)
- Browser cursors stay steady while typing when terminal apps request blink modes or bracket redraws with cursor visibility changes. → [web](./docs/guide/web.md#browser-appearance-and-input)
- Pixel pets and the pixel context meter render as true pixels in ttyd browser attaches for qualifying tmux rooms; pets keep their native proportions, leave tmux borders and neighboring panes intact, and suppress placeholder glyphs before xterm paints them, while mixed or unsupported clients continue to fall back to sextant cell art. → [pets](./docs/guide/pets.md#crisp-pixels-and-cell-art)
- Homebrew upgrades through `rimz update` and the install script refresh formula data first, so a stale tap no longer skips a released build. → [installation](./docs/guide/installation.md)
- Queued `done`-gated messages deliver when an agent's clean turn parks on background work instead of waiting for the background process to exit. → [messaging](./docs/guide/messaging.md)

## [0.4.1] (2026-07-21)

A follow-up to 0.4.0: one shared daemon serves browser access for Zellij and tmux rooms alike, those rooms get readable fonts and colors, and the agent compatibility matrix is rewritten around what you see rather than how it is wired.

### Added

- Browser rooms can delegate authentication to a reverse proxy through `[web] auth_header`, bind a configured IP through `[web] interface`, and restrict non-loopback clients to `[web] trusted_proxies` through a source-address gate. → [web](./docs/guide/web.md#behind-a-reverse-proxy)
- Sidebar worktree PR badges open their pull request through terminal hyperlinks, including across SSH and browser attaches. → [sidebar](./docs/guide/sidebar.md#the-agent-cards)
- First-run setup asks whether to enable auto-continue and automatic Codex reset-credit redemption together, defaulting yes. → [loops](./docs/guide/loops.md)
- Browser rooms provision a verified JetBrainsMono or CaskaydiaCove Nerd Font, refresh xterm after the face loads so icons render instead of blocks, accept a local or HTTPS custom font, and use the active RimZ terminal colors. The browser cursor stays steady, Shift+Enter reaches agents as a soft newline, tmux copy-mode yanks and Shift-drag selections reach the system clipboard, disconnect and resize popups use a dark-glass style, and macOS Option chords reach tmux without leaking composed characters. → [web](./docs/guide/web.md#browser-appearance-and-input)

### Changed

- On macOS, the install script delegates latest-release installs to Homebrew when it is available, while other versions, explicit destinations, and existing non-Homebrew installs keep the direct download; a Homebrew failure falls back to the release archive. → [installation](./docs/guide/installation.md)
- Browser access for Zellij and tmux rooms is served by one shared ttyd daemon on the exact configurable `[web] port`, with each authenticated URL selecting its validated RimZ session through `?arg=`. Room start ensures the daemon best-effort, and the separate Zellij web server integration has been removed. → [web](./docs/guide/web.md)
- Remote browser tunnels use the same connection panel and supervised SSH master as terminal attaches. One prep call ensures the room and shared daemon and returns its credential before forwarding; recovery revives the remote daemon while preserving the local URL. → [web](./docs/guide/web.md#open-a-remote-room)
- The agent compatibility matrix reads in behaviour rather than plumbing. Each of the six capabilities — State, Live, History, Account, Ask, Subagents — now answers what you see and when you see it: full means complete and live, partial means a working version whose limit the matrix names, unsupported means nothing to render. Every adapter declares its own six marks, so `rimz coverage` leads with that grid and prints each cell's limit in plain terms, `rimz coverage --wiring` keeps the integration-concern and lifecycle-hook grids for adapter work, and a test holds the published tables to what the adapters declare. Several marks moved as a result, Antigravity's State and Copilot's Subagents up, Cursor's Subagents and the History column for Amp, Qwen, and Kimi down. → [agent support](./docs/reference/agent-support.md)
- The compatibility promise now names its surface. RimZ is a binary, and what it supports is the binary: commands and flags, `--json` output, exit codes, config keys, and persisted formats, on the 0.x terms above. The `rimz` crate publishes a library target so the binary, tests, and benches can link the domain modules, and it carries no compatibility promise — its names, signatures, and module layout move with the implementation and every release. Install `rimz` as a command; `cargo add rimz` is unsupported.

### Fixed

- Scheduled loop wakes honor the `[harness] smart_compact` default, so unattended prompts compact first at the configured context threshold.
- tmux rooms stop accumulating duplicate `*:sync` and `*:extkeys` terminal features; the next room start cleans existing duplicates, and the Escape disambiguation delay now defaults to tmux's upstream 10ms for reliable input over SSH.
- `rimz remote connect --web` keeps its ControlMaster-owned tunnel in the foreground after opening the browser instead of treating the forward helper's clean exit as a detach.
- A remote browser tunnel that accepts connections and then loses SSH with exit 255 now reconnects immediately instead of reporting that it was never established.
- `rimz loop add`, `remove`, and `rename` on project tasks keep project trust when the workspace was already trusted, so your own task edit needs no re-review.
- Broken TOML files now report the offending line, name duplicated keys when applicable, and give a concrete fix across config, setup, doctor, start, remote-alias, and project-trust surfaces. This landed after the `v0.4` tag was cut, so it reaches you first in this release.
- `rimz setup` keeps explicit config values on their documented template lines, preserving inline field notes and avoiding commented duplicates that could become duplicate-key errors when uncommented later. This also landed after the `v0.4` tag.
- Interrupting a Claude subagent no longer leaves its finished parent pinned to `running` in the sidebar; the next parent tool or turn boundary closes the interrupted child from Claude's durable transcript marker.
- Tagged releases stamp a clean version again. The release job checks out by commit hash, which brought down no tags, so `v0.4` binaries reported `0.4.0+g<sha>` instead of `0.4.0` — enough to make attach's version-drift check compare against a build string no release ever published.
- Homebrew upgrades fire on both macOS architectures. The generated formula pinned its version only inside the Intel block, and Homebrew's filename heuristic read `x86_64` as the version on every release, so `brew upgrade` never saw a new one. → [installation](./docs/guide/installation.md)

## [0.4.0] (2026-07-20)

Four themes: provider accounts and money become a queryable surface of their own, remote attach gains port forwarding and honest reconnect reporting, RimZ updates itself, and a broad correctness and performance pass steadies the sidebar, Zellij presence, and spend accounting while cutting the work behind each frame.

### Added

- `rimz asks` carries the agent's context message on question and plan asks, renders it above the questions in `show`, and exposes it as `context` in JSON. → [asks](./docs/reference/cli/asks.md)
- `rimz update` upgrades RimZ through the install path you already used. Homebrew delegates to `brew upgrade`, Cargo delegates to `cargo install --locked rimz`, and a prebuilt install downloads the release, verifies its checksum, smoke-tests it, and atomically replaces the running binary. When the binary changes, the new build reloads your running sidebars. `--version` selects a release tag, including the rolling `latest-main`. → [maintenance](./docs/reference/cli/maintenance.md)
- A one-line install script for verified prebuilt binaries on macOS and Linux, with no `sudo` required for a user-owned destination. Unsupported platforms report the exact `cargo install` fallback rather than failing silently. → [installation](./docs/guide/installation.md)
- `rimz providers` reports the account picture behind the provider dashboard: login status, plan, agent CLI version, included rate-limit windows, paid and reset credits, published spend, and daily-cap state. It runs inside or outside a room, `--json` emits a stable report array for scripts, and `--refresh` bypasses the caches for one call. → [providers CLI](./docs/reference/cli/providers.md)
- Grok Build is a first-class agent: passive hooks, rewind-aware session history, native or locally priced completed-turn dollars, and local account identity. Permission decisions stay in Grok's own TUI. → [agent support](./docs/reference/agent-support.md)
- Codex redeems reset credits on its own when blocked time or an approaching expiry justifies it, learns the seven-day window's burn rate, and schedules chains of expiring credits early enough for each one to catch a refill.
- Copilot reports monthly account quotas, usage history, and subagent telemetry.
- Qwen gates managed runs on the exact account quota rather than an estimate.
- Remote attach forwards ports. A listener that starts on the host after you attach, owned by your remote user on port 1024 or above, is forwarded to `127.0.0.1` on your machine. Forwards survive a link recovery, close when you detach, and skip a local port that is already busy. `--no-auto-forward` opts out for one connection or for a saved alias. → [remote](./docs/guide/remote.md)
- Remote reconnect runs behind a recovery panel that paces attempts and reports what it is actually waiting on, rather than showing a countdown it cannot honour. A wifi, route, or address change triggers an immediate retry.
- `rimz stats` makes automation savings and usage direction visible: Assists now covers auto-continue, pre-delivery auto-compaction, credit redemption, and crash/reboot recovery, while the insight lines add Week/Month spend trends, cost per session, and daily average. `--assists` renders the durable history and `--json` publishes the same assist rollup and events. → [insight](./docs/guide/insight.md) · [loop internals](./docs/internals/harness/loops.md#the-assist-log)
- The sidebar carries pull-request CI verdicts from GitHub and Gitea through the shared PR cache, so lanes, the cockpit summary, and `rimz agents` all show the same state. CI tracking follows a pull request after it merges.
- `rimz doctor` is evidence-driven. It preserves runtime evidence between runs, coordinates authoritative probes, and folds backend logs and diagnostic records into scoped incidents that each name a fix. It also lists Zellij presence-plugin generations and distinguishes an absent agent from a missing hook.
- `rimz gc` explains every worktree it keeps instead of silently retaining it.
- Attach warns about version drift: a stale local room build, or a version gap between your client and a remote host.
- Inside a team's channel, a bare role handle resolves against that team. From a pane in `#forge`, `rimz agents reviewer` means `rimz agents forge.reviewer`, and the new agent joins the lane it resolved from. A role that would launch a different agent than a same-named profile refuses and names both forms. Scheduled loop tasks still resolve their specs literally. → [teams](./docs/guide/teams.md)

### Changed

- `rimz stats` folds models below 1.0% of window spend and agents below 1.0% of window sessions into `Other`, keeping the panel's long tail compact while `--json` preserves the complete breakdown. → [insight](./docs/guide/insight.md)
- `rimz remote bandwidth` is now `rimz pane bandwidth`: the per-pane write-rate profiler is a pane primitive and works for local rooms; only the SSH `WIRE` rows are remote-specific. → [pane](./docs/reference/cli/pane.md#bandwidth)
- RimZ runs its own tmux server at `<runtime-root>/rimz/tmux/server`, one per runtime domain, holding one session per workspace. Your default tmux server is no longer touched: RimZ's server-global options and root key bindings stay confined to it, and a server born in a directory you later delete can no longer strand new panes there. `rimz doctor` reports a same-named session left on the legacy default server and gives you the command that retires it.
- Remote version skew is tiered by how far apart the two builds are. A patch difference warns, a minor difference requires an explicit one-shot bypass, and a major difference refuses the attach.
- The sidebar defaults to the wide dashboard width when pets are enabled, including on narrow views and when launch geometry is unknown. Explicit width configuration and room overrides keep precedence.
- An explicit `--model` overrides a profile's declared model silently, instead of warning about the conflict.
- `rimz worktree remove` refuses a worktree an agent or an open pane is still working in, naming what holds it, and `--force` warns before overriding. It previously checked only Git state, so it could delete a checkout out from under a running agent. A stale session record from a crashed agent still does not block removal. → [worktrees](./docs/guide/worktrees.md#cleanup-once-work-lands)
- Turning on `remote_control.claude` starts the Claude host unattended. Claude asks `Enable Remote Control? (y/n)` once per machine, and RimZ now records that answer for you before the host starts, so the pane serves instead of waiting on a prompt in a room you left. Setting Claude's `remoteDialogSeen` to `false` by hand still refuses at start with the fix. → [remote](./docs/guide/remote.md)
- The `⇅ rc` flag and `rimz doctor` report whether the Claude host is actually serving, read from Claude's own record of the process behind each project root. A host whose server stopped now reads as down instead of staying healthy on a pane that outlived it.

### Fixed

- The live `rimz stats` pane stays full-screen in `rimzd`, so the mouse wheel no longer reveals terminal scrollback behind the dashboard.
- Grok cards use completed-turn input as their context occupancy, so the fresh/cache/output detail matches the displayed total, and locally estimate dollars from model pricing when current Grok Build records omit native cost ticks.
- Remote attach keeps the panel layout stable while a confirmed session opens, animates the Multiplexer stage, and turns its handoff arrow green as the remote session renders. SSH control-client failures now appear as a recovery cause instead of leaking raw diagnostics over the panel.
- Held stats dashboards survive spending-service contention and repaired Zellij daemon panes close with their commands, so `rimzd` stops accumulating failed content panes.
- Zellij presence upgrades launch background writers, so pane discovery, selection sync, and animation continue after leaving the tab that was active during `rimz reload`. Existing affected sessions self-repair on their next reload.
- Sidebar: a cleanly finished turn with a background shell still running shows `✓` immediately alongside `⋯ bg`.
- Provider dashboard session stats count spend from the full machine-global work burst, including providers driven through cross-provider teams.
- Remote: the connect panel stays visible through the attach handoff and reports it as still in flight; terminal state is restored after an SSH session and verified across reconnects; supervised SSH children stay in the foreground so password, two-factor, and host-key prompts reach you; the confirmed master socket survives a reconnect; listener reports no longer depend on link timing.
- Sidebar: the active body filter applies in every tab, and cross-tab jumps publish their selection and viewport before the switch so the destination no longer blinks; filters and viewport still hold across card jumps and group toggles; a lone finished card stays visible and its elapsed clock heats; renderers whose panes disappeared are reaped, and panes are verified before an orphan sweep; tmux focus resolves from client views; account usage is fetched when rate-limit bars read unknown; and the sidebar no longer floods Zellij with pane probes.
- The cockpit holds its spend figures across a workspace-spending republication. A reader whose worktree roots lagged the producer scored the published tally as unreachable and read the room as empty, so the headline spend and session count blanked for several seconds at a time.
- Zellij presence: stale plugins are retired on the current reload and force-closed when they linger, topology publication waits for redock geometry, pane kind survives cache enrichment, and tab focus repairs from attached client views. Opening an unfocused tab returns focus to the leading tab only when a client is attached to receive it, so a detached room stops writing a Zellij error per tab.
- `rimz doctor` names the cause of a failing presence wake using evidence from the window it reports on. The plugin used to discard a failure as soon as the next wake succeeded, which on a busy room erased almost every intermittent cause before it could be sampled, leaving the count with generic reload advice beside it. Doctor also grades two Zellij log records below alarm: an action addressing a pane that had already closed, and a pane whose configured directory is gone, which Zellij starts in the inherited directory and names in the row. → [troubleshooting](./docs/guide/troubleshooting.md)
- Agents: Claude usage is read from the correct record, compaction clears a dismissed ask, prompt descriptions have the `user_query` envelope peeled off, provider CLI versions are normalized, and Kimi and Qwen card context is stable. Cursor follows conversation switches after a clear, Amp keeps interactive mode for an empty prompt, and Grok's lifecycle and native permission status are restored.
- Worktrees and store: post-removal cleanup attempts every step instead of stopping at the first failure, temporary refs are cleaned on every exit, landed snapshots are detected after a history rewrite, pull-request checkouts are forge-authoritative, sessions retire when their worktree is removed, and ended sessions are retained for an explicit resume.
- Resume proves a session is redeemable before resuming it, and a resume replays the posture its profile declared.
- `rimz loop` reserves a watch overflow row so the display fits.
- Storage accounting no longer leaks hardlinks or overcounts disk size.
- `rimz workspace` refuses an unresolved root instead of hashing an empty path, and normalizes resolved git roots.

### Performance

- The sidebar reuses matching consumer fold stamps, shares producer workspace projections, memoizes stable untracked line counts, and attributes fetch-fold causes, cutting repeated work on every frame.
- The spending service buffers its socket writes, and historical store discovery is bounded.
- Fleet spending aggregation hashes session identities instead of repeatedly ordering transcript paths, and reuses normalized workspace origins instead of allocating a path per entry, cutting the live-scale global refresh and additional-workspace folds by about 13% and 25% respectively.

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

[0.4.1]: https://github.com/rimio-ai/rimz/compare/v0.4...v0.4.1
[0.4.0]: https://github.com/rimio-ai/rimz/compare/v0.3...v0.4
[0.3.0]: https://github.com/rimio-ai/rimz/compare/v0.2...v0.3
[0.2.0]: https://github.com/rimio-ai/rimz/releases/tag/v0.2

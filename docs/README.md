# RimZ documentation

RimZ puts your coding agents in one Zellij or tmux room and routes your attention to whichever one needs you. This page maps the whole documentation set.

New here? Start with the [README](../README.md) for what RimZ is and why it exists, then read [installation](./guide/installation.md) and [set up your machine](./guide/setup.md) in that order. Once the room is running, the guides under [working with agents](#working-with-agents) and [harness engineering](#harness-engineering) cover the daily work, and the [reference](#reference) answers a specific flag or field when you need it.

## Getting started

- [Introduction](../README.md): what RimZ is, what it does, and the everyday moves, with a runnable get-started block.
- [Installation](./guide/installation.md): the install script, a current Zellij or tmux, and a clean `rimz doctor`, plus Homebrew, prebuilt archives, Cargo, source, Docker, updating, and uninstalling.
- [Set up your machine](./guide/setup.md): one run of `rimz setup`, question by question: the config it writes, the agent hooks it installs, the color and glyph probes, the pet, the hands-off automation consent, and your first room.

## Working with agents

- [Sidebar](./guide/sidebar.md): read the column that routes your attention, from the cockpit and the agent cards to the seven states a session moves through, and follow the ranking that decides which agent needs you next.
- [Agents](./guide/fleet.md): run the stock CLIs in the room, shape an agent for one job with a profile, compose several into one layout, and read and drive the running fleet.
- [Token insight](./guide/insight.md): read what the fleet costs and how hard it is working, across `rimz stats`, the live cockpit, and the provider dashboard, and how every figure is counted and priced.
- [Remote](./guide/remote.md): attach to a room on another host over SSH with a self-healing link, kept alive across reboots, and answer agent asks from the providers' own mobile apps.
- [Web](./guide/web.md): open a room in a browser behind one authenticated local address, on this machine or tunnelled from a server, pick among live rooms in the session manager, broadcast one room read-only, and put your own reverse proxy in front.

## Harness Engineering

- [Worktrees](./guide/worktrees.md): isolate a layout or team on its own Git branch so several run in parallel without clobbering each other.
- [Messaging](./guide/messaging.md): reach agents by handle, park text for the turn boundary or steer the live turn, ask an agent a question and print its reply, and group the fleet into channels.
- [Teams](./guide/teams.md): pair models by role, launch the whole set with one name, follow and hand over the stages on its shared board, and reopen or retire the team as a single unit.
- [Scripting](./guide/scripting.md): drop an agent into a shell script, a cron line, or CI with `rimz agents -p`: one prompt, one exit code, JSON or streaming output, background runs to join later, and agents launching agents.
- [Subagents](./guide/subagents.md): let your agents hand bounded tasks to children that run in their own panes under a deadline, decide which children each agent may launch, and watch, answer, and stop them.
- [Loops](./guide/loops.md): fire agent turns on a clock or a room signal, guard them with a shell check, let agents set their own alarms, and keep the fleet moving through rate limits and full contexts.
- [Notifications](./guide/notifications.md): the banner and bell you get for free, a handler that pushes to your phone or runs any command you like, and handlers that answer the routine prompt for you.
- [Budgets](./guide/budget.md): enforce dollar caps on one turn, one agent, one loop task, a room's fleet, or a provider account, resume the work a cap parked, and gate background tasks on the subscription window's surplus.
- [Shared LSP](./guide/lsp.md): start one language server per checkout at launch and share it read-only among every agent working there, within a memory budget, and see, query, and stop it from your shell.

## Customization

- [Configuration](./guide/configuration.md): every setting and the file that owns it, across config, agent profiles and teams, loop tasks, and project trust.
- [Provider accounts](./guide/accounts.md): run a room's Claude or Codex agents under a second account, with its own limits, budget, and sessions.
- [Theming](./guide/theme.md): the palette, color depth, color slots, glyph sets, status-head animations, the sidebar's sizing and meter stops, and provider branding.
- [Pets](./guide/pets.md): the animated companion on the dashboard: what it acts out, the built-in and petdex catalogs, your own sprite sheets, the pixel and cell-art render tiers, and what it fetches.
- [Zellij and tmux](./guide/multiplexer.md): what a room asserts in your multiplexer and what stays yours, which backend it picks, and a baseline config for the sessions you run outside it, shipped ready to adopt under [examples/](../examples/README.md).

## Help

- [Troubleshooting](./guide/troubleshooting.md): start with `rimz doctor`, then the symptom catalogue: a room or agent that will not start, an agent that will not report, a sidebar or terminal drawing wrong, notifications and messages that never arrive, scheduled work that never runs, a setting that did nothing, and resetting state.
- [Security and Trust](./guide/security.md): what RimZ changes on your machine and what reverses each change, the two places config can run a command (project trust and notification handlers), what the sandbox view does and does not hide, and what leaves the box.

## Reference

- [Reference index](./reference/README.md): every reference page by topic.
- [CLI](./reference/cli.md): the command map and conventions, with a page per scene: [getting started](./reference/cli/getting-started.md), [remote](./reference/cli/remote.md), [web](./reference/cli/web.md), [agents](./reference/cli/agents.md), [subagents](./reference/cli/subagents.md), [teams](./reference/cli/teams.md), [asks](./reference/cli/asks.md), [message](./reference/cli/message.md), [wait](./reference/cli/wait.md), [transcript](./reference/cli/transcript.md), [pane](./reference/cli/pane.md), [events](./reference/cli/events.md), [accounts](./reference/cli/accounts.md), [stats](./reference/cli/stats.md), [budget](./reference/cli/budget.md), [providers](./reference/cli/providers.md), [channels](./reference/cli/channel.md), [worktrees](./reference/cli/worktree.md), [loop](./reference/cli/loop.md), [language servers](./reference/cli/lsp.md), [hooks and trust](./reference/cli/hooks-trust.md), [config](./reference/cli/config.md), and [maintenance](./reference/cli/maintenance.md).
- [Agent support](./reference/agent-support.md): per-agent status, integration surface, and permission-mode mapping for every built-in adapter, including Kimi Code and Grok Build.
- [Agent plugins](./reference/agent-plugins.md): connect a third-party agent CLI with a bundle: registering and checking it, the manifest, the canonical event envelope, the shim, and the probe contracts. The surface is early and not ready for public use; its contracts change without notice.
- [Changelog](../CHANGELOG.md): what changed in each release, tagged and dated. RimZ is alpha, so read a release's "Changed" entries before you upgrade.

## How it works

[DESIGN.md](../DESIGN.md) states the attention problem, the design pillars, and the invariants; [ARCHITECTURE.md](../ARCHITECTURE.md) is the runtime shape and the on-disk state; the [internals](./internals/README.md) document each subsystem in depth. To work on RimZ itself, start at [CONTRIBUTING.md](../CONTRIBUTING.md).

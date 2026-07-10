# Rimz documentation

Rimz puts your coding agents in one Zellij or tmux room and routes your attention to whichever one needs you. This page maps the whole documentation set.

New here? Start with the [README](../README.md) for what Rimz is and why it exists, then read [installation](./guide/installation.md) and [set up your machine](./guide/setup.md) in that order. Once the room is running, the guides under [working with agents](#working-with-agents) and [harness engineering](#harness-engineering) cover the daily work, and the [reference](#reference) answers a specific flag or field when you need it.

## Getting started

- [Introduction](../README.md): what Rimz is, what it does, and the everyday moves, with a runnable get-started block.
- [Installation](./guide/installation.md): prerequisites and every install path (Homebrew, prebuilt binary, Cargo, source), verified with `rimz doctor`.
- [Set up your machine](./guide/setup.md): the one-time pass that makes Rimz a daily driver, covering config init, agent hooks, true color, pets, and the hands-off loop settings.

## Working with agents

- [Sidebar](./guide/sidebar.md): read the zones, the agent cards and their lifecycle, and the process rows, and follow how Rimz decides which agent needs you.
- [Agents](./guide/agents.md): run the stock CLIs in the room, shape an agent for one job with a profile, and compose several into one layout.
- [Token Insight](./guide/insight.md): read what the fleet costs and how hard it is working, from the live cockpit to `rimz stats`, and how every figure is calculated.
- [Remote](./guide/remote.md): attach to a room on another host over SSH, a multiplexer attach with a self-healing link, kept alive across reboots.
- [Web](./guide/web.md): open a room in the browser, on the host or tunnelled from a server, gated by a login token.

## Harness Engineering

- [Worktrees](./guide/worktrees.md): isolate a layout or team on its own Git branch so several run in parallel without clobbering each other.
- [Messaging](./guide/messaging.md): steer and queue agents by handle, deliver at the turn boundary or on a schedule, and let agents talk to each other in channels.
- [Teams](./guide/teams.md): pair models by role, launch the whole set with one name, and reopen or resume it as a single unit.
- [Scripting](./guide/scripting.md): supervised one-shot `-p` turns with exit codes, JSON and streaming output, and the background-run primitives that drop agents into scripts and CI.
- [Loops](./guide/loops.md): schedule turns on a clock, guard them with watchdogs, let agents set their own alarms, and keep the fleet moving with auto-continue.
- [Notifications](./guide/notifications.md): reach your phone or run your own command when an agent needs you, and let handlers clear routine prompts themselves.

## Customization

- [Configuration](./guide/configuration.md): every setting and the file that owns it, across config, agent profiles and teams, loop tasks, and project trust.
- [Theming](./guide/theme.md): palettes, color depth, glyph styles, animations, and provider branding.
- [Pets](./guide/pets.md): the dashboard companion — built-in and petdex pets, your own sprite sheets, and the pixel and cell-art render tiers.
- [Zellij and tmux](./guide/multiplexer.md): recommended multiplexer options, parity keybindings, and a themed status bar, shipped ready to adopt under [examples/](../examples/README.md).

## Help

- [Troubleshooting](./guide/troubleshooting.md): start with `rimz doctor`, then the fixes for a room that will not start, agents not reporting, degraded banners, version drift, and resetting state.
- [Security and Trust](./guide/security.md): what Rimz changes on your machine and how to undo it, the two places config can run a command (project trust and notification handlers), and what leaves the box.

## Reference

- [CLI](./reference/cli.md): the command map and conventions, with a page per scene: [getting started](./reference/cli/getting-started.md), [remote](./reference/cli/remote.md), [web](./reference/cli/web.md), [agents](./reference/cli/agents.md), [message](./reference/cli/message.md), [transcript](./reference/cli/transcript.md), [pane](./reference/cli/pane.md), [stats](./reference/cli/stats.md), [channels](./reference/cli/channel.md), [worktrees](./reference/cli/worktree.md), [loop](./reference/cli/loop.md), [hooks and trust](./reference/cli/hooks-trust.md), [config](./reference/cli/config.md), and [maintenance](./reference/cli/maintenance.md).
- [Agent support](./reference/agent-support.md): per-agent status, integration surface, and permission-mode mapping for Claude Code, Codex, Pi, and OpenCode.

## How it works

[DESIGN.md](../DESIGN.md) states the attention problem, the design pillars, and the invariants; [ARCHITECTURE.md](../ARCHITECTURE.md) is the runtime shape and the on-disk state; the [internals](./internals/README.md) document each subsystem in depth. To work on Rimz itself, start at [CONTRIBUTING.md](../CONTRIBUTING.md).

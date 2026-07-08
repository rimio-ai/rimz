# Rimz documentation

Rimz puts your coding agents in one Zellij or tmux room and routes your attention to whichever one needs you. This page maps the whole documentation set.

New here? Start with the [README](../README.md) for what Rimz is and why it exists, then read [installation](./guide/installation.md), [the quickstart](./guide/quickstart.md), and [set up your machine](./guide/setup.md) in that order. Once the room is running, the guides under [working with agents](#working-with-agents) and [harness](#harness) cover the daily work, and the [reference](#reference) answers a specific flag or field when you need it.

## Getting started

- [Introduction](../README.md): what Rimz is, what it does, and the everyday moves, with a runnable get-started block.
- [Installation](./guide/installation.md): prerequisites and every install path (Homebrew, prebuilt binary, Cargo, source), verified with `rimz doctor`.
- [Quickstart](./guide/quickstart.md): install to a working fleet, step by step, from the consent gate to your first question routed to your keyboard.
- [Set up your machine](./guide/setup.md): the one-time pass that makes Rimz a daily driver, covering config init, agent hooks, true color, pets, and the hands-off loop settings.

## Working with agents

- [Sidebar](./guide/sidebar.md): read the zones, the agent cards and their lifecycle, and the process rows, and follow how Rimz decides which agent needs you.
- [Agents](./guide/agents.md): run the stock CLIs in the room, shape an agent for one job with a profile, and compose several into one layout.
- [Remote](./guide/remote.md): attach to a room on another host over SSH, a multiplexer attach with a self-healing link, kept alive across reboots.
- [Web](./guide/web.md): open a room in the browser, on the host or tunnelled from a server, gated by a login token.

## Harness

- [Worktrees](./guide/worktrees.md): isolate a layout or team on its own Git branch so several run in parallel without clobbering each other.
- [Messaging](./guide/messaging.md): steer and queue agents by handle, deliver at the turn boundary or on a schedule, and let agents talk to each other in channels.
- [Teams](./guide/teams.md): pair models by role, launch the whole set with one name, and reopen or resume it as a single unit.
- [Scripting agents](./guide/scripting.md): supervised one-shot `-p` turns with exit codes, JSON and streaming output, and the background-run primitives that drop agents into scripts and CI.
- [Loops and schedules](./guide/loops.md): schedule turns on a clock, guard them with watchdogs, wire notification handlers, and keep the fleet moving with auto-continue.

## Customization

- [Configuration](./reference/configuration.md): every setting and the file that owns it, across config, agent profiles and teams, loop tasks, and project trust.
- [Theming and pets](./guide/theme.md): palettes, color depth, glyph styles, animations, provider branding, and the sidebar pets.
- [Zellij and tmux baselines](./guide/multiplexer.md): recommended multiplexer options, parity keybindings, and a themed status bar, shipped ready to adopt under [examples/](../examples/README.md).

## Help

- [Troubleshooting](./guide/troubleshooting.md): start with `rimz doctor`, then the fixes for a room that will not start, agents not reporting, degraded banners, version drift, and resetting state.
- [Security and trust](./guide/security.md): the threat model and the guardrails, covering project trust, notification handlers, hook safety, and privacy settings.

## Reference

- [CLI](./reference/cli.md): the command map and conventions, with a page per group: [getting started](./reference/cli/getting-started.md), [agents](./reference/cli/agents.md), [message](./reference/cli/message.md), [transcript](./reference/cli/transcript.md), [pane](./reference/cli/pane.md), [loop](./reference/cli/loop.md), [channels](./reference/cli/channel.md), [worktrees](./reference/cli/worktree.md), [web](./reference/cli/web.md), [hooks and trust](./reference/cli/hooks-trust.md), and [maintenance](./reference/cli/maintenance.md).
- [Agent support](./reference/agent-support.md): per-agent status, integration surface, and permission-mode mapping for Claude Code, Codex, Pi, and OpenCode.
- [The sidebar on screen](./interface/sidebar.md): every zone, glyph, and meter of the sidebar, with rendered frames.

## How it works

[DESIGN.md](../DESIGN.md) states the attention problem, the design pillars, and the invariants; [ARCHITECTURE.md](../ARCHITECTURE.md) is the runtime shape and the on-disk state; the [internals](./internals/README.md) document each subsystem in depth. To work on Rimz itself, start at [CONTRIBUTING.md](../CONTRIBUTING.md).

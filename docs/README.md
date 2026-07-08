# Rimz documentation

Rimz puts your coding agents in one Zellij or tmux room and routes your attention to whichever one needs you. The [README](../README.md) is the product introduction; this page is the map of everything else.

New to Rimz? Read in order: [installation](./guide/installation.md), [the quickstart](./guide/quickstart.md), then [set up your machine](./guide/setup.md). Coming back for something specific? Jump to [using the room](#using-the-room), [automation](#automation), or the [reference](#reference).

## Getting started

| Page | What it covers |
| --- | --- |
| [Installation](./guide/installation.md) | Prerequisites, and every install path — Homebrew, prebuilt binaries, Cargo, source — verified with `rimz doctor`. |
| [Quickstart](./guide/quickstart.md) | Install to a working fleet: the consent gate, the first agent card, the first question routed to your keyboard. |
| [Set up your machine](./guide/setup.md) | The one-time pass that makes Rimz a daily driver: config init, agent hooks, true color, pets, the hands-off loop settings, and a Zellij/tmux baseline. |

## Using the room

| Page | What it covers |
| --- | --- |
| [Agents, worktrees, and teams](./guide/agents.md) | Launch agents by name, compose layouts, isolate work in Rimz-owned worktrees, and put a team on a feature. |
| [Messaging](./guide/messaging.md) | Steer and queue agents by handle, deliver at the turn boundary or on a schedule, and let agents talk to each other in channels. |
| [The sidebar](./guide/sidebar.md) | Reading the sidebar: the zones, agent cards and their lifecycle, process rows, and how Rimz decides which agent needs you. |
| [Remote and web](./guide/remote.md) | Reattach locally, connect to a room on an SSH server over a self-healing link, and open a room in the browser. |

## Automation

| Page | What it covers |
| --- | --- |
| [Scripting agents](./guide/scripting.md) | `rimz agents -p` as `claude -p` for every agent: supervised one-shot turns, exit codes, JSON and streaming output, and the orchestration primitives. |
| [Loops and hands-off operation](./guide/loops.md) | Schedule turns on a clock, guard them with watchdogs, wire notification handlers, and keep the fleet moving with auto-continue. |

## Customization

| Page | What it covers |
| --- | --- |
| [Configuration](./reference/configuration.md) | Every setting: the config files, agent profiles and teams, loop tasks, behavior toggles, and project config. |
| [Theming and pets](./guide/theme.md) | Schemes, palettes, color depth, glyphs, animations, provider styling, and the sidebar pets. |
| [Zellij and tmux baselines](./guide/multiplexer.md) | Recommended multiplexer settings, parity keybindings, and a themed status bar, shipped ready to adopt under [examples/](../examples/README.md). |

## Help

| Page | What it covers |
| --- | --- |
| [Troubleshooting](./guide/troubleshooting.md) | First stop `rimz doctor`, then the fixes: a room that will not start, agents not reporting, degraded banners, version drift, and resetting state. |
| [Security and trust](./guide/security.md) | The threat model and the guardrails: project trust, notification handlers, hook safety, and privacy settings. |

## Reference

| Page | What it covers |
| --- | --- |
| [CLI](./reference/cli.md) | The command map and conventions, with a page per group: [getting started](./reference/cli/getting-started.md), [agent control](./reference/cli/agents.md), [channels](./reference/cli/channel.md), [web](./reference/cli/web.md), [hooks and trust](./reference/cli/hooks-trust.md), [maintenance](./reference/cli/maintenance.md). |
| [Agent support](./reference/agent-support.md) | Per-agent status, integration surface, and permission-mode mapping for Claude Code, Codex, Pi, and OpenCode. |
| [The sidebar on screen](./interface/sidebar.md) | Every zone, glyph, and meter of the sidebar, with rendered frames. |

## How it works

[DESIGN.md](../DESIGN.md) states the attention problem, the design pillars, and the invariants; [ARCHITECTURE.md](../ARCHITECTURE.md) is the runtime shape and the on-disk state; [docs/internals/](./internals/) documents each subsystem in depth. To work on Rimz itself, start at [CONTRIBUTING.md](../CONTRIBUTING.md).

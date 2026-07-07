# Rimz documentation

Rimz puts your coding agents in one Zellij or tmux room and routes your attention to whichever one needs you. The [README](../README.md) is the product introduction; this page is the map of everything else.

New to Rimz? Read in order: [installation](./guide/installation.md), [your first session](./guide/experience.md), then [set up your machine](./guide/setup.md). Coming back for something specific? Jump to [using the room](#using-the-room) or the [reference](#reference).

## Getting started

| Page | What it covers |
| --- | --- |
| [Installation](./guide/installation.md) | Prerequisites, installing from Homebrew, prebuilt binaries, Cargo, or source, verifying with `rimz doctor`, uninstalling. |
| [Your first session](./guide/experience.md) | Install to a working fleet: the consent gate, the first agent card, the first question routed to your keyboard. |
| [Set up your machine](./guide/setup.md) | The one-time pass that makes Rimz a daily driver: config init, agent hooks, true color, pets, the hands-off loop settings, and a Zellij/tmux baseline. |

## Using the room

| Page | What it covers |
| --- | --- |
| [Product tour](./guide/product.md) | The working scenarios, in the order people scale: triage a local fleet, put a team on a feature, run it on a server, engineer the loop, script agents in pipelines. |
| [The sidebar](./guide/sidebar.md) | How to read the sidebar: the zones, agent cards and their lifecycle, process rows, and how the ranking decides what needs you. |
| [Security and trust](./guide/security.md) | The threat model and the guardrails: project trust, notification handlers, hook safety, and privacy settings. |

## Customization

| Page | What it covers |
| --- | --- |
| [Configuration](./reference/configuration.md) | Every setting: the four config files, agent profiles and teams, loop tasks, behavior toggles, and project config. |
| [Theming and pets](./guide/theme.md) | Schemes, palettes, color depth, glyphs, animations, provider styling, and the sidebar pets. |
| [Zellij and tmux baselines](./guide/setup.md#configure-your-multiplexer) | Recommended multiplexer settings, shipped ready to adopt under [examples/](../examples/README.md). |

## Reference

| Page | What it covers |
| --- | --- |
| [CLI](./reference/cli.md) | The command map and conventions, with a page per group: [getting started](./reference/cli/getting-started.md), [agent control](./reference/cli/agents.md), [channels](./reference/cli/channel.md), [web](./reference/cli/web.md), [hooks and trust](./reference/cli/hooks-trust.md), [maintenance](./reference/cli/maintenance.md). |
| [The sidebar on screen](./interface/sidebar.md) | Every zone, glyph, and meter of the sidebar, with rendered frames. |

## How it works

[DESIGN.md](../DESIGN.md) states the attention problem, the design pillars, and the invariants; [ARCHITECTURE.md](../ARCHITECTURE.md) is the runtime shape and the on-disk state; [docs/internals/](./internals/) documents each subsystem in depth. To work on Rimz itself, start at [CONTRIBUTING.md](../CONTRIBUTING.md).

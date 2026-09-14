# RimZ reference

These pages answer one specific question about the shipped product: what a command's flags do, what it prints, which exit code it returns, which agents RimZ supports and how far, and what an external agent plugin must declare. Use them once you know what you want to do. To learn a workflow first, start at the [user documentation](../README.md); to see how a surface works inside the code, read the [internals](../internals/README.md).

`rimz <command> --help` prints the exhaustive flag list, straight from the binary. The reference pages teach the model behind each command and the forms worth knowing.

## Commands

To find a command's page by its name, use the command map in [cli.md](./cli.md). That page also states the rules every command page assumes: the global flags, addressing, which room and backend a command reaches, script output, exit codes, durations, color, and agent prose. This table finds a page by topic.

| Page | What it covers |
| --- | --- |
| [getting-started.md](./cli/getting-started.md) | Opening, reaching, and diagnosing a local room: `rimz`, `start`, `attach`, `sessions`, `list`, `setup`, and `doctor`. |
| [remote.md](./cli/remote.md) | `rimz remote`: connecting to a room on another host over SSH, saved aliases, installing RimZ on the host, port forwarding, and opening a remote room in the browser. |
| [web.md](./cli/web.md) | `rimz web`: opening a room in the browser, read-only broadcasts, the ttyd daemons, the machine credential, and the JSON payloads. |
| [agents.md](./cli/agents.md) | `rimz agents`: launching layouts and team roles, supervised `-p` runs, resume, launch plans, registering a third-party kind, agent cards and the verbs that drive one agent, attribution, and the address grammar every agent-facing command shares. |
| [subagents.md](./cli/subagents.md) | `rimz subagents`: launching supervised children from an agent, fanout, the fleet report, waits, list and stop, profiles, and pane placement. |
| [teams.md](./cli/teams.md) | `rimz teams`: listing, inspecting, launching, resuming, and driving named teams, stage flips, and installing team bundles. |
| [asks.md](./cli/asks.md) | `rimz asks` and `rimz answer`: reading the prompt that blocks an agent, answering it in the agent's native UI, and what each agent accepts. |
| [message.md](./cli/message.md) | `rimz message`: parking text for the next turn boundary, steering a live turn, scheduling and cross-agent conditions, fan-out, the message header, reply waits, statuses, and the inbox verbs. |
| [wait.md](./cli/wait.md) | `rimz wait`: a self-only wakeup after a delay, a process exit, or a watched command, and listing or canceling pending waits. |
| [transcript.md](./cli/transcript.md) | `rimz transcript`: rendering a channel or one agent from RimZ's durable transcript log, as the human view or JSON. |
| [pane.md](./cli/pane.md) | `rimz pane`: pane targets, listing, capturing, sending text and keys, focus, zoom, split, detach, and per-pane bandwidth. |
| [events.md](./cli/events.md) | `rimz events`: following lifecycle and signal lines as JSON Lines, both line schemas and signal sources, emitting signals, and the reserved families. |
| [accounts.md](./cli/accounts.md) | `rimz accounts`: adding, listing, and removing named provider accounts, each a separate provider home. |
| [stats.md](./cli/stats.md) | `rimz stats`: the machine-wide token and dollar panel, cache freshness, the held dashboard, JSON fields, and the assist timeline. |
| [providers.md](./cli/providers.md) | `rimz providers`: login status, rate-limit windows, credits, spend, and daily-cap state, with freshness and JSON fields. |
| [budget.md](./cli/budget.md) | `rimz agents budget` and `rimz budget`: inspecting and changing one agent's dollar cap and the room and provider-account daily caps, and what a cap blocks. |
| [channel.md](./cli/channel.md) | `rimz channel`: creating, listing, and removing durable named lanes without a Git checkout. |
| [worktree.md](./cli/worktree.md) | `rimz worktree`: creating, checking out a pull request, listing, entering, landing, removing, and sweeping RimZ-owned Git worktrees. |
| [loop.md](./cli/loop.md) | `rimz loop`: clock, signal, and watch tasks; project tasks; waits and checks; budgets, the surplus gate, and strikes; run forensics; and the timer. |
| [hooks-trust.md](./cli/hooks-trust.md) | `rimz hooks` and `rimz trust`: installing and removing agent hooks, and granting or revoking project trust. |
| [config.md](./cli/config.md) | `rimz config`, `list-themes`, and `list-pets`: the config files and key routing, `init`, `path`, `get`, `set` value parsing and refusals, and the theme and pet pickers. |
| [maintenance.md](./cli/maintenance.md) | `coverage`, `workspace`, `update`, `reload`, `sidebar repair`, `reset`, `gc`, `uninstall`, and `ping`: checking adapter coverage, moving a store, rotating the event log, upgrading, repairing a room, sweeping stale state, and removing RimZ. |

## Agents

| Page | What it covers |
| --- | --- |
| [agent-support.md](./agent-support.md) | Every built-in adapter: support tiers, `rimz coverage`, the six capabilities and the compatibility matrix, per-agent gaps, config homes and skill roots, launch flags (permission modes, model, effort, prompt replacement, auto-compaction), the wiring matrix, the lifecycle hook surface, and each agent's mapping doc. |
| [agent-plugins.md](./agent-plugins.md) | External agent plugins, an early and unstable surface: registering and checking a bundle, the manifest, the canonical event envelope, the shim contract, probe contracts, and how coverage, doctor, and `rimz start` treat a plugin. |

## See also

- [User documentation](../README.md): the guide pages that teach each workflow end to end.
- [Internals](../internals/README.md): how each subsystem works, for contributors who read the code. Most reference pages link the internals page behind their own mechanics.
- [The sidebar](../interface/sidebar.md): what the sidebar shows on screen.

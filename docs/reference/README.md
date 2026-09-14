# RimZ reference

These pages answer a specific question about the shipped product: what a command's flags do, which agents RimZ supports and how far, and what an external agent plugin must declare. Look a fact up here once you know what you want to do. To learn a workflow first, start at the [user documentation](../README.md); to see how a surface works inside the code, read the [internals](../internals/README.md).

`--help` stays the exhaustive flag list, straight from the binary. These pages teach the model behind each command and the forms worth knowing.

## The command line

[cli.md](./cli.md) is the command map: it indexes every `rimz` command by scene and collects what every command page assumes: the global flags, addressing, which room and backend a command reaches, script output, exit codes, durations, color, and agent-prose rendering. Each command group has its own page under `cli/`.

| Page | What it covers |
| --- | --- |
| [getting-started.md](./cli/getting-started.md) | Opening, reaching, and diagnosing a local room: `rimz`, `start`, `attach`, `sessions`, `list`, `setup`, and `doctor`. |
| [remote.md](./cli/remote.md) | `rimz remote`: attaching to a room on another host over SSH, aliases, remote setup, and dev-server forwards. |
| [web.md](./cli/web.md) | `rimz web`: opening any room in the browser through the machine-wide ttyd daemon. |
| [agents.md](./cli/agents.md) | `rimz agents`: cards, launching panes and teams, supervised `-p` runs, focus, stop, resume, budgets, and the address grammar every agent-facing command shares. |
| [subagents.md](./cli/subagents.md) | `rimz subagents`: delegating one bounded prompt to a supervised child, fanout, waits, and the agent-only launch rule. |
| [teams.md](./cli/teams.md) | `rimz teams`: listing, inspecting, launching, resuming, and driving named teams, stage flips, and installing team bundles. |
| [asks.md](./cli/asks.md) | `rimz asks` and `rimz answer`: reading the prompt that blocks an agent and answering it in the agent's native UI. |
| [message.md](./cli/message.md) | `rimz message`: parking text for the next turn boundary, steering a live turn, scheduling, reply waits, and the inbox verbs. |
| [wait.md](./cli/wait.md) | `rimz wait`: a self-only wakeup after a delay, a process exit, or a watched command. |
| [transcript.md](./cli/transcript.md) | `rimz transcript`: rendering a channel or one agent from RimZ's durable transcript log. |
| [pane.md](./cli/pane.md) | `rimz pane`: listing, capturing, sending to, and focusing panes. |
| [events.md](./cli/events.md) | `rimz events`: following lifecycle transitions as JSON lines, the event schema, and emitting signals. |
| [accounts.md](./cli/accounts.md) | `rimz accounts`: declaring named provider accounts as separate provider homes. |
| [stats.md](./cli/stats.md) | `rimz stats`: account-global token and dollar history, windows, and breakdowns. |
| [providers.md](./cli/providers.md) | `rimz providers`: login status, rate-limit windows, credits, spend, and daily-cap state, with freshness and JSON fields. |
| [channel.md](./cli/channel.md) | `rimz channel`: durable named lanes without a Git checkout. |
| [worktree.md](./cli/worktree.md) | `rimz worktree`: creating, entering, landing, removing, and sweeping RimZ-owned Git worktrees. |
| [loop.md](./cli/loop.md) | `rimz loop`: clock, signal, and watch tasks, waits and checks, and run forensics. |
| [hooks-trust.md](./cli/hooks-trust.md) | `rimz hooks` and `rimz trust`: installing and removing agent hooks, and granting or revoking project trust. |
| [config.md](./cli/config.md) | `rimz config`, `list-themes`, and `list-pets`: reading and editing the machine config set by dotted key. |
| [maintenance.md](./cli/maintenance.md) | `coverage`, `workspace`, `update`, `reload`, `reset`, `gc`, `uninstall`, and `ping`: inspecting, repairing, and sweeping a room. |

## Agents

| Page | What it covers |
| --- | --- |
| [agent-support.md](./agent-support.md) | Every built-in adapter: support tiers, the six capabilities, the compatibility and wiring matrices, launch-prompt replacement, auto-compaction windows, the lifecycle hook surface, per-agent permission-mode mappings, and versions. |
| [agent-plugins.md](./agent-plugins.md) | External agent plugins, an early and unstable surface: registering and validating a bundle, the manifest, the canonical event envelope, the shim contract, probe contracts, and failure behavior. |

## Where to go next

- [User documentation](../README.md) — the guide pages that teach each workflow end to end.
- [Internals](../internals/README.md) — how each subsystem works, for contributors who read the code. Most reference pages link the internals page behind their mechanics: the agent model behind [agent-support.md](./agent-support.md), [subagents.md](../internals/harness/subagents.md) behind the subagent verbs, and [transcript.md](../internals/harness/transcript.md) behind `rimz transcript`.
- [interface/sidebar.md](../interface/sidebar.md) — what the sidebar shows on screen.

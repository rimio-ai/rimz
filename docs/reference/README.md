# RimZ reference

These pages answer one specific question about the shipped product: what a command's flags do, what it prints, which exit code it returns, which agents RimZ supports and how far, and what a definition file or an external agent plugin must declare. Use them once you know what you want to do. To learn a workflow first, start at the [user documentation](../README.md). To see how a surface works inside the code, read the [internals](../internals/README.md).

`rimz <command> --help` prints the flag list and defaults from the binary. The reference pages add what help leaves out: outputs, `--json` fields, refusals, and exit codes.

## Commands

To find a command's page by its name, use the command map in [cli.md](./cli.md). That page also states the rules every command page assumes: the global flags, addressing, which room and backend a command reaches, script output, exit codes, durations, color, and agent prose. It also documents `rimz keys`. This table finds a page by topic.

| Page | What it covers |
| --- | --- |
| [getting-started.md](./cli/getting-started.md) | Opening, reaching, and diagnosing a local room: `rimz`, `start`, `attach`, `sessions`, `list`, `setup`, and `doctor`. |
| [remote.md](./cli/remote.md) | `rimz remote`: connecting to a room on another host over SSH, version checks, port forwarding and the forwarded-port view, saved aliases, installing RimZ on the host, and opening a remote room in the browser. |
| [web.md](./cli/web.md) | `rimz web`: opening a room in the browser, printing its URL, the writable and broadcast daemons, read-only shares, named sign-in tokens, the JSON payloads, and configuration. |
| [palette.md](./cli/palette.md) | `rimz palette`: searching live cards, commands, and worktree groups in the terminal or a popup. |
| [agents.md](./cli/agents.md) | `rimz agents`: the address grammar every agent-facing command shares, launching layouts and team roles, supervised `-p` runs, resume, profiles and launch plans, registering a third-party kind, agent cards, and the verbs that drive one agent. |
| [subagents.md](./cli/subagents.md) | `rimz subagents`: launching supervised children from an agent, fanout, the fleet report, waits, list and stop, paused and stalled children, follow-up messages, profiles, pane placement, and the deadline ladder. |
| [teams.md](./cli/teams.md) | `rimz teams`: listing, inspecting, launching, resuming, driving, and waiting on team cohorts, team reports, board entries, and stage flips. |
| [asks.md](./cli/asks.md) | `rimz asks` and `rimz answer`: raising and withdrawing asks, reading the prompt that blocks an agent, the ask JSON, answering in the agent's native UI, and what each agent accepts. |
| [message.md](./cli/message.md) | `rimz message`: parking text for the next turn boundary, steering a live turn, conditions, fan-out, the message header, statuses, and the inbox verbs. |
| [wait.md](./cli/wait.md) | `rimz wait`: a self-only wakeup after a delay, a process exit, a polled check, a file change, or a watched command, and listing or canceling pending waits. |
| [transcript.md](./cli/transcript.md) | `rimz transcript`: rendering a channel or one agent from RimZ's durable transcript log, as the human view or JSON. |
| [pane.md](./cli/pane.md) | `rimz pane`: pane targets, listing, capturing, sending text and keys, focus, zoom, split, detach, and per-pane bandwidth. Also `rimz sidebar frame` and its section filter. |
| [sidebar.md](./cli/sidebar.md) | `rimz sidebar`: walking cards and worktrees, numbered slots, previous-card focus, needs-you filtering, closing panes, and reopening agent cards. |
| [events.md](./cli/events.md) | `rimz events`: listing recent signals, following lifecycle and signal lines as JSON Lines, both line schemas, emitting signals, and the reserved families. |
| [accounts.md](./cli/accounts.md) | `rimz accounts`: comparing provider plans, usage, launch health, and account details, declaring, selecting, and removing accounts, and redeeming Claude or Codex reset credits. |
| [stats.md](./cli/stats.md) | `rimz stats`: the machine-wide token and dollar panel, cache freshness, the held dashboard, JSON fields, and the assist timeline. |
| [budget.md](./cli/budget.md) | `rimz agents tune … budget` and `rimz budget`: reading and changing agent, room, and provider-account dollar caps, and what a cap blocks. |
| [lsp.md](./cli/lsp.md) | `rimz lsp`: shared language-server queries, reading source by anchor, notes-anchor checks, editor attachment, the shim, listing, stopping, status, exit codes, and configuration. |
| [worktree.md](./cli/worktree.md) | `rimz worktree`: creating, checking out a pull request, listing, entering, printing paths, landing, removing, and sweeping RimZ-owned Git worktrees. |
| [loop.md](./cli/loop.md) | `rimz loop`: clock, signal, and watch tasks, project tasks, waits and checks, budgets, the surplus gate, the start throttle, strikes, run forensics, and the timer. |
| [workflow.md](./cli/workflow.md) | `rimz workflow`: starting, listing, showing, canceling, resuming, restarting, and validating workflow runs, the fields of each node kind, the schema, the waiting line, on-break handling, failure text, workflow loop tasks, the JSON shapes, and exit codes. |
| [hooks-trust.md](./cli/hooks-trust.md) | `rimz hooks` and `rimz trust`: installing and removing agent hooks, granting or revoking project trust, and agent folder trust. |
| [config.md](./cli/config.md) | `rimz config`, `list-themes`, and `list-pets`: the config files, `init`, `path`, `get`, `set` value parsing and refusals, and the theme and pet pickers. |
| [packs.md](./cli/packs.md) | `rimz packs`: Git and linked sources, install review and confirmation, updates, listing and showing packs, exclusions, customized overrides, the manifest, the installation record, and exit codes. |
| [notes.md](./cli/notes.md) | `rimz notes`: saving a Markdown note from stdin or a file, the store layout, the header RimZ stamps, slugs and numbered names, listing the newest matches through `--filter` over every header key and the note's age, paths alone with `-q`, and the JSON shape. |
| [paths.md](./cli/paths.md) | `rimz paths`: the RimZ home, the room's state and runtime directories, and every account-global directory, as a table or JSON. |
| [maintenance.md](./cli/maintenance.md) | `coverage`, `workspace`, `update`, `reload`, `sidebar repair`, `reset`, `gc`, `uninstall`, and `ping`: checking adapter coverage, resolving and moving a store, rotating the event log, upgrading, repairing and resetting a room, sweeping stale state, and removing RimZ. |

## Agents and definitions

| Page | What it covers |
| --- | --- |
| [definitions.md](./definitions.md) | Machine Markdown definitions: trees and tiers, frontmatter keys, inheritance chains, kind bases, traits, tools, delegation and skills, teams and seats, stages, signals, compaction and keep-warm, validation, and moving existing configuration. |
| [agent-support.md](./agent-support.md) | Every built-in adapter: `rimz coverage`, the six capabilities and the compatibility matrix, per-agent notes, config homes and skill roots, launch flags (provider-session resume, permission modes, model and effort, fast mode, prompt replacement, interrupt key, auto-compaction), the wiring matrix, the lifecycle hook surface, and each agent's mapping page. |
| [agent-plugins.md](./agent-plugins.md) | External agent plugins, an early and unstable surface: registering and checking a bundle, the bundle layout and manifest, the canonical event envelope, the shim contract, probe contracts, coverage and doctor, and the protocol version. |

## Rooms

| Page | What it covers |
| --- | --- |
| [keymaps.md](./keymaps.md) | The room keymaps under `[room] style = "rimz"`: choosing `rimz`, `tmux`, `zellij` or `vim`, default prefixes, the key grammar, the modes and their stickiness, scrollback flavours, every action's tmux and Zellij effect, each bundled table with what it leaves unbound, and the tmux prefix option. |

## See also

- [User documentation](../README.md): the guide pages that teach each workflow end to end.
- [Internals](../internals/README.md): how each subsystem works, for contributors who read the code.
- [The sidebar](../interface/sidebar.md): what the sidebar shows on screen.

# The rimz command line

> See [DESIGN.md](../../DESIGN.md#commitments) for the commitments this surface operationalizes.

`rimz` runs one room per project and gives you the verbs to live in it: open and attach the room, launch and steer agents, ask questions from scripts, and keep the ledger-backed workspace healthy. Every command resolves to the room for the directory you run it in, so you can call it from any pane, any worktree, or a script on the same machine and reach the same workspace.

This page is the map. It orients you, indexes every command, and collects the conventions that hold across all of them. Each command group has its own page with the full synopsis, examples, and edge cases.

## Start here

Open the room for the project you are in:

```sh
cd ~/code/query-engine
rimz
```

`rimz` resolves the workspace, creates or reattaches the Zellij or tmux session, opens the sidebar, and drops you in. From there you work three ways, each with its own entry point:

- **Drive agents live.** Launch them into panes and tabs with [`rimz agents`](./cli/agents.md#agents), then [`steer`](./cli/agents.md#steer-live-agents) and [`queue`](./cli/agents.md#queue-the-next-message) text to them by name.
- **Script the fleet.** Run one supervised agent turn with [`rimz agents … -p`](./cli/agents.md#supervised-runs--p) and branch on its exit code, or block on a human decision with [`rimz feed ask`](./cli/feed.md#feed-items-and-decisions).
- **Reach a room anywhere.** Attach over SSH with [`rimz remote`](./cli/getting-started.md#remote-rooms).

## Find a command

| Group | Commands | Reference |
| --- | --- | --- |
| **Open and connect rooms** | `rimz`, `start`, `attach`, `remote`, `list`, `setup`, `doctor` | [Getting started](./cli/getting-started.md) |
| **Work with agents** | `agents`, `transcript`, `steer`, `queue`, `pane`, `worktree`, `loop` | [Agent control](./cli/agents.md) |
| **Decisions, hooks, and trust** | `feed`, `event`, `resolver`, `hooks`, `trust` | [Feed, resolvers, hooks, and trust](./cli/feed.md) |
| **Configure and maintain** | `config`, `coverage`, `list-pets`, `list-themes`, `workspace`, `reload`, `reset`, `gc`, `ping` | [Maintenance](./cli/maintenance.md) |

Two surfaces have their own reference outside this map: [`rimz config`](./configuration.md) edits the per-machine config (the [maintenance page](./cli/maintenance.md#configure-the-machine) covers the command mechanics), and [`rimz stats`](../internals/reach/welcome.md#rimz-stats) renders the token-activity lobby.

## Start and attach a workspace

```sh
rimz [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>]
rimz start [PATH] [same flags]
rimz attach [SESSION] [same flags]
```

`rimz` is `rimz start .`. `rimz start [PATH]` opens the room for a path; `rimz attach [SESSION]` attaches by exact session name, or by the current directory when you omit the name.

**One room per root.** Rimz chooses the room by walking outward from the path: an explicit `--root` wins, then the enclosing git repository, then a project marker, then the directory itself as a directory workspace. That choice is the *root class*, and it is what makes the same `query-engine` repo resolve to one room whether you run `rimz` from its top or a nested package. The session pins `RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, so commands run in panes that wander through subdirectories still write to the one ledger.

**Attach or print.** An interactive terminal attaches; a non-interactive caller prints the attach command instead, which is the shape scripts and shell wrappers want. `--attach`, `--no-attach`, and `--print` force the choice (`--print` is an alias for `--no-attach`).

**Resume on rebirth.** When a room comes back from a reboot or a crashed multiplexer, Rimz offers to recover the agents that were running, defaulting yes; non-interactive starts recover. `--no-resume` brings the room up empty. Live agents in a healthy room are never touched by this — the flag only governs the recovery launch.

Full bootstrap, remote, list, setup, and doctor examples are in [Getting started](./cli/getting-started.md).

## Conventions

These hold across the whole CLI, so each command page assumes them rather than repeating them.

**`--help` is the flag reference.** Every command and subcommand prints its full flags and defaults with `--help`. These pages teach the model and the forms worth knowing; they leave the exhaustive switch list to `--help`, which never drifts from the binary.

**Addressing agents.** `steer`, `queue`, `transcript`, and the `agents` management verbs all name agents the same way — `@<handle>` for who, `#<channel>` for which worktree or in-place team, or a raw pane id. The one canonical explanation is [Addressing agents](./cli/agents.md#addressing-agents).

**Pick the backend with `--mux`.** When both Zellij and tmux are installed, `--mux zellij` or `--mux tmux` chooses the backend for that invocation. With one installed, Rimz uses it.

**Override the room with `--root`.** `--root <path>` overrides workspace resolution — the escape hatch for monorepos and deliberate directory rooms.

**Scripting output is `--json`.** Read commands that take `--json` emit a stable, machine-readable document; that is the surface scripts should parse. Human tables stay compact and may change to read better. Supervised runs use their own `--output-format` instead of `--json`.

**Exit codes carry the verdict.** A supervised [`agents … -p`](./cli/agents.md#supervised-runs--p) run exits `0` completed, `1` failed, `124` timed out, `130` canceled, so a script branches on the result without parsing output. Other commands follow the usual `0` success / non-zero error convention. [`rimz ping`](./cli/maintenance.md#ping) prints `ok` for a liveness probe.

**Durations and sizes are unit strings.** Timeouts and intervals take `s`, `m`, `h`, and `d` (`30s`, `15m`, `4h`, `30d`); byte sizes take `B`, `KB`, `KiB`, `MB`, `MiB`, `GB`, and `GiB`.

**Color follows the terminal.** `--color auto` (the default) honors the terminal and the `NO_COLOR`/`CLICOLOR` environment; `--color always` and `--color never` force it.

## Commands Rimz calls for you

Hidden helper commands are the machinery behind hooks, sidebars, statuslines, and agent wrappers. They stay out of `rimz --help` and are not part of the user-facing contract, so they can change between releases.

They include `rimz sidebar …` (the sidebar's data and focus API), `rimz statusline feed` (the installed statusline datasource), `rimz hooks feed` (the hook decision entrypoint), `rimz queue deliver` (the turn-boundary delivery helper), `rimz agents exec` and `rimz agents auto-continue` (the launch wrapper and the rate-limit-reset nudge), `rimz agents refresh-usage` (the per-provider account-usage probe), `rimz loop run` (the scheduled-turn runner), `rimz worktree cleanup`, and `rimz codex …` (the Codex session-enrichment helpers). The protocols behind them live in the owning internals docs: [ledger](../internals/sidebar/ledger.md), [state](../internals/sidebar/state.md), [agent](../internals/agents/agent.md), [provider](../internals/agents/provider.md), and [harness](../internals/agents/harness.md).

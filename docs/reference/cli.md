# The rimz command line

`rimz` is one binary. Run it in a project and it opens that project's room — a Zellij or tmux session with the sidebar — and gives you the verbs to live in it: launch and steer agents, read the fleet, schedule turns, and keep the workspace healthy. Every command resolves to the room for the directory you run it in, so the same command reaches the same workspace from any pane, any worktree, or a script on the machine. How Rimz picks that room from the directory is [the root model in ARCHITECTURE.md](../../ARCHITECTURE.md).

This page is the map: it indexes every command and collects the conventions that hold across all of them. Each command has a page with the full synopsis, flags, and what it does on your machine.

## Find a command

| Scene | Commands | Reference |
| --- | --- | --- |
| **Open and attach a room** | `rimz`, `start`, `attach`, `list`, `setup`, `doctor` | [Getting started](./cli/getting-started.md) |
| **Reach a room anywhere** | `remote`, `web` | [Remote](./cli/remote.md) · [Web](./cli/web.md) |
| **Run and steer agents** | `agents`, `asks`, `answer`, `message`, `transcript`, `pane` | [Agents](./cli/agents.md) · [Asks](./cli/asks.md) · [Message](./cli/message.md) · [Transcript](./cli/transcript.md) · [Pane](./cli/pane.md) |
| **Cost and usage** | `stats` | [Stats](./cli/stats.md) |
| **Lanes, worktrees, and schedules** | `channel`, `worktree`, `loop` | [Channels](./cli/channel.md) · [Worktrees](./cli/worktree.md) · [Loop](./cli/loop.md) |
| **Hooks and trust** | `hooks`, `trust` | [Hooks and trust](./cli/hooks-trust.md) |
| **Configure appearance and behavior** | `config`, `list-themes`, `list-pets` | [Config](./cli/config.md) |
| **Maintain and recover** | `coverage`, `workspace`, `reload`, `reset`, `gc`, `uninstall`, `ping` | [Maintenance](./cli/maintenance.md) |

The [configuration guide](../guide/configuration.md) owns the file model behind `rimz config`; the [config reference](./cli/config.md) covers the command mechanics.

## Conventions

These hold across the whole CLI, so each command page assumes them rather than repeating them.

**`--help` is the flag reference.** Every command and subcommand prints its full flags and defaults with `--help`, straight from the binary. These pages teach the model and the forms worth knowing; they leave the exhaustive switch list to `--help`, which never drifts.

**Addressing agents.** `message`, `transcript`, `pane`, and the `agents` management verbs all name agents the same way: `@<handle>` for who, `#<channel>` for which named lane, worktree, or in-place team, or a raw pane id. The one canonical explanation is [Addressing agents](./cli/agents.md#addressing-agents).

**`--root` overrides the room.** Any command takes `--root <path>` to target a workspace other than the one the cwd resolves to — the escape hatch for monorepos and deliberate directory rooms. The session also pins `RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, so commands run in panes that wander through subdirectories still reach the one store. The resolution model is in [ARCHITECTURE.md](../../ARCHITECTURE.md).

**Pick the backend with `--mux`.** When both Zellij and tmux are installed, `--mux zellij` or `--mux tmux` chooses the backend for that invocation; `--zellij` and `--tmux` are shorthands. With one installed, Rimz uses it.

**Scripting output is `--json`.** Read commands that take `--json` emit a stable, machine-readable document; that is the surface scripts parse. Human tables stay compact and may change to read better. Supervised runs shape their own output with `--output-format` instead.

**Exit codes carry the verdict.** A supervised [`agents … -p`](./cli/agents.md#supervised-runs--p) run exits `0` completed, `1` failed, `124` timed out, `130` canceled, so a script branches without parsing output. Other commands follow the usual `0` success / non-zero error convention. [`rimz ping`](./cli/maintenance.md#ping) prints `ok` for a liveness probe.

**Durations and sizes are unit strings.** Timeouts and intervals take `s`, `m`, `h`, and `d` (`30s`, `15m`, `4h`, `30d`); byte sizes take `B`, `KB`, `KiB`, `MB`, `MiB`, `GB`, and `GiB`.

**Color follows the terminal.** `--color auto` (the default) honors the terminal and the `NO_COLOR`/`CLICOLOR` environment; `--color always` and `--color never` force it.

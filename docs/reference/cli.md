# CLI reference

`rimz` is one binary. Run bare `rimz` in a project and it opens that project's room, a Zellij or tmux session with the sidebar, and attaches you to it; every other command acts on the room for the directory you run it in, so the same command reaches the same room from any pane, worktree, or script on the machine. How RimZ picks the room from a directory is [the root model in ARCHITECTURE.md](../../ARCHITECTURE.md).

This page indexes every command and states the rules that hold across all of them. Each command page assumes these rules instead of repeating them, and `rimz <command> --help` prints the full flag list and defaults for any command or subcommand.

## Find a command

| Scene | Commands | Reference |
| --- | --- | --- |
| Open and attach a room | `rimz`, `start`, `attach`, `sessions`, `list`, `setup`, `doctor` | [Getting started](./cli/getting-started.md) |
| Reach a room from elsewhere | `remote`, `web` | [Remote](./cli/remote.md) · [Web](./cli/web.md) |
| Run and steer agents | `agents`, `subagents`, `teams`, `asks`, `answer`, `message` (alias `msg`), `wait`, `transcript`, `pane`, `events` | [Agents](./cli/agents.md) · [Subagents](./cli/subagents.md) · [Teams](./cli/teams.md) · [Asks](./cli/asks.md) · [Message](./cli/message.md) · [Wait](./cli/wait.md) · [Transcript](./cli/transcript.md) · [Pane](./cli/pane.md) · [Events](./cli/events.md) |
| Provider accounts | `accounts` | [Accounts](./cli/accounts.md) |
| Cost, usage, and budgets | `stats`, `providers`, `budget` | [Stats](./cli/stats.md) · [Providers](./cli/providers.md) · [Budget](./cli/budget.md) |
| Lanes, worktrees, and schedules | `channel`, `worktree`, `loop` | [Channels](./cli/channel.md) · [Worktrees](./cli/worktree.md) · [Loop](./cli/loop.md) |
| Hooks and trust | `hooks`, `trust` | [Hooks and trust](./cli/hooks-trust.md) |
| Configure appearance and behavior | `config`, `list-themes`, `list-pets` | [Config](./cli/config.md) |
| Maintain and recover | `coverage`, `workspace`, `update`, `reload`, `reset`, `gc`, `uninstall`, `ping` | [Maintenance](./cli/maintenance.md) |

Tab completion for commands, flags, and live room names is set up in the [shell completion guide](../guide/setup.md#shell-completion).

## Global flags

These flags work on every command and in any position after `rimz`.

| Flag | Effect |
| --- | --- |
| `--root <PATH>` | Target the room for `PATH` instead of the one the current directory resolves to. See [Which room a command reaches](#which-room-a-command-reaches). |
| `--mux <zellij\|tmux>` | Use this backend for the invocation. See [Which backend a command uses](#which-backend-a-command-uses). |
| `--zellij`, `--tmux` | Shorthands for `--mux zellij` and `--mux tmux`. Passing more than one backend selector fails with `choose one of --mux, --zellij, --tmux`. |
| `--color <auto\|always\|never>` | When to color human output. Default `auto`. See [Color](#color). |
| `-h`, `--help` | Print the flags and defaults of the command it follows. |
| `-V`, `--version` | Print the RimZ version (top level only). |

Bare `rimz`, `start`, and `attach` also take launch options (`--attach`, `--no-attach` or `--print`, `--no-resume`, `--refresh-ms`), and `start` takes `--account`, covered in [Start the room](./cli/getting-started.md#start-the-room).

## Conventions

### Addressing agents

`message`, `transcript`, `pane`, and the `agents` management verbs name agents the same way: `@<handle>` for who, `#<channel>` for which named lane, worktree, or in-place team, or a raw pane id such as `tmux:%12`. The grammar is defined once, in [Addressing agents](./cli/agents.md#addressing-agents).

### Which room a command reaches

A command reaches the room for the directory it runs in. RimZ resolves that directory to its enclosing Git repository, else the nearest project-marker directory, else the directory itself, so every worktree of one repository shares one room.

Inside a room, panes carry the room's identity in `RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, set when the room is born. Commands that act on the running room's agents (`agents`, `subagents`, `teams`, `asks`, `answer`, `budget`, `message`, `wait`, `pane`, `transcript`, `events`, `hooks`) follow that identity, so they reach the same room even from a subdirectory that is its own repository. Commands that open, configure, or maintain a room by path (`start`, `attach`, `web`, `channel`, `worktree`, `trust`, `loop add`, `gc`, `doctor`, `setup`, `reset`, `workspace`) resolve from the directory alone.

`--root <PATH>` overrides both. Use it in a monorepo whose packages you run as separate rooms, or to reach a room from outside its directory.

### Which backend a command uses

RimZ picks the multiplexer backend in this order and stops at the first step that decides:

1. `--mux`, `--zellij`, or `--tmux` on the command line.
2. The multiplexer you are already inside, detected from `ZELLIJ` or `ZELLIJ_PANE_ID`, then `TMUX` or `TMUX_PANE`.
3. `default` in the `[mux]` config table. If that backend is not installed, the command fails and names it.
4. The installed backend. When both Zellij and tmux are installed, RimZ uses tmux.

With neither installed, the command fails with `no multiplexer found: install zellij or tmux`. The `[mux]` table is described in the [configuration guide](../guide/configuration.md#multiplexer-room-options).

### Output for scripts

Parse `--json`, not the human tables. Read commands that print a table or card take `--json` and emit a machine-readable document; the human layout changes between releases to read better. Supervised runs (`rimz agents <spec> <prompt> -p`) shape their output with `--output-format text|json|stream-json` instead, described in [Supervised runs](./cli/agents.md#supervised-runs--p).

### Exit codes

Most commands exit with one of three codes:

| Code | Meaning |
| --- | --- |
| `0` | The command succeeded. |
| `1` | The command failed. The error is on stderr, usually with the fix. |
| `2` | The command line was invalid: an unknown command or flag, or a value that does not parse. |

Commands that report an outcome add their own codes, so a script branches without parsing output:

| Command | Codes | Reference |
| --- | --- | --- |
| `agents -p`, `agents wait`, `subagents wait`, `message --wait` | The run's status: `0` completed, `1` failed, `123` verify failed, `124` timed out, `125` budget exceeded, `130` canceled | [Supervised runs](./cli/agents.md#supervised-runs--p) |
| `subagents <PROFILE>`, `subagents fanout` | `125` a room or account cap, or a Qwen quota window, refused the launch; with `--wait`, the run's status | [Launch one child](./cli/subagents.md#launch-one-child) |
| `answer` | `2` the target is not asking, its ask is no longer current, or its pane cannot be reached; `3` the answer is invalid for the ask or the agent does not support structured answers | [Answer an ask](./cli/asks.md#answer-an-ask) |

### Durations

A duration is one integer followed by one unit: `30s`, `15m`, `4h`, `30d`. Compound values such as `1h30m` are rejected. The units are `ms`, `s`, `m`, `h`, and `d`, and each flag accepts the subset that fits it (a supervised `--timeout` takes `s` through `d`; `gc --older-than` stops at `h`). A value with a unit the flag does not accept fails at parse time and lists the units it takes:

```console
$ rimz gc --older-than 1h30m
error: invalid value '1h30m' for '--older-than <OLDER_THAN>': unknown duration unit `h30m`; use s/m/h
```

### Color

`--color auto`, the default, colors output when stdout is a terminal. The environment adjusts that decision, checked in this order:

| Condition | Result |
| --- | --- |
| `NO_COLOR` is set and non-empty | No color. |
| `CLICOLOR_FORCE` is set and non-empty | Color, even through a pipe. |
| `CLICOLOR=0` | No color. |
| stdout is a terminal, and `TERM` is set to something other than `dumb` (or `CLICOLOR` is set, or `CI` is set) | Color. |
| Anything else | No color. |

`--color always` and `--color never` skip these checks.

### Agent prose

Agent-authored text that a command prints in full, such as a supervised run's answer, a message body, or an ask's context, renders as Markdown when stdout is colored: headings, emphasis, links, lists, task lists, code, quotes, and tables, wrapped to the terminal width (at most 100 columns). When color is off (piped output, `--color never`, or the environment rules above), the same text prints exactly as the agent wrote it, which is what scripts and other agents read. `--color always` forces the rendered form through a pipe. `--json` output and one-line previews always carry the raw text.

# The rimz command line

> See [DESIGN.md](../../DESIGN.md#invariants) for the invariants this surface operationalizes.

`rimz` runs one room per project and gives you the verbs to live in it: open and attach the room, launch and steer agents, and keep the store-backed workspace healthy. Every command resolves to the room for the directory you run it in, so you can call it from any pane, any worktree, or a script on the same machine and reach the same workspace.

This page is the map. It orients you, indexes every command, and collects the conventions that hold across all of them. Each command group has its own page with the full synopsis, examples, and edge cases.

## Start here

Open the room for the project you are in:

```sh
cd ~/code/query-engine
rimz
```

`rimz` resolves the workspace, creates or reattaches the Zellij or tmux session, opens the sidebar, and drops you in. From there you work three ways, each with its own entry point:

- **Drive agents live.** Launch them into panes and tabs with [`rimz agents`](./cli/agents.md#agents), then use [`rimz message`](./cli/message.md) to interrupt now with `--steer`, park for the next boundary, or schedule the earliest delivery time.
- **Script the fleet.** Run one supervised agent turn with [`rimz agents … -p`](./cli/agents.md#supervised-runs--p) and branch on its exit code, or put turns on a schedule with [`rimz loop`](./cli/loop.md).
- **Reach a room anywhere.** Attach over SSH with [`rimz remote`](./cli/getting-started.md#remote-rooms), or open a Zellij room in the browser with [`rimz web`](./cli/web.md).

## Find a command

| Group | Commands | Reference |
| --- | --- | --- |
| **Open and connect rooms** | `rimz`, `start`, `attach`, `remote`, `web`, `list`, `stats`, `setup`, `doctor` | [Getting started](./cli/getting-started.md) · [Web](./cli/web.md) · [Stats](../internals/welcome.md#rimz-stats) |
| **Run and steer agents** | `agents`, `message`, `transcript`, `pane` | [Agents](./cli/agents.md) · [Message](./cli/message.md) · [Transcript](./cli/transcript.md) · [Pane](./cli/pane.md) |
| **Lanes and schedules** | `channel`, `worktree`, `loop` | [Channels](./cli/channel.md) · [Worktrees](./cli/worktree.md) · [Loop](./cli/loop.md) |
| **Hooks and trust** | `hooks`, `trust` | [Hooks and trust](./cli/hooks-trust.md) |
| **Configure and maintain** | `config`, `coverage`, `list-pets`, `list-themes`, `workspace`, `reload`, `reset`, `gc`, `uninstall`, `ping` | [Maintenance](./cli/maintenance.md) |

One surface has its own reference outside this map: [`rimz config`](./configuration.md) edits the per-machine config; the [maintenance page](./cli/maintenance.md#configure-the-machine) covers the command mechanics.

## One room per root

Rimz chooses the room by walking outward from the directory you run it in: an explicit `--root` wins, then the enclosing git repository, then a project marker, then the directory itself as a directory workspace. That choice is the *root class*, and it is what makes the same `query-engine` repo resolve to one room whether you run `rimz` from its top or from a nested package.

The session pins `RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, so commands run in panes that wander through subdirectories still write to the one store. `--root <path>` overrides resolution on any command; it is the escape hatch for monorepos and deliberate directory rooms.

The commands that open, attach, and recover rooms (`rimz`, `start`, `attach`) are covered in [Getting started](./cli/getting-started.md#start-the-room).

## Conventions

These hold across the whole CLI, so each command page assumes them rather than repeating them.

**`--help` is the flag reference.** Every command and subcommand prints its full flags and defaults with `--help`. These pages teach the model and the forms worth knowing; they leave the exhaustive switch list to `--help`, which never drifts from the binary.

**Addressing agents.** `message`, `transcript`, and the `agents` management verbs all name agents the same way: `@<handle>` for who, `#<channel>` for which named lane, worktree, or in-place team, or a raw pane id. The one canonical explanation is [Addressing agents](./cli/agents.md#addressing-agents).

**Pick the backend with `--mux`.** When both Zellij and tmux are installed, `--mux zellij` or `--mux tmux` chooses the backend for that invocation; `--zellij` and `--tmux` are shorthands for those forms. With one installed, Rimz uses it.

**Scripting output is `--json`.** Read commands that take `--json` emit a stable, machine-readable document; that is the surface scripts should parse. Human tables stay compact and may change to read better. Supervised runs use their own `--output-format` instead of `--json`.

**Exit codes carry the verdict.** A supervised [`agents … -p`](./cli/agents.md#supervised-runs--p) run exits `0` completed, `1` failed, `124` timed out, `130` canceled, so a script branches on the result without parsing output. Other commands follow the usual `0` success / non-zero error convention. [`rimz ping`](./cli/maintenance.md#ping) prints `ok` for a liveness probe.

**Durations and sizes are unit strings.** Timeouts and intervals take `s`, `m`, `h`, and `d` (`30s`, `15m`, `4h`, `30d`); byte sizes take `B`, `KB`, `KiB`, `MB`, `MiB`, `GB`, and `GiB`.

**Color follows the terminal.** `--color auto` (the default) honors the terminal and the `NO_COLOR`/`CLICOLOR` environment; `--color always` and `--color never` force it.

## Commands Rimz calls for you

Hidden helper commands are the machinery behind hooks, sidebars, statuslines, and agent wrappers. They stay out of `rimz --help` and are not part of the user-facing contract, so they can change between releases.

| Command | What it powers |
| --- | --- |
| `rimz sidebar …` | The sidebar's data and focus API |
| `rimz statusline feed` | The installed statusline datasource |
| `rimz hooks feed` | The hook decision entrypoint |
| `rimz message deliver` | Turn-boundary message delivery |
| `rimz message sweep` | The scheduled-message wake helper |
| `rimz agents exec` | The launch wrapper around every agent process |
| `rimz agents auto-continue` | The rate-limit-reset nudge |
| `rimz agents refresh-usage` | The per-provider account-usage probe |
| `rimz loop run` | The scheduled-turn runner |
| `rimz worktree cleanup` | Supervised worktree cleanup after a pane closes |
| `rimz web token ensure` | Login-token provisioning for the remote web relay |
| `rimz codex …` | Codex session-enrichment helpers |

The protocols behind them live in the owning internals docs: [store](../internals/store.md), [state](../internals/sidebar/state.md), [agent](../internals/agents/model.md), [provider](../internals/agents/providers.md), and [harness](../internals/harness/harness.md).

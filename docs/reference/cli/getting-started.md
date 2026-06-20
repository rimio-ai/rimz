# Getting Started CLI

Rimz starts one room for the workspace you are in, then keeps that room attachable from the local machine or over SSH. This page covers the bootstrap commands: start, attach, remote, list, setup, and doctor.

For the product contract behind one root mapping to one room, see [DESIGN.md](../../../DESIGN.md#commitments). For generated config and setup, see [configuration.md](../configuration.md). For remote reconnect and link-health mechanics, see [remote.md](../../internals/reach/remote.md).

## Quick examples

Start from a project directory and enter the room:

```sh
rimz setup
cd ~/code/query-engine
rimz
```

Create or reattach a room for another path and print the attach command for a script:

```sh
rimz start --print ~/code/query-engine
rimz attach --print
```

Connect to a remote project path without saving an alias:

```sh
rimz remote connect dev-box:~/code/query-engine
```

Save a remote alias, connect to it, and inspect local room state:

```sh
rimz remote add prod agent@prod-box:query-engine
rimz remote connect prod
rimz list
rimz doctor --audit
```

## When to use

| Need | Command |
| --- | --- |
| Open the current project's room fast | `rimz` |
| Start or reattach a room for a path | `rimz start [PATH]` |
| Attach by current directory or exact session name | `rimz attach [SESSION]` |
| Connect to a remote room over SSH | `rimz remote connect <alias-or-target>` |
| Save, rename, list, or remove remote aliases | `rimz remote add`, `rename`, `list`/`ls`, `del`/`rm` |
| Find known rooms and their live mux backend | `rimz list` |
| Bootstrap this machine's default config | `rimz setup` |
| Explain backend, hook, trust, resolver, room, and sidebar health | `rimz doctor` |

`--mux <MUX>` overrides backend selection for a command. `--root <ROOT>` overrides workspace-root resolution when a monorepo or nested directory needs an explicit room boundary.

## Start the room

```sh
rimz [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>]
rimz start [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>] [PATH]
rimz attach [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>] [SESSION]
```

`rimz` is the default action and behaves like `rimz start .`. `rimz start [PATH]` resolves the workspace root, creates or reattaches the Zellij or tmux session, launches the sidebar pane, and enters the room when stdin and stdout are interactive TTYs.

`--attach` forces Rimz to enter the mux session. `--no-attach` and `--print` print the attach command instead, which is the usual shape for scripts, shell wrappers, and non-interactive terminals.

`PATH` defaults to `.`. Workspace resolution follows the product commitment: explicit `--root`, then the enclosing git repository, then project-marker directories, then the directory itself as a directory workspace.

Inside the selected mux backend, the automatic `rimz` path reports the selected directory's room and exits so the existing client stays active. Use `--attach` only when you explicitly want to hand control to the mux attach command.

Reborn rooms prompt to recover prior agents, defaulting yes; non-interactive starts recover. `--no-resume` skips that recovery. Existing live agents stay live; the flag controls the recovery launch path.

`--refresh-ms <ms>` overrides the sidebar render cadence for sidebars born by this launch. Persistent cadence lives in machine config.

`rimz attach` with no `SESSION` uses the current directory's workspace. `rimz attach <SESSION>` targets an exact session name; if Rimz has a workspace record for that session, it restores the room's sidebar and recovery state before attaching.

## Remote rooms

```sh
rimz remote add <NAME> <TARGET> [--no-reconnect] [--no-resume] [--mux <MUX>]
rimz remote connect <ALIAS_OR_TARGET> [--reset] [--no-reconnect] [--attach|--no-attach|--print]
rimz remote reset <ALIAS_OR_TARGET> [--no-reconnect] [--attach|--no-attach|--print]
rimz remote del <NAME>
rimz remote rm <NAME>
rimz remote rename <OLD> <NEW>
rimz remote list [--json]
rimz remote ls [--json]
```

Raw remote targets use `[user@]host:<session-or-path>`. A target after the colon that contains `/` or starts with `~` is a remote path and runs remote `rimz start`; a bare word is a remote session name and runs remote `rimz attach`.

Examples of valid raw targets are `dev-box:query-engine`, `dev-box:~/code/query-engine`, `agent@prod-box:/srv/query-engine`, and `user@[::1]:query-engine`. Spell another user's home as an absolute path, such as `/home/alice/code`, because `~user` does not expand through the guarded remote command.

`rimz remote connect <target>` builds the guarded `ssh -t` command locally and runs the remote host's own `rimz`. Your SSH config, keys, ports, and jump hosts apply through normal `ssh` resolution.

`rimz remote add <name> <target>` saves an alias in `~/.config/rimz/remote.toml`. `rimz remote connect <name>` resolves that alias; an input containing `:` is always treated as a raw target, and every other input is treated as an alias name.

Remote reconnect is enabled by default. `--no-reconnect` hands the link to one SSH run for that invocation; `remote add --no-reconnect` saves that one-shot default on the alias.

`rimz remote connect --reset <alias-or-target>` and `rimz remote reset <alias-or-target>` pass `--no-resume` to the remote `rimz`, so a remote room born or reborn by that command comes up empty instead of recovering prior agents. `remote add --no-resume` saves the same birth behavior on the alias.

`--attach`, `--no-attach`, and `--print` mirror local attach behavior. `--print` emits the SSH command instead of executing it.

For `remote add`, `--mux <MUX>` becomes part of the saved alias only when the flag is scoped to `remote` or `add`, such as `rimz remote --mux tmux add prod prod-box:query-engine` or `rimz remote add --mux tmux prod prod-box:query-engine`. A top-level `rimz --mux tmux remote add ...` affects only that invocation and is not saved.

`rimz remote list` and `rimz remote ls` list saved aliases; `--json` emits machine-readable rows. `rimz remote del` and `rimz remote rm` delete an alias. `rimz remote rename <old> <new>` changes the alias name while keeping its target and saved defaults.

## List rooms

```sh
rimz list [-a|--all] [--json]
```

`rimz list` joins known Rimz workspace records with live Zellij and tmux sessions. The default view shows running rooms and rooms active in the last 24 hours.

`-a` and `--all` include dormant known workspaces. `--json` emits `workspace_id`, `project_root`, `session_name`, `running_on`, and `last_activity` for scripts.

## Setup and doctor

```sh
rimz setup [--yes]
```

`rimz setup` prints a first-run report: selected multiplexer, workspace root, root class, trust state when available, per-machine config path, detected agent binaries, and hook install status.

In an interactive terminal, setup offers to keep and refresh an existing per-machine config against the current templates; incompatible or unknown keys are skipped with a warning. The prompt also offers a clean overwrite. `--yes` takes the non-interactive path: it merges existing files, writes missing files, and makes no hook installs or trust grants.

Run `rimz config init --force` for an explicit clean reset. The generated config path and field model are described in [configuration.md](../configuration.md).

```sh
rimz doctor [--audit] [--json] [--output PATH]
```

`rimz doctor` reports the resolved workspace, backend and version, session health, duplicate live sidebar sessions for the workspace, sidebar pane, agent hook status and capability coverage, remote-control state, room tree, Rimz storage footprint, protocol versions, trust state, unauthorized resolver heartbeats, agent rollup, and recent sidebar diagnostics. The human report renders each area as a titled section with a status glyph carrying the verdict.

`--audit` expands the agent rollup with durable historical detail. `--json` emits the same report as one machine-readable document, with typed states and raw timestamps for diffing or tooling. `--output PATH` writes the report (human text, or JSON with `--json`) to a file atomically instead of stdout. Run doctor first when a room, hook, resolver, sidebar, or backend behaves unexpectedly; each failing check prints the next fix where Rimz knows one.

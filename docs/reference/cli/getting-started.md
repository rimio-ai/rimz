# Getting started CLI

Rimz opens one room for the project you are in and keeps that room attachable — locally or over SSH. This page covers the commands that bootstrap and reach a room: `start`, `attach`, `remote`, `list`, `setup`, and `doctor`.

The shared model behind these — how Rimz picks the room, and when it attaches versus prints the attach command — is in [cli.md → Start and attach a workspace](../cli.md#start-and-attach-a-workspace). This page is the per-command detail.

```sh
rimz setup                 # one-time: detect the machine, write default config
cd ~/code/query-engine
rimz                        # open the room and drop in
```

| Need | Command |
| --- | --- |
| Open the current project's room | `rimz` |
| Start or reattach a room for a path | `rimz start [PATH]` |
| Attach by directory or exact session name | `rimz attach [SESSION]` |
| Connect to a remote room over SSH | `rimz remote connect <alias-or-target>` |
| Save, rename, list, or remove remote aliases | `rimz remote add` / `rename` / `list` / `del` |
| Find known rooms and their live backend | `rimz list` |
| Bootstrap this machine's config | `rimz setup` |
| Diagnose backend, hook, trust, and room health | `rimz doctor` |

## Start the room

```sh
rimz [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <MS>]
rimz start [PATH] [same flags]
rimz attach [SESSION] [same flags]
```

`rimz` is `rimz start .`. `rimz start [PATH]` resolves the workspace root, creates or reattaches the Zellij or tmux session, launches the sidebar, and enters the room on an interactive terminal. `PATH` defaults to `.`.

`rimz attach` with no `SESSION` uses the current directory's room. `rimz attach <SESSION>` targets an exact session name; when Rimz has a workspace record for it, it restores the room's sidebar and recovery state before attaching.

A few specifics:

- Inside the selected mux backend, the automatic `rimz` path reports the directory's room and exits so the existing client stays active. Use `--attach` only to deliberately hand control to the mux attach command.
- `--no-resume` skips recovering prior agents when a room is reborn; live agents are unaffected. The default recovers (yes on a prompt, automatically when non-interactive).
- `--refresh-ms <ms>` overrides the sidebar render cadence for sidebars born by this launch; the persistent cadence lives in machine config.

## Remote rooms

```sh
rimz remote add <NAME> <TARGET> [--no-reconnect] [--no-resume] [--mux <MUX>]
rimz remote connect <ALIAS_OR_TARGET> [--reset] [--no-reconnect] [--attach|--no-attach|--print]
rimz remote reset <ALIAS_OR_TARGET> [--no-reconnect] [--attach|--no-attach|--print]
rimz remote del|rm <NAME>
rimz remote rename <OLD> <NEW>
rimz remote list|ls [--json]
```

`rimz remote connect` builds a guarded `ssh -t` command locally and runs the remote host's own `rimz`, so your SSH config, keys, ports, and jump hosts all apply through normal `ssh` resolution.

A raw target is `[user@]host:<session-or-path>`. After the colon, a value containing `/` or starting with `~` is a remote path and runs remote `rimz start`; a bare word is a remote session name and runs remote `rimz attach`. Valid targets include `dev-box:query-engine`, `dev-box:~/code/query-engine`, `agent@prod-box:/srv/query-engine`, and `user@[::1]:query-engine`. Spell another user's home as an absolute path (`/home/alice/code`), because `~user` does not expand through the guarded command.

- `rimz remote add <name> <target>` saves an alias in `~/.config/rimz/remote.toml`. Any input with a `:` is treated as a raw target; everything else is an alias name.
- Reconnect supervision is on by default. `--no-reconnect` hands the link to one SSH run; `remote add --no-reconnect` saves that as the alias default.
- `remote connect --reset` and `remote reset` pass `--no-resume` to the remote `rimz`, so a remote room comes up empty instead of recovering; `remote add --no-resume` saves that birth behavior.
- `--attach`, `--no-attach`, and `--print` mirror local behavior; `--print` emits the SSH command instead of running it.
- For `remote add`, `--mux` is saved on the alias only when scoped to `remote` or `add` (`rimz remote add --mux tmux …`); a top-level `rimz --mux tmux remote add …` affects only that invocation.

Link-health and reconnect mechanics are in [remote.md](../../internals/reach/remote.md).

## List rooms

```sh
rimz list [-a|--all] [--json]
```

`rimz list` joins known Rimz workspace records with live Zellij and tmux sessions. The default view shows running rooms and rooms active in the last 24 hours; `--all` includes dormant ones. `--json` emits `workspace_id`, `project_root`, `session_name`, `running_on`, and `last_activity` for scripts.

## Setup and doctor

```sh
rimz setup [--yes]
```

`rimz setup` prints a first-run report — selected multiplexer, workspace root and class, trust state, config path, detected agent binaries, and hook install status. In an interactive terminal it offers to keep and refresh an existing config against the current templates (skipping incompatible keys with a warning) or to overwrite cleanly. `--yes` takes the non-interactive path: merge existing files, write missing ones, and make no hook installs or trust grants. For an explicit clean reset, use `rimz config init --force`; the config model is in [configuration.md](../configuration.md).

```sh
rimz doctor [--audit] [--json] [--output PATH]
```

`rimz doctor` is the first thing to run when a room, hook, resolver, sidebar, or backend behaves unexpectedly. It reports the resolved workspace, backend and version, session health, duplicate sidebars, hook status, remote-control state, room tree, storage footprint, protocol versions, trust state, unauthorized resolver heartbeats, the agent rollup, and recent sidebar diagnostics — each as a titled section with a status glyph, and each failing check printing the next fix where Rimz knows one. `--audit` expands the rollup with historical detail, `--json` emits the whole report as one machine-readable document, and `--output PATH` writes it atomically to a file. Static adapter coverage has its own command, [`rimz coverage`](./maintenance.md#adapter-coverage).

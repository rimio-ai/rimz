# Getting started CLI

Rimz opens one room for the project you are in and keeps that room attachable, locally or over SSH. This page covers the commands that bootstrap and reach a room: `start`, `attach`, `remote`, `list`, `setup`, and `doctor`. How Rimz picks the room in the first place is [cli.md → One room per root](../cli.md#one-room-per-root).

```sh
rimz setup                 # one-time: detect the machine, write config, choose hooks and appearance
cd ~/code/query-engine
rimz                       # open the room and drop in
```

| Need | Command |
| --- | --- |
| Open the current project's room | `rimz` |
| Start or reattach a room for a path | `rimz start [PATH]` |
| Attach by directory or exact session name | `rimz attach [SESSION]` |
| Connect to a remote room over SSH | `rimz remote connect <alias-or-target>` |
| Open a Zellij room in the browser | `rimz web open [PATH]` |
| Save, update, rename, list, or remove remote aliases | `rimz remote add` / `update` / `rename` / `list` / `rm` |
| Attribute render-stream bytes in the current room | `rimz remote bandwidth` |
| Find known rooms and their live backend | `rimz list` |
| Bootstrap this machine's config | `rimz setup` |
| Diagnose backend, hook, trust, and room health | `rimz doctor` |

## Start the room

```sh
rimz [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <MS>]
rimz start [PATH] [same flags]
rimz attach [SESSION] [same flags]
```

`rimz` is `rimz start .`. `rimz start [PATH]` resolves the workspace root, creates or reattaches the Zellij or tmux session, launches the sidebar, and enters the room; `PATH` defaults to `.`.

`rimz attach` with no `SESSION` uses the current directory's room. `rimz attach <SESSION>` targets an exact session name; when Rimz has a workspace record for it, it restores the room's sidebar and recovery state before attaching.

**Attach or print.** An interactive terminal attaches; a non-interactive caller prints the attach command instead, which is the shape scripts and shell wrappers want. `--attach`, `--no-attach`, and `--print` force the choice (`--print` is an alias for `--no-attach`). Inside the selected mux backend, the automatic `rimz` path reports the directory's room and exits so the existing client stays active; use `--attach` only to deliberately hand control to the mux attach command.

**Resume on rebirth.** When a room comes back from a reboot or a crashed multiplexer, Rimz offers to recover the agents that were running, defaulting yes; non-interactive starts recover automatically. `start` prints context first, such as `rimz: this room's previous session ended with agents still running (2026-07-02 17:37)`, before the recovery prompt names the count and labels. `--no-resume` brings the room up empty. Live agents in a healthy room are never touched by this; the flag only governs the recovery launch.

`--refresh-ms <MS>` overrides the sidebar render cadence for sidebars born by this launch; the persistent cadence lives in machine config.

## Remote rooms

```sh
rimz remote add dev-box dev-box:query-engine     # save an alias
rimz remote connect dev-box                      # attach the saved room over SSH
rimz remote connect dev-box --web                # open the remote Zellij web UI locally
rimz remote connect agent@prod-box:/srv/query-engine
rimz remote bandwidth --secs 5                   # attribute pane write-rate in this room
```

`rimz remote connect` builds a guarded `ssh -t` command locally and runs the remote host's own `rimz`, so your SSH config, keys, ports, and jump hosts all apply through normal `ssh` resolution.

A raw target is `[user@]host:<session-or-path>`. After the colon, a value containing `/` or starting with `~` is a remote path and runs remote `rimz start`; a bare word is a remote session name and runs remote `rimz attach`. Valid targets include `dev-box:query-engine`, `dev-box:~/code/query-engine`, `agent@prod-box:/srv/query-engine`, and `user@[::1]:query-engine`. Spell another user's home as an absolute path (`/home/alice/code`), because `~user` does not expand through the guarded command.

| Subcommand | Effect |
| --- | --- |
| `remote connect <alias-or-target>` | Attach the room over SSH, reconnect-supervised |
| `remote add <name> <target>` | Save an alias in `~/.config/rimz/remote.toml` |
| `remote update <name> <target>` | Replace a saved alias's target and flags |
| `remote rename <old> <new>` | Rename a saved alias |
| `remote list` | Print saved aliases |
| `remote rm <name>` | Remove a saved alias |
| `remote reset <alias-or-target>` | Connect with recovery skipped, so the remote room comes up empty |
| `remote bandwidth` | Attribute pane write-rate inside a served room |

The details that matter in practice:

- `remote add` treats any input with a `:` as a raw target and everything else as an alias name. On an existing name it prompts to overwrite in an interactive terminal and errors otherwise, so a saved alias is never silently replaced; use `remote update` in a script. `update` takes the same flags as `add`, errors when the alias does not exist, and resets flags you do not pass to their defaults.
- Reconnect supervision is on by default. `--no-reconnect` hands the link to one SSH run; `remote add --no-reconnect` saves that as the alias default.
- `remote connect --reset` and `remote reset` pass `--no-resume` to the remote `rimz`; `remote add --no-resume` saves that birth behavior on the alias.
- `--attach`, `--no-attach`, and `--print` mirror local behavior; `--print` emits the SSH command instead of running it.
- For `remote add` and `remote update`, `--mux`, `--zellij`, or `--tmux` given anywhere on the invocation is saved on the alias; `rimz remote connect --mux <name>` keeps `--mux` as a per-invocation override.
- `rimz remote bandwidth [--secs N] [--json]` runs on the Linux host serving the room and samples VFS write-rate counters to attribute per-pane terminal output on both backends; tmux reports pane pids natively, and Zellij pane pids resolve through Rimz's process matcher. Use it inside the room when a remote attach looks chatty; full-screen TUIs such as agents mid-turn or system monitors should dominate the report.

### Remote rooms in the browser

`rimz remote connect <target> --web` opens the remote room in your local browser instead of your terminal. The sequence:

1. Runs remote `rimz web open --print --json` over a prep connection, asking the recovery prompt there when your terminal is interactive.
2. Relays the serving machine's cached Zellij web login token.
3. Starts a supervised SSH local-forward tunnel to the remote Zellij web server.
4. Prints the bare `http://127.0.0.1:<port>/<session>` URL and opens your local browser best-effort.
5. Stays in the foreground until Ctrl-C.

`--web-port <port>` pins the local browser origin; otherwise Rimz derives a stable port from the session name in `8300..8399`.

Link health, web tunneling, reconnect mechanics, and bandwidth attribution are in [remote.md](../../internals/reach/remote.md).

## List rooms

```sh
rimz list [-a|--all] [--json]
```

`rimz list` joins known Rimz workspace records with live Zellij and tmux sessions. The default view shows running rooms and rooms active in the last 24 hours with a `LAST_SEEN` column; `--all` includes dormant ones and renders a recorded death as `crashed · 16 agents · 2026-07-02 17:37`. `--json` emits `workspace_id`, `project_root`, `session_name`, `running_on`, `last_activity`, and `last_death` for scripts.

## Setup and doctor

```sh
rimz setup [--yes]
```

`rimz setup` prints a first-run report: selected multiplexer, workspace root and class, trust state, config path, detected agent binaries, and hook install status. In an interactive terminal it offers to keep and refresh an existing config against the current templates (skipping incompatible keys with a warning) or to overwrite cleanly, offers hook install for detected agents with missing hooks, then asks the color-and-icon probe and pet questions. `--yes` takes the non-interactive path: merge existing files, write missing ones, and make no hook installs, trust grants, or appearance changes. For an explicit clean reset, use `rimz config init --force`; the config model is in [configuration.md](../configuration.md).

```sh
rimz doctor [--audit] [--json] [--output PATH]
```

`rimz doctor` is the first thing to run when a room, hook, sidebar, or backend behaves unexpectedly. Each check prints as a titled section with a status glyph, and each failing check prints the next fix where Rimz knows one. The report covers:

- **Identity and paths** — current OS user, absolute rimz binary path, resolved workspace with absolute paths
- **Backend** — backend and version, PATH-visible backend binaries, server-log excerpts, server socket, session health, duplicate sidebars
- **Integration** — hook status, remote-control state, protocol versions, trust state
- **State** — storage footprint, live agent problem rows, message-delivery failures, recent sidebar diagnostics
- **Verdict** — the closing summary line

`--audit` widens the agent section to every observed session, `--json` emits the whole report as one machine-readable document, and `--output PATH` writes it atomically to a file. Static adapter coverage has its own command, [`rimz coverage`](./maintenance.md#adapter-coverage).

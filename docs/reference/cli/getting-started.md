# Getting started CLI

`rimz` opens one room for the project you are in and keeps it attachable. The room is a plain Zellij or tmux session with the sidebar added — your keybinds, layout, and scrollback stay yours — so leaving is a normal multiplexer detach and [`rimz reset`](./maintenance.md#reload-reset-gc-and-uninstall) is the escape hatch if one ever wedges. This page covers the commands that open, reach, and diagnose a local room: `start`, `attach`, `list`, `setup`, and `doctor`. Reaching a room on another host is [`rimz remote`](./remote.md); how Rimz picks the room from your directory is [the root model in ARCHITECTURE.md](../../../ARCHITECTURE.md).

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
| Find known rooms and their live backend | `rimz list` |
| Bootstrap this machine's config | `rimz setup` |
| Diagnose backend, hook, trust, and room health | `rimz doctor` |
| Connect to a room on another host | `rimz remote connect <alias-or-target>` |

## Start the room

```sh
rimz [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <MS>]
rimz start [PATH] [same flags]
rimz attach [SESSION] [same flags]
```

`rimz` is `rimz start .`. `rimz start [PATH]` resolves the workspace root, creates or reattaches the Zellij or tmux session, launches the sidebar, and enters the room; `PATH` defaults to `.`. Nothing is destroyed on the way in — an existing session is reattached, not replaced.

`rimz attach` with no `SESSION` uses the current directory's room. `rimz attach <SESSION>` targets an exact session name; when Rimz has a workspace record for it, it restores the room's sidebar and recovery state before attaching.

**Attach or print.** An interactive terminal attaches; a non-interactive caller prints the attach command instead, which is the shape scripts and shell wrappers want. `--attach`, `--no-attach`, and `--print` force the choice (`--print` is an alias for `--no-attach`). Inside the selected mux backend, the automatic `rimz` path reports the directory's room and exits so the existing client stays active; use `--attach` only to deliberately hand control to the mux attach command.

**Resume on rebirth.** When a room comes back from a reboot or a crashed multiplexer, Rimz offers to recover the agents that were running, defaulting yes; non-interactive starts recover automatically. `start` prints context first, such as `rimz: this room's previous session ended with agents still running (2026-07-02 17:37)`, before the recovery prompt names the count and labels. `--no-resume` brings the room up empty. Live agents in a healthy room are never touched by this; the flag only governs the recovery launch.

`--refresh-ms <MS>` overrides the sidebar render cadence for sidebars born by this launch; the persistent cadence lives in machine config.

## List rooms

```sh
rimz list [-a|--all] [--json]
```

`rimz list` joins known Rimz workspace records with live Zellij and tmux sessions. The default view shows running rooms and rooms active in the last 24 hours with a `LAST_SEEN` column; `--all` includes dormant ones and renders a recorded death as `crashed · 16 agents · 2026-07-02 17:37`. `--json` emits `workspace_id`, `project_root`, `session_name`, `running_on`, `last_activity`, and `last_death` for scripts. It reads records only and changes no room.

## Setup and doctor

```sh
rimz setup [--yes]
```

`rimz setup` prints a first-run report — selected multiplexer, workspace root and class, trust state, config path, detected agent binaries, and hook install status — and makes changes only with your consent. In an interactive terminal it offers to keep and refresh an existing config against the current templates (skipping incompatible keys with a warning) or to overwrite cleanly, offers hook install for detected agents with missing hooks, then asks the color-and-icon probe and pet questions. `--yes` takes the non-interactive path: merge existing files, write missing ones, and make no hook installs, trust grants, or appearance changes. For an explicit clean reset of config, use [`rimz config init --force`](./config.md#read-and-edit-config); the config model is the [configuration guide](../../guide/configuration.md).

```sh
rimz doctor [--audit] [--json] [--output PATH]
```

`rimz doctor` is the first thing to run when a room, hook, sidebar, or backend behaves unexpectedly. It reads and reports only; it changes nothing. Each check prints as a titled section with a status glyph, and each failing check prints the next fix where Rimz knows one. The report covers:

- **Identity and paths** — current OS user, absolute rimz binary path, resolved workspace with absolute paths
- **Backend** — backend and version, PATH-visible backend binaries, server-log excerpts, server socket, session health, duplicate sidebars
- **Integration** — hook status, remote-control state, protocol versions, trust state
- **State** — storage footprint, live agent problem rows, message-delivery failures, recent sidebar diagnostics
- **Last incident** — previous room death cause and time, lost agents, `recovered N of M`, and the crash forensic archive path when a prior incarnation died
- **Verdict** — the closing summary line

`--audit` widens the agent section to every observed session, `--json` emits the whole report as one machine-readable document, and `--output PATH` writes it atomically to a file. Static adapter coverage has its own command, [`rimz coverage`](./maintenance.md#adapter-coverage).

# Getting started CLI

`rimz` opens one room for the project you are in and keeps it attachable. The room is a plain Zellij or tmux session with the sidebar added — your keybinds, layout, and scrollback stay yours — so leaving is a normal multiplexer detach and [`rimz reset`](./maintenance.md#update-reload-reset-gc-and-uninstall) is the escape hatch if one ever wedges. This page covers the commands that open, reach, and diagnose a local room: `start`, `attach`, `sessions`, `list`, `setup`, and `doctor`. Reaching a room on another host is [`rimz remote`](./remote.md); how RimZ picks the room from your directory is [the root model in ARCHITECTURE.md](../../../ARCHITECTURE.md).

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
| Pick, create, and enter rooms interactively | `rimz sessions` |
| Find known rooms and their live backend | `rimz list` |
| Open the current room in a browser | `rimz web open` |
| Bootstrap this machine's config | `rimz setup` |
| Diagnose backend, hook, trust, and room health | `rimz doctor` |
| Connect to a room on another host | `rimz remote connect <alias-or-target> [--force-version]` |

Enable command, flag, and live room completion from the [shell completion setup](../../guide/setup.md#shell-completion).

## Start the room

```sh
rimz [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <MS>]
rimz start [PATH] [same flags]
rimz attach [SESSION] [same flags]
```

`rimz` is `rimz start .`. `rimz start [PATH]` resolves the workspace root, creates or reattaches the Zellij or tmux session, launches the sidebar, and enters the room; `PATH` defaults to `.`. Nothing is destroyed on the way in — an existing session is reattached, not replaced.

When `[web] enabled = true`, room start also ensures the shared ttyd browser daemon best-effort. A missing ttyd or occupied `[web] port` prints a warning and leaves the room available; use [`rimz web`](./web.md) to inspect or control browser access directly.

`rimz attach` with no `SESSION` uses the current directory's room. `rimz attach <SESSION>` targets an exact session name; when RimZ has a workspace record for it, it restores the room's sidebar and recovery state before attaching.

**Attach or print.** An interactive terminal attaches; a non-interactive caller prints the attach command instead, which is the shape scripts and shell wrappers want. `--attach`, `--no-attach`, and `--print` force the choice (`--print` is an alias for `--no-attach`). Inside the selected mux backend, the automatic `rimz` path reports the directory's room and exits so the existing client stays active; use `--attach` only to deliberately hand control to the mux attach command.

**Resume on rebirth.** When a room comes back from a reboot or a crashed multiplexer, RimZ offers to recover the agents that were running, defaulting yes; non-interactive starts recover automatically. `start` prints context first, such as `rimz: this room's previous session ended with agents still running (2026-07-02 17:37)`, before the recovery prompt names the count and labels. `--no-resume` brings the room up empty. Live agents in a healthy room are never touched by this; the flag only governs the recovery launch.

`--refresh-ms <MS>` overrides the sidebar render cadence for sidebars born by this launch; the persistent cadence lives in machine config.

## Pick a session

```sh
rimz sessions
```

`rimz sessions` opens the themed manager for every live RimZ room on the machine. Each card shows the repository and path, live root-agent counts and attention, and the configured headline session, token, and spend totals. Rooms with prompt activity in the last 24 hours lead, then the remaining rooms follow by recent workspace activity.

Use ↑/↓ or j/k to move, Enter to attach, printable keys to filter repository names and paths, Backspace to edit, and Esc to clear the filter before quitting. `n` opens the new-session selector, so that letter is reserved instead of entering the filter.

The new-session selector combines dormant known workspaces with a directory browser rooted at `$HOME`. Use ↑/↓ to move, → or Tab to descend, ← or Backspace on an empty filter to ascend, Enter to birth and attach a room for the selected path, and Esc to return to the live-room list. Unreadable directories keep the selector available with an empty directory section and a notice.

Run the manager from a plain terminal. It refuses to nest inside tmux or Zellij and directs an existing mux client to `rimz attach`; non-terminal stdio prints the live-session listing and exits nonzero.

## List rooms

```sh
rimz list [-a|--all] [--json]
```

`rimz list` joins known RimZ workspace records with live Zellij and tmux sessions. The default view shows running rooms and rooms active in the last 24 hours with a `LAST_SEEN` column; `--all` includes dormant ones and renders a recorded death as `crashed · 16 agents · 2026-07-02 17:37`. `--json` emits `workspace_id`, `project_root`, `session_name`, `running_on`, `last_activity`, and `last_death` for scripts. It reads records only and changes no room.

## Setup and doctor

```sh
rimz setup [--yes]
```

`rimz setup` prints a first-run report — selected multiplexer, workspace root and class, trust state, config path, detected agent binaries, and hook install status — and makes changes only with your consent. In an interactive terminal it offers to keep and refresh an existing config against the current templates (removing unknown keys with a named report) or to overwrite cleanly, then presents one summarized install or refresh for detected agents with missing hooks or stale RimZ-owned whole-file integrations. The same refresh removes unknown keys from `~/.agents` fragments while preserving their comments and recognized settings; unparseable fragments stay untouched. The summary names every affected file and points to `rimz hooks install --dry-run` for exact unified diffs before the color-and-icon probe and pet questions. `--yes` takes the non-interactive path: merge existing files, repair fragments, write missing files, and make no hook installs, upgrades, trust grants, or appearance changes. For an explicit clean reset of config, use [`rimz config init --force`](./config.md#read-and-edit-config); the config model is the [configuration guide](../../guide/configuration.md).

```sh
rimz doctor [--audit] [--clear] [--json] [--output PATH]
```

`rimz doctor` is the first thing to run when a room, hook, sidebar, or backend behaves unexpectedly. It reads and reports only unless you pass `--clear`. Each check prints as a titled section with a status glyph, and each failing check prints the next fix where RimZ knows one. The report covers:

- **Identity and paths** — current OS user, absolute rimz binary path, resolved workspace with absolute paths
- **Machine config** — parse and validation failures from the four machine files and every discovered `~/.agents` fragment, with source paths and fixes
- **Backend** — backend and version, PATH-visible backend binaries, scoped and grouped logical server-log issues with routine lifecycle records counted on one line, room ownership and liveness per backend, owner-scoped presence, accepted Zellij plugin-generation telemetry, server socket, session health, duplicate sidebars
- **Integration** — a roll-up of the agents already reporting, a row per agent whose hooks need a command, a footer summarizing absent agents whose hooks are offered automatically after installation, remote-control state, protocol versions, trust state
- **State** — storage footprint, live agent problem rows, message-delivery failures, and evidence-folded sidebar incidents with independent source severity, state, and impact
- **Last incident** — previous room death cause and time, lost agents, `recovered N of M`, and the crash forensic archive path when a prior incarnation died
- **Verdict** — the closing summary line: counts by severity, the sections holding them, and what each glyph asks of you

`--audit` widens the agent section to every observed session, `--json` emits the whole report as one machine-readable `rimz.doctor.v1` document, and `--output PATH` writes it atomically to a file. The mux log object states its `host_user { uid }` or `server` scope and carries scan counts, the watermark it cut at with the count of records that fell before it, and grouped issues with occurrence timestamps and bounded samples. Diagnostics carry grouped incidents with record and observer counts, suppression/drop totals, evidence references, and `investigate`, `contained`, `recovered`, or `expected` state; only investigative `warn`/`alarm` impact affects the closing health tally. `--clear` stamps the current workspace so diagnostics, the last incident, message failures, and multiplexer log records written through that moment stay out of this and future reports; the diagnostic files, server logs, and durable event log remain on disk for direct inspection. Static adapter coverage has its own command, [`rimz coverage`](./maintenance.md#adapter-coverage).

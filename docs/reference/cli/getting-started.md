# Getting started CLI

These commands open, find, and diagnose a room on this machine: bare `rimz`, `start`, `attach`, `sessions`, `list`, `setup`, and `doctor`. A room is a plain Zellij or tmux session with the RimZ sidebar added, so your keybinds, layout, and scrollback stay yours, and leaving it is a normal multiplexer detach. Which room a directory maps to, and the global flags (`--root`, `--mux`, `--color`), are defined on the [CLI reference](../cli.md#which-room-a-command-reaches). A room on another host is [`rimz remote`](./remote.md); a room in the browser is [`rimz web`](./web.md).

```sh
rimz setup                 # once per machine: report, write config, offer hooks and appearance
cd ~/code/query-engine
rimz                       # open this project's room and attach
```

| Need | Command |
| --- | --- |
| Open the current project's room | `rimz` |
| Open or reattach the room for a path | `rimz start [PATH]` |
| Attach to a room by session name | `rimz attach [SESSION]` |
| Pick, create, and enter rooms interactively | `rimz sessions` |
| List known rooms and the backend running each | `rimz list` |
| Report the machine and write default config | `rimz setup` |
| Diagnose backend, hooks, trust, and room health | `rimz doctor` |

Command, flag, and live session-name completion is set up in the [shell completion guide](../../guide/setup.md#shell-completion).

## Start the room

```sh
rimz [--attach | --no-attach | --print] [--no-resume] [--refresh-ms <MS>]
rimz start [PATH] [launch options] [--account <KIND=NAME>]...
rimz attach [SESSION] [launch options]
```

Bare `rimz` is `rimz start .`. `rimz start [PATH]` resolves the room for `PATH` (default `.`), creates its session if none is running, launches the sidebar, and attaches. A running session is reattached as it is, on the backend it runs under; start never replaces or restarts it, and a `--mux` naming the other backend fails and prints both fixes (attach to the running room, or reset it). [`rimz reset`](./maintenance.md#update-reload-reset-gc-and-uninstall) is the command that rebuilds a room.

| Launch option | Effect |
| --- | --- |
| `--attach` | Attach to the session even when RimZ would print the attach command. |
| `--no-attach`, `--print` | Print the attach command on stdout instead of attaching. |
| `--no-resume` | Bring a reborn room up empty, without recovering its prior agents. See [Resume on rebirth](#resume-on-rebirth). |
| `--refresh-ms <MS>` | Sidebar render cadence in milliseconds for sidebars this launch creates, clamped to 16 through 1000. The persistent setting is `refresh_ms` under [`[theme.display]`](../../guide/theme.md#display). |
| `--account <KIND=NAME>` | `start` only, repeatable. Launch that provider's agents under a named account. See [Accounts](#accounts). |

`--attach`, `--no-attach`, and `--print` are mutually exclusive.

### Attach or print

Without `--attach` or `--print`, RimZ attaches only when both stdin and stdout are terminals and you are not already inside the selected backend. Otherwise it prints the attach command on stdout, which is what a script or shell wrapper wants. When RimZ attaches, it exits with the multiplexer client's exit code once you detach.

| Situation | `rimz` and `rimz start` | `rimz attach` |
| --- | --- | --- |
| Terminal, outside the backend | Attach | Attach |
| stdin or stdout is not a terminal | Print the attach command | Print the attach command |
| Inside a session of the selected backend | Report this directory's room on stderr and exit 0 | Print the attach command |
| `--attach` | Attach | Attach |
| `--no-attach` or `--print` | Print the attach command | Print the attach command |

The report from inside a session names the room instead of nesting one session in another:

```console
$ rimz
You're already inside a zellij session, which can't host a nested room.
This directory's room is `rimz-rimz-f89e49`. Detach to (re)launch it, or run `rimz` from outside the session.
```

Inside a session, `rimz start --account` fails instead, because accounts apply only when a room is born.

### Attach by session name

`rimz attach` with no `SESSION` opens the current directory's room, creating it if needed. Unlike `start`, it skips the first-run, hook, and trust prompts below, takes no `--account`, and does not start the browser daemon. `rimz attach <SESSION>` takes an exact session name, the `SESSION` column of [`rimz list`](#list-rooms). When RimZ has a workspace record for that session, it restores the room's sidebar and recovery state before attaching. A session RimZ has no record for is attached, or printed, as a plain multiplexer session.

### Prompts at room birth

A `rimz start` that creates the session, run from a terminal, can ask up to four things before it attaches. A start that reattaches a running session asks none of them; `rimz attach` asks only the recovery question.

| Prompt | When | Default |
| --- | --- | --- |
| First-run questions: truecolor, Nerd Font icons, pet, hands-off automation | This start wrote the machine config for the first time | The current settings |
| Install or refresh hooks for the listed agents | A detected agent has no RimZ hooks, or a stale RimZ-owned integration | Enter installs all; `n` or end of input installs none |
| Trust this project's config | The project ships `.rimz/config.toml` with an ungranted executable surface, and you have not declined this version | No |
| Recover prior agents | The room is reborn with agents from its previous session | Yes |

A start without a terminal on stdin, including a remote reconnect, asks nothing: it prints a notice for missing hooks, leaves trust unchanged, and recovers prior agents. Hooks are covered in [Hooks and trust](./hooks-trust.md), trust in the [security guide](../../guide/security.md).

### Resume on rebirth

A room is reborn when `start` finds its record but no running session, typically after a reboot or a multiplexer crash. RimZ first prints why the previous session ended, then offers to recover the agents that were running:

```console
$ rimz
rimz: this room's previous session ended with agents still running (2026-07-02 17:37)
Recover 2 agents (claude, codex)? [Y/n]
```

After a reboot the first line reads `rimz: machine rebooted since this room was last open (...)`. The recovered agents are reported on stderr as `rimz: resumed 2 agents: ...`, and any left behind as `rimz: not resumed: <label> (<reason>)`. `--no-resume` skips recovery and brings the room up empty; the durable records stay, so a later rebirth without the flag can still recover them. A healthy running room is never affected. The `[resume]` settings are in the [setup guide](../../guide/setup.md#resume-agents-after-a-reboot).

### Accounts

`--account <KIND=NAME>` (for example `--account claude=work`) launches that provider's agents under a named account declared with [`rimz accounts`](./accounts.md); `default` names the provider's own home. Without the flag, a new room takes the project's `[accounts]` selection from `.rimz/config.toml`, else `default`. The selection is fixed when the room is born. `start` refuses, before building the room, in these cases:

| Case | Fix it names |
| --- | --- |
| `--account` differs from the room's recorded accounts | `rimz reset --account <KIND=NAME>` |
| The project sets `[accounts]` but is untrusted or its trust is stale | Review with `rimz trust`, then `rimz trust grant` |
| The account is not declared | `rimz accounts add <kind> <name>` |
| The account's home is missing or lacks RimZ hooks | `rimz accounts add <kind> <name>` |
| The account's hooks are installed but the agent has not trusted them | The agent-specific step the error prints |

The workflow is in the [accounts guide](../../guide/accounts.md).

### Browser access at start

When `[web] enabled = true`, `rimz start` also starts the shared ttyd browser daemon. This is best-effort: a missing ttyd or an occupied `[web] port` prints a warning (`rimz: browser daemon was not started: ...`) and the room opens anyway. `rimz attach` does not start the daemon. [`rimz web`](./web.md) inspects and controls browser access.

## Pick a session

```sh
rimz sessions
```

`rimz sessions` opens a full-screen manager of every live RimZ room on the machine. Each room is a two-line card: the repository name and path, then live agent counts by kind (`claude ×2`), a `●` count of agents that need attention, and the session, token, and spend totals of the sidebar's headline window ([`spend_window`](../../guide/configuration.md#sidebar-rendering)). Rooms with a prompt in the last 24 hours come first, newest prompt first; the rest follow by most recent workspace activity. Detaching from a room you entered returns you to the manager.

| Key (room list) | Action |
| --- | --- |
| `↑` `↓`, `k` `j`, scroll wheel | Move the selection |
| `Enter`, or click the selected card | Attach to the selected room |
| Any other printable key | Add to the filter on repository name and path |
| `Backspace` | Delete the last filter character |
| `n` | Open the new-session selector |
| `Esc` | Clear the filter; with an empty filter, quit |
| `Ctrl-C` | Quit |

The filter cannot contain `j`, `k`, or `n`, because those keys act first.

The new-session selector lists dormant known workspaces, then the current directory, then its non-hidden subdirectories, starting at `$HOME`. Typing filters the list.

| Key (new session) | Action |
| --- | --- |
| `↑` `↓`, scroll wheel | Move the selection |
| `→`, `Tab` | Descend into the selected directory |
| `←`, or `Backspace` with an empty filter | Go up one directory |
| `Enter` | Create the room for the selected path and attach |
| `Esc` | Return to the room list |

An unreadable directory shows an empty directory list with a notice. `rimz sessions` refuses to run inside Zellij or tmux and points at `rimz attach`. Without a terminal on stdin and stdout, it prints the live sessions in its error message and exits 1.

The browser's room picker is the same manager ([web guide](../../guide/web.md)).

## List rooms

```sh
rimz list [-a | --all] [--json]
```

`rimz list` joins RimZ's workspace records with the live Zellij and tmux sessions, matched by session name. By default it shows running rooms and rooms active in the last 24 hours; `--all` adds dormant ones. Running rooms sort first, then by most recent activity. It reads records only and changes no room. With no rows, it prints nothing.

| Column | Content |
| --- | --- |
| `WORKSPACE` | Workspace id (`ws_...`). |
| `SESSION` | Multiplexer session name, the argument to `rimz attach`. |
| `PROJECT_ROOT` | Absolute project root. |
| `RUNNING` | `zellij`, `tmux`, or `-` when no session is live. |
| `LAST_SEEN` | Last activity as `YYYY-MM-DD HH:MM`. A stopped room whose last session died shows the death instead, such as `crashed · 16 agents · 2026-07-02 17:37` or `rebooted · 1 agent · ...`. |

`--json` prints an array with one object per row:

| Field | Type |
| --- | --- |
| `workspace_id` | string |
| `project_root` | string |
| `session_name` | string |
| `running_on` | `"zellij"`, `"tmux"`, or `null` |
| `last_activity` | RFC 3339 timestamp or `null` |
| `last_death` | the `LAST_SEEN` death summary string, or `null` |

## Set up the machine

```sh
rimz setup [--yes]
```

`rimz setup` prints a report (multiplexer and version, project root and root class, trust state, config path and whether it exists, and each agent's binary location and hook status) and then acts according to how it was run:

| Run | What it changes |
| --- | --- |
| From a terminal | If config exists, asks `Keep your current config?` (default yes): yes merges it against the current templates, no overwrites it with fresh templates. Then repairs `~/.agents` fragments, writes a missing `remote.toml` template, offers one hook install or refresh for detected agents, and asks the truecolor, Nerd Font, pet, and hands-off automation questions. |
| `--yes` | Merges config against the templates, repairs `~/.agents` fragments, and writes missing files. Installs no hooks, grants no trust, and changes no appearance or automation setting. |
| Without a terminal and without `--yes` | Nothing. Prints the report and `No terminal input is available; setup changed nothing.` |

A merge names every file it wrote, merged, or left untouched. An unparseable file is left as it is, and interactive setup stops at that point until you fix it. What a merge keeps and removes is defined in the [configuration guide](../../guide/configuration.md#generate-and-refresh-the-files). The hook summary points at `rimz hooks install --dry-run` for the exact diffs. [`rimz config init --force`](./config.md#read-and-edit-config) is the clean reset. The walkthrough is the [setup guide](../../guide/setup.md).

## Diagnose with doctor

```sh
rimz doctor [--audit] [--json] [--output <PATH>] [--clear]
```

`rimz doctor` reports the machine, the backend, and the room for the current directory in one pass. It changes nothing unless you pass `--clear`, and it exits 0 whatever it finds; read the closing line, or the JSON, to act on the result.

| Flag | Effect |
| --- | --- |
| `--audit` | List every observed agent session in `AGENTS`, not only live problem rows. |
| `--json` | Print the report as one `rimz.doctor.v1` JSON document. |
| `--output <PATH>` | Write the report to `PATH` atomically instead of stdout. Human output is written without color. |
| `--clear` | Before reporting, dismiss this workspace's recorded diagnostics, last incident, message failures, and multiplexer log records up to now. |

The human report opens with the RimZ version, OS user, and binary path, then prints these sections in order. A section marked conditional appears only when it has something to show.

| Section | Reports |
| --- | --- |
| `WORKSPACE` | Workspace id, project root and root class, worktree root and branch, session name, socket-path headroom. |
| `MULTIPLEXER` | Backend and version against the floor, binary, server log scan and its scope, sockets, room ownership, session health, Zellij presence and plugins, ttyd. |
| `TERMINAL` | Color depth. |
| `MACHINE CONFIG` | Parse and validation errors in the machine config files and `~/.agents` fragments, with paths and fixes. |
| `SANDBOX` | Isolation mode, bubblewrap path and version, mount probe result. |
| `HOOKS` | Agents reporting to RimZ, a row with the fix for each agent whose hooks need a command, agents not found on the machine. |
| `ACCOUNTS` | Conditional. Each named account's home and status, marking this room's account. |
| `AGENT PLUGINS` | Conditional. Each plugin manifest, its validation result, and its probes. |
| `LOOP TASKS` | Configured loop tasks with target, trigger, and root. |
| `REMOTE CONTROL` | Remote-control hosts and their readiness, or `off`. |
| `STORAGE` | RimZ's disk use by area. |
| `PROTOCOLS` | Conditional on a resolved workspace. Event and sidebar protocol versions, and build drift between writers. |
| `TRUST` | Conditional on a resolved workspace. `trusted`, `stale`, `untrusted`, or `no project config`. |
| `AGENTS` | Conditional on a resolved workspace. Live agent counts and problem rows. |
| `MESSAGES` | Conditional on a resolved workspace. Messages that failed to deliver. |
| `DIAGNOSTICS` | Conditional on a resolved workspace. Incidents grouped from RimZ's diagnostic records. |
| `LAST INCIDENT` | Conditional. How the previous session died, when, the agents lost, `recovered: N of M`, and the forensics path. |

Each row carries a glyph: `✓` healthy, `!` degraded but working, `✗` broken, with the fix beside it. The closing line counts problems and warnings and names their sections, for example `✗ 2 problems in HOOKS, MESSAGES  ·  ! 1 warning in DIAGNOSTICS`, or prints `✓ everything checked is healthy`. Only incidents still in the `investigate` state count toward it; `contained`, `recovered`, and `expected` incidents are summarized as context.

`--clear` stamps the workspace, so records written before that moment stay out of this and every later report. The diagnostic files, server logs, and event log stay on disk. It fails when the current directory does not resolve to a workspace.

How to read each section and fix what it flags is the [troubleshooting guide](../../guide/troubleshooting.md#start-with-rimz-doctor). Static adapter coverage is a separate command, [`rimz coverage`](./maintenance.md#adapter-coverage).

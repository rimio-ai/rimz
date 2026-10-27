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
| Open a directory's room and attach; first run asks setup questions | `rimz start [PATH]` |
| Enter a room by name; no first-run questions | `rimz attach [SESSION]` |
| Full-screen picker: live rooms, their agents, and who needs you | `rimz sessions` |
| List rooms: running, or stopped within 24h | `rimz list`, `rimz ls` |
| Report the machine and write default config | `rimz setup` |
| Diagnose backend, hooks, trust, and room health | `rimz doctor` |

Command, flag, and live session-name completion is set up in the [shell completion guide](../../guide/setup.md#shell-completion).

## Start the room

```sh
rimz [--attach | --no-attach | --print] [--no-resume] [--refresh-ms <MS>]
rimz start [PATH] [launch options] [--account <KIND@NAME>]...
rimz attach [SESSION] [launch options]
```

Bare `rimz` is `rimz start .`. `rimz start [PATH]` resolves the room for `PATH` (default `.`), creates its session if none is running, launches the sidebar, and attaches. A room written by an older RimZ is torn down and replaced by a fresh room; its history is not carried over. Replacement happens only when no running room holds the workspace; otherwise start or `rimz attach` without a session argument refuses with the held-room error below, leaving the room unchanged. For other rooms, a running session is reattached as it is, on the backend it runs under, except that a `rimzd` tab you closed is reopened from its layout. A room is held while a sidebar pane or a live supervisor lives: a second birth on the same workspace exits nonzero, regardless of backend or session name. The error names the recorded session as already held by another running room, with `rimz attach` and `rimz reset --no-start` as fixes, and for a leftover or renamed session, closing it from its multiplexer (or renaming it back); the running room and its workspace record are unchanged. Once neither a sidebar pane nor a supervisor lives, the room is unheld. Attaching to that room relaunches its sidebar only if no other room holds the workspace; otherwise the attach exits nonzero without relaunching it. An attach during a sidebar restart can be refused until the sidebar returns. [`rimz reset`](./maintenance.md#reset-a-wedged-room) is the command that rebuilds a room.

| Launch option | Effect |
| --- | --- |
| `--attach` | Enter the room after opening it, where RimZ would otherwise only print the attach command. |
| `--no-attach`, `--print` | Open the room, then print the attach command instead of entering it. |
| `--no-resume` | Bring a reborn room up empty, without recovering its prior agents. See [Resume on rebirth](#resume-on-rebirth). |
| `--refresh-ms <MS>` | Sidebar render cadence in milliseconds for sidebars this launch creates, clamped to 16 through 1000. The persistent setting is `refresh_ms` under [`[theme.display]`](../../guide/theme.md#display). |
| `--account <KIND@NAME>` | `start` only, repeatable. Launch that provider's agents under a named account. See [Accounts](#accounts). |

`--attach`, `--no-attach`, and `--print` are mutually exclusive.

### Attach or print

Without an explicit attach flag, RimZ attaches only when both stdin and stdout are terminals and you are not already inside the selected backend. Outside that backend, a missing terminal makes it print the attach command on stdout and a notice on stderr: the room is open but not entered; pass `--attach` to enter anyway. Inside the selected backend, `rimz` and `rimz start` report the room situation instead, without opening a room or writing its state. `rimz attach` prints the command and a notice to detach from that backend before retrying. Explicit `--print` or `--no-attach` adds no fallback notice. When RimZ attaches, it exits with the multiplexer client's exit code once you detach. On Zellij before 0.46, a client that exits with a nonzero code within two seconds while the room is still live is attached again, at most twice, each time with a notice on stderr, and the exit code is the last client's ([why](../../guide/troubleshooting.md#zellij-refuses-a-client-right-after-it-starts)).

| Situation | `rimz` and `rimz start` | `rimz attach` |
| --- | --- | --- |
| Terminal, outside the backend | Attach | Attach |
| Outside the backend, stdin or stdout is not a terminal | Print the attach command and explain the fallback on stderr | Print the attach command and explain the fallback on stderr |
| Inside a session of the selected backend | Report the room situation on stderr and exit 0 | Print the attach command and explain the fallback on stderr |
| `--attach` | Attach | Attach |
| `--no-attach` or `--print` | Print the attach command | Print the attach command |

Already in this directory's own room:

```console
$ rimz
You're already in this directory's room (`rimz-f89e`, zellij).
```

This directory's room is running elsewhere:

```console
$ rimz
This directory's room `rimz-f89e` is running in another zellij session.
Detach from this session, then run `rimz` here to enter it.
```

This directory's room is not running:

```console
$ rimz
This directory's room isn't running, and zellij can't start one inside this session.
Detach and run `rimz` here, or run it from another terminal.
```

For tmux, the same messages name `tmux` instead of `zellij`.

Inside a session, `rimz start --account` fails instead, because accounts apply only when a room is born.

### Attach by session name

`rimz attach` with no `SESSION` opens the current directory's room, creating it if needed. Unlike `start`, it skips the first-run, hook, and trust prompts below, takes no `--account`, and does not start the browser daemon. `rimz attach <SESSION>` takes an exact session name from [`rimz list`](#list-rooms). When RimZ has a workspace record for that session, it restores the room's sidebar and reconciles parked recovery state before attaching, creating the session if it is stopped. An attach to a running room asks nothing and leaves recovery candidates parked. A live session RimZ has no record for is attached, or printed, as a plain multiplexer session. With neither a record nor a live session, RimZ refuses instead of creating an empty session, and suggests at most one nearby recorded name:

```console
$ rimz --mux zellij attach agnts-72ff
error: no room or zellij session named `agnts-72ff`
  did you mean `agents-72ff`?   all rooms: rimz list --all
```

Without a nearby name, the second line is `  all rooms: rimz list --all`.

`rimz start` goes through the same health check on the way in, so either command is the way back into a room whose sidebar was closed or whose session came back wrong after a reboot.

### Prompts at room birth

A `rimz start` that creates the session, run from a terminal, can ask up to five things before it attaches. A start or attach that reattaches a running session asks none of them.

| Prompt | When | Default |
| --- | --- | --- |
| First-run questions: truecolor, Nerd Font icons, pet, hands-off automation (`[y/n/choose]` over auto-continue, Codex auto-redeem when `codex` is installed, and idle compaction) | This start wrote the machine config for the first time | The current settings; idle compaction defaults to `100k`, the other automation rows off ([setup](../../guide/setup.md)) |
| Install or refresh hooks for the listed agents | A detected agent has no RimZ hooks, or a stale RimZ-owned integration | Enter installs all; `n` or end of input installs none |
| Trust this project's config | The project ships `.rimz/config.toml` with an ungranted executable surface, and you have not declined this version | No |
| Trust this folder for the listed agents | Detected Codex, Claude, or Qoder has no folder-trust decision under the room's account; shows the proposed file changes first. A decline is remembered per workspace and agent kind; `rimz trust grant` clears it. When no listed kind can be written (Claude in `$HOME`, say), it prints each fix, asks nothing, and remembers those kinds as declined | No |
| Recover prior agents | The room is reborn with agents from its previous session | Yes |

A start without a terminal on stdin, including a remote reconnect, asks nothing: at birth it prints notices for missing hooks and undecided agent folder trust, leaves trust unchanged, and recovers prior agents. It does not remember a decline. Every start also prints the fix for saved-untrusted Codex folders, without offering a grant or remembering a decline for them. A running room leaves recovery candidates parked. The commands behind these prompts are in [agent hooks](./hooks-trust.md#agent-hooks) and [project trust](./hooks-trust.md#project-trust); the trust model is in the [security guide](../../guide/security.md).

### Resume on rebirth

A room is reborn when `start` finds its record but no running session, typically after a reboot or a multiplexer crash. RimZ first prints why the previous session ended, then offers to recover the agents that were running:

```console
$ rimz
rimz: this room's previous session ended with agents still running (2026-07-02 17:37)
Recover 2 agents (claude, codex)? [Y/n]
```

After a reboot the first line reads `rimz: machine rebooted since this room was last open (...)`. Rebirth recovery reports on stderr as `rimz: resumed 2 agents: ...`, and any left behind as `rimz: not resumed: <label> (<reason>)`. When some agents cannot be resumed, RimZ lists each with its reason and asks a second question, whether to drop them, which defaults to no: kept agents are offered again at the next rebirth. `--no-resume` skips recovery and brings the room up with no agents, and so does answering `n` to the first question. Typed at a terminal, both are one-way: RimZ records the agents it did not recover as ended, so a later start without the flag has nothing left to seed from. Their transcripts and history stay on disk.

A running room never asks a recovery question, including web and remote entries, and opens no recovery tabs on entry. When agents remain parked it prints one stderr notice naming their count: `rimz agents resume <lane>` brings a lane back, and an attended `rimz start --no-resume` ends the parked candidates. The latter prints an ended count instead. With recovery on, even an unattended entry retires an old parked team session whose seat a live replacement already fills. An unattended rebirth resumes what it can and keeps the rest parked for an explicit resume or a later rebirth. The `[resume]` settings are in the [configuration guide](../../guide/configuration.md#resume).

### Accounts

`--account <KIND@NAME>` (for example `--account claude@work`) pins that provider's launch account from accounts declared with [`rimz accounts`](./accounts.md); `default` pins the provider's own home. Pins persist across starts. Without a pin, each launch follows the trusted project's `[accounts]`, then the machine's `[accounts.use]`, then `default`, live. Without `--account`, start writes no pin. On re-entry from outside the room, the flag can pin an inheriting provider or repeat its existing pin, but not replace a different pin. `rimz accounts use` pins future launches inside a running room; `--reset` unpins, without changing running agents' accounts. Start warns about configured account problems and opens the room; the warning stays in the sidebar and `rimz doctor` until the next start or attach judges it again. A launch using the broken selection refuses, never switching to `default`. An explicit `--account` request still refuses if invalid, conflicting, or unable to launch. These are the problems and fixes:

| Case | Fix it names |
| --- | --- |
| `--account` differs from an existing pin for that provider | `rimz accounts use <KIND> <NAME>` inside the room |
| Any `--account`, when you run `start` from inside the room's own multiplexer session | `rimz accounts use <KIND> <NAME>` |
| A provider inherits and the project sets `[accounts]` but is untrusted or its trust is stale | Review with `rimz trust`, then `rimz trust grant`. Pinned providers ignore project selections. |
| The machine `config.toml` is unreadable, for a provider inheriting or pinned to a named account | Correct the file; `rimz config get` shows the strict error. A provider pinned to `default` needs neither project nor machine configuration. |
| A deciding `[accounts.use]` entry names an undeclared account | `rimz accounts add <kind> <name>`, or clear it with `rimz accounts use --global <kind> default` |
| A deciding `[accounts.use]` entry names an account for a provider without named accounts | `rimz accounts use --global <kind> default` |
| The account is not declared | `rimz accounts add <kind> <name>` |
| The account's home is missing or lacks RimZ hooks | `rimz accounts add <kind> <name>` |
| The account's hooks are installed but the agent has not trusted them | The agent-specific step the error prints |

The workflow is in the [accounts guide](../../guide/accounts.md).

### Browser access at start

When `[web] enabled = true`, `rimz start` also starts the writable browser daemon. This is best-effort: a missing ttyd or an occupied `[web] port` prints a warning (`rimz: browser daemon was not started: ...`) and the room opens anyway. `rimz attach` does not start the daemon. [`rimz web`](./web.md) inspects and controls browser access.

## Pick a session

```sh
rimz sessions
```

`rimz sessions` opens a full-screen manager of every live RimZ room on the machine. Each room is a two-line card: the repository name and path, then live agent counts by kind (`claude ×2`), a `●` count of agents that need attention, and the session, token, and spend totals of the sidebar's headline window ([`spend_window`](../../guide/configuration.md#sidebar-rendering)). Rooms with a prompt in the last 24 hours come first, newest prompt first; the rest follow by the time RimZ last wrote the workspace record, then by repository name and project path. A narrow terminal drops the card's right-hand metrics rather than wrapping them: the token total goes first, then the session count. The dollar figure always stays. Detaching from a room you entered returns you to the manager.

| Key (room list, filter closed) | Action |
| --- | --- |
| `↑` `↓`, `k` `j`, scroll wheel | Move the selection |
| `Enter`, or click the selected card | Attach to the selected room |
| `n` | Open the new-session selector |
| `/` | Open the filter on repository name and path |
| `Esc` | Quit |
| `Ctrl-C` | Quit |

| Key (filter open) | Action |
| --- | --- |
| Any printable key, including `j`, `k`, `n`, and `/` | Add to the filter |
| `Backspace` | Delete the last filter character |
| `↑` `↓`, scroll wheel | Move the selection |
| `Enter`, or click the selected card | Attach to the selected room |
| `Esc` | Close the filter, keeping its text applied and visible |
| `Ctrl-C` | Quit |

The filter line stays visible. Its cursor appears only while the filter is open; reopen it with `/` to edit or backspace away the text.

The new-session selector lists dormant known workspaces, then the current directory, then its non-hidden subdirectories, starting at `$HOME`. Typing filters the list.

| Key (new session) | Action |
| --- | --- |
| `↑` `↓`, scroll wheel | Move the selection |
| `→`, `Tab` | Descend into the selected directory |
| `←`, or `Backspace` with an empty filter | Go up one directory |
| `Enter` | Create the room for the selected path and attach |
| `Esc` | Return to the room list |

An unreadable directory shows an empty directory list with a notice. `rimz sessions` refuses to run inside Zellij or tmux and points at `rimz attach`. Without a terminal on stdin and stdout, it prints the live sessions in its error message, ends with `for a list, run: rimz list`, and exits 1.

The browser's room picker is the same manager ([web guide](../../guide/web.md)).

## List rooms

```sh
rimz list [-a | --all] [--json]
```

`rimz list` (alias `rimz ls`) joins RimZ's workspace records with the live Zellij and tmux sessions, matched by session name. By default it shows running rooms and rooms active in the last 24 hours; `--all` adds dormant ones. Running rooms sort first, then by most recent activity. It reads records only and changes no room.

| Column | Content |
| --- | --- |
| `ROOM` | Multiplexer session name, the argument to `rimz attach`. The current room has `(here)` after its name. |
| `PROJECT` | Project root, abbreviated with `~` under your home directory. |
| `MUX` | `zellij`, `tmux`, or `-` when no session is live. |
| `LAST ACTIVE` | Relative age of the last activity, such as `5h ago`. A stopped room whose last session died shows the death instead, such as `crashed · 16 agents · 2h ago` or `rebooted · 1 agent · 2h ago`. |

With no rows, human output leaves stdout empty and prints a hint on stderr. If the default view hides dormant rooms, the hint gives their count and names `rimz list --all`. With no known rooms, it says to run `rimz` in a project. Both exit 0. An empty JSON view prints `[]` on stdout and nothing on stderr.

`--json` prints an array with one object per row:

| Field | Type |
| --- | --- |
| `workspace_id` | string |
| `project_root` | string |
| `session_name` | string |
| `running_on` | `"zellij"`, `"tmux"`, or `null` |
| `last_activity` | RFC 3339 timestamp or `null` |
| `last_death` | death summary with a UTC `YYYY-MM-DD HH:MM` time, such as `crashed · 16 agents · 2026-07-02 17:37`, or `null` |
| `current` | boolean: whether this is the calling pane's verified room |
| `last_death_detail` | object with `cause` (`"crash"` or `"reboot"`), `at` (RFC 3339 timestamp), and `lost_agents` (array of objects with `kind`, `agent_id`, and optional `name`), or `null` |

## Set up the machine

```sh
rimz setup [--config-only]
```

`rimz setup` prints a report (multiplexer and version, project path and its kind, project trust, config path and whether it exists, and hooks grouped into installed, not installed, and not on PATH) and then acts according to how it was run. Paths under your home use `~`. Installed hooks with a newer version available carry `(upgrade available)` after the agent name. Empty hook groups are omitted; agents found without hook-install support are omitted too. Agents with blocked hook installation stay under `not installed`, but are excluded from the `rimz hooks install` hint.

| Run | What it changes |
| --- | --- |
| From a terminal | If config exists, asks `Keep your current config?` (default yes): yes merges it against the current templates, no overwrites it with fresh templates. Then repairs `~/.rimz` fragments, writes a missing `remote.toml` template, offers one hook install or refresh for detected agents, and asks the truecolor, Nerd Font, pet, and hands-off automation questions; the automation question lists one row per behavior and takes `y`, `n`, or `choose`. |
| `--config-only` | Merges config against the templates, repairs `~/.rimz` fragments, and writes missing files, including the remote template and built-in consensus copy. Installs no hooks, grants no trust, and changes no appearance or automation setting. `--yes` remains an accepted hidden alias. |
| Without a terminal and without `--config-only` | Nothing. Prints the report and `Setup changed nothing: there is no terminal to ask from.` Then names `rimz setup --config-only` for config and, only when agents lack hooks, `rimz hooks install` with their count. |

A merge names every file it wrote, merged, or left untouched. An unparseable file is left as it is, and interactive setup stops at that point until you fix it. What a merge keeps and removes is defined in the [configuration guide](../../guide/configuration.md#generate-and-refresh-the-files). The hook summary points at `rimz hooks install --dry-run` for the exact diffs. [`rimz config init --force`](./config.md#write-the-templates) is the clean reset. The walkthrough is the [setup guide](../../guide/setup.md).

## Diagnose with doctor

```sh
rimz doctor [--audit] [--json] [--no-log-text] [--output <PATH>] [--clear]
```

`rimz doctor` reports the machine, the backend, and the room for the current directory in one pass. It does not reconfigure the room, and it exits 0 whatever it finds; read the closing fix block, or the JSON, to act on the result. It briefly starts configured language servers that are not already running to check startup, then terminates their process groups. Without `--clear` or `--output`, RimZ writes no state, including with `--audit` or `--json`; a configured server may write its own files. `--clear` writes a history watermark; `--output` writes the named report file.

| Flag | Effect |
| --- | --- |
| `--audit` | Widen every section to all its rows: observed agent sessions, loop tasks, language servers, and retained message failures. Also measure disk use for `STORAGE` and `TEMP UNITS`, walking every file under the RimZ home. For a bug report: `rimz doctor --audit --no-log-text --output doctor.txt`. The audit can take minutes; an agent runs it under [`rimz wait --run`](./wait.md). |
| `--json` | Print the report as one `rimz.doctor.v1` JSON document, including top-level `audit: false` or `true`. |
| `--no-log-text` | Omit multiplexer log record text from summaries, samples, and excerpts in either output format; retain severity, timestamps, counts, Zellij module targets, and RimZ-authored diagnoses. |
| `--output <PATH>` | Write the report to `PATH` atomically instead of stdout. Human output is written without color. |
| `--clear` | Before reporting, dismiss this workspace's recorded diagnostics, last incident, message failures, and multiplexer log records up to now. |

When stderr is a terminal, a delayed stderr progress line names the oldest check still running and counts the others, including with `--json`, `--output`, or piped stdout. It clears before the report prints. The usual `RIMZ_NO_PROGRESS` and agent-environment gates also apply.

The room section also lists each configuration setting skipped at its latest start or attach, with the recorded warning and fix. In JSON the room object carries `skipped: [{"setting": "...", "message": "..."}]`, or an empty array. These recorded warnings remain until the next room entry judges the configuration again.

When `agents.shell` is a bare name, an informational row after the skipped settings shows `"bash" → /usr/bin/bash` for the room's saved lookup. Without a matching saved lookup, it shows doctor's own PATH hit, or `not on PATH`, marked `not saved by the room; each launch looks it up`. When the saved path is no longer a file, it shows the same lookup marked `saved path <path> is no longer a file; each launch looks it up`, and the gone path never appears as the arrow target. Other distinct files on doctor's PATH follow with a `rimz config set agents.shell <path>` pin command; copies of the same file collapse. This row adds no warning or problem to the tally. JSON adds `mux.ready.room.agent_shell` with `name`, `path` (or `null`), `saved`, and `also_on_path`; the field and row are omitted for an unset or absolute value. A gone saved path reports `saved: false` and doctor's own lookup in `path`, without an additional field.

Plain `--json` carries multiplexer log text verbatim, up to 8 KiB per log record; add `--no-log-text` to omit it without changing other report sections.

The human report opens with the RimZ version, OS user, and binary path, then prints these sections in order. A section marked conditional appears only when it has something to show.

| Section | Reports |
| --- | --- |
| `WORKSPACE` | Workspace id, project root and root class, worktree root and branch, session name, socket-path headroom. |
| `MULTIPLEXER` | Backend and version against the floor, binary, server log scan and its scope, sockets, room ownership, skipped settings, bare-name agent shell lookup, session health, daemon tab shape, Zellij presence and plugins, ttyd. |
| `TERMINAL` | Color depth. |
| `HOME` | The RimZ home and whether `RIMZ_HOME` set it, the definition root `RIMZ_AGENTS_HOME` points at when it is set, and one warning naming legacy XDG roots still on disk but not read. The closing block carries the manual [move steps](../../guide/configuration.md#moving-from-the-xdg-roots). |
| `MACHINE CONFIG` | Parse and validation errors in the machine config files and `~/.rimz` fragments, with paths and fixes. |
| `SANDBOX` | Isolation mode, bubblewrap path and version, mount probe result. |
| `HIBERNATE` | Whether agent hibernation is enabled and active, or why it is skipped; systemd executable and user manager, delegated memory controller, visible swap, and ancestor swap limits, with a fix per failure. Capability failures are warnings; invalid durations are errors. Zero free swap warns without disabling hibernation. See [configuration](../../guide/configuration.md#agent-hibernation). |
| `HOOKS` | Agents reporting to RimZ, status rows for hooks needing attention, and agents not found on the machine. An uncounted prompt-cache fallback line names installed kinds whose static settings and defaults use the fallback for any sign-in mode, including unknown, with a per-kind configuration hint; it is omitted when none do. Hook fixes appear in the closing block. JSON always carries `prompt_cache_fallback: {"ttl": "1h", "kinds": ["amp", "pi"]}` (configured duration or `"off"`, sorted kinds, possibly empty) under the unchanged `rimz.doctor.v1` schema. Live cache writes may override this static summary. |
| `FOLDER TRUST` | Conditional on a resolved workspace. Detected agents' folder-trust decisions under this room's accounts (outside a room: trusted project, then machine, then provider default, as a new room there would). The decided summary names `kind@login`; undecided or restricted rows warn, with fixes in the closing block. JSON: `folder_trust.ready.rows`. See [agent folder trust](./hooks-trust.md#agent-folder-trust) for the states. |
| `EMPTY-DIR` | Warns for each declared account whose configured `empty-dir` profiles fall back to the parent's directory, naming the profiles, account, and non-grantable reason when applicable. Also warns about stray files in `~/.rimz/empty`, even without a workspace or configured profile. Fixes appear in the closing block. Omitted when there is nothing to warn about. JSON: `empty_dir.rows` (folder-trust rows plus `profiles`), `empty_dir.check` (`ready: null` when clean, or `unavailable.error`), and optional `profiles_error` when project profiles could not be loaded for configured machine profiles. See [empty-directory folder trust](./hooks-trust.md#empty-directory-folder-trust). |
| `ACCOUNTS` | Conditional. Each default or named account's home, launch problems, cache-only usage standing, and default layers: `this room`, `this project`, `new rooms` ([`rimz accounts list`](./accounts.md#list) explains each layer). Standing is `ready`, `spent, resets in <countdown>` as uncounted context, or `out of credits, no reset` as a warning. Every cached reading includes its age. Limit-paused or failed agents on a spent or exhausted account are named here rather than counted again in `AGENTS`. Launch problems outrank standing, with their specific repair in the closing block. JSON rows include `room`, `machine_default`, and nullable `usage` with `standing` (`ready`, `spent`, `exhausted`), `resets_at`, `read_at`, and `agents`. No provider refresh is performed. |
| `AGENT PLUGINS` | Conditional. Each plugin manifest, its validation result, and its probes. |
| `LOOP TASKS` | Task count and how many need attention, then invalid or unscheduled tasks and tasks holding worktrees, followed by loop timer state. A holding row says `holds worktree NAME` even without `--audit`; it is information, not a warning. JSON task rows always include `holds_worktrees`, an array of held RimZ-owned worktree paths (`[]` when none). `--audit` lists all tasks with target (`check` for command-only tasks), trigger, root, and room state. Warns when a live clock task has neither an open room nor an active timer. |
| `LSP` | Running, starting, indexing, and crashed machine-wide shared servers, ordered by running then alarms then the rest, with checkout and server breaking ties, plus recorded crash exit/stderr. Other rows collapse into `N more dormant or not started`, with `rimz doctor --audit` as the hint; `--audit` shows them all. Current-checkout startup checks and the last unrecovered refusal or queue timeout follow, naming the event and its age. Memory-pressure and recycled dormancy are neutral, not warnings. |
| `REMOTE CONTROL` | Remote-control hosts and their readiness, or `off`. A host skipped at room birth reads `off for this run`, with its blocker and fixes underneath. |
| `STORAGE` | A hint to use `--audit` for RimZ's disk use by area. Default runs do not walk file trees; JSON omits `disk_usage`. With `--audit`, shows the total and an area/size/path table, and JSON includes `disk_usage`. |
| `TEMP UNITS` | Machine-wide free space and configured floor, reclaimable unit count, count past the grace and the earliest time a unit still inside the grace becomes reclaimable (omitted when every reclaimable unit is already past the grace and waits for the next sweep), and in-use unit count. `--audit` adds byte totals for reclaimable and in-use units. With `gc.auto = false`, an uncounted `eviction` row names the pressure-eviction pause, even above the floor. Below the floor, warns when reclaimable units exist: normally `rimz gc --tmp-floor`; while paused, `rimz config set gc.auto true` resumes pressure eviction, and `rimz gc --all` is offered only when units are past the grace, with their count. With none reclaimable, reports a problem and asks you to stop or restart the agents holding the largest units (`rimz agents show @handle` prints the path). While paused, those stopped agents' units remain until eviction resumes, or a hand `rimz gc --all` removes them once past the grace. JSON: `temp_units.ready` contains filesystem byte figures, unit counts, `eviction_paused` (always present, true exactly when `gc.auto = false`), and nullable `earliest_reclaim` (null when no reclaimable unit is still inside the grace); `reclaimable_bytes` and `in_use_bytes` are present only with `--audit`. An unavailable inventory is `temp_units.unavailable.error`. Collection is read-only, with no cache writes. |
| `PROTOCOLS` | Conditional on a resolved workspace. Event and sidebar protocol versions, and build drift between writers. |
| `TRUST` | Conditional on a resolved workspace. `trusted`, `stale`, `untrusted`, or `no project config`. |
| `AGENTS` | Conditional on a resolved workspace. Live counts and failed or paused rows using card status, with handle, account, branch, reason, and age. Rows attributed to a spent or exhausted account are uncounted context. `--audit` includes all observed sessions. JSON rows add `handle`, `account`, nullable `turn_error`, and `attributed_to_account`. |
| `MESSAGES` | Conditional on a resolved workspace. Open-message counts and stuck deliveries needing attention. Recent terminal failures are one uncounted context line grouped by status, with `rimz message list --all --system` as the hint. `--audit` adds the retained failure table. JSON `failure_counts` counts failures before the ten-row table limit. |
| `DIAGNOSTICS` | Conditional on a resolved workspace. Incidents grouped from RimZ's diagnostic records. `--audit` includes every settled incident with its time and summary as uncounted context; plain mode summarizes settled kinds. |
| `LAST INCIDENT` | Conditional. One context line naming how the previous session died and when. Lost-agent names, recovered count, and forensics path remain in JSON. |

Rows have two tiers: `!` warnings and `✗` problems count as findings needing attention; `▸` context and `·` neutral background facts do not count. `✓` marks a healthy check. The closing block counts findings and names their sections, for example `✗ 2 problems in HOOKS, MESSAGES  ·  ! 1 warning in DIAGNOSTICS`, then lists problems followed by warnings. Findings in the same section with the same fix share one entry naming their count and subjects, with one `fix:` line; the headline still counts each finding. Entries keep the order of their first finding within each severity. With no counted findings, it prints only `✓ everything checked is healthy`. `--audit` widens rows, never the headline or fixes: Waiting agents are routed by the sidebar, and ended sessions are contextual history. Only diagnostic incidents still in the `investigate` state count toward it; `contained`, `recovered`, and `expected` incidents are summarized as context. Hook drain lag counts as one warning per episode until 15 quiet minutes pass; its row shows the worst sampled lag and peak sampled pending bytes.

The LSP startup checks use the current checkout's effective configuration and root markers. Running servers are not started again; untrusted project servers are not executed. Checks run concurrently, waiting up to ten seconds for startup. Failures, invalid configurations, and untrusted servers each count as a problem; a timeout is a warning. Fixes appear in the closing block. An unrecovered refusal or queue timeout warns with readable memory figures and holders or its reason; for the same checkout and server, it disappears when a check finds the server `running`, or a running registry entry has a later ready timestamp (falling back to its start timestamp when readiness was not recorded). A `started` check also clears a refusal that names a reason, but never one with memory figures: the check starts the server without memory admission. A later dormant entry does not clear it. JSON adds `lsp.ready.checks`, sorted by server name, with outcomes `running`, `started`, `failed`, `timed_out`, `invalid`, or `untrusted`. No resolved workspace means an empty check array. When the checkout's LSP configuration cannot be loaded, the machine-wide table still renders, `lsp.ready.checks_error` carries the error, and doctor counts it as one problem.

`--clear` stamps the workspace, so records written before that moment stay out of this and every later report. The diagnostic files, server logs, and event log stay on disk. It fails when the current directory does not resolve to a workspace.

How to read each section and fix what it flags is the [troubleshooting guide](../../guide/troubleshooting.md#start-with-rimz-doctor). Static adapter coverage is a separate command, [`rimz coverage`](./maintenance.md#check-adapter-coverage).

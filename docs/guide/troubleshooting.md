# Troubleshooting

Diagnosing a room by hand means `ps`, `tmux ls`, a provider log, and a guess about which of them is lying. RimZ keeps the room's truth in one durable store, and `rimz doctor` reads it along with the machine around it in a single pass, printing the fix beside most of what it finds. Run it first. The rest of this page is the symptom catalogue: what went wrong, the command that fixes it, and the guide that owns the full story.

## Start with `rimz doctor`

`rimz doctor` probes the machine, the room, and RimZ's own state, then prints a verdict per row: the multiplexer and whether its version clears the floor, per-machine config parsing, sandbox support, per-agent hook status, provider accounts, scheduled loop tasks, project trust, terminal color depth, workspace and store health, the live agents, messages that failed to land, and recent incidents. Nothing in the room starts, stops, or moves, so run it as often as you like.

```sh
rimz doctor
```

The report reads top to bottom, machine setup first and live room state last. This capture is shortened, with `…` where lines were dropped:

```
RimZ doctor
  version: 0.4.3+g85a16b08cf04
  user:    marvin (uid 1000)
  …

WORKSPACE
  id:              ws_f89e49906df0621ad2765112
  project root:    /home/marvin/workspace/project-rimz/rimz
  session:         rimz-rimz-f89e49
  …

MULTIPLEXER
  backend:        tmux
  version:        tmux 3.7c
  tmux floor:     ✓ OK (>= 3.5.0 required)
  session health: ✓ ok
  presence:       ✓ event mode (poked 94s ago)
  ttyd web:       ✓ ttyd version 1.7.7 (/usr/bin/ttyd)

TERMINAL
  depth:   ✓ truecolor (mode truecolor)
  …

HOME
  home:         ~/.rimz (RIMZ_HOME)
  legacy roots: ! ~/.config/rimz, ~/.local/state/rimz, ~/.local/share/rimz (not read)
      fix: RimZ now keeps everything under /home/marvin/.rimz and no longer reads …

MACHINE CONFIG
  config files: ✓ all present files parse

SANDBOX
  mode:    sandbox
  bwrap:   /usr/bin/bwrap
  …
  probe:   ✓ passed

HOOKS
  13 reporting to RimZ: amp, antigravity, claude, codex, copilot, cursor, droid, grok, kimi, kiro, opencode, pi, qwen

…

PROTOCOLS
  event:   rimz.event.v2
  sidebar: rimz.plugin.v5
    ! mixed rimz builds writing this workspace: 2 distinct builds; run `rimz reload` to converge
      …

TRUST
  trust: ✓ trusted (granted 2026-09-16T05:39:29.842070054Z)

AGENTS
  10 live: 2 running, 1 waiting, 7 success

MESSAGES
  1 open: 1 queued — `rimz message list`

DIAGNOSTICS
  12 recent incidents (/home/marvin/.rimz/ws/rimz-f89e/diag.log.jsonl)
   KIND                               SEEN  SUMMARY
!  mixed_build_writers             22m ago  prior frame from build 371a4cbf5aec; this producer is bf47174d6f7c · old build bf47174d6f7c
    …
    ▸ 1 handled and closed themselves: pane_carry

! 13 warnings in HOME, PROTOCOLS, DIAGNOSTICS
  ✗ marks something broken, with the command that fixes it beside it; ! marks something degraded that still works
```

Most symptoms below show up as a named doctor row with the fix already attached, so the report doubles as the fix list.

### Reading the report

Each row carries a glyph that sets its tone:

- `✓` clear, `▸` informational, `·` a neutral background fact.
- `!` degraded: working, but worth a look.
- `✗` broken. In a table the command to run is in the `FIX` column; elsewhere it sits on the line beneath the row.

The last line counts the findings and names the sections holding them, or reads `✓ everything checked is healthy` alone. Jump to the sections it names and clear the `✗` rows first.

The tail sections report live state rather than machine setup: `AGENTS` counts the agents doctor observed, `MESSAGES` lists sends that failed to land, and `DIAGNOSTICS` lists incidents RimZ recorded about itself. An incident takes a row when RimZ has not yet explained it, and contributes its warning or problem to the tally; incidents it matched to an ordinary cause, a client leaving the session or a pane's terminal closing, are counted and named on one closing line instead. A busy room produces far more of the second kind than the first. Treat both as context for a bug report rather than a to-do list, and use `rimz doctor --json` when you want them structured.

The `MULTIPLEXER` section's `log` row names the server log, how far back the scan reached, and who writes into it. The Zellij log covers every Zellij session your user owns, so an issue it reports can come from a session that is not this room; the tmux log is scoped to RimZ's own server. Check that scope before attributing an issue to the room.

## A room or agent won't start

### Zellij or tmux is missing or too old

RimZ needs Zellij 0.44.2 or newer, or tmux 3.5 or newer ([install one](./installation.md#install-zellij-or-tmux)); pixel pets add tmux 3.6. The `MULTIPLEXER` section reports the backend RimZ resolved and its version against the floor, as `tmux floor: ✓ OK (>= 3.5.0 required)` or `TOO OLD`. Install or upgrade the multiplexer when the row flags it.

`rimz start` refuses to open a room on a Zellij below the floor and names the upgrade. On tmux it opens the room anyway, so the doctor row is your only warning there. When both backends are installed and RimZ resolved the one you did not want, pick explicitly with `--zellij`, `--tmux`, or `--mux <name>`.

### RimZ reports an existing room instead of opening one

A RimZ room is its own Zellij or tmux session, so it cannot nest inside a session of that same backend you are already attached to. Run bare `rimz` (or `rimz start`) from inside one and it names the room rather than nesting:

```
You're already inside a zellij session, which can't host a nested room.
This directory's room is `rimz-rimz-f89e49`. Detach to (re)launch it, or run `rimz` from outside the session.
```

Open a fresh terminal window that is not attached to Zellij or tmux, then run `rimz` there.

### `tmux ls` does not list my RimZ room

RimZ's tmux rooms run on a second tmux server, on a socket under the runtime root, which is what keeps the server-wide options RimZ sets out of your own sessions. Your `tmux ls`, `tmux attach`, and `tmux kill-server` therefore never see a room. Address that server with `-S`:

```sh
tmux -S "${XDG_RUNTIME_DIR:-/tmp/rimz-$(id -u)}/rimz/tmux/server" ls
```

`rimz attach` is the shorter way in. Zellij has no such split: a room is an ordinary session in `zellij ls`. [Your tmux rooms run on their own server](./multiplexer.md#your-tmux-rooms-run-on-their-own-server) explains why.

### RimZ says the workspace path does not exist

`rimz start -- /path/to/project` only creates a room for a directory that already exists. Correct a typo or create the directory first, then run the command again. Remote path targets follow the same rule before attach; `rimz remote list` shows a saved alias so you can correct its target.

### The room refuses because of a provider account

`rimz start` checks every selected named account before it builds anything, rather than opening a room whose agents cannot reach their home. Three refusals and their fixes are in [start a room on an account](./accounts.md#start-a-room-on-an-account): an account name that is not declared, a declared account whose home carries no RimZ hooks, and a room still selecting an account you removed.

One more refusal comes from project config rather than from the account itself. `project account selections in .rimz/config.toml are untrusted` means the repository's `[accounts]` table sits behind the trust gate, because choosing which provider account your agents run under is part of the surface you grant. Run `rimz trust status`, read the diff, then `rimz trust grant`.

### Sandbox isolation is unavailable

The `SANDBOX` section reports the configured mode, the bubblewrap path and version, the probe result, and the fix. Sandbox mode needs Linux and the `bubblewrap` package (`bwrap` on `PATH`). A failed probe usually means unprivileged user namespaces are disabled (`kernel.unprivileged_userns_clone` or `user.max_user_namespaces`) or an AppArmor or LSM policy blocks them.

Follow the probe's error for your host, or switch to host isolation, which needs no bubblewrap:

```sh
rimz config set agents.isolation host
```

The switch affects new launches, not existing panes. Profile `skills` lists can stay: host mode ignores them. [Sandbox isolation](./security.md#sandbox-isolation) covers the limits of each mode.

### Zellij is live but not responding

Zellij can still list a room after that room has stopped answering pane queries. Attaching then opens an alternate screen that cannot draw, so RimZ refuses before attach and leaves every pane untouched. `rimz doctor` confirms it with a `live but not responding` session-health verdict. If it does not recover, rebuild the room:

```sh
rimz reset
```

Reset is destructive and asks before replacing the wedged room; [rebuild a wedged room](#rebuild-a-wedged-room-with-rimz-reset) has the exact lifecycle and the non-interactive flags.

### An agent pane says "starting without skill …"

The agent is starting, but RimZ left one skill out of this launch because it could not prepare that skill's user-only copy for the profile's `skills` list in sandbox mode. The warning names the skill, its source path, and whether RimZ could not read it or could not rewrite its metadata. Nothing in the installed skill changed, and every other skill is still available.

Fix the source the warning names: make the file readable, or bring its frontmatter into the YAML shape RimZ's rewriter accepts ([skills](./configuration.md#skills)). The alternative is to add the skill's bare name to that profile's `skills` list, which binds it exactly as installed with no rewriting, and makes it model-callable where its author's restrictions allow.

## An agent isn't reporting

### An agent shows as a plain process row

An agent earns a live card once its reporting hooks are installed. The hooks are a handful of lines RimZ adds to the agent's own config file; through them the agent reports its turns, prompts, and blocking questions to the sidebar. Without them the agent runs fine, but the sidebar can only draw a plain process row. Preview the lines, then install them:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install              # wire every detected agent that has an installer
rimz hooks install claude       # wire one agent by name
```

The install is additive, so your existing hooks stay, and `rimz doctor` reports per-agent hook status afterward. Restart the agent so it picks up the new hooks. To back the change out, `rimz hooks uninstall [AGENT]` removes exactly what RimZ added and restores any statusline it wrapped.

Three agents have a wrinkle beyond the install, and [agent support](../reference/agent-support.md) carries the rest of the per-agent detail. Kiro needs CLI 2.13.0 or later, and `rimz hooks install kiro` names the installed version when it refuses. Grok snapshots its hook registry for a running TUI, so after a reinstall reload the registry through Grok's Hooks UI or start a new session; a session still holding the old snapshot keeps calling the command RimZ replaced. Copilot reads its model and token figures from a local telemetry file, so an OTLP exporter configured in your environment routes them to your own pipeline and leaves that card carrying lifecycle alone; pin `COPILOT_OTEL_EXPORTER_TYPE=file` and an explicit `COPILOT_OTEL_FILE_EXPORTER_PATH` when you want both.

### A card went quiet after a config edit

If a card stops updating after you hand-edited an agent's config, the RimZ-managed hook block was probably disturbed. Run `rimz hooks install --dry-run` to see the current diff, then `rimz hooks install` to restore the block. Some agents gate hooks behind their own trust prompt; when one reports installed-but-untrusted hooks, the `HOOKS` row in `rimz doctor` prints the exact fix.

### Codex sessions drop with a WebSocket reset about hourly

`WebSocket protocol error: Connection reset without closing handshake` can mean Codex's own updater is running a different executable from the managed standalone install. `rimz doctor` reports that state in its `REMOTE CONTROL` section, as `Codex remote-control updater version skew`, and prints the two process paths that disagree.

The updater clears the skew by itself. Its next successful hourly pass restarts the shared app-server, then replaces the updater binary, and that pass disconnects every daemon-backed Codex session one time. You either wait for it or choose the moment: run the managed-binary `codex app-server daemon bootstrap --remote-control` command doctor prints while no valuable Codex turns are running, then resume the sessions it drops. A `codex remote-control stop` and `start` pair restarts only the app-server and clears nothing.

### Zellij pane discovery stopped

On Zellij, RimZ loads a small presence plugin so the sidebar learns which panes exist and where. Its permission grant is seeded for you on the first attach, and revoking it in Zellij's plugin manager stops pane discovery until you restore it. Re-grant the plugin's permissions in Zellij, then run `rimz reload`. The plugin ships no pane content anywhere, and the next room start seeds the grant again either way; [the Zellij presence plugin](./security.md#the-zellij-presence-plugin) has the full model.

Doctor's `presence plugin` row says what each loaded plugin is doing for the sidebar: writing pane topology, superseded by a newer plugin, or loaded and idle. When wakes are failing it names the cause the plugin host itself reported, drawn from the same window as the counts beside it. A stale writer, a second loaded plugin, and an outdated build all point at the same fix:

```sh
rimz reload
```

## The sidebar looks wrong

### The sidebar pane is gone

Closing the sidebar pane costs you nothing durable: every agent keeps working, and a card that needed you is still unread and still ranked when the column returns.

```sh
rimz sidebar repair    # remount a missing, duplicate, wedged, or mis-docked sidebar
```

`rimz reload` will not do it: reload upgrades sidebars that are still alive and never opens a pane. It does say so, naming the room and the verb (``1 room with no sidebar; mount it again with `rimz reload --repair`.``), and `rimz reload --repair` runs both operations in order.

### The "Sidebar degraded" banner

A banner such as `⚠ Sidebar degraded for 8s: snapshot failed` means a render could not read a fresh snapshot, usually because the binary moved on disk or the durable state directory vanished mid-write. Nothing is lost: the durable store stays the source of truth, so fixing the underlying cause (reinstall or repoint the binary, confirm the state directory exists) clears the banner on the next good frame.

Once it recovers, the banner steps down to a dim, dismissable notice (`⚠ last alert 8s ago: …  ·  x dismiss`), so a failure that flickered past is still visible after the fact. Press `x` to clear it; two consecutive failed renders re-arm it, which is why a single bad frame does not.

### Colors, glyphs, or pets don't render

Two upgrades need terminal support beyond color:

```sh
rimz config set theme.style modern        # truecolor plus Nerd Font icons
rimz config set theme.pets.enabled true   # an animated companion on the dashboard
```

`modern` needs a Nerd Font installed in the terminal. Pets render as crisp pixels only in Ghostty, kitty, and a browser tab served by RimZ, and inside tmux that also needs tmux 3.6 or newer with `allow-passthrough on`. In a tmux room every attached client has to qualify, so one plain terminal attaching drops the room to cell art within ten seconds. Zellij rooms are always cell art, whatever the host terminal supports and whatever `rimz doctor`'s `zellij kitty graphics` row says: that row probes the terminal, and no render path consults it.

The full appearance model is [theming](./theme.md), and the per-terminal pet notes are in the [pets guide](./pets.md#crisp-pixels-and-cell-art).

### A pane looks stale after upgrading RimZ

Upgrading the RimZ binary does not upgrade the sidebars already drawing: each one keeps the build it started with, and `rimz doctor`'s `PROTOCOLS` section warns when more than one build is writing the workspace. Reconcile the running sidebars onto the new build:

```sh
rimz reload             # re-exec sidebars in place; every pane stays unchanged
rimz sidebar repair     # repair missing, duplicate, mis-docked, or wedged sidebars
rimz reload --repair    # run those two independent operations in order
rimz doctor --clear     # dismiss diagnostics, incidents, and log records from before the upgrade
```

`rimz reload` is the pane-preserving fix: it re-execs each sidebar in place and leaves every other pane alone, it runs from anywhere, and it leaves stopped sessions stopped. `rimz start` and `rimz attach` print the same warning when a live room still runs a different sidebar build. A sidebar that misses the command's reporting window keeps converging from the durable room record on its own, so run `rimz sidebar repair` only for structural damage; a replacement mounts and proves a current-build heartbeat before the old pane closes.

Once the live writers converge, `rimz doctor --clear` records a workspace watermark, so pre-upgrade diagnostics, the last incident, message failures, and multiplexer log records stop appearing without deleting their evidence files.

## The terminal misbehaves

### Scroll wheel sends arrow keys

tmux disables outer mouse reporting while it repaints an attaching client. Terminals with alternate scroll enabled turn wheel ticks during that window into arrow keys, so scrolling immediately after attach can step back through an agent's prompt history. RimZ saves and disables alternate scroll for the life of every multiplexer attach it launches, then restores the prior setting when the client exits. A bare `tmux attach` bypasses that bracket, so attach through `rimz`.

When an older RimZ build has already left a tab converting wheel ticks to arrow keys, run `reset` once at a shell prompt or open a new tab. RimZ's browser client suppresses the same fallback in xterm.js while no application owns the mouse; reload an older browser tab after upgrading so it picks that up.

### Garbled terminal after a remote link death

Staircased lines, or Enter and Ctrl-C echoing as `^M` and `^C`, mean a dead SSH link left the local terminal in raw mode. RimZ repairs the terminal on the next `rimz remote connect`; on an older build, run `reset` once at a shell prompt.

## Notifications and messages don't arrive

### Notifications don't fire

RimZ raises a desktop notification by writing a terminal notification escape (OSC 777) from the sidebar; your terminal turns it into the OS banner, even over SSH. When no banner appears, check in order:

- **A pane you are already watching never pushes.** This is the most common reason a flip was silent. The same rule batches agents that flip in the same second and holds one agent to a push every five seconds ([what fires before you configure anything](./notifications.md#what-fires-before-you-configure-anything)).
- **Zellij drops the escape.** `desktop = "auto"` therefore skips it in a Zellij room. For OS banners there, wire a `[[notifications.handler]]` command (`notify-send`, `ntfy`, or anything else); the shape is in [push it anywhere with a handler](./notifications.md#push-it-anywhere-with-a-handler).
- **tmux needs `allow-passthrough` on.** RimZ turns it on in its own rooms, which is how the escape reaches your terminal from a window you are not looking at. A personal config that forces it off blocks it.
- **Your terminal and your OS both get a veto.** The terminal has to support notification escapes, and the OS has to allow notifications from that terminal app (on macOS, System Settings, then Notifications).
- **Only the trigger statuses raise a banner.** `notifications.triggers` defaults to `["waiting", "failed"]`. Add `"success"` if you expect completion pings.

A missed notification loses nothing: the sidebar is the source of truth, so the card stays unread and ranked until you visit it, and an agent that keeps waiting earns a fresh nudge every minute.

### A message never lands

Start with the message itself rather than the agent:

```sh
rimz message show <MESSAGE_ID>
```

The report ends in a delivery check that names the first unmet condition and the command that clears it, so you never have to guess. Everything the check tests, and what each answer means, is in [why a parked message is still waiting](./messaging.md#why-a-parked-message-is-still-waiting). A send that failed outright instead of parking shows up in `rimz doctor`'s `MESSAGES` section, most often as `receiver not found` for an agent that is no longer running.

## Browser and remote access

### ttyd is missing, too old, or a browser room will not start

Browser access for both Zellij and tmux needs ttyd 1.7.5 or newer on the serving machine. Run `ttyd --version`; with Homebrew use `brew install ttyd` or `brew upgrade ttyd`, and on Debian or Ubuntu install from a current apt repository or the ttyd release page when `apt` offers an older build. Explicit web commands refuse an older version, while a normal room start prints the same fix and keeps the terminal room running.

If ttyd is installed but `rimz web open` times out, run `rimz web status`, then `rimz web stop` and open again. Check that the exact `[web] port` (default `8200`) is free on loopback, and change it when another process owns it.

### A room link opens the session manager

Every room link lands in the session manager instead of its room when RimZ could not build its own browser page. The shared ttyd daemon says so on stderr as it starts (`rimz: skipping browser terminal fixes: ...`), and ttyd then serves its own stock page, which ignores the `?room=` in the link. The theme, the provisioned font, and the key handling go with it. Pick the room from the manager meanwhile, and run `rimz web restart` once you have fixed whatever the warning named. [Browser appearance and input](./web.md#browser-appearance-and-input) lists what the RimZ page adds.

### The remote link keeps dropping

A remote room is plain SSH under a supervisor that reconnects itself when the link drops, and the sidebar footer carries a `⇄ remote` badge that reads link health at a glance. A link that will not hold usually fails the underlying SSH prerequisites. Confirm you can `ssh` to the target unattended (key-based auth, a reachable host), then reconnect. Saved aliases and the reconnect model are in [remote](./remote.md).

## Scheduled and scripted work isn't running

### A loop keeps reporting "previous run still active"

The task's previous runner still owns its overlap lock, so later fires record `overlapped` instead of stacking another turn. Run `rimz loop show <name>` to see the linked run id, holder PID, and start age, then release the lock:

```sh
rimz loop stop <name>
```

Stop gives durable cancellation a grace period, sends SIGTERM only when the lock is still held, and reports success once the lock releases. A lock that survives both steps exits with status `1` and prints its path and holder PID; inspect that process and terminate it yourself when safe. RimZ leaves SIGKILL under operator control.

### A clock task never fired

A clock task no clock has picked up shows `-` in the `NEXT` column of `rimz loop list`. Either no room is open for that task's project and the loop timer is not installed, or the timer is installed but its tick could not reach your systemd user session, in which case it refuses rather than firing and prints the fix. `rimz loop timer status` says which. [Who keeps time](./loops.md#who-keeps-time) covers both, along with what a timer fire does to your machine.

Signal and watch tasks need no timekeeper: whatever emits the signal or runs the command fires them, room or no room.

### A scripted run never finishes

A `rimz agents -p` run or a scheduled agent turn that sits at `running` is usually parked. RimZ holds a run open past a clean turn end while something is still owed to it: an armed `rimz wait`, or children still working. When nothing is owed any more, RimZ ends the run itself: it fails with `parked on a wake that never arrived` and exits `1`, a few seconds after the last thing it was holding for goes away.

Kiro is the exception that really does hang: it fires no hook for a cancelled or errored turn, so give every `rimz agents kiro -p` a `--timeout`, which turns a wedged turn into exit `124`.

A run that ended instantly instead never started at all. The usual cause is reporting hooks the agent's own trust prompt has not approved; the message names the fix, and no run record was written either way. [One run, one exit code](./scripting.md#one-run-one-exit-code) maps every code.

### An agent is parked on a dollar cap

A card reading `⏸` with `budget: $5.04 of $5.00` or `fleet budget: $50.21 of $50.00/day` is not wedged. A cap stopped that agent's spending, and RimZ pressed Esc in its pane the way you would. Nothing exited, and the session files are where the provider left them. Five things reopen it, the next human prompt and a raised cap among them, and [what resumes a parked agent](./budget.md#what-resumes-a-parked-agent) lists all five.

While the room or account scope has no headroom, automation stays shut: `-p` launches exit `125` and loop fires record `budget skipped`. Interactive launches stay available, so a crossed cap never locks you out of your own room.

## A setting did nothing

### Project config isn't taking effect

A cloned repository can ship a `.rimz/config.toml` that names agents, profiles, teams, loop tasks, hooks, environment variables, and provider accounts, any of which can run a command or redirect your credentials. So RimZ keeps that file inert until you trust the workspace: on an untrusted clone it reads only structural metadata, and every command-running field stays disabled.

```sh
rimz trust status    # show the trust state and, when stale, a field-level diff
rimz trust grant     # pin the current executable surface as trusted
```

Editing any command-running field re-hashes the config, so `rimz trust status` and `rimz doctor` report `stale` on the next read (there is no background sweep) and those fields disable themselves until you re-grant. Review the diff, then run `rimz trust grant` again. The full model, including exactly which fields count, is in [project trust](./security.md#project-trust).

Two fields load, enter the trust hash so a grant covers them, and are then read by nothing: `launch_command` on an `[[agents]]` entry, and a `[[hooks]]` table. Declaring either has no effect today. One table is refused outright: a `[layout]` table fails the load with an error telling you to move it to `$RIMZ_HOME/config.toml`. Ignore that fix and delete the table, because per-machine config has no `[layout]` either ([project config](./configuration.md#project-config)).

### RimZ cannot parse a config file

The `MACHINE CONFIG` section names any `config.toml`, `theme.toml`, or `loop.toml` RimZ cannot parse, with the precise error. A broken file does not stop the room: `rimz start` warns on stderr and opens with built-in defaults for every setting in that file. Fix it, then restart so RimZ loads the values you meant.

Markdown definitions fail differently. A broken profile, team, or subagent definition refuses only the launches that select it, while read-only views keep showing the definitions that loaded. `rimz agents validate` names the file and the error; nothing rewrites a definition for you.

### A `[zellij]` or `[tmux]` setting did nothing

Three causes, all covered in [change what the room asserts](./multiplexer.md#change-what-the-room-asserts). Zellij treats a boolean RimZ passes on the command line as a toggle against your `config.kdl`, so setting the same key `true` in both files turns it off: set it in one file. A tmux window-scoped value, meaning the pane borders, passthrough, and per-client resize, reaches only windows opened after the change. And deleting a `[tmux]` key leaves the last value RimZ asserted in place rather than restoring your `~/.tmux.conf` value.

A room restart applies the first two. The third needs the key set back to the value you want.

### A theme or width change did nothing

`theme.scheme` fails quietly. A bundled theme name that does not resolve, a path that is not there, or an inline `[colors]` block with a missing key or an unparseable color is dropped with no message, and the sidebar keeps `TokyoNight Night`. Set the scheme through the command instead, which validates and refuses:

```sh
rimz config set theme "Catppuccin Mocha"
```

Two keys never reach a live sidebar at all. `theme.display.width_percent` and `max_cols` are read once, when the sidebar process starts, and `rimz reload` refetches in place without re-reading them. Press `a` and `d` in the sidebar to set the width now, or end the room and start it again. [Display](./theme.md#display) covers both.

### `rimz setup` dropped my config comments

`rimz setup` rebuilds each per-machine file from the shipped template and transfers your explicit values into the fresh document. Standalone comment lines do not survive that merge, and the merge report does not mention them. `rimz config set` edits the file in place and keeps its comments, so prefer it on a config you have annotated, and copy the file aside before rerunning setup.

## Reset and clean up

These commands touch state RimZ owns: its rooms, its runtime files, and the worktrees it created for you. Your main checkout and anything holding unlanded work stay put. Each one says exactly what it removes and what it keeps.

### Start a room clean

To skip recovering prior agents when a room is reborn, pass `--no-resume` to `rimz`, `rimz start`, or `rimz attach`. The room comes up empty instead of seeding the fleet you left.

This is a one-way skip, not a deferred recovery: the agents that were not brought back are recorded as ended, and the next start without the flag has nothing left to seed from. Their transcripts and history stay in the store, behind `rimz agents show` and `rimz transcript`.

```sh
rimz --no-resume    # come up empty: skip recovering prior agents
```

### Rebuild a wedged room with `rimz reset`

`rimz reset` is the escape hatch for a room that is stuck, or that came back wrong after a reboot. Every step is one you could run by hand: it tears the session down, sweeps the orphaned processes and the cached session state a rebuild would otherwise reuse, archives the room's records, then rebuilds and reattaches. It touches only this workspace's room.

```sh
rimz reset              # rebuild this workspace's room from clean state
rimz reset --yes        # skip the confirmation prompt (required off a TTY)
rimz reset --no-start   # tear down only, then print the rerun hint
rimz reset --hard       # also drop the prior-agent carryover, so rebirth seeds nothing
```

The rebuilt room comes up with no agents in it, whichever flags you pass. A plain reset keeps the prior-agent carryover for history; `--hard` removes that too.

### Sweep stale state with `rimz gc`

`rimz gc` sweeps runtime state that has outlived its use: orphaned atomic-write temp files, dead workspace stores, abandoned queued messages, and clean RimZ-marked worktrees whose work has already landed with no live pane inside. It keeps anything dirty, pending, or unproven, and it always prints a checklist of what it cleaned, what it kept, and why. Every open room also runs it once a day on its own ([automatic sweeps](../reference/cli/maintenance.md#automatic-sweeps)). Run `--dry-run` first to see the plan:

```sh
rimz gc --dry-run          # preview reclaimable state, remove nothing
rimz gc                    # sweep runtime state older than gc.older_than (7d by default)
rimz gc --older-than 1d    # tighten the cutoff to a day
```

```
gc — would reclaim 62 MB (dry run)
  checked 4 of 8 areas · cutoff 7d

  ✦ worktrees       would remove 3 · 62 MB · 4 kept
      kept: api-redesign — in use
      kept: auth-fix — not merged yet
      …
      would remove: guides-tune  21 MB  merged
      …
  ✦ workspaces      would prune 31 · 9.2 KB
      would prune: ws_8282bdd4e723c6a86e061fa8 — abandoned setup, never used (280 B)
      …
  ✦ runtime         would remove 178 stale files · 109 KB — 5 roots scanned
      …
  ✓ temp files      none orphaned
  – messages        skipped (dry run)
  – event log       skipped (dry run)
  – agent cache     skipped (dry run)
  – loop schedules  skipped (dry run)
```

A kept worktree carries its reason (`in use`, `not merged yet`), so nothing with unlanded work is ever swept.

### Where state lives, and full removal

Per-machine config lives under `~/.rimz/` (`config.toml`, `theme.toml`, `loop.toml`, `remote.toml`), and durable room state lives in the same home, one directory per project under `~/.rimz/ws/`. Both are plain files you can read and edit, and [`rimz paths`](../reference/cli/paths.md) prints every location.

To remove RimZ from the machine, `rimz uninstall` takes out installed hooks, running rooms, runtime state, and the binaries it finds; durable stores and per-machine config stay unless you ask for them:

```sh
rimz uninstall            # hooks, rooms, runtime state, binaries; keeps stores and config
```

`--state`, `--config`, and `--all` widen the removal to durable stores and per-machine config; the exact scope of each flag is in the [maintenance reference](../reference/cli/maintenance.md#uninstall-rimz). Project-local `.rimz/` directories and RimZ-owned worktrees stay in place, because they can hold project config and unlanded work.

## Filing an issue

Capture the state RimZ sees and attach it to the report:

```sh
rimz doctor --json --output rimz-doctor.json    # full environment report as JSON
```

`rimz doctor --json` is the artifact to attach: it carries the backend, versions, per-agent hook status, trust state, and the room health that most reports need, and it carries no prompts and no transcripts. One thing to skim before you attach it to a public issue: the multiplexer log lines doctor flagged are kept verbatim, so read those rows if your log could hold a path or a hostname you would rather not publish.

## See also

- [Set up your machine](./setup.md): the first-pass configuration these fixes assume.
- [Security and trust](./security.md): the trust model, the sandbox limits, and the presence grant behind three of the fixes above.
- [Multiplexer](./multiplexer.md): what a room asserts on Zellij and tmux, and how to change it.
- [CLI reference: maintenance](../reference/cli/maintenance.md): every flag on `reset`, `gc`, `reload`, `sidebar repair`, and `uninstall`.

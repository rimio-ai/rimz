# Troubleshooting

Diagnosing a room by hand means `ps`, `tmux ls`, a provider log, and a guess about which of them is lying. RimZ keeps the room's truth in one durable store, and `rimz doctor` reads it along with the machine around it in a single pass, collecting what needs your attention into a closing fix block. Run it first. The rest of this page is the symptom catalogue: what went wrong, the command that fixes it, and the guide that owns the full story.

## Start with `rimz doctor`

`rimz doctor` probes the machine, the room, and RimZ's own state: the multiplexer and whether its version clears the floor, per-machine config parsing, sandbox support, per-agent hook status, provider accounts, scheduled loop tasks, project trust, terminal color depth, workspace and store health, the live agents, messages that failed to land, and recent incidents. A normal run changes no RimZ state and starts, stops, or moves no room agents. It may briefly start configured language servers to check startup, then terminate those probes; a server may write its own files. Only `--clear` writes a history watermark and `--output` writes the report to the named file.

```sh
rimz doctor
```

The report reads top to bottom, machine setup first and live room state last. This capture uses a fresh home with hibernation disabled and legacy directories still present:

```console
RimZ doctor
  version: 0.4.3+gd4a4fa34b3f4.dirty
  user:    marvin (uid 1000)
  binary:  /tmp/doctor-round5.urAnxRtn/rimz

WORKSPACE
  id:              ws_225df3094012a213addf6b7d
  project root:    /home/me/project
  root class:      directory
  worktree root:   /home/me/project
  worktree branch: <detached>
  session:         project-225d
  sock headroom:   ✓ OK (67/108 bytes for /tmp/dr5uArT/rimz/ws/project-225d/sock)

MULTIPLEXER
  backend:    tmux
  version:    tmux 3.7c
  tmux floor: ✓ OK (>= 3.5.0 required)
  tmux popup: ✓ supported
  binary:     /home/me/bin/tmux — tmux 3.7c
  log:        server logging off (start tmux with `-v` to enable)
  socket:     /tmp/dr5uArT/rimz/tmux/server
  room:       absent
  presence:   not applicable — this workspace room is absent on tmux
  ttyd web:   ✓ ttyd version 1.7.7 (/usr/bin/ttyd)
    ✓ duplicate sessions: none

TERMINAL
  depth:   ✓ truecolor (mode auto)
  signals: truecolor-advertised=true COLORTERM=truecolor TERM=xterm-256color terminfo-truecolor=false

HOME
  home: ~/.rimz (RIMZ_HOME)
    ! legacy roots ~/.config/rimz, ~/.local/state/rimz are still on disk and not read

MACHINE CONFIG
  config files: ✓ all present files parse

SANDBOX
  mode:    host
  bwrap:   /usr/bin/bwrap
  version: bubblewrap 0.11.0
  probe:   ▸ passed

HIBERNATE
  enabled: false
  active:  skipped: disabled

HOOKS
  no agent reports to RimZ yet
   AGENT   STATUS
✗  claude  not installed
    · not found on this machine: amp, antigravity, codex, copilot, cursor, droid, grok, kimi, kiro, opencode, pi, qwen
      RimZ offers their hooks as soon as one appears

FOLDER TRUST
   AGENT   LOGIN    STATUS
!  claude  default  undecided

ACCOUNTS
   KIND    NAME     HOME  STATUS  DEFAULT FOR
✓  claude  default  -     ready   new rooms
✓  codex   default  -     ready   new rooms

LOOP TASKS
  none configured

LSP
    · no shared language servers

REMOTE CONTROL
  remote control: off

STORAGE
  rimz on disk: 38 B
   AREA     SIZE  PATH
·  home     38 B  ~/.rimz
·  runtime     -  /tmp/dr5uArT/rimz

PROTOCOLS
  event:   rimz.event.v2
  sidebar: rimz.plugin.v5

TRUST
  trust: · no project config

AGENTS
  none observed

MESSAGES
  no open messages

DIAGNOSTICS
  no recent records (/home/me/.rimz/ws/ws-225df3094012a213addf6b7d/audit/diag.log.jsonl)

✗ 1 problem in HOOKS  ·  ! 2 warnings in HOME, FOLDER TRUST
  ✗ HOOKS: claude not installed
      fix: run `rimz hooks install claude` to wire claude agents
  ! HOME: legacy roots ~/.config/rimz, ~/.local/state/rimz are still on disk and not read
      fix: move what you still need from ~/.config/rimz, ~/.local/state/rimz into ~/.rimz by hand, then delete them (docs/guide/configuration.md#moving-from-the-xdg-roots)
  ! FOLDER TRUST: claude@default folder trust is undecided
      fix: run `rimz trust grant --agents claude` in `/home/me/project`, or answer claude's folder-trust prompt once there
```

Most symptoms below show up as a named doctor row, with the next action collected in the closing block, so the report doubles as the fix list.

If agent state stays stale or messages wait behind queued hooks, look for a drain-lag warning in DIAGNOSTICS. It shows the episode's worst sampled delay and peak backlog size. The warning clears after 15 minutes without another lag report; `rimz doctor --clear` dismisses the retained history immediately without deleting the evidence.

### Reading the report

Start at the closing block, not at the longest table. Rows have two tiers:

- `!` warnings and `✗` problems need attention. Each counts once. Findings in the same section that share a fix are collected into one closing entry naming their count and subjects, with the fix printed once.
- `▸` context and `·` neutral background facts do not count. `✓` means the check is healthy.

The closing headline counts findings and names their sections, followed by all problems and then all warnings with their fixes. Work down that list. If nothing counts, the report ends with `✓ everything checked is healthy`, even when context rows remain. `--audit` adds rows without changing that verdict: agents waiting for an answer belong in the sidebar, and ended sessions are history, not doctor findings.

A spent account with a future reset is context, not another warning to clear. Its row names agents paused or failed on that limit, without counting them again under `AGENTS`. An account with no credits and no reset warns instead. Account readings come from the cache and show their age; doctor does not refresh provider usage. Agent rows use the same status as their cards.

`MESSAGES` keeps past delivery failures in one context line grouped by status. Follow `rimz message list --all --system` to inspect them; stuck deliveries still earn a finding and fix. `DIAGNOSTICS` separates incidents still needing investigation from ones RimZ handled and closed itself. Only investigative warnings and problems count; settled incidents are context, not a to-do list. `--audit` expands those settled incidents with their times and summaries, and lists each routine multiplexer-log group with its span.

Plain output keeps loop tasks that need attention and running or crashed language servers visible, summarizing the rest. Memory-pressure dormancy is neutral: the next query can restart the server. When reporting a bug, widen those sections, include retained message-failure rows and observed agent sessions, and omit multiplexer log text:

```sh
rimz doctor --audit --no-log-text --output doctor.txt
```

Use `rimz doctor --json` for structured evidence. The [doctor reference](../reference/cli/getting-started.md#diagnose-with-doctor) lists the sections and flags.

The `MULTIPLEXER` section's `log` row names the server log, how far back the scan reached, and who writes into it. The Zellij log covers every Zellij session your user owns, so an issue it reports can come from a session that is not this room; the tmux log is scoped to RimZ's own server. Check that scope before attributing an issue to the room.

## A room or agent won't start

### Zellij or tmux is missing or too old

RimZ needs Zellij 0.44.2 or newer, or tmux 3.5 or newer ([install one](./installation.md#install-zellij-or-tmux)); pixel pets add tmux 3.6. The `MULTIPLEXER` section reports the backend RimZ resolved and its version against the floor, as `tmux floor: ✓ OK (>= 3.5.0 required)` or `TOO OLD`. Install or upgrade the multiplexer when the row flags it.

RimZ refuses to open a room on either backend below its floor and names the upgrade, and the same check runs when you attach to a room that already exists, so upgrading the multiplexer is the way back in. An unrecognised tmux version, including a development build, also refuses: install a supported release build, or start a new room with `--mux zellij`. When both backends are installed and RimZ resolved the one you did not want, pick explicitly with `--zellij`, `--tmux`, or `--mux <name>`.

### RimZ won't nest a room inside the current session

A RimZ room is its own Zellij or tmux session, so it cannot nest inside a session of that same backend you are already attached to. Run bare `rimz` (or `rimz start`) from inside one and it reports whether this directory's room is your current room, is running elsewhere, or is not running. In the last case:

```
This directory's room isn't running, and zellij can't start one inside this session.
Detach and run `rimz` here, or run it from another terminal.
```

If you're already in this directory's room, there is nothing to do. Otherwise detach and run `rimz` here, or open a fresh terminal outside the backend and run `rimz` there. The [CLI reference](../reference/cli/getting-started.md#attach-or-print) shows all three reports.

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

A provider following defaults or pinned to a named account refuses when the machine `config.toml` is unreadable, rather than falling back to the wrong account. Correct the file; `rimz config get` shows the strict error. A provider pinned to `default` needs neither project nor machine configuration. An invalid deciding `[accounts.use]` selection names its source and the commands to declare or clear it. Each refusal affects that provider only; start checks them all. See [account selection](./accounts.md#start-a-room-on-an-account).

### Sandbox isolation is unavailable

The `SANDBOX` section reports the configured mode, the bubblewrap path and version, the probe result, and the fix. Sandbox mode needs Linux and the `bubblewrap` package (`bwrap` on `PATH`). A failed probe usually means unprivileged user namespaces are disabled (`kernel.unprivileged_userns_clone` or `user.max_user_namespaces`) or an AppArmor or LSM policy blocks them.

Follow the probe's error for your host, or switch to host isolation, which needs no bubblewrap:

```sh
rimz config set agents.isolation host
```

The switch affects new launches, not existing panes. Profile `skills` lists can stay: host mode ignores them. [Sandbox isolation](./security.md#sandbox-isolation) covers the limits of each mode.

### A Rust build in a sandboxed pane fails through sccache

A Cargo build that uses sccache as its `rustc-wrapper` fails in a sandboxed pane with `error writing dependencies ... No such file or directory` or `exit status: 254`. The sccache server is shared by every pane and normally compiles in the view of whichever process started it, where the sandboxed pane's `/tmp` paths do not exist. RimZ sets `SCCACHE_CLIENT_SIDE=1` in sandboxed panes so each build compiles in its own pane and still shares one cache, but sccache only honors it from version 0.17, and ignores it while `SCCACHE_LOG` is set or distributed compilation is configured. Unset `SCCACHE_LOG` before you rebuild, even when debugging this failure.

Upgrade sccache, then stop the old server so the next build starts a new one:

```sh
cargo install sccache --locked
sccache --stop-server
```

To keep host shells from compiling inside a server that a sandboxed pane started, also set `client_side_mode = true` in your sccache config file. [Sandbox isolation](./security.md#sandbox-isolation) covers what a sandboxed pane sees.

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

The install is additive, so your existing hooks stay, and `rimz doctor` reports per-agent hook status afterward. Restart the agent so it picks up the new hooks. A restart, resume, or fork refuses until the hooks are installed and, for Codex, trusted in its `/hooks` screen. To back the change out, `rimz hooks uninstall [AGENT]` removes exactly what RimZ added and restores any statusline it wrapped.

Three agents have a wrinkle beyond the install, and [agent support](../reference/agent-support.md) carries the rest of the per-agent detail. Kiro needs CLI 2.31.0 or later, and `rimz hooks install kiro` names the installed version when it refuses. Grok snapshots its hook registry for a running TUI, so after a reinstall reload the registry through Grok's Hooks UI or start a new session; a session still holding the old snapshot keeps calling the command RimZ replaced. When an administrator has set Grok's `allow_managed_hooks_only` policy, Grok runs only hooks from its system policy files, so `rimz hooks install grok` and every Grok launch refuse and name the policy file; the fix is for the administrator to add `rimz hooks feed --source grok` as command hooks in `/etc/grok/managed_config.toml` or `/etc/grok/requirements.toml`. Copilot reads its model and token figures from a local telemetry file, so an OTLP exporter configured in your environment routes them to your own pipeline and leaves that card carrying lifecycle alone; pin `COPILOT_OTEL_EXPORTER_TYPE=file` and an explicit `COPILOT_OTEL_FILE_EXPORTER_PATH` when you want both. Copilot also refuses to launch when its settings disable hooks, or when an explicit ask launch would inherit automatic permission approval; follow the error's named-file fix to enable hooks or restore manual permissions.

### A card went quiet after a config edit

If a card stops updating after you hand-edited an agent's config, the RimZ-managed hook block was probably disturbed. Run `rimz hooks install --dry-run` to see the current diff, then `rimz hooks install` to restore the block. Some agents gate hooks behind their own trust prompt; when one reports installed-but-untrusted hooks, the `HOOKS` row in `rimz doctor` prints the exact fix.

### Codex sessions drop with a WebSocket reset about hourly

`WebSocket protocol error: Connection reset without closing handshake` can mean Codex's own updater is running a different release from the managed install. `rimz doctor` reports that state in its `REMOTE CONTROL` section, as `Codex remote-control updater version skew`, and prints the two process paths that disagree.

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

`rimz reload` will not do it: reload upgrades sidebars that are still alive and never opens a pane. It does say so, counting the rooms and naming the verb (``1 room with no sidebar reporting in; mount or replace it with `rimz reload --repair`.``), and `rimz reload --repair` runs both operations in order.

### The "Sidebar degraded" banner

A banner such as `⚠ Sidebar degraded for 8s: snapshot failed` means a render could not read a fresh snapshot, usually because the binary moved on disk or the durable state directory vanished mid-write. Nothing is lost: the durable store stays the source of truth, so fixing the underlying cause (reinstall or repoint the binary, confirm the state directory exists) clears the banner on the next good frame.

Once it recovers, the banner steps down to a dim, dismissable notice (`⚠ last alert 8s ago: …  ·  x dismiss`), so a failure that flickered past is still visible after the fact. Press `x` to clear it; two consecutive failed renders re-arm it, which is why a single bad frame does not.

### Colors, glyphs, or pets don't render

Two upgrades need terminal support beyond color:

```sh
rimz config set theme.style modern        # truecolor plus Nerd Font icons
rimz config set theme.pets.enabled true   # an animated companion on the dashboard
```

`modern` needs a Nerd Font installed in the terminal. Pets render as crisp pixels only in Ghostty, kitty, and a browser tab served by RimZ, and inside tmux that also needs tmux 3.6 or newer with `allow-passthrough on`. In a tmux room every attached client has to qualify, so one plain terminal attaching drops the room to cell art within ten seconds. Zellij rooms are always cell art. When the probe finds host-terminal support, `rimz doctor`'s `zellij kitty graphics` row says so, but also explains that Zellij rooms still draw cell art because Zellij rejects the placement the pixel tier uses.

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

First check `rimz doctor`'s `ttyd web` row. It appears for both Zellij and tmux while `[web] enabled` is true, and reports the resolved binary and version or the fix for a missing or older binary.

Browser access for both Zellij and tmux needs ttyd 1.7.5 or newer on the serving machine. Run `ttyd --version`; with Homebrew use `brew install ttyd` or `brew upgrade ttyd`, and on Debian or Ubuntu install from a current apt repository or the ttyd release page when `apt` offers an older build. Explicit web commands refuse an older version, while a normal room start prints the same fix and keeps the terminal room running.

If ttyd is installed but `rimz web open` times out, run `rimz web status`, then `rimz web stop` and open again. Check that the exact `[web] port` (default `8200`) is free on loopback, and change it when another process owns it.

### A room link opens the session manager

Every room link lands in the session manager instead of its room when RimZ could not build its own browser page. The writable browser daemon says so on stderr as it starts (`rimz: skipping browser terminal fixes: ...`), and ttyd then serves its own stock page, which ignores the `?room=` in the link. The theme, the provisioned font, and the key handling go with it. Pick the room from the manager meanwhile, and run `rimz web restart` once you have fixed whatever the warning named. [Browser appearance and input](./web.md#browser-appearance-and-input) lists what the RimZ page adds.

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

`rimz doctor` flags live clock tasks with no open room and no active loop timer under `LOOP TASKS`. Run `rimz loop timer install`, then `rimz loop timer status`, or run `rimz start` in the task's root to keep time in an open room. Disabled and paused tasks are not flagged.

A clock task no clock has picked up shows `-` in the `STATE` column of `rimz loop list`. Either no room is open for that task's project and the loop timer is not installed, or the timer is installed but its tick could not reach your systemd user session, in which case it refuses rather than firing and prints the fix. `rimz loop timer status` says which. [Who keeps time](./loops.md#who-keeps-time) covers both, along with what a timer fire does to your machine.

Signal and watch tasks need no timekeeper: whatever emits the signal or runs the command fires them, room or no room.

### A scripted run never finishes

A `rimz agents -p` run or a scheduled agent turn that sits at `running` is usually parked. RimZ holds a run open past a clean turn end while something is still owed to it: an armed `rimz wait`, or children still working. When nothing is owed any more, RimZ ends the run itself: it fails with `parked on a wake that never arrived` and exits `1`, a few seconds after the last thing it was holding for goes away.

Kiro fires no hook for a cancelled or errored turn, so give every `rimz agents kiro -p` a `--timeout`, which turns a wedged turn into exit `124`. While a sidebar runs, a cancelled Kiro turn releases `message --wait` and `agents wait`; an errored turn still waits out the deadline. Supervised runs wait out their deadline on both cancel and error.

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

Two fields load, enter the trust hash so a grant covers them, and are then read by nothing: `launch_command` on an `[[agents]]` entry, and a `[[hooks]]` table. Declaring either has no effect today. One table is refused outright: a `[layout]` table fails the load with an error naming what replaced it, `placement` under `[agents]` for where a launch lands and a team definition for a whole room shape. Delete the table; per-machine config has no `[layout]` either ([project config](./configuration.md#project-config)).

### RimZ cannot parse a config file

The `MACHINE CONFIG` section names parse and validation errors in `config.toml`, `theme.toml`, and `loop.toml`. An unreadable `config.toml` refuses a provider following defaults or pinned to a named account, so account resolution cannot silently fall back. Only a pin to `default` bypasses that read; merely inheriting `default` does not. Unreadable `theme.toml` and `loop.toml` produce warnings and built-in defaults for that file. Invalid accounts, account budgets, or notification settings do stop start. Invalid palette sources only produce notices; other theme settings still apply. Fix the reported error, then restart so RimZ loads the values you meant.

Markdown definitions fail differently. A broken profile, team, or subagent definition refuses only the launches that select it, while read-only views keep showing the definitions that loaded. `rimz start` summarizes broken definitions in a few lines grouped by cause; `rimz agents validate` lists each file and error. Nothing rewrites a definition for you.

### A `[zellij]` or `[tmux]` setting did nothing

Run `rimz start` to apply changes to an open room. Zellij's start-only settings, and removal of an optional `[zellij]` key, need a new room. If `[zellij] mouse_mode = true` still leaves the mouse disabled, remove or change `mouse_mode false` in `config.kdl` before attaching. If `pane_frames = true` draws no frame, split the work pane: Zellij frames a pane only when its tab holds another framed pane, and the sidebar and bottom bar do not count. See [change what the room asserts](./multiplexer.md#change-what-the-room-asserts) for the start-only list and when each backend applies changes.

### A theme or width change did nothing

`rimz start` and `rimz doctor` report a bundled theme name that does not resolve, a missing scheme path, or an inline `[colors]` block with a missing required key or an unparseable color. Start still opens the room and names the palette the sidebar keeps; other theme settings still apply ([theme fallback](./theme.md#color-scheme)). Fix the named source, or set the scheme through the command, which validates and refuses:

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

Typed at a terminal, this is a one-way skip, not a deferred recovery: the agents that were not brought back are recorded as ended, and the next start without the flag has nothing left to seed from. From a script or any start with no terminal, the room still comes up empty but the prior top-level agents stay parked: a start into the running room tells you so, and `rimz agents resume <lane>` brings a lane back. Subagents those parked agents had running are closed, because a resumed parent relaunches any children it still needs. Their transcripts and history stay in the store, behind `rimz agents show` and `rimz transcript`.

```sh
rimz --no-resume    # come up empty: skip recovering prior agents
```

### Rebuild a wedged room with `rimz reset`

`rimz reset` is the escape hatch for a room that is stuck, or that came back wrong after a reboot. Every step is one you could run by hand: it tears the session down, sweeps the orphaned processes and the cached session state a rebuild would otherwise reuse, archives the room's records, then rebuilds and reattaches. It touches only this workspace's room.

```sh
rimz reset              # rebuild this workspace's room from clean state
rimz reset --yes        # skip the confirmation prompt (required off a TTY)
rimz reset --no-start   # tear down only, then print the rerun hint
rimz reset --hard       # also forget history, runs, and scratch
```

The rebuilt room comes up with no agents in it, whichever flags you pass. A plain reset keeps prior-agent history, transcripts, messages, run records, scratch and skill copies. `--hard` also deletes those transcripts, message history, runs, scratch, room temporary files, and prior-agent carryover. In a sandboxed room it first ends any background process an agent left running inside its sandbox view, since that process still uses the temporary files being deleted; if one will not stop, the reset names it and asks you to stop it and run `rimz reset --hard` again. Rotated event archives and provider-owned session files remain. Standing fleet budget choices survive either reset.

### Sweep stale state with `rimz gc`

When stale state piles up, `rimz gc` cleans the current room: expired scratch, terminal runs and audit history, orphaned temp files, abandoned messages, and clean RimZ-marked worktrees whose work has landed with no live pane inside. It keeps anything dirty, unlanded, or unknown and reports why. Add `--all` to sweep other rooms and shared state, including dead workspace stores. It never creates a store. Open rooms sweep daily, with one elected machine sweep also covering closed rooms ([automatic sweeps](../reference/cli/maintenance.md#automatic-sweeps)). Run `--dry-run` first:

```sh
rimz gc --dry-run          # preview the current room, remove nothing
rimz gc                    # sweep the current room
rimz gc --older-than 1d    # tighten the cutoff to a day
rimz gc --all --dry-run    # preview every room and shared machine state
rimz gc --all              # sweep that machine-wide scope
```

A kept worktree carries every reason (`in use`, `uncommitted changes`, `unlanded work`, `unknown`) and its live holders, so nothing with unlanded work is ever swept.

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
rimz doctor --json --no-log-text --output rimz-doctor.json
```

The report carries the backend, versions, per-agent hook status, trust state, and the room health that most reports need. `--no-log-text` omits multiplexer log record text from summaries, samples, and excerpts in both JSON and human reports. Only severity, timestamps, counts, and Zellij module targets survive from the records, alongside RimZ's own diagnostic wording; other report sections are unchanged. A maintainer may ask for the unstripped report if those details are not enough to diagnose the problem.

## See also

- [Set up your machine](./setup.md): the first-pass configuration these fixes assume.
- [Security and trust](./security.md): the trust model, the sandbox limits, and the presence grant behind three of the fixes above.
- [Multiplexer](./multiplexer.md): what a room asserts on Zellij and tmux, and how to change it.
- [CLI reference: maintenance](../reference/cli/maintenance.md): every flag on `reset`, `gc`, `reload`, `sidebar repair`, and `uninstall`.

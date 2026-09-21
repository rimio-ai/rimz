# Remote

A RimZ room is a plain Zellij or tmux session that lives on one host: a fleet on a server, or the room you left running on the machine at home. It keeps working headless while nobody is attached. `rimz remote connect` opens an SSH session to that host and attaches to the room there, so your local terminal renders exactly what the host is running, sidebar and agent panes alike. The room, its agents, and their transcripts never leave the host.

Two other ways in reach the same room. `rimz remote connect dev --web` opens it in a local browser instead of this terminal ([Web](./web.md)), and the providers' own mobile apps can answer an agent's questions without attaching at all ([answer asks from your phone](#answer-asks-from-your-phone)).

## Connect to a room on another host

You already have a way to do this: `ssh dev-box`, then `tmux attach`. RimZ keeps every part of it. The same `ssh` runs, so your `~/.ssh/config`, keys, agent, ports, and jump hosts apply exactly as they do for any `ssh` you type, and the far end runs the host's own `rimz`. What it adds is everything around the attach: one word instead of two commands and a remembered path, a link that reconnects itself, and a badge that tells you when the link is the slow part.

Point it at a target and it connects:

```sh
rimz remote connect dev-box:~/code/query-engine   # a path: that project's room
rimz remote connect dev-box:query-engine          # a bare name: resolved on the host
```

The target is `[user@]host:<session-or-path>`, spelled like an `scp` destination. A suffix containing `/` or starting with `~` is a path, and the host's `rimz` opens that project's room. A bare name is resolved on the host: a directory by that name under the remote `HOME` wins, and otherwise RimZ attaches to the session by that name. When a directory and a session share a name, `dev-box:session:query-engine` forces the session. The [remote CLI reference](../reference/cli/remote.md#targets) has the full table.

A connect that fails prints its own fix on stderr: a missing `rimz` on the host names the `rimz remote setup` command to run, and a path that does not exist names the target to correct.

Because you attach to the session the host is already running, everything you left is there: every agent in its `#channel` tab, every question still waiting in the sidebar, plus whatever finished while you were gone.

### Install rimz on the host

Run setup once, before the first connect, when the host has no `rimz` on its `PATH`:

```sh
rimz remote setup dev-box
rimz remote setup agent@prod-box:/srv/query-engine
rimz remote setup dev                           # a saved alias
```

It takes a saved alias, a raw target, or a bare `[user@]host`, and needs nothing but `ssh` locally. Over that connection it reads the host's OS and architecture with `uname`, downloads the matching release archive and its `SHA256SUMS` from the project's latest GitHub release, checks the archive against them, and installs the binary at `~/.local/bin/rimz`. That directory is on the `PATH` `remote connect` hands the host's `rimz`, so the next attach finds it. Prebuilt releases cover Linux x86_64 and macOS on arm64 and x86_64; any other platform stops with `no prebuilt for <os>/<arch>` and points at the [installation guide](./installation.md).

Running `setup` again installs the latest release over the old binary, which is also the fix when the two ends disagree on version. RimZ compares them before entering the room: a patch difference warns and proceeds, a minor one refuses until you upgrade the older side or pass `--force-version` for that one attach, and a major one refuses outright ([version checks](../reference/cli/remote.md#version-checks)). To take RimZ off a host again, delete `~/.local/bin/rimz` and the `~/.rimz/` directory there.

### Save the target as an alias

Save the target once and the trip is one word. An alias carries the target and its connection defaults, so `rimz remote connect dev` is the whole journey:

```sh
rimz remote add dev dev-box:~/code/query-engine   # save the target as `dev`
rimz remote connect dev                           # open it, reconnecting link and all
rimz remote list                                  # every saved alias (alias: ls)
rimz remote update dev dev-box:~/code/api         # replace one alias's target and settings
rimz remote rename dev devbox                     # rename one
rimz remote rm devbox                             # forget one
```

Aliases live on your machine in `~/.rimz/remote.toml`, and none of these commands contacts the host. `rimz remote list` groups them under their SSH destination, so the same host under two users reads as two groups, and prints each alias's full target and its settings; `--json` prints one flat array instead. `update` rewrites the whole record, so a setting you leave off returns to its default. The field table is in the [alias reference](../reference/cli/remote.md#saved-aliases).

## A link that heals itself

A plain `ssh` ends the moment the connection does. `rimz remote connect` supervises the link instead: when the train wifi cuts out or the laptop sleeps, it reconnects on its own and reattaches to the untouched room on the host, so a flaky connection never costs you your place. Closing the room itself ends the command cleanly.

One panel carries the first connection and every recovery after it, and four rows track the path to your room: end-to-end internet access over HTTP, the SSH server, the next SSH session, and the multiplexer attach that hands you the room. A failed attempt shows the last SSH error verbatim, and with the network down the panel waits without inventing a countdown. Ctrl-C stops the flow at any point, including while the host is asking an attended setup, hook-installation, or trust question. The server row behaves differently on two kinds of route: a proxied target drops it, because a direct dial would not test the path SSH takes, and a VPN or TUN route keeps it but skips its unreliable TCP check and lets SSH prove the path.

Retries pace themselves. While the network is up, RimZ opens an SSH connection in the background every couple of seconds for the first three minutes, then widens the gap toward thirty seconds as the outage drags on. A change in the local address your machine routes from, which is what a new wifi network or a VPN coming up looks like, resets that pacing and triggers an immediate attempt, so a laptop that wakes on a different network is back in seconds instead of waiting out a backoff.

Your terminal tells you on every edge, because a dead link cannot count on the remote sidebar to reach you. A probe that stalls raises a local terminal notification. A confirmed drop and a recovery raise one and also fire any [notification handler](./notifications.md) you configured, as the `link_lost` and `link_restored` kinds. All of it obeys your notification settings, so a machine with sound off stays quiet.

A retry runs unattended, and that limits what it can settle for you. Hook installation, project trust, and recovery consent all need an answer, so a reconnect leaves them pending for the next start you make while you are present rather than parking the link on a prompt nobody is there to type into. An initial password, two-factor, or host-key prompt is the exception: it falls back to one plain interactive `ssh` attempt so you can answer it.

On Zellij, a reconnect retires an orphaned earlier attach that used the same target spelling from this machine, so opening the same room twice from one terminal moves it to the newer one, while attachments from different machines coexist.

Two flags tune the posture:

```sh
rimz remote connect dev --no-reconnect   # one ssh run, no supervisor, no health probe
rimz remote connect dev --reset          # a room born by this connect comes up empty
```

`rimz remote reset dev` is the shorthand for the second. Save either choice on an alias with `rimz remote add ... --no-reconnect`, or with `--no-resume`, which is the alias spelling of `--reset`.

## Reading the link badge

A supervised connection carries a health probe alongside your session: a small ping travels the same SSH link every two seconds, and its round trip drives a badge in the sidebar footer. The probe rides the real connection rather than ICMP, so the badge reflects what your session actually feels. A `--no-reconnect` run skips the probe and shows no badge.

- `⇄ remote 210ms` is the round trip to the host. It warms from `⇄ remote …` on the first samples, then shades from green at or under 100ms, through gold and amber, to red at 400ms and above.
- `⇄ remote 210ms 15%` appends probe loss once it passes 10 percent, measured over the last thirty probes as the share with no answer inside two seconds. A clean link shows no percentage, and that color climbs from green to red across 0 to 30 percent loss.
- `⇄ remote ?` means the reading went stale: no fresh sample for ten seconds, usually a struggling link or a reconnect in flight. Past two minutes the badge disappears.

The badge shows the worse of its two axes, so a link with a fast round trip and heavy loss still reads hot.

## Ports forward themselves

Attach normally, then start `python3 -m http.server 3000`, `pnpm serve`, or another dev server in a room pane on the host. Within seconds `http://localhost:3000` opens on your local machine, with no second flag and no second SSH command.

RimZ forwards a listener when it starts after you attach, belongs to your remote user, uses port 1024 or above, and binds a loopback or wildcard address. The local side binds `127.0.0.1` only, so the forwarded service stays on your machine. Discovery keeps the thirty-two lowest qualifying ports, and at most sixteen forwards stay open at once.

A forward closes shortly after its server stops, reopens with the room after a link recovery, and every forward closes when you detach. A port already busy on your local machine is skipped, with a terminal notification naming the port and the retry: free it, then stop and restart the server on the host, because the listener disappearing and coming back is what triggers the retry.

`rimz remote connect dev --no-auto-forward` turns forwarding off for one connection, and `rimz remote add dev dev-box:~/code/query-engine --no-auto-forward` saves the switch. Forwarding needs the supervised link, so `--no-reconnect` and `--web` connections never forward, and listener discovery reads the host's `/proc`, so Linux hosts only. Other hosts keep the normal remote connection without automatic forwards.

## What crosses the link

The room crosses as a screen. The agents, their transcripts, the git work, and the store all run on the host; SSH carries the multiplexer's rendered output one way and your keystrokes the other, the same bytes a local attach would paint. The multiplexer redraws only the focused tab and diffs each frame, and every attach turns on SSH compression, so an idle room is nearly silent and a busy pane costs about what watching it locally would. Forwarded ports are the exception: that traffic is your own dev server's, and it flows only while you are attached.

To measure the actual traffic, attach, then run `rimz pane bandwidth` from inside the remote shell. It belongs on the Linux host serving the room, where the write-rate counters live; from your own machine it has nothing to read. It samples for five seconds and attributes the output rate per pane, with the compressed SSH wire-rate alongside:

```sh
rimz pane bandwidth   # on the host: per-pane output and the SSH wire-rate over five seconds
```

The per-pane figures are raw producer output. The `WIRE(ssh↑)` and `WIRE(ssh↓)` rows are the compressed payload actually on the socket, normally far below the sum, because the multiplexer throttles to the focused tab before SSH compresses what is left. `--secs` widens the window, and the [pane reference](../reference/cli/pane.md) covers what the command prints when a host cannot measure.

## Continuity across reboots

The room and its state both live on the host, in durable flat files under `~/.rimz/` there, so the room survives a mux crash or a reboot of the host. On the next start you make while you are present, the host's `rimz` asks whether to recover the agents it found and defaults to yes: they come back idle in their tabs, one prompt from where they stopped (`claude --resume`, `codex resume`, `pi --session`). A start with no terminal on stdin recovers without asking, and a room you closed deliberately stays closed. A supervised reconnect after a host reboot takes that automatic path, and hook-installation and project-trust offers stay pending for your next attended start.

To bring a room up empty instead, or to rebuild one that came back wrong, run `rimz` with `--no-resume` or `rimz reset` on the host ([reset and clean up](./troubleshooting.md#reset-and-clean-up)).

Keeping the agent processes themselves alive across a reboot is not something RimZ does. Reach for systemd, tmux-resurrect, or Zellij resurrect on the host to carry them across a restart, and RimZ reattaches to whatever is still running ([DESIGN.md → Non-goals](../../DESIGN.md#non-goals)).

## Answer asks from your phone

A fleet that runs while you are out still stops to ask: a permission prompt, a plan approval, a question only you can decide. Claude Code and Codex each ship **remote control** (`claude remote-control` and `codex remote-control`), the bridge behind their official mobile apps: it links a machine to your account so the app can see and drive the sessions running on it. The feature is entirely the provider's. What RimZ adds is keeping it up, because the bridge only helps if it is already running when an agent stops to ask, and starting infrastructure with the room is a room's job.

Two per-machine toggles opt in. Both are off by default:

```sh
rimz config set remote_control.claude true    # keep `claude remote-control` up with the room
rimz config set remote_control.codex true     # keep Codex's remote-control daemon up
```

With a toggle on, the ask reaches your phone as a push from the provider's own app, your answer lands in the same session on the machine running the room, and the turn continues in its pane as if you had typed it there. Leave the room on a server, go to dinner, and a 9 p.m. question is one tap instead of a fleet stalled until morning.

RimZ stays out of the path: each toggle starts the provider's own command with the room and nothing more. Exactly what runs:

- **Claude.** `claude remote-control --spawn worktree`, as a long-lived pane in the room's background `rimzd` tab, run from the project root, so a session you start from the phone is carved into its own on-demand worktree instead of touching your checkout. While the host is up, the Claude block on the [provider dashboard](./sidebar.md#the-provider-dashboard) wears a `⇅ rc` flag.
- **Codex.** `codex remote-control start`, which brings up Codex's own daemon with remote control enabled, one per Codex account and shared by every room on that account. It runs from Codex's managed standalone install, not from whatever `codex` your `PATH` finds, so the toggle does nothing without that install; `rimz doctor` prints the installer command when it is missing.

`rimz start` checks the preconditions before it opens the room. An enabled toggle whose agent is not installed is skipped so the room still opens, and `rimz doctor` names the install fix. An installed host with a fixable misconfiguration (a Claude older than remote control, `disableRemoteControl` set) refuses at start with the fix spelled out, so a toggle you left on does not quietly do nothing.

Claude asks `Enable Remote Control? (y/n)` the first time its host runs on a machine, and records that the dialog ran as `remoteDialogSeen` in `~/.claude.json`. Turning the toggle on is you answering that question, so RimZ writes the record before the host starts and an unattended pane serves immediately instead of holding on a prompt nobody is there to type into. Set `remoteDialogSeen` to `false` by hand and RimZ keeps your word: it leaves the value alone and refuses at start with the fix.

The RimZ toggle covers the machine-level Claude host, and Claude's own `remoteControlAtStartup: true`, in `~/.claude/settings.json`, additionally makes every session you type into a pane reachable from the app. RimZ lights the `⇅ rc` flag for either one, reading Claude's own record of the process serving each project root, so a host whose server stopped reads as down rather than riding on a pane that outlived it.

Codex's own updater owns daemon upgrades, and one of its passes can disconnect live Codex sessions once. `rimz doctor` warns while that is outstanding, and [troubleshooting](./troubleshooting.md#codex-sessions-drop-with-a-websocket-reset-about-hourly) covers choosing the timing yourself.

Undo is the same toggle set back to `false`: it closes the RimZ-managed Claude host panes in every running room, or stops the Codex daemon. One known gap: a session you spawn from the phone runs headless in its worktree with no local pane, and the sidebar does not render these remote agents.

What a live toggle does to a running room, and where the hosts sit in the daemon view, are in [configuration → remote control](./configuration.md#remote-control); which providers carry the surface at all is the `remote` row of the [wiring matrix](../reference/agent-support.md#the-wiring-matrix).

## See also

- [Web](./web.md): open the same room in a browser, locally or tunnelled from a server.
- [Agents](./fleet.md): what the room holds that you are reattaching to.
- [The sidebar](./sidebar.md): where the link badge sits, and how the cards you come back to are ranked.
- [Troubleshooting](./troubleshooting.md): a link that will not connect, a room that will not start, resetting state.
- [Remote CLI](../reference/cli/remote.md): every `rimz remote` flag, the target table, the alias fields, and the exit codes.
- [Configuration](./configuration.md): `remote.toml` and the remote-control keys.

# Remote

Remote is your multiplexer attach, run over SSH. A RimZ room is a plain Zellij or tmux session that lives on one host: a fleet on a server, or the room you left running on the machine at home. It keeps working headless while no one is attached. `rimz remote connect` opens an SSH session to that host and attaches to the room there, so your local terminal renders exactly what the host is running, sidebar and agent panes alike. The room, its agents, and its state never leave the host; SSH just carries the screen. To open a remote room in a browser instead of a terminal, see [Web](./web.md); to answer an agent's questions from the provider's mobile app without attaching at all, see [answer asks from your phone](#answer-asks-from-your-phone).

## Connect to a room on another host

Point `rimz remote connect` at a target and RimZ builds the SSH command, connects, and attaches to the room on the far side:

```sh
rimz remote connect dev-box:~/code/query-engine   # [user@]host:<session-or-path>
```

The target is `[user@]host:<session-or-path>`. After the colon, a path opens (or creates) that project's room, and a bare session name reaches one by name. Under the hood this is close to typing `ssh -t dev-box rimz attach query-engine` yourself: a normal SSH session that runs the host's own `rimz`, so your SSH config, keys, agent, ports, and jump hosts all apply exactly as for any `ssh` you run.

The connection warns when your local and remote RimZ versions differ, so you can upgrade the older side before their behavior drifts.

Because you attach to the session the host is already running, everything you left is there: every agent in its `#channel` tab, every question still ranked exactly where it was, plus whatever finished while you were gone, already triaged.

### Install rimz on the host

Run setup once before the first connect when the host does not already have `rimz` on its PATH:

```sh
rimz remote setup dev-box
rimz remote setup agent@prod-box:/srv/query-engine
rimz remote setup dev                           # saved alias
```

`rimz remote setup <alias-or-host>` opens SSH to the saved alias, raw target, or bare `[user@]host`, detects the host OS and architecture there, verifies the matching prebuilt release archive, and installs `rimz` to `~/.local/bin/rimz`. That directory is on the PATH that `remote connect` repairs, so the next attach can run immediately.

**Save an alias** once and the trip is one word. An alias carries the target and its reconnect defaults, so `rimz remote connect dev` is the whole journey:

```sh
rimz remote add dev dev-box:~/code/query-engine   # save the target as `dev`
rimz remote connect dev                           # open it, reconnecting link and all
rimz remote list                                  # every saved alias (alias: ls)
rimz remote rename dev devbox                     # rename an alias
rimz remote rm devbox                             # forget one
```

## A link that heals itself

A plain `ssh` ends the moment the connection drops. `rimz remote connect` supervises the SSH link instead: when the train wifi cuts out or a laptop sleeps, it reconnects on its own and reattaches to the untouched room on the host, so a flaky connection never costs you your place. After a brief grace, the recovery panel checks end-to-end internet access over HTTP, the SSH server, and the next SSH session; proxy-based targets omit the server checkpoint. With the network down it waits without a dishonest countdown, and any wifi, route, or address change triggers an immediate safety attempt. With the network up it establishes SSH in the background every couple of seconds, shows the last SSH error when one fails, and returns you straight to the room when transport and authentication succeed. Retry attempts run unattended, so hook installation, project trust, and recovery consent wait for the next start you make while present instead of parking the link. Your terminal beeps when the link drops and again when it comes back, and any notification handler fires on the same edges, because a dead link cannot count on the remote sidebar to reach you.

On Zellij, a reconnect retires an orphaned prior attach from the same device before entering the room; opening that same remote room twice from one device therefore moves the attachment to the newer terminal, while attachments from different devices coexist.

Two flags tune the posture:

```sh
rimz remote connect dev --no-reconnect   # one ssh run, no supervisor, no health probe
rimz remote connect dev --reset          # attach a fresh room (passes --no-resume through)
```

`rimz remote reset dev` is the shorthand for that last one. The link supervisor and its reconnect policy are in [the internals](../internals/remote.md).

## Reading the link badge

A supervised connection carries a health probe alongside your session: a small ping travels the same SSH link every couple of seconds, and its round trip drives a badge in the sidebar footer. The probe rides the real connection rather than ICMP, so the badge reflects what your session actually feels. A `--no-reconnect` run skips the probe and shows no badge.

- `⇄ remote 210ms` reads the round trip to the host. It warms from `⇄ remote …` on the first samples, then shades from green under roughly 100ms, through amber, to red past 400ms.
- `⇄ remote 210ms 15%` appends packet loss once it climbs past about 10 percent, measured as the share of recent pings that never came back. A clean link shows no percentage.
- `⇄ remote ?` means the reading went stale with no fresh sample for a while, usually a struggling link or a reconnect in flight.

The badge always shows the worse of latency and loss, so a fast but lossy link still reads red.

## What crosses the link

Only the screen crosses the wire. The agents, their transcripts, the git work, and the store all run on the host; SSH carries the multiplexer's rendered output one way and your keystrokes the other, the same bytes a local attach would paint. The multiplexer redraws only the focused tab and diffs each frame, and every attach turns on SSH compression, so an idle room is nearly silent and a busy pane costs about what watching it locally would.

To measure the actual traffic, `rimz remote bandwidth` samples the room and attributes the output rate per pane, with the compressed SSH wire-rate alongside:

```sh
rimz remote bandwidth --secs 5   # per-pane output and the SSH wire-rate
```

Run it on the Linux host serving the room, where the write-rate counters live, from inside the remote shell after you attach. The per-pane figures are raw producer output; the `WIRE(ssh)` rows are the compressed payload actually on the socket, normally far below the sum, because the multiplexer throttles to the focused tab before SSH compresses what is left.

## Continuity across reboots

The room and its state both live on the host, in durable flat files under `~/.local/state/rimz/` there, so the room survives a mux crash or a reboot of the host. On the next attended start the host's `rimz` offers the fleet back: prior agents idle in their tabs, one prompt from where they stopped (`claude --resume`, `codex resume`, `pi --session`). The offer defaults yes, non-interactive starts recover automatically, and a room you closed deliberately stays closed. A supervised reconnect after a reboot uses that automatic recovery path; hook installation and project-trust offers remain pending for the next attended start. Run these on the host:

```sh
rimz --no-resume         # come up empty: skip recovering prior agents
rimz reset               # force a clean rebirth of a stuck or resurrected room
rimz reset --hard        # rebuild without seeding prior agents
```

Keeping the agent processes alive across a reboot belongs to the host. Reach for systemd, tmux-resurrect, or Zellij resurrect to carry them across a restart, and RimZ reattaches to whatever is still running ([DESIGN.md → Non-goals](../../DESIGN.md#non-goals)).

## Answer asks from your phone

A fleet that runs while you are out still stops to ask: a permission prompt, a plan approval, a question only you can decide. Claude Code and Codex each ship **remote control** (`claude remote-control` and `codex remote-control`), the bridge behind their official mobile apps: it links a machine to your account so the app can see and drive the sessions running on it. The feature is entirely the provider's; what RimZ adds is the remembering — the bridge only helps if it is already up when an agent stops to ask, and starting infrastructure with the room is a room's job.

Two per-machine toggles opt in. Both are off by default:

```sh
rimz config set remote_control.claude true    # keep `claude remote-control` up with the room
rimz config set remote_control.codex true     # keep Codex's remote-control daemon up, once per machine
```

With a toggle on, the ask reaches your phone as a push from the provider's own app, your answer lands in the same session on the machine running the room, and the turn continues in its pane as if you had typed it there. Leave the room on a server, go to dinner, and a 9 p.m. question is one tap instead of a fleet stalled until morning.

RimZ stays out of the path: each toggle starts the provider's own command with the room and nothing more. Exactly what runs:

- **Claude.** `claude remote-control --spawn worktree`, as a long-lived pane in the room's background `rimzd` tab, run from the project root — so a session you start from the phone is carved into its own on-demand worktree instead of touching your checkout. While the host is up, the Claude block on the [provider dashboard](./sidebar.md#the-provider-dashboard) wears a `⇅ rc` flag.
- **Codex.** `codex remote-control start`, which brings up Codex's own per-user daemon with remote control enabled — one per machine, shared by every room, and the same daemon Codex's TUI already routes through.

Codex's standalone updater owns daemon upgrades. `rimz doctor` warns when that live updater executes a different binary from the managed Codex target, including a deleted executable, because its next hourly refresh can replace the shared app-server and disconnect active Codex sessions. Finish valuable turns, run `cd ~; codex remote-control stop; sleep 3; codex remote-control start` once, then resume the sessions this deliberate recycle disconnects; RimZ reports the skew but leaves its disruptive timing to you.

The RimZ toggle covers the machine-level Claude host; Claude's own `remoteControlAtStartup: true` (in `~/.claude/settings.json`) additionally makes every session you type into a pane reachable from the app, and RimZ lights the `⇅ rc` flag for that setting too.

`rimz start` checks the preconditions. An enabled toggle whose agent is missing is skipped so the room still opens, and `rimz doctor` names the install fix; an installed host with a fixable misconfiguration (a Claude older than remote control, `disableRemoteControl` set) refuses at start with the fix spelled out, so an enabled toggle always means a working bridge.

Undo is the same toggle set back to `false`: it closes the RimZ-managed Claude host panes in every running room, or stops the Codex daemon. One known gap: a session you spawn from the phone runs headless in its worktree with no local pane, and the sidebar does not yet render these remote agents.

Key detail and the daemon-view placement live in [configuration → remote control](./configuration.md#remote-control); which providers carry the surface is the `remote` row of the [coverage matrix](../reference/agent-support.md#the-coverage-matrix).

## See also

- [Web](./web.md) — open the same room in a browser, locally or tunnelled from a server.
- [Agents](./fleet.md) — what the room holds that you are reattaching to.
- [The sidebar](./sidebar.md) — reading the link-health badge and the recovered column.
- [Troubleshooting](./troubleshooting.md) — a link that will not connect, a room that will not start, resetting state.
- [CLI reference](../reference/cli/remote.md) · [Configuration](./configuration.md) — the `remote` command surface and `remote.toml`.

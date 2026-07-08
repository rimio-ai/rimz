# Remote

Remote is your multiplexer attach, run over SSH. A Rimz room is a plain Zellij or tmux session that lives on one host: a fleet on a server, or the room you left running on the machine at home. It keeps working headless while no one is attached. `rimz remote connect` opens an SSH session to that host and attaches to the room there, so your local terminal renders exactly what the host is running, sidebar and agent panes alike. The room, its agents, and its state never leave the host; SSH just carries the screen. To open a remote room in a browser instead of a terminal, see [Web](./web.md).

## Connect to a room on another host

Point `rimz remote connect` at a target and Rimz builds the SSH command, connects, and attaches to the room on the far side:

```sh
rimz remote connect dev-box:~/code/query-engine   # [user@]host:<session-or-path>
```

The target is `[user@]host:<session-or-path>`. After the colon, a path opens (or creates) that project's room, and a bare session name reaches one by name. Under the hood this is close to typing `ssh -t dev-box rimz attach query-engine` yourself: a normal SSH session that runs the host's own `rimz`, so your SSH config, keys, agent, ports, and jump hosts all apply exactly as for any `ssh` you run.

Because you attach to the session the host is already running, everything you left is there: every agent in its `#channel` tab, every question still ranked exactly where it was, plus whatever finished while you were gone, already triaged.

**Save an alias** once and the trip is one word. An alias carries the target and its reconnect defaults, so `rimz remote connect dev` is the whole journey:

```sh
rimz remote add dev dev-box:~/code/query-engine   # save the target as `dev`
rimz remote connect dev                           # open it, reconnecting link and all
rimz remote list                                  # every saved alias (alias: ls)
rimz remote rename dev devbox                     # rename an alias
rimz remote rm devbox                             # forget one
```

## A link that heals itself

A plain `ssh` ends the moment the connection drops. `rimz remote connect` supervises the SSH link instead: when the train wifi cuts out or a laptop sleeps, it reconnects on its own and reattaches to the untouched room on the host, so a flaky connection never costs you your place. Your terminal beeps when the link drops and again when it comes back, and any notification handler fires on the same edges, because a dead link cannot count on the remote sidebar to reach you.

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

The room and its state both live on the host, in durable flat files under `~/.local/state/rimz/` there, so the room survives a mux crash or a reboot of the host. On the next start the host's `rimz` offers the fleet back: prior agents idle in their tabs, one prompt from where they stopped (`claude --resume`, `codex resume`, `pi --session`). The offer defaults yes, non-interactive starts recover automatically, and a room you closed deliberately stays closed. Run these on the host:

```sh
rimz --no-resume         # come up empty: skip recovering prior agents
rimz reset               # force a clean rebirth of a stuck or resurrected room
rimz reset --hard        # rebuild without seeding prior agents
```

Keeping the agent processes alive across a reboot belongs to the host. Reach for systemd, tmux-resurrect, or Zellij resurrect to carry them across a restart, and Rimz reattaches to whatever is still running ([DESIGN.md → Non-goals](../../DESIGN.md#non-goals)).

## See also

- [Web](./web.md) — open the same room in a browser, locally or tunnelled from a server.
- [Quickstart](./quickstart.md) — the first session, including leaving and coming back.
- [Agents](./agents.md) — what the room holds that you are reattaching to.
- [The sidebar](./sidebar.md) — reading the link-health badge and the recovered column.
- [Troubleshooting](./troubleshooting.md) — a link that will not connect, a room that will not start, resetting state.
- [CLI reference](../reference/cli/getting-started.md) · [Configuration](../reference/configuration.md) — the `remote` command surface and `remote.toml`.

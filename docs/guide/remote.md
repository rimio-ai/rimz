# Remote

A Rimz room is a plain Zellij or tmux session, so it travels the way any terminal session does: start it on your laptop or a server, detach, and pick it up from anywhere. Over SSH the link heals itself when the connection drops, and the room keeps running headless while nobody renders. The sidebar rebuilds from durable state the moment you reattach: every agent where you left it, every pending question still waiting. To open a room in a browser instead of a terminal, see [Web](./web.md).

## Reattach on the same machine

Detach with your multiplexer's own key: Zellij `Ctrl-O d`, tmux `prefix d`. The room stays alive, agents working and their events still recorded while no sidebar renders.

Coming back is the command you already know — `rimz` in the project directory returns to that project's room. From anywhere else, name the session:

```sh
rimz                     # reattach to the room for the current project
rimz list                # every known room and which mux runs it
rimz list --all          # include dormant rooms, not just the last 24h
rimz attach query-engine # reach one by session name from any directory
```

`rimz attach` enters the session by default; `--print` (alias `--no-attach`) prints the attach command instead, for a script or a wrapper. `--no-resume` brings the room up empty instead of recovering prior agents.

## Connect to a room on another host

Point `rimz remote connect` at a target and Rimz opens the room over SSH, reconstructing the sidebar from the remote store:

```sh
rimz remote connect dev-box:~/code/query-engine   # user@host:<session-or-path>
```

The target is `[user@]host:<session-or-path>` — a path opens (or creates) that project's room, a bare session name reaches one by name. Everything you left is there: every agent in its `#channel` tab, every question still ranked exactly where it was, plus whatever finished while you were gone, already triaged.

**Save an alias** once and the trip is one word. An alias carries the target and its reconnect defaults, so `rimz remote connect dev` is the whole journey:

```sh
rimz remote add dev dev-box:~/code/query-engine   # save the target as `dev`
rimz remote connect dev                           # open it, reconnecting link and all
rimz remote list                                  # every saved alias (alias: ls)
rimz remote rename dev devbox                      # rename an alias
rimz remote rm devbox                              # forget one
```

## A link that heals itself

`rimz remote connect` supervises the SSH link and reconnects on its own when the train wifi drops or a laptop sleeps, so a flaky connection never costs you the room. The sidebar footer reads link health at a glance — a `⇄ remote 210ms` badge shows the round trip, and the badge reports when the link is reconnecting.

The link is plain SSH: bring your own keys and agent, exactly as for any `ssh` you run. Two flags tune the posture:

```sh
rimz remote connect dev --no-reconnect   # a single ssh run, no supervisor
rimz remote connect dev --reset          # a fresh remote room (passes --no-resume through)
```

`rimz remote reset dev` is the shorthand for that last one. The link supervisor and its reconnect policy are in [the internals](../internals/remote.md).

## Continuity survives reboots; keeping processes alive is the host's job

The store is a directory of flat files under `~/.local/state/rimz/`, written durably, so continuity survives a reboot or a mux crash. On the next start Rimz offers the fleet back: prior agents idle in their tabs, one prompt from where they stopped (`claude --resume`, `codex resume`, `pi --session`). The offer defaults yes, non-interactive starts recover automatically, and a room you closed deliberately stays closed.

```sh
rimz --no-resume         # come up empty: skip recovering prior agents
rimz reset               # force a clean rebirth of a stuck or resurrected room
rimz reset --hard        # rebuild without seeding prior agents
```

Keeping the processes themselves alive across a reboot is the host's job, not Rimz's — reach for systemd, tmux-resurrect, or Zellij resurrect for that, and Rimz reattaches to whatever is still running ([DESIGN.md → Non-goals](../../DESIGN.md#non-goals)).

## See also

- [Web](./web.md) — open the same room in a browser, locally or tunnelled from a server.
- [Quickstart](./quickstart.md) — the first session, including leaving and coming back.
- [Agents](./agents.md) — what the room holds that you are reattaching to.
- [The sidebar](./sidebar.md) — reading the link-health badge and the recovered column.
- [Troubleshooting](./troubleshooting.md) — a link that will not connect, a room that will not start, resetting state.
- [CLI reference](../reference/cli/getting-started.md) · [Configuration](../reference/configuration.md) — the `remote` command surface and `remote.toml`.

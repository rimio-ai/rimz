# Remote CLI

`rimz remote` attaches to a room on another host over SSH. The local `rimz` builds an `ssh` command and runs the host's own `rimz` at the far end, so your `~/.ssh/config`, keys, ports, and jump hosts apply as they do for any `ssh`. The room, its agents, and its store stay on the host. Only `remote setup` writes files there outside the room; saved aliases live on your machine in `~/.rimz/remote.toml` (under `$RIMZ_HOME` when it is set). Why you attach this way, the reconnecting link, and the link badge are in the [remote guide](../../guide/remote.md).

```sh
rimz remote connect <alias-or-target> [--reset] [--no-reconnect] [--force-version] [--no-auto-forward] [--web [--web-port <port>]] [--attach | --no-attach | --print]
rimz remote reset <alias-or-target> [same flags as connect, without --reset]
rimz remote setup <alias-or-target-or-host>
rimz remote add <name> <target> [--no-reconnect] [--no-resume] [--no-auto-forward]
rimz remote update <name> <target> [--no-reconnect] [--no-resume] [--no-auto-forward]
rimz remote rename <old> <new>
rimz remote rm <name>
rimz remote list [--json]            # alias: ls
```

Every subcommand also takes the [global flags](../cli.md#global-flags). `--mux`, `--zellij`, and `--tmux` matter on `connect`, `reset`, `add`, and `update` only.

## Targets

A target is `[user@]host:<session-or-path>`, spelled like an `scp` destination. The host part is a hostname, an address, or a `Host` alias from your SSH config; write an IPv6 address in brackets (`user@[::1]:query-engine`). The suffix after the colon decides what the host's `rimz` runs:

| Suffix | Example | What runs on the host |
| --- | --- | --- |
| `session:<name>` | `dev-box:session:query-engine` | `rimz attach` to the session by that name, even when a directory has the same name. |
| Contains `/` or starts with `~` | `dev-box:~/code/query-engine`, `agent@prod-box:/srv/query-engine`, `dev-box:./query-engine` | `rimz start` for that directory. A missing directory fails before any room is born. |
| Anything else | `dev-box:query-engine`, `dev-box:.agents` | `rimz start` when a directory by that name exists under the remote `HOME`, otherwise `rimz attach` to the session by that name. |

Relative paths are anchored to the remote `HOME`, whatever directory the SSH login starts in. `~` and `~/…` expand to the remote `HOME`, but `~user` is rejected: write another user's home as an absolute path (`/home/alice/code`). The rules are the same for raw targets and saved aliases, for terminal and browser connections. The last row is decided again on every connect and reconnect, so use an explicit `session:` or path suffix when a directory and a session share a name.

## Connect to a remote room

`rimz remote connect` takes a saved alias or a raw target. An argument containing `:` is a raw target; anything else is looked up as an alias and fails with `no such remote alias` when none matches.

```sh
rimz remote connect dev                                  # a saved alias
rimz remote connect agent@prod-box:/srv/query-engine     # a raw target
rimz remote connect dev --no-reconnect                   # one ssh run, no supervisor
rimz remote reset dev                                    # same as connect --reset
```

By default the connection is supervised: RimZ keeps an SSH ControlMaster open, reconnects after a drop, measures the link for the sidebar badge, and forwards new dev-server ports. It ends when you detach or the remote room closes. `--no-reconnect` hands the terminal to a single `ssh` run with none of that.

| Flag | Effect |
| --- | --- |
| `--reset` | Pass `--no-resume` to the host's `rimz`: a room born by this connect comes up without its prior agents. A room that is already running is attached as it is. `rimz remote reset` is the same command. |
| `--no-reconnect` | Run `ssh` once, with no reconnect, link badge, or port forwarding. |
| `--force-version` | Attach despite a minor version difference with the host, for this invocation only. See [Version checks](#version-checks). |
| `--no-auto-forward` | Do not forward new remote listeners for this connection. |
| `--web` | Open the room in your local browser instead of this terminal. See [Open a remote room in the browser](#open-a-remote-room-in-the-browser). |
| `--web-port <port>` | The local browser port for `--web`. Requires `--web`. |
| `--attach` | Connect even when stdin or stdout is not a terminal. |
| `--no-attach`, `--print` | Print the `ssh` command instead of running it, to inspect or wrap it. |
| `--mux <zellij\|tmux>`, `--zellij`, `--tmux` | Backend the host's `rimz` uses, overriding the alias's saved `mux`. |

Without `--attach` or `--print`, `connect` runs `ssh` only when both stdin and stdout are terminals; otherwise it prints the command, as `--print` does.

A flag on the command line can only switch a saved default off. An alias saved with `--no-reconnect`, `--no-resume`, or `--no-auto-forward` keeps that setting on every connect; change it with [`remote update`](#saved-aliases).

### Version checks

The host's `rimz` compares its version with yours before it enters the room:

| Difference | Result |
| --- | --- |
| Patch, or a version that does not parse | A warning, then the room. |
| Minor | Refused. The error tells you to upgrade the older side (`rimz remote setup` upgrades the host) or retry with `--force-version`, which turns the refusal into a warning. |
| Major | Refused, with the same upgrade fix. `--force-version` does not apply. |

`--force-version` is never saved on an alias.

### Port forwarding

A supervised terminal connection forwards a remote dev server to the same port on your machine when the server starts listening after you attach. The listener must belong to your remote user, use port 1024 or above, and bind a loopback or wildcard address. The local end binds only `127.0.0.1`, and a local port already in use is skipped. Discovery reads the host's `/proc`, so it works on Linux hosts only. `--no-reconnect` and `--web` connections never forward. The guide walks through it in [Ports forward themselves](../../guide/remote.md#ports-forward-themselves); limits and timing are in [remote internals](../../internals/remote.md#port-auto-forwarding).

### Exit codes

A supervised `connect` exits `0` when you detach or the remote room ends, and `1` with the fix on stderr when it cannot continue: `rimz` missing on the host (the message names `rimz remote setup`), an explicit path that does not exist, or a refused version.

With `--no-reconnect`, `connect` exits with the `ssh` run's status, so the host's refusals come through as their own codes:

| Code | Meaning |
| --- | --- |
| `65` | Minor version difference, refused. |
| `66` | Major version difference, refused. |
| `67` | The explicit path does not exist on the host. |
| `127` | `rimz` is not on the host's `PATH`. Run `rimz remote setup`. |
| `255` | `ssh` could not connect or the connection dropped. |

## Saved aliases

An alias names a target and its connection defaults, so `rimz remote connect dev` is the whole command.

```sh
rimz remote add dev dev-box:~/code/query-engine --tmux    # save target and backend
rimz remote update dev dev-box:~/code/query-engine        # replace; unset flags return to defaults
rimz remote rename dev devbox
rimz remote rm devbox
```

| Subcommand | Effect |
| --- | --- |
| `add <name> <target>` | Save a new alias. When the name is taken and stdin is a terminal, it asks whether to update the existing alias; without a terminal, or when you decline, it fails with `remote alias ... already exists`. Use `update` in scripts. |
| `update <name> <target>` | Replace an existing alias's target and settings. Settings you do not pass, `mux` included, return to their defaults. Fails when the alias does not exist. |
| `rename <old> <new>` | Rename an alias. Fails when `<old>` does not exist or `<new>` is taken. |
| `rm <name>` | Delete an alias. Fails when it does not exist. |
| `list` | Print aliases grouped by SSH destination, each with its full target. |

None of these contact the host. `add` and `update` check the name and parse the target before saving. Each alias is one `[[remote]]` table in `remote.toml`:

| Field | Default | Set by | Meaning |
| --- | --- | --- | --- |
| `name` | required | `<name>` | 1 to 64 ASCII letters, digits, `-`, or `_`, not starting with `-`. |
| `target` | required | `<target>` | A [target](#targets). |
| `reconnect` | `true` | `--no-reconnect` saves `false` | Supervise and reconnect the link. |
| `no_resume` | `false` | `--no-resume` saves `true` | Pass `--no-resume` when a connect births the room. |
| `mux` | unset | `--mux`, `--zellij`, or `--tmux` | Backend the host's `rimz` uses. |
| `auto_forward` | `true` | `--no-auto-forward` saves `false` | Forward new remote listeners. |

`remote list` prints one section per SSH destination (`[user@]host`), so the same host under two users gives two sections. Sections and the aliases in them sort by name; an alias whose target no longer parses appears first, under `Invalid targets`. With no aliases, it prints nothing.

```console
$ rimz remote list
  NAME  TARGET                              RECONNECT     RESUME     MUX   FORWARD

agent@prod-box
  prod  agent@prod-box:~/code/query-engine  no-reconnect  no-resume  tmux  off

dev-box
  dev   dev-box:query-engine                reconnect     resume     -     auto
```

`remote list --json` prints every alias in one flat array:

```json
{
  "remotes": [
    {
      "auto_forward": true,
      "mux": null,
      "name": "dev",
      "no_resume": false,
      "reconnect": true,
      "target": "dev-box:query-engine"
    }
  ]
}
```

## Install rimz on the host

`rimz remote setup` installs the latest RimZ release on a host over SSH. It takes a saved alias, a raw target, or a bare `[user@]host`, and needs only `ssh` locally.

```sh
rimz remote setup dev-box
```

On the host it runs these steps, and stops at the first that fails:

1. Detect the platform with `uname`. Prebuilt releases exist for Linux x86_64 and macOS (arm64 and x86_64); any other platform fails with `no prebuilt for <os>/<arch>`.
2. Download the release archive and `SHA256SUMS` from the project's latest GitHub release with `curl`, into a temporary directory.
3. Check the archive against `SHA256SUMS`.
4. Install the binary to `~/.local/bin/rimz` with mode `0755`, replacing any binary there, and run `rimz --version`.

On success it prints `rimz installed on <host>` and the `rimz remote connect` command to run next. A failure exits `1` and points at the [installation guide](../../guide/installation.md). Because it installs the latest release, `setup` is also how you upgrade the host after a version refusal. `connect` adds `~/.local/bin` to the `PATH` it runs `rimz` with, so the next connect finds the binary.

## Open a remote room in the browser

`rimz remote connect <alias-or-target> --web` opens the remote room in your local browser and stays in the foreground until Ctrl-C.

```console
$ rimz remote connect dev --web
http://127.0.0.1:8368/?room=rimz-query-engine-a1b2c3
rimz: tunnel up — reconnects automatically; Ctrl-C stops
```

It runs these steps:

1. Run `rimz web open --print --json` on the host, which births the room if needed and ensures the host's shared browser daemon. When your terminal is interactive, the host asks its recovery question here.
2. Bind a local browser port on `127.0.0.1`. Without `--web-port`, the port comes from the session name in `8300..8399`, so a room keeps the same URL between runs; a taken port moves to the next free one. With `--web-port`, that exact port is used and a taken port fails.
3. Forward a second, ephemeral local port to the host's daemon over SSH.
4. Relay browser traffic from the browser port to the forward, adding the host's Basic credential to each request. The credential is never printed and the browser shows no password prompt.
5. Print the URL on stdout, open your browser best-effort, and print `rimz: tunnel up` on stderr.

A supervised `--web` connection repeats steps 1 and 3 after a dropped link, prints `rimz: tunnel to <host> restored`, and keeps the same URL. With `--no-reconnect`, the command exits when the tunnel drops. `--web` does not forward dev-server ports, and it cannot be combined with `--print` or run without a terminal unless you pass `--attach`; both fail with `--web is web-only and has no SSH attach command; drop --print`.

The browser room itself, and the host's `[web]` settings it needs, are [`rimz web`](./web.md). The relay and tunnel mechanics are in [web internals](../../internals/web.md#remote-rooms) and [remote internals](../../internals/remote.md#web-tunnels).

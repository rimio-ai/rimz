# Remote CLI

`rimz remote` attaches to a room on another host over SSH. The local `rimz` builds an `ssh` command and runs the host's own `rimz` at the far end, so your `~/.ssh/config`, keys, ports, and jump hosts apply as they do for any `ssh`. The room, its agents, and its store stay on the host. Only `remote setup` writes files there outside the room; saved aliases live on your machine in `~/.rimz/remote.toml` (under `$RIMZ_HOME` when it is set). Why you attach this way, the reconnecting link, and the link badge are in the [remote guide](../../guide/remote.md).

```sh
rimz remote connect <alias-or-target> [--resume | --no-resume] [--reconnect | --no-reconnect] [--auto-forward | --no-auto-forward] [--force-version] [--web [--web-port <port>]] [--attach | --no-attach | --print]
rimz remote setup <alias-or-target-or-host> [--latest]
rimz remote add <name> <target> [--resume | --no-resume] [--reconnect | --no-reconnect] [--auto-forward | --no-auto-forward]
rimz remote update <name> [target] [same pairs as add] [--no-mux]
rimz remote rename <old> <new>
rimz remote rm <name>
rimz remote list [--json]            # alias: ls
```

Every subcommand also takes the [global flags](../cli.md#global-flags). `--mux`, `--zellij`, and `--tmux` matter on `connect`, `add`, and `update` only.

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
rimz remote connect dev --no-resume                      # do not recover prior agents
```

By default the connection is supervised: RimZ keeps an SSH ControlMaster open, reconnects after a drop, measures the link for the sidebar badge, and forwards new dev-server ports. It ends when you detach or the remote room ends. `--no-reconnect` hands the terminal to a single `ssh` run with none of that.

| Flag | Effect |
| --- | --- |
| `--resume`, `--no-resume` | Recover prior agents, or pass `--no-resume` to the host: a newly born room comes up without prior agents; an already-running room's parked agents are ended. The host reports the count, or that nothing changed. |
| `--reconnect`, `--no-reconnect` | Supervise the link, or run `ssh` once with no reconnect, link badge, or port forwarding. |
| `--force-version` | Attach despite a minor version difference with the host, for this invocation only. See [Version checks](#version-checks). |
| `--auto-forward`, `--no-auto-forward` | Enable or disable forwarding new remote listeners for this connection. |
| `--web` | Open the room in your local browser instead of this terminal. See [Open a remote room in the browser](#open-a-remote-room-in-the-browser). |
| `--web-port <port>` | The local browser port for `--web`, by default derived from the session name in `8300..8399`. Requires `--web`. |
| `--attach` | Connect even when stdin or stdout is not a terminal. |
| `--no-attach`, `--print` | Print the `ssh` command instead of running it, to inspect or wrap it. |
| `--mux <zellij\|tmux>`, `--zellij`, `--tmux` | Backend the host's `rimz` uses, overriding the alias's saved `mux`. |

Without `--attach` or `--print`, `connect` runs `ssh` only when both stdin and stdout are terminals; otherwise it prints the command on stdout and one notice on stderr explaining that nothing was connected and suggesting `--attach`. Explicit `--print` or `--no-attach` adds no fallback notice. The printed command runs directly in a shell without a local reconnect supervisor or an outer terminal bracket. `--web` has no command to print: without a terminal, pass `--attach` to run the tunnel; with `--print`, drop that flag.

Each setting starts from the saved alias (or `true` for a raw target). An explicit positive or negative flag overrides it for this invocation; within each pair the last flag wins. Use [`remote update`](#saved-aliases) to save a change. An unknown alias error suggests a nearby name, lists up to eight saved names, and shows how to connect directly to a host.

### Version checks

The host's `rimz` compares its version with yours before it enters the room:

| Difference | Result |
| --- | --- |
| Patch, or a version that does not parse | A warning, then the room. |
| Minor | Refused. The error tells you to upgrade the older side (`rimz remote setup` upgrades the host) or retry with `--force-version`, which turns the refusal into a warning. |
| Major | Refused, with the same upgrade fix. `--force-version` does not apply. |

`--force-version` is never saved on an alias.

### Port forwarding

A supervised terminal connection forwards a remote dev server to the same port on your machine when the server starts listening after you attach. The listener must belong to your remote user, use port 1024 or above, and bind a loopback or wildcard address. The local end binds only `127.0.0.1`, and a local port already in use is skipped. Discovery reads the host's `/proc`, so it works on Linux hosts only. `--no-reconnect` and `--web` connections never forward.

Two ceilings bound a busy host: discovery keeps the 32 lowest qualifying ports, and at most 16 forwards stay open at once. A port skipped because the local end was busy is retried when the remote listener stops and starts again, not when you reconnect, so free the local port before you restart the remote server. The guide walks through it in [Ports forward themselves](../../guide/remote.md#ports-forward-themselves); limits and timing are in [remote internals](../../internals/remote.md#port-auto-forwarding).

### Exit codes

A supervised `connect` exits `0` when you detach or the remote room ends. A fatal SSH run preserves its exit status and prints the fix on stderr; a local path preflight refusal exits `67`. Signal death and other local errors exit `1`.

With or without `--no-reconnect`, the host's refusals come through as their own codes. Browser preparation also preserves the known host refusal codes below; other preparation failures exit `1`.

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
rimz remote update dev dev-box:~/code/query-engine        # change target, keep settings
rimz remote update dev --resume --no-mux                  # enable recovery, clear backend pin
rimz remote rename dev devbox
rimz remote rm devbox
```

| Subcommand | Effect |
| --- | --- |
| `add <name> <target>` | Save a new alias. When the name is taken and stdin is a terminal, it asks whether to update the existing alias; without a terminal, or when you decline, it fails with `remote alias ... already exists`. Use `update` in scripts. |
| `update <name> [target]` | Merge explicit settings and an optional target into an existing alias. Omitted values stay unchanged. `--no-mux` clears the pin and conflicts with backend flags. Fails when the alias does not exist. |
| `rename <old> <new>` | Rename an alias. Fails when `<old>` does not exist or `<new>` is taken. |
| `rm <name>` | Delete an alias. Fails when it does not exist. |
| `list` | Print aliases grouped by SSH destination, each with its full target. |

None of these contact the host. `add` and `update` check the name and parse the target before saving. `add` defines a whole alias from defaults, including when you confirm replacing an existing one; without a terminal, a duplicate names `rimz remote update <name> <target>` as the fix. Successful writes print one stdout receipt: `saved <name> -> <target>; connect: rimz remote connect <name>`, `updated <name> -> <target>`, `renamed <old> -> <new>`, or `removed <name> (was <target>)`. Saved and updated targets gain parenthesized non-default settings using the same words as `list`. Each alias is one `[[remote]]` table in `remote.toml`:

| Field | Default | Set by | Meaning |
| --- | --- | --- | --- |
| `name` | required | `<name>` | 1 to 64 ASCII letters, digits, `-`, or `_`, not starting with `-`. |
| `target` | required | `<target>` | A [target](#targets). |
| `reconnect` | `true` | `--reconnect`, `--no-reconnect` | Supervise and reconnect the link. |
| `resume` | `true` | `--resume`, `--no-resume` | `false` passes `--no-resume` to the host. |
| `mux` | unset | `--mux`, `--zellij`, or `--tmux` | Backend the host's `rimz` uses. |
| `auto_forward` | `true` | `--auto-forward`, `--no-auto-forward` | Forward new remote listeners. |

Unknown alias fields, including the old `no_resume` key, fail the load rather than being ignored.

`remote list` prints one section per SSH destination (`[user@]host`), so the same host under two users gives two sections. Sections and the aliases in them sort by name; an alias whose target no longer parses appears first, under `Invalid targets`. Rows show the full target and only non-default settings, ordered as backend, `no-reconnect`, `no-resume`, `no-auto-forward`. With no aliases, it prints `no saved remotes; save one with: rimz remote add <name> [user@]host:<session-or-path>`.

```console
$ rimz remote list

agent@prod-box
  prod  agent@prod-box:~/code/query-engine  tmux, no-reconnect, no-resume, no-auto-forward

dev-box
  dev  dev-box:query-engine
```

`remote list --json` prints every alias in one flat array:

```json
{
  "remotes": [
    {
      "auto_forward": true,
      "mux": null,
      "name": "dev",
      "reconnect": true,
      "resume": true,
      "target": "dev-box:query-engine"
    }
  ]
}
```

## Install rimz on the host

`rimz remote setup` installs this client's release on a host over SSH, dropping any prerelease or build suffix from the version to select its release tag. `--latest` selects the newest release instead. It takes a saved alias, a raw target, or a bare `[user@]host`, and needs only `ssh` locally.

```sh
rimz remote setup dev-box
```

On the host it runs these steps, and stops at the first that fails:

1. Detect the platform with `uname`. Prebuilt releases exist for Linux x86_64 and macOS (arm64 and x86_64); any other platform fails with `no prebuilt for <os>/<arch>`.
2. Download the release archive and `SHA256SUMS` from `releases/download/v<X.Y.Z>/` with `curl`, into a temporary directory (`releases/latest/download/` with `--latest`).
3. Check the archive against `SHA256SUMS`.
4. Install the binary to `~/.local/bin/rimz` with mode `0755`, replacing any binary there, and run `rimz --version`.

Before connecting, stderr names the version being installed (or the latest release). On success the host prints `rimz <version> installed on <host> at ~/.local/bin/rimz`, using the installed binary's version, and the client names the `rimz remote connect` command to run next. A failure exits `1` and points at the [installation guide](../../guide/installation.md). If the pinned release does not exist (a development build ahead of releases), the download error names the missing tag and suggests rerunning with `--latest`. The default pin matches the host to your client after a version refusal. `connect` adds `~/.local/bin` to the `PATH` it runs `rimz` with, so the next connect finds the binary.

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

A supervised `--web` connection repeats steps 1 and 3 after a dropped link, prints `rimz: tunnel to <host> restored`, and keeps the same URL. With `--no-reconnect`, the command exits when the tunnel drops. `--web` does not forward dev-server ports. It refuses `--print` because a browser tunnel has no SSH command to print, and asks you to drop that flag. Without a terminal it instead asks you to pass `--attach` to run the tunnel anyway.

The browser room itself, and the host's `[web]` settings it needs, are [`rimz web`](./web.md). The relay and tunnel mechanics are in [web internals](../../internals/web.md#remote-rooms) and [remote internals](../../internals/remote.md#web-tunnels).

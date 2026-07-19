# Remote CLI

`rimz remote` attaches to a room on another host over SSH. `remote connect` builds a guarded `ssh -t` command on your machine and runs the remote host's own `rimz`, so your `~/.ssh/config`, keys, ports, and jump hosts apply through normal SSH resolution — RimZ adds no daemon and opens no port of its own. It changes nothing on the remote host until you ask: `remote setup` is the only subcommand that installs a binary there, and aliases are plain lines in `~/.config/rimz/remote.toml` you remove with `remote rm`. Why you attach this way, and how the link heals itself, is the [remote guide](../../guide/remote.md).

## Remote rooms

```sh
rimz remote add dev-box dev-box:query-engine     # save an alias in remote.toml
rimz remote setup dev-box                        # install rimz on the remote host
rimz remote connect dev-box                      # attach the saved room over SSH
rimz remote connect dev-box --web                # open the remote room's web UI locally
rimz remote connect dev-box --forward 3000       # browse a host dev server on localhost:3000
rimz remote connect dev-box --force-version      # bypass one minor version mismatch
rimz remote connect agent@prod-box:/srv/query-engine
rimz remote bandwidth --secs 5                   # attribute pane write-rate in this room
```

A raw target is `[user@]host:<session-or-path>`. After the colon, a value containing `/` or starting with `~` is a remote path and runs remote `rimz start`; a bare word is a remote session name and runs remote `rimz attach`. Valid targets include `dev-box:query-engine`, `dev-box:~/code/query-engine`, `agent@prod-box:/srv/query-engine`, and `user@[::1]:query-engine`. Spell another user's home as an absolute path (`/home/alice/code`), because `~user` does not expand through the guarded command.

| Subcommand | Effect |
| --- | --- |
| `remote connect <alias-or-target>` | Attach the room over SSH, reconnect-supervised |
| `remote setup <alias-or-host>` | Install `rimz` to `~/.local/bin` on the host over SSH |
| `remote add <name> <target>` | Save an alias in `~/.config/rimz/remote.toml` |
| `remote update <name> <target>` | Replace a saved alias's target and flags |
| `remote rename <old> <new>` | Rename a saved alias |
| `remote list` | Print saved aliases |
| `remote rm <name>` | Remove a saved alias |
| `remote reset <alias-or-target>` | Connect with recovery skipped, so the remote room comes up empty |
| `remote bandwidth` | Attribute pane write-rate inside a served room |

The details that matter in practice:

- `remote add` treats any input with a `:` as a raw target and everything else as an alias name. On an existing name it prompts to overwrite in an interactive terminal and errors otherwise, so a saved alias is never silently replaced; use `remote update` in a script. `update` takes the same flags as `add`, errors when the alias does not exist, and resets flags you do not pass to their defaults.
- `remote setup <alias-or-host>` accepts a saved alias, a raw `[user@]host:<session-or-path>` target, or a bare `[user@]host`, then installs the verified prebuilt release to `~/.local/bin/rimz` on that host. This is the only remote subcommand that writes to the remote host outside a room. When `remote connect` or `remote connect --web` finds no remote binary, the local error points back to this command.
- Reconnect supervision is on by default. `--no-reconnect` hands the link to one SSH run; `remote add --no-reconnect` saves that as the alias default.
- `remote connect --reset` and `remote reset` pass `--no-resume` to the remote `rimz`; `remote add --no-resume` saves that birth behavior on the alias.
- `remote connect --force-version` and `remote reset --force-version` attach despite a minor client/host version difference, including with `--web`. The bypass applies to that invocation only, is not saved by `remote add` or `remote update`, and does not bypass a major difference.
- `--attach`, `--no-attach`, and `--print` mirror local behavior; `--print` emits the SSH command instead of running it, so you can inspect or wrap it.
- `--forward <[LOCAL:]REMOTE>` (short `-L`) repeats and opens loopback-only local forwards: `3000` keeps the same port, while `8080:3000` maps local 8080 to remote 3000. `remote add` and `remote update` persist their forward values on an alias, `remote connect` adds per-invocation values, and a busy local port refuses before SSH starts. With `--print`, RimZ skips the bind check and emits the corresponding `-L` arguments.
- For `remote add` and `remote update`, `--mux`, `--zellij`, or `--tmux` given anywhere on the invocation is saved on the alias; `rimz remote connect --mux <name>` keeps `--mux` as a per-invocation override.
- `rimz remote bandwidth [--secs N] [--json]` runs on the Linux host serving the room and samples VFS write-rate counters to attribute per-pane terminal output on both backends; tmux reports pane pids natively, and Zellij pane pids resolve through RimZ's process matcher. Use it inside the room when a remote attach looks chatty; full-screen TUIs such as agents mid-turn or system monitors should dominate the report.

## Remote rooms in the browser

`rimz remote connect <target> --web` opens the remote room in your local browser instead of your terminal. The sequence:

1. Runs remote `rimz web open --print --json` over a prep connection, asking the recovery prompt there when your terminal is interactive.
2. Selects Zellij or ttyd from the remote room mux and relays its login token or Basic-Auth credential.
3. Starts a supervised SSH local-forward tunnel to the selected remote web engine.
4. Prints the bare `http://127.0.0.1:<port>/<session>` URL and opens your local browser best-effort.
5. Stays in the foreground until Ctrl-C, which tears the tunnel down.

`--web-port <port>` pins the local browser origin; otherwise RimZ derives a stable port from the session name in `8300..8399`. The room itself is [`rimz web`](./web.md).

Additional `--forward` values ride the same supervised web tunnel and close with it.

Link health, web tunneling, reconnect mechanics, and bandwidth attribution are in [remote internals](../../internals/remote.md).

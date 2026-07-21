# Remote CLI

`rimz remote` attaches to a room on another host over SSH. `remote connect` builds a guarded `ssh -t` command on your machine and runs the remote host's own `rimz`, so your `~/.ssh/config`, keys, ports, and jump hosts apply through normal SSH resolution. An ordinary terminal attach adds no daemon; `--web` ensures the remote host's shared ttyd daemon. A supervised connection opens loopback-only local forwards when it detects new remote dev-server listeners. It changes nothing on the remote host until you ask: `remote setup` is the only subcommand that installs a binary there, and aliases are plain lines in `~/.config/rimz/remote.toml` you remove with `remote rm`. Why you attach this way, and how the link heals itself, is the [remote guide](../../guide/remote.md).

## Remote rooms

```sh
rimz remote add dev-box dev-box:query-engine     # save an alias in remote.toml
rimz remote setup dev-box                        # install rimz on the remote host
rimz remote connect dev-box                      # attach the saved room over SSH
rimz remote connect dev-box --web                # open the remote room's web UI locally
rimz remote connect dev-box --force-version      # bypass one minor version mismatch
rimz remote connect agent@prod-box:/srv/query-engine
```

A raw target is `[user@]host:<session-or-path>`. After the colon, a value containing `/` or starting with `~` is a remote path and runs remote `rimz start`; a bare word is a remote session name and runs remote `rimz attach`. Valid targets include `dev-box:query-engine`, `dev-box:~/code/query-engine`, `agent@prod-box:/srv/query-engine`, and `user@[::1]:query-engine`. Spell another user's home as an absolute path (`/home/alice/code`), because `~user` does not expand through the guarded command. A supervised terminal connect also forwards qualifying listeners that start after attach to the same local port.

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

The details that matter in practice:

- `remote add` treats any input with a `:` as a raw target and everything else as an alias name. On an existing name it prompts to overwrite in an interactive terminal and errors otherwise, so a saved alias is never silently replaced; use `remote update` in a script. `update` takes the same flags as `add`, errors when the alias does not exist, and resets flags you do not pass to their defaults. `--no-auto-forward` persists as `auto_forward = false` on the alias.
- `remote setup <alias-or-host>` accepts a saved alias, a raw `[user@]host:<session-or-path>` target, or a bare `[user@]host`, then installs the verified prebuilt release to `~/.local/bin/rimz` on that host. This is the only remote subcommand that writes to the remote host outside a room. When `remote connect` or `remote connect --web` finds no remote binary, the local error points back to this command.
- Reconnect supervision is on by default. `--no-reconnect` hands the link to one SSH run; `remote add --no-reconnect` saves that as the alias default.
- `remote connect --reset` and `remote reset` pass `--no-resume` to the remote `rimz`; `remote add --no-resume` saves that birth behavior on the alias.
- `remote connect --force-version` and `remote reset --force-version` attach despite a minor client/host version difference, including with `--web`. The bypass applies to that invocation only, is not saved by `remote add` or `remote update`, and does not bypass a major difference.
- `--attach`, `--no-attach`, and `--print` mirror local behavior; `--print` emits the SSH command instead of running it, so you can inspect or wrap it.
- Automatic port forwarding is on by default for supervised terminal connections. `remote connect --no-auto-forward` and `remote reset --no-auto-forward` disable it once; `remote add --no-auto-forward` and `remote update --no-auto-forward` save the disabled default. `--no-reconnect` and `--web` have no ControlMaster probe channel, so they do not auto-forward.
- For `remote add` and `remote update`, `--mux`, `--zellij`, or `--tmux` given anywhere on the invocation is saved on the alias; `rimz remote connect --mux <name>` keeps `--mux` as a per-invocation override.

## Remote rooms in the browser

`rimz remote connect <target> --web` opens the remote room in your local browser instead of your terminal. The sequence:

1. Runs remote `rimz web open --print --json` over a prep connection, asking the recovery prompt there when your terminal is interactive.
2. Reads the shared ttyd port and Basic-Auth credential from that prep response and prints the credential on stderr.
3. Starts a supervised SSH local-forward tunnel to the remote ttyd port.
4. Prints the bare `http://127.0.0.1:<port>/?arg=<session>` URL and opens your local browser best-effort.
5. Stays in the foreground until Ctrl-C, which tears the tunnel down.

`--web-port <port>` pins the local browser origin; otherwise RimZ derives a stable port from the session name in `8300..8399`. The room itself is [`rimz web`](./web.md).

Link health, web tunneling, and reconnect mechanics are in [remote internals](../../internals/remote.md).

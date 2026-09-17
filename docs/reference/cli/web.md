# Web CLI

`rimz web` serves RimZ rooms, Zellij or tmux, to a browser through ttyd. Two machine-wide ttyd daemons do the serving. The writable daemon listens on `127.0.0.1:8200` by default and reaches every live room on the machine behind one Basic-auth credential. The broadcast daemon listens on `127.0.0.1:8201`, has no authentication, drops browser input, and serves only the rooms you share. Why and when to use each, the session manager, and the reverse-proxy recipe are in the [web guide](../../guide/web.md).

```sh
rimz web open [PATH | --session <name>] [--print] [--no-start] [--no-resume] [--json]
rimz web url [PATH | --session <name>] [--json]
rimz web share [PATH | --session <name>] [--print] [--json]
rimz web unshare [PATH | --session <name> | --all]
rimz web status [--json]
rimz web start
rimz web restart
rimz web stop
rimz web token create
rimz web token list
rimz web token revoke rimz
rimz web token revoke-all
```

`rimz web` with no subcommand is `rimz web open`. Every subcommand also takes the [global flags](../cli.md#global-flags); `--mux`, `--zellij`, `--tmux`, and `--root` matter only on the verbs that take a room. `PATH` defaults to the current directory and cannot be combined with `--session`.

Two preconditions apply across the command:

| Precondition | Checked by | Refusal |
| --- | --- | --- |
| `[web] enabled = true` (the default) | `open`, `share` | ``Browser access is disabled: set `[web] enabled = true` in the RimZ config on the machine serving this room (`rimz config path`) to allow browser sharing.`` |
| ttyd 1.7.5 or newer on `PATH` (or at `RIMZ_TTYD_BIN`) | `open`, `share`, `start`, `restart`, and `token create` while the writable daemon is online | `ttyd 1.7.5 or newer is required for browser access; ...` when missing, `ttyd <version> is too old for browser access; ...` when older. Both name the install or upgrade command. |

Every verb exits `0` on success and `1` with the error on stderr; an invalid command line exits `2`.

## Open a room in the browser

`rimz web open` takes you from a directory or session name to a browser tab on that room:

1. Check the preconditions above.
2. Resolve the room. A `PATH` whose room is not running is born, like `rimz start` without attaching, and recovers its prior agents unless you pass `--no-resume` (see [Resume on rebirth](./getting-started.md#resume-on-rebirth)). `--session` needs a session RimZ already knows and births it the same way when it is not running.
3. Wait up to 5 seconds for the session to appear on its backend.
4. Ensure the writable daemon: start it when it is offline, and replace it when its listener, auth settings, or browser page no longer match the config.
5. Print the URL and credential, then open the browser.

```console
$ rimz web open --print
http://127.0.0.1:8200/?room=rimz-query-engine-a1b2c3
ttyd basic auth for this machine: user rimz, password <secret>
```

The URL is the only line on stdout, so `url=$(rimz web open --print)` captures it. The credential line goes to stderr. With `[web] auth_header` set, stderr first gets ``rimz: authentication is delegated to the reverse proxy (trusted header `<header>`)``. The URL starts with `[web] base_url` when it is set, otherwise `http://127.0.0.1:<port>` whatever `[web] interface` is, and ends in `/?room=<session>`.

| Flag | Effect |
| --- | --- |
| `--session <name>` | Open the room with this exact session name instead of resolving a path. |
| `--print` | Print the URL and credential without launching a browser. |
| `--no-start` | Use the writable daemon only if it is already online; otherwise fail with ``the shared ttyd daemon is offline; run `rimz web start` or omit `--no-start` ``. The running daemon is used as it is, even when the config has changed since it started. |
| `--no-resume` | Bring a room born by this command up empty, without recovering its prior agents. A running room is unaffected. |
| `--json` | Print the [`rimz.web.v2` payload](#open-and-url-payload) on stdout, credential included. The browser does not open and the stderr credential line is not printed. |

Other refusals:

| Situation | Error |
| --- | --- |
| `--session` names a session with no RimZ workspace record | ``session `<name>` is not a known RimZ workspace session; run `rimz list` or open the workspace with `rimz start` first`` |
| The session did not appear within 5 seconds | ``<mux> session `<name>` is not addressable after web preparation. Run `rimz reset` from the workspace, then retry `rimz web open`.`` |
| `[web] port` is taken by another process | ``[web] port <port> is already in use by another process; choose a free port in `rimz config path` `` |
| The credential file is gone while the daemon runs | ``the shared ttyd daemon credential is missing; run `rimz web token create`, then retry`` |

A remote room opens in your local browser through [`rimz remote connect --web`](./remote.md#open-a-remote-room-in-the-browser), which runs `rimz web open` on the host for you.

## Print a room's URL

`rimz web url` prints the URL of a room RimZ has already born, without birthing it, starting the daemon, or creating a credential. The room does not have to be running. A path RimZ has never opened fails with ``workspace session `<name>` has not been born by RimZ; run `rimz web open <path>` or `rimz start <path>` first``.

The port comes from the running writable daemon, or from `[web] port` while it is offline, so the two differ when you have changed the port without restarting. `--json` prints the [`rimz.web.v2` payload](#open-and-url-payload), with the saved credential when one exists.

## Start, stop, and inspect the daemons

These verbs act on the daemons without naming a room. Each daemon keeps its record under `~/.rimz/web/`; a record whose process has died is cleaned up by whichever verb reads it next.

| Verb | Writable daemon | Broadcast daemon | Prints |
| --- | --- | --- | --- |
| `start` | Starts it, or keeps a running daemon that matches the config and replaces one that does not. | Untouched. | `ttyd: online on <address> (pid <pid>)` |
| `restart` | Always starts a fresh process. Run it after changing `[web]` or `[theme]`. | Restarted when the share list is non-empty, stopped when it is empty. | The `start` line, then `ttyd: was offline; started a fresh daemon` if it was down, then `share: online on <address> (pid <pid>)` if the broadcast daemon was restarted. |
| `stop` | Stops it. | Stops it. The share list is kept. | `stopped <n> ttyd daemons` (`daemon` when `n` is 1) |
| `status` | Reports it. | Reports it with the share list. | Two lines, below. |

```console
$ rimz web status
ttyd: online on 127.0.0.1:8200 (pid 48121)
share: offline (configured listener 127.0.0.1:8201), sharing: (none)
```

An offline daemon reports the listener from `[web] interface`, `port`, and `share_port`. The `share:` line lists shared sessions, comma-separated, even while the broadcast daemon is stopped. `status --json` prints the [status payload](#status-payload).

Listener and auth settings have no command-line flags: set them under `[web]` and run `rimz web restart`. Both daemons serve plain HTTP; HTTPS comes from a reverse proxy in front. `rimz start` also starts the writable daemon, best-effort, when `[web] enabled` is true (see [Browser access at start](./getting-started.md#browser-access-at-start)). Which command starts or stops which daemon, `rimz reload` included, is tabulated in [web internals](../../internals/web.md#who-starts-and-stops-them).

## Share a read-only broadcast

`rimz web share` adds one running room to the share list and prints its viewer URL on the broadcast daemon, starting that daemon when needed. The URL starts with `[web] share_base_url` when it is set, otherwise `http://127.0.0.1:<share_port>`. Anyone who can reach the listener can watch the room without a password, and nobody can type into it. The browser opens unless you pass `--print` or `--json`.

```console
$ rimz web share --print
http://127.0.0.1:8201/?room=rimz-query-engine-a1b2c3
```

On a non-loopback `[web] interface`, stderr also gets `rimz: warning: broadcast is unauthenticated; anyone reaching <interface>:<port> can watch`. `--json` prints the [`rimz.web.share.v1` payload](#share-payload).

`share` needs a room RimZ has born and that is running now. A room with no record fails like `url`; a born room that is not running fails with `this room is not shared`. The share list is durable: it survives `rimz web stop` and a reboot, and the next `share` or `restart` serves every room on it again.

A viewer URL whose room is not on the list, not known to RimZ, or not running shows `this room is not shared` and nothing else, so a viewer cannot discover other rooms by editing `?room=`. tmux viewers attach read-only and cannot resize the room; Zellij has no read-only attach, so ttyd's dropped input is the only barrier and a viewer's window size can change the room's layout.

`rimz web unshare` removes rooms from the list:

| Form | Effect | Prints |
| --- | --- | --- |
| `unshare [PATH]` or `unshare --session <name>` | Removes that room. The room does not need to be running. When other rooms stay shared, the broadcast daemon is stopped and started fresh, which disconnects every viewer; viewers of still-shared rooms reconnect. Removing the last room stops the daemon. | ``stopped sharing `<session>` ``, or `` `<session>` was not shared `` with no daemon change. |
| `unshare --all` | Empties the list and stops the broadcast daemon. | `stopped sharing all rooms`, or `no rooms were shared`. |

Removing one room stops the old broadcast process before checking the config, so an invalid `[web]` setting fails the command with viewers already disconnected and no broadcast daemon running.

## Rotate or revoke the credential

The writable daemon has one credential for the whole machine, named `rimz`. RimZ creates it the first time the daemon starts and stores it under `~/.rimz/web/` with mode 0600. It stays out of URLs and logs; `open`, `url --json`, and `token create` are the only commands that print it. It applies in trusted-header mode too, where the gate presents it to ttyd.

| Command | Effect | Prints |
| --- | --- | --- |
| `token create` | Mints a new secret. When the writable daemon is online, restarts it (and its gate) so the old secret stops working. | stderr: `ttyd basic auth for this machine: user rimz, password <secret>`; stdout: `rotated ttyd credential and restarted <0 or 1> daemon(s)` |
| `token list` | Shows when the credential was created. | `rimz: <RFC 3339 timestamp>`, or nothing when there is no credential. |
| `token revoke rimz`, `token revoke-all` | Stops the writable daemon and deletes the credential. Any name other than `rimz` fails with ``ttyd credential `<name>` does not exist (the single credential is `rimz`)``. | `revoked ttyd credential and stopped <0 or 1> daemon(s)` |

None of these touch the broadcast daemon. After a revoke, the next `open` or `start` creates a fresh credential.

`token create --read-only` is refused with ``ttyd read-only access is per process, not per credential; use `rimz web share` for a read-only broadcast``.

## JSON output

### Open and url payload

`open --json` and `url --json` print one object:

```json
{"version":"rimz.web.v2","url":"http://127.0.0.1:8200/?room=rimz-query-engine-a1b2c3","session":"rimz-query-engine-a1b2c3","port":8200,"tunnel_port":8200,"auth":{"mode":"basic"},"credential":{"username":"rimz","secret":"..."}}
```

| Field | Meaning |
| --- | --- |
| `version` | Always `rimz.web.v2`. |
| `url` | The browser URL, as printed without `--json`. |
| `session` | The room's session name. |
| `port` | The public writable listener: the running daemon's port, or `[web] port` while it is offline (`url` only). |
| `tunnel_port` | The port where ttyd itself listens, which `rimz remote connect --web` forwards to. It equals `port` when ttyd listens directly, and is a loopback ephemeral port when RimZ's gate stands in front (with `[web] auth_header` or a non-empty `trusted_proxies`). `null` from `url` while the daemon is offline. |
| `auth` | `{"mode":"basic"}`, or `{"mode":"trusted_header","header":"<header>"}` with `[web] auth_header` set. Offline `url` reads it from the config. |
| `credential` | `{"username":"rimz","secret":"<secret>"}`. Always present from `open`; omitted from `url` when no credential exists. Trusted-header payloads carry it too, because ttyd behind the gate still requires it. |

### Share payload

`share --json` prints:

```json
{"version":"rimz.web.share.v1","url":"http://127.0.0.1:8201/?room=rimz-query-engine-a1b2c3","session":"rimz-query-engine-a1b2c3","port":8201}
```

`port` is the broadcast listener. The payload has no credential.

### Status payload

`status --json` prints:

```json
{"version":"rimz.web.v2","online":true,"pid":48121,"interface":"127.0.0.1","port":8200,"share":{"online":false,"pid":null,"interface":"127.0.0.1","port":8201,"sessions":[]}}
```

| Field | Meaning |
| --- | --- |
| `version` | The schema, `rimz.web.v2`. |
| `online`, `pid` | Whether the writable daemon is running, and its ttyd pid (`null` when offline). |
| `interface`, `port` | The writable listener, from the running daemon or, offline, from `[web] interface` and `port`. |
| `share.online`, `share.pid` | The same for the broadcast daemon. |
| `share.interface`, `share.port` | The broadcast listener, offline from `[web] interface` and `share_port`. |
| `share.sessions` | The share list, sorted, whether or not the broadcast daemon is running. |

## Configuration

The `[web]` keys and their defaults are in the [configuration guide](../../guide/configuration.md#web-access) and the [web internals key table](../../internals/web.md#configuration). Changes to a running daemon take effect at `rimz web restart`.

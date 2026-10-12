# Web CLI

`rimz web` serves RimZ rooms, Zellij or tmux, to a browser. The writable daemon's sign-in gate listens on `127.0.0.1:8200` by default and reaches every live room on the machine through named Basic-auth tokens or a trusted reverse proxy. The broadcast daemon listens on `127.0.0.1:8201`, has no authentication, drops browser input, and serves only the rooms you share. Both use ttyd to serve the terminal. Why and when to use each, the session manager, and the reverse-proxy recipe are in the [web guide](../../guide/web.md).

```sh
rimz web open [PATH | --session <name>] [--print] [--no-start] [--no-resume] [--json]
rimz web url [PATH | --session <name>] [--json]
rimz web share [PATH | --session <name>] [--print] [--json]
rimz web unshare [PATH | --session <name> | --all]
rimz web status [--json]
rimz web start
rimz web restart
rimz web stop
rimz web token create <NAME>
rimz web token show [NAME]
rimz web token list [--json]
rimz web token revoke <NAME>
rimz web token revoke-all
```

`rimz web` with no subcommand is `rimz web open`. Every subcommand also takes the [global flags](../cli.md#global-flags); `--mux`, `--zellij`, `--tmux`, and `--root` matter only on the verbs that take a room. `PATH` defaults to the current directory and cannot be combined with `--session`.

Two preconditions apply across the command:

| Precondition | Checked by | Refusal |
| --- | --- | --- |
| `[web] enabled = true` (the default) | `open`, `share`, `start`, `restart` | ``Browser access is disabled: set `[web] enabled = true` in the RimZ config on the machine serving this room (`rimz config path`) to allow browser sharing.`` |
| ttyd 1.7.5 or newer on `PATH` (or at `RIMZ_TTYD_BIN`) | `open`, `share`, `start`, `restart`, and `token create` while the writable daemon is online | `ttyd 1.7.5 or newer is required for browser access; ...` when missing, `ttyd <version> is too old for browser access; ...` when older. Both name the install or upgrade command. |

Every verb exits `0` on success and `1` with the error on stderr; an invalid command line exits `2`.

## Open a room in the browser

`rimz web open` takes you from a directory or session name to a browser tab on that room:

1. Check the preconditions above.
2. Resolve the room. A `PATH` whose room is not running is born, like `rimz start` without attaching, and recovers its prior agents unless you pass `--no-resume` (see [Resume on rebirth](./getting-started.md#resume-on-rebirth)). `--session` needs a session RimZ already knows and births it the same way when it is not running.
3. Wait up to 5 seconds for the session to appear on its backend.
4. Ensure the writable daemon: start it when it is offline, and replace it when its listener, auth settings, or browser page no longer match the config.
5. Print the URL and sign-in instructions, then report the browser launch outcome.

```console
$ rimz web open --print
http://127.0.0.1:8200/?room=rimz-query-engine-a1b2c3
rimz: sign in as user rimz; password: rimz web token show rimz
```

The URL is the only line on stdout, so `url=$(rimz web open --print)` captures it. Sign-in instructions go to stderr, never a password. With several tokens they list the usernames and point to `rimz web token show <name>`. With `[web] auth_header` set, the line is `rimz: sign in through your reverse proxy (header <header>)`. The URL starts with `[web] base_url` when set, otherwise `http://127.0.0.1:<port>` whatever `[web] interface` is, and ends in `/?room=<session>`.

Without `--print` or `--json`, stderr reports `rimz: launched <opener>` (`xdg-open` on Linux, `open` on macOS), or `rimz: no browser launched (<reason>); open the URL above`. Reasons are `<opener> is not installed`, `<opener> failed to start: <error>`, or `this is an SSH session with no display`. SSH means `SSH_CONNECTION` or `SSH_TTY` is set; on Linux a set `DISPLAY` or `WAYLAND_DISPLAY` permits launching, while macOS skips in any SSH session. A launched opener is not a claim that a browser window appeared. `share` and `remote connect --web` report the same outcomes.

| Flag | Effect |
| --- | --- |
| `--session <name>` | Open the room with this exact session name instead of resolving a path. |
| `--print` | Print the URL and sign-in instructions without launching a browser. |
| `--no-start` | Use the writable daemon only if it is already online with token authentication; otherwise fail with ``the writable browser daemon is offline; run `rimz web start` or omit `--no-start` ``. A compatible running daemon is used as it is, even when the config has changed since it started. |
| `--no-resume` | Bring a room born by this command up empty, without recovering its prior agents. A running room is unaffected. |
| `--json` | Print the [`rimz.web.v2` payload](#open-payload) on stdout, including the upstream credential used by the tunnel relay. No browser launch or sign-in notice. |

Other refusals:

| Situation | Error |
| --- | --- |
| `--session` names a session with no RimZ workspace record | ``session `<name>` is not a known RimZ workspace session; run `rimz list` or open the workspace with `rimz start` first`` |
| The session did not appear within 5 seconds | ``<mux> session `<name>` is not addressable after web preparation. Run `rimz reset` from the workspace, then retry `rimz web open`.`` |
| `[web] port` is taken by another process | ``[web] port <port> is already in use by another process; choose a free port in `rimz config path` `` |
| The token store is gone while using `--no-start` | ``the browser token store is missing; run `rimz web token create <name>`, then retry`` |

A remote room opens in your local browser through [`rimz remote connect --web`](./remote.md#open-a-remote-room-in-the-browser), which runs `rimz web open` on the host for you.

## Print a room's URL

`rimz web url` prints the URL of a room RimZ has already born, without birthing it, starting the daemon, or creating a credential. The room does not have to be running. A path RimZ has never opened fails with ``workspace session `<name>` has not been born by RimZ; run `rimz web open <path>` or `rimz start <path>` first``.

The port comes from the running writable daemon, or from `[web] port` while it is offline, so the two differ when you have changed the port without restarting. `--json` prints the credential-free [`rimz.web.url.v1` payload](#url-payload).

## Start, stop, and inspect the daemons

These verbs act on the daemons without naming a room. Each daemon keeps its record under `~/.rimz/web/`; a record whose process has died is cleaned up by whichever verb reads it next.

| Verb | Writable daemon | Broadcast daemon | Prints |
| --- | --- | --- | --- |
| `start` | Requires `[web] enabled = true`. Starts it, or keeps a running daemon that matches the config and replaces one that does not. | Untouched. | `writable daemon online at <url> (listening on <address>, gate pid <n>, ttyd pid <m>)` |
| `restart` | Requires `[web] enabled = true`. Always starts fresh. Run it after changing `[web]` or `[theme]`. | Restarted when the share list is non-empty, stopped when empty. | The `start` line, then `writable daemon was offline; started fresh` if it was down, then `broadcast daemon online at <url> (pid <n>)` if restarted. |
| `stop` | Stops it. | Stops it. The share list is kept. | Names the stopped daemon(s), or `no browser daemon was running`. Stopping the broadcast adds `; the share list is kept`. |
| `status` | Reports its URL, sign-in mode, listener, and both pids. | Reports its URL, listener, pid, and share list. | Four lines, below. |

```console
$ rimz web status
writable   online    http://127.0.0.1:8200   sign-in: tokens rimz (rimz web token show rimz)
                     listening on 127.0.0.1:8200 (gate pid 48120, ttyd pid 48121)
broadcast  offline   no rooms shared
                     would listen on 127.0.0.1:8201 with no password
```

Setting `[web] enabled = false` does not stop a running daemon; use `rimz web stop`. `status`, `stop`, and every `token` operation, including revocation, remain usable while disabled.

An offline daemon reports the configured listener as `would listen on <address>`. The broadcast line lists shared sessions even while stopped. Sign-in is `tokens <names> (rimz web token show <name>)`, `no token (rimz web token create <name>)`, or `reverse proxy (header <header>)`. `status --json` prints the [status payload](#status-payload).

Listener and auth settings have no command-line flags: set them under `[web]` and run `rimz web restart`. Both daemons serve plain HTTP; HTTPS comes from a reverse proxy in front. `rimz start` also starts the writable daemon, best-effort, when `[web] enabled` is true (see [Browser access at start](./getting-started.md#browser-access-at-start)). Which command starts or stops which daemon, `rimz reload` included, is tabulated in [web internals](../../internals/web.md#who-starts-and-stops-them).

## Share a read-only broadcast

`rimz web share` adds one running room to the share list and prints its viewer URL on the broadcast daemon, starting that daemon when needed. The URL starts with `[web] share_base_url` when it is set, otherwise `http://127.0.0.1:<share_port>`. Anyone who can reach the listener can watch the room without a password, and nobody can type into it. The browser opens unless you pass `--print` or `--json`.

```console
$ rimz web share --print
http://127.0.0.1:8201/?room=rimz-query-engine-a1b2c3
```

On a non-loopback `[web] interface`, stderr also gets `rimz: warning: broadcast is unauthenticated; anyone reaching <interface>:<port> can watch`. `--json` prints the [`rimz.web.share.v1` payload](#share-payload).

`share` needs a room RimZ has born and that is running now. A room with no record fails like `url`; a stopped room fails with ``<session> is not running, and only a running room can be shared; start it with `rimz start`, then retry``. The share list survives `rimz web stop` and a reboot, and the next `share` or `restart` serves every room on it again.

A viewer URL whose room is not on the list, not known to RimZ, or not running shows `this room is not shared` and nothing else, so a viewer cannot discover other rooms by editing `?room=`. tmux viewers attach read-only and cannot resize the room; Zellij has no read-only attach, so ttyd's dropped input is the only barrier and a viewer's window size can change the room's layout.

`rimz web unshare` removes rooms from the list:

| Form | Effect | Prints |
| --- | --- | --- |
| `unshare [PATH]` or `unshare --session <name>` | Removes that room. The room need not be running. Restarts the broadcast when rooms remain, otherwise stops it. | ``stopped sharing `<session>`; viewers of <n> other shared rooms were disconnected and will reconnect`` (singular for 1), or ``stopped sharing `<session>`; the broadcast daemon is stopped`` for the last room. An unchanged list prints `` `<session>` was not shared ``. |
| `unshare --all` | Empties the list and stops the broadcast daemon. | `stopped sharing all rooms`, or `no rooms were shared`. |

Removing one room stops the old broadcast process before checking the config, so an invalid `[web]` setting fails the command with viewers already disconnected and no broadcast daemon running.

## Manage sign-in tokens

Each token name is a Basic-auth username: 1-64 characters from `[A-Za-z0-9._-]`, starting with a letter or digit. Tokens grant writable access to every live room on the machine. The first start creates `rimz` when no tokens exist. The private store is `~/.rimz/web/web-tokens.json`, mode 0600; an older saved password is adopted as `rimz` without changing it.

| Command | Effect | Output |
| --- | --- | --- |
| `token create <NAME>` | Creates a new token; an existing name is refused. Restarts an online writable daemon. | stderr: `rimz: token <name>: password <secret>`; stdout: `created token <name> and restarted the writable daemon`, or `created token <name>; the writable daemon is offline`. |
| `token show [NAME]` | Reads a password; name defaults to `rimz`. | The password alone on stdout. |
| `token list` | Lists tokens sorted by name. | One `<name>  created <local timestamp> (<age> ago)` line per token; when empty, `no tokens; run rimz web token create <name>` (exit 0). |
| `token list --json` | Lists names and creation times without secrets. | The [token-list payload](#token-list-payload). |
| `token revoke <NAME>` | Removes one token and restarts the writable daemon, disconnecting its sessions; the last token stops it. | `revoked token <name> and restarted the writable daemon`, `revoked token <name> and stopped the writable daemon (no tokens remain)`, or `revoked token <name>; the writable daemon is offline`. |
| `token revoke-all` | Removes all tokens and stops the writable daemon. | `revoked <n> tokens and stopped the writable daemon`, `revoked <n> tokens; the writable daemon is offline`, or `no tokens to revoke`. |

A duplicate name fails with ``token `<name>` already exists; revoke it first or pick another name``. A missing name fails with ``token `<name>` does not exist; existing tokens: <names>``, or `; no tokens exist`. These operations leave the broadcast daemon untouched and behave the same in trusted-header mode. Read-only viewing is a per-room broadcast: use `rimz web share`.

An online token change stops the writable daemon before attempting its replacement. If restart fails, the token change still succeeds, the daemon stays stopped, and stderr reports the cause. Revocation then reports `revoked token <name> and stopped the writable daemon (restart failed)` instead of claiming that no tokens remain. Fix the reported problem and run `rimz web start` to restore access with the remaining tokens.

## JSON output

### Open payload

`open --json` keeps the remote tunnel contract, `rimz.web.v2`:

```json
{"version":"rimz.web.v2","url":"http://127.0.0.1:8200/?room=rimz-query-engine-a1b2c3","session":"rimz-query-engine-a1b2c3","port":8200,"tunnel_port":41820,"auth":{"mode":"basic"},"credential":{"username":"rimz-upstream","secret":"..."}}
```

| Field | Meaning |
| --- | --- |
| `version` | Always `rimz.web.v2`, unchanged for mixed-version remote clients. |
| `url`, `session` | The public browser URL and room session name. |
| `port` | The public writable listener. |
| `tunnel_port` | The private loopback port where ttyd listens; the remote relay forwards here, bypassing the public gate. |
| `auth` | `{"mode":"basic"}`, or `{"mode":"trusted_header","header":"<header>"}`. |
| `credential` | The private upstream pair injected by the gate and tunnel relay, not a browser sign-in token. Present in every auth mode. |

### URL payload

`url --json` has no `credential` key, online or offline:

```json
{"version":"rimz.web.url.v1","url":"http://127.0.0.1:8200/?room=rimz-query-engine-a1b2c3","session":"rimz-query-engine-a1b2c3","port":8200,"tunnel_port":null,"auth":{"mode":"basic"}}
```

`tunnel_port` is `null` offline, otherwise the private upstream port. Offline `port` and `auth` come from config; online they describe the running daemon.

### Status payload

`status --json` names the writable and broadcast blocks separately:

```json
{"version":"rimz.web.status.v1","writable":{"online":true,"url":"http://127.0.0.1:8200","auth":{"mode":"basic"},"interface":"127.0.0.1","port":8200,"pid":48121,"gate_pid":48120,"tokens":["rimz"]},"broadcast":{"online":false,"url":"http://127.0.0.1:8201","interface":"127.0.0.1","port":8201,"pid":null,"sessions":[]}}
```

| Field | Meaning |
| --- | --- |
| `writable.online`, `broadcast.online` | Whether each daemon is running. |
| `url` in each block | The session-manager URL without `?room=`, using `base_url` or `share_base_url` when configured. |
| `interface`, `port` in each block | The running public listener, or its configured value offline. |
| `pid` in each block | ttyd's pid, `null` offline. |
| `writable.gate_pid` | The gate's pid, `null` offline. |
| `writable.auth` | The running sign-in mode, or config offline. |
| `writable.tokens` | Sorted saved token names, in every auth mode. |
| `broadcast.sessions` | Sorted shared rooms, even when the broadcast is stopped. |

Neither block includes a credential.

### Share payload

`share --json` is unchanged:

```json
{"version":"rimz.web.share.v1","url":"http://127.0.0.1:8201/?room=rimz-query-engine-a1b2c3","session":"rimz-query-engine-a1b2c3","port":8201}
```

`port` is the broadcast listener. The payload has no credential.

### Token-list payload

`token list --json` includes names and RFC 3339 creation times, never secrets:

```json
{"version":"rimz.web.tokens.v1","tokens":[{"name":"rimz","created_at":"2026-01-01T00:00:00Z"}]}
```

## Configuration

The `[web]` keys and their defaults are in the [configuration guide](../../guide/configuration.md#web-access) and the [web internals key table](../../internals/web.md#configuration). Changes to a running daemon take effect at `rimz web restart`.

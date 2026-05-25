# Web access

> See [DESIGN.md](../../DESIGN.md) and [multiplexers.md](./multiplexers.md) for the commitments this doc extends.

This document defines the target shape for web access. The feature is Zellij-only in its first version. tmux remains first-class for core Rimz behaviour; web access is an attach convenience, not a correctness path.

## Contract

Rimz exposes a workspace in the browser through Zellij's web server. Zellij owns the browser terminal, session transport, login tokens, cookies, TLS, remote terminal attach, and the `/session-name` URL scheme. Rimz owns workspace resolution, stable session naming, session/sidebar birth, URL construction, and diagnostics.

Rimz does not proxy terminal I/O, implement browser auth, store Zellij login tokens, or scrape panes. The ledger, hooks, resolver bridge, and sidebar wakeups work the same way whether the attached client is a terminal emulator or a browser.

## User shape

```sh
cd ~/code/terrain-lab
rimz web open
```

`rimz web open` resolves the current workspace, selects the Zellij backend, ensures the Rimz session exists with the normal sidebar layout, ensures the Zellij web server is online, and prints or opens the session URL.

The session URL is the Zellij web base URL plus the Rimz session name:

```text
<zellij-web-base>/<rimz-session-name>
```

For example:

```text
http://127.0.0.1:8082/rimz-terrain-lab-<path-hash>
```

Behind a reverse proxy with Zellij's `web_client.base_url` set:

```text
https://devbox.example/zellij/rimz-terrain-lab-<path-hash>
```

The route is the existing Rimz/Zellij session identity. There is no separate Rimz web session ID.

## Session names

Rimz session names are derived from the project root because the product invariant is `project repo == Rimz workspace == multiplexer session`.

The session name should stay URL-safe, shell-friendly, and collision-resistant:

```text
rimz-<safe-project-name>-<path-hash>
```

Using the literal project path as `rimz-<path>` is possible only if Rimz encodes it first. Raw paths contain `/`, spaces, home directories, and sometimes private customer or branch names. `/` is especially bad for Zellij web because the session name lives in a URL path segment. A raw `rimz-/home/me/code/terrain-lab` would be parsed as multiple route segments, not one session name.

The path hash is the right identity anchor. It keeps one stable session per project root, avoids collisions between repos with the same basename, keeps URLs short, and avoids leaking full local paths. The readable prefix is still useful for humans scanning `zellij list-sessions`.

## Planned CLI

Stage one keeps the surface small and delegates directly to `zellij web`:

```sh
rimz web open [PATH] [--print] [--no-start]
rimz web url [PATH] [--json]
rimz web status [--json]
rimz web start [--daemonize] [--ip <ip>] [--port <port>] [--cert <path>] [--key <path>]
rimz web stop
rimz web token create [--read-only] [--name <name>]
rimz web token list
rimz web token revoke <name>
rimz web token revoke-all
```

`rimz web` is an alias for `rimz web open`.

`open` is workspace-aware. It must not only print a Zellij URL for a session that has never been born through Rimz, because a bare Zellij web route can create a normal Zellij session without Rimz's workspace record or sidebar layout.

`url` is pure URL construction after workspace resolution and status/config lookup. It does not start the web server unless paired with `open`.

`start`, `stop`, `status`, and `token` are thin wrappers around Zellij's web CLI. Token commands print only what Zellij prints. Rimz never places tokens in URLs.

When backend selection resolves to tmux, every `rimz web ...` command returns an unsupported-backend diagnostic with the selected backend and the normal `rimz attach` path.

## Server ownership

Zellij's web server is off by default. Rimz checks it with:

```sh
zellij web --status
```

When `rimz web open` needs to start it, it runs:

```sh
zellij web --start --daemonize
```

Explicit server options pass through to Zellij:

```sh
rimz web start --daemonize --ip 127.0.0.1 --port 8082
rimz web start --daemonize --ip 0.0.0.0 --port 443 --cert /path/to/fullchain.pem --key /path/to/key.pem
```

Rimz treats the web server as host-local runtime state. A host reboot may leave the ledger intact while the Zellij web server is offline. Operators who need web access after reboot use Zellij config, systemd, or another host supervisor to start the web server.

## URL sources

The default base URL is Zellij's default:

```text
http://127.0.0.1:8082
```

`rimz web status` prefers the base URL reported by `zellij web --status`. External reverse-proxy hostnames are not discoverable from Zellij when the local server listens on `127.0.0.1`, so Rimz also supports a per-machine base URL:

```toml
# ~/.config/rimz/preferences.toml
[web.zellij]
base_url = "https://devbox.example/zellij"
auto_start = true
```

This setting is per-machine, not project config. A committed project should not publish one contributor's private hostname, tunnel, or reverse-proxy path.

Session names are generated as URL-safe ASCII, but URL construction still treats the session name as one path segment.

## Zellij configuration

Zellij can enable the web server from its own config:

```text
web_server true
web_server_ip "127.0.0.1"
web_server_port 8082
```

When serving behind a reverse proxy path, Zellij's web client must know the path prefix:

```text
web_client {
    base_url "/zellij"
}
```

Rimz's configured `web.zellij.base_url` and Zellij's `web_client.base_url` must agree. If Zellij serves under `/zellij`, Rimz must construct routes under the same prefix.

## Security

A browser-attached Zellij session is shell access as the local user. Treat a login token like SSH access to the account.

Rules:

- Zellij authentication stays mandatory. Rimz never bypasses it.
- Login tokens are never embedded in URLs, query strings, logs, feed items, or workspace state.
- Read-only tokens are for observation only, but terminal output can still contain secrets.
- HTTPS is required when listening on anything other than `127.0.0.1`.
- A reverse proxy is the supported shape for untrusted networks, because Zellij's web server does not provide its own rate limiting.
- Web configuration is local machine policy. It does not enter the project trust hash unless a future project config field executes a command.

The safest remote shape is:

```text
browser -> HTTPS reverse proxy with rate limiting -> zellij web on 127.0.0.1
```

## Remote terminal attach

Zellij can also attach to a web-served session from another terminal:

```sh
zellij attach https://devbox.example/zellij/rimz-terrain-lab-<path-hash> --token <login-token>
```

Rimz does not need to wrap this in the first version. The same session URL from `rimz web url` is enough for users who prefer Zellij's native remote attach command.

## Testing

Implementation tests cover:

- URL construction from a base URL plus Rimz session name.
- reverse-proxy path handling.
- `zellij web --status` parsing.
- `zellij web --start --daemonize` argv construction.
- token command argv construction.
- unsupported-backend diagnostics for tmux.
- workspace-first behaviour: `open` ensures the Rimz session before returning the browser URL.

Tests do not launch a real browser. Zellij integration tests self-skip when the binary is absent and use the existing Zellij trace fixture for argv-level checks.

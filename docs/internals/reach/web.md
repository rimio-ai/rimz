# Web access

> See [DESIGN.md](../../../DESIGN.md) and [multiplexers.md](../sidebar/multiplexers.md) for the commitments this doc extends.

Rimz opens a Zellij room in the browser by delegating terminal transport and authentication to `zellij web` while keeping workspace resolution, session birth, sidebar layout, and diagnostics in Rimz.

## Contract

Zellij owns the web server, browser terminal, session transport, login tokens, cookies, TLS, and the `/session-name` route. Rimz owns the workspace-to-session mapping, the Rimz sidebar birth path, URL construction, remote SSH tunneling, and fail-fast diagnostics when the selected backend or Zellij binary cannot serve web clients.

The ledger, hooks, resolver bridge, and sidebar wakeups work the same way whether the attached client is a terminal emulator or a browser. Rimz never stores Zellij login tokens, puts them in URLs, proxies terminal I/O, or scrapes browser clients.

## CLI

```sh
rimz web open [PATH] [--session <name>] [--print] [--no-start] [--json]
rimz web url [PATH] [--session <name>] [--json]
rimz web status [--json]
rimz web start [--daemonize] [--ip <ip>] [--port <port>] [--cert <path>] [--key <path>]
rimz web stop
rimz web token create [--read-only] [--name <name>]
rimz web token list
rimz web token revoke <name>
rimz web token revoke-all
```

`rimz web` is `rimz web open`. `open` resolves the workspace, requires the selected backend to be Zellij, ensures the Rimz room exists with the normal sidebar layout, births new rooms with Zellij `web_sharing on`, starts `zellij web --start --daemonize` when allowed and offline, prints the URL, and opens the local browser unless `--print` or `--json` is set. `url` resolves the same session and prints the URL without starting the server or birthing a room; it requires an existing Rimz workspace record so a URL never points at a bare Zellij session. `--session <name>` targets an existing Rimz workspace session by exact session name for local scripting and remote prep.

`status`, `start`, `stop`, and `token` are thin wrappers over Zellij's web CLI. Token commands relay only Zellij's output. `status --json`, `open --json`, and `url --json` emit versioned JSON with `version = "rimz.web.v1"`; the `open`/`url` payload includes `url`, `session`, `base_url`, `ip`, `port`, and `token_count`.

## Session names and URLs

Rimz session names come from the canonical project root: `rimz-<basename-slug>-<hash6>`. The slug is the root basename with non-ASCII-alphanumeric characters mapped to `-`, capped for short mux names; the six-character suffix comes from the workspace id. The durable identity remains the `workspace_id`, not the session string, so a future derivation change can retire and rebirth the room under a new name.

The browser URL is one path segment under the Zellij web base URL:

```text
<zellij-web-base>/<rimz-session-name>
```

With the default Zellij web listener this is `http://127.0.0.1:8082/rimz-project-a1b2c3`. Behind a reverse proxy path, set `[web.zellij].base_url = "https://devbox.example/zellij"` so Rimz prints `https://devbox.example/zellij/rimz-project-a1b2c3`.

## Server and config

Rimz checks server state with `zellij web --status`, parses the Zellij 0.44.3 online/offline strings, and treats unknown output as a typed parse failure with the raw line in the diagnostic. The default base URL is `http://127.0.0.1:8082`; a configured `[web.zellij].base_url` overrides the URL users open, while the parsed status URL still supplies the loopback `ip` and `port` for remote tunneling.

```toml
[web.zellij]
base_url = "https://devbox.example/zellij"
auto_start = true
```

This section is per-machine policy. It does not enter the project trust hash because it executes no command and commonly names private hostnames or local tunnels.

Zellij's own config still controls server defaults and sharing policy:

```text
web_server true
web_server_ip "127.0.0.1"
web_server_port 8082
web_sharing "on"
```

If a room already existed with Zellij web sharing off, restart the room through `rimz web open` or set `web_sharing "on"` in Zellij config before using the browser route. If Zellij config locks web sharing to `disabled`, Zellij rejects browser clients until that config changes and the session restarts.

## Remote rooms

`rimz remote connect <target> --web` opens the remote Zellij room in the local browser through an SSH local-forward tunnel, then proceeds to the normal terminal attach.

The local process first runs a non-PTY prep command on the remote host:

```text
ssh -o ConnectTimeout=10 -- <host> '<PATH repair>; exec rimz web open --print --json ...'
```

That remote `rimz web open` resolves or verifies the workspace, births the Rimz room with `web_sharing on` when the target is a path, starts the remote Zellij web server when allowed, and returns `rimz.web.v1`. A tmux room, old remote Rimz, old Zellij, or disabled web capability fails here before the terminal attach starts.

When the payload reports `token_count = 0`, local Rimz runs `rimz web token create` remotely and relays Zellij's one-time token output under a short banner. Rimz never stores the token and never puts it in the URL.

The local tunnel uses a stable deterministic port derived from the session name in `8300..8399`, scanning to the next free port on collision; `--web-port <port>` overrides it and fails if the port is already in use. The tunnel always forwards to remote `127.0.0.1:<remote-web-port>` and is supervised separately from the interactive attach with the same established-link reconnect policy. The browser URL is `http://127.0.0.1:<local-port>/<session>`, so browser cookies remain tied to a stable local origin across reconnects and repeat runs.

The remote path deliberately uses three SSH connections: prep, tunnel, and attach. Key or agent authentication is the intended shape; password authentication prompts for each connection. The tunnel is not coupled to the attach ControlMaster, so it can reconnect independently when the terminal link drops.

## Security

A browser-attached Zellij session is shell access as the room's user. Treat a login token like SSH access to the account.

- Zellij authentication stays mandatory.
- Login tokens stay out of URLs, query strings, logs, feed items, and workspace state.
- Read-only tokens are observation-only, while terminal output can still contain secrets.
- HTTPS is required when listening on anything other than `127.0.0.1`.
- A reverse proxy with rate limiting is the supported shape for untrusted networks.

The safest public shape is:

```text
browser -> HTTPS reverse proxy with rate limiting -> zellij web on 127.0.0.1
```

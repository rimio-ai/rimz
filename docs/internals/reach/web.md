# Web access

> See [DESIGN.md](../../../DESIGN.md) and [multiplexers.md](../sidebar/multiplexers.md) for the commitments this doc extends.

Rimz opens a Zellij room in the browser by delegating terminal transport and authentication to `zellij web` while keeping workspace resolution, session birth, sidebar layout, and diagnostics in Rimz.

## Contract

Zellij owns the web server, browser terminal, session transport, login tokens, cookies, TLS, and the `/session-name` route. Rimz owns the workspace-to-session mapping, the Rimz sidebar birth path, URL construction, remote SSH tunneling, and fail-fast diagnostics for an explicit `--mux tmux`, a room already live under tmux, or a Zellij binary that cannot serve web clients.

The ledger, hooks, run bridge, and sidebar wakeups work the same way whether the attached client is a terminal emulator or a browser. Rimz keeps the serving machine's login token in a plaintext 0600 state file and keeps tokens out of URLs. Browser terminal I/O stays inside Zellij, and browser-client state stays outside Rimz.

## CLI

```sh
rimz web open [PATH] [--session <name>] [--print] [--no-start] [--no-resume] [--json]
rimz web url [PATH] [--session <name>] [--json]
rimz web status [--json]
rimz web start [--daemonize] [--ip <ip>] [--port <port>] [--cert <path>] [--key <path>]
rimz web stop
rimz web token create [--read-only]
rimz web token list
rimz web token revoke <name>
rimz web token revoke-all
```

`rimz web` is `rimz web open`. `open` resolves the workspace, assumes Zellij unless the command explicitly passes `--mux tmux`, ensures the Rimz room exists with the normal sidebar layout, loads and grants the presence plugin, asks that plugin to enable browser sharing for the session, starts `zellij web --start --daemonize` when allowed and offline, prints the URL, ensures the serving machine has a cached login token, prints that token value, and opens the local browser unless `--print` or `--json` is set. `--no-resume` skips rebirth recovery, and the hidden `--confirm-resume` flag lets remote prep ask the recovery question over inherited SSH stdin/stderr. `url` resolves the same session and prints the URL without starting the server, birthing a room, changing sharing state, or provisioning a token; it requires an existing Rimz workspace record so a URL never points at a bare Zellij session. `--session <name>` targets an existing Rimz workspace session by exact session name for local scripting and remote prep.

`status`, `start`, `stop`, and `token` are thin wrappers over Zellij's web CLI except for `token ensure`, which returns the cached login token or mints and caches one. Successful `token revoke` and `token revoke-all` clear the cache so the next ensure mints fresh. `status --json`, `open --json`, and `url --json` emit versioned JSON with `version = "rimz.web.v1"`; the `open`/`url` payload includes `url`, `session`, `base_url`, `ip`, `port`, and `token_count`.

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
[web]
enabled = true

[web.zellij]
base_url = "https://devbox.example/zellij"
auto_start = true
font = "JetBrainsMono Nerd Font Mono"
style_client = true
```

`[web] enabled` defaults to true and allows Rimz to auto-grant its embedded Zellij presence plugin, enable browser sharing, and start the Zellij web server when allowed. When it is false, `rimz web open` fails before any room side effect with guidance to change the config on the machine serving the room; `rimz remote connect --web` relays that remote-side failure from the prep command. This section is per-machine policy and stays outside the project trust hash because it executes no command and commonly names private hostnames or local tunnels.

When `style_client` is true, Rimz derives a top-level Zellij `web_client` block from the active `[theme]` Alacritty palette and `font`, merges it over the user's resolved Zellij config, writes the generated copy to the Rimz state directory, and starts `zellij web` with `ZELLIJ_CONFIG_FILE` pointing at that copy. The generated block replaces any existing top-level `web_client` in the copied config while preserving other Zellij settings. Browser styling is machine-global for the Zellij web daemon and applies when Rimz starts the server; an already-online server keeps its current config until restart. If the user's Zellij config is unreadable or invalid, or the active scheme palette is incomplete, Rimz skips browser styling with a one-line stderr note and starts the server on the user's real config.

Zellij's own config still controls server defaults and sharing policy:

```text
web_server true
web_server_ip "127.0.0.1"
web_server_port 8082
```

Every `rimz web open` enables browser sharing at runtime. Rimz first seeds Zellij's `permissions.kdl` cache for its own presence plugin with `ReadApplicationState`, `RunCommands`, `Reconfigure`, and `StartWebServer`, keyed to the plugin's canonical path, then sends a boot pipe so the plugin is loaded and granted before sending the `rimz:share_session` pipe that calls Zellij's `share_current_session()`. Zellij grants cached permissions without a floating prompt, so the runtime share works for clientless rooms. If the pipe fails or the session metadata never confirms `web_clients_allowed true`, Rimz still prints the URL and writes a stderr note naming the remaining checks: Zellij version, presence-plugin artifact availability, and `[web] enabled = true`. If Zellij config locks web sharing to `disabled`, Zellij rejects browser clients until that config changes and the session restarts.

## Remote rooms

`rimz remote connect <target> --web` opens the remote Zellij room in the local browser through an SSH local-forward tunnel and stays in the foreground supervising that tunnel until Ctrl-C.

The local process first runs a non-PTY prep command on the remote host with stdout captured for JSON and stdin/stderr inherited for live recovery prompts:

```text
ssh -o ConnectTimeout=10 -- <host> '<PATH repair>; export TERM=xterm-256color; export COLORTERM=truecolor; exec rimz web open --print --json --confirm-resume ...'
```

That remote `rimz web open` resolves or verifies the workspace, births the Rimz room when the target is a path, asks the rebirth recovery prompt over the prep connection when the local terminal is interactive, enables sharing through the presence plugin, starts the remote Zellij web server when allowed, writes its stderr notes directly to the local terminal, and returns `rimz.web.v1` without minting a token. A project whose room is already live under tmux, old remote Rimz, old Zellij, or disabled web capability fails here before browser access opens.

The prep command births browser panes under `xterm-256color` plus `COLORTERM=truecolor`, the xterm.js-compatible shape, because Zellij's session server forks their environment from this non-PTY command and browser clients always render 24-bit color.

Remote web relays the serving machine's cached Zellij login token before opening the browser. The cache lives at `$XDG_STATE_HOME/rimz/web-login-token.json`, falls back with the rest of Rimz state when `XDG_STATE_HOME` is unset, and is written as plaintext JSON with mode 0600 beside Zellij's hashed token store. Local `rimz web open` and remote `token ensure` use the same cache: cache hit prints the value, cache miss mints a Zellij token, parses the token value out of `zellij web --create-token`, stores it, and prints it. `token_count` remains in `rimz.web.v1` as server state, not as proof this browser is logged in or that the user still holds a token value. Rimz keeps the token out of URLs, logs, ledger events, and workspace state.

The local tunnel uses a stable deterministic port derived from the session name in `8300..8399`, scanning to the next free port on collision; `--web-port <port>` overrides it and fails if the port is already in use. The tunnel always forwards to remote `127.0.0.1:<remote-web-port>` and uses the same established-link reconnect policy as remote attach unless `--no-reconnect` is set. The browser URL is `http://127.0.0.1:<local-port>/<session>`, so browser cookies remain tied to a stable local origin across reconnects and repeat runs.

The remote path uses three SSH connections: prep with stdin/stderr for recovery, idempotent token provisioning, and tunnel. Key or agent authentication is the intended shape; password authentication prompts for each connection. The tunnel runs independently in the foreground, reconnects when allowed, and stops when the local Rimz process receives Ctrl-C.

## Security

A browser-attached Zellij session is shell access as the room's user. Treat the cached login token like an SSH private key on the serving machine.

- Zellij authentication stays mandatory.
- The cached login token is plaintext on the serving machine at mode 0600 and stays out of URLs, query strings, logs, ledger events, and workspace state.
- Successful Rimz revokes clear the plaintext cache; external Zellij revokes require deleting the cache file or running a Rimz revoke before the next open.
- Read-only tokens are observation-only, while terminal output can still contain secrets.
- HTTPS is required when listening on anything other than `127.0.0.1`.
- A reverse proxy with rate limiting is the supported shape for untrusted networks.

The safest public shape is:

```text
browser -> HTTPS reverse proxy with rate limiting -> zellij web on 127.0.0.1
```

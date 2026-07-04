# Web CLI

`rimz web` opens a Zellij-backed Rimz room in the browser. Zellij serves the terminal and owns authentication; Rimz resolves the workspace, ensures the normal sidebar room exists, constructs the URL, and reports unsupported backend or version problems before returning a route.

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

`rimz web` is `rimz web open`. `open` starts from `PATH` or `.` and births the Rimz room with Zellij `web_sharing on` if needed; for an existing room, it asks the presence plugin to enable sharing at runtime. Human output prints the URL and a freshly minted one-time Zellij login token; `--json` emits the `rimz.web.v1` payload without minting a token. `--session <name>` targets an existing Rimz workspace session by exact session name. `--print` skips browser launch, and `--no-start` refuses when `zellij web` is offline.

`url` prints the route without birthing a room or starting the server. It requires an existing Rimz workspace record, so a script never receives a URL that would create a bare Zellij session without the Rimz sidebar.

`status`, `start`, `stop`, and `token` delegate to Zellij's web CLI. Token output is Zellij's one-time output; Rimz never stores tokens or embeds them in URLs.

Configure reverse-proxy URLs under per-machine config:

```toml
[web.zellij]
base_url = "https://devbox.example/zellij"
auto_start = true
```

Remote browser access is `rimz remote connect <target> --web`; see [Getting started → Remote rooms](./getting-started.md#remote-rooms) and [web internals](../../internals/reach/web.md#remote-rooms).

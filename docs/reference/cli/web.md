# Web CLI

`rimz web` opens a Zellij-backed Rimz room in the browser. Zellij serves the terminal and owns authentication; Rimz resolves the workspace, ensures the normal sidebar room exists, constructs the URL, and reports unsupported backend or version problems before returning a route.

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

`rimz web` is `rimz web open`. `open` starts from `PATH` or `.` and ensures the Rimz room exists, then loads and grants the presence plugin before asking it to enable browser sharing at runtime. Human output prints the URL and the serving machine's cached Zellij login token; a missing cache mints one token, stores it as plaintext mode 0600 at `$XDG_STATE_HOME/rimz/web-login-token.json`, and prints it. Login tokens stay out of URLs.

| Flag | Effect |
| --- | --- |
| `--session <name>` | Target an existing Rimz workspace session by exact session name |
| `--print` | Skip the browser launch; print the URL only |
| `--no-start` | Refuse when `zellij web` is offline instead of starting it |
| `--no-resume` | Skip recovering the room's prior agents |
| `--json` | Emit the `rimz.web.v1` payload without provisioning a token |
| `--confirm-resume` | Hidden: prompt over stdin/stderr, used by remote prep |

`url` prints the route without birthing a room or starting the server. It requires an existing Rimz workspace record, so a script never receives a URL that would create a bare Zellij session without the Rimz sidebar.

`status`, `start`, `stop`, and most `token` commands delegate to Zellij's web CLI. Hidden `token ensure` prints the cached token value on stdout, minting and caching one when absent. Successful `token revoke` and `token revoke-all` clear the plaintext cache, so the next `open` or `token ensure` mints fresh.

Configure reverse-proxy URLs under per-machine config:

```toml
[web]
enabled = true

[web.zellij]
base_url = "https://devbox.example/zellij"
auto_start = true
font = "JetBrainsMono Nerd Font Mono"
style_client = true
```

`[web] enabled` defaults to true. Set it to false to make `rimz web open` and `rimz remote connect --web` fail before room changes or permission-cache seeding. `style_client` defaults to true, deriving Zellij's browser-terminal `web_client` font and colors from `[theme]` when Rimz starts the server; set it to false to leave your own Zellij `web_client` config in charge. `font` defaults to `JetBrainsMono Nerd Font Mono`.

Remote browser access is `rimz remote connect <target> --web`; see [Getting started → Remote rooms](./getting-started.md#remote-rooms) and [web internals](../../internals/reach/web.md#remote-rooms).

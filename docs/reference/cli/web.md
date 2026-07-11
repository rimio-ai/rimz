# Web CLI

`rimz web` opens a RimZ room in the browser through the room's backend: `zellij web` for Zellij and ttyd for tmux.

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

`rimz web` is `rimz web open`.

`open` resolves or births the room, verifies that its session is addressable on the selected backend, starts the backend's web engine when allowed, prints the URL and credential, and opens the browser.

| Flag | Effect |
| --- | --- |
| `--session <name>` | Target an existing RimZ workspace session by exact name |
| `--print` | Skip the browser launch |
| `--no-start` | Refuse when the engine is offline |
| `--no-resume` | Skip recovering the room's prior agents |
| `--json` | Emit `rimz.web.v1` without printing the credential |

`url` requires an existing workspace record and prints its route without birthing a room, starting a server, or provisioning a credential.

`status` and `stop` are machine-wide across both engines.

`start` controls the machine-wide Zellij server; under tmux selection it directs the user to `rimz web open` because ttyd servers are per room.

Token commands dispatch through normal mux selection.

Zellij supports named read/write or read-only tokens.

ttyd exposes one credential named `rimz`; `create` rotates it and restarts live instances, `list` prints its creation time, and revoke stops live instances before clearing it.

ttyd read-only credentials are refused because ttyd's read-only setting belongs to the whole process.

JSON `open` and `url` payloads include `version`, `engine`, `url`, `session`, `base_url`, `ip`, `port`, and `token_count`; a missing `engine` from an older peer defaults to `zellij`.

`status --json` adds `tmux_instances`, whose rows contain `session`, `pid`, and `port`.

Configure public URL prefixes under `[web.zellij]` and `[web.tmux]`; see the [web guide](../../guide/web.md) and [configuration guide](../../guide/configuration.md#web-access).

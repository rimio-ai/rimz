# Web CLI

`rimz web` opens any RimZ room in the browser through one machine-wide ttyd daemon.

```sh
rimz web open [PATH] [--session <name>] [--print] [--no-start] [--no-resume] [--json]
rimz web url [PATH] [--session <name>] [--json]
rimz web status [--json]
rimz web start
rimz web stop
rimz web token create [--read-only]
rimz web token list
rimz web token revoke <name>
rimz web token revoke-all
```

`rimz web` is `rimz web open`.

`open` resolves or births the room, verifies that its session is addressable on the selected backend, ensures the shared daemon, prints the URL and credential, and opens the browser.

| Flag | Effect |
| --- | --- |
| `--session <name>` | Target an existing RimZ workspace session by exact name. |
| `--print` | Skip the browser launch. |
| `--no-start` | Require the shared daemon to already be online. |
| `--no-resume` | Skip recovering the room's prior agents. |
| `--json` | Emit the `rimz.web.v2` payload on stdout; the credential is inside the payload. |

`url` requires an existing workspace record and computes its route without birthing a room or starting the daemon. JSON output provisions the machine credential when absent because the v2 payload includes it.

`start`, `status`, and `stop` act on the one machine daemon and do not need a room. The port comes from `[web] port`; command-line listener and TLS overrides are not supported.

The JSON `open` and `url` payload is:

```json
{"version":"rimz.web.v2","url":"http://127.0.0.1:8200/?arg=rimz-project-a1b2c3","session":"rimz-project-a1b2c3","port":8200,"credential":{"username":"rimz","secret":"..."}}
```

`status --json` emits `version`, `online`, `pid`, and `port`.

The one credential is named `rimz`. `create` rotates it and restarts the live daemon, `list` prints its creation time, and either revoke verb stops the daemon before clearing it.

`--read-only` is rejected because ttyd's read-only setting belongs to the whole process.

Configure the daemon under `[web]`; see the [web guide](../../guide/web.md) and [configuration guide](../../guide/configuration.md#web-access).

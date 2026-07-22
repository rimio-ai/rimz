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

`open` resolves or births the room, verifies that its session is addressable on the selected backend, ensures the shared daemon, prints the URL and Basic credential or trusted-header proxy note, and opens the browser.

| Flag | Effect |
| --- | --- |
| `--session <name>` | Target an existing RimZ workspace session by exact name. |
| `--print` | Skip the browser launch. |
| `--no-start` | Require the shared daemon to already be online. |
| `--no-resume` | Skip recovering the room's prior agents. |
| `--json` | Emit the `rimz.web.v2` payload on stdout; Basic `open` includes the credential. |

`url` requires an existing workspace record and inspects its route without birthing a room, starting the daemon, or creating a credential. A live daemon's port wins over a changed configured port; offline inspection uses `[web] port`. JSON output includes the saved credential when one exists and omits `credential` otherwise.

`start`, `status`, and `stop` act on the one machine daemon and do not need a room. Human status prints the configured `[web] interface` and `port`; command-line listener and TLS overrides are not supported.

The JSON `open` payload is:

```json
{"version":"rimz.web.v2","url":"http://127.0.0.1:8200/?arg=rimz-project-a1b2c3","session":"rimz-project-a1b2c3","port":8200,"auth":{"mode":"basic"},"credential":{"username":"rimz","secret":"..."}}
```

The `url --json` payload has the same fields, with optional `credential`. Trusted-header payloads use `"auth":{"mode":"trusted_header","header":"X-Authentik-Username"}` and omit `credential`.

`status --json` emits `version`, `online`, `pid`, and `port`.

The one credential is named `rimz`. `create` rotates it and restarts the live daemon, `list` prints its creation time, and either revoke verb stops the daemon before clearing it.

`create` refuses while `[web] auth_header` is set and tells you to unset that field to return to Basic Auth. `list` and both revoke forms continue to manage any saved Basic credential; revoke still stops a trusted-header daemon.

`--read-only` is rejected because ttyd's read-only setting belongs to the whole process.

Configure the daemon under `[web]`; see the [web guide](../../guide/web.md) and [configuration guide](../../guide/configuration.md#web-access).

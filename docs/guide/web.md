# Web

A RimZ room can render in a browser instead of a terminal: the same sidebar, panes, and agents, open at one URL on either Zellij or tmux.

RimZ keeps the engine behind the command: Zellij rooms use `zellij web`, while tmux rooms use one loopback-only [ttyd](https://github.com/tsl0922/ttyd) process per room.

## Install the serving engine

Zellij browser access needs Zellij 0.44.3 or newer with web support.

tmux browser access needs ttyd on the machine serving the room:

```sh
brew install ttyd        # macOS or Linuxbrew
apt install ttyd         # Debian or Ubuntu
```

`rimz doctor` reports the detected backend's browser capability, and a tmux `rimz web open` refuses with the install commands when ttyd is missing.

## Serve a local room

```sh
rimz web            # ensure the room and server, print URL + credential, open browser
rimz web url        # print the existing room's URL without starting anything
rimz web status     # report Zellij plus every live ttyd room server
rimz web stop       # stop both engines on this machine
```

`rimz web` is `rimz web open`.

RimZ resolves the room's live backend first when no explicit mux is selected, then follows the normal `--mux`, environment, config, and installed-backend selection.

### Zellij rooms

RimZ starts the machine-wide `zellij web` server when needed, enables web sharing for the room through the presence plugin, and prints the room route under the Zellij base URL.

The browser shows Zellij's “Security Token Required” page; paste the login token RimZ prints.

Zellij owns the server, terminal transport, cookies, TLS, sharing policy, and token store.

### tmux rooms

RimZ starts a ttyd process bound to `127.0.0.1` for that room, with the route `/<session>` and an attached `tmux attach -t <session>` child.

The browser shows a Basic-Auth prompt; use the printed user `rimz` and password.

ttyd servers are per room, so `rimz web start` remains a Zellij-server command; use `rimz web open` to start a tmux room server.

## Open a remote room

```sh
rimz remote connect dev --web
rimz remote connect dev --web --web-port 8443
```

The remote room selects its own engine and returns the same `rimz.web.v1` prep payload.

RimZ relays that engine's credential, opens an SSH local-forward from your laptop's `127.0.0.1` to the remote loopback listener, and opens `http://127.0.0.1:<local-port>/<session>`.

The tunnel stays in the foreground, reconnects with the normal remote-link policy, and uses a stable port from 8300–8399 unless `--web-port` overrides it.

## Credentials

The verbs follow the selected backend:

```sh
rimz web token create
rimz web token list
rimz web token revoke <name>
rimz web token revoke-all
```

Zellij keeps its normal token store; RimZ caches the current plaintext token at mode 0600 so later local and remote opens can show it again.

ttyd has one credential named `rimz` per serving machine; `create` rotates it and restarts live ttyd room servers, while either revoke command stops those servers and clears the credential.

`--read-only` works for Zellij tokens.

ttyd read-only mode is a process policy rather than a credential policy, so this release refuses `rimz --mux tmux web token create --read-only` instead of presenting a misleading watcher credential.

Credentials stay out of URLs, logs, store events, and workspace state; treat either value like an SSH private key.

## Configuration

```toml
[web]
enabled = true

[web.zellij]
base_url = "https://devbox.example/zellij"
auto_start = true
font = "JetBrainsMono Nerd Font Mono"
style_client = true

[web.tmux]
base_url = "https://devbox.example/tmux"
auto_start = true
```

Each `base_url` is the prefix RimZ prints when a reverse proxy mounts that engine; the room session remains the final path segment.

## Security boundary

Both engines bind to loopback by default and require authentication.

RimZ invokes ttyd with write access, origin checks, mandatory Basic Auth, and an explicit loopback bind; Zellij keeps its own mandatory token gate.

The browser session is shell access as the serving user, and terminal output can contain secrets even when a Zellij token is read-only.

Put HTTPS and rate limiting in front before exposing either listener beyond loopback:

```text
browser -> HTTPS reverse proxy with rate limiting -> loopback web engine
```

## See also

- [Remote](./remote.md) — saved aliases and reconnect behavior.
- [Web CLI reference](../reference/cli/web.md) — every subcommand and flag.
- [Configuration](./configuration.md#web-access) — per-machine settings.
- [Web internals](../internals/web.md) — state files, ports, and remote relay.
- [Troubleshooting](./troubleshooting.md) — missing engines and failed starts.

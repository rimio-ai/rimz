# Web

A Rimz room can render in a browser instead of a terminal. Zellij serves the room over its own web server, so the same sidebar, panes, and agents open at a URL: on the machine running the room, or tunnelled from a server to the browser on your laptop.

This is a Zellij feature. A tmux room reattaches over SSH ([Remote](./remote.md)) but does not serve the browser tunnel.

## Serve the room on this machine

On the host running the room, `rimz web` starts Zellij's web server and opens the room in your browser:

```sh
rimz web open      # start the server and open the room's URL
rimz web url       # print the URL without starting the server
rimz web status    # whether the server is running, and where
rimz web stop      # stop it
```

`rimz web` is `rimz web open`: it ensures the room exists, starts the server when it is offline, prints the URL and a login token, and opens your browser. `rimz web url` prints the route for an existing room without touching the server, so a script never receives a URL that would spin up a bare Zellij session with no sidebar.

## Open a remote room in your browser

From your laptop, add `--web` to a remote connect and Rimz tunnels the server's room into your local browser over SSH:

```sh
rimz remote connect dev --web              # open the remote room at 127.0.0.1
rimz remote connect dev --web --web-port 8443
```

Rimz starts the remote web server when needed, forwards a local port to it, opens the browser at `http://127.0.0.1:<port>/<session>`, and stays in the foreground supervising the tunnel until Ctrl-C. The tunnel rides the same self-healing link as a terminal attach, so a dropped connection reconnects on its own. Without `--web-port`, Rimz derives a stable local port from the session name, so the browser origin and its cookies survive reconnects and repeat runs. The target and saved aliases behave exactly as for a terminal connect; see [Remote](./remote.md).

## Access is scoped by a login token

A browser-attached room is shell access as the room's user, so Zellij gates it behind a login token. Rimz mints one on first use, and you manage them explicitly:

```sh
rimz web token create            # mint a login token (--read-only for a watcher)
rimz web token list              # token names and creation dates
rimz web token revoke <name>     # revoke one by name
rimz web token revoke-all        # revoke every token
```

The token is cached as plaintext at mode `0600` on the machine serving the room, and stays out of URLs, logs, and store events; treat it like an SSH private key there. Remote `--web` relays that same cached token to your browser, so a remote session reuses the server's token instead of minting a new one.

## Security

- A read-only token is observation-only, but terminal output can still carry secrets.
- Keep the listener on `127.0.0.1`. Anything reachable beyond loopback wants HTTPS in front, with a reverse proxy and rate limiting as the supported public shape.
- A revoke through Rimz clears the plaintext cache; a token revoked directly in Zellij leaves the cache in place, so revoke through Rimz or delete the cache file before the next `open`.

## See also

- [Remote](./remote.md) — the SSH link the `--web` tunnel rides, saved aliases, and reconnect.
- [Web CLI reference](../reference/cli/web.md) — every `rimz web` subcommand and flag.
- [Configuration](../reference/configuration.md#web-access) — the `[web]` keys and reverse-proxy `base_url`.
- [Web internals](../internals/web.md) — the token model, server lifecycle, and remote-tunnel mechanics.
- [Troubleshooting](./troubleshooting.md) — a room that will not start or serve.

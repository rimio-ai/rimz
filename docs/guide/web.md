# Web

A RimZ room can render in a browser instead of a terminal: the same sidebar, panes, and agents, open at a URL. This runs on Zellij's own web server, so RimZ stays thin. On the machine running the room, RimZ turns that server on and points your browser at the right room. From your laptop, RimZ tunnels the remote server down over SSH. Either way the web server, the browser terminal, the login page, and the TLS are Zellij's; RimZ resolves which room you mean and wires up the plumbing.

Browser access is a Zellij capability, so the serving machine needs Zellij 0.44.3 or newer with web support built in. A tmux room has no equivalent: reattach it over SSH ([Remote](./remote.md)), or run the project under Zellij to serve it to a browser.

## What RimZ adds, and what Zellij owns

Everything security-sensitive stays inside Zellij's web server, which is mature machinery RimZ does not reimplement:

- **Zellij owns** the web server process, the terminal you see in the browser, the session transport, the login token, cookies, TLS, and the listening socket.
- **RimZ owns** the mapping from a project directory to its room, birthing that room with the normal sidebar layout, turning on browser sharing, building the URL, and (for a remote room) the SSH tunnel.

So the browser talks to Zellij, not to RimZ. RimZ gets you to the door and Zellij guards it.

## Serve the room on this machine

On the host running the room, `rimz web` starts Zellij's web server, turns on browser sharing for the room, and opens it in your browser:

```sh
rimz web            # start the server if offline, print the URL and token, open the browser
rimz web url        # print the room's URL without touching the server
rimz web status     # whether the server is running, and where
rimz web stop       # stop the server
```

`rimz web` is `rimz web open`. It ensures the room exists, starts `zellij web` when it is offline, prints the URL and a login token, and opens your browser. This is the whole of the local feature: your browser connects straight to Zellij's server on `127.0.0.1`, and RimZ relays no terminal traffic of its own.

The browser lands on Zellij's "Security Token Required" page. Paste the token RimZ printed in the terminal, and the room loads with its sidebar and panes. The [token section](#the-login-token) covers where that token lives and how to rotate it.

`rimz web url` prints the route for a room that already exists without starting or touching the server, so a script never receives a URL that would spin up a bare Zellij session with no sidebar.

## Open a remote room in your browser

From your laptop, add `--web` to a remote connect and RimZ tunnels the server's room into your local browser over SSH:

```sh
rimz remote connect dev --web              # open the remote room at 127.0.0.1
rimz remote connect dev --web --web-port 8443
```

The remote web server stays bound to the remote host's own loopback and never listens on a public interface. RimZ starts that server when needed, opens an SSH local-forward from a port on your laptop's `127.0.0.1` to the remote's `127.0.0.1:<web-port>`, and opens your browser at `http://127.0.0.1:<local-port>/<session>`. Your browser hits your own machine; SSH carries the bytes to the remote server. Nothing about the room is exposed on the network between the two hosts.

RimZ stays in the foreground supervising the tunnel until Ctrl-C, and the tunnel rides the same self-healing link as a terminal attach, so a dropped connection reconnects on its own. Without `--web-port`, RimZ derives a stable local port from the session name, so the browser origin and its cookies survive reconnects and repeat runs. The target and saved aliases behave exactly as for a terminal connect; see [Remote](./remote.md).

## The login token

A browser-attached room is shell access as the room's user, so Zellij gates it behind a login token, the same way it would for `zellij web` on its own. The token is Zellij's, and RimZ only manages it for you:

```sh
rimz web token create            # mint a login token (--read-only for a watcher)
rimz web token list              # token names and creation dates
rimz web token revoke <name>     # revoke one by name
rimz web token revoke-all        # revoke every token
```

The first time you serve a room, RimZ asks Zellij to mint a token (`zellij web --create-token` under the hood), then caches the value on the serving machine as plaintext JSON at mode `0600`, alongside Zellij's own hashed token store. Every later `rimz web` reuses that cached token instead of minting a fresh one, so the value you paste stays stable across opens. The token stays out of URLs, logs, and store events; treat it like an SSH private key on that machine.

For a local room the serving machine is your own, so the token is minted, cached, and printed right there in your terminal. For a remote room the serving machine is the remote host: RimZ reads that host's cached token over the same SSH connection and relays it to your local browser, so a remote session reuses the server's token rather than minting a new one on your laptop.

## Security boundary

The feature is safe to run because the exposure is narrow and the guard is Zellij's, not a RimZ invention. Two facts define the boundary:

- The server listens on `127.0.0.1` by default. Locally, only your machine reaches it. Remotely, only your SSH tunnel reaches it, because the remote server stays on the remote's loopback.
- Authentication is mandatory and it is Zellij's token gate. No token, no room.

Keep the rest in view:

- A read-only token is observation-only, but terminal output can still carry secrets, so a read-only viewer is not a safe way to share a room with sensitive scrollback.
- A revoke through RimZ clears the plaintext cache. A token revoked directly in Zellij leaves the cache in place, so revoke through RimZ, or delete the cache file, before the next `open`.
- To reach a room from beyond loopback, put HTTPS in front. A reverse proxy with rate limiting is the supported public shape:

```text
browser  ->  HTTPS reverse proxy with rate limiting  ->  zellij web on 127.0.0.1
```

## See also

- [Remote](./remote.md) — the SSH link the `--web` tunnel rides, saved aliases, and reconnect.
- [Web CLI reference](../reference/cli/web.md) — every `rimz web` subcommand and flag.
- [Configuration](./configuration.md#web-access) — the `[web]` keys and reverse-proxy `base_url`.
- [Web internals](../internals/web.md) — the token model, server lifecycle, and remote-tunnel mechanics.
- [Troubleshooting](./troubleshooting.md) — a room that will not start or serve.
</content>
</invoke>

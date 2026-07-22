# Web

A terminal attach works well while you have the right terminal open. When you need the same room from a browser, RimZ can put its existing Zellij or tmux session behind one local ttyd address without moving the room, agents, or state into another service.

## Install ttyd on the serving machine

Both Zellij and tmux browser rooms use [ttyd](https://github.com/tsl0922/ttyd):

```sh
brew install ttyd        # macOS or Linuxbrew
apt install ttyd         # Debian or Ubuntu
```

`rimz doctor` reports the resolved ttyd path and version. A normal `rimz start` still opens the terminal room when ttyd is absent; it prints the install fix because browser startup is best-effort.

## Serve a local room

```sh
rimz web            # ensure the room and shared daemon, print URL + credential, open browser
rimz web url        # print an existing room's URL without requiring the daemon
rimz web share      # share one live room as a read-only broadcast
rimz web unshare    # revoke one room's broadcast
rimz web start      # start the machine-wide daemon without targeting a room
rimz web restart    # restart active browser daemons with current config
rimz web status     # report writable and broadcast listeners
rimz web stop       # stop both browser daemons
```

`rimz web` is `rimz web open`.

RimZ resolves or births the room first, confirms that its mux session is live, and ensures one ttyd daemon bound to `127.0.0.1:8200` by default. Every Zellij and tmux room on the machine shares that process.

The printed route is `http://127.0.0.1:8200/?arg=<session>`. ttyd passes the selected session to a hidden RimZ shim, which accepts only a live session backed by a RimZ workspace record and attaches with the correct mux command. Changing the URL argument cannot run an arbitrary command.

The browser shows a Basic-Auth prompt. Use the printed user `rimz` and password. Add `--no-start` when a supervisor owns ttyd and the command should fail rather than start it.

With `[web] enabled = true`, every normal `rimz start` also asks for the shared daemon after the room is ready. A missing binary, occupied port, or daemon error warns on stderr and leaves the room usable in the terminal.

## Share a read-only broadcast

```sh
rimz web share [PATH] --print
rimz web share --session rimz-project-a1b2c3 --json
rimz web unshare [PATH]
rimz web unshare --session rimz-project-a1b2c3
rimz web unshare --all
```

`share` requires an already-live RimZ room, adds only that room to a durable allowlist, starts a second ttyd daemon on `127.0.0.1:8201` by default, and prints its viewer URL. The viewer daemon has no Basic-Auth prompt and drops all browser input; it cannot reach another live room by changing `?arg=` because its shim accepts only allowlisted sessions and gives the same generic refusal for unknown, stopped, and unshared names.

The viewer link is deliberately unauthenticated. Keep the listener on loopback for local viewing, or put HTTPS and any desired viewer authentication in a reverse proxy before exposing it. Set `share_base_url` to the proxy's public prefix; `auth_header` and `trusted_proxies` govern only the writable listener. A shared room on a non-loopback `interface` prints a warning that anyone who can reach `share_port` can watch.

`unshare` restarts the broadcast daemon while other rooms remain shared, which disconnects every existing viewer and lets still-shared tabs reconnect. Removing the last room or using `--all` stops the daemon. The allowlist survives `rimz web stop`; `rimz web status` shows both its retained sessions and whether the broadcast daemon is online.

tmux viewers attach read-only and with `ignore-size`, so they cannot type or resize the presenter's layout. Zellij has no read-only or size-isolated attach mode: ttyd still blocks viewer input, but a viewer window resize can influence the shared Zellij session geometry.

## Browser appearance and input

RimZ gives ttyd the active theme and configured browser font when the daemon starts. `JetBrainsMono Nerd Font Mono` and `CaskaydiaCove Nerd Font Mono` are built-in presets: RimZ downloads verified regular and bold faces and caches them under `$XDG_CACHE_HOME/rimz/web-fonts`.

Set `font_source` to a local `.ttf`, `.otf`, `.woff`, or `.woff2` file, or to an HTTPS URL. A family with no preset and no source asks the browser to resolve an installed font. `style_client = false` keeps ttyd's browser colors while retaining keyboard, cursor, clipboard, and reconnect fixes.

The compatibility layer keeps the cursor steady when terminal apps request blinking while preserving their requested cursor shapes, preserves Shift+Enter and macOS Option-as-Meta input, sends tmux copy-mode yanks and Shift-drag selections to the clipboard, refreshes xterm after a downloaded font loads, and renders pixel pets plus the pixel context meter in qualifying tmux rooms. Missing font bytes warn and fall back to monospace. Zellij rooms, tmux below 3.6, disabled passthrough, a stock-page fallback, or any attached plain terminal use sextant cell art instead.

A browser tab kept open while RimZ upgrades can retain the previous compatibility page until it reloads. Reload that tab after an upgrade to converge it with the new shared daemon.

Appearance is fixed when the shared daemon starts. After changing `[theme]` or web styling, run:

```sh
rimz web restart
```

## Behind a reverse proxy

A reverse proxy can terminate HTTPS and let an Authentik forward-auth decision identify the user while ttyd keeps its machine-wide Basic Auth behind that public edge. Set Authentik's Traefik forward-auth middleware to return `X-Authentik-Username` in its auth response headers, attach that middleware to the router serving RimZ, and make the proxy overwrite or remove any client-supplied copy of that header before forwarding.

Point RimZ at the public URL and name the header the proxy injects:

```toml
[web]
base_url = "https://shell.example.com/rimz"
auth_header = "X-Authentik-Username"
```

When Traefik runs on the same host and reaches the host through a Docker bridge, expose the listener and admit only that bridge CIDR:

```toml
[web]
base_url = "https://shell.example.com/rimz"
auth_header = "X-Authentik-Username"
interface = "0.0.0.0"
trusted_proxies = ["172.18.0.0/16"]
```

RimZ starts Basic-authenticated ttyd on an ephemeral loopback port and a small authorization gate on `0.0.0.0:8200`. The gate accepts only loopback or configured source CIDRs, requires a non-empty `X-Authentik-Username` on every HTTP request, strips client-supplied `Authorization`, and presents ttyd's Basic credential upstream. Use the proxy host's address or subnet for a proxy on another LAN or VPC host. Keep the host firewall restricted to the same sources; `trusted_proxies` sees the TCP peer address, so configure the CIDR for the address that actually reaches RimZ after container and host networking.

Restart after changing the auth or listener shape:

```sh
rimz web restart
```

The gate accepts any non-empty value in the configured header after validating the peer address, so make the authenticating proxy the only non-loopback source that can reach the port. An empty `trusted_proxies` list accepts only a proxy connecting from loopback. The gate admits loopback as a source but still requires the header there; the private ttyd listener separately requires Basic Auth.

The trusted-header decision applies only at the public gate. `rimz remote connect --web` tunnels through SSH directly to the private ttyd listener and uses the printed machine credential, so it works the same way in Basic and trusted-header configurations.

## Open a remote room

```sh
rimz remote connect dev --web
rimz remote connect dev --web --web-port 8443
```

RimZ uses one SSH prep call to birth or resume the remote room, ensure its shared daemon, and return the credential plus the private tunnel target. It then forwards a local loopback port to the remote ttyd listener and opens `http://127.0.0.1:<local-port>/?arg=<session>`. This path is uniform whether the remote public edge uses Basic or trusted-header auth.

The tunnel stays in the foreground and follows the normal remote recovery policy. Recovery repeats prep, so a stopped daemon comes back and a rotated credential is printed again; the local URL stays stable. Without `--web-port`, the local port derives from the session in 8300–8399 and scans forward when busy.

## Credentials

```sh
rimz web token create
rimz web token list
rimz web token revoke rimz
rimz web token revoke-all
```

One credential named `rimz` serves the whole machine in every auth mode. `create` rotates it and restarts the live daemon and gate. Either revoke command stops the daemon and clears the credential.

ttyd read-only mode belongs to the whole process, so `rimz web token create --read-only` points to the separate `rimz web share` broadcast instead of presenting a misleading per-user permission.

Treat the password like an SSH private key. It stays out of the URL, logs, events, and workspace records.

## Configuration

```toml
[web]
enabled = true
port = 8200
share_port = 8201
interface = "127.0.0.1"
# base_url = "https://devbox.example/rimz"
# share_base_url = "https://watch.example/rimz"
# auth_header = "X-Authentik-Username"
# trusted_proxies = ["172.18.0.0/16"]
font = "JetBrainsMono Nerd Font Mono"
# font_source = "/path/to/font.woff2"
style_client = true
```

`interface` selects the bind address for both daemons; `port` selects the writable listener and `share_port` selects the read-only broadcast listener. `base_url` and `share_base_url` change the respective prefixes RimZ prints when a reverse proxy fronts RimZ; the `/?arg=<session>` query remains. `auth_header` puts a proxy-validated identity header at the writable authorization gate while ttyd retains Basic Auth, and non-empty `trusted_proxies` admits those source addresses to that gate.

## Security boundary

By default, RimZ invokes ttyd with write access, origin checks, mandatory Basic Auth, and an explicit loopback bind.

The one machine credential authenticates the shared listener, so it grants access to every live RimZ room on that machine rather than only the room named in the first URL. An authenticated client can submit another session argument, and a missing or rejected argument lists the live RimZ rooms. A remote `--web` tunnel forwards this same machine-wide surface through its local port.

The broadcast listener is a separate process without `-W` or `-c`: ttyd drops input and admits connections without authentication, while RimZ's per-connection shim limits attachment to the durable room allowlist and never lists other rooms. Its output can still contain secrets. Bind it to loopback or place it behind a reverse proxy and firewall before public exposure.

The browser session is shell access as the serving user, and terminal output can contain secrets. Treat either the credential or trusted-header boundary as machine-wide shell access. Put HTTPS and rate limiting in front before exposing the listener beyond loopback:

```text
browser -> HTTPS authenticating proxy -> authorization gate -> Basic-authenticated loopback ttyd
```

## See also

- [Remote](./remote.md) — saved aliases and reconnect behavior.
- [Web CLI reference](../reference/cli/web.md) — every subcommand and flag.
- [Configuration](./configuration.md#web-access) — per-machine settings.
- [Web internals](../internals/web.md) — daemon argv, state, validation, and remote wire format.
- [Troubleshooting](./troubleshooting.md) — missing ttyd and failed starts.

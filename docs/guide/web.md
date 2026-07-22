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
rimz web start      # start the machine-wide daemon without targeting a room
rimz web restart    # restart it with the current binary and config
rimz web status     # report the daemon's pid and configured port
rimz web stop       # stop the daemon
```

`rimz web` is `rimz web open`.

RimZ resolves or births the room first, confirms that its mux session is live, and ensures one ttyd daemon bound to `127.0.0.1:8200` by default. Every Zellij and tmux room on the machine shares that process.

The printed route is `http://127.0.0.1:8200/?arg=<session>`. ttyd passes the selected session to a hidden RimZ shim, which accepts only a live session backed by a RimZ workspace record and attaches with the correct mux command. Changing the URL argument cannot run an arbitrary command.

The browser shows a Basic-Auth prompt. Use the printed user `rimz` and password. Add `--no-start` when a supervisor owns ttyd and the command should fail rather than start it.

With `[web] enabled = true`, every normal `rimz start` also asks for the shared daemon after the room is ready. A missing binary, occupied port, or daemon error warns on stderr and leaves the room usable in the terminal.

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

ttyd read-only mode belongs to the whole process, so `rimz web token create --read-only` is rejected rather than presenting a misleading per-user permission.

Treat the password like an SSH private key. It stays out of the URL, logs, events, and workspace records.

## Configuration

```toml
[web]
enabled = true
port = 8200
interface = "127.0.0.1"
# base_url = "https://devbox.example/rimz"
# auth_header = "X-Authentik-Username"
# trusted_proxies = ["172.18.0.0/16"]
font = "JetBrainsMono Nerd Font Mono"
# font_source = "/path/to/font.woff2"
style_client = true
```

`interface` and `port` select the exact public listener. `base_url` changes the prefix RimZ prints when a reverse proxy fronts RimZ; the `/?arg=<session>` query remains. `auth_header` puts a proxy-validated identity header at the authorization gate while ttyd retains Basic Auth, and non-empty `trusted_proxies` admits those source addresses to the gate.

## Security boundary

By default, RimZ invokes ttyd with write access, origin checks, mandatory Basic Auth, and an explicit loopback bind.

The one machine credential authenticates the shared listener, so it grants access to every live RimZ room on that machine rather than only the room named in the first URL. An authenticated client can submit another session argument, and a missing or rejected argument lists the live RimZ rooms. A remote `--web` tunnel forwards this same machine-wide surface through its local port.

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

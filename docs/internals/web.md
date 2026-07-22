# Web access

> See [DESIGN.md](../../DESIGN.md) and [multiplexers.md](./multiplexers.md) for the commitments this doc extends.

RimZ serves every local Zellij and tmux room through one authenticated ttyd daemon, with a loopback listener by default and an optional source-address gate for reverse proxies.

## Contract

The daemon owns browser transport and rendering; RimZ owns room birth, session validation, attach argv, credential state, URL construction, diagnostics, and remote SSH forwarding.

The store, hooks, sidebar, and wake paths are unchanged for a browser client, and RimZ proxies no pane I/O.

`[web] interface` and `port` select one exact listener, default `127.0.0.1:8200`. A live daemon serves every room on both backends, so mux choice affects only the attach command selected after a browser connects.

## Daemon

Basic Auth uses this structurally fixed argv:

```text
ttyd -W -O -a -c rimz:<secret> -i 127.0.0.1 -p <port> [-t <client-option>...] [-I <custom-index>] <current-rimz-exe> web exec
```

Trusted-header auth replaces `-c rimz:<secret>` with `-H <header>`. `-W` enables input, `-O` enforces origin checks, `-a` appends URL `arg` values to the command, and `-i` selects the configured IP listener.

An empty `trusted_proxies` list binds ttyd directly to `<interface>:<port>`. A non-empty list starts ttyd first on `127.0.0.1:<ephemeral>`, waits for that upstream, starts the hidden detached `rimz web gate` process on the configured listener, waits for the public listener, and only then writes daemon state. Startup tears down every process already started when a later step fails.

The gate parses the configured bare IPs and IPv4 or IPv6 CIDRs before any process change. It accepts loopback peers and peers inside a matching same-family CIDR, drops every other connection, and splices accepted TCP streams to ttyd without parsing HTTP or reading pane data.

Room URLs have the shape `<base>/?arg=<percent-encoded-session>`. ttyd appends the decoded session to the hidden shim as `rimz web exec <session>`.

The shim accepts only a session with a durable RimZ workspace record and a matching live mux session. It never treats the browser argument as an argv fragment. A valid tmux target execs `tmux -S <managed-socket> attach -t <session>`; a valid Zellij target execs `zellij attach <session>`. A missing, unknown, or stopped target prints the currently live RimZ sessions and exits 1.

The ttyd binary resolves from `RIMZ_TTYD_BIN`, then `PATH`. A missing binary reports the Homebrew and apt install fix. `interface` must parse as an IP address, each trusted proxy must parse as an IP or CIDR, and an occupied configured listener returns a typed error that points to `[web] port`.

RimZ spawns ttyd and the optional gate with null stdio and their own process groups, then writes `$XDG_STATE_HOME/rimz/web-ttyd.json` with `pid`, `port`, `interface`, `auth`, `trusted_proxies`, and optional `gate: {pid, upstream_port}`. Old `{pid, port}` records deserialize as Basic Auth on loopback without a gate. The record is live only while ttyd is the recorded process, the optional gate is a recorded `rimz web gate` process, and the configured listener accepts a connection; readers remove stale records.

The desired listener, auth mode, and proxy list participate in daemon reuse. Any drift stops the old processes and starts the desired shape; Basic mode also requires the credential file before reuse.

State transitions hold `$XDG_STATE_HOME/rimz/web-ttyd.lock`, so concurrent room starts converge on one process and credential rotation cannot race stale-record cleanup.

The first shared-daemon start after an upgrade consumes the old `$XDG_STATE_HOME/rimz/web-ttyd/` per-session records. RimZ sends SIGTERM only when a recorded pid still names `ttyd`, then removes the legacy directory; malformed records, recycled pids, and cleanup errors are debug diagnostics and do not block the new daemon.

`rimz web stop` sends SIGTERM to the gate and ttyd, waits one second while refreshing the process table, uses SIGKILL for a survivor, waits for the public listener to close, and removes the record.

## Credential and browser client

In Basic mode, the one credential named `rimz` lives at `$XDG_STATE_HOME/rimz/web-ttyd-credential.json`, mode 0600, with `name`, `created_at`, and `secret`.

Rotation stops and restarts the one live daemon so the old secret stops working immediately. Revocation stops the daemon and removes the credential. ttyd read-only mode is process-wide, so RimZ rejects read-only credential creation.

Trusted-header mode neither reads nor mints the credential, and its JSON payload omits `credential`. `rimz web token create` refuses while `auth_header` is set and tells the user to unset it to return to Basic Auth; list and revoke retain their normal file behavior.

The daemon always passes `macOptionIsMeta=true` and `cursorBlink=false`. With `style_client = true`, it also projects the shared theme into xterm.js options and resolves the configured font.

The built-in Nerd Font families use SHA-256-pinned regular and bold faces. HTTPS custom sources use a URL-hashed cache entry, local sources are read directly, and supported files end in `.ttf`, `.otf`, `.woff`, or `.woff2`. Font bytes live under `$XDG_CACHE_HOME/rimz/web-fonts`; `RIMZ_WEB_FONTS_OFFLINE` makes resolution cache-only.

ttyd serves no additional static route, so RimZ caches a generated index under `$XDG_CACHE_HOME/rimz/web-ttyd`. A cache miss starts a throwaway loopback ttyd on an ephemeral port, fetches its stock `/` page with temporary Basic Auth, stops it, and injects the font faces plus the compatibility bootstrap.

The bootstrap refreshes xterm after fonts load, keeps the cursor steady across reconnects, preserves Shift+Enter and macOS Meta chords, bridges OSC 52 and browser selections to the clipboard, and restyles disconnect and resize overlays. A font or index failure warns and falls back without blocking the daemon.

## Commands and room start

`rimz web open` resolves or births the room, confirms the session is addressable, ensures the shared daemon, and returns its URL and auth mode plus a credential in Basic mode. `--no-start` requires an already-live daemon.

`rimz web url` reads the room identity, existing credential, and live daemon state without changing the daemon or credential. It uses the live port when the daemon runs and the configured port otherwise; its v2 JSON omits `credential` when none exists. `start`, `status`, and `stop` act on the one machine daemon and need no room target.

After a normal `rimz start` makes the room ready, `[web] enabled = true` asks RimZ to ensure the daemon. This path is deliberately best-effort: missing ttyd, a port collision, or a start failure prints a warning and never refuses the room.

## Configuration

`[web]` carries `enabled`, `interface`, `port`, `base_url`, `auth_header`, `trusted_proxies`, `font`, `font_source`, and `style_client`.

`enabled` defaults to true, `interface` defaults to `127.0.0.1`, `port` defaults to 8200, and an absent `base_url` resolves to `http://127.0.0.1:<port>`. A reverse proxy can set `base_url` to its public prefix; RimZ appends `/?arg=<session>`.

A non-empty trimmed `auth_header` selects trusted-header auth, while an empty or absent value selects Basic Auth. `trusted_proxies` is empty by default; setting it enables the gate even in Basic mode. Trusted-header auth on a non-loopback interface without the gate returns a warning that names the CIDR or firewall fixes.

The section is per-machine and stays outside the trust hash because no field executes a command. `font_source` is a read-only local path or HTTPS URL.

## Remote rooms

Remote prep is one non-PTY `rimz web open --print --json` call. Its additive `rimz.web.v2` payload includes `auth: {mode: "basic"}` or `auth: {mode: "trusted_header", header: "<name>"}`; missing `auth` defaults to Basic for older v2 peers, and Basic payloads include `credential: {username, secret}`.

The local side checks the exact schema, prints the returned Basic-Auth credential, chooses a local port, and forwards it to `127.0.0.1:<remote-port>`. There is no second token-provisioning SSH call. A trusted-header payload fails before tunnel setup and directs the user to its reverse-proxy URL because an SSH port forward cannot inject the required header.

The local port derives from the session in 8300–8399 and scans on collision. Recovery repeats prep so it can rebirth the room, restart the daemon, discover a changed port, and print a changed secret while keeping the local URL stable. Version skew uses the existing remote-upgrade diagnostic; v1 payloads are not accepted.

## Security

The default listener binds to loopback and requires Basic Auth.

ttyd treats a configured auth header as proof of authentication when it is present and non-empty; it does not validate the value or filter source addresses. Any client that reaches an ungated listener can spoof the header and receive a shell, so a non-loopback trusted-header listener uses `trusted_proxies` or an equivalent firewall boundary that admits only the authenticating proxy.

The gate always trusts loopback because local processes can already reach its loopback ttyd upstream. On a multi-user host, any local user who can connect and supply the configured header can reach the shell as the RimZ-serving user; use host-level user isolation when that boundary matters.

Credentials stay out of URLs, logs, store events, and workspace records. The v2 credential appears only in explicit JSON output that reports a saved credential and the human stderr relay.

The browser session is shell access as the serving user. A reverse proxy that exposes the listener provides HTTPS and rate limiting.

# Web access

> See [DESIGN.md](../../DESIGN.md) and [multiplexers.md](./multiplexers.md) for the commitments this doc extends.

RimZ serves every local Zellij and tmux room through one Basic-authenticated writable ttyd daemon and serves explicitly shared rooms through a separate unauthenticated, input-blocked broadcast daemon; both bind loopback by default.

## Contract

The daemon owns browser transport and rendering; RimZ owns room birth, session validation, attach argv, credential state, URL construction, diagnostics, and remote SSH forwarding.

The store, hooks, sidebar, and wake paths are unchanged for a browser client, and RimZ proxies no pane I/O.

`[web] interface` selects both bind addresses, `port` selects the writable listener at 8200 by default, and `share_port` selects the broadcast listener at 8201 by default. The writable daemon can reach every room after machine-wide authentication; the broadcast daemon reaches only its durable allowlist.

## Daemon

ttyd uses this structurally fixed authentication argv in every mode:

```text
ttyd -W -O -a -c rimz:<secret> -i 127.0.0.1 -p <port> [-t <client-option>...] [-I <custom-index>] <current-rimz-exe> web exec
```

`-W` enables input, `-O` enforces origin checks, `-a` appends URL `arg` values to the command, and `-i` selects ttyd's listener. Trusted-header authentication changes the edge in front of ttyd; it never changes ttyd's Basic-authenticated core.

Basic mode with an empty `trusted_proxies` list binds ttyd directly to `<interface>:<port>`. A non-empty list or trusted-header mode starts ttyd first on `127.0.0.1:<ephemeral>`, waits for that upstream, starts the hidden detached `rimz web gate` process on the configured listener, waits for the public listener, and only then writes daemon state. Startup tears down every process already started when a later step fails.

The gate parses the configured bare IPs and IPv4 or IPv6 CIDRs before any process change. It accepts loopback peers and peers inside a matching same-family CIDR and drops every other connection. Basic mode splices accepted TCP streams unchanged. Trusted-header mode parses each HTTP request head, requires exactly one occurrence of the configured header with a non-empty trimmed value, optionally requires a byte-exact case-sensitive match in `auth_users`, removes any client `Authorization`, and injects `Authorization: Basic <machine-credential>` before forwarding; a rejected identity receives 401, request bodies with `Content-Length` pass unchanged, chunked requests close, keep-alive requests are checked independently, and a WebSocket upgrade switches to a raw splice. Responses pass unchanged.

Room URLs have the shape `<base>/?arg=<percent-encoded-session>`. ttyd appends the decoded session to the hidden shim as `rimz web exec <session>`.

The shim accepts only a session with a durable RimZ workspace record and a matching live mux session. It never treats the browser argument as an argv fragment. On terminal stdio, a valid target enters the session manager's auto-attach path: it runs the tmux or Zellij attach as a child, then presents the list when that child ends. Non-terminal stdio keeps exec replacement with `tmux -S <managed-socket> attach -t <session>` or `zellij attach <session>`; the separately validated share shim also keeps its read-only exec path.

A missing, unknown, or stopped target on terminal stdio opens the themed session manager. It joins durable workspace records with one live mux probe, sorts and filters by displayed repository name and path, and attaches the selected room as an inherited-stdio child after releasing raw input while preserving the alternate screen. The picker emits private OSC 7717 with the attached session before the child starts and an empty session whenever the list takes over; the browser mirrors that target in `?arg=` so reconnect and refresh continuity follow the current view, stale targets clear when the list opens, and every detach returns to the list. Agent counts, attention, and the configured headline spend window read through `PublishedSnapshotReader`, keeping one incremental consumer cursor per live session and degrading an unreadable snapshot to an unenriched card. Non-terminal stdio prints the same live-session listing and exits 1.

The ttyd binary resolves from `RIMZ_TTYD_BIN`, then `PATH`. A missing binary reports the Homebrew and apt install fix. `interface` must parse as an IP address, each trusted proxy must parse as an IP or CIDR, `auth_users` requires a non-empty `auth_header`, and every user must remain non-empty after trimming. These config preconditions fail before process changes with a typed error that names the fix, and an occupied configured listener returns a typed error that points to `[web] port`.

RimZ spawns ttyd and the optional gate with null stdio and their own process groups, then writes `$XDG_STATE_HOME/rimz/web-ttyd.json` with `pid`, `port`, `interface`, `auth`, `auth_users`, `trusted_proxies`, `basic_upstream`, optional `gate: {pid, upstream_port}`, and optional `pixel_protocol`. `basic_upstream` proves that ttyd uses the layered Basic-auth contract, and `pixel_protocol` is present only when the live daemon serves a generated page with RimZ's current pixel compatibility layer. Records written before `basic_upstream` deserialize it as false and are replaced before reuse; records without `auth_users` default to an empty allowlist. The record is live only while ttyd is the recorded process, the optional gate is a recorded `rimz web gate` process, and the configured listener accepts a connection; readers remove stale records.

The desired listener, auth mode, user allowlist, proxy list, gate presence, and Basic-upstream marker participate in daemon reuse. Any drift stops the old processes and starts the desired shape, and every mode requires the credential file before reuse.

State transitions hold `$XDG_STATE_HOME/rimz/web-ttyd.lock`, so concurrent room starts converge on one process and credential rotation cannot race stale-record cleanup.

The first shared-daemon start after an upgrade consumes the old `$XDG_STATE_HOME/rimz/web-ttyd/` per-session records. RimZ sends SIGTERM only when a recorded pid still names `ttyd`, then removes the legacy directory; malformed records, recycled pids, and cleanup errors are debug diagnostics and do not block the new daemon.

`rimz web restart` performs the same stop under the daemon lock when a daemon is online and always starts a fresh process with the current binary and browser profile. `rimz web stop` sends SIGTERM to the gate and ttyd, waits one second while refreshing the process table, uses SIGKILL for a survivor, waits for the public listener to close, and removes the record.

## Broadcast daemon

The read-only surface uses a second ttyd process with structurally separate argv:

```text
ttyd -O -a -i <interface> -p <share_port> [-t <client-option>...] [-I <custom-index>] <current-rimz-exe> web exec --share
```

The missing `-W` makes ttyd 1.7 and later drop client input, and the missing `-c` makes the viewer URL unauthenticated. `auth_header`, `auth_users`, `trusted_proxies`, the authorization gate, credentials, and remote `--web` forwarding apply only to the writable daemon. The broadcast process receives the same theme, font, reconnect fixes, and pixel-compatible custom index.

`$XDG_STATE_HOME/rimz/web-share.json` stores `{ "sessions": [...] }` through temp-file plus rename. `share` validates a durable workspace record and live mux session before adding one sorted session and ensuring the daemon. The hidden share shim re-reads the file for every connection, repeats the record and liveness checks, and returns the single `this room is not shared` error for missing, unknown, unshared, and dead targets without listing other rooms.

A valid tmux broadcast execs `tmux -S <managed-socket> attach -t <session> -r -f ignore-size` when the probed tmux supports client flags; `-r` blocks mux input as defense in depth and `ignore-size` excludes the viewer from window sizing. A valid Zellij broadcast execs the ordinary `zellij attach <session>` because Zellij has no read-only attach; ttyd remains the input boundary, and viewer geometry can influence the session.

The daemon record at `$XDG_STATE_HOME/rimz/web-ttyd-share.json` carries `pid`, `port`, `interface`, and optional `pixel_protocol`; `$XDG_STATE_HOME/rimz/web-ttyd-share.lock` serializes allowlist and process transitions. A record is live only while its pid names ttyd and the listener accepts a connection. Listener or pixel-protocol drift replaces the process before reuse. Both writable and broadcast records participate in the tmux pixel-client ancestry check.

Removing one session rewrites the allowlist and restarts the daemon so every existing viewer disconnects; still-shared browser tabs can reconnect through ttyd. Removing the final session or using `unshare --all` stops the process. `web stop` stops both daemons but retains the allowlist, `web restart` restarts the broadcast daemon when that list is non-empty, and `rimz reload` replaces each browser daemon only when it is online.

## Credential and browser client

The one credential named `rimz` lives at `$XDG_STATE_HOME/rimz/web-ttyd-credential.json`, mode 0600, with `name`, `created_at`, and `secret`. ttyd requires it in every auth mode, and a trusted-header gate reads the file at startup to precompute the injected Basic authorization value; the secret stays off gate argv.

Rotation stops and restarts the live writable daemon so the old secret stops working immediately. Revocation stops that daemon and removes the credential. ttyd read-only mode is process-wide, so RimZ rejects read-only credential creation and directs users to the separate broadcast process.

Credential creation, rotation, listing, and revocation have the same behavior in Basic and trusted-header modes. Rotation restarts both ttyd and its gate, so the gate's startup read receives the new secret.

The daemon always passes `macOptionIsMeta=true` and `cursorBlink=false`. With `style_client = true`, it also projects the shared theme into xterm.js options and resolves the configured font.

The built-in Nerd Font families use SHA-256-pinned regular and bold faces. HTTPS custom sources use a URL-hashed cache entry, local sources are read directly, and supported files end in `.ttf`, `.otf`, `.woff`, or `.woff2`. Font bytes live under `$XDG_CACHE_HOME/rimz/web-fonts`; `RIMZ_WEB_FONTS_OFFLINE` makes resolution cache-only.

ttyd serves no additional static route, so RimZ caches a generated index under `$XDG_CACHE_HOME/rimz/web-ttyd`. A cache miss starts a throwaway loopback ttyd on an ephemeral port, fetches its stock `/` page with temporary Basic Auth, stops it, and injects the font faces plus the compatibility bootstrap.

The bootstrap refreshes xterm after fonts load, keeps the cursor steady across reconnects and app-emitted blink sequences while preserving requested cursor shapes, preserves Shift+Enter and macOS Meta chords, bridges OSC 52 and browser selections to the clipboard, keeps the browser URL and reconnect target on the attached room through private OSC 7717, restyles disconnect and resize overlays, and installs a bounded Kitty graphics compatibility layer. It swallows dead-key compositions immediately after a handled Option chord, pins xterm's cursor-blink option steady while native handling keeps requested cursor shapes, and holds the last cursor cell as a short-lived overlay across application redraw hides, bounded at 300 milliseconds. A font or index failure warns and falls back without blocking the daemon.

The generated page marks each U+10EEEE plus Kitty row/column combining-mark cluster invisible as it enters xterm, then restores visibility before following text. Xterm still retains the complete placeholder cluster and RGB image id in its buffer, while its WebGL renderer paints the cell's real background instead of a carrier-colored fallback glyph. The pixel layer consumes RimZ's transmit, virtual-placement, and delete subset before xterm parses it, retains at most 128 decoded PNGs by image id, and draws each image through a clip path made only from its placeholder cells on a DPR-scaled overlay canvas. Each redraw reads the cells' row and column marks plus RGB image id, preserving image aspect and logical origin while keeping paint inside the visible placement through scroll, resize, reconnect, partial diffs, and tmux client switches.

The tmux capability probe accepts an `xterm-256color` rendering client when its pid ancestry reaches the live ttyd pid recorded with the current `pixel_protocol`. The generated bootstrap participates in the cache key, so a browser-script or profile-schema change generates a fresh cached page; a pixel-protocol change replaces the stale shared daemon at the next start. Stock-page fallback omits the field and stays sextant-capable only. A browser tab kept open across a RimZ upgrade can retain the previous page generation against the replacement daemon; reload the page to converge it.

## Commands and room start

`rimz web open` resolves or births the room, confirms the session is addressable, ensures the shared daemon, and returns its URL, auth mode, credential, and tunnel target. `--no-start` requires an already-live daemon.

`rimz web url` reads the room identity, existing credential, and live daemon state without changing the daemon or credential. It uses the live port when the daemon runs and the configured port otherwise; its v2 JSON omits `credential` when none exists. `share` and `unshare` own the broadcast allowlist; `restart`, `status`, and `stop` cover both daemon records. `rimz reload` restarts each daemon when it is online so a newly installed build supplies its current browser client; an offline daemon stays offline, and a restart failure warns without failing reload.

After a normal `rimz start` makes the room ready, `[web] enabled = true` asks RimZ to ensure the daemon. This path is deliberately best-effort: missing ttyd, a port collision, or a start failure prints a warning and never refuses the room.

## Configuration

`[web]` carries `enabled`, `interface`, `port`, `share_port`, `base_url`, `share_base_url`, `auth_header`, `auth_users`, `trusted_proxies`, `font`, `font_source`, and `style_client`.

`enabled` defaults to true, `interface` defaults to `127.0.0.1`, `port` defaults to 8200, and `share_port` defaults to 8201. Absent base URLs resolve to `http://127.0.0.1:<respective-port>`; a reverse proxy can set either public prefix, and RimZ appends `/?arg=<session>`.

A non-empty trimmed `auth_header` selects trusted-header auth and always enables the gate, while an empty or absent value selects direct Basic Auth unless `trusted_proxies` enables the gate. `auth_users` defaults empty and permits any single non-empty identity; a non-empty list requires trusted-header mode and matches the request's trimmed identity byte-for-byte against entries trimmed during spec construction. `trusted_proxies` is empty by default. Trusted-header auth on a non-loopback interface with an empty proxy allowlist warns that only loopback proxies can connect and names the CIDR fix for a proxy on another host.

The section is per-machine and stays outside the trust hash because no field executes a command. `font_source` is a read-only local path or HTTPS URL.

## Remote rooms

Remote prep is one non-PTY `rimz web open --print --json` call. Its additive `rimz.web.v2` payload includes `auth: {mode: "basic"}` or `auth: {mode: "trusted_header", header: "<name>"}`, `credential: {username, secret}`, and `tunnel_port`. Missing `auth` defaults to Basic and missing `tunnel_port` falls back to `port` for older v2 peers.

The local side checks the exact schema, prints the returned Basic-Auth credential, chooses a local port, and forwards it to `127.0.0.1:<tunnel_port>`. For a gated daemon, this lands directly on ttyd's loopback Basic-authenticated upstream; for a direct daemon, `tunnel_port` equals the public `port`. There is no second token-provisioning SSH call. A legacy trusted-header payload without a credential still fails before tunnel setup and directs the user to its reverse-proxy URL.

The local port derives from the session in 8300–8399 and scans on collision. Recovery repeats prep so it can rebirth the room, restart the daemon, discover a changed port, and print a changed secret while keeping the local URL stable. Version skew uses the existing remote-upgrade diagnostic; v1 payloads are not accepted.

## Security

The default listener binds to loopback and requires Basic Auth.

The gate treats a configured auth header as proof of the public proxy's authentication only after the peer address passes the loopback-or-allowlist check. It strips client authorization and presents the machine credential to ttyd itself, so ttyd never trusts a public identity header.

The source gate always admits loopback, but trusted-header authorization still requires the configured header there; loopback carries no header-auth bypass. The private ttyd upstream remains protected by Basic Auth. Use host-level user isolation when another local user can read the serving user's credential file or execute as that user.

Credentials stay out of URLs, logs, store events, and workspace records. The v2 credential appears only in explicit JSON output that reports a saved credential and the human stderr relay.

The broadcast listener intentionally has no RimZ authentication and prints a visible warning whenever an active share binds a non-loopback interface. Its allowlist limits rooms rather than viewers; anyone who can reach the listener can read the terminal output of every allowlisted room. Public deployments put HTTPS, optional viewer authentication, and network filtering in front of `share_port`.

The browser session is shell access as the serving user. A reverse proxy that exposes the listener provides HTTPS and rate limiting.

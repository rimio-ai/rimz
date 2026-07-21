# Web access

> See [DESIGN.md](../../DESIGN.md) and [multiplexers.md](./multiplexers.md) for the commitments this doc extends.

RimZ serves every local Zellij and tmux room through one authenticated ttyd daemon bound to the machine loopback interface.

## Contract

The daemon owns browser transport and rendering; RimZ owns room birth, session validation, attach argv, credential state, URL construction, diagnostics, and remote SSH forwarding.

The store, hooks, sidebar, and wake paths are unchanged for a browser client, and RimZ proxies no pane I/O.

`[web] port` selects one exact listener, default `8200`. A live daemon serves every room on both backends, so mux choice affects only the attach command selected after a browser connects.

## Daemon

The structurally fixed argv is:

```text
ttyd -W -O -a -c rimz:<secret> -i 127.0.0.1 -p <port> [-t <client-option>...] [-I <custom-index>] <current-rimz-exe> web exec
```

`-W` enables input, `-O` enforces origin checks, `-a` appends URL `arg` values to the command, `-c` requires Basic Auth, and `-i` keeps the listener on loopback.

Room URLs have the shape `<base>/?arg=<percent-encoded-session>`. ttyd appends the decoded session to the hidden shim as `rimz web exec <session>`.

The shim accepts only a session with a durable RimZ workspace record and a matching live mux session. It never treats the browser argument as an argv fragment. A valid tmux target execs `tmux -S <managed-socket> attach -t <session>`; a valid Zellij target execs `zellij attach <session>`. A missing, unknown, or stopped target prints the currently live RimZ sessions and exits 1.

The ttyd binary resolves from `RIMZ_TTYD_BIN`, then `PATH`. A missing binary reports the Homebrew and apt install fix. A configured port held by a process outside the recorded daemon returns a typed error that points to `[web] port`.

RimZ spawns ttyd with null stdio and its own process group, waits up to five seconds for the configured port, then writes `$XDG_STATE_HOME/rimz/web-ttyd.json` as `{pid, port}`. The record is live only while the pid exists and its loopback port accepts a connection; readers remove stale records.

State transitions hold `$XDG_STATE_HOME/rimz/web-ttyd.lock`, so concurrent room starts converge on one process and credential rotation cannot race stale-record cleanup.

The first shared-daemon start after an upgrade consumes the old `$XDG_STATE_HOME/rimz/web-ttyd/` per-session records. RimZ sends SIGTERM only when a recorded pid still names `ttyd`, then removes the legacy directory; malformed records, recycled pids, and cleanup errors are debug diagnostics and do not block the new daemon.

`rimz web stop` sends SIGTERM, waits one second while refreshing the process table, uses SIGKILL for a survivor, and removes the record.

## Credential and browser client

The one credential named `rimz` lives at `$XDG_STATE_HOME/rimz/web-ttyd-credential.json`, mode 0600, with `name`, `created_at`, and `secret`.

Rotation stops and restarts the one live daemon so the old secret stops working immediately. Revocation stops the daemon and removes the credential. ttyd read-only mode is process-wide, so RimZ rejects read-only credential creation.

The daemon always passes `macOptionIsMeta=true` and `cursorBlink=false`. With `style_client = true`, it also projects the shared theme into xterm.js options and resolves the configured font.

The built-in Nerd Font families use SHA-256-pinned regular and bold faces. HTTPS custom sources use a URL-hashed cache entry, local sources are read directly, and supported files end in `.ttf`, `.otf`, `.woff`, or `.woff2`. Font bytes live under `$XDG_CACHE_HOME/rimz/web-fonts`; `RIMZ_WEB_FONTS_OFFLINE` makes resolution cache-only.

ttyd serves no additional static route, so RimZ caches a generated index under `$XDG_CACHE_HOME/rimz/web-ttyd`. A cache miss starts a throwaway loopback ttyd on an ephemeral port, fetches its stock `/` page with temporary Basic Auth, stops it, and injects the font faces plus the compatibility bootstrap.

The bootstrap refreshes xterm after fonts load, keeps the cursor steady across reconnects, preserves Shift+Enter and macOS Meta chords, bridges OSC 52 and browser selections to the clipboard, and restyles disconnect and resize overlays. A font or index failure warns and falls back without blocking the daemon.

## Commands and room start

`rimz web open` resolves or births the room, confirms the session is addressable, ensures the shared daemon, and returns its URL and credential. `--no-start` requires an already-live daemon.

`rimz web url` reads the room identity and computes the configured URL without requiring the daemon to run. `start`, `status`, and `stop` act on the one machine daemon and need no room target.

After a normal `rimz start` makes the room ready, `[web] enabled = true` asks RimZ to ensure the daemon. This path is deliberately best-effort: missing ttyd, a port collision, or a start failure prints a warning and never refuses the room.

## Configuration

`[web]` carries `enabled`, `port`, `base_url`, `font`, `font_source`, and `style_client`.

`enabled` defaults to true, `port` defaults to 8200, and an absent `base_url` resolves to `http://127.0.0.1:<port>`. A reverse proxy can set `base_url` to its public prefix; RimZ appends `/?arg=<session>`.

The section is per-machine and stays outside the trust hash because no field executes a command. `font_source` is a read-only local path or HTTPS URL.

## Remote rooms

Remote prep is one non-PTY `rimz web open --print --json` call. Its `rimz.web.v2` payload is `{version, url, session, port, credential: {username, secret}}`.

The local side checks the exact schema, prints the returned Basic-Auth credential, chooses a local port, and forwards it to `127.0.0.1:<remote-port>`. There is no second token-provisioning SSH call.

The local port derives from the session in 8300–8399 and scans on collision. Recovery repeats prep so it can rebirth the room, restart the daemon, discover a changed port, and print a changed secret while keeping the local URL stable. Version skew uses the existing remote-upgrade diagnostic; v1 payloads are not accepted.

## Security

The production listener binds to loopback and requires authentication.

Credentials stay out of URLs, logs, store events, and workspace records. The v2 credential appears only in the explicit JSON prep response and the human stderr relay.

The browser session is shell access as the serving user. A reverse proxy that exposes the listener provides HTTPS and rate limiting.

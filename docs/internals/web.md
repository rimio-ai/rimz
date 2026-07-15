# Web access

> See [DESIGN.md](../../DESIGN.md) and [multiplexers.md](./multiplexers.md) for the commitments this doc extends.

RimZ exposes one browser-access command and dispatches transport and authentication to the room's backend.

## Contract

Normal mux resolution selects the engine: explicit `--mux` wins, otherwise a live session's backend applies before the active mux environment, configured default, and installed binaries.

`WebEngine` serializes as `zellij` or `ttyd` in `rimz.web.v1`; deserialization defaults a missing field to `zellij` for older remote peers.

RimZ owns workspace resolution, room birth, URL construction, local credential caches, diagnostics, and remote SSH tunneling.

The serving engine owns terminal transport, browser rendering, connections, and cookies.

The store, hooks, sidebar, and wake paths are unchanged for a browser client, and RimZ proxies no pane I/O.

The `web` module exposes one concrete `WebEngine` lifecycle seam for preflight, open, inspection, status, stop, and credentials; its private Zellij and ttyd modules own subprocesses, parsing, generated config, credential state, and instance inventory, while CLI handlers own room orchestration and presentation.

## Zellij engine

Zellij uses one machine-wide `zellij web` server, normally at `127.0.0.1:8082`.

`open` checks `zellij web --status`, starts it with `--daemonize` when allowed, grants the presence plugin's web permissions, and sends the runtime sharing pipe for the room.

RimZ merges the configured font and active palette into a generated `web_client` KDL file when `style_client` is enabled.

The plaintext token cache is `$XDG_STATE_HOME/rimz/web-login-token.json`, mode 0600; Zellij retains its own hashed token store.

## ttyd engine

tmux uses one ttyd process per room.

The process argv is structurally fixed:

```text
ttyd -W -O -c rimz:<secret> -i 127.0.0.1 -p <port> -b /<session> tmux attach -t <session>
```

`-W` enables terminal input, `-O` enforces origin checks, `-c` makes Basic Auth mandatory, `-i` keeps the listener on loopback, and `-b` gives both engines the uniform `<base>/<session>` URL.

The ttyd binary resolves from `RIMZ_TTYD_BIN`, then `PATH`; a missing binary returns the Homebrew and apt install fix before a room server can start.

Ports derive deterministically from the session name in 8200–8299 and scan forward on collision.

RimZ spawns ttyd with null stdio and its own process group, waits up to five seconds for the loopback port, then records the instance.

The credential cache is `$XDG_STATE_HOME/rimz/web-ttyd-credential.json`, mode 0600, with `name`, `created_at`, and `secret`.

One credential named `rimz` serves the machine; rotation snapshots the sorted live inventory once, stops that exact batch, and restarts those sessions in the same order so the old secret stops working immediately.

Instance records live under `$XDG_STATE_HOME/rimz/web-ttyd/<encoded-session>.json` with `session`, `pid`, and `port`; readers continue to accept old records with extra fields.

An instance is live only when its pid exists and its loopback port accepts a connection; one inventory read snapshots the process table, probes every record, sorts live sessions, and discards stale records.

`stop` sends SIGTERM to the full batch, shares one one-second grace window while refreshing the process table once per poll, then uses SIGKILL for survivors and removes their records.

## Commands

`open` and `url` are room-scoped and engine-aware.

`status` and `stop` cover the machine-wide Zellij server plus every recorded ttyd instance.

`start` remains the Zellij server verb because a ttyd server requires a target room.

Token verbs operate on Zellij's token store or ttyd's single machine credential; ttyd read-only creation is rejected because read-only is a process-wide ttyd flag.

## Configuration

`[web] enabled` gates opening either engine before room sharing proceeds.

`[web.zellij]` carries `base_url`, `auto_start`, `font`, and `style_client`.

`[web.tmux]` carries `base_url` and `auto_start`.

Both sections are per-machine and stay outside the trust hash because no field executes a command.

## Remote rooms

Remote prep remains one non-PTY `rimz web open --print --json` call, so the remote host resolves its own room backend and returns `engine`, `session`, and listener `port`.

The local side runs hidden `web token ensure` with `--mux zellij` or `--mux tmux`, relays either Zellij's token or ttyd's user `rimz` and password, and then starts the same SSH local-forward supervisor.

The local tunnel port derives from the session in 8300–8399, scans on collision, and forwards to `127.0.0.1:<remote-port>`.

An old remote omits `engine` and is interpreted as Zellij; an old local peer can tunnel a new ttyd payload but cannot word or select the credential relay correctly, so the existing version-upgrade diagnostic remains the recovery path.

## Security

Both production argv paths bind to loopback and require authentication.

Credentials stay out of URLs, logs, store events, and workspace records.

Reverse proxies that expose a listener beyond loopback provide HTTPS and rate limiting.

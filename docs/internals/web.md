# Web access

RimZ serves its rooms to a browser through a writable daemon and a broadcast daemon. The writable gate reaches every live Zellij and tmux room behind named Basic-auth tokens or trusted-header authentication, forwarding to a private ttyd listener. The broadcast daemon has no authentication, drops browser input, and reaches only rooms on its allowlist. Both public listeners bind loopback by default.

ttyd owns the HTTP and WebSocket transport and the xterm.js page. RimZ owns everything around it: validating the room a URL names, the attach argv, the token store, the always-on writable authorization gate, the generated browser page, the daemon records, and remote SSH forwarding. A browser client is one more multiplexer client: the store, hooks, sidebar, and waits see nothing different, and RimZ proxies no pane I/O.

The [web guide](../guide/web.md) shows what a user sees, and the [web CLI reference](../reference/cli/web.md) lists every subcommand and flag.

## Source map

| File | Owns |
| --- | --- |
| [`web/mod.rs`](../../crates/rimz/src/web/mod.rs) | The public entry points, `WebErr`, per-verb payloads, session validation, room URLs, the OSC 7717 sequence, and the launch-context scrub list. |
| [`web/ttyd.rs`](../../crates/rimz/src/web/ttyd.rs) | ttyd resolution and the version floor, the token store, both daemon records and locks, spawn, reuse, stop, and the broadcast allowlist. |
| [`web/gate.rs`](../../crates/rimz/src/web/gate.rs) | The source-address, named-token and trusted-header gate, and the remote tunnel relay. |
| [`web/ttyd/client.rs`](../../crates/rimz/src/web/ttyd/client.rs) | ttyd client options, theme projection, font resolution, and the generated index page with its bootstrap script. |
| `web/ttyd/*.js` | The bootstrap modules: `ws_url.js` (room to `arg`), `mouse_flow.js` (motion pacing and drag re-arm), `input_guard.js` (Option chords and Shift+Enter), `pixel_layer.js` (Kitty graphics). |
| [`cli/web/mod.rs`](../../crates/rimz/src/cli/web/mod.rs) | The `rimz web` handlers, including the hidden `exec` and `gate` subcommands. |
| [`cli/sessions/picker.rs`](../../crates/rimz/src/cli/sessions/picker.rs) | The session picker shared by `rimz sessions` and browser sessions. |
| [`config/web.rs`](../../crates/rimz/src/config/web.rs) | `WebPrefs`, the `[web]` section. |
| [`remote/web.rs`](../../crates/rimz/src/remote/web.rs), [`cli/remote/web.rs`](../../crates/rimz/src/cli/remote/web.rs) | The local relay port (`bind_local_relay`) and the remote prep, relay, and tunnel rounds. |

## Daemon

### Argv

Both daemons run the current `rimz` binary under ttyd, with different flags:

```text
writable:  ttyd -W -O -a -P 3600 -c rimz-upstream:<secret> -i 127.0.0.1 -p <ephemeral> [-t <option>...] [-I <index>] <rimz> web exec
broadcast: ttyd    -O -a -P 3600                  -i <interface> -p <share_port> [-t <option>...] [-I <index>] <rimz> web exec --share
```

`spawn_spec_for` builds both. The flags that matter:

| Flag | Effect |
| --- | --- |
| `-W` | Accepts browser input. The broadcast daemon omits it, so ttyd drops every keystroke. |
| `-O` | Enforces WebSocket origin checks. |
| `-a` | Appends the URL's `arg` query values to the command, which is how the session name reaches `rimz web exec`. |
| `-P 3600` | Pings each WebSocket once an hour. See below. |
| `-c rimz-upstream:<secret>` | Basic auth with the private upstream credential. Only the writable daemon has it. |
| `-i`, `-p` | The writable upstream is always `127.0.0.1` and an ephemeral port, and its gate uses the configured interface and port. Broadcast uses the configured interface and share port directly. |
| `-t`, `-I` | Client options and the generated index page (see [the browser client](#the-browser-client)). |

The ping interval is long on purpose. ttyd hands libwebsockets a hang-up deadline of the interval plus 7 seconds, and a browser that has stopped draining a saturated stream can miss its pong. At ttyd's 5-second default (10-second hang-up), a busy terminal closes and silently reattaches. ttyd has no special case for `-P 0`: it sets a 0-second ping and a 7-second hang-up, so zero never disables the check (`tsl0922/ttyd@1.7.7:src/server.c::main`).

| | Writable | Broadcast |
| --- | --- | --- |
| Port | `[web] port` (8200) | `[web] share_port` (8201) |
| Authentication | Named Basic tokens or trusted header at the gate | None |
| Rooms reachable | Every room with a workspace record and a live session | Rooms on the allowlist |
| Record | `web-ttyd.json` | `web-ttyd-share.json` |
| Lock | `web-ttyd.lock` | `web-ttyd-share.lock` |
| Attach | `tmux -S <socket> -u attach -t <session>` or `zellij attach <session>` (`attach_existing_command`) | tmux adds `-r -f ignore-size`, and Zellij attaches normally (`attach_readonly_command`) |

Every state file lives under `~/.rimz/web/` (`disk::paths::web_dir`, which follows `RIMZ_HOME`).

### Starting

`ensure_daemon` (writable) and `ensure_broadcast_locked` (broadcast) converge the machine on one process that matches the current config. The writable path runs in this order:

1. Validate config with `desired_spec`: `interface` must parse as an IP address, each `trusted_proxies` entry must parse as an IP or CIDR, `auth_users` requires a non-empty `auth_header`, and no user may be empty after trimming. Each failure is a typed `WebErr` that names the fix.
2. Resolve ttyd from `RIMZ_TTYD_BIN`, then `PATH`, and require `ttyd --version` to parse at or above `MIN_TTYD_VERSION` (1.7.5). A missing binary reports the Homebrew and apt install, and an old or unparseable version reports the floor and the upgrade.
3. Take `web-ttyd.lock`, so concurrent room starts converge on one process and a credential rotation cannot race stale-record cleanup.
4. Read the record and check it is live (see [the record](#the-record)). A stale record is removed, and any of its processes still running are terminated.
5. Check that the configured listener is free unless the live record already holds it (`prepare_fresh_start`). An occupied port is `ConfiguredPortInUse`, which points at `[web] port`. Then build the client profile, which may generate the index page.
6. Reuse the live daemon if its record matches the desired shape and the token store is nonempty. Otherwise mint the default token if none exists, stop the live daemon, check the port again, and start fresh (`start_fresh_locked`).

A fresh start binds ttyd to `127.0.0.1:<ephemeral>` and waits up to 5 seconds (`START_TIMEOUT`) for it, then spawns the hidden `rimz web gate` on the configured listener, pointed at that upstream. It waits up to 5 seconds more for the public listener, writes the record only after it accepts, and stops every process it started when a later step fails.

Both ttyd processes and the gate spawn with null stdio in their own process group. Before spawning ttyd, `without_ttyd_launch_context` removes multiplexer membership through `CommandSpec::without_mux_context`, then the launching pane's room, worktree, and agent variables, the client size, and remote-attach markers through `TTYD_AMBIENT_CONTEXT_ENV`. Machine environment such as `HOME`, `XDG_*`, `PATH`, locale, logging, and the `RIMZ_*_BIN` overrides passes through, so every browser attach sees the machine and none sees the pane that happened to start the daemon.

The broadcast path is the same without the gate or the credential. It validates only `interface`, and its port check reports `[web] share_port`.

### The record

`web-ttyd.json` (`WritableDaemonRecord`) is written with `write_temp_then_rename_cache`. Its fields:

| Field | Meaning |
| --- | --- |
| `pid`, `port`, `interface` | ttyd's pid and the public listener, which the gate holds. |
| `auth` | `{"mode":"basic"}` or `{"mode":"trusted_header","header":"<name>"}`. |
| `auth_users` | The trimmed identity allowlist, omitted when empty. |
| `trusted_proxies` | The proxy list as configured. |
| `gate` | `{pid, upstream_port}`: the gate process and ttyd's private port. |
| `token_auth` | The gate supports the token store and ttyd requires its separate upstream credential. |
| `launch_context_scrubbed` | ttyd was spawned without the launch pane's environment. |
| `pixel_protocol` | Present only when ttyd serves the generated page with the current `TTYD_PIXEL_PROTOCOL`. |
| `index_key` | The generated page's cache key. |

`web-ttyd-share.json` (`TtydProcessRecord`) carries `pid`, `port`, `interface`, `launch_context_scrubbed`, `pixel_protocol`, and `index_key`.

A record is live only while its pid names `ttyd`, its listener accepts a TCP connection, and, when the record has a `gate`, the gate pid is a `rimz web gate` process (`daemon_status_locked`). A reader that finds any of these false terminates what survives and removes the record.

A live record is reused only when every desired value matches (`record_matches`): the listener, the auth mode, `auth_users`, `trusted_proxies`, a gate is present, both markers are set, and the `index_key` matches the page the current binary would generate. A record without `gate`, `token_auth`, or `launch_context_scrubbed` reads those as absent or false, so it never matches and is replaced. Any drift stops the old processes and starts the desired shape. An upgrade, font change, or ttyd version that changes the page therefore replaces the daemon at the next ensure. A missing or empty token store also forces replacement and default-token creation. The broadcast record compares the listener, its marker, and `index_key`.

### Who starts and stops them

| Caller | Writable daemon | Broadcast daemon |
| --- | --- | --- |
| `rimz start` with `[web] enabled = true` | Ensures it after the room is ready. Any failure, including a missing or old ttyd, prints `rimz: browser daemon was not started: …` and the room starts anyway. | Untouched. |
| `rimz web open` | Preflights ttyd, births the room if it is not live, waits up to 5 seconds for the session to be addressable, then ensures the daemon. | Untouched. |
| `rimz web open --no-start` | Runs the same preflight, birth, and wait, then requires a live record with `token_auth` and a token store. A record without `token_auth` fails with `TtydOffline` rather than returning an incompatible upstream pair. It skips the config-drift comparison. | Untouched. |
| `rimz web url` | Reads the record without credentials and changes nothing beyond stale-record cleanup. | Untouched. |
| `rimz web start` | Refuses when `[web] enabled = false`, and otherwise ensures it. | Untouched. |
| `rimz web restart` | Refuses when `[web] enabled = false`, and otherwise starts a fresh process, stopping the live one first. | Untouched when disabled. Otherwise restarts it when the allowlist is non-empty, or stops it. |
| `rimz web stop` | Stops it. | Stops it. The allowlist stays. |
| `rimz reload` | Restarts it only when it is online, whatever `[web] enabled` says. | Restarts it only when online, or stops it when the allowlist is empty. |
| `rimz web token create <NAME>` | Restarts it when online. | Untouched. |
| `rimz web token revoke <NAME>` | Restarts it when online, or stops it when no tokens remain. | Untouched. |
| `rimz web token revoke-all` | Stops it. | Untouched. |
| `rimz web share` | Untouched. | Ensures it. |
| `rimz web unshare` | Untouched. | Restarts it, or stops it when the list becomes empty. |
| `rimz web unshare --all` | Untouched. | Stops it. |

`rimz start` is best-effort because browser access is enrichment on a room the user asked to open. The explicit web commands treat the same checks as fatal preconditions. `rimz reload` warns instead of failing. Its restart path (`web::restart_if_online`) checks liveness before it validates config, so a config error there leaves a live daemon running and reports the error as a warning. A writable error also skips the broadcast restart, since the two run in sequence.

Stopping sends SIGTERM to the gate and ttyd, polls the process table for up to one second, sends SIGKILL to any survivor, waits up to one second for the public listener to close, and removes the record.

## The gate

`rimz web gate` (`gate::serve`) always holds the writable public listener, and ttyd listens only behind it. The gate takes its listener, upstream, `--allow` entries, and the trusted-header name and users on its argv. It reads the tokens and the upstream credential from the token store once at startup, so no secret appears on its argv. A token change therefore restarts the gate ([tokens](#tokens)).

Each connection passes three checks in order: the peer address, the request head limits, and authentication.

The peer check (`public_peer_allowed`) depends on the auth mode. In Basic mode with an empty `trusted_proxies` list it admits every peer, because the token authenticates the request whatever its source. With a nonempty list, or in trusted-header mode, it maps an IPv4-mapped IPv6 address back to IPv4, accepts loopback and any address inside a same-family allowlist entry, and closes everything else without a response. `--allow` entries are bare IPs or IPv4 and IPv6 CIDRs (`gate::Cidr`), and `desired_spec` has already parsed them before any process changed.

The head limits bound a peer that never authenticates:

| Limit | Value | Effect |
| --- | --- | --- |
| Head deadline | 30 s (`HEAD_DEADLINE`) | Each request head must arrive whole within it, measured across every read so a peer dripping bytes cannot extend it. The deadline is cleared before a body or an upgraded WebSocket is relayed. |
| Unauthenticated connections | 64 (`MAX_UNAUTHENTICATED`) | Further peers are closed at accept. A connection frees its slot when its first request is authorized. |
| Head size | 64 KiB (`MAX_REQUEST_HEAD`), 128 headers (`MAX_HEADERS`) | A larger or unparseable head closes the connection. |
| Upstream connect | 2 s (`UPSTREAM_CONNECT_TIMEOUT`) | The gate connects to ttyd only after the first request is authorized. |

A failure to spawn a connection thread drops that connection and leaves the gate running.

Authentication (`rewrite_request_head`) checks each request head, so keep-alive requests on one connection are each checked on their own:

1. In Basic mode, exactly one `Authorization` header must decode as a token name and matching secret. The comparison runs over every token with no early return on a mismatching byte (`constant_time_equal`). A failure gets `401 Unauthorized` with `WWW-Authenticate: Basic realm="RimZ"`, zero content length, and a closed connection.
2. In trusted-header mode, the configured header must appear exactly once, with a non-empty value after trimming. When `auth_users` is non-empty, that value must equal one entry byte for byte, case-sensitively. A failure gets `401 Unauthorized` with no `WWW-Authenticate` header, and the connection closes.
3. In either mode every client `Authorization` header is dropped and `Authorization: Basic <upstream-credential>` is added.
4. A body with `Content-Length` is forwarded unchanged. A chunked request, a malformed head, or conflicting lengths close the connection.
5. The response is relayed with its framing intact. A WebSocket upgrade that ttyd accepts with `101` turns the connection into a raw splice in both directions.

Loopback peers pass the address check but still need a token or the header. The gate turns a proxy's identity header into ttyd's Basic credential only after the peer passed the address check, and ttyd itself never trusts an identity header (the gate forwards it unchanged with the rest of the head).

When trusted-header auth binds a non-loopback interface with an empty proxy list, `auth_warnings` warns that only loopback proxies can connect and names the `[web] trusted_proxies` fix.

## Attaching a room

A room URL is `<base>/?room=<percent-encoded-session>` (`join_session_url`). ttyd only understands `arg`, so the generated page's `ws_url.js` rewrites the WebSocket URL: when the page has `?room=`, the socket carries `?arg=<session>`, and otherwise the page's own query passes through, so a `?arg=` link still works. ttyd appends the decoded value to the command, and the browser session runs `rimz web exec <session>`.

`rimz web exec` never treats the browser value as argv. `live_session_target` accepts it only when the session has a durable workspace record and a live session in one mux probe, and the attach command comes from that backend's `attach_existing_command`. What happens next depends on whether stdin and stdout are terminals, which they are under ttyd:

| Target | Terminal stdio | Non-terminal stdio |
| --- | --- | --- |
| Valid | The [session picker](#the-session-picker) attaches it as a child, then shows the room list when it detaches. | Emits the OSC 7717 room sync, runs the attach as a child, and exits with its status. |
| Unknown or stopped | The session picker opens with a notice naming the rejected session. | Prints the rejection and the live-session listing, and exits 1. |
| Missing or empty | The session picker opens with no notice. | Prints `no session was provided` and the listing, and exits 1. |

If the picker cannot set up the terminal, a valid target falls back to the non-terminal attach, and an invalid one to the listing error.

## The session picker

The picker in `cli/sessions/picker.rs` is the same list `rimz sessions` shows in a terminal. The browser runs it in `Mode::Web`, which adds the OSC 7717 room sync, and `Mode::Terminal` emits no browser sequences.

Every 2 seconds (`PROBE_INTERVAL`) the picker joins durable workspace records with one live mux probe. Rooms with a prompt in the last 24 hours (the newest `turn_started_at` among non-subagent agents) rank first by that prompt time, and the rest follow by the workspace record's `updated_at`. `/` opens a filter over the repository display name and path, and with it closed `j` and `k` move the selection and `n` opens the new-room view. Each card's agent counts per kind, attention count, and headline spend window come from the room's published snapshot through `PublishedSnapshotReader`, one reader per live session. An unreadable snapshot leaves the card without them.

On a full-size screen the list box is 40% of the terminal width, clamped to 58 through 84 columns, and 24 rows tall beneath a RIMZ banner. A screen too small for the banner gets the compact full-frame layout.

Selecting a room hands the terminal off without leaving the alternate screen or turning off mouse reporting (`TerminalModeGuard::handoff_keep_screen`), runs the attach as a child with inherited stdio, and restores the picker when the child exits. Keeping both closes the gap in which a terminal turns a residual wheel tick into an arrow key for the attach. A failed or non-zero attach becomes a notice on the list.

The browser tab follows the picker through private OSC 7717 sequences (`session_sync_osc`): `rimz-session=<session>` and `rimz-name=<repo>`, both percent-encoded. They are written before each attach and cleared whenever the list takes over. The page's handler rewrites the URL's `?room=`, drops any `?arg=`, and sets the tab title to `<repo> · RimZ`, or `RimZ` on the list. A reload or reconnect therefore returns to the current view, and a stale target is cleared as soon as the list opens.

The new-room view lists workspaces with a record and no live session, then the non-hidden subdirectories of the current directory, which starts at `$HOME`. Confirming a path calls the detached room birth (`ensure_workspace_room_detached`) and attaches the new session as a child. That path skips the ttyd preflight, which only `rimz web open` runs, so `rimz sessions` can create a room on a machine without ttyd. A birth error returns to the picker with a notice.

## Sharing a room

The broadcast allowlist is `web-share.json`, `{"sessions": [...]}`, kept sorted and deduplicated and written with temp-file plus rename. All allowlist and broadcast process changes hold `web-ttyd-share.lock`.

`rimz web share` requires `[web] enabled`, a workspace record, and a live session. It adds the session, ensures the broadcast daemon, and restores the previous allowlist if the daemon fails to start. The share URL uses `share_base_url` and the `rimz.web.share.v1` payload.

The share shim, `rimz web exec --share <session>` (`web::share_attach_command`), re-reads the allowlist on every connection and repeats the record and liveness checks. Every rejection, whether the session is missing, unknown, unshared, or dead, returns the same `this room is not shared` error, so a viewer learns nothing about other rooms. The shim never opens the picker: it writes the OSC 7717 room sync and attaches.

A tmux target attaches with `-r`, which blocks mux input as a second barrier, and with `-f ignore-size`, so the viewer's window size does not resize the room. Both flags are within RimZ's tmux floor, so the shim adds them unconditionally. Zellij has no read-only attach: the shim runs a normal `zellij attach`, ttyd is the only input barrier, and the viewer's size can affect the session.

Revocation disconnects viewers by restarting the process. `unshare` of one room rewrites the allowlist, stops the daemon, and starts a fresh one if rooms remain. Viewers of still-shared rooms reconnect through ttyd. Removing the last room, or `unshare --all`, stops it.

## Tokens

`web-tokens.json` (`TokenStore`) has version `rimz.web.tokens.v1`, an `upstream` username/secret pair, and `tokens` sorted by unique name, each with `name`, `created_at`, and a 24-character `secret`. It is written mode 0600 through `write_private_temp_then_rename`. Names are 1-64 ASCII letters, digits, dots, underscores, or hyphens, starting with a letter or digit. A store that fails to parse, or has the wrong version or upstream username, an empty secret, or an invalid, duplicate, or unsorted name, reports `TtydJson` with the path. `ensure_daemon` mints `rimz` whenever the token list is empty, in either auth mode.

The upstream username is `rimz-upstream`. Its secret is minted with the store and rotated on every token change, including while offline (`apply_tokens_locked`). ttyd's `/token` endpoint hands that credential to any browser that has signed in, so rotation also invalidates a revoked user's saved upstream credential.

A machine with only the older `web-ttyd-credential.json` migrates on first read (`read_tokens`): its password becomes token `rimz`, the new store is published, then the old file is removed.

| Command | Effect |
| --- | --- |
| `token create <NAME>` | Refuses duplicates, creates a secret, and restarts the online writable daemon so its gate reads the new token set. |
| `token show [NAME]` | Prints only that password, for `rimz` by default. |
| `token list` | Prints sorted names and creation times. JSON includes no secrets. |
| `token revoke <NAME>` | Refuses a missing name. Restarts an online writable daemon with remaining tokens, or stops it when none remain. |
| `token revoke-all` | Stops the writable daemon and clears the token list. |

These commands behave the same in Basic and trusted-header modes. An online token change stops the old daemon before publishing the new token set, then attempts a restart. If restart preflight or startup fails, the change remains saved and the daemon stays stopped, and the command reports a warning. Neither a configuration error nor a missing ttyd can leave the old gate accepting a revoked token.

## The browser client

`client::profile` builds the ttyd options and the page. Every daemon gets `-t macOptionIsMeta=true`, `-t cursorBlink=false`, `-t titleFixed=RimZ`, and `-t disableLeaveAlert=true`. With `[web] enabled = false`, that is all: no theme, no font, and no generated page.

With `style_client = true`, the profile adds `fontFamily=<font>,monospace` and a `theme=` object projected from the resolved [palette](./theme.md#resolving-a-palette), and resolves font faces (`resolve_font`). A non-empty `font_source` wins over the built-in families:

- A `font_source` starting with `https://` is fetched once and cached under the SHA-256 of the URL. Any other scheme is refused. A local path is read directly, with `~` expanded. The face must end in `.ttf`, `.otf`, `.woff`, or `.woff2` and be at most 16 MiB, and it is registered as weight 400 only.
- Without a `font_source`, a `font` naming a built-in family (`JetBrainsMono Nerd Font Mono` or `CaskaydiaCove Nerd Font Mono`) downloads the Nerd Fonts v3.4.0 regular and bold faces, checks each against a pinned SHA-256, and caches them.
- Any other `font` embeds no face. The browser gets only the family name and falls back to `monospace` unless the viewer has the font installed.

Fonts cache under `~/.rimz/cache/web-fonts/`. With `RIMZ_WEB_FONTS_OFFLINE` set to any value, nothing is downloaded: a face that is not cached warns, and a local `font_source` path is still read.

### The generated page

ttyd answers only `/`, `/token`, and `/ws`, and `-I` replaces the `/` page without adding a route, so fonts and scripts have to live inside that page. `ensure_custom_index` keys the page by a SHA-256 over the `CUSTOM_INDEX_SCHEMA` string, the full bootstrap script, the ttyd version string, the font family, and each face's extension, weight, and byte digest (`custom_index_key`, 32 hex characters). It caches the page as `~/.rimz/cache/web-ttyd/index-<key>.html`. On a cache miss it starts a throwaway ttyd on a loopback ephemeral port with a temporary credential, fetches the stock `/` page, stops it, and injects a `<style>` block (the `@font-face` rules and the overlay restyle) before `</head>` and the bootstrap script before `</body>`.

Because the bootstrap script is part of the key, any change to it produces a new page and a new `index_key`, and the next ensure replaces either daemon. A tab left open across that replacement keeps the old page until it reloads.

Every page failure only warns. If the stock page cannot be fetched, lacks a `</head>` or `</body>` marker, or cannot be cached, the daemon starts on ttyd's stock page with neither `pixel_protocol` nor `index_key` in its record. Theme and font failures warn the same way.

### What the bootstrap does

The script reaches into xterm.js private API (`term._core` and its `coreService`, `optionsService`, and `coreMouseService`), so a ttyd release that bundles a different xterm.js can break it. The ttyd version string is part of the page key, so such a release gets a fresh page at the next ensure. The script waits up to 30 seconds for `window.term`, then installs:

| Behaviour | Mechanism |
| --- | --- |
| Room routing | `ws_url.js` wraps `WebSocket` to map `?room=` to `arg` (see [attaching a room](#attaching-a-room)). |
| Tab URL and title | An OSC 7717 handler (see [the session picker](#the-session-picker)). |
| Clipboard | OSC 52 writes and every xterm selection change go to `navigator.clipboard`. |
| Option chords | `input_guard.js` sends `ESC <key>` for Option plus a letter or digit, swallows the following `keypress`, and blocks a dead-key composition that starts within 250 ms on xterm's root before the textarea can forward the accent. |
| Shift+Enter | Sent as `CSI 13;2u`. |
| Steady cursor | `cursorBlink` is forced back to false whenever an app turns it on, and cursor shapes are kept. When an app hides the cursor, the last cursor cell is drawn as an overlay for up to 300 ms. |
| Wheel | xterm's wheel-to-arrow-keys fallback is suppressed while the alternate screen has no mouse protocol. |
| Font refresh | After the faces load, the font family is reset, the texture atlas cleared, and the terminal refreshed so Nerd Font glyphs replace fallback boxes. |
| Overlays | ttyd's disconnect and resize overlays are restyled, and `Press ⏎ to Reconnect` reads `Press Enter to reconnect`. |
| Mouse pacing and drag re-arm | `mouse_flow.js`, below. |
| Kitty graphics | `pixel_layer.js`, below. |

The handlers survive `term.reset`, which reinstalls the key, wheel, and cursor handling.

### Mouse pacing and drag re-arm

tmux can repaint the whole pane for each drag coordinate, and ttyd keeps its output pump running after the browser sends a flow-control pause: its PTY starts with a `paused` flag that resume never clears, so every later pause returns early (`tsl0922/ttyd@1.7.7:src/pty.c::pty_pause`, unchanged since 1.7.5). A fast drag therefore floods the browser with repaints and the pane-border resize lags far behind the pointer. `mouse_flow.js` paces motion reports on the WebSocket send path, with the interval tuned at a 179x38 terminal. The first report goes out immediately, motion slower than one report per 50 ms (`MOTION_INTERVAL_MS`) passes through, and faster motion keeps only the newest coordinate for the rest of each 50 ms slice. A button press or release, or any non-mouse input, flushes the pending coordinate first, so a drag ends exactly where the pointer stopped. Reads from the WebSocket are never delayed, and each new socket resets the pacing state.

tmux clears every xterm mouse mode before re-enabling the set it wants whenever a pane changes tracking mode. xterm.js 5.4.0 and 5.5.0 remove their document-level held-drag listeners on `?1002l`, and the following `?1002h` restores the protocol flags without reinstalling the listeners, which only a mousedown does (`xtermjs/xterm.js@5.4.0:src/browser/Terminal.ts::Terminal::bindMouse`). A held drag across such a mode change would otherwise stop reporting.

`installMouseDragRearm` repairs it. It tracks the primary button in its own capture-phase listeners and subscribes to `coreMouseService.onProtocolChange`. When drag reporting returns while the button is still held, it dispatches one synthetic mousedown to reinstall xterm's listeners, and swallows the press report that mousedown produces before it reaches the pacing state or the socket. A release that lands while xterm's mouseup listener is detached (with reporting off, or before the re-arm runs) never reaches xterm, so re-arm remembers it. Once drag reporting returns, re-arm follows the swallowed mousedown with a synthetic mouseup at the last pointer position, so tmux does not keep a drag the user already ended. If the private `coreMouseService` API is missing, re-arm installs nothing and records `rearm-unavailable`. The live test `backend::web::tmux_mouse_mode_churn_keeps_held_drag_tracking` then fails, and `tests/fixtures/mouse-flow/harness.mjs` covers the same logic without a browser.

Adding `rimzdebug=1` to the page URL exposes the pacing state and the last 256 decisions (sends, coalesces, flushes, re-arms, swallowed presses, replayed releases) at `window.__rimzWeb`.

### The pixel layer

`pixel_layer.js` renders the sidebar's Kitty graphics in the browser. It wraps `term.write`, so it sees output before xterm parses it, and consumes RimZ's subset of the Kitty protocol: PNG transmits (`a=t`, `f=100`, including chunked continuations), virtual placements (`a=p`, `U=1`), and deletes by image id (`a=d`, `d=i` or `d=I`). Both delete forms remove the image and its placement. It keeps at most 128 decoded images, and each image is limited to 4 MiB of payload.

Each placeholder cell (U+10EEEE plus the row and column combining marks) is wrapped in SGR 8 and 28 as it enters xterm. xterm keeps the full cluster and the RGB image id in its buffer, while its WebGL renderer paints the cell's real background instead of a fallback glyph. A DPR-scaled canvas above the screen draws each image clipped to the cells of its placement. A placement of more than one row scales the image to fit with its aspect kept, centred horizontally and aligned to the bottom row. A one-row placement stretches the image across its cells.

Render, scroll, and resize events share one animation-frame scan. The scan skips rows without a placeholder, reads the row and column marks and image id of the rest, and leaves the canvas alone when the visible scene matches the painted one. A moved placeholder, a new image or placement, a resize, a reconnect, or a tmux client switch invalidates the scene and repaints it.

The sidebar only sends Kitty graphics to a tmux client it trusts to render them. A browser client qualifies when it descends from a live ttyd whose record carries the current `pixel_protocol`. The full rule is the `kitty_clients` row in [pets.md](./sidebar/pets.md#capability-probes). A daemon on the stock page has no `pixel_protocol`, so browser clients get sextant cell art.

## Configuration

`[web]` (`WebPrefs`) is per-machine. It is outside the trust hash because no field executes a command.

| Key | Default | Effect |
| --- | --- | --- |
| `enabled` | `true` | Allows `rimz web open`, `share`, `start`, and `restart`, the ensure on `rimz start`, and the styled client. Does not stop a running daemon: use `rimz web stop`. `status`, `stop`, and all `token` operations, including revocation, remain usable while disabled. |
| `interface` | `127.0.0.1` | Bind address for both daemons. Must parse as an IP address. |
| `port` | `8200` | Writable listener. |
| `share_port` | `8201` | Broadcast listener. |
| `base_url` | unset | Public prefix for writable URLs. Unset means `http://127.0.0.1:<port>`. RimZ appends `/?room=<session>`. |
| `share_base_url` | unset | The same for broadcast URLs, with `share_port`. |
| `auth_header` | unset | A non-empty trimmed value selects trusted-header sign-in at the always-on gate. |
| `auth_users` | empty | Identity allowlist for trusted-header auth. Empty accepts any single non-empty identity. |
| `trusted_proxies` | empty | IPs or CIDRs admitted besides loopback. An empty list admits all peers in Basic mode, but only loopback in trusted-header mode. |
| `font` | `JetBrainsMono Nerd Font Mono` | Browser font family. |
| `font_source` | unset | Local path or HTTPS URL of a custom face. |
| `style_client` | `true` | Projects the theme and font into the browser. |

## Remote rooms

`rimz remote connect <target> --web` opens a remote room in a local browser. [remote.md](./remote.md#web-tunnels) owns the supervision rounds, and this section owns the payload, the relay, and its port.

Each round prepares the remote side with one non-PTY `rimz web open --print --json`. The `rimz.web.v2` payload (`WebOpenPayload`) carries `url`, `session`, `port`, `tunnel_port`, `auth`, and `credential: {username, secret}`. `tunnel_port` is ttyd's private port (`gate.upstream_port`) and the credential is the upstream pair, so the tunnel lands on ttyd's Basic-authenticated listener and never on the gate. A payload without `auth` reads as Basic, and one without `tunnel_port` tunnels to `port` (`WebOpenPayload::tunnel`). Any other `version` fails with an error telling the user to upgrade the remote binary. A trusted-header payload without a credential fails before any tunnel is set up and points at its reverse-proxy URL.

The local side runs two loopback ports:

1. The relay port, which the browser uses. `bind_local_relay` derives it from a CRC32 of the session name in 8300 through 8399, scans upward with wraparound on collision, and keeps the listener bound so no other process can take the port between selection and use. `--web-port` binds exactly that port instead.
2. An ephemeral forward port, which SSH forwards to `127.0.0.1:<tunnel_port>` on the remote.

The relay (`gate::serve_tunnel`) accepts only loopback peers, drops any `Authorization` header from the browser, and adds the returned Basic credential to every request before forwarding it to the SSH forward. Safari needs this: it sends the WebSocket upgrade without the cached Basic credential, and ttyd authenticates the upgrade (`web/gate/tests.rs::safari_websocket_without_authorization_is_injected_and_spliced` pins the shape). The user never sees a password prompt, and no second SSH call is needed to fetch a token.

The relay stays bound across recovery rounds. Each round repeats prep, so it can rebirth the room, restart the daemon, or pick up a changed port or credential, opens a fresh forward, and swaps the relay's target atomically. The local URL stays the same for the whole connection.

## Security boundaries

The browser session is shell access as the serving user. The defaults keep it on loopback behind Basic auth, and a deployment that exposes it puts HTTPS and rate limiting in a reverse proxy in front.

- The writable ttyd always requires the separate upstream credential. The [gate](#the-gate) authenticates named tokens or a proxy's identity header, then presents the upstream credential to ttyd itself. No peer address, loopback included, skips that authentication.
- The remote tunnel relay accepts unauthenticated requests on local loopback and presents the remote credential. That matches the trust boundary of a raw `ssh -L`: any process that can connect as, or run as, the tunnel owner can use it, so shared hosts need user isolation.
- Secrets stay out of URLs, store events, workspace records, and logs. Human-facing passwords appear only in `token create` and `token show`, and `open` points at `token show`. Only `open --json` includes the private upstream pair for the remote relay. URL, status, and token-list payloads contain no secrets. The remote tunnel holds its credential in memory and does not print it, with one exception: a prep reply that fails to parse as JSON is quoted, up to its first 300 bytes and unredacted, in the error (`cli/remote/web.rs::remote_output_context`).
- The broadcast listener has no authentication by design. Its allowlist limits which rooms are served, not who watches: anyone who reaches `share_port` can read every allowlisted room. RimZ warns each time it ensures or restarts the broadcast daemon on a non-loopback interface, and a public deployment puts HTTPS, viewer authentication, and network filtering in front of `share_port`.

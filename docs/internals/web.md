# Web access

RimZ serves its rooms to a browser through two machine-wide ttyd daemons. The writable daemon reaches every live Zellij and tmux room behind one Basic-auth credential. The broadcast daemon has no authentication, drops browser input, and reaches only rooms on its allowlist. Both bind loopback by default.

ttyd owns the HTTP and WebSocket transport and the xterm.js page. RimZ owns everything around it: validating the room a URL names, the attach argv, the credential, the optional authorization gate in front of ttyd, the generated browser page, the daemon records, and remote SSH forwarding. A browser client is one more multiplexer client: the store, hooks, sidebar, and waits see nothing different, and RimZ proxies no pane I/O.

The [web guide](../guide/web.md) shows what a user sees, and the [web CLI reference](../reference/cli/web.md) lists every subcommand and flag.

## Source map

| File | Owns |
| --- | --- |
| [`web/mod.rs`](../../crates/rimz/src/web/mod.rs) | The public entry points, `WebErr`, the `rimz.web.v2` payloads, session validation, room URLs, the OSC 7717 sequence, and the launch-context scrub list. |
| [`web/ttyd.rs`](../../crates/rimz/src/web/ttyd.rs) | ttyd resolution and the version floor, the credential, both daemon records and locks, spawn, reuse, stop, and the broadcast allowlist. |
| [`web/gate.rs`](../../crates/rimz/src/web/gate.rs) | The source-address and trusted-header gate, and the remote tunnel relay. |
| [`web/ttyd/client.rs`](../../crates/rimz/src/web/ttyd/client.rs) | ttyd client options, theme projection, font resolution, and the generated index page with its bootstrap script. |
| `web/ttyd/*.js` | The bootstrap modules: `ws_url.js` (room to `arg`), `mouse_flow.js` (motion pacing and drag re-arm), `input_guard.js` (Option chords and Shift+Enter), `pixel_layer.js` (Kitty graphics). |
| [`cli/web/mod.rs`](../../crates/rimz/src/cli/web/mod.rs) | The `rimz web` handlers, including the hidden `exec` and `gate` subcommands. |
| [`cli/sessions/picker.rs`](../../crates/rimz/src/cli/sessions/picker.rs) | The session picker shared by `rimz sessions` and browser sessions. |
| [`config/web.rs`](../../crates/rimz/src/config/web.rs) | `WebPrefs`, the `[web]` section. |

## Daemon

### Argv

Both daemons run the current `rimz` binary under ttyd, with different flags:

```text
writable:  ttyd -W -O -a -P 3600 -c rimz:<secret> -i <interface> -p <port> [-t <option>...] [-I <index>] <rimz> web exec
broadcast: ttyd    -O -a -P 3600                  -i <interface> -p <share_port> [-t <option>...] [-I <index>] <rimz> web exec --share
```

`spawn_spec_for` builds both. The flags that matter:

| Flag | Effect |
| --- | --- |
| `-W` | Accepts browser input. The broadcast daemon omits it, so ttyd drops every keystroke. |
| `-O` | Enforces WebSocket origin checks. |
| `-a` | Appends the URL's `arg` query values to the command, which is how the session name reaches `rimz web exec`. |
| `-P 3600` | Pings each WebSocket once an hour. See below. |
| `-c rimz:<secret>` | Basic auth with the machine credential. Only the writable daemon has it. |
| `-i`, `-p` | The listener. For a gated writable daemon this is `127.0.0.1` and an ephemeral port; otherwise it is the configured interface and port. |
| `-t`, `-I` | Client options and the generated index page (see [the browser client](#the-browser-client)). |

The ping interval is long on purpose. libwebsockets closes a socket whose pong misses a grace window of seven seconds past the interval, and a browser that has stopped draining a saturated stream can miss it; at ttyd's five-second default a busy terminal closes and silently reattaches. `-P 0` pings continuously and starves ttyd's output pump instead of disabling the check. ttyd stores the interval plus seven in a `uint16_t`, so the value must stay at or below 65528.

| | Writable | Broadcast |
| --- | --- | --- |
| Port | `[web] port` (8200) | `[web] share_port` (8201) |
| Authentication | Basic auth, optionally behind the gate | None |
| Rooms reachable | Every room with a workspace record and a live session | Rooms on the allowlist |
| Record | `web-ttyd.json` | `web-ttyd-share.json` |
| Lock | `web-ttyd.lock` | `web-ttyd-share.lock` |
| Attach | `tmux -S <socket> attach -t <session>` or `zellij attach <session>` | tmux adds `-r`, and `-f ignore-size` on tmux 3.2 or newer; Zellij attaches normally |

Every state file lives under `$XDG_STATE_HOME/rimz/`.

### Starting

`ensure_daemon` (writable) and `ensure_broadcast_locked` (broadcast) converge the machine on one process that matches the current config. The writable path runs in this order:

1. Validate config with `desired_spec`: `interface` must parse as an IP address, each `trusted_proxies` entry must parse as an IP or CIDR, `auth_users` requires a non-empty `auth_header`, and no user may be empty after trimming. Each failure is a typed `WebErr` that names the fix.
2. Resolve ttyd from `RIMZ_TTYD_BIN`, then `PATH`, and require `ttyd --version` to parse at or above `MIN_TTYD_VERSION` (1.7.5). A missing binary reports the Homebrew and apt install; an old or unparseable version reports the floor and the upgrade.
3. Take `web-ttyd.lock`, so concurrent room starts converge on one process and a credential rotation cannot race stale-record cleanup.
4. Reap legacy per-session daemons: for each record under `$XDG_STATE_HOME/rimz/web-ttyd/`, send SIGTERM if its pid still names `ttyd`, then remove the directory. Malformed records, recycled pids, and cleanup errors are debug logs only.
5. Read the record and check it is live (see [the record](#the-record)). A stale record is removed, and any of its processes still running are terminated.
6. Build the client profile, which may generate the index page, and check that the configured listener is free unless the live record already holds it. An occupied port is `ConfiguredPortInUse`, which points at `[web] port`.
7. Reuse the live daemon if its record matches the desired shape. Otherwise stop it, create the credential if none exists, and start fresh.

A fresh start has two shapes. With Basic auth and an empty `trusted_proxies` list, ttyd binds `<interface>:<port>` directly. With trusted-header auth or any trusted proxy, ttyd binds `127.0.0.1:<ephemeral>` and RimZ spawns the hidden `rimz web gate` on the configured listener, pointed at that upstream. Either way RimZ waits up to 5 seconds (`START_TIMEOUT`) for the public listener, writes the record only after it accepts, and stops every process it started when a later step fails.

Both ttyd processes and the gate spawn with null stdio in their own process group. Before spawning ttyd, `without_ttyd_launch_context` removes the launching pane's identity from the environment: multiplexer membership, room, worktree, and agent variables, the client size, and remote-attach markers (`TTYD_AMBIENT_CONTEXT_ENV`). Machine environment such as `HOME`, `XDG_*`, `PATH`, locale, logging, and the `RIMZ_*_BIN` overrides passes through, so every browser attach sees the machine and none sees the pane that happened to start the daemon.

The broadcast path is the same without the gate, the credential, or the legacy reap. It validates only `interface`, and its port check reports `[web] share_port`.

### The record

`web-ttyd.json` is written with `write_temp_then_rename_cache`. Its fields:

| Field | Meaning |
| --- | --- |
| `pid`, `port`, `interface` | ttyd's pid and the public listener. |
| `auth` | `{"mode":"basic"}` or `{"mode":"trusted_header","header":"<name>"}`. |
| `auth_users` | The trimmed identity allowlist; omitted when empty. |
| `trusted_proxies` | The proxy list as configured. |
| `gate` | `{pid, upstream_port}` when the gate runs. |
| `basic_upstream` | ttyd itself requires the Basic credential in every mode. |
| `launch_context_scrubbed` | ttyd was spawned without the launch pane's environment. |
| `pixel_protocol` | Present only when ttyd serves the generated page with the current `TTYD_PIXEL_PROTOCOL`. |
| `index_key` | The generated page's cache key. |

`web-ttyd-share.json` carries `pid`, `port`, `interface`, `launch_context_scrubbed`, `pixel_protocol`, and `index_key`.

A record is live only while its pid names `ttyd`, its listener accepts a TCP connection, and, for a gated daemon, the gate pid is a `rimz web gate` process. A reader that finds any of these false terminates what survives and removes the record.

A live record is reused only when every desired value matches: the listener, the auth mode, `auth_users`, `trusted_proxies`, whether a gate runs, both markers set, and the `index_key` of the page the current binary would generate. Missing markers deserialize as false, so a record without them is always replaced. Any drift stops the old processes and starts the desired shape; an upgrade, font change, or ttyd version that changes the page therefore replaces the daemon at the next ensure. Reuse fails with `TtydCredentialMissing` when the credential file is gone. The broadcast record compares the listener, its marker, and `index_key`.

### Who starts and stops them

| Caller | Writable daemon | Broadcast daemon |
| --- | --- | --- |
| `rimz start` with `[web] enabled = true` | Ensures it after the room is ready. Any failure, including a missing or old ttyd, prints `rimz: browser daemon was not started: …` and the room starts anyway. | Untouched. |
| `rimz web open` | Preflights ttyd before birthing the room, waits up to 5 seconds for the session to be addressable, then ensures the daemon. | Untouched. |
| `rimz web open --no-start` | Requires a live record and the credential, and fails with `TtydOffline` otherwise. It skips the config-drift comparison; the ttyd version is still checked before the room is resolved. | Untouched. |
| `rimz web url` | Reads the record and credential; changes nothing beyond stale-record cleanup. | Untouched. |
| `rimz web start` | Ensures it. | Untouched. |
| `rimz web restart` | Always starts a fresh process, stopping the live one first. | Restarts it when the allowlist is non-empty, otherwise stops it. |
| `rimz web stop` | Stops it. | Stops it; the allowlist stays. |
| `rimz reload` | Restarts it only when it is online. | Restarts it only when online, or stops it when the allowlist is empty. |
| `rimz web token create` | Restarts it when online. | Untouched. |
| `rimz web token revoke`, `revoke-all` | Stops it. | Untouched. |
| `rimz web share` | Untouched. | Ensures it. |
| `rimz web unshare` | Untouched. | Restarts it, or stops it when the list becomes empty. |
| `rimz web unshare --all` | Untouched. | Stops it. |

`rimz start` is best-effort because browser access is enrichment on a room the user asked to open; the explicit web commands treat the same checks as fatal preconditions. `rimz reload` warns instead of failing. Its restart path checks liveness and reaps legacy daemons before it validates config, so a config error there leaves a live daemon running and reports the error as a warning.

Stopping sends SIGTERM to the gate and ttyd, polls the process table for up to one second, sends SIGKILL to any survivor, waits up to one second for the public listener to close, and removes the record.

## The gate

`rimz web gate` (`gate::serve`) stands on the public listener whenever ttyd cannot be exposed directly. It parses its `--allow` entries as bare IPs or IPv4 and IPv6 CIDRs, and `desired_spec` has already parsed them before any process changed.

For each connection the gate first checks the peer. It maps an IPv4-mapped IPv6 address back to IPv4, accepts loopback and any address inside a same-family allowlist entry, and closes everything else without a response. With Basic auth, an accepted connection is spliced to ttyd byte for byte, and the browser answers ttyd's Basic challenge itself.

With trusted-header auth, the gate reads the machine credential once at startup, keeping the secret off its argv, and rewrites each HTTP request head before forwarding it:

1. The configured header must appear exactly once, with a non-empty value after trimming. When `auth_users` is non-empty, that value must equal one entry byte for byte, case-sensitively. Otherwise the client gets `401 Unauthorized` and the connection closes.
2. Every client `Authorization` header is dropped and `Authorization: Basic <machine-credential>` is added.
3. A body with `Content-Length` is forwarded unchanged. A chunked request, a malformed head, or conflicting lengths close the connection.
4. The response is relayed with its framing intact. Keep-alive requests on one connection are each checked independently, and a WebSocket upgrade that ttyd accepts with `101` turns the connection into a raw splice.

Loopback peers pass the address check but still need the header. The gate turns a proxy's identity header into ttyd's Basic credential only after the peer passed the address check, and ttyd itself never trusts an identity header.

When trusted-header auth binds a non-loopback interface with an empty proxy list, `auth_warnings` warns that only loopback proxies can connect and names the `[web] trusted_proxies` fix.

## Attaching a room

A room URL is `<base>/?room=<percent-encoded-session>` (`join_session_url`). ttyd only understands `arg`, so the generated page's `ws_url.js` rewrites the WebSocket URL: when the page has `?room=`, the socket carries `?arg=<session>`, and otherwise the page's own query passes through, so a `?arg=` link still works. ttyd appends the decoded value to the command, and the browser session runs `rimz web exec <session>`.

`rimz web exec` never treats the browser value as argv. `live_session_target` accepts it only when the session has a durable workspace record and a live session in one mux probe, and the attach command comes from that backend's `attach_existing_command`. What happens next depends on whether stdin and stdout are terminals, which they are under ttyd:

| Target | Terminal stdio | Non-terminal stdio |
| --- | --- | --- |
| Valid | The [session picker](#the-session-picker) attaches it as a child, then shows the room list when it detaches. | Emits the OSC 7717 room sync, runs the attach as a child, and exits with its status. |
| Missing, unknown, or stopped | The session picker opens with a notice naming the rejected session. | Prints the live-session listing and exits 1. |

If the picker cannot set up the terminal, a valid target falls back to the non-terminal attach, and an invalid one to the listing error.

## The session picker

The picker in `cli/sessions/picker.rs` is the same list `rimz sessions` shows in a terminal. The browser runs it in `Mode::Web`, which adds the OSC 7717 room sync; `Mode::Terminal` emits no browser sequences.

Every 2 seconds (`PROBE_INTERVAL`) the picker joins durable workspace records with one live mux probe. Rooms with a prompt in the last 24 hours (the newest `turn_started_at` among non-subagent agents) rank first by that prompt time; the rest follow by the workspace record's `updated_at`. Typing filters by repository display name and path. Each card's agent counts per kind, attention count, and headline spend window come from the room's published snapshot through `PublishedSnapshotReader`, one reader per live session; an unreadable snapshot leaves the card without them.

On a full-size screen the list box is 40% of the terminal width, clamped to 58 through 84 columns, and 24 rows tall beneath a RIMZ banner. A screen too small for the banner gets the compact full-frame layout.

Selecting a room hands the terminal off without leaving the alternate screen or turning off mouse reporting, runs the attach as a child with inherited stdio, and restores the picker when the child exits. A failed or non-zero attach becomes a notice on the list.

The browser tab follows the picker through private OSC 7717 sequences (`session_sync_osc`): `rimz-session=<session>` and `rimz-name=<repo>`, both percent-encoded. They are written before each attach and cleared whenever the list takes over. The page's handler rewrites the URL's `?room=`, drops any `?arg=`, and sets the tab title to `<repo> · RimZ`, or `RimZ` on the list. A reload or reconnect therefore returns to the current view, and a stale target is cleared as soon as the list opens.

The `n` key opens the new-room view. It lists workspaces with a record and no live session, then the non-hidden subdirectories of the current directory, which starts at `$HOME`. Confirming a path calls the detached room birth (`ensure_workspace_room_detached`) and attaches the new session as a child. That path skips the ttyd preflight, which only `rimz web open` runs, so `rimz sessions` can create a room on a machine without ttyd. A birth error returns to the picker with a notice.

## Sharing a room

The broadcast allowlist is `web-share.json`, `{"sessions": [...]}`, kept sorted and deduplicated and written with temp-file plus rename. All allowlist and broadcast process changes hold `web-ttyd-share.lock`.

`rimz web share` requires `[web] enabled`, a workspace record, and a live session. It adds the session, ensures the broadcast daemon, and restores the previous allowlist if the daemon fails to start. The share URL uses `share_base_url` and the `rimz.web.share.v1` payload.

The share shim, `rimz web exec --share <session>`, re-reads the allowlist on every connection and repeats the record and liveness checks. Every rejection, whether the session is missing, unknown, unshared, or dead, returns the same `this room is not shared` error, so a viewer learns nothing about other rooms. A valid tmux target attaches with `-r`, which blocks mux input as a second barrier, and with `-f ignore-size` where supported, so the viewer's window size does not resize the room. Zellij has no read-only attach: the shim runs a normal `zellij attach`, ttyd is the only input barrier, and the viewer's size can affect the session.

Revocation disconnects viewers by restarting the process. `unshare` of one room rewrites the allowlist, stops the daemon, and starts a fresh one if rooms remain; viewers of still-shared rooms reconnect through ttyd. Removing the last room, or `unshare --all`, stops it.

## The credential

The one credential, named `rimz`, lives at `web-ttyd-credential.json` with `name`, `created_at`, and a 24-character `secret`, written mode 0600 through `write_private_temp_then_rename`. ttyd requires it in every auth mode. `ensure_daemon` creates it on first start.

| Command | Effect |
| --- | --- |
| `token create` | Mints a new secret. When the writable daemon is online, restarts ttyd and any gate so the old secret stops working and the gate reads the new one. |
| `token create --read-only` | Refused with `TtydReadOnlyCredential`: ttyd's read-only mode belongs to the whole process, and the error points at `rimz web share`. |
| `token list` | Prints `rimz: <created_at>`. |
| `token revoke rimz`, `revoke-all` | Stops the writable daemon and deletes the file. Any other name is `TtydCredentialNotFound`. |

These commands behave the same in Basic and trusted-header modes.

## The browser client

`client::profile` builds the ttyd options and the page. Every daemon gets `-t macOptionIsMeta=true`, `-t cursorBlink=false`, `-t titleFixed=RimZ`, and `-t disableLeaveAlert=true`. With `[web] enabled = false`, that is all: no theme, no font, and no generated page.

With `style_client = true`, the profile adds `fontFamily=<font>,monospace` and a `theme=` object projected from the resolved [palette](./theme.md#resolving-a-palette), and resolves font faces:

- A `font` naming a built-in family (`JetBrainsMono Nerd Font Mono` or `CaskaydiaCove Nerd Font Mono`) downloads the Nerd Fonts v3.4.0 regular and bold faces, checks each against a pinned SHA-256, and caches them.
- A `font_source` starting with `https://` is fetched once and cached under the SHA-256 of the URL. Any other scheme is refused. A local path is read directly, with `~` expanded.
- A custom face must end in `.ttf`, `.otf`, `.woff`, or `.woff2` and be at most 16 MiB.

Fonts cache under `$XDG_CACHE_HOME/rimz/web-fonts/`. With `RIMZ_WEB_FONTS_OFFLINE` set to any value, resolution uses the cache only.

### The generated page

ttyd serves no extra static routes, so fonts and scripts have to live inside the index page it serves with `-I`. `ensure_custom_index` keys the page by a SHA-256 over the `CUSTOM_INDEX_SCHEMA` string, the full bootstrap script, the ttyd version string, the font family, and each face's bytes, and caches it as `$XDG_CACHE_HOME/rimz/web-ttyd/index-<key>.html`. On a cache miss it starts a throwaway ttyd on a loopback ephemeral port with a temporary credential, fetches the stock `/` page, stops it, and injects a `<style>` block (the `@font-face` rules and the overlay restyle) before `</head>` and the bootstrap script before `</body>`.

Because the bootstrap script is part of the key, any change to it produces a new page and a new `index_key`, and the next ensure replaces either daemon. A tab left open across that replacement keeps the old page until it reloads.

Every page failure only warns. If the stock page cannot be fetched, lacks a `</head>` or `</body>` marker, or cannot be cached, the daemon starts on ttyd's stock page with neither `pixel_protocol` nor `index_key` in its record. Theme and font failures warn the same way.

### What the bootstrap does

The script targets the xterm.js bundled by ttyd 1.7.5 and newer, which is why the version floor sits there. It waits for `window.term`, then installs:

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

tmux can repaint the whole pane for each drag coordinate, and ttyd 1.7.7 keeps its output pump running after the browser sends a flow-control pause. In a 179x38 trace, about 63 motion reports per second left tmux's pane width advancing roughly once every two seconds. `mouse_flow.js` therefore paces motion reports on the WebSocket send path. The first report goes out immediately, motion slower than one report per 50 ms (`MOTION_INTERVAL_MS`) passes through, and faster motion keeps only the newest coordinate for the rest of each 50 ms slice. A button press or release, or any non-mouse input, flushes the pending coordinate first, so a drag ends exactly where the pointer stopped. Reads from the WebSocket are never delayed, and each new socket resets the pacing state.

tmux clears every xterm mouse mode before re-enabling the set it wants whenever a pane changes tracking mode. xterm.js 5.4.0 removes its document-level held-drag listeners on `?1002l`, and the following `?1002h` restores the protocol flags without reinstalling the listeners, which only a mousedown does. `installMouseDragRearm` tracks the primary button in its own capture-phase listeners and subscribes to `coreMouseService.onProtocolChange`. When drag reporting returns while the button is still held, it dispatches one synthetic mousedown to reinstall xterm's listeners and swallows the press report that mousedown produces before it reaches the pacing state or the socket. If the private `coreMouseService` API is missing, re-arm installs nothing and records `rearm-unavailable`, and the live browser regression fails.

Adding `rimzdebug=1` to the page URL exposes the pacing state and the last 256 decisions (sends, coalesces, flushes, re-arms, swallowed presses) at `window.__rimzWeb`.

### The pixel layer

`pixel_layer.js` renders the sidebar's Kitty graphics in the browser. It wraps `term.write`, so it sees output before xterm parses it, and consumes RimZ's subset of the Kitty protocol: PNG transmits (`a=t`, `f=100`, including chunked continuations), virtual placements (`a=p`, `U=1`), and deletes by image id (`a=d`, `d=i`). It keeps at most 128 decoded images, and each image is limited to 4 MiB of payload.

Each placeholder cell (U+10EEEE plus the row and column combining marks) is wrapped in SGR 8 and 28 as it enters xterm. xterm keeps the full cluster and the RGB image id in its buffer, while its WebGL renderer paints the cell's real background instead of a fallback glyph. A DPR-scaled canvas above the screen draws each image clipped to the cells of its placement, preserving the image's aspect and origin.

Render, scroll, and resize events share one animation-frame scan. The scan skips rows without a placeholder, reads the row and column marks and image id of the rest, and leaves the canvas alone when the visible scene matches the painted one. A moved placeholder, a new image or placement, a resize, a reconnect, or a tmux client switch invalidates the scene and repaints it.

The sidebar only sends Kitty graphics to a tmux client it trusts to render them. A browser client qualifies when it descends from a live ttyd whose record carries the current `pixel_protocol`; the full rule is the `kitty_clients` row in [pets.md](./sidebar/pets.md#render-tiers). A daemon on the stock page has no `pixel_protocol`, so browser clients get sextant cell art.

## Configuration

`[web]` (`WebPrefs`) is per-machine. It is outside the trust hash because no field executes a command.

| Key | Default | Effect |
| --- | --- | --- |
| `enabled` | `true` | Allows `rimz web open` and `share`, the ensure on `rimz start`, and the styled client. |
| `interface` | `127.0.0.1` | Bind address for both daemons; must parse as an IP address. |
| `port` | `8200` | Writable listener. |
| `share_port` | `8201` | Broadcast listener. |
| `base_url` | unset | Public prefix for writable URLs; unset means `http://127.0.0.1:<port>`. RimZ appends `/?room=<session>`. |
| `share_base_url` | unset | The same for broadcast URLs, with `share_port`. |
| `auth_header` | unset | A non-empty trimmed value selects trusted-header auth and starts the gate. |
| `auth_users` | empty | Identity allowlist for trusted-header auth; empty accepts any single non-empty identity. |
| `trusted_proxies` | empty | IPs or CIDRs the gate admits besides loopback; non-empty starts the gate even with Basic auth. |
| `font` | `JetBrainsMono Nerd Font Mono` | Browser font family. |
| `font_source` | unset | Local path or HTTPS URL of a custom face. |
| `style_client` | `true` | Projects the theme and font into the browser. |

## Remote rooms

`rimz remote connect <target> --web` opens a remote room in a local browser. [remote.md](./remote.md#web-tunnels) owns the supervision rounds; this section owns the payload, the relay, and its port.

Each round prepares the remote side with one non-PTY `rimz web open --print --json`. The `rimz.web.v2` payload (`WebOpenPayload`) carries `url`, `session`, `port`, `tunnel_port`, `auth`, and `credential: {username, secret}`. A payload without `auth` reads as Basic, and one without `tunnel_port` tunnels to `port`. Any other `version` fails with an error telling the user to upgrade the remote binary. A trusted-header payload without a credential fails before any tunnel is set up and points at its reverse-proxy URL.

`tunnel_port` is the gate's upstream port for a gated daemon and the public `port` otherwise, so the tunnel always lands on ttyd's Basic-authenticated listener and never on the gate.

The local side runs two loopback ports:

1. The relay port, which the browser uses. `bind_local_relay` derives it from a CRC32 of the session name in 8300 through 8399, scans upward with wraparound on collision, and keeps the listener bound so no other process can take the port between selection and use. `--web-port` binds exactly that port instead.
2. An ephemeral forward port, which SSH forwards to `127.0.0.1:<tunnel_port>` on the remote.

The relay (`gate::serve_tunnel`) accepts only loopback peers, drops any `Authorization` header from the browser, and adds the returned Basic credential to every request before forwarding it to the SSH forward. Safari needs this: WebKit omits cached Basic credentials from WebSocket upgrades, and ttyd authenticates the upgrade. The user never sees a password prompt, and no second SSH call is needed to fetch a token.

The relay stays bound across recovery rounds. Each round repeats prep, so it can rebirth the room, restart the daemon, or pick up a changed port or credential, opens a fresh forward, and swaps the relay's target atomically. The local URL stays the same for the whole connection.

## Security boundaries

The browser session is shell access as the serving user. The defaults keep it on loopback behind Basic auth, and a deployment that exposes it puts HTTPS and rate limiting in a reverse proxy in front.

- ttyd always requires the machine credential. Trusted-header auth changes only what stands in front of it: the [gate](#the-gate) accepts a proxy's identity header only from an admitted peer and presents the credential to ttyd itself.
- Loopback peers pass the gate's address check but never bypass the header requirement.
- The remote tunnel relay accepts unauthenticated requests on local loopback and presents the remote credential. That matches the trust boundary of a raw `ssh -L`: any process that can connect as, or run as, the tunnel owner can use it, so shared hosts need user isolation.
- The credential stays out of URLs, store events, workspace records, and logs. It is printed only where a user asked for it: `rimz web open` writes `ttyd basic auth for this machine: user rimz, password <secret>` to stderr, `open --json` and `url --json` include it in the payload, and `token create` prints the new one. The remote tunnel holds it in memory and does not print it.
- The broadcast listener has no authentication by design. Its allowlist limits which rooms are served, not who watches: anyone who reaches `share_port` can read every allowlisted room. RimZ warns each time it ensures or restarts the broadcast daemon on a non-loopback interface, and a public deployment puts HTTPS, viewer authentication, and network filtering in front of `share_port`.

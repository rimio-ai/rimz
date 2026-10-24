# Sidebar live check

Use this before hand-off for a renderer, pipeline line, shared data plane, or click-routing change where a unit test cannot show the live frame. For a pixel transport change, [Capture kitty graphics](#capture-kitty-graphics) records what the sidebar sends a kitty-capable client.

## Hold a room and join it

Build the testkit binary first; without it `sandbox room` refuses with `development rimz missing`:

```sh
cargo build -p rimz --bin rimz --features testkit
```

`cargo xtask sandbox room --mux <tmux|zellij> [--for <duration>]` holds a disposable room and prints its room card; the room verb is Linux-only. Keep it running while you check the room. The room dies after 30 minutes unless `--for` says otherwise (`--for 45m`, or `--for off` to hold it until you stop it).

A room that fails to start keeps its sandbox root at the path the error prints, with the startup error in `<root>/kept`. The room's servers and processes are stopped. Remove the root with the printed `rm -rf` command when you are done inspecting it.

`cargo xtask sandbox in <root> [--cwd <dir>] [--as <@handle> | --as-ancestor <@handle>] [--] <command>` runs one command inside that held room. `--as` uses the agent's launch environment; `--as-ancestor` runs beneath its stub with no agent or pane keys, exercising caller detection by ancestry. Both need Linux. Without `--cwd` the command runs in the team's worktree, the card's `Worktree:` line; once a check removes that tree, pass `--cwd "$ROOT/home/room"`, the room's project. The cwd and identity flags may come in either order before the command. Use a qualified handle (`@coder#probe`) if the short form (`@coder`) is ambiguous.

Every command on the card spells that verb `target/debug/xtask` instead: the held room's own `cargo xtask` owns the target-directory lock for as long as it runs, so a joined `cargo` command waits for it rather than doing anything. Paste the commands from your own card: its root, pane ids, and `rimz` path are particular to that room. A copy of a build, held while the checkout rebuilds, keeps the file name `rimz`: RimZ sees through an agent pane's `rimz agents exec <kind>` wrapper to its provider only when that program is named `rimz`, so a pane launched from a copy under another name reads as a generic process rather than its agent.

A check that needs two rooms, such as a pane id addressed across rooms, starts the second inside the same sandbox. With `$BIN` the card's `rimz` path and `<mux>` the room's backend:

```sh
mkdir "$ROOT/home/probe2"
target/debug/xtask sandbox in "$ROOT" --cwd "$ROOT/home/probe2" -- git init -q .
target/debug/xtask sandbox in "$ROOT" --cwd "$ROOT/home/probe2" -- "$BIN" --<mux> start "$ROOT/home/probe2" --no-attach
```

A command run with `--cwd "$ROOT/home/probe2"` resolves the second room. Plain `sandbox in` scrubs the Zellij and tmux session keys, so a raw Zellij action from outside a pane picks no session once two are live; name one with `env ZELLIJ_SESSION_NAME=<session>` ahead of the command. The same scrub means a command without `--zellij`, such as the `Resume:` line a receipt prints, picks tmux in a Zellij room ([which backend a command uses](../reference/cli.md#which-backend-a-command-uses)); to run a printed command as a user would, add `default = "zellij"` under `[mux]` in the sandbox home's machine config, `$ROOT/home/.rimz/config.toml`. A second Zellij room started with `--no-attach` has no client, and Zellij builds a new tab's panes only while a client is attached, so every tab launch into it fails with `layout panes did not materialize`. Attach a client first, as `sandbox room` does for the first room, for example from a detached tmux session running `"$BIN" --zellij attach <session>`.

## Room cards

The card names the sandbox root, the room's private multiplexer and session, and the team's checkout (Worktree). One Sidebar block follows per sidebar pane, each with its tab and three commands: Look, Capture, and Click. A standard room has three: the daemon tab, a shell tab, and the team's `#probe` tab. A tab can carry more than one sidebar pane, so address a sidebar by its pane id, not by counting blocks. Role lines map each agent to its pane and process id (`pid -` when the multiplexer reported none). Each As line is a command prefix for that agent: append a RimZ command, or swap `--as` for `--as-ancestor` to exercise ancestry. Stage and Owner give the initial board state, Flip moves the stage to Review, and Focus reads focus from the multiplexer.

A tmux card has this shape, with one block shown per line kind (`<root>`, `<session>`, `<bin>`, and the pane ids come from your room):

```text
Sandbox root: <root>
Mux: tmux  Session: <session>
Worktree: <root>/home/room-worktrees/probe
Sidebar: tmux:<pane>  Tab: <tab>
  Look: target/debug/xtask sandbox in '<root>' -- '<bin>' '--tmux' 'pane' 'focus' 'tmux:<pane>'
  Capture: target/debug/xtask sandbox in '<root>' -- '<bin>' --tmux pane capture 'tmux:<pane>'
  Click: target/debug/xtask sandbox in '<root>' -- '<bin>' --tmux sidebar click 'tmux:<pane>' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Role: @coder#probe  tmux:<pane>  pid <pid>
  As: target/debug/xtask sandbox in '<root>' --as '@coder#probe' -- '<bin>' --tmux
Stage: Build  Owner: coder
Flip: target/debug/xtask sandbox in '<root>' -- '<bin>' --tmux teams flip Review 'live check' --team forge
Focus: target/debug/xtask sandbox in '<root>' -- tmux -S '<root>/runtime/rimz/tmux/server' display -p '#{pane_id}'
Run a sidebar's Look first, then capture or click it: an unwatched sidebar can hold a stale frame.
```

A Zellij card differs in two lines. Look switches the client to the sidebar's tab, and Focus lists every pane:

```text
  Look: target/debug/xtask sandbox in '<root>' -- 'zellij' '--session' '<session>' 'action' 'go-to-tab' '<tab number>'
Focus: target/debug/xtask sandbox in '<root>' -- zellij --session '<session>' action list-panes -a -j
```

Its pane ids read `zellij:terminal_<n>`, and every other command carries `--zellij` in place of `--tmux`.

## Seven checks

Run the sequence on both backends, using each room's own card.

### 1. First pane clock

Run Look for one sidebar, then its Capture twice across a displayed clock-unit boundary: a second apart below a minute, a minute apart below an hour. The compact pipeline clock must advance.

### 2. Another pane clock and stage flips

**2a.** Run Look for another sidebar, then its Capture twice across a displayed clock-unit boundary. Its pipeline clock must also advance. Allow a beat after Look before the first capture, or a sidebar that was cold will read its pre-Look frame.

**2b.** Keep that pane watched. Run the card's Flip, then repeat that pane's Capture and measure from the flip to the first frame showing `Review`. It must replace `Build` within 5 s. The pipeline line sits under the team's worktree group:

```text
 ⑂ probe · forge                           ≡ main
   ● ◉  Review                               0:13
```

**2c.** Hold the room with `env -u NO_COLOR` (for example `env -u NO_COLOR cargo xtask sandbox room --mux <tmux|zellij> --for 15m`), or the ANSI capture carries no color. Keep the pane watched and record the earlier Build duration from the board ledger (the published `stage_prior_secs` carries the same interval). Run the card's Flip with `Build` in place of `Review`, then repeat Capture until the first Build frame. Record the update latency, the stage time, and the run total, all read from the watched pane's frames, not inferred from the publication. The pass rules:

- The Review dot is now hollow and warm (`warn`). Add `--ansi` to Capture to keep the dot's color, since a plain capture cannot prove tone. In the default theme the warm tone is foreground `38;2;224;175;104`.
- The stage clock resumes Build's earlier duration plus the time since the return. It does not restart at zero.

Flip back to `Review` before checks 3 and 4.

Flip to `Ship` for a fresh clock under a minute.

### 3. Client views on each tab

For every Sidebar block, run its Look, then its Capture. Repeat the Capture while the frame is still the pre-Look one: a capture taken a few tens of milliseconds after Look can still show `Build`. Each tab must show `Review` within 5 s of Look.

Attach two real terminal clients to the card's private mux session, using the same isolated environment as its Look command. Read each client's terminal output, not only server-side pane captures. With enough cards to scroll, check both sharing cases:

- **tmux:** both clients view the same sidebar. Scroll from each client in turn; both screens must change together. Detach one; the survivor must keep its scroll and selection after a fresh fold.
- **Zellij:** put the clients on different tabs and scroll one; the other's cards must stay put. Move that client onto the other's tab; it must join the view already there. Move away again; it must carry the joined view, then scroll independently. Before each move, confirm that the host's published client map places the client where the last move put it, and keep that record with the frames: a move made before the host observed the previous one cannot be attributed.

For retention, shorten `[sidebar] afk_after_secs` to 20 in the disposable room's isolated `RIMZ_HOME/config.toml`, not its project config. Scroll and select a card, detach every client, and reattach within that window: the held view must return. Compare after the client map settles, keeping the same native working-pane baseline. Detach again, wait past the window, and reattach: with no other retained view the sidebar starts at the top without a carried selection. Start the expiry wait when the published client map shows full detach, not when the terminals close; an unviewed pane roster can wait ten seconds before sampling again. A new client's native working-pane focus can immediately establish a new highlight. Keep the host alive throughout; a host restart loses full views. The executable proofs live in `crates/rimz/tests/integration/backend/tmux/sidebar.rs::two_clients_share_the_sidebar_view_through_detach` and `crates/rimz/tests/integration/backend/zellij/shared_view.rs::two_clients_split_join_and_carry_the_sidebar_view`.

The reattach past the window also checks the tab-read dwell. Before detaching, finish an agent on the landing tab so its card is unread, and leave it unfocused. After the reattach, the tab's unread cards clear together about 2.5 s after the first fold that observes the client on that tab, and they keep their order while they clear. That fold can trail the reattach command by more than a second, so judge a capture taken about 5 s after the reattach, not one taken at 2.5 s. A sidebar born on a background tab behaves the same on its first reveal: finish an agent there, switch to the tab, and its unread cards clear after the dwell. A tab on screen since the room started keeps its unread siblings marked until each is focused or a later switch back into the tab arms the dwell.

### 4. Pipeline click routes to the owner

On the team's tab, run the sidebar's Look and take a fresh Capture. Count rows from zero to the pipeline line and set `ROOM_ROW` to that row. Do not reuse a row from an earlier frame, since selection can shift rows. Run the card's Focus to record the starting focus, then its Click (`sidebar click <pane> 2 <row>`), then Focus again. The target must be the reviewer pane from the card's `Role: @reviewer#probe` line: on tmux, Focus prints that pane id, and on Zellij that pane joins the focused set in the `list-panes` output.

On Zellij the held room's attached client follows no focus sent from outside a pane, so `list-clients` and the active tab stay where they were after any click, a card click included. Prove a pane jump there from the target tab's focused set before and after the click (the sidebar pane leaves it and the target stays) and from the fused `focused_pane` in the renderer's fold trace. The client's tab switch itself needs a real attached session. tmux shows it through `list-clients`.

### 5. Wheel glide

On each backend, use an attached room with more cards than viewport rows (expand groups or shorten the terminal if needed). Focus the sidebar and wheel down, then up: the cards should glide over about a tenth of a second, and the scrollbar should follow the visible cards rather than jump ahead. Wheel again during a glide to check that it continues from the visible row without jumping back. Press a selection key during a glide: the glide should stop in one frame, revealing the new card only if none of it is visible. Click a visible card mid-glide and confirm that the pane it opens matches the card under the pointer.

### 6. Cross-tab click row

On each backend, use two tabs with at least two agent cards visible in each sidebar. In tab 1 select card A so it opens, with card B's pane in tab 2. Capture tab 1's sidebar with `rimz pane capture` and note B's first-line row, then click B. Capture the first frame in tab 2's sidebar: B must land on the same row and A must still be open. Repeat after tab 2's sidebar previously selected a card above B with an open delegation section, to catch a landing below the clicked row as well as one above it. Zellij's actual client tab switch needs an attached session, as in check 4.

In `audit/focus.log.jsonl`, inspect the latest `push_handshake` records with `jq 'select(.event.kind=="push_handshake")'`: the source outcome and destination peer verdict must both be `painted`. The main diagnostic log must have no `clicked_row_displaced` for B. A `displaced` peer verdict means a frame was painted but missed the requested row, even when the source outcome is `painted`.

Also drive a rapid cross-tab burst from the one visible source sidebar, sending every click to that same renderer before its first jump dispatches. Do not alternate source sidebars: that misses input queued on the source with the pending request. Only the final cross-tab target should switch; a re-click on a same-tab card should focus without waiting for the pending jump. Use `click_delivery` and `jump_progress` to distinguish delivery, lease arming, and supersession from a missing handshake.

A change to jump timing under load needs a loaded large room. As a scale reference, an idle room of 100 tabs picks a push up in about 1 to 45 ms, and a busy real room in about 100 to 700 ms: a room that still answers in idle times is not loaded enough. On tmux, grow the held room past 100 tabs with a `tmux new-window -t <session>:` loop, then confirm with `rimz sidebar renderers --json` that one host serves a renderer per tab. Hold the load for the whole measurement with two generators run through `sandbox in --as`:

- four concurrent loops of `rimz sidebar notify-test '@reviewer#probe' --no-command --force-bell`, each pausing 80 ms after every send;
- `command_changed` sidebar-event datagrams (`"v": "rimz.sidebar-event.v2"`, the room's workspace ID and session name, a shell pane and its command) sent 20 times a second to the host's `wake.sock` under the room's runtime directory.

Compare `push_handshake` records from two builds under that same load, never against an idle run. To hold a room on a fixed build while the checkout rebuilds, copy `debug/rimz` and `xtask` into a directory `P` as `P/debug/rimz` and `P/xtask`, and run every sandbox verb as `CARGO_TARGET_DIR=P P/xtask sandbox ...`: `sandbox` resolves `rimz` from `$CARGO_TARGET_DIR/debug`. The source sidebar reflows after a click, so a burst reads each later click's row from a capture taken after the previous click, not from one taken before the burst.

### 7. Failed focus

On each backend, force a focus dispatch to fail without racing a pane removal. Put an executable shim named after the mux (`tmux` or `zellij`) in a directory on the room's `PATH`, ahead of the real binary. It forwards every call to the real binary, except that while a marker directory exists it exits non-zero with a message on stderr for a focus of one target pane (`select-pane -t <pane>` on tmux, `action focus-pane-id` on Zellij). Kill the room's `sidebar host` by PID so its successor resolves the shim; the supervisors reattach. Create the marker, click the target's card, capture the sidebar, then remove the marker. The highlight must return to the session-focused card, and the bottom line must read `last alert 0s ago: focus failed: <card>: <error>` with `x dismiss`.

## Room host

A room runs one `rimz sidebar host` for the session and one `rimz sidebar serve` supervisor per tab ([state.md](../internals/sidebar/state.md#the-room-host-and-its-attachments)). The host is the only painter; `serve` refuses non-terminal stdout or unavailable runtime paths before socket or terminal-mode work. Take the process shape from inside the room, where the session name is on your card:

```sh
ps -eo pid,ppid,rss,args | grep -E 'rimz sidebar (host|serve)' | grep -v grep
```

Expect one `sidebar host` line and one `sidebar serve` line per tab, never a child `serve` painter. The host's parent is the `sidebar serve` supervisor that started it, and once that supervisor exits the host is reparented; it runs in its own session either way, so no pane's job control reaches it.

The host's fetch worker folds for the whole room. About once a minute it appends a `fetch_reduction_stats` record to the room's `audit/diag.log.jsonl`, and `cycles` over `interval_ms` is the room's fold rate: about one a second when idle, whatever the number of tabs. Set `ROOT` and `SESSION` from the room card:

```sh
jq -c 'select(.event.kind == "fetch_reduction_stats") | .event | {cycles, interval_ms}' \
  "$ROOT/home/.rimz/ws/$SESSION/audit/diag.log.jsonl"
```

Six checks cover what a unit test cannot:

1. **Host death.** Kill the host by PID with `kill -9`. Each supervisor reads the end of its stream, clears its pane to `sidebar: host went away` and `retrying in 1s`, then attaches to a successor host one of them starts. Check both a young host and one that has painted for over a minute: both follow notice-and-retry, with delays doubling to 60 seconds on repeated failures. Look on each tab, then repeat the `ps` and confirm one new host and the same supervisors. No supervisor paints cards while the host is unavailable. The successor host starts with no fold, so each sidebar paints the placeholder until its first fold arrives. When you time successive kills, wait more than 60 seconds between them. `RIMZ_TEST_SIDEBAR_STABLE_RUN_MS` shortens that window on a testkit build, but `sandbox room` strips every `RIMZ_*` variable from the room it holds, so it cannot be set there; space the kills instead. A kill that lands inside the previous host's first minute gets the doubled retry delay, not the 1 s first retry, and its timing measures the backoff.
2. **Supervisor death.** Kill one tab's `sidebar serve` by PID. The host drops that pane: its heartbeat leaves the room's runtime directory (the testkit build's `rimz sidebar renderers` no longer lists the instance), and the other tabs keep painting.
3. **Last pane.** Close every tab's sidebar pane, or kill the session. The host exits about ten seconds after its last pane detaches; `ps` shows no `sidebar host` for the session after that.
4. **Reload onto different bytes.** Start the held room with `target/debug/xtask sandbox room`, rather than `cargo xtask sandbox room`, so it does not hold Cargo's lock during the rebuild. Set `ROOT` and `BIN` from that room's card. Copy the running build before rebuilding if `BIN` names Cargo's output. The build-version override below changes embedded bytes and therefore the linker build identity; changing a runtime environment variable or touching a source file without changing its contents does not. A reload onto the build already painting restarts nothing (`rimz reload` prints `… already on the current build.`), which is why this check needs a second, different build.

   ```sh
   RELOAD_DIR=$(mktemp -d "${TMPDIR:-/tmp}/rimz-reload.XXXXXXXX")
   RIMZ_BUILD_VERSION_OVERRIDE=0.4.3+sidebar-reload \
     cargo build -p rimz --bin rimz --features testkit --locked
   RELOAD_BIN="$RELOAD_DIR/rimz"
   cp "$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)/debug/rimz" "$RELOAD_BIN"
   target/debug/xtask sandbox in "$ROOT" -- "$RELOAD_BIN" reload
   ```

   Record the room's host PID before and after. Expect `Reloaded 3 sidebars across 1 session.` for the standard three-tab room, one successor host, and every tab painting after its Look. The supervisors remain until the new painting build passes the stable window and preflight; inspect `/proc/<supervisor-pid>/exe` on Linux to distinguish re-exec from an unchanged command line. Repeat with a different override value for another reload. Across the room-socket upgrade, old supervisors still paint and receive room events but ignore keyboard, mouse, and resize input for about 60 seconds before promotion; this one-time input window does not apply between two stream-input builds. A mixed-build probe can shorten it with `RIMZ_TEST_SIDEBAR_STABLE_RUN_MS` in the supervisor's environment. Use a hand-built disposable room with its own short `XDG_RUNTIME_DIR` for that probe: the sandbox launcher scrubs ambient `RIMZ_*` keys. Press `?` before and after promotion and capture the help overlay, rather than treating a fresh heartbeat as proof of input.

5. **One notification datagram, every attachment.** Notifications must be enabled in the sandbox's config (`[notifications] enabled = true`). Set `SESSION` from the room card, then send to its reviewer; `--force-bell` avoids requiring a genuine unread episode and `--no-command` skips external notification handlers.

   ```sh
   target/debug/xtask sandbox in "$ROOT" -- "$BIN" config get notifications.enabled
   target/debug/xtask sandbox in "$ROOT" -- strace -f -e trace=sendto \
     -o "$ROOT/tmp/notify.sendto" "$BIN" sidebar notify-test '@reviewer#probe' \
     --force-bell --no-command
   rg sendto "$ROOT/tmp/notify.sendto"
   jq -c 'select(.event.kind == "bell_ring") | {instance_id, event}' \
     "$ROOT/home/.rimz/ws/$SESSION/audit/notify.log.jsonl"
   ```

   Expect `Notification test sent: 1`, one send to `host.<digest>.wake.sock`, and a new `bell_ring` decision for each of the three renderer instances. Compare them with `sidebar renderers --json`. Only the reviewer's tab rings: the daemon tab reports `daemon_view` and the unrelated shell tab reports `pane_not_in_view`. Notifications emit terminal control bytes, not a painted banner; a pane capture cannot prove delivery. On both tmux and Zellij, check the per-instance decisions at the consumer, then Look and Capture each tab to confirm it still paints.

6. **Degraded room.** Make the shared refresh fail while the rest of the room's state stays readable. To do that, move the event log aside and put an unreadable directory at its path. Do not run `chmod 000` on the whole state directory: the producer reads an unreadable store as an empty room, so the cards fall back to generic rows and no alert is raised. With `SESSION` set as in check 5:

   ```sh
   LOG="$ROOT/home/.rimz/ws/$SESSION/log/events.log.jsonl"
   mv "$LOG" "$LOG.held"
   mkdir "$LOG"
   chmod 000 "$LOG"
   ```

   Hold the fault for at least 40 seconds and Look at each tab during it. Every sidebar keeps its last cards under `⚠ Sidebar degraded for …: snapshot failed:`, with a reason that ends in `Permission denied (os error 13)`. No tab shows `sidebar: host went away`, and the `ps` above lists the same host and supervisors. Then restore the log:

   ```sh
   chmod 700 "$LOG"
   rmdir "$LOG"
   mv "$LOG.held" "$LOG"
   ```

   Within about two seconds, each sidebar paints fresh content under the dim `⚠ last alert …s ago` notice.

## Make one sidebar unresponsive

`rimz sidebar repair` replaces a sidebar whose pane is still listed but which no fresh current-build heartbeat claims. On Zellij that replacement is an anchored split of the old sidebar, and checking it means the new sidebar takes the old one's rectangle while every work pane keeps its own. Making that state takes some care, because the host writes every pane's heartbeat:

- Stopping a `sidebar serve` with SIGSTOP changes nothing, since the host keeps its pane's heartbeat fresh.
- Killing a `sidebar serve` closes its pane (check 2 above). The repair then adds a sidebar instead of replacing one.
- Freezing the host stales every heartbeat. A replacement's new `serve` also attaches to that frozen host, so its mount proof (six seconds) never sees a heartbeat. The repair closes the new pane and exits 1.

So freeze the host, keep the other sidebars' heartbeat files fresh by hand for longer than the five-second heartbeat TTL, and thaw the host once the repair has sampled liveness. List the heartbeats with the pane each one claims, take `HOST` from the `ps` line above, and name the files that should stay live:

```sh
HB="$ROOT/runtime/rimz/ws/<room>/live/heartbeat"
for f in "$HB"/*.json; do echo "$f $(jq -r .pane_id "$f")"; done
set -- "$HB/sidebar.<keep-1>.json" "$HB/sidebar.<keep-2>.json"
target/debug/xtask sandbox in "$ROOT" -- zellij --session <session> action list-panes -a -j > "$ROOT/tmp/before.json"
kill -STOP "$HOST"
for i in 1 2 3 4 5 6 7; do touch "$@"; sleep 1; done
target/debug/xtask sandbox in "$ROOT" -- "$BIN" --zellij sidebar repair & REPAIR=$!
sleep 1.5; kill -CONT "$HOST"; wait "$REPAIR"; echo "exit=$?"
target/debug/xtask sandbox in "$ROOT" -- zellij --session <session> action list-panes -a -j > "$ROOT/tmp/after.json"
```

Expect `Recovered 1 sidebar in place.` and `Closed 1 duplicate or unresponsive sidebar.` with exit 0. In `after.json`, the new sidebar has a new terminal id at the old sidebar's `pane_x`, `pane_y`, `pane_columns` and `pane_rows`. Every work pane in every tab matches `before.json`. Run the repair once more: it prints `Sidebar structure is healthy across 1 session.` A daemon tab is worth its own run, since its companions sit beside the content pane. Leaving the host frozen through the repair is the failure case: exit 1, `sidebars could not be repaired`, and no new sidebar pane left behind.

## Add a sidebar in place

`rimz sidebar repair` also adds a sidebar to a tab that has none ([in-place repair](../internals/multiplexers.md#in-place-repair)). RimZ adds a missing sidebar only in its reconcile pass, which runs when a command enters the room (`rimz start`, or a supervised run such as `rimz agents -p` or a loop fire) and on `rimz sidebar repair` or `rimz reload --repair`; opening a tab runs none. A plain `new-tab` already carries a sidebar, since the room's `new_tab_template` holds one, so open the test tab from a one-pane layout instead. Where Zellij places the split depends on the view's size, so set the attached client's view first: `stty` on the client's terminal resizes it. The client is the `zellij attach` line in `ps` that has a `pts/` terminal:

```sh
ps -C zellij -o pid,tty,args | rg -F '<session>'
stty -F /dev/pts/<n> cols 180 rows 50
target/debug/xtask sandbox in "$ROOT" -- zellij --session <session> action new-tab --name lone --layout-string 'layout { pane; }'
target/debug/xtask sandbox in "$ROOT" -- zellij --session <session> action list-panes -a -j > "$ROOT/tmp/before.json"
target/debug/xtask sandbox in "$ROOT" -- "$BIN" --zellij sidebar repair; echo "exit=$?"
target/debug/xtask sandbox in "$ROOT" -- zellij --session <session> action list-panes -a -j > "$ROOT/tmp/after.json"
```

Expect `Recovered 1 sidebar in place.` with exit 0. In `after.json`, the new sidebar in the `lone` tab sits at `pane_x` 0 and `pane_y` 0 with the tab's full 50 rows and 45 columns, and the work pane sits to its right with its `pane_y` and `pane_rows` from `before.json`. Run the repair once more: it prints `Sidebar structure is healthy across 1 session.`

To check a tab with floating panes shown, set a wide view (`cols 240 rows 50`), open another one-pane tab the same way with `--name floats`, open a floating pane in it, and confirm that Zellij reports the floats visible before the repair:

```sh
target/debug/xtask sandbox in "$ROOT" -- zellij --session <session> action new-pane --floating -- sleep 600
target/debug/xtask sandbox in "$ROOT" -- zellij --session <session> action list-tabs --json > "$ROOT/tmp/tabs.json"
jq '.[] | select(.name == "floats") | .are_floating_panes_visible' "$ROOT/tmp/tabs.json"
```

The check reads `true`. After the repair, expect the same exit and dock, a sidebar 60 columns wide, the floating pane still listed in `list-panes`, and `are_floating_panes_visible` now `false`: the add hides the floating panes and never closes them.

## First frame of a new tab

A new tab's sidebar seeds its first frame from the warm host's last delivered fold in memory ([state.md](../internals/sidebar/state.md)), so its first non-blank screen already holds the room's cards. A cold host has no seed and paints the placeholder until its first fold. The seed is projected for the new pane. Until a frame lists this new sidebar in its topology, focus derivation seats no selection, on the seed and the first cached fetch fold alike: every card rests at its configured density, with no card open by selection, no spines, no lane, and no header seal, even when the fold's focus names an agent in the previous tab. The loop-state regression test checks both paints before the correction. To check the live backend, poll the new pane from the moment the tab opens with one of the scripts below. Save it under the room's root and run it with `cargo xtask sandbox in <root> -- <script>`, since it needs the room's environment. Each script writes one file per non-blank capture, named by the milliseconds since the tab was opened.

tmux, given the room's tmux socket, its session, and an output directory:

```sh
#!/bin/sh
T="tmux -S $1"; OUT=$3; mkdir -p "$OUT"
$T list-panes -a -F '#{pane_id}' | sort > "$OUT/before"
t0=$(date +%s%N)
$T new-window -t "$2:"
i=0
while [ $i -lt 120 ]; do
  now=$(( ($(date +%s%N) - t0) / 1000000 ))
  for p in $($T list-panes -a -F '#{pane_id}' | sort | comm -13 "$OUT/before" -); do
    f="$OUT/$(printf '%05d' $now)-${p#%}.txt"
    $T capture-pane -p -t "$p" > "$f" 2>/dev/null
    grep -q '[^[:space:]]' "$f" || rm -f "$f"
  done
  i=$((i+1)); sleep 0.01
done
```

Zellij, given the session, the testkit `rimz` binary, an output directory, and the next terminal ids (the highest id in `rimz pane list` plus one onward; a new tab takes two or three):

```sh
#!/bin/sh
S=$1; B=$2; OUT=$3; shift 3; mkdir -p "$OUT"
t0=$(date +%s%N)
zellij --session "$S" action new-tab
i=0
while [ $i -lt 32 ]; do
  for n in "$@"; do
    now=$(( ($(date +%s%N) - t0) / 1000000 ))
    f="$OUT/$(printf '%05d' $now)-$n.txt"
    "$B" --zellij pane capture "zellij:terminal_$n" > "$f" 2>/dev/null
    grep -q '[^[:space:]]' "$f" || rm -f "$f"
  done
  i=$((i+1))
done
```

Capture the whole interval, not just the settled frame: the 32-iteration Zellij loop spans about three seconds. A tmux capture is a few milliseconds apart, so the first file is close to the first paint. A Zellij capture goes through the session and takes about 0.2 s, so it proves the first screen seen and bounds the correction only coarsely.

Attribute the captures with the [focus trace](../internals/diagnostics.md#where-diagnostics-land), not just the final screen. Set `FOCUS_LOG` to the room's persistent `audit/focus.log.jsonl` (the state directory is printed by `rimz paths`), and `INSTANCE_ID` to the new sidebar's instance from the testkit command `rimz sidebar renderers`. Record `START_MS=$(date +%s%3N)` immediately before running the capture script and `END_MS=$(date +%s%3N)` immediately after it. Run this where the log is readable, and keep earlier publications in the input so a seed can correlate to its pre-tab frame:

```sh
jq -s --arg instance "$INSTANCE_ID" --argjson start "$START_MS" --argjson end "$END_MS" '
  [.[] | select(.event.kind == "frame_published")] as $frames
  | .[]
  | select(.event.kind == "fold_decided" and .instance_id == $instance
      and .at_ms >= $start and .at_ms <= $end)
  | . as $fold
  | {
      at_ms,
      fold: .event,
      frame: ([$frames[]
        | select(.workspace_id == $fold.workspace_id and .session_name == $fold.session_name
            and .event.produced_at_ms <= ($fold.event.panes_produced_at_ms // 0))]
        | max_by(.event.produced_at_ms) | .event)
    }
' "$FOCUS_LOG"
```

Each output pairs a `fold_decided` with the latest `frame_published` whose `produced_at_ms` is at or below the fold's `panes_produced_at_ms`. Equality is not required: identical publications and folds append no record. A missing correlated publication needs the earlier log, not an assumption that the fold was correct. Read `source`, `phase`, `focus_origin`, and `fused_event_sent_at_ms` beside the captures to tell publication truth from an event-fused decision.

The run passes when all of these hold:

1. **Captures.** The earliest file for the sidebar pane already names the room's agents. Any selection belongs to the new tab, never the previous tab's pane. A seed from before the tab's birth has no selection-open card and no `▌`, `▐`, `▎`, or `🮇` cells.
2. **Folds.** Every `fold_decided` has `baseline` and `selected_after` absent or naming a pane in the new tab. A seed can rest on a pre-tab publication that names the old pane, but then it seats nothing: a seed resting on a fold older than the tab has `own_view: false`.
3. **Publications.** Every `frame_published` from the roster growth (`pane_count` increases) up to the first later `focus_origin: session_focus` publication that names the new shell, either names the new shell, has no `focused_pane`, or keeps the pre-growth focus with `focus_origin: prior` and `client_sample_withheld: true`. A `client_view` or `session_focus` publication naming the pre-tab pane in that span fails.
4. **Zellij seating.** The new attachment's first `fold_decided` with `panes_produced_at_ms` at or after the roster growth carries `baseline` and `selected_after` naming the new shell. No already-open attachment appends a fold with `selected_before` set and `selected_after` absent, including idle selections past the six-row cap. `audit/diag.log.jsonl` has no `newborn_quarantined` for these shells.

Rule 4 depends on the root pid, which lets the producer derive the new shell's command and cwd before admission, so the shell is a row in the roster publication itself. Read one new shell's `/proc/<pid>/cmdline` to confirm its argv[0] is the shell path, not a login name starting with `-`. A missing root pid or unreadable process entry falls back to waiting for Zellij's command and cwd events. On tmux, expect the new tab to seat within tens of milliseconds of opening.

Save the room's whole `audit/` directory beside the run report, not only `focus.log.jsonl`: row admission is recorded in `audit/diag.log.jsonl` and `audit/binding.log.jsonl`, and the focus trace cannot say why a pane had no row. When a record fails a rule, repeat the same script on a build of the branch's merge-base and report both runs. A failure that the merge-base build reproduces is already on the trunk: report it as an advisory in the PR and do not block the change on it. A failure that only the branch shows blocks.

A stale seed is a race that has shown up about once in 18 tabs on tmux, so one tab proves little. Wrap the tmux script in a loop that opens 18 tabs and keeps each tab's window and new pane ids beside its captures. Given the tmux script's path, the socket, the session, and an output directory:

```sh
#!/bin/sh
ONE=$1; SOCK=$2; S=$3; OUT=$4
n=1
while [ $n -le 18 ]; do
  D="$OUT/tab$(printf '%02d' $n)"; mkdir -p "$D"
  date +%s%3N > "$D/start_ms"
  "$ONE" "$SOCK" "$S" "$D"
  date +%s%3N > "$D/end_ms"
  tmux -S "$SOCK" list-panes -a -F 'tmux:#{pane_id}' | sort > "$D/after"
  sed 's/^/tmux:/' "$D/before" | comm -13 - "$D/after" > "$D/new"
  sleep 1.5
  n=$((n+1))
done
```

Then judge rule 2 for every `fold_decided` in each tab's window, from every sidebar, with `OUT` that output directory and `FOCUS_LOG` as above:

```sh
for D in "$OUT"/tab*; do
  jq -c --argjson start "$(cat "$D/start_ms")" --argjson end "$(cat "$D/end_ms")" \
    --arg tab "${D##*/}" --rawfile new "$D/new" '
    ($new | split("\n") | map(select(. != ""))) as $panes
    | select(.event.kind == "fold_decided" and .at_ms >= $start and .at_ms <= $end)
    | .event as $fold
    | {
        tab: $tab, at_ms, instance_id, fold: $fold,
        ok: (([$fold.baseline, $fold.selected_after] | map(select(. != null))
              | all(. as $pane | $panes | index($pane) != null))
          and ($fold.seed != true or $fold.own_view == false
              or ($fold.panes_produced_at_ms // 0) >= $start))
      }
  ' "$FOCUS_LOG"
done > "$OUT/folds.jsonl"
jq -c 'select(.ok | not)' "$OUT/folds.jsonl"
```

The 18-tab run passes when the last command prints nothing and a warm host's `folds.jsonl` holds 18 records with `seed: true`, one per tab. The judge covers rule 2 only: read rule 3 from the `frame_published` records with the first `jq` block.

Two Zellij checks guard the neighbours of these rules, read from the same log. With focus resting on a pane, open and close a pane in another tab without moving focus (`zellij action new-pane --tab-id <n> --no-focus`, then `close-pane --pane-id <id>`): no `frame_published` moves `focused_pane` off the resting pane, and no attachment appends a `fold_decided`. Then switch between two existing tabs (`zellij action go-to-tab <n>`): no publication carries `client_sample_withheld`, and the destination sidebar's seating fold has `fused_event_sent_at_ms` set and lands before the `session_focus` publication that names the destination, under about 100 ms in a small room.

## Inspect the data behind a card

A frame shows what the renderer decided; `rimz sidebar snapshot --json` computes the same view-model in the calling process. Run the binary you built, not the installed one. The default invokes a private mux listing (a fresh Zellij pushed topology can answer without a subprocess), probes agent projection privately, and refreshes heavy lanes. It does not publish the host's pane frame, agent projection, or metrics history and cannot nudge the host with a pane-publication wake. `--no-produce` passively reuses the host's publications and forks neither mux nor git, making it the cheap read in a room you would rather not disturb.

One rendered card is a row in a worktree group, and its clocks and card fields live there:

```sh
target/debug/rimz sidebar snapshot --json > /tmp/snap.json
jq '.worktree_groups[].rows[] | select(.handle == "coder")' /tmp/snap.json
```

`.agents[]` on the same snapshot is the rollup the fold reads, not what the card paints: it carries each session's own unfolded clock and no nested children, so a check that reads it will report the display fields missing. Row-level projections exist only under `.worktree_groups[].rows[]`: the folded `status`, the `attention_score` that orders the card, and the nested `sub_agents[]`. A row has no `attention` or `display_status` key, so a `jq` filter naming one prints `null` rather than failing. A child that lifts its parent shows in the row's `attention_score` and the group's `status_counts`, while the row's own `status` can stay `idle`.

Two rules for a check whose subject is time:

- A room's own agents are the only real delegating parents available. A sandbox room's agents are synthetic and do not delegate work to provider subagents, so a scenario that needs a parent waiting on live subagents runs in the room you are working in: launch a child that keeps working, then observe the parent. A Claude-native child line whose check needs no live wait replays in the sandbox instead ([Native subagent replay](#native-subagent-replay)).
- Put the wait in a background script (`sleep <secs>` followed by the capture commands, run detached) rather than a foreground sleep. An agent harness refuses a long foreground sleep, and any tool call you make during the window stamps the very activity clock you are trying to age.

### Age a card into a stale line with an expired cache

A stale line or an expired cache clock takes a day or an hour at the defaults. Shorten both in the held room, with `$ROOT` and `$BIN` from the room card (`config set` writes `theme.display` keys to the room's `theme.toml` and the rest to `config.toml`; `rimz doctor` confirms both parse):

```sh
rz() { target/debug/xtask sandbox in "$ROOT" -- "$BIN" --tmux "$@"; }
rz config set theme.display.stale_after_secs 60
rz config set harness.prompt_cache_margin 5s
rz config set harness.prompt_cache_ttl.claude 1m
rz config set harness.cache_keepalive false
rz config set harness.idle_compact off
```

The stub agents carry no session, so feed each one a Claude turn that ends (a statusline gives its card a meter, which is where the full card pins its age clock), then mark it read, since an unread card never shrinks:

```sh
s=$(uuidgen); w="$ROOT/home/room-worktrees/probe"
feed() { target/debug/xtask sandbox in "$ROOT" --as '@coder#probe' -- "$BIN" --tmux "$@"; }
jq -nc --arg s "$s" --arg w "$w" '{hook_event_name:"SessionStart",session_id:$s,cwd:$w,source:"startup"}' | feed hooks feed --source claude
jq -nc --arg s "$s" --arg w "$w" '{hook_event_name:"UserPromptSubmit",session_id:$s,cwd:$w,prompt:"stale probe"}' | feed hooks feed --source claude
jq -nc --arg s "$s" '{session_id:$s,model:{id:"claude-sonnet-4-6",display_name:"Sonnet 4.6"},context_window:{context_window_size:200000,used_percentage:20,current_usage:{input_tokens:10000,cache_read_input_tokens:30000,cache_creation_input_tokens:0,output_tokens:100}},cost:{total_cost_usd:0.01}}' | feed statusline feed --source claude
jq -nc --arg s "$s" --arg w "$w" '{hook_event_name:"Stop",session_id:$s,cwd:$w,last_assistant_message:"done"}' | feed hooks feed --source claude
rz sidebar mark-read '@coder#probe'
```

After a minute the card shrinks to its stale line, and a minute past the last request its cache expires. Capture with `rz pane capture --ansi <sidebar>` to keep the colours: plain capture drops the SGR runs a tone check compares. Selecting the stale card expands it to show the meter's age clock, and the layout freeze holds that shape for a while after selection moves away, so capture each shape after the frame settles. `rz config set harness.prompt_cache_ttl.claude off` takes the cache clock away for the unknown-lifetime case.

## Land the team's worktree

A fresh room's `probe` worktree is pristine, not finished, so it draws no receipt and no landed chip. To check a folded finished group, give each agent a finished turn, close the board, and merge a commit from the worktree into the trunk. The `--no-ff` merge matters: the branch commit that is not on the trunk's first-parent line is the evidence that the worktree did work. Run this with `$ROOT`, `$BIN` and `<mux>` from the room card:

```sh
WT="$ROOT/home/room-worktrees/probe"
feed() {
  jq -nc --arg event "$2" --arg role "$1" --arg cwd "$WT" \
    '{hook_event_name:$event,session_id:("landed-"+$role),cwd:$cwd,source:"startup",prompt:"landed check"}' |
    target/debug/xtask sandbox in "$ROOT" --as "@$1#probe" -- "$BIN" --<mux> hooks feed --source claude
}
for role in coder reviewer; do feed "$role" SessionStart; feed "$role" UserPromptSubmit; done
sleep 2
for role in coder reviewer; do feed "$role" Stop; done
target/debug/xtask sandbox in "$ROOT" -- "$BIN" --<mux> teams flip Done 'landed check' --team forge
target/debug/xtask sandbox in "$ROOT" --cwd "$WT" -- git commit --allow-empty -m 'Exercise landed receipt'
target/debug/xtask sandbox in "$ROOT" --cwd "$ROOT/home/room" -- git merge --no-ff probe -m 'Land probe'
target/debug/xtask sandbox in "$ROOT" -- "$BIN" --<mux> sidebar snapshot --json |
  jq '.worktree_groups[] | {key, finished, clean, trunk_sync}'
```

The `probe` group should report `finished: true`, `clean: true` and `trunk_sync: "merged"`. Focus a pane outside the group (a new pane started in `$ROOT/home/room`), since focusing a member keeps its cards open. Then click `[ ▸ 1 landed ]` to show the receipt.

## Native subagent replay

The sandbox's `claude` is a stub (`xtask/assets/sandbox-room/claude`), so no real Claude child ever starts in a held room. A change to a Claude-native child line (its tokens, model, status, or clock) is still checkable in one pass: copy a real parent and child transcript into the room, then feed the parent's hooks and the child's `subagentStatusLine` payload by hand, in the order Claude would send them.

**Setup.** Hold a tmux room from the testkit build (see [Hold a room and join it](#hold-a-room-and-join-it)). From its card note the root, the `@coder#probe` As prefix, and a `#probe` sidebar. `--as` supplies the agent's session keys and hook owner pid; no process-environment copy is needed.

**Transcripts.** Pick one Claude session that launched a subagent and copy exactly two files from `~/.claude/projects/<project>/` into the room's `tmp/replay/`, keeping their relative layout: `<session>.jsonl` and `<session>/subagents/agent-<child>.jsonl`. The adapter derives the child directory from the parent transcript's path (`subagents_dir` in `agents/adapters/claude/subagents.rs`), so a flattened copy reads no child. Copy nothing else from `~/.claude`: no credentials, no settings.

When no session there holds a `subagents/` child file, run the replay without transcripts: drop `transcript_path` and `agent_transcript_path` from the payloads below, and give the model on `SessionStart` (`"model":"claude-sonnet-4-6"`) and the child's `model` and `effort` on its `tasks[]` entry. The child's line then shows status, model, effort, compactions, and its clock, but no token figure, so the token pass rules below do not apply.

```sh
mkdir -p "$ROOT/tmp/replay/$SESSION/subagents"
cp ~/.claude/projects/<project>/"$SESSION".jsonl "$ROOT/tmp/replay/"
cp ~/.claude/projects/<project>/"$SESSION"/subagents/agent-"$CHILD".jsonl "$ROOT/tmp/replay/$SESSION/subagents/"
```

**Payloads.** Four JSON files, each fed on stdin. `transcript_path` is the copied parent, `cwd` is the card's Worktree, and `agent_id` / `tasks[].id` is `<child>` from the child file's name (`agent-<child>.jsonl`). A `tokenCount` unlike the transcript's figure shows which source the line paints.

```json
{"hook_event_name":"SessionStart","session_id":"<session>","cwd":"<root>/home/room-worktrees/probe","source":"startup","transcript_path":"<root>/tmp/replay/<session>.jsonl"}
{"hook_event_name":"SubagentStart","session_id":"<session>","agent_id":"<child>","agent_type":"Explore","cwd":"<root>/home/room-worktrees/probe","transcript_path":"<root>/tmp/replay/<session>.jsonl"}
{"columns":80,"transcript_path":"<root>/tmp/replay/<session>.jsonl","tasks":[{"id":"<child>","type":"Explore","status":"running","description":"Replay occupancy check","tokenCount":999999}]}
{"hook_event_name":"SubagentStop","session_id":"<session>","agent_id":"<child>","agent_type":"Explore","cwd":"<root>/home/room-worktrees/probe","transcript_path":"<root>/tmp/replay/<session>.jsonl","agent_transcript_path":"<root>/tmp/replay/<session>/subagents/agent-<child>.jsonl"}
```

**Feed.** Run each as the parent pane, with the built binary by its absolute path (`$PWD/target/debug/rimz` from the worktree root). `SubagentStop` is the one that carries `agent_transcript_path`, so check the running line between the third and fourth feeds.

```sh
x() { target/debug/xtask sandbox in "$ROOT" --as '@coder#probe' -- "$PWD/target/debug/rimz" --tmux "$@"; }
x hooks feed --source claude < parent-start.json
x hooks feed --source claude < child-start.json
x statusline feed --source claude --subagent < child-feed.json
# observe the running line here
x hooks feed --source claude < child-stop.json
```

Each feed exits 0 with no output.

To check a child's `↻ N` compaction count, feed a completed compaction attributed to the child between the status-line feed and the stop: a `PreCompact` then a `PostCompact`, each with the parent's `session_id` and the child's `agent_id`. The closed pair adds one to the child's `compaction_count` in the snapshot and `↻ 1` to the end of its second line.

```json
{"hook_event_name":"PreCompact","session_id":"<session>","agent_id":"<child>","agent_type":"Explore","cwd":"<root>/home/room-worktrees/probe","trigger":"auto","custom_instructions":null}
{"hook_event_name":"PostCompact","session_id":"<session>","agent_id":"<child>","agent_type":"Explore","cwd":"<root>/home/room-worktrees/probe","trigger":"auto","compact_summary":"Sidebar compaction replay."}
```

A payload held in a zsh variable goes to the feed through `printf '%s\n' "$payload" |`, not `echo`: zsh's `echo` expands the `\n` escapes inside JSON strings and the feed reads corrupt JSON.

**Observe.** Read the child three ways, running and again after the stop:

```sh
target/debug/xtask sandbox in "$ROOT" -- "$PWD/target/debug/rimz" --tmux sidebar snapshot --json > /tmp/snap.json
jq -c --arg c "$CHILD" '.worktree_groups[].rows[].sub_agents[]? | select(.id == $c) | {status, tokens}' /tmp/snap.json
target/debug/xtask sandbox in "$ROOT" -- "$PWD/target/debug/rimz" --tmux agents show '@coder#probe' --json > /tmp/show.json
jq -c '.agent.sub_agents[].tokens' /tmp/show.json
```

`agents show --json` wraps the card in an `agent` envelope, so a bare `.sub_agents` reads null. The frame comes from the pane's Look then Capture, as in the [seven checks](#seven-checks). The expected window is the newest assistant request in the child transcript, prompt side only:

```sh
jq -s '[.[] | select(.type == "assistant") | .message.usage] | last | .input_tokens + .cache_read_input_tokens + .cache_creation_input_tokens' "$ROOT/tmp/replay/$SESSION/subagents/agent-$CHILD.jsonl"
```

The pass rules, read running and again after the stop:

- The snapshot's `tokens.window` equals that sum on both reads, and `status` moves from `running` to `success`. The payload's `tokenCount` of 999999 never replaces it.
- `agents show --json` reports the same `tokens`.
- The child line in the frame shows a spinner while running and `✓` after the stop, with a second line `▤ <window> · <model>`, the window abbreviated in thousands (`121k`).

The replay proves the adapter and renderer path from hook to frame. It does not prove that Claude sends these payloads in this order or shape: that contract lives in [claude-reference.md](../externals/agent-adapter/claude-reference.md). The recipe runs on Zellij with `--zellij` in place of `--tmux`.

**Quiet and stalled child.** To watch a native child go quiet and then stall, feed the first three payloads and leave the stop unfed. The child's entry shows plain runtime, the faced quiet clock from five minutes of silence, and `!` with `silent Nm` once `stalled_after_secs` has passed. Shorten that window before the feed by appending to the sandbox home's config:

```sh
printf '[agents.attention]\nstalled_after_secs = 480\n' >> "$ROOT/home/.rimz/config.toml"
```

`jq .attention.stalled_after_secs` on the snapshot confirms the room read it. One transcript pair replays once per room: a second copy fed to the room's other agent moves the session off the first card, and its child arrives already `success`, so a second scenario takes a fresh room.

After SessionStart, the same `x()` helper also identifies the caller for commands such as `wait` ([Run a command as an agent](#run-a-command-as-an-agent)).

## Run a command as an agent

Plain `sandbox in` scrubs the caller's session keys and does not descend from a room agent. A caller-sensitive command such as `rimz wait --check true` therefore refuses:

```console
error: arming a wait is only available to an agent RimZ can identify; run this command from an agent pane
```

`--as` copies the selected agent's session keys while retaining the sandbox roots, and sets `RIMZ_AGENT_PID` to the stub for hook attribution. It runs the command directly with inherited stdio, like plain `in`. Neither flag sends SessionStart: before an explicit feed, `wait` refuses the provisional session:

```console
error: the calling agent has not registered a real session yet
```

**Recipe.** From the checkout root, set `ROOT` to the card's sandbox root and `MUX` to its backend (`tmux` or `zellij`). Feed one SessionStart as the selected agent, then run the command. For a transcript replay, use its chosen session id instead of minting one here.

```sh
BIN="$PWD/target/debug/rimz"
a() { target/debug/xtask sandbox in "$ROOT" --as '@coder' -- "$BIN" "--$MUX" "$@"; }
rz() { target/debug/xtask sandbox in "$ROOT" -- "$BIN" "--$MUX" "$@"; }
jq -n --arg session "$(cat /proc/sys/kernel/random/uuid)" --arg cwd "$ROOT/home/room-worktrees/probe" '{hook_event_name:"SessionStart",session_id:$session,cwd:$cwd,source:"startup"}' > "$ROOT/tmp/session-start.json"
a hooks feed --source claude < "$ROOT/tmp/session-start.json"
a wait --check true
target/debug/xtask sandbox in "$ROOT" --as-ancestor '@coder' -- "$BIN" "--$MUX" wait --check true
```

The room restarts the stub agent when its process is killed, so an agent leaves a tab only when its pane closes. No hand recipe makes an agent exit into a kept shell; the live `producer_*` backend tests cover that case.

`a` runs a RimZ command as the agent and `rz` runs one as the plain user shell (`rz agents`). Keep each prefix in a function, not a variable: zsh does not split an unquoted variable into words, so `$PREFIX agents` fails there as one unknown command.

ssh reads `~/.ssh` from your passwd home, not `$HOME`, so an SSH command a sandbox agent runs uses your real keys and config.

A held room's provider has no folder-trust decision for its worktree, so the first `a subagents claude ...` refuses with `claude has no trust decision for ...` and names `rimz trust grant --agents claude` as the fix. Run that grant as `rz trust grant --agents claude`: the plain-shell prefix writes the sandbox's home, while a bare `rimz trust grant` from your shell writes your real provider config. The launch then succeeds.

`--as-ancestor` starts the command below the stub, with exactly plain `in`'s environment: no `RIMZ_AGENT_*` or pane key. It relays stdin, stdout, and stderr through pipes, not a tty, and reports a failed command's exit status. An absent or dead serving seat refuses with the instruction to hold a fresh room with this build. Both modes default to the card's Worktree; `--cwd` overrides it. A relative program path containing `/` resolves from your checkout, as it does for plain `in`.

Session-death recovery needs the same feed for every agent. The stub agents hold provisional session ids, which the producer leaves out of `records/live-roster.json`, so an unprimed room publishes an empty roster while its agents are alive and the next `rimz start` has nothing to recover. Feed one SessionStart per agent and wait for the roster to name them before ending the session.

The feed exits 0 with no output, and it runs once per room: later `a` calls reuse the registered session. A SessionStart alone leaves the card fresh, and a fresh card has no subagents or waits line, so a wait armed now stays off the frame. To check delegation lines, also feed `UserPromptSubmit` (with a `prompt` field) and `Stop` for the same session before arming. The card then reads sleeping, with `⧖ waits (n)` and its entries. A fed hook lands asynchronously: before the step that reads it, poll `rz agents show '@coder#probe'` until its status reflects the hook.

On both backends, both `wait` calls must resolve the caller to `@coder#probe`. The arming lines read like this, `--as` first and `--as-ancestor` second (wait names are random):

```console
armed wait-still-marker: check: true → @coder#probe
armed wait-simple-lane: check: true → @coder#probe
```

This check proves caller resolution, not delivery of the wait's result to the agent. To inspect hook attribution, use `sidebar snapshot --json` and read the selected agent's `runtime_owner` under `agents`: `agents show --json` does not expose that field.

The listing that follows `armed` can read `starting` before the detached watcher takes its lock. A watch without a live watcher reads `lost` only after the host's 30-second startup grace; `wait list` and `loop list` use the same grace.

## Cache clock band

A card's cache-age pin and its `cache expires in Nm` line move with the agent's quiet time against the provider's prompt-cache lifetime, an hour for Claude. Scenario playback cannot show them: a story's cards carry no cache lifetime, so every pin reads muted. In a held room, shorten the lifetime so the warning band arrives within minutes. The room picks the setting up without a restart. With the `rz` prefix from [Run a command as an agent](#run-a-command-as-an-agent):

```sh
rz config set harness.prompt_cache_ttl.claude 24m
```

With a 24-minute lifetime the band starts at 12 minutes quiet (half the lifetime, which comes before its last ten minutes) and the cache expires at 24. Prime the agent as that section does: a SessionStart, then `UserPromptSubmit` and `Stop` for the same session. Select the card with the sidebar's Click command so its stats and cache line show. Confirm that the card's row reads `cache.ceiling_secs` 1440 ([Inspect the data behind a card](#inspect-the-data-behind-a-card)), then run Look and Capture from a detached script at each quiet age you check, as the time rules there say. The pin stays muted before 12 minutes. From 12 it turns yellow and the countdown line appears, it is orange near 18 and red near 24, and past 24 it reads a greyed-red `◉` with `cache expired`.

## Capture a CLI frame at a chosen size

A full-screen CLI view such as `rimz loop watch` paints its own terminal, so a capture of a room pane shows it only at that pane's size. To read its frame at a width and height you choose, run it on a private tmux server inside the sandbox and capture that server's pane. With `$BIN` the card's `rimz` path and `ROOT` its sandbox root, from the checkout root:

```sh
cat > "$ROOT/tmp/frame.sh" <<EOF
#!/bin/sh
tmux -L frame new-session -d -s frame -x 64 -y 38 "$BIN loop watch --hold"
sleep 4
tmux -L frame capture-pane -p -t frame
tmux -L frame kill-server
EOF
chmod +x "$ROOT/tmp/frame.sh"
target/debug/xtask sandbox in "$ROOT" --cwd "$ROOT/home/room" -- "$ROOT/tmp/frame.sh"
```

The `--cwd` places the view in the held room, since `loop watch` refuses outside an open room. To compare sizes in one run, start one session per size on the same server before the `sleep`.

Check a line that fills the view's last column here, not only in a unit test. An erase sent while the cursor sits on a line's last cell clears that cell in tmux, as in a real terminal, while the `vt100` crate behind the unit and integration tests keeps it. That is why `crates/rimz/src/tui.rs::write_crlf` issues each erase while the cursor is still at the start of its line. Running the view under `script` with an `stty` size printed no frame, for a reason not established, so use tmux for this capture.

## Capture kitty graphics

A held room draws cell bars, never pixel meters or pixel pets, because no kitty-capable client is looking at it. On tmux the sidebar sends kitty graphics only when every rendering client's termname is a kitty-capable one (`sidebar_pane/pixel/probe.rs` `termname_allowed`: `xterm-kitty`, `kitty`, `xterm-ghostty`, `ghostty`), and the room keeps its own `tmux-256color` client attached. To check which image ids go out, the `p=` on each placement, or the `d=I` deletes, replace that client with a recorded one that reports `xterm-kitty`. This recipe is tmux-only.

Start the room with `env -u NO_COLOR`: with `NO_COLOR` set, pets and meters are suppressed by design. Zellij renders pets and meters on the cell tier only, so pixel checks are tmux-only.

A held room shows no pet until one is configured. To check pixel pets, turn one on in the room's theme file, `$ROOT/home/.rimz/theme.toml`, under its `[theme.pets]` table (pets are a theme setting, so a `[pets]` table in `config.toml` does nothing): set `enabled = true`, `glyphs = "pixel"`, and `pet` to the path of a local 1536x1872 PNG sheet, which avoids a network fetch. The running sidebar picks the change up within a few seconds.

Set `ROOT` and `SESSION` from your card's `Sandbox root:` and `Session:` values. The machine usually has no `xterm-kitty` terminfo, so alias one from `xterm-256color` under the room's tmp. Then detach the room's rendering client (control-mode `1` lines are RimZ's own link; leave them), and attach a `script`-recorded client for 30 seconds, long enough for the sidebar's 10-second caps refresh to see it:

```sh
S="$ROOT/runtime/rimz/tmux/server"
t() { target/debug/xtask sandbox in "$ROOT" -- tmux -S "$S" "$@"; }
mkdir -p "$ROOT/tmp/terminfo"
infocmp -x xterm-256color | sed '1,/^xterm-256color|/s/^xterm-256color|/xterm-kitty|/' | tic -x -o "$ROOT/tmp/terminfo" -
t list-clients -F '#{client_control_mode} #{client_termname} #{client_name}'
t detach-client -t "$(t list-clients -F '#{client_control_mode} #{client_name}' | awk '$1==0{print $2}')"
target/debug/xtask sandbox in "$ROOT" -- sh -c "TERMINFO='$ROOT/tmp/terminfo' TERM=xterm-kitty timeout 30 script -qfec 'tmux -S $S attach -t $SESSION' '$ROOT/tmp/kitty.raw'" < /dev/null > /dev/null
grep -ao $'\e_G[^;\e]*' "$ROOT/tmp/kitty.raw" | sed -E 's/i=[0-9]+/i=N/; s/[xy]=[0-9]+//g' | sort | uniq -c | sort -rn
```

`detach-client -a` detaches every client except the one named, so name the rendering client directly as above. The tally has this shape, one line per distinct command with its count first (counts depend on the room):

```text
   <n> _Ga=d,d=I,i=N,q=2
   <n> _Gm=0
   <n> _Ga=t,f=100,i=N,q=2,m=1
   <n> _Ga=p,U=1,i=N,p=1,c=38,r=1,q=2
```

The `d=I` lines are the sweeps a sidebar makes over its leased id window before its first transmit. The `a=t` transmits are chunked (`m=1` then `m=0`), and each virtual placement carries `p=1`. Drop `-o` and read the raw lines to see the actual ids.

## Replay a kitty capture through Ghostty

The tally above shows what RimZ sent, which does not prove how a terminal parsed it. A graphics fix that depends on ordering (two sidebars on one tmux client, say) is proven only at the consumer, so feed the `kitty.raw` from [Capture kitty graphics](#capture-kitty-graphics) through Ghostty's own stream and APC parser, and record the result of every kitty command.

Check out the Ghostty tag under the room's tmp. Ghostty v1.3.1 builds with zig 0.15.2, which `uvx` fetches from PyPI, so no system zig is needed:

```sh
G="$ROOT/tmp/ghostty"
git clone -q --depth 1 --branch v1.3.1 https://github.com/ghostty-org/ghostty "$G"
uvx --from ziglang==0.15.2 python -m ziglang version
```

The harness is a zig test inside Ghostty's kitty module. It mirrors `ghostty-org/ghostty@v1.3.1:src/termio/stream_handler.zig::StreamHandler.apcEnd`: APC bytes go to Ghostty's `apc.Handler`, each completed kitty command runs through `Terminal.kittyGraphics`, and every other action goes to the read-only stream handler. RimZ sends `q=2`, which suppresses the reply an error would produce, so a patch to `graphics_exec.zig` records each response message in a file-scope variable before the quiet check discards it:

```sh
cd "$G" && git apply <<'EOF'
--- a/src/terminal/kitty/graphics_exec.zig
+++ b/src/terminal/kitty/graphics_exec.zig
@@ -12,6 +12,10 @@ const Image = image.Image;
 const ImageStorage = @import("graphics_storage.zig").ImageStorage;

 const log = std.log.scoped(.kitty_gfx);
+var rimz_replay_result: []const u8 = "OK (no response)";
+test "petchunks-replay" {
+    try @import("petchunks_replay.zig").run(&rimz_replay_result);
+}

 /// Execute a Kitty graphics command against the given terminal. This
 /// will never fail, but the response may indicate an error and the
@@ -74,6 +78,7 @@ pub fn execute(
         => .{ .message = "ERROR: unimplemented action" },
     };

+    rimz_replay_result = if (resp_) |resp| resp.message else "OK (no response)";
     // Handle the quiet settings
     if (resp_) |resp| {
         if (!resp.ok()) {
EOF
cat > "$G/src/terminal/kitty/petchunks_replay.zig" <<'EOF'
const std = @import("std");
const Terminal = @import("../Terminal.zig");
const apc = @import("../apc.zig");
const stream = @import("../stream.zig");
const readonly = @import("../stream_readonly.zig");

pub fn run(result: *const []const u8) !void {
    const alloc = std.heap.smp_allocator;
    const input = try std.process.getEnvVarOwned(alloc, "PETCHUNKS_INPUT");
    defer alloc.free(input);
    const output = try std.process.getEnvVarOwned(alloc, "PETCHUNKS_OUTPUT");
    defer alloc.free(output);
    const bytes = try std.fs.cwd().readFileAlloc(alloc, input, 100 * 1024 * 1024);
    defer alloc.free(bytes);
    var file = try std.fs.cwd().createFile(output, .{});
    defer file.close();
    var terminal = try Terminal.init(alloc, .{ .rows = 50, .cols = 200 });
    defer terminal.deinit(alloc);
    const H = struct {
        terminal: *Terminal,
        alloc: std.mem.Allocator,
        apc_handler: apc.Handler = .{},
        file: std.fs.File,
        result: *const []const u8,
        commands: usize = 0,
        errors: usize = 0,
        pub fn deinit(self: *@This()) void {
            self.apc_handler.deinit();
        }
        pub fn vt(self: *@This(), comptime action: stream.Action.Tag, value: stream.Action.Value(action)) !void {
            switch (action) {
                .apc_start => self.apc_handler.start(),
                .apc_put => self.apc_handler.feed(self.alloc, value),
                .apc_end => {
                    var cmd = self.apc_handler.end() orelse return;
                    defer cmd.deinit(self.alloc);
                    switch (cmd) {
                        .kitty => |*kitty_cmd| {
                            _ = self.terminal.kittyGraphics(self.alloc, kitty_cmd);
                            self.commands += 1;
                            const message = self.result.*;
                            if (!std.mem.startsWith(u8, message, "OK")) self.errors += 1;
                            var buf: [1024]u8 = undefined;
                            const line = try std.fmt.bufPrint(&buf, "command={d} action={s} result={s}\n", .{ self.commands, @tagName(kitty_cmd.control), message });
                            try self.file.writeAll(line);
                        },
                    }
                },
                else => {
                    var handler = readonly.Handler.init(self.terminal);
                    try handler.vt(action, value);
                },
            }
        }
    };
    var parser = stream.Stream(H).initAlloc(alloc, .{ .terminal = &terminal, .alloc = alloc, .file = file, .result = result });
    defer parser.deinit();
    try parser.nextSlice(bytes);
    var buf: [256]u8 = undefined;
    var total: usize = 0;
    var ids = terminal.screens.active.kitty_images.images.keyIterator();
    while (ids.next()) |id| {
        try file.writeAll(try std.fmt.bufPrint(&buf, "image={d}\n", .{id.*}));
        total += 1;
    }
    try file.writeAll(try std.fmt.bufPrint(&buf, "SUMMARY commands={d} errors={d} images={d}\n", .{ parser.handler.commands, parser.handler.errors, total }));
}
EOF
```

Run the test on the capture. The harness reads its input and output paths from the environment, and writes one `command=… action=… result=…` line per kitty command, one `image=` line per image left in storage, and a `SUMMARY` line:

```sh
cd "$G" && PETCHUNKS_INPUT="$ROOT/tmp/kitty.raw" PETCHUNKS_OUTPUT="$ROOT/tmp/kitty.results" \
  uvx --from ziglang==0.15.2 python -m ziglang build test -Dapp-runtime=none -Doptimize=ReleaseFast -Dtest-filter=petchunks-replay
grep SUMMARY "$ROOT/tmp/kitty.results"
awk -F 'result=' '/result=/ && $2 !~ /^OK/ { n[$2]++ } END { for (e in n) print n[e], e }' "$ROOT/tmp/kitty.results"
```

The first build compiles Ghostty's test binary, which takes about two minutes with a warm zig package cache and longer when it must fetch Ghostty's dependencies. A rerun on another capture reuses it and takes seconds. Replay a capture from the base branch and one from your change, and compare the two `SUMMARY` lines: a clean replay reads `errors=0`. The harness parses a byte stream in one pass, so it covers Ghostty's parser and image storage, not the app's rendering, and it pins one Ghostty tag. Newer Ghostty can differ (on `main` a delete also cancels a half-received chunked image), so name the tag you replayed against.

## Hibernate an agent

A held room takes hibernation's skip path as it comes. Two sandbox gaps cause this, neither of them a product bug, and a freeze, deepen, and wake check needs both closed:

- **User bus.** The sandbox's `XDG_RUNTIME_DIR` is `$ROOT/runtime`, so `systemctl --user` cannot reach the user manager. `$ROOT/runtime/rimz/ws/<room>/live/hibernate/admission.json` reads `"available": false`, and doctor's `active:` line names the unreachable manager. Link the bus into that directory (`ln -s /run/user/$(id -u)/bus /run/user/$(id -u)/systemd "$ROOT/runtime/"`), then start the room again so its birth publishes a fresh admission: `target/debug/xtask sandbox in "$ROOT" --cwd "$ROOT/home/room" -- "$BIN" --<mux> start "$ROOT/home/room" --no-attach`.
- **Stub cmdline.** The stub `claude` runs as `/bin/sh …/claude`, and provider classification reads `sh` (it unwraps only node, npx, and bun), so no provider pid is found. Edit the room's copy of the stub so its first line re-execs under the provider's name (`exec -a claude bash "$0" "$@"`, guarded so it runs once), then `rimz agents restart` the agent.

Set a short schedule in `$ROOT/home/.rimz/config.toml` (`[agents.hibernate]` with `after = "1m"`, `deep_after = "3m"`) and move client focus off the agent: a focused pane never freezes. Then read the scope's `cgroup.freeze` and `memory.stat`, run `agents show` for the hibernation line, `rimz message` the agent to wake it, and check the `wake` and `wake_refault` records in `audit/hibernate.log.jsonl` and the `Hibernate:` line of `rimz stats`.

## Check against a real provider login

A held sandbox room replaces `HOME`, so a check that needs a real Claude or Codex turn runs in a host room started from the branch binary. Every `rimz` that room spawns has to be the branch build: the sender's shell, the provider hooks (`rimz hooks feed` resolves from the agent's inherited `PATH`), and the sidebar host (which reads `RIMZ_BIN`). Start it from a host shell; `rimz start` refuses inside a RimZ scoped view, so an agent cannot start this room for you.

```sh
cargo build -p rimz --bin rimz
mkdir -p /var/tmp/rimz-livecheck/bin /var/tmp/rimz-livecheck/ws
cp target/debug/rimz /var/tmp/rimz-livecheck/bin/
export RIMZ_BIN=/var/tmp/rimz-livecheck/bin/rimz PATH=/var/tmp/rimz-livecheck/bin:$PATH
cd /var/tmp/rimz-livecheck/ws
rimz trust grant --agents claude,codex
rimz start --tmux
```

Before you read any timing, confirm from the shell that sends the messages that `command -v rimz` prints the staged path and `rimz --version` prints the branch build. If a login shell's rc (a `~/.zshrc` that prepends `~/.cargo/bin`, say) puts the installed `rimz` back first on `PATH`, the check silently runs the released binary and its result can read as a failure of your change.

A room built by hand, outside `cargo xtask sandbox`, needs `XDG_RUNTIME_DIR` set to a short path (a few characters under `/tmp`): socket paths under a long directory exceed the AF_UNIX limit and the room fails to start. The state root may be long.

## Check a workflow against a real provider

Use the [real-provider venue](#check-against-a-real-provider-login), not a held sandbox room. Keep the real `HOME` and account login, but isolate RimZ's state and runtime. Resolve the worktree's build directory rather than assuming `target/debug`:

```sh
cargo build -p rimz --bin rimz
export LIVE_SOURCE="$PWD" LIVE_ROOT="$(mktemp -d /tmp/w4XXXXXX)"
LIVE_BIN="$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)/debug/rimz"
mkdir -p "$LIVE_ROOT/bin" "$LIVE_ROOT/home/agents" "$LIVE_ROOT/home/cache" "$LIVE_ROOT/r"
chmod 700 "$LIVE_ROOT" "$LIVE_ROOT/home" "$LIVE_ROOT/r"
cp "$LIVE_BIN" "$LIVE_ROOT/bin/rimz"
cp -R "$HOME/.rimz/cache/providers" "$LIVE_ROOT/home/cache/"
mkdir -p "$LIVE_ROOT/home/workflows"
cp -R "$LIVE_SOURCE/crates/rimz/tests/fixtures/workflows/babysit-pr" "$LIVE_ROOT/home/workflows/"
git clone --quiet --no-hardlinks --branch "$(git branch --show-current)" "$LIVE_SOURCE" "$LIVE_ROOT/repo"
git -C "$LIVE_ROOT/repo" remote set-url origin git@github.com:rimio-ai/rimz.git
git -C "$LIVE_ROOT/repo" remote set-url --push origin disabled://workflow-live-proof
export RIMZ_HOME="$LIVE_ROOT/home" XDG_RUNTIME_DIR="$LIVE_ROOT/r"
export RIMZ_BIN="$LIVE_ROOT/bin/rimz" PATH="$LIVE_ROOT/bin:$PATH"
export CLAUDE_CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$(realpath "$HOME/.claude")}"
command -v rimz
rimz --version
sha256sum "$LIVE_BIN" "$RIMZ_BIN"
```

The clone's branch must already have a PR. The fixture reads that PR; the proof must not push, comment, merge, or otherwise edit it. Keep `prompts/sweep.md` unchanged. Put a `gh` shim at `$LIVE_ROOT/bin/gh`, make it executable, and allow only the read commands the check needs. For example, with the host's real `gh` at `/usr/bin/gh`:

```text
#!/bin/sh
case "${1:-} ${2:-}" in
    'pr view'|'pr checks'|'pr diff'|'run view'|'run list') exec /usr/bin/gh "$@" ;;
    *) printf 'live-proof guard: gh writes and non-allowlisted commands are disabled\n' >&2; exit 97 ;;
esac
```

Before starting the room, verify `command -v gh` names that shim. From the clone, `git push --dry-run origin HEAD` must fail against the disabled push URL, and both `gh pr comment <number> --body <text>` and `gh api --method POST <endpoint>` must be refused by the shim before any network request. Do not run those probes without first verifying the shim path.

Do not copy the machine config or named accounts. They can select account homes absent from the isolated state, or trigger unrelated hook preconditions. Use the shell's real `CLAUDE_CONFIG_DIR`; when the shell has none, the fallback above resolves its existing default login. Do not copy credentials, print account files, or modify that home beyond the clone-path trust grant below and normal provider sessions.

Create `$RIMZ_HOME/config.toml` with these contents, substituting the absolute `$LIVE_ROOT/agent-shell` path for the example path:

```text
[agents]
isolation = "scoped"
shell = "/tmp/REPLACE/agent-shell"

[agents.hibernate]
enabled = false

[accounts.use]
claude = "default"

[remote_control]
claude = false
codex = false
```

The `default` login is implicit; do not add an `[accounts.claude.default]` table. Remove unrelated project loop tasks from the disposable clone's `.rimz/config.toml` before granting project trust. Copy `$HOME/.rimz/agents/claude.md` into `$RIMZ_HOME/agents/claude.md` (a nonempty base is required), then create `$RIMZ_HOME/agents/live-claude.md`:

```text
---
agent: claude
model: haiku
description: Claude-only workflow live proof.
tools: [Bash, Edit, Glob, Grep, Read, Write]
subagents: []
---

This is an isolated live check. Never modify remote objects, remove the write guards, or launch children. If a guard prevents progress, end the turn with a concise report rather than wait for input.
```

Set every fixture agent reference to `live-claude`, and select `scope = "project"`. Leave `prompts/sweep.md` byte-identical to the committed fixture. For deterministic retry routing, replace only the copied `checks/pr.py` with:

```text
# /// script
# requires-python = ">=3.11"
# ///
import json
import os
import pathlib

context = json.loads((pathlib.Path(os.environ["RIMZ_WORKFLOW_DIR"]) / "context.json").read_text())
previous = context.get("prev_attempt") or {}
outcome = "needs-sweep" if previous.get("outcome") in {None, "died"} else "merged"
print(json.dumps({"outcome": outcome}))
```

Here `merged` is a synthetic routing result, not evidence that the real draft PR merged. The real sweep reads the PR but cannot merge it. Validate both profiles and the workflow before starting. Profiles need tools; omitting them fails validation. Do not add `skills: []` without the `Skill` tool.

A login shell can reorder `PATH` after launch, even with `RIMZ_BIN` correct. Create an executable `$LIVE_ROOT/agent-shell` with an absolute staging path substituted below:

```text
#!/bin/sh
export PATH=/tmp/REPLACE/bin:$PATH
exec /bin/bash --noprofile --norc "$@"
```

Verify the actual provider process's `PATH`, `RIMZ_BIN`, and `RIMZ_HOME`, not just the sending shell's. The configured shell also keeps provider tool shells on the staged binary.

Start from a detached tmux driver so the room host can outlive the sending tool call. Create `$LIVE_ROOT/start.sh` with these contents. Keep `LIVE_ROOT` and `CLAUDE_CONFIG_DIR` exported to the driver:

```text
#!/bin/sh
set -eu
export RIMZ_HOME="$LIVE_ROOT/home" XDG_RUNTIME_DIR="$LIVE_ROOT/r"
export RIMZ_BIN="$LIVE_ROOT/bin/rimz" PATH="$LIVE_ROOT/bin:$PATH"
export RIMZ_ISOLATION="${1:-scoped}"
unset CODEX_HOME RIMZ_SANDBOX_HOST_PATHS
unset RIMZ_AGENT_ID RIMZ_AGENT_KIND RIMZ_AGENT_NAME RIMZ_AGENT_PROFILE
unset RIMZ_AGENT_MODEL RIMZ_AGENT_EFFORT RIMZ_CHANNEL RIMZ_RUN_ID RIMZ_LOOP_TASK
unset RIMZ_PROJECT_ROOT RIMZ_WORKTREE_PATH RIMZ_WORKSPACE_ID RIMZ_SHARED
unset ZELLIJ ZELLIJ_SESSION_NAME ZELLIJ_SOCKET_DIR
tmux -S "$LIVE_ROOT/driver.sock" set-window-option remain-on-exit on
cd "$LIVE_ROOT/repo" || exit 1
"$RIMZ_BIN" start --tmux "$LIVE_ROOT/repo" --no-attach
```

```sh
env -u TMUX -u TMUX_PANE tmux -S "$LIVE_ROOT/driver.sock" -f /dev/null \
  new-session -d -s workflow-m4-driver "sh '$LIVE_ROOT/start.sh'"
```

Before launch, use the same isolated environment with the parent agent's room identity removed to run `rimz trust grant --agents claude` from the clone. Grant only that clone path, not the provider home. An implicit Codex folder-trust prompt can still appear; decline it. No Codex account or workflow node is needed.

Capture startup stdout, stderr, and exit status outside the disposable root. From inside a RimZ scoped view, start refuses with `this pane runs inside a RimZ scoped view; start the room from a host shell`. With permission to use host execution, change the isolated config's `agents.isolation` to `"host"` and respawn the driver with `sh '$LIVE_ROOT/start.sh' host`. Both the machine setting and ambient `RIMZ_ISOLATION=host` matter, and `RIMZ_SANDBOX_HOST_PATHS` must be unset too; `rimz start` has no isolation flag. If host birth also refuses, retain the refusal rather than fabricating room state.

Run subsequent commands through a wrapper with the same isolated environment, `RIMZ_ISOLATION=host`, and `RIMZ_SANDBOX_HOST_PATHS` and parent identity variables unset, changing directory to the clone first. Preserve both human and JSON `workflow show` output:

1. Run `workflow run babysit-pr`. Resolve the real step PID from its supervised run record and process tree, then `kill -KILL <pid>`. Do not manually advance this workflow. The room tick must journal `died` and launch a distinct attempt, within about a minute.
2. Let the retry end normally through provider hooks. Its attempt row must have numeric `cost_usd`, and human detail must show cost. The `scope project` line names the clone root, and both `work/notes/` and `work/signs/` exist.
3. Use a separate project-scoped workflow whose start node is a Claude judge using the fixture's `checks/triage-judge.md`. Set `window = "5h.left>=99"` and route both `skip` and `triage` to `@done`. With the account's left capacity below the threshold, `show` reports a merged window wait whose count grows. Rewrite only the isolated provider cache's `entries["claude@default"].limits.windows` 300-minute reading to `used_percentage = 0`, with fresh `observed_at` and `refreshed_at_ms`; preserve other entries. Immediately call `workflow advance <window-run-id>` so this consumer reads the changed cache before a provider refresh replaces it. This manual advance is for the window run only. The wait closes and a real Claude judge launches.

The cache shape and reopen mechanism are exercised by `crates/rimz/tests/integration/workflow/attempts.rs::closed_window_merges_without_visits_then_reopens`. `resume` has no window-threshold option. Do not edit the sealed workflow to manufacture a reopen. Record the cache publication, the consumer's launched attempt, and final journal, not just the rewrite.

Tear down the room with the isolated wrapper's `rimz --tmux reset --yes --no-start <clone>`, then kill the named driver with `tmux -S "$LIVE_ROOT/driver.sock" kill-session -t workflow-m4-driver`. Check child PIDs and any remaining processes belonging to this exact isolated home; stop only those PIDs. Save scripts and non-secret output, then remove `$LIVE_ROOT`. Do not copy provider account files into the evidence.

## Probe a loop judge against a real provider

A [judge](../reference/cli/loop.md#waits-and-checks) asks a real Claude or Codex model, so it needs a provider login too, and a firing verdict then starts a real agent. Two choices keep the run to the judge alone:

- Ask a question whose answer cannot fire the action: one that must answer false under `--on success`, or one that must answer true under `--on fail`.
- Run from a checkout the provider already trusts. A scratch repository is untrusted, and the fire ends `Errored` naming `rimz trust grant --agents <kind>` as the fix.

Fire by hand, so the scheduler never runs the task, then remove it. `--in 1d` gives the task a trigger that stays a day away.

```sh
cargo build -p rimz --bin rimz
BIN="$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)/debug/rimz"
"$BIN" loop add acheck --in 1d --agent haiku --prompt "say ok" \
  --judge haiku --judge-prompt "Does README.md contain the word zebra-quartz-9?" --on success
"$BIN" loop fire acheck
"$BIN" loop logs acheck
"$BIN" loop remove acheck
```

`haiku` stands for any profile of yours that resolves to one Claude or Codex agent. `loop fire` reports ``judge `haiku` declined`` and starts nothing, and `loop logs` holds the judge's verdict, reason, cost, and tokens. For the other polarity, ask a question that is true and pass `--on fail`: the fire reports ``judge `haiku` passed`` and still starts nothing.

## Traps

- A live check of anything the host, a hook, or a loop fire spawns must run in a disposable room built from the worktree. In the real room those children are the installed `rimz`, so the check silently exercises the released binary instead of your change and passes either way. The held room also replaces `HOME`, so no provider login is reachable inside it and a real provider turn cannot be part of such a check.
- Look before capturing or clicking: a settled unwatched sidebar retains its last painted frame until reveal. Data changes alone emit no hidden frame bytes and unpainted views do no reductions. Establish known ownership and let startup or reconciliation finish before using a hidden pane as a negative control; input can reveal it, but hidden resize changes only geometry. Pane-local reconciliation obligations still run.
- Look is `pane focus` on tmux and `zellij action go-to-tab` on Zellij, and the card prints the right one. `rimz pane focus` does not move the Zellij client: the sidebar stays narrow with its clock stopped at `0:00` until a `go-to-tab` widens it and resumes the clock.
- A previously selected card stays held open, with its stats line and its subagent and wait entries, until collapsing it would no longer move the selected card, or until a wheel tick, unread-banner jump, or toggle releases it. A check of an unselected card's shape (a stale card's one line, a hidden subagents and waits line) releases the hold first; otherwise the held card reads as a shape that failed to change.
- On tmux, a sidebar's selection follows focus among its own tab's panes: `pane focus` on a pane in another tab leaves it where it was. To select a card whose pane sits in another tab, or to move the selection off a team, run the sidebar's Click with that card's 0-based row from a fresh capture.
- A pending wait turns an idle or done stub card sleeping (`☾`, `wakes in 31m`), and a sleeping card shows its subagents and waits line at rest. A check of an idle or done card's shape arms no wait; feed the stub an ended turn instead, as in [Age a card into a stale line with an expired cache](#age-a-card-into-a-stale-line-with-an-expired-cache).
- `sidebar click` is testkit-only. It sends an instance-addressed click through the host's room socket, exercising hit testing and focus routing but skipping terminal input and its parsing.
- The pipeline line renders no owner name; the click's focus target is the evidence for ownership.
- Read focus from the mux using the card's Focus command, not `rimz pane list`, which reports no focus on Zellij. Zellij's `is_focused` is per-tab and non-unique ([zellij-reference.md](../externals/mux-adapter/zellij-reference.md#types)): the check reads focus among the team tab's panes, so it proves pane focus inside that tab and not which tab the client is viewing.
- To drive sidebar keys on Zellij, send them through the attached client (`tmux send-keys -t room-client` via `sandbox in`). The held room attaches in normal mode, where Zellij itself takes `Ctrl+n` and `Ctrl+p`: lock the client first with `tmux send-keys -t room-client C-g`. `zellij action switch-mode` and `focus-pane-id` do not change the attached client (its status bar stays `NORMAL` and its keys keep landing in the agent pane), and `Alt+p` sent through the outer tmux client does not move focus. Focus the sidebar with `"$BIN" --zellij sidebar focus --toggle --session-name <session>`, the command the chord runs, and read the client's focused pane from `zellij --session <session> action list-clients`.
- A relative program path passed directly to `sandbox in` resolves from your checkout. A path nested inside `env` or `sh -c` belongs to that command instead; pass the RimZ binary by absolute path in those cases.
- Wrapping `sandbox room` in your own `bwrap` (to keep its files in a scratch directory) needs `--dev-bind /dev /dev`; without it the room exits before starting with `starting sandbox cleanup reaper` / `Permission denied (os error 13)`.
- To live-check a signal emitter from an agent's shell tool, keep the emitting call alive until the run is recorded: `rz events emit <signal>; sleep 2; rz loop show <task> --json` in one call, with `rz` from [Run a command as an agent](#run-a-command-as-an-agent). An emit whose call ends at once can record no run at all. The cause is not established (possibly the shell tool cleans up the emitter's detached child when the call returns). Judge delivery by the run record or `rimz message show <id>`, never by the emitter's `fired` count, which reads the same whether the run was recorded or not.
- A held room starts without provider hooks: run `rimz hooks install <kind>` through `sandbox in` before launching any agent kind other than the room's stubs. A supervised loop check (`loop add <name> --in 1m --agent worker …`) needs `rimz hooks install claude` before the add, since `loop add` refuses an agent task whose provider hooks are missing, and the retry costs time on the room's `--for` timer.
- For a scoped agent in a held room, launch a new one through `sandbox in`: `rimz agents launch claude --isolation scoped -w probe`. The room launches its team with `--isolation host`, and `rimz agents restart` keeps a recorded `--isolation` over the machine `agents.isolation`, so restarting a team agent after you change the config leaves it on host. Name the lane with `-w`: `--channel probe` is refused, because a worktree backs that channel.
- The held stats view needs a terminal: a plain `rimz stats --refresh --hold` under `cargo xtask sandbox` exits `No such device or address (os error 6)`. Give it one with `script -qfc '<bin> stats --refresh --hold' <file>` inside the sandbox, with `<bin>` the RimZ binary by absolute path. To measure the spending service it elects, sample the owner's threads with `ls /proc/<pid>/task | wc -l`.
- Stop the room by letting `--for` expire or killing the `xtask sandbox room` PID itself. Ctrl-C or a signal to its whole process group also cleans the room, because the reaper runs outside that group. Killing a wrapper shell leaves the room running. Kill by PID from `pgrep -x xtask`, not with `pkill -f <pattern>` or a loop over `pgrep -f <pattern>`: the pattern matches the invoking shell's own command line, so either kills the shell that ran it.

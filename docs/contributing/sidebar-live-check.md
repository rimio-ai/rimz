# Sidebar live check

A unit test proves what the renderer decides, and a scenario PNG proves how a frame draws ([sidebar-screenshots.md](./sidebar-screenshots.md)). Only a live room proves that the frame reaches a real pane in time, on a real multiplexer. Run [the check sequence](#the-check-sequence) on both backends before hand-off for any change to the renderer, the pipeline line, the shared data plane, or click routing. Run each later section too when your change touches its area:

| Your change touches | Also run |
| --- | --- |
| The sidebar host, its supervisors, reload, or notifications | [Room host](#room-host) |
| `rimz sidebar repair` or reconcile | [Sidebar repair](#sidebar-repair) |
| A new tab's first paint or the seed | [First frame of a new tab](#first-frame-of-a-new-tab) |
| Pixel meters, budget bars, logos, or pets | [Pixel graphics](#pixel-graphics) |

Each check runs in a held [sandbox room](./sandbox-rooms.md). Hold one room per backend, and take every command from that room's room card. Later sections use the `ROOT`, `BIN`, `MUX`, `SESSION`, `STATE`, `rz`, and `a` names that [Run a command in the room](./sandbox-rooms.md#run-a-command-in-the-room) defines.

In this page a card is an agent's entry in the sidebar. The pipeline line is the board row under a team's worktree group. It carries the team, one dot per stage, the current stage and its time, and the run's total time at the right.

## Before you start

Run a sidebar's Look before its Capture or Click. A sidebar on a tab nobody views can hold an old frame, and a capture taken a few tens of milliseconds after Look can still show it. Repeat the Capture until the frame changes.

Arriving on a tab starts the tab-read dwell, 2.5 seconds (`crates/rimz/src/sidebar/timing.rs::TAB_READ_DWELL`). Once the client stays that long, every unread card whose agent's pane is on that tab turns read. So to capture an unread card, capture it through a sidebar on another tab.

A check whose subject is time waits in a detached script (`sleep <secs>`, then the captures). An agent harness refuses a long foreground sleep, and any tool call made during the wait counts as activity on the clock you are waiting on.

## The check sequence

Run all eight on each backend. The backends differ only where a check says so. The first four need only the room as it starts.

1. **Clocks advance.** Look at one sidebar, then Capture it twice across a clock-unit boundary: a second apart below a minute, a minute apart below an hour. Both clocks on the pipeline line must advance. Repeat on a second sidebar.
2. **A stage flip reaches every tab.** Note the time beside `Build` on the pipeline line. Keep the `#probe` sidebar watched and run the room card's Flip. `Review` must replace `Build` within 5 seconds. Then Look and Capture every other sidebar: each must show `Review` within 5 seconds of its Look. The worktree group then reads:

   ```text
   ▎⑂ probe ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ≡ main🮇
   ▎  forge ● ◉ Review 2s ○                      22m🮇
   ```

3. **A returned stage resumes its clock.** Run the Flip with `Build` in place of `Review`. The stage time must resume from the Build time you noted, never from `0s`. Capture with `--ansi`: the dot for the Review stage, second on the line, must be hollow and in the warm `warn` tone (`38;2;224;175;104` in the default theme). Flip back to `Review` afterwards.
4. **A pipeline click focuses the owner.** On the team's tab, Look and take a fresh Capture. Set `ROOM_ROW` to the pipeline line's row, counted from zero, from that capture only, since selection can shift rows. Run Focus, then Click, then Focus again. Focus must move to the reviewer pane from the room card's `Role: @reviewer#probe` line. On Zellij, the reviewer pane joins the focused set in the `list-panes` output.
5. **Client views.** Run this check when the change touches what a client sees. Attach two terminal clients to the room's session (`target/debug/xtask sandbox in "$ROOT" -- tmux -S "$ROOT/runtime/rimz/tmux/server" attach -t "$SESSION"` on tmux, `"$BIN" --zellij attach "$SESSION"` through `sandbox in` on Zellij) and read each client's own screen, not only pane captures. On tmux, both clients view the same sidebar: scrolling from either moves both, and after one detaches the other keeps its scroll and selection. On Zellij, clients on different tabs scroll independently, a client that moves onto the other's tab joins the view there, and it keeps that view when it moves away. The integration tests `two_clients_share_the_sidebar_view_through_detach` (tmux) and `two_clients_split_join_and_carry_the_sidebar_view` (Zellij) hold the same rules.
6. **Wheel glide.** Give the room more cards than the sidebar has rows: launch extra stub agents with `rz agents launch claude --isolation host -w extra`, or shorten the terminal. Focus the sidebar and wheel down, then up. The cards must glide over about a tenth of a second, and the scrollbar must follow the visible cards. A second wheel during a glide continues from the visible row. A selection key stops the glide within one frame. A click mid-glide opens the card under the pointer.
7. **A cross-tab click lands on the same row.** Launch two stub agents with `-w extra` as in check 6, which opens an `#extra` tab, so two tabs each hold two agents. In tab 1, select card A so it expands, with card B's pane in tab 2. Note B's row in a tab 1 capture, then click B. In tab 2's first frame, B must sit on the same row and A must still be open. In `$STATE/audit/focus.log.jsonl`, the latest `push_handshake` record must show `painted` for both the source outcome and the destination peer, with only the destination sidebar in `peers`. A `displaced` peer verdict means the frame missed the row. Then send a rapid burst of cross-tab clicks from that one sidebar. The last target's tab must end in view, and a target superseded before its jump dispatched must not switch tabs.
8. **A failed focus shows an alert.** Write an executable shim named `tmux` (or `zellij`) in a scratch directory. It forwards every call to the real binary, but while a marker directory exists it exits nonzero for a focus of the reviewer's pane (`select-pane -t <pane>` on tmux, `action focus-pane-id` on Zellij). Hold a room with that directory first on `PATH`, so the host inherits it. Create the marker, click the reviewer's card, and capture. The highlight must return to the focused card, and the bottom line must read `last alert 0s ago: focus failed: <card>: <error>` with `Esc dismiss`.

On Zellij, the held room's attached client does not follow a focus sent from outside a pane, so its active tab does not change after a click. Prove a jump there from the target tab's focused set before and after the click, and from `snapshot_focused_pane` in the renderer's `fold_decided` record in `$STATE/audit/focus.log.jsonl`. tmux shows the client's tab switch in `list-clients`.

## Room host

A room runs one `rimz sidebar host` for the session and one `rimz sidebar serve` supervisor per tab ([state.md](../internals/sidebar/state.md#the-room-host-and-its-attachments)). The host is the only painter. A sandbox room runs its binary from under its root, so filter on `$ROOT` to leave out your real rooms:

```sh
ps -eo pid,ppid,rss,args | grep -E 'rimz sidebar (host|serve)' | grep -F "$ROOT"
```

Expect one `sidebar host` line and one `sidebar serve` line per tab. Run these checks:

1. **Host death.** `kill -9` the host. Each sidebar shows `sidebar: host went away` and `retrying in 1s`, then attaches to one successor host and paints the placeholder until its first fold. An attachment that ends within a minute doubles the next retry delay, up to 60 seconds, so space repeated kills more than 60 seconds apart when you time them.
2. **Supervisor death.** Kill one tab's `sidebar serve`. That pane's instance leaves `rimz sidebar renderers`, and the other tabs keep painting.
3. **Last pane.** Close every sidebar pane. The host exits about ten seconds after the last one detaches.
4. **Reload onto different bytes.** This check rebuilds, so hold a fresh room with `target/debug/xtask sandbox room`, which does not hold Cargo's lock. Build a second binary with `RIMZ_BUILD_VERSION_OVERRIDE` set to any distinct version (`0.4.3+sidebar-reload`), copy it out of the target directory, and run `target/debug/xtask sandbox in "$ROOT" -- <copy> reload`. Expect `✓ 1 room on rimz <new-build>` and `✓ <path> <mux> <session> 3 sidebars`, one successor host, and every tab painting after its Look. A reload onto the build already running restarts no sidebar.
5. **One notification, every sidebar.** Set `notifications.enabled true` in the room, then run `rz sidebar notify-test '@reviewer#probe' --force-bell --no-command`. Expect `Notification test sent: 1` and a new `bell_ring` record per renderer in `$STATE/audit/notify.log.jsonl`. Only the reviewer's tab rings. The daemon tab reports `daemon_view` and the shell tab `pane_not_in_view`.
6. **Degraded room.** Move `$STATE/log/events.log.jsonl` aside and put an unreadable directory at its path. A `chmod 000` on the whole state directory reads as an empty room instead. For at least 40 seconds, every sidebar keeps its last cards under `⚠ Sidebar degraded for …: snapshot failed:` with `Permission denied (os error 13)`, and the host stays the same process. Restore the log: within about two seconds each sidebar paints fresh content under `⚠ last alert …s ago`.

To count what the host reads, trace it from its start: a `ptrace_scope` of 1 refuses `strace -p` on a process that is not your descendant. Tracing slows every process in the room, so take CPU figures from an untraced room.

```sh
strace -f -qq -ttt -e trace=openat,clone,clone3,fork,vfork -s 256 \
  -o "$TMPDIR/room.strace" cargo xtask sandbox room --mux tmux --for 20m
```

## Sidebar repair

`rimz sidebar repair` replaces a sidebar that is still listed but that no fresh heartbeat claims, and adds one to a tab that has none ([in-place repair](../internals/multiplexers.md#in-place-repair)). On Zellij the new sidebar must take the old one's rectangle while every work pane keeps its own, so compare `zellij --session <session> action list-panes -a -j` before and after.

To make a sidebar unresponsive, freeze the host with `kill -STOP`, and keep the other sidebars' heartbeat files under `$ROOT/runtime/rimz/ws/$SESSION/live/heartbeat/` fresh with `touch` for longer than the five-second heartbeat TTL. Start the repair, then thaw the host with `kill -CONT` about 1.5 seconds later: the repair proves a replacement mounted by waiting for its heartbeat, which a frozen host never writes. Expect `Recovered 1 sidebar in place.` and `Closed 1 duplicate or unresponsive sidebar.` with exit 0. A second repair prints `Sidebar structure is healthy across 1 session.` Stopping a `sidebar serve` instead changes nothing, since the host keeps its heartbeat fresh.

To check the add, open a tab from a one-pane layout (`zellij action new-tab --layout-string 'layout { pane; }'`): a plain `new-tab` already carries a sidebar. Zellij places the split by the view's size, so set the client's size first with `stty -F /dev/pts/<n> cols 180 rows 50` on the `zellij attach` client's terminal.

## First frame of a new tab

A new tab's sidebar paints its first frame from the warm host's last fold, so its first non-blank screen already lists the room's cards ([state.md](../internals/sidebar/state.md#the-room-host-and-its-attachments)). No card is open by selection until a frame lists the new sidebar, and the previous tab's pane is never the selection. Poll the new pane's captures from the moment the tab opens, and attribute each with the `fold_decided` and `frame_published` records in `$STATE/audit/focus.log.jsonl` ([focus trace](../internals/diagnostics.md#the-focus-trace)). Every `fold_decided` must have `baseline` and `selected_after` absent or naming a pane in the new tab. A stale seed has shown up about once in 18 tabs on tmux, so open at least 18 tabs before you call the check passed. A failure that the merge-base build also shows is already on the trunk: report it as advisory.

## Inspect the data behind a card

A frame shows what the renderer decided. `rimz sidebar snapshot --json` computes the same view-model in the calling process, and `--no-produce` reuses the host's publications without running the multiplexer or git. One card is a row in a worktree group:

```sh
rz sidebar snapshot --json --no-produce > "$ROOT/tmp/snap.json"
jq '.worktree_groups[].rows[] | select(.handle == "coder")' "$ROOT/tmp/snap.json"
```

The row carries what the card paints: the folded `status`, the `attention_score` that orders it, and the nested `sub_agents[]`, each left out when zero or empty. `.agents[]` is the rollup the fold reads, with each session's own clock and no children, so a check that reads it finds the card's fields missing. A row has no `attention` or `display_status` key, so a filter that names one prints `null`. `agents show --json` reports the agent under an `agent` key.

## Put a card in a chosen state

Feed the stub agents with [`feed-claude-turn.sh`](../../scripts/live-check/feed-claude-turn.sh) ([Act as an agent](./sandbox-rooms.md#act-as-an-agent)), then shorten the clocks you need to watch:

- **Stale line and cache expiry.** A card with no activity for `stale_after_secs` shrinks to one stale line. Run `rz config set theme.display.stale_after_secs 60`, `harness.prompt_cache_margin 5s`, `harness.prompt_cache_ttl.claude 1m`, `harness.cache_keepalive false`, and `harness.idle_compact off`. Feed a finished turn, then `rz agents mark '@coder#probe' read`, since an unread card never shrinks. After a minute the card shrinks to its stale line, unless it is selected or expanded, and a minute after the last request its cache expires. A real agent's observed split cache write overrides these lifetimes, so use a stub.
- **Cache warning band.** Set `harness.prompt_cache_ttl.claude 24m` and feed a finished turn. The row's `cache.ceiling_secs` reads 1440. Select the card with Click. The cache-age pin on the context meter stays muted through 12 minutes quiet, then warms from yellow through orange to red near 24, and the selected card's cache line counts down in that band. After 24 it reads `◉ cache expired` in greyed red.
- **Compaction gap.** Feed `start prompt window compact=6092 stop`. The context meter shows `↻` and the compacted size as an estimate until the first real reading: `↻ ━───…  3.0%` in a 200k window. Set `WINDOW` for another window.
- **Pull request state.** Copy [`scripts/live-check/gh`](../../scripts/live-check/gh), a fake `gh`, into a scratch directory with a `mode` file holding `CONFLICTING`. Hold a new room with that directory first on `PATH` (`env -u NO_COLOR PATH="<dir>:$PATH" cargo xtask sandbox room --mux tmux`), then give the worktree an origin with `target/debug/xtask sandbox in "$ROOT" -- git remote add origin https://github.com/rimz-live-check/probe.git`. Run `rz pane focus` on the coder's pane from the room card's Role line and keep it focused: a viewed pane puts the repository on the 20-second refresh, while the sidebar alone leaves it on the 5-minute one. The `#probe` header ends in `↯ main` in the `caution` tone. Write `MERGEABLE` to `mode` and the next probe returns it to `⑃ main`. Every call the host makes lands in `gh.calls` beside the shim.

Three things hold a card in a shape you did not ask for. A previously selected card stays open until collapsing it would no longer move the selection, or until a wheel tick, unread-banner jump, or toggle releases it. A pending wait turns an idle card sleeping (`☾`), and a sleeping card shows its subagents and waits line. A booked launch keeps its `▷ booked (N)` line on every card shape until you `rz loop remove <task>`.

## Pixel graphics

A held room's own client reports `tmux-256color`, so its sidebars draw cell bars and text emblems, never pixel meters, budget bars, logos, or pets. Pixel checks are tmux-only, since Zellij draws only the cell tier. [`scripts/live-check/kitty-capture.sh`](../../scripts/live-check/kitty-capture.sh) swaps that client for a recorded one that reports `xterm-kitty`, then tallies what the sidebars sent. The swap leaves the room with no client, so run it last or hold a fresh room afterwards:

```console
$ scripts/live-check/kitty-capture.sh "$ROOT" 20
== graphics commands
    512 _Ga=d,d=I,i=N,q=2
      1 _Gm=0
      1 _Ga=t,f=100,i=N,q=2,m=1
      1 _Ga=t,f=100,i=N,q=2
      1 _Ga=p,U=1,i=N,p=1,c=9,r=1,q=2
      1 _Ga=p,U=1,i=N,p=1,c=12,r=1,q=2
== transmits per image id
      1 i=5374208
      1 i=5374210
```

The `d=I` lines are the sweeps a sidebar makes over its slot's id window each time it takes the slot. A large image is sent in chunks (`m=1`, then `m=0`), and each placement carries `p=1`. An unchanged image is sent once while its sidebar keeps the slot. A second kitty-capable client attached partway resends every held slot once ([sending images](../internals/sidebar/pets.md#sending-images)). After a tab switch, the arriving sidebar's first frame carries no graphics: the pixel tier follows in the next frame ([image ids and slots](../internals/sidebar/pets.md#image-ids-and-slots)). To see pets, set `enabled = true`, `glyphs = "pixel"`, and `pet` to the path of any local 1536x1872 PNG sheet under `[theme.pets]` in `$ROOT/home/.rimz/theme.toml`.

The tally shows what RimZ sent, not how a terminal parsed it. For a fix that depends on ordering, replay the capture through Ghostty's own parser with [`scripts/live-check/ghostty-replay/run.sh`](../../scripts/live-check/ghostty-replay/run.sh) `"$ROOT/tmp/kitty.raw"`. It builds Ghostty v1.3.1 once (a few minutes), records the result of each kitty command Ghostty parsed, and prints a summary such as `SUMMARY commands=517 errors=0 images=2` beside the capture's own count of graphics sequences. A clean replay reads `errors=0` with `commands` equal to that count. To compare with the base branch, take a second capture in a room held with `RIMZ_BIN` naming a base-branch build. The replay covers Ghostty's parser and image storage, not its rendering, and newer Ghostty can differ.

## Traps

- `rimz pane focus` does not move the Zellij client: the sidebar stays narrow, with its clock stopped, until a `go-to-tab` widens it. Use the room card's Look.
- On tmux, a sidebar's selection follows focus only among its own tab's panes. To select a card whose pane is on another tab, use the sidebar's Click with that card's row.
- The pipeline line names no owner. The click's focus target is the evidence of ownership.
- To send sidebar keys on Zellij, type through the attached client: `tmux send-keys -t room-client` via `sandbox in`. The client starts in locked mode, so `Ctrl+n` and `Ctrl+p` reach the pane; a `C-g` would move it to Zellij's normal mode, which takes them. Focus the sidebar with `"$BIN" --zellij sidebar focus --toggle --session-name <session>`.
- To press room keymap chords such as `Alt+1` or `Alt+;`, hold the room in RimZ style, since a held room runs native style and leaves them unbound. Set `RIMZ_BIN` to a wrapper that runs `config set room.style rimz` before it execs the real binary on `--tmux start` or `--zellij start` ([Set up the room for a check](./sandbox-rooms.md#set-up-the-room-for-a-check)). Build the real binary with `--features testkit`, or the room's preflight refuses it. Send each chord as bytes through the attached client via `sandbox in`: `tmux send-keys -t room-client -H 1b 3b` presses `Alt+;`.
- A sidebar nobody views mirrors layout changes from the most recently painted view, without changing its own view. To prove a hidden pane got a mirror, capture it without revealing it. A change that only edits a number on a line of unchanged width sends no hidden frame bytes, so use one to confirm the capture tells the two cases apart.

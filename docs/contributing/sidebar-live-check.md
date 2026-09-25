# Sidebar live check

Use this before hand-off for a renderer, pipeline line, consumer path, or click-routing change where a unit test cannot show the live frame.

## Hold a room and join it

Build the testkit binary first:

```sh
cargo build -p rimz --bin rimz --features testkit
```

`cargo xtask sandbox room --mux <tmux|zellij> [--for <duration>]` holds a disposable room and prints its room card; the room verb is Linux-only. Keep it running while you check the room. The room dies after 30 minutes unless `--for` says otherwise (`--for 45m`, or `--for off` to hold it until you stop it).

`cargo xtask sandbox in <root> -- <command>` runs one command inside that held room. Every command on the card spells that verb `target/debug/xtask` instead: the held room's own `cargo xtask` owns the target-directory lock for as long as it runs, so a joined `cargo` command waits for it rather than doing anything. Use the ready-to-paste commands from your own card; the cards below record real runs, so their roots, pane IDs, and the long absolute `rimz` path — one machine's resolved `target/debug/rimz` — are not reusable.

## Room cards

The root identifies the held sandbox; the mux and session identify its private multiplexer, and Worktree names the team's checkout. Each Sidebar block names its pane, tab, and producer or consumer role, followed by Look, Capture, and Click commands. A tab can carry more than one sidebar pane, so read the role labels rather than counting blocks. The Role lines map agent roles to panes. Stage and Owner give the initial board state; Flip changes the stage, and Focus reads focus from the multiplexer.

### tmux

```text
Sandbox root: /tmp/rimz-sandbox-NBM9ln
Mux: tmux  Session: rimz-room-130105
Worktree: /tmp/rimz-sandbox-NBM9ln/home/room-worktrees/probe
Sidebar: tmux:%3  Tab: rimzd  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%3'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --tmux pane capture 'tmux:%3'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --tmux sidebar click 'tmux:%3' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: tmux:%0  Tab: zsh  producer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%0'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --tmux pane capture 'tmux:%0'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --tmux sidebar click 'tmux:%0' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: tmux:%9  Tab: #probe  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%9'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --tmux pane capture 'tmux:%9'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --tmux sidebar click 'tmux:%9' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: tmux:%7  Tab: #probe  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%7'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --tmux pane capture 'tmux:%7'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --tmux sidebar click 'tmux:%7' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Role: @coder#probe  tmux:%6
Role: @reviewer#probe  tmux:%8
Stage: Build  Owner: coder
Flip: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --tmux teams flip Review 'live check' --team forge
Focus: target/debug/xtask sandbox in '/tmp/rimz-sandbox-NBM9ln' -- tmux -S '/tmp/rimz-sandbox-NBM9ln/runtime/rimz/tmux/server' display -p '#{pane_id}'
Run a sidebar's Look first, then capture or click it: an unwatched sidebar can hold a stale frame.
```

### Zellij

```text
Sandbox root: /tmp/rimz-sandbox-JISAUo
Mux: zellij  Session: rimz-room-ca2bf6
Worktree: /tmp/rimz-sandbox-JISAUo/home/room-worktrees/probe
Sidebar: zellij:terminal_0  Tab: rimzd  producer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- 'zellij' '--session' 'rimz-room-ca2bf6' 'action' 'go-to-tab' '1'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --zellij pane capture 'zellij:terminal_0'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --zellij sidebar click 'zellij:terminal_0' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: zellij:terminal_4  Tab: zsh  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- 'zellij' '--session' 'rimz-room-ca2bf6' 'action' 'go-to-tab' '2'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --zellij pane capture 'zellij:terminal_4'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --zellij sidebar click 'zellij:terminal_4' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: zellij:terminal_6  Tab: #probe  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- 'zellij' '--session' 'rimz-room-ca2bf6' 'action' 'go-to-tab' '3'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --zellij pane capture 'zellij:terminal_6'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --zellij sidebar click 'zellij:terminal_6' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Role: @coder#probe  zellij:terminal_7
Role: @reviewer#probe  zellij:terminal_8
Stage: Build  Owner: coder
Flip: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- '/mnt/data/build/marvin/cargo/rimz/sidebar-sandbox-14bd145736ae92747c4a/debug/rimz' --zellij teams flip Review 'live check' --team forge
Focus: target/debug/xtask sandbox in '/tmp/rimz-sandbox-JISAUo' -- zellij --session 'rimz-room-ca2bf6' action list-panes -a -j
Run a sidebar's Look first, then capture or click it: an unwatched sidebar can hold a stale frame.
```

## Four checks

Run the sequence on both backends, using each room's own card.

### 1. Producer clock

Run Look for the sidebar labelled `producer`, then its Capture twice across a displayed clock-unit boundary: a second apart below a minute, a minute apart below an hour. The compact pipeline clock should advance. The recorded tmux producer advanced `24s -> 26s`, and the Zellij producer `15s -> 17s`.

### 2. Consumer clock and flip adoption

**2a.** Run Look for a sidebar labelled `consumer`, then its Capture twice across a displayed clock-unit boundary. Its pipeline clock should also advance: the recorded tmux consumer showed `40s -> 42s`, and the Zellij consumer `30s -> 33s`. Allow a beat after Look before the first capture, or a sidebar that was cold will read its pre-Look frame.

**2b.** Keep that consumer watched. Run the card's Flip, then repeat that consumer's Capture and measure from the flip to the first frame showing `Review`. It must replace `Build` within 5 s. The real watched-consumer measurements were:

```text
   tmux:%3 Build -> Review after 1241 ms
   zellij:terminal_4 Build -> Review after 2849 ms
```

This is the first `Review` frame read from the watched tmux consumer `tmux:%3`:

```text
 ⌘ room                                    ~/room

 ◎ 0                              ◇ 0 ↘ 0 ↗ 0 ◌ 0
 ¤ 2                                        $0.00
 ────────────────────────────────────────────────
 ? 0   ! 0   ⏸︎ 0   ✓ 0            ⢿ 0   ☾ 0   ○ 2

 ⑂ probe · forge                           ≡ main
   ● ◉  Review                               0:13
 ○ coder
 ○ reviewer

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇
▌○ zsh                                           ▐























 ────────────────────────────────────────────────
           Claude v9.9.9 · Claude Max
  ▐▛███▜▌  ◎ 2  ◇ – ↘ – ↗ – ◌ –               $ –
 ▝▜█████▛▘ 5h  ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱
   ▘▘ ▝▝   7d  ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱

  ── Total: ─────────────────────────────────────
  W: ◎ 0                          ◇ 0 ↘ 0 ↗ 0 ◌ 0
  M: ◎ 0                          ◇ 0 ↘ 0 ↗ 0 ◌ 0
  W: $0.00                               M: $0.00

                                       ? for help
```

### 3. Adoption on each tab

For every Sidebar block, run its Look, then its Capture — repeating the Capture if the first frame is still the pre-Look one. Each tab must show `Review` within 5 s of Look. In the recorded runs the first capture already carried it: tmux at `22 ms`, `24 ms`, `24 ms` and `26 ms` after Look, Zellij at `38 ms` and `28 ms`. The third Zellij pane still showed `Build` at `22 ms` and carried `Review` on the next capture, which is what the repeat is for.

### 4. Pipeline click routes to the owner

On the team's tab, run the sidebar's Look and take a fresh Capture. Count rows from zero to the pipeline line and set `ROOM_ROW` to that row; do not reuse a row from an earlier frame, since selection can shift rows. Run the card's Focus to record the starting focus, then its Click (`sidebar click <pane> 2 <row>`), then Focus again. The target must be the reviewer pane from the card.

The recorded row was `row0=8` on both backends. tmux focus moved from `%3` to `%8`, matching `Role: @reviewer#probe  tmux:%8`. On Zellij, `8=forge.reviewer@#probe` joined the focused set, matching `Role: @reviewer#probe  zellij:terminal_8`.

## Inspect the data behind a card

A frame shows what the renderer decided; `rimz sidebar snapshot --json` shows what it decided it from. Run it from the binary you built, not the installed one: both the default and `--no-produce` fold the rows in the calling process, so the snapshot is always your code's output, and the only way to see another build's behaviour is to run that build. What the flag trades is the pane truth underneath: the default forks `list-panes` and git for a current roster, while `--no-produce` reuses the pane frame and agent projection the producer already published and forks neither, which makes it the cheap read in a room you would rather not disturb.

One rendered card is a row in a worktree group, and its clocks and card fields live there:

```sh
target/debug/rimz sidebar snapshot --json > /tmp/snap.json
jq '.worktree_groups[].rows[] | select(.handle == "coder")' /tmp/snap.json
```

`.agents[]` on the same snapshot is the rollup the fold reads, not what the card paints: it carries each session's own unfolded clock and no nested children, so a check that reads it will report the display fields missing. Row-level projections — the child-activity fold, display status, attention — exist only under `.worktree_groups[].rows[]`.

Two rules for a check whose subject is time:

- A room's own agents are the only real delegating parents available. A sandbox room's agents are synthetic and launch no children, so a scenario that needs a parent waiting on live subagents runs in the room you are working in: launch a child that keeps working, then observe the parent.
- Put the wait in a background script (`sleep <secs>` followed by the capture commands, run detached) rather than a foreground sleep. An agent harness refuses a long foreground sleep, and any tool call you make during the window stamps the very activity clock you are trying to age.

## Traps

- A live check of anything the elder, a hook, or a loop fire spawns must run in a disposable room built from the worktree. In the real room those children are the installed `rimz`, so the check silently exercises the released binary instead of your change and passes either way. The held room also replaces `HOME`, so no provider login is reachable inside it and a real provider turn cannot be part of such a check.
- Look before capturing or clicking: an unwatched sidebar can hold a stale frame, because the renderer suppresses a dirty paint while the attached client is known to be looking elsewhere (`sidebar_pane/app/loop_state.rs` `dirty_paintable`). It is not a reliable negative control, so do not assert it: across runs an unwatched clock froze on tmux and on Zellij, and on one Zellij run it kept ticking.
- Look is `pane focus` on tmux and `zellij action go-to-tab` on Zellij, and the card prints the right one. `rimz pane focus` never moved the Zellij client: the sidebar stayed ~21 columns wide with its clock stopped at `0:00` until one `go-to-tab`, which widened it to 48 columns and resumed the clock.
- `sidebar click` is testkit-only. It uses the renderer's wakeup socket, exercising hit testing and focus routing but skipping terminal input and its parsing.
- The pipeline line renders no owner name; the click's focus target is the evidence for ownership.
- Read focus from the mux using the card's Focus command, not `rimz pane list`, which reports no focus on Zellij. Zellij's `is_focused` is per-tab and non-unique ([zellij-reference.md](../externals/mux-adapter/zellij-reference.md#types)): the check reads focus among the team tab's panes, so it proves pane focus inside that tab and not which tab the client is viewing.
- Stop the room by letting `--for` expire or killing the `xtask sandbox room` PID itself. Killing a wrapper shell leaves the room running. Kill by PID from `pgrep`, not with `pkill -f <pattern>`: the pattern matches the invoking shell's own command line, so `pkill` kills the shell that ran it.

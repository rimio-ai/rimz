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

The root identifies the held sandbox; the mux and session identify its private multiplexer, and Worktree names the team's checkout. Each Sidebar block names its pane, tab, and producer or consumer role, followed by Look, Capture, and Click commands. A tab can carry more than one sidebar pane, so read the role labels rather than counting blocks. The Role lines map agent roles to panes and give each pane's process ID (`pid -` when the multiplexer reported none). Stage and Owner give the initial board state; Flip changes the stage, and Focus reads focus from the multiplexer.

### tmux

```text
Sandbox root: /tmp/rimz-sandbox-BcOEKE
Mux: tmux  Session: room-2f4c
Worktree: /tmp/rimz-sandbox-BcOEKE/home/room-worktrees/probe
Sidebar: tmux:%3  Tab: rimzd  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%3'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --tmux pane capture 'tmux:%3'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --tmux sidebar click 'tmux:%3' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: tmux:%0  Tab: zsh  producer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%0'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --tmux pane capture 'tmux:%0'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --tmux sidebar click 'tmux:%0' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: tmux:%9  Tab: #probe  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%9'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --tmux pane capture 'tmux:%9'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --tmux sidebar click 'tmux:%9' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: tmux:%7  Tab: #probe  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%7'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --tmux pane capture 'tmux:%7'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --tmux sidebar click 'tmux:%7' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Role: @coder#probe  tmux:%6  pid 1930429
Role: @reviewer#probe  tmux:%8  pid 1930467
Stage: Build  Owner: coder
Flip: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --tmux teams flip Review 'live check' --team forge
Focus: target/debug/xtask sandbox in '/tmp/rimz-sandbox-BcOEKE' -- tmux -S '/tmp/rimz-sandbox-BcOEKE/runtime/rimz/tmux/server' display -p '#{pane_id}'
Run a sidebar's Look first, then capture or click it: an unwatched sidebar can hold a stale frame.
```

### Zellij

```text
Sandbox root: /tmp/rimz-sandbox-CmV7r2
Mux: zellij  Session: room-8597
Worktree: /tmp/rimz-sandbox-CmV7r2/home/room-worktrees/probe
Sidebar: zellij:terminal_0  Tab: rimzd  producer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- 'zellij' '--session' 'room-8597' 'action' 'go-to-tab' '1'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --zellij pane capture 'zellij:terminal_0'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --zellij sidebar click 'zellij:terminal_0' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: zellij:terminal_4  Tab: zsh  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- 'zellij' '--session' 'room-8597' 'action' 'go-to-tab' '2'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --zellij pane capture 'zellij:terminal_4'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --zellij sidebar click 'zellij:terminal_4' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: zellij:terminal_6  Tab: #probe  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- 'zellij' '--session' 'room-8597' 'action' 'go-to-tab' '3'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --zellij pane capture 'zellij:terminal_6'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --zellij sidebar click 'zellij:terminal_6' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Role: @coder#probe  zellij:terminal_7  pid 1948379
Role: @reviewer#probe  zellij:terminal_8  pid 1948382
Stage: Build  Owner: coder
Flip: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- '/mnt/data/build/marvin/cargo/rimz/room-card-pid-7885e5e36022eada6b14/debug/rimz' --zellij teams flip Review 'live check' --team forge
Focus: target/debug/xtask sandbox in '/tmp/rimz-sandbox-CmV7r2' -- zellij --session 'room-8597' action list-panes -a -j
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

**2c.** Keep the consumer watched and record the earlier Build duration from the board ledger. Run the card's Flip with `Build` in place of `Review`, then repeat Capture until the first Build frame. Record the adoption latency and stage / total clock. The Review dot must now be hollow and warm (`warn`), and the stage clock must resume Build's earlier duration plus the time since the return, not restart at zero. Add `--ansi` to Capture to retain the dot's color; a plain capture cannot prove tone. Flip back to `Review` before continuing checks 3 and 4.

The 2026-09-25 flip-back check used `env -u NO_COLOR cargo xtask sandbox room --mux <backend> --for 15m` with the testkit build. Both ANSI captures retained a hollow Review dot with foreground `38;2;224;175;104` (the warm tone). Earlier Build time below is the ledger interval, also confirmed by the published `stage_prior_secs`; the clock and adoption latency were read from the watched consumer, not inferred from publication.

| Backend | Watched consumer | Earlier Build time | Review -> Build adoption | First resumed stage / total |
| --- | --- | --- | --- | --- |
| tmux | `tmux:%3` | 58 s | 1409 ms | `59s / 1m` |
| Zellij | `zellij:terminal_0` | 65 s | 2034 ms | `1m / 1m` |

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

- A room's own agents are the only real delegating parents available. A sandbox room's agents are synthetic and launch no children, so a scenario that needs a parent waiting on live subagents runs in the room you are working in: launch a child that keeps working, then observe the parent. A Claude-native child line whose check needs no live wait replays in the sandbox instead ([Native subagent replay](#native-subagent-replay)).
- Put the wait in a background script (`sleep <secs>` followed by the capture commands, run detached) rather than a foreground sleep. An agent harness refuses a long foreground sleep, and any tool call you make during the window stamps the very activity clock you are trying to age.

## Native subagent replay

The sandbox's `claude` is a stub (`xtask/assets/sandbox-room/claude`), so no real Claude child ever starts in a held room. A change to a Claude-native child line (its tokens, model, status, or clock) is still checkable in one pass: copy a real parent and child transcript into the room, then feed the parent's hooks and the child's `subagentStatusLine` payload by hand, in the order Claude would send them.

**Setup.** Hold a tmux room from the testkit build (see [Hold a room and join it](#hold-a-room-and-join-it)). From its card note the root, the `@coder#probe` Role pane (`%6` in the tmux card above), and a `#probe` consumer sidebar. The hooks also want the parent pane's process as `RIMZ_AGENT_PID`: take `PARENT_PID` from the same Role line (`pid 1930429` in the tmux card above).

**Transcripts.** Pick one Claude session that launched a subagent and copy exactly two files from `~/.claude/projects/<project>/` into the room's `tmp/replay/`, keeping their relative layout: `<session>.jsonl` and `<session>/subagents/agent-<child>.jsonl`. The adapter derives the child directory from the parent transcript's path (`subagents_dir` in `agents/adapters/claude/subagents.rs`), so a flattened copy reads no child. Copy nothing else from `~/.claude`: no credentials, no settings.

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
x() { target/debug/xtask sandbox in "$ROOT" -- env RIMZ_AGENT_PID="$PARENT_PID" TMUX_PANE="$PARENT_PANE" "$PWD/target/debug/rimz" --tmux "$@"; }
x hooks feed --source claude < parent-start.json
x hooks feed --source claude < child-start.json
x statusline feed --source claude --subagent < child-feed.json
# observe the running line here
x hooks feed --source claude < child-stop.json
```

Each feed exits 0 with no output.

**Observe.** Read the child three ways, running and again after the stop:

```sh
target/debug/xtask sandbox in "$ROOT" -- "$PWD/target/debug/rimz" --tmux sidebar snapshot --json > /tmp/snap.json
jq -c --arg c "$CHILD" '.worktree_groups[].rows[].sub_agents[]? | select(.id == $c) | {status, tokens}' /tmp/snap.json
target/debug/xtask sandbox in "$ROOT" -- "$PWD/target/debug/rimz" --tmux agents show '@coder#probe' --json > /tmp/show.json
jq -c '.agent.sub_agents[].tokens' /tmp/show.json
```

`agents show --json` wraps the card in an `agent` envelope, so a bare `.sub_agents` reads null. The frame comes from the consumer's Look then Capture, as in the four checks. The expected window is the newest assistant request in the child transcript, prompt side only:

```sh
jq -s '[.[] | select(.type == "assistant") | .message.usage] | last | .input_tokens + .cache_read_input_tokens + .cache_creation_input_tokens' "$ROOT/tmp/replay/$SESSION/subagents/agent-$CHILD.jsonl"
```

In the recorded run (2026-09-26, tmux) that sum was `2 + 118420 + 2589 = 121011`. The snapshot read `{"status":"running","tokens":{"window":121011}}`, then `{"status":"success","tokens":{"window":121011}}` after the stop; the `tokenCount` of 999999 did not replace it. The consumer's child line, running and stopped:

```text
▌    ⢁ Explore · Replay occupancy check          ▐
▌      ▤ 121k · Opus 5                      ◔ <1m▐

▌    ✓ Explore · Replay occupancy check     ◔ <1m▐
▌      ▤ 121k · Opus 5                           ▐
```

The replay proves the adapter and renderer path from hook to frame. It does not prove that Claude sends these payloads in this order or shape; that contract lives in [claude-reference.md](../externals/agent-adapter/claude-reference.md). It has run on tmux only.

## Traps

- A live check of anything the elder, a hook, or a loop fire spawns must run in a disposable room built from the worktree. In the real room those children are the installed `rimz`, so the check silently exercises the released binary instead of your change and passes either way. The held room also replaces `HOME`, so no provider login is reachable inside it and a real provider turn cannot be part of such a check.
- Look before capturing or clicking: an unwatched sidebar can hold a stale frame, because the renderer suppresses a dirty paint while the attached client is known to be looking elsewhere (`sidebar_pane/app/loop_state.rs` `dirty_paintable`). It is not a reliable negative control, so do not assert it: across runs an unwatched clock froze on tmux and on Zellij, and on one Zellij run it kept ticking.
- Look is `pane focus` on tmux and `zellij action go-to-tab` on Zellij, and the card prints the right one. `rimz pane focus` never moved the Zellij client: the sidebar stayed ~21 columns wide with its clock stopped at `0:00` until one `go-to-tab`, which widened it to 48 columns and resumed the clock.
- `sidebar click` is testkit-only. It uses the renderer's wakeup socket, exercising hit testing and focus routing but skipping terminal input and its parsing.
- The pipeline line renders no owner name; the click's focus target is the evidence for ownership.
- Read focus from the mux using the card's Focus command, not `rimz pane list`, which reports no focus on Zellij. Zellij's `is_focused` is per-tab and non-unique ([zellij-reference.md](../externals/mux-adapter/zellij-reference.md#types)): the check reads focus among the team tab's panes, so it proves pane focus inside that tab and not which tab the client is viewing.
- A command inside `sandbox in` does not resolve a relative binary path against your worktree: `target/debug/rimz` fails with `env: 'target/debug/rimz': No such file or directory` and exit 127. Pass the binary by absolute path.
- Wrapping `sandbox room` in your own `bwrap` (to keep its files in a scratch directory) needs `--dev-bind /dev /dev`; without it the room exits before starting with `starting sandbox cleanup reaper` / `Permission denied (os error 13)`.
- Stop the room by letting `--for` expire or killing the `xtask sandbox room` PID itself. Killing a wrapper shell leaves the room running. Kill by PID from `pgrep`, not with `pkill -f <pattern>`: the pattern matches the invoking shell's own command line, so `pkill` kills the shell that ran it.

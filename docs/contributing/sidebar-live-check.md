# Sidebar live check

Use this before hand-off for a renderer, pipeline line, consumer path, or click-routing change where a unit test cannot show the live frame. For a pixel transport change, [Capture kitty graphics](#capture-kitty-graphics) records what the sidebar sends a kitty-capable client.

## Hold a room and join it

Build the testkit binary first; without it `sandbox room` refuses with `development rimz missing`:

```sh
cargo build -p rimz --bin rimz --features testkit
```

`cargo xtask sandbox room --mux <tmux|zellij> [--for <duration>]` holds a disposable room and prints its room card; the room verb is Linux-only. Keep it running while you check the room. The room dies after 30 minutes unless `--for` says otherwise (`--for 45m`, or `--for off` to hold it until you stop it).

`cargo xtask sandbox in <root> [--cwd <dir>] [--as <@handle> | --as-ancestor <@handle>] [--] <command>` runs one command inside that held room. `--as` uses the agent's launch environment; `--as-ancestor` runs beneath its stub with no agent or pane keys, exercising caller detection by ancestry. Both need Linux. The cwd and identity flags may come in either order before the command. Use a qualified handle (`@coder#probe`) if the short form (`@coder`) is ambiguous.

Every command on the card spells that verb `target/debug/xtask` instead: the held room's own `cargo xtask` owns the target-directory lock for as long as it runs, so a joined `cargo` command waits for it rather than doing anything. Use the ready-to-paste commands from your own card; the cards below record real runs, so their roots, pane IDs, and the long absolute `rimz` path (one machine's resolved `target/debug/rimz`) are not reusable.

A check that needs two rooms, such as a pane id addressed across rooms, starts the second inside the same sandbox. With `$BIN` the card's `rimz` path and `<mux>` the room's backend:

```sh
mkdir "$ROOT/home/probe2"
target/debug/xtask sandbox in "$ROOT" --cwd "$ROOT/home/probe2" -- git init -q .
target/debug/xtask sandbox in "$ROOT" --cwd "$ROOT/home/probe2" -- "$BIN" --<mux> start "$ROOT/home/probe2" --no-attach
```

A command run with `--cwd "$ROOT/home/probe2"` resolves the second room. Plain `sandbox in` scrubs the Zellij and tmux session keys, so a raw Zellij action from outside a pane picks no session once two are live; name one with `env ZELLIJ_SESSION_NAME=<session>` ahead of the command.

## Room cards

The root identifies the held sandbox; the mux and session identify its private multiplexer, and Worktree names the team's checkout. Each Sidebar block names its pane, tab, and producer or consumer role, followed by Look, Capture, and Click commands. A tab can carry more than one sidebar pane, so read the role labels rather than counting blocks. The Role lines map agent roles to panes and give each pane's process ID (`pid -` when the multiplexer reported none). Each As line is a command prefix for that agent: append a RimZ command, or swap `--as` for `--as-ancestor` to exercise ancestry. Stage and Owner give the initial board state; Flip changes the stage, and Focus reads focus from the multiplexer.

### tmux

```text
Sandbox root: /tmp/rimz-sandbox-FYlgGi
Mux: tmux  Session: room-fdab
Worktree: /tmp/rimz-sandbox-FYlgGi/home/room-worktrees/probe
Sidebar: tmux:%3  Tab: rimzd  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%3'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --tmux pane capture 'tmux:%3'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --tmux sidebar click 'tmux:%3' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: tmux:%0  Tab: zsh  producer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%0'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --tmux pane capture 'tmux:%0'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --tmux sidebar click 'tmux:%0' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: tmux:%9  Tab: #probe  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%9'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --tmux pane capture 'tmux:%9'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --tmux sidebar click 'tmux:%9' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Role: @coder#probe  tmux:%6  pid 2593482
  As: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' --as '@coder#probe' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --tmux
Role: @reviewer#probe  tmux:%8  pid 2593536
  As: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' --as '@reviewer#probe' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --tmux
Stage: Build  Owner: coder
Flip: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --tmux teams flip Review 'live check' --team forge
Focus: target/debug/xtask sandbox in '/tmp/rimz-sandbox-FYlgGi' -- tmux -S '/tmp/rimz-sandbox-FYlgGi/runtime/rimz/tmux/server' display -p '#{pane_id}'
Run a sidebar's Look first, then capture or click it: an unwatched sidebar can hold a stale frame.
```

### Zellij

```text
Sandbox root: /tmp/rimz-sandbox-wsZYXO
Mux: zellij  Session: room-1828
Worktree: /tmp/rimz-sandbox-wsZYXO/home/room-worktrees/probe
Sidebar: zellij:terminal_0  Tab: rimzd  producer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- 'zellij' '--session' 'room-1828' 'action' 'go-to-tab' '1'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --zellij pane capture 'zellij:terminal_0'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --zellij sidebar click 'zellij:terminal_0' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: zellij:terminal_4  Tab: zsh  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- 'zellij' '--session' 'room-1828' 'action' 'go-to-tab' '2'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --zellij pane capture 'zellij:terminal_4'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --zellij sidebar click 'zellij:terminal_4' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Sidebar: zellij:terminal_6  Tab: #probe  consumer
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- 'zellij' '--session' 'room-1828' 'action' 'go-to-tab' '3'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --zellij pane capture 'zellij:terminal_6'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --zellij sidebar click 'zellij:terminal_6' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
Role: @coder#probe  zellij:terminal_7  pid 2600984
  As: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' --as '@coder#probe' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --zellij
Role: @reviewer#probe  zellij:terminal_8  pid 2600987
  As: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' --as '@reviewer#probe' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --zellij
Stage: Build  Owner: coder
Flip: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- '/mnt/data/build/marvin/cargo/rimz/15280db74799f67c8255/debug/rimz' --zellij teams flip Review 'live check' --team forge
Focus: target/debug/xtask sandbox in '/tmp/rimz-sandbox-wsZYXO' -- zellij --session 'room-1828' action list-panes -a -j
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

## Room host

A room with a terminal on its sidebar panes runs one `rimz sidebar host` for the session and one `rimz sidebar serve` supervisor per tab, with no worker beside them ([state.md](../internals/sidebar/state.md#the-room-host-and-its-attachments)). Take the process shape from inside the room, where the session name is on your card:

```sh
ps -eo pid,ppid,rss,args | grep -E 'rimz sidebar (host|serve)' | grep -v grep
```

Expect one `sidebar host` line and one `sidebar serve` line per tab. The host's parent is the `sidebar serve` supervisor that started it, and once that supervisor exits the host is reparented; it runs in its own session either way, so no pane's job control reaches it. A `sidebar serve` pair on one tab, a parent and a child with the same arguments, is a pane on its fallback worker; that is correct right after a host death and wrong in a settled room.

Three checks cover what a unit test cannot:

1. **Host death.** Kill the host by PID with `kill -9`. Every pane keeps its last frame, then repaints: each supervisor reads the end of its stream and attaches to a host one of them starts, or runs its own worker when its attachment was younger than the stable-run window. Look on each tab, then repeat the `ps` and confirm one host again with no worker: a supervisor on a worker probes for a host once the stable-run window has passed, about a minute, then stops the worker and attaches its pane to that host.
2. **Supervisor death.** Kill one tab's `sidebar serve` by PID. The host drops that pane: its heartbeat and wakeup socket leave the room's runtime directory (the testkit build's `rimz sidebar renderers` no longer lists the instance), and the other tabs keep painting.
3. **Last pane.** Close every tab's sidebar pane, or kill the session. The host exits about ten seconds after its last pane detaches; `ps` shows no `sidebar host` for the session after that.

A reload is the fourth: after `rimz reload` onto a new build the host's PID changes and every tab repaints once, with no tab left on a worker.

## Inspect the data behind a card

A frame shows what the renderer decided; `rimz sidebar snapshot --json` shows what it decided it from. Run it from the binary you built, not the installed one: both the default and `--no-produce` fold the rows in the calling process, so the snapshot is always your code's output, and the only way to see another build's behaviour is to run that build. What the flag trades is the pane truth underneath: the default forks `list-panes` and git for a current roster, while `--no-produce` reuses the pane frame and agent projection the producer already published and forks neither, which makes it the cheap read in a room you would rather not disturb.

One rendered card is a row in a worktree group, and its clocks and card fields live there:

```sh
target/debug/rimz sidebar snapshot --json > /tmp/snap.json
jq '.worktree_groups[].rows[] | select(.handle == "coder")' /tmp/snap.json
```

`.agents[]` on the same snapshot is the rollup the fold reads, not what the card paints: it carries each session's own unfolded clock and no nested children, so a check that reads it will report the display fields missing. Row-level projections — the child-activity fold, display status, attention — exist only under `.worktree_groups[].rows[]`.

Two rules for a check whose subject is time:

- A room's own agents are the only real delegating parents available. A sandbox room's agents are synthetic and do not delegate work to provider subagents, so a scenario that needs a parent waiting on live subagents runs in the room you are working in: launch a child that keeps working, then observe the parent. A Claude-native child line whose check needs no live wait replays in the sandbox instead ([Native subagent replay](#native-subagent-replay)).
- Put the wait in a background script (`sleep <secs>` followed by the capture commands, run detached) rather than a foreground sleep. An agent harness refuses a long foreground sleep, and any tool call you make during the window stamps the very activity clock you are trying to age.

## Native subagent replay

The sandbox's `claude` is a stub (`xtask/assets/sandbox-room/claude`), so no real Claude child ever starts in a held room. A change to a Claude-native child line (its tokens, model, status, or clock) is still checkable in one pass: copy a real parent and child transcript into the room, then feed the parent's hooks and the child's `subagentStatusLine` payload by hand, in the order Claude would send them.

**Setup.** Hold a tmux room from the testkit build (see [Hold a room and join it](#hold-a-room-and-join-it)). From its card note the root, the `@coder#probe` As prefix, and a `#probe` consumer sidebar. `--as` supplies the agent's session keys and hook owner pid; no process-environment copy is needed.

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
x() { target/debug/xtask sandbox in "$ROOT" --as '@coder#probe' -- "$PWD/target/debug/rimz" --tmux "$@"; }
x hooks feed --source claude < parent-start.json
x hooks feed --source claude < child-start.json
x statusline feed --source claude --subagent < child-feed.json
# observe the running line here
x hooks feed --source claude < child-stop.json
```

Each feed exits 0 with no output.

A payload held in a zsh variable goes to the feed through `printf '%s\n' "$payload" |`, not `echo`: zsh's `echo` expands the `\n` escapes inside JSON strings and the feed reads corrupt JSON.

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

`a` runs a RimZ command as the agent and `rz` runs one as the plain user shell (`rz agents`). Keep each prefix in a function, not a variable: zsh does not split an unquoted variable into words, so `$PREFIX agents` fails there as one unknown command.

`--as-ancestor` starts the command below the stub, with exactly plain `in`'s environment: no `RIMZ_AGENT_*` or pane key. It relays stdin, stdout, and stderr through pipes, not a tty, and reports a failed command's exit status. An absent or dead serving seat refuses with the instruction to hold a fresh room with this build. Both modes default to the card's Worktree; `--cwd` overrides it. A relative program path containing `/` resolves from your checkout, as it does for plain `in`.

Session-death recovery needs the same feed for every agent. The stub agents hold provisional session ids, which the producer leaves out of `records/live-roster.json`, so an unprimed room publishes an empty roster while its agents are alive and the next `rimz start` has nothing to recover. Feed one SessionStart per agent and wait for the roster to name them before ending the session.

The feed exits 0 with no output. A SessionStart alone leaves the card fresh, and a fresh card has no subagents or waits line, so a wait armed now stays off the frame. To check delegation lines, also feed `UserPromptSubmit` (with a `prompt` field) and `Stop` for the same session before arming. The card then reads sleeping, with `⧖ waits (n)` and its entries. Both backends resolved the caller to `@coder#probe`. These arming-line excerpts show `--as` followed by `--as-ancestor`, first on tmux:

```console
armed wait-still-marker: check: true → @coder#probe
armed wait-simple-lane: check: true → @coder#probe
```

And on Zellij:

```console
armed wait-fair-nova: check: true → @coder#probe
armed wait-kind-brook: check: true → @coder#probe
```

This check proves caller resolution, not wait completion. To inspect hook attribution, use `sidebar snapshot --json` and read the selected agent's `runtime_owner` under `agents`; `agents show --json` does not expose that field. Before the feed, both rooms still showed idle `○ coder` and `○ reviewer` rows, as before the serving stub.

The listing that follows `armed` can read `watcher lost` with AGE `-` right after arming; `wait list` two seconds later reads `watching pid`, so treat it as a listing race, not a failed arm. The feed only needs to run once per room; later `a` calls reuse the registered session. The run proved arming only, not delivery of the wait's result to the agent.

## Capture kitty graphics

A held room draws cell bars, never pixel meters or pixel pets, because no kitty-capable client is looking at it. On tmux the sidebar sends kitty graphics only when every rendering client's termname is a kitty-capable one (`sidebar_pane/pixel/probe.rs` `termname_allowed`: `xterm-kitty`, `kitty`, `xterm-ghostty`, `ghostty`), and the room keeps its own `tmux-256color` client attached. To check which image ids go out, the `p=` on each placement, or the `d=I` deletes, replace that client with a recorded one that reports `xterm-kitty`. This recipe is tmux-only.

Take `ROOT` and the session from your card (`Mux: tmux  Session: room-e5ac`). The machine usually has no `xterm-kitty` terminfo, so alias one from `xterm-256color` under the room's tmp. Then detach the room's rendering client (control-mode `1` lines are RimZ's own link; leave them), and attach a `script`-recorded client for 30 seconds, long enough for the sidebar's 10-second caps refresh to see it:

```sh
S="$ROOT/runtime/rimz/tmux/server" SESSION=room-e5ac
t() { target/debug/xtask sandbox in "$ROOT" -- tmux -S "$S" "$@"; }
mkdir -p "$ROOT/tmp/terminfo"
infocmp -x xterm-256color | sed '1,/^xterm-256color|/s/^xterm-256color|/xterm-kitty|/' | tic -x -o "$ROOT/tmp/terminfo" -
t list-clients -F '#{client_control_mode} #{client_termname} #{client_name}'
t detach-client -t "$(t list-clients -F '#{client_control_mode} #{client_name}' | awk '$1==0{print $2}')"
target/debug/xtask sandbox in "$ROOT" -- sh -c "TERMINFO='$ROOT/tmp/terminfo' TERM=xterm-kitty timeout 30 script -qfec 'tmux -S $S attach -t $SESSION' '$ROOT/tmp/kitty.raw'" < /dev/null > /dev/null
grep -ao $'\e_G[^;\e]*' "$ROOT/tmp/kitty.raw" | sed -E 's/i=[0-9]+/i=N/; s/[xy]=[0-9]+//g' | sort | uniq -c | sort -rn
```

`detach-client -a` detaches every client except the one named, so name the rendering client directly as above. In the recorded run (2026-09-28, tmux 3.7c) the tally read:

```text
   1536 _Ga=d,d=I,i=N,q=2
      2 _Gm=0
      2 _Ga=t,f=100,i=N,q=2,m=1
      2 _Ga=p,U=1,i=N,p=1,c=38,r=1,q=2
```

The `d=I` lines are the sweeps a sidebar makes over its leased id window before its first transmit; the `a=t` transmits are chunked (`m=1` then `m=0`), and each virtual placement carries `p=1`. Counts depend on the room. Drop `-o` and read the raw lines to see the actual ids.

## Replay a kitty capture through Ghostty

The tally above shows what RimZ sent, which does not prove how a terminal parsed it. A graphics fix that depends on ordering (two sidebars on one tmux client, say) is proven only at the consumer, so feed the `kitty.raw` from [Capture kitty graphics](#capture-kitty-graphics) through Ghostty's own stream and APC parser, and record the result of every kitty command. This harness proved the one-envelope-per-image fix in PR #627. On a capture of two `rimz list-pets` writers in one tmux window, Ghostty v1.3.1 read `errors=85` before the fix (41 `EINVAL: invalid data`, 39 `EINVAL: dimensions required`, 5 `ENOENT: image not found`) and `errors=0` after, over 4368 commands each.

Check out the Ghostty tag under the room's tmp. Ghostty v1.3.1 builds with zig 0.15.2, which `uvx` fetches from PyPI, so no system zig is needed:

```sh
G="$ROOT/tmp/ghostty"
git clone -q --depth 1 --branch v1.3.1 https://github.com/ghostty-org/ghostty "$G"
uvx --from ziglang==0.15.2 python -m ziglang version
```

The harness is a zig test inside Ghostty's kitty module. It mirrors `src/termio/stream_handler.zig::StreamHandler.apcEnd`: APC bytes go to Ghostty's `apc.Handler`, each completed kitty command runs through `Terminal.kittyGraphics`, and every other action goes to the read-only stream handler. RimZ sends `q=2`, which suppresses the reply an error would produce, so a patch to `graphics_exec.zig` records each response message in a file-scope variable before the quiet check discards it:

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

The first build compiles Ghostty's test binary (1 minute 46 seconds on a machine whose zig package cache was already warm, longer when it must fetch Ghostty's dependencies); a rerun on another capture reuses it and takes seconds. Replay a capture from the base branch and one from your change, and compare the two `SUMMARY` lines: a clean replay reads `errors=0`. The harness parses a byte stream in one pass, so it covers Ghostty's parser and image storage, not the app's rendering, and it pins one Ghostty tag. Newer Ghostty can differ (on `main` a delete also cancels a half-received chunked image), so name the tag you replayed against.

## Check against a real provider login

A held sandbox room replaces `HOME`, so a check that needs a real Claude or Codex turn runs in a host room started from the branch binary. Every `rimz` that room spawns has to be the branch build: the sender's shell, the provider hooks (`rimz hooks feed` resolves from the agent's inherited `PATH`), and the sidebar elder (which reads `RIMZ_BIN`). Start it from a host shell; `rimz start` refuses inside a RimZ sandbox, so an agent cannot start this room for you.

```sh
cargo build -p rimz --bin rimz
mkdir -p /var/tmp/rimz-livecheck/bin /var/tmp/rimz-livecheck/ws
cp target/debug/rimz /var/tmp/rimz-livecheck/bin/
export RIMZ_BIN=/var/tmp/rimz-livecheck/bin/rimz PATH=/var/tmp/rimz-livecheck/bin:$PATH
cd /var/tmp/rimz-livecheck/ws
rimz trust grant --agents claude,codex
rimz start --tmux
```

Before you read any timing, confirm from the shell that sends the messages that `command -v rimz` prints the staged path and `rimz --version` prints the branch build. If a login shell's rc (a `~/.zshrc` that prepends `~/.cargo/bin`, say) puts the installed `rimz` back first on `PATH`, the check silently runs the released binary. Once, that read as a false park miss.

A room built by hand, outside `cargo xtask sandbox`, needs `XDG_RUNTIME_DIR` set to a short path (a few characters under `/tmp`): socket paths under a long directory exceed the AF_UNIX limit and the room fails to start. The state root may be long.

## Traps

- A live check of anything the elder, a hook, or a loop fire spawns must run in a disposable room built from the worktree. In the real room those children are the installed `rimz`, so the check silently exercises the released binary instead of your change and passes either way. The held room also replaces `HOME`, so no provider login is reachable inside it and a real provider turn cannot be part of such a check.
- Look before capturing or clicking: an unwatched sidebar can hold a stale frame, because the renderer suppresses a dirty paint while the attached client is known to be looking elsewhere (`sidebar_pane/app/loop_state.rs` `dirty_paintable`). It is not a reliable negative control, so do not assert it: across runs an unwatched clock froze on tmux and on Zellij, and on one Zellij run it kept ticking.
- Look is `pane focus` on tmux and `zellij action go-to-tab` on Zellij, and the card prints the right one. `rimz pane focus` never moved the Zellij client: the sidebar stayed ~21 columns wide with its clock stopped at `0:00` until one `go-to-tab`, which widened it to 48 columns and resumed the clock.
- `sidebar click` is testkit-only. It uses the renderer's wakeup socket, exercising hit testing and focus routing but skipping terminal input and its parsing.
- The pipeline line renders no owner name; the click's focus target is the evidence for ownership.
- Read focus from the mux using the card's Focus command, not `rimz pane list`, which reports no focus on Zellij. Zellij's `is_focused` is per-tab and non-unique ([zellij-reference.md](../externals/mux-adapter/zellij-reference.md#types)): the check reads focus among the team tab's panes, so it proves pane focus inside that tab and not which tab the client is viewing.
- A relative program path passed directly to `sandbox in` resolves from your checkout. A path nested inside `env` or `sh -c` belongs to that command instead; pass the RimZ binary by absolute path in those cases.
- Wrapping `sandbox room` in your own `bwrap` (to keep its files in a scratch directory) needs `--dev-bind /dev /dev`; without it the room exits before starting with `starting sandbox cleanup reaper` / `Permission denied (os error 13)`.
- To live-check a signal emitter from an agent's shell tool, keep the emitting call alive until the run is recorded: `rz events emit <signal>; sleep 2; rz loop show <task> --json` in one call, with `rz` from [Run a command as an agent](#run-a-command-as-an-agent). An emit that ended its call recorded no run at all, while the same emit followed by the two-second wait delivered. The cause is not established; the guess is that the shell tool cleans up the emitter's detached child when the call returns. Judge delivery by the run record or `rimz message show <id>`, never by the emitter's `fired` count, which read the same both times.
- A supervised loop check (`loop add <name> --in 1m --agent worker …`) needs the stub provider's hooks before the add: run `rimz hooks install claude` through `sandbox in` first. The held room starts without them, and `loop add` refuses an agent task whose provider hooks are missing, which otherwise costs a retry while the room's `--for` timer runs.
- Stop the room by letting `--for` expire or killing the `xtask sandbox room` PID itself. Ctrl-C or a signal to its whole process group also cleans the room, because the reaper runs outside that group. Killing a wrapper shell leaves the room running. Kill by PID from `pgrep`, not with `pkill -f <pattern>`: the pattern matches the invoking shell's own command line, so `pkill` kills the shell that ran it.

# The Lobby — the room picker

> **Design.** The welcome screen — the *lobby* — is where Rimz lands when the entry path has no room to enter. It lists every room you know, local and remote, and takes you into one with a keypress. The room's sidebar then routes you across panes; the lobby routes you across rooms, one level up.

The lobby is one renderer over data Rimz already keeps: `rimz list`'s joined view of known workspaces and live multiplexer sessions, plus the saved remote aliases. It picks a room and dispatches to an existing command — `rimz attach`, `rimz start`, or `rimz remote connect host:session` — so resume, room gating, and supervised reconnect come for free.

## Where it appears

The lobby is the entry path's behaviour outside a workspace; it carries no command of its own. The fast path stays exactly as it is: `cd ~/code/query-engine && rimz` resolves that room and enters it directly. The lobby is what `rimz` opens when there is no room to enter, in place of refusing.

- `rimz` at a directory with no enclosing room — `$HOME` or `/`, which Rimz refuses today — opens the local lobby. The fast path never reaches it, so inside a project `rimz` still goes straight to the room and no second way in competes with it. `--root` still forces a deliberate directory room.
- `rimz remote connect <host>`, where the target is a bare host with no `:session-or-path`, opens the remote lobby for that host. `rimz remote connect pluto-xterm` lists `pluto-xterm`'s rooms; `rimz remote connect pluto-xterm:edgelord` and saved aliases still connect to that one room directly.

The wordmark and the pace heatmap the lobby renders also stand alone as [`rimz stats`](#rimz-stats), so you can read your pace from inside a room where the lobby never appears.

The lobby is a TTY surface. When stdin and stdout are not both interactive, the dead-end keeps its printed guidance and `rimz remote connect <host>` keeps its current behaviour, so scripts and pipes see no new interactive prompt. This mirrors the opportunistic-attach commitment in [DESIGN.md](../../../DESIGN.md#commitments).

## The local lobby

The lobby reads top to bottom in three zones, the way the sidebar does: the **wordmark** that names the surface, the **room list** that is the job, and the pinned **pace** graph of your token use. The room list is the scrolling body between two pieces of hero chrome that yield space first.

```
  ██████╗ ██╗███╗   ███╗  ███████╗
  ██╔══██╗██║████╗ ████║  ╚══███╔╝
 ██████╔╝██║██╔████╔██║    ███╔╝
██╔══██╗██║██║╚██╔╝██║   ███╔╝
  ██║  ██║██║██║ ╚═╝ ██║  ███████╗
  ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝  ╚══════╝
  The control room for your coding agents

 ◎ 6 rooms   ¤ 9 live                          ? 1   ! 1   ⢿ 1   ○ 3
 ─────────────────────────────────────────────────────────────────────
  local
▌? query-engine    ~/code/query-engine         zellij   ¤4 ?2      ◔ 2m
 ⢿ rimz            ~/work/rimz                  tmux     ¤3         ◔ 7m
 ! payments        ~/code/payments              tmux     ¤2 !1      ◑ 22m
 ○ docs-site       ~/code/docs-site             ·                   3d
   +2 older
 ─────────────────────────────────────────────────────────────────────
  remote
 ⇄ edgelord        pluto-xterm:/root/workspace/edgelord
 ⇄ prod            agent@prod-box:query-engine
 ⇄ pluto-xterm     browse rooms                              ›
 + connect a host…
 ─────────────────────────────────────────────────────────────────────
  your pace · tokens / day                          ◇ 412M this month
        Mar        Apr        May        Jun
        ░ ▒ █ ▓ ▒ █ █ ▓ █ ▓ ▒ ░ ▓ █ ▒ ·
  Mon   ▒ █ ▓ ▒ █ ▓ ░ ▒ ▓ █ █ ▓ █ ▓ ▒ ░
        █ ▓ ▒ ░ ▓ █ ▓ ▒ ▒ ░ ▒ █ ▓ █ █ ▒
  Wed   ▓ ▒ ░ · ▒ ▓ █ ▓ ░ ▒ ▓ █ ▒ ▓ █ ▓
        ▒ ░ · ░ █ ▓ ▒ ░ ▓ █ ▓ ▒ █ ▓ ▒ ░
  Fri   ░ · ░ ▒ ▓ ▒ ░ · █ ▓ █ ▓ ▓ █ ▒ ·
        · ░ ▒ ▓ ▒ ░ · ░ ▓ ▒ ░ ▒ ░ ▒ ▓ █
        less · ░ ▒ ▓ █ more              W 1.4B · M 5.2B · $8,666
 ─────────────────────────────────────────────────────────────────────
 ↵ enter   ␣ needs you   n new   x kill   / find   ? help       q quit
```

**The wordmark** renders the Rimz ASCII logo from the README in the brand tone, then the tagline beneath it. It is hero chrome: a short *or* narrow terminal collapses it to the compact `⌘ rimz · the control room for your coding agents` identity line — the logo's block art is ~33 columns wide, so a split pane never wraps it — and the room list keeps the space. The logo carries no state; it names the front door.

**The summary** reads the fleet of rooms at a glance, and its unit is the room throughout: `◎` the count of known rooms, `¤` the live agents summed across running rooms, then a room-level make-up pinned right — how many rooms want an answer (`?`), want a look (`!`), are working (`⢿`), or sit idle and dormant (`○`). The make-up reads the same glyphs the sidebar spends on agents, one altitude up: in the lobby a `?` is a room with someone waiting, in the sidebar a `?` is the waiting agent. The counts span every known room, folded rows included, so the tally stays honest while the body shows only the recent ones.

**A room row** leads with the room's worst-needed state, the same status vocabulary the sidebar uses on an agent row, aggregated over the room: a room with a waiting agent leads with `?`, one with a failed turn `!`, a calm working room `⢿`, a dormant known room `○`. The lobby is an attention map over rooms the way the sidebar is one over panes, the neediest first. The rest of the row reads left to right: the session name in the identity tone, the project root home-abbreviated and dim (left-truncating with a leading `…` before it crowds the columns), the running backend (`zellij` / `tmux`, or `·` for a dormant known room with no live session), the live signal in a fixed-width cluster (`¤N` agents, then the `?n` / `!n` attention tallies, each in its own slot so the age column never shifts as numbers change), and a last-activity age pinned right — the `◔→◉` clock-fill face for a live room, a coarse `3d` / `1w` for a dormant one. A dormant room is one Rimz remembers from the ledger whose multiplexer session is gone; entering it rebuilds the room from the durable rollup.

**Where the cursor lands, and how many rooms show.** Opened from inside a project, the lobby preselects that project's room, its name carried a step brighter so home is visible after the cursor moves; opened anywhere else, the cursor lands on the room that most needs you — the oldest `?`, then `!` — and falls back to the most recently active. `␣` re-jumps to the neediest at any time, the sidebar's own key one altitude up. The list leads with running rooms and rooms active recently, the way `rimz list` defaults; older dormant rooms fold behind a dim `+K older` that `a` unfolds and folds again, so a machine with fifty rooms opens to the handful you actually touch.

**The remote section** holds two kinds of entry, and the row says which `↵` does. A **room** — a saved alias from `~/.config/rimz/remote.toml` whose target names a session or path — connects straight to that room. A **host** — a row ending in a `›` chevron — drills into the host's remote lobby instead, so you never guess whether enter connects or browses. Hosts come from the saved aliases' hosts and `+ connect a host…` for an ad-hoc one; a host carries no eager probe, so the lobby never blocks on SSH until you choose to enter it. The `⇅ reconnect` flag rides a room row when its alias saved supervision.

The full glyph legend is canonical in [the sidebar interface reference](../../interface/sidebar.md#reading-the-glyphs); the lobby reuses it rather than restating it.

### Your pace — the token heatmap

The pace panel is a contribution graph of your token use, account-global, the trailing weeks of days, pinned above the footer. Each cell is one day, its shade rising with the tokens that day burned — the GitHub heatmap read in the terminal, the README's "Know Your Pace" brought to the front door before you pick a room. It is the panel [`rimz stats`](#rimz-stats) renders on its own; here it sits as hero chrome above the room list.

- **The cell ramp is five steps `· ░ ▒ ▓ █`,** a calm day through your heaviest. The density carries the reading and color reinforces it: truecolor tints those same five glyphs along one cool, lightness-varying ramp from the [theme pipeline](../sidebar/sidebar.md), held distinct from the status reds and greens so a busy day reads as volume, not as "good" or "wrong"; `NO_COLOR` reads the density alone, and the single-hue lightness ramp stays legible to colorblind eyes. The scale is per-graph — the busiest day in view sets `█` — so the texture reads against your own rhythm, not an absolute ceiling.
- **The figures speak the dashboard's vocabulary.** `◇` this month's tokens pins top-right, and the legend row carries the trailing-week and trailing-month token totals and the 30-day `$`, so the texture and the hard numbers sit together. `t` toggles the cell value between tokens and dollars; the legend stays and the scale re-bases.
- **It reads like the GitHub graph.** Month labels ride the top, weekday labels (Mon/Wed/Fri) the left, the week opening on Sunday; the trailing span fits the terminal width, more weeks on a wider screen.

The data is the full-history token-and-spend walk the provider dashboard already runs ([provider.md](../agents/provider.md)), bucketed by day from the transcript JSONL, account-global and read-only. It reads the shared `spending.json` cache and refreshes it in memory — the same incremental walk the producer runs — without writing it back, so the pace adds no new write path and never races the producer's owned cache.

The panel is hero chrome like the wordmark, and the room list is the job, so it yields space in two steps before the list ever loses a row: the full grid on a tall terminal, then a single-line sparkline of the trailing weeks with the same totals (`your pace ▁▂▄▆█▆▄▂▃ · M 5.2B · $8,666`) when height is tight, then gone. One read of the picture stays one keypress from the list whatever the size.

### First run and the empty list

The first time someone runs `rimz` at `$HOME`, there are no rooms to list, so the lobby opens on its actions instead of a blank column. The wordmark stays — this is the front door — and the body offers the moves that start a room, with the most likely one selected. There is no pace graph until something has been recorded.

```
  ██████╗ ██╗███╗   ███╗  ███████╗
   …  the control room for your coding agents

  No rooms yet — start one.

▌▸ start a room in this directory                                 ~/
 ▸ connect to a remote host…
 ─────────────────────────────────────────────────────────────────────
 ↵ choose   n path…   r remote   ? help                        q quit
```

This is the same surface the dead-end reaches, so the moment that used to print "refuses `$HOME`" now offers the fix as a choice: `↵` starts a room in the current directory, `n` prompts for any path, `r` opens the remote flow. Once a room exists, the list takes over and these actions live on those keys.

## rimz stats

`rimz stats` renders the wordmark, the token-activity heatmap, and your account-global usage insights on their own — the lobby's hero chrome as a command you can run anywhere, including inside a room where the lobby never appears. It reads your pace without leaving the workspace you are in, and it gives the README's "Know Your Pace" a name at the front of the CLI. The wordmark centres on the terminal; the body sits beneath it.

```
                    ██████╗ ██╗███╗   ███╗  ███████╗
                    ██╔══██╗██║████╗ ████║  ╚══███╔╝
                    ██████╔╝██║██╔████╔██║    ███╔╝
                    ██╔══██╗██║██║╚██╔╝██║   ███╔╝
                    ██║  ██║██║██║ ╚═╝ ██║  ███████╗
                    ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝  ╚══════╝
                The control room for your coding agents

  Token activity
        Mar        Apr        May        Jun
        ░ ▒ █ ▓ ▒ █ █ ▓ █ ▓ ▒ ░ ▓ █ ▒ ·
  Mon   ▒ █ ▓ ▒ █ ▓ ░ ▒ ▓ █ █ ▓ █ ▓ ▒ ░
        █ ▓ ▒ ░ ▓ █ ▓ ▒ ▒ ░ ▒ █ ▓ █ █ ▒
  Wed   ▓ ▒ ░ · ▒ ▓ █ ▓ ░ ▒ ▓ █ ▒ ▓ █ ▓
        ▒ ░ · ░ █ ▓ ▒ ░ ▓ █ ▓ ▒ █ ▓ ▒ ░
  Fri   ░ · ░ ▒ ▓ ▒ ░ · █ ▓ █ ▓ ▓ █ ▒ ·
        · ░ ▒ ▓ ▒ ░ · ░ ▓ ▒ ░ ▒ ░ ▒ ▓ █
  Less                              ░ ▒ ▓ █ More

  Week   ◇ 1.4B    ·  $1,902
  Month  ◇ 5.2B    ·  $8,666
  Year   ◇ 61B     ·  $103,540

  Models
  ● Opus 4.8 (87.8%)                ● Haiku 4.5 (3.1%)
    In: 61.0m · Out: 260.6m           In: 4.7m · Out: 6.6m
  ● Fable 5 (4.8%)                  ● Opus 4.7 (2.9%)
    In: 4.8m · Out: 12.9m             In: 1.8m · Out: 8.7m

  Sessions: 697
  Active days: 27/28                Longest streak: 27 days
  Most active day: May 29           Current streak: 27 days
```

The heatmap is the one the lobby embeds: the same five-step `· ░ ▒ ▓ █` ramp, the same per-graph scale, the same account-global full-history walk described under [your pace](#your-pace--the-token-heatmap) above — one renderer with two homes. It reads in or out of a workspace because the pace is keyed to the provider account rather than any one room.

Beneath it the panel adds the figures the heatmap implies. The **Week / Month / Year** totals are the trailing 7/30/365 days, tokens (`◇`) and dollars side by side. The **Models** breakdown shares each model's slice of your `◇` token total over the full history, with its input and output split, friendliest-name first (`claude-opus-4-8` reads as `Opus 4.8`). The **insights** close it: total sessions and the heaviest single day over the history, the trailing-four-week active ratio, and your longest and current active-day streaks. Tokens carry the per-model breakdown so the model layer never depends on a priced model — an unpriced model still attributes its tokens.

`rimz stats` prints the panel and returns to the shell, so it pipes and scrolls like any report rather than holding a TUI it has nothing to dispatch from. `--dollars` scales the heatmap by spend instead of tokens — the spend view the lobby's `t` toggles to — and `--json` emits the per-day buckets, the trailing windows, the model breakdown, and the insights for scripts. It is read-only: it refreshes the shared spending cache in memory and never writes it back, so it adds no write path and never races the sidebar producer's owned cache.

## The remote lobby

Selecting a host — or running `rimz remote connect pluto-xterm` — builds the guarded `ssh` invocation, runs the remote host's own `rimz list --json`, and renders that host's rooms with the same grammar. The header names the host and pins the SSH link badge; entering a room runs `rimz remote connect <host>:<session>`, attaching over the supervised link with its reconnect and link-health probe ([remote.md](./remote.md)).

```
 ⌘ pluto-xterm                                          ⇄ remote 38ms
   /root/workspace · ssh

 ◎ 3 rooms   ¤ 5 live                          ? 1   ⢿ 2   ○ 0
 ─────────────────────────────────────────────────────────────────────
▌? edgelord        /root/workspace/edgelord     zellij   ¤3 ?1      ◔ 4m
 ⢿ api             /root/workspace/api          tmux     ¤2         ◔ 9m
 ○ infra           /root/workspace/infra        ·                   2d
 ─────────────────────────────────────────────────────────────────────
 ↵ attach   h back   r refresh   ? help                          q quit
```

While the host is being reached, the body carries a `⠙ listing rooms on pluto-xterm…` line. A host that cannot answer states the fix in place and holds the lobby open — `! pluto-xterm: rimz not found on host · install: cargo install rimz` — the fail-fast precondition rendered where you can act on it, never a half-built remote attach. The link badge follows the same tone ramp as the sidebar footer's: calm gray warming to red as RTT and loss climb.

## Keys

Movement and focus mirror the sidebar, so one set of hands works both surfaces.

- `↑`/`↓` or `k`/`j` move; `J`/`K` jump between the local and remote sections; `g`/`G` go to the first or last row; `1`–`9` select by position; `␣` jumps to the room that most needs you.
- `↵` or `l` enters the selected row: attach a live local room, rebirth a dormant one, attach a remote room, or — on a `›` host — drill into its remote lobby.
- `h` or `←` steps back from a remote lobby to the local lobby (`Esc` does the same when nested); `q` quits to the shell from anywhere.
- `n` starts a new room — a path prompt defaulting to the current directory, dispatched as `rimz start <path>`; `r` is the remote key — connect or browse a host from the local lobby, re-probe the host's rooms from a remote one.
- `a` unfolds every known room and folds the dormant tail again; `/` fuzzy-finds over name and path with type-ahead, `Esc` clears the find.
- `x` kills the selected room's session through the backend, behind a confirm that names the casualty — `kill query-engine · 4 agents, 1 waiting?` — and defaults to No, since a kill ends live turns.
- `t` toggles the pace graph between tokens and dollars; `?` shows the legend-and-keys overlay.

## Launch is dispatch

The lobby resolves a choice into one existing command and hands the terminal over, the same way `rimz` hands off to the multiplexer's attach today. It owns the picking; the dispatched command owns the room.

| Choice | Dispatches |
| --- | --- |
| Local running room | `rimz attach <session>` |
| Local dormant known room | `rimz start <project_root>` — rebirth and resume |
| New (`n`) | `rimz start <path>` |
| Remote host (browse) | the remote lobby — `ssh … rimz list --json` |
| Remote room | `rimz remote connect <host>:<session>` |
| Kill (`x`) | the backend's `kill-session <name>`, after confirm |

This keeps the lobby a thin launcher: resume-on-rebirth, room gating, the daemon view, and supervised reconnect all live in the commands it calls, so the picker never reimplements them.

## How it is built

The lobby is a foreground client, not the pane-resident producer. It owns the terminal directly through `tui::TerminalModeGuard`, reads crossterm key and mouse events on a normal event loop, and wakes on a slow tick to refresh room rollups and the remote link badge. Rendering reuses the sidebar's [theme pipeline](../sidebar/sidebar.md) — the same palette, semantic slots, and `NO_COLOR`-safe shapes — so the two surfaces read as one product.

- **One source for two renderers.** The local rows come from the same join `rimz list` uses: `workspace::known_workspaces()` paired with each backend's `list_sessions()`. That join graduates from `cli/list.rs` into a shared function returning typed rows, so `rimz list` (the `--json` scripting projection) and the lobby render one truth, the way the snapshot view-model feeds the sidebar and `rimz pane list`.
- **Instant first paint.** The names and paths come from `known_workspaces()`, which is a cheap directory read, so the lobby draws the list on the first frame; the live signal, ages, and pace graph fill in on the next tick as the session probe and snapshot reads land. A launcher that stalls on a spinner before showing a single room has already lost — rooms first, enrichment after.
- **Read-only on the ledger.** Each room's agent count and attention come from its durable `snapshots/latest.json`, read in process, never a write and never a per-room producer — so the lobby stays cheap across dozens of rooms and live truth arrives only when you enter. The lobby stays out of the ledger-write import graph, the same boundary the sidebar holds.
- **The pace graph reuses the spend walk.** `compute_daily_spend` buckets the same deduplicated full-history entries the provider dashboard tallies, by UTC day, account-global, reusing the cross-file Claude dedup so a day's tokens match the trailing windows. It reads the shared `spending.json` cache and refreshes it in memory, never writing back — so the front door adds no new accounting path, only a new read of one Rimz already keeps.
- **Remote enumeration reuses the JSON contract.** Browsing a host runs the existing `rimz list --json` over a guarded `ssh` command built from `remote::RemoteTarget` and the link-probe's PATH-repair snippet; the rows parse from the documented `--json` schema (`workspace_id`, `project_root`, `session_name`, `running_on`, `last_activity`).
- **Cross-backend parity.** `list_sessions` already answers for tmux and Zellij; `x` calls a `MuxBackend` kill that both implement; entering dispatches to `rimz attach` / `start` / `remote connect`, which already pick the backend. No lobby behaviour depends on a backend-only feature.

The lobby has no command of its own: it is reached from the entry path — the `rimz start` dead-end at a roomless directory and `rimz remote connect <host>` — and its TUI lives in `src/welcome/` as a sibling of `sidebar_pane/`, split into render, input, and state the way the sidebar pane is. `rimz stats` is a one-shot render in `cli/stats.rs` that prints the wordmark and the pace panel and exits; the pace-panel renderer lives in `src/welcome/`, so the lobby and `rimz stats` draw one heatmap implementation.

## Keeping these frames honest

The lobby's rendered frames are golden-tested the way the sidebar's are: `cargo xtask test` re-renders each scenario and diffs it against a committed `.snap`, and a frame change updates the `.snap` and this doc together. Planned scenarios: the local lobby with running and dormant rooms, the cursor preselecting the cwd room, the dormant tail folded and `a`-unfolded, the empty first-run list, the wordmark collapsed on a short and a narrow terminal, the room-level make-up summary, the pace heatmap in color and under `NO_COLOR`, the pace sparkline at tight height, the kill confirm naming its casualty, the remote lobby mid-probe, an unreachable host with its fix line, and the find overlay.

## Non-goals

- The lobby picks and reaches rooms; it does not orchestrate across them. One root maps to one room, and entering hands off to that room's own sidebar.
- It adds no daemon. It is a foreground client that reads the ledger and the alias store and execs an existing command; nothing of it survives the handoff.
- `rimz list` stays the scriptable, `--json` projection. The lobby is its interactive sibling, not its replacement.

## Open questions

- **Dormant-room hygiene.** Whether `x` on a dormant known room (no live session) should also archive its ledger record, or stay a session-only kill and leave `rimz workspace`/`rimz gc` to own ledger cleanup.
- **Fresh vs resume on entry.** Whether the lobby surfaces a per-entry `--no-resume` modifier for rebirthing a dormant room empty, or leaves that to `rimz start --no-resume`.
- **`rimz stats` reach.** Whether `rimz stats` stays account-global on this machine, or also takes a host (`rimz stats <host>`) to read a remote account's pace over the same guarded `ssh` the remote lobby runs `rimz list --json` through.
- **Switching rooms from inside a room.** Whether the sidebar earns a key that pops the lobby to jump to another room, making the lobby the room switcher mid-session, or whether the front door stays the entry surface only.
- **Nested and overlapping rooms.** Whether the list flattens every room or mirrors `rimz doctor`'s room tree when a repo's child repos or two overlapping rooms share a path, so the nesting `doctor` surfaces reads here too.

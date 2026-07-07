# Your first session

> This page walks your first session, from install to a working fleet: what you type, what appears on screen, and why. [product.md](./product.md) is the working tour of everything Rimz does; the exact rendering of every glyph, meter, and zone is the [interface reference](../interface/sidebar.md). The frames below follow the renderer's real structure with illustrative values.

Rimz makes one promise, and this walk tests it end to end: it names the agent that needs you and takes you straight to its pane, where you answer in the agent's own UI. Everything else (the consent gate, the cards, the ranking, the reattach) serves that loop.

## Install, then one command

```sh
cargo install --locked rimz     # or: brew tap rimio/homebrew-rimz && brew install rimz

cd ~/code/query-engine          # any project you already have
rimz
```

Rimz needs Zellij (0.44+) or tmux (3.5+) on the machine; `rimz doctor` confirms your build clears the floor, and [installation.md](./installation.md) covers building from source.

Run `rimz` from a plain terminal. The room is its own Zellij or tmux session, so it cannot start from inside a session you are already attached to: if you live in a multiplexer, open a fresh terminal window outside it first. Running `rimz` inside one refuses with the room's name and the same advice.

The first run detects your multiplexer and your agents, writes the per-machine config set under `~/.config/rimz/` (`config.toml`, `theme.toml`, `agents.toml`, `loop.toml`, `remote.toml`, every key commented with its default), and asks only what it cannot detect. No account, no daemon, no hand-written config stands between you and the first frame.

## The consent gate

One question comes before the room: showing what an agent is doing means adding reporting hooks to the agent's own config, and Rimz never edits your agent config without asking. The same first-run flow carries the appearance probe and the pet opt-in, so the whole exchange reads top to bottom:

```
╭──────────────────────────────────────────────╮
│ rimz · first-run setup                       │
│                                              │
│ Rimz routes attention across your coding     │
│ agents into one sidebar.                     │
│                                              │
│ These hooks only report events to Rimz. They │
│ never answer a prompt for you.               │
╰──────────────────────────────────────────────╯

Rimz found 2 coding agents on this machine: claude, codex.
To show what an agent is doing, Rimz adds reporting hooks to the agent's config.
Each hook is one line like:  rimz hooks feed --source <agent>.
One quick question. Reversible any time with `rimz hooks uninstall`.

  claude · 1 of 2
    13 hooks → ~/.claude/settings.json (additive — existing hooks kept)
    also sets your statusLine to report context to Rimz (removed on uninstall)
    undo → rimz hooks uninstall claude

  codex · 2 of 2
    10 hooks → ~/.codex/config.toml (new file)
    undo → rimz hooks uninstall codex

  Add reporting hooks?  [Y/n]

✓ claude  13 hooks → ~/.claude/settings.json
✓ codex  10 hooks → ~/.codex/config.toml
All set — your agents appear in the sidebar as they run.

  ▐▐▐▐▐▐▐▐▐▐▐▐  (smooth color gradient)
                  (distinct icons)

  Icons and gradient render cleanly? [y/N] y
  Want a pet? It lives in the sidebar and reacts to your fleet. [y/N] y
✓ modern style: truecolor + Nerd Font icons
✓ rocky joins the room (rimz list-pets: more)
Next: docs/guide/setup.md for setup, `rimz config` for preferences.
Hands-off loop knobs live in ~/.config/rimz/loop.toml.
Opening the room...
```

The gate answers the reasonable fears before you can ask them.

- The change is additive and names the exact config path; `rimz hooks install --dry-run` prints the full patch before you consent, and replays it any time after.
- The boundary is stated in the consent itself: the hooks report events, and answering a prompt stays with you.
- Every exit stays open: Enter wires every listed agent, `n` or EOF installs nothing, and an unwired agent still shows up as a plain process row with a hint on how to wire it later.
- Install is per-machine state: later runs go straight to the room, and `rimz doctor` reports per-agent status.
- A committed project config is its own separate gate with its own diff ([trust.md](../internals/harness/trust.md)); a toy project never shows it.

Backing out is the mirror of opting in: `rimz hooks uninstall` removes exactly what the gate added and restores your statusline. The authoritative hook set and config shape live in [agent.md → Hook install](../internals/agents/agent.md#hook-install-the-visible-security-step).

## The room, empty

Consent done, Rimz creates the session and drops you in: a working shell pane on the right, focused, and the sidebar pinned left. Even empty, the column already shows its core idea, one row per pane:

```
 ⌘ query-engine

 ◎ 0                              ◇ 0 ↘ 0 ↗ 0 ◌ 0
 ¤ 0                                        $0.00
 ─────────────────────────────────────────────────

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌○ zsh

                                       ? for help
```

`⌘ query-engine` is the project you are standing in, the header counts sessions (`◎`) and live agents (`¤`) with the room's token and dollar tallies pinned right, and the shell pane is itself a row, grouped under the worktree you are standing in. With no agents in the room there is no attention line to scan yet.

On Zellij, Rimz loads a small presence plugin that reports pane topology; its permission grant is seeded automatically into Zellij's own permission store, so no extra prompt appears. Revoking the grant in Zellij disables pane discovery until restored, and `rimz doctor` names the fix ([security.md → The Zellij presence plugin](./security.md#the-zellij-presence-plugin)).

## Your first agent

Type `claude` in the shell pane. Within a second or two the row that read `○ zsh` becomes the agent's card, the same row re-skinned, not a second one:

```
 ? 0   ! 0   ⏸ 0   ✓ 0                 ⢿ 0   ○ 1

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌○ claude · Opus 4.8 · xhigh
```

The session-start hook fired, the store overlaid identity onto the pane, and the row updated: your agent is in the sidebar, correctly named, with its model and effort. No config, no flag, no restart. An idle agent fills no attention bucket; it is presence, not a cue.

Give Claude a task and the card fills out: the leading glyph animates (`⠁` while it reasons, `⣾` once it edits), the description line carries what it is on, and the context meter starts filling:

```
 ? 0   ! 0   ⏸ 0   ✓ 0                 ⢿ 1   ○ 0

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌⣾ claude · Opus 4.8 · xhigh · 200k         $0.42
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━──────────────────────── 41.2%
```

A running agent is not a cue to do anything: the `?` and `!` buckets hold at zero, so you get coffee or open a second agent. A running agent that goes silent past the stall window (30 minutes by default, `[agents.attention]` in `agents.toml`) escalates to `!` on its own, so a wedged run cannot hide behind a spinner.

## A question reaches you

Open two more agents while the first works (another `claude` on tests, a `codex` on the API) and go heads-down in one of their panes. Then the first Claude hits a permission prompt: the hook lands the waiting signal in the store, the row flips to `? waiting`, rises to the top of its worktree, the cockpit line counts it (`? 1`), and a native notification fires.

```
 ⌘ query-engine

 ◎ 3                      ◇ 52k ↘ 14k ↗ 38k ◌ 41k
 ¤ 3 (1)                                    $2.46
 ─────────────────────────────────────────────────
 ? 1   ! 0   ⏸ 0   ✓ 0                 ⢿ 2   ○ 0

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus 4.8 · xhigh · 200k         $1.27
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━──────────────────────── 41.2%
▎⠁ claude · Sonnet 4.6 · high · 200k        $0.31
▎  add tests
▎  ▣ ━━━━━━──────────────────────────────── 18.0%
▎⣾ codex · GPT 5.5 · high                   $0.88
▎  refactor api
▎  ▣ ━━━━━━━━━━━━━━━━━━━━━━━─────────────── 63.4%

                                       ? for help
```

Even from another pane or another app, the OS notification reaches you:

```
  ⬤ claude needs you · query-engine
    Permission — fix auth flow
```

Select the row, click the notification, or hit the triage key from the next section, and you land in Claude's pane reading the actual prompt: the real command Claude wants to run, approved or denied in Claude's own UI. You were heads-down somewhere else, and Rimz tapped you on the shoulder with exactly the right pane, one keystroke away. You never had to stop and ask which of these terminals is blocked.

The mechanics behind that moment are small and deliberate. Every waiting row routes you to the agent's UI, where the full context and the safe defaults already live. Notifications are best-effort polish: clicking one focuses the terminal and pre-selects the row, but the store is authoritative, so a missed notification loses nothing. Three agents going waiting at once coalesce into one notification, and an agent that stays waiting past a threshold earns a single nudge rather than a stream.

The same waiting state is a hook for automation later: a notification handler can wake a script that answers a routine prompt right in the agent's pane, and anything the script skips stays `? waiting` and still routes to you ([where to go next](#where-to-go-next)).

## A fleet, and the two keys that tame it

Now the load the product was built for: two more agents in a second worktree, plus a deploy script in a pane of its own. This walk typed each agent into its own pane to show the mechanics; `rimz agents claude,codex --worktree=feature-migration` opens the pair in one command, and the [product tour](./product.md) covers layouts and teams. Either way, the column stays scannable.

```
 ⌘ query-engine

 ◎ 8                      ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 5 (2)                                    $4.20
 ─────────────────────────────────────────────────
 ? 1   ! 1   ⏸ 0   ✓ 0                 ⢿ 2   ○ 1

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus 4.8 · xhigh · 200k         $1.27
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━──────────────────────── 41.2%
▎⠁ claude · Sonnet 4.6 · high · 200k        $0.31
▎  add tests
▎  ▣ ━━━━━━──────────────────────────────── 18.0%
▎⣾ codex · GPT 5.5 · high                   $0.88
▎  refactor api
▎  ▣ ━━━━━━━━━━━━━━━━━━━━━━━─────────────── 63.4%

 ⑂ feature-migration ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ +230 -23
 ! claude · Opus 4.8 · xhigh                $2.05
   db migrate
   ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━───── 84.1%
 ○ codex · GPT 5.5 · low

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
 ○ deploy.sh

                                       ? for help
```

The cockpit line is where the eye lands first: `? 1   ! 1`, one waiting and one failed, summed across every worktree. Above it, the token mix and `$4.20` read the day's pace, and `(2)` counts unread rows. Ranking does the triage: waiting and failed rows rise first, unread rows break ties, idle agents and process rows settle below, and each worktree caps its tail with a dim `+K more`. Exactly one row is ever in motion, the oldest one that needs you, so the eye lands on where to go next.

Two keys collapse seeing the blocked pane and getting to it into one motion:

- `Alt+p` (config: `[sidebar] focus_key`) reaches the sidebar from any pane in the room, bound only inside the Rimz session, so your global mux config is untouched.
- `Space` (or `n`) jumps to the next item that needs you, in ranking order, and focuses its pane; press it again for the next, `N` steps back.

Five agents, two keys, straight to the oldest blocked one; the same two keys hold at fifty. Clicking works everywhere too: rows jump, cockpit buckets filter the column by status, and the `↑ N need you` banner appears when the lead unread card is scrolled out of view. `?` opens the keys-and-filter overlay in place, so all of this is learnable without leaving the room ([interface reference → Bottom chrome](../interface/sidebar.md#bottom-chrome)).

The grouping matches how the work divides: only same-worktree agents share files, so each worktree reads as one bounded block, the one holding your selection bracketed as a lane. The `┄ external ┄` divider holds scripts, CI, and panes outside any worktree, and always sorts last. Every status reads by glyph shape under `NO_COLOR` and to color-blind eyes; color only reinforces ([DESIGN.md → Triage at a glance](../../DESIGN.md#triage-at-a-glance)).

## Leave, and come back from anywhere

A new tab changes the roster, not the layout: every tab is born with its own sidebar pane, all of them render the same room-wide snapshot, and selecting any row jumps to that agent's pane wherever it lives in the session. Tabs are viewports; worktrees are the subdivision.

Detach with your multiplexer's own key (Zellij `Ctrl-O d`, tmux `prefix d`). The room keeps running headless, agents working, hooks landing in the store while nobody renders.

Coming back on the same machine is the command you already know: `rimz` in the project directory returns to the room. `rimz list` names every known room and which multiplexer runs it; `rimz attach <session>` reaches one by that session name from any directory. From another machine, hours later, on a laptop or a tablet:

```sh
rimz remote connect dev-box:~/code/query-engine
```

The link is plain SSH with a supervisor that reconnects itself when the train wifi drops, and a `⇄ remote 210ms` badge in the sidebar footer reads link health. Save an alias once (`rimz remote add dev dev-box:~/code/query-engine`) and `rimz remote connect dev` is the whole trip; `--web` tunnels the same room to your local browser.

The sidebar comes back exactly as you left it: every agent where it was, every question still waiting, ranked identically, plus whatever finished while you were gone, already triaged by the same ranking. The first usable frame paints from durable state with no loading screen. Continuity is store-owned ([DESIGN.md → Invariants](../../DESIGN.md#invariants)); keeping the processes alive across reboots is the host's job (systemd, tmux-resurrect, Zellij resurrect; [DESIGN.md → Non-goals](../../DESIGN.md#non-goals)).

## When something goes wrong

You have to be able to tell a stale frame from a current one at a glance, so failures render as failures. When a snapshot fetch fails (the binary moved, the store directory vanished mid-write), a banner names the cause and counts how long the frame has been stale:

```
 ⌘ query-engine

 ◎ 1
 ¤ 1
 ─────────────────────────────────────────────────
 ? 0   ! 0   ⏸ 0   ✓ 0                 ⢿ 1   ○ 0

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌⢿ claude · Opus 4.8 · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━──────────────────────── 41.2%


 ! Sidebar degraded for 8s: snapshot
 failed: store not found
```

On recovery the banner steps down to a dim, dismissable notice (`⚠ last alert 8s ago: … · x dismiss`), so a failure that flickered past is still visible after the fact; `x` clears it and a fresh failure re-arms it.

The same honesty runs through trust and drift: an untrusted `.rimz/config.toml` keeps its command-running fields inert until you review the diff and run `rimz trust grant`, and a version mismatch after an upgrade lands in `rimz doctor` rather than a silently frozen pane. Banners, the trust state, and `rimz doctor` are the three places Rimz tells you what it currently cannot promise.

## Where to go next

- [Set up your machine](./setup.md): the one-time pass that makes Rimz a daily driver (true color and Nerd Font glyphs, pets, the hands-off loop settings, a Zellij/tmux baseline).
- [Product tour](./product.md): the working scenarios (teams on features in isolated worktrees, messaging agents by handle, scripting supervised runs with `-p`, engineering loops past your attention span).
- [Attention](./attention.md): how the ranking decides what needs you.
- Automate the waiting state: a notification handler answers routine prompts in the agent's own pane, and anything outside its policy still routes to you ([notifications.md](../internals/sidebar/notifications.md), [security.md](./security.md)).

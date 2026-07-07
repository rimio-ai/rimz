# The first session: install to fleet

> This page walks your first session, from install to a working fleet: what you type, what appears on screen, and why. [product.md](./product.md) is the working tour of everything Rimz does; the exact rendering of every glyph, meter, and zone is the [interface reference](../interface/sidebar.md). The frames below follow the renderer's real structure with illustrative values.

Rimz makes one promise, and this walk tests it end to end: it names the agent that needs you and takes you straight to its pane, where you answer in the agent's own UI. Everything else (the consent gate, the cards, the ranking, the reattach) serves that loop.

## Install, then one command

```sh
cargo install --locked rimz     # or: brew tap rimio/homebrew-rimz && brew install rimz

cd ~/code/query-engine          # any project you already have
rimz
```

`rimz` needs Zellij (0.44+) or tmux (3.5+) on the machine; `rimz doctor` confirms your build clears the floor, and [installation.md](./installation.md) covers building from source.

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

The gate answers the reasonable fears before you ask them.

- The change is additive and names the exact config path; readers who want the full patch run `rimz hooks install --dry-run` before consenting.
- The boundary is stated in the consent itself: the hooks report events, and answering a prompt stays with the reader.
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

The reader did nothing extra, and their agent is in the sidebar, correctly named, with its model and effort. The session-start hook fired, the store overlaid identity onto the pane, and the row updated: no config, no flag, no restart. This is the activation moment, the first time the product does something for them, and the latency budget is tight: the row has to update within a second or two of the hook or the magic reads as lag. An idle agent fills no attention bucket; it is presence, not a cue.
Give Claude a task and the card fills out: the leading glyph animates (`⠁` while it reasons, `⣾` once it edits), the description line carries what it is on, and the context meter starts filling:

```
 ? 0   ! 0   ⏸ 0   ✓ 0                 ⢿ 1   ○ 0

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌⣾ claude · Opus 4.8 · xhigh · 200k         $0.42
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━──────────────────────── 41.2%
```

A running agent is not a cue to do anything: the `?` and `!` buckets hold at zero, so you get coffee or open a second agent. A running agent that goes silent past the stall window (30 minutes by default, `[agents.attention]`) escalates to `!` on its own, so a wedged run cannot hide behind a spinner.

## The question reaches you

This is the moment Rimz earns its place. Claude hits a permission prompt: the hook lands the waiting signal in the store, the row flips to `? waiting`, rises to the top of its worktree, the cockpit line counts it (`? 1`), and a native notification fires.
```
 ⌘ query-engine

 ◎ 12                     ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 6 (2)                                    $4.20
 ─────────────────────────────────────────────────
 ? 1   ! 1   ⏸ 0   ✓ 0                 ⢿ 2   ○ 2

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

Even from another pane or another app, the OS notification reaches the reader:

```
  ⬤ claude needs you · query-engine
    Permission — fix auth flow
```

They select the row, or click the notification, or hit the global triage key from the next section, and land in Claude's pane reading the actual prompt: the real command Claude wants to run, approved or denied in Claude's own UI. They were heads-down somewhere else and Rimz tapped them on the shoulder with exactly the right pane, one keystroke away. They never had to stop and ask which of these terminals is blocked. That is the whole pitch, and it just worked.

The reasons it lands this way are small and deliberate. Every waiting row routes the reader to the agent's UI, where the full context and the safe defaults already live. Notifications are best-effort polish: clicking one focuses the terminal and pre-selects the row, but the store is authoritative, so a missed notification loses nothing. Three agents going waiting at once coalesce into one notification, and an agent that stays waiting past a threshold earns a single nudge rather than a stream.

The same waiting state is a hook for automation later (the growing-into-it section below): a notification handler can wake a script that answers a routine prompt right in the agent's pane, and the row clears when the agent moves on. Anything the script skips stays `? waiting` and still routes to the pane.

## A fleet, and the one key that tames it

The reader does what they came to do: spins up four more agents across two worktrees, plus a deploy script in a pane of its own. This is the load the product was built for, and it stays scannable.

```
 ⌘ query-engine

 ◎ 12                  ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 6                                    $4.20
 ────────────────────────────────────────────
 ? 1   ! 1   ⏸ 0   ✓ 0              ⢿ 2   ○ 2

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━────────────────────   41%
▏⠁ claude · Sonnet · high · 200k
▏  add tests
▏  ▣ ━━━━━━────────────────────────────   18%
▏⢿ codex · GPT 5.5 · high
▏  refactor api
▏  ▣ ━━━━━━━━━━━━━━━━━━━━━─────────────   63%

 feature-migration                   +230 -23
 ! claude · Opus · xhigh · 1m   db migrate
   ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━─────── 84.1%
 ○ codex · GPT 5.5 · low

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
 ○ deploy.sh

                                       ? for help
```

The cockpit line is where the eye lands first: `? 1   ! 1`, one waiting and one failed, summed across every worktree. Above it, the token mix and `$4.20` read the day's pace, and `(2)` counts unread rows. Ranking does the triage: waiting and failed rows rise first, unread rows break ties, idle agents and process rows settle below, and each worktree caps its tail with a dim `+K more`. Exactly one row is ever in motion, the oldest one that needs you, so the eye lands on where to go next.

Two keys collapse seeing the blocked pane and getting to it into one motion:

- `Alt+p` (config: `[sidebar] focus_key`) reaches the sidebar from any pane in the room, bound only inside the Rimz session, so your global mux config is untouched.
- `Space` (or `n`) jumps to the next item that needs you, in ranking order, and focuses its pane; press it again for the next, `N` steps back.

Twelve agents, two keys, straight to the oldest blocked one. Clicking works everywhere too: rows jump, cockpit buckets filter the column by status, and the `↑ N need you` banner appears when the lead unread card is scrolled out of view. `?` opens the keys-and-filter overlay in place, so all of this is learnable without leaving the room ([interface reference → Bottom chrome](../interface/sidebar.md#bottom-chrome)).

The grouping matches how the work divides: only same-worktree agents share files, so each worktree reads as one bounded block, the one holding your selection bracketed as a lane. The `┄ external ┄` divider holds scripts, CI, and panes outside any worktree, and always sorts last. Every status reads by glyph shape under `NO_COLOR` and to color-blind eyes; color only reinforces ([DESIGN.md → Triage at a glance](../../DESIGN.md#triage-at-a-glance)).

## Leave, and come back from anywhere

Detach with your multiplexer's own key (Zellij `Ctrl-O d`, tmux `prefix d`). The room keeps running headless, agents working, hooks landing in the store while nobody renders. Hours later, from a laptop or a tablet:

Opening a new tab or window and starting a fifth agent there changes the roster, not the layout. Every tab is born with its own sidebar pane, and all of them render the same room-wide snapshot, so the column is identical everywhere and selecting any row jumps to that agent's pane wherever it lives in the session. The sidebar's own pane is chrome, excluded from the roster, and it self-closes when the last working pane in its tab exits. Tabs are viewports; worktrees are the subdivision.

Then the reader closes the laptop with the mux's own detach key (Zellij `Ctrl-O d`, tmux `prefix d`). The room keeps running headless on the host, and the store keeps queuing events while nobody renders. Hours later they reattach from a tablet on the train.

```
$ ssh dev-box rimz attach query-engine
   reconstructing query-engine from store…```

The link is plain SSH with a supervisor that reconnects itself when the train wifi drops, and a `⇄ remote 210ms` badge in the sidebar footer reads link health. Save an alias once (`rimz remote add dev dev-box:~/code/query-engine`) and `rimz remote connect dev` is the whole trip; `--web` tunnels the same room to your local browser.

The sidebar comes back exactly as the reader left it: every agent where it was, every question still waiting, ranked identically, plus whatever finished while they were gone, already triaged by the same ranking. The first usable frame paints from the store immediately, since a resize or attach is itself a wakeup, so reattach reconstructs from durable state with no loading screen.

This is what changes how the reader works: start a run on the dev box, close everything, and pick it up on a phone at the airport. Continuity is store-owned ([DESIGN.md → Invariants](../../DESIGN.md#invariants)); the running processes are the host's job — systemd, tmux-resurrect, Zellij resurrect ([DESIGN.md → Non-goals](../../DESIGN.md#non-goals)).

## When something is wrong

The honesty commitment gets tested when a fetch fails: the binary moved, the store directory vanished mid-write, a snapshot is half-written. The reader has to be able to tell a stale frame from a current one at a glance.

```
 ⌘ query-engine

 ◎ 1
 ¤ 1
 ────────────────────────────────────────────
 ? 0   ! 0   ⏸ 0   ✓ 0              ⢿ 1   ○ 0

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌⢿ claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━────────────────────   41%


 ! Sidebar degraded for 8s: snapshot
 failed: store not found```

The banner names the cause and counts how long the frame has been stale. On recovery it steps down to a dim, dismissable notice (`⚠ last alert 8s ago: … · x dismiss`), so a failure that flickered past is still visible after the fact; `x` clears it and a fresh failure re-arms it.

The same honesty runs through trust and drift: an untrusted `.rimz/config.toml` keeps its command-running fields inert until you review the diff and run `rimz trust grant`, and a version mismatch after an upgrade lands in `rimz doctor` rather than a silently frozen pane. Banners, the trust state, and `rimz doctor` are the three places Rimz tells you what it currently cannot promise.

## Where to go next

## Growing into it: building on the room

By now the reader is hooked on the observe-and-route loop, and the product grows with them along paths they discover when they need them.

Automation over the waiting state is the morning-after upgrade: tired of approving `cargo check` for the eighth time, the reader wires a notification handler that wakes on waiting rows and answers the routine ones in the agent's own pane — a bounded-pattern script over `rimz pane capture` and `rimz pane send`, or a supervised agent delegate launched with `rimz agents <kind> -p`. Anything outside policy stays waiting and routes to the reader as before, which is what lets the fleet keep working while they sleep. Handler wiring is in [notifications.md](../internals/sidebar/notifications.md); the safety posture is in [security.md](./security.md).

Unattended runs are the same idea without waiting on a person, and `rimz agents <kind> "<prompt>" -p` makes the whole shape scriptable; the detail lives in [product.md → Put your pipeline in the room](./product.md#put-your-pipeline-in-the-room).

And because agents and scripts share one CLI, a deploy or migration script can steer the same fleet — hand work to a running agent with `rimz message`, run a judgment call as a supervised turn, read back transcripts — straight from the pipeline ([the pipeline scenario](./product.md#put-your-pipeline-in-the-room)).

## The experience in one screen

| Section | Reader does | Sees | Feels |
| --- | --- | --- | --- |
| First run | installs, consents, lands in the room | config pointer, one hook consent prompt, then `○ zsh` and a hint | reassured, then oriented |
| The agent shows up | types `claude`, then prompts it | row re-skins to `○ claude`, then `⢿ running` | delight, then calm |
| The question reaches you | gets notified, jumps to the pane | `? waiting`, an OS notification | the pitch lands |
| The fleet | presses Space | grouped roster, `? 1  ! 1` | in control |
| Detach, reattach | closes laptop, ssh back | the column reconstructed from the store | relief, then trust |
| When wrong | hits a failure | a labeled degraded banner | trust through honesty |
| Growing into it | wires a notification handler | routine rows clearing | leverage |

The arc runs from curiosity to reassurance to delight to the pitch landing to mastery to trust. If the question moment does not land inside five minutes, nothing after it matters.

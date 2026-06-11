# The Rimz experience: first run to fleet

> A walkthrough from the chair of someone meeting Rimz for the first time. [product.md](./product.md) is the working tour and [DESIGN.md](../../DESIGN.md) holds the invariants; this doc follows the felt experience from the first keystroke to a ten-agent fleet. The frames below are illustrative sketches of the moment; the exact, machine-checked rendering of every glyph, meter, and zone is the [interface reference](../interface/sidebar.md), and renderer mechanics live in [docs/internals/sidebar/sidebar.md](../internals/sidebar/sidebar.md).

The reader this doc is written for runs Claude Code and Codex agents all day, several at once, and is tired of flipping tabs to find the one that is blocked. They saw Rimz on Hacker News an hour ago and want to feel the value in under five minutes or they close the tab. Every choice below serves that five minutes.

Two commitments run underneath the whole walk. Rimz is honest by default: the column shows the truth about what is running, and when a fetch fails it labels the frame out loud rather than passing stale data off as fresh. And Rimz notifies and routes: its whole job is to name the agent that needs you and take you straight to its pane, where you answer in the agent's own UI, where the full context lives.

## First run: consent, then the room

Discovery happens before the terminal. The reader reads the Hacker News post, clicks through to a landing page that earns the install in three lines (one room per project, a sidebar that tells you which agent needs you, survives detach and reattach from anywhere), and runs a single command.

```sh
# one of:
brew install rimz
cargo install rimz
curl -fsSL https://rimz.sh/install | sh

cd ~/code/query-engine   # a real, small project they already have
rimz
```

The first command is `rimz`, and it auto-detects the multiplexer (Zellij or tmux) and the agents (Claude, Codex), discovering or asking for everything else in flow. There is no init wizard, no config file, and no account between the reader and the first frame.

The first time `rimz` runs on a machine, it does not drop straight into the room. To show what an agent is doing, Rimz adds reporting hooks to the agents already on the machine, and editing their config is exactly the thing a cautious reader is nervous about. So the first screen is a clean inline consent gate, terminal-native and left in scrollback, that treats the change as a security surface and turns nervousness into trust.

```
  rimz hook install

  Rimz routes attention across your coding agents into one sidebar.
  To show what an agent is doing, it adds reporting hooks to the agents on this machine.
  These hooks only report events to Rimz. They never answer a prompt for you.

  Detected agents (space toggles):
  > [x] claude  8 events  ~/.claude/settings.json
    [x] codex   6 events  ~/.codex/config.toml

  What changes: additive config edits; existing hooks are kept.
    + claude: 8 events at ~/.claude/settings.json
    + codex: 6 events at ~/.codex/config.toml

  Diff  1/24
  --- ~/.claude/settings.json
  +++ ~/.claude/settings.json
  @@ -8,6 +8,14 @@
       "UserPromptSubmit": [
         { "hooks": [{ "type": "command", "command": "my-existing-hook" }] }
       ],
  +    "SessionStart": [
  +      { "hooks": [{ "type": "command", "command": "rimz hooks feed --source claude" }] }
  +    ],

  [Enter] install selected   [Space] toggle   [d] hide diff   [s/Esc] skip
```

The gate answers the two fears before they are spoken. The change is additive and names the keys it preserves, so "it will overwrite my hooks" dies in one line, and the diff is a real unified diff with unchanged regions collapsed (press `d` to expand it). It states the boundary in the consent itself: the hooks report events, and answering a prompt stays with the reader. Space toggles any agent that should stay unwired, and `s` or Esc skips entirely, installing nothing and still dropping the reader into the room, where an unwired agent shows up as a plain process row and the empty-room hint says how to wire it later. Hook install is per-machine state, so later runs go straight to the room, and `rimz doctor` reports per-agent status for anyone who forgets. A committed project config, if this repo ever carries one, is its own separate gate with its own diff (see [trust.md](../internals/sidebar/trust.md)); a toy project has none, so day one never shows it. The authoritative wired set and config shape live in [hooks.md](../internals/agents/hooks.md#hook-install--the-visible-security-step).

Backing out is the mirror of opting in, and the reader is told so right here: `rimz hooks uninstall` removes exactly what the gate added, the additive diff in reverse, and leaves the agents untouched. A tool you can cleanly remove is a tool worth trying.

With consent done, Rimz makes sure the session exists and drops the reader in: a working shell pane on the right, focused and pristine, and the sidebar pinned left at about 30% width. On Zellij, one more one-time approval can greet them, a small floating prompt from Zellij itself asking to let Rimz's presence plugin watch pane state, focus panes after tab switches, and run commands. That plugin is the push channel that keeps the sidebar fresh without polling and keeps tab switches landing on work ([security.md](./security.md#the-zellij-presence-plugin)); `y` dismisses it for good across sessions, and declining keeps Zellij's native focus memory while the sidebar polls.

```
 ⌘ query-engine

 ◎ 0
 ¤ 0
 ────────────────────────────────────────────

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌○ zsh

                  ? for help
```

The column shows presence from the very first frame: the shell pane is itself a row. With nothing needing attention, the cockpit make-up line is omitted and a dim hint points at the one next thing to do. The `⌘ query-engine` line is the project name the reader recognizes, and the `▏main` lane shows which worktree they are standing in. Even empty, the column has already demonstrated its core idea, one row per pane, before any agent exists, and the first agent or feed item will fill the body without disturbing the chrome around it.

## The agent shows up, and starts working

The reader types `claude` in the shell pane and just looks at its input box, without prompting it yet. Within about a second, the pane that read `○ zsh` becomes the agent's row. The same row, re-skinned into one entry, not a second one.

```
 ⌘ query-engine

 ◎ 1
 ¤ 1
 ────────────────────────────────────────────
 ? 0   ! 0   ⏸ 0   ✓ 0              ⢿ 0   ○ 1

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌○ claude · Opus · xhigh
▌  —
▌  ▣ ──────────────────────────────────    0%

                  ? for help
```

The reader did nothing extra, and their agent is in the sidebar, correctly named, with its model and effort. The session-start hook fired, the ledger overlaid identity onto the pane, and the row updated: no config, no flag, no restart. This is the activation moment, the first time the product does something for them, and the latency budget is tight: the row has to update within a second or two of the hook or the magic reads as lag. An idle agent fills no attention bucket; it is presence, not a cue.

Then the reader gives Claude a task. The prompt and the first tool call move the row to `⢿ running`, and the task slot fills with the agent's reported task, or the first twenty or so characters of the prompt.

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

                  ? for help
```

The attention buckets hold at `? 0  ! 0`, because running is not a cue to do anything. The reader goes to get coffee, or opens a second agent. There is no global "updated 2s ago" stamp anywhere in the product; freshness is per-row and fetch health is the degraded banner's job, so a resting card stays calm with no age on it. A running agent that wedges outs itself by escalating to the static `!` attention state once it falls silent past the stall window (thirty minutes by default), not by a creeping timestamp. A coarse last-activity age surfaces in exactly one place, the expanded work line you opt into by selecting the row.

## The question reaches you

This is the moment Rimz earns its place. Claude hits a permission prompt: a feed item is written to the ledger, the row flips to `? waiting`, rises to the top of its worktree, the cockpit make-up counts it (`? 1`), and a native notification fires.

```
 ⌘ query-engine

 ◎ 1
 ¤ 1
 ────────────────────────────────────────────
 ? 1   ! 0   ⏸ 0   ✓ 0              ⢿ 0   ○ 0

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━────────────────────   41%

                 ? for help        ␣ next ?!
```

Even from another pane or another app, the OS notification reaches the reader:

```
  ⬤ claude needs you · query-engine
    Permission — fix auth flow
```

They select the row, or click the notification, or hit the global triage key from the next section, and land in Claude's pane reading the actual prompt: the real command Claude wants to run, approved or denied in Claude's own UI. They were heads-down somewhere else and Rimz tapped them on the shoulder with exactly the right pane, one keystroke away. They never had to stop and ask which of these terminals is blocked. That is the whole pitch, and it just worked.

The reasons it lands this way are small and deliberate. Every waiting row routes the reader to the agent's UI, where the full context and the safe defaults already live, rather than trying to reproduce the decision on the row. Notifications are best-effort polish: clicking one focuses the terminal and pre-selects the row, but the ledger is authoritative, so a missed notification loses nothing. Three agents going waiting at once coalesce into one notification, and an agent that stays waiting past a threshold earns a single nudge rather than a stream.

Enrol a resolver later (the growing-into-it section below) and this same waiting row shows the chain working the item instead of asking the reader: the glyph becomes a braille spinner and the task slot reads the resolver and its remaining budget. It still counts in the `?` tally, because the item is pending and just being handled, and it returns to `? waiting` only if the chain comes up empty.

## A fleet, and the one key that tames it

The reader does what they came to do: spins up four more agents across two worktrees, plus a deploy script paused at a gate. This is the load the product was built for, and it stays scannable.

```
 ⌘ query-engine

 ◎ 12                  ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 6                                    $4.20
 ────────────────────────────────────────────
 ? 2   ! 1   ⏸ 0   ✓ 0              ⢿ 2   ○ 1

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━────────────────────   41%
▏⢄ claude · Sonnet · high · 200k
▏  add tests
▏  ▣ ━━━━━━────────────────────────────   18%
▏⢿ codex · GPT 5.5 · high
▏  refactor api
▏  ▣ ━━━━━━━━━━━━━━━━━━━━━─────────────   63%

 feature-migration                   +230 -23
 ! claude · Opus · xhigh · 1m
   db migrate
   ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━─────   84%
 ○ codex · GPT 5.5 · low
   —
   ▣ ──────────────────────────────────    0%

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ? 1
 ? deploy.sh
   Deploy staging?

                 ? for help        ␣ next ?!
```

The cockpit make-up is the first thing the eye lands on: `? 2   ! 1`, two waiting and one failed, summed across every worktree, counting even rows hidden by a per-worktree cap. Ranking does the triage automatically: waiting and failed rows rise first, unread rows break ties inside the same status, idle agents and process rows settle below, and each worktree caps that tail with a dim `+K more` while keeping active, blocked, paused, finished, and focused rows on screen.

The power move is going straight to the blocked pane. A single session-scoped keystroke (`␣`, shown as "next ?!" in the footer) focuses the next item that needs attention, in ranking order, without the reader ever focusing the sidebar. Twelve agents, one key, straight to the oldest blocked one; press it again for the next. Seeing the blocked pane and getting to it are different actions, and this key collapses them, so triage cost stays flat as the fleet grows. It is bound only inside the Rimz session, so the reader's global mux config is untouched.

The grouping matches the reader's mental model. Groups are keyed on worktree isolation, since only same-worktree agents share files: a header marks each one, and the worktree the reader has selected reads as one bracketed lane, a thin spine down its full height with a faint dotted seal capping its header and the selected card inside it bolder. The `external` catch-all holds scripts, CI, and panes outside any worktree; it renders as a dim `┄ external ┄` divider and sorts last unless it holds something waiting or failed. The room scales past one repo too: `rimz start` in `~/code`, or on a headless box with no source control, makes that directory the room, where each child repo is a pod with its own branch and churn and the same cockpit, ranking, and jump triage the whole machine ([the fleet room](./product.md#many-repos-one-room)).

The footer advertises `?`, and pressing it overlays the legend and keys, so the glyph vocabulary is learnable in place without leaving the room.

```
 keys & legend
 ↑/↓ select   1-9 jump   ↵ jump
 ␣ next ?!   ←/→ provider tab
 x dismiss   r reload   ? close
 ⢿ working   ⢄ thinking   ? waiting
 ! attention   ○ idle   ✓ done
```

Every status is legible under `NO_COLOR` and to color-blind readers because the glyph shape carries the meaning; color is a second, redundant channel, and the legend shows both. Twelve agents on one line is the attention-at-a-glance design [DESIGN.md](../../DESIGN.md#attention-at-a-glance) commits to, carrying a real fleet.

## Detach and reattach from anywhere

Opening a new tab or window and starting a fifth agent there changes the roster, not the layout. Every tab is born with its own sidebar pane, and all of them render the same room-wide snapshot, so the column is identical everywhere and selecting any row jumps to that agent's pane wherever it lives in the session. The sidebar's own pane is chrome, excluded from the roster, and it self-closes when the last working pane in its tab exits, so a lone sidebar never lingers. Tabs are viewports; worktrees are the subdivision.

Then the reader closes the laptop with the mux's own detach key (Zellij `Ctrl-O d`, tmux `prefix d`). The room keeps running headless on the host, and the ledger keeps queuing events while nobody renders. Hours later they reattach from a tablet on the train.

```
$ ssh dev-box rimz attach query-engine
   reconstructing query-engine from ledger…
```

The same reattach has a first-class form: `rimz remote connect dev-box:query-engine` builds the guarded SSH and reconnects itself when the train wifi drops, and `rimz remote connect dev-box:~/code/query-engine` starts the room if it is not up yet. Named aliases live in `~/.config/rimz/remote.toml`, so `rimz remote connect prod` points at the same host without retyping it.

The sidebar comes back exactly as the reader left it: every agent where it was, every question still waiting, ranked identically, plus whatever finished while they were gone, already triaged by the same ranking. The first usable frame paints from the ledger immediately, since a resize or attach is itself a wakeup, so reattach reconstructs from durable state with no loading screen. This is what changes how the reader works: start a run on the dev box, close everything, and pick it up on a phone at the airport. Continuity is ledger-owned ([DESIGN.md → Commitments](../../DESIGN.md#commitments)); the running processes are the host's job (systemd, tmux-resurrect, Zellij resurrect), which [DESIGN.md → Non-goals](../../DESIGN.md#non-goals) states plainly rather than over-promising.

## When something is wrong

The honesty commitment gets tested when a fetch fails: the binary moved, the ledger directory vanished mid-write, a snapshot is half-written. The reader has to be able to tell a stale frame from a current one at a glance.

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
 failed: ledger not found
```

The loop keeps the last good snapshot for the body but pins a sticky banner to the bottom edge, status-bar style, so the body truncates before the banner ever clips, and the banner explains why the UI is not updating and for how long. When a fetch finally succeeds the banner steps down to a dim `⚠ last alert 8s ago: … · x dismiss` notice, so a failure that flickered past stays visible, and it clears for good when the reader presses `x` (a fresh failure re-arms it). The footer steps aside while the alert is active, because an empty body under a failed fetch is a missing snapshot, not an empty room. A tool that says "I am degraded, here is why, and here is for how long" earns more trust than one that quietly keeps showing old data.

The same honesty extends to trust and protocol. An untrusted `.rimz/config.toml` keeps its command-running fields inert until the reader runs `rimz trust grant` after reviewing the diff, and a sidebar whose protocol version drifts after an upgrade gets a `rimz doctor` mismatch report instead of a rail that silently stops updating. Banners, the trust state, and `rimz doctor` are the three places Rimz tells the reader what it cannot currently vouch for.

## Growing into it: resolvers, then everything else

By now the reader is hooked on the observe-and-route loop, and the product grows with them along paths they discover when they need them, each an addition to the same feed seen from a new angle.

Resolvers are the morning-after upgrade. Tired of approving `cargo check` for the eighth time, the reader enrols a resolver once: one of the two that ship ready-made (`hook_bridge_resolver.py` for routine permissions, `pane_send_resolver.py` for well-known terminal prompts), or a small process of their own wrapping a smarter model, with `rimz resolver add opus-policy --order 10 --budget 30s --binary …`. Now routine answers happen ahead of them and the hard ones abstain back to their pane exactly as before. The framing that keeps it safe is that the reader was already the answerer in every section up to here; the resolver just slots ahead of them, and the chain always ends with them. Deeper chains, Slack then PagerDuty, follow the same shape. This is what lets a fleet keep working while the reader sleeps. Mechanics are in [resolvers.md](../internals/agents/resolvers.md).

Unattended runs are the same idea with no human at the end of the chain. Launch agents with their own bypass flag for a straight-through run, or enrol a permissive resolver for a real per-decision audit trail in the ledger; the two compose, with the resolver handling routine cases and the bypass flag as the ultimate fallback. `rimz run "<prompt>"` makes the whole shape scriptable: a cron job launches a supervised turn and blocks for the final message, streams progress when a supervisor wants NDJSON, and leaves a visible pane that `rimz run send` or a human can nudge when the agent asks. The detail is in [product.md → Put your pipeline on the feed](./product.md#put-your-pipeline-on-the-feed).

And because agents and scripts share one CLI, a deploy or migration script can post to the same sidebar and block on its own question, answerable straight from the column. Once the reader is living in the room, that capability is there the day a script needs it ([the pipeline scenario](./product.md#put-your-pipeline-on-the-feed)).

## The experience in one screen

| Section | Reader does | Sees | Feels |
| --- | --- | --- | --- |
| First run | installs, consents, lands in the room | additive-diff gate, then `○ zsh` and a hint | reassured, then oriented |
| The agent shows up | types `claude`, then prompts it | row re-skins to `○ claude`, then `⢿ running` | delight, then calm |
| The question reaches you | gets notified, jumps to the pane | `? waiting`, an OS notification | the pitch lands |
| The fleet | hits "next ?!" | grouped roster, `? 2  ! 1` | in control |
| Detach, reattach | closes laptop, ssh back | the column reconstructed from the ledger | relief, then trust |
| When wrong | hits a failure | a labeled degraded banner | trust through honesty |
| Growing into it | enrols a resolver | the chain working the row | leverage |

The arc runs from curiosity to reassurance to delight to the pitch landing to mastery to trust. If the question moment does not land inside five minutes, nothing after it matters; every earlier section exists to get the reader there with their guard down and their agents already on screen.

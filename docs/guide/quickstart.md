# Quickstart

Your first session, from install to a working fleet: what you type, what appears on screen, and why.

Rimz makes one promise, and this walk tests it end to end: it names the agent that needs you and takes you straight to its pane, where you answer in the agent's own UI. Everything runs inside the Zellij or tmux you already use, with your keybinds and the official apps untouched. The frames below carry illustrative values; the [interface reference](../interface/sidebar.md) draws every glyph and meter exactly.

## Install, then one command

```sh
cargo install --locked rimz     # or: brew tap rimio/homebrew-rimz && brew install rimz

cd ~/code/query-engine          # any project you already have
rimz
```

Rimz needs Zellij (0.44+) or tmux (3.5+) on the machine; `rimz doctor` confirms your build clears the floor, and [installation.md](./installation.md) covers every install path and building from source.

Run `rimz` from a plain terminal. The room is its own Zellij or tmux session, so it cannot start from inside a session you are already attached to: if you live in a multiplexer, open a fresh terminal window outside it first. Running `rimz` inside one refuses with the room's name and the same advice.

The first run detects your multiplexer and your agents, writes the per-machine config set under `~/.config/rimz/` (`config.toml`, `theme.toml`, `agents.toml`, `loop.toml`, `remote.toml`, every key commented with its default), and asks only what it cannot detect. No account, no daemon, no hand-written config stands between you and the first frame.

## The consent gate

One question comes before the room: showing what an agent is doing means adding reporting hooks to the agent's own config, and Rimz never edits your agent config without asking. The same first-run flow carries the appearance probe and the pet opt-in, so the whole exchange reads top to bottom:

```
rimz · first-run setup
────────────────────────────────────────────────

Rimz found 2 coding agents: claude, codex.
To show them live in the sidebar, it adds reporting hooks to each agent's config.

  claude  13 hooks → ~/.claude/settings.json  existing kept
          + wraps your statusline for live context — yours restored on uninstall
  codex   10 hooks → ~/.codex/config.toml     new file

Each hook is one `rimz hooks feed` line — it reports events, never acts or answers for you.
  undo     rimz hooks uninstall
  preview  rimz hooks install --dry-run

Add reporting hooks? [Y/n]

✓ claude  13 hooks → ~/.claude/settings.json
✓ codex  10 hooks → ~/.codex/config.toml
All set — your agents appear in the sidebar as they run.

  ▐▐▐▐▐▐▐▐▐▐▐▐  (smooth color gradient)
                  (distinct icons)

  Icons and gradient render cleanly? [y/N] y
  Want a pet? It lives in the sidebar and reacts to your fleet. [y/N] y
✓ modern style: truecolor + Nerd Font icons
✓ rocky joins the room (rimz list-pets: more)
Next → docs/guide/setup.md · rimz config for preferences
Opening the room...
```

The gate is designed to answer the reasonable fears before you can ask them: the change is additive and names the exact config path, the `preview` line prints the full patch before you consent, and each hook is a `rimz hooks feed` line that reports events while answering a prompt stays with you. Enter wires every listed agent; `n` or EOF installs nothing, and an unwired agent still shows up as a plain process row. Backing out is the mirror of opting in — the footer's `rimz hooks uninstall` removes exactly what the gate added and restores your statusline. The authoritative hook set and the safety model are in [security.md](./security.md) and [agent.md → Hook install](../internals/agents/model.md#hook-install-the-visible-security-step).

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

On Zellij, Rimz loads a small presence plugin that reports pane topology; its permission grant is seeded automatically, so no extra prompt appears. If pane discovery ever stops, `rimz doctor` names the fix ([security.md → The Zellij presence plugin](./security.md#the-zellij-presence-plugin)).

## Your first agent

Type `claude` in the shell pane. Within a second or two the row that read `○ zsh` becomes the agent's card, the same row re-skinned, not a second one:

```
 ? 0   ! 0   ⏸ 0   ✓ 0                 ⢿ 0   ○ 1

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌○ claude · Opus 4.8 · xhigh
```

The session-start hook fired, Rimz's durable on-disk record — the store — overlaid identity onto the pane, and the row updated: your agent is in the sidebar, correctly named, with its model and effort. No config, no flag, no restart. An idle agent isn't asking for anything, so it fills no attention bucket.

Give Claude a task and the card fills out: the leading glyph animates (`⠁` while it reasons, `⣾` once it edits), the description line carries what it is on, and the context meter starts filling:

```
 ? 0   ! 0   ⏸ 0   ✓ 0                 ⢿ 1   ○ 0

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌⣾ claude · Opus 4.8 · xhigh · 200k         $0.42
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━──────────────────────── 41.2%
```

A running agent needs nothing from you: the `?` and `!` buckets hold at zero, so you get coffee or open a second agent. A running agent that goes silent past the stall window (30 minutes by default) escalates to `!` on its own, so a wedged run cannot hide behind a spinner. How the column ranks and lives is [the sidebar guide](./sidebar.md).

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

Select the row, click the notification, or hit the triage key from the next section, and you land in Claude's pane reading the actual prompt: the real command Claude wants to run, approved or denied in Claude's own UI. You were heads-down somewhere else, and Rimz tapped you on the shoulder with exactly the right pane, one keystroke away.

The store is authoritative, so notifications are best-effort polish: a missed one loses nothing, several waiting at once coalesce into one, and an agent that stays waiting earns a single nudge rather than a stream. That same waiting state is the hook automation hangs on later — a notification handler can answer a routine prompt right in the agent's pane, and anything it skips stays `? waiting` and still routes to you ([loops.md](./loops.md)).

## A fleet, and the two keys that tame it

Now the load the product was built for: two more agents in a second worktree, plus a deploy script in a pane of its own. This walk typed each agent into its own pane to show the mechanics; `rimz agents claude,codex --worktree=feature-migration` opens the pair in one command; [agents.md](./agents.md) covers layouts, [worktrees](./worktrees.md) covers the isolation, and [teams](./teams.md) puts models in named roles. Either way, the column stays scannable.

```
 ⌘ query-engine

 ◎ 8                      ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 5 (2)                                    $4.20
 ─────────────────────────────────────────────────
 ? 1   ! 1   ⏸ 0   ✓ 0                 ⢿ 2   ○ 1

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus 4.8 · xhigh · 200k         $1.27
▌  ⋮  the three cards from above, unchanged

 ⑂ feature-migration ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ +230 -23
 ! claude · Opus 4.8 · xhigh                $2.05
   db migrate
   ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━───── 84.1%
 ○ codex · GPT 5.5 · low

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
 ○ deploy.sh

                                       ? for help
```

The cockpit line is where the eye lands first: `? 1   ! 1`, one waiting and one failed, summed across every worktree. Ranking does the triage — waiting and failed rows rise first, unread rows break ties, idle agents and process rows settle below — and exactly one row is ever in motion, the oldest one that needs you, so the eye lands on where to go next.

Two keys collapse seeing the blocked pane and getting to it into one motion:

- `Alt+p` reaches the sidebar from any pane in the room, bound only inside the Rimz session, so your global mux config is untouched.
- `Space` (or `n`) jumps to the next item that needs you, in ranking order, and focuses its pane; press it again for the next, `N` steps back.

Five agents, two keys, straight to the oldest blocked one; the same two keys hold at fifty. Clicking works everywhere too, and `?` opens the keys-and-filter overlay in place. Why the column groups by worktree and how the ranking decides is [the sidebar guide](./sidebar.md).

## Leave, and come back from anywhere

Detach with your multiplexer's own key (Zellij `Ctrl-O d`, tmux `prefix d`); the room keeps running headless, agents working. Reattach on the same machine with `rimz` in the project directory, or from any other machine over SSH:

```sh
rimz remote connect dev-box:~/code/query-engine
```

The link reconnects itself when the train wifi drops, and the sidebar comes back exactly as you left it — every agent where it was, every question still waiting, plus whatever finished while you were gone. Aliases and link health are [remote](./remote.md); opening a room in a browser is [web](./web.md).

## Where to go next

- [Set up your machine](./setup.md) — the one-time pass that makes Rimz a daily driver: true color and Nerd Font glyphs, pets, the hands-off loop settings, and a Zellij/tmux baseline.
- [Agents](./agents.md) — launch agents by name and compose several into one layout.
- [Worktrees](./worktrees.md) — isolate a layout or team on its own branch for parallel work.
- [Teams](./teams.md) — pair models by role and launch the whole set on one feature.
- [Messaging](./messaging.md) — steer and queue agents by handle, and let them talk to each other.
- [The sidebar](./sidebar.md) — reading the column, the agent lifecycle, and how the ranking decides what needs you.
- [Loops and schedules](./loops.md) — automate the waiting state so the fleet only needs you for real decisions.
- [Remote](./remote.md) — connect to a room on a server over a self-healing link.
- [Web](./web.md) — open a room in the browser, on the host or tunnelled from a server.

# The sidebar, on screen

> What the sidebar *looks like*, section by section, with the real frames it draws. For how it is built — presence model, launch, reload recovery, the view-model — see [docs/internals/sidebar.md](../internals/sidebar.md). For the design rationale behind the glyph law, see [DESIGN.md → Attention at a glance](../../DESIGN.md#attention-at-a-glance).

The sidebar is one narrow column that answers a single question: **which pane needs you, right now.** Every pane in the room is a row; agents are enriched from the ledger; everything groups by the worktree it lives in. It routes you to the pane — you read and answer in the agent's own UI.

It has three reading zones, top to bottom: the **cockpit** (who is here, what it costs, what needs you), the **agent cards** (one card per pane, grouped by worktree), and the **provider dashboard** (your accounts and their budgets). Bottom chrome — a footer and a health line — pins under all three.

Every frame below is what the renderer actually paints. The structure, glyphs, columns, and alignment are exact; the live values (ages, percentages, resets, counts) are illustrative. Color is a muted 256-color palette by default — `NO_COLOR` drops the color but keeps every shape, so each meter still reads by its fill. The frames here are colorless text, so they show exactly the `NO_COLOR` reading.

## The whole frame at a glance

A real room: one Claude agent working in `main`, its card selected, with the per-provider dashboard pinned at the bottom.

```
 ⌘ query-engine                                          ← workspace identity
 ✦ 1                                            $1.27    ← head-count · fleet spend
 ────────────────────────────────────────────────────
 ? 0   ! 0   ○ 0                      ✽ 0   ⢿ 1   ✓ 0    ← make-up: who needs you | who's busy
 ◷ 12m · ◇ 76.5k                                         ← fleet totals: time · tokens

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄    ← the worktree you're in (lane spine ▏)
▌⣾ claude · Opus 4.8 (1M) · high · auto         $1.27    ← line 1: identity · capability · $cost
▌  ledger refactor                                       ← line 2: what it's on
▌  ▣ ━━━━━━━━━━━━━━━━────────────────────────── 38.2%    ← context meter: how full the window is
▌  ◇ 76.5k ↘ 64.2k ↗ 12.3k ◌ 68.0k                       ← tokens (selected/full only)
▌  ◷ 12m worked · +214 -31                         1m    ← time worked · diff · last activity

 ────────────────────────────────────────────────────
 Claude Code v2.1.158 · Claude Max               ⇅ rc    ← provider · plan · remote-control flag
  ▐▛███▜▌  $3.50 · ◇ 486.0k                               ← brand emblem · account spend · tokens
 ▝▜█████▛▘ 5h ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱ ↻ 2h06m    ← 5-hour budget left, until reset
   ▘▘ ▝▝   7d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱ ↻ 1d02h    ← 7-day budget left, until reset

 Codex v0.135.0 · ChatGPT Pro
  ▐▛███▜▌  $1.20 · ◇ 88.0k
 ▝▜█████▛▘ ∞  ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱             ← API-key account: no meter to spend
   ▘▘ ▝▝  
                      ? for help                         ← footer (pinned to the bottom edge)
```

The rest of this doc reads that frame zone by zone.

## Reading the glyphs

One vocabulary runs through the whole sidebar: a shape carries the meaning, color reinforces it. This is the complete legend, and the canonical home for it — every other doc points here, and the `?` overlay inside the app shows a short version in place.

**Status — the leading cell of every agent row.**

| glyph | state | meaning | needs you |
|-------|-------|---------|-----------|
| `?`   | waiting    | the agent asked something; answer in its pane | yes — yellow, reddens when ignored |
| `!`   | attention  | a failed turn, or a working agent gone silent past the stall window | yes — yellow, reddens when ignored |
| `⏸`   | rate-limited | resting on an account whose rate-limit window is spent — parked until it resets, resumes with a `continue` | waiting on the reset — held amber, never reddens |
| `⢿`   | working    | running and editing — animates `⣾⣽⣻⢿⡿⣟⣯⣷` in clay | no |
| `✽`   | thinking   | running in read-only plan mode — sparkles `· ✢ ✳ ✶ ✻ ✽` in clay | no |
| `⠙`   | resolving  | a resolver is answering on the bridge — braille spin | pending, being handled |
| `○`   | idle       | alive, nothing to do | no |
| `✓`   | done       | finished cleanly | no |
| `○`/`⢿` dim | process | a pane with no agent (shell, editor); idle shows the hollow `○`, real work the `⢿` spinner — both in the dim process tone | no |

Two short-lived heads ride over the base status on the leading cell, so they never earn a cockpit bucket of their own:

| head | meaning |
|------|---------|
| `▇` compacting | condensing its context window — pulses `▁▃▄▅▆▇▆▅▄▃` in violet, then returns to its resting head |
| `´` waiting on subagents | the main agent delegated to its children; the work is in the rows below — a low clay wave (`_` bobbing up to `´` and back) |

Only live states move. The two actionable attention glyphs (`?` / `!`) hold still and **redden from yellow to red** once a row sits unanswered past the neglect window (`[sidebar] attention_redden_secs`) — a fresh ask reads calm-urgent, a long-ignored one heats up. A working agent that goes silent past the stall window escalates to a static `!` instead of spinning on. The `⏸` rate-limited head is attention-class but parked: it holds still in a held amber and never reddens, since waiting for the reset is the only move. A parent waiting on subagents is exempt from the stall escalation — its quiet wave is the children's work, not a wedge.

**Posture — the permission pill, sized by blast radius.**

| pill | meaning |
|------|---------|
| *(none)* | `default` — the baseline, omitted |
| `plan`   | read-only — calm blue |
| `auto`   | edits inside the sandbox — amber |
| `yolo`   | bypasses every gate — bold red (loud even when the rest dims) |

**Meters and stats — one grammar everywhere.**

| token | reads as |
|-------|----------|
| `▣ ━━━━──── 38.2%` | context meter — how full the window is (blue → amber → red), bar fills as used |
| `◇` `↘` `↗` `◌`   | tokens: total · input · output · cached |
| `◷ 12m worked`    | time the agent has worked |
| `+127 -43`        | lines added / removed |
| `●●●○○ 3/5`       | todo progress |
| `$1.27`           | spend — money-green, always two decimals |
| `▰▱` / `∞`        | provider budget bar (fill = left) / unmetered account |
| `↻ 2h06m`         | when that budget resets |

**Structure and chrome.**

| mark | meaning |
|------|---------|
| `⌘ name`        | the workspace |
| `✦ N` / `✧ M`   | main agents you launched / subagents they spawned |
| `▏`             | the selection lane — the worktree you're in |
| `▌`             | the selected card |
| `┄ external ┄`  | out-of-project panes (scripts, CI, stray shells) |
| `─`             | a section hairline |
| `⇅ rc`          | remote control is on for that provider |

## Zone 1 — the cockpit

The top block. Fixed height, so the rows below it never jump as agents change state. Top to bottom it answers: *whose room is this, what's it costing, who needs me, and what has the fleet done.*

```
 ⌘ query-engine                            ~/code/query-engine
 ✦ 5   ✧ 2                                            $4.20
 ──────────────────────────────────────────────────────────
 ? 2   ! 1   ○ 1                        ✽ 1   ⢿ 1   ✓ 0
 ◷ 41m · ◇ 486.0k · ◆ 4
```

- **Identity.** The workspace name behind `⌘`, with the project path dim on the right edge (home-abbreviated to `~/…`; it left-truncates with a leading `…` before it ever crowds the name).
- **Head-count and spend.** `✦` is the agents you launched; `✧` (only when present) is the subagents they spawned this turn. The fleet's total spend pins right. An empty room reads `✦ 0` with no spend.
- **The make-up — split by who might want you.** The **left cluster** is worth a glance: `?` waiting and `!` failed (yellow, reddening when any of their rows goes stale), a `⏸` rate-limited count right after them (held amber, parked), then a free `○` idle agent grouped at its right edge — calm, but a free agent wants work. The **right cluster** is the busy tail: `✽` thinking before `⢿` working (you read a plan before it acts), then `✓` done. The fixed buckets always show, so a zero reads a faint `? 0` and the line is scannable by position; the `⏸` bucket is the one exception — a rare, non-actionable state, it appears only when an agent is actually parked, so it never costs width on a narrow rail.
- **Fleet totals.** Time worked behind a teal `◷`, the `◇` token total, and `◆` commits ahead of trunk. A field that no agent reported is dropped.

An **empty room** has no make-up line at all — just identity and the `✦ 0` head-count:

```
 ⌘ query-engine
 ✦ 0
 ──────────────────────────────────────────────────────────

 no agents yet
 install hooks:
 rimz hooks install claude
```

The first-run hint names the real next step: an un-wired room points at `rimz hooks install`, a wired one reads `run claude or codex / in a pane to begin`. It clears the instant the first agent or pane appears.

## Zone 2 — the agent cards

The body: one card per pane, grouped under the worktree it lives in. A worktree is total isolation — only same-worktree agents collaborate — so each group reads as one bounded block.

### The card

An agent is a small stacked card. The resting card is three lines; selecting it (or running `full` density) appends the deeper stats. Selection never reshapes a line already on screen — it only *appends* and lights the spine — so the card never reflows as it expands.

```
▌⣾ claude · Opus 4.8 (1M) · high · auto         $1.27    line 1 — glyph · name · model · effort · posture, $cost right
▌  ledger refactor                          ●●●○○ 3/5    line 2 — what it's on; todo pins right when wide
▌  ▣ ━━━━━━━━━━━━━━━━────────────────────────── 38.2%    context meter — the resting card's one bar
▌  ◇ 76.5k ↘ 64.2k ↗ 12.3k ◌ 68.0k                       selected/full — token breakdown
▌  ◷ 12m worked · +214 -31                         1m    selected/full — time worked · diff · last-activity age
```

- **Line 1 — identity.** The animated leading cell, then the agent kind (dim — the glyph and its color carry identity), then capability: model, effort, and the posture pill. Capability degrades by width — a wide card carries model · effort · posture, a medium one drops effort, a narrow one keeps just the name. The session `$cost` pins right.
- **Line 2 — what it's on.** The session name you gave it (`--name` / `/rename`), else the agent's task, else its latest prompt (which lingers once the turn ends and the task clears, so an unnamed session stays labelled until it earns a name), else an em dash. On a wide card, todo dots pin right.
- **The context meter (`▣`).** The resting card's one bar. The bar fills as the window is *used*; its value is always the percent used. While the window is calm it splits into colored segments showing *where* the tokens went (cache writes / cache reads / fresh input); once it warns it goes a solid amber/red. The `▣` glyph reads *how full* the window is on its own ramp — blue while cold, warming as it fills.
- **Selected / full lines.** The token breakdown (`◇` total · `↘` input · `↗` output · `◌` cached) and the work line (`◷` time worked · the agent's own diff · a coarse last-activity age pinned right). These are the only place an age appears — the resting card stays calm.

The `▣`, `◇`, and `◷` glyphs share one lead column, so the card reads as an aligned grid.

**Density and selection.** `[sidebar] density` sets the resting height: `compact` (default) shows the three top lines, `full` adds the token and work lines. Selecting any row always reveals the full five-line card, so the deepest data is one keystroke away:

```
   resting (compact)          selected — only appends, never reshapes
   ─────────────────          ────────────────────────────────────────
    ⣾ claude · Opus · auto    ▌⣾ claude · Opus · auto            $1.27
      ledger refactor         ▌  ledger refactor
      ▣ ━━━━━━━──── 38.2%     ▌  ▣ ━━━━━━━──── 38.2%
                              ▌  ◇ 76.5k ↘ 64.2k ↗ 12.3k ◌ 68.0k   ← appended
                              ▌  ◷ 12m worked · +214 -31       1m   ← appended
```

The expanded card also lists any **subagents** the agent spawned this turn — a dim `subagents (N)` header then one indented `glyph type` line each (`⢿ Explore`, `○ review`). Subagents have no pane of their own, so they never get a row; they nest here only.

### Attention rows

A waiting or failed agent is the whole point. Its glyph leads, bold, and the card rises to the top of its worktree:

```
▏feature-migration ┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌! claude · Opus · yolo
▌  db migrate
▌  ▣ ──────────────────────    0%
```

A `?` waiting row reads the same with a `?` glyph. The row carries *who* needs you and *what task*, and selecting it lands you in the agent's pane, where the full prompt and its safe defaults live — that is the row's job, to route you there. A script's `feed ask` is the one item answerable from the sidebar itself: it chose Rimz as its surface, so its declared options render as buttons on the row.

**A resolver answering** replaces the `?` with a braille spinner and fills the task slot with the resolver name and its remaining budget — the item is pending, just being handled. It flips back to `? waiting` if the chain exhausts:

```
? claude  fix auth flow  1m      →      ⠙ claude  opus-policy 24s   1m
```

### Process rows

A pane no agent has stamped reads in the dim process tone — a hollow `○` (the same idle glyph an agent shows, never the agent's clay) for an idle shell or editor, the `⢿` spinner for a pane doing real work like a build, test, or install. An active pane anchors its primary line on the shell that owns it, so the line stays put as commands come and go, and carries the live command in full on a dim second line:

```
○ zsh
⢿ zsh
    sudo npm install -g @openai/codex
```

The label is the program the pane runs, read past a `sudo` wrapper and through a `node`/`npx` launcher (`sudo npm install -g @openai/codex` is an `npm` install, not a codex agent; `node …/codex` is codex). No status, no meter, never counted in the cockpit — it is presence, not a cue. It is still a jump target, and the moment an agent's hook stamps that pane it becomes that agent's card.

### Worktree groups and the selection lane

Worktrees stack as bounded blocks. The one **holding your selection** reads as a single bracketed lane: a thin `▏` spine down its header and every row, a dotted `┄` seal on its header, and the selected card itself lit with a bold `▌`. Every other worktree carries a blank gutter, so the lane is the only marker on screen and the selection is unmistakable. The worktree header carries the worktree's total diff against trunk (`+230 -23`) on the right.

```
▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄    ← selected worktree: lane spine + dotted seal
▌? claude
▌  permission
▌  ▣ ──────────────────────────    0%
▏⣾ codex · GPT-5.5
▏  add tests
▏  ▣ ──────────────────────────    0%

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ? 1   ← out-of-project panes, attention-only tally
 ? deploy.sh
   Deploy staging?
```

The `external` block is the catch-all for panes outside the project — untethered scripts, CI, stray shells. It renders as a dim divider rather than a worktree header, sorts last unless it holds something waiting, and keeps an attention-only `? n` / `! n` tally so an out-of-project ask still surfaces.

**Ranking is automatic: the most attention-hungry rises, nothing else moves.** Within a worktree, rows sort `waiting → failed → idle → done → working` (a working agent is the least needy, so it settles to the bottom). Attention rows sort oldest-first, so the longest-overdue is always on top. Worktrees themselves sort by their most-urgent member.

**The cap.** Each worktree shows a capped number of rows (configurable) with a dim `+K more`. The cap trims only the calm tail; every `waiting`/`failed` row is always shown, so the cap can never hide something that needs you:

```
▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌⣾ codex
▌  task-0
▌  ▣ ────────────────────────    0%
▏⣾ codex
▏  task-1
▏  ▣ ────────────────────────    0%
▏  +3 more
```

### Jump — the row is the link

You don't read where to go; you go. Selecting a row focuses that pane — no mux pane number is ever shown.

- `↑/↓` select a row, `↵` jump to it.
- `1`–`9` jump by the row's visible position.
- `␣` jump to the **next thing that needs you** — the oldest waiting/failed row, without selecting first. One key tames a fleet; press again for the next.
- A click anywhere in a card's block jumps to it.

## Zone 3 — the provider dashboard

The budgets are account-scoped — every session of a provider shares one account's budget — so they leave the rows for a pinned panel at the bottom. One block per provider, including any account that is logged in but idle this run, so your accounts and budgets show even between turns. Each provider shows one bar per window it reports — both Claude and Codex a 5-hour and a 7-day — so the dashboard tracks whatever windows a provider exposes. Codex budgets are pulled out-of-band from the app-server: active sessions refresh during long turns, and a logged-in idle account refreshes from the shared cache path.

A **metered account** drains one "mana" bar per budget window toward its reset. The bar fills with what's *left*, ramping green → amber → red as it empties — and a fully-spent window (0% left) flips its whole empty track red, so an exhausted budget never reads as an untouched one. Each window's label (`5h`/`7d`) wears its own bar's color, so the row reads as one unit. A spent longer window gates the shorter ones: once the `7d` is exhausted the `5h` row is painted exhausted too — red, no countdown — regardless of its own reading, since that budget is unusable until the longer window resets.

These are **sliding windows** that begin counting only on your first token, so until then the provider keeps sliding the reset a full window-length ahead. A window whose reset still sits ~a full window out has **not started** (it still reads ~1% used, not 0 — so it's the reset distance that gives it away). Any usage above that ~1% floor means it has already started, countdown and all; only a 0–1% window with a near-full reset qualifies. A not-started window shows a near-full bar with **no countdown**, reading "ready — send a message to start it" rather than a misleading ticking placeholder; the countdown appears once your first token fixes the reset and it begins ticking down.

```
 Claude Code v2.1.158 · Claude Max               ⇅ rc    header — product · version · plan, rc flag right
  ▐▛███▜▌  $3.50 · ◇ 486.0k                               emblem + account spend · tokens
 ▝▜█████▛▘ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱ ↻ 2h06m    5-hour window left, until reset
   ▘▘ ▝▝   7d  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱ ↻ 1d02h    7-day window, same start/end column
```

The dashboard isn't pinned to a fixed set of windows. If a provider reports a single window — as Codex briefly did during a server-side bug that widened its window to ~30 days — it paints one bar, labeled by its length, instead of misrendering:

```
 Codex v0.136.0 · ChatGPT Pro                            header — product · version · plan
  ▐▛███▜▌  $1.20 · ◇ 88.0k                                emblem + account spend · tokens
 ▝▜█████▛▘ 30d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱ ↻ 28d04h    a single window, labeled by length
```

An **unmetered (API-key) account** has no budget to drain, so it shows an `∞` bar — the icon in the front slot, an empty track, no countdown:

```
 Codex v0.135.0 · ChatGPT Pro
  ▐▛███▜▌  $1.20 · ◇ 88.0k
 ▝▜█████▛▘ ∞  ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱
   ▘▘ ▝▝  
```

Every bar across every block shares one start column and one end column, so the dashboard reads as one aligned grid. A blank line separates blocks. The `⇅ rc` flag pins to a block's top-right when remote control is on for that provider (Claude only — it's host infrastructure, never its own row). Below ~34 columns the emblem is dropped and the bars run full-width. The brand emblem, color, and name are config-driven (`[sidebar.providers.<kind>]`, see [configuration.md](../reference/configuration.md)).

## Bottom chrome

Pinned to the bottom edge, below all three zones. The body is truncated before this chrome is ever clipped, so it can never scroll off.

**Footer.** The faintest line. At rest it is just `? for help`; the triage key joins it only when something actually needs you, so the signature `␣` stays discoverable without shouting:

```
                      ? for help            ← nothing needs you
      ␣ next ?!   ? for help                ← at least one waiting/failed row
```

**Help overlay** (`?`). The legend and keys, in place:

```
 keys & legend
 ↑/↓ select   1-9 jump   ↵ jump
 ␣ next ?!   x dismiss   r reload   ? close
 ⢿ working   ✽ thinking   ? waiting
 ! attention   ○ idle   ✓ done   dim = process
 posture: plan · auto · yolo
```

**Health alert.** When the refresh loop can't read the room, a sticky line takes over the bottom and the footer steps aside — an empty body under a failed fetch is a missing snapshot, not an empty room:

```
 ! Sidebar degraded for 8s: snapshot failed: ledger not found
```

On recovery it doesn't vanish; it lingers as a dim, dismissable notice so a failure that flickered past is still visible after the fact:

```
 ⚠ last alert 8s ago: snapshot failed: ledger not found  ·  x dismiss
```

Press `x` to dismiss it; a fresh failure re-arms it. `r` reloads the tab.

## Keeping these frames honest

The renderer's golden tests in [`crates/rimz-sidebar/src/render/`](../../crates/rimz-sidebar/src/render/) are the machine-checked source of truth for these frames — `cargo xtask test` re-renders each scenario and diffs it against a committed `.snap`. The frames in this doc are drawn from those scenarios:

| scenario | golden test |
|----------|-------------|
| empty room, un-wired | `first_run_nudge` |
| empty room, wired | `first_run_nudge_wired` |
| narrow card | `l0_density_minimal_row` |
| capability + posture | `agent_capability` |
| selected, enriched card | `enriched_selected_agent_card` |
| worktree grouping + external | `worktree_attention_map` |
| per-worktree cap | `group_cap_with_overflow` |
| provider dashboard | `provider_dashboard` |
| health alert | `degraded_banner` |

When the renderer changes how something looks, update the `.snap` (the test prints the diff) and this doc together.

# The sidebar, on screen

> What the sidebar *looks like*, section by section, with the real frames it draws. For how it is built — presence model, launch, reload recovery, the view-model — see [docs/internals/sidebar.md](../internals/sidebar.md). For the design rationale behind the glyph law, see [DESIGN.md → Attention at a glance](../../DESIGN.md#attention-at-a-glance).

The sidebar is one narrow column that answers a single question: **which pane needs you, right now.** Every pane in the room is a row; agents are enriched from the ledger; everything groups by the worktree it lives in. It routes you to the pane — you read and answer in the agent's own UI.

It has three reading zones, top to bottom: the **cockpit** (who is here, what it costs, what needs you), the **agent cards** (one card per pane, grouped by worktree), and the **provider dashboard** (your accounts and their budgets). Bottom chrome — a footer and a health line — pins under all three.

Every frame below is what the renderer actually paints. The structure, glyphs, columns, and alignment are exact; the live values (ages, percentages, resets, counts) are illustrative. Color is a muted 256-color palette by default — `NO_COLOR` drops the color but keeps every shape, so each meter still reads by its fill. The frames here are colorless text, so they show exactly the `NO_COLOR` reading.

## The whole frame at a glance

A real room: one Claude agent working in `main`, its card selected, with the per-provider dashboard pinned at the bottom.

```
 ⌘ query-engine                                          ← workspace identity
                                                         ← blank line below the name
 ¤ 1                   ◇ 76k ↘ 12k ↗ 64k ◍ 20k ◌ 68k    ← live agents · today's tokens (right)
 ◎ 12                                            $4.20    ← sessions today · today's fleet spend
 ────────────────────────────────────────────────────
 ? 0   ! 0   ○ 0   ⏸ 0                      ⢿ 1   ✓ 0    ← make-up: who needs you | who's busy

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄    ← the worktree you're in (lane spine ▏)
▌⣾ claude · Opus 4.8 · high · 1M                $1.27    ← line 1: identity · capability · $cost
▌  ledger refactor                                       ← line 2: what it's on
▌  ▣ ━━━━━━━━━━━━━━━━────────────────────────── 38.2%    ← context meter: how full the window is
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                 ◷ 1m    ← context line: filled window · last-activity age

 ────────────────────────────────────────────────────
 Claude v2.1.158 · Claude Max                    ⇅ rc    ← provider · plan · remote-control flag
                                                         ← blank line below the name
  ▐▛███▜▌  ◇ 486k ↘ 64k ↗ 422k ◍ 12k ◌ 68k      $3.50    ← brand emblem · today's tokens · spend
 ▝▜█████▛▘ 5h ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱ ↻ 2h06m    ← 5-hour budget left, until reset
   ▘▘ ▝▝   7d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱ ↻ 1d02h    ← 7-day budget left, until reset

 W: ◎  92  ◇ 16.5k ↘ 2.3k ↗ 14.2k ◌ 168k       $140.57  ← week ledger: sessions · tokens · spend
 M: ◎ 212  ◇ 76.5k ↘ 12.3k ↗ 64.2k ◌ 668k      $240.57  ← month ledger
                      ? for help                         ← footer (pinned to the bottom edge)
```

The rest of this doc reads that frame zone by zone.

## Reading the glyphs

One vocabulary runs through the whole sidebar: a shape carries the meaning, color reinforces it. This is the complete legend, and the canonical home for it — every other doc points here, and the `?` overlay inside the app shows a short version in place.

**Status — the leading cell of every agent row.**

| glyph | state | meaning | needs you |
|-------|-------|---------|-----------|
| `?`   | waiting    | the agent asked something; answer in its pane | yes — yellow, reddens when ignored |
| `!`   | attention  | a failed turn, a turn dead on a provider API error, or a working agent gone silent past the stall window | yes — yellow, reddens when ignored |
| `⏸`   | rate-limited | resting on an account whose rate-limit window is spent — parked until it resets, resumes with a `continue` | waiting on the reset — held amber, never reddens |
| `⢿`   | working    | running and editing — animates `⣾⣽⣻⢿⡿⣟⣯⣷` in clay | no |
| `✽`   | thinking   | running, before the turn's first file edit — sparkles `· ✢ ✳ ✶ ✻ ✽` in clay | no |
| `⠙`   | resolving  | a resolver is answering on the bridge — braille spin | pending, being handled |
| `○`   | idle       | alive, nothing to do | no |
| `✓`   | done       | finished cleanly | no |
| `○`/`⢿` dim | process | a pane with no agent (shell, editor); idle shows the hollow `○`, real work the `⢿` spinner — both in the dim process tone | no |

Three short-lived heads ride over the base status on the leading cell, so they never earn a cockpit bucket of their own:

| head | meaning |
|------|---------|
| `✽` thinking | the running turn before its first file edit — the agent is reasoning and reading, not yet writing; a research turn that never edits sparkles end to end |
| `▇` compacting | condensing its context window — pulses `▁▃▄▅▆▇▆▅▄▃` in violet, then returns to its resting head |
| `´` waiting on subagents | the main agent delegated to its children; the work is in the rows below — a low clay wave (`_` bobbing up to `´` and back) |

Every running agent — whichever head rides its cell — counts as **working** (`⢿`) in the cockpit make-up.

The two actionable attention glyphs (`?` / `!`) **breathe** — a slow brightness pulse that swells from dim to bold and fades back over ~2.4s, never blanking — to pull the eye back to an unanswered row, and they **redden from yellow to red** once a row sits unanswered past the neglect window (`[sidebar] attention_redden_secs`), so a fresh ask reads calm-urgent and a long-ignored one heats up. A working agent that goes silent past the stall window escalates to a breathing `!` instead of spinning on. The `⏸` rate-limited head is attention-class but parked: it holds still in a held amber and never reddens or breathes, since waiting for the reset is the only move. A parent waiting on subagents is exempt from the stall escalation — its quiet wave is the children's work, not a wedge. An idle agent with no prompt yet shows a gentle `.`→`..`→`...` loading cue on line 2 in place of the em dash, drifting on the same lazy ~2.4s cycle as the breath.

**Window — the model's context window on the identity line.**

A dim magnitude token (`258k`, `1M`) closing the capability cluster: the live out-of-band reading (Claude's statusline, Codex's app-server) when one exists, else the hook-derived window, omitted until a source names it.

**Meters and stats — one grammar everywhere.**

| token | reads as |
|-------|----------|
| `▣ ━━━━──── 38.2%` | context meter — how full the window is (blue → amber → red), bar fills as used; an empty 0% window reads the hollow `▢` |
| `▤ 76k`           | filled context — the absolute tokens in the window (the `▣` meter's numerator); leads the card's context line |
| `◇` `↘` `↗` `◍` `◌` | tokens: total · input · output · cache-write · cache-read |
| `◷ 1m`            | last-activity age — shown only once it crosses a full minute; its tone ramps as the provider prompt cache cools: dim while a resume still hits cache, amber from 20m idle, red from 1h — a red age means resuming likely re-reads the whole context uncached |
| `+127 -43`        | lines added / removed |
| `●●●○○ 3/5`       | todo progress |
| `$1.27`           | spend — money-green, always two decimals |
| `▰▱` / `∞`        | provider budget bar (fill = left) / unmetered account |
| `↻ 2h06m`         | when that budget resets |

**Structure and chrome.**

| mark | meaning |
|------|---------|
| `⌘ name`        | the workspace |
| `¤ N`           | the live agents in the room right now |
| `◎ N`           | sessions (threads) that have run today (cockpit) / in the window (ledger) |
| `⧉ N`           | the subagents an agent spawned this turn (expanded card) |
| `▏`             | the selection lane — the worktree you're in |
| `▌`             | the selected card |
| `┄ external ┄`  | out-of-project panes (scripts, CI, stray shells) |
| `─`             | a section hairline |
| `⇅ rc`          | remote control is on for that provider |

## Zone 1 — the cockpit

The top block. Fixed height, so the rows below it never jump as agents change state. Top to bottom it answers: *whose room is this, what's it costing, who needs me, and what has the fleet done.*

```
 ⌘ query-engine                            ~/code/query-engine

 ¤ 5                       ◇ 76k ↘ 12k ↗ 64k ◍ 12k ◌ 68k
 ◎ 12                                                 $4.20
 ──────────────────────────────────────────────────────────
 ? 2   ! 1   ○ 1   ⏸ 0                            ⢿ 2   ✓ 0
```

- **Identity.** The workspace name behind `⌘`, with the project path dim on the right edge (home-abbreviated to `~/…`; it left-truncates with a leading `…` before it ever crowds the name). A blank line sets it apart from the summary below.
- **Summary — who's here and what today burned.** Two lines, each a count on the left with today's numbers pinned right. Line 1: `¤` the live agents in the room right now, with today's accumulated token breakdown — `◇` total · `↘` input · `↗` output · `◍` cache-write · `◌` cache-read — pinned to the right edge in the coarse integer form (it drops when today recorded no tokens, leaving `¤ N` alone). Line 2: `◎` the sessions (threads) that have run today, with today's spend pinned right. The counts read from the live fleet and the JSONL `value_tally`'s today window. An empty room reads `¤ 0` over `◎ 0`.
- **Today's spend.** The fleet's total spend for today, pinned to the right of the sessions line, **counting up** in a smooth eased climb as a turn lands (with a brief brighten as it settles) — the cockpit's one animated number. It joins the line once today records spend.
- **The make-up — split by who might want you.** The **left cluster** is worth a glance: `?` waiting and `!` failed (yellow, reddening when any of their rows goes stale, and breathing), then a free `○` idle agent — calm, but a free agent wants work — and a `⏸` rate-limited count closing the cluster (held amber, parked). The **right cluster** is the busy tail: `⢿` working (every running agent — the thinking sparkle and the compaction pulse are per-row heads, not buckets), then `✓` done. Every bucket always shows, so a zero reads a faint `? 0` and the line is scannable by position.

An **empty room** has no make-up line at all — just identity and the `¤ 0` / `◎ 0` summary:

```
 ⌘ query-engine

 ¤ 0
 ◎ 0
 ──────────────────────────────────────────────────────────

 no agents yet
 install hooks:
 rimz hooks install claude
```

The first-run hint names the real next step: an un-wired room points at `rimz hooks install`, a wired one reads `run claude or codex / in a pane to begin`. It clears the instant the first agent or pane appears.

## Zone 2 — the agent cards

The body: one card per pane, grouped under the worktree it lives in. A worktree is total isolation — only same-worktree agents collaborate — so each group reads as one bounded block.

### The card

An agent is a small stacked card. The resting card is four lines; selecting it appends any subagents. Selection never reshapes a line already on screen — it only *appends* and lights the spine — so the card never reflows as it expands.

```
▌⣾ claude · Opus 4.8 · high · 1M                $1.27    line 1 — glyph · name · model · effort · window, $cost right
▌  ledger refactor                          ●●●○○ 3/5    line 2 — what it's on; todo pins right when wide
▌  ▣ ━━━━━━━━━━━━━━━━────────────────────────── 38.2%    context meter — the resting card's one bar
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                ◷ 1m    context line — filled window · composition · age (◷, ≥1m)
```

- **Line 1 — identity.** The animated leading cell, then the agent kind (dim — the glyph and its color carry identity), then capability: model, effort, and the context-window token (`258k`, `1M`). Capability degrades by width — a wide card carries model · effort · window, a medium one drops effort, a narrow one keeps just the name. The session `$cost` pins right.
- **Line 2 — what it's on.** The session name you gave it (`--name` / `/rename`), else the agent's task, else its latest prompt (which lingers once the turn ends and the task clears, so an unnamed session stays labelled until it earns a name). An idle agent with nothing to show yet animates a `.`→`..`→`...` loading cue here instead of an em dash. A turn that died on a provider API error takes the line over with the upstream error text, dim (`API Error: Overloaded`), for as long as its `!` escalation holds — the card says why without a jump ([internals/agent.md → the state machine](../internals/agent.md#the-state-machine)). On a wide card, todo dots pin right.
- **The context meter (`▣`/`▢`).** The resting card's one bar — `▢` hollow at an empty 0% window, `▣` once anything fills it. The bar fills as the window is *used*; its value is always the percent used. While the window is calm it splits into colored segments showing *where* the tokens went (cache writes / cache reads / fresh input); once it warns it goes a solid amber/red. The `▣` glyph reads *how full* the window is on its own ramp — blue while cold, warming as it fills.
- **The context line.** Part of the resting card, and the meter's absolute companion: `▤` the filled part of the window — `input + cache-write + cache-read` of the latest API call, exactly the numerator the `▣` percent scales, so the bar and this figure read as one measurement — then a `·` seam and the call's composition ordered by how the window filled: `◌` read back from cache, `◍` newly written to cache, `↘` fresh input, `↗` output generated (which joins the window next turn). The four composition columns keep the same disjoint meanings the cockpit and fleet ledger accumulate; the `◇` totals stay fleet vocabulary, because this line answers what is in the window, not what today burned. A coarse `◷` last-activity age pins right once it crosses a full minute — a just-active agent shows the line alone, left-aligned, rather than a misleading `1m` — and its tone ramps as the prompt cache cools (dim · amber from 20m · red from 1h, the legend's cache ramp), so a red age warns that resuming pays for the context again. An agent whose context carries no per-call split (Codex, or Claude before its first API call) shows the bare `▤` rollup total alone.

The `▣`/`▢`, `▤`, and `◷` glyphs share one lead column, so the card reads as an aligned grid.

**Selection.** The resting card is the four lines above. Selecting any row lights the bold `▌` spine and *appends* the agent's subagents beneath — it never reshapes a line already on screen, so the card never reflows:

```
   resting                        selected — only appends, never reshapes
   ─────────────────              ────────────────────────────────────────
    ⣾ claude · Opus · 1M          ▌⣾ claude · Opus · 1M              $1.27
      ledger refactor             ▌  ledger refactor
      ▣ ━━━━━━━──── 38.2%         ▌  ▣ ━━━━━━━──── 38.2%
      ▤ 76k · ◌ 68k ◍ 6k         ▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k    ◷ 1m
                                  ▌  ⧉ subagents (1)                  ← appended
                                  ▌    ⢿ Explore
```

The expanded card also lists any **subagents** the agent spawned this turn — a dim `⧉ subagents (N)` header then, per child, the status glyph and type with what the parent asked it to do, and a deeper-indented second line carrying its token spend `◇` and elapsed work `◷` pinned right under the parent's stats:

```
▌  ⧉ subagents (2)
▌    ⢿ Explore — locate the render seam
▌        ◇ 12.4k                              ◷ 1m30s
▌    ✓ review — audit the trust hash
▌        ◇ 3.1k                                  ◷ 45s
```

The description, tokens, and elapsed ride in from Claude's `subagentStatusLine` (Claude-only; harvested at install time). A Codex child, or a Claude child before its first render, shows just the `glyph type` line. Subagents have no pane of their own, so they never get a row; they nest here only.

### Attention rows

A waiting or failed agent is the whole point. Its glyph leads, bold, and the card rises to the top of its worktree:

```
▏feature-migration ┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌! claude · Opus · 1M
▌  db migrate
▌  ▢ ──────────────────────    0%
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
▌  ▢ ──────────────────────────    0%
▏⣾ codex · GPT-5.5
▏  add tests
▏  ▢ ──────────────────────────    0%

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
▌  ▢ ────────────────────────    0%
▏⣾ codex
▏  task-1
▏  ▢ ────────────────────────    0%
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
 Claude v2.1.158 · Claude Max                    ⇅ rc    header — product · version · plan, rc flag right
                                                         blank line below the name
  ▐▛███▜▌  ◇ 486k ↘ 422k ↗ 64k ◍ 12k ◌ 68k      $3.50    emblem + today's token breakdown · spend (right)
 ▝▜█████▛▘ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱ ↻ 2h06m    5-hour window left, until reset
   ▘▘ ▝▝   7d  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱ ↻ 1d02h    7-day window, same start/end column
```

The dashboard isn't pinned to a fixed set of windows. If a provider reports a single window — as Codex briefly did during a server-side bug that widened its window to ~30 days — it paints one bar, labeled by its length, instead of misrendering:

```
 Codex v0.136.0 · ChatGPT Pro                            header — product · version · plan

  ▐▛███▜▌  ◇ 88k ↘ 76k ↗ 12k ◍ 0 ◌ 8k           $1.20    emblem + today's token breakdown · spend
 ▝▜█████▛▘ 30d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱ ↻ 28d04h    a single window, labeled by length
```

An **unmetered (API-key) account** has no budget to drain, so it shows an `∞` bar — the icon in the front slot, an empty track, no countdown:

```
 Codex v0.135.0 · ChatGPT Pro

  ▐▛███▜▌  ◇ 88k ↘ 76k ↗ 12k ◍ 0 ◌ 8k           $1.20
 ▝▜█████▛▘ ∞  ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱
   ▘▘ ▝▝  
```

Every bar across every block shares one start column and one end column, so the dashboard reads as one aligned grid. A blank line separates blocks. The `⇅ rc` flag pins to a block's top-right when remote control is on for that provider (Claude only — it's host infrastructure, never its own row). Below ~34 columns the emblem is dropped and the bars run full-width. The brand emblem, color, and name are config-driven (`[sidebar.providers.<kind>]`, see [configuration.md](../reference/configuration.md)).

### The fleet ledger

The fleet's running totals seal the bottom of the dashboard, above the footer — a quiet two-row ledger you learn to glance at, never a cue that competes with the rows. A trailing-week (`W:`) row and a trailing-month (`M:`) row, fleet-wide across every provider.

```
 W: ◎  92  ◇ 16.5k ↘ 2.3k ↗ 14.2k ◌ 168k       $140.57
 M: ◎ 212  ◇ 76.5k ↘ 12.3k ↗ 64.2k ◌ 668k      $240.57
```

- **The rows.** Each reads `◎ sessions  ◇ total ↘ input ↗ output ◌ cache-read  $spend`: the thread count, the precise one-decimal token figures (the exact record beside the cockpit's coarse live read), and the spend pinned to the right edge in money-green. The `◇` total carries the same soft-violet as the cards. Every numeric field is right-aligned into one shared grid, so the `W:` / `M:` labels stack and each column lines up. The `◍` cache-write field is omitted here — the ledger keeps to the headline figures.
- **No animation.** The ledger figures are static — only today's headline (the cockpit's `$`) counts up. The windows escalate `today → week → month`.

Every figure is computed from the transcript JSONL — Codex's dollars priced from its token counts, every provider that logs usage counted, all of them fleet-wide. The ledger is dropped until something has been recorded.

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
| capability + window | `agent_capability` |
| selected, enriched card | `enriched_selected_agent_card` |
| worktree grouping + external | `worktree_attention_map` |
| per-worktree cap | `group_cap_with_overflow` |
| provider dashboard | `provider_dashboard` |
| fleet ledger (week/month) | `fleet_ledger` |
| health alert | `degraded_banner` |

When the renderer changes how something looks, update the `.snap` (the test prints the diff) and this doc together.

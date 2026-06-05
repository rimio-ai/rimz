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
 ◎ 12                  ◇ 76k ↘ 12k ↗ 64k ◍ 20k ◌ 68k    ← sessions today · today's tokens (right)
 ¤ 1                                             $4.20    ← live agents · today's fleet spend
 ────────────────────────────────────────────────────
 ? 0   ! 0   ○ 0   ⏸ 0                      ⢿ 1   ✓ 0    ← make-up: who needs you | who's busy

▏⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ +127 -43    ← the worktree you're in (lane spine ▏)
▌⣾ claude · Opus 4.8 · high · 1m                $1.27    ← line 1: identity · capability · $cost
▌  ledger refactor                                       ← line 2: what it's on
▌  ▣ ━━━━━━━━━━━━━━━━────────────────────────── 38.2%    ← context meter: how full the window is
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                 ◔ 1m    ← context line: filled window · last-activity age

 ────────────────────────────────────────────────────
 Claude v2.1.158 · Claude Max                    ⇅ rc    ← provider · plan · remote-control flag
                                                         ← blank line below the name
  ▐▛███▜▌  ◎ 12  ◇ 486k ↘ 64k ↗ 422k ◌ 68k      $3.50    ← brand emblem · sessions · today's tokens · spend
 ▝▜█████▛▘ 5h ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱ ↻ 2h06m    ← 5-hour budget left, until reset
   ▘▘ ▝▝   7d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱ ↻ 1d02h    ← 7-day budget left, until reset

  W: ◎  92  ◇ 16.5k ↘  2.3k ↗ 14.2k ◌ 168k     $140.57  ← week ledger: sessions · tokens · spend
  M: ◎ 212  ◇ 76.5k ↘ 12.3k ↗ 64.2k ◌ 668k     $240.57  ← month ledger
                      ? for help                         ← footer (pinned to the bottom edge)
```

The rest of this doc reads that frame zone by zone.

## Reading the glyphs

One vocabulary runs through the whole sidebar: a shape carries the meaning, color reinforces it. This is the complete legend, and the canonical home for it — every other doc points here, and the `?` overlay inside the app shows a short version in place.

**Status — the leading cell of every agent row.**

| glyph | state | meaning | needs you |
|-------|-------|---------|-----------|
| `?`   | waiting    | the agent asked something; answer in its pane | yes — yellow, heating amber then red with the age clock; the breath quickens with it, blinking at red |
| `!`   | attention  | a failed turn, a turn dead on a provider API error, or a working agent gone silent past the stall window | yes — yellow, heating amber then red with the age clock; the breath quickens with it, blinking at red |
| `⏸`   | rate-limited | on an account whose rate-limit window is spent — parked mid-task or at rest until it resets, resumes with a `continue` | waiting on the reset — held amber, never heats |
| `⢿`   | working    | running and editing — animates `⣾⣽⣻⢿⡿⣟⣯⣷` in clay | no |
| `✽`   | thinking   | running, before the turn's first file edit — sparkles `· ✢ ✳ ✶ ✻ ✽` in clay | no |
| `⠙`   | resolving  | a resolver is answering on the bridge — braille spin | pending, being handled |
| `○`   | idle       | alive, nothing to do | no |
| `✓`   | done       | finished cleanly | no |
| `○`/`⢿`   | process | a pane with no agent (shell, editor); idle shows the hollow `○` in the idle quiet green, real work the `⢿` spinner in the working clay — the whole row one dim step below the agent cards, never a cockpit tally | no |

Three short-lived heads ride over the base status on the leading cell, so they never earn a cockpit bucket of their own:

| head | meaning |
|------|---------|
| `✽` thinking | the running turn before its first file edit — the agent is reasoning and reading, not yet writing; a research turn that never edits sparkles end to end |
| `▇` compacting | condensing its context window — pulses `▁▃▄▅▆▇▆▅▄▃` in violet, then returns to its resting head |
| `´` waiting on subagents | the main agent delegated to its children; the work is in the rows below — a low clay wave (`_` bobbing up to `´` and back) |

Every running agent — whichever head rides its cell — counts as **working** (`⢿`) in the cockpit make-up.

The two actionable attention glyphs (`?` / `!`) **breathe** — a brightness pulse that swells from dim to bold and fades back, never blanking — to pull the eye back to an unanswered row, and they **heat with the age clock**: yellow from the first second, amber once the row sits unanswered past the half hour, red past the hour — the same quarter-hour ramp the `◔` age glyph beside them wears — so a fresh ask reads calm-urgent and a long-ignored one visibly heats up. The breath paces with the same heat: a slow ~2.4s swell while yellow, double-time once amber, and at red the swell gives way to a hard bold↔dim blink — so the cadence alone carries the urgency even under `NO_COLOR`. A working agent that goes silent past the stall window escalates to a breathing `!` instead of spinning on. The `⏸` rate-limited head is attention-class but parked: it holds still in a held amber and never heats or breathes, since waiting for the reset is the only move. A parent waiting on subagents is exempt from the stall escalation — its quiet wave is the children's work, not a wedge. An idle agent with no prompt yet shows a gentle `.`→`..`→`...` loading cue on line 2 in place of the em dash, drifting on the same lazy ~2.4s cycle as the resting breath.

On a truecolor terminal the breath also **glows**: the glyph, the agent name, and the card's gutter spine swell in color brightness on the same wave and the same heat pacing (the red blink keeps the cell to itself), and brief color flashes mark the moments of change — a card lights as it enters `waiting`/`failed`, settles green as its ask resolves or its rate limit lifts, fades in on arrival, and the `▌` spine flicks under a freshly landed selection. All of it is color only, layered over the same glyphs: `NO_COLOR`, a 256-color terminal, or a `glow = false` opt-out in `[sidebar]` ([configuration](../reference/configuration.md#glow)) reads the exact modifier breath above, nothing missing but the shimmer (mechanics in [internals/sidebar.md → The runtime loop](../internals/sidebar.md#the-runtime-loop)).

**Window — the model's context window on the identity line.**

A lowercase magnitude token (`258k`, `1m`) closing the capability cluster: the live out-of-band reading (Claude's statusline, Codex's app-server) when one exists, else the hook-derived window, omitted until a source names it. Dim-weight chrome — a capability label, not a status signal — tinted by size class so the magnitude reads at a glance: clay amber for a 1m+ window, gold at 258k, sky blue at 128k, plain gray below; the context meter's severity ramp keeps the loud color slot.

**Meters and stats — one grammar everywhere.**

| token | reads as |
|-------|----------|
| `▣ ━━━━──── 38.2%` | context meter — how full the window is, bar fills as used; an empty 0% window reads the hollow `▢`. Glyph, bar, and the `▤` head below share one severity ramp — calm blue → yellow → amber → red, bands configurable via `[sidebar.context]` |
| `▤ 76k`           | filled context — the absolute tokens in the window (the `▣` meter's numerator); leads the card's context line in the meter's severity tone |
| `◇` `↘` `↗` `◍` `◌` | tokens: total · input · output · cache-write · cache-read. One color per marker, everywhere: the `◇` total soft-violet, the rest their bar-segment tones (`◌` blue · `◍` yellow · `↘` red · `↗` green) — so the card's context line legends the bar and the cockpit/dashboard/ledger lines speak the same vocabulary; the figures beside them read at full strength on the fleet lines |
| `◔ 1m`            | last-activity age — shown only once it crosses a full minute; the clock face fills by the quarter hour (`◔` ≤15m · `◑` ≤30m · `◕` ≤45m · `●` ≤60m · `◉` past it) and its tone steps the same quarters: dim through `◔` (a resume still hits cache), yellow to the half hour, amber beyond it, red past the hour — a red age means resuming likely re-reads the whole context uncached. On a subagent line the same face and ramp read the child's elapsed work, as a fixed three-cell `m`/`h` label (`<1m` under a minute, never seconds) |
| `C  11%`          | CPU utilisation of the pane's foreground process; the three resource stats ride process rows only, as one dim fixed-width grid — each figure right-aligned in its slot, so the cluster never shifts as values change |
| `M 512M`          | resident set size (RSS) — `k` / `M` / `G` |
| `⇅   3M/s`        | combined VFS I/O rate (rchar + wchar bytes/s) |
| `+127 -43`        | lines added / removed |
| `⇡3 ⇣1`           | commits ahead / behind the trunk (worktree header; zero components drop) |
| `≡ main`          | worktree fully landed on the trunk — zero ahead, zero diff, safe to remove (behind doesn't count against it); the trunk worktree itself never wears it |
| `●●●○○ 3/5`       | todo progress |
| `$1.27`           | spend — money-green, always two decimals; omitted while a session's cost still rounds to zero |
| `▰▱` / `∞`        | provider budget bar (fill = left), draining green → yellow → amber → red by what remains / unmetered account — the `∞` icon and its empty track share the provider's brand color |
| `↻ 2h06m`         | when that budget resets |

**Structure and chrome.**

| mark | meaning |
|------|---------|
| `⌘ name`        | the workspace |
| `¤ N`           | the live agents in the room right now — the glyph in the agents' working clay |
| `◎ N`           | sessions (threads) that have run today (cockpit) / in the window (ledger) — teal in both |
| `⧉ N`           | the subagents an agent spawned this turn (expanded card) — the marker violet, the label dim |
| `⑂ name`        | a worktree group header — its live branch |
| `▏`             | the selection lane — the worktree you're in |
| `▌`             | the selected card |
| `┄ external ┄`  | out-of-project panes (scripts, CI, stray shells) |
| `─`             | a section hairline |
| `▐` / `▕`       | the cards' scrollbar — thumb / track, riding the right margin while the overflowing viewport is moving, settling away about a second after the scroll stops (`[sidebar] scrollbar` pins or removes it, [configuration](../reference/configuration.md#scrollbar)) |
| `┤ Tab ├`       | the provider dashboard's active tab — a brand-colored chip notched into the panel's top hairline between `┤ ├` caps, so the pick reads by shape under `NO_COLOR` |
| `⇅ rc`          | remote control is on for that provider |

## Zone 1 — the cockpit

The top block. Fixed height, so the rows below it never jump as agents change state. Top to bottom it answers: *whose room is this, what's it costing, who needs me, and what has the fleet done.*

```
 ⌘ query-engine                            ~/code/query-engine

 ◎ 12                      ◇ 76k ↘ 12k ↗ 64k ◍ 12k ◌ 68k
 ¤ 5                                                  $4.20
 ──────────────────────────────────────────────────────────
 ? 2   ! 1   ○ 1   ⏸ 0                            ⢿ 2   ✓ 0
```

- **Identity.** The workspace name behind `⌘`, with the project path dim on the right edge (home-abbreviated to `~/…`; it left-truncates with a leading `…` before it ever crowds the name). A blank line sets it apart from the summary below.
- **Summary — who's here and what today burned.** Two lines, each a colored glyph and full-strength count on the left with today's numbers pinned right. Line 1 is the day at a glance: `◎` (teal) the sessions (threads) that have run today, with today's accumulated token breakdown — `◇` total · `↘` input · `↗` output · `◍` cache-write · `◌` cache-read, each marker in its one color — pinned to the right edge in the coarse integer form (it drops when today recorded no tokens, leaving `◎ N` alone). Line 2: `¤` (the agents' working clay) the live agents in the room right now, with today's spend pinned right. The counts read from the live fleet and the JSONL `value_tally`'s today window. An empty room reads `◎ 0` over `¤ 0`.
- **Today's spend.** The fleet's total spend for today, pinned to the right of the live-agents line, **counting up** in a smooth eased climb as a turn lands (with a brief brighten as it settles) — the cockpit's one animated number. It joins the line once today records spend.
- **The make-up — split by who might want you.** The **left cluster** is worth a glance: `?` waiting and `!` failed (breathing, each wearing its oldest row's age heat over a yellow floor), then a free `○` idle agent — calm, but a free agent wants work — and a `⏸` rate-limited count closing the cluster (held amber, parked). The **right cluster** is the busy tail: `⢿` working (every running agent — the thinking sparkle and the compaction pulse are per-row heads, not buckets), then `✓` done. Every bucket always shows — the glyph in its semantic tone, a zero count faint beside it — so the line is scannable by position and reads as a stable colored legend.

An **empty room** has no make-up line at all — just identity and the `◎ 0` / `¤ 0` summary:

```
 ⌘ query-engine

 ◎ 0
 ¤ 0
 ──────────────────────────────────────────────────────────

 no agents yet
 install hooks:
 rimz hooks install claude
```

The first-run hint names the real next step: an un-wired room points at `rimz hooks install`, a wired one reads `run claude or codex / in a pane to begin`. It clears the instant the first agent or pane appears.

## Zone 2 — the agent cards

The body: one card per pane, grouped under the worktree it lives in. A worktree is total isolation — only same-worktree agents collaborate — so each group reads as one bounded block.

**The cards scroll between the pinned zones.** When the cards outgrow the pane, they scroll between the cockpit above and the provider dashboard below — both stay put — and a thin scrollbar rides the right margin while the viewport is moving: a solid `▐` thumb over a hairline `▕` track, the position carried by shape so it reads under `NO_COLOR`. The bar follows the motion — a wheel scroll or the selection-driven auto-follow — and settles away about a second after the view stops, so a resting column stays clean; `[sidebar] scrollbar = "always" | "never"` pins it up or removes it ([configuration](../reference/configuration.md#scrollbar)). The viewport follows the selection: picking any row — arrows, a click landing, `␣` — brings its card, expanded subagent list included, fully into view, and a card taller than the window pins its first line to the top. The mouse wheel scrolls the viewport freely without moving the selection — peek anywhere; the next selection change snaps the view back to the selected card. `?` reveals the help overlay at the zone's tail whatever the scroll position, and the view holds there while the overlay is open.

### The card

An agent is a small stacked card. The resting card is four lines; selecting it appends any subagents. Selection never reshapes a line already on screen — it only *appends* and lights the spine — so the card never reflows as it expands.

```
▌⣾ claude · Opus 4.8 · high · 1m                $1.27    line 1 — glyph · name · model · effort · window, $cost right
▌  ledger refactor                          ●●●○○ 3/5    line 2 — what it's on; todo pins right when wide
▌  ▣ ━━━━━━━━━━━━━━━━────────────────────────── 38.2%    context meter — the resting card's one bar
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                ◔ 1m    context line — filled window · composition · age (≥1m)
```

- **Line 1 — identity.** The animated leading cell, then the agent kind in its provider brand color (mid-gray chrome for unknown kinds), then capability: model, effort, and the context-window token (`258k`, `1m`) in dim-weight chrome tinted by size class (clay amber 1m+ · gold 258k · sky 128k · gray below). Capability degrades by width — a wide card carries model · effort · window, a medium one drops effort, a narrow one keeps just the name. The session `$cost` pins right, joining the line once the session has actually spent — an idle agent at `$0.00` shows nothing.
- **Line 2 — what it's on.** The session name you gave it (`--name` / `/rename`), else the agent's task, else its latest prompt (which lingers once the turn ends and the task clears, so an unnamed session stays labelled until it earns a name). An idle agent with nothing to show yet animates a `.`→`..`→`...` loading cue here instead of an em dash. A turn that died on a provider API error takes the line over with the upstream error text, dim (`API Error: Overloaded`), for as long as its `!` escalation holds — the card says why without a jump ([internals/agent.md → the state machine](../internals/agent.md#the-state-machine)). On a wide card, todo dots pin right.
- **The context meter (`▣`/`▢`).** The resting card's one bar — `▢` hollow at an empty 0% window, `▣` once anything fills it. The bar fills as the window is *used*; its value is always the percent used. Glyph and bar wear one severity — calm blue → yellow → amber → red, bands tunable via `[sidebar.context]` ([configuration.md](../reference/configuration.md#context-meter)). While the meter rests calm the fill splits into colored segments showing *where* the tokens went (`◍` cache writes · `◌` cache reads · `↘` fresh input); once it warms the bar goes one solid severity run.
- **The context line.** Part of the resting card, and the meter's absolute companion: `▤` the filled part of the window — `input + cache-write + cache-read` of the latest API call, exactly the numerator the `▣` percent scales, wearing the meter's severity tone so the bar and this figure read as one measurement — then a `·` seam and the call's composition ordered by how the window filled: `◌` read back from cache, `◍` newly written to cache, `↘` fresh input, `↗` output generated (which joins the window next turn), each marker in its bar-segment color so the line doubles as the bar's legend. A zero column drops whole — the line shows what filled the window — so a Codex card, whose protocol reports no per-call cache-write, simply never grows a `◍`. The composition columns keep the same disjoint meanings the cockpit and fleet ledger accumulate; the `◇` totals stay fleet vocabulary, because this line answers what is in the window, not what today burned. A coarse last-activity age pins right once it crosses a full minute — a just-active agent shows the line alone, left-aligned, rather than a misleading `1m` — as the clock-fill glyph (`◔`→`◉`, the legend's quarter-hour face) whose tone steps the same quarters (dim · yellow from 15m · amber from 30m · red past the hour, the legend's age ramp), so a red age warns that resuming pays for the context again. A delegating parent's age reads the freshest of its own and its children's activity, so it stays honest while the work is theirs. Claude legends the split from its statusline; Codex from its rollout's per-call usage on the lifecycle rail. An agent whose context carries no per-call split yet shows the bare `▤` rollup total alone — any agent before its first API call, or a statusline-fed card right after `/compact` (a rollout-fed split refreshes with the next call instead).

The `▣`/`▢` and `▤` glyphs share one lead column, so the card reads as an aligned grid.

**Selection.** The resting card is the four lines above. Selecting any row lights the bold `▌` spine and *appends* the agent's subagents beneath — it never reshapes a line already on screen, so the card never reflows:

```
   resting                        selected — only appends, never reshapes
   ─────────────────              ────────────────────────────────────────
    ⣾ claude · Opus · 1m          ▌⣾ claude · Opus · 1m              $1.27
      ledger refactor             ▌  ledger refactor
      ▣ ━━━━━━━──── 38.2%         ▌  ▣ ━━━━━━━──── 38.2%
      ▤ 76k · ◌ 68k ◍ 6k         ▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k    ◔ 1m
                                  ▌  ⧉ subagents (1)                  ← appended
                                  ▌    ⢿ Explore
```

The expanded card also lists any **subagents** the agent spawned this turn — a `⧉ subagents (N)` header (the marker violet, the label dim) then, per child in spawn order (creation time ascending, stable across refreshes), the same live head an agent row wears — the `✽` thinking sparkle while the child reasons, the `⢿` working fill while it acts, the static verdict once it lands — and the type with what the parent asked it to do, then a deeper-indented second line carrying its token spend `◇`, model, and reasoning effort with elapsed work pinned right under the parent's stats: the clock-fill glyph (filling with the child's worked span) over a fixed three-cell `m`/`h` label — `<1m` under a minute, never seconds — toned by the parent's age ramp, so the clusters stack into one column across children and a long-running child visibly heats up. A finished child holds its `✓` (or `!`) on the list until the parent's next turn clears it:

```
▌  ⧉ subagents (2)
▌    ✻ Explore — locate the render seam
▌      ◇ 12.4k · Opus 4.8                       ◔ 14m
▌    ✓ review — audit the trust hash
▌      ◇ 3.1k · Haiku 4.5 · high                ◔ <1m
```

The description, tokens, and elapsed ride in from Claude's `subagentStatusLine` (Claude-only; harvested at install time); the model, effort, and turn phase from the child's own lifecycle events, so siblings on different models read apart at a glance and a reasoning child sparkles like its parent would (Claude reports a child's effort on its `SubagentStop`, so the effort token typically joins the line as the child finishes). A child with none of them — a Codex child, or a Claude child before its first render — shows just the `glyph type` line. Subagents have no pane of their own, so they never get a row; they nest here only.

### Attention rows

A waiting or failed agent is the whole point. Its glyph leads, bold, and the card rises to the top of its worktree:

```
▏⑂ feature-migration ┄┄┄┄┄┄┄┄┄┄┄┄
▌! claude · Opus · 1m
▌  db migrate
▌  ▢ ──────────────────────    0%
```

A `?` waiting row reads the same with a `?` glyph. The row carries *who* needs you and *what task*, and selecting it lands you in the agent's pane, where the full prompt and its safe defaults live — that is the row's job, to route you there. A script's `feed ask` is the one item answerable from the sidebar itself: it chose Rimz as its surface, so its declared options render as buttons on the row.

**A resolver answering** replaces the `?` with a braille spinner and fills the task slot with the resolver name and its remaining budget — the item is pending, just being handled. It flips back to `? waiting` if the chain exhausts:

```
? claude  fix auth flow  1m      →      ⠙ claude  opus-policy 24s   1m
```

### Process rows

A pane no agent has stamped reads like a slim agent card, one dim step quieter: a hollow `○` in the idle agent's quiet green for an idle shell or editor, the `⢿` spinner in the working clay for a pane doing real work like a build, test, or install, and the program name in the same soft weight. That weight is the boundary: inside a worktree group the process rows settle below the agent cards, and the slight dim — carried as weight, so it survives `NO_COLOR` — reads them as the group's command tail rather than more agents. An active pane anchors its primary line on the shell that owns it, so the line stays put as commands come and go, and carries the live command in full on a second line. At wide (L2) widths the row pins `C  <n>%  M <n>[k/M/G]  ⇅  <n>[k/M/G]/s` right on line 1 — CPU, RAM, and combined VFS I/O rate as one dim fixed-width grid (each figure right-aligned in its slot, a metric not yet sampled blank-filling it, so the cluster never wanders as values change), in the same right slot an agent card gives its `$cost` — so resource load reads at a glance without leaving the sidebar. The stats are process-row vocabulary; an agent card keeps that line for its identity and cost:

```
○ zsh
⢿ zsh                           C  34%  M 512M  ⇅   8M/s
    cargo build --release
```

The label is the program the pane runs, read past a `sudo` wrapper and through a `node`/`npx` launcher (`sudo npm install -g @openai/codex` is an `npm` install, not a codex agent; `node …/codex` is codex). No status, no meter, never counted in the cockpit — it is presence, not a cue. It is still a jump target, and the moment an agent's hook stamps that pane it becomes that agent's card.

### Worktree groups and the selection lane

Worktrees stack as bounded blocks. The one **holding your selection** reads as a single bracketed lane: a thin `▏` spine down its header and every row, a dotted `┄` seal on its header, and the selected card itself lit with a bold `▌`. Every other worktree carries a blank gutter, so the lane is the only marker on screen and the selection is unmistakable.

The worktree header carries the worktree's git story on the right: the `⇡`/`⇣` commit delta against the trunk, then the worktree's total diff (`⇡3 ⇣1 +230 -23`, zero components dropped). A fully-landed worktree — zero commits ahead and a zero diff against its fork point — collapses the whole cluster to `≡ main`: nothing left to land, safe to remove. Behind sits outside that test, since the trunk moving on makes a landed worktree no less removable; and the trunk worktree itself never wears the marker — "landed on itself" says nothing, so its header keeps the plain cluster. The trunk is auto-detected (`main` → `master` → the remote's default) and overridable per machine ([configuration](../reference/configuration.md#trunk-branch)).

```
▏⑂ feature-migration ┄┄┄┄┄ ⇡3 ⇣1  +230 -23    ← in flight: commits ahead/behind, then the diff
▏⑂ feature-landed ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ≡ main    ← fully landed: nothing to land, safe to remove
```

```
▏⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄    ← selected worktree: lane spine + dotted seal
▌? claude
▌  permission
▌  ▢ ──────────────────────────    0%
▏⣾ codex · GPT 5.5
▏  add tests
▏  ▢ ──────────────────────────    0%

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ? 1   ← out-of-project panes, attention-only tally
 ? deploy.sh
   Deploy staging?
```

The `external` block is the catch-all for panes outside the project — untethered scripts, CI, stray shells. It renders as a dim divider rather than a worktree header, sorts last unless it holds something waiting, and keeps an attention-only `? n` / `! n` tally so an out-of-project ask still surfaces.

**Ranking is automatic: the most attention-hungry rises, nothing else moves.** Within a worktree, rows sort `waiting → failed → done → working → idle` (a parked idle agent is the least needy, so it settles to the bottom — which is exactly where a freshly-launched agent appears). Attention rows sort oldest-first, so the longest-overdue is always on top. Worktrees themselves sort by their most-urgent member.

**The cap.** Each worktree shows a capped number of rows (configurable) with a dim `+K more`. The cap trims only the calm tail; every `waiting`/`failed` row is always shown, so the cap can never hide something that needs you:

```
▏⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
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
- `←/→` switch the provider dashboard's tab — a pick in place, never a jump.
- A click anywhere in a card's block jumps to it.
- The mouse wheel scrolls the card list without moving the selection; the next selection change snaps the view back to the selected card.

## Zone 3 — the provider dashboard

The budgets are account-scoped — every session of a provider shares one account's budget — so they leave the rows for a pinned panel at the bottom. With several accounts the panel is **tabbed**: the panel's top hairline becomes a tab rail naming every provider (each label in its brand color, the active tab a brand-colored chip notched into the line between `┤ ├` caps; every tab reserves its cap cells, so switching never shifts a label), and one account's block paints at a time, so the budgets read one account deep instead of stacking. The rail names the account, so the block's header drops the product name and reads plan-first (`Claude Max · v2.1.158`), indented to start over the `◎` stats column. The active tab **follows the selected pane's provider** — select a codex pane and the dashboard reads the ChatGPT account behind it; a process pane falls to the first tab. `←`/`→` or a click on a tab label picks one by hand; the pick holds until you select a pane of a *different* provider, then the follow-the-selection default takes over. A single account keeps its bare block — nothing to switch, no tab rail. Every account that is logged in but idle this run still earns its tab, so your accounts and budgets show even between turns. Each provider shows one bar per window it reports — both Claude and Codex a 5-hour and a 7-day — so the dashboard tracks whatever windows a provider exposes. Codex budgets are pulled out-of-band from the app-server: active sessions refresh during long turns, and a logged-in idle account refreshes from the shared cache path.

Each block's stats line speaks the fleet ledger's vocabulary, scoped to the provider: today's `◎` session count, then the `◇ ↘ ↗ ◌` token breakdown (the `◍` cache-write figure omitted, like the ledger rows), with the spend pinned right.

A **metered account** drains one "mana" bar per budget window toward its reset. The bar fills with what's *left*, ramping green → yellow → amber → red as it empties — and a fully-spent window (0% left) flips its whole empty track red, so an exhausted budget never reads as an untouched one. Each window's label (`5h`/`7d`) wears its own bar's color, so the row reads as one unit. A spent longer window gates the shorter ones: once the `7d` is exhausted the `5h` row is painted exhausted too — red, no countdown — regardless of its own reading, since that budget is unusable until the longer window resets.

These are **sliding windows** that begin counting only on your first token, so until then the provider keeps sliding the reset a full window-length ahead. A window whose reset still sits ~a full window out has **not started** (it still reads ~1% used, not 0 — so it's the reset distance that gives it away). Any usage above that ~1% floor means it has already started, countdown and all; only a 0–1% window with a near-full reset qualifies. A not-started window shows a near-full bar with **no countdown**, reading "ready — send a message to start it" rather than a misleading ticking placeholder; the countdown appears once your first token fixes the reset and it begins ticking down.

```
 ──┤ Claude ├─── Codex ──────────────────────────────    tab rail — the panel's top hairline; the selected pane runs Claude
                                                         blank line below the rail
           Claude Max · v2.1.158                 ⇅ rc    header — plan · version over the stats column, rc flag right
  ▐▛███▜▌  ◎ 12  ◇ 486k ↘ 422k ↗ 64k ◌ 68k      $3.50    emblem + sessions · today's tokens · spend (right)
 ▝▜█████▛▘ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱ ↻ 2h06m    5-hour window left, until reset
   ▘▘ ▝▝   7d  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱ ↻ 1d02h    7-day window, same start/end column
```

Switching the tab (`→`, or a click on `Codex`) re-notches the `┤ ├` caps onto the picked tab and swaps the block in place — every tab reserves its cap cells (painted as rail fill when resting), so no label ever shifts. This account is **unmetered** (an API key): no budget to drain, so it shows an `∞` bar — the icon in the front slot and an empty track, both in the provider's brand color so the row reads as one branded unmetered bar, no countdown:

```
 ─── Claude ───┤ Codex ├─────────────────────────────    the picked tab, chipped in place — labels hold still

           ChatGPT Pro · v0.135.0                        header — plan · version
  ▐▛███▜▌  ◎ 3  ◇ 88k ↘ 76k ↗ 12k ◌ 8k          $1.20    emblem + sessions · today's tokens · spend
 ▝▜█████▛▘ ∞   ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱            unmetered — branded `∞` bar, no countdown
   ▘▘ ▝▝
```

The dashboard isn't pinned to a fixed set of windows, either — a provider's bars are whatever windows it reports. When a server-side bug briefly widened Codex's window to ~30 days, the block painted one bar labeled by its length instead of misrendering:

```
 Codex v0.136.0 · ChatGPT Pro

  ▐▛███▜▌  ◎ 3  ◇ 88k ↘ 76k ↗ 12k ◌ 8k          $1.20
 ▝▜█████▛▘ 30d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱ ↻ 28d04h    a single window, labeled by length
```

A **Pi block** names its version and the subscription it runs on — `Pi v0.78.0 · Anthropic OAuth` — read out-of-band from `pi -v` and Pi's auth file (the freshest session's provider picks among several credentials; [account.md](../internals/account.md#per-provider-mapping)). Pi reads no window surface of its own, but an OAuth sub *is* a sibling provider's account — Anthropic OAuth is the Claude account, OpenAI OAuth the Codex one — so the Pi tab paints that account's 5h/7d bars: same budget, same bars as the sibling's own tab. A sub with no sibling readings shows no bars, and an API key the `∞` bar.

Every bar shares one start column and one end column whichever tab is active, so the dashboard reads as one aligned grid. The `⇅ rc` flag pins to the block's top-right when remote control is on for that provider (Claude only — it's host infrastructure, never its own row). Below ~34 columns the emblem is dropped and the bars run full-width. The brand emblem, color, and name are config-driven (`[sidebar.providers.<kind>]`, see [configuration.md](../reference/configuration.md)).

### The fleet ledger

The fleet's running totals seal the bottom of the dashboard, above the footer — a quiet two-row ledger you learn to glance at, never a cue that competes with the rows. A trailing-week (`W:`) row and a trailing-month (`M:`) row, fleet-wide across every provider.

```
  W: ◎  92  ◇ 16.5k ↘  2.3k ↗ 14.2k ◌ 168k     $140.57
  M: ◎ 212  ◇ 76.5k ↘ 12.3k ↗ 64.2k ◌ 668k     $240.57
```

- **The rows.** Each reads `◎ sessions  ◇ total ↘ input ↗ output ◌ cache-read  $spend`: the thread count, the precise one-decimal token figures (the exact record beside the cockpit's coarse live read) at full strength, and the spend pinned to the right edge in money-green. A one-cell lead pad sets the `W:`/`M:` tags a hair off the chrome edge; the tags wear sky blue, and each marker its one shared color — the teal `◎`, the soft-violet `◇`, the segment-toned `↘ ↗ ◌`. Every numeric field is right-aligned into one shared grid, so the `W:` / `M:` labels stack and each column lines up. The `◍` cache-write field is omitted here — the ledger keeps to the headline figures.
- **No animation.** The ledger figures are static — only today's headline (the cockpit's `$`) counts up. The windows escalate `today → week → month`.

Every figure is computed from the transcript JSONL — Codex's dollars priced from its token counts, every provider that logs usage counted, all of them fleet-wide. The ledger is dropped until something has been recorded.

## Bottom chrome

Pinned to the bottom edge, below all three zones. The body is truncated before this chrome is ever clipped, so it can never scroll off.

**Footer.** The darkest chrome line — the hairline rules' own tone, receding to pure scaffolding. At rest it is just `? for help`; the triage key joins it only when something actually needs you, so the signature `␣` stays discoverable without shouting:

```
                      ? for help            ← nothing needs you
      ␣ next ?!   ? for help                ← at least one waiting/failed row
```

**Help overlay** (`?`). The legend and keys, in place:

```
 keys & legend
 ↑/↓ select   1-9 jump   ↵ jump
 ␣ next ?!   ←/→ provider tab
 x dismiss   r reload   ? close
 ⢿ working   ✽ thinking   ? waiting
 ! attention   ○ idle   ✓ done
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
| card context line + age pin | `agent_card_context_age` |
| codex card composition (no `◍`) | `codex_card_context_composition` |
| process row + resource stats | `process_row_resource_stats` |
| agents + dimmed process tail | `agents_process_tail` |
| subagent list + elapsed column | `subagent_two_line_entry` |
| worktree grouping + external | `worktree_attention_map` |
| landed worktree header (`≡`) | `worktree_equal_to_trunk` |
| per-worktree cap | `group_cap_with_overflow` |
| cards overflow, scrollbar mid-scroll | `scroll_overflow_shows_bar` |
| selection-driven scroll to bottom, bar settled away | `scroll_offset_follows_selection_to_bottom` |
| tall expanded card pinned to top | `scroll_pins_tall_expanded_card_top` |
| wheel pin holds the viewport | `scroll_manual_offset_holds` |
| scrollbar hidden once settled | `scrollbar_hides_after_settle` |
| scrollbar pinned (`always`) | `scrollbar_always_mode` |
| scrollbar removed (`never`) | `scrollbar_never_mode` |
| provider dashboard, tabbed (derived tab) | `provider_dashboard` |
| provider dashboard, manual tab pick | `provider_dashboard_codex_tab` |
| fleet ledger (week/month) | `fleet_ledger` |
| health alert | `degraded_banner` |

When the renderer changes how something looks, update the `.snap` (the test prints the diff) and this doc together.

# The sidebar, on screen

> What the sidebar *looks like*, section by section, with the real frames it draws. For how it is built — presence model, launch, reload recovery, the view-model — see [docs/internals/sidebar/sidebar.md](../internals/sidebar/sidebar.md). For the design rationale behind the glyph law, see [DESIGN.md → Attention at a glance](../../DESIGN.md#attention-at-a-glance).

The sidebar is one narrow column that answers a single question: **which pane needs you, right now.** Every pane in the room is a row; agents are enriched from the ledger; everything groups by the worktree it lives in. It routes you to the pane — you read and answer in the agent's own UI.

It has three reading zones, top to bottom: the **cockpit** (who is here, what it costs, what needs you), the **agent cards** (one card per pane, grouped by worktree), and the **provider dashboard** (your accounts and their budgets). Bottom chrome — a footer and a health line — pins under all three.

Every frame below is what the renderer actually paints. The structure, glyphs, columns, and alignment are exact; the live values (ages, percentages, resets, counts) are illustrative. Color comes from the active sidebar palette: truecolor terminals get RGB tones, while indexed terminals get those tones quantized to xterm 256 colors. `NO_COLOR` drops the color but keeps every shape, so each meter still reads by its fill. The frames here are colorless text, so they show exactly the `NO_COLOR` reading.

## The whole frame at a glance

A complete frame: a selected agent in a worktree, with the per-provider dashboard pinned at the bottom. The cockpit figures use the current Rimz room's project root and grouped worktrees; the provider dashboard and ledger figures use account-global transcript totals.

```
 ⌘ query-engine                    ~/code/query-engine    ← workspace identity

 ◎ 91                          ◇ 32M ↘ 28M ↗ 3M ◌ 472M    ← sessions today · today's tokens (right)
 ¤ 16 (2)                                      $420.00    ← live agents · unread count · today's fleet usd value
 ─────────────────────────────────────────────────────
 ? 3   ! 0   ⏸ 0   ✓ 8                       ⢿ 3   ○ 2    ← make-up: attention/parked/done | working/free

▎⑂ feature ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ +127 -43    ← the worktree you're in · commits diff · lines diff
▌⣾ claude · Opus 4.8 · xhigh · 1m                $1.27    ← line 1: identity · model · effort · context window · usd value
▌  ledger refactor                                        ← line 2: session description
▌  ▣ ━━━━━━━━━━━━━━━━─────────────────────────── 38.2%    ← context window progress: how full the context window is
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                   ◔ 1m    ← token stats: filled toks in context window
▌  ⧉ subagents (2)                                        ← subagents spawned this turn
▌    ✓ Explore — locate the render seam                   ← done child: subagent kind · task
▌      ◇ 12k · Opus 4.8                          ◔ 10m    ← tokens · model · elapsed time
▌    ⠁ Explore — audit the trust hash                     ← active child: thinking head
▌      ◇  3k · Opus 4.8                          ◔  3m    ← same

 ─────────────────────────────────────────────────────
  Claude v2.1.169 · Claude Max                    ⇅ rc    ← provider · version · plan · remote-control flag

  ▐▛███▜▌  ◎ 53  ◇ 16M ↘ 13M ↗ 2M ◌ 198M       $188.88    ← today stats: sessions · tokens · usd value
 ▝▜█████▛▘ 5h ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱   ↻ 1h47m    ← 5-hour budget left, until reset time
   ▘▘ ▝▝   7d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱   ↻ 5d22h    ← 7-day budget left, until reset time

 W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88    ← week stats: sessions · tokens · usd value
 M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66    ← month stats: sessions · tokens · usd value

                                                    ? for help  ← footer help
```

The rest of this doc reads that frame zone by zone.

## Reading the glyphs

One vocabulary runs through the whole sidebar: a shape carries the meaning, color reinforces it. This is the complete legend, and the canonical home for it — every other doc points here, and the `?` overlay inside the app shows a short version in place.

**Status — the leading cell of every agent row.**

| glyph | state | meaning | needs you |
|-------|-------|---------|-----------|
| `?`   | waiting    | the agent asked something; answer in its pane | yes — yellow floor, then a continuous age heat toward red; unread rows carry the unread attention effect across the glyph, name, and description together — the flowing shimmer reserved to the lead row, every other unread row settling to a steady `bright` crest, all over a soft uniform unread card wash — until focused |
| `!`   | attention  | a failed turn, a turn dead on a provider API error, or a working agent gone silent past the configurable stall window | yes — yellow floor, then a continuous age heat toward red; unread rows carry the unread attention effect across the glyph, name, and description together — the flowing shimmer reserved to the lead row, every other unread row settling to a steady `bright` crest, all over a soft uniform unread card wash — until focused |
| `⏸`   | paused | stopped mid-turn on a provider limit — rate-limit or overload; resumes after the provider recovers or the window resets | waiting on recovery — held amber when read; unread rows hold the steady unread emphasis until focused |
| `⢿`   | working    | running and editing — animates `⣾⣽⣻⢿⡿⣟⣯⣷` in clay | no |
| `⠁`   | thinking   | running, before the turn's first file edit — animates `⠁⠂⠄⡀⡈⡐⡠⣀⣁⣂⣄⣌⣔⣤⣥⣦⣮⣶⣷⣿⡿⠿⢟⠟⡛⠛⠫⢋⠋⠍⡉⠉⠑⠡⢁` in clay | no |
| `⠙`   | resolving  | a resolver is answering on the bridge — braille spin | pending, being handled |
| `○`   | idle       | alive, nothing to do | no |
| `✓`   | done       | finished cleanly | no; an unread result is a look, not the lead — a soft completion cue announces it, then it settles to the steady `bright` crest across the glyph, name, and description, over a faint green card wash, until focused, never the flowing shimmer |
| `○`/`⢿`   | process | a pane with no agent (shell, editor); idle shows the hollow neutral `○`, real work the `⢿` spinner in the working clay — the whole row one soft step below the agent cards, never a cockpit tally | no |

Three short-lived heads ride over the base status on the leading cell, so they never earn a cockpit bucket of their own:

| head | meaning |
|------|---------|
| `⠁` thinking | the running turn before its first file edit — the agent is reasoning and reading, not yet writing; a research turn that never edits stays in the thinking animation end to end |
| `▇` compacting | condensing its context window — pulses `▁▃▄▅▆▇▆▅▄▃` in violet, then returns to its resting head |
| `⢄` waiting on subagents | the main agent delegated to its children; the work is in the rows below — a clay braille wave (`⢄⢂⢁⡁⡈⡐⡠`) |

Every running agent — whichever head rides its cell — counts as **working** (`⢿`) in the cockpit make-up. The status-head frames, base colors, effects, and speeds are themeable per machine through `[sidebar.animations]` ([theme](../reference/theme.md#animations)).

The two actionable attention glyphs (`?` / `!`) **carry the unread attention effect**: a light beam shimmering across the row by default, pulling the eye to an unanswered row. They also **heat with the age clock** — a yellow floor immediately, then a continuous OKLab slide from warn through caution to alarm between 15 minutes and the hour. That is the same age ramp the `◔` age glyph beside them wears, so a fresh ask reads calm-urgent and a long-ignored one visibly heats up. The effect paces with age on one clamped cadence: a fresh ask flows slowly, an older one faster, and red heat fastest of all. The continuous signal is reserved for the **one row that most needs you** — the oldest unread row awaiting an answer, the `␣` triage head: its lead glyph, card name, and description carry the shimmer (or `blink`) together, always bold, until its pane is focused in any tab. Every other unread row — a recovered working/idle row, a paused row, and an unread result included — settles to the steady `bright` crest, held bold and unmistakable against the calm rows around it after the transition cue that already announced it, so one pane is the only thing in motion and the eye lands on where to go next. Each unread card also grounds on a soft, uniform wash — one panel marking the row as unseen the way a mail inbox shades an unread line, with the status carried by the `?`/`!`/`⏸`/`✓` glyph. It is a lighter tint of the selection blue: the `selection_bg` panel lifted in lightness with its cool hue held, landing on the same cool-blue family the scheme derives for the selection band, one clear step brighter, so it reads at a scanning glance where the one-cell glyph is too small to. The selected card keeps the attention through its bright `▌` spine and recessed band, so the brighter unread fill is the "needs you" surface without reading as selection; calm rows stay clean, the selection band wins when a card is both selected and unread, and the wash clears when the pane is focused. `[sidebar.animations] unread` swaps the lead row's flowing shimmer for a constant `bright` or a hard 2-pole `blink` ([theme](../reference/theme.md#unread-attention)). A working agent gone silent past the configurable stall window (30 minutes by default) escalates to an attention `!`; the exception is a provider kind with a spent, unreset window, which pauses instead. The `⏸` paused head is attention-class but parked: it holds still in a held amber and never heats while read, because waiting for provider recovery is the only move. A parent waiting on subagents is exempt from the stall escalation — its quiet wave is the children's work. An idle agent with no prompt yet shows a static `...` on line 2 in place of the em dash.

On a truecolor terminal the unread attention effect rides an OKLab lightness lift on the glyph, name, and description, brightening toward a crest held bold: the default shimmer flows that crest across the lead actionable row, `bright` holds it, and `blink` hard-toggles to it. In 256-color mode the tones quantize to the nearest xterm step, and under `NO_COLOR` the effect rides the weight alone — a held bold for `bright`/`blink`, a moving bold cell for the shimmer — while the `?`/`!`/`⏸`/`✓` shape carries the meaning. Brief color flashes mark the moments of change: a card lights as it enters `waiting`/`failed`, settles green as its ask resolves, a pause lifts, or a turn finishes cleanly, fades in on arrival, and the `▌` spine flicks under a freshly landed selection. The base pulse follows `[sidebar.theme].mode`; `[sidebar] glow` gates only the transition-flash tier. `NO_COLOR`, a `glow = "never"` opt-out, or a terminal where truecolor is unavailable still reads the same shapes and the deeper weight-based fallback. `glow = "always"` forces transition flashes on a truecolor terminal whose `COLORTERM` an SSH hop dropped; `[sidebar.theme].mode = "truecolor"` is the matching lever for the base palette (mechanics in [internals/sidebar/sidebar.md → The runtime loop](../internals/sidebar/sidebar.md#the-runtime-loop)).

**Window — the model's context window on the identity line.**

A lowercase magnitude token (`258k`, `1m`) closing the capability cluster: the live out-of-band reading (Claude's statusline, Codex's app-server) when one exists, else the hook-derived window, omitted until a source names it. Dim-weight chrome — a capability label, not a status signal — tinted by size class so the magnitude reads at a glance: bright cyan for a 1m+ window, blue at 258k, muted gray at 128k, the plain dim weight below; the context meter's severity ramp keeps the loud color slot.

**Meters and stats — one grammar everywhere.**

| token | reads as |
|-------|----------|
| `▣ ━━━━╸━──── 38.2%` | context meter — how full the window is, bar fills as used; an empty 0% window reads the hollow `▢`. Glyph, bar, and the `▤` head below share one health tone from the OKLab ramp — healthy green at rest, then gold, orange, and alarm red through the configurable `[sidebar.context]` bands. The dominant cache-read run carries that current tone; narrow `╸` caps separate trailing accents for cache-write and fresh input |
| `▤ 76k`           | filled context — the absolute tokens in the window (the `▣` meter's numerator); leads the card's context line in the meter's severity tone |
| `◇` `↘` `↗` `◌` / `◍` | token markers: fleet lines read total · input (including cache creation) · output · cache-read; the card context line adds `◍` for per-call cache-write. Marker tones stay stable: `◇` total is blue; input `↘` is a vermilion warmed past the caution amber toward alarm — the costliest read, yet kept under the danger red so red stays exclusive to danger; cache-write `◍` is the compaction/delegation violet; output `↗` is green; cache-read `◌` is cyan — so the card's context line legends the bar and the cockpit/dashboard/ledger lines speak the same vocabulary; the figures beside them read at the soft middle tier on the fleet lines and subagent rows, and a step deeper at the dim chrome on the card's context line, so the colored markers carry it |
| `↻ N`             | context compactions — the agent card's lifetime count of completed context-window compactions, shown from the first; yellow, trailing the context line after a `·` |
| `◔ 1m`            | last-activity age — shown only once it crosses a full minute; the clock face fills by the quarter hour (`◔` ≤15m · `◑` ≤30m · `◕` ≤45m · `●` ≤60m · `◉` past it) and its tone stays dim through `◔`, then slides continuously from warn through caution to alarm by the hour — a red age means resuming likely re-reads the whole context uncached. On a subagent line the same face and ramp read the child's elapsed work, as a fixed three-cell `m`/`h` label (`<1m` under a minute, never seconds) |
| `C  11%`          | CPU utilisation of the pane's foreground process; the three resource stats ride working or stuck process rows only, while an idle shell/editor row stays bare. They appear as one fixed-width grid — each marker in its own DIM-weighted tone (`C` sky · `M` sage · `⇅` violet), each figure right-aligned in its dim slot, and the whole cluster appears only after CPU, memory, and I/O all have values — so the cluster reads whole from the first visible reading and never shifts as values change |
| `M 512M`          | resident set size (RSS) — `k` / `M` / `G` |
| `⇅   3M/s`        | combined VFS I/O rate (rchar + wchar bytes/s) |
| `+127 -43`        | lines added / removed against the trunk — committed, staged, unstaged, and untracked content all counted |
| `⇡3 ⇣1`           | commits ahead / behind the trunk (worktree header; zero components drop) |
| `≡ main`          | worktree IS the trunk tip — zero ahead, zero behind, zero diff, clean tree (no uncommitted changes, no untracked files); the trunk worktree itself never wears it |
| `✓ main`          | worktree holds no work of its own — zero ahead, zero diff, clean tree — but the trunk has moved on: done, safe to remove |
| `●●●○○ 3/5`       | todo progress |
| `$1.27`           | spend — dollar green, always two decimals; omitted while a session's cost still rounds to zero |
| `▰▱` / `▱▱` / `ex` / `api` | provider account bar: included-window budget (`5h`/`7d`/`30d`, fill = left), unknown budget as a dim empty track, paid extra usage as `ex`, and API-key spend as `api`; a value of `∞` means uncapped or not reported |
| `↻ 2h06m`         | when a provider budget resets; a sustainable burn pace keeps the marker soft, then it slides continuously from gold through amber to red as spend outruns the window, staying soft when pace is unknowable. The same glyph appears as `↻ N` on an agent card's context line for completed compactions, disambiguated by zone and by bare integer versus duration |

**Structure and chrome.**

| mark | meaning |
|------|---------|
| `⌘ name`        | the workspace |
| `¤ N`           | the live agents in the room right now — the glyph in the agents' working clay |
| `◎ N`           | sessions (threads) that have run today (cockpit) / in the window (ledger) — teal in both |
| `⧉ N`           | the subagents an agent spawned this turn (expanded card) — the marker violet, the label soft |
| `⑂ name`        | a group header with a git story — a worktree's live branch, or a directory room's child repo |
| `name` (bold)   | a directory room's own pod — name-only, no git story |
| `▎`             | the selection lane — the worktree you're in, a dim selection-tone bracket |
| `▌`             | the selected card's bright selection-tone spine, over a dark `selection_bg` band that fills the whole card — at truecolor the band recesses a flat step darker, so the card reads as one recessed panel |
| `┄ external ┄`  | out-of-project panes (scripts, CI, stray shells) |
| `─`             | a section hairline |
| `▐` / `▕`       | the cards' scrollbar — thumb / track, riding the right margin while the overflowing viewport is moving, settling away about a second after the scroll stops (`[sidebar] scrollbar` pins or removes it, [configuration](../reference/configuration.md#sidebar-rendering)) |
| `┤ Tab ├`       | an active tab under `NO_COLOR` — the caps carry the pick by shape when the fill drops; make-up buckets keep their fixed cells and mark the pick with reverse video |
| `⇅ rc`          | remote control is on for that provider |
| `⇄ remote 210ms` | remote SSH link badge in the footer — RTT EWMA; loss appears only above `10%`, and `⇄ remote ?` means the last stats are stale |

Remote-link badge tones are color-only: a calm link stays soft gray, then latency and loss slide it continuously from gold through amber to red, bold at the critical end. Under `NO_COLOR`, the numbers carry the state. The badge pins to the footer's left edge when it fits, and `? for help` pins to the footer's right edge.

## Zone 1 — the cockpit

The top block. Fixed height, so the rows below it never jump as agents change state. Top to bottom it answers: *whose room is this, what's it costing, who needs me, and what has the fleet done.*

```
 ⌘ query-engine                    ~/code/query-engine

 ◎ 12                          ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 6 (2)                                         $4.20
 ─────────────────────────────────────────────────────
 ? 2   ! 1   ⏸ 0   ✓ 0                       ⢿ 2   ○ 1
```

- **Identity.** The workspace name behind `⌘`, with the project path dim on the right edge (home-abbreviated to `~/…`; it left-truncates with a leading `…` before it ever crowds the name). A blank line sets it apart from the summary below.
- **Summary — who's here and what today burned.** Two lines, each a colored glyph and soft-tier count on the left with today's numbers pinned right. Line 1 is the day at a glance: `◎` (teal) the sessions (threads) that have run today in this room, with the room's accumulated token breakdown — `◇` total · `↘` input, including cache creation · `↗` output · `◌` cache-read, each marker in its one color — pinned to the right edge in the coarse integer form (it drops when today recorded no tokens, leaving `◎ N` alone). Line 2: `¤` (the agents' working clay) the live agents in the room right now, followed by a steady unread count like `(2)` when unseen rows exist, with the room's spend pinned right. The counts read from the live room and the workspace-scoped JSONL tally's today window. An empty room reads `◎ 0` over `¤ 0`.
- **Today's spend.** The room's workspace-scoped spend for today, pinned to the right of the live-agents line, **counting up** in an eased odometer roll the moment any in-scope agent's cost moves — every jump lands inside 1.2s of 200ms clicks, big first steps easing into a penny-sized landing on the exact figure, with a brief brighten as it settles. It joins the line once the room records spend.
- **Scope paths.** The workspace scope is path-prefix based over the project root and grouped worktree roots; a checkout reached through a different symlink spelling than the transcript's `cwd` can read as outside the room until the paths agree.
- **The make-up — split by who might want you.** The **left cluster** is worth a glance: `?` waiting and `!` failed each wear their oldest row's continuous age heat over a yellow floor, then `⏸` paused and `✓` done. A bucket echoes the continuous signal only when it owns the lead unread row — the one row that most needs you — so `?` or `!` flows on that row's age-paced cadence while every other unread bucket, `⏸` and `✓` included, holds the steady `bright` crest; with no unread match a bucket holds static, `?`/`!` resting on their oldest row's heat, `⏸` at its parked tone, and `✓` at its done tone. The unread count holds steady on the `¤` summary line. The **right cluster** is the live-capacity tail: `⢿` working (every running agent — the thinking animation and the compaction pulse are per-row heads, not buckets), then a free `○` idle agent; either bucket holds the same steady unread emphasis while a visible recovered row is unread. Every bucket always shows; colored statuses wear their semantic tone, the idle glyph and count rest at the soft stat tier, and zero counts use the same soft tier beside their glyphs.
- **Each non-zero bucket is click-to-filter.** Clicking a bucket narrows the agent cards to that status; clicking it again — or answering the bucket down to zero, or jumping to any card — returns to all, and a zero bucket is inert. Keyboard filters mirror the buckets and add the unread lens: `u` unread, `q` waiting, `!`/`e` attention, `p` paused, `d` done, `w` working, `o` idle, `a` all. The waiting key is `q` (question), leaving `?` as the footer's help key. The picked bucket paints as a padded chip for colored statuses — dark ink on the fill, bold, with one space on each side like the dashboard tab — while idle keeps the soft stat gray and adds reverse video and weight; the unread lens is keyboard-only and leaves the cockpit buckets unpicked. Under `NO_COLOR`, reverse video marks the same fixed `glyph count` cells. The counts always span the full room, filtered or not, so the line stays the room's honest tally while the body narrows.

An **empty room** has no make-up line at all — just identity and the `◎ 0` / `¤ 0` summary:

```
 ⌘ query-engine

 ◎ 0
 ¤ 0
 ──────────────────────────────────────────────────────────
```

## Zone 2 — the agent cards

The body: one card per pane, grouped under the worktree it lives in. A worktree is total isolation — only same-worktree agents collaborate — so each group reads as one bounded block.

While a [make-up bucket](#zone-1--the-cockpit) or the unread lens is picked, the body shows only the matching cards: non-matching rows, process rows, worktree groups left empty, and the `+K more` line all step aside until the pick clears.

**The cards scroll between the pinned zones.** When the cards outgrow the pane, they scroll between the cockpit above and the provider dashboard below — both stay put — and a thin scrollbar rides the right margin while the viewport is moving: a solid `▐` thumb over a hairline `▕` track, the position carried by shape so it reads under `NO_COLOR`. The bar follows the motion — a wheel scroll or the selection-driven auto-follow — and settles away about a second after the view stops, so a resting column stays clean; `[sidebar] scrollbar = "always" | "never"` pins it up or removes it ([configuration](../reference/configuration.md#sidebar-rendering)). The viewport follows the selection: picking any row — `j`/`k` or arrows, `J`/`K` worktree jumps, a click landing, `␣` — brings its card, expanded subagent list included, fully into view, and a card taller than the window pins its first line to the top. The mouse wheel scrolls the viewport freely without moving the selection — peek anywhere; the next selection change snaps the view back to the selected card. `?` reveals the help overlay at the zone's tail whatever the scroll position, and the view holds there while the overlay is open.

### The card

An agent is a small stacked card. The standard resting card is four lines; selecting it appends any subagents. Selection never reshapes a standard card line already on screen — it only *appends* and lights the spine — so the card never reflows as it expands.

`[sidebar] card_density` tunes that body without changing routing: `auto` uses the standard card, `expanded` shows subagents on every parent card, and `compact` trims resting cards by status while the selected card opens to the full standard shape. Compact resting cards read idle as identity only, running/waiting as identity + description + meter, and paused/done/failed as identity + description.

idle:

```
○ claude · Opus 4.8 · xhigh · 1m                         ← line 1 — glyph · name · model · effort · window
  refactor auth module                                   ← line 2 — what it's on
  ▢ ───────────────────────────────────────────    0%    ← context meter — empty window
  ▤ 12k · ◌ 10k ↘ 2k                             ◔ 4m    ← context line — filled window · composition · age
```

complete:

```
✓ claude · Opus 4.8 · xhigh · 1m                $2.14
  ledger refactor
  ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━───────────── 78.4%
  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                   ◔ 8m
```

- **Line 1 — identity.** The animated leading cell, then the agent kind in its provider brand color — its lightness receding a touch on a calm unselected card so the hue stays true while it rests quieter, mid-gray chrome for unknown kinds — then capability: model, effort, and the context-window token (`258k`, `1m`) at the dim chrome — metadata a step under the soft stat figures, so the brand-colored name and the task line carry the card — over `·` seams of the same weight, the window tinted by size class (bright cyan 1m+ · blue 258k · gray 128k · dim below). The idle lead `○` rests at the soft stat tier, matching the cockpit idle bucket while staying quieter than status colors. Capability degrades by width — a wide card carries model · effort · window, a medium one drops effort, a narrow one keeps just the name. The session `$cost` pins right, counting up in the cockpit headline's eased odometer tick with the same settle brighten, and joins the line once the session has actually spent — an idle agent at `$0.00` shows nothing.
- **Line 2 — what it's on.** Codex uses app-server thread metadata first: `preview`, then thread `name`. When Codex has no thread metadata, and for other agents, the row falls through to the named session (`--name` / `/rename`), then the agent's task, then its latest prompt (which lingers once the turn ends and the task clears, so an unnamed session stays labelled until it earns richer metadata). An idle agent with nothing to show yet shows static `...` here instead of an em dash. A turn that died on a provider API error takes the line over with the upstream error text, soft (`API Error: Overloaded`), for as long as its `!` escalation holds — the card says why without a jump ([internals/agents/agent.md → displayed status](../internals/agents/agent.md#displayed-status)). On a wide card, todo dots pin right.
- **The context meter (`▣`/`▢`).** The resting card's one bar — `▢` hollow at an empty 0% window, `▣` once anything fills it. The bar fills as the window is *used*; its value is always the percent used. Glyph, bar, and the `▤` head wear one health amount — healthy green at rest, then gold, orange, and alarm red through the tunable `[sidebar.context]` bands ([configuration.md](../reference/configuration.md#sidebar-bands)). The bar shows *where* the window went at every fill level: the dominant cache-read run carries the current health tone, then narrow `╸` caps separate flat accents for cache writes (`◍`) and fresh input (`↘`). A row with no per-call split yet paints one flat health run.
- **The context line.** Part of the resting card, and the meter's absolute companion. `▤` is the filled part of the window — `input + cache-write + cache-read` of the latest API call, exactly the numerator the `▣` percent scales, wearing the meter's severity tone so the bar and this figure read as one measurement. After a `·` seam comes the call's composition, ordered by how the window filled: `◌` read back from cache, `◍` newly written to cache, `↘` fresh input, `↗` output generated (which joins the window next turn). Each marker takes its bar-segment color so the line doubles as the bar's legend, the figures a step deeper in the dim chrome so the markers carry it. A zero column drops whole — the line shows what filled the window — so a Codex card, whose protocol reports no per-call cache-write, simply never grows a `◍`. The composition columns stay disjoint per call — unlike the fleet lines, whose `↘` input subsumes cache-write — and the `◇` totals stay fleet vocabulary, because this line answers what is in the window, not what today burned. A completed-compaction count joins as yellow `· ↻ N` from the first compaction, so a condensed session shows its re-read history at a glance. A coarse last-activity age pins right once it crosses a full minute — a just-active agent shows the line alone, left-aligned, rather than a misleading `1m`. The age is the clock-fill glyph (`◔`→`◉`, the legend's quarter-hour face), and its tone stays dim through the first quarter hour, then slides continuously through the legend's age ramp to red by the hour, so a red age warns that resuming pays for the context again. A delegating parent's age reads the freshest of its own and its children's activity, so it stays honest while the work is theirs. Claude legends the split from its statusline; Codex from its rollout's per-call usage on the lifecycle rail. An agent whose context carries no per-call split yet shows the bare `▤` rollup total alone — any agent before its first API call, or a statusline-fed card right after `/compact` (a rollout-fed split refreshes with the next call instead).

The `▣`/`▢` and `▤` glyphs share one lead column, so the card reads as an aligned grid.

**Selection.** In `auto` and `expanded`, the resting card is the four lines above. Selecting any row lights the bold `▌` spine and *appends* the agent's subagents beneath — it never reshapes a line already on screen, so the card never reflows:

resting:

```
 ⣾ claude · Opus · 1m                           $1.27
   ledger refactor
   ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━──────────── 78.4%
   ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                  ◔ 1m
```

selected — only appends, never reshapes
```
▌⣾ claude · Opus · 1m                           $1.27
▌  ledger refactor
▌  ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━──────────── 78.4%
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                  ◔ 1m
▌  ⧉ subagents (1)                                       ← appended
▌    ⢿ Explore
```

The expanded card also lists any **subagents** the agent spawned this turn. A `⧉ subagents (N)` header (the marker violet, the label soft) opens the list, then one entry per child in spawn order (creation time ascending, stable across refreshes). Each entry leads with the same live head an agent row wears — the `⠁` thinking animation while the child reasons, the `⢿` working fill while it acts, the static verdict once it lands — followed by the child's type and what the parent asked it to do. A deeper-indented second line carries the child's metadata: token spend `◇` (the card's whole-unit figure, never a decimal), model, and reasoning effort. That line is a per-card grid — the figure right-aligned, the model padded to the widest sibling, a missing field blank-filling its slot — so the metadata stacks into columns across children. Elapsed work pins right under the parent's stats: the clock-fill glyph (filling with the child's worked span) over a fixed three-cell `m`/`h` label (`<1m` under a minute, never seconds), toned by the parent's age ramp, so a long-running child visibly heats up. A finished child holds its `✓` (or `!`) on the list until the parent's next turn clears it:

```
▌  ⧉ subagents (2)
▌    ⠁ Explore — locate the render seam
▌      ◇ 12k · Opus 4.8                         ◔ 14m
▌    ✓ review — audit the trust hash
▌      ◇  3k · Haiku 4.5 · high                 ◔ <1m
```

The description, tokens, and elapsed time ride in from Claude's `subagentStatusLine` (Claude-only, harvested at install time). The model, effort, and turn phase come from the child's own lifecycle events, so siblings on different models read apart at a glance and a reasoning child uses the same thinking animation its parent would. (Claude reports a child's effort on its `SubagentStop`, so the effort token typically joins the line as the child finishes.) A child with none of these — a Codex child, or a Claude child before its first render — shows just the `glyph type` line. Subagents have no pane of their own, so they never get a row; they nest here only.

### Attention rows

A waiting or failed agent is the whole point. Its glyph leads, bold, and the card rises to the top of its worktree:

```
▎⑂ feature-migration ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌! claude · Opus · 1m
▌  db migrate
▌  ▢ ──────────────────────────────────────────    0%    ← context meter — empty window
```

A `?` waiting row reads the same with a `?` glyph. The row carries *who* needs you and *what task*, and selecting it lands you in the agent's pane, where the full prompt and its safe defaults live — that is the row's job, to route you there. A script's `feed ask` is the one item answerable from the sidebar itself: it chose Rimz as its surface, so its declared options render as buttons on the row.

### Process rows

A pane no agent has stamped reads like a slim agent card, one soft step quieter: a hollow neutral `○` for an idle shell or editor, the `⢿` spinner in the working clay for a pane doing real work (a build, test, or install), and the program name in the soft middle tier. That step is the boundary. Inside a worktree group the process rows settle below the agent cards, and the slight recession — falling back to DIM weight under `NO_COLOR` — reads them as the group's command tail rather than more agents. An active pane anchors its primary line on the shell that owns it, so the line stays put as commands come and go, and carries the live command in full on a second line. At wide (L2) widths a working or stuck row pins a resource grid right on line 1 — `C  <n>%  M <n>[k/M/G]  ⇅  <n>[k/M/G]/s`, CPU, RAM, and combined VFS I/O rate; an idle `○` shell or editor stays bare even when sampled values exist. Each marker takes its own DIM-weighted tone (`C` sky, `M` sage, `⇅` violet) with its figure right-aligned in a dim slot, and the whole cluster waits until CPU, memory, and I/O all have values, so it reads whole from the first reading and never wanders as values change. It rides the same right slot an agent card gives its `$cost`, so active resource load reads at a glance without leaving the sidebar. The stats are process-row vocabulary; an agent card keeps that line for its identity and cost:

```
○ zsh
⢿ zsh                        C  34%  M 512M  ⇅   8M/s
    cargo build --release
```

The label is the program the pane runs, read past a `sudo` wrapper and through a `node`/`npx` launcher (`sudo npm install -g @openai/codex` is an `npm` install, not a codex agent; `node …/codex` is codex). No status, no meter, never counted in the cockpit — it is presence, not a cue. It is still a jump target, and the moment an agent's hook stamps that pane it becomes that agent's card.

### Worktree groups and the selection lane

Worktrees stack as bounded blocks under quiet neutral headings, so the worktree names organize the column without competing with attention or the selection. The worktree **holding your selection** reads as a single bracketed lane painted in the selection tone at three intensities: a dim `▎` spine and dotted `┄` seal down its header and every row, then the selected card lit with a bright `▌` spine over a dark `selection_bg` band that fills the whole card — subagents included — so the selected block reads as one recessed card. At truecolor the band recesses a flat step darker than `selection_bg`, giving the card real depth; at 256-color the band stays a flat fill, and under `NO_COLOR` it drops and the bright spine and bold weight carry it. Every other worktree carries a blank gutter, so the lane and band are the only selection markers on screen and the pane you're in is unmistakable.

The worktree header carries the worktree's git story on the right: the `⇡`/`⇣` commit delta against the trunk, then the worktree's total diff (`⇡3 ⇣1 +230 -23`, zero components dropped) — untracked file content counts into the `+`, so work `git diff` can't see still reads as work. A worktree holding nothing of its own — zero commits ahead, a zero diff, and a clean `git status` (no uncommitted changes, no untracked files) — collapses the whole cluster to a landed marker: `≡ main` when it sits exactly at the trunk tip (this checkout IS main), `✓ main` once the trunk has moved on — done, safe to remove; behind never blocks removability, it only picks the marker. The trunk worktree itself wears neither — "landed on itself" says nothing, so its header keeps the plain cluster. The trunk is auto-detected (`main` → `master` → the remote's default) and overridable per machine ([configuration](../reference/configuration.md#sidebar-rendering)).

```
▎⑂ feature-migration ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ⇡3 ⇣1  +230 -23    ← in flight: commits ahead/behind, then the diff
▎⑂ feature-current ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ≡ main    ← at the trunk tip: this checkout IS main
▎⑂ feature-landed ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ✓ main    ← landed, trunk moved on: safe to remove
```

```
▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄    ← selected worktree: lane spine + dotted seal
▌? claude
▌  permission
▌  ▢ ──────────────────────────────────────────    0%
▎⣾ codex · GPT 5.5
▎  add tests
▎  ▢ ──────────────────────────────────────────    0%

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄    ← out-of-project panes, attention-only tally
 ? deploy.sh
   Deploy staging?
```

The `external` block is the catch-all for panes outside the project — untethered scripts, CI, stray shells. It renders as a dim divider rather than a worktree header, always sorts last, and keeps an attention-only `? n` / `! n` tally so an out-of-project ask still surfaces from the tail.

A [directory room](../reference/cli.md#start-and-attach-a-workspace) groups the same way with its child repos as the pods: each child repo keeps the full `⑂` header with its own per-repo git story, while the panes the room root itself holds sit under a **name-only header** — the directory's basename in bold, no fork glyph, no git cluster, because a plain directory has neither a fork nor a trunk to measure against. It is still a jump target and still wears the selection lane.

```
▎⑂ query-engine ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ⇡2  +12 -3    ← a child repo: full pod header, per-repo stats
▌⣾ claude
▌  db migrate

 agents                                                  ← the room's own pod: name-only, no git story
 ○ zsh
```

**Ranking is automatic: the most attention-hungry rises, nothing else moves.** Within a worktree, rows sort `waiting → failed → paused → done → working → idle` (a parked idle agent is the least needy, so it settles to the bottom — which is exactly where a freshly-launched agent appears). Attention rows sort oldest-first, so the longest-overdue is always on top. Worktrees themselves sort by their most-urgent member.

**The cap.** Each worktree shows a capped number of rows (configurable) with a dim `+K more`. The cap trims only the idle/process tail; active, blocked, paused, finished, and focused rows stay visible:

```
▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄    ← selected worktree: lane spine + dotted seal
▌⣾ codex
▌  task-0
▌  ▢ ──────────────────────────────────────────    0%
▎⣾ codex
▎  task-1
▎  ▢ ──────────────────────────────────────────    0%
▎  +3 more
```

### Jump — the row is the link

You don't read where to go; you go. Selecting a row focuses that pane — no mux pane number is ever shown.

- `↑`/`↓` or `k`/`j` select a row; `K`/`J` select the previous or next worktree's first visible row; `↵` or `l` jumps to the selected pane.
- `1`–`9` jump by the row's visible position.
- `␣` jump to the **next thing that needs you** — unread needs-a-look rows first, oldest episode first, then read waiting/failed rows oldest first, without selecting first. One key tames a fleet; press again for the next.
- `u`, `q`, `!`/`e`, `p`, `d`, `w`, and `o` filter the body to unread/waiting/attention/paused/done/working/idle; the active filter key toggles back to all, and `a` clears to all directly.
- `←/→` switch the provider dashboard's tab when the dashboard is tabbed — a pick in place, never a jump.
- A click anywhere in a card's block jumps to it.
- The mouse wheel scrolls the card list without moving the selection; the next selection change snaps the view back to the selected card.

## Zone 3 — the provider dashboard

The budgets are account-scoped — every session of a provider shares one account's budget — so they leave the rows for a pinned panel at the bottom. `provider_tabs = "auto"` stacks one or two accounts as visible blocks and switches to a tab rail at three or more; `always` tabs whenever more than one provider is present; `never` stacks every provider block. `provider_list` picks which provider kinds show and their order, including the `"all"` token for "the rest here." In tabbed mode, the panel's top hairline becomes a tab rail naming every provider: each label in its brand color at full strength, the active tab a brand-filled bold chip set into the line. The rail's glyphs are identical whichever tab is active, so a switch moves color and weight alone — and under `NO_COLOR`, where the fill drops, `┤ ├` caps notch the active tab instead, carrying the pick by shape. One account's block paints at a time, so the budgets read one account deep instead of stacking. The rail names the account, so the block's header drops the product name and reads plan-first (`Claude Max · v2.1.169`), indented to start over the `◎` stats column. While no source has reported the binary version yet, the header shows `v?`; the plan label stays absent until the account surface names one. The active tab **follows the selected pane's provider** — select a codex pane and the dashboard reads the ChatGPT account behind it; a process pane falls to the first tab. `←`/`→` or a click on a tab label picks one by hand; the pick holds until you select a pane of a *different* provider, then the follow-the-selection default takes over. A single account keeps its bare block. Every account that is logged in but idle this run still earns its block, so your accounts and budgets show even between turns. A metered provider normally shows the shortest and longest included windows it reports — both Claude and Codex a 5-hour and a 7-day today — and swaps a spent included row's partner to `ex` when paid extra usage becomes the useful next reading. Out-of-band account usage keeps those rows useful between turns: Claude refreshes from its local OAuth usage endpoint; Codex refreshes from the app-server first and falls back to its local OAuth usage endpoint when the app-server cannot supply account usage.

Each block's stats line speaks the fleet ledger's vocabulary, scoped to the provider: today's `◎` session count, then the `◇ ↘ ↗ ◌` token breakdown, where `↘` input subsumes cache creation, with the spend pinned right.

A **metered account** drains one "mana" bar per included budget window toward its reset. The bar fills with what's *left*, sliding continuously green → gold → amber → red as it empties over a dim empty track — and a fully-spent window (0% left) flips its whole empty track red, so an exhausted budget never reads as an untouched one. Each window's label (`5h`/`7d`) wears its own bar's tone, while the `↻` reset countdown beside it wears the spend pace: soft when the current burn rate sustains to reset, then sliding gold → amber → red as it outruns the window, falling back to the quiet soft tier when pace is unknowable. A spent longer window gates the shorter ones: once the `7d` is exhausted the `5h` row is painted exhausted too — red, no countdown — regardless of its own reading, since that budget is unusable until the longer window resets. When paid extra usage is available or unknown, the second row becomes `ex`; known usage reads `$used/$limit`, a known remaining balance reads dollars, and an unknown or uncapped value reads `∞` over a dim empty track.

These are **sliding windows** that begin counting only on your first token, so until then the provider keeps sliding the reset a full window-length ahead. A window whose reset still sits ~a full window out has **not started** (it still reads ~1% used, not 0 — so it's the reset distance that gives it away). Any usage above that ~1% floor means it has already started, countdown and all; only a 0–1% window with a near-full reset qualifies. A not-started window shows a near-full bar with **no countdown**, reading "ready — send a message to start it" rather than a misleading ticking placeholder; the countdown appears once your first token fixes the reset and it begins ticking down.

When Rimz is reading only cached budgets and the longest cached window has already reset, the balance is unknown until the provider refreshes. The panel keeps the account and window labels, but every cached budget row becomes a dim empty track with no countdown, so it does not claim either a refreshed full budget or an exhausted one. A metered account whose window list has not arrived yet paints the same dim empty track with a blank label slot, preserving the budget grid without inventing a window.

Provider blocks stacked (`provider_tabs = "never"`). Claude and Codex use account-global provider totals; Pi is shown as an idle API-key block to pin its built-in emblem:

```
  Claude v2.1.169 · Claude Max                   ⇅ rc
  ▐▛███▜▌  ◎ 58  ◇ 17M ↘ 15M ↗ 2M ◌ 198M      $188.88
 ▝▜█████▛▘ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱ ↻ 1h47m
   ▘▘ ▝▝   7d  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱ ↻ 5d22h

  Codex v0.137.0 · ChatGPT Pro                   ⇅ rc
  ▗▛███▜▖  ◎ 42  ◇ 16M ↘ 15M ↗ 1M ◌ 272M      $288.88
 ▐▜▌ ▚ ▐▛▌ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱ ↻ 1h45m
  ▝▀▀▀▀▀▘  7d  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱ ↻ 3d19h

  Pi v0.78.1 · OpenAI API
  █▜███▛█  ◎ 19  ◇  8M ↘  7M ↗ 1M ◌ 142M      $420.42
 ▝▜▛▀▀▀▜▛▘ api ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱ $420.42∞
  ▝▘   ▝▘
```

Switching the tab (`→`, or a click on a provider label) moves the chip's fill onto the picked tab and swaps the block in place — the rail's glyphs are identical in both states, so the pick is pure color and not a cell moves. An **unmetered** account (an API key) shows an `api` bar: trailing-month transcript spend against an optional display ceiling, with an `∞` value when no ceiling is configured. The dashboard isn't pinned to a fixed set of windows: provider windows are labeled by their reported length, and paid usage gets a separate `ex` row only when it matters to the account's usable budget.

A **Pi block** names its version and the subscription it runs on — `Pi v0.78.1 · Anthropic OAuth` — read out-of-band from `pi --version` and Pi's auth file (the freshest session's provider picks among several credentials; [provider.md → Per-provider mapping](../internals/agents/provider.md#per-provider-mapping)). Pi reads no window surface of its own, but an OAuth sub *is* a sibling provider's account — Anthropic OAuth is the Claude account, OpenAI OAuth the Codex one — so the Pi tab paints that account's 5h/7d bars: same budget, same bars as the sibling's own tab. A sub with no sibling readings shows the unknown-track placeholder for a metered account, and an API key gets the `api` paid-usage row.

Every bar shares one start column and one end column whichever tab is active, so the dashboard reads as one aligned grid. The `⇅ rc` flag pins to the block's top-right when remote control is on for that provider — host infrastructure, never its own row. Below ~34 columns the emblem is dropped and the bars run full-width. The brand emblem, color, and name are config-driven (`[sidebar.providers.<kind>]`, see [theme.md](../reference/theme.md#provider-styling)).

### The fleet ledger

The account-global running totals seal the bottom of the dashboard, above the footer — a quiet two-row ledger you learn to glance at, never a cue that competes with the rows. The trailing-week (`W:`) and trailing-month (`M:`) rows span every provider.

```
 W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88
 M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66
```

- **The rows.** Each reads `◎ sessions  ◇ total ↘ input ↗ output ◌ cache-read  $spend`: the thread count, the precise one-decimal token figures (the exact record beside the cockpit's coarse live read) at the soft tier, and the spend pinned to the right edge in dollar green. A one-cell lead pad sets the `W:`/`M:` tags a hair off the chrome edge; the tags wear sky blue, and each marker its one shared color — the teal `◎`, the blue `◇`, the expense-vermilion `↘`, the green `↗`, and the cyan `◌`. Every numeric field is right-aligned into one shared grid, so the `W:` / `M:` labels stack and each column lines up. Cache creation is counted in the `↘` input figure, so the ledger keeps to the headline figures.
- **No animation.** The ledger figures are static — the count-ups live above, today's headline in the cockpit and each card's `$cost`. The windows escalate `today → week → month`.

Every figure is computed from the transcript JSONL — Codex's dollars priced from its token counts, every provider that logs usage counted, all of them account-global. The ledger is dropped until something has been recorded.

## Bottom chrome

Pinned to the bottom edge, below all three zones. The body is truncated before this chrome is ever clipped, so it can never scroll off.

**Footer.** Faint chrome — the deepest legible gray, receding to pure scaffolding. It is just `? for help`, pinned to the bottom-right edge:

```
                                                   ? for help
```

**Pane-source notice.** When the producer repairs a partial pane read by carrying live panes from the prior frame, a dim line appears above the footer while the room stays interactive:

```
 ⚠ pane source degraded · 2 carried panes · 8s
```

When the renderer is holding a successful-but-regressive fetch behind the last good frame, the same layer says why the rows are intentionally stale:

```
 ⚠ pane updates held · empty pane frame
```

These notices clear when the next accepted pane frame lands. A health alert takes over the bottom line while a fetch failure is active.

**Help overlay** (`?`). The legend and keys, in place, in the faint chrome tier — summoned reference, not live state:

```
 keys & legend
 move     j/k rows   J/K worktrees
 focus    l or ↵     1-9 direct
 accounts ←/→ tabs
 filter   q waiting  !/e attention
 system   r reload   x dismiss
 help     ? close
 ⢿ working   ⠁ thinking   ? waiting
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

The renderer's golden tests in [`crates/rimz/src/sidebar_pane/render/`](../../crates/rimz/src/sidebar_pane/render/) are the machine-checked source of truth for these frames — `cargo xtask test` re-renders each scenario and diffs it against a committed `.snap`. The frames in this doc are drawn from those scenarios:

| scenario | golden test |
|----------|-------------|
| narrow card | `l0_density_minimal_row` |
| capability + window | `agent_capability` |
| selected, enriched card | `enriched_selected_agent_card` |
| card context line + age pin | `agent_card_context_age` |
| card context line + compactions | `agent_card_context_compactions` |
| codex card composition (no `◍`) | `codex_card_context_composition` |
| process row + resource stats | `process_row_resource_stats` |
| agents + dimmed process tail | `agents_process_tail` |
| subagent list + elapsed column | `subagent_two_line_entry` |
| worktree grouping + external | `worktree_attention_map` |
| equal-to-trunk header (`≡`) | `worktree_equal_to_trunk` |
| clear worktree header (`✓`) | `worktree_clear_safe_to_remove` |
| dirty tree blocks the markers | `worktree_dirty_tree_keeps_the_cluster` |
| per-worktree cap | `group_cap_with_overflow` |
| cockpit unread count | `cockpit_unread_count` |
| make-up bucket picked, body filtered | `make_up_filter_failed` |
| unread lens picked, body filtered | `make_up_filter_unread` |
| cards overflow, scrollbar mid-scroll | `scroll_overflow_shows_bar` |
| selection-driven scroll to bottom, bar settled away | `scroll_offset_follows_selection_to_bottom` |
| tall expanded card pinned to top | `scroll_pins_tall_expanded_card_top` |
| wheel pin holds the viewport | `scroll_manual_offset_holds` |
| scrollbar hidden once settled | `scrollbar_hides_after_settle` |
| scrollbar pinned (`always`) | `scrollbar_always_mode` |
| scrollbar removed (`never`) | `scrollbar_never_mode` |
| provider dashboard, tabbed (derived tab) | `provider_dashboard` |
| provider dashboard, manual tab pick | `provider_dashboard_codex_tab` |
| provider dashboard, stacked auto layout | `provider_dashboard_stacked` |
| fleet ledger (week/month) | `fleet_ledger` |
| health alert | `degraded_banner` |
| pane-source notice | `render_truth_degraded_notice_keeps_room_chrome` |

When the renderer changes how something looks, update the `.snap` (the test prints the diff) and this doc together.

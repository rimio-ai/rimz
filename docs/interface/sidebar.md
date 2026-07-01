# The sidebar, on screen

The sidebar is one narrow column that answers a single question: **which pane needs you, right now.** Every pane in the room is a row; agents are enriched from the ledger; everything groups by the worktree it lives in. The sidebar routes you to the pane — you read and answer in the agent's own UI.

It stacks three reading zones, top to bottom — the **cockpit** (who is here, what it costs, what needs you), the **agent cards** (one card per pane, grouped by worktree), and the **provider dashboard** (your accounts, their budgets, and the optional pet) — with bottom chrome (a footer and a health line) pinned under all three.

This page shows what the sidebar *means on screen*. Three companions own the rest: [theme.md](../reference/theme.md) is the knobs that restyle every tone, glyph, and animation; [internals/sidebar/sidebar.md](../internals/sidebar/sidebar.md) is how the column is built — presence, launch, reload recovery, the view-model; [DESIGN.md → Attention at a glance](../../DESIGN.md#attention-at-a-glance) is why it works this way. Where a behavior or a tone is produced elsewhere, this page names what you see and links the mechanism rather than restating it.

Every frame below is what the renderer actually paints — structure, glyphs, columns, and alignment exact; the live values (ages, percentages, resets, counts) illustrative. Color comes from the active palette, RGB on a truecolor terminal and quantized to xterm 256 colors on an indexed one. These frames are colorless text, so they read as `NO_COLOR` does: shapes carry every meaning, and color only reinforces.

## The whole frame at a glance

A complete frame: a selected agent in a worktree, with the per-provider dashboard pinned at the bottom. The cockpit figures use the current Rimz room's project root and grouped worktrees; the provider dashboard and ledger figures use account-global transcript totals.

```
 ⌘ query-engine                    ~/code/query-engine    ← workspace identity

 ◎ 91                          ◇ 32M ↘ 28M ↗ 3M ◌ 472M    ← headline sessions · headline tokens (right)
 ¤ 16 (2)                                      $420.00    ← live agents · unread count · headline fleet usd value
 ─────────────────────────────────────────────────────
 ? 3   ! 0   ⏸ 0   ✓ 8                       ⢿ 3   ○ 2    ← make-up: attention/parked/done | working/free
 ↑ 2 need you                                            ← unread jump banner: appears when the oldest waiting agent is scrolled out; click scrolls to top

▎⑂ feature ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ⇡3  +127 -43  ⊙ main    ← selected worktree · commits/diff · PR marker
▌⣾ claude · Opus 4.8 · xhigh · 1m                $1.27    ← line 1: identity · model · effort · context window · usd value
▌  ledger refactor                                        ← line 2: session description
▌  ▣ ━━━━━━━━━━━━━━━━─────────────────────────── 38.2%    ← context window progress: how full the context window is
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                   ◔ 8m    ← token stats: filled toks in context window
▌  ⧉ subagents (2)                                        ← subagents spawned this turn
▌    ✓ Explore — locate the render seam                   ← done child: collapses to one line
▌    ⠁ Explore — audit the trust hash                     ← active child: thinking head
▌      ◇ 3k · Opus 4.8                           ◔  3m    ← running child: tokens · model · elapsed

 ─────────────────────────────────────────────────────
  Claude v2.1.169 · Claude Max                    ⇅ rc    ← provider · version · plan · remote-control flag

  ▐▛███▜▌  ◎ 53  ◇ 16M ↘ 13M ↗ 2M ◌ 198M       $188.88    ← headline stats: sessions · tokens · usd value
 ▝▜█████▛▘ 5h ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱   ↻ 1h47m    ← 5-hour budget left, until reset time
   ▘▘ ▝▝   7d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱   ↻ 5d22h    ← 7-day budget left, until reset time

  ── Total: ────────────────────────────────────────────    ← fleet-total scope
  W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88    ← week stats: sessions · tokens · usd value
  M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66    ← month stats: sessions · tokens · usd value

                                                    ? for help  ← footer help
```

The rest of this doc reads that frame zone by zone.

## Reading the glyphs

One vocabulary runs through the whole sidebar: a shape carries the meaning, color reinforces it. This is the complete legend, and the canonical home for it — every other doc points here, and the `?` overlay inside the app shows a short version in place.

The tables show the default Unicode set. `[theme.glyphs]` can select Nerd Font or override either shipped inline set without changing the meanings; see [theme.md → Glyphs](../reference/theme.md#glyphs).

**Status — the leading cell of every agent row.**

| glyph | state | meaning | needs you |
|-------|-------|---------|-----------|
| `?`   | waiting    | asked you something; answer in its pane | yes |
| `!`   | attention  | a failed turn, a turn dead on a provider API error, or a working agent gone silent past the stall window | yes |
| `⏸`   | paused     | stopped mid-turn on a provider rate-limit or overload; resumes when the provider recovers or the window resets | on recovery |
| `⢿`   | working    | running and editing | no |
| `⠁`   | thinking   | running, before the turn's first file edit | no |
| `⠙`   | resolving  | a resolver is answering on the bridge | being handled |
| `○`   | idle       | alive, nothing to do | no |
| `✓`   | done       | finished cleanly | a look, not the lead |
| `○`/`⢿` | process  | a pane with no agent (shell, editor): hollow `○` when idle, `⢿` while it does real work — one soft step below the agent cards, never a cockpit tally | no |

Three short-lived **heads** ride over the base status on the leading cell, so they never earn a cockpit bucket of their own — every running agent, whichever head it wears, counts as **working** (`⢿`) in the make-up:

| head | meaning |
|------|---------|
| `⠁` thinking | the running turn before its first file edit — reasoning and reading, not yet writing; a research turn that never edits stays here end to end |
| `▇` compacting | condensing its context window, then returns to its resting head |
| `⢄` waiting on subagents | delegated to its children; the work is in the rows below |

Every head's shape, color, effect, and speed is per-machine theme through [`[theme.animations]`](../reference/theme.md#animations); the glyph shapes through [`[theme.glyphs]`](../reference/theme.md#glyphs).

**Attention and the unread inbox.** The two actionable glyphs hold a **fixed tone at any age** — `?` yellow, `!` red — and never heat toward a hotter color; `⏸` holds blue. What moves is the *unread* signal. A row reads unread from the moment it needs you until you focus its pane, and the sidebar marks it two ways: a soft **wash** fills the card the way a mail inbox shades an unread line, and the **one row that most needs you** — the oldest unanswered `?`/`!` — animates across its glyph, name, and description. Every other unread row, an unread `✓` result included, holds a steady bright crest instead, so exactly one pane is ever in motion and the eye lands on where to go next. The selected card keeps its own signature — the bright `▌` spine and recessed band — and wins when a card is both selected and unread.

A working agent gone silent past the stall window (`[agents.attention]`, 30 minutes by default) escalates to `!` — unless its provider window is spent without reset, which pauses (`⏸`) instead. A parent waiting on subagents is exempt: its quiet wave is the children's work. An idle agent with no prompt yet shows a static `...` on line 2 in place of the em dash.

How the wash, the crest, and the lead-row motion are produced — `shimmer` vs. `bright` vs. `blink`, the age-paced cadence, the OKLab lift, the transition flashes, and the per-depth and `NO_COLOR` fallbacks — is theme behavior: [theme.md → Unread attention](../reference/theme.md#unread-attention) and [Glow](../reference/theme.md#glow).

**Window — the model's context window on the identity line.** A lowercase magnitude token (`258k`, `1m`) closing the capability cluster: the live out-of-band reading (Claude's statusline, Codex's app-server) when one exists, else the hook-derived window, omitted until a source names it. It is dim capability chrome, not a status signal, tinted louder as the window grows so the size reads at a glance.

**Meters and stats — one grammar everywhere.** Each meter carries its value by shape, so it reads under `NO_COLOR`; the tone ramps that color it live in [theme.md → Display](../reference/theme.md#display).

| token | reads as |
|-------|----------|
| `▣ ━━━━╸━──── 38.2%` | context meter — how full the window is; the bar fills as used, `▢` hollow at 0%. The fill also shows *where* the window went: a wide cache-read run in the meter's health tone, with narrow `╸` caps separating cache-write and fresh-input accents |
| `▤ 76k`           | filled context — the absolute tokens in the window, the `▣` meter's numerator |
| `◇ ↘ ↗ ◌` / `◍`   | token markers, one stable color each: `◇` total (blue) · `↘` input incl. cache creation (deep red, the costliest read) · `↗` output (blue) · `◌` cache-read (green); the card's context line adds `◍` cache-write (violet) for the per-call split |
| `↻ N`             | completed context compactions — the card's lifetime count, from the first; trails the context line after a `·` |
| `◔ 5m`            | last-activity age — shown once it crosses five minutes, so a card stays quiet through normal churn; the face fills by the quarter hour (`◔`≤15m · `◑`≤30m · `◕`≤45m · `●`≤60m · `◉` past) and heats to red by the hour, where resuming likely re-reads the whole context uncached. On a running subagent line the same face reads the child's elapsed work as a fixed `m`/`h` label (`<1m` under a minute) |
| `C 11%` / `M 512M` / `⇅ 3M/s` | a working process row's CPU · resident memory (RSS) · combined VFS I/O rate — one fixed-width grid (`C` sky · `M` sage · `⇅` violet) that appears only once all three have values |
| `+127 -43`        | lines added / removed against the trunk — committed, staged, unstaged, and untracked all counted |
| `⇡3 ⇣1`           | commits ahead / behind the trunk (worktree header; zero components drop) |
| `≡ main`          | pristine worktree — clean, no worktree-owned commits, at the trunk tip |
| `✓ main`          | merged worktree — its work is landed and nothing remains to offer; remove it |
| `⑂ main` / `⊙ main` / `✕ main` / `⟳ main` | trunk state marker after diverged stats: plain branch, PR open, PR closed, or local rebase/merge/cherry-pick in progress |
| `$1.27`           | spend — dollar green, two decimals; omitted while a session's cost rounds to zero |
| `▰▱` / `▱▱` / `ex` / `api` | provider account bar: included-window budget (`5h`/`7d`/`30d`, fill = left), unknown budget as a dim empty track, paid extra usage as `ex`, API-key spend as `api`; `∞` means uncapped or not reported |
| `↻ 2h06m`         | when a provider budget resets; soft at a sustainable burn pace, heating toward red as spend outruns the window (the bare-integer `↻ N` above is the compaction count — same glyph, told apart by duration vs. count) |

**Structure and chrome.**

| mark | meaning |
|------|---------|
| `⌘ name`        | the workspace, with the name in the green `good` tone |
| `¤ N`           | the live agents in the room right now — the glyph in the agents' working clay |
| `◎ N`           | sessions (threads) that have run in the configured headline window (cockpit/provider) / in the ledger window — teal in both |
| `⧉ N`           | the subagents an agent spawned this turn (expanded card) — the marker violet, the label soft |
| `⋯ bg`          | an agent parked on background work — a faint secondary marker after the description, so the live status above stays honest |
| `⑂ name` / `⮌ name` | a group header with a git story — branch for pristine/diverged worktrees, merge for landed removable worktrees |
| `name` (bold)   | a directory room's own pod — name-only, no git story |
| `▎`             | the selection lane — the worktree you're in, a dim selection-tone bracket |
| `▌`             | the selected card's bright spine, over a `selection_bg` band that fills and recesses the whole card so it reads as one panel |
| `┄ external ┄`  | out-of-project panes (scripts, CI, stray shells) |
| `─`             | a section hairline |
| `▐` / `▕`       | the cards' scrollbar — thumb / track, shown while the viewport moves and settling away ~1s after it stops (`[theme.display] scrollbar` pins or removes it) |
| `┤ Tab ├`       | an active tab under `NO_COLOR` — the caps carry the pick by shape when the fill drops; make-up buckets keep their fixed cells and mark the pick with reverse video |
| `⇅ rc`          | remote control is on for that provider |
| `zᶻ idle · 17m` / `zᶻ away` | AFK presence in the footer — input has been idle for the configured idle window (`[sidebar] afk_after_secs`, 15 minutes by default) on tmux, or no terminal client is attached |
| `⇄ remote 210ms` | remote SSH link badge in the footer — RTT EWMA; loss appears only above `10%`, and `⇄ remote ?` means the last stats are stale |

The AFK badge is quiet chrome: it appears only while away, uses the muted tone, and takes the footer's left edge. The remote-link badge yields to it and appears after two spaces only when the line still fits before `? for help`.

Remote-link badge tones are color-only: a healthy link reads green, then latency and loss slide it continuously through yellow and amber to red, bold at the critical end; a warming link stays neutral until it has an RTT sample. Under `NO_COLOR`, the numbers carry the state. The badge pins to the footer's left edge in the common active case, and `? for help` pins to the footer's right edge.

## Zone 1 — the cockpit

The top block. Fixed height, so the rows below it never jump as agents change state. Top to bottom it answers: *whose room is this, what's it costing, who needs me, and what has the fleet done.*

```
 ⌘ query-engine                    ~/code/query-engine

 ◎ 12                          ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 6 (2)                                         $4.20
 ─────────────────────────────────────────────────────
 ? 2   ! 1   ⏸ 0   ✓ 0                       ⢿ 2   ○ 1
```

- **Identity.** The workspace name behind `⌘` renders in the green `good` tone, with the project path dim on the right edge (home-abbreviated to `~/…`; it left-truncates with a leading `…` before it ever crowds the name). A blank line sets it apart from the summary below.
- **Summary — who's here and what the headline window burned.** Two lines, each a colored glyph and soft-tier count on the left with headline numbers pinned right. Line 1 is the configured spend window at a glance: `◎` (teal) the sessions (threads) that have run in this room, with the room's accumulated token breakdown — `◇` total · `↘` input, including cache creation · `↗` output · `◌` cache-read, each marker in one color — pinned to the right edge in the coarse integer form (it drops when the headline recorded no tokens, leaving `◎ N` alone). Line 2: `¤` (the agents' working clay) the live agents in the room right now, followed by a steady unread count like `(2)` when unseen rows exist, with the room's spend pinned right. The counts read from the live room and the workspace-scoped JSONL tally's `[sidebar] spend_window`: `"session"` (default), `"24h"`, or `"today"` using the global `timezone`. An empty room reads `◎ 0` over `¤ 0`.
- **Headline spend.** The room's workspace-scoped spend for the configured headline window, pinned to the right of the live-agents line, **counting up** in an eased odometer roll the moment any in-scope agent's cost moves — every jump lands inside 1.2s of 200ms clicks, big first steps easing into a penny-sized landing on the exact figure, with a brief brighten as it settles. It joins the line once the room records spend.
- **Scope paths.** The workspace scope is path-prefix based over the project root and grouped worktree roots; a checkout reached through a different symlink spelling than the transcript's `cwd` can read as outside the room until the paths agree.
- **The make-up — split by who might want you.** The **left cluster** counts who needs an answer: `?` waiting, `!` failed, `⏸` paused, `✓` done. The **right cluster** is the live-capacity tail: `⢿` working (every running agent, whatever head it wears) and `○` idle. Every bucket always shows, zeros included, each in its semantic tone. A bucket carries the [unread signal](#reading-the-glyphs) only when it owns the lead unread row, so the make-up line mirrors the inbox without ever putting more than that one bucket in motion.
- **The unread jump banner — the agent that needs you, one click away.** The `↑ N need you` line appears when the lead unread card has scrolled out of view and stays hidden while that card is on screen, toned by the lead's status — yellow for a waiting lead, red for a failed one. Clicking it scrolls the card list back to the top, where the lead ranks, and pins that view; the inbox key (`n`/`Space`) jumps to and focuses the lead. It is the steady companion to the viewport's snap — when an unread arrives the cards scroll to reveal it, and once you scroll or navigate past it this banner is how you get back. It clears the moment nothing is awaiting you.
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

**The cards scroll between the pinned zones.** When the cards outgrow the pane they scroll between the cockpit and the dashboard — both stay put — and the right-margin scrollbar (`▐` thumb, `▕` track) appears while the viewport moves. The viewport follows the **selection**: picking any row brings its card, expanded subagent list included, fully into view, pinning a too-tall card's first line to the top. The mouse wheel scrolls freely without moving the selection — peek anywhere, and the next selection change snaps the view back. `?` swaps the card body for the keys-and-legend overlay while the cockpit, footer, and alert rails stay pinned.

### The card

An agent is a small stacked card. The standard resting card is four lines; selecting it appends any subagents. Selection never reshapes a standard card line already on screen — it only *appends* and lights the spine — so the card never reflows as it expands.

`[theme.display] card_density` tunes that body without changing routing: `auto` uses the standard card, `expanded` shows subagents on every parent card, and `compact` trims resting cards by status while the selected card opens to the full standard shape. Compact resting cards read idle as identity only, running/waiting as identity + description + meter, and paused/done/failed as identity + description.

idle:

```
○ claude · Opus 4.8 · xhigh · 1m                         ← line 1 — glyph · name · model · effort · window
  refactor auth module                                   ← line 2 — what it's on
  ▢ ───────────────────────────────────────────    0%    ← context meter — empty window
  ▤ 12k · ◌ 10k ↘ 2k                             ◔ 6m    ← context line — filled window · composition · age
```

complete:

```
✓ claude · Opus 4.8 · xhigh · 1m                $2.14
  ledger refactor
  ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━───────────── 78.4%
  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                   ◔ 8m
```

- **Line 1 — identity.** The animated leading cell, then the agent's handle — its team role or profile, else the kind — in the provider brand color (so a team reads `planner` / `coder` / `reviewer`; unknown kinds mid-gray), then capability: model · effort · window token, dim metadata under the brand-colored name. Capability degrades by width — wide carries model · effort · window, medium drops effort, narrow keeps just the name. The session `$cost` pins right and joins the line once the session has actually spent (an idle agent at `$0.00` shows nothing), counting up in the same eased odometer roll as the cockpit headline.
- **Line 2 — what it's on.** The label falls through a priority list until something names the session: Codex thread metadata (`preview`, then `name`) first, then the named session (`--name` / `/rename`), the launch `--description`, the agent's task, and finally its latest prompt — which lingers after the turn so an unnamed session stays labelled. An idle agent with nothing yet shows a static `...`. A turn that died on a provider API error takes the line over with the upstream error text (`API Error: Overloaded`) for as long as its `!` holds, so the card says why without a jump ([agent.md → displayed status](../internals/agents/agent.md#displayed-status)).
- **The context meter (`▣`/`▢`).** The resting card's one bar: `▢` hollow at 0%, `▣` once anything fills it, the value always the percent *used*. The fill also shows *where* the window went — a dominant cache-read run in the current health tone, with `╸` caps separating cache-write (`◍`) and fresh-input (`↘`) accents; a row with no per-call split yet paints one flat run. The health bands are tunable ([theme.md → Display](../reference/theme.md#display)).
- **The context line.** The meter's absolute companion. `▤` is the filled window — `input + cache-write + cache-read` of the latest API call, the numerator the `▣` percent scales. After a `·` comes the call's composition, ordered by how the window filled — `◌` cache-read, `◍` cache-write, `↘` fresh input, `↗` output — each marker doubling as the bar's legend. A zero column drops whole, so a Codex card (no per-call cache-write in its protocol) never grows a `◍`; these columns stay disjoint per call, unlike the fleet lines whose `↘` subsumes cache-write. A completed-compaction count joins as `· ↻ N` from the first, and the last-activity age pins right once it crosses five minutes (a delegating parent reads the freshest of its own and its children's activity). Claude legends the split from its statusline; Codex, Pi, and OpenCode use their lifecycle rail — and any agent before its first API call, or a statusline card right after `/compact`, shows the bare `▤` total alone.

The `▣`/`▢` and `▤` glyphs share one lead column, so the card reads as an aligned grid.

**Selection.** In `auto` and `expanded`, the resting card is the four lines above. Selecting any row lights the bold `▌` spine and *appends* the agent's subagents beneath — it never reshapes a line already on screen, so the card never reflows:

resting:

```
 ⣾ claude · Opus · 1m                           $1.27
   ledger refactor
   ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━──────────── 78.4%
   ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                  ◔ 8m
```

selected — only appends, never reshapes
```
▌⣾ claude · Opus · 1m                           $1.27
▌  ledger refactor
▌  ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━──────────── 78.4%
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                  ◔ 8m
▌  ⧉ subagents (1)                                       ← appended
▌    ⢿ Explore
```

The expanded card also lists any **subagents** the agent spawned this turn. A `⧉ subagents (N)` header (the marker violet, the label soft) opens the list, then one entry per child in spawn order (creation time ascending, stable across refreshes). Each entry leads with the same live head an agent row wears — the `⠁` thinking animation while the child reasons, the `⢿` working fill while it acts, the static verdict once it lands — followed by the child's type and what the parent asked it to do. While a child is **running**, a deeper-indented second line carries its metadata: token spend `◇` (the card's whole-unit figure, never a decimal) and model, with the live elapsed work pinned right — the clock-fill glyph (filling with the child's worked span) over a fixed three-cell `m`/`h` label (`<1m` under a minute, never seconds), toned by the age ramp. That line is a per-card grid — the figure right-aligned, the model padded to the widest sibling, a missing field blank-filling its slot — so the metadata stacks into columns across the running children. A **finished** child collapses to its single type line: its `✓` (or `!`) verdict and description stay on the list until the parent's next turn clears it, but the metadata row drops, since a done child has no live work span to track:

```
▌  ⧉ subagents (2)
▌    ⠁ Explore — locate the render seam
▌      ◇ 12k · Opus 4.8                         ◔ 14m
▌    ✓ review — audit the trust hash
```

The description, tokens, and elapsed time ride in from Claude's `subagentStatusLine` (Claude-only, harvested at install time). The model and turn phase come from the child's own lifecycle events, so siblings on different models read apart at a glance and a reasoning child uses the same thinking animation its parent would. A running child with none of these — a Codex child, or a Claude child before its first render — shows just its `glyph type` line, and a finished child collapses to its single type-and-description line. Subagents have no pane of their own, so they never get a row; they nest here only.

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

A pane no agent has stamped reads like a slim agent card, one soft step quieter: a hollow `○` for an idle shell or editor, the `⢿` spinner for a pane doing real work (a build, test, install), the program name in the soft middle tier. Inside a worktree group the process rows settle below the agent cards, the recession reading them as the group's command tail rather than more agents. An active pane anchors its primary line on the shell that owns it, so it stays put as commands come and go, and carries the live command in full on a second line; a working or stuck row pins the [resource grid](#reading-the-glyphs) (`C`/`M`/`⇅`) into the same right slot a card gives its `$cost`, while an idle shell stays bare even when values exist.

```
○ zsh
⢿ zsh                        C  34%  M 512M  ⇅   8M/s
    cargo build --release
```

The label is the program the pane runs, read past a `sudo` wrapper and through a `node`/`npx` launcher (`sudo npm install -g @openai/codex` is an `npm` install, not a codex agent; `node …/codex` is codex). No status, no meter, never counted in the cockpit — it is presence, not a cue. It is still a jump target, and the moment an agent's hook stamps that pane it becomes that agent's card.

### Worktree groups and the selection lane

Worktrees stack as bounded blocks under quiet neutral headings, so the names organize the column without competing with attention or the selection. The worktree **holding your selection** reads as one bracketed lane: a dim `▎` spine and dotted `┄` seal down its header and every row, then the selected card lit with the bright `▌` spine over the recessed `selection_bg` band — subagents included — so the selected block reads as one card. Every other worktree carries a blank gutter, so the lane and band are the only selection markers on screen and the pane you're in is unmistakable.

The worktree header carries the worktree's git story on the right: pristine worktrees collapse to `≡ main`, merged worktrees collapse to `✓ main`, and diverged worktrees show the `⇡`/`⇣` commit delta against the trunk, then the total diff, then a trunk marker. The marker ladder is local reconciling (`⟳`) over PR merged (`✓`), PR closed (`✕`), PR open (`⊙`), then plain branch (`⑂`). The `+/-` churn counts committed, staged, unstaged, and untracked file content, so work `git diff` cannot see still reads as work. The trunk worktree itself wears no pristine/merged verdict — "landed on itself" says nothing, so its header keeps the plain cluster. The trunk is auto-detected (`main` → `master` → the remote's default) and overridable per machine ([configuration](../reference/configuration.md#sidebar-rendering)).

```
▎⑂ feature-migration ┄┄┄┄┄┄┄┄┄┄┄┄ ⇡3 ⇣1  +230 -23  ⊙ main    ← diverged with an open PR
▎⑂ fresh-fork ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ≡ main    ← pristine: no worktree-owned commits
▎⮌ feature-landed ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ✓ main    ← merged: safe to remove
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

A [directory room](../reference/cli.md#start-and-attach-a-workspace) groups git-backed agents by the worktree root their hooks resolve, at any depth in the room: each active checkout keeps the full `⑂` header with its own git story, while the panes the room root itself holds sit under a **name-only header** — the directory's basename in bold, no fork glyph, no git cluster, because a plain directory has neither a fork nor a trunk to measure against. It is still a jump target and still wears the selection lane.

```
▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ⇡2  +12 -3            ← a git-backed row: full pod header, per-checkout stats
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

- `↑`/`↓` or `k`/`j` select a row; `K`/`J` select the previous or next worktree's first visible row; `g`/`G` select the first or last row; `↵` or `l` jumps to the selected pane.
- `n`/`N` jump to the **next/previous thing that needs you** — unread needs-a-look rows first, oldest episode first, then read waiting/failed rows oldest first — and focus it to read. `␣` is an alias for `n`. One key tames a fleet; `N` walks back.
- `1`–`9` jump by the row's visible position.
- `m`/`M` mark the selected row read / unread **without jumping** — clear a finished card from the inbox, or re-flag one to keep it on your radar.
- `u`, `q`, `!`/`e`, `p`, `d`, `w`, and `o` filter the body to unread/waiting/attention/paused/done/working/idle; the active filter key toggles back to all, and `a` clears to all directly.
- `←/→` switch the provider dashboard's tab when the dashboard is tabbed — a provider pick in place, never a jump.
- A click anywhere in a card's block jumps to it.
- The mouse wheel scrolls the card list without moving the selection; the next selection change snaps the view back to the selected card.

From any pane in the room, the configurable `Alt+p` ([configuration](../reference/configuration.md#sidebar-rendering)) focuses the sidebar and toggles back to your last working pane — the one key that reaches the room from inside an agent.

## Zone 3 — the provider dashboard

The budgets are account-scoped — every session of a provider shares one account's budget — so they pin to a panel at the bottom rather than riding the rows. How accounts stack or tab, and in what order, is display config (`provider_tabs`, `provider_list`, `max_provider_blocks`; [theme.md → Display](../reference/theme.md#display)).

In **tabbed** mode the panel's top hairline becomes a tab rail naming every provider, the active tab a brand-filled bold chip (the rail's glyphs are identical across tabs, so a switch moves color and weight alone; `┤ ├` caps mark the pick under `NO_COLOR`). One account's block paints at a time, so the budgets read one account deep, and the header reads plan-first (`Claude Max · v2.1.169`; `v?` until a source reports the binary version, the plan label absent until the account surface names one). With pets disabled the active tab **follows the selected pane's provider** — select a codex pane and the dashboard reads its ChatGPT account, a process pane falls to the first tab; `←`/`→` or a click picks one by hand until you select a pane of a different provider. Every logged-in account earns its block even when idle this run, so your budgets show between turns — kept current by per-provider out-of-band refresh ([provider.md](../internals/agents/provider.md)).

With `[theme.pets] enabled = true`, the active provider block narrows and one pane-local companion rides its right edge, driven by fused fleet state plus a canned caption. It renders as true pixels where tmux passthrough and the terminal support kitty graphics, else as sextant cell art in `auto`. Pixel pets reserve `15×9` cells and cell-art pets reserve `18×9` cells; in the dashboard both tiers get one empty row underneath, so the art ends one row above the panel bottom and the provider block keeps identical height across tiers. The rail still shows provider tabs only, and the same `←`/`→` and click controls pick a tab. The provider block keeps its auto layout tiers — wide the full historical row, normal the taller totals block beside the pet column, narrow dropping token splits then version text as it crowds — and the sprite drops out entirely under `NO_COLOR` or when the pane is too narrow to hold it.

```
 ── Claude ──── Codex ──────────────────────────
                                      resting
          Claude Max · v2.1.169      ▄▟████▙▄
 ▐▛███▜▌ ◎ 58  ◇ 17M ↘ 15M ↗ 2M ◌ 198M $188.88
▝▜█████▛▘ 5h ▰▰▰▰▰▰▱▱ ↻ 1h47m       ██▀█ █▀██
  ▘▘ ▝▝   7d ▰▰▰▰▰▰▱▱ ↻ 5d22h       ██ ▄ ▄ ██
                                      ▜█▄██▄█▛
 ── Total: ────────────────────────    ▀▀  ▀▀
 W: ◎ 420        ◇ 202.9M ◌ 5.2B
 M: ◎ 860        ◇ 420.0M ◌ 10.8B
 W: $3,888.88          M: $8,666.66
 ⇄ remote 210ms              ? for help
```

Each block's stats speaks the fleet ledger's vocabulary, scoped to the provider: the configured headline `◎` session count, then the `◇ ↘ ↗ ◌` token breakdown, where `↘` input subsumes cache creation, with the spend pinned right. The stats row stays one row in every provider layout; normal and narrow hide the input/output split only when the width needs it. The `Total:` delimiter switches from provider-headline facts to account-global fleet totals. Wide paints `W:` and `M:` as two full rows with USD pinned right; normal and narrow split each token row into a left `W:`/`M:` session cluster and right-aligned token stats, then put `W: $...` on the left and `M: $...` on the right of the third total row. A cold or empty cache keeps the rows and reads `$0.00`.

A **metered account** drains one "mana" bar per included budget window toward its reset. The bar fills with what's *left*, sliding continuously green → gold → amber → red as it empties over a dim empty track — and a fully-spent window (0% left) flips its whole empty track red, so an exhausted budget never reads as an untouched one. Each window's label (`5h`/`7d`) wears its own bar's tone, while the `↻` reset countdown beside it wears the spend pace: soft when the current burn rate sustains to reset, then sliding gold → amber → red as it outruns the window, falling back to the quiet soft tier when pace is unknowable. A spent longer window gates the shorter ones: once the `7d` is exhausted the `5h` row is painted exhausted too — red, no countdown — regardless of its own reading, since that budget is unusable until the longer window resets. When paid extra usage is available or unknown, the second row becomes `ex`; known usage reads `$used/$limit`, a known remaining balance reads dollars, and an unknown or uncapped value reads `∞` over a dim empty track.

These are **sliding windows** that begin counting only on your first token, so until then the provider keeps sliding the reset a full window-length ahead. A window whose reset still sits ~a full window out has **not started** (it still reads ~1% used, not 0 — so it's the reset distance that gives it away). Any usage above that ~1% floor means it has already started, countdown and all; only a 0–1% window with a near-full reset qualifies. A not-started window shows a near-full bar with **no countdown**, reading "ready — send a message to start it" rather than a misleading ticking placeholder; the countdown appears once your first token fixes the reset and it begins ticking down.

When Rimz is reading only cached budgets and the longest cached window has already reset, the balance is unknown until the provider refreshes. The panel keeps the account and window labels, but every cached budget row becomes a dim empty track with no countdown, so it does not claim either a refreshed full budget or an exhausted one. A metered account whose window list has not arrived yet paints the same dim empty track with a blank label slot, preserving the budget grid without inventing a window.

Provider blocks stacked (`theme.display.provider_tabs = "never"`). Claude and Codex use account-global provider totals; Pi is shown as an idle API-key block to pin its built-in emblem:

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

An **unmetered** account (an API key) shows an `api` bar: trailing-month transcript spend against an optional display ceiling, `∞` when none is configured. The dashboard isn't pinned to fixed windows — each is labeled by its reported length, and paid usage earns a separate `ex` row only when it matters to the account's usable budget.

A **Pi block** names its version and the backing account it runs on — `Pi v0.78.1 · Anthropic OAuth`. Pi keeps its budget readings under the `pi` provider: live sessions publish response-header windows, idle OAuth accounts refresh out-of-band, and an API key gets the `api` paid-usage row instead ([provider.md → Per-provider mapping](../internals/agents/provider.md#per-provider-mapping)).

Every bar shares one start column and one end column whichever tab is active, so the dashboard reads as one aligned grid. The `⇅ rc` flag pins to the block's top-right when remote control is on for that provider — host infrastructure, never its own row. Below ~34 columns the emblem is dropped and the bars run full-width. The brand emblem, color, and name are config-driven (`[theme.providers.<kind>]`, see [theme.md](../reference/theme.md#provider-styling)).

### The fleet ledger

The account-global running totals seal the bottom of the dashboard, above the footer — a quiet two-row ledger you learn to glance at, never a cue that competes with the rows. The trailing-week (`W:`) and trailing-month (`M:`) rows span every provider.

```
 W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88
 M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66
```

- **The rows.** Each reads `◎ sessions  ◇ total ↘ input ↗ output ◌ cache-read  $spend` — the precise one-decimal record beside the cockpit's coarse live read, every field right-aligned into one shared grid so the `W:`/`M:` labels stack and the columns line up. Cache creation folds into the `↘` input figure, keeping the ledger to the headline numbers.
- **No animation.** The ledger figures are static — the count-ups live above, the configured headline in the cockpit and each card's `$cost`. The windows escalate `headline → week → month`.

Every figure is computed from the transcript JSONL — Codex's dollars priced from its token counts, every provider that logs usage counted, all of them account-global. The ledger reads the last persistent account-global totals when they exist, and `$0.00` until something has been recorded.

## Bottom chrome

Pinned to the bottom edge, below all three zones. The body is truncated before this chrome is ever clipped, so it can never scroll off.

**Footer.** Faint chrome — the deepest legible gray, receding to pure scaffolding. It is just `? for help`, pinned to the bottom-right edge:

```
                                                   ? for help
```

When the room is AFK, the footer adds a muted sleep badge at the left edge and keeps help on the right. tmux reports both detached and attached-but-idle after the configured idle window (`[sidebar] afk_after_secs`, 15 minutes by default), so it can show `zᶻ idle · 17m`; Zellij reports attached/detached only, so it shows `zᶻ away` once every terminal client detaches:

```
zᶻ idle · 17m                                      ? for help
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

**Help overlay** (`?`). The legend and keys replace the card body as a centered floating block in the faint chrome tier — summoned reference, not live state:

```
╭ keys & legend ───────────────────────╮
│ ↕ j/k rows   J/K worktrees  g/G ends │
│ ⏎ l focus    1-9 direct              │
│ ␣ n/N needs-you  (Space = n)         │
│ ✉ m/M read / unread                  │
│ ↔ ←/→ account tabs                   │
│ filter                               │
│   ? q waiting    ! e attention       │
│   ⏸︎ p paused     ✓ d done            │
│   ⢿ w working    ○ o idle            │
│   u unread       a all               │
│ global  Alt+p sidebar (toggle)       │
│ ⟳ r reload   ✕ x dismiss   ? close   │
│   ⠁ thinking     ⢄ delegating        │
╰──────────────────────────────────────╯
```

**Health alert.** When the refresh loop can't read the room, a sticky line takes over the bottom and the footer steps aside — an empty body under a failed fetch is a missing snapshot, not an empty room:

```
 ⚠ Sidebar degraded for 8s: snapshot failed: ledger not found
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
| unread jump banner | `unread_jump_banner` |
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

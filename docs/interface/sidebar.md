# The sidebar on screen

This page is the reference for what the sidebar draws: every line, glyph, bar, and notice, and every key it answers to. Use it to look up something you saw. To learn how to read the column and how it decides which agent needs you, start with [the sidebar guide](../guide/sidebar.md). To restyle any color, glyph, or animation shown here, see [theming](../guide/theme.md).

The frames are plain text in the default Unicode glyph set, laid out the way the renderer prints them. Names, counts, ages, and dollar figures are examples. On screen each mark also has a color from your palette, but shape carries the meaning, so the column reads the same under `NO_COLOR`.

## The whole frame

The sidebar stacks three zones and a footer. The [cockpit](#the-cockpit) at the top and the [provider dashboard](#the-provider-dashboard) and [footer](#bottom-chrome) at the bottom stay fixed. The [agent cards](#the-agent-cards) scroll between them.

```
 ⌘ query-engine                     ~/code/query-engine     ← workspace name and path

 ◎ 91                           ◇ 32M ↘ 28M ↗ 3M ◌ 472M     ← sessions in the spend window, their tokens
 ¤ 16 (2) ⑃ 1                                   $420.00     ← live agents, unread count, open PRs, spend
 ──────────────────────────────────────────────────────
 ? 3   ! 0   ⏸︎ 0   ✓ 8                  ⢿ 3   ☾ 1   ○ 1     ← make-up line: agents by status

▎⑂ feature ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ⇡3  +127 -43  ⑃ main🮇    ← worktree header: branch, commits, diff, PR state
▌⢄ claude · Opus 4.8 · xhigh · 200k               $1.27▐    ← status, handle, model, effort, window, cost
▌  store refactor                                      ▐    ← what the session is on
▌  ▣ ━━━━━━━━━━━━━━━━╺━╺───────────────────────── 38.2%▐    ← context meter
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k · 97%              ◔ 8m▐    ← tokens in the window, cache hit, last activity
▌  ⧉ subagents (2) · ⧖ waits (3)                  $0.42▐    ← subagents and waits line
▌    ⠁ Explore · audit the trust hash                  ▐    ← running subagent
▌      ◇ 3k · Opus 4.8                            ◔  3m▐    ← its tokens, model, elapsed time
▌    ✓ Explore · locate the render seam           ◔  1m▐    ← finished subagent, time since it finished
▌    ◷ timer · in 12m                             ◔ 18m▐    ← timer wait, time since it was armed
▌    ⣾ shell · Run the test suite                 ◔  4m▐    ← background shell
▌      cargo test                                      ▐    ← its command
▌    ⌁ signal · pr.merged                         ◔  5m▐    ← signal wait
▎○ zsh                                                 🮇    ← process row

 ──────────────────────────────────────────────────────
 Claude v2.1.169 · Claude Max                      ⇅ rc     ← provider, version, plan, remote control

  ▐▛███▜▌  ◎ 53  ◇ 16M ↘ 13M ↗ 2M ◌ 198M        $188.88     ← provider sessions, tokens, spend
 ▝▜█████▛▘ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱ ↻  1h47m     ← 5-hour budget left, time to reset
   ▘▘ ▝▝   7d  ▰▰▰▰▰▰▰▰▰▰▰▰▰╱▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱ ↻  5d22h     ← 7-day budget left, model sub-cap tick

  ── Total: ───────────────────────────────────────────     ← fleet store
  W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88     ← trailing week, all providers
  M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66     ← trailing month

 ⇄ remote 210ms                              ? for help     ← footer
```

The cockpit counts this room only: its project root and the worktrees grouped under it. The provider dashboard and the fleet store count every session on the account, in any project.

## Reading the glyphs

One glyph vocabulary runs through the whole sidebar. The tables group it by where a glyph appears and show the default Unicode set. `[theme.glyphs]` can switch to a Nerd Font set or override single glyphs without changing a meaning ([theming: glyphs](../guide/theme.md#glyphs)).

### Status glyphs

A status glyph leads every agent card and labels every bucket of the cockpit's make-up line.

| glyph | status | meaning | needs you |
|-------|--------|---------|-----------|
| `?` | waiting | asked you something: a permission, a plan approval, a question | yes |
| `!` | failed | the turn errored, died on a provider API error, went silent past the stall window, or repeated one tool call 20 times | yes |
| `⏸︎` | paused | stopped mid-turn on a provider rate limit, overload, or dropped connection; nothing to answer until the provider recovers; a pause that can no longer resume becomes `!` | after recovery: prompt it to continue, or let [auto-continue](../guide/configuration.md#resume) do it (off by default) |
| `✓` | done | the turn finished cleanly | a look |
| `⢿` | working | running a turn; the cell animates through braille frames (`⣾`, `⣽`, ...), and any of them reads as working | no |
| `☾` | sleeping | resting until an armed one-shot wait fires | no |
| `○` | idle | alive, nothing in flight | no |

`?` is always yellow, `!` always red, and `⏸︎` always blue, whatever the card's age. What each status means in an agent's life is [the agent lifecycle](../guide/sidebar.md#the-agent-lifecycle). The mux tab bar reuses `!`, `?`, `⏸︎`, `⢿`, and `✓` as a static suffix on the tab name ([the sidebar guide](../guide/sidebar.md#glance-jump-answer)). A suffix always draws in these compact Unicode shapes, even when the column runs a configured [glyph set](../guide/theme.md#glyphs). A tab RimZ named after its agents goes back to the shell's name once they all exit; a tab you named yourself keeps its name.

On a process row, `!` means the process is stuck ([process rows](#process-rows)).

A running agent can show a head in place of `⢿`. Whichever head it shows, it counts as working in the cockpit.

| glyph | head | meaning |
|-------|------|---------|
| `⠁` | thinking | the turn has not edited a file yet; a research turn that never edits stays here to the end |
| `▇` | compacting | condensing its context window |
| `⢄` | waiting on subagents | delegated to its children; their entries are listed under the card |
| `⠙` | resolving | a working-family spinner, themable as `resolving` |

Each head's frames, color, effect, and speed are configurable under [`[theme.animations]`](../guide/theme.md#animations).

### Card glyphs

| mark | meaning |
|------|---------|
| `▣ ━━━╺━───  38.2%` | context meter: the share of the context window in use; `▢` while it is 0% |
| `▤ 76k` | tokens in the context window now |
| `◌` `◍` `↘` `↗` | cache-read, cache-write, fresh input, and output tokens. The cockpit, dashboard, and fleet store have no `◍` column, so their `↘` includes cache writes. |
| `◇` | total tokens: `↘` plus `↗`, with `◌` counted beside it and not in it |
| `97%` | session cache hit: green from 90%, yellow from 70%, red below |
| `↻ 2` | completed context compactions |
| `⟲ 5` | consecutive identical tool calls, shown from 3 through 19 |
| `◔ 8m` | an age or elapsed time; the face fills by the quarter hour: `◔` to 15 minutes, `◑` to 30, `◕` to 45, `●` to 60, `◉` past an hour |
| `200k`, `1m` | the model's context window size |
| `$1.27` | cost in dollars, two decimals |
| `⋯ bg` after the description | the turn finished while background work it started is still running |
| `C 34%` `M 512M` `⇅ 8M/s` | a working process row's CPU, resident memory, and I/O rate |

Each token marker keeps one color everywhere: `◇` blue, `↘` deep red, `↗` cyan, `◌` green, `◍` violet.

### Worktree header glyphs

| mark | meaning |
|------|---------|
| `⑂ name` | a worktree group, named by its branch |
| `⮌ name` | a worktree group whose work has landed |
| `# name` | a named channel with no git state |
| `✓` `✕` `◌` beside the name | the trunk's HEAD-commit CI, or a branch's open or merged pull request's CI: passing, failing, running |
| `#91` | the branch's pull request |
| `⇡3 ⇣1` | commits ahead of and behind the trunk |
| `+127 -43` | lines added and removed against the trunk |
| `⟳` `✓` `✕` `⑃` `≡` `⑂` before the trunk name | where the work stands; see [worktree headers](#worktree-headers) |
| `▸` | a collapsed finished group |
| `+3 more` / `− less` | hidden idle rows; click to expand or collapse |

### Pipeline glyphs

Each declared stage gets one dot; `Done` has no separate slot. These roles live under `[theme.glyphs.<set>.pipeline]` independently of the clock and status glyphs.

| role | Unicode | Nerd Font | meaning |
|------|---------|-----------|---------|
| `passed` | `●` | `●` | before the current stage |
| `current` | `◉` | `◉` | current stage |
| `future` | `○` | `○` | after the current stage |
| `done` | `●` | `󰗠` (U+F05E0) | replaces the last dot at `Done` |

### Subagent and wait glyphs

| mark | meaning |
|------|---------|
| `⧉` | subagents the agent has spawned |
| `⧖` | waits the agent has armed |
| `◷` | a timer wait |
| `⌁` | a signal wait |
| working spinner, in violet | a live watch (command, check, PID, file) or a background shell |
| `+7 older` | finished subagents folded away; click to list them |

### Dashboard glyphs

| mark | meaning |
|------|---------|
| `▰▰▰▱▱` | budget bar: the filled part is the budget left |
| `╱` | a model's own sub-cap, as a tick on its parent window's bar |
| `5h` `7d` `30d` | an included budget window, labeled by its length |
| `cr`, `bld`, `dep` | a quota the provider or a plugin names |
| `ex` | paid extra usage |
| `api` | an API-key budget |
| `↻ 1h47m` | time until the window resets |
| `↻ 2` in a block header | Codex rate-limit reset credits available |
| `∞` | no limit |
| `–` | the provider reports no figure for this slot |
| `⇅ rc` | remote control is on for this provider: green when its server is up, red when a configured server is down |
| `W:` `M:` | fleet totals for the trailing week and month |

### Cockpit and chrome glyphs

| mark | meaning |
|------|---------|
| `⌘ name` | the workspace |
| `◎ N` | sessions that ran in the spend window |
| `¤ N` | agents alive right now |
| `(2)` | unread cards |
| `⑃ N` | open pull requests on the agents' branches |
| `↑ 2 need you` | the card that most needs you is scrolled out of view |
| `▌` ... `▐` | the selected card's left and right spines |
| `▎` ... `🮇` | the selection lane: every row of the group that holds the selection |
| `┄` | the dotted seal on the selected group's header |
| `┄ external ┄` | the group for panes outside the project |
| `─` | a section rule |
| `▐` / `▕` on the right edge | scrollbar thumb and track, drawn over the right spines while the cards scroll |
| `┤ Tab ├` | the active dashboard tab under `NO_COLOR` |
| `zᶻ idle`, `zᶻ away` | you are away from the terminal |
| `⇄ remote 210ms` | SSH link round-trip time |
| `⚠` | a pane-source notice or health alert |

`🮇` is from Unicode's Symbols for Legacy Computing block. A font without it may show a placeholder at the lane's right edge; nothing else depends on it.

## The cockpit

The cockpit is the top block. Its lines stay put as agents change status, and only the unread banner adds or removes a row.

```
 ⌘ query-engine                     ~/code/query-engine

 ◎ 12                           ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 6 (2) ⑃ 1                                      $4.20
 ──────────────────────────────────────────────────────
 ? 2   ! 1   ⏸︎ 0   ✓ 0                  ⢿ 2   ☾ 0   ○ 1
 ↑ 2 need you
```

Lines 2 and 3 cover the spend window, `[sidebar] spend_window`: `"session"` (the default) starts at your first prompt after five idle hours, `"24h"` is the trailing 24 hours, and `"today"` starts at midnight ([configuration](../guide/configuration.md#sidebar-rendering)).

| line | shows |
|------|-------|
| 1 | `⌘` and the workspace name in green. The project path sits on the right with your home directory as `~`; when space runs out the path loses its left end behind `…`. |
| 2 | `◎` sessions that ran in this room during the spend window, and their tokens on the right: `◇` total, which is `↘` input including cache creation plus `↗` output, then `◌` cache-read beside it. |
| 3 | `¤` live agents, `(N)` unread cards when there are any, `⑃ N` open pull requests when there are any, and the room's spend for the window on the right. |
| 4 | the make-up line: agents by status. |
| 5 | the `↑ N need you` banner, only while it applies. |

Before any spend arrives the token breakdown shows zeroes and the spend shows `$0.00`. When an agent's cost moves, the spend counts up to the new figure within 1.2 seconds. It never decreases inside one window and resets when the window rolls. Antigravity reports current usage that cannot be summed over time, so its cost shows on its card and stays out of this total. How every figure is computed is [Token Insight](../guide/insight.md).

The room's scope is a path prefix: the project root and each grouped worktree root. A checkout you reach through a symlink spelled differently from the path in the agent's transcript can count as outside the room until the two paths agree.

The `⑃ N` count is red when any of those pull requests fails CI, yellow while one is running, green when every known verdict passes, and blue when CI is unknown.

### The make-up line

The left cluster counts agents that may want you: `?` waiting, `!` failed, `⏸︎` paused, `✓` done. The right cluster counts the rest: `⢿` working, `☾` sleeping, `○` idle. Every bucket shows even at zero, and the counts always cover the whole room.

Click a non-zero bucket, the unread count, or the `⑃` count to filter the cards to it. The filter applies in every tab of the room and stays on while you move between matching cards. The active filter is drawn as a filled chip, or in reverse video under `NO_COLOR`. To clear it, click it again, press its key again, press `A`, or pick another filter. It also clears when its count reaches zero. The filter keys are in [Keys and mouse](#keys-and-mouse).

With single-digit counts, all seven buckets fit from 39 columns. Below that the idle bucket is dropped, and at 34 columns the sleeping count goes too. The filter keys still work for a dropped bucket.

### The unread banner

`↑ N need you` appears when the card that most needs you, the oldest unread `?` or `!`, has scrolled out of view. It is yellow for a waiting card and red for a failed one. Click it to scroll back to the top of the cards, where ranking places that card, or press `n` to jump to the next card that needs you. It disappears when that card is on screen or nothing is waiting for you.

### An empty room

A room with no agents has no make-up line:

```
 ⌘ query-engine                     ~/code/query-engine

 ◎ 0                                    ◇ 0 ↘ 0 ↗ 0 ◌ 0
 ¤ 0                                              $0.00
 ──────────────────────────────────────────────────────
```

## The agent cards

The body holds one card per pane, grouped under the worktree the pane works in. When the cards outgrow the space they scroll between the cockpit and the dashboard, and a scrollbar shows on the right edge while the view moves, fading about a second after it stops (`[theme.display] scrollbar` pins or removes it). The view follows the selection: selecting a card brings all of it into view, and a card taller than the view is pinned by its first line. The mouse wheel scrolls without moving the selection, and the next selection change brings the view back.

### The card

```
▌✓ claude · Opus 4.8 · xhigh · 200k               $2.14▐    ← identity line
▌  store refactor                                      ▐    ← description line
▌  ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╺━━╺────────── 78.4%▐    ← context meter
▌  ▤ 157k · ◌ 149k ◍ 6k ↘ 2k ↗ 2k · 97% · ↻ 2      ◔ 8m▐    ← stats line
▌  ⧉ subagents (2) · ⧖ waits (1)                  $0.42▐    ← subagents and waits line
```

| line | shows |
|------|-------|
| identity | The status glyph, then the agent's handle in its provider's brand color: its team role, explicit name, or profile, else the agent kind. Then, dimmer, the model, the agent's reasoning setting as its provider names it (an effort level such as `high` or `xhigh`, or `thinking`), and the context window size. The session's cost sits on the right once it rounds to at least `$0.01`, and counts up like the cockpit spend. |
| description | What the session is on: the first of the session name, the provider's thread preview, the launch description, the agent's task, the session's first prompt, and its latest prompt. Codex's automatic title and Claude's `--name` or `/rename` count as the session name; a name that only repeats the start of the prompt is skipped. |
| context meter | The percent of the context window in use, as a bar. The fill shows where the window went: a run of cache reads first, then a segment each for cache writes and fresh input, each starting with `╺`. The bar runs from green toward red as the window fills, at the stops set in [`[theme.display.context_meter]`](../guide/theme.md#display). |
| stats line | `▤` tokens in the window: the latest API call's cache-read, cache-write, and fresh input added together, which is the amount the meter's percent measures. Then the same split in numbers, the session cache hit, `↻ N` compactions, and `⟲ N` repeated tool calls. The time since the agent's last activity sits on the right once it passes five minutes. |
| subagents and waits | Present once the session has spawned a subagent or armed a wait. See [Subagents and waits](#subagents-and-waits). |

On a narrow sidebar the identity line drops the reasoning token first, then the model and window, and keeps the handle. The window size shows on non-idle cards only.

On the stats line, a column that is zero or unreported is left out. A provider that reports only session totals, as stock Droid does, shows `◇ total ↘ input ↗ output ◌ cache-read` on this line instead, and its meter stays empty. The cache hit is cached input divided by all input, and it is absent until the session has input counters.

The meter draws windows up to 256k tokens linearly and larger ones on a log curve that reaches full strength at 1M, so a large window keeps detail in its working range. The percent is always the raw share in use. The age clock heats toward red as the hour approaches, because a prompt after an hour of quiet usually re-reads the whole context uncached. It measures the agent's own quiet time, so a parent waiting on its subagents keeps heating while they work — its own session is making no call, and its cache ages the whole wait. The children's own times ride their entries under the card. Bands, curve, and tones are set under [`[theme.display]`](../guide/theme.md#display).

### Card shapes

A card's lines are set by its stage. Within a stage, data fills in place and no line moves.

| card | lines |
|------|-------|
| fresh: idle, never prompted | identity, plus the description when it was launched with one |
| fresh, selected | adds the empty meter `▢ 0%`, and animated dots (`.`, `..`, `...`) in the description slot when there is no description |
| engaged: after its first prompt | identity, description, meter, stats, with `▢ 0%` and `▤ 0` until data arrives |
| engaged, with subagents or waits | adds the subagents and waits line |
| selected | lights the spines and lists the subagent and wait entries below the card |

Selecting a card only appends lines below it. When the selected agent belongs to a named team, every visible teammate's card opens the same way, and the spines stay on the selected card.

`[theme.display] card_density` changes how much a resting card shows:

| value | resting cards |
|-------|---------------|
| `auto` (default) | the shapes above |
| `expanded` | every engaged card lists its subagent and wait entries |
| `compact` | idle: identity. Running and waiting: identity, description, meter. Paused, done, sleeping, failed: identity and description. Selecting a card restores its full shape. |

### Subagents and waits

```
▌  ⧉ subagents (2)                              $0.42▐
▌    ⠁ review · audit the trust hash                 ▐
▌      ◇  3k · Haiku 4.5                        ◔ <1m▐
▌    ✓ Explore · locate the render seam   ◔ <1m $0.42▐
▌      ◇ 12k · Opus 4.8  · high                      ▐
```

A card with waits uses the same entry layout:

```
▌  ⧖ waits (4)                                       ▐
▌    ◷ timer · in 12m                           ◑ 18m▐
▌    ⣾ pid · 16776                              ◔  3m▐
▌    ⣾ command · cargo                          ◔  4m▐
▌      cargo xtask gate --name foo_test              ▐
▌    ⌁ signal · pr.merged                       ●  1h▐
▌      2h left                                       ▐
```

`⧉ subagents (N)` counts every child the session has spawned, both the provider's native subagents and children launched with [`rimz subagents`](../reference/cli/subagents.md), for as long as RimZ retains the session's history. Their known cost sits on the right. The card's cost on the identity line already includes it, so do not add the two. `⧖ waits (N)` counts armed one-shot [waits](../reference/cli/wait.md) plus, for Claude, the shell commands it left running in the background. Either half shows alone when only one applies, and below 46 columns the line shortens to `⧉ N · ⧖ M`.

Selecting a card opens the section. Clicking the line opens or closes it without focusing the pane, on any card, and that choice outranks selection and `card_density` until the sidebar restarts. In `compact` density, select the card first to reach the line.

Each entry starts with its live state or wait icon, then a type word and a ` · ` separator before the headline. Without a headline, the separator disappears too. Detail sits on a muted, indented second line.

| entry | lead | type · headline | right side | second line |
|-------|------|------|------------|-------------|
| running subagent | `⠁` while it reasons, `⢿` while it acts | launch profile or kind · description, else task if different from the type | cost, when known | reported metadata: `◇` tokens, model, effort, and elapsed time |
| finished subagent | `✓` or `!` | launch profile or kind · description, else task if different from the type | time since it finished, then cost | reported metadata: `◇` tokens, model, effort |
| timer | `◷` | `timer · in 12m`, or `timer · due` once the time passes | time since armed | never |
| PID | working spinner | `pid · 16776` | time since armed | never |
| command | working spinner | `command · cargo` (program name) | time since armed | full command, with the program path trimmed |
| check | working spinner | `check · test` (program name) | time since armed | full command, with the program path trimmed |
| file | working spinner | `file · app.log changes`, or ``file · app.log matches `<pattern>` `` | time since armed | never |
| background shell | working spinner | `shell ·` description, else program name, else just `shell` | time since RimZ first saw it | command when known, with the program path trimmed |
| signal | `⌁` | `signal · pr.merged` (selector) | time since armed | only with a deadline: `2h left`, then `0m left` at or past it |

Entries list in a fixed order, and opening more of the list only appends rows:

1. Running subagents, in the order they started. They are never capped.
2. Finished subagents, newest first, that finished within `recent_subagent_secs` (900 seconds by default), up to `max_recent_subagents` (5 by default).
3. Waits: timers by due time, then watches, then background shells, then signals. A wait leaves the list when it fires or its job ends.
4. `+K older`, which folds every other finished subagent. Click it to append them. When no subagent is running or recent, opening the section lists the older ones directly.

The listed subagents plus `K` equal the count in the line. `K` can exceed the rows it reveals, because a native subagent that was replaced while still running is counted and has no row. The older list folds again when you close the section, send the parent a new prompt, or run `/clear` or `/compact`. Messages from other agents and RimZ's own deliveries leave it open.

A running subagent's clock heats with age like a card's. A finished subagent's clock stays muted, and so does a wait's, since a wait is pending by design. A wait has no clock when its arm time is unknown. A subagent without metadata shows a single line; command and check waits show their command on a second line. Subagents never get a card of their own while their parent's card is visible. Which fields each provider reports for a child is in [sidebar internals](../internals/sidebar/sidebar.md#sub-agent-lists).

### Unread and attention marks

A card is unread from the moment it becomes waiting, failed, paused, or done until you focus its pane or press `m`. An unread card has a soft wash behind it. The one card that most needs you, the oldest unanswered `?` or `!`, also animates across its glyph, name, and description, and the make-up bucket that owns it animates with it. Every other unread card is bright and bold without motion, so one thing on screen moves at a time. A card that is both selected and unread shows the selection. The animation style is `[theme.animations] unread` ([theming: unread attention](../guide/theme.md#unread-attention)).

A waiting or failed card looks like any other card with `?` or `!` in the lead. When a turn dies on a provider API error, the description shows the provider's error text for as long as the `!` holds:

```
▎⑂ feature-migration ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇
▌! claude · Opus 4.8 · high · 200k                $1.27▐
▌  API Error: Overloaded                               ▐
▌  ▣ ━━━━━━━━━━━━━━━━╺━╺───────────────────────── 38.2%▐
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                        ▐
```

Two checks raise `!` without a report from the agent. A working agent that stays silent for 30 minutes becomes `!`, or `⏸︎` when its provider budget window is spent. A parent waiting on its subagents is exempt. A working agent that repeats one tool call with the same arguments shows `⟲ N` on its stats line from the third call; at 20 it becomes `!` and the description reads `loop: <tool> ×<count>`. Any different call clears the run. All three thresholds are under [`[agents.attention]`](../guide/sidebar.md#tuning).

A sleeping card names its first pending wait in the description:

```
▌☾ claude                                  ▐
▌  wakes in 12m                            ▐
▌  ▢ ────────────────────────────────    0%▐
▌  ▤ 0                                     ▐
▌  ⧖ 1                                     ▐
▌    ◷ timer · in 12m                 ◔ <1m▐
```

| wait | description |
|------|-------------|
| timer | `wakes in 12m`, then `wakes now` |
| PID | `wakes after pid 16776` |
| command or check | `wakes after <command>` |
| file | `wakes when app.log changes`, or ``wakes when app.log matches `<pattern>` `` |
| signal | `wakes on pr.merged`, or `wakes on pr.merged · 2h left` with a deadline |

`☾` replaces only an idle or done status. Working, waiting, failed, paused, and waiting on subagents all take precedence, and a standing subscription does not make an agent sleep. Sleeping opens no unread mark and sends no notification, and an earlier unread result stays unread through the sleep.

### Process rows

A pane that no agent runs in, such as a shell, an editor, or a build, shows as a process row below the group's agent cards, drawn dimmer than a card.

```
 ○ zsh
 ⣾ cargo                       C  34%  M 512M  ⇅   8M/s
   cargo build --release
```

An idle row is `○` and the program's name. A working row spins, shows the running command on a second line, and shows CPU, memory, and I/O on the right once all three have values. A process stuck in uninterruptible sleep for ten seconds, or left as a zombie, shows `!`. The name is the program the pane really runs: RimZ reads past `env`, `sudo`, `timeout`, `sh -c`, and `node` or `npx` launchers, so `sudo npm install -g @openai/codex` reads as `npm`. Process rows are never counted in the cockpit. They are jump targets, and when an agent starts in the pane the row becomes that agent's card.

### Worktree headers

Each group starts with a header. Its left side names the work, and its right side shows where the work stands against the trunk.

```
▎⑂ feature-migration ◌ #91 ┄┄┄┄ ⇡3 ⇣1  +230 -23  ⑃ main🮇    ← diverged, open PR #91, CI running
▎⑂ fresh-fork ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ≡ main🮇    ← pristine: no commits of its own
▎⮌ feature-landed ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ✓ main🮇    ← merged: safe to remove
```

On the left: `⑂` or `⮌`, the branch name, the CI verdict (`✓` passing, `✕` failing, `◌` running), and the pull request number when the branch has one. An open or merged pull request supplies the CI verdict; a non-trunk branch without one shows no CI glyph, while the trunk shows its own HEAD-commit CI. Closed pull requests show no CI glyph. The team badge `· forge` goes on the pipeline line when one is drawn, on the roster line when the group is folded, and on the header otherwise. In terminals that support hyperlinks, `#91` opens the pull request. When two groups have the same branch name, each adds a muted `· repo` qualifier, the shortest path suffix that tells the checkouts apart. When the header runs out of room, the name shortens first, then the pull request number is dropped, then the CI verdict last.

On the right: commits ahead of and behind the trunk with zero counts left out, then lines added and removed, then the trunk marker. The line counts include committed, staged, unstaged, and untracked work, so work that `git diff` does not show still counts. The first marker that applies is shown:

| marker | meaning |
|--------|---------|
| `⟳ main` | a local rebase, merge, or cherry-pick is in progress |
| `✓ main`, with `#N` on the left | the pull request merged; muted, and the commit and line figures are dropped |
| `✕ main` | the pull request closed unmerged; red, and the figures stay |
| `⑃ main` | the pull request is open |
| `✓ main`, no `#N` | no pull request, and the trunk already contains the work: safe to remove |
| `≡ main` | pristine: a clean tree with no commits of its own, at the trunk's tip |
| `⑂ main` | a plain branch |

Pristine and merged headers show the marker alone. The trunk's own checkout shows no marker, only its commit and line figures. RimZ takes the trunk from `[sidebar] trunk`, then `main`, `master`, and the remote's default branch ([configuration](../guide/configuration.md#sidebar-rendering)).

The group that holds the selection is drawn as a lane: a dim `▎` down its left edge, `🮇` down its right, and a dotted seal across its header. Inside it the selected card has bright `▌` and `▐` spines over a recessed background, subagent entries included. Other groups have a blank gutter and a header without the seal.

```
▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇    ← selected group: lane spine and dotted seal
▌? claude                                              ▐    ← selected card: bright spine on both edges
▌  permission                                          ▐
▌  ▢ ────────────────────────────────────────────    0%▐
▌  ▤ 0                                                 ▐
▎⣾ codex · GPT 5.5 · 272k                              🮇    ← same group, not selected
▎  add tests                                           🮇
▎  ▢ ────────────────────────────────────────────    0%🮇
▎  ▤ 0                                                 🮇

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ? 1     ← panes outside the project, with its own ? and ! count
 ? claude
   Deploy staging?
   ▢ ────────────────────────────────────────────    0%
   ▤ 0
```

Panes outside the project (scripts, CI, stray shells) collect under the dim `external` divider, which always sorts last. It keeps a `? n` or `! n` count so an ask from outside the project still shows.

A room opened on a plain directory groups git-backed agents by their checkout, each under a full header. The panes the directory itself holds sit under a bold header with the directory's name, with no glyph or git figures, because a plain directory has no trunk to compare against.

```
▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ⇡2  +12 -3🮇    ← a checkout inside the directory: full header
▌⣾ claude                                              ▐
▌  db migrate                                          ▐
▌  ▢ ────────────────────────────────────────────    0%▐
▌  ▤ 0                                                 ▐

 agents                                                     ← the directory's own group: name only
 ○ zsh
```

A group with one staged team and a readable worktree `blackboard.md` containing a `Stage:` line gets a pipeline line directly below its header, above the first card. The team's members must identify one worktree:

```
▎⑂ pipeline ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇
▎  ● ● ◉ ○ ○  Implement (47:12) · forge              🮇
▌⣾ planner                                           ▐
```

The dots follow declared stage order: passed, current, then future. Passed dots are green and future dots are muted. Flipping backward moves the current dot back. At `Done`, every dot is passed and the last becomes the green done seal; the word `Done` keeps the meaning visible without color. A board stage outside the declared pipeline shows its name without a track. The current dot takes the visible stage owner's status color and animation phase, or stays muted when that owner has no visible card. The line never creates `!`, unread state, attention ranking, tab status, or a notification.

The clock sits in muted parentheses after the stage name, followed by the team badge; the whole line is left-aligned, with no right-pinned clock. It measures time in the current stage while running and the whole run's total at `Done`, with a zero-padded leading field: `03:11` below an hour, `01:47:12` thereafter. The stage clock starts at the latest parseable entry into that stage in the board's `## Progress` (or `## Progress log`) ledger, including an `opened` entry. A backward flip restarts the stage it enters; a same-stage re-flip does not. No recorded stage entry means no running clock, not a fallback to run time. At `Done`, the total runs from the first parseable ledger entry (or the latest flip out of `Done`) to the last `-> Done` entry; a `Done -> Done` re-flip does not move the stop. A missing or future start, or `Done` without a valid stop, omits the clock and its parentheses. Clicking the line focuses the visible stage owner's pane, otherwise the first actionable team member, otherwise the group's first visible row.

### Row cap and finished groups

A group shows at most six idle and process rows. The rest fold behind a dim `+K more`. Click it to show every row, and click `− less` to fold them again. Working, waiting, failed, paused, done, unread, and selected cards are never folded. While a filter is active the cap is off, and rows and groups that do not match are hidden.

```
 ⑂ main
 ○ codex
 ○ codex
 ○ codex
 ○ codex
 ○ codex
 ○ codex
   +3 more
```

A group is finished when its pull request merged or closed, or the trunk contains its work, its tree is clean, and no member is working or needs you. A finished group with several agents collapses to its header and a two-line receipt, unread results included:

```
 ⮌ merged-work                                   ✓ main
 ▸ rimz  ✓ planner  ✓ coder  ✓ reviewer           $4.02
   ◇ 1M ↘ 300k ↗ 80k ◌ 900k · 75%                  ◉ 2h
```

The first line lists the team name when the members share one, each member's final status and name, `+n` for members and shells that do not fit, and the group's lifetime cost. The second shows its lifetime tokens and cache hit, with its active time on the right, or the time since it finished once that record expires. Both cover every session the team ran in this worktree, resumed sessions and subagents included. When the line is too narrow for one member it reads `▸ +K done`.

Click the header or either receipt line, press `s`, or focus a member to show the cards, and click the header to collapse them again. Each revealed card shows that member's lifetime cost, so the cards add up to the receipt. A finished group with one agent, and a group with only process rows, stay open.

A folded finished group hides its pipeline line and puts the stage after the team name on the roster line:

```
 ⑂ pipeline
 ▸ forge · Done  ✓ planner  ✓ coder
```

Expanding the group restores the pipeline line. The order of cards and groups follows status and age, and read state never moves a card. The rules are in [how the column is ordered](../guide/sidebar.md#how-the-column-is-ordered).

## The provider dashboard

Provider budgets belong to an account, and every session of that provider shares them, so they sit in a fixed panel at the bottom instead of on the cards. A provider whose account has metered budgets or recorded usage keeps its block even when none of its sessions runs in this room, and RimZ refreshes its budgets between turns.

```
 ─── Claude ──── Codex ────────────────────────────────

           Claude Max · v2.1.169                   ⇅ rc
  ▐▛███▜▌  ◎ 53  ◇ 16M ↘ 13M ↗ 2M ◌ 198M        $188.88
 ▝▜█████▛▘ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱ ↻  1h47m
   ▘▘ ▝▝   7d  ▰▰▰▰▰▰▰▰▰▰▰▰▰╱▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱ ↻  5d22h
```

With `provider_tabs = "auto"` (the default), two providers stack and three or more share the panel under a tab rail, one block at a time. The active tab follows the selected card's provider, and falls to the first tab for a process row. `←`, `→`, or a click picks a tab by hand until you select a card of another provider. Under `NO_COLOR` the active tab is marked `┤ Claude ├`. `[theme.display]` sets tabs or stacking with `provider_tabs`, the providers and their order with `provider_list`, and the most stacked blocks with `max_provider_blocks`, 3 by default ([theming: display](../guide/theme.md#display)). By default providers with sessions in this room come first, then the recently used, then the recently logged in.

| line | shows |
|------|-------|
| header | In tabs: the plan, then the version. Stacked: the provider and version, then the plan. The version reads `v?` until a source reports it, and the plan is absent until the account names one. `⇅ rc` and Codex's `↻ N` reset credits sit on the right. |
| stats | `◎` sessions in the [spend window](#the-cockpit), the `◇ ↘ ↗ ◌` tokens with cache creation counted in `↘`, and the provider's spend. A provider with no usage history keeps the row: `◎` counts its sessions active in this room, and the token and dollar slots show `–`. |
| budget rows | one bar per budget window: label, bar, and reset countdown |

Stacked blocks (`provider_tabs = "never"`):

```
 Claude v2.1.169 · Claude Max                      ⇅ rc

  ▐▛███▜▌  ◎ 53  ◇ 16M ↘ 13M ↗ 2M ◌ 198M        $188.88
 ▝▜█████▛▘ ex  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱      $50     ← on paid extra usage, $50 budget
   ▘▘ ▝▝   7d  ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱ ↻  5d22h     ← spent: empty track, red on screen

 Codex v0.137.0 · ChatGPT Pro                 ↻ 2  ⇅ rc

  ▗▛▀▀▀▜▖  ◎ 42  ◇ 16M ↘ 15M ↗ 1M ◌ 272M        $288.88
 ▐█ ▜▖  █▌ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰              ← not started: full bar, no countdown
  ▝▀▀▀▀▀▘  7d  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱ ↻  3d19h     ← running

 Pi v0.80.6 · OpenAI API

  █▜███▛█  ◎ 19  ◇  8M ↘  7M ↗ 1M ◌ 142M        $420.42
 ▝▜▛▀▀▀▜▛▘ api ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰ ∞            ← API key with no budget
  ▝▘   ▝▘
```

### Budget bars

A bar's filled part is the budget left. Every bar starts and ends on the same columns, so the rows compare at a glance.

| you see | meaning |
|---------|---------|
| a partly filled bar and `↻ 1h47m` | The window is running. The fill slides from green through gold and amber to red as it empties. |
| a full bar and no countdown | The window has not started. Providers start the clock at your first token, so there is no reset to count down to yet. |
| an empty track, all red | The window is spent. |
| a short window painted spent while a longer one is spent | The longer window gates it: a spent `7d` makes `5h` unusable until the `7d` resets, so `5h` shows red with no countdown whatever its own reading. |
| a dim empty track and no countdown | The reading is unknown: RimZ holds only a cached reading whose window has since reset, or the provider has not reported its windows yet. |
| a full bar and `∞` | No limit applies: an API key without a budget, a quota the provider reports as unlimited, or a limit the provider has lifted for now. |
| an `ex` row with `$50` | The account is on paid extra usage because an included window is spent. The bar is the share of the extra budget left, and the figure is that budget in whole dollars. When the extra credits are known to be usable, `ex` takes the first row and the longest included window stays second; otherwise the spent window stays first. |
| an `ex` row with no budget figure | Extra usage has no limit set: the row shows the dollars remaining or used, or `∞` over a dim track when the provider reports neither. |
| an `api` row | An API key. With a display budget configured, the bar drains against your trailing-month spend and the dollars left sit on the right. Without one, the bar is full and reads `∞`. |
| `cr`, `bld`, `dep`, and other names | A quota the provider or a plugin names, such as Copilot's AI credits. Named quotas are independent: one spent quota does not mark another spent. A named quota with no reported length shows its reset in a quiet tone. |

The color of the `↻` glyph beside the countdown shows your pace. It is neutral when the current burn rate lasts to the reset, slides through gold and amber to red as spending outruns the window, and cools toward green when spending runs well under pace, once 40% of the window has passed. The stops are `[theme.display.budget_bar.burn_rate]` ([theming: display](../guide/theme.md#display)).

A model's own cap inside a window draws as a `╱` tick on that window's bar instead of a row. The fill and the tick use different scales: the fill is the window's budget left, and the tick's position is the share of the model's cap left, across the full width of the bar. Only exactly 0% reaches the left end and only exactly 100% reaches the right. The tick's color follows the model's share, and it has no label or countdown. In [the whole frame](#the-whole-frame), the `7d` bar has about 65% left while the tick near 42% says the model's weekly cap is closer to spent than the window. A tick still draws on a spent bar when the model's reading is known. An unknown reading or an unlimited row has no tick.

Codex's `↻ N` in the block header counts rate-limit reset credits. The glyph is red, amber, yellow, then green as the nearest credit's expiry moves further away, and grey at a week or more. It blinks while a spent window makes redeeming one useful.

How budgets are read, cached, and refreshed is in [provider internals](../internals/agents/providers.md).

### Pets

With `[theme.pets] enabled = true`, the active block narrows and an animated companion with a caption sits at its right edge. It draws as pixels where the terminal supports kitty graphics (15 by 9 cells) and as cell art otherwise (18 by 9 cells). It is hidden under `NO_COLOR` and when the pane is too narrow. See [pets](../guide/pets.md).

### Narrow panes

The pipeline keeps its clock and stage name ahead of its track and team badge: the team drops first, then the dots disappear as a whole, then the name ellipsizes, and the clock goes last. Dropping the track never buys the team badge back, and the line draws neither a partial track nor a clipped clock.

As the pane narrows, a block drops the input and output token split, then the version text. Below 36 columns the provider emblem goes and the bars run the full width. A pet narrows the block further.

### The fleet store

The last two rows of the dashboard total every provider and every project on this machine's accounts, for the trailing week (`W:`) and the trailing month (`M:`).

```
  ── Total: ───────────────────────────────────────────
  W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88
  M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66
```

Each row reads sessions, total tokens, input including cache creation, output, cache-read, and spend, to one decimal and aligned in columns. The figures come from the agents' transcripts, with Codex's dollars priced from its token counts, and read `$0.00` until something is recorded. They do not animate. On narrower panes the dollars move to a third row, `W: $...` on the left and `M: $...` on the right, and the token rows drop the input and output split when they must. How the totals are computed is [Token Insight](../guide/insight.md#how-the-numbers-are-calculated).

## Bottom chrome

The footer is the last line, apart from a recovered alert, which sits beneath it. The cards give up space before the dashboard or the footer do, so neither scrolls away.

```
 zᶻ idle · 17m                               ? for help
```

| mark | when |
|------|------|
| `? for help` | always, at the right edge |
| `zᶻ idle`, then `zᶻ idle · 17m` | tmux: no input for `[sidebar] afk_after_secs` (15 minutes by default); minutes are added after the first minute |
| `zᶻ away` | no terminal client is attached; Zellij reports only this state |
| `⇄ remote 210ms` | the room runs over SSH: the smoothed round-trip time, with packet loss added above 10%; `⇄ remote ?` means the last reading is stale |

The away badge takes the left edge. The remote badge sits there otherwise, and follows the away badge only when the line has room. The remote badge's color runs from green through yellow and amber to red, bold at the worst, and stays neutral until the link has a first reading.

### Notices and alerts

When RimZ repairs a partial read of the multiplexer's panes by carrying panes over from the last frame, a dim notice appears above the footer and the room stays usable:

```
 ⚠ pane source degraded · 2 carried panes · 8s
```

When it holds back an update that would have emptied the rows, the notice says why the rows are stale:

```
 ⚠ pane updates held · empty pane frame
```

Both clear on the next good read. When the sidebar cannot read the room at all, an alert replaces the footer. The cards keep showing the last good read, so they may be stale:

```
 ⚠ Sidebar degraded for 8s: snapshot failed: store not found
```

After recovery the alert stays as a dim notice until you press `x`, so a failure that came and went is still visible. A new failure raises it again. `r` reloads the tab.

```
 ⚠ last alert 8s ago: snapshot failed: store not found  ·  x dismiss
```

If the alert persists, see [troubleshooting](../guide/troubleshooting.md#the-sidebar-degraded-banner).

### The help overlay

`?` opens the overlay over the bottom right of the cards. Any key closes it except an unbound Ctrl or Alt chord, and so does focus leaving the sidebar. It lists your configured movement keys, so it differs from this frame after a rebind.

```
╭ help ─────────────────────────────────╮
│ keys                                  │
│ ↕ j/k rows          ↕ J/K   worktrees │
│ ↕ g/G ends          ↕ ^f/^b page      │
│ ↕ H/L screen        ⏎ l     focus     │
│ ⏎ 1-9 direct        ␣ n/N   needs-you │
│ ✉ m   read/unread   ✉ M     read all  │
│ ↔ ←/→ account tabs  ⟳ r     reload    │
│ ✕ x   dismiss       ↕ a/d   width     │
│                                       │
│ filter                                │
│ ? q   waiting       ! e     attention │
│ ⏸︎ p   paused        ✓ s     done      │
│ ⢿ w   working       ○ o     idle      │
│ ☾ z   sleeping                        │
│ ● u   unread        ≡ A     all       │
│                                       │
│ ▐ alt p sidebar                       │
│   alt g zoom                          │
╰────────── any key to close ───────────╯
```

## Keys and mouse

These keys work while the sidebar has focus. From any other pane, `Alt+p` focuses the sidebar, and pressing it again returns to a work pane in the sidebar's tab: Zellij returns to the exact pane you left, and tmux takes the first work pane in that tab. `Alt+g` zooms the focused work pane to fullscreen and never zooms the sidebar. Both are set by `[sidebar] focus_key` and `zoom_key` ([configuration](../guide/configuration.md#sidebar-rendering)).

| key | action |
|-----|--------|
| `j` / `k`, `↓` / `↑` | select the next or previous row, without changing focus |
| `J` / `K` | select the first row of the next or previous worktree |
| `g` / `G` | select the first or last row |
| `Ctrl+f` / `Ctrl+b`, `PageDown` / `PageUp` | move a page; under tmux's default prefix `Ctrl+b` never arrives, so use `PageUp` |
| `H` / `L` | select the first or last row on screen |
| `Enter`, `l` | focus the selected row's pane |
| `1` to `9` | focus the pane of the Nth row, counted from the top of the list |
| `n`, `Space` | jump to the next card that needs you: unread cards oldest first, then read `?` and `!` cards oldest first |
| `N` | the same walk, backward |
| `m` | mark the selected card read or unread, without jumping |
| `M` | mark every card read |
| `←` / `→` | switch the dashboard tab |
| `a` / `d` | make the sidebar one step narrower or wider in every tab of the room; the width holds through terminal resizes, reloads, and reattaches until the session ends ([width rules](../guide/sidebar.md#setting-the-width)) |
| `r` | reload this tab's sidebar |
| `x` | dismiss a recovered alert |
| `?` | open the help overlay |

| filter key | cards shown |
|------------|-------------|
| `u` | unread |
| `q` | waiting (`?` is the help key) |
| `!`, `e` | failed; the overlay labels it attention |
| `p` | paused |
| `s` | done |
| `w` | working |
| `z` | sleeping |
| `o` | idle |
| `A` | all |

Pressing the active filter's key again also returns to all. Movement keys and `a` / `d` can be rebound under `[sidebar.keys]`, and a rebound chord takes priority over a fixed key, so it can shadow a filter. The other keys are fixed.

| mouse | action |
|-------|--------|
| click a card or process row | focus its pane |
| click a pipeline line | focus the visible stage owner, else the first actionable team member, else the group's first visible row |
| click a make-up bucket, the unread count, or `⑃ N` | filter the cards |
| click `↑ N need you` | scroll to the top of the cards |
| click a subagents and waits line | open or close its entries |
| click `+K older`, `+K more`, `− less` | unfold or fold rows |
| click a finished group's header or receipt | show or collapse its cards |
| click a dashboard tab | switch provider |
| wheel | scroll the cards without moving the selection |
| drag the pane border | set the sidebar width for the room |

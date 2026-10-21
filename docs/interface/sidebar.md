# The sidebar on screen

This page is the reference for what the sidebar draws: every line, glyph, bar, and notice, and every key it answers to. Use it to look up something you saw. To learn how to read the column and how it decides which agent needs you, start with [the sidebar guide](../guide/sidebar.md). To restyle any color, glyph, or animation shown here, see [theming](../guide/theme.md).

The frames are plain text in the default Unicode glyph set, laid out the way the renderer prints them. Names, counts, ages, and dollar figures are examples. On screen each mark also has a color from your palette, but shape carries the meaning, so the column reads the same under `NO_COLOR`.

## The whole frame

The sidebar stacks three zones and a footer. The [cockpit](#the-cockpit) at the top and the [provider dashboard](#the-provider-dashboard) and [footer](#bottom-chrome) at the bottom stay fixed. The [agent cards](#the-agent-cards) scroll between them.

```
 ⌘ query-engine                     ~/code/query-engine     ← workspace name and path

 ◎ 91                           ◇ 32M ↘ 28M ↗ 3M ◌ 472M     ← sessions in the spend window, their tokens
 ¤ 16 ⑃ 1                                       $420.00     ← live agents, open PRs, spend
 ──────────────────────────────────────────────────────
 ? 3   ! 0   ⏸︎ 0   ✓ 8                  ⢿ 3   ☾ 1   ○ 1     ← make-up line: units by status

▎⑂ feature ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ⇡3  +127 -43  ⑃ main🮇    ← worktree header: branch, commits, diff, PR state
▌⣾ claude · Opus 4.8 · high · 200k                $1.27▐    ← status, handle, model, effort, window, cost
▌  store refactor                                      ▐    ← what the session is on
▌  ▣ ━━━━━━━━━━━━╺━╺─────────────────────── 38.2% ◔  7m▐    ← context meter and own quiet age
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                        ▐    ← selected card's tokens in the window
▌  ⧉ subagents (2) · ⧖ waits (3)                  $0.42▐    ← subagents and waits line
▌    ⠁ Explore · audit the trust hash                  ▐    ← running subagent
▌      ▤ 3k · Opus 4.8                               3m▐    ← its tokens, model, muted runtime
▌    ✓ Explore · locate the render seam              9m▐    ← finished subagent, frozen runtime
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

A running or waiting card with async questions adds a yellow `?` line below its description: the oldest question, clipped to the card width, followed by `+N` for additional questions. This reuses the waiting glyph and color without changing the card's status glyph, attention ordering, or notifications. Answer it [in the Codex pane](../reference/cli/asks.md#questions-while-codex-works) with Shift+Left; `rimz answer` refuses async questions.

| glyph | status | meaning | needs you |
|-------|--------|---------|-----------|
| `?` | waiting | asked you something: a permission, a plan approval, a question | yes |
| `!` | failed | the turn errored, died on a provider API error, went silent past the stall window (also a launched child whose first turn never started), or repeated one tool call 20 times | yes |
| `⏸︎` | paused | stopped mid-turn on a provider rate limit, overload, or dropped connection, or one of the children it launched is; the description quotes the reason in italics while the provider recovers; a pause that can no longer resume becomes `!` | after recovery: prompt the parked agent to continue, or let [auto-continue](../guide/loops.md#auto-continue) do it (off by default) |
| `✓` | done | the turn finished cleanly | a look |
| `⢿` | working | running a turn, waiting on in-flight work, or running a background shell; the cell animates through braille frames (`⣾`, `⣽`, ...), and any of them reads as working | no |
| `☾` | sleeping | resting on a timer, signal, condition, or file watch, including through a child | no |
| `○` | idle | alive, nothing in flight | no |

`?` is always yellow, `!` always red, and `⏸︎` always blue, whatever the card's age. What each status means in an agent's life is [the agent lifecycle](../guide/sidebar.md#the-agent-lifecycle). The mux tab bar reuses `!`, `?`, `⏸︎`, `⢿`, and `✓` as a static suffix on the tab name ([the sidebar guide](../guide/sidebar.md#glance-jump-answer)). A suffix always draws in these compact Unicode shapes, even when the column runs a configured [glyph set](../guide/theme.md#glyphs). A tab RimZ named after its agents goes back to the shell's name once they all exit; a tab you named yourself keeps its name.

On a process row, `!` means the process is stuck ([process rows](#process-rows)).

A running agent can show a head in place of `⢿`. Whichever head it shows, it counts as working in the cockpit.

| glyph | head | meaning |
|-------|------|---------|
| `⠁` | thinking | the turn has not edited a file yet; a research turn that never edits stays here to the end |
| `▇` | compacting | condensing its context window |
| `⢄` | waiting on subagents | delegated to running children; their entries are listed under the card; a launched child's provider park replaces this head with `⏸︎`, or `!` once it can no longer resume |
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
| `◔ 8m` | an age, never a duration. The face rounds down to completed quarters of the scale (eighths with Nerd Font), with a minimum first face. The full face appears only at the ceiling, then becomes `◉` past it: the provider's prompt-cache lifetime for a card's own age, `stalled_after_secs` for child quiet, and an hour for waits. |
| `9m`, `2h` | plain muted runtime, frozen for finished children; pipeline durations use two units from an hour up, such as `4h12m`. |
| `200k`, `1M` | the model's context window size |
| `$1.27` | cost in dollars, two decimals |
| `⋯ bg` after the description | the turn finished while background work it started is still running |
| `C 34%` `M 512M` `⇅ 8M/s` | a working process row's CPU, resident memory, and I/O rate |

Each token marker keeps one color everywhere: `◇` blue, `↘` deep red, `↗` cyan, `◌` green, `◍` violet.

### Worktree header glyphs

| mark | meaning |
|------|---------|
| `⑂ name` | a worktree group, named by its branch |
| `↩ name` | a worktree group whose work has landed |
| `# name` | an explicit `--channel` lane with no git state |
| `✓` `✕` `◌` beside the name | the trunk's HEAD-commit CI, or a branch's open or merged pull request's CI: passing, failing, running |
| `#91` | the branch's pull request |
| `#522→#530→#540,#541` | a same-repository PR stack, bottom first; comma-separated branches end the stack |
| `→` (`worktree.pr_stack`) | the next level of a PR stack |
| `⇡3 ⇣1` | commits ahead of and behind the trunk |
| `+127 -43` | lines added and removed against the trunk |
| `⟳` `✓` `✕` `⇉` `⑃` `≡` `⑂` before the trunk name | where the work stands; see [worktree headers](#worktree-headers) |
| `▸` | a collapsed finished group |
| `▸` / `▾` (`worktree.landed_closed` / `worktree.landed_open`) | the landed chip, closed / open; both glyph sets use these shapes |
| `+3 more` / `− less` | hidden idle rows; click to expand or collapse |

### Pipeline glyphs

Each declared stage gets one dot; `Done` has no separate slot. These roles live under `[theme.glyphs.<set>.pipeline]` independently of the clock and status glyphs.

| role | Unicode | Nerd Font | meaning |
|------|---------|-----------|---------|
| `passed` | `●` | `●` | before the current stage |
| `current` | `◉` | `◉` | current stage |
| `future` | `○` | `○` | after the current stage; in the `warn` tone when the run already reached it |
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
| `$` | the organisation spend limit of a Claude apps gateway |
| `ex` | paid extra usage |
| `api` | an API-key budget |
| `↻ 1h47m` | time until the window resets |
| `↻ 2` in a block header | Codex reset credits with auto-redeem off, or Claude limit resets: redeeming is yours to do ([`rimz accounts redeem`](../reference/cli/accounts.md#redeem) for either provider) |
| `⟳ 2` in a block header | auto-redeem armed: a credit would be spent if the longest window ran dry and parked an agent now |
| `‖ 2` in a block header | auto-redeem holding: a dry window would park the fleet until its natural reset |
| `· 3h` after the credit count | time until the nearest credit expires, within a week |
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
| `⑃ N` | open pull requests on the agents' branches, stacked pull requests included |
| `↑ 2 need you` / `↓ 2 need you` | the card that most needs you is above / below the view |
| `▌` ... `▐` | the selected card's left and right spines |
| `▎` ... `🮇` | the selection lane: every row of the group that holds the selection |
| `┄` | the dotted seal on the selected group's header |
| `┄ external ┄` | the group for panes outside the project |
| `─` | a section rule |
| `▐` / `▕` on the right edge | scrollbar thumb and track, drawn over the right spines while the cards scroll |
| `┤ Tab ├` | the active dashboard tab under `NO_COLOR` |
| `zᶻ idle`, `zᶻ away` | you are away from the terminal |
| `⇄ remote 210ms` | SSH link round-trip time |
| `/ auth` | the text search, above the dashboard |
| `⚠` | a pane-source notice or health alert |

`🮇` is from Unicode's Symbols for Legacy Computing block. A font without it may show a placeholder at the lane's right edge; nothing else depends on it.

## The cockpit

The cockpit is the top block. Its lines stay put as agents change status; only the unread banner and [parked workflow runs](#parked-workflow-runs) add or remove rows.

```
 ⌘ query-engine                     ~/code/query-engine

 ◎ 12                           ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 6 ⑃ 1                                          $4.20
 ──────────────────────────────────────────────────────
 ? 2   ! 1   ⏸︎ 0   ✓ 0                  ⢿ 2   ☾ 0   ○ 1
 ↑ 2 need you
```

Lines 2 and 3 cover the spend window, `[sidebar] spend_window`: `"session"` (the default) starts at your first prompt after five idle hours, `"24h"` is the trailing 24 hours, and `"today"` starts at midnight ([configuration](../guide/configuration.md#sidebar-rendering)).

| line | shows |
|------|-------|
| 1 | `⌘` and the workspace name in green. The project path sits on the right with your home directory as `~`; when space runs out the path loses its left end behind `…`. |
| 2 | `◎` sessions that ran in this room during the spend window, and their tokens on the right: `◇` total, which is `↘` input including cache creation plus `↗` output, then `◌` cache-read beside it. |
| 3 | `¤` live agents, `⑃ N` open pull requests on the room's worktree branches when there are any, stacked pull requests included and each counted once, and the room's spend for the window on the right. |
| then | one line per [parked workflow run](#parked-workflow-runs), only while a run is parked, above the rule. |
| 4 | the make-up line: units by status. |
| 5 | the `↑ N need you` or `↓ N need you` banner, only while it applies. |

Before any spend arrives the token breakdown shows zeroes and the spend shows `$0.00`. When an agent's cost moves, the spend counts up to the new figure within 1.2 seconds. Spend that grew while you were on another tab shows its current figure when you arrive, without counting up. It never decreases inside one window and resets when the window rolls. Antigravity reports current usage that cannot be summed over time, so its cost shows on its card and stays out of this total. How every figure is computed is [Token Insight](../guide/insight.md).

The room's scope is a path prefix: the project root and each grouped worktree root. A checkout you reach through a symlink spelled differently from the path in the agent's transcript can count as outside the room until the two paths agree.

The `⑃ N` colour reads each worktree's own pull request, since stacked pull requests carry no CI: red when any fails CI, yellow while one is running, green when every known verdict passes, and blue when CI is unknown.

### Parked workflow runs

A [workflow run](../guide/workflows.md) that parks for you adds one line under the spend line, oldest park first, and the line goes when you resume or cancel the run. The top zone does not scroll, so it shows at most three such lines; past three, a fourth line `? +N parked` counts the rest, and `rimz workflow list` names them:

```
 ◎ 0                                                            ◇ 0 ↘ 0 ↗ 0 ◌ 0
 ¤ 0                                                                      $0.00
 ? babysit-pr triage asked: Which compatibility fix should ship before the r 5m
 ? review sweep cap                                                          2m
 ──────────────────────────────────────────────────────────────────────────────
```

Each line is the waiting `?` glyph, the workflow, the node it parked at, and the park reason (`user`, `cap`, `deadline`, `timeout`, `invalid`, `asked`, or `check`), followed by its text when the park carries one: the question for `asked`, the check's own `reason` for `user`. The text loses its right end at the pane edge, and the time since the park stays on the right. The line is not a click target: `rimz workflow show <run>` and `rimz workflow resume <run>` act on it ([workflow commands](../reference/cli/workflow.md)). On a watched tab it appears within one data tick of the park.

### The make-up line

The make-up line counts units: a team's members within one worktree group, or one solo agent. Process rows are no unit. The left cluster is `?` waiting, `!` failed, `⏸︎` paused, `✓` done; the right is `⢿` working, `☾` sleeping, `○` idle. Every bucket shows even at zero, including a room holding only landed results.

Any member can add `?`, `!`, or `⏸︎`. A staged board at `Done` gives `✓`, even without an owner. Otherwise a visible owner's displayed status decides `⢿`, `☾`, or `○` (an owner showing `✓` also gives `○` until the board reaches `Done`). Other members add attention only. Without a usable pipeline and owner, the team's calm bucket is working first, otherwise sleeping, idle, or done. A solo unit follows its row. The calm bucket counts beside attention only when it is `⢿`. Only `✓` excludes finished groups. For example, a working team with idle teammates is `⢿ 1`, `○ 0`; a team with a waiting member and working owner is `? 1`, `⢿ 1`. The same team name in two worktree groups counts twice; cards in `external` count solo. `¤` still counts agents, so the buckets need not sum to it.

Picking a status shows every member of every unit that counts in that bucket. A filter chooses only which cards appear: shown cards keep their provider colour and open with their teammates exactly as without a filter. Selection keys (`j`/`k`, `g`/`G`, pages, `H`/`L`, `[`/`]`) reach every shown card; clicks, digits, `Enter`, and the `n`/`N`/`Space` inbox walk keep their usual behaviour within the shown roster. Changing the filter keeps a still-shown selection, without starting a browse pick; typing a search highlights its first visible row. Unread and open-PR lenses keep their per-card and per-group membership, respectively.

Click a non-zero bucket or the `⑃` count to filter the cards to it; press `u` to filter to unread cards. The filter applies in every tab of the room and stays on while you move between matching cards. The active filter is drawn as a filled chip, or in reverse video under `NO_COLOR`. To clear it, click it again, press its key again, press `A`, or pick another filter. It also clears when its count reaches zero. The filter keys are in [Keys and mouse](#keys-and-mouse).

At narrow widths the right-hand buckets clip from the idle end. A bucket without room for its full click target is not clickable; existing filter keys still work when their bucket is clipped.

### The unread banner

`↑ N need you` or `↓ N need you` counts units with at least one unread waiting or failed member. It appears when the card that most needs you, the oldest unread `?` or `!`, has scrolled out of view. The arrow points toward that card: up when above the painted view or hidden by the filter, down when below. During a wheel glide its visibility reflects where the cards will land, so it does not appear or disappear midway through the motion; its arrow follows the painted position. It is yellow for a waiting card and red for a failed one. Click it to reveal that card where it sits without changing selection, or scroll to the top if the filter hides it; press `n` to jump to the next card that needs you. It disappears when that card is on screen or nothing is waiting for you.

### An empty room

A room with no agents has no make-up line:

```
 ⌘ query-engine                     ~/code/query-engine

 ◎ 0                                    ◇ 0 ↘ 0 ↗ 0 ◌ 0
 ¤ 0                                              $0.00
 ──────────────────────────────────────────────────────
```

## The agent cards

The body holds one card per pane, grouped under the worktree the pane works in. When the cards outgrow the space they scroll between the cockpit and the dashboard, and a scrollbar shows on the right edge while the view moves, fading about a second after it stops (`[theme.display] scrollbar` pins or removes it). The selected card keeps its screen row while any line of it is visible; otherwise the topmost visible card holds the view. Cards above it growing, shrinking, expanding, collapsing, or re-sorting, and the unread banner appearing or disappearing, leave it in place. Previous selections stay open while the layout freeze is armed, even when there is room to collapse them. Only data or configuration changes with no room to absorb them still move the card at the top or bottom.

Selecting a card already on screen, even partially, scrolls nothing. Selecting an off-screen card with this sidebar's keys or mouse brings it into view with the least motion; a card taller than the view lands by its first line. Native tab switches, outside focus commands, and the initial focus on attach move the highlight without scrolling. A click into another tab places the card on the same row within that tab's card view. A toggle (`+K more`, a delegation header, or `+K older`) preserves the viewport instead of pulling it toward the selection. Opening the landed chip scrolls only far enough to show the first receipt it reveals. Unfolding a finished group also brings the member it focuses into view with the least motion.

A mouse wheel tick glides the cards to their new position over about a tenth of a second without moving the selection; under load the glide drops frames rather than slowing down. The view then stays anchored to what it shows, without the unchanged selection pulling it back. Only the wheel glides: selection reveals, the unread-banner click, jumps, and corrections for changes above land in one frame. Those corrections alone do not wake the scrollbar.

When a native switch or outside focus command leaves the focused card off-screen, a named edge marker appears after 500 ms on tmux or 300 ms on Zellij: `↓ <name>` below the view, or `↑ <name>` above it. It replaces an edge line without moving the cards. The unread banner takes priority over the up-marker; the down-marker can show alongside it. Click the marker to reveal the card with the least scrolling, without changing the selection. It disappears when the card comes into view or you select another card with the sidebar's own input.

Here the focused card sits below the view after a tab switch:

```
 ⑂ alpha
 ⣾ codex
   task-0
   ▢ ──────────────────────────    0%
 ⣾ codex
   task-1
   ▢ ──────────────────────────    0%
 ↓ codex
 ────────────────────────────────────
```

### The card

The selected card includes its stats line:

```
▌⣾ claude · Opus 4.8 · high · 200k                $1.27▐    ← identity line
▌  store refactor                                      ▐    ← description line
▌  ▣ ━━━━━━━━━━━━╺━╺─────────────────────── 38.2% ◔  7m▐    ← context meter and own quiet age
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                        ▐    ← selected card's stats line
```

| line | shows |
|------|-------|
| identity | The status glyph, then the agent's handle in its provider's brand color: its team role, explicit name, or profile, else the agent kind. Then, dimmer, the model, the agent's reasoning setting as its provider names it (an effort level such as `high` or `xhigh`, or `thinking`), and the context window size. The session's cost sits on the right once it rounds to at least `$0.01`, and counts up like the cockpit spend. |
| description | The open ask while waiting for input, when available, led by its first question and wrapped over at most two lines. Otherwise the displayed error or pause reason in italics, else the first working wait's label while working through a wait or first sleeping wait's label while sleeping, else what the session is on: the first of the session name, the provider's thread preview, the launch description, the agent's task, the session's first prompt, and its latest prompt. Codex's automatic title and Claude's `--name` or `/rename` count as the session name; a name that only repeats the start of the prompt is skipped. |
| context meter | The percent of the context window in use, as a bar. The fill shows where the window went: a run of cache reads first, then a segment each for cache writes and fresh input, each starting with `╺`. The bar runs from green toward red as the window fills, at the stops set in [`[theme.display.context_meter]`](../guide/theme.md#display). Once the prompt cache expires, the fill becomes one solid greyed-red run with no segments or caps; the measured fraction and percent stay the same, and `▣` wears the same expired-cache tone. After a compaction completes, the meter shows the provider's post-compaction figure where it reports one and an empty `0%` otherwise, until the next model call reports usage. |
| stats line | On a selected or held engaged card, or under `expanded` density: `▤` tokens in the window, the latest API call's cache-read, cache-write, and fresh input added together, which is the amount the meter's percent measures. Then the same split in numbers, the session cache hit, `↻ N` compactions, and `⟲ N` repeated tool calls. From 46 columns the age is on the meter; below that it pins right on this line. |
| cache line | Wherever the stats line shows, after stats: a countdown once the cache warning band starts, then the expired-cache notice and available rebuild estimate. |
| subagents and waits | Present once the session has spawned a subagent or armed a wait, except on closed, unreached idle and done cards. See [Subagents and waits](#subagents-and-waits). |

A waiting description selects the first sentence ending in `?`, or the whole ask when none does. Sentences end at `.`, `!`, or `?` followed by whitespace or the end of the text; a colon stays with its question. The selected text word-wraps at the description's two-cell indent. Short asks stay one line; text beyond two lines, or a word wider than a line, leaves `…` at the end of the last shown line. The separate queued-question line keeps its one-line form.

Before expiry the card reads `◷ cache expires in 8m`, in the clock's yellow-to-orange-to-red tone; minutes round up, with `in 1m` at the lifetime boundary. After expiry it reads `◉ cache expired · ◍ ~$1.40 to re-warm`: the notice uses the muted expired-cache tone, the seam stays muted, and the estimate uses the cache-write tone. Below 46 columns these become `◷ expires in 8m` and `◉ expired · ◍ ~$1.40`. The estimate rounds to two significant figures, keeping cents below $10 and whole dollars above; an unknown price or a figure below half a cent leaves only the expired notice. There is no cache line while calm, held warm, or without a known lifetime. Like stats, it stays on previously selected held cards and shows on every engaged card under `expanded` density, including teammates.

On a narrow sidebar the identity line drops the reasoning token first, then the model and window, and keeps the handle. The window size shows on non-idle cards only. At 46 columns or wider, every context meter reserves a fixed five-cell age cluster after its percent, blank until five minutes quiet. Below 46 columns the meter keeps its full bar and the age moves to the right of the selected or held card's stats line. Selected, resting, fresh, and compact meters all use the same slot, keeping their bars and percentages aligned.

On the selected or held card's stats line (every engaged card under `expanded` density), a column that is zero or unreported is left out. A provider that reports only session totals, as stock Droid does, shows `◇ total ↘ input ↗ output ◌ cache-read` on this line instead, and its meter stays empty. The cache hit is cached input divided by all input, and it is absent until the session has input counters.

The meter draws windows up to 256k tokens linearly and larger ones on a log curve that reaches full strength at 1M, so a large window keeps detail in its working range. The percent is always the raw share in use. The age clock fills over the provider's prompt-cache lifetime (60 minutes for Claude, 30 for Codex, or your [`[harness.prompt_cache_ttl]`](../guide/configuration.md#idle-compaction) override). Its warning band and cache countdown start after half that lifetime, or in the last ten minutes if earlier (the whole lifetime when shorter than ten minutes). The clock stays muted before the band, then slides from yellow through orange to red at expiry. Past the lifetime it switches to the ringed `◉` face and, with the meter's fill and lead glyph, takes the greyed-red expired-cache tone: no action can save a cache that has already expired. A provider with no known lifetime fills the clock over an hour, muted throughout, and never expires the meter. While a [keep-warm horizon](../guide/configuration.md#keep-warm) is actively holding the cache, the clock instead shows the time since the agent's last real turn in steady green, filling over the horizon; once the horizon passes or its stage condition stops holding, the usual ramp resumes from the last ping. Wait ages and the card's breathing keep the hour scale; child quiet uses the configured stall window, and durations never heat. Outside a hold the age clock counts from the later of the agent's own activity and its last cache ping, so a ping restarts the ramp without reading as work. The age clock measures the agent's own quiet time, so a parent waiting on its subagents keeps counting while they work: its own session is making no call, and its cache ages the whole wait. The children's own times ride their entries under the card. The context meter's bands, log scale, and tones are set under [`[theme.display]`](../guide/theme.md#display).

From a minute up, coarse ages share one scale: whole minutes (`12m`), hours (`3h`), then days (`2d`), floored and capped at `99d`. Each fits in three cells. Child runtimes keep `<1m` below a minute; card quiet ages stay hidden below five minutes.

### Card shapes

A card's slots are set by its stage, age, density, and selection reach. Data fills those slots in place; a waiting description can take a second line.

| card | lines |
|------|-------|
| stale: read idle or done, quiet past `stale_after_secs` | one line: state, handle, description, cost, and age; selection, a held expansion, teammate reach, or a subagents and waits section clicked open restores the card |
| fresh: idle, never prompted, including teammates and held cards | identity, plus the description when it was launched with one |
| fresh, live-selected | adds the empty meter `▢ 0%`, and animated dots (`.`, `..`, `...`) in the description slot when there is no description |
| engaged, resting: after its first prompt | identity, description, meter, with `▢ 0%` until data arrives |
| engaged, with subagents or waits | adds the subagents and waits line, except on unreached idle and done cards with their entries closed |
| selected or held, engaged | adds the stats line (`▤ 0` until data arrives) below the meter and lists the subagent and wait entries below the card; only the live selection lights the spines |

Selecting an engaged card inserts its stats line below the meter and appends entries; the lines above the meter stay in place. When the selected agent belongs to a named team, every visible matching engaged teammate's card opens too, but without stats, and with its subagent and wait entries folded behind the `⧉ subagents (n)` / `⧖ waits (n)` summary line. Previous engaged selections keep their stats and entries, and their teammates' open shapes, until the layout freeze expires. Fresh teammates and held cards stay at identity plus any launch description. Wheel ticks, unread-banner jumps, and toggles keep those cards open; explicit section toggles still apply immediately. Only the live selection wears the selected spines. `card_density = "expanded"` keeps stats on every engaged card and lists entries everywhere, teammates included.

`[theme.display] card_density` changes how much a resting card shows:

| value | resting cards |
|-------|---------------|
| `auto` (default) | the shapes above |
| `expanded` | every engaged card keeps its stats line and lists its subagent and wait entries |
| `compact` | idle: identity. Running and waiting: identity, description, meter. Paused, done, sleeping, failed: identity and description. Selecting a card restores its full shape. |

`[theme.display] stale_after_secs` defaults to 86400 (24 hours); `0` disables shrinking. Only read idle and done cards shrink, using the card's last activity including its children. Stale takes priority over `compact`; `expanded` never shrinks cards. Every width tier keeps the description, cost, and coarse age on the stale line, clipping the description to fit. The age keeps that child-inclusive quiet time but wears the same cache tone as the meter's age pin, without a clock face.

### Subagents and waits

```
▌  ⧉ subagents (2)                              $0.42▐
▌    ⠁ review · audit the trust hash                 ▐
▌      ◇  3k · Haiku 4.5                          <1m▐
▌    ✓ Explore · locate the render seam         $0.42▐
▌      ▤ 12k · Opus 4.8  · high · ↻ 2              1m▐
```

A card with waits uses the same entry layout:

```
▌  ⧖ waits (5)                                       ▐
▌    ◷ timer · in 12m                           ◔ 18m▐
▌    ⣾ pid · 16776                              ◔  3m▐
▌    ⣾ command · cargo                          ◔  4m▐
▌      cargo xtask gate --name foo_test              ▐
▌    ⌁ signal · pr.merged                       ●  1h▐
▌    ◷ stop · after 3m idle, in 1m              ◔  2m▐
```

`⧉ subagents (N)` counts every child the session has spawned, both the provider's native subagents and children launched with [`rimz subagents`](../reference/cli/subagents.md), for as long as RimZ retains the session's history. Their known cost sits on the right. The card's cost on the identity line already includes it, so do not add the two. `⧖ waits (N)` counts armed one-shot [waits](../reference/cli/wait.md), a pending [`stop --when-idle`](../reference/cli/agents.md#stop) request, and, for Claude, the shell commands it left running in the background. Either half shows alone when only one applies, and below 46 columns the line shortens to `⧉ N · ⧖ M`.

Selecting a card opens the section. Clicking the line opens or closes it and focuses the card's own live pane, even when one of its child panes is focused. The open or closed choice outranks selection and `card_density` until the sidebar restarts, and follows the viewed sidebar into unviewed tabs. Select an idle or done card first to reach the line unless it is already open, held, a teammate, or under `expanded` density. In `compact` density, select the card first.

Click either line of a launched child's entry to focus its live pane, across tabs when needed. Entries without a live pane, including provider-native children, still focus the parent; a click never resumes an ended child. A current waiting child counts its parent in the cockpit's `?` bucket without changing the parent's displayed status or lead glyph. When the parent's attention comes from a waiting launched child, `n`/`N`/`Space` focuses the first such child's live pane in entry order; with no such pane it focuses the parent. The parent's own waiting or failed status takes precedence. While a child's pane is focused, the parent card carries the selection band and both lines of that child's entry show `▌` in the indent beside the lead icon. The mark disappears when you browse to another card; with the section closed, only the parent band shows. Focusing a child does not mark the parent read; the parent clears as any card does, by focusing it or dwelling on a tab that shows its pane.

Each entry starts with its live state or wait icon, then a type word and a ` · ` separator before the headline. A subagent's type word wears its own provider's brand color at the soft weight of a calm card's handle, even when it runs on a different provider from its parent; wait type words keep the body tone. Without a headline, the separator disappears too. A launched child's displayed error label replaces the description or task in italics, for both paused and failed states. Detail sits on an indented second line; model names stay muted.

| entry | lead | type · headline | right side | second line |
|-------|------|------|------------|-------------|
| running subagent | `⠁` while it reasons, `⢿` while it acts | launch profile or kind · description, else task if different from the type | cost, when known | `▤` tokens in the child's window when the child reports its context, else `◇` tokens over its whole run, then model, effort, and `↻ N` completed compactions when above zero, dropped whole when the line is too narrow; plain muted runtime pins right, replaced from five minutes quiet by a face and quiet age heated toward the configured stall threshold |
| parked subagent | `⏸︎`, or `!` once it can no longer resume | launch profile or kind · limit label in italics, else description or task | cost, when known | tokens, model, effort, then `↻ N` completed compactions when above zero, dropped whole when the line is too narrow; plain muted runtime pins right, counting while parked and frozen once failed |
| stalled subagent | `!` | launch profile or kind · `silent Nm` | cost, when known | tokens, model, effort, then `↻ N` completed compactions when above zero, dropped whole when the line is too narrow; quiet age pins right with a face, in alarm at and past the stall threshold |
| finished subagent | `✓` or `!` | launch profile or kind · description, else task if different from the type | cost, when known, after frozen runtime when there is no second line | tokens, model, effort, then `↻ N` completed compactions when above zero, dropped whole when the line is too narrow; plain muted runtime pins right, frozen at the child's last activity |
| timer | `◷` | `timer · in 12m`, or `timer · due` once the time passes | time since armed | never |
| PID | working spinner | `pid · 16776` | time since armed | never |
| command | working spinner | `command · cargo` (program name) | time since armed | full command, with the program path trimmed |
| check | working spinner | `check · test` (program name) | time since armed | full command, with the program path trimmed |
| file | working spinner | `file · app.log changes`, or ``file · app.log matches `<pattern>` `` | time since armed | never |
| background shell | working spinner | `shell ·` description, else program name, else just `shell` | time since RimZ first saw it | command when known, with the program path trimmed |
| signal | `⌁` | `signal · pr.merged` (selector) | time since armed | never |
| idle stop | `◷` | `stop · after 3m idle, in 1m`: the requested rest, then the time left or `due` while the agent rests and its clock runs | time since requested | never |

Entries list in a fixed order, and opening more of the list only appends rows:

1. Live subagents, including paused children, in the order they started. They are never capped.
2. Finished subagents, newest first, that finished within `recent_subagent_secs` (900 seconds by default), up to `max_recent_subagents` (5 by default).
3. Waits: timers by due time, then watches, then background shells, then signals. A wait leaves the list when it fires or its job ends.
4. `+K older`, which folds every other finished subagent. Click it to append them and focus the parent's live pane. When no subagent is running or recent, opening the section lists the older ones directly.

The listed subagents plus `K` equal the count in the line. `K` can exceed the rows it reveals, because a native subagent that was replaced while still running is counted and has no row. The older list folds again when you close the section, send the parent a new prompt, or run `/clear` or `/compact`. Messages from other agents and RimZ's own deliveries leave it open.

A subagent's `▤` glyph heats from green to red on the worse of window percent and window tokens when the child reports its window size, using the same bands as a card's context line. Without a reported window size, it heats on tokens alone. A running subagent's clock keeps its own quiet-age ramp over the configured stall window. A finished subagent's clock stays muted, and so does a wait's, since a wait is pending by design. A wait has no clock when its arm time is unknown. A subagent without metadata shows a single line; command and check waits show their command on a second line. Subagents never get a card of their own while their parent's card is visible. Which fields each provider reports for a child is in [sidebar internals](../internals/sidebar/sidebar.md#sub-agent-lists).

### Unread and attention marks

A card is unread from the moment it becomes waiting, failed, paused, or done until you focus its pane or press `m`. An unread card has a soft wash behind it. The one card that most needs you, the oldest unanswered `?` or `!`, also animates across its glyph, name, and description, and the make-up bucket that owns it animates with it. Every other unread card is bright and bold without motion, so one thing on screen moves at a time. A card that is both selected and unread shows the selection. The animation style is `[theme.animations] unread` ([theming: unread attention](../guide/theme.md#unread-attention)).

A waiting or failed card looks like any other card with `?` or `!` in the lead. A waiting card shows its open question on the second line when one is available, otherwise its session label:

```
▌? codex · GPT 5.5 · xhigh · 272k               $4.36▐
▌  Allow the pricing cache write?                    ▐
▌  ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━──────────   71% ◉ 40m▐
▌  ▤ 193k · ◌ 185k ↘ 7k ↗ 24k                        ▐
▌  ◉ cache expired                                   ▐
```

When a turn dies on a provider API error, the description shows the provider's error text for as long as the `!` holds:

```
▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇
▌! claude · Opus 4.8 · high · 200k              $1.27▐
▌  API Error: Bad Request                            ▐
▌  ▣ ━━━━━━━━━━━╺━╺────────────────────── 38.2%      ▐
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                      ▐
```

Two checks raise `!` without a report from the agent. A working agent that stays silent for 30 minutes becomes `!`, or `⏸︎` when its provider budget window is spent. Any open native or launched child can also become `!`, labelled `silent Nm`; sleep, pause, and displayed errors exclude it from this check. A parent waiting on a child still displayed as working is exempt. A stalled child no longer holds its turn, and a native child's failure lifts the parent card's attention because it has no pane of its own. A working agent that repeats one tool call with the same arguments shows `⟲ N` from the third call on the selected or held card's stats line, or under `expanded` density; at 20 it becomes `!` and the description reads `loop: <tool> ×<count>`. Any different call clears the run. All three thresholds are under [`[agents.attention]`](../guide/sidebar.md#tuning).

A sleeping card names its first sleeping wait in the description:

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
| signal | `wakes on pr.merged` |
| subagent | `wakes when @calm-fox reports` |
| team | `wakes when forge#feat-x reaches Done` |

Subagent waits draw no separate entry and do not count in `⧖`: the child is already visible, as a row under `⧉` or, for a peer launched with `rimz agents`, as its own root card. A team wait counts in `⧖` and draws a static `team · stage Review` entry with its elapsed age, or `stage unknown` when its board is unavailable.

`☾` replaces only an idle or done status, for timers, signals, conditions, and file watches, including a child's sleeping wait. A PID, command, check, unfinished team, working child, or listed background shell instead keeps the card working. A working-through-wait card names its first working wait. Waiting, failed, and paused (including a launched child's provider park) take precedence, and a standing subscription does not make an agent sleep. Sleeping opens no unread mark and sends no notification, and an earlier unread result stays unread through the sleep.

Hibernation does not change a card's state, glyph, ranking, or unread result. A frozen agent keeps the same working or sleeping wait classification, or its idle or done state when nothing is pending. `rimz agents show` carries the hibernation details.

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
▎↩ feature-landed ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ✓ main🮇    ← merged: safe to remove
```

On the left: `⑂` or `↩`, the branch name, the CI verdict (`✓` passing, `✕` failing, `◌` running), and the pull request number when the branch has one. An open or merged pull request supplies the CI verdict; a non-trunk branch without one shows no CI glyph, while the trunk shows its own HEAD-commit CI. Closed pull requests show no CI glyph. The team badge goes on the pipeline line when one is drawn, on the roster line when the group is folded, and on the header otherwise. In terminals that support hyperlinks, each PR number opens its pull request. A same-repository stack on GitHub or Gitea replaces the single badge with its levels, bottom first: `#522→#530→#540,#541`. Commas join PRs branching from the same level, where the display stops. The stack appears only when the entire run fits beside the full name and CI; otherwise only the branch's own PR number remains. When two groups have the same branch name, each adds a muted `· repo` qualifier, the shortest path suffix that tells the checkouts apart. When the header runs out of room, the name shortens first, then the pull request number is dropped, then the CI verdict last.

On the right: commits ahead of and behind the trunk with zero counts left out, then lines added and removed, then the trunk marker. The line counts include committed, staged, unstaged, and untracked work, so work that `git diff` does not show still counts. The first marker that applies is shown:

| marker | meaning |
|--------|---------|
| `⟳ main` | a local rebase, merge, or cherry-pick is in progress |
| `✓ main`, with `#N` on the left | the pull request merged; muted, and the commit and line figures are dropped |
| `✕ main` | the pull request closed unmerged; red, and the figures stay |
| `⇉ main` (` main` with Nerd Fonts, `worktree.pr_queue`) | the open pull request is in the merge queue; red when the queue removed it without merging it |
| `⑃ main` | the pull request is open |
| `✓ main`, no `#N` | no pull request, and the trunk already contains the work: safe to remove |
| `≡ main` | pristine: a clean tree with no commits of its own, at the trunk's tip |
| `⑂ main` | a plain branch |

Pristine and merged headers show the marker alone. The trunk's own checkout shows no marker, only its commit and line figures. RimZ takes the trunk from `[sidebar] trunk`, then `main`, `master`, and the remote's default branch ([configuration](../guide/configuration.md#sidebar-rendering)).

The group that holds the selection is drawn as a lane: a dim `▎` down its left edge, `🮇` down its right, and a dotted seal across its header. Inside it the selected card has bright `▌` and `▐` spines over a recessed background, subagent entries included. Other groups have a blank gutter and a header without the seal. With no selection, every card rests at its configured density: no card or teammate opens by selection, and no group has a lane, spines, or a header seal. Expanded density still lists delegation entries; where a delegation header is shown, its override still controls the entries.

No selection, compact cards:

```
 ⌘ query-engine

 ◎ 0                                  ◇ 0 ↘ 0 ↗ 0 ◌ 0
 ¤ 2                                            $0.00
 ────────────────────────────────────────────────────
 ? 0   ! 0   ⏸︎ 0   ✓ 0                ⢿ 0   ☾ 0   ○ 2

 ⑂ main · forge
 ○ planner · opus
 ○ coder · opus



 ────────────────────────────────────────────────────
  W: ◎ 0  ◇ 0 ↘ 0 ↗ 0 ◌ 0                       $0.00
  M: ◎ 0  ◇ 0 ↘ 0 ↗ 0 ◌ 0                       $0.00

                                           ? for help
```

```
▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇    ← selected group: lane spine and dotted seal
▌? claude                                              ▐    ← selected card: bright spine on both edges
▌  permission                                          ▐
▌  ▢ ──────────────────────────────────────    0%      ▐
▌  ▤ 0                                                 ▐
▎⣾ codex · GPT 5.5 · 272k                              🮇    ← same group, not selected
▎  add tests                                           🮇
▎  ▢ ──────────────────────────────────────    0%      🮇

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ? 1     ← panes outside the project, with its own ? and ! count
 ? claude
   Deploy staging?
   ▢ ──────────────────────────────────────    0%
```

Panes outside the project (scripts, CI, stray shells) collect under the dim `external` divider, which always sorts last. It keeps a `? n` or `! n` count so an ask from outside the project still shows.

A room opened on a plain directory groups git-backed agents by their checkout, each under a full header. The panes the directory itself holds sit under a bold header with the directory's name, with no glyph or git figures, because a plain directory has no trunk to compare against.

```
▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ⇡2  +12 -3🮇    ← a checkout inside the directory: full header
▌⣾ claude                                              ▐
▌  db migrate                                          ▐
▌  ▢ ──────────────────────────────────────    0%      ▐
▌  ▤ 0                                                 ▐

 agents                                                     ← the directory's own group: name only
 ○ zsh
```

A group with one staged team and a readable worktree `blackboard.md` containing a `Stage:` line gets a pipeline line directly below its header, above the first card. The team's members must identify one worktree:

```
▎⑂ pipeline ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇
▎  forge ● ● ◉ Implement 47m ○ ○                  47m🮇
▌⣾ planner                                           ▐
```

The team badge leads, followed by the track; a stage outside the declared pipeline has no track and shows ` · ` between the badge and its name. The dots follow declared stage order: passed, current, then future, with the current stage's name immediately after its dot. Passed dots are green and future dots are muted unless already reached this run, in which case they are yellow: the shape says not done, the color says been there. Flipping backward moves the current dot back and warms the dots it left behind. At `Done`, every dot is passed and the last becomes the green done seal, followed by `Done` to keep the meaning visible without color. A board stage outside the declared pipeline shows its name without a track. The current dot and stage name take the visible stage owner's status color and animation phase, or stay muted when that owner has no visible card. The line itself adds no attention. An unfinished stage with a resolved owner and only idle or successful members, with no attention from their current children, raises `!` on the owner after `[agents.attention] stalled_after_secs` without member activity or a stage flip. Its error line reads `stage <Stage>: nothing running <age>`, with the usual unread and notification behaviour.

Without color there is no revisit cue; a forward re-entry warms nothing to its right.

The stage duration sits immediately after the stage name, such as `Implement 4h12m`; the run total alone is pinned right, such as `4h51m`. Both are muted, without a clock face or slash. Below an hour a duration uses one whole unit (`45s`, `18m`); from an hour it uses two scaled units (`1h00m`, `4h12m`, `1d02h`). An unknown stage time leaves just the name; an unknown total leaves the right side empty. At `Done`, only the frozen total appears. The stage clock sums every visit to the current stage since the run started, counted from parseable entries in the board's `## Progress` (or `## Progress log`) ledger, including an `opened` entry. A backward flip resumes the stage's accumulated time; a same-stage re-flip changes nothing. The total starts at the first parseable ledger entry (or the latest flip out of `Done`, which also clears earlier visits) and runs until now, or until the last `-> Done` entry at `Done`; a `Done -> Done` re-flip does not move the stop. On a RimZ-written board the stage time never exceeds the total. A missing or future start omits that duration, and `Done` without a valid stop omits the clock. If a hand-edited `Stage:` disagrees with the ledger's open stage, the stage clock is absent. Clicking the line focuses the visible stage owner's pane, otherwise the first actionable team member, otherwise the group's first visible row.

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

A group is finished when its pull request merged or closed, or the trunk contains its work, its tree is clean, and no member is working or blocked. Every finished group with an agent collapses to its header and a two-line receipt, unread results included:

```
 ↩ merged-work                                   ✓ main
 ▸ rimz  ✓ planner  ✓ coder  ✓ reviewer           $4.02
   ◇ 1M ↘ 300k ↗ 80k ◌ 900k · 75%                    2h
```

The first line lists the team name when the members share one, each member's final status and name, `+n` for members and shells that do not fit, and the group's lifetime cost. The second shows its lifetime tokens and cache hit, with plain muted active duration on the right, or an age with a clock face since it finished once that record expires. Both cover every session the team ran in this worktree, resumed sessions and subagents included. When the line is too narrow for one member it reads `▸ +K done`.

Click a folded group's header or either receipt line to show the cards and focus the first receipt member's live pane, bringing its card into view. Focusing any member also shows the cards. The expanded header only collapses them; it does not move focus. The `s` lens excludes results in finished groups. Each revealed card shows that member's lifetime cost, so the cards add up to the receipt. A single-agent receipt uses the same status-and-handle chip; until lifetime totals arrive, the receipt pins the hidden agents' current session costs instead. A group with only process rows stays open.

Finished groups follow all unfinished project work and precede `external`. At 54 columns, the closed landed chip replaces their headers and receipts:

```
 ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ [ ▸ 3 landed ] ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
```

Click anywhere on the chip's line to open it. If the receipts it reveals are below the fold, the cards scroll only far enough to show the first receipt in full, keeping the chip on screen. When the chip and receipt cannot fit together, the chip pins to the top of the card view and as much of the receipt shows as fits. Receipts already in view leave the viewport still. Click the chip again to close it; the selection never moves, though the chip may. Under `NO_COLOR` the chip is reverse video. The choice follows the viewed sidebar into unviewed tabs, starts closed after a host restart, and is mouse-only like `+K more`. Open groups remain collapsed to receipts until revealed:

```
 ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ [ ▾ 3 landed ] ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄

 ↩ landed-b
 ▸ ✓ landed-b

 ↩ landed-c
 ▸ ✓ landed-c

 ↩ landed-a
 ▸ ○ landed-a
```

The count is groups, not agents. A focused, held, or individually expanded group stays visible below the closed chip without changing the count; process-only groups also stay visible. A filter or search suppresses the chip and reveals matching cards uncollapsed, but `s` never matches finished groups. Unread results there remain reachable through `u`; `n` and `␣` skip hidden cards, while the actionable `↑/↓ N need you` banner is unchanged. `rimz sidebar frame` starts closed; `--expand` opens the chip and every card. On narrow panes the dotted rule shrinks before the label is trimmed from the right.

A folded finished group hides its pipeline line and puts the stage after the team name on the roster line:

```
 ⑂ pipeline
 ▸ forge · Done  ✓ planner  ✓ coder
```

Expanding the group restores the pipeline line. The order of cards and groups follows status and age, and read state never moves a card. The rules are in [how the column is ordered](../guide/sidebar.md#how-the-column-is-ordered).

## The provider dashboard

Provider budgets belong to an account, and every session of that provider shares them, so they sit in a fixed panel at the bottom instead of on the cards. Each provider gets one block, for the account the room launches under; sessions still running on another account count toward none. A provider whose account has metered budgets or recorded usage keeps its block even when none of its sessions runs in this room, and RimZ refreshes its budgets between turns.

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
| header | In tabs: the plan, then the version. Stacked: the provider and version, then the plan. The version reads `v?` until a source reports it, and the plan is absent until the account names one or while subscription access is lapsed. `⇅ rc` and the reset-credit marker (`↻ N`, or Codex-only `⟳ N` or `‖ N`) sit on the right; a lapse suppresses reset credits too. |
| stats | `◎` sessions in the [spend window](#the-cockpit), the `◇ ↘ ↗ ◌` tokens with cache creation counted in `↘`, and the provider's spend. A provider with no usage history keeps the row: `◎` counts its sessions active in this room, and the token and dollar slots show `–`. |
| budget rows | one bar per budget window: label, bar, and reset countdown; while lapsed, one bold alarm row replaces every bar |

Stacked blocks (`provider_tabs = "never"`):

```
 Claude v2.1.169 · Claude Max                      ⇅ rc

  ▐▛███▜▌  ◎ 53  ◇ 16M ↘ 13M ↗ 2M ◌ 198M        $188.88
 ▝▜█████▛▘ 7d  ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱ ↻  5d22h     ← spent: empty track, red on screen
   ▘▘ ▝▝   ex  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱      $43     ← paid extra usage, $43 left

 Codex v0.137.0 · ChatGPT Pro 200        ⟳ 2 · 3h  ⇅ rc

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

A lapsed subscription keeps the historical stats but removes the plan label and replaces all bars with one bold alarm row. The header uses the account's valid display label or its name (here `work`), and the row names the product alone. The first-seen local timestamp is truncated with the row at narrow widths, never wrapped:

```
────────────────────────────────────────────────────────────────────────────────────────────────────
Claude · work v2.1.158                                                                          ⇅ rc

 ▐▛███▜▌  ◎ 12  ◇ 498k ↘ 434k ↗ 64k ◌ 68k                                                      $3.50
▝▜█████▛▘ plan lapsed · no Claude access · since <first seen>
  ▘▘ ▝▝
```

| you see | meaning |
|---------|---------|
| `plan lapsed · no Claude access · since …` and no bars | Claude Code subscription access was rejected. The header has no plan label, and the timestamp is when RimZ first saw the rejection, in local time. A successful usage read clears the lapse; it does not age into an unknown track. |
| a partly filled bar and `↻ 1h47m` | The window is running. The fill slides from green through gold and amber to red as it empties. |
| a full bar and no countdown | The window has not started. Providers start the clock at your first token, so there is no reset to count down to yet. |
| an empty track, all red | The window is spent. |
| a short window painted spent while a longer one is spent | The longer window gates it: a spent `7d` makes `5h` unusable until the `7d` resets, so `5h` shows red with no countdown whatever its own reading. |
| a dim empty track and no countdown | The reading is unknown: RimZ holds only a cached reading whose window has since reset, or the provider has not reported its windows yet. |
| a full bar and `∞` | No limit applies: an API key without a budget, extra usage with no limit or balance figure, a quota the provider reports as unlimited, or a limit the provider has lifted for now. |
| an `ex` row | Paid extra usage, shown while an included window is spent and extra usage is available: the longest spent window comes first, then `ex`. The figure is dollars left, never the budget size. With a limit the bar drains; without one a positive balance draws full. Reported unlimited extra usage draws full with `∞`, whether or not a display ceiling is set. There is no `ex` row when the provider reports nothing, extra usage is off, or no dollars are left: the dashboard then shows the included windows (`5h` and `7d`) in its place. Dollar figures on `ex` and `api` floor: cents below $10 (dropping `.00`), grouped whole dollars below $100,000, whole thousands below $1 million, then millions with one decimal. |
| an `api` row | An API key. With a display budget configured, the bar drains against your trailing-month spend and the dollars left sit on the right. Without one, the bar is full and reads `∞`. |
| `cr`, `bld`, `dep`, and other names | A quota the provider or a plugin names, such as Copilot's AI credits. Named quotas are independent: one spent quota does not mark another spent. A named quota with no reported length shows its reset in a quiet tone. |

The color of the `↻` glyph beside the countdown shows your pace. It is neutral when the current burn rate lasts to the reset, slides through gold and amber to red as spending outruns the window, and cools toward green when spending runs well under pace, once 40% of the window has passed. The stops are `[theme.display.budget_bar.burn_rate]` ([theming: display](../guide/theme.md#display)).

A model's own cap inside a window draws as a `╱` tick on that window's bar instead of a row. The fill and the tick use different scales: the fill is the window's budget left, and the tick's position is the share of the model's cap left, across the full width of the bar. Only exactly 0% reaches the left end and only exactly 100% reaches the right. The tick's color follows the model's share, and it has no label or countdown. In [the whole frame](#the-whole-frame), the `7d` bar has about 65% left while the tick near 42% says the model's weekly cap is closer to spent than the window. A spent or gated bar draws no tick; the model's reading returns with the parent's reset. An unknown reading or an unlimited row has no tick.

A block header counts banked Codex reset credits or Claude limit resets with `↻` in the body tone: redeeming is yours to do ([`rimz accounts redeem`](../reference/cli/accounts.md#redeem) for either provider). The marker is absent at zero. Pi and OpenCode accounts using Anthropic show the same banked count. Only Codex forecasts what [auto-redeem](../guide/loops.md#auto-redeem) would do if the longest window ran dry and parked an agent right now: a green `⟳` means armed (a credit would be spent, or no natural reset is known to wait for). A muted `‖` means holding: the fleet would park until the reset. When the reset is nearer than `resume.auto_redeem_min_gain` and the nearest credit outlives it by a day, it stays a hold until then. With Codex auto-redeem off, RimZ still rescues a credit in its last 30 minutes; Claude redemption remains manual only. Within a week of the nearest credit's expiry the time left follows the count, floored to one unit (`· 6d`, `· 14h`, `· 29m`). That time heats from green to red while the marker is `↻`, and deepens from the body tone to green while auto-redeem handles it. The marker holds still in every state. The Nerd Font glyph set draws the three as `󰑐`, `󰁪`, and `󰏤`.

How budgets are read, cached, and refreshed is in [provider internals](../internals/agents/providers.md).

### Pets

With `[theme.pets] enabled = true`, the active block narrows and an animated companion with a caption sits at its right edge. It draws as pixels where the terminal supports kitty graphics (15 by 9 cells) and as cell art otherwise (18 by 9 cells). It is hidden under `NO_COLOR` and when the pane is too narrow. See [pets](../guide/pets.md).

### Narrow panes

The pipeline reserves the whole run total, then admits the first shape that fits: track, name, and stage time; track and name; name and stage time; or an ellipsized name. The team badge fits only beside a drawn track or an undeclared stage and drops first. Stage time can reappear after the dots drop; dropping a declared track never buys the team badge back. The name keeps two cells beside an admitted total, and neither duration nor track draws partially.

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
 zᶻ away                 ? for help
```

| mark | when |
|------|------|
| `? for help` | always, at the right edge |
| `zᶻ idle`, then `zᶻ idle · 17m` | tmux: no input for `[sidebar] afk_after_secs` (15 minutes by default); minutes are added after the first minute |
| `zᶻ away` | no terminal client is attached; Zellij reports only this state |
| `⇄ remote 210ms` | the room runs over SSH: the smoothed round-trip time, with packet loss added above 10%; `⇄ remote ?` means the last reading is stale |

The away badge takes the left edge. The remote badge sits there otherwise, and follows the away badge only when the line has room. The remote badge's color runs from green through yellow and amber to red, bold at the worst, and stays neutral until the link has a first reading.

### Notices and alerts

When the room opened with configuration problems, a dim notice above the footer keeps them visible: `⚠ 2 settings skipped at start · rimz doctor` (singular `setting` for one). Doctor lists the settings, warnings, and fixes. Fixing the configuration clears the notice at the next start or attach; an empty or unreadable room record shows no notice.

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

While the alert is active, `/` does nothing; `r` reloads the tab, `x` dismisses the alert, and `?` and movement keys keep their usual meanings. If the alert appears while you are typing a search, the draft is discarded and the committed query stays.

After recovery the alert stays as a dim notice until you press `x`, so a failure that came and went is still visible. A new failure raises it again.

```
 ⚠ last alert 8s ago: snapshot failed: store not found  ·  x dismiss
```

If the alert persists, see [troubleshooting](../guide/troubleshooting.md#the-sidebar-degraded-banner).

### The help overlay

`?` opens the overlay over the bottom right of the cards. Any key the sidebar reads closes it and is consumed, and so does focus leaving the sidebar. While typing a search, `?` enters a character instead. The overlay lists your configured movement keys, so it differs from this frame after a rebind.

```
╭ help ─────────────────────────────────╮
│ keys                                  │
│   /   search          esc   clear     │
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
| `j` / `k`, `↓` / `↑` | select the next or previous row, without changing focus; with no selection, select the first visible row |
| `J` / `K` | select the first row of the next or previous worktree |
| `g` / `G` | select the first or last row |
| `Ctrl+f` / `Ctrl+b`, `PageDown` / `PageUp` | move a page; under tmux's default prefix `Ctrl+b` never arrives, so use `PageUp` |
| `H` / `L` | select the first or last row on screen |
| `Enter`, `l` | focus the selected row's pane |
| `1` to `9` | focus the pane of the Nth row, counted from the top of the list |
| `n`, `Space` | jump to the next card that needs you: unread cards oldest first, then read `?` and `!` cards oldest first; attention lifted by a waiting launched child targets its live pane, first in entry order, otherwise the parent's pane |
| `N` | the same walk, backward |
| `m` | mark the selected card read or unread, without jumping; with no selection, do nothing |
| `M` | mark every card read |
| `←` / `→` | switch the dashboard tab |
| `a` / `d` | make the sidebar one step narrower or wider in every tab of the room; the width holds through terminal resizes, reloads, and reattaches until the session ends ([width rules](../guide/sidebar.md#setting-the-width)) |
| `r` | reload this tab's sidebar |
| `x` | dismiss a recovered alert |
| `?` | open the help overlay |
| `/` | start or edit a text search |
| `Esc` | clear only the text search; while help is open, close help only |

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
| `A` | all; clear both the status/unread/PR pick and the text search |

Pressing the active filter's key again clears just that pick; a text search stays active. Movement keys and `a` / `d` can be rebound under `[sidebar.keys]`, and outside search typing a rebound chord takes priority over a fixed key, so it can shadow a filter. The other keys are fixed.

### Text search

The search has its own pinned line between the cards and the dashboard. It replaces the dashboard's blank separator, so starting a search does not move the cards. Without a dashboard it takes one additional row above the fleet store or other bottom chrome. The committed query stays visible even during a health alert, explaining why the stale cards remain narrowed. The line does not respond to clicks.

Typing (band across the row, cursor after the query):

```
 / auth                        3/16
 ──────────────────────────────────
```

While typing, the whole line carries the selected-card background band, including its gutter and right edge; a reverse-video cursor follows `auth`. After `Enter`, `/ auth` becomes one filled chip and the band disappears. Without color, the reversed cursor distinguishes typing from the reversed committed chip. The leading search glyph is `/` in the Unicode set and a magnifying glass in the Nerd Font set.

Committed (chip around `/ auth`, count outside it):

```
 / auth                        3/16
 ──────────────────────────────────
```

The right-edge count is matching visible rows over all rows, processes included. A status pick narrows only the numerator; a matching group label counts every row it keeps. An empty draft has a cursor but no count. On narrow panes the count disappears before the query clips, and clipping preserves the cursor.

`/` starts typing, seeded with the committed query. Matching is case-insensitive substring over worktree/group names and qualifiers, branches, agent names and handles, teams, and PR numbers (`#123` or `123`). A matching group name, team, or PR keeps all its rows; otherwise only matching rows remain. Search composes with the status, unread, or open-PR pick, hides empty groups, and lifts the fold cap. The cockpit counts still describe the whole fleet.

Every printable key pressed without `Ctrl` or `Alt` enters the draft, including `n`, `j`, `q`, `r`, `?`, `A`, and digits. `Backspace` removes one character; on an empty draft it cancels. `↑` / `↓` and `Ctrl+p` / `Ctrl+n` move among matches; every other `Ctrl` or `Alt` chord does nothing, whether or not `[sidebar.keys]` binds it. Every edit selects the first match again. `Enter` commits the query and focuses the selected match, or ends typing without focus when the draft is empty. A zero-match query stays set and shows `no match` in the body.

`Esc` clears the text search, whether typing or committed, leaving any status, unread, or open-PR pick intact. It also works during a health alert; while help is open it only closes help. Any click that focuses a pane while typing also drops the search, including a subagents and waits line or `+K older`. Other clicks commit the draft and then act normally; the wheel scrolls without ending typing. Focus leaving the sidebar, or the sidebar degrading, discards only the draft. The committed query follows you across tabs and survives a sidebar reload; the draft is local to the sidebar where you type and does not survive a reload. After `Enter`, clicks keep the query, ordinary movement stays inside the matches, and `n` / `N` keep their needs-you meaning. Press `A` to clear both narrowings.

### Mouse

| mouse | action |
|-------|--------|
| click a card or process row | select the card immediately and focus its pane |
| click a pipeline line | focus the visible stage owner, else the first actionable team member, else the group's first visible row |
| click a make-up bucket or `⑃ N` | filter the cards |
| click `↑ N need you` / `↓ N need you` | reveal the lead card where it sits, or scroll to the top if the filter hides it |
| click `↑ <name>` / `↓ <name>` | reveal the focused card without changing selection |
| click a subagents and waits line | toggle its entries, select the card and focus its own live pane |
| click either line of a subagent entry | select the parent card and focus the child's live pane, else the parent's pane |
| click `+K older` | append older entries, select the card and focus the parent's live pane |
| click `+K more`, `− less` | expand or cap the group without moving focus |
| click a folded finished group's header or either receipt line | show its cards and focus the first receipt member's live pane, revealing its card |
| click an expanded finished group's header | collapse its cards without moving focus |
| click `[ ▸ N landed ]` / `[ ▾ N landed ]` | show or hide landed receipts, also in unviewed sidebars; opening scrolls to the first receipt it reveals |
| click a dashboard tab | switch provider |
| wheel | scroll the cards without moving the selection |
| drag the pane border | set the sidebar width for the room |

For 400 ms after cards move without your input, a click targets the card in the preceding frame, or does nothing if it has left. Scrolling, browsing, and other input paints clear that window.

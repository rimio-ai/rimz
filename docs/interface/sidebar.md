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
▌⢄ claude · Opus 4.8 · high · 200k                $1.27▐    ← status, handle, model, effort, window, cost
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
  W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88     ← last 7 local days, all providers
  M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66     ← last 30 local days

 ⇄ 210ms                                     ? for help     ← footer
```

The cockpit counts this room only: its project root and the worktrees grouped under it. The provider dashboard and the fleet store count every session on the account, in any project.

## Reading the glyphs

One glyph vocabulary runs through the whole sidebar. The tables group it by where a glyph appears and show the default Unicode set. `[theme.glyphs]` can switch to a Nerd Font set or override single glyphs without changing a meaning ([theming: glyphs](../guide/theme.md#glyphs)).

### Status glyphs

A status glyph leads every agent card and labels every bucket of the cockpit's make-up line.

A running or waiting card with async questions adds a yellow `?` line below its description: the oldest question, clipped to the card width, followed by `+N` for additional questions. This reuses the waiting glyph and color without changing the card's status glyph, attention ordering, or notifications. Answer it [in the Codex pane](../reference/cli/asks.md#questions-while-codex-works) with Shift+Left; `rimz answer` refuses async questions.

| glyph | status | meaning | needs you |
|-------|--------|---------|-----------|
| `?` | waiting | asked you something: a permission, a plan approval, a question, or a raised question over a resting card | yes |
| `!` | failed | a manually raised alert, or the turn errored, died on a provider API error, went silent past the stall window (also a launched child whose first turn never started), or repeated one tool call 20 times | yes |
| `⏸︎` | paused | stopped mid-turn on a provider rate limit, overload, or dropped connection, or one of the children it launched is; the description quotes the reason in italics while the provider recovers; a pause that can no longer resume becomes `!` | after recovery: prompt the parked agent to continue, or let [auto-continue](../guide/loops.md#auto-continue) do it (off by default) |
| `✓` | done | the turn finished cleanly | a look |
| `⢿` | working | running a turn, waiting on in-flight work, or running a background shell; active turns animate through braille frames (`⣾`, `⣽`, ...), while a resting turn uses the waiting head below | no |
| `☾` | sleeping | resting on a timer, signal, condition, or file watch, including through a child | no |
| `○` | idle | alive, nothing in flight | no |

`?` is always yellow, `!` always red, and `⏸︎` always blue, whatever the card's age. What each status means in an agent's life is [the agent lifecycle](../guide/sidebar.md#the-agent-lifecycle). The mux tab bar reuses `!`, `?`, `⏸︎`, `⢿`, and `✓` as a static suffix on the tab name ([the sidebar guide](../guide/sidebar.md#glance-jump-answer)). A suffix always draws in these compact Unicode shapes, even when the column runs a configured [glyph set](../guide/theme.md#glyphs). A tab RimZ named after its agents goes back to the shell's name once they all exit; a tab you named yourself keeps its name.

An auto-launched Codex permission stays Running during its first 60 seconds of automatic review. A rollout showing that the asking Code Mode cell finished closes the ask and keeps the card Running. Otherwise, if still open, it becomes `?` on the next refresh, with a fresh unread episode at the 60-second visibility instant rather than the hook time. Questions and plan approvals remain immediate ([Codex asks](../internals/agents/adapter_codex.md#asks-and-answers)).

On a process row, `!` means the process is stuck ([process rows](#process-rows)). On a [not-tracked row](#not-tracked-rows), `○` means RimZ reads nothing from that agent, not that the agent is idle.

A running agent can show a head in place of `⢿`. Whichever head it shows, it counts as working in the cockpit.

| glyph | head | meaning |
|-------|------|---------|
| `⠁` | thinking | the turn has not edited a file yet; a research turn that never edits stays here to the end |
| `▇` | compacting | condensing its context window |
| `⢄` | waiting on handed-off work | delegated to running children; their entries are listed under the card; also a turn that ended waiting on a command, check, PID, team, or peer agent, or a background shell; a launched child's provider park replaces this head with `⏸︎`, or `!` once it can no longer resume |

Each head's frames, color, effect, and speed are configurable under [`[theme.animations]`](../guide/theme.md#animations). On a card, each head takes the card's emphasis like the status glyph it replaces.

### Card glyphs

| mark | meaning |
|------|---------|
| `▣ ━━━╺━───┤ 75.0%` | context meter: progress toward native compaction when known, otherwise the share of the session's usable window in use with an open track; `▢` while nothing is in use |
| `┤` and `372k/1M` | native compaction point: the wall and `372k` share dim violet, while the model capacity `/1M` recedes (Codex pairs its point with catalog capacity, such as `372k/872k`; OpenCode predicts its default point, such as `835k/1M`); the wall stays on idle and narrow cards even when the label drops |
| `↻ ─────────` / `↻ ━──────── 5.0%` (Nerd Font: ``) | compacted context awaiting its first real reading: a green glyph on an empty muted track without a size estimate, otherwise a flat estimate fill and percentage; a positive estimate below 0.1% reads `<0.1%` with a sliver; no stats line in either case |
| `▤ 76k` | tokens in the context window now |
| `◌` `◍` `↘` `↗` | cache-read, cache-write, fresh input, and output tokens. The cockpit, dashboard, and fleet store have no `◍` column, so their `↘` includes cache writes. |
| `◇` | total tokens: `↘` plus `↗`, with `◌` counted beside it and not in it |
| `97%` | session cache hit: green from 90%, yellow from 70%, red below |
| `↻ 2` | completed context compactions |
| `⟲ 5` | consecutive identical tool calls, shown from 3 through 19 |
| `◔ 8m` | an age, never a duration. The face rounds down to completed quarters (eighths with Nerd Font), with a minimum first face. Card and wait ages fill over an hour and stay full past it, never ringed. Child quiet fills over `stalled_after_secs` and keeps `◉` past that window; `◉` also leads the cache-expired line, not a card age. |
| `9m`, `2h` | plain muted runtime, frozen for finished children, and the inactive receipt's age, which carries no face; pipeline durations use two units from an hour up, such as `4h12m`. |
| `200k`, `1M` | the model's published context size, falling back to its usable window |
| `ϟ` (Nerd Font: ``) | fast mode, after the model on a card, after the effort on a subagent row (after the model when the row has no effort): bold amber, always still on child rows; on cards, still at rest and crackling only while running |
| `↪` beside the model | tier routing passed over an earlier entry; `rimz agents show @<handle>` names the tier and every reason. The default mark is the same in both glyph sets (`card.tier_move`). |
| `$1.27` | cost in dollars, two decimals |
| `⋯ bg` after the description | the turn finished while background work it started is still running |
| `↧ N` (Nerd Font: `󱃚 N`) | N open messages are due for this agent, from any sender; delivery can still be waiting on another gate |
| `✎` (Nerd Font: `󰏫`) | recent counted input would hold a message sent to this pane now |
| `C 34%` `M 512M` `⇅ 8M/s` | a working process row's CPU, resident memory, and I/O rate |
| `· not tracked` | a recognized agent pane whose reporting is known to be unavailable |
| `tip:` | how to enable reporting; the label drops below 32 pane columns |
| `…` on a not-tracked row | the label or fix has been shortened to fit |

Each token marker keeps one color everywhere: `◇` blue, `↘` deep red, `↗` cyan, `◌` green, `◍` violet.

### Worktree header glyphs

| mark | meaning |
|------|---------|
| `⑂ name` | a checkout group named by its branch, or a channel named by a listed worktree's marker |
| `↩ name` | a worktree group whose work has landed |
| `# name` | a channel with no listed marker-backed checkout |
| `✓` `✕` `◌` beside the name | the trunk's HEAD-commit CI, or a branch's open or merged pull request's CI: passing, failing, running |
| `#91` | the branch's pull request |
| `#522→#530→#540,#541` | a same-repository PR stack, bottom first; comma-separated branches end the stack |
| `→` (`worktree.pr_stack`) | the next level of a PR stack |
| `⇡3 ⇣1` | commits ahead of and behind the trunk |
| `+127 -43` | lines added and removed against the trunk |
| `⟳` `✓` `✕` `⇉` `↯` `⑃` `≡` `⑂` before the trunk name | where the work stands; see [worktree headers](#worktree-headers) |
| `▸` | a collapsed inactive group |
| `▸` / `▾` (`worktree.inactive_closed` / `worktree.inactive_open`) | the inactive chip, closed / open; both glyph sets use these shapes |
| `+3 more` / `− less` | hidden idle rows; click to expand or collapse |

### Pipeline glyphs

Each declared stage gets one dot; `Done` has no separate slot. These roles live under `[theme.glyphs.<set>.pipeline]` independently of the clock and status glyphs.

| role | Unicode | Nerd Font | meaning |
|------|---------|-----------|---------|
| `passed` | `●` | `●` | before the current stage |
| `current` | `◉` | `◉` | current stage |
| `future` | `○` | `○` | after the current stage; in the `warn` tone when the run already reached it |
| `done` | `●` | `󰗠` (U+F05E0) | replaces the last dot at `Done` |
| `check_passed` | `◆` | `◆` | a reached check on a workflow rail, not used by team stages |
| `check_future` | `◇` | `◇` | an unreached check on a workflow rail, not used by team stages |

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
| `╎` | a tighter longer window's allowance, measured on this bar's scale |
| `5h` `7d` `mo` | an included budget window, labeled by its length (`mo` covers 28 to 31 days) |
| `cr`, `bld`, `dep` | a quota the provider or a plugin names |
| `$` | the organisation spend limit of a Claude apps gateway |
| `ex` | paid extra usage |
| `api` | an API-key or Claude Console budget |
| `↻ 1h47m` | time until the window resets |
| `↻ 2` in a block header | Codex reset credits with auto-redeem off, or Claude limit resets: redeeming is yours to do ([`rimz accounts redeem`](../reference/cli/accounts.md#redeem) for either provider) |
| `⟳ 2` in a block header | auto-redeem armed: a credit would be spent if the longest window ran dry and parked an agent now |
| `‖ 2` in a block header | auto-redeem holding: a dry window would park the fleet until its natural reset |
| `· 3h` after the credit count | time until the nearest credit expires, within a week |
| `∞` | no limit |
| `–` | the provider reports no figure for this slot |
| `⇅ rc` | remote control is on for this provider: green when its server is up, red when a configured server is down |
| `W:` `M:` | fleet totals for the last 7 and 30 local calendar days, including today |

### Cockpit and chrome glyphs

| mark | meaning |
|------|---------|
| `⌘ name` | the workspace |
| `◎ N` | sessions that ran in the spend window |
| `¤ N` | agents alive right now |
| `⑃ N` | open pull requests on the room's worktree groups, including groups kept by live workflows and stacked pull requests |
| `↑ 2 need you` / `↓ 2 need you` | the card that most needs you is above / below the view |
| `▌` ... `▐` | the selected card's left and right spines |
| `▏` inside an entry's indent | the selected child entry or `+K older`; held when the sidebar takes focus |
| muted subagent headline | no live child pane: a click goes to the parent; error labels keep their italic emphasis |
| `▎` ... `🮇` | the selection lane: every row of the group that holds the selection |
| `┄` | the dotted seal on the selected group's header |
| `┄ external ┄` | panes with no channel or known containing place |
| `─` | a section rule |
| `▐` / `▕` on the right edge | scrollbar thumb and track, drawn over the right spines while the cards scroll |
| `┤ Tab ├` | the active dashboard tab under `NO_COLOR` |
| `zᶻ idle`, `zᶻ away` | you are away from the terminal |
| `⇄ 210ms` | SSH link round-trip time |
| `⧗ stall mem 34%` | machine resource pressure: time tasks spent stalled, not utilisation |
| `mem 6%↓` | a fading stall; its ten-second average has eased while the minute average remains high |
| `/ auth` | the text search, above the dashboard |
| `⚠` | a notice or alert |

`🮇` is from Unicode's Symbols for Legacy Computing block. A font without it may show a placeholder at the lane's right edge; nothing else depends on it.

## The cockpit

The cockpit is the top block. Its lines stay put as agents change status; the unread banner adds or removes a row. The make-up line is absent in an empty or process-only room.

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
| 4 | the make-up line: units by status. |
| 5 | the `↑ N need you` or `↓ N need you` banner, only while it applies. |

Before any spend arrives the token breakdown shows zeroes and the spend shows `$0.00`. When an agent's cost moves, the spend counts up to the new figure within 1.2 seconds. Spend that grew while you were on another tab shows its current figure when you arrive, without counting up. It never decreases inside one window and resets when the window rolls. Antigravity reports current usage that cannot be summed over time, so its cost shows on its card and stays out of this total. How every figure is computed is [Token Insight](../guide/insight.md).

The spend scope uses the project root, listed worktree roots, and the configured worktree home, so cleaned-up checkouts still count. Grouping instead uses the deepest known place and follows no symlinks: a shell whose cwd resolves outside those places goes to `external`.

The `⑃ N` colour reads each worktree's own open pull request, since stacked pull requests without their own group carry no facts. The first matching rule wins: red when any CI fails or the merge queue removed a pull request, amber-orange when any conflicts with its base at its current head, yellow while any CI runs, green when every open pull request's CI passes, and blue otherwise.

With `NO_COLOR` the count has no hue, but a conflict still has its own header glyph. On a 16-colour terminal, amber-orange may share yellow's colour; the header glyph still distinguishes a conflict from running CI.

### The make-up line

The make-up line counts units: a team's members within one worktree group, or one solo agent. Process rows, including not-tracked rows, are no unit. The left cluster is `?` waiting, `!` failed, `⏸︎` paused, `✓` done; the right is `⢿` working, `☾` sleeping, `○` idle. Every bucket shows even at zero, including a room holding only landed results.

Any member can add `?`, `!`, or `⏸︎`. A staged board at `Done` gives `✓`, even without an owner. Otherwise a visible owner's displayed status decides `⢿`, `☾`, or `○` (an owner showing `✓` also gives `○` until the board reaches `Done`). Other members add attention only. Without a usable pipeline and owner, the team's calm bucket is working first, otherwise sleeping, idle, or done. A solo unit follows its row. The calm bucket counts beside attention only when it is `⢿`. Only `✓` excludes inactive groups. For example, a working team with idle teammates is `⢿ 1`, `○ 0`; a team with a waiting member and working owner is `? 1`, `⢿ 1`. The same team name in two worktree groups counts twice; cards in `external` count solo. `¤` still counts agents, so the buckets need not sum to it.

Picking a status shows every member of every unit that counts in that bucket. A filter chooses only which cards appear: shown cards keep their provider colour and selection opens only the selected card, exactly as without a filter. Selection keys (`j`/`k`, `g`/`G`, pages, `H`/`L`, `[`/`]`) reach every shown card; clicks, digits, `Enter`, and the `n`/`N`/`Space` inbox walk keep their usual behaviour within the shown roster. Changing the filter keeps a still-shown selection, without starting a browse pick; typing a search highlights its first visible row. Unread and open-PR lenses keep their per-card and per-group membership, respectively.

Click a non-zero bucket or the `⑃` count to filter the cards to it; press `u` to filter to unread cards. The filter applies in every tab of the room and stays on while you move between matching cards. The active filter is drawn as a filled chip, or in reverse video under `NO_COLOR`. To clear it, click it again, press its key again, press `A`, or pick another filter. It also clears when its count reaches zero. Work kept unfolded by a layout hold still counts; a pick supported only by that work clears when the hold ends. The filter keys are in [Keys and mouse](#keys-and-mouse).

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

Selecting a card already on screen, even partially, scrolls nothing. Selecting an off-screen card with this sidebar's keys or mouse brings it into view with the least motion; a card taller than the view lands by its first line. Native tab switches, outside focus commands, and the initial focus on attach move the highlight without scrolling. A click into another tab places the card on the same row within that tab's card view. A toggle (`+K more`, a delegation header, or `+K older`) preserves the viewport instead of pulling it toward the selection. Opening the inactive chip scrolls only far enough to show the first receipt it reveals; closing brings the chip back into view and clamps the offset to the remaining content. Unfolding an inactive group also brings the member it focuses into view with the least motion.

A mouse wheel tick glides the cards to their new position over about a tenth of a second without moving the selection; under load the glide drops frames rather than slowing down. The view then stays anchored to what it shows, without the unchanged selection pulling it back. Only the wheel glides: selection reveals, the unread-banner click, jumps, and corrections for changes above land in one frame. Those corrections alone do not wake the scrollbar.

When a native switch or outside focus command leaves the focused card off-screen, a named edge marker appears after 500 ms on tmux or 300 ms on Zellij: `↓ <name>` below the view, or `↑ <name>` above it. It replaces an edge line without moving the cards. The unread banner takes priority over the up-marker; the down-marker can show alongside it. Click the marker to reveal the card with the least scrolling, without changing the selection. It disappears when the card comes into view or you select another card with the sidebar's own input.

Here the focused card sits below the view after a tab switch:

```
 ⑂ alpha
 ⣾ codex
   task-0
   ▢ ──────────────────────────  0.0%
 ⣾ codex
   task-1
   ▢ ──────────────────────────  0.0%
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
| identity | The status glyph, then the agent's handle in its provider's brand color: its team role, explicit name, or profile, else the agent kind. Then, dimmer, the model, the agent's reasoning setting as its provider names it (an effort level such as `high` or `xhigh`, or `thinking`), and the context size: `372k/1M` for Claude, `372k/872k` for Codex, or `835k/1M` for OpenCode when compaction-scaled, otherwise the published capacity alone (falling back to the usable window). Codex shows the context size and compaction point only once it reports a window, when its first turn starts. The point shares the wall's dim violet; `/capacity` is muted. Codex's capacity is its catalog maximum, falling back to its session's raw window. From L1, when it fits without shortening the handle, a fast model adds a bold amber bolt immediately after its name, before reasoning or context size: still at rest, crackling only while running. A provider's live fast-mode reading wins; a fast launch supplies the badge only while the live reading is unknown. The session's cost sits on the right once it rounds to at least `$0.01`, and counts up like the cockpit spend. |
| description | The open ask while waiting for input, when available: the first line of its first structured question, then ` +N` for N more questions in that ask, wrapped over at most two lines. Without structured questions, the ask's summary supplies the text. Otherwise the displayed error or pause reason in italics, else the first working wait's label while working through a wait or first sleeping wait's label while sleeping, else what the session is on: the first of the session name, the provider's thread preview, the launch description, the agent's task, the session's first prompt, and its latest prompt. Codex's automatic title and Claude's `--name` or `/rename` count as the session name; a name that only repeats the start of the prompt is skipped. Optional `✎ ↧ N` message marks pin right on its last line, after any ` +N`; the text clips first, without adding a line. |
| context meter | Progress toward the provider-capped native compaction point when known, including Codex's own point when RimZ sets none and OpenCode's predicted default point; otherwise the percent of the session's usable window in use. The label shows one decimal, except for `<0.1%` below a tenth and `100%` at the ceiling. A compaction-scaled track ends in `┤`, reserved inside the existing bar width. Fill and percentage stop at 100%, reaching but never covering the wall. The fill shows where the context went: a run of cache reads first, then a segment each for cache writes and fresh input, each starting with `╺`. The bar runs from green toward red as the selected scale fills, at the stops set in [`[theme.display.context_meter]`](../guide/theme.md#display). Once the prompt cache expires, the fill becomes one solid greyed-red run with no segments or caps; the measured fraction and percent stay the same, and `▣` wears the same expired-cache tone. A no-cache card never takes the expired-cache tone. After a compaction completes, `↻` stays until the first real context reading from a model call. A provider-reported compacted size fills the track as a flat estimate with a percentage and the glyph in the fill's tone; a positive estimate below 0.1% reads `<0.1%` with a sliver, while a zero estimate reads `0.0%` with no fill. Without an estimate, the glyph is green and the empty track stays muted, with no percentage. The wall stays through both states. An agent that reports no context usage keeps this state after its first compaction. For measured usage, `▢` means exactly empty, while any positive fraction uses `▣`. An engaged card without a usage reading keeps `▢` and `0.0%`; a fresh, never-prompted card has no meter. |
| stats line | On a selected or held engaged card, or under `expanded` density: `▤` tokens in the window, the latest API call's cache-read, cache-write, and fresh input added together, which is the amount the meter's percent measures. Then the same split in numbers, the session cache hit, `↻ N` compactions, and `⟲ N` repeated tool calls. Hidden entirely between a compaction and its first real reading, even when the meter shows an estimate; the reading restores it, including `↻ N`. From 46 columns the age is on the meter; below that it pins right on this line, or stays on the meter while the line is hidden if the card has a cache. |
| cache line | On a selected or held engaged card, or under `expanded` density, below the meter and any stats: a countdown once the cache warning band starts, then the expired-cache notice and available rebuild estimate. Hidden on no-cache cards. |
| subagents and waits | Present once the session has spawned a subagent or armed a wait, except on closed, unreached idle and done cards. See [Subagents and waits](#subagents-and-waits). |

With native compaction configured at 372k on a 1M model, this [58-column frame](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__compaction_unicode_color_measured.snap) measures 279k as 75.0% toward the wall. The stats line still counts the occupied tokens:

```
▌⣾ claude · Opus · high · 372k/1M                        ▐
▌  index docs                                            ▐
▌  ▣ ━━━━━━━━━━━━━━━━━━━━━╺━━━━━━╺━━────────┤ 75.0%      ▐
▌  ▤ 279k · ◌ 200k ◍ 60k ↘ 19k                           ▐
```

The [expired-cache](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__compaction_unicode_color_expired.snap), post-compaction [estimate](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__compaction_unicode_color_estimate.snap), and [unknown-size](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__compaction_unicode_color_empty.snap) frames keep that wall, as do idle cards. Window-scaled cards keep the open-ended track shown above.

Codex pairs its effective point with the model capacity: [372k/872k](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__codex_compaction_capacity.snap), [244k/872k on a smaller session window](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__codex_compaction_lowered.snap), or [300k/872k from its own setting when RimZ sets none](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__codex_compaction_own.snap):

```
▌⣾ codex · GPT 6.1 Sol · high · 244k/872k                ▐
▌  index docs                                            ▐
▌  ▣ ━━━━━━━━━━━━━━━━───────────────────────┤ 40.8%      ▐
▌  ▤ 100k                                                ▐
```

The compaction label yields in whole tokens after the other identity fields. These [53-column](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__compaction_label_unicode_53.snap), [50-column](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__compaction_label_unicode_50.snap), and [49-column](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__compaction_label_unicode_49.snap) frames keep the handle, model, bolt, tier mark, effort and cost fixed:

```
▌⣾ claude-docsmith · Opus ϟ ↪ · high · 372k/1M $1.20▐
▌⣾ claude-docsmith · Opus ϟ ↪ · high · 372k $1.20▐
▌⣾ claude-docsmith · Opus ϟ ↪ · high       $1.20▐
```

After compaction, without a size estimate (58 columns, window-scaled):

```
▌○ claude · Opus 4.8 · high                         $1.27▐
▌  store refactor                                        ▐
▌  ↻ ────────────────────────────────────────            ▐
```

With a provider-reported estimate, the same mark keeps the flat fill and percentage until the first real reading; both variants hide the stats line:

```
▌✓ codex                                                 ▐
▌  compact the context                                   ▐
▌  ↻ ━━╸────────────────────────────────────┤  5.5%      ▐
```

A resting fast-mode card at L2 (54 columns):

```
 ⌘ query-engine

 ◎ 0                                  ◇ 0 ↘ 0 ↗ 0 ◌ 0
 ¤ 1                                            $0.00
 ────────────────────────────────────────────────────
 ? 0   ! 0   ⏸︎ 0   ✓ 0                ⢿ 0   ☾ 0   ○ 1

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇
▌○ codex · GPT 6.1 Sol ϟ · xhigh                $1.00▐
▌  Ship the badge                                    ▐
 ────────────────────────────────────────────────────
  W: ◎ 0  ◇ 0 ↘ 0 ↗ 0 ◌ 0                       $0.00
  M: ◎ 0  ◇ 0 ↘ 0 ↗ 0 ◌ 0                       $0.00

                                           ? for help
```

A waiting description starts from the first line of the first structured question, not the form title; without questions it keeps the ask's summary. It selects the first sentence ending in `?`, or the whole summary when none does. Sentences end at `.`, `!`, or `?` followed by whitespace or the end of the text; a colon stays with its question. The selected text word-wraps at the description's two-cell indent. Short asks stay one line; text beyond two lines, or a word wider than a line, leaves `…` at the end of the last shown line. When the ask holds more questions, ` +N` follows the text, after any `…`, with its width reserved so it is never cut. N counts the other questions in this same ask, including questions beyond an adapter's stored-question cap. Optional message marks follow that count on the last line, clipping text rather than adding another line. The card does not show options. The separate queued-question line keeps its one-line form and its `+N` counts other queued asks instead.

The [short](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__waiting_question_count_short.snap), [wrapped](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__waiting_question_count_truncated.snap), and [16-column](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__waiting_question_count_narrow.snap) frames show ` +2` intact, including after `…`.

A card has no prompt cache when its meter draws `▢`, including before any usage reading, or between compaction and the next prompt. It shows no age pin or cache line and never wears the expired-cache tone. A keep-warm hold does not restore its pin. Its stale one-line age keeps counting, muted.

Before expiry the card reads `◷ cache expires in 8m`, in the clock's yellow-to-orange-to-red tone; minutes round up, with `in 1m` at the lifetime boundary. After expiry it reads `◉ cache expired · ◍ ~$1.40 to re-warm`: the notice uses the muted expired-cache tone, the seam stays muted, and the estimate uses the cache-write tone. Below 46 columns these become `◷ expires in 8m` and `◉ expired · ◍ ~$1.40`. The estimate rounds to two significant figures, keeping cents below $10 and whole dollars above; an unknown price or a figure below half a cent leaves only the expired notice. There is no cache line while calm, held warm, without a known lifetime, or on a no-cache card. Like stats, it stays on previously selected held cards and shows on every engaged card under `expanded` density, including teammates.

On a narrow sidebar the identity line drops the reasoning token below 46 columns, then the model and context label below 32, and keeps the handle. A tier move adds `↪` beside the model at 32 columns and wider; below 32 columns it drops with the model. Within either capability tier, the compaction label yields space after the handle, model, bolt, tier mark, reasoning and pinned cost: `372k/1M`, then `372k`, then nothing, never a partial token. The context label shows on non-idle cards only; the compaction wall stays even on idle and L0 cards. At 46 columns or wider, every context meter reserves a fixed five-cell age cluster after its percent, blank on no-cache cards or until five minutes quiet. Below 46 columns the meter keeps its full bar and the age moves to the right of the selected or held card's stats line. While stats are hidden after compaction, only a card with a cache keeps the meter's age slot at every width. Selected, resting, and compact meters all use the same slot, keeping their bars and percentages aligned.

This 46-column [rendered frame](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__tier_move_card.snap) shows a tier move beside the model:

```text
 ⌘ query-engine

 ◎ 0                          ◇ 0 ↘ 0 ↗ 0 ◌ 0
 ¤ 1                                    $0.00
 ────────────────────────────────────────────
 ? 0   ! 0   ⏸︎ 0   ✓ 0        ⢿ 0   ☾ 0   ○ 1

▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇
▌○ codex · astra ↪                           ▐
▌  route the work                            ▐
 ────────────────────────────────────────────
  W: ◎ 0  ◇ 0 ↘ 0 ↗ 0 ◌ 0               $0.00
  M: ◎ 0  ◇ 0 ↘ 0 ↗ 0 ◌ 0               $0.00

                                   ? for help
```

On the selected or held card's stats line (every engaged card under `expanded` density), a column that is zero or unreported is left out. A provider that reports only session totals, as stock Droid does, shows `◇ total ↘ input ↗ output ◌ cache-read` on this line instead, and its meter stays empty. The cache hit is cached input divided by all input, and it is absent until the session has input counters.

The meter draws denominators up to 256k tokens linearly and larger ones on a log curve that reaches full strength at 1M, using the compaction point when scaled to it and the model window otherwise. The percent is the unwarped share on that scale, capped at 100%. On cards with a cache, the age face fills over the first hour of the age its label shows, then stays full (`●`, or `circle_slice_8` with Nerd Font): full means an hour or more, not cache expiry or the end of a hold. Tone and expiry follow the agent's resolved prompt-cache lifetime: its newest split cache write, then sign-in or plain settings, built-in defaults (Claude 1h; Codex subscription 12h, API key or unknown 30m), then the 1h fallback. [`[harness.prompt_cache_ttl]`](../guide/configuration.md#idle-compaction) configures the static chain. Its warning band and cache countdown start after half that lifetime, or in the last ten minutes if earlier (the whole lifetime when shorter than ten minutes), and never more than an hour before expiry, so the countdown reads at most `60m`. The clock stays muted before the band, then slides from yellow through orange to red at expiry. Past the lifetime it keeps its hour-scale face and, with the meter's fill and lead glyph, takes the greyed-red expired-cache tone: no action can save a cache that has already expired. The separate cache line keeps `◉ cache expired`. A card with tokens but no resolved lifetime because of `off` in that chain keeps its muted age pin and never expires the meter. For a card with a cache, while a [keep-warm horizon](../guide/configuration.md#keep-warm) is actively holding the cache, the clock stays steady green on the same hour scale; once the horizon passes or its stage condition stops holding, the usual ramp resumes from the last request. Wait ages also fill over an hour and stay full past it, never ringed. The card's breathing keeps the hour scale; child quiet uses the configured stall window and keeps its ring past that window, and durations never heat. Held or not, the age clock counts from the later of the agent's own activity and its last cache request, pings included, so a ping restarts the number and ramp without reading as work. Child activity never refreshes that age, so a parent waiting on its subagents keeps counting while they work: its own session is making no call, and its cache ages the whole wait. The children's own times ride their entries under the card. The context meter's bands, log scale, and tones are set under [`[theme.display]`](../guide/theme.md#display).

From a minute up, coarse ages share one scale: whole minutes (`12m`), hours (`3h`), then days (`2d`), floored and capped at `99d`. Each fits in three cells. Child runtimes keep `<1m` below a minute; card age pins stay hidden below five minutes.

### Card shapes

A card's slots are set by its stage, age, density, and selection reach. Data fills those slots in place; a waiting description can take a second line.

| card | lines |
|------|-------|
| stale: read idle or done, quiet past `stale_after_secs` | one line: state, handle, description, optional message marks, age in a fixed column, and cost pinned right; selection, a held expansion, or a subagents and waits section clicked open restores the card |
| fresh: idle, never prompted, including selected and held cards | identity, plus the launch description except under `compact` density |
| engaged, resting: after its first prompt | identity, description, meter, with `▢ 0.0%` until data arrives |
| engaged, with subagents or waits | adds the subagents and waits line, except on unreached idle and done cards with their entries closed |
| selected or held, engaged | adds the stats line (`▤ 0` until data arrives) below the meter and lists the subagent and wait entries below the card; only the live selection lights the spines |

Selecting an engaged card inserts its stats line below the meter and appends entries; the lines above the meter stay in place. Selection opens only that card; it does not change teammates' shapes. Previous engaged selections keep their own stats and entries until the layout freeze expires, without opening their teammates. Fresh cards stay at identity plus any launch description even when selected or held, with no meter or compose dots; Compact keeps identity alone. The first prompt, turn, history, or context usage makes the card engaged. Wheel ticks, unread-banner jumps, and toggles keep previous selections open; explicit section toggles still apply immediately. Only the live selection wears the selected spines. `card_density = "expanded"` keeps stats on every engaged card and lists entries everywhere, teammates included.

`[theme.display] card_density` changes how much a resting card shows:

| value | resting cards |
|-------|---------------|
| `auto` (default) | the shapes above |
| `expanded` | every engaged card keeps its stats line and lists its subagent and wait entries |
| `compact` | idle: identity. Running and waiting: identity, description, meter. Paused, done, sleeping, failed: identity and description. Selecting an engaged card restores its full shape; a fresh card stays at identity. |

`[theme.display] stale_after_secs` defaults to 86400 (24 hours); `0` disables shrinking. Only read idle and done cards shrink, using the card's last activity including its children. Stale takes priority over `compact`; `expanded` never shrinks cards. Every width tier keeps the coarse age and cost on the stale line, clipping the description (and at the narrowest widths the handle) to leave at least two blank cells before the age. The age's right edge sits two cells left of a 6-cell cost slot at the right edge, held even with no cost; a wider cost shifts the age left on that row only. The age uses the same own cache clock and tone as the meter's age pin, without a clock face; on a no-cache card the number stays, always muted. A keepalive refreshes the number without reopening the card; child activity still postpones shrinking but never refreshes the displayed age.

Message marks sit on the description's last line, right-aligned with the context meter's percentage: pen first, then the queued-message glyph and count, separated by one space. No mark reserves space when it does not apply. Description text clips before the marks; on an ask's last line, the text shortens with `…` before its intact ` +N` and the marks. The marks never add a line or change an ask's wrapping height, and never appear on process rows.

On cards without a description slot (compact idle and fresh unlabeled cards, including selected fresh cards), the marks use the same percentage column. If the right-pinned cost or a stale card's age takes that column, they yield left: one blank cell before the cost or two before the age. The cost and age keep their columns. A line too full to hold every mark at that column moves the marks right toward the edge before any glyph drops. Below 32 pane columns the queued count drops but both glyphs remain. On identity-only and stale cards, if even the glyphs cannot fit beside the full handle and cost, they drop rather than shortening the handle. The pen is accent-colored, the queued glyph muted, and the count body text; their shapes remain distinct under `NO_COLOR`.

This [46-column frame](../../crates/rimz/src/sidebar_pane/render/snapshots/rimz__sidebar_pane__render__tests__message_marks_unicode_false_both.snap) shows both marks, with the description yielding first:

```text
▌⣾ coder                                $1.27▐
▌  store refactor with a very lon ✎ ↧ 3      ▐
▌  ▢ ────────────────────────────  0.0%      ▐
▌  ▤ 0                                       ▐
```

The count includes queued and claimed messages whose schedule has arrived, whose conditions have been recorded as met, and which have not expired, across all delivery gates and senders. Already-sent and terminal messages do not count. A condition newly true but not yet recorded waits for the next delivery sweep. The pen uses the configured [`typing_hold`](../reference/cli/message.md#when-a-message-lands) window (30 seconds by default); setting it to `"off"` removes the pen. On Linux, counted terminal input belongs to the pane even after its viewer leaves; on other platforms, recent input follows clients currently viewing it. [Messaging](../guide/messaging.md#park-steer-interrupt-or-schedule) explains the Linux timing gaps.

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

Selecting a card opens the section. Clicking the line opens or closes it and focuses the card's own live pane, even when one of its child panes is focused. The open or closed choice outranks selection and `card_density` while the client view is retained, and follows the client view between tabs. Select an idle or done card first to reach the line unless it is already open, held, or under `expanded` density. In `compact` density, select the card first.

Click either line of a launched child's entry to focus its live pane, across tabs when needed. Entries without a live pane, including provider-native children, still focus the parent; a click never resumes an ended child. A current waiting child counts its parent in the cockpit's `?` bucket without changing the parent's displayed status or lead glyph. When the parent's attention comes from a waiting launched child, `n`/`N`/`Space` focuses the first such child's live pane in entry order; with no such pane it focuses the parent. The parent's own waiting or failed status takes precedence. Focusing a child selects its parent card and puts the entry cursor on that child when its entry is open. The light `▏` in the indent marks that cursor on both entry lines, or on `+K older`; it stays when `Alt+p` focuses the sidebar. The parent keeps its selection band, and moving to the parent card or another card removes the entry mark; with the section closed, only the parent band shows. Focusing a child does not mark the parent read; the parent clears as any card does, by focusing it or dwelling on a tab that shows its pane.

With the sidebar focused, `j`/`k` and the arrows walk each card, then its entries with live panes and the `+K older` line. Waits and pane-less entries are skipped. `Enter`/`l` focuses a child, or opens `+K older` and focuses the parent, returning the cursor to the card. Worktree, end, page, screen-edge, digit, needs-you and search motions remain card-level and clear the entry cursor; `m` still marks the parent.

Each entry starts with its live state or wait icon, then a type word and a ` · ` separator before the headline. A subagent's type word wears its own provider's brand color at the soft weight of a calm card's handle, even when it runs on a different provider from its parent; wait type words keep the body tone. Without a headline, the separator disappears too. A subagent's headline uses its description, else task, else first usable prompt, skipping each when it repeats the type. An ordinary headline without a live pane is muted, matching the entry's click back to the parent. A launched child's displayed error label replaces that headline in body-tone italics, for both paused and failed states. Detail sits on an indented second line; model names stay muted. A launched child in fast mode adds a still, bold amber bolt one space after its effort, or after its model when it has no effort, in every state. Without a model, there is no bolt. The model column reserves no bolt cells, so a non-fast sibling has no blank bolt slot and effort columns remain aligned. Compactions follow the bolt and drop before it as the line narrows. The bolt drops whole if it cannot fit before the clock, without shortening the model or effort. Provider-native children never show a bolt.

| entry | lead | type · headline | right side | second line |
|-------|------|------|------------|-------------|
| running subagent | `⠁` while it reasons, `⢿` while it acts | launch profile or kind · description, else task, else first usable prompt; each skipped if it repeats the type | cost, when known | `▤` tokens in the child's window when the child reports its context, else `◇` tokens over its whole run, then model, effort with its optional fast-mode bolt, and `↻ N` completed compactions when above zero, dropped whole when the line is too narrow; plain muted runtime pins right, replaced from five minutes quiet by a face and quiet age heated toward the configured stall threshold |
| parked subagent | `⏸︎`, or `!` once it can no longer resume | launch profile or kind · limit label in italics, else description, task, or first usable prompt | cost, when known | tokens, model, effort with its optional fast-mode bolt, then `↻ N` completed compactions when above zero, dropped whole when the line is too narrow; plain muted runtime pins right, counting while parked and frozen once failed |
| stalled subagent | `!` | launch profile or kind · `silent Nm` | cost, when known | tokens, model, effort with its optional fast-mode bolt, then `↻ N` completed compactions when above zero, dropped whole when the line is too narrow; quiet age pins right with a face, in alarm at and past the stall threshold |
| finished subagent | `✓` or `!` | launch profile or kind · description, else task, else first usable prompt; each skipped if it repeats the type | cost, when known, after frozen runtime when there is no second line | tokens, model, effort with its optional fast-mode bolt, then `↻ N` completed compactions when above zero, dropped whole when the line is too narrow; plain muted runtime pins right, frozen at the child's finish |
| timer | `◷` | `timer · in 12m`, or `timer · due` once the time passes | time since armed | never |
| PID | working spinner | `pid · 16776` | time since armed | never |
| command | working spinner | `command · cargo` (program name) | time since armed | full command, with the program path trimmed |
| check | working spinner | `check · test` (program name) | time since armed | full command, with the program path trimmed |
| file | working spinner | `file · app.log changes`, or ``file · app.log matches `<pattern>` `` | time since armed | never |
| background shell | working spinner | `shell ·` description, else program name, else just `shell` | time since RimZ first saw it | command when known, with the program path trimmed |
| signal | `⌁` | `signal · pr.merged` (selector) | time since armed | never |
| idle stop | `◷` | `stop · after 3m idle, in 1m`: the requested rest, then the time left or `due` while the agent rests and its clock runs | time since requested | never |

Entries list in a fixed order, and opening more of the list only appends rows:

1. Unfinished subagents, including paused and stalled children, in the order they started. They are never capped.
2. Finished subagents, newest finish first, that finished within `recent_subagent_secs` (900 seconds by default), up to `max_recent_subagents` (5 by default). A child whose turn ended cleanly with background work still running takes that turn end as its finish instant once its entry shows `✓`. Closing a pane or reaping a session does not change that finish instant, except that reaping such a child rests it at idle, back among the unfinished entries.
3. Waits: timers by due time, then watches, then background shells, then signals. A wait leaves the list when it fires or its job ends.
4. `+K older`, which folds every other finished subagent. Click it to append them and focus the parent's live pane. When no subagent is unfinished or recent, opening the section lists the older ones directly.

The listed subagents plus `K` equal the count in the line. `K` can exceed the rows it reveals, because a native subagent that was replaced while still running is counted and has no row. The older list folds again when you close the section, send the parent a new prompt, or run `/clear` or `/compact`. Messages from other agents and RimZ's own deliveries leave it open.

A subagent's `▤` glyph heats from green to red on the worse of window percent and window tokens when the child reports its window size, using the same bands as a card's context line. Without a reported window size, it heats on tokens alone. A running subagent's clock keeps its own quiet-age ramp over the configured stall window. A finished subagent's clock stays muted, and so does a wait's, since a wait is pending by design. A wait has no clock when its arm time is unknown. A subagent without metadata shows a single line; command and check waits show their command on a second line. Subagents never get a card of their own while their parent's card is visible. Which fields each provider reports for a child is in [sidebar internals](../internals/sidebar/sidebar.md#sub-agent-lists).

### Unread and attention marks

A card is unread from the moment it becomes waiting, failed, paused, or done until you focus its pane or press `m`. An unread card has a soft wash behind it. The one card that most needs you, the oldest unanswered `?` or `!`, also animates across its glyph, name, and description, and the make-up bucket that owns it animates with it. Every other unread card is bright and bold without motion, so one thing on screen moves at a time. A card that is both selected and unread shows the selection. The animation style is `[theme.animations] unread` ([theming: unread attention](../guide/theme.md#unread-attention)).

A waiting or failed card looks like any other card with `?` or `!` in the lead. A waiting card shows its open question on the second line when one is available, otherwise its session label:

```
▌? codex · GPT 5.5 · xhigh · 257k/286k          $4.36▐
▌  Allow the pricing cache write?                    ▐
▌  ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━╸───────── 75.1% ◑ 40m▐
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
▌  ▢ ────────────────────────────────  0.0%▐
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

`☾` replaces only an idle or done status, for timers, signals, conditions, and file watches, including a child's sleeping wait. A PID, command, check, unfinished team, working child, or listed background shell instead keeps the card working. After the turn ends, that card shows the `⢄` waiting head and still counts as working in the cockpit. A working-through-wait card names its first working wait. Waiting, failed, and paused (including a launched child's provider park) take precedence, and a standing subscription does not make an agent sleep. Sleeping opens no unread mark and sends no notification, and an earlier unread result stays unread through the sleep.

Hibernation does not change a card's state, glyph, ranking, or unread result. A frozen agent keeps the same working or sleeping wait classification, or its idle or done state when nothing is pending. `rimz agents show` carries the hibernation details.

### Process rows

A pane with no tracked agent, such as a shell, an editor, or a build, shows as a process row. Its ordinary lead glyph dims; the program name keeps the normal body tone. It shares the calm cards' newest-first order below asks and failures. An agent launched in that shell pane inherits its place, including after focus leaves and the layout freeze expires; an agent exiting back to the shell leaves its place to that shell. A new pane still enters at the top, and an attention-zone crossing still restamps the card.

```
▌○ codex                                   ▐
▎○ zsh                                     🮇
▎⣾ claude                                  🮇
▎  db migrate                              🮇
▎  ▢ ────────────────────────────────  0.0%🮇
```

Here the shell was first seen between the two agents' launches, so it sits between their cards.

```
 ○ zsh
 ⣾ cargo                       C  34%  M 512M  ⇅   8M/s
   cargo build --release
```

An idle row is `○` and the program's name. A working row spins, shows the running command on a second line, and shows CPU, memory, and I/O on the right once all three have values. A process stuck in uninterruptible sleep for ten seconds, or left as a zombie, shows `!`. The name is the program the pane really runs: RimZ reads past `env`, `sudo`, `timeout`, `sh -c`, and `node` or `npx` launchers, so `sudo npm install -g @openai/codex` reads as `npm`. Process rows are never counted in the cockpit. They are jump targets, and when a tracked agent starts in the pane the row becomes that agent's card. During an in-place launch, RimZ's launcher and supervised wrapper leave the shell's row idle and without a command detail until the card replaces it. Other RimZ commands, including `rimz agents logs -f`, keep their working row and detail.

### Not-tracked rows

A recognized agent pane with no bound session gets two lines when RimZ knows why it cannot read the agent. This [54-column frame](../../crates/rimz/src/sidebar/scenario/tests/snapshots/rimz__sidebar__scenario__tests__coverage__not_tracked_fixture_frame.snap) shows missing hooks:

```text
▌○ qoder · not tracked                               ▐
▌  tip: rimz hooks install qoder, then restart       ▐
```

The cause is missing hooks, installed hooks blocked by trust or policy, or a reporting mode the integration explicitly cannot follow. The tip gives the fix for that cause. A bound session takes precedence; a pane with working reporting and no known blocker keeps its idle card before the first prompt. If RimZ cannot inspect the integration's reporting, it leaves an ordinary process row rather than guessing a cause.

The suffix `· not tracked` stays visible: a long program label shortens first with `…`. The fix stays on one line, cut at the last whole word that fits, dropping a trailing comma before `…`; a first word wider than the line is cut within the word. Below 32 pane columns, only the `tip: ` label drops. Neither selection nor a stuck `!` adds a third line, a command detail, or resource stats. Clicking either line jumps to the pane.

Not-tracked rows sort as process rows do, never count in the cockpit, and fold under `+K more` like other process rows. After applying the tip and restarting where requested, normal reporting can replace the row with an agent card.

### Worktree headers

Each group starts with a header. Its left side names the work, and its right side shows where the work stands against the trunk.

```
▎⑂ feature-migration ◌ #91 ┄┄┄┄ ⇡3 ⇣1  +230 -23  ⑃ main🮇    ← diverged, open PR #91, CI running
▎⑂ fresh-fork ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ≡ main🮇    ← pristine: no commits of its own
▎↩ feature-landed ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ✓ main🮇    ← merged: safe to remove
```

On the left: `⑂` or `↩`, the branch name, the CI verdict (`✓` passing, `✕` failing, `◌` running), and the pull request number when the branch has one. An open or merged pull request supplies the CI verdict; a non-trunk branch without one shows no CI glyph, while the trunk shows its own HEAD-commit CI. Closed pull requests show no CI glyph. The team badge goes on the pipeline line when one is drawn, on the roster line when the group is folded, and on the header otherwise. In terminals that support hyperlinks, each PR number opens its pull request. A same-repository stack on GitHub or Gitea replaces the single badge with its levels, bottom first: `#522→#530→#540,#541`. Commas join PRs branching from the same level, where the display stops. The stack appears only when the entire run fits beside the full name and CI; otherwise only the branch's own PR number remains. A group outside the project root and not among its listed worktrees always adds a muted `· repo` qualifier naming its repo folder, even when that repeats the label. Colliding branch labels use the shortest path suffix that tells the checkouts apart; foreign groups deepen their qualifier by the same rule. When the header runs out of room, the name shortens first, then the pull request number is dropped, then the CI verdict last.

On the right: commits ahead of and behind the trunk with zero counts left out, then lines added and removed, then the trunk marker. The line counts include committed, staged, unstaged, and untracked work, so work that `git diff` does not show still counts. The first marker that applies is shown:

| marker | meaning |
|--------|---------|
| `⟳ main` | a local rebase, merge, or cherry-pick is in progress |
| `✓ main`, with `#N` on the left | the pull request merged; muted, and the commit and line figures are dropped |
| `✕ main` | the pull request closed unmerged; red, and the figures stay |
| `⇉ main` (` main` with Nerd Fonts, `worktree.pr_queue`) | the open pull request is in the merge queue; red when the queue removed it without merging it and it has not been re-armed; a removal outranks a conflict, while queued yields to it |
| `↯ main` (` main` with Nerd Fonts, `worktree.pr_conflict`) | the open pull request conflicts with its base at its current head; amber-orange |
| `⑃ main` | the pull request is open |
| `✓ main`, no `#N` | no pull request, and the trunk already contains the work: safe to remove |
| `≡ main` | pristine: a clean tree with no commits of its own, at the trunk's tip |
| `⑂ main` | a plain branch |

Pristine and merged headers show the marker alone. The trunk's own checkout shows no marker, only its commit and line figures. RimZ takes the trunk from `[sidebar] trunk`, then `main`, `master`, and the remote's default branch ([configuration](../guide/configuration.md#sidebar-rendering)).

The group that holds the selection is drawn as a lane: a dim `▎` down its left edge, `🮇` down its right, and a dotted seal across its header. Inside it the selected card has bright `▌` and `▐` spines over a recessed background, subagent entries included. Other groups have a blank gutter and a header without the seal. With no live selection, no group has a lane, spines, or a header seal. Previous selections can stay held open; other cards rest at their configured density. Expanded density still lists delegation entries; where a delegation header is shown, its override still controls the entries.

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
▌  ▢ ──────────────────────────────────────  0.0%      ▐
▌  ▤ 0                                                 ▐
▎⣾ codex · GPT 5.5 · 257k/286k                         🮇    ← same group, not selected
▎  add tests                                           🮇
▎  ▢ ──────────────────────────────────────  0.0%      🮇

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ? 1     ← panes with no channel and no known place, with its own ? and ! count
 ? claude
   Deploy staging?
   ▢ ──────────────────────────────────────  0.0%
```

Panes with no channel or known containing place (scripts, CI, stray shells) collect under the dim `external` divider, after active project groups and before the inactive chip. An agent's git root can make a foreign place outside the project; shells beneath it join that group. The external divider keeps a `? n` or `! n` count so an untethered ask still shows.

A room opened on a plain directory groups git-backed agents by their checkout, each under a full header. The panes the directory itself holds sit under a bold header with the directory's name, with no glyph or git figures, because a plain directory has no trunk to compare against.

```
▎⑂ main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ⇡2  +12 -3🮇    ← a checkout inside the directory: full header
▌⣾ claude                                              ▐
▌  db migrate                                          ▐
▌  ▢ ──────────────────────────────────────  0.0%      ▐
▌  ▤ 0                                                 ▐

 agents                                                     ← the directory's own group: name only
 ○ zsh
```

A group with one staged team and a readable worktree `.rimz/team/blackboard.md` containing a `Stage:` line gets a pipeline line heading the team's own block. It follows the workflow section, when present, and precedes the team's cards; other cohort blocks follow, then solo cards and process rows. Cohort members keep launch layout order, even when asking or failing; other rows keep attention rank. Blocks without a pipeline sort by earliest pane creation, missing last, then cohort name. This partition fills the six-row cap before idle solo rows. The team's members must identify one worktree:

```
▎⑂ pipeline ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄🮇
▎  forge ● ● ◉ Implement 47m ○ ○                  47m🮇
▌⣾ planner                                           ▐
```

The team badge leads, followed by the track; a stage outside the declared pipeline has no track and shows ` · ` between the badge and its name. The dots follow declared stage order: passed, current, then future, with the current stage's name immediately after its dot. Passed dots are green and future dots are muted unless already reached this run, in which case they are yellow: the shape says not done, the color says been there. Flipping backward moves the current dot back and warms the dots it left behind. At `Done`, every dot is passed and the last becomes the green done seal, followed by `Done` to keep the meaning visible without color. A board stage outside the declared pipeline shows its name without a track. The current dot and stage name take the visible stage owner's status color and animation phase, or stay muted when that owner has no visible card. The line itself adds no attention. An unfinished stage with a resolved owner and only idle or successful members, with no attention from their current children, raises `!` on the owner after `[agents.attention] stalled_after_secs` without member activity or a stage flip. Its error line reads `stage <Stage>: nothing running <age>`, with the usual unread and notification behaviour.

Without color there is no revisit cue; a forward re-entry warms nothing to its right.

The optional [`harness.stall_nudge`](../guide/configuration.md#stalled-team-nudge) adds no visual state: after the existing `!`, it queues one notice to the last-active teammate at 55 minutes of quiet per unfinished stage visit, only with no pending wait or real message. A turn the notice starts restarts the `!` window as any real turn does; keep-warm pings do not.

The stage duration sits immediately after the stage name, such as `Implement 4h12m`; the run total alone is pinned right, such as `4h51m`. Both are muted, without a clock face or slash. Below an hour a duration uses one whole unit (`45s`, `18m`); from an hour it uses two scaled units (`1h00m`, `4h12m`, `1d02h`). An unknown stage time leaves just the name; an unknown total leaves the right side empty. At `Done`, only the frozen total appears. The stage clock sums every visit to the current stage since the run started, counted from parseable entries in the board's `## Progress` (or `## Progress log`) ledger, including an `opened` entry. A backward flip resumes the stage's accumulated time; a same-stage re-flip changes nothing. The total starts at the first parseable ledger entry (or the latest flip out of `Done`, which also clears earlier visits) and runs until now, or until the last `-> Done` entry at `Done`; a `Done -> Done` re-flip does not move the stop. On a RimZ-written board the stage time never exceeds the total. A missing or future start omits that duration, and `Done` without a valid stop omits the clock. If a hand-edited `Stage:` disagrees with the ledger's open stage, the stage clock is absent. Clicking the line focuses the visible stage owner's pane, otherwise the first actionable team member, otherwise the group's first visible row.

### Workflow strips

An Active or Parked workflow keeps its worktree group, header, PR, CI and queue facts even between steps with no pane. Its strip heads the workflow section: header, workflow strip, current step's card, team strip, team cards, other cohort blocks, then solo cards and process rows. A team without live panes leaves no cards behind. When the run ends, its strip disappears; a group with no rows disappears with it. If several workflows share a checkout, the newest run supplies the strip.

```text
 ⑂ merge-lane ✓ #91                            ⇉ main
   land ● ◉ watch queued 23m ○ ◇ ○ ◇              25m
```

The workflow badge leads the main rail in walk order: reached steps and checks are `●` and `◆`, future ones `○` and `◇`. Only the current node is named, in bold after `◉`. A current detour keeps its parent named (`◆ watch ╰─ ◉ fix ×2`); past detours do not draw. `×N` appears from the second visit, yellow when a visit cap is used up. Reached follows the run's history, as on the rail in `rimz workflow show`, so nodes skipped by `resume --at` stay `○`/`◇`. An unreadable sealed definition draws the current node without a track.

With a current step card, the mark and name use its status tone and animation phase and omit the state word. Otherwise Active uses a static running tone, Parked a waiting tone, followed by the check outcome, `held`, `takeover`, `running` for a step, `checking`, or `parked`. The parked word is yellow. A hold reads `holding <outcome> <age>/<limit>`, using `?` for an unreadable limit. The duration is the outcome's age at a check, node-entry age at a step, or time since parking; the whole run's age pins right. A held session names its step, not the check holding it.

The strip adds neither a selectable row nor attention. Clicking it focuses the current step card when one is shown; otherwise it does nothing. A group with no rows has no lane selection, row ordinal, header jump, or unread reminder, and keyboard row navigation skips it. Its header drops churn before shortening its name. Its open PR still contributes to `⑃ N`, and that lens shows the group, counting it once; status and unread lenses exclude it. Search matches its label, qualifier, team, PR number (`#91`), or workflow name, and counts a rowless match once.

On narrow panes, the first fitting shape drops the run total, then the track while keeping a detour's parent, then the parent, badge, and duration, then ellipsizes the state word. The current mark and node name never shorten. The detour connector follows `chrome.box_bottom_left` and `chrome.hairline`, including ASCII overrides, in either glyph set.

A live workflow keeps its group out of the inactive section, landed or unlanded, with members or rowless. When the run ends, a group with members returns to the ordinary quiet window on its members' clocks; a rowless group disappears. The six-row cap and the header fold still apply to a group with a live run.

### Row cap and inactive groups

A group shows at most six rows in their section order, folding excess idle cards and process rows, including not-tracked rows, behind a dim `+K more`. Click it to show every row, and click `− less` to fold them again. The unfold survives focus changes and layout-hold expiry; it clears when you fold it or no row past the cap can hide. Working, waiting, failed, paused, done, unread, and selected cards are never folded. When only process panes remain hot, the first hot process row is also exempt, but cooled shells are not. While a filter is active the cap is off, and rows and groups that do not match are hidden.

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

A group becomes inactive once its latest agent launch or non-keepalive prompt is more than an hour old if landed, otherwise more than a day old. Running, asking, failed and parked members, and live workflow runs, block folding; unread rows block unlanded groups only. The main checkout and external groups never fold. Landed means a merged or closed PR or work merged into trunk, with a clean or unknown tree state. A dirty merged tree uses the unlanded window. Process-only groups use their latest activity instead of launches and prompts. Every inactive group with an agent collapses to its header and a two-line receipt:

```
 ↩ merged-work                                   ✓ main
 ▸ rimz  ✓ planner  ✓ coder  ✓ reviewer           $4.02
   ◇ 1M ↘ 300k ↗ 80k ◌ 900k · 75%                    2h
```

The first line lists the team name when the members share one, each member's final status and name, `+n` for members and shells that do not fit, and the group's lifetime cost. The second shows its lifetime tokens and cache hit, with time since the latest agent activity pinned right. This compact age is always present, plain and muted without a glyph, even under five minutes (`30s`, `12m`, `2h`). Cache pings do not move it, and process activity counts only in groups without agents. Cost and tokens cover every session the team ran in this worktree, resumed sessions and subagents included; the sidebar shows no work-time figure. When the line is too narrow for one member it reads `▸ +K done`.

Click a member's name or status glyph on a folded group's first receipt line to show the cards and focus that member's live pane, bringing its card into view. Clicking anywhere else on the receipt, or its folded header, does the same for the first receipt member. A member without a live pane opens the cards without moving focus. The cards stay unfolded through focus changes and layout-hold expiry until you fold the expanded header, close the inactive peek, or the group leaves the roster or has nothing left to fold. Focusing any member also shows the cards, in place without changing fold membership or count. Without a user unfold, newly inactive groups with members stay out while selected, focused, or held by the layout freeze; they fold on the first pass after those holds release. Revival is immediate, even during a freeze. The expanded header only folds a user unfold; it does not move focus. A header click on cards revealed only by focus, a layout hold, or a filter leaves that reveal unchanged. The `s` lens excludes results in inactive groups. Each revealed card shows that member's lifetime cost, so the cards add up to the receipt. A single-agent receipt uses the same status-and-handle chip; until lifetime totals arrive, the receipt pins the hidden agents' current session costs instead. A group with only process rows stays open.

Inactive groups follow all active project work and `external`, at the very bottom. At 54 columns, the closed inactive chip replaces their headers and receipts:

```
 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
 ✓ external
   task-external
   ▢ ────────────────────────────────────  0.0%

 ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ [ ▸ 3 inactive ] ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
```

Click anywhere on the chip, or press `i`, to peek at the first 10 inactive groups in presented order, most recent activity first, without hiding active work. If the receipts it reveals are below the fold, the cards scroll only far enough to show the first receipt in full, keeping the chip on screen. When the chip and receipt cannot fit together, the chip pins to the top of the card view and as much of the receipt shows as fits. Receipts already in view leave the viewport still. Toggle again to close it and bring the chip back into view; the selection never moves. Closing resets the page count and folds the inactive cards opened through receipts, except those still focused or held. Under `NO_COLOR` the chip is reverse video. The peek changes only when you click the chip or press `i`. Focus changes, receipt jumps, and tab switches keep its pages and unfolded receipts, with the destination showing it open from the first frame. Clients sharing a view share the toggle; splitting across tabs copies it, after which each copy toggles independently. Returning to a continuing viewer rejoins their view. It starts closed for a new view or host restart. Open groups remain collapsed to receipts until revealed:

```
 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
 ✓ external
   task-external
   ▢ ────────────────────────────────────  0.0%

 ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ [ ▾ 25 inactive ] ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄

 ↩ old-01
 ▸ ○ inactive-a
                                                   2h

 ↩ old-02
 ▸ ○ inactive-a
                                                   2h

 ↩ old-03
 ▸ ○ inactive-a
                                                   2h

 ↩ old-04
 ▸ ○ inactive-a
                                                   2h

 ↩ old-05
 ▸ ○ inactive-a
                                                   2h

 ↩ old-06
 ▸ ○ inactive-a
                                                   2h

 ↩ old-07
 ▸ ○ inactive-a
                                                   2h

 ↩ old-08
 ▸ ○ inactive-a
                                                   2h

 ↩ old-09
 ▸ ○ inactive-a
                                                   2h

 ↩ old-10
 ▸ ○ inactive-a
                                                   2h
   +15 older
```

If more inactive groups remain undrawn, a muted `+K older` line follows the last drawn one at the end of the body. Click it to reveal the next 10 without moving the viewport or selection. Focused, held, individually expanded, and process-only groups also draw beyond the page, and K counts only undrawn groups. Closing and reopening starts at 10 again.

The counts are groups, not agents. `M unlanded` is information, not a filter, and is omitted when M is zero. Every cell of the line toggles the same peek. A focused or held group stays visible below the closed chip without changing the count; process-only groups also stay visible. Any filter or a search suppresses the chip and reveals matching cards uncollapsed, but `s` never matches inactive groups. Unread results there remain reachable through `u`; `n` and `␣` skip hidden cards, while the actionable `↑/↓ N need you` banner is unchanged. `rimz sidebar frame` starts closed; `--expand` opens the chip and every card without a page cut or older tail. On narrow panes the breakdown drops first, then the label, preserving `[ ▸ N ]`; at widths too small for that frame, only the count remains.

A folded inactive group hides its pipeline line and puts the stage after the team name on the roster line:

```
 ⑂ pipeline
 ▸ forge · Done  ✓ planner  ✓ coder
                                                   0s
```

Expanding the group restores the pipeline line. Cohort blocks keep layout order, including asks and failures. Among other rows, asks and failures lead, newest attention first. Calm solo cards and process rows follow together in hot, cooling, and archived bands, newest order stamp first within each. An in-place launch inherits its shell's stamp; an attention-zone crossing still restamps the card. Process rows cool from the pane's first sighting; after an agent exits, they keep its last-activity clock if its card had cooled or archived, otherwise they age from the exit. Busy work never renews this clock. Groups holding asks or failures lead by urgency, then the main checkout, including when it has no agent. Other active groups put unlanded work before landed work, even when landed members are running. Within each set, groups sort by their newest agent launch, newest first; groups without agents follow by newest pane creation. At equal urgency within an active tier, unlanded groups lead landed ones, and groups with rows precede rowless workflow groups within each set. Rowless groups sort by newest node entry. Inactive groups with rows sort by latest agent activity, newest first, regardless of landedness. Read state never changes row order. The rules are in [how the column is ordered](../guide/sidebar.md#how-the-column-is-ordered).

## The provider dashboard

Provider budgets belong to an account, so they sit in a fixed panel at the bottom instead of on the cards. Each provider gets one block, for the account the room launches under; sessions still running on another account count toward none. Pi can use several backing credentials under that account: its block follows the selected Pi card's credential, in tabs and in stacked mode, while sessions, tokens, and spend stay whole-account. Without a Pi card reporting a known credential selected, it holds this view's last selected credential while available, then uses the provider found by the account check. If nothing is known yet, the header and stats remain without a plan or budget row. A provider whose account has metered budgets or recorded usage keeps its block even when none of its sessions runs in this room, and RimZ refreshes its budgets between turns.

```
 ─── Claude ──── Codex ────────────────────────────────

           Claude Max · v2.1.169                   ⇅ rc
  ▐▛███▜▌  ◎ 53  ◇ 16M ↘ 13M ↗ 2M ◌ 198M        $188.88
 ▝▜█████▛▘ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱ ↻  1h47m
   ▘▘ ▝▝   7d  ▰▰▰▰▰▰▰▰▰▰▰▰▰╱▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱ ↻  5d22h
```

With `provider_tabs = "auto"` (the default), two providers stack and three or more share the panel under a tab rail, one block at a time. The active tab follows the selected card's provider, holds the last agent's provider for a process row, and falls to the first tab only when neither is available. `←`, `→`, or a click picks a tab by hand until you select a card of another provider. Under `NO_COLOR` the active tab is marked `┤ Claude ├`. `[theme.display]` sets tabs or stacking with `provider_tabs`, the providers and their order with `provider_list`, and the most stacked blocks with `max_provider_blocks`, 3 by default ([theming: display](../guide/theme.md#display)). By default providers with sessions in this room come first, then the recently used, then the recently logged in.

| line | shows |
|------|-------|
| header | In tabs: the plan, then the version. Stacked: the provider and version, then the plan. The version reads `v?` until a source reports it, and the plan is absent until the account names one or while subscription access is lapsed. `⇅ rc` and the reset-credit marker (`↻ N`, or Codex-only `⟳ N` or `‖ N`) sit on the right; a lapse suppresses reset credits too. |
| stats | `◎` sessions in the [spend window](#the-cockpit), the `◇ ↘ ↗ ◌` tokens with cache creation counted in `↘`, and the provider's spend. A provider with no usage history keeps the row: `◎` counts its sessions active in this room, and the token and dollar slots show `–`. |
| budget rows | the two shortest time windows (or the sole window), with a label, bar, and reset countdown; a longer spent cap takes the second row, or leads usable extra usage. Named-quota accounts draw every enforced quota and bypass those substitutions. Lifted windows appear only when no enforced window remains. While lapsed, one bold alarm row replaces every bar |

An emblem taller than the stats and bars (counting at most three of those rows) raises one row into the header, or the spacer in wide stacked blocks. Three-row emblems with one bar save a line; two-bar blocks keep their layout. Four-row emblems keep their existing crest and three body rows; two-row emblems already fit a one-bar block.

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
  █▜███▛█
 ▝▜▛▀▀▀▜▛▘ ◎ 19  ◇  8M ↘  7M ↗ 1M ◌ 142M        $420.42
  ▝▘   ▝▘  api ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰ ∞            ← API key with no budget
```

### Budget bars

A bar's filled part is the budget left. Every bar starts and ends on the same columns, so the rows compare at a glance.

A lapsed subscription keeps the historical stats but removes the plan label and replaces all bars with one bold alarm row. An API-billed login never shows this alarm. The header uses the account's valid display label or its name (here `work`), and the row names the product alone. The first-seen local timestamp is truncated with the row at narrow widths, never wrapped:

```
────────────────────────────────────────────────────────────────────────────────────────────────────
Claude · work v2.1.158                                                                          ⇅ rc
 ▐▛███▜▌
▝▜█████▛▘ ◎ 12  ◇ 498k ↘ 434k ↗ 64k ◌ 68k                                                      $3.50
  ▘▘ ▝▝   plan lapsed · no Claude access · since <first seen>
```

| you see | meaning |
|---------|---------|
| `plan lapsed · no Claude access · since …` and no bars | Claude Code subscription access was rejected, never an API-billed login running out of credit. The header has no plan label, and the timestamp is when RimZ first saw the rejection, in local time. A successful usage read clears the lapse; it does not age into an unknown track. |
| a partly filled bar and `↻ 1h47m` | The window is running. The fill slides from green through gold and amber to red as it empties. |
| a full bar and no countdown | The sliding window has not started: its clock starts at your first token. Fixed periods, including billing months, count down even with no usage. |
| an empty track, all red | The window is spent. |
| a short window painted spent while a longer one is spent | The longer window gates it: a spent `7d` makes `5h` unusable until the `7d` resets, so `5h` shows red with no countdown whatever its own reading. |
| a dim empty track and no countdown | The reading is unknown: RimZ holds only a cached reading whose window has since reset, or the provider has not reported its windows yet. |
| a full bar and `∞` | No limit applies: an API-key or Claude Console login without a counting monthly cap or display ceiling, or extra usage with no limit or balance figure. Unlimited named quotas and lifted limits also draw this bar, but only when no enforced window remains. |
| an `ex` row | Paid extra usage on accounts with only time windows, shown while an included window is spent and extra usage is available: the longest spent window comes first, then `ex`. The figure is dollars left, never the budget size. With a limit the bar drains; without one a positive balance draws full. Reported unlimited extra usage draws full with `∞`, whether or not a display ceiling is set. There is no `ex` row when the provider reports nothing, extra usage is off, or no dollars are left. Without usable extra usage, a longer spent window replaces the second included row (`5h` and `mo` when the month is spent). Dollar figures on `ex` and `api` floor: cents below $10 (dropping `.00`), grouped whole dollars below $100,000, whole thousands below $1 million, then millions with one decimal. |
| an `api` row | An API-key or Claude Console login. A counting `/month` account cap drains against your history pool's spend since 00:00 UTC on the 1st, with the dollars left on the right; raises enlarge the bar's cap. Without a monthly cap, a display ceiling drains against 30-local-day spend; without either, the bar is full and reads `∞`. A daily cap does not replace that display ceiling. Stale month spend paints an unknown track until refreshed; a pool with no spend this month shows the full cap left. The row never shows the provider's credit balance. |
| `cr`, `bld`, `dep`, and other names | A quota the provider or a plugin names, such as Copilot's AI credits. Named quotas are independent: one spent quota does not mark another spent. A named quota with no reported length shows its reset in a quiet tone. |

The color of the `↻` glyph beside the countdown shows your pace. It is neutral when the current burn rate lasts to the reset, slides through gold and amber to red as spending outruns the window, and cools toward green when spending runs well under pace, once 40% of the window has passed. The stops are `[theme.display.budget_bar.burn_rate]` ([theming: display](../guide/theme.md#display)).

A model's own cap inside a window draws as a `╱` tick on that window's bar instead of a row. The fill and the tick use different scales: the fill is the window's budget left, and the tick's position is the share of the model's cap left, across the full width of the bar. Only exactly 0% reaches the left end and only exactly 100% reaches the right. The tick's color follows the model's share, and it has no label or countdown. In [the whole frame](#the-whole-frame), the `7d` bar has about 65% left while the tick near 42% says the model's weekly cap is closer to spent than the window. A spent or gated bar draws no tick; the model's reading returns with the parent's reset. An unknown reading or an unlimited row has no tick.

A longer allowance can cap a shorter bar before that bar empties. When the provider reports comparable amounts, `╎` marks the tightest longer allowance on the shorter bar's scale: a monthly remainder equal to a third of a week's allowance puts a tick at one third of `7d`. It appears only below what the bar shows left, uses that share's budget color, and has no countdown. Tiny remainders stay off the left edge; if a model tick shares its cell, the ceiling wins. Percentage-only providers keep their existing bars and model ticks.

Here the month's remaining allowance is one third of the week's limit, below the week's 80% remaining but above the short window's remaining allowance. Only `7d` gets the ceiling:

```
────────────────────────────────────────────────────────────
Qwen v1.0.0 · Alibaba Coding Plan

 ▐▀▀▀▀▀▌  ◎ 12  ◇ 498k ↘ 434k ↗ 64k ◌ 68k              $3.50
▐█ ▘ ▘ █▌ 5h  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱ ↻  1h00m
 ▝▀▀▀▀▀▘  7d  ▰▰▰▰▰▰▰▰▰▰▰▰╎▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱ ↻  1h00m
```

A block header counts banked Codex reset credits or Claude limit resets with `↻` in the body tone: redeeming is yours to do ([`rimz accounts redeem`](../reference/cli/accounts.md#redeem) for either provider). The marker is absent at zero. Pi and OpenCode accounts using Anthropic show the same banked count. Only Codex forecasts what [auto-redeem](../guide/loops.md#auto-redeem) would do if the longest window ran dry and parked an agent right now: a green `⟳` means armed (a credit would be spent, or no natural reset is known to wait for). A muted `‖` means holding: the fleet would park until the reset. When the reset is nearer than `resume.auto_redeem_min_gain` and the nearest credit outlives it by a day, it stays a hold until then. With Codex auto-redeem off, RimZ still rescues a credit in its last 30 minutes; Claude redemption remains manual only. Within a week of the nearest credit's expiry the time left follows the count, floored to one unit (`· 6d`, `· 14h`, `· 29m`). That time heats from green to red while the marker is `↻`, and deepens from the body tone to green while auto-redeem handles it. The marker holds still in every state. The Nerd Font glyph set draws the three as `󰑐`, `󰁪`, and `󰏤`.

How budgets are read, cached, and refreshed is in [provider internals](../internals/agents/providers.md).

### Pets

With `[theme.pets] enabled = true`, the active block narrows and an animated companion with a caption sits at its right edge. It draws as pixels where the terminal supports kitty graphics (15 by 9 cells) and as cell art otherwise (18 by 9 cells). It is hidden under `NO_COLOR` and when the pane is too narrow. See [pets](../guide/pets.md).

### Narrow panes

The pipeline reserves the whole run total, then admits the first shape that fits: track, name, and stage time; track and name; name and stage time; or an ellipsized name. The team badge fits only beside a drawn track or an undeclared stage and drops first. Stage time can reappear after the dots drop; dropping a declared track never buys the team badge back. The name keeps two cells beside an admitted total, and neither duration nor track draws partially.

As the pane narrows, a block drops the input and output token split, then the version text. Below 34 columns the provider emblem goes and the bars run the full width. A pet narrows the block further.

### The fleet store

The last two rows of the dashboard total every provider and every project on this machine's accounts, for the last 7 (`W:`) and 30 (`M:`) local calendar days, including today.

```
  ── Total: ───────────────────────────────────────────
  W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88
  M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66
```

Each row reads sessions, total tokens, input including cache creation, output, cache-read, and spend, to one decimal and aligned in columns. The figures come from the agents' transcripts, with Codex's dollars priced from its token counts, and read `$0.00` until something is recorded. They do not animate. On narrower panes the dollars move to a third row, `W: $...` on the left and `M: $...` on the right, and the token rows drop the input and output split when they must. How the totals are computed is [Token Insight](../guide/insight.md#how-the-numbers-are-calculated).

## Bottom chrome

The footer is the last line, apart from hook-drainer alerts and recovered refresh alerts, which sit beneath it. The cards give up space before the dashboard or the footer do, so neither scrolls away.

```
 zᶻ away                 ? for help
```

| mark | when |
|------|------|
| `⇄ 210ms`, `⇄ 210ms 15%` | an SSH room's round-trip time, with probe loss added above 10% |
| `⇄ …`, `⇄ stale` | the SSH reading is warming up or stale |
| `⧗ stall mem 34% · cpu 81%` | machine/container resources stalling on the room host; absent when none is tripped |
| `mem 6%↓` | a fading stall item, muted instead of bold amber |
| `zᶻ idle`, then `zᶻ idle · 17m` | no human input for `[sidebar] afk_after_secs` (15 minutes by default), on both backends once an input timestamp is known; minutes are added after the first minute |
| `zᶻ away` | no terminal client is attached |
| `? for help`, `?` | help at the right edge, shortened or dropped when the line is crowded |

The SSH anchor takes the left edge, followed by the stall notice and then the presence badge, with two cells between groups. Help stays pinned right with at least a two-cell gap. The anchor's color runs from green through yellow and amber to red, bold at the worst, and stays neutral until the link has a first reading; stale readings are muted. On both backends, `afk_after_secs` also bounds retention of departed clients' sidebar views ([configuration](../guide/configuration.md#sidebar-rendering)).

Stall figures are Linux PSI's `some` averages: the percentage of time at least one task waited for memory, CPU, or I/O, not how much of the resource was used. An item shows when its ten-second or minute average reaches 10% for memory, 50% for CPU, or 25% for I/O. The figure is the current ten-second average, rounded to a whole percent. Below 80% of its threshold it fades with `↓`; it clears when both averages fall below the threshold. Active items come first, then fading ones, with memory, CPU, I/O in that order within each group. The lead and active figures are bold amber; fading items are muted (dim under `NO_COLOR`). Unavailable readings contribute no item.

As the pane narrows, presence drops first, help shortens to `?`, and a healthy or warming SSH figure becomes bare `⇄`. Bad-link figures and `stale` never drop. Next help drops, the items after the first collapse to `+cpu` for one hidden item or `+N` for several, the tag becomes numeric even for one item, and the word `stall` drops while its glyph stays. `?` returns whenever it fits beside a later shape; the final shape clips at the right edge. An active refresh alert hides the footer and its stall notice until the room can be read again.

### Notices and alerts

Automated starts that need a fix add warning notices above the footer, one per cause: a failing capacity check, the disk floor, or an unreadable agent count. Each names the oldest held start's reason and points to `rimz doctor`. Condition fires, workflow steps, and live blocking fires in this room all count; retries do not. With several starts of one cause, `· N held` appears. Narrow panes clip the reason first, keeping the count and command. These notices clear on the next fold after the holds clear, have no dismiss key, and keep the footer unfolded. An active sidebar alert hides them until recovery, like the skipped-settings notice.

```
 ⚠ capacity check failing: claude exit 1 · 4 held · rimz doctor
 ⚠ starts held: free disk 1G < 2G at /repo · rimz doctor
 ⚠ starts held: cannot count open agents: broken record · 2 held · rimz doctor
```

Self-clearing holds add no sidebar line; inspect them with `rimz loop list` or `rimz workflow list`.

At 46 columns the same notices keep their suffixes:

```
 ⚠ capacity check fail · 4 held · rimz doctor
 ⚠ starts held: free disk 1G <  · rimz doctor
 ⚠ starts held: cannot · 2 held · rimz doctor
```

A [workflow run](../guide/workflows.md) raises no attention notice. Failures lead to an on-break agent or end the run; an agent asks in its own pane. Legacy parked runs retain the worktree strip's yellow `parked` word and appear in `rimz workflow list`.

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

If an agent's hook drainer stays unreachable for 10 seconds, a separate alert warns that its card may be stale. It names the card by its displayed label, falling back to `@name` or the provider name if the card is missing:

```
 ⚠ hook drainer unreachable for 40s · coder's card may be stale
```

When several agents are affected, it names the earliest one's card: `coder and 2 other cards may be stale`. The dashboard, footer, notices, pets, and search stay usable; this alert never discards a search draft. Pressing `x` cannot dismiss an active hook-drainer alert. After recovery it stays active for about 3 seconds, then lingers dimly until `x`:

```
 ⚠ last alert 2m ago: hook drainer unreachable for 40s · x dismiss
```

An active `Sidebar degraded` alert hides the hook-drainer line. Pressing `x` clears that alert and any recovered drainer alert. After refresh recovery both alert lines can show, with the hook-drainer line directly above the refresh alert. Short reconnects never raise a sidebar alert. `rimz doctor` retains the outage evidence even after dismissal; see [troubleshooting](../guide/troubleshooting.md#the-hook-drainer-unreachable-alert).

### The help overlay

`?` opens the overlay over the bottom right of the cards. Any key the sidebar reads closes it and is consumed, and so does focus leaving the sidebar. While typing a search, `?` enters a character instead. The overlay lists your configured movement keys, so it differs from this frame after a rebind.

```
╭ help ───────────────────────────────────╮
│ keys                                    │
│   /   search           esc   clear      │
│ ↕ j/k cards/entries  ↕ J/K   worktrees  │
│ ↕ g/G ends           ↕ ^f/^b page       │
│ ↕ H/L screen         ⏎ l     focus/open │
│ ⏎ 1-9 direct         ␣ n/N   needs-you  │
│ ✉ m   read/unread    ✉ M     read all   │
│ ↔ ←/→ account tabs   ⟳ r     reload     │
│ ✕ x   dismiss        ↕ a/d   width      │
│   i   inactive fold                     │
│                                         │
│ filter                                  │
│ ? q   waiting        ! e     attention  │
│ ⏸︎ p   paused         ✓ s     done       │
│ ⢿ w   working        ○ o     idle       │
│ ☾ z   sleeping                          │
│ ● u   unread         ≡ A     all        │
│                                         │
│ ▐ alt p sidebar                         │
│   alt g zoom                            │
╰─────────── any key to close ────────────╯
```

## Keys and mouse

These keys work while the sidebar has focus. From any other pane, `Alt+p` focuses the sidebar, and pressing it again returns to a work pane in the sidebar's tab: Zellij returns to the exact pane you left, and tmux takes the first work pane in that tab. `Alt+g` zooms the focused work pane to fullscreen and never zooms the sidebar. Both are set by `[sidebar] focus_key` and `zoom_key` ([configuration](../guide/configuration.md#sidebar-rendering)).

| key | action |
|-----|--------|
| `j` / `k`, `↓` / `↑` | walk cards, their open live-pane entries and `+K older`, without changing focus; skip waits and pane-less entries; with no selection, select the first card |
| `J` / `K` | select the first row of the next or previous worktree |
| `g` / `G` | select the first or last row |
| `Ctrl+f` / `Ctrl+b`, `PageDown` / `PageUp` | move a page; under tmux's default prefix `Ctrl+b` never arrives, so use `PageUp` |
| `H` / `L` | select the first or last row on screen |
| `Enter`, `l` | focus the selected card or child; on `+K older`, open history and focus the parent, returning the cursor to the card |
| `1` to `9` | focus the pane of the Nth row, counted from the top of the list |
| `n`, `Space` | jump to the next card that needs you: unread cards oldest first, then read `?` and `!` cards oldest first; attention lifted by a waiting launched child targets its live pane, first in entry order, otherwise the parent's pane |
| `N` | the same walk, backward |
| `m` | mark the selected card read or unread, without jumping; with no selection, do nothing |
| `M` | mark every card read |
| `i` | toggle the inactive peek; opening reveals its chip and first receipt, closing returns to the chip; selection stays put |
| `←` / `→` | switch the dashboard tab |
| `a` / `d` | make the sidebar one step narrower or wider in every tab of the room; the width holds through terminal resizes, reloads, and reattaches until the session ends ([width rules](../guide/sidebar.md#setting-the-width)) |
| `r` | reload this tab's sidebar |
| `x` | dismiss recovered alerts or clear an active `Sidebar degraded` alert |
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
| `A` | all; clear both the body pick and the text search |

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
| click a card or process row, including either line of a not-tracked row | select the row immediately and focus its pane |
| click a pipeline line | focus the visible stage owner, else the first actionable team member, else the group's first visible row |
| click a workflow strip | focus its current step's visible card, otherwise do nothing |
| click a make-up bucket or `⑃ N` | filter the cards |
| click `↑ N need you` / `↓ N need you` | reveal the lead card where it sits, or scroll to the top if the filter hides it |
| click `↑ <name>` / `↓ <name>` | reveal the focused card without changing selection |
| click a subagents and waits line | toggle its entries, select the card and focus its own live pane |
| click either line of a subagent entry | select the parent card and focus the child's live pane, else the parent's pane |
| click `+K older` | append older entries, select the card and focus the parent's live pane |
| click `+K more`, `− less` | expand or cap the group without moving focus; an unfold survives focus changes |
| click a member's name or status glyph on a folded inactive group's first receipt line | show its cards and focus that member's live pane, revealing its card |
| click a folded inactive group's header or any other receipt cell | show its cards and focus the first receipt member's live pane, revealing its card |
| click an expanded inactive group's header | fold cards you unfolded without moving focus; cards focus, a layout hold, or a filter reveals stay shown |
| click anywhere on the inactive chip, or press `i` | show or hide a paged peek at inactive receipts without filtering; opening reveals the first receipt, closing returns to the chip and resets paging |
| click the inactive peek's `+K older` | reveal the next 10 inactive groups, keeping the viewport and selection |
| click a dashboard tab | switch provider |
| wheel | scroll the cards without moving the selection |
| drag the pane border | set the sidebar width for the room |

For 400 ms after cards move without your input, a click targets the card in the preceding frame, or does nothing if it has left. Scrolling, browsing, and other input paints clear that window.

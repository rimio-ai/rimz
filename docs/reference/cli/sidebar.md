# Sidebar cards

`rimz sidebar` navigates and acts on the cards the sidebar shows. These commands work from any pane, without moving focus into the column first. For reading a frame, see [sidebar frames](./pane.md#render-a-sidebar-frame); for repairing the column, see [`sidebar repair`](./maintenance.md#repair-sidebars).

| Command | Effect |
| --- | --- |
| `walk next\|prev [--worktree]` | Focus the next or previous visible row, or the first row of the next or previous worktree group. |
| `slot N` | Focus room-wide slot `N` (`1` to `9`). |
| `last` | Focus the view's previous card. |
| `filter needs-you [--toggle]` | Set, or toggle, the needs-you lens. |
| `close [--pane PANE_ID \| --from-pane PANE_ID \| --tab]` | Close the highlighted row's pane, the pane of the card that owns `PANE_ID`, the card of the pane a key was pressed in, or every card in the focused tab. |
| `reopen` | Resume the most recently closed agent card. |

Native room style, the default, binds none of these commands to multiplexer keys. With `[room] style = "rimz"`, every bundled [room keymap](../keymaps.md#actions) binds `walk`, `last`, `filter needs-you --toggle`, `close`, and `reopen`; the `tmux` and `zellij` keymaps also bind `slot`, to the prefix digits and the tab-mode digits, while `rimz` and `vim` leave it unbound, and only `rimz` binds `close --tab`, to `Alt+X`. `rimz keys` lists the chords. Inside the sidebar pane, the default [sidebar keys](../../guide/configuration.md#sidebar-keys) reach the same actions under both styles: `1` to `9` for `slot`, `Tab` for `last`, `f n` for `filter needs-you --toggle`, and `d d` for `close --pane` on the selected card. The [command palette](./palette.md) offers `walk`, `slot`, and `last` as room rows.

## Room and view

Every command accepts `--session-name S`, and [`split`](#split-from-the-sidebar) requires it. Without it, RimZ resolves the room from the current directory (or the global `--root`) and its room pin, as other participant commands do. With it, RimZ uses the room of multiplexer session `S`, even from an unrelated directory. Use the global `--mux tmux` or `--mux zellij` when backend detection would choose the wrong multiplexer.

`walk`, `slot`, `last`, `filter`, and `close` ask the room's sidebar host to act on its current view, including its selection, order, and layout freeze. The acting card is the highlight: it follows the focused card, but stays on the last card while focus is on the sidebar or another non-card pane. All attached clients must share one view. With no attached client, a client on a tab without a sidebar, or clients on different tabs, the command is a no-op rather than a guess at whose selection to change.

A focus that `walk`, `slot`, or `last` moves onto an agent's card follows the [card click](../../interface/sidebar.md#mouse): when the agent has left the card's pane, focus stays where it was and the sidebar shows a `focus failed` notice with the reason, while the command still exits 0, since the host answers before the focus runs.

An open help panel closes before the command acts. An unfinished search draft is cancelled; a committed search remains part of the lens.

`close --pane` and `close --from-pane` are the exception: each names its row, so it acts through any attached client's ready view, clients may view different tabs, and it leaves help and the search draft as they are. `close --tab` asks the same way for each pane of its tab. `reopen` does not use the sidebar host at all, so it works with no attached client.

## Walk cards or worktrees

```sh
rimz sidebar walk next
rimz sidebar walk prev --worktree
rimz sidebar walk next --session-name my-room --mux tmux
```

`walk next|prev` focuses the next or previous visible top-level row with a pane, wrapping at either end. Agent cards, shells, editors, and other process rows take part. Child entries, pane-less rows, and rows hidden behind a fold do not. A filter or committed search narrows the walk to its matches. When the highlight is not among those rows, `next` starts at the first row and `prev` at the last.

`--worktree` moves to the first pane-backed row of the next or previous visible worktree group, wrapping and skipping groups with no visible pane-backed row. Each command moves the selection immediately, so repeated commands advance even before the previous focus jump lands. The [layout freeze](../../guide/sidebar.md#the-layout-freeze) keeps the order steady while you walk.

## Jump by slot

```sh
rimz sidebar slot 3
```

Slots number every top-level pane-backed row in the room by pane birth, across groups, filters, and folds. The rows do not show their numbers; the sidebar's `1` to `9` keys use the same ones. `slot N` focuses that row's pane even when a filter or fold hides it; the row stays hidden under that lens. Closing a pane renumbers the later slots. Only `1` through `9` are accepted. A slot beyond the current row count is a no-op.

## Return to the previous card

```sh
rimz sidebar last
```

`last` focuses the previous card in this view. A jump between cards advances history immediately, so `last` returns to the card before that jump even while focus is still landing. Repeating it toggles between the two cards without waiting. Visiting the sidebar or a popup does not advance history. Opening a new tab keeps the card you left as the previous card, even before the new tab's card appears. When the previous card's pane leaves the room, `last` goes to the newest older card still in the room. Empty history, or history with no card left in the room, is a no-op. History lives in the sidebar host and is forgotten when the host restarts.

## Show what needs you

```sh
rimz sidebar filter needs-you
rimz sidebar filter needs-you --toggle
```

Without `--toggle`, the command sets the lens, and is a no-op when the lens is already on. With `--toggle`, it turns the lens off when it is on, and on otherwise. Turning it on when nothing matches is a no-op.

The lens shows exactly the waiting and failed cards, including a parent lifted by a waiting child or by a provider-native child's failure. Paused cards and process rows are excluded. Unlike the make-up buckets, it selects individual cards rather than every teammate. The header reads `needs-you: N`.

The lens is room-shared and intersects a committed search. It clears itself when its room-wide count reaches zero, and while a layout freeze holds matching rows it waits for the freeze to release them. `F` in the sidebar clears both the lens and the search.

## Close and reopen

```sh
rimz sidebar close
rimz sidebar close --pane tmux:%4
rimz sidebar close --from-pane tmux:%4
rimz sidebar close --tab
rimz sidebar reopen
```

`close` closes the highlighted row's pane immediately, without confirmation. With `--pane PANE_ID` it closes the top-level row whose pane that is instead, whatever is highlighted; a launched child's pane, which nests under its parent's row, stops that child alone. `--from-pane PANE_ID` names the pane a key was pressed in, as the `card.close` key passes it: it closes as `--pane` does, except that a sidebar pane or a pane in the dashboard tab closes nothing and prints nothing, and when the multiplexer cannot list the session's panes nothing is closed and the command exits 1. For an agent row with a stored agent behind it, it asks the multiplexer where that agent is now and uses the same stop path as [`rimz agents stop`](./agents.md#stop), stopping launched children before their parent: the stop closes the pane the agent is in at that moment, which need not be the row's pane, and a row that lags a swap stops the session that replaced it. When no pane hosts that agent, the stop reports why and the row's pane stays open; when the multiplexer cannot be read, nothing is closed and the command exits 1. For any other row (an agent with no stored agent behind it, such as an idle agent CLI, a shell, editor, TUI, or untracked agent), it closes the row's pane and whatever runs there. When the pane it closes is the last tiled pane of its tab besides the sidebar, `close` closes the whole tab instead, floating panes included, in one multiplexer command. When that tab is the session's only one, it closes the pane alone and the sidebar then closes itself with the emptied tab, so the session outlives the close as the agent exits; run from a pane of that tab, it also closes the pane alone. Only root sessions that `agents stop` would offer to resume, with a saved conversation, enter the reopen list; child sessions and process rows cannot be reopened this way.

`close --tab` closes the tab the attached client's focus is in, without confirmation. It needs one attached client focused on one live pane, and exits 1 otherwise. It stops every agent in the tiled panes of that tab, each the way `close --pane` stops that pane's agent, in one stop, and then closes the tab, sidebar and floating panes included, in one multiplexer command. Each reopenable agent enters the reopen list on its own, in pane order, so each `reopen` brings back one of them. Focus on the sidebar, or in the dashboard tab, closes nothing. When the host refuses any of the tab's panes, it prints that reason and closes nothing; when one agent's stop fails before its pane closes, the tab's other tiled panes close one by one, that agent's pane keeps the tab open, and the command exits 1 naming that agent. In the session's only tab, or run from a pane of that tab, it closes the tab's tiled panes one by one instead, and the sidebar then closes itself with the emptied tab.

`reopen` resumes the most recently sidebar-closed agent that is not already live. It runs the resume outside any pane, so the agent opens in a new focused tab, not its old tab. A team seat runs `rimz agents TEAM.ROLE --continue`, adding `-w NAME` when it ran in a managed worktree; a solo session runs `rimz agents resume @SESSION-ID`. The resume's own receipt stays off stdout. Each command tries one entry. A failed resume reports its error and drops only that entry, leaving older cards for the next `reopen`.

The room remembers at most 32 closed agent cards, in `lanes/closed-cards.json` under the room's runtime directory. A successful reopen consumes its entry and discards newer entries whose agents are already live again. An empty or all-live list is a no-op that keeps the list. The list is a convenience undo, not session history: room reset, teardown, and reboot discard it, and other ways of closing an agent add no entry. Use [`rimz agents resume`](./agents.md#resume-agents) to recover sessions independently of this list.

## Split from the sidebar

```sh
rimz --mux tmux sidebar split down --from-pane tmux:%1 --session-name my-room
```

`split right|down` is the hidden verb a room keymap's split key runs when it is pressed on the sidebar; the sidebar itself never splits. `--from-pane` names that sidebar and `--session-name` its multiplexer session; both are required. The command focuses the work pane behind the sidebar (the tab's last focused work pane while it is still open in the sidebar's view, else the first work pane there), then splits it to the right or below with focus in the new pane, in that pane's directory. On tmux, when that work pane is a member of a collapsed stack with other members, the new pane opens beside the whole stack, where the split key pressed in that member splits the member alone. On Zellij 0.44.x it can make no pane. It samples the attached clients' view to focus the work pane, and never refuses on how many clients are attached.

| Outcome | Exit | stderr |
| --- | --- | --- |
| Split | 0 | Nothing. |
| The sidebar is alone in its view | 0 | `rimz: sidebar is the only pane in its view; nothing to split` |
| `--from-pane` is not a RimZ sidebar in the session | 1 | `error: pane PANE_ID is not a RimZ sidebar in session S` |
| The session is not a RimZ room | 1 | `error: session S is not a managed RimZ room` |

stdout stays empty.

## Output and exit codes

All six commands keep stdout empty, so a key binding never opens tmux's `run-shell -b` output overlay for a harmless no-op.

| Outcome | Exit | stderr |
| --- | --- | --- |
| Success | 0 | Nothing. |
| No-op | 0 | `sidebar VERB: REASON`, from the table below. |
| No room for `--session-name S` | 1 | `error: no room for session S` |
| Sidebar host unavailable (no listener, end of file, or no reply within 5 seconds) | 1 | ``error: sidebar host: CAUSE; run `rimz sidebar repair` `` |
| Failed stop, pane close, or resume | 1 | The error; for a resume, `error: sidebar reopen: resume failed (STATUS): STDERR`. |
| Invalid arguments, including a slot outside `1..=9` or a malformed `--pane` | 2 | The usage error. |

| Command | No-op reasons |
| --- | --- |
| `walk` | `no visible cards` |
| `slot` | `slot N is empty: K cards` |
| `last` | `no previous card`, `previous card left the room` |
| `filter needs-you` | `already showing needs-you`, `nothing needs you` |
| `close` | `no highlighted card`, `no card owns pane PANE_ID` |
| `reopen` | `no closed card to reopen` |
| Any but `reopen` | `no attached clients`, `client's tab has no sidebar`, `clients view different tabs; focus one client`, `sidebar view is not ready; try again`, `sidebar busy; try again`, `request expired; try again`, `view changed while the request was queued; try again` |

A reason from a per-command row also appears as a fading notice in the sidebar, unless another alert is already showing there. The reasons in the `Any but reopen` row are decided before the request reaches a view, so they appear only on stderr. A sidebar key that runs `close --pane` shows that command's first stderr line as the notice.

# Sidebar cards

`rimz sidebar` navigates and acts on the cards the sidebar shows. These commands work from any pane, without moving focus into the column first. For reading a frame, see [sidebar frames](./pane.md#render-a-sidebar-frame); for repairing the column, see [`sidebar repair`](./maintenance.md#repair-sidebars).

| Command | Effect |
| --- | --- |
| `walk next\|prev [--worktree]` | Focus the next or previous visible row, or the first row of the next or previous worktree group. |
| `slot N` | Focus room-wide slot `N` (`1` to `9`). |
| `last` | Focus the view's previous card. |
| `filter needs-you [--toggle]` | Set, or toggle, the needs-you lens. |
| `close [--pane PANE_ID]` | Close the highlighted row's pane, or the pane of the card that owns `PANE_ID`. |
| `reopen` | Resume the most recently closed agent card. |

Native room style, the default, binds none of these commands to multiplexer keys. With `[room] style = "rimz"`, every bundled [room keymap](../keymaps.md#actions) binds all six (its `filter.needs_you` key runs `filter needs-you --toggle`), and `rimz keys` lists the chords. Inside the sidebar pane, the default [sidebar keys](../../guide/configuration.md#sidebar-keys) reach the same actions under both styles: `1` to `9` for `slot`, `Tab` for `last`, `f n` for `filter needs-you --toggle`, and `d d` for `close --pane` on the selected card. The [command palette](./palette.md) offers `walk`, `slot`, and `last` as room rows.

## Room and view

Every command accepts `--session-name S`. Without it, RimZ resolves the room from the current directory (or the global `--root`) and its room pin, as other participant commands do. With it, RimZ uses the room of multiplexer session `S`, even from an unrelated directory. Use the global `--mux tmux` or `--mux zellij` when backend detection would choose the wrong multiplexer.

`walk`, `slot`, `last`, `filter`, and `close` ask the room's sidebar host to act on its current view, including its selection, order, and layout freeze. The acting card is the highlight: it follows the focused card, but stays on the last card while focus is on the sidebar or another non-card pane. All attached clients must share one view. With no attached client, a client on a tab without a sidebar, or clients on different tabs, the command is a no-op rather than a guess at whose selection to change.

An open help panel closes before the command acts. An unfinished search draft is cancelled; a committed search remains part of the lens.

`close --pane` is the exception: it names its row, so it acts through any attached client's ready view, clients may view different tabs, and it leaves help and the search draft as they are. `reopen` does not use the sidebar host at all, so it works with no attached client.

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

`last` focuses the previous card in this view. A jump between cards advances history immediately, so `last` returns to the card before that jump even while focus is still landing. Repeating it toggles between the two cards without waiting. Visiting the sidebar or a popup does not advance history. Empty history, or a previous card whose pane left the room, is a no-op. History lives in the sidebar host and is forgotten when the host restarts.

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
rimz sidebar reopen
```

`close` closes the highlighted row's pane immediately, without confirmation. With `--pane PANE_ID` it closes the top-level row whose pane that is instead, whatever is highlighted. For an agent row whose session has a bound pane, it uses the same stop path as [`rimz agents stop`](./agents.md#stop), stopping launched children before their parent. For any other row (an agent without a bound session, a shell, editor, TUI, or untracked agent), it closes the row's pane and whatever runs there. Only root sessions that `agents stop` would offer to resume, with a saved conversation, enter the reopen list; child sessions and process rows cannot be reopened this way.

`reopen` resumes the most recently sidebar-closed agent that is not already live. It runs the resume outside any pane, so the agent opens in a new focused tab, not its old tab. A team seat runs `rimz agents TEAM.ROLE --continue`, adding `-w NAME` when it ran in a managed worktree; a solo session runs `rimz agents resume @SESSION-ID`. The resume's own receipt stays off stdout. Each command tries one entry. A failed resume reports its error and drops only that entry, leaving older cards for the next `reopen`.

The room remembers at most 32 closed agent cards, in `lanes/closed-cards.json` under the room's runtime directory. A successful reopen consumes its entry and discards newer entries whose agents are already live again. An empty or all-live list is a no-op that keeps the list. The list is a convenience undo, not session history: room reset, teardown, and reboot discard it, and other ways of closing an agent add no entry. Use [`rimz agents resume`](./agents.md#resume-agents) to recover sessions independently of this list.

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

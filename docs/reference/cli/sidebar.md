# Sidebar cards

`rimz sidebar` navigates and acts on the cards the sidebar shows. These commands work from any pane, without moving focus into the column first. They add no default key bindings. For reading a frame, see [sidebar frames](./pane.md#render-a-sidebar-frame); for repairing the column, see [`sidebar repair`](./maintenance.md#repair-sidebars).

## Room and view

Every card verb accepts `--session-name S`. Without it, RimZ resolves the room from the current directory and its room pin, as other participant commands do. With it, the named room is used even from an unrelated directory. Use the global `--mux tmux` or `--mux zellij` when backend detection would choose the wrong multiplexer.

Walking, slots, `last`, filtering, and closing use the sidebar host's current view, including its selection, order and layout freeze. The acting card is the highlight: it follows the focused card, but stays on the last card while focus is on the sidebar or another non-card pane. All attached clients must share one view. No attached clients, clients looking at different views, or a tab without a sidebar produce a no-op rather than guessing whose selection to change.

An open help panel closes before the command acts. An unfinished search draft is cancelled; a committed search remains part of the lens.

## Walk cards or worktrees

```sh
rimz sidebar walk next
rimz sidebar walk prev --worktree
rimz sidebar walk next --session-name my-room --mux tmux
```

`walk next|prev` focuses the next or previous visible top-level row with a pane, wrapping at either end. Agent cards, shells, editors and other process rows participate. Child entries, pane-less rows and rows hidden behind a fold do not. A filter or committed search narrows the walk to its matches. Without a highlight, `next` starts at the first row and `prev` at the last.

`--worktree` moves to the first pane-backed row in the next or previous visible worktree group, wrapping and skipping groups the lens emptied. Each press moves selection immediately, so repeated commands advance even before the previous focus jump lands. The [layout freeze](../../guide/sidebar.md#the-layout-freeze) keeps the order steady while you walk.

## Jump by slot

```sh
rimz sidebar slot 3
```

Slots number every top-level pane-backed card in the room by pane birth, across groups, filters and folds. The first nine show a dim digit when space permits; the sidebar's `1`–`9` keys use the same numbers. `slot N` focuses that card's pane even when a filter or fold hides its card; the card stays hidden under that lens. Closing a card renumbers the later slots. Only `1` through `9` are accepted; a valid slot beyond the current card count is a no-op.

## Return to the previous card

```sh
rimz sidebar last
```

`last` focuses the previous card in this view. A jump between cards advances history immediately, so `last` returns to the card before that jump even while focus is still landing. Repeating it toggles between the two cards without waiting. Visiting the sidebar or a popup does not advance history. A removed previous card or empty history is a no-op. History lives in the sidebar host and is forgotten when it restarts.

## Show what needs you

```sh
rimz sidebar filter needs-you
rimz sidebar filter needs-you --toggle
```

Without `--toggle`, the command sets the lens; with it, it turns the lens on or off. It shows exactly the waiting and failed cards, including parents lifted by a waiting child or a provider-native child's failure. Paused cards and process rows are excluded. Unlike the make-up buckets, this lens selects individual cards rather than every teammate. The header reads `needs-you: N`.

The lens is room-shared, intersects a committed search, and clears automatically when its room-wide count reaches zero. Setting it when nothing matches is a no-op. `F` in the sidebar clears both the lens and the search.

## Close and reopen

```sh
rimz sidebar close
rimz sidebar close --pane tmux:%4
rimz sidebar reopen
```

`close` closes the highlighted row's pane immediately, without confirmation. With `--pane <PANE_ID>` it closes the card that owns that pane instead, whatever is highlighted and whichever tabs the attached clients view; a pane no card owns is refused with `sidebar close: no card owns pane <PANE_ID>`. The sidebar's `d d` runs this form for the selected card. For a session with a bound pane it uses the same stop path as `rimz agents stop`, stopping launched children before their parent. For an agent without a bound session, a shell, editor, TUI or untracked agent, it closes the row's pane and whatever runs there. Only resumable root sessions with a saved conversation enter the reopen list; child sessions and process rows cannot be reopened this way.

`reopen` resumes the most recently sidebar-closed agent that is not already live, in a new focused tab, not its old tab. A team seat uses `rimz agents TEAM.ROLE --continue`, with its saved worktree name when applicable; a solo session uses `rimz agents resume @SESSION-ID`. Resume receipts stay off stdout. Each command tries one entry. A failed resume reports its error and drops only that entry, leaving older cards for the next `reopen`.

The room remembers at most 32 closed agent cards. Successful reopen consumes its entry and discards newer entries whose agents are already live again. An empty or all-live list is a no-op that preserves the list. The list is a convenience undo, not session history: room reset, teardown and reboot discard it. Other ways of closing an agent do not add an entry. Use [`rimz agents resume`](./agents.md#resume-agents) to recover sessions independently of this list.

## Output and exit codes

These six verbs always keep stdout empty. Success and no-ops exit **0**; no-ops explain why on stderr, and refusals that reach the current sidebar view also show a fading notice there. A reattaching sidebar, a full input queue, or a sidebar too busy to reply is a no-op with a retry message, not a repair fault. These delivery refusals appear only on stderr. Missing rooms, an unavailable host (no listener, EOF or no wire reply before the client's timeout), or a failed stop or resume exit **1** with the error on stderr. Invalid arguments, including a slot outside `1..=9`, exit **2**. An unavailable host names `rimz sidebar repair` as the next step.

This matters for key-bound commands: tmux's `run-shell -b` displays stdout or a nonzero exit in `view-mode` over the user's pane. A harmless no-op must not open that overlay.

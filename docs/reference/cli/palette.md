# Palette

`rimz palette` searches the room's live cards and offers the commands you can run on them. The bottom line shows the exact `rimz` command Enter runs. Use it in the current terminal, or open it over your work:

```sh
rimz palette
rimz palette --popup
rimz palette --popup --mode commands
```

The popup is centred at 80% of the client's width and height on tmux and Zellij. Exiting closes it. This command installs no keybinding.

## Search and move

The first character of the query selects the mode. `--mode` only seeds that query; erasing its prefix returns to cards.

| Query | Mode | Enter |
| --- | --- | --- |
| no prefix | Cards | Focus the selected pane. |
| `>` | Commands | Run the selected command. |
| `#` | Worktrees | Focus the group's first live card. |
| `+` | New agent | Nothing yet. The placeholder says this arrives in a later release. |

Type to filter. Up/Down and Ctrl-p/Ctrl-n move the selection; Enter runs it. Esc and Ctrl-c leave without running anything, with exit status 0. There is no mouse interaction. The view supports terminals of at least 32 columns by 8 rows.

Card search matches subsequences of the handle, pet name, provider kind, worktree, branch, PR number, and description. The description is the agent's launch description, else its task, else its first prompt. Handle matches rank above description matches; equal matches keep the room snapshot's order. Each row shows its handle or program, provider kind, status, pet name, description, worktree, branch, and PR number when known. A child running in another lane than its parent shows that lane's worktree with its PR number, or the lane name alone when the room lists no worktree for it, beside the child's own branch. Only rows with a live pane appear, including pane-backed children and ordinary processes. Closed cards remain available through `rimz agents resume`, not this list.

The list refreshes about every two seconds, keeping the selected row by identity while it remains present. Worktree mode lists the snapshot's groups, including the root and external places, but omits groups without a live candidate. It does not change a shell's directory.

With nothing selected, the bottom line reads `nothing to run`. If no sidebar has published the room yet, cards are empty and a notice names `rimz sidebar repair`; the palette does not start a sidebar itself.

## Commands

Commands mode names its target above the list. Moving to a card in cards mode makes that card the target; otherwise the opener's card is the target. With no matching opener, the heading says `no card` and only room commands appear.

| Target | Commands | Result |
| --- | --- | --- |
| Agent card | `pane focus`, `agents restart`, `agents stop`, `agents compact` | Act on the selected agent and close on success. `agents compact` appears only for kinds it can compact. |
| Agent card | `agents show`, `agents logs -n 40`, `pane capture` | Show output. |
| Process | `pane focus` | Focus it and close on success. |
| Process | `pane capture` | Show output. |
| Room | `agents list`, `asks`, `teams list`, `worktree list`, `doctor`, `stats` | Show output. |
| Every live room | `sidebar repair` | Repair the sidebars of every live room, not only this one, and close on success. |

Every command starts with `rimz`. Card commands use the full `@handle#channel` address. When that would select another agent or none, they use `@<session id>`, and a process or an agent pane with no session yet uses its pane ID. A failed action keeps the palette open with the exit status and first stderr line. Output commands leave the palette view, run in the terminal, and wait for a key before closing. Each command retains its own refusals and checks.

## Room and popup targeting

Without `--session-name`, the command follows the current room identity, like other participant commands. `--session-name S` addresses that room explicitly; an unknown room refuses before any popup opens. `--from-pane PANE` supplies the opener when launching from outside its pane; otherwise the current pane is used when available.

`--popup` opens the same palette in the chosen backend (`--mux tmux` or `--mux zellij`). On tmux, `--client NAME` chooses the attached client and requires `--popup`. Without it, exactly one client must be attached; no clients names `rimz attach`, and several clients asks for `--client`. On Zellij, actions reach the last client to type; another client typing during an open or focus can redirect it. `--client` is tmux-only.

Opening on Zellij reveals the tab's floating layer, so existing hidden floats may appear and remain shown after close, and focus then lands on such a float rather than the pane that opened the palette. A second open is ignored while a floating pane titled `rimz palette` is present; a just-closed popup may briefly remain in the cached listing. On tmux, popup geometry is not recomputed when the terminal resizes.

Without a terminal, use `rimz palette --popup` instead of the in-terminal form.

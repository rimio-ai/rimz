# Palette

`rimz palette` searches the room's live cards, offers the commands you can run on them, and launches new agents. The bottom line shows the exact `rimz` command Enter runs. Use it in the current terminal, or open it over your work:

```sh
rimz palette
rimz palette --popup
rimz palette --popup --mode commands
rimz palette --popup --mode new --new-worktree
rimz palette --popup --repeat
```

The popup is centred over the room's panes on tmux and Zellij and sized per axis at 60% of the width and 50% of the height, at most 160 by 40 cells and at least 100 by 24, or the whole pane area when that is smaller. The size counts the border, so the palette's content is two cells smaller each way. The four [`[theme.display.popup]`](../../guide/theme.md#popup-size) settings change the target and the ceiling at the next open; a ceiling set below the floor wins. When the pane area cannot be read, the popup opens at the two percentages alone. Exiting closes it. This command installs no keybinding.

## Search and move

The first character of the query selects the mode. `--mode` only seeds that query; erasing its prefix returns to cards.

| Query | Mode | Enter |
| --- | --- | --- |
| no prefix | Cards | Focus the selected pane. |
| `>` | Commands | Run the selected command. |
| `#` | Worktrees | Focus the group's first live card. |
| `+` | New agent | Move to the next [launch step](#launch-an-agent); on the last, launch. |

Type to filter. Up/Down and Ctrl-p/Ctrl-n move the selection; Enter runs it. Esc and Ctrl-c leave without running anything, with exit status 0. There is no mouse interaction. The view supports terminals of at least 32 columns by 8 rows.

Card search matches subsequences of the handle, pet name, provider kind, worktree, branch, PR number, and description. The description is the agent's launch description, else its task, else its first prompt. Handle matches rank above description matches; equal matches keep the room snapshot's order. Each row shows its handle or program, provider kind, status, pet name, description, worktree, branch, and PR number when known. A child running in another lane than its parent shows that lane's worktree with its PR number, or the lane name alone when the room lists no worktree for it, beside the child's own branch. Only rows with a live pane appear, including pane-backed children and ordinary processes. Closed cards remain available through `rimz agents resume`, not this list.

The list refreshes about every two seconds, keeping the selected row by identity while it remains present. Worktree mode lists the snapshot's groups, including the root and external places, but omits groups without a live candidate. It does not change a shell's directory.

With nothing selected, the bottom line reads `nothing to run`. If no sidebar has published the room yet, cards are empty and a notice names `rimz sidebar repair`; the palette does not start a sidebar itself.

## Launch an agent

The `+` mode launches one agent in four steps: profile, where, name (only for a new worktree), and prompt. Enter moves to the next step; Enter on the prompt launches, and an empty prompt launches with none. Backspace on an empty field returns to the previous step, and on the profile step it erases the query as in every mode, so erasing the `+` returns to cards. Past the profile step, typed characters belong to the field: a `>` or `#` in a prompt is text.

- **Profile** lists the profiles `rimz agents profiles` lists, matched by subsequence against name, model, and description.
- **Where** lists the current checkout first, then the root checkout when it is not the current one, then the other RimZ-owned worktrees, then `new worktree from trunk`. The current checkout is that of the opener's card; an opener with no card, such as a plain shell, means the root checkout. An unmanaged checkout appears only when it is the current one. Worktree entries appear only when the room is a Git repository. Typing filters by name and branch.
- **Name** names the new worktree. An empty name generates one. A name that breaks the [worktree name rules](./agents.md#channel-worktree-and-placement), or names a worktree the where step lists, shows why and Enter does nothing.
- **Prompt** is optional free text, delivered as the launch's prompt.

The bottom line shows the command Enter will run at every step, built from the selections so far: `rimz agents launch <profile>`, then `-w NAME` for a RimZ-owned worktree, `--cwd PATH` for an unmanaged current checkout, nothing for the root checkout, or `-w [NAME] --from-trunk` for a new worktree, then the prompt as the last word. A new worktree is cut [as team launches cut theirs](./agents.md#channel-worktree-and-placement). The palette does not check a prompt; one the launch refuses fails with the launch's own message.

`--new-worktree` starts the `+` mode with `new worktree from trunk` selected in the where step, and the bottom line names it before that step is reached. It needs `--mode new`; with another mode it refuses with `--new-worktree needs --mode new` (exit 2).

Launches are offered only while the room's multiplexer session is live, read when the palette first enters the `+` mode, together with the profiles and worktrees; otherwise the profile step shows the refusal naming `rimz start` and `rimz attach`.

The launch leaves the palette view and runs in its terminal, so the launch's own questions (entering an existing worktree, a cross-repository worktree, project trust) are answered there. It runs without the palette's pane, so it opens a new tab and takes focus rather than replacing the popup. On success the palette closes; on failure the output stays until a key is pressed.

### Repeat the last launch

Each palette launch that exits 0 is recorded for the room; a failed launch keeps the previous record, and a shell `rimz agents launch` records nothing. `rimz palette --repeat` runs the recorded command again exactly, place and prompt included, without opening the palette view: it prints `$ <command>`, then runs it in the terminal as the `+` mode does, and on failure waits for a key and exits 1. With no readable record it exits 1 with `no palette launch to repeat in this room; start one with rimz palette --mode new`; with `--popup` that refusal comes before any popup opens. `--repeat` conflicts with `--mode` and `--new-worktree`. The record lives in the room's runtime state, so room reset, teardown, and reboot discard it.

## Commands

Commands mode names its target above the list. Moving to a card in cards mode makes that card the target; otherwise the opener's card is the target. With no matching opener, the heading says `no card` and only room commands appear.

| Target | Commands | Result |
| --- | --- | --- |
| Agent card | `pane focus`, `agents restart`, `agents stop`, `agents compact` | Act on the selected agent and close on success. `agents compact` appears only for kinds it can compact. |
| Agent card | `agents show`, `agents logs -n 40`, `pane capture` | Show output. |
| Process | `pane focus` | Focus it and close on success. |
| Process | `pane capture` | Show output. |
| Room | `agents list`, `asks`, `teams list`, `worktree list`, `doctor`, `stats` | Show output. |
| Room | `sidebar walk next`, `sidebar walk prev`, `sidebar slot 1` through `sidebar slot 9`, `sidebar last` | Navigate cards through the sidebar and close on success. |
| Every live room | `sidebar repair` | Repair the sidebars of every live room, not only this one, and close on success. |

Every command starts with `rimz`. Card-targeted commands use the full `@handle#channel` address. When that would select another agent or none, they use `@<session id>`, and a process or an agent pane with no session yet uses its pane ID. A failed action keeps the palette open with the exit status and first stderr line. Output commands leave the palette view, run in the terminal, and wait for a key before closing. Each command retains its own refusals and checks.

Sidebar navigation rows carry `--session-name` for the palette's room. They use the sidebar's highlight, not the palette's card target. Walk follows the sidebar's visible roster; slots use the room-wide birth numbers; `last` returns to the view's previous card. See [sidebar cards](./sidebar.md) for their view rules and no-op behaviour. Sidebar close, filter, and reopen are not palette rows.

Card read, unread, and alert marks are shell commands: [`rimz agents mark TARGET read|unread|alert`](./agents.md#mark). They are not palette actions.

## Room and popup targeting

Without `--session-name`, the command follows the current room identity, like other participant commands. `--session-name S` addresses that room explicitly; an unknown room refuses before any popup opens. `--from-pane PANE` supplies the opener when launching from outside its pane; otherwise the current pane is used when available.

`--popup` opens the same palette in the chosen backend (`--mux tmux` or `--mux zellij`). On tmux, `--client NAME` chooses the attached client and requires `--popup`. Without it, exactly one client must be attached; no clients names `rimz attach`, and several clients asks for `--client`. On Zellij, actions reach the last client to type; another client typing during an open or focus can redirect it. `--client` is tmux-only. The popup runs the palette with the outer command's `--mode`, `--new-worktree`, or `--repeat`.

Opening on Zellij reveals the tab's floating layer, so existing hidden floats may appear and remain shown after close, and focus then lands on such a float rather than the pane that opened the palette. A second open is ignored while a floating pane titled `rimz palette` is present; a just-closed popup may briefly remain in the cached listing. On tmux, popup geometry is not recomputed when the terminal resizes.

Without a terminal, use `rimz palette --popup` instead of the in-terminal form.

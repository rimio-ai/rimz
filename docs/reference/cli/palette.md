# Palette

`rimz palette` searches the room's live cards, offers the commands you can run on them, and launches new agents. The bottom line shows the exact `rimz` command Enter runs. Use it in the current terminal, or open it over your work:

```sh
rimz palette
rimz palette --popup
rimz palette --popup --mode commands
rimz palette --popup --mode new --new-worktree
rimz palette --popup --repeat
```

| Flag | Meaning |
| --- | --- |
| `--mode MODE` | Seed the query: `cards` (default, no prefix), `commands` (`>`), `worktrees` (`#`), or `new` (`+`). |
| `--new-worktree` | With `--mode new`, preselect `new worktree from trunk` in the [where step](#launch-an-agent). |
| `--repeat` | Run this room's [last palette launch](#repeat-the-last-launch) again, without the palette view. Conflicts with `--mode` and `--new-worktree`. |
| `--session-name S` | Address room `S` instead of the current room. |
| `--from-pane PANE` | The opener's pane, whose card [commands mode](#commands) targets until the selection moves. |
| `--popup` | Open in a centred multiplexer popup. |
| `--client NAME` | The attached tmux client to open the popup on. Requires `--popup`. tmux only. |

The global `--mux tmux|zellij` picks the backend.

The popup is centred over the room's panes on tmux and Zellij and sized per axis at 60% of the width and 50% of the height, at most 160 by 40 cells and at least 100 by 24, or the whole pane area when that is smaller. The size counts the border, so the palette's content is two cells smaller each way. The four [`[theme.display.popup]`](../../guide/theme.md#popup-size) settings change the target and the ceiling at the next open, and a ceiling set below the floor wins. When the pane area cannot be read, the popup opens at the two percentages alone. The popup closes when the palette exits.

The command installs no keybinding. The sidebar's default [room keymap](../../guide/configuration.md#room-keymap) opens the popup with `:` (commands), `o` (new agent), `O` (new agent in a new worktree), and `.` (repeat the last launch).

## Search and move

The first character of the query selects the mode. `--mode` only seeds that query: erasing its prefix returns to cards.

| Query | Mode | Enter |
| --- | --- | --- |
| no prefix | Cards | Focus the selected pane. |
| `>` | Commands | Run the selected command. |
| `#` | Worktrees | Focus the group's first live card. |
| `+` | New agent | Move to the next [launch step](#launch-an-agent), and on the last, launch. |

Type to filter and Backspace to erase. Up/Down and Ctrl-p/Ctrl-n move the selection, and Enter runs it. Esc and Ctrl-c leave without running anything. There is no mouse interaction. The view supports terminals of at least 32 columns by 8 rows.

Matching is a case-insensitive subsequence: the query's letters must appear in order, and letters that sit earlier in the text or next to each other rank higher. Card search matches the handle (or program), pet name, provider kind, worktree, branch, PR number, and description, in that order, and a match in an earlier field ranks above any match in a later one. Equal matches keep the room snapshot's order. The other modes match the query against the whole row.

A card's first line shows its handle or program, provider kind, and status. Its second line shows the pet name, description, worktree, branch, and `#PR` when known. The description is the agent's launch description, else its task, else its first prompt. A process shows its command and one of `idle`, `busy`, or `stuck`. A child running in another lane than its parent shows that lane's worktree with its PR number, or the lane name alone when the room lists no worktree for it, beside the child's own branch. Only rows with a live pane appear, including pane-backed children and ordinary processes. Closed cards remain available through [`rimz agents resume`](./agents.md#resume-agents), not this list.

The list refreshes about every two seconds and keeps the selected row while it remains present, including when a card moves to its session id as the agent registers. Worktrees mode lists one `#<worktree>` row per group that has a live card, including the root and external places. It does not change a shell's directory.

With nothing selected, the bottom line reads `nothing to run`. If no sidebar has published the room yet, the list is empty and the notice reads `` no sidebar has published this room yet; run `rimz sidebar repair` ``. The palette does not start a sidebar itself. A snapshot it cannot read shows `reading the room: <error>`.

## Launch an agent

The `+` mode launches one agent in four steps: profile, where, name (only for a new worktree), and prompt. Enter moves to the next step. Enter on the prompt launches, and an empty prompt launches with none. Backspace on an empty field returns to the previous step. On the profile step it erases the query as in every mode, so erasing the `+` returns to cards. Past the profile step, typed characters belong to the field: a `>` or `#` in a prompt is text.

- **Profile** lists the profiles `rimz agents profiles` lists, without team roles, matched against name, model, and description.
- **Where** lists the current checkout first, marked `current`, then the root checkout when it is not the current one, then the other RimZ-owned worktrees by name, then `new worktree from trunk`. The current checkout is that of the opener's card. An opener with no card, such as a plain shell, means the root checkout. An unmanaged checkout appears only when it is the current one. Worktree entries appear only when the room's root is a Git repository whose worktrees can be listed. Typing filters by name and branch.
- **Name** names the new worktree. An empty name generates one. A name that breaks the [worktree name rules](./agents.md#channel-worktree-and-placement), or names a worktree the where step lists (`` worktree `NAME` is listed; pick it in the where step ``), shows why, and Enter does nothing.
- **Prompt** is optional free text, delivered as the launch's prompt.

The bottom line shows the command Enter will run at every step, built from the selections so far: `rimz agents launch <profile>`, then `-w NAME` for a RimZ-owned worktree, `--cwd PATH` for an unmanaged current checkout, nothing for the root checkout, or `-w [NAME] --from-trunk` for a new worktree, then the prompt as the last word. A new worktree is cut [as team launches cut theirs](./agents.md#channel-worktree-and-placement). The palette does not check a prompt. A prompt the launch refuses fails with the launch's own message.

`--new-worktree` starts the `+` mode with `new worktree from trunk` selected in the where step, and the bottom line names it before that step is reached. It needs `--mode new`. With another mode it refuses with `--new-worktree needs --mode new` (exit 2).

The palette reads the profiles, the worktrees, and whether the room's multiplexer session is live when it first enters the `+` mode. Launches are offered only while that session is live. Otherwise the profile step shows the refusal, `` no live RimZ room `SESSION`; run `rimz start` first or enter one with `rimz attach` ``. A machine config that cannot load shows `loading machine config: <error>` there instead.

The launch leaves the palette view and runs in its terminal, so the launch's own questions (entering an existing worktree, a cross-repository worktree, project trust) are answered there. It runs without the palette's pane, so it opens a new tab and takes focus instead of replacing the popup. On success the palette closes. On failure it prints the error and `Press any key to close`, and closes after a key.

### Repeat the last launch

Each palette launch that exits 0 is recorded for the room. A failed launch keeps the previous record, and a shell `rimz agents launch` records nothing. `rimz palette --repeat` runs the recorded command again exactly, place and prompt included, without opening the palette view. It prints `$ <command>`, then runs it in the terminal as the `+` mode does. On failure it prints the error, waits for a key, and exits 1. With no readable record it exits 1 with `no palette launch to repeat in this room; start one with rimz palette --mode new`. With `--popup`, that refusal comes before any popup opens. The record lives in the room's runtime state, so room reset, teardown, and reboot discard it.

## Commands

Commands mode names its target on a `target:` line above the list. Moving the selection in cards mode makes that card the target. Otherwise, or once that card leaves the room, the opener's card is the target. With no matching opener, the line reads `target: no card` and only room commands appear.

| Target | Commands | Result |
| --- | --- | --- |
| Agent card | `pane focus`, `agents restart`, `agents stop`, `agents compact` | Act on the selected agent and close on success. `agents compact` appears only for kinds it can compact. |
| Agent card | `agents show`, `agents logs -n 40`, `pane capture` | Show output. |
| Process | `pane focus` | Focus it and close on success. |
| Process | `pane capture` | Show output. |
| Room | `agents list`, `asks`, `teams list`, `worktree list`, `doctor`, `stats` | Show output. |
| Room | `sidebar walk next`, `sidebar walk prev`, `sidebar slot 1` through `sidebar slot 9`, `sidebar last` | Navigate cards through the sidebar and close on success. |
| Every live room | `sidebar repair` | [Repair the sidebars](./maintenance.md#repair-sidebars) of every live room, not only this one, and close on success. |

Every command starts with `rimz` and runs from the room's project root, pinned to the palette's room. Card-targeted commands use the full `@handle#channel` address. When that would select another agent or none, they use `@<session id>`, and a process or an agent pane with no session yet uses its pane ID. Each command keeps its own refusals and checks.

An action runs with no input. When it fails, the palette stays open and shows its exit status and first stderr line, as in `exit status: 1: <first line>`. An output command leaves the palette view and runs in the terminal, then prints `Press any key to close` and closes after a key. When it fails, its exit status is printed before that prompt.

Sidebar navigation rows carry `--session-name` for the palette's room. They use the sidebar's highlight, not the palette's card target. Walk follows the sidebar's visible roster, slots use the room-wide birth numbers, and `last` returns to the view's previous card. See [sidebar cards](./sidebar.md) for their view rules and no-op behaviour. Sidebar close, filter, and reopen are not palette rows.

Card read, unread, and alert marks are shell commands: [`rimz agents mark TARGET read|unread|alert`](./agents.md#mark). They are not palette actions.

## Room and popup targeting

Without `--session-name`, the command follows the current room identity, like other participant commands. `--session-name S` addresses that room explicitly. An unknown room refuses with `` no room for session S: run `rimz start` there, or pass --root <room> `` before any popup opens. `--from-pane PANE` supplies the opener when launching from outside its pane. Otherwise the current pane is used when there is one.

`--popup` opens the same palette in the chosen backend, passing on the outer command's `--mode`, `--new-worktree`, or `--repeat`, the room, and the opener. On tmux, the popup is anchored to the opener's pane when there is one. Without `--client`, exactly one client must be attached: no client refuses with `` no client is attached to session S; attach with `rimz attach` ``, and several refuse with `several clients are attached to session S; pass --client <name>`. On Zellij, `--client` refuses with `--client is only supported with --mux tmux`. Zellij actions reach the last client to type, so another client typing during an open or focus can redirect it.

Opening on Zellij reveals the tab's floating layer, so existing hidden floats may appear and remain shown after close, and focus then lands on such a float instead of the pane that opened the palette. A second open is ignored while a floating pane titled `rimz palette` is present. A just-closed popup may briefly remain in the cached listing. On tmux, popup geometry is not recomputed when the terminal resizes.

Without a terminal on stdin and stdout, the in-terminal form refuses with `` rimz palette needs a terminal; run `rimz palette --popup` ``.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | The palette closed: after Esc or Ctrl-c, an action, an output command, or a launch, whether or not that command succeeded. `--repeat` exits 0 when the launch succeeds. On tmux, `--popup` exits 0 once the popup closes, whatever the palette inside exited with. On Zellij, it exits 0 once the floating pane opens, or at once when a palette popup is already open. |
| 1 | A refusal: no room, no terminal, a tmux client problem, `--client` on Zellij, or a multiplexer that refused the popup. `--repeat` with no record or with a failed launch. |
| 2 | Invalid arguments: an unknown `--mode`, `--client` without `--popup`, `--new-worktree` without `--mode new`, or `--repeat` with `--mode` or `--new-worktree`. |

# Tab

`rimz tab` shows the room's fixed views: the dashboard, and nine numbered terminals. A RimZ-style room binds them to keys (see the [room keymap](../../guide/configuration.md#room-keymap)); the commands work in any room when you run them yourself.

```sh
rimz tab dashboard
rimz tab terminal 3
rimz tab terminal 3 --session-name myroom
```

| Command | What it shows |
| --- | --- |
| `rimz tab dashboard` | The dashboard tab, `rimzd`. When you closed it, the command reopens it first, then shows it. |
| `rimz tab terminal N` | Terminal `N`, for `N` from 1 to 9: a tab named `term-N` holding a shell beside the sidebar. The first call opens it at the room's main checkout, whatever pane you are in; every later call shows the same tab, idle or running a command. |

| Flag | Meaning |
| --- | --- |
| `--session-name S` | Act on room `S`. Without it, the room comes from the current directory and the global `--root`. |

The global `--mux tmux|zellij` picks the backend. The commands never print to stdout.

## Terminals

A terminal's number is its tab name. RimZ records nothing else about it:

- Closing a terminal (exiting its shell, `card.close` on its sidebar row, or closing the pane) frees its number. The next `rimz tab terminal N` opens a new one, and no other terminal is renumbered.
- Renaming the tab gives up its number: the next call opens a new `term-N`. A tab you named `term-N` yourself counts as terminal `N`.
- An agent launched in place inside a terminal takes the tab, which turns into that agent's tab. The number frees, and the next call opens a fresh shell.
- The sidebar shows a terminal as an ordinary shell row, with no number.
- Terminals are not restored: after the room restarts, the first call opens each one again. `rimz doctor`, `rimz sidebar repair`, and resume treat a terminal tab as any other working tab.

`term-1` through `term-9` keep their names while they hold only a shell, where RimZ would otherwise rename a tab it opened to the shell's name. A status glyph still follows the name while an agent in the tab runs; a shell command running there adds none.

Calls that race for the same number (a held key) leave one tab: opening a terminal waits on the lock `rimz sidebar repair` holds.

## Outcomes

| Outcome | Exit | stderr |
| --- | --- | --- |
| The view is shown: found, opened, or reopened | 0 | nothing |
| Zellij room with no client focused on a terminal pane | 0 | `tab VERB: no client is attached to session S`; nothing opens |
| `N` outside 1 to 9, or missing | 2 | the usage error |
| The session is not a live RimZ room | 1 | the error, naming the session |
| Another repair held the session past 60 seconds | 1 | `another repair holds session S; press again` |
| The dashboard reopen or a multiplexer command failed | 1 | the error; a failed reopen names `rimz sidebar repair` |

On tmux, showing a tab moves the session's current window, so every client attached to the session follows, as every RimZ jump does. On Zellij the command passes no client, so with several clients attached, Zellij picks which one switches tabs.

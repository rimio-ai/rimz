# Room keymaps

With `[room] style = "rimz"`, RimZ binds a room keymap in the multiplexer: chords that work from any pane, a prefix layer, and sticky modes. `[keys] keymap` picks one of four bundled keymaps, `rimz`, `tmux`, `zellij` or `vim`, and `[keys.bind.<mode>]` tables override single keys on top of it. The [configuration guide](../guide/configuration.md#room-keymap) shows the settings; this page lists the vocabulary, the modes, and every bundled table. `rimz keys` prints the resolved keymap for your machine.

The in-pane `[keys.sidebar]` keys are separate and stay the same under every keymap ([sidebar keys](../guide/configuration.md#sidebar-keys)).

## Choosing a keymap

| keymap | default prefix | scrollback | home for |
| --- | --- | --- | --- |
| `rimz` | `Alt+p` | `vi` | the sidebar-first keymap |
| `tmux` | `Ctrl+b` | `native` | tmux's default prefix table |
| `zellij` | `Ctrl+g` | `native` | Zellij's unlock-first preset, with locked as home |
| `vim` | `Alt+w` | `vi` | vim's `CTRL-W` window table |

- `keymap` defaults to `rimz`. Any other name warns `keys.keymap: expected rimz, tmux, zellij, or vim; using the rimz keymap`, and `rimz config set keys.keymap` refuses it.
- `[keys] prefix` unset takes the keymap's default prefix. A set prefix wins under every keymap and moves every `<prefix>` entry in the tables below. An invalid prefix warns and uses the keymap's default.
- `vim` uses `Alt+w` rather than vim's `Ctrl+w` because a room prefix is captured in every pane: `Ctrl+w` is delete-word in agent input boxes and shells. `Alt+w` keeps the same letter table behind the same `w`. Set `prefix = "Ctrl+w"` to get the real chord; `Ctrl+w Ctrl+w` then cycles panes, and delete-word reaches a pane only through a free prefix-mode key such as `[keys.bind.prefix] W = "pane.send_prefix"`, so `Ctrl+w W` sends `Ctrl+w`. Binding `Ctrl+w` itself there does nothing, since the configured prefix applies last.

## Keys

A key is one ASCII graphic character, or one of `Left`, `Right`, `Up`, `Down`, `Esc`, `Enter`, optionally modified with one `Alt+` or `Ctrl+` (`meta`/`m` and `control`/`c` are aliases, and `-` may replace `+`). Case matters. Arrows display as written (`Alt+Left`).

- Every normal-mode key needs a modifier, so a bare `Left` in `[keys.bind.normal]` is refused.
- The prefix is `Alt` or `Ctrl` plus one ASCII graphic character. An arrow prefix is refused.
- Ctrl chords that alias Enter, Esc, Tab or Backspace (`Ctrl+m`, `Ctrl+[`, `Ctrl+i`, `Ctrl+h`) are refused in normal and prefix modes.

## Modes

| mode | entered by | sidebar label | after an action |
| --- | --- | --- | --- |
| `normal` | home; `mode.exit` | none | stays |
| `prefix` | `mode.prefix` | `PREFIX` | returns to normal |
| `pane` | `mode.pane` | `PANE` | a motion stays; any other action returns to normal |
| `tab` | `mode.tab` | `TAB` | a motion stays; any other action returns to normal |
| `resize` | `mode.resize` | `RESIZE` | a motion stays; any other action returns to normal |
| `move` | `mode.move` | `MOVE` | a motion stays; any other action returns to normal |

`mode.scrollback` enters the multiplexer's copy or scroll mode, labelled `SCROLL`. A mode action always switches to its mode. The motions are `pane.focus.*`, `pane.focus.next`, `pane.swap.*`, `pane.resize.*`, `tab.next`, `tab.prev`, `card.next`, `card.prev`, `worktree.next` and `worktree.prev`. An unbound key in prefix mode leaves the table on tmux and is swallowed; on Zellij it does nothing and the mode stays until `Esc`.

### Scrollback

- **`vi`**: on tmux RimZ sets global `mode-keys vi` and restores your prior value when the room keymap leaves it. On Zellij the scroll, search and enter-search modes get vi-like keys.
- **`native`**: on tmux `mode-keys` stays as you set it. On Zellij the scroll trio gets Zellij's stock keys, and every exit returns to locked: `j`/`Down` and `k`/`Up` scroll a line, `Ctrl f`/`PageDown`/`Right`/`l` and `Ctrl b`/`PageUp`/`Left`/`h` a page, `d`/`u` half a page, `e` edits the scrollback, `s` starts a search, and `Ctrl c`/`Esc`/`Enter` leave. In search, `n`/`p` find down and up, and `c`/`w`/`o` toggle case sensitivity, wrap and whole word.

## Actions

A binding value is an action name, `"none"` (removes the bundled key), or `{ run = "rimz ..." }`.

| action | tmux | Zellij |
| --- | --- | --- |
| `pane.focus.left` / `right` / `up` / `down` | `select-pane -L` / `-R` / `-U` / `-D` | `MoveFocus` |
| `pane.focus.next` | `select-pane -t :.+` | `FocusNextPane` |
| `pane.swap.left` / `right` / `up` / `down` | `swap-pane` with the neighbour in that direction | `MovePane` |
| `pane.split.right` / `down` | `split-window -h` / `-v` in the pane's directory | `NewPane Right` / `Down` |
| `pane.resize.left` / `right` / `up` / `down` | `resize-pane -L` / `-R` / `-U` / `-D` | `Resize Increase <direction>` |
| `pane.zoom` | `rimz pane zoom` | `rimz pane zoom` |
| `pane.close` | `kill-pane` | `CloseFocus` |
| `pane.send_prefix` | `send-keys` of the prefix | `Write` of the prefix's bytes |
| `tab.new` | `new-window` in the pane's directory | `NewTab` |
| `tab.next` / `tab.prev` | `next-window` / `previous-window` | `GoToNextTab` / `GoToPreviousTab` |
| `mode.prefix` / `pane` / `tab` / `resize` / `move` | `switch-client -T rimz-<mode>` | `SwitchToMode` |
| `mode.scrollback` | `copy-mode` | scroll mode |
| `mode.exit` | back to the root table | back to locked |
| `session.detach` | `detach-client` | `Detach` |
| `keys.help` | `rimz keys --wait` in a popup | `rimz keys --wait` in a floating pane |
| `palette.cards` / `commands` / `new_agent` | the [palette](./cli/palette.md) in that mode | the palette in that mode |
| `card.next` / `card.prev` | `rimz sidebar walk next` / `prev` | same |
| `worktree.next` / `worktree.prev` | `rimz sidebar walk next` / `prev --worktree` | same |
| `card.slot.1` .. `card.slot.9` | `rimz sidebar slot N` | same |
| `card.last` | `rimz sidebar last` | same |
| `filter.needs_you` | `rimz sidebar filter needs-you --toggle` | same |
| `card.close` / `card.reopen` | `rimz sidebar close` / `reopen` | same |
| `sidebar.focus` | `rimz sidebar focus --toggle` | same |

`card.close` closes the highlighted card's pane, which a reopen can bring back ([close and reopen](./cli/sidebar.md)). `pane.send_prefix` sends the resolved prefix chord: `Ctrl+<c>` is one byte, the letter's code AND `0x1f` (`Ctrl+b` is byte 2), and `Alt+<c>` is `27` followed by the character (`Alt+w` is `27 119`).

## The card layer

The normal mode of `tmux`, `zellij` and `vim`:

| key | action |
| --- | --- |
| `Alt+j` / `Alt+k` | `card.next` / `card.prev` |
| `Alt+J` / `Alt+K` | `worktree.next` / `worktree.prev` |
| `Alt+?` | `filter.needs_you` |
| `Alt+n` | `palette.new_agent` |
| `Alt+1` .. `Alt+9` | `card.slot.1` .. `card.slot.9` |
| `Alt+;` | `card.last` |
| prefix | `mode.prefix` |

`zellij` adds `Alt+h` `pane.focus.left` and `Alt+l` `pane.focus.right`, Zellij's own stock chords. None of the three binds `Alt+t`, so an agent's own `Alt+t` reaches it.

## `rimz`

Prefix `Alt+p`, scrollback `vi`. Its pane and tab modes are empty.

| mode | key | action |
| --- | --- | --- |
| normal | the card layer, plus `Alt+h` / `Alt+l` | `pane.focus.left` / `right` |
| normal | `Alt+t` | `tab.new` |
| prefix | `p`, prefix | `palette.commands` |
| prefix | `/` | `palette.cards` |
| prefix | `z` | `pane.zoom` |
| prefix | `s` / `v` | `pane.split.down` / `pane.split.right` |
| prefix | `h j k l` | `pane.focus.*` |
| prefix | `H J K L` | `pane.swap.*` |
| prefix | `r` | `mode.resize` |
| prefix | `[` | `mode.scrollback` |
| prefix | `,` / `.` | `tab.prev` / `tab.next` |
| prefix | `x` / `X` / `u` | `card.close` / `pane.close` / `card.reopen` |
| prefix | `d` | `session.detach` |
| prefix | `?` | `keys.help` |
| prefix | `Esc` | `mode.exit` |
| resize | `h j k l` | `pane.resize.*` |
| move | `h j k l` | `pane.swap.*` |
| resize, move | `Esc`, `Enter` | `mode.exit` |

## `tmux`

Prefix `Ctrl+b`, scrollback `native`. Its pane and tab modes are empty.

| prefix key | action | tmux default |
| --- | --- | --- |
| prefix | `pane.send_prefix` | send-prefix |
| `"` / `%` | `pane.split.down` / `pane.split.right` | split-window |
| `c` | `tab.new` | new-window |
| `n` / `p` | `tab.next` / `tab.prev` | next-window, previous-window |
| `l`, `;` | `card.last` | last-window, last-pane |
| `1` .. `9` | `card.slot.N` | select-window |
| `o` | `pane.focus.next` | select-pane -t :.+ |
| `Left` `Down` `Up` `Right` | `pane.focus.*` | select-pane |
| `{` / `}` | `pane.swap.left` / `pane.swap.right` | swap-pane (tmux swaps by index, RimZ by direction) |
| `z` | `pane.zoom` | resize-pane -Z |
| `[` | `mode.scrollback` | copy-mode |
| `x` | `card.close` | kill-pane |
| `X` | `pane.close` | |
| `u` | `card.reopen` | |
| `d` | `session.detach` | detach-client |
| `?` | `keys.help` | list-keys |
| `:` | `palette.commands` | command-prompt |
| `w`, `f` | `palette.cards` | choose-tree, find-window |
| `j` / `k` | `card.next` / `card.prev` | |
| `J` / `K` | `worktree.next` / `worktree.prev` | |
| `N` | `palette.new_agent` | |
| `r` | `mode.resize` | refresh-client |
| `Esc` | `mode.exit` | |

- resize: `h j k l` and arrows `pane.resize.*`; `Esc`, `Enter` `mode.exit`.
- move: `h j k l` and arrows `pane.swap.*`; `Esc`, `Enter` `mode.exit`.

Not bound: `&`, `!`, `Space`, `,`, `$`, `.`, `'`, `(` and `)`, `0`, `]` and the other buffer keys, `q`, `t`, `i`, marks, rotate, `C-z`, layouts, the repeatable `M-` and `C-` arrow resizes, and menus. Of the vocabulary, `mode.pane`, `mode.tab`, `mode.move`, `filter.needs_you` (on `Alt+?` only) and `sidebar.focus` have no prefix key.

### Your tmux prefix

tmux checks the session `prefix` and `prefix2` options before any key table, so a tmux prefix equal to a key RimZ binds in normal mode would never reach RimZ. When the global `prefix` or `prefix2` equals such a key, RimZ records the value once in the server option `@rimz_prefix` or `@rimz_prefix2` and sets the option to `None`. When a later apply no longer binds that key (native style, another keymap or another prefix), RimZ restores the recorded value and removes the marker. A tmux prefix that collides with nothing is left alone. Only a global prefix (`set -g prefix`) is checked; a session-local prefix keeps winning over RimZ's key.

## `zellij`

Prefix `Ctrl+g`, scrollback `native`. Locked is home, as in Zellij's unlock-first preset.

| mode | key | action |
| --- | --- | --- |
| prefix | prefix, `Esc`, `Enter` | `mode.exit` |
| prefix | `p` / `t` | `mode.pane` / `mode.tab` |
| prefix | `r` / `m` / `s` | `mode.resize` / `mode.move` / `mode.scrollback` |
| prefix | `d` | `session.detach` |
| prefix | `/` / `:` | `palette.cards` / `palette.commands` |
| prefix | `x` / `u` | `card.close` / `card.reopen` |
| prefix | `?` | `keys.help` |
| pane | `h j k l`, arrows | `pane.focus.*` (stays) |
| pane | `p` | `pane.focus.next` (stays) |
| pane | `n`, `r` / `d` | `pane.split.right` / `pane.split.down` |
| pane | `x` / `X` | `card.close` / `pane.close` |
| pane | `f` | `pane.zoom` |
| tab | `h`, `k`, `Left`, `Up` | `tab.prev` (stays) |
| tab | `l`, `j`, `Right`, `Down` | `tab.next` (stays) |
| tab | `n` | `tab.new` |
| tab | `1` .. `9` | `card.slot.N` |
| resize | `h j k l`, arrows | `pane.resize.*` (stays) |
| move | `h j k l`, arrows | `pane.swap.*` (stays) |
| pane, tab, resize, move | `Esc`, `Enter` | `mode.exit` |

Not bound: tab close, rename, sync and break; floating, embed, pin and frame toggles; swap layouts; `Alt+i`, `Alt+o`, `Alt+f`, `Alt+[`, `Alt+]`, `Alt+=`, `Alt+-`; and the session-manager plugins. Of the vocabulary, `pane.send_prefix` and `sidebar.focus` are unbound.

## `vim`

Prefix `Alt+w`, scrollback `vi`. Its pane and tab modes are empty.

| prefix key | action | vim |
| --- | --- | --- |
| prefix, `w` | `pane.focus.next` | `CTRL-W CTRL-W`, `CTRL-W w` |
| `h j k l`, arrows | `pane.focus.*` | `CTRL-W h/j/k/l` |
| `p` | `card.last` | `CTRL-W p` |
| `H J K L` | `pane.swap.*` | `CTRL-W H/J/K/L` |
| `s`, `n` / `v` | `pane.split.down` / `pane.split.right` | `CTRL-W s`, `n` / `v` |
| `c`, `q` | `card.close` | `CTRL-W c`, `q` |
| `o` | `pane.zoom` | `CTRL-W o` |
| `t` / `T` | `tab.next` / `tab.prev` | `gt` / `gT` |
| `e` | `tab.new` | `:tabedit` |
| `N` | `mode.scrollback` | `CTRL-W N` in a terminal |
| `:` / `/` | `palette.commands` / `palette.cards` | command line, search |
| `r` | `mode.resize` | |
| `u` | `card.reopen` | |
| `d` | `session.detach` | |
| `?` | `keys.help` | |
| `Esc` | `mode.exit` | |

- resize: `h j k l` and arrows `pane.resize.*`; `<` left, `>` right, `+` down, `-` up; `Esc`, `Enter` `mode.exit`.
- move: `h j k l` and arrows `pane.swap.*`; `Esc`, `Enter` `mode.exit`.

Not bound: vim's `CTRL-W W`, `x` (exchange), `r` (rotate), `T` (move to a new tab), `=`, `_` and `|`, and the `CTRL-W CTRL-<letter>` variants. Here `r` and `T` carry the RimZ actions in the table above. Of the vocabulary, `pane.close`, `pane.send_prefix`, `mode.pane`, `mode.tab`, `mode.move` and `sidebar.focus` are unbound.

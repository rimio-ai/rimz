# Zellij and tmux

RimZ runs your agents inside Zellij or tmux, the terminal multiplexer you may already use. If you already live in one, that sentence is the whole integration: your keybinds, your theme, and every session you run outside RimZ stay exactly as they were. If you have barely touched a multiplexer, this page sets you up with a comfortable one and explains what it is doing under RimZ.

## Why a multiplexer is under RimZ

A coding agent is long-running. It thinks for minutes, works for hours, and you want to close the laptop and pick the run back up from another machine. A plain terminal tab cannot do that: quit the terminal and the agent dies with it. A multiplexer is the tool that already solved this, decades before agents existed. Every agent in a RimZ room needs what it provides:

- a **persistent session** that outlives the terminal window, so you detach, walk away, and reattach from anywhere,
- a **pseudo-terminal per pane**, so each agent's TUI runs in a real terminal with full color, resize, and key handling,
- **panes, tabs, and windows** to arrange a whole fleet on one screen.

RimZ needs all three, so rather than reimplement a terminal, a session manager, and a window system, it drives the one you already run. Some tools in this space ship their own terminal UI and reinvent that stack; RimZ builds on Zellij and tmux instead. The agents keep running their stock CLIs in ordinary panes, detach and reattach and scrollback and copy-mode stay exactly as your multiplexer already does them, and nothing about your terminal becomes RimZ-specific.

The one catch is age. Multiplexers predate coding agents by decades, and their defaults were tuned for shells and editors, not for a TUI that streams tokens, wants Shift+Enter for a soft newline, and expects 24-bit color and a deep scrollback. So RimZ asserts a short list of room settings agents need, and the baseline configs below make the rest of day-to-day work pleasant.

## What RimZ changes, and what stays yours

RimZ's room is session-scoped. On every session birth and reattach it applies the settings agents need (locked mode and single-click sidebar jumps on Zellij; mouse, focus events, notification passthrough, soft-newline keys, and clipboard on tmux; a deep scrollback on both) and stops there. It does not edit your `~/.config/zellij/config.kdl` or `~/.tmux.conf`. Your theme, keybinds, copy-mode, and status bar are yours, inside the room and in every session you run outside it. The per-setting detail is in [set up your machine](./setup.md#configure-your-multiplexer); the exact keys and defaults are in [configuration](./configuration.md#multiplexer-room-options).

One thing more happens on Zellij: RimZ seeds a permission grant for the presence plugin it ships, so the first attach is not interrupted by Zellij's plugin prompt. Your `config.kdl` stays untouched and the grant is yours to revoke; the full boundary is in [security and trust](./security.md#the-zellij-presence-plugin).

`rimz config init --print` lists every room option RimZ applies, with its default. Nothing outside the room changes, so undoing RimZ is closing the room.

### Room overrides in RimZ config

The `[zellij]` and `[tmux]` tables in `~/.config/rimz/config.toml` tune the room-scoped settings, and `[mux] default` picks the backend when both are installed:

```toml
[mux]
default = "zellij"              # unset resolves to tmux when both are installed

[zellij]
pane_frames = true              # optional override; unset, your config.kdl wins

[tmux]
# pane_border_status = "top"    # optional override; unset, your ~/.tmux.conf wins
# pane_border_lines = "heavy"
```

An optional key left unset falls through to your own Zellij or tmux config. A key you set here wins inside the room, because RimZ reasserts room options on every attach. Setting `[tmux] pane_border_status` makes RimZ own `pane-border-format` too, so it titles work panes and blanks the sidebar's border row; unset, your `~/.tmux.conf` format applies and may title the sidebar. The full model is in [configuration → Multiplexer room options](./configuration.md#multiplexer-room-options).

## Adopt the baseline

You need none of the configuration on this page: a freshly installed multiplexer works because the room sets its own options. The baselines are for your comfort and for the sessions you run outside RimZ. The fastest good config is the one RimZ ships under [examples/](../../examples/README.md), Zellij as a complete starting file and tmux as sourceable modules:

```sh
# From the rimz checkout
cp examples/zellij/config.kdl ~/.config/zellij/config.kdl        # Zellij: the whole baseline, one file
printf 'source-file %s\n' "$PWD"/examples/tmux/{agents,quality-of-life,zellij-keys,theme-tokyonight}.conf >> ~/.tmux.conf
```

Copy the whole thing if you are starting fresh; lift the blocks you want if you already have a config. The rest of this page walks through what each block does and why.

## Layouts and messages are ordinary pane operations

Everything RimZ does inside the room reduces to multiplexer commands you could run by hand, which is worth knowing before you trust it with your fleet.

An agent is a plain process in an ordinary pane. A layout spec is pane splits: `rimz agents claude,codex` asks the multiplexer to split two panes side by side, then launches a CLI in each, exactly the splits you would make yourself, in one command instead of several. The grammar (commas split columns, plus signs tile rows, slashes stack rows) is in [agents → compose a layout](./fleet.md#compose-a-layout).

`rimz message @coder "rebase first"` types that text into the agent's pane through the multiplexer's own send primitive, the same bytes as if you typed them at the keyboard, and `rimz pane capture @coder` reads the pane's visible text the same way. The sidebar is one more pane. You could list, focus, and drive every pane in the room with `zellij action` or `tmux` directly, and RimZ uses exactly those primitives. How messages route and when they land is in [messaging](./messaging.md).

## Zellij

The file is `~/.config/zellij/config.kdl`. `zellij setup --dump-config` prints the full default set, and `zellij setup --check` validates your edits. Every key here is catalogued in the [Zellij upstream reference](../externals/mux-adapter/zellij-reference.md#configuration).

[examples/zellij/config.kdl](../../examples/zellij/config.kdl) is the whole baseline, the [essentials](./setup.md#zellij) plus every block below and the `tokyo-night` theme, as one file, since Zellij reads a single config. Starting fresh, copy it; with an existing `config.kdl`, lift the blocks you want. Unlisted keys keep Zellij's defaults either way.

Clipboard travels over OSC52 by default, so yanking works through SSH to your local clipboard. A terminal that needs a helper can set one explicitly:

```kdl
// copy_command "wl-copy"      // Wayland;  "xclip -selection clipboard" on X11;  "pbcopy" on macOS
```

### Recommended

These tune the look and feel. None are required; each makes day-to-day work nicer.

```kdl
theme "tokyo-night"                    // any bundled or custom theme; matches RimZ's default sidebar
pane_frames true                       // titled borders mark the focused pane
styled_underlines true                 // colored/curly underlines from agents and editors
osc8_hyperlinks true                   // clickable links in command output
show_startup_tips false                // skip the startup tip banner
show_release_notes false               // skip the release-notes pane on upgrade
mouse_hover_effects false              // no hover frames or help text; calmer with many panes
session_serialization false            // prefer clean session births over held resurrection panes
// default_shell "zsh"                 // uncomment to pin a shell; unset uses $SHELL
```

`pane_frames true` draws a titled border around each pane so you can always see which one holds focus, the single most useful upgrade for a multi-agent layout. RimZ enforces its room's own mouse behavior, so your personal `focus_follows_mouse` and `mouse_click_through` settings no longer break single-click sidebar jumps.

Inside a RimZ room, opening a new pane splits the focused pane along its longer visual edge, and closing that pane returns the space to its split sibling.

### Alt chords in locked mode

RimZ opens its room in Zellij's locked mode, which hands every keystroke straight to the focused pane, so an agent's TUI gets your typing until you press `Ctrl+g` for a Zellij mode. That is what you want almost always, and it puts Zellij's own shortcuts one extra keypress away. A `locked` keybinds block keeps the chords you use constantly reachable while everything else still flows to the agent; the block merges with Zellij's defaults, so nothing else changes.

```kdl
keybinds {
    locked {
        // Tabs.
        bind "Alt t" { NewTab; }
        bind "Alt x" { CloseTab; }
        bind "Alt ," { GoToPreviousTab; }
        bind "Alt ." { GoToNextTab; }
        bind "Alt 1" { GoToTab 1; }     // Alt-1 through Alt-5 jump straight to a tab
        bind "Alt 2" { GoToTab 2; }
        bind "Alt 3" { GoToTab 3; }
        bind "Alt 4" { GoToTab 4; }
        bind "Alt 5" { GoToTab 5; }
        // Panes.
        bind "Alt n" { NewPane; }
        bind "Alt h" { MoveFocus "left"; }
        bind "Alt j" { MoveFocus "down"; }
        bind "Alt k" { MoveFocus "up"; }
        bind "Alt l" { MoveFocus "right"; }
        bind "Alt f" { ToggleFloatingPanes; }
        // Session.
        bind "Alt d" { Detach; }
    }
}
```

A chord bound here no longer reaches the app in the pane, so it shadows zsh's `Alt` keys (`Alt-f` forward-word, `Alt-.` last-arg) and any agent-TUI `Alt` binding. Trim the set to the chords you actually use. The [tmux parity block below](#zellij-parity-alt-chords) mirrors these same chords, so one set of muscle memory drives both multiplexers.

### A note on resurrection

RimZ disables Zellij session serialization inside its room, because it owns rebuilding a room after a reboot or crash: it re-seeds the prior agents itself ([resume on rebirth](../internals/sidebar/sidebar.md#resume-on-rebirth)) rather than resurrecting a wall of suspended command panes that come back dead. Setting `session_serialization false` in your own `config.kdl` gives non-RimZ sessions the same clean-birth posture when you prefer running panes over resurrection.

RimZ also disables Zellij's session metadata loop inside its room. At roughly 100 panes on Zellij 0.44.3 that loop rewrites `session-metadata.kdl` every few seconds and runs process discovery through `ps`, a visible share of the Zellij server CPU. RimZ starts and attaches rooms with `disable_session_metadata true` so that work stays out of the room.

## tmux

The file is `~/.tmux.conf` (or `~/.config/tmux/tmux.conf`); reload it with `tmux source-file ~/.tmux.conf` or the `prefix` + `r` binding below. Every option here is catalogued in the [tmux upstream reference](../externals/mux-adapter/tmux-reference.md#options).

tmux is the spartan one out of the box: no mouse, a short scrollback, and thin, untitled pane borders. The blocks below ship as four self-contained modules under [examples/tmux/](../../examples/README.md#tmux--tmux), [`agents.conf`](../../examples/tmux/agents.conf) (the [essentials](./setup.md#tmux)), [`quality-of-life.conf`](../../examples/tmux/quality-of-life.conf) (copy-mode, window names, splits), [`zellij-keys.conf`](../../examples/tmux/zellij-keys.conf) (the parity chords), and [`theme-tokyonight.conf`](../../examples/tmux/theme-tokyonight.conf) (frames and status bar), so your `~/.tmux.conf` stays yours and adopts by reference:

```sh
# From the rimz checkout: source the modules you want; drop any line.
printf 'source-file %s\n' "$PWD"/examples/tmux/{agents,quality-of-life,zellij-keys,theme-tokyonight}.conf >> ~/.tmux.conf
tmux source-file ~/.tmux.conf
```

### Recommended

These tune copy-mode, window behavior, and splits. None are required; each makes day-to-day work nicer.

```tmux
# Copy-mode: vi keys, mouse-drag yanks and exits, gentler scroll.
setw -g mode-keys vi
bind -T copy-mode-vi v send -X begin-selection
bind -T copy-mode-vi y send -X copy-pipe-no-clear
bind -T copy-mode-vi MouseDragEnd1Pane send -X copy-pipe-and-cancel
# Anchor the selection the instant a drag opens copy-mode, so the first drag
# copies instead of stranding a highlight. Keep tmux's guard that forwards the
# drag to a mouse-aware app or an already-open mode.
bind -T root MouseDrag1Pane if -F "#{||:#{pane_in_mode},#{mouse_any_flag}}" { send -M } { copy-mode -M ; send -X begin-selection }
bind -T copy-mode-vi WheelUpPane   send -X -N 3 scroll-up
bind -T copy-mode-vi WheelDownPane send -X -N 3 scroll-down

# Windows: compact numbering, stable names, panes resize across clients.
set -g  renumber-windows on
setw -g aggressive-resize on
setw -g automatic-rename off     # keep windows at the name they were given
setw -g allow-rename off         # ignore app title escapes renaming them

# Reload, and splits that keep the current directory and follow the pane's
# longer visual edge (the split behavior RimZ rooms give Zellij).
bind r source-file ~/.tmux.conf \; display "tmux.conf reloaded"
set -g @smart_split_is_wide '#{?#{&&:#{window_cell_width},#{window_cell_height}},#{e|>=:#{e|*:#{pane_width},#{window_cell_width}},#{e|*:#{pane_height},#{window_cell_height}}},#{e|>=:#{pane_width},#{pane_height}}}'
bind | if -F '#{E:@smart_split_is_wide}' 'split-window -h -c "#{pane_current_path}"' 'split-window -v -c "#{pane_current_path}"'
bind - if -F '#{E:@smart_split_is_wide}' 'split-window -h -c "#{pane_current_path}"' 'split-window -v -c "#{pane_current_path}"'
bind c new-window -c "#{pane_current_path}"
```

The root `MouseDrag1Pane` override opens copy-mode and begins the selection in one step. tmux's default opens copy-mode with `copy-mode -M` alone, which can leave the first drag unanchored: the highlight appears but the release copies nothing, so you press `q` and drag again to get it. Beginning the selection on entry makes the first drag copy and exit like the rest. The `pane_in_mode`/`mouse_any_flag` guard keeps the defaults intact, so a drag inside a mouse-aware TUI or an already-open copy-mode still forwards to that app.

`automatic-rename off` with `allow-rename off` keeps window names where they were set: RimZ titles agent windows when it opens them, and your own windows keep the name you give them instead of tracking the foreground command. The `@smart_split_is_wide` splits mirror RimZ's Zellij rooms, so a wide pane splits left and right, a tall pane splits top and bottom, comparing pixel dimensions when tmux knows the terminal cell size.

### Zellij-parity Alt chords

If you move between tmux and Zellij, mirroring the [locked-mode `Alt` chords above](#alt-chords-in-locked-mode) lets the same keys drive both. These are no-prefix bindings, so they shadow zsh's `Alt` keys, the same trade the Zellij block already makes.

```tmux
# Tabs == tmux windows.
bind -n M-t new-window -c "#{pane_current_path}"    # new tab
bind -n M-x kill-window                             # close tab
bind -n M-, previous-window
bind -n M-. next-window
bind -n M-i swap-window -d -t -1                    # move tab left
bind -n M-o swap-window -d -t +1                    # move tab right
bind -n M-1 select-window -t 1                      # Alt-1 … Alt-9 jump straight to a tab
# … M-2 through M-9 in the shipped module

# Panes.
bind -n M-n if -F '#{E:@smart_split_is_wide}' 'split-window -h -c "#{pane_current_path}"' 'split-window -v -c "#{pane_current_path}"'
bind -n M-h select-pane -L
bind -n M-j select-pane -D
bind -n M-k select-pane -U
bind -n M-l select-pane -R
# Zellij's floating pane, approximated as a centered popup. tmux 3.7 adds native
# floating panes (new-pane); the popup toggles cleanly and works on the 3.5 floor.
bind -n M-f display-popup -C \; display-popup -E -w 50% -h 50% -t "#{client_session}:#{active_window_index}" -d "#{pane_current_path}"
# Toggle pane frames, like Zellij's pane-mode z.
bind -n M-z if -F '#{!=:#{pane-border-status},off}' 'setw pane-border-status off' 'setw pane-border-status top'

# Resize with Alt+arrows (tmux has no spatial pane move).
bind -n M-Left  resize-pane -L 3
bind -n M-Right resize-pane -R 3
bind -n M-Up    resize-pane -U 2
bind -n M-Down  resize-pane -D 2

# Session.
bind -n M-d detach-client
```

`Alt+[` and `Alt+]` (Zellij's other tab-cycle pair) are left out on purpose: `Alt+[` sends `ESC [`, which collides with CSI escape sequences in some terminals.

### Titled pane borders and a themed status bar

tmux draws thin, untitled pane borders and a plain status bar by default. This module gives panes titled frames, the closest tmux gets to Zellij's, and styles the status bar as Powerline tabs in TokyoNight Night, the palette of RimZ's default sidebar scheme, so the whole room reads as one surface. It assumes the outer terminal uses a Nerd Font or another Powerline-capable face. The full palette is [theme-tokyonight.conf](../../examples/tmux/theme-tokyonight.conf); the pieces that carry it:

```tmux
# Titled pane borders: the tmux analog of Zellij's pane frames.
set -g pane-border-status top
set -g pane-border-lines heavy               # solid frame lines instead of thin ACS
set -g pane-border-format " #{pane_index} #{pane_current_command} "
set -g pane-border-style "fg=#414868"
set -g pane-active-border-style "fg=#7aa2f7"

# Status bar: blue session block, green focused tab, dim inactive tabs.
set -g status-style "bg=#1a1b26,fg=#c0caf5"
set -g status-left  "#[fg=#1a1b26,bg=#7aa2f7,bold] #S "
set -g window-status-separator ""
```

`pane-border-status top` labels each pane's border with its index and running command, so a grid of agents stays legible at a glance. RimZ inherits this setting when its [`[tmux] pane_border_status`](./configuration.md#multiplexer-room-options) override is unset; set that override and RimZ titles work panes and blanks the sidebar's own border row. tmux does not draw a pane's outer window edge, so panes are not fully boxed like Zellij frames.

Swap the hex values for your own scheme's palette; `rimz list-themes` names every scheme RimZ bundles if you want the sidebar and status bar to match.

## See also

- [Set up your machine → configure your multiplexer](./setup.md#configure-your-multiplexer): the short list of settings the room applies, one by one.
- [Configuration → multiplexer room options](./configuration.md#multiplexer-room-options): every `[zellij]` and `[tmux]` key with its default.
- [examples/](../../examples/README.md): the shipped baseline files these blocks come from.
- [Security and trust → the Zellij presence plugin](./security.md#the-zellij-presence-plugin): the one permission grant RimZ seeds, and how to revoke it.
- [Zellij](../externals/mux-adapter/zellij-reference.md) and [tmux](../externals/mux-adapter/tmux-reference.md) upstream references: every option catalogued at the source.

# Zellij and tmux baselines

A multiplexer config worth keeping: modern defaults, agent-friendly behavior, and one set of muscle memory across Zellij and tmux. Everything on this page ships ready to copy under [examples/](../../examples/README.md) — tmux as sourceable modules, Zellij as a complete starting file:

```sh
# From the rimz checkout
cp examples/zellij/config.kdl ~/.config/zellij/config.kdl        # Zellij: the whole baseline, one file
printf 'source-file %s\n' "$PWD"/examples/tmux/{agents,quality-of-life,zellij-keys,theme-tokyonight}.conf >> ~/.tmux.conf
```

Rimz itself needs none of this: every room sets its own options on session start and reattach, and the handful of settings that make agent sessions behave correctly everywhere — the essentials — are in [set up your machine → Configure your multiplexer](./setup.md#configure-your-multiplexer). This page is the rest of the walkthrough: the quality-of-life options, one keybinding set that drives both multiplexers, a themed status bar, and the room overrides Rimz exposes in its own config.

## Room overrides in Rimz config

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

Optional keys left unset fall through to your own Zellij or tmux config; a key you set here wins inside the room, because Rimz reasserts room options on every attach. Setting `[tmux] pane_border_status` makes Rimz own `pane-border-format` too — it titles work panes and blanks the sidebar's border row — while unset, your `~/.tmux.conf` format applies and may title the sidebar. `rimz config init --print` lists every room option with its default, and the full model is in [configuration → Multiplexer room options](./configuration.md#multiplexer-room-options).

## Zellij

The file is `~/.config/zellij/config.kdl`; `zellij setup --dump-config` prints the full default set, and `zellij setup --check` validates your edits. Every key here is catalogued in the [Zellij upstream reference](../externals/mux-adapter/zellij-reference.md#configuration).

[examples/zellij/config.kdl](../../examples/zellij/config.kdl) is the whole baseline — the [essentials](./setup.md#zellij) plus every block below and the `tokyo-night` theme — as one file, since Zellij reads a single config. Starting fresh, copy it; with an existing `config.kdl`, lift the blocks you want. Unlisted keys keep Zellij's defaults either way.

Clipboard travels over OSC52 by default, so yanking works through SSH to your local clipboard. A terminal that needs a helper can set one explicitly:

```kdl
// copy_command "wl-copy"      // Wayland;  "xclip -selection clipboard" on X11;  "pbcopy" on macOS
```

### Recommended

These tune the look and feel. None are required; each makes day-to-day work nicer.

```kdl
pane_frames true                       // titled borders mark the focused pane
styled_underlines true                 // colored/curly underlines from agents and editors
osc8_hyperlinks true                   // clickable links in command output
default_shell "zsh"                    // or your shell of choice
theme "dracula"                        // any bundled or custom theme
show_startup_tips false                // skip the startup tip banner
show_release_notes false               // skip the release-notes pane on upgrade
mouse_hover_effects false              // no hover frames/help text; calmer with many panes
session_serialization false            // prefer clean session births over held resurrection panes
```

`pane_frames true` draws a titled border around each pane so you can always see which one holds focus — the single most useful upgrade for a multi-agent layout. Rimz enforces its room's own mouse behavior, so your personal `focus_follows_mouse` and `mouse_click_through` settings no longer break single-click sidebar jumps.

Rimz rooms let Zellij split the focused pane along its longer visual edge when you open a new pane, and closing that pane returns the space to its split sibling.

### Alt chords in locked mode

Living in locked mode means Zellij's stock shortcuts are out of reach until you press `Ctrl+g`. A `locked` keybinds block puts the chords you use constantly one keypress away while everything else still flows to the agent; the block merges with Zellij's defaults, so nothing else changes.

```kdl
keybinds {
    locked {
        bind "Alt t" { NewTab; }
        bind "Alt x" { CloseTab; }
        bind "Alt d" { Detach; }
        bind "Alt h" { MoveFocus "left"; }
        bind "Alt j" { MoveFocus "down"; }
        bind "Alt k" { MoveFocus "up"; }
        bind "Alt l" { MoveFocus "right"; }
        bind "Alt n" { NewPane; }
        bind "Alt f" { ToggleFloatingPanes; }
        bind "Alt ," { GoToPreviousTab; }
        bind "Alt ." { GoToNextTab; }
    }
}
```

A chord bound here no longer reaches the app in the pane, so it shadows zsh's `Alt` keys (`Alt-f` forward-word, `Alt-.` last-arg) and any agent-TUI `Alt` binding — pick the set you actually use. The [tmux parity block below](#zellij-parity-alt-chords-optional) mirrors these same chords, so one set of muscle memory drives both multiplexers.

### A note on resurrection

Rimz disables Zellij session serialization inside its room, because it owns rebuilding a room after a reboot or crash: it re-seeds the prior agents itself ([resume on rebirth](../internals/sidebar/sidebar.md#resume-on-rebirth)) rather than resurrecting a wall of suspended command panes. Setting `session_serialization false` in your own `config.kdl` gives non-Rimz sessions the same clean-birth posture when you prefer running panes over resurrection.

Rimz also disables Zellij's session metadata loop inside its room. At roughly 100 panes on Zellij 0.44.3 that loop rewrites `session-metadata.kdl` every few seconds and runs process discovery through `ps`, a visible share of the Zellij server CPU; Rimz starts and attaches rooms with `disable_session_metadata true` so that work stays out of the room.

## tmux

The file is `~/.tmux.conf` (or `~/.config/tmux/tmux.conf`); reload it with `tmux source-file ~/.tmux.conf` or the `prefix` + `r` binding below. Every option here is catalogued in the [tmux upstream reference](../externals/mux-adapter/tmux-reference.md#options).

Everything below ships as four self-contained modules under [examples/tmux/](../../examples/README.md#tmux--tmux) — [`agents.conf`](../../examples/tmux/agents.conf) (the [essentials](./setup.md#tmux)), [`quality-of-life.conf`](../../examples/tmux/quality-of-life.conf) (copy-mode, window names, splits), [`zellij-keys.conf`](../../examples/tmux/zellij-keys.conf) (the parity chords), and [`theme-tokyonight.conf`](../../examples/tmux/theme-tokyonight.conf) (frames and status bar) — so your `~/.tmux.conf` stays yours and adopts by reference:

```sh
# From the rimz checkout: source the modules you want; drop any line.
printf 'source-file %s\n' "$PWD"/examples/tmux/{agents,quality-of-life,zellij-keys,theme-tokyonight}.conf >> ~/.tmux.conf
tmux source-file ~/.tmux.conf
```

### Recommended

These tune copy-mode, window behavior, pane borders, and splits. None are required; each makes day-to-day work nicer.

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

# Titled pane borders — tmux's analog of Zellij's pane frames.
set -g pane-border-status top
set -g pane-border-lines heavy   # solid frame lines instead of dashed ACS
set -g pane-border-format " #{pane_index} #{pane_current_command} "
set -g pane-border-style "fg=colour238"
set -g pane-active-border-style "fg=colour39"

# Reload the config; splits that keep the current directory and follow the
# pane's longer visual edge (the split behavior Rimz rooms give Zellij).
bind r source-file ~/.tmux.conf \; display "tmux.conf reloaded"
set -g @smart_split_is_wide '#{?#{&&:#{window_cell_width},#{window_cell_height}},#{e|>=:#{e|*:#{pane_width},#{window_cell_width}},#{e|*:#{pane_height},#{window_cell_height}}},#{e|>=:#{pane_width},#{pane_height}}}'
bind | if -F '#{E:@smart_split_is_wide}' 'split-window -h -c "#{pane_current_path}"' 'split-window -v -c "#{pane_current_path}"'
bind - if -F '#{E:@smart_split_is_wide}' 'split-window -h -c "#{pane_current_path}"' 'split-window -v -c "#{pane_current_path}"'
bind c new-window -c "#{pane_current_path}"
```

The root `MouseDrag1Pane` override opens copy-mode and begins the selection in one step. tmux's default opens copy-mode with `copy-mode -M` alone, which can leave the first drag unanchored — the highlight appears but the release copies nothing, so you press `q` and drag again to get it. Beginning the selection on entry makes the first drag copy and exit like the rest. The `pane_in_mode`/`mouse_any_flag` guard keeps the defaults intact: a drag inside a mouse-aware TUI or an already-open copy-mode still forwards to that app.

`automatic-rename off` with `allow-rename off` keeps window names where they were set — Rimz titles agent windows when it opens them, and your own windows keep the name you give them instead of tracking the foreground command. The `@smart_split_is_wide` splits mirror Rimz's Zellij rooms: a wide pane splits left/right, a tall pane splits top/bottom, comparing pixel dimensions when tmux knows the terminal cell size.

`pane-border-status top` labels each pane's border with its index and running command, so a grid of agents stays legible at a glance — the closest tmux gets to Zellij's titled frames. Rimz inherits this setting when its [`[tmux] pane_border_status`](./configuration.md#multiplexer-room-options) override is unset; when you set that override, Rimz titles work panes and blanks the sidebar's own border row. tmux does not draw a pane's outer window edge, so panes are not fully boxed like Zellij frames.

### Zellij-parity Alt chords (optional)

If you move between tmux and Zellij, mirroring the [locked-mode `Alt` chords above](#alt-chords-in-locked-mode) lets the same keys drive both multiplexers. These are no-prefix bindings, so they shadow zsh's `Alt` keys — the same trade the Zellij block already makes.

```tmux
# Tabs == tmux windows.
bind -n M-t new-window -c "#{pane_current_path}"    # new tab
bind -n M-x kill-window                             # close tab
bind -n M-, previous-window
bind -n M-. next-window
bind -n M-i swap-window -d -t -1                    # move tab left
bind -n M-o swap-window -d -t +1                    # move tab right
bind -n M-1 select-window -t 1                      # jump straight to a tab
bind -n M-2 select-window -t 2
bind -n M-3 select-window -t 3
bind -n M-4 select-window -t 4
bind -n M-5 select-window -t 5

# Panes.
bind -n M-n if -F '#{E:@smart_split_is_wide}' 'split-window -h -c "#{pane_current_path}"' 'split-window -v -c "#{pane_current_path}"'
bind -n M-h select-pane -L
bind -n M-j select-pane -D
bind -n M-k select-pane -U
bind -n M-l select-pane -R
# Zellij's floating pane, approximated as a fresh centered popup.
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

### A themed status bar (optional)

tmux's status bar is yours everywhere, including inside Rimz rooms. This block styles it as Powerline-style tabs in TokyoNight Night — the palette of Rimz's default sidebar scheme — so the whole room reads as one surface. It assumes the outer terminal uses a Nerd Font or another Powerline-capable face.

```tmux
set -g status-interval 5
set -g status-position bottom
set -g status-style "bg=#1a1b26,fg=#c0caf5"
set -g status-left  "#[fg=#1a1b26,bg=#7aa2f7,bold] #S "
set -g status-right "#[fg=#414868,bg=#1a1b26]#[fg=#c0caf5,bg=#414868] %H:%M #[fg=#7aa2f7,bg=#414868]#[fg=#c0caf5,bg=#414868] #h "
set -g status-left-length 60
set -g status-right-length 80
# Powerline tabs: blue session block, green focused tab, dim inactive tabs.
set -g window-status-separator ""
set -g window-status-style "bg=#1a1b26,fg=#a9b1d6,noreverse"
set -g window-status-current-style "bg=#1a1b26,fg=#1a1b26,bold,noreverse"
set -g window-status-format "#[noreverse,fg=#{?window_start_flag,#7aa2f7,#1a1b26},bg=#24283b]#[noreverse,fg=#a9b1d6,bg=#24283b] #{?automatic_rename,#I:#W,#W} #[noreverse,fg=#24283b,bg=#1a1b26]"
set -g window-status-current-format "#[noreverse,fg=#{?window_start_flag,#7aa2f7,#1a1b26},bg=#9ece6a]#[noreverse,fg=#1a1b26,bg=#9ece6a,bold] #{?automatic_rename,#I:#W,#W} #[noreverse,fg=#9ece6a,bg=#1a1b26]"
set -g message-style "fg=#1a1b26,bg=#7aa2f7,bold"
setw -g mode-style "fg=#c0caf5,bg=#283457"
```

Swap the hex values for your own scheme's palette; `rimz list-themes` names every scheme Rimz bundles if you want the sidebar and status bar to match.

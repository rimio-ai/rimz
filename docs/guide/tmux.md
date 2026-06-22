# Configure tmux

Rimz runs inside the tmux you already use, and sets the room's behavior for you: on every session birth and reattach it applies the mouse, focus-event, passthrough, history, CSI-u soft-newline key, and clipboard options agents need ([installation → configure your multiplexer](./installation.md#configure-your-multiplexer), [configuration → multiplexer room options](../reference/configuration.md#multiplexer-room-options)). Your `~/.tmux.conf` owns what Rimz leaves to you — true-color rendering, copy-mode, the status bar, and your keybindings — so those settings matter even inside the room, and they own your tmux sessions outside Rimz entirely.

This guide is a baseline that makes tmux modern and pleasant everywhere. The file lives at `~/.tmux.conf` (or `~/.config/tmux/tmux.conf`); reload it with `tmux source-file ~/.tmux.conf` or the `prefix` + `r` binding below. Every option here is catalogued in the [tmux upstream reference](../externals/mux-adapter/tmux-reference.md#options).

## Essential

Two groups. The first — **true color** — sets your terminal type and RGB passthrough. Inside its room Rimz already restores `COLORTERM=truecolor` when your terminal advertises it, but tmux still needs `default-terminal` and the RGB overrides for its own color handling and for the tmux sessions you run outside Rimz. The second group are behaviors Rimz applies inside its room; set them here so your own sessions behave the same.

```tmux
# True color + italics: advertise a color terminal, pass RGB and styles through.
set -g default-terminal "tmux-256color"
set -ga terminal-overrides ",*256col*:RGB,alacritty:RGB,wezterm:RGB"
set -ga terminal-features ",*:RGB,*:usstyle,*:clipboard"

# Behaviors a modern TUI agent relies on.
set -g  mouse on                 # scroll, select panes, resize
set -g  history-limit 100000     # long Claude/Codex output stays in scrollback
set -sg escape-time 0            # no ESC lag in helix, nvim, fzf, agent TUIs
set -g  focus-events on          # editors and agents see focus changes
set -g  allow-passthrough on     # let desktop notifications pass through tmux
set -s  extended-keys on             # distinguish modified Enter from Enter
set -s  extended-keys-format csi-u   # forward Shift+Enter / Alt+Enter as CSI-u
set -ga terminal-features "*:extkeys" # ask the outer terminal to send them
set -g  set-clipboard on         # yank into the host clipboard over OSC52
```

`escape-time 0` removes the lag that otherwise makes `Esc` feel sticky in any full-screen TUI. `extended-keys`, `extended-keys-format csi-u`, and `terminal-features … extkeys` let an agent's composer receive Shift+Enter and Alt+Enter as CSI-u soft newlines while plain Enter still submits. Rimz applies the same `extkeys` feature inside its room. `set-clipboard on` carries a yank out through OSC52, so you can copy agent output to your local clipboard even over SSH. `allow-passthrough on` lets the desktop-notification bytes Rimz emits reach your terminal ([notifications](../internals/sidebar/notifications.md)).

## Recommended

These tune copy-mode, the status bar, and pane borders. None are required; each makes day-to-day work nicer.

```tmux
# Copy-mode: vi keys, mouse-drag yanks without exiting, gentler scroll.
setw -g mode-keys vi
bind -T copy-mode-vi v send -X begin-selection
bind -T copy-mode-vi y send -X copy-pipe-no-clear
bind -T copy-mode-vi MouseDragEnd1Pane send -X copy-pipe-no-clear
bind -T copy-mode-vi WheelUpPane   send -X -N 3 scroll-up
bind -T copy-mode-vi WheelDownPane send -X -N 3 scroll-down

# Keep windows compactly numbered and panes resizing across clients.
set -g  renumber-windows on
setw -g aggressive-resize on

# Titled pane borders — tmux's analog of Zellij's pane frames.
set -g pane-border-status top
set -g pane-border-format " #{pane_index} #{pane_current_command} "
set -g pane-active-border-style "fg=colour39"

# Reload the config; splits that keep the current directory.
bind r source-file ~/.tmux.conf \; display "tmux.conf reloaded"
bind | split-window -h -c "#{pane_current_path}"
bind - split-window -v -c "#{pane_current_path}"
bind c new-window      -c "#{pane_current_path}"
```

`pane-border-status top` labels each pane's border with its index and running command, so a grid of agents stays legible at a glance — the closest tmux gets to Zellij's titled frames. Rimz inherits this setting when its [`[tmux] pane_border_status`](../reference/configuration.md#multiplexer-room-options) override is unset; when you set that override, Rimz titles work panes and blanks the sidebar's own border row. tmux does not draw a pane's outer window edge, so panes are not fully boxed like Zellij frames.

## Match Zellij's no-prefix keys (optional)

If you move between tmux and Zellij, mirroring Zellij's locked-mode `Alt` chords lets the same keys drive both multiplexers. These are no-prefix bindings, so they shadow zsh's `Alt` keys (`Alt-f` forward-word, `Alt-.` last-arg) — the same trade Zellij's locked mode already makes.

```tmux
bind -n M-t new-window   -c "#{pane_current_path}"   # Zellij Alt-t  new tab
bind -n M-n split-window -c "#{pane_current_path}"   # Zellij Alt-n  new pane
bind -n M-h select-pane -L                           # focus left
bind -n M-j select-pane -D                           # focus down
bind -n M-k select-pane -U                           # focus up
bind -n M-l select-pane -R                           # focus right
bind -n M-d detach-client                            # Zellij Alt-d  detach
```

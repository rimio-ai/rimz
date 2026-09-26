# Zellij and tmux

RimZ runs your agents inside Zellij or tmux, and ships no terminal of its own. If you already live in one, that is most of the integration: a room is an ordinary session, the agents run their stock CLIs in ordinary panes, and detach, reattach, scrollback, and copy-mode work exactly as they always have, because they are your multiplexer doing them. Every session you run outside RimZ is untouched.

The defaults are the exception. Multiplexers were tuned for shells and editors decades before an agent TUI streamed tokens, wanted Shift+Enter for a soft newline, and expected 24-bit color and a deep scrollback. So a room asserts a short list of settings agents need and leaves the rest to you. This page is that list, then a baseline configuration worth adopting for the sessions you run outside a room.

## What the room changes, and what stays yours

A room is one Zellij or tmux session. Every time RimZ opens or reattaches it, RimZ asserts the settings agents need and stops there; it never edits `~/.config/zellij/config.kdl` or `~/.tmux.conf`. Detach the way you always do, and `rimz start` in the project directory, or `rimz attach <session>` from anywhere, brings you back to the same panes.

If an older RimZ wrote the room, `rimz start` tears it down and opens a fresh room instead. Its history is not carried over. See [starting a room](../reference/cli/getting-started.md#start-the-room).

On Zellij the room starts in locked mode, so your keystrokes reach the agent until you press `Ctrl+g` for a Zellij mode. A single click on a sidebar card jumps to that agent whatever your own mouse settings say. A new pane opened with no direction splits the focused pane along its longer visual edge, and closing it returns the space to that sibling.

Three more Zellij differences have nothing to do with typing. Session serialization is off, because RimZ rebuilds a room after a crash or reboot itself rather than letting Zellij resurrect a wall of suspended command panes that come back dead. Zellij's per-second session-metadata loop is off too, because at around 100 panes it costs a visible share of the Zellij server's CPU. And because RimZ supplies the room's tab layout, a RimZ tab carries Zellij's one-row compact bar instead of the status chrome your own `default_layout` would draw.

On tmux the room turns on the mouse, focus events, a 100,000-line scrollback, notification passthrough, the OSC 52 clipboard, clickable OSC 8 links, atomic redraws, and the extended-keys handling that makes Shift+Enter and Alt+Enter soft newlines while plain Enter still submits. It sets `escape-time` to 10 ms, keeps window numbering compact, resizes panes per attached client, and titles the outer terminal.

Everything else is yours, inside the room and out: your theme, your prefix, your copy-mode bindings, your status bar, your keybinds. `~/.tmux.conf` and `config.kdl` are still read in a room, and the settings above layer over them. The one place the room reaches into your key tables is tmux's root table. It binds `S-Enter` and `M-Enter` there for soft newlines, plus a `User240` that turns a bare `ESC[27u` back into plain Escape; some terminals, Ghostty among them, answer the extended-keys request with that sequence.

For the panes RimZ creates, the outer terminal's title is the room and a short pane identity: `myrepo-a1b2 | zsh`, `myrepo-a1b2 | codex`. Agent and channel panes carry their RimZ identity; any other pane shows its running command on tmux, and on Zellij the name it was launched with. Applications cannot overwrite it, so the shell that likes to set your title to an SSH host and a working path does not, and you can still pick the room's window out of a taskbar.

Closing every RimZ room undoes all of this. The settings live in the session, nothing was written to your config, and on tmux the whole server they live in exits with its last room. One thing outlives the room: on Zellij, RimZ seeds a permission grant for the presence plugin it ships, in Zellij's own permission cache, so your first attach is not interrupted by a plugin prompt. Revoking it stops the plugin until the next room birth seeds it again. The full boundary, and what the plugin may do, are in [security and trust](./security.md#the-zellij-presence-plugin).

### Your tmux rooms run on their own server

RimZ rooms are not on your default tmux server. They run on a second one, on a socket under the runtime root. That is what keeps the settings tmux scopes to a whole server (clipboard, focus events, extended keys, escape time, the three root bindings) out of your own sessions: they stop at that socket. `tmux ls` in your own terminal does not list a RimZ room, and your `tmux kill-server` cannot touch one. Address the RimZ server with `-S`:

```sh
tmux -S "${XDG_RUNTIME_DIR:-/tmp/rimz-$(id -u)}/rimz/tmux/server" ls
```

It holds one session per project and exits when the last one closes. `rimz paths` prints the same directory as `runtime root`.

Zellij has no such split: a RimZ room is an ordinary session in `zellij ls`, and `zellij action` addresses it with no extra flag. On either backend, a new room's session name is its state directory name under `~/.rimz/ws/`, such as `myrepo-a1b2`, also shown by `rimz list` and `rimz gc`. A running room keeps its older name across an upgrade; `rimz start` and `rimz attach` join it unchanged. The next birth, after the session ends or you run `rimz reset`, takes the state directory name.

### Two keys that work from any pane

The sidebar's own keys fire only when the sidebar has focus, so RimZ registers two chords with the multiplexer itself. `Alt+p` focuses the sidebar and pressing it again returns you to the pane you left, which is the whole [jump loop](./sidebar.md#glance-jump-answer). `Alt+g` fullscreens the focused work pane, and picks a working sibling first when the sidebar is what has focus.

tmux takes them as root-table bindings on RimZ's server; on Zellij the presence plugin installs them at runtime, so your `config.kdl` is unchanged. Either way they end with the session. `sidebar.focus_key` and `sidebar.zoom_key` change or disable them ([configuration](./configuration.md#sidebar-rendering)). Choose a chord your agents do not use, and on Zellij leave `Ctrl+g` alone unless some other chord still reaches Zellij's modes, because it is the default escape from locked mode.

### Change what the room asserts

The `[zellij]` and `[tmux]` tables in `~/.rimz/config.toml` set the values the room asserts:

```toml
[zellij]
pane_frames = true              # unset, your config.kdl decides

[tmux]
history_limit = 200000
pane_border_status = "top"      # unset, your ~/.tmux.conf decides
```

The backends differ in what an unset key means. `[tmux]` carries a RimZ value for every key but the two pane-border ones, so leaving a key out means RimZ's default rather than yours. `[zellij]` passes only the keys you set, plus the four it always passes: `mouse_click_through` and `focus_follows_mouse` behind single-click jumps, `session_serialization`, and `disable_session_metadata`. Every key with its default is in [configuration → multiplexer room options](./configuration.md#multiplexer-room-options).

A changed value reaches the room the next time RimZ opens or reattaches its session. tmux's window-scoped settings are the exception: the pane borders, passthrough, and per-client resize reach only windows opened after the change. When an edit looks ignored, restart the room.

A Zellij boolean is a request rather than a setting. Zellij XORs a boolean command-line option against the same key in your `config.kdl`, so `pane_frames = true` here while `config.kdl` already says `pane_frames true` turns frames off. Set a Zellij boolean in one file, not both.

## Everything in the room is an ordinary pane operation

What RimZ does inside the room reduces to multiplexer commands you could run yourself, which is worth knowing before you hand it a room full of agents.

An agent is a plain process in an ordinary pane. A layout is pane splits: `rimz agents claude,codex` asks the multiplexer to split two panes side by side and launches a CLI in each, exactly the splits you would make by hand, in one command instead of several. The layout grammar is in [agents → compose a layout](./fleet.md#compose-a-layout).

`rimz message @coder "rebase first"` resolves the handle to one live pane and writes the text into it as a single bracketed paste, then presses Enter as a separate key, using the multiplexer's own primitives. `rimz pane capture @coder` reads the pane's visible text with `capture-pane`. The sidebar is one more pane. You can list, focus, and drive every pane in a room yourself, with `zellij action` or with `tmux -S` against the socket above, and RimZ is calling exactly those commands. What a message carries and when it lands is in [messaging](./messaging.md).

## Which backend a room uses

With both installed, RimZ takes the first of these that answers:

1. `--mux zellij` or `--mux tmux` on the command, or their `--zellij` and `--tmux` shorthands;
2. the multiplexer you are already inside;
3. `[mux] default` in `~/.rimz/config.toml`, which makes `rimz start` refuse when it names a backend that is not installed;
4. whichever one is installed; tmux when both are.

A room that already exists keeps the backend it was born on, whatever that resolution says. The floors are Zellij 0.44.2 and tmux 3.5, with 3.6 or newer preferred; [installation](./installation.md#install-zellij-or-tmux) has the versions and how to get a current build on your platform.

## Adopt a baseline

You need none of the configuration below. A freshly installed multiplexer works, because the room sets its own options. These baselines are for your comfort, and for the sessions you run outside RimZ. The fastest good config is the one RimZ ships under [examples/](../../examples/README.md): Zellij as one complete file, tmux as four sourceable modules.

```sh
# From the rimz checkout. Run the line for the multiplexer you use.
mkdir -p ~/.config/zellij && cp examples/zellij/config.kdl ~/.config/zellij/config.kdl

printf 'source-file %s\n' "$PWD"/examples/tmux/{agents,quality-of-life,zellij-keys,theme-tokyonight}.conf >> ~/.tmux.conf
tmux source-file ~/.tmux.conf
```

Take that path if you are starting fresh. The `cp` replaces an existing `config.kdl`, so with a config you care about, lift the blocks you want instead; the tmux line only appends, and dropping a `source-file` line undoes it. The rest of this page is what each block does and why.

## Zellij

The file is `~/.config/zellij/config.kdl`. `zellij setup --dump-config` prints the full default set, and `zellij setup --check` validates your edits. Every key here is catalogued in the [Zellij upstream reference](../externals/mux-adapter/zellij-reference.md#configuration).

Zellij reads a single config file, so [examples/zellij/config.kdl](../../examples/zellij/config.kdl) ships as one: the essentials below, every block after them, and the `tokyo-night` theme. Keys it does not list keep Zellij's defaults.

### Essentials

These six make a coding-agent session behave correctly: long output stays readable, the keyboard reaches the agent, and copied text lands where you expect.

```kdl
default_mode "locked"                  // hand typing straight to the agent; Ctrl+g enters Zellij
scroll_buffer_size 100000              // keep long agent output scrollable
mouse_mode true                        // scroll, select, and resize with the mouse
copy_on_select true                    // selecting text copies it
copy_clipboard "system"                // yank into the OS clipboard
support_kitty_keyboard_protocol true   // Shift+Enter and friends reach TUI agents
```

`default_mode "locked"` is the one that matters most. Locked mode passes ordinary keystrokes to the focused pane, so an agent's TUI gets your input until you deliberately press `Ctrl+g` for a Zellij mode. A RimZ room already opens in locked mode; setting it here keeps your own sessions consistent and your muscle memory intact. Zellij inherits truecolor from the terminal it runs in, so there is no color option to set.

Clipboard travels over OSC 52, which means a yank inside an SSH session still reaches your local clipboard. A terminal that needs a helper instead can name one:

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
session_serialization false            // start clean instead of resurrecting suspended panes
// default_shell "zsh"                 // uncomment to pin a shell; unset uses $SHELL
```

`pane_frames true` draws a titled border around each pane so you can always see which one holds focus, which matters once a dozen panes look alike. `session_serialization false` gives your own sessions the clean-birth posture RimZ rooms already have: running panes on the next start instead of a wall of suspended ones.

### Alt chords in locked mode

Locked mode hands every keystroke to the pane, which is what you want almost always, and it puts Zellij's own shortcuts one extra keypress away. A `locked` keybinds block keeps the chords you use constantly reachable while everything else still flows to the agent. The block merges with Zellij's defaults, so no other mode changes.

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

A chord bound here stops reaching the app in the pane, so it shadows zsh's `Alt` keys (`Alt-f` forward-word, `Alt-.` last-arg) and any agent-TUI `Alt` binding. Trim the set to the chords you actually use. The [tmux parity block below](#zellij-parity-alt-chords) mirrors these same chords, so one set of muscle memory drives both multiplexers.

## tmux

The file is `~/.tmux.conf` (or `~/.config/tmux/tmux.conf`); reload it with `tmux source-file ~/.tmux.conf` or the `prefix` + `r` binding below. Every option here is catalogued in the [tmux upstream reference](../externals/mux-adapter/tmux-reference.md#options).

tmux is the spartan one out of the box: no mouse, a short scrollback, and thin, untitled pane borders. The blocks below ship as four self-contained modules under [examples/tmux/](../../examples/README.md#tmux), so your `~/.tmux.conf` stays yours and adopts by reference: [`agents.conf`](../../examples/tmux/agents.conf) (the essentials), [`quality-of-life.conf`](../../examples/tmux/quality-of-life.conf) (copy-mode, window names, splits), [`zellij-keys.conf`](../../examples/tmux/zellij-keys.conf) (the parity chords), and [`theme-tokyonight.conf`](../../examples/tmux/theme-tokyonight.conf) (frames and status bar).

### Essentials

The first three lines advertise a color terminal and pass RGB and styles through, which tmux needs for its own color handling. The rest are the behaviors a RimZ room applies for you, so setting them here is what makes the sessions you run outside a room match.

```tmux
# True color + italics: advertise a color terminal, pass RGB and styles through.
set -g default-terminal "tmux-256color"
set -ga terminal-overrides ",*256col*:RGB,alacritty:RGB,wezterm:RGB"
set -ga terminal-features ",*:RGB,*:usstyle,*:clipboard"

# Behaviors a modern TUI agent relies on.
set -g  mouse on                      # scroll, select panes, resize
set -g  history-limit 100000          # long Claude/Codex output stays in scrollback
set -sg escape-time 10                # upstream 3.5+ default; safely joins split ESC sequences
set -ga terminal-features ",*:sync"   # atomic redraws: no torn frames
set -ga terminal-features ",*:hyperlinks"  # clickable OSC 8 links
set -g  focus-events on               # editors and agents see focus changes
set -g  allow-passthrough on          # desktop notifications pass through tmux
set -g  set-clipboard on              # yank into the host clipboard over OSC 52

# Modified Enter: soft newlines for agents, plain Enter still submits.
set -s  extended-keys on
set -s  extended-keys-format csi-u
set -ga terminal-features "*:extkeys"
set -s  user-keys[240] "\e[27u"        # name Ghostty's modifier-less CSI-u Esc
bind-key -n User240 send-keys Escape   # normalize it back to plain Esc
bind-key -n S-Enter send-keys Escape "[13;2u"
bind-key -n M-Enter send-keys Escape "[13;3u"
```

Three of these earn a note; the comments carry the rest. `escape-time 10` is the upstream default from tmux 3.5, down from 500 ms: it gives tmux a short window to join an ESC byte with the rest of its sequence, which avoids split-ESC misparses over SSH while staying imperceptible. The extended-keys trio and the two Enter bindings let an agent's composer receive Shift+Enter and Alt+Enter as soft newlines; on tmux 3.5.x that trades clean multiline paste while extended keys are active, so use Ctrl+J for a soft newline there, or tmux 3.6 and up for both. `allow-passthrough on` is what lets the desktop-notification bytes RimZ writes reach your terminal.

### Recommended

These tune copy-mode, window behavior, and splits. None are required; each makes day-to-day work nicer. Two of them, `renumber-windows` and `aggressive-resize`, a RimZ room already sets for itself.

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
# longer visual edge, the way Zellij splits.
bind r source-file ~/.tmux.conf \; display "tmux.conf reloaded"
set -g @smart_split_is_wide '#{?#{&&:#{window_cell_width},#{window_cell_height}},#{e|>=:#{e|*:#{pane_width},#{window_cell_width}},#{e|*:#{pane_height},#{window_cell_height}}},#{e|>=:#{pane_width},#{pane_height}}}'
bind | if -F '#{E:@smart_split_is_wide}' 'split-window -h -c "#{pane_current_path}"' 'split-window -v -c "#{pane_current_path}"'
bind - if -F '#{E:@smart_split_is_wide}' 'split-window -h -c "#{pane_current_path}"' 'split-window -v -c "#{pane_current_path}"'
bind c new-window -c "#{pane_current_path}"
```

The root `MouseDrag1Pane` override opens copy-mode and begins the selection in one step. tmux's default opens copy-mode with `copy-mode -M` alone, which can leave the first drag unanchored: the highlight appears but the release copies nothing, so you press `q` and drag again to get it. Beginning the selection on entry makes the first drag copy and exit like the rest. The `pane_in_mode` and `mouse_any_flag` guard keeps the defaults intact, so a drag inside a mouse-aware TUI or an already-open copy-mode still forwards to that app.

`automatic-rename off` with `allow-rename off` keeps window names where they were set: RimZ titles agent windows when it opens them, and your own windows keep the name you gave them instead of tracking the foreground command. The `@smart_split_is_wide` splits give tmux the placement Zellij does natively, so a wide pane splits left and right and a tall pane top and bottom, comparing pixel dimensions when tmux knows the terminal's cell size. Both `|` and `-` run it, so either key gives you the sensible split rather than a fixed direction.

### Zellij-parity Alt chords

If you move between tmux and Zellij, mirroring the [locked-mode `Alt` chords above](#alt-chords-in-locked-mode) lets the same keys drive both. These are no-prefix bindings, so they shadow zsh's `Alt` keys, the same trade the Zellij block makes.

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

`Alt+[` and `Alt+]`, Zellij's other tab-cycle pair, are left out on purpose: `Alt+[` sends `ESC [`, which collides with CSI escape sequences in some terminals.

### Titled pane borders and a themed status bar

tmux draws thin, untitled pane borders and a plain status bar by default. This module gives panes titled frames, the closest tmux gets to Zellij's, and styles the status bar as Powerline tabs in TokyoNight Night, the palette of RimZ's default sidebar scheme, so the whole room reads as one surface. It assumes the outer terminal uses a Nerd Font or another Powerline-capable face.

```tmux
# Titled pane borders: the tmux analog of Zellij's pane frames.
set -g pane-border-status top
set -g pane-border-lines heavy               # solid frame lines instead of thin ACS
set -g pane-border-style "fg=#414868"
set -g pane-active-border-style "fg=#7aa2f7"

# Status bar: blue session block, green focused tab, dim inactive tabs.
set -g status-style "bg=#1a1b26,fg=#c0caf5"
set -g status-left  "#[fg=#1a1b26,bg=#7aa2f7,bold] #S "
set -g window-status-separator ""
```

`pane-border-status top` labels each pane's border, so a grid of agents stays legible at a glance. The module's own `pane-border-format` puts the pane index and running command on that row and colors it by focus; it is one long line, so read it in [theme-tokyonight.conf](../../examples/tmux/theme-tokyonight.conf) rather than here. tmux does not draw a pane's outer window edge, so panes are not fully boxed the way Zellij frames are.

A RimZ room inherits these borders while [`[tmux] pane_border_status`](./configuration.md#multiplexer-room-options) is unset. Set that override to `top` or `bottom` and RimZ owns `pane-border-format` too: it titles work panes and blanks the sidebar's border row, so the sidebar reads frameless.

Swap the hex values for your own scheme's palette. `rimz list-themes` names every scheme RimZ bundles, if you want the sidebar and the status bar to match.

## See also

- [Set up your machine](./setup.md): the one-time pass that writes the per-machine config and installs the agent hooks the sidebar reads.
- [Configuration → multiplexer room options](./configuration.md#multiplexer-room-options): every `[mux]`, `[zellij]`, and `[tmux]` key with its default.
- [examples/](../../examples/README.md): the shipped baseline files these blocks come from.
- [Security and trust → the Zellij presence plugin](./security.md#the-zellij-presence-plugin): the one permission grant RimZ seeds, and what the plugin may do with it.
- [Troubleshooting → Zellij or tmux is missing or too old](./troubleshooting.md#zellij-or-tmux-is-missing-or-too-old): what `rimz doctor` says about your backend, and how to read it.
- [Zellij](../externals/mux-adapter/zellij-reference.md) and [tmux](../externals/mux-adapter/tmux-reference.md) upstream references: every option catalogued at the source.

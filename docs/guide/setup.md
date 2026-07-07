# Set up your machine

Rimz runs with zero configuration, and one setup pass makes it a comfortable daily driver — on a laptop or on the remote server you SSH into. This guide picks up where [installation](./installation.md) ends, with `rimz` on your `PATH` and a multiplexer that clears the version floor: initialize the config, wire agent hooks, turn on true color and a pet, switch on the hands-off loop behaviors, and give your own Zellij or tmux a baseline worth keeping.

The fast path is three commands and a room. The rest of the page is what each step does and the settings worth choosing while you are here.

```sh
rimz setup            # detect the machine, write config, choose hooks and appearance
rimz hooks install    # wire every detected agent's hooks into Rimz
rimz doctor           # confirm the machine is ready
cd ~/code/your-project && rimz
```

## Initialize the config

`rimz setup` prints a first-run report — the selected multiplexer, workspace root, trust state, config path, detected agent binaries, and hook install status — and writes any missing per-machine config under `~/.config/rimz/`. On an interactive terminal it offers to keep an existing config and refresh it against the current templates, offers hook install for detected agents, shows a live color-and-icon probe, and asks whether to enable a sidebar pet. The first `rimz` run on a terminal asks the same hook, glyph, and pet questions when it creates the config. `rimz setup --yes` takes the non-interactive path (merge existing files, write missing ones, no hook installs or trust grants), which suits a server provisioning script.

Four files carry the settings this guide touches:

| File | Owns |
| --- | --- |
| `~/.config/rimz/config.toml` | room behavior: resume, auto-continue, smart compaction, notifications, multiplexer room overrides |
| `~/.config/rimz/theme.toml` | sidebar appearance: scheme, color depth, glyphs, pets |
| `~/.config/rimz/agents.toml` | agent profiles, teams, worktree defaults, attention timing |
| `~/.config/rimz/loop.toml` | scheduled loop tasks: window pings, watchdogs, self-wakes |

Every key ships commented with its default and an inline note, so the generated template is the field reference:

```sh
rimz config init --print                     # every key, its default, and what it does
rimz config get                              # the whole effective config as TOML
rimz config set theme "Catppuccin Mocha"     # edit one dotted key in the owning file
```

A commented line keeps following the defaults shipped by future Rimz versions; uncommenting makes it this machine's override. `rimz config set` routes a dotted key to the file that owns it, validates the value, and writes durably. The config model — tiers, merge order, and every behavior section including notifications — is in the [configuration reference](../reference/configuration.md).

## Install agent hooks

Hooks are how a running agent reports to the room: turn starts and ends, permission prompts, and blocking questions reach the sidebar through hook events. Install them into every detected agent's per-user config:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install              # every detected agent on PATH (claude, codex, pi, opencode)
rimz hooks install claude       # one agent kind
```

The install is additive — your existing hooks stay — and each report names the file it edits and the undo (`rimz hooks uninstall [AGENT]`). For agents with a statusline, Rimz wraps the command so the sidebar reads live context, and restores yours on uninstall. The first `rimz` run and interactive `rimz setup` offer the same install with a consent prompt and diff preview, so `rimz hooks install` is mainly for adding an agent later or re-checking the surface. Some agents gate hooks behind their own trust prompt; when one reports installed-but-untrusted hooks, `rimz doctor` prints the exact fix. Command detail is in [the hooks CLI](../reference/cli/hooks-trust.md#agent-hooks).

## True color

The sidebar and agent TUIs render best at 24-bit color, and three layers decide whether you get it:

- **Your terminal.** Pick one that advertises truecolor — Ghostty, WezTerm, Kitty, and Alacritty all do. This is the whole story for local terminal-attached Zellij, which inherits color support from the terminal it runs in.
- **Rimz.** `[theme] mode = "auto"` (the default) emits truecolor whenever `COLORTERM` or the `$TERM` terminfo advertises it. Inside a Rimz tmux room, Rimz stamps `COLORTERM=truecolor` at birth when the launching terminal advertises it, so `auto` resolves to truecolor despite tmux's `tmux-256color` default; `rimz remote` carries the same stamp over SSH when the local terminal advertises it, and `rimz web` stamps browser-born rooms because xterm.js renders 24-bit color. Pin `mode = "truecolor"` for rooms born before this support or for other stripping hops.
- **Your own tmux sessions.** tmux needs `default-terminal` and the RGB overrides in [the tmux baseline below](#tmux) for its own color handling outside Rimz rooms.

With a Nerd Font in the terminal, one line upgrades the glyphs too:

```toml
[theme]
style = "modern"       # truecolor + Nerd Font icons; "default" = auto color + Unicode
# mode = "truecolor"   # force RGB when auto-detection is defeated
```

Schemes, palette slots, and the full display model are in [theming](../reference/theme.md).

The first-run glyph probe writes `style = "modern"` for you when the gradient is smooth and the sampled sidebar icons render cleanly.

## Pets

Pets add a small animated companion to the sidebar's provider dashboard, following the fleet's state. The setup pet question writes `enabled = true` for the default `rocky` pet; enable or change one manually in `~/.config/rimz/theme.toml`:

```toml
[theme.pets]
enabled = true
pet = "rocky"      # built-in id, https:// URL, local sheet path, or petdex pet
glyphs = "auto"    # pixels when the runtime supports them, else sextant cell art
voice = true       # canned captions on fleet-status changes
```

`rimz list-pets` previews every built-in as cell art; the shipped ids are `codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, and `null-signal`.

`glyphs = "auto"` renders crisp pixels when the terminal is Ghostty or kitty — inside tmux that also needs tmux 3.6 or newer with `allow-passthrough on` — and falls back to sextant cell art everywhere else, including all Zellij rooms. `glyphs = "sextant"` pins the portable cell art. Bring-your-own sheets, the offline switch, and the privacy boundary are in [theming → Pets](../reference/theme.md#pets).

## Keep the fleet moving

Rimz routes attention by default and leaves every decision to you. Four opt-in behaviors keep agents productive through reboots, rate limits, and full context windows — the difference between a fleet that waits for you and one that only needs you for real decisions. All four live in per-machine config.

### Resume agents after a reboot

```toml
[resume]
on_rebirth = true    # already the default
max = 128            # cap on agents one rebirth relaunches
```

When a room is reborn after a reboot or a multiplexer crash, Rimz offers to bring back the prior agents from its durable records — each restored agent starts idle in its worktree tab. This is on by default; the knobs bound it, and `rimz start --no-resume` or `on_rebirth = false` gives a clean empty room instead. The mechanics are in [sidebar internals → Resume on rebirth](../internals/sidebar/sidebar.md#resume-on-rebirth).

### Auto-continue parked turns

```toml
[resume]
auto_continue = true                       # off by default
# auto_continue_backoff_secs = [180, 300]  # first retry after 3m, then every 5m
# auto_continue_max_retries = 13           # stop after ~63 minutes of retries
# auto_continue_text = "continue"          # the nudge typed into the parked pane
```

A turn that dies mid-flight parks its agent: a rate limit, a spend limit, a provider overload, or a transient API error (a stalled stream, timeout, or dropped connection). `auto_continue` picks those turns back up by typing `continue` into the pane through the same audited path as `rimz message` — rate-limit and spend-limit parks resume when the provider's budget window resets, and overload or transient-error parks retry on the backoff ramp until the retry cap. The model is in [provider internals → Auto-continue](../internals/agents/provider.md#auto-continue).

### Compact before the prompt lands

```toml
[harness]
smart_compact = "70%"    # or an occupied-token count such as "120000"
```

`smart_compact` makes `rimz message` compact-first: when the target agent's context window has reached the threshold, Rimz submits the agent's `/compact` ahead of your text so the prompt lands against a fresh window instead of dying at the context ceiling. Unset, compaction stays opt-in per send through `rimz message --smart-compact`. The mechanics are in [message internals → Smart compaction](../internals/harness/message.md#smart-compaction).

### Prime provider windows on a schedule

A provider's budget window starts counting on first use, so a window that starts when you sit down ends mid-afternoon. A scheduled `<kind>-ping` loop task starts the window on your clock instead — one cheap ping per provider primes the whole account:

```sh
rimz loop add morning --spec claude-ping --prompt ping --at 07:00 --days weekdays
rimz loop add follow-reset --spec claude-ping --prompt ping --at-reset   # re-prime when the window resets
```

or hand-edit `~/.config/rimz/loop.toml`:

```toml
[tasks.morning]
spec = "claude-ping"
prompt = "ping"
root = "/home/you/code/app"
at = "07:00"
days = "weekdays"
```

The ping runs at the lowest effort, skips when the provider's window is already counting down, and fires only while a room for `root` is open. The same `[tasks]` table also schedules watchdogs and self-wakes — an agent turn on an interval, gated on a shell check such as `cargo test` or `gh run watch` — covered in [configuration → Loop tasks](../reference/configuration.md#loop-tasks) and [the loop CLI](../reference/cli/agents.md#schedule-turns-with-loop).

## Configure your multiplexer

Rimz sets the room's behavior for you: on every session birth and reattach it applies the options agents need — locked mode and single-click sidebar jumps on Zellij; mouse, focus events, OSC passthrough, CSI-u soft-newline keys, and clipboard on tmux; 100k-line scrollback on both — so a freshly installed multiplexer works without touching its config. Your own `~/.config/zellij/config.kdl` or `~/.tmux.conf` owns everything Rimz leaves alone — the theme, true color, copy-mode, the status bar, and your keybindings — inside the room and in every session you run outside Rimz. The baselines below make either multiplexer modern and pleasant everywhere, and both ship ready to adopt under [examples/](../../examples/README.md) — tmux as sourceable modules, Zellij as a complete starting file.

### Room overrides in Rimz config

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

Optional keys left unset fall through to your own Zellij or tmux config; a key you set here wins inside the room, because Rimz reasserts room options on every attach. Setting `[tmux] pane_border_status` makes Rimz own `pane-border-format` too — it titles work panes and blanks the sidebar's border row — while unset, your `~/.tmux.conf` format applies and may title the sidebar. `rimz config init --print` lists every room option with its default, and the full model is in [configuration → Multiplexer room options](../reference/configuration.md#multiplexer-room-options).

### Zellij

The file is `~/.config/zellij/config.kdl`; `zellij setup --dump-config` prints the full default set, and `zellij setup --check` validates your edits. Every key here is catalogued in the [Zellij upstream reference](../externals/mux-adapter/zellij-reference.md#configuration).

[examples/zellij/config.kdl](../../examples/zellij/config.kdl) is this whole baseline — every block below plus the `tokyo-night` theme — as one file, since Zellij reads a single config. Starting fresh, copy it; with an existing `config.kdl`, lift the blocks you want. Unlisted keys keep Zellij's defaults either way:

```sh
cp examples/zellij/config.kdl ~/.config/zellij/config.kdl   # from the rimz checkout
zellij setup --check
```

#### Essential

These settings make a coding-agent session behave correctly — long output stays readable, the keyboard reaches the agent, and copied text lands where you expect.

```kdl
default_mode "locked"                  // hand typing straight to the agent; Ctrl+g enters Zellij
scroll_buffer_size 100000              // keep long agent output scrollable
mouse_mode true                        // scroll, select, and resize with the mouse
copy_on_select true                    // selecting text copies it
copy_clipboard "system"                // yank into the OS clipboard
support_kitty_keyboard_protocol true   // Shift+Enter and friends reach TUI agents
```

`default_mode "locked"` is the one that matters most: locked mode passes ordinary keystrokes to the focused pane, so an agent's TUI gets your input until you deliberately press `Ctrl+g` for a Zellij mode. Rimz already opens its room in locked mode; setting it here keeps your own sessions consistent and your muscle memory intact.

Clipboard travels over OSC52 by default, so yanking works through SSH to your local clipboard. A terminal that needs a helper can set one explicitly:

```kdl
// copy_command "wl-copy"      // Wayland;  "xclip -selection clipboard" on X11;  "pbcopy" on macOS
```

Zellij inherits true color from the terminal it runs in, so there is no color flag to set — pick a terminal with 24-bit color and Zellij and your agents render in full color.

#### Recommended

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

`pane_frames true` draws a titled border around each pane so you can always see which one holds focus — the single most useful upgrade for a multi-agent layout. Rimz enforces its room's mouse pair through the presence plugin, so your personal `focus_follows_mouse` and `mouse_click_through` settings no longer break single-click sidebar jumps.

Rimz rooms let Zellij split the focused pane along its longer visual edge when you open a new pane, and closing that pane returns the space to its split sibling.

#### Alt chords in locked mode

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

#### A note on resurrection

Rimz disables Zellij session serialization inside its room, because it owns rebirth: when a room must come back after a reboot or crash, Rimz re-seeds the prior agents itself ([resume on rebirth](../internals/sidebar/sidebar.md#resume-on-rebirth)) rather than resurrecting a wall of suspended command panes. Setting `session_serialization false` in your own `config.kdl` gives non-Rimz sessions the same clean-birth posture when you prefer running panes over resurrection.

Rimz also disables Zellij's session metadata loop inside its room. At roughly 100 panes on Zellij 0.44.3 that loop rewrites `session-metadata.kdl` every few seconds and runs process discovery through `ps`, a visible share of the Zellij server CPU; Rimz starts and attaches rooms with `disable_session_metadata true` so that work stays out of the room.

### tmux

The file is `~/.tmux.conf` (or `~/.config/tmux/tmux.conf`); reload it with `tmux source-file ~/.tmux.conf` or the `prefix` + `r` binding below. Every option here is catalogued in the [tmux upstream reference](../externals/mux-adapter/tmux-reference.md#options).

Everything below ships as four self-contained modules under [examples/tmux/](../../examples/README.md#tmux--tmux) — [`agents.conf`](../../examples/tmux/agents.conf) (the essentials), [`quality-of-life.conf`](../../examples/tmux/quality-of-life.conf) (copy-mode, window names, splits), [`zellij-keys.conf`](../../examples/tmux/zellij-keys.conf) (the parity chords), and [`theme-tokyonight.conf`](../../examples/tmux/theme-tokyonight.conf) (frames and status bar) — so your `~/.tmux.conf` stays yours and adopts by reference:

```sh
# From the rimz checkout: source the modules you want; drop any line.
printf 'source-file %s\n' "$PWD"/examples/tmux/{agents,quality-of-life,zellij-keys,theme-tokyonight}.conf >> ~/.tmux.conf
tmux source-file ~/.tmux.conf
```

#### Essential

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
set -ga terminal-features ",*:sync"   # atomic redraws for tmux sessions outside Rimz
set -g  focus-events on          # editors and agents see focus changes
set -g  allow-passthrough on     # let desktop notifications pass through tmux
set -s  extended-keys on             # distinguish modified Enter from Enter
set -s  extended-keys-format csi-u   # forward Shift+Enter / Alt+Enter as CSI-u
set -ga terminal-features "*:extkeys" # ask the outer terminal to send them
bind-key -n S-Enter send-keys Escape "[13;2u"
bind-key -n M-Enter send-keys Escape "[13;3u"
set -g  set-clipboard on         # yank into the host clipboard over OSC52
```

`escape-time 0` removes the lag that otherwise makes `Esc` feel sticky in any full-screen TUI. Synchronized output (DECSET 2026), paired with `escape-time 0`, lets tmux buffer a frame's writes and forward them to your terminal as one atomic redraw, so rapid repaints such as Rimz's animated pixel pets and full-screen TUIs never show a half-painted frame; Rimz applies it inside the room, and the config line above gives your own tmux sessions the same behavior. `extended-keys`, `extended-keys-format csi-u`, and `terminal-features … extkeys` let an agent's composer receive Shift+Enter and Alt+Enter as CSI-u soft newlines while plain Enter still submits; the explicit `S-Enter` / `M-Enter` binds make those keys reach agents that do not request modifyOtherKeys themselves. On tmux 3.5.x, this trades clean multiline clipboard paste while extended keys are active; use Ctrl+J, `\`+Enter, or tmux 3.6+ for both modified-Enter newlines and clean paste. `set-clipboard on` carries a yank out through OSC52, so you can copy agent output to your local clipboard even over SSH. `allow-passthrough on` lets the desktop-notification bytes Rimz emits reach your terminal ([notifications](../internals/sidebar/notifications.md)).

#### Recommended

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

`pane-border-status top` labels each pane's border with its index and running command, so a grid of agents stays legible at a glance — the closest tmux gets to Zellij's titled frames. Rimz inherits this setting when its [`[tmux] pane_border_status`](../reference/configuration.md#multiplexer-room-options) override is unset; when you set that override, Rimz titles work panes and blanks the sidebar's own border row. tmux does not draw a pane's outer window edge, so panes are not fully boxed like Zellij frames.

#### Zellij-parity Alt chords (optional)

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

#### A themed status bar (optional)

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

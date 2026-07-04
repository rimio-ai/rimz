# Configure Zellij

Rimz runs inside the Zellij you already use, and sets the room's behavior for you: on every session birth and reattach it applies the options agents need ([installation → configure your multiplexer](./installation.md#configure-your-multiplexer), [configuration → multiplexer room options](../reference/configuration.md#multiplexer-room-options)). Your own `~/.config/zellij/config.kdl` owns everything Rimz leaves alone — your theme, true-color and font rendering, default shell, copy-mode, and keybindings — and it owns your Zellij sessions outside Rimz entirely.

This guide is a baseline that makes Zellij modern and pleasant everywhere. The file lives at `~/.config/zellij/config.kdl`; `zellij setup --dump-config` prints the full default set, and every key here is catalogued in the [Zellij upstream reference](../externals/mux-adapter/zellij-reference.md#configuration).

## Essential

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

Zellij inherits true color from the terminal it runs in, so there is no color flag to set — pick a terminal with 24-bit color (Ghostty, WezTerm, Kitty, Alacritty) and Zellij and your agents render in full color.

## Recommended

These tune the look and feel. None are required; each makes day-to-day work nicer.

```kdl
pane_frames true                       // titled borders mark the focused pane
styled_underlines true                 // colored/curly underlines from agents and editors
osc8_hyperlinks true                   // clickable links in command output
default_shell "zsh"                    // or your shell of choice
theme "dracula"                        // any bundled or custom theme
show_startup_tips false                // skip the startup tip banner
show_release_notes false               // skip the release-notes pane on upgrade
session_serialization false            // prefer clean session births over held resurrection panes
```

`pane_frames true` draws a titled border around each pane so you can always see which one holds focus — the single most useful upgrade for a multi-agent layout. Rimz enforces its room's mouse pair through the presence plugin, so your personal `focus_follows_mouse` and `mouse_click_through` settings no longer break single-click sidebar jumps.

Rimz rooms let Zellij split the focused pane along its longer visual edge when you open a new pane, and closing that pane returns the space to its split sibling.

## A note on resurrection

Rimz disables Zellij session serialization inside its room, because it owns rebirth: when a room must come back after a reboot or crash, Rimz re-seeds the prior agents itself ([resume on rebirth](../internals/sidebar/sidebar.md#resume-on-rebirth)) rather than resurrecting a wall of suspended command panes. Setting `session_serialization false` in your own `config.kdl` gives non-Rimz sessions the same clean-birth posture when you prefer running panes over resurrection.

Rimz also disables Zellij's session metadata loop inside its room. At roughly 100 panes on Zellij 0.44.3 that loop rewrites `session-metadata.kdl` every few seconds and runs process discovery through `ps`, a visible share of the Zellij server CPU; Rimz starts and attaches rooms with `disable_session_metadata true` so that work stays out of the room.

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
focus_follows_mouse true               // focus the pane under the pointer
mouse_click_through true               // a single click both focuses and registers
```

`pane_frames true` draws a titled border around each pane so you can always see which one holds focus — the single most useful upgrade for a multi-agent layout. `focus_follows_mouse` and `mouse_click_through` make a click land on its target the first time; Rimz already sets both inside its room (single-click sidebar jumps depend on them), and setting them here brings the same feel to your own sessions.

## A note on resurrection

Rimz disables Zellij session serialization inside its room, because it owns rebirth: when a room must come back after a reboot or crash, Rimz re-seeds the prior agents itself ([resume on rebirth](../internals/sidebar/sidebar.md#resume-on-rebirth)) rather than resurrecting a wall of suspended command panes. This is scoped to the Rimz session — your own Zellij sessions can keep `session_serialization true` if you rely on resurrection outside Rimz, and the two settings never collide.

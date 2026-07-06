# Examples

Copy-ready configuration and integration samples. The multiplexer configs are the full versions of the baselines walked through in [the setup guide](../docs/guide/setup.md#configure-your-multiplexer).

## tmux — `tmux/`

Four self-contained modules, so you adopt what you want by adding `source-file` lines to your own `~/.tmux.conf` — your config stays yours, and `git pull` updates the modules in place:

| Module | Carries |
| --- | --- |
| [`agents.conf`](./tmux/agents.conf) | the behaviors agent TUIs rely on: true color, no escape lag, long scrollback, focus events, passthrough, OSC52 clipboard, Shift+Enter/Alt+Enter soft newlines |
| [`quality-of-life.conf`](./tmux/quality-of-life.conf) | vi copy-mode with a working first-drag yank, stable window names, current-directory splits along the pane's longer edge |
| [`zellij-keys.conf`](./tmux/zellij-keys.conf) | opt-in no-prefix Alt chords matching Zellij's locked mode; shadows the shell's Alt keys |
| [`theme-tokyonight.conf`](./tmux/theme-tokyonight.conf) | titled pane frames and a Powerline status bar in Rimz's default TokyoNight Night palette; assumes a Nerd Font |

Take everything with one command (works with or without an existing config), then reload:

```sh
printf 'source-file %s\n' "$PWD"/examples/tmux/{agents,quality-of-life,zellij-keys,theme-tokyonight}.conf >> ~/.tmux.conf
tmux source-file ~/.tmux.conf
```

`agents.conf` needs tmux 3.5 or newer — the same floor Rimz enforces. On a machine with an older distro tmux, upgrade first ([installation](../docs/guide/installation.md#prerequisites)).

## Zellij — `zellij/`

[`config.kdl`](./zellij/config.kdl) is a complete starting point: locked-mode-first behavior, the `tokyo-night` theme, and locked-mode Alt chords mirroring the tmux module. Zellij reads one config file, so start fresh with a copy, or lift blocks into an existing config — unlisted keys keep Zellij's defaults either way:

```sh
cp examples/zellij/config.kdl ~/.config/zellij/config.kdl
zellij setup --check
```

## Resolvers — `resolvers/`

Reference resolver implementations for the decision bridge; see [`resolvers/README.md`](./resolvers/README.md).

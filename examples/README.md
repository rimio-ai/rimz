# Examples

Copy-ready configuration and integration samples. The multiplexer configs are the full versions of the baselines walked through in [Zellij and tmux baselines](../docs/guide/multiplexer.md).

## tmux — `tmux/`

Four self-contained modules, so you adopt what you want by adding `source-file` lines to your own `~/.tmux.conf` — your config stays yours, and `git pull` updates the modules in place:

| Module | Carries |
| --- | --- |
| [`agents.conf`](./tmux/agents.conf) | the behaviors agent TUIs rely on: true color, no escape lag, long scrollback, focus events, passthrough, OSC52 clipboard, Shift+Enter/Alt+Enter soft newlines |
| [`quality-of-life.conf`](./tmux/quality-of-life.conf) | vi copy-mode with a working first-drag yank, stable window names, current-directory splits along the pane's longer edge |
| [`zellij-keys.conf`](./tmux/zellij-keys.conf) | opt-in no-prefix Alt chords matching Zellij's locked mode; shadows the shell's Alt keys |
| [`theme-tokyonight.conf`](./tmux/theme-tokyonight.conf) | titled pane frames and a Powerline status bar in RimZ's default TokyoNight Night palette; assumes a Nerd Font |

Take everything with one command (works with or without an existing config), then reload:

```sh
printf 'source-file %s\n' "$PWD"/examples/tmux/{agents,quality-of-life,zellij-keys,theme-tokyonight}.conf >> ~/.tmux.conf
tmux source-file ~/.tmux.conf
```

`agents.conf` needs tmux 3.5 or newer — the same floor RimZ enforces. On a machine with an older distro tmux, upgrade first ([installation](../docs/guide/installation.md#prerequisites)).

## Zellij — `zellij/`

[`config.kdl`](./zellij/config.kdl) is a complete starting point: locked-mode-first behavior, the `tokyo-night` theme, and locked-mode Alt chords mirroring the tmux module. Zellij reads one config file, so start fresh with a copy, or lift blocks into an existing config — unlisted keys keep Zellij's defaults either way:

```sh
cp examples/zellij/config.kdl ~/.config/zellij/config.kdl
zellij setup --check
```

## Forge agent team — `teams/forge/`

[`teams/forge`](./teams/forge/) is one RimZ drop-in fragment for the plan → code → review loop: `@planner` runs Claude, `@coder` runs Codex, and `@reviewer` runs Claude. Its `team.toml` declares the three profiles and the team, and the three Markdown files are the role prompts. The [teams README](./teams/README.md) walks the whole loop: roles, hand-offs, install, and customization.

Install the fragment by copying it into the agents home:

```sh
mkdir -p ~/.agents/teams
cp -r examples/teams/forge ~/.agents/teams/
```

A same-named directory in `~/.agents/teams` is overwritten; remove it first if you want a clean copy. Entries in `~/.config/rimz/agents.toml` override fragment entries with the same names.

Launch with `rimz agents forge`; the launch grammar lives in the [agents CLI reference](../docs/reference/cli/agents.md). Each role answers to `@planner`, `@coder`, or `@reviewer`.

The `claude` and `codex` CLIs must be on `PATH`. The profiles in `team.toml` pin models (`fable`, `opus`) and Codex feature flags; adjust them there to taste. The coder's PR step expects a `pr` skill and falls back to plain `gh` or `tea` without it.

Try the team before installing by pointing RimZ at this checkout:

```sh
RIMZ_AGENTS_HOME="$PWD/examples" rimz agents forge
```

## Third-party agent plugin — `agent-plugin/`

[`agent-plugin`](./agent-plugin/) is a complete ScriptBot process plugin: a manifest, scripted agent, canonical event shim behavior, priced spend probe, account probe, and fixture transcript. Its [README](./agent-plugin/README.md) installs the bundle and launches the demo; the public contract is [agent-plugins.md](../docs/reference/agent-plugins.md).

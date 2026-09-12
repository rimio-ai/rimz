# Examples

Copy-ready configuration and integration samples. The multiplexer configs are the full versions of the baselines walked through in [Zellij and tmux baselines](../docs/guide/multiplexer.md).

## tmux — `tmux/`

Four self-contained modules, so you adopt what you want by adding `source-file` lines to your own `~/.tmux.conf` — your config stays yours, and `git pull` updates the modules in place:

| Module | Carries |
| --- | --- |
| [`agents.conf`](./tmux/agents.conf) | the behaviors agent TUIs rely on: true color, reliable Escape handling, long scrollback, focus events, passthrough, OSC52 clipboard, Shift+Enter/Alt+Enter soft newlines |
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

## Agent teams — `teams/`

Three RimZ drop-in team fragments, one per shape of work: [`forge`](./teams/forge/) plans, builds, and reviews a change worth designing first; [`mill`](./teams/mill/) puts an architect in the lead for a refactor that has to remove surface; [`spot`](./teams/spot/) drops the design stage entirely for a small fix. Each directory is a `team.toml` declaring the roles, layout, pipeline stages, git-excluded scratch files, and a `ci.failed` → `coder` signal binding, plus one Markdown prompt per role. Their `blackboard.md` carries the Stage line and append-only Progress history; forge's first `rimz teams flip <stage> "<progress note>"` opens the board and records each later hand-off. The [teams README](./teams/README.md) walks all three: pipelines, hand-offs, install, and customization.

Install a release-matched bundle from GitHub:

```sh
rimz teams install forge
rimz teams install spot
```

From a repository checkout, copying remains the local-edit alternative:

```sh
mkdir -p ~/.agents/teams
cp -r examples/teams/forge ~/.agents/teams/
```

`rimz teams install forge --force` replaces files in a same-named installed directory; the plain install preserves it. Entries in `~/.config/rimz/agents.toml` override fragment entries with the same names.

Launch with `rimz teams forge -w feat-x`; the lifecycle grammar lives in the [teams CLI reference](../docs/reference/cli/teams.md). Each role answers to its role handle — `@planner`, `@architect`, `@coder`, `@reviewer`. The signal binding is armed when the coder registers, scoped to its worktree, and retired with that session. Failed CI delivers a `Type: SIGNAL` message directly to the coder, not whoever pushed. Without an explicit branch/path match, launching this binding on the root checkout is refused; use `-w` or launch from a linked worktree. `rimz teams show forge#feat-x` separates declared bindings from live subscriptions.

The `claude` and `codex` CLIs must be on `PATH`, for all three teams. Each `team.toml` pins its models (`fable`, `opus`, the current GPT) and Codex feature flags; adjust them there to taste. The prompts also name helper skills that are not shipped here, `pr` among them, and state the outcome alongside each, so a role without one falls back to plain `git` or `gh`.

Try a team before installing by pointing RimZ at this checkout:

```sh
RIMZ_AGENTS_HOME="$PWD/examples" rimz teams forge -w feat-x
```

## Third-party agent plugin — `agent-plugin/`

[`agent-plugin`](./agent-plugin/) is a complete ScriptBot process plugin: a manifest, scripted agent, canonical event shim behavior, priced spend probe, account probe, and fixture transcript. Its [README](./agent-plugin/README.md) installs the bundle and launches the demo; the public contract is [agent-plugins.md](../docs/reference/agent-plugins.md).

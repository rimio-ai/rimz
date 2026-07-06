<div align="center"><pre>
  ██████╗ ██╗███╗   ███╗  ███████╗
  ██╔══██╗██║████╗ ████║  ╚══███╔╝
 ██████╔╝██║██╔████╔██║    ███╔╝
██╔══██╗██║██║╚██╔╝██║   ███╔╝
  ██║  ██║██║██║ ╚═╝ ██║  ███████╗
  ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝  ╚══════╝
  The control room for your coding agents
</pre></div>

<p align="center"><strong>agents fleet · harness dashboard · programmable · local & remote · tmux & zellij · token insight</strong></p>

<p align="center">
  <a href="https://github.com/rimio/rimz/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/rimio/rimz/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://crates.io/crates/rimz"><img alt="crates.io" src="https://img.shields.io/crates/v/rimz.svg"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
</p>

<p align="center">
  <a href="#get-started">Get started</a> ·
  <a href="#what-it-does">What it does</a> ·
  <a href="#everyday-moves">Everyday moves</a> ·
  <a href="#agent-compatibility-matrix">Agents</a> ·
  <a href="#documentation">Docs</a> ·
  <a href="#install">Install</a>
  <!-- · website · Discord · llms.txt - join here when live -->
</p>

<p align="center"><sub><b>AI agents / LLMs:</b> read <a href="AGENTS.md"><code>/AGENTS.md</code></a>.<!-- llms.txt and the hosted docs index land here --></sub></p>

---

Rimz is a realtime dashboard for harnessing agentic coding: one human and tens of agents working together in one zellij or tmux room, where everything about every agent reads at a glance. Every agent gets a live card (state, task, context health, live cost), and the sidebar routes your attention to whichever one needs you.

Rimz is also the substrate for harness and loop engineering: one uniform interface across Claude Code, Codex, Pi, and OpenCode, a durable message system for steering and queuing agents, supervised runs with exit codes for scripts and CI, and scheduled loops that keep the fleet working on a clock. All of it is a single lightweight binary inside the tmux or zellij you already run.

<p align="center">
  <img src="docs/rimz-full.png" alt="A Rimz room: the sidebar triaging a fleet of coding agents beside their panes" width="100%">
  <br/><sub>The sidebar triages the fleet on the left; agents work in their own panes.</sub>
</p>

## What it does

- **Realtime Harness Dashboard:** working state and task, model and effort, context health and compactions, token mix down to cache reads, live dollar cost, and the subagent tree
- **Attention, Routed:** one glance at the cockpit line (`? 2  ! 1 …`) reads the whole fleet, the column below arrives already triaged, and one keypress drops you into the pane that is waiting
- **Know Your Pace:** $ and token insight for today, week, and month, with every provider's plan and 5h/7d budget bars draining in real time; one look tells you where the week is going
- **Extremely Lightweight:** a single binary that wraps the harnesses you already run (Claude Code, Codex, Pi, OpenCode) inside your familiar zellij or tmux: same keybinds, same terminal, zero learning curve, and the official web, desktop, and mobile apps all keep working
- **Local or Remote, Continuously:** start the room on your macbook or a server, close the laptop, and reattach from anywhere; the link heals itself, and even a reboot brings the room back with layout and agents resumed
- **Worktrees, for every Agent:** open agents together, side by side in an isolated worktree with dynamic layout: `claude,codex` starts Claude planning beside Codex reviewing, `vim,codex+term` puts your editor, an agent, and a shell in one tab
- **Messages, agents chat as in Slack:** agents message each other and you by handle (`@codex#feat-a`), with steer/queue delivery that respects agent state and the context window; `rimz message` is the same surface for you and for scripts
- **Scriptable, End to End:** `rimz agents -p` is `claude -p` for every agent, with exit codes, JSON output, streaming, and full observability, so agents drop into scripts, CI, and workflows
- **Auto Continue, while you're Away:** agents keep working after you step away: a rate-limit pause resumes the moment the budget window resets, transient API errors retry on a backoff ramp, and a full context window compacts before the next prompt lands
- **Pets, your beloved Companion:** an opt-in animated sprite on the provider dashboard that follows the fleet's state, rendered as pixels where the terminal supports them and cell art everywhere else

## How it works

```
 terminal — ghostty · iterm2 · warp · kitty · vscode …
   zellij or tmux — your keybinds, your layout ...

     ┌─────────┐   ┌────────────────────────────────────────┐
     │ sidebar │   │  claude · codex · pi · opencode agents │
     └────▲────┘   └──────────────┬─────────────────────────┘
          │ renders               │ hooks · transcripts (.jsonl) · oauth api
          │                       │ statusline (claude) · app-server (codex) · extensions (pi/opencode)
          │                       │ ...
          │                       ▼
          └─────────────────────  rimz  ◀──  git status · /proc stats
```

- **Agents report themselves:** sessions, tool calls, live status, and blocking questions arrive the moment they happen, through each agent's own hooks, transcripts, and APIs
- **Rimz fuses every channel:** agent events, git churn, process stats, and account state combine into one live picture, and the sidebar renders it

→ [DESIGN.md](./DESIGN.md) · [ARCHITECTURE.md](./ARCHITECTURE.md)

## Get started

```sh
# 1 — Install
cargo install --locked rimz                 # or Homebrew (see Install)

# 2 — Open the room
cd ~/code/query-engine
rimz

# 3 — Launch agents and work; the sidebar surfaces whoever needs you
claude                                      # or: codex, pi

# 4 — Worktrees, teams, dynamic layout
rimz agents claude,codex --worktree=feat/x       # Claude + Codex, side by side
rimz agents 'vim,claude+term' --worktree=feat/y  # editor, agent, shell in one tab
rimz agents peer --worktree=feat/z               # a saved agent team

# 5 — Native SSH remote, with self-healing reconnect
rimz remote connect dev-box:~/code/query-engine
```

Hooks install on the first `rimz` run, with your consent and a diff preview. → [the product tour](./docs/guide/product.md)

## Everyday moves

**Launch agents in layouts.** One spec describes the shape: commas split columns, `+` tiles rows, `/` stacks them, and a trailing prompt broadcasts to every agent in it.

```sh
rimz agents claude,codex "Refactor token refresh; keep the public API stable."
rimz agents 'vim,codex+term'          # editor | agent stacked over a shell
rimz agents claude/codex/term         # one Zellij stack; tmux tiles rows
```

**Give each task its own worktree.** `--worktree` launches into an isolated Rimz-owned Git worktree; the tab is named after it and becomes the agents' channel.

```sh
rimz agents claude,codex --worktree=feat/x            # isolate a feature branch
rimz agents claude --worktree "Take one approach."    # parallel attempts,
rimz agents claude --worktree "Take another one."     # each in a fresh worktree
rimz agents codex --from-pr 42 "Review this pull request."
```

**Save the layouts you reuse as teams.** A named team in `agents.toml` gives each role a handle; launch it whole, or re-add one role to the running team.

```sh
rimz agents peer --worktree=feat/x    # built-in team: claude,codex side by side
rimz agents pcr                       # your own team: planner, coder, reviewer
rimz agents pcr.reviewer              # re-add one role, same handle and lane
```

**Pick up where you left off.** `--resume` reopens the newest closed session, cohort, or team matching the spec, and a room reborn after a reboot offers every prior agent back.

```sh
rimz agents claude --resume           # resume the freshest closed Claude session
rimz agents pcr --resume              # reopen the team, every role restored
rimz agents pcr -w feat/x --resume    # that exact worktree's team
rimz start                            # after a reboot: offers the whole room back
```

**Message agents like teammates.** Every agent gets a handle, `@codex#feat-a` style, named by kind and scoped by its worktree or channel. The default delivery parks until the agent's turn ends; `--steer` interrupts now; `--schedule` sets the delivery time.

```sh
rimz message @claude "add coverage for the expiry edge cases"    # lands at the turn boundary
rimz message --steer @claude "stop: the parser test comes first" # lands now
rimz message --schedule 60m @codex#feat-b "run the smoke test"
rimz message @all "summarize what changed when you reach a boundary"
rimz message                                                     # the current lane's inbox
```

**Script an agent like a CLI.** `-p` runs one supervised turn and exits with the run's status code, so a script or CI job branches on the outcome.

```sh
rimz agents codex "Prepare the release checklist." -p --timeout 30m --output-format json
cat build-error.txt | rimz agents claude -p 'explain the root cause' > out.txt
rimz agents claude "Run the migration audit." -p --detach   # prints a pet name, returns
rimz agents wait swift-otter --stream                       # block on it later
```

**Keep the fleet moving while you sleep.** Scheduled pings start a provider's budget window on your clock, and check-guarded loops watch CI or tests and wake an agent on the result. Auto-continue (`[resume] auto_continue`) and smart compaction (`[harness] smart_compact`) switch on in config; the [setup guide](./docs/guide/setup.md#keep-the-fleet-moving) walks all four.

```sh
rimz loop add morning --spec claude-ping --at 07:00 --days weekdays   # prime the 5h window
rimz loop add watchdog --check "cargo test" --on fail \
    --spec codex --prompt "fix the failing test" --every 15m
```

**Work from anywhere.** A room is plain zellij or tmux under SSH: save an alias, connect with self-healing reconnect, or open the room in a browser.

```sh
rimz remote add dev dev-box:~/code/query-engine
rimz remote connect dev          # the room rebuilds, every agent where you left it
rimz remote connect dev --web    # the same room, tunneled to your local browser
rimz web open                    # a local Zellij room in the browser
```

## Configuration

Rimz runs with zero configuration; one pass makes it yours. `rimz setup` writes the per-machine defaults under `~/.config/rimz/` (`config.toml`, `theme.toml`, `agents.toml`, `loop.toml`), every key shipped commented with its default and an inline note.

```sh
rimz setup                                  # detect the machine, write default config
rimz config set theme "Catppuccin Mocha"    # edit one key; Rimz routes it to the owning file
```

The [setup guide](./docs/guide/setup.md) covers the first pass end to end: agent hooks, true color, pets, the hands-off loop behaviors, and a modern zellij/tmux baseline with [ready-to-adopt example configs](./examples/README.md). The full key catalog is the [configuration reference](./docs/reference/configuration.md).

## Agent compatibility matrix

| Agent       | Status | Integration                                                       |
|-------------|:------:|-------------------------------------------------------------------|
| Claude Code | ✅     | hooks · statusline · `.jsonl` transcripts · `claude --resume`     |
| Codex       | ✅     | hooks + `notify` · app-server · rollout `.jsonl` · `codex resume` |
| Pi          | beta   | extension API · session `.jsonl` · `pi --session`                 |
| OpenCode    | alpha  | extension API · session `.jsonl`                                  |

Adapters are thin layers over the same hook and transcript primitives ([agents internals](./docs/internals/agents/agent.md)); the agents run stock, in your terminal, with the official apps untouched.

## Documentation

- [Product tour](./docs/guide/product.md) — the room, the loop, and the scenarios people run, local fleet to scripted pipeline
- [Set up your machine](./docs/guide/setup.md) — config, hooks, true color, pets, and the zellij/tmux baselines
- [Attention](./docs/guide/attention.md) — how the sidebar decides what needs you
- [Security](./docs/guide/security.md) — threat model and guardrails
- [The sidebar on screen](./docs/interface/sidebar.md) — rendered frames and the glyph legend
- [CLI reference](./docs/reference/cli.md) · [Configuration](./docs/reference/configuration.md) · [Theming](./docs/reference/theme.md)
- [DESIGN.md](./DESIGN.md) · [ARCHITECTURE.md](./ARCHITECTURE.md) · [internals](./docs/internals/) — how it works, in depth

## Install

```sh
cargo install --locked rimz     # from crates.io

# or via Homebrew — tap once, then install:
brew tap rimio/homebrew-rimz
brew install rimz
```

zellij (0.44+) or tmux (3.5+) runs the room; `rimz doctor` confirms your build clears the floor. Building from source is `git clone … && cargo xtask install`; prerequisites and the pinned toolchain live in [the installation guide](./docs/guide/installation.md).

Hooks are how agents report to the room. The first `rimz` run offers to install them with a diff preview, and `rimz hooks install` does the same on demand:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install              # every detected agent (claude, codex, pi, opencode)
rimz doctor                     # verify backend, hooks, and room health
```

The install is additive (your existing hooks stay), and `rimz hooks uninstall` undoes it. Rimz is pre-release: the agent adapters, both multiplexer backends, and the sidebar are implemented in-tree.

## Contributing

```sh
git clone https://github.com/rimio/rimz.git && cd rimz
cargo xtask install     # build and install the binary to Cargo bin + /usr/local/bin
cargo xtask test        # the nextest suite
cargo xtask ci          # non-test checks + the plain nextest suite
```

Contributor rules and the gate stack live in [rust-conventions.md](./docs/contributing/rust-conventions.md); the working contract is [AGENTS.md](./AGENTS.md).

## License

MIT. See [LICENSE](./LICENSE).

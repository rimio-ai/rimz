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

<!-- badges: CI · coverage · crates.io · license - land here when published -->

<p align="center">
  <a href="#get-started-60-seconds">Get started</a> ·
  <a href="#what-it-does">What it does</a> ·
  <a href="#agent-compatibility-matrix">Agents</a> ·
  <a href="#documentation">Docs</a> ·
  <a href="#install">Install</a>
  <!-- · website · Discord · llms.txt - join here when live -->
</p>

<p align="center"><sub><b>AI agents / LLMs:</b> read <a href="AGENTS.md"><code>/AGENTS.md</code></a>.<!-- llms.txt and the hosted docs index land here --></sub></p>

---

Rimz is a realtime dashboard for harnessing agentic coding: one human and tens of agents working together in one zellij or tmux room, where everything about every agent reads at a glance. Every agent gets a live card (state, task, context health, live cost), and the sidebar routes your attention to whichever one needs you.

<p align="center">
  <img src="docs/rimz-full.png" alt="A Rimz room: the sidebar triaging a fleet of coding agents beside their panes" width="100%">
  <br/><sub>One room: the sidebar triages the fleet on the left, agents work in their own panes, the provider dashboard tracks plan and spend below.</sub>
</p>

## What it does

- **Realtime Harness Dashboard:** working state and task, model and effort, context health and compactions, token mix down to cache reads, live dollar cost, and the subagent tree
- **Attention, Routed:** one glance at the cockpit line (`? 2  ! 1 …`) reads the whole fleet, the column below arrives already triaged, and one keypress drops you into the pane that is waiting
- **Know Your Pace:** $ token insight for today, week, and month, with every provider's plan and 5h/7d budget bars draining in real time; one look tells you where the week is going
- **Extremely Lightweight:** a single binary that wraps the harnesses you already run (Claude Code, Codex, Pi) inside your familiar zellij or tmux: same keybinds, same terminal, and the official web, desktop, and mobile apps all keep working
- **Local or Remote, Continuously:** start the room on your macbook or a server, close the laptop, and reattach from anywhere; the link heals itself, and even a reboot brings the room back with layout and agents resumed
- **Worktrees, for every Agent:** open agents together, side by side in the same worktree with dynamic layout. For example use `claude,codex` to start Claude planning and Codex reviewing for agentic peer programming, or use `vim,codex+term` to start editor, agent and terminal side by side.
- **Scriptable and Steerable:** the `claude -p` you missed, brought back as `rimz agents -p`. Plus `steer` and `queue` which add dynamic control over agent harness, easily integrate agents into your scripts, your CI, your workflow with observability
- **Auto Recover, while you're Away:** agents keep working after you step away. A rate-limit park resumes the moment the window resets, API hiccups recover on their own, context compacts along the way, and routine questions fall to a resolver chain that always ends with you

## How it works

```
 terminal — ghostty · iterm2 · warp · kitty · vscode …
   zellij or tmux — your keybinds, your layout ...

     ┌─────────┐   ┌─────────────────────────────┐
     │ sidebar │   │  claude · codex · pi agents │
     └────▲────┘   └──────────────┬──────────────┘
          │ renders               │ hooks · transcripts (.jsonl)
          │                       │ statusline (claude) · app-server (codex)
          │                       │ ...
          │                       ▼
          └─────────────────────  rimz  ◀──  git status · /proc stats
```

- **Agents report themselves:** sessions, tool calls, completions, and blocking questions arrive through each harness's own hooks and transcripts, the moment they happen
- **Rimz fuses every channel:** agent events, git churn, and process stats combine into one live picture, and the sidebar renders it

→ [DESIGN.md](./DESIGN.md) · [ARCHITECTURE.md](./ARCHITECTURE.md)

## Get started

```sh
# 1 — Install
cargo install rimz                          # or: brew install rimz

# 2 — Open the room
cd ~/code/query-engine
rimz

# 3 — Launch agents and work; the sidebar surfaces whoever needs you
claude                                      # or: codex, pi

# 4 — Worktrees, dynamic layout
rimz agents claude,codex --worktree=feat/x      # Claude + Codex, side by side
rimz agents 'vim,claude+term' --worktree=feat/y # vim beside a stacked Claude and shell

# 5 — Step away; everything keeps running
#     zellij: Ctrl-O d · tmux: prefix d
rimz remote connect dev-box:query-engine    # come back from any machine
```

Hooks install at first time running `rimz`, with your consent and a diff preview. → [the product tour](./docs/guide/product.md)

## Everyday moves

**The room, from anywhere.** Saved aliases carry the target and reconnect defaults, and after a reboot the room comes back populated, with every prior agent re-seeded idle in its own pane via `claude --resume`, `codex resume`, or `pi --session`.

```sh
rimz remote add dev dev-box:~/code/query-engine
rimz remote connect dev        # the room rebuilds, every agent where you left it
```

**Two agents, one feature.** Built-in and inline layouts compose any grid, the sidebar groups cards by worktree with per-tree diff churn, and a worktree is removed only after its work proves landed on the base branch.

```sh
rimz agents peer --worktree=feat/great    # customizable layout alias, peer for claude,codex
rimz agents 'vim,codex+term'              # or layout dynamically
```

**Pipelines, steering, and queues.** A run that stops to ask survives the stop: the question takes the normal path to your sidebar and your phone, you answer from anywhere, and the run finishes while the script is still blocking.

```sh
rimz agents codex --worktree=deps --timeout 4h -p "update dependencies, run the suite, open a PR"
rimz steer @claude -- "focus on the failing parser test"
rimz queue @codex --on done -- "open a PR summary"
```

→ [the four scenarios, in full](./docs/guide/product.md)

## Agent compatibility matrix

| Agent       | Status | Integration                                                       |
|-------------|:------:|-------------------------------------------------------------------|
| Claude Code | ✅     | hooks · statusline · `.jsonl` transcripts · `claude --resume`     |
| Codex       | ✅     | hooks + `notify` · app-server · rollout `.jsonl` · `codex resume` |
| Pi          | ✅     | extension API · session `.jsonl` · `pi --session`                 |

Adapters are thin layers over the same hook and transcript primitives ([agents internals](./docs/internals/agents/agent.md)); the agents run stock, in your terminal, with the official apps untouched.

## Install

```sh
cargo install rimz          # or: brew install rimz
```

zellij or tmux runs the room. Building from source is `git clone … && cargo xtask install`; prerequisites, the pinned toolchain, and the wasm target live in [the installation guide](./docs/guide/installation.md). Rimz is pre-release: the Claude Code, Codex, and Pi adapters, both multiplexer backends, and the sidebar are implemented in-tree.

## Contributing

```sh
git clone https://github.com/rimz/rimz.git && cd rimz
cargo xtask install     # build and install the binary + zellij presence plugin
cargo xtask test        # the nextest suite
cargo xtask ci          # the full gate stack
```

Contributor rules and the gate stack live in [rust-conventions.md](./docs/contributing/rust-conventions.md); the working contract is [AGENTS.md](./AGENTS.md).

## License

MIT. See [LICENSE](./LICENSE).

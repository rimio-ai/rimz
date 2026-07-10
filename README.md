<div align="center"><pre>
  ██████╗ ██╗███╗   ███╗  ███████╗
  ██╔══██╗██║████╗ ████║  ╚══███╔╝
 ██████╔╝██║██╔████╔██║    ███╔╝
██╔══██╗██║██║╚██╔╝██║   ███╔╝
  ██║  ██║██║██║ ╚═╝ ██║  ███████╗
  ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝  ╚══════╝
  The control room for your coding agents
</pre></div>

<p align="center"><strong>agent fleet · harness dashboard · loops · local & remote · tmux & zellij · token insight</strong></p>

<p align="center">
  <a href="https://github.com/rimio-ai/rimz/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/rimio-ai/rimz/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://crates.io/crates/rimz"><img alt="crates.io" src="https://img.shields.io/crates/v/rimz.svg"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
</p>

<p align="center">
  <a href="#get-started">Get started</a> ·
  <a href="#what-it-does">What it does</a> ·
  <a href="#project-status">Status</a> ·
  <a href="#everyday-moves">Everyday moves</a> ·
  <a href="#configuration">Configuration</a> ·
  <a href="#agent-compatibility-matrix">Agents</a> ·
  <a href="#documentation">Docs</a> ·
  <a href="#install">Install</a>
  <!-- · website · Discord · llms.txt - join here when live -->
</p>

<p align="center"><sub><b>AI agents / LLMs:</b> read <a href="AGENTS.md"><code>/AGENTS.md</code></a>.<!-- llms.txt and the hosted docs index land here --></sub></p>

---

RimZ is a realtime dashboard for harnessing agentic coding: one human and tens of agents working together in one Zellij or tmux room, where everything about every agent reads at a glance. Every agent gets a live card (state, task, context health, live cost), and the sidebar routes your attention to whichever one needs you.

<p align="center">
  <img src="docs/rimz-full.png" alt="A RimZ room: the sidebar triaging a fleet of coding agents beside their panes" width="100%">
  <br/><sub>The sidebar triages the fleet on the left; agents work in their own panes.</sub>
</p>


RimZ stays out of your way: a single lightweight binary inside the Zellij or tmux you already run, with your keybinds intact, the agent CLIs stock, and the official web, desktop, and mobile apps untouched. The same footprint carries the primitives that **harness engineering** and **loop engineering** build on: the sidebar is the observability layer, one uniform interface reaches Claude Code, Codex, Copilot, Gemini CLI, Pi, OpenCode, and Cursor, a durable message system steers and queues agents, supervised runs carry exit codes into scripts and CI, and scheduled wakeups keep the fleet on a clock. The harness itself (guardrails, policies, self-running loops) is yours to build on those primitives.

## Project status

RimZ is **alpha software**, moving fast. Expect rough edges and the occasional bug, and expect the surface to shift: commands, flags, config keys, and output formats can change between releases while the design settles.

It is also heavily used, on itself. RimZ is built with RimZ: the fleet behind this repository routinely runs 50–100 concurrent agents across 10–30 parallel worktrees and PRs, and a single room stays responsive with 100+ agents from multiple providers working at once. Driven through teams and loops, most of the routine engineering here — features, bug fixes, CI repair — already flows through the harness with little hand-holding.

Read that as: ready for personal, daily use today; for production workflows that need a stable interface, wait for the 1.0 release.

## What it does

<p align="center">
  <img src="docs/rimz-gallery.png" alt="" width="100%">
  <br/><sub>Realtime harness dashboard, with rich information at a glance</sub>
</p>

- **Realtime Harness Dashboard:** working state and task, model and effort, context health and compactions, live token stats and dollar cost, and the subagent tree
- **Attention, Routed:** one glance at the cockpit line (`? 2  ! 1 …`) reads the whole fleet, the column below arrives already triaged, and one keypress drops you into the pane that is waiting
- **Know Your Pace:** $ and token insight for today, week, and month, with every provider's plan and 5h/7d budget bars draining in real time; one look tells you where the week is going
- **Worktrees, for every Agent:** open agents together, side by side in an isolated worktree with dynamic layout: `claude,codex` starts Claude planning beside Codex reviewing, `vim,codex+term` puts your editor, an agent, and a shell in one tab
- **Messages, agents chat as in Slack:** every agent answers to a handle (`@codex`, `@planner`); steer/queue delivery guarantees the message lands, respecting agent state and the context window, and agents talk to each other and to you inside channels
- **Scriptable, End to End:** `rimz agents -p` is `claude -p` for every agent, with exit codes, JSON output, streaming, and the full transcript kept, so agents drop into scripts, CI, and workflows
- **Loops, Yours to Engineer:** `rimz loop` schedules supervised runs on a clock (calendar, interval, cron, or a check-guarded watchdog that runs a command and wakes an agent on the result), and notification handlers run your own command the moment a row needs eyes
- **Auto Continue, while you're Away:** a rate-limit pause resumes the moment the budget window resets and transient API overload retries on a backoff ramp; agents recover themselves and keep working while you're gone
- **Answer from your Phone:** official remote control puts your terminal sessions in the Claude and ChatGPT mobile apps: kick off a task at your desk, answer its questions from your phone, come back and review in the pane; one seamless session, end to end
- **Pets, your beloved Companion:** an animated sprite on the provider dashboard that keeps you company, running while the agents run and waving when one waits
- **Local or Remote, Continuously:** start on your MacBook or a server, close the laptop, and reattach from anywhere; the link heals itself every time you reconnect
- **Extremely Lightweight:** a single binary that hooks the agents you already run, inside your familiar Zellij or tmux: same keybinds, same terminal, zero learning curve; all the official web, desktop, and mobile apps keep working

## How it works

```
 terminal — ghostty · iterm2 · warp · kitty · vscode …
   zellij or tmux — your keybinds, your layout

     ┌─────────┐       ┌──────────────────────────────────────────────────┐
     │ sidebar │       │ claude · codex · copilot · gemini · pi · opencode │
     └────▲────┘       └────▲────────────────────┬────────────────────────┘
          │                 │                    │
          │ renders         │ types into panes   │ hooks · transcripts (.jsonl) · oauth api
          │ the fleet       │ messages · -p runs │ statusline (claude) · app-server (codex)
          │                 │                    │ extensions (pi/opencode) · …
          │                 │                    ▼
          └─────────────────┴──────────────────  rimz  ◀──  git status · /proc stats
```

- **Agents report themselves:** sessions, tool calls, live status, and blocking questions arrive the moment they happen, through each agent's own hooks, transcripts, and APIs
- **RimZ drives the panes:** messages, steering, and `-p` harness runs land as keystrokes in the agent's own pane, so every agent runs its stock CLI in a full terminal, exactly as if you typed
- **RimZ fuses every channel:** agent events, git churn, process stats, and account state combine into one live picture, and the sidebar renders it

→ [DESIGN.md](./DESIGN.md) · [ARCHITECTURE.md](./ARCHITECTURE.md)

## Get started

```sh
# 1 — Install
cargo install --locked rimz                 # or Homebrew (see Install)

# 2 — Open the room
cd ~/code/query-engine
rimz

# 3 — Launch agents and work; the sidebar surfaces whoever needs you
claude
codex

# 4 — Worktrees, dynamic layouts, agent teams
rimz agents claude,codex --worktree=feat-x       # Claude + Codex, side by side
rimz agents 'vim,claude+term' --worktree=feat-y  # editor, agent, shell in one tab

# 5 — Native SSH remote, with self-healing reconnect
rimz remote connect dev-box:~/code/query-engine
```

Hooks install on the first `rimz` run, with your consent and a diff preview. → [set up your machine](./docs/guide/setup.md) · [enable dynamic shell completion](./docs/guide/setup.md#shell-completion)

## Everyday moves

The commands below run from any pane in the room, and from any script or CI job that reaches it. They compose: a profile becomes a team, the team lands in a worktree, the worktree's agents take messages, and a schedule fires the whole thing while you sleep.

**Start agents by name.** Type `claude` in any pane and it joins the room: the stock CLI with your flags and its own session files, no `rimz` command in the path.

```sh
claude
codex
```

[`rimz agents`](./docs/guide/agents.md) earns its keystrokes when you want more than the default. A `-auto` or `-yolo` suffix sets the permission mode, and a profile in `agents.toml` pins a model, effort, system prompt, and launch args behind one word, so a planner that reasons hard and edits nothing is a name you reuse.

```sh
rimz agents claude          # stock agent, own pane
rimz agents codex-yolo      # permission modes: -auto, -ask, -plan, -yolo
rimz agents planner         # your profile: model, effort, system prompt
```

**Launch layouts into worktrees.** One spec describes the shape: `,` splits, `+` tiles, `/` stacks. Add [`-w`](./docs/guide/worktrees.md) and the whole layout lands in an isolated RimZ-owned Git worktree, seeded with the untracked files it needs, so two lines of work run side by side without touching each other or your main checkout.

```sh
rimz agents claude,codex -w feat-a             # two agents, side by side
rimz agents planner,coder+reviewer -w feat-b   # profiles compose like kinds
rimz agents 'vim,codex+term' -w feat-c         # editor | agent stacked over a shell
rimz agents codex --from-pr 42                 # worktree checked out from a pull request
```

**Combine models as teams.** A named [team](./docs/guide/teams.md) in `agents.toml` gives each role a handle and launches the whole set in its layout, each role in its own context window, cooperating over messages. Pair model strengths across providers: one plans, another writes the code, a third reviews the diff blind. RimZ is built this way; `examples/teams/` ships the `forge` team it uses.

```sh
rimz agents claude:planner,codex:coder -w feat-once   # one-off roles without agents.toml
rimz agents forge -w feat-complex   # planner, coder, reviewer on one feature
```

**Message agents like teammates.** Every agent answers to a [handle](./docs/guide/messaging.md), named by kind, profile, or team role: `@codex` reaches the one in your channel, `@codex#feat-a` reaches across the workspace. Every message becomes a durable record, so it lands: parked at the turn boundary by default, `--steer` to interrupt the live turn now, `--schedule` to deliver later. The same command serves you, your scripts, and the agents themselves, which use it to talk to each other.

```sh
rimz message @claude "add coverage for the expiry edge cases"      # parks at the turn boundary
rimz message @planner "draft the implementation plan"              # by profile or team role
rimz message @coder --after @planner "planner's done — read plan.md and start"
rimz message @coder --wait "did the migration land? one line"       # print the reply from this agent's context
rimz message @all --wait --json "status? one line"                   # gather a labeled reply map from the whole channel
rimz message --steer @claude "stop: the parser test comes first"   # lands now
rimz message --schedule 60m @codex#feat-b "run the smoke test"     # lands in an hour
git diff main | rimz message @reviewer --stdin "review this"       # instruction plus stdin context
rimz message @all "summarize what changed at the next boundary"    # the whole channel
```

**Script an agent like any CLI.** [`rimz agents -p`](./docs/guide/scripting.md) is `claude -p` for every agent: one supervised turn, one exit code a script or CI job branches on, the same flag for Claude, Codex, Gemini CLI, Pi, and OpenCode. The turn still runs in a real pane you can watch, answer, and steer while the pipeline waits on it.

```sh
rimz agents codex "Prepare the release checklist." -p --timeout 30m --output-format json
cat build-error.txt | rimz agents claude -p --stdin 'explain the root cause'   # stdin appends to the prompt

rimz agents claude "Run the migration audit." -p --bg       # returns now, prints the run's name
rimz agents wait swift-otter --stream                       # block on it later, tail the answer
rimz agents wait otter fox --any                            # race agents; first to finish wins, prints its name
```

**Run the fleet on a schedule.** [`rimz loop`](./docs/guide/loops.md) fires agent turns on a clock: daily at a set time, on an interval, from a cron line, or once after a delay. Add `--check` and the task becomes a watchdog, running the script first and waking the agent only on its result. A `<kind>-ping` task primes budget windows: a lowest-effort turn starts the provider's window on your clock and skips when one is already counting down. Switch on [auto-continue and smart compaction](#configuration) and the loop runs hands-off; add `--budget 20/day` to bound what hands-off work costs.

```sh
rimz loop add morning --agent claude-ping --prompt ping --every weekday --at 07:00  # prime the 5h window
rimz loop add nudge --wake @planner --prompt "resume the review" --in 30m           # one-shot wake
rimz loop add watchdog --check "cargo test" --on fail \
    --agent codex --prompt "fix the failing test" --every 15m                       # watch, then wake
```

**Work from anywhere.** A room is plain Zellij or tmux under SSH: save an alias and [reconnect over a link that heals itself](./docs/guide/remote.md), or [tunnel the room into a local browser](./docs/guide/web.md). Close the laptop mid-run, reattach from another machine, and every agent is where you left it.

```sh
rimz remote add dev dev-box:~/code/query-engine
rimz remote connect dev          # the room rebuilds, every agent where you left it
rimz remote connect dev --web    # the same room in your browser at 127.0.0.1
```

## Configuration

RimZ runs with zero configuration, and everything you can tune is plain TOML in files you own: no config daemon, no bespoke language, nothing new to learn. `rimz setup` detects the machine and writes the per-machine defaults under `~/.config/rimz/` — `config.toml` (behavior), `theme.toml` (appearance), `agents.toml` (profiles and teams), `loop.toml` (schedules), `remote.toml` (SSH aliases) — every key commented with its default and an inline note, so the files are their own reference. After that first pass, [`rimz config set`](./docs/guide/configuration.md) edits one dotted key: it routes the key to the owning file, validates the value, and writes it durably, so you change behavior without hand-editing TOML.

```sh
rimz setup                                 # detect the machine, write the commented defaults
rimz config set theme "Catppuccin Mocha"   # edit one key; `rimz list-themes` shows the choices
```

### True color, Nerd Font, and pets

A terminal that advertises truecolor (Ghostty, WezTerm, Kitty, and Alacritty all do) gets 24-bit color out of the box, inside RimZ tmux rooms and over `rimz remote` too. With a Nerd Font in the terminal, [`theme.style`](./docs/guide/theme.md) turns on the sharper glyphs, and one more line adds a pet.

```sh
rimz config set theme.style modern        # truecolor + Nerd Font icons; "default" = auto color + Unicode
rimz config set theme.pets.enabled true   # an animated companion on the provider dashboard
rimz config set theme.pets.pet rocky      # `rimz list-pets` previews all pets
```

Pets render as crisp pixels in Ghostty and kitty terminals; inside tmux that also needs tmux 3.6+ with `allow-passthrough on`. Everywhere else, including Zellij, the same pet renders as cell art. The [pets guide](./docs/guide/pets.md) has the catalog, [petdex.dev](https://petdex.dev/) installs, and bring-your-own sheets.

### Auto-continue and smart compaction

Two settings keep agents working [while you are away](./docs/guide/loops.md#built-in-recovery). Auto-continue picks parked turns back up: a rate-limit park resumes the moment the provider's budget window resets, and a transient API error retries on a backoff ramp. Smart compaction makes `rimz message` compact-first: past the threshold, RimZ submits `/compact` ahead of your text, so a long turn lands against a fresh context window instead of dying at the ceiling.

```sh
rimz config set resume.auto_continue true     # off by default; resumes rate-limit and API-error parks
rimz config set harness.smart_compact "70%"   # compact before a message once context passes 70%
```

Add a [scheduled ping](#everyday-moves) to start each provider's budget window on your clock, and the fleet only needs you for real decisions.

[Bound dollar spend](./docs/guide/budget.md) at the scale you promise yourself: `rimz config set harness.budget 50/day` turns on a cap for each room's whole fleet, `[accounts.budget]` turns on one provider-login cap across rooms, and `rimz budget` shows or changes the live headroom.

### Asks on your phone

Claude Code and Codex ship remote control, the bridge their official mobile apps drive. Two toggles make the room bring it up at every start (`claude remote-control` as a background pane, the Codex remote-control daemon ensured once per machine) so a blocking ask pushes to your phone, your answer lands in the same session, and the turn continues in its pane as if you had typed it there:

```sh
rimz config set remote_control.claude true
rimz config set remote_control.codex true
```

Both are off by default, and the [agents guide](./docs/guide/agents.md#answer-asks-from-your-phone) shows exactly what each toggle runs.

The [setup guide](./docs/guide/setup.md) covers the first pass end to end: agent hooks, appearance, the hands-off behaviors, and a modern Zellij/tmux baseline with [ready-to-adopt example configs](./examples/README.md). The full key catalog is the [configuration guide](./docs/guide/configuration.md).

## Agent compatibility matrix

| Agent       | Status | Integration                                                       |
|-------------|:------:|-------------------------------------------------------------------|
| Claude Code | ✅     | hooks · statusline · `.jsonl` transcripts · `claude --resume`     |
| Codex       | ✅     | hooks + `notify` · app-server · rollout `.jsonl` · `codex resume` |
| Copilot     | alpha  | hooks · `copilot --resume`                                        |
| Gemini CLI  | beta   | hooks · session `.jsonl` · `gemini --resume`                      |
| Pi          | beta   | extension API · session `.jsonl` · `pi --session`                 |
| OpenCode    | alpha  | extension API · session `.jsonl`                                  |
| Cursor      | alpha  | command hooks · opaque transcript metadata · `agent --resume`     |

Adapters are thin layers over the same hook and transcript primitives; the agents run stock, in your terminal, with the official apps untouched. Per-agent status, integration surface, and permission-mode mapping live in [agent support](./docs/reference/agent-support.md); the adapter boundary itself is in the [agents internals](./docs/internals/agents/model.md).

## Documentation

The [documentation index](./docs/README.md) maps the whole set. Highlights:

- [Set up your machine](./docs/guide/setup.md) — install to a working fleet: config, hooks, true color, pets, and the Zellij/tmux baselines
- [Working with agents](./docs/guide/agents.md) — [agents](./docs/guide/agents.md) · [the sidebar](./docs/guide/sidebar.md) · [token insight](./docs/guide/insight.md) · [remote](./docs/guide/remote.md) · [web](./docs/guide/web.md)
- [Harness](./docs/guide/messaging.md) — [messaging](./docs/guide/messaging.md) · [teams](./docs/guide/teams.md) · [worktrees](./docs/guide/worktrees.md) · [scripting agents](./docs/guide/scripting.md) · [loops & schedules](./docs/guide/loops.md)
- [CLI reference](./docs/reference/cli.md) · [Configuration](./docs/guide/configuration.md) · [Theming](./docs/guide/theme.md) · [Troubleshooting](./docs/guide/troubleshooting.md)
- [DESIGN.md](./DESIGN.md) · [ARCHITECTURE.md](./ARCHITECTURE.md) · [internals](./docs/internals/README.md) — how it works, in depth

## Install

`Cargo` install

```sh
cargo install --locked rimz     # from crates.io
```

`homebrew` install

```
brew tap rimio/homebrew-rimz
brew install rimz
```

Hooks are how agents report to the room. The first `rimz` run offers to install them with a diff preview, and `rimz hooks install` does the same on demand:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install              # every detected agent (claude, codex, copilot, gemini, pi, opencode)
rimz doctor                     # verify backend, hooks, and room health
```

The install is additive (your existing hooks stay), and `rimz hooks uninstall` undoes it. RimZ is pre-release ([project status](#project-status)): the agent adapters, both multiplexer backends, and the sidebar are implemented in-tree.

## Contributing

Contributor rules and the gate stack live in [rust-conventions.md](./docs/contributing/rust-conventions.md); the working contract is [AGENTS.md](./AGENTS.md).

## License

MIT. See [LICENSE](./LICENSE).

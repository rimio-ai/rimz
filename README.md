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
  <a href="#project-status">Status</a> ·
  <a href="#what-it-does">What it does</a> ·
  <a href="#get-started">Get started</a> ·
  <a href="#everyday-moves">Everyday moves</a> ·
  <a href="#configuration">Configuration</a> ·
  <a href="#agent-compatibility-matrix">Agents</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#documentation">Docs</a> ·
  <a href="https://rimz.rimio.ai/llms.txt">llms.txt</a>
</p>

<p align="center"><sub><b>AI agents / LLMs:</b> fetch <a href="https://rimz.rimio.ai/llms.txt">the live index</a> / <a href="https://rimz.rimio.ai/llms-full.txt">full docs blob</a>.</sub></p>

---

RimZ puts your coding agents in one Zellij or tmux room and routes your attention to whichever one needs you. Every agent gets a live card in the sidebar (state, task, context health, live cost), so one human follows tens of agents at a glance, and one click lands in the pane that is waiting.

<p align="center">
  <img src="https://raw.githubusercontent.com/rimio-ai/rimz/HEAD/docs/rimz-full.png" alt="A RimZ room: the sidebar triaging a fleet of coding agents beside their panes" width="100%">
  <br/><sub>The sidebar triages the fleet on the left; agents work in their own panes.</sub>
</p>

RimZ is one lightweight binary inside the Zellij or tmux you already run. Your keybinds stay, the agent CLIs run stock, and the official web, desktop, and mobile apps keep working untouched.

That small footprint carries the primitives **harness engineering** and **loop engineering** build on: the sidebar for observability, one command grammar for [every supported agent](#agent-compatibility-matrix), durable messages that steer and queue, supervised runs with exit codes for scripts and CI, teams and subagents that split work across models, wakeups that put the fleet on a clock or on an event, and dollar budgets that bound what it spends. The harness itself (guardrails, policies, self-running loops) is yours to build on top.

## Project status

RimZ is **beta software**. The basic features work smoothly for daily use, and the surface is moving fast: commands, flags, config keys, and output formats can change between releases, and you will meet the occasional rough edge. The [changelog](./CHANGELOG.md) records what moved in each release.

It is also heavily used, on itself. RimZ is built with RimZ. Driven through teams and loops, most of the routine engineering here (features, bug fixes, CI repair) already flows through the harness with little hand-holding.

Read that as: ready for personal, daily use today; for production workflows that need a stable interface, wait for the 1.0 release.

## What it does

<p align="center">
  <img src="https://raw.githubusercontent.com/rimio-ai/rimz/HEAD/docs/rimz-gallery.png" alt="" width="100%">
  <br/><sub>Realtime harness dashboard, with rich information at a glance</sub>
</p>

### See the fleet

- Each agent card shows working state and task, model and effort, context health and compactions, live token stats and dollar cost, and the subagent tree.
- The cockpit line (`? 2  ! 1 …`) reads the whole fleet in one glance, the column below it arrives already triaged, and one click drops you into the pane that is waiting.
- Spending and token insight covers today, the week, and the month, with plan and 5h/7d budget bars for providers that expose those account surfaces, so one look tells you where the week is going.
- A pet, an animated sprite on the provider dashboard, keeps you company: it runs while the agents run and waves when one waits.

### Run agents together

- One layout spec opens agents side by side in an isolated Git worktree: `claude,codex` starts Claude planning beside Codex reviewing, and `vim,codex+term` puts your editor, an agent, and a shell in one tab.
- A team pairs models by role, such as a Fable planner with an Astra coder, and launches as one unit, each role on the model best at its job (reasoning depth, instruction following, speed, price). A mixed team catches what a single model lets through.
- A staged team keeps its progress on a board in the worktree and hands a stage to the next role with one command, so a member that crashed or compacted picks the run back up.
- Every agent answers to a handle (`@codex`, `@planner`). Messages are durable, park at the turn boundary or steer the live turn, and carry agent-to-agent talk inside channels the same way they carry yours.
- An agent launches supervised children with `rimz subagents`, each a full agent in its own pane with a nested sidebar row, a durable transcript, and its spend credited to the parent. Children mix providers freely.

### Keep it running without you

- `rimz agents -p` is `claude -p` for every agent that can run headless, with exit codes, JSON output, streaming, and the full transcript kept, so agents drop into scripts, CI, and workflows.
- `rimz loop` schedules supervised runs on a clock (calendar, interval, cron, or a check-guarded watchdog that runs a command and wakes an agent on the result) or on a signal. RimZ emits CI, pull request, agent, and team signals itself, and anything else can emit its own.
- `rimz wait` lets an agent arm its own timer, command, process, polled check, or file watch and end its turn instead of sleeping.
- A rate-limit pause resumes the moment the budget window resets, transient API overload retries on a backoff ramp, and smart compaction compacts a filling context before the next message lands, so agents keep working while you are gone.
- Dollar budgets cap one turn, one agent, one loop task, the room, or a provider account. A turn that crosses a cap is stopped and parked.
- Notification handlers run your own command the moment a row needs eyes: a push to your phone, or a script that answers the routine prompt for you.

### Work from anywhere

- Start on your MacBook or a server, close the laptop, and reattach from anywhere over SSH or in a browser tab; the link heals itself every time you reconnect.
- When an agent stops to ask, the question reaches you in the official Claude and ChatGPT mobile apps exactly as if you were driving the CLI by hand. Your answer lands in the same terminal session, and RimZ is never between you and the official apps.
- A room runs its Claude or Codex agents under a second provider account, with its own limits, budget, and sessions.
- On Linux, sandbox isolation gives each agent pane a bubblewrap mount view: the room's own `/tmp` and a curated skill set. It is a mount view and does not contain the agent.

## Get started

```sh
# 1. Install
curl -fsSL https://raw.githubusercontent.com/rimio-ai/rimz/main/scripts/install.sh | sh

# 2. Open the room
cd ~/code/query-engine
rimz

# 3. Launch agents and work; the sidebar surfaces whoever needs you
claude
codex

# 4. Worktrees, dynamic layouts, agent teams
rimz agents claude,codex --worktree=feat-x       # Claude + Codex, side by side
rimz agents 'vim,claude+term' --worktree=feat-y  # editor, agent, shell in one tab

# 5. Native SSH remote, with self-healing reconnect
rimz remote connect dev-box:~/code/query-engine
```

### Other ways to install

```sh
brew install rimio-ai/rimz/rimz     # Homebrew
cargo install --locked rimz         # Cargo, from crates.io
rimz update                         # later: update in place, whichever way you installed
```

On macOS, the install script's default latest-release install uses Homebrew automatically when `brew` is available. Prebuilt binaries, building from source, and uninstalling are in the [installation guide](./docs/guide/installation.md). For a self-contained room with RimZ, both multiplexers, ttyd, and the priority agent CLIs preinstalled, [run the Docker image](./docs/guide/installation.md#run-in-docker).

### Hooks

Hooks are how agents report to the room. The first `rimz` run offers to install them, with a summary of every file it touches and your consent, and `rimz hooks install` does the same on demand:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install              # every agent detected on the machine
rimz doctor                     # verify backend, hooks, and room health
```

The install is additive (your existing hooks stay), and `rimz hooks uninstall` undoes it. → [set up your machine](./docs/guide/setup.md) · [enable dynamic shell completion](./docs/guide/setup.md#shell-completion) · [security and trust](./docs/guide/security.md)

## Everyday moves

The commands below run from any pane in the room, and from any script or CI job that reaches it. They compose: a profile becomes a team, the team lands in a worktree, the worktree's agents take messages, and a schedule fires the whole thing while you sleep.

### Start agents

**Run the official CLIs, untouched.** Every supported agent joins the room the same way: type its own command into any pane, exactly as you do today. The stock binary runs with your flags, your config, and its own session files; the hooks you approved at setup report it to the sidebar, and nothing sits between you and the CLI.

```sh
claude
codex
```

[`rimz agents`](./docs/guide/fleet.md) earns its keystrokes when you want more than the default. A `-auto` or `-yolo` suffix sets the permission mode, and a [Markdown definition](./docs/reference/definitions.md) under `~/.rimz/agents/` pins a model, effort, craft, and tools behind one word, so a planner that reasons hard and edits nothing is a name you reuse.

```sh
rimz agents claude          # stock agent, own pane
rimz agents codex-yolo      # permission modes: -auto, -ask, -plan, -yolo
rimz agents planner         # your profile: model, effort, system prompt
```

**Launch layouts into worktrees.** One spec describes the shape: `,` splits, `+` tiles, `/` stacks. Add [`-w`](./docs/guide/worktrees.md) and the whole layout lands in an isolated RimZ-owned Git worktree, seeded with the untracked files it needs, so two lines of work run side by side without touching each other or your main checkout.

```sh
rimz agents claude,codex -w feat-a             # two agents, side by side
rimz agents planner,coder+reviewer -w feat-b   # profiles compose like kinds
rimz agents 'vim,codex+term' -w feat-c         # editor | agent tiled over a shell
rimz agents 'claude/codex/term' -w feat-d      # one stack of three rows (a Zellij stack; tmux tiles them)
rimz agents codex --from-pr 42                 # worktree checked out from a pull request
```

**Combine models as teams.** A named [team](./docs/guide/teams.md) in `~/.rimz/teams/<name>.md` gives each role a handle and launches the whole set in its layout, each role in its own context window, cooperating over messages. Pair model strengths across providers: one plans, another writes the code, a third reviews the diff blind. A staged team tracks its progress on `blackboard.md` in the worktree, and `rimz teams flip` hands a stage to the next role. RimZ is built this way; `examples/teams/` ships the `forge` team it uses, plus `mill` for refactors and `spot` for small fixes.

```sh
rimz agents claude:planner,codex:coder -w feat-once   # one-off roles without a saved team
rimz teams install forge                              # release-matched shipped team
rimz teams forge -w feat-complex                      # planner, coder, reviewer on one feature
rimz teams spot -w fix-expiry                         # coder and reviewer on a small fix
rimz teams show forge#feat-complex                    # roles, status, and where the run stands
rimz teams wait forge#feat-complex                    # block until the board reaches Done
rimz agents attribution --md                          # credit the lane's agents and models in a PR footnote
```

### Steer the fleet

**Message agents like teammates.** Every agent answers to a [handle](./docs/guide/messaging.md), named by kind, profile, or team role: `@codex` reaches the one in your channel, `@codex#feat-a` reaches across the workspace. Every message becomes a durable record you can read back and steer: parked at the turn boundary by default, `--steer` to write into the live turn now, `--schedule` to deliver later. The same command serves you, your scripts, and the agents themselves, which use it to talk to each other.

```sh
# Park at the next turn boundary; address by kind, profile, or team role
rimz message @claude "add coverage for the expiry edge cases"
rimz message @planner "draft the implementation plan"
rimz message @coder --after @planner "planner's done: read plan-notes.md and start"

# Ask and print the reply from the agent's own context
rimz message @coder --wait "did the migration land? one line"
rimz message @all --wait --json "status? one line"     # a labeled reply map for the whole channel

# Write into the live turn now, or schedule for later
rimz message --steer @claude "stop: the parser test comes first"
rimz message --interrupt @claude "stop: the user changed direction" # stop the turn, then deliver fresh
rimz message --schedule 60m @codex#feat-b "run the smoke test"

# Pipe context in, or broadcast to everyone
git diff main | rimz message @reviewer --stdin "review this"
rimz message @all "summarize what changed at the next boundary"
```

**Answer a blocked agent without finding its pane.** [`rimz asks`](./docs/reference/cli/asks.md) lists the prompts that currently block agents, with each question's own text and options, and `rimz answer` submits a choice in the agent's native UI.

```sh
rimz asks                        # open prompts in this channel; --all for every channel
rimz asks show @planner          # one prompt with its context and choices
rimz answer @planner allow       # submit a choice by label
rimz answer @coder 2             # or by position
```

### Automate the routine

**Script an agent like any CLI.** [`rimz agents -p`](./docs/guide/scripting.md) is `claude -p` with one grammar for every agent that can run headless: one prompt, supervision until the work ends, one exit code a script or CI job branches on, and swapping the provider behind a pipeline is a one-word change. It still runs in a real pane you can watch, answer, and steer while the pipeline waits on it.

```sh
# One supervised run, one exit code, one JSON result to branch on
rimz agents codex "Prepare the release checklist." -p --timeout 30m --output-format json

# stdin appends to the prompt
cat build-error.txt | rimz agents claude -p --stdin 'explain the root cause'

# Fire in the background; it returns now and prints the run's name
rimz agents claude "Run the migration audit." -p --bg

# Block on that run later and tail the answer
rimz agents wait swift-otter --stream

# Race several runs; the first to finish wins and prints its result
rimz agents wait otter fox --any
```

**Let agents launch agents.** [`rimz subagents`](./docs/guide/subagents.md) is the same path packaged for an agent RimZ launched: one command, the parent's checkout and channel inherited, and a petname printed for a later join. Each child is a full agent with its own pane beside the parent's, a nested row in the sidebar, a run record and transcript that outlive the turn, and a question that routes to you instead of failing silently. Launches return at once and the children work in parallel; a parent that does not join gets one report once every child has settled.

```sh
# From inside an agent's turn
rimz subagents codex "review this diff for correctness; report concrete findings"
rimz subagents wait @otter @fox      # collect named children's results
rimz subagents stop --all            # stop every live child

# Fan one audit across providers, then join all of it
rimz subagents fanout --wait <<'JSON'
[
  {"profile":"codex","prompt":"review correctness; report concrete findings"},
  {"profile":"claude","prompt":"review the interface; report concrete findings"}
]
JSON
```

**Run the fleet on a schedule.** [`rimz loop`](./docs/guide/loops.md) fires agent turns on a clock: daily at a set time, on an interval, from a cron line, or once after a delay. Add `--check` and the task becomes a watchdog, running the script first and waking the agent only on its result. There is no scheduler daemon: an open room keeps time, and `rimz loop timer install` adds a systemd or launchd timer for the hours when none is open. Switch on [auto-continue and smart compaction](#configuration) and the loop runs hands-off; add `--budget-per-day 20` to bound what hands-off work costs.

```sh
# 07:00 every weekday: overnight changes, summarized before you sit down
rimz loop add standup --agent claude --every weekday --at 07:00 \
    --prompt "Summarize what landed on main since yesterday and flag anything that needs review"

# Watchdog: run the check first, wake the agent only on failure
rimz loop add watchdog --check "cargo test" --on fail \
    --agent codex --prompt "fix the failing test" --every 15m
```

**Wait without a pane.** [`rimz wait`](./docs/reference/cli/wait.md) is the alarm an agent sets for itself instead of holding its turn open on `sleep`: it ends the turn, and when the delay elapses, the command finishes, or the file changes, RimZ delivers a message back into the same conversation naming what fired and on what.

```sh
rimz wait --in 30m                           # one-shot wait after a delay
rimz wait --check 'nc -z localhost 3000'     # poll until the service answers
rimz wait --file build.log --grep 'READY'    # a new matching line in a file
rimz wait --pid 16776                        # an existing process exits
rimz wait -- gh run watch --exit-status      # a command exits, with its output in the message
```

**Wake on a signal.** A timer guesses when something will happen; a [signal](./docs/guide/loops.md#signals-the-rooms-event-bus) is the thing itself. RimZ emits `ci.passed` and `ci.failed`, `pr.merged` and `pr.closed`, and agent and team lifecycle signals from what the room already watches, and anything can emit its own. A CI or pull request subscription follows the calling agent's own worktree branch.

```sh
# Wake me when CI fails on the branch this agent is working on
rimz loop add ci-red --signal ci.failed --wait

# Wake me once, on the next merge
rimz loop add merged --signal pr.merged --wait --once

# Your own signal: a git hook, a deploy script, or another agent emits it
rimz events emit deploy.finished --json '{"env":"prod","version":"1.4.2"}'
```

### Step away

**Work from anywhere.** A room is plain Zellij or tmux under SSH: save an alias and [reconnect over a link that heals itself](./docs/guide/remote.md), or [tunnel the room into a local browser](./docs/guide/web.md), which needs ttyd on the machine that serves the room. Close the laptop mid-run, reattach from another machine, and every agent is where you left it.

```sh
rimz remote add dev dev-box:~/code/query-engine
rimz remote connect dev          # the room rebuilds, every agent where you left it
rimz remote connect dev --web    # the same room in your browser at 127.0.0.1
```

**Answer from your phone.** A fleet that runs while you are out still stops to ask: a permission prompt, a plan approval, a question only you can decide. Claude Code and Codex ship remote control, the bridge behind their official mobile apps, and two toggles keep that bridge up with every room. The ask reaches your phone as a push from the provider's own app, your answer lands in the same session on the machine running the room, and the turn moves on as if you had typed it there. RimZ stays out of the path: each toggle starts the provider's own command with the room and nothing more.

```sh
rimz config set remote_control.claude true    # keep `claude remote-control` up with the room
rimz config set remote_control.codex true     # ensure codex's remote-control daemon, once per machine
```

Both are off by default, and setting one back to `false` undoes it. The [remote guide](./docs/guide/remote.md#answer-asks-from-your-phone) shows exactly what each toggle runs. For every other agent, a [notification handler](./docs/guide/notifications.md) pushes the ask anywhere a shell command can reach.

## Configuration

RimZ runs with zero configuration, and everything you can tune is a plain file you own: no config daemon, no bespoke language. `rimz setup` detects the machine and writes commented defaults under `~/.rimz/` (`config.toml`, `theme.toml`, `loop.toml`, `remote.toml`), so the files are their own reference. Agent profiles, teams, and skills are Markdown beside them, under `~/.rimz/agents/`, `~/.rimz/teams/`, and `~/.rimz/skills/`. After setup, [`rimz config set`](./docs/guide/configuration.md) routes any dotted key to the owning file, validates the value, and writes it durably.

For the best experience, we recommend one pass of these:

```sh
rimz setup                                    # once: detect the machine, write the commented defaults

# Appearance: truecolor, sharper glyphs, a companion
rimz config set theme.style modern            # truecolor + Nerd Font icons
rimz config set theme.pets.enabled true       # an animated pet; `rimz list-pets` previews them

# Hands-off work: recover and keep going while you're away
rimz config set resume.auto_continue true     # resume rate-limit and API-error parks
rimz config set harness.smart_compact 200k    # compact before a message once context passes 200k tokens (a percentage like "70%" works too)
rimz config set harness.budget 50/day         # cap what the room spends in a day

# Answer asks from your phone (the move above)
rimz config set remote_control.claude true
rimz config set remote_control.codex true
```

What each group does, with the depth one link away:

- The modern look wants a truecolor terminal (Ghostty, WezTerm, Kitty, Alacritty) and a Nerd Font, inside RimZ tmux rooms and over `rimz remote` too. The color scheme defaults to TokyoNight Night; `rimz config set theme "Catppuccin Mocha"` picks any bundled scheme from `rimz list-themes`. Pets render as crisp pixels in Ghostty and kitty (tmux additionally needs 3.6+ with `allow-passthrough on`) and as cell art everywhere else, Zellij included. → [theming](./docs/guide/theme.md) · [pets](./docs/guide/pets.md)
- Auto-continue resumes a parked agent the moment the provider's budget window resets and retries transient API errors on a backoff ramp; smart compaction sends the agent's compact command ahead of your text once context passes the threshold, so a long turn lands on a fresh window. Between them the pauses that would have stalled the fleet until morning clear themselves, and you are left with the decisions that actually need you. The room budget caps what that freedom costs, and the same cap exists per turn, per agent, per loop task, and per provider account. → [loops → keep the fleet moving](./docs/guide/loops.md#keep-the-fleet-moving) · [budgets](./docs/guide/budget.md)
- The remote-control toggles are the [answer-from-your-phone move](#step-away) above; the [remote guide](./docs/guide/remote.md#answer-asks-from-your-phone) shows exactly what each one runs.

Two more settings change what a room launches under:

```sh
rimz accounts add claude work                 # declare a second Claude or Codex account and install its hooks
rimz start --account claude=work              # a room whose Claude agents all run under `work`
rimz config set agents.isolation sandbox      # Linux: a bubblewrap mount view for every agent pane
```

- A [provider account](./docs/guide/accounts.md) is fixed when the room is born, and everything the room launches (agents, team members, subagents, loop tasks, restarts) runs under it, with its own limits, budget, and sessions. `rimz reset --account claude=default` rebuilds the room on Claude's own home.
- [Sandbox isolation](./docs/guide/security.md#sandbox-isolation) gives each agent the room's own `/tmp` and the RimZ skill library merged into its provider's skill root, with a profile's `skills:` list deciding which skills the model may call on its own. It is a mount view and not containment: the host filesystem stays writable and your credentials stay readable. `rimz start` refuses to launch when bubblewrap is missing or unusable, and `--isolation host|sandbox` overrides the setting for one launch.

The [setup guide](./docs/guide/setup.md) walks the whole first pass, agent hooks included; the [Zellij and tmux guide](./docs/guide/multiplexer.md) has a modern baseline with [ready-to-adopt example configs](./examples/README.md), and the full key catalog is the [configuration guide](./docs/guide/configuration.md).

## Agent compatibility matrix

**Claude Code and Codex are the daily drivers** and give the best experience today.

**Pi and OpenCode are in alpha**: wired end to end and close behind.

Every other agent is **experimental**: wired and tested against its documented surface, but not yet dogfooded enough by the author, so expect the occasional bug and please [report what you hit](https://github.com/rimio-ai/rimz/issues). Any of them still mostly just works: the CLI runs stock in your terminal and the official apps stay untouched.

| Agent       | Status       | State | Live | History | Account | Ask | Subagents |
|-------------|--------------|:-----:|:----:|:-------:|:-------:|:---:|:---------:|
| Claude Code | Supported    |   ●   |  ●   |    ●    |    ●    |  ●  |     ●     |
| Codex       | Supported    |   ●   |  ●   |    ●    |    ●    |  ●  |     ●     |
| Pi          | Alpha        |   ●   |  ●   |    ●    |    ●    |  ●  |     ◐     |
| OpenCode    | Alpha        |   ●   |  ●   |    ●    |    ●    |  ●  |     ●     |
| Antigravity | Experimental |   ●   |  ◐   |    ◐    |    ●    |  ◐  |     ◐     |
| Copilot     | Experimental |   ●   |  ◐   |    ◐    |    ◐    |  ●  |     ◐     |
| Droid       | Experimental |   ●   |  ◐   |    ◐    |    ✗    |  ●  |     ✗     |
| Cursor      | Experimental |   ●   |  ◐   |    ◐    |    ◐    |  ◐  |     ◐     |
| Amp         | Experimental |   ●   |  ◐   |    ◐    |    ◐    |  ●  |     ✗     |
| Kiro        | Experimental |   ◐   |  ◐   |    ◐    |    ✗    |  ◐  |     ✗     |
| Qwen        | Experimental |   ●   |  ◐   |    ◐    |    ◐    |  ●  |     ●     |
| Kimi        | Experimental |   ●   |  ◐   |    ◐    |    ●    |  ●  |     ◐     |
| Grok        | Experimental |   ●   |  ◐   |    ●    |    ◐    |  ●  |     ●     |


<sub>● full: complete and live · ◐ partial: a working version with a stated limit · ✗ unsupported</sub>

A mark answers what you see and when; the mechanism behind it is in the reference.

- **State**: the card tracks the agent's whole life. It appears at session start, follows working, waiting, and idle, and clears when the session ends.
- **Live**: while a turn runs, the card shows context-window fill, the token breakdown behind it, and a dollar figure for the work in flight.
- **History**: every past session reads end to end, with per-turn tokens and dollars feeding `rimz stats` and the provider dashboard.
- **Account**: your login and plan, plus each usage window (5h/7d, monthly credits, a balance) with its fill and reset.
- **Ask**: the agent stops to ask, the card raises Waiting, and the question's own text and options reach `rimz asks`.
- **Subagents**: child agents nest under the parent card as they start, each with its name, task, and model.

A ◐ names its own limit: part of the detail, or the whole of it a beat late. The [agent support](./docs/reference/agent-support.md) reference spells out every cell and the mechanism behind it, and `rimz coverage` prints the same grid on your own machine.

<sub><b>Latest version only.</b> RimZ tracks each agent's most recent release; older CLI versions are not supported.</sub>

## How it works

```
 terminal — ghostty · iterm2 · warp · kitty · vscode …
   zellij or tmux — your keybinds, your layout

     ┌─────────┐       ┌────────────────────────────────────┐
     │ sidebar │       │ claude · codex · pi · opencode · … │
     └────▲────┘       └────▲────────────────────┬──────────┘
          │                 │                    │
          │ renders         │ types into panes   │ hooks · transcripts (.jsonl) · oauth api
          │ the fleet       │ messages · -p runs │ statusline (claude) · app-server (codex)
          │                 │                    │ extensions (pi/opencode) · …
          │                 │                    ▼
          └─────────────────┴──────────────────  rimz  ◀──  git status · /proc stats
```

- Agents report themselves. Sessions, tool calls, live status, and blocking questions arrive the moment they happen, through each agent's own hooks, transcripts, and APIs.
- RimZ drives the panes. Messages, steering, and `-p` harness runs land as keystrokes in the agent's own pane, so every agent runs its stock CLI in a full terminal, exactly as if you typed.
- RimZ fuses every channel. Agent events, git churn, process stats, and account state combine into one live picture, and the sidebar renders it.

→ [DESIGN.md](./DESIGN.md) · [ARCHITECTURE.md](./ARCHITECTURE.md)

## Documentation

The [documentation index](./docs/README.md) maps the whole set. Highlights:

- [Set up your machine](./docs/guide/setup.md): one run of `rimz setup`, with the config it writes, the agent hooks, the color and glyph probes, the pet, and hands-off automation
- [Working with agents](./docs/README.md#working-with-agents): [agents](./docs/guide/fleet.md) · [the sidebar](./docs/guide/sidebar.md) · [token insight](./docs/guide/insight.md) · [remote](./docs/guide/remote.md) · [web](./docs/guide/web.md)
- [Harness engineering](./docs/README.md#harness-engineering): [worktrees](./docs/guide/worktrees.md) · [messaging](./docs/guide/messaging.md) · [teams](./docs/guide/teams.md) · [scripting agents](./docs/guide/scripting.md) · [loops & schedules](./docs/guide/loops.md) · [notifications](./docs/guide/notifications.md) · [budgets](./docs/guide/budget.md)
- [Customization](./docs/README.md#customization): [configuration](./docs/guide/configuration.md) · [provider accounts](./docs/guide/accounts.md) · [theming](./docs/guide/theme.md) · [pets](./docs/guide/pets.md) · [Zellij and tmux](./docs/guide/multiplexer.md)
- [CLI reference](./docs/reference/cli.md) · [Troubleshooting](./docs/guide/troubleshooting.md) · [Security and trust](./docs/guide/security.md) · [Changelog](./CHANGELOG.md)
- [DESIGN.md](./DESIGN.md) · [ARCHITECTURE.md](./ARCHITECTURE.md) · [internals](./docs/internals/README.md): how it works, in depth

## Contributing

[CONTRIBUTING.md](./CONTRIBUTING.md) covers building from source and the gates a change passes. [AGENTS.md](./AGENTS.md) is the working contract for humans and coding agents alike, and the code shape is in [rust-conventions.md](./docs/contributing/rust-conventions.md).

The alpha and experimental agents above are where help lands fastest: bug reports and adapter fixes are how an agent graduates to Supported, and both are welcome. The adapter playbook is [agent-adapters.md](./docs/contributing/agent-adapters.md).

## License

MIT. See [LICENSE](./LICENSE).

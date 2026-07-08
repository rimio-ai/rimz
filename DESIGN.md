# Design

Rimz is the harness layer for agentic coding: one human, a fleet of coding agents, one room per project. The room is a Zellij or tmux session with a sidebar that reads the whole fleet at a glance; underneath it, one CLI carries every event into a durable store, so the room survives detach, reload, reboot, and reattach from anywhere.

Leverage over a raw model call accrues in layers: prompt engineering, context engineering, **harness engineering**, **loop engineering**. A harness is everything wrapped around one agent run: the tools, guardrails, feedback loops, and observability that make it reliable. A loop is the control structure above that: act, observe, decide, repeat, driving runs toward a goal unattended.

Rimz supplies the primitives those two layers build on (fleet observability, one uniform interface over every agent, durable messaging, supervised runs, scheduled wakeups) and touches nothing else: your terminal, your multiplexer, the stock agent CLIs, and the official apps all keep working as they are. The harness and loops you compose on those primitives stay yours, with their own policy and credentials.

The problem Rimz answers is attention. A fleet emits far more than one person can read (prompts, tool calls, completions, failures, token burn, rate-limit stalls), and Rimz spends that finite attention well: it turns the wall of activity into "this pane, right now, needs you," and otherwise stays quiet. A good harness spends your attention only where a run genuinely needs a human, and a good loop removes the need for it between runs; every choice below serves that economy.

> **Invariant.** Rimz routes your attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

## Triage at a glance

The sidebar is a presence and attention map: one row per live pane, enriched from the store, grouped by worktree. One glance answers what is working, what is blocked, what errored, and what it is costing — and keeps answering as the fleet grows.

- The column arrives triaged: hot work inside the one-hour prompt-cache window ranks first, then warm work up to the 24-hour archive boundary, then archived work, with an attention score ordering each band. A row turns unread the moment it needs you (`waiting`, `failed`, `paused`, `success`) and stays marked until you focus it; exactly one row is ever in motion, so the eye lands where to go next.
- Two symbols carry every call for attention — `?` *needs your answer*, `!` *needs a look* — by shape, so the signal survives `NO_COLOR` and color-blind eyes; color only reinforces. The fixed cockpit line (`? 2  ! 1 …`) compresses the fleet: a row of zeros means nothing needs you, skip the scan.
- Five states cover every agent — `running`, `waiting`, `idle`, `success`, `failed` — plus a derived `paused` for provider-limit parks, with short-lived heads (thinking, compacting, delegating) riding `running`. A context meter, token totals, live dollar cost, diff stats, and last-activity age ride the rows; the provider dashboard carries the pace, its 5h/7d budget bars draining in real time.
- Presence is live and facts are durable: a row exists because a pane runs right now and clears itself when the agent exits, while everything the agent did stays in the store. Stats enrich display; the store and explicit events decide state and correctness.

The glyph legend and rendered frames live in [the interface reference](./docs/interface/sidebar.md), the ranking and presence mechanics in [sidebar.md](./docs/internals/sidebar/sidebar.md), and the reader's guide to the column and its ranking in [the sidebar guide](./docs/guide/sidebar.md).

## Answering in the agent's own UI

The moment an agent asks to run a command is the moment a human can stop something destructive. Rimz keeps that moment in the agent's own UI and spends its effort getting you there fast. A blocking prompt (a permission request, a plan approval, a question) reaches Rimz through the agent's hooks and sets the agent's `waiting` state, the hook returns the agent-native neutral no-op so the prompt stays on screen exactly as the agent rendered it, and the sidebar routes you there: the row turns `?`, notifications fire, and focusing the row lands you in the pane. Your answer clears the state through the same lifecycle channel, and the transcript keeps the question and the answer. The state machine and clearing edges live in [model.md](./docs/internals/agents/model.md).

## A fleet run like a team

A channel is a named lane where a human and a few agent colleagues work one line of work, backed by a bare name, a Git worktree, an in-place team as `<dir>/<team>`, or the room's directory itself. Spawning separates three independent choices: agents (which tools), layout (the shape on screen), and channel (which lane), so a planner-and-reviewer pair in any lane is one command.

- A member is addressed as `@handle#channel`: `@claude` the kind, `@planner` the role, `@swift-otter` the one instance, `@all` the lane. Addresses resolve against live presence: one match delivers, many ask you to pick (or `--all` fans out), and `--create` launches the member straight from its address. Identity stays light on purpose, enough to name who needs you; the weight is on the cooperation inside the lane.
- `message` reaches a member in either tense: `--steer` types into the live pane now, the default parks text for the next open turn, `--schedule` sets a delivery floor, and `--on done|any` picks the boundary gate. Every mode routes through the one pane-send primitive and records an audit event that omits the message text.
- Automation is a teammate. `rimz agents … -p` drives one member headless — cron, CI, or a script launches the turn, reads the answer or a stream, and branches on the exit code — through the same hooks, store, and pane path an interactive member uses.

The launcher, address grammar, delivery gates, and supervised-run machinery live in [harness.md](./docs/internals/harness/harness.md).

## A programmable room

Everything reaches the room through one CLI, and agent hooks are simply its primary callers: `rimz message` reaches any member, `rimz agents -p` runs a supervised turn and returns its answer, and `rimz pane capture` and `rimz pane send` read and type into any pane. Anything you do by hand, a script can do through the same commands: a CI gate messages a reviewer agent, a cron job launches a nightly turn and branches on its exit code, a notification handler wakes a command the moment a row needs eyes. This surface is where harness engineering lands: permission posture, guardrails, and self-correction wire up as your own commands over these primitives, enforced by the room rather than asked of the model.

Loop engineering composes those primitives into a routine: `rimz loop` drives supervised runs on a clock (calendar, interval, cron, poll-until), notification handlers turn `waiting` and `failed` rows into desktop alerts, bells, or command wakeups you build on, and the message system carries steering text between members at safe turn boundaries. The intelligence behind a loop is yours — a bounded script, a smarter model, or another agent through the same harness ([harness.md](./docs/internals/harness/harness.md)).

## Built on what you already run

- Zellij and tmux own panes, sessions, attach/detach, and scrollback; Rimz drives them through a thin backend seam and leaves your keybinds and layout exactly as they were. The store, CLI, and sidebar model are identical on both backends; core behaviour uses only primitives both share.
- Durable state is a directory of flat files written with atomic temp-file-plus-rename: no daemon, no database, no schema to migrate. It survives detach, reload, and reboot, travels over SSH, and reads with `cat`.

## Invariants

Each line is a decision a reader might challenge, with the reason on the same line.

- **One root, one room.** A workspace root — a git repo whose worktrees group inside the room, a project-marker directory, or any directory, `$HOME` and `/` included — maps to one workspace, one mux session, one store, one sidebar, and one live backend, and a rival mux over the same path is refused while the first lives. Ten agents across five branches stay scannable as one room, and a headless box with no source control gets the same room.
- **A pane's workspace is the session it lives in.** Session birth pins the workspace identity into the mux environment and commands honor the verified pin before re-deriving from cwd, so an agent in a nested repo still writes to the room's store. Overlapping rooms are legal and surfaced, and a deliberate per-repo room stays one `rimz start` away.
- **The store owns durability.** Agent state and event history outlive detach, sidebar reload, sidebar crash, and no-client mode; the sidebar renders the store, and correctness lives one layer down.
- **A reborn room offers its fleet back.** After a reboot or mux crash, Rimz offers to re-seed prior agents from the durable rollup into their channel tabs, restored idle (`claude --resume`, `codex resume`, `pi --session`); the prompt defaults to recovery, non-interactive starts recover, and agents you closed deliberately stay closed. Continuity is Rimz-owned and transcript-based — the rebirth is guaranteed, the transcript with asks and answers is durable, and the provider's own resume rides on top as best-effort enrichment.
- **One view-model, many renderers.** The `rimz sidebar snapshot` JSON is the shared view-model; the native pane and CLI listings are projections of it, and any future renderer joins the same way. None owns state; none gates correctness.
- **Degraded frames say so.** The sidebar keeps the last good snapshot and pins a labeled banner with cause and age; banners, the trust state, and `rimz doctor` are where Rimz reports what it cannot currently vouch for.
- **Interactive attach is opportunistic.** `rimz` enters the mux only when stdin/stdout are TTYs and the caller is not already inside it; non-interactive callers get a printed attach command, and explicit flags override.
- **Loop engineering is explicit and per-machine.** A notification handler or scheduled loop runs only where you wire it, lives in per-machine config, and owns its own credentials and policy.
- **Pane I/O is explicit, and it enriches rather than decides.** `pane capture` and `pane send` are public primitives for humans and scripts, and `message` routes human-authored text through the same send path — delivering only at open turn boundaries, held while the agent waits on your answer. Pane contents and transcripts decorate rows; the store, hooks, and explicit events decide permissions, state, and correctness.
- **Headless works.** Hooks and `rimz agents -p` run with no sidebar and no attached client; the sidebar is a UI over a workspace that runs fine without one.

## Non-goals

- A cloud control plane or cross-workspace orchestrator: one project, one room.
- An agents-only control surface: humans and scripts drive the room through the same CLI an agent's hooks use.
- Built-in answer policy: Rimz gets you to the prompt; anything that answers for you is yours to build on the public primitives.
- Process resurrection across host restart: the store survives a reboot; running sessions belong to tmux-resurrect, Zellij resurrect, systemd, or another supervisor.

# Design

Rimz is the harness layer for agentic coding: one human, a fleet of coding agents, one room per project. The room is a Zellij or tmux session with a sidebar that reads the whole fleet at a glance; underneath it, one CLI carries every event into a durable ledger, so the room survives detach, reload, reboot, and reattach from anywhere.

The problem Rimz answers is attention. A fleet emits far more than one person can read — prompts, tool calls, completions, failures, token burn, rate-limit stalls — and Rimz spends that finite attention well: it turns the wall of activity into "this pane, right now, needs you," and otherwise stays quiet. Every choice below serves that job.

> **Invariant.** Rimz routes your attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI. A resolver you wire explicitly may answer routine prompts in that same UI, and the prompt remains there for you.

## Triage at a glance

The sidebar is a presence and attention map: one row per live pane, enriched from the ledger, grouped by worktree. One glance answers what is working, what is blocked, what errored, and what it is costing — and keeps answering as the fleet grows.

- The column arrives triaged: hot work inside the one-hour prompt-cache window ranks first, then warm work up to the 24-hour archive boundary, then archived work, with an attention score ordering each band. A row turns unread the moment it needs you (`waiting`, `failed`, `paused`, `success`) and stays marked until you focus it; exactly one row is ever in motion, so the eye lands where to go next.
- Two symbols carry every call for attention — `?` *needs your answer*, `!` *needs a look* — by shape, so the signal survives `NO_COLOR` and color-blind eyes; color only reinforces. The fixed cockpit line (`? 2  ! 1 …`) compresses the fleet: a row of zeros means nothing needs you, skip the scan.
- Five states cover every agent — `running`, `waiting`, `idle`, `success`, `failed` — plus a derived `paused` for provider-limit parks, with short-lived heads (thinking, compacting, delegating) riding `running`. A context meter, token totals, live dollar cost, diff stats, and last-activity age ride the rows; the provider dashboard carries the pace, its 5h/7d budget bars draining in real time.
- Presence is live and facts are durable: a row exists because a pane runs right now and clears itself when the agent exits, while everything the agent did stays in the ledger. Stats enrich display; the ledger and explicit events decide state and correctness.

The glyph legend and rendered frames live in [the interface reference](./docs/interface/sidebar.md), the ranking and presence mechanics in [sidebar.md](./docs/internals/sidebar/sidebar.md), and the reasoning about what deserves attention in [attention.md](./docs/guide/attention.md).

## Decisions in the agent's own UI

The moment an agent asks to run a command is the moment a human can stop something destructive. Rimz keeps that moment in the agent's own UI and spends its effort getting you there fast; anything you wire to answer for you types into that same UI and records what it did, so delegation changes who answers — never where, and never off the record.

Every ask is recorded with one `surface`, which decides who waits and which answers are legal:

| Surface | Source | Who waits | Behaviour |
| --- | --- | --- | --- |
| `native_ui` | agent hook | no one — hooks return immediately | the ask is recorded, sidebars wake, and the prompt stays in the agent's UI for you or a handler |
| `script` | `rimz feed ask` | the calling script | the script blocks on its per-request socket until `rimz feed resolve` answers or its timeout fires |

These two are the whole contract. An unattended run either carries the agent's own bypass flag (`native_ui`) or a handler that answers in the prompt and records `--by <name>`; sockets, nonces, and CAS rules live in [ledger.md](./docs/internals/sidebar/ledger.md).

## A fleet run like a team

A channel is a named lane where a human and a few agent colleagues work one line of work, backed by a bare name, a Git worktree, an in-place team as `<dir>/<team>`, or the room's directory itself. Spawning separates three independent choices — agents (which tools), layout (the shape on screen), channel (which lane) — so a planner-and-reviewer pair in any lane is one command.

- A member is addressed as `@handle#channel`: `@claude` the kind, `@planner` the role, `@swift-otter` the one instance, `@all` the lane. Addresses resolve against live presence — one match delivers, many ask you to pick (or `--all` fans out), and `--create` launches the member straight from its address. Identity stays light on purpose, enough to name who needs you; the weight is on the cooperation inside the lane.
- `message` reaches a member in either tense: `--steer` types into the live pane now, the default parks text for the next open turn, `--schedule` sets a delivery floor, and `--on done|any` picks the boundary gate. Every mode routes through the one pane-send primitive and records an audit event that omits the message text.
- Automation is a teammate. `rimz agents … -p` drives one member headless — cron, CI, or a script launches the turn, reads the answer or a stream, and branches on the exit code — through the same hooks, ledger, and pane path an interactive member uses.

The launcher, address grammar, delivery gates, and supervised-run machinery live in [harness.md](./docs/internals/agents/harness.md).

## A programmable room

Everything reaches the room through one CLI — `rimz event` announces, `rimz feed` asks and answers, `rimz agents -p` runs a supervised turn — and agent hooks are simply its primary callers. A `terraform apply` or a CI gate posts to the same column an agent uses, so anything an agent can surface, a script can too. The sidebar is one renderer of the shared state; `rimz feed list` is another.

Loop engineering composes those primitives into policy: a notification handler wakes with the request id and pane, inspects with `rimz feed show` and `rimz pane capture`, answers in the agent's own UI with `rimz pane send` or `rimz message`, and records who acted with `rimz feed resolve --by <name>`. The intelligence behind it is yours — a bounded-pattern script, a smarter model, or another agent through the same harness; reference handlers live in [resolvers.md](./docs/internals/agents/resolvers.md).

## Built on what you already run

- Zellij and tmux own panes, sessions, attach/detach, and scrollback; Rimz drives them through a thin backend seam and leaves your keybinds and layout exactly as they were. The ledger, CLI, and sidebar model are identical on both backends; core behaviour uses only primitives both share.
- Durable state is a directory of flat files written with atomic temp-file-plus-rename: no daemon, no database, no schema to migrate. It survives detach, reload, and reboot, travels over SSH, and reads with `cat`.

## Invariants

Each line is a decision a reader might challenge, with the reason on the same line.

- **One root, one room.** A workspace root — a git repo whose worktrees group inside the room, a project-marker directory, or any directory, `$HOME` and `/` included — maps to one workspace, one mux session, one ledger, one sidebar, and one live backend, and a rival mux over the same path is refused while the first lives. Ten agents across five branches stay scannable as one room, and a headless box with no source control gets the same room.
- **A pane's workspace is the session it lives in.** Session birth pins the workspace identity into the mux environment and commands honor the verified pin before re-deriving from cwd, so an agent in a nested repo still writes to the room's ledger. Overlapping rooms are legal and surfaced, and a deliberate per-repo room stays one `rimz start` away.
- **The ledger owns durability.** Ask and event state outlives detach, sidebar reload, sidebar crash, and no-client mode; the sidebar renders the ledger, and correctness lives one layer down.
- **A reborn room offers its fleet back.** After a reboot or mux crash, Rimz offers to re-seed prior agents from the durable rollup into their channel tabs, restored idle (`claude --resume`, `codex resume`, `pi --session`); the prompt defaults to recovery, non-interactive starts recover, and agents you closed deliberately stay closed. Continuity is Rimz-owned and transcript-based — the rebirth is guaranteed, the transcript with asks and answers is durable, and the provider's own resume rides on top as best-effort enrichment.
- **One view-model, many renderers.** The `rimz sidebar snapshot` JSON is the shared view-model; the native pane and CLI listings are projections of it, and any future renderer joins the same way. None owns state; none gates correctness.
- **Degraded frames say so.** The sidebar keeps the last good snapshot and pins a labeled banner with cause and age; banners, the trust state, and `rimz doctor` are where Rimz reports what it cannot currently vouch for.
- **Interactive attach is opportunistic.** `rimz` enters the mux only when stdin/stdout are TTYs and the caller is not already inside it; non-interactive callers get a printed attach command, and explicit flags override.
- **Loop engineering is explicit and per-machine.** A notification handler runs only where you wire it, owns its credentials and policy, answers through pane primitives, and records its name with `--by`.
- **Pane I/O is explicit, and it enriches rather than decides.** `pane capture` and `pane send` are public primitives for humans and handlers, and `message` routes human-authored text through the same send path — delivering only at open turn boundaries, held while an ask is pending. Pane contents and transcripts decorate rows; the ledger, hooks, and explicit events decide permissions, state, and correctness.
- **Headless works.** Hooks, `rimz feed ask`, and `rimz agents -p` run with no sidebar and no attached client; the sidebar is a UI over a workspace that runs fine without one.

## Non-goals

- A cloud control plane or cross-workspace orchestrator: one root, one room.
- An agents-only event surface: humans and scripts announce and answer through the same CLI an agent's hooks use.
- Built-in answer policy: policy is yours to write, and reference handlers ship as examples ([resolvers.md](./docs/internals/agents/resolvers.md)).
- Process resurrection across host restart: the ledger survives a reboot; running sessions belong to tmux-resurrect, Zellij resurrect, systemd, or another supervisor.

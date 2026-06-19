# Design

Rimz gives every project one room: a Zellij or tmux session with a sidebar where you see every coding agent at a glance. One CLI carries every event into one feed, and the room's state survives detach, sidebar reload, and reattach from anywhere.

Rimz stays small because it builds on what you already run: your multiplexer, a flat-file ledger, and one CLI that every event flows through.

> **Invariant.** Rimz routes your attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

The moment an agent asks to run a command is the moment a human can stop something destructive, so Rimz keeps that moment in the agent's own UI and spends its effort getting you there fast. When the routine answers start to repeat, enrol a resolver to handle them ahead of you, explicitly, in a chain that still ends with you.

A worktree is a channel: one copy of the code where a human and a few agent-colleagues work together. You address a colleague by kind within the channel — `@claude#auth-refresh` names the Claude working auth-refresh — and combine kinds the way a team combines roles: a planner, a reviewer, an executor, and in time a tester driving a real browser. Identity stays light on purpose, enough to name who needs you; the weight is on the cooperation between the human and the agents sharing a channel.

## The design problem

One human, many agents, finite attention. A fleet emits far more than one person can read: prompts, tool calls, completions, failures, token burn, rate-limit stalls. The product's whole job is to decide, against that limited attention, what to render and what to highlight — to turn a wall of activity into "this pane, right now, needs you," and otherwise stay quiet. Every design choice below answers that question.

## Design choices

### Attention at a glance

The sidebar is a worktree-keyed presence and attention map: one row per live pane, each agent enriched from the ledger, grouped by the worktree it lives in. It answers four questions in a single glance (*what is working smoothly, what is blocked, what errored, and how much has been done*) and keeps answering them as the fleet grows.

- Attention rises by unread inbox first, then read attention oldest-first, then fresh calm work, then process rows, then calm rows whose activity is past the one-hour attention ceiling. A row becomes unread when it enters `waiting`, `failed`, `paused`, or `success`, including rows already in that state when you attach, and it stays unread until you focus/view it or `rimz sidebar mark-read` clears it. A per-worktree cap trims only the idle/process tail and keeps active, blocked, paused, finished, and focused rows visible. The column arrives triaged.
- Two symbols carry every call for attention: `?` (*needs your answer*) and `!` (*needs a look*). The signal survives `NO_COLOR` and color-blindness; color is a second, redundant channel. Genuinely live work travels through spinner frames. Any unread row breathes with a deeper pulse and bold card until you read it, even if the agent has recovered to `running` or `idle`; calm read rows hold still.
- A fixed cockpit line (`? 2  ! 1 …`) compresses the whole fleet, while the live-agent summary adds a breathing unread count as `¤ 6 (2)` when unseen rows exist. A row of zeros plus no unread count means nothing needs you, so you skip the scan entirely.
- A row exists because a pane is running right now, read live from the multiplexer's pane list. An agent's durable facts (status, task, enrichments) come from the ledger. An agent that exits is gone the moment its pane reverts to a shell; the row clears on its own.

The full glyph vocabulary and every rendered frame live in [the interface reference](./docs/interface/sidebar.md); how presence, ranking, and recovery are computed lives in [docs/internals/sidebar/sidebar.md](./docs/internals/sidebar/sidebar.md).

### Realtime state and rich stats

Routing attention is half the read; the other half is *how the work is going*. Rimz gives every agent a small, legible state and rides it with rich live stats, so progress and health read without ever leaving the sidebar.

- Each agent rolls up to one of five states: `running`, `waiting`, `idle`, `success`, `failed`. Rimz observes that state and derives one of its own: `paused`, a display park for an agent that stopped mid-turn on a provider limit, lifted by provider recovery, window reset, or the next hook event. Short-lived heads ride the running state (thinking, compaction, delegation) so the moment-to-moment phase shows inside one state ([docs/internals/agents/agent.md](./docs/internals/agents/agent.md)).
- A context meter, token totals, live dollar cost, diff stats, todo progress, and a last-activity age ride the rows, so how far along and how healthy read at a glance: which agent is burning toward a rate limit, which is one approval from done. The provider dashboard carries the pace: spend for today, the week, and the month per provider, with 5h/7d budget bars draining in real time, so one look tells you where the week is going. These enrich display; the ledger and explicit events decide state and correctness.

### Run the fleet like a team

A fleet is a team, and Rimz gives it what a team needs: a channel per line of work, a member you can name, and a way to reach that member in either tense. Spawning separates three independent choices — agents (which tools run), layout (the shape on screen), worktree (which channel) — so a planner-and-reviewer pair in any channel is one command.

- A worktree is a channel and an agent is a member, addressed as `@handle#channel`: `@claude` the kind, `@planner` the role, `@swift-otter` the one instance, `@all` the whole channel. The address resolves against live presence — one match delivers, many ask you to pick (or `--all` fans out), zero misses or, with `--create`, launches the member straight from its address.
- You reach a member in two tenses, because work happens in two tenses. `steer` types into a live pane now; `queue` leaves a task that delivers at the member's next open turn, gated by `--on done` or `--on any`. Both route human-authored text through the one pane-send primitive, and both record an audit event that omits the message text.
- Automation is a teammate. A supervised `rimz agents … -p` run drives one member headless — a cron job, a CI gate, a PR hook, or a script launches the turn, reads the final answer or a stream, and exits on a script-friendly status code — through the same hooks, ledger, and pane path an interactive member uses.

The launcher, the address grammar, and the steer/queue and supervised-run machinery live in [docs/internals/agents/harness.md](./docs/internals/agents/harness.md).

### Stay light

Rimz adds a sidebar and a feed to the terminal you already run, and keeps its own footprint small.

- Zellij and tmux already own panes, sessions, attach/detach, and scrollback. Rimz drives them through a thin backend seam and leaves your keybinds, your layout, and your detach/reattach exactly as they were.
- Durable state is a directory of flat files written with atomic temp-file-plus-rename: no daemon to keep alive, no schema to migrate. It survives detach, reload, and reboot, travels over SSH, and reads with `cat`.
- The ledger, the bridge, the CLI, and the sidebar model are identical on Zellij and tmux; core behaviour uses only primitives both backends share.

### One feed, one CLI

Every event reaches the room through one CLI: `rimz event …` to announce, `rimz feed …` to ask and answer, and `rimz agents … -p` to launch one supervised agent turn from a script.

Agent hooks are the primary writers; the same primitives are open to anything else on the machine, so a `terraform apply` or a CI gate can announce itself or post a question to the same feed an agent writes to.

The sidebar is one renderer of that feed; `rimz feed list` is another. Agent integrations are adapters over those primitives, which is what lets a script reach every surface an agent does.

### Resolve when you opt in

By default Rimz observes and routes: an agent asks in its own UI, Rimz writes the feed item, wakes the sidebar, and walks you to the pane. Fast, with the answer where the full context lives.

Resolvers keep that loop running when you step away. A **resolver** is an external process you trust on this machine, enrolled explicitly, that answers routine feed items ahead of you. Resolvers form an ordered chain that always ends with you: a fast policy first, then slower escalation, then you.

Rimz ships the protocol and leaves the policy to you. Two reference resolvers ship as examples — **hook-bridge** for routine permissions and **pane-send** for well-known terminal prompts. The chain, the heartbeat, and the two examples live in [docs/internals/agents/resolvers.md](./docs/internals/agents/resolvers.md).

## The three operating paths

Every actionable feed item is created with one `surface`, and the surface decides who holds the agent's hook open and which answers are legal.

| Surface | Source | Hook blocks? | Behaviour |
| --- | --- | --- | --- |
| `native_ui` | Agent hook, no fresh resolver enrolled | no | The hook writes the feed item, wakes sidebars, and exits. The agent's own UI asks the human. |
| `bridge` | Agent hook with a fresh enrolled resolver | yes | The hook writes the item, binds a per-request socket, and waits up to the agent's hook cap for a resolver answer, falling back to `native_ui` on timeout. |
| `script` | A script that called `rimz feed ask` | yes | The script blocks until a human or resolver answers, or its own timeout fires. No agent involved. |

These three are the whole contract. Unattended auto-approve is one of these paths made permissive: the agent's own bypass flag (`native_ui`) or a permissive resolver (`bridge`). Both leave a record in the ledger. The wire-level surfaces, sockets, and CAS rules are in [docs/internals/sidebar/ledger.md](./docs/internals/sidebar/ledger.md).

## Commitments

Each line is a decision a reader might challenge, with the reason on the same line.

- **One root, one room.** A workspace root (a git repo whose worktrees group inside the room, a project-marker directory, or any directory) maps to one workspace, one multiplexer session, one ledger, one sidebar. A repo with five branches and ten agents stays scannable as one room, and a headless box running agents in a bare directory gets the same room with no source control; its child repos group inside the room the way a repo's worktrees do. The directory tier announces itself at start and refuses exactly `$HOME` and `/` (almost always an accident; `--root` forces).
- **A pane's workspace is the session it lives in.** Session birth stamps an identity pin into the mux environment, and participating commands honor the verified pin before re-deriving from cwd, so an agent in a nested repo still writes to the room's ledger. Room-choosing commands resolve fresh, keeping a deliberate per-repo room one `rimz start` away; overlapping rooms are legal and surfaced (`rimz start` notice, the `rimz doctor` room tree).
- **The ledger owns durability.** Feed state outlives detach, sidebar reload, sidebar crash, and no-client mode. The sidebar renders the ledger; correctness lives one layer down.
- **A reborn room comes back populated.** When a session must be reborn (reboot, multiplexer crash, or a stuck room), Rimz re-seeds the prior agents from the durable rollup into `#channel` tabs grouped by worktree, each restored idle (`claude --resume`, `codex resume`, `pi --session`). Continuity is Rimz-owned and transcript-based: the clean rebirth is guaranteed, and the resume rides on top as best-effort enrichment. Multiplexer serialization brings panes back suspended and unhealthy, so the rebirth builds from the ledger instead.
- **One feed, many renderers.** The `rimz sidebar snapshot` JSON is the shared view-model; the native pane and CLI listings are projections of it — `rimz pane list` joins the live pane spine with the agent overlay the sidebar renders, grouped by tab instead of by worktree — and any future renderer joins the same way. None owns state; none gates correctness.
- **Degraded frames say so.** The sidebar keeps the last good snapshot and pins a labeled banner with cause and age; banners, the trust state, and `rimz doctor` are the places Rimz reports what it cannot currently vouch for.
- **Interactive attach is opportunistic.** `rimz` enters the selected mux only when stdin/stdout are TTYs and the caller is not already inside it; non-interactive callers get a printed attach command, and explicit flags override.
- **Resolvers are explicit and per-machine.** A resolver engages the bridge when it is on the local allowlist *and* heartbeating freshly. The allowlist plus a fresh heartbeat is the trust boundary; same-UID file access alone never grants it.
- **Transcripts and panes enrich display.** Pane contents and transcripts decorate rows; the ledger and explicit events decide permissions, state, and correctness. Core reads a pane only to render it.
- **Pane I/O is explicit.** `pane capture` and `pane send` are public primitives for humans and resolvers. `steer` and `queue` route human-authored text through the same send primitive; deferred delivery types only at done transitions and pending asks hold delivery. State decisions come from the ledger, hooks, and sidecars, while pane reads stay in rendering and resolver-owned inspection.
- **Headless works.** Hooks, the bridge, `rimz feed ask`, and `rimz agents -p` run with no sidebar and no attached client. The sidebar is a UI over a workspace that runs fine without one.

## Non-goals

- Not a cloud control plane or cross-workspace orchestrator: one root, one room.
- An agents-only feed is a non-goal: scripts and humans announce and answer through the same CLI an agent's hooks use.
- Resolver policy is yours to write. Two reference resolvers ship as examples (see [resolvers.md](./docs/internals/agents/resolvers.md)).
- Process resurrection across host restart is the host's job. The ledger survives a reboot; running sessions need tmux-resurrect, Zellij resurrect, systemd, or another supervisor.

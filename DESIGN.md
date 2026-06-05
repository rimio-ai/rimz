# Design

Rimz gives every project one room — a Zellij or tmux session with a sidebar — where you see every coding agent at a glance: what is working smoothly, what is blocked, what errored, and how much each has done. Humans, scripts, CI, and coding agents share one feed through one CLI, and the room's state survives detach, sidebar reload, and reattach from anywhere.

The product is narrow on purpose. Rimz wraps a multiplexer you already run, persists workspace state in a flat-file ledger, surfaces attention in a sidebar, and exposes one CLI that agents, scripts, and humans all speak.

> **Invariant.** Rimz routes your attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

This is the call that shapes everything below. The moment an agent asks to run a command is the moment a human can stop something destructive, so Rimz keeps that moment in the agent's own UI and spends its effort getting you there fast. When the routine answers start to repeat, you enrol a resolver to handle them ahead of you — explicitly, and in a chain that still ends with you.

## The design problem

One human, many agents, finite attention. A fleet emits far more than one person can read: prompts, tool calls, completions, failures, token burn, rate-limit stalls. The product's whole job is to decide, against that limited attention, **what to render and what to highlight** — to turn a wall of activity into "this pane, right now, needs you," and otherwise stay quiet. Every design choice below answers that question.

## Design choices

### Attention at a glance

The sidebar is a worktree-keyed presence and attention map: one row per live pane, agents enriched from the ledger, grouped by the worktree they live in. It is built to answer four questions in a single glance — *what is working smoothly, what is blocked, what errored, and how much has been done* — and to keep answering them as the fleet grows.

- **A small state vocabulary the agent owns.** Each agent rolls up to one of five states — `running`, `waiting`, `idle`, `success`, `failed`. Rimz observes that state; the one state it derives is `rate_limited`, a display park for agents whose account window is spent, lifted the moment the window resets ([docs/internals/agent.md](./docs/internals/agent.md)).
- **Shape carries meaning; color reinforces.** Two symbols carry every call for attention — `?` *needs your answer* and `!` *needs a look* — so the signal survives `NO_COLOR` and color-blindness, with color as a second, redundant channel. Only genuinely-live work animates; a calm, blocked, or finished agent holds still.
- **Ranking is the triage.** The most overdue attention rises to the top, oldest first; calm work settles below; a per-worktree cap trims only the calm tail and never hides a row that needs you. You don't sort — the column arrives triaged.
- **One line summarises the room.** A fixed cockpit make-up (`? 2  ! 1 …`) compresses the whole fleet to a single line: a row of zeros means nothing needs you, so you skip the scan entirely.
- **Rich stats, display-only.** A context meter, token totals, diff stats, todo progress, a last-activity age, and account-scoped usage budgets ride the rows and a per-provider dashboard, so "how far along" and "how healthy" read without leaving the sidebar. These enrich display; they never drive a decision.

The full glyph vocabulary and every rendered frame live in [the interface reference](./docs/interface/sidebar.md); how presence, ranking, and recovery are computed lives in [docs/internals/sidebar.md](./docs/internals/sidebar.md).

One law sits under all of it: **presence is live, the ledger is truth.** A row exists because a pane is running right now — read live from the multiplexer's pane list — while an agent's durable facts (status, task, enrichments) come from the ledger. An agent that exits is gone the moment its pane reverts to a shell; liveness is the live process, never a status the ledger has to retract.

### Stay light

Rimz adds a sidebar and a feed to the terminal you already run.

- **Wrap the multiplexer; don't build one.** Zellij and tmux already own panes, sessions, attach/detach, and scrollback, and your muscle memory already lives there. Rimz drives them through a thin backend seam and leaves your keybinds, your layout, and your detach/reattach exactly as they were.
- **A ledger, not a database.** Durable state is a directory of flat files written with atomic temp-file-plus-rename. There is no daemon to keep alive and no schema to migrate; it survives detach, reload, and reboot, travels over SSH, and you can read it with `cat`.
- **Both backends are first-class.** The ledger, the bridge, the CLI, and the sidebar model are identical on Zellij and tmux, and core behaviour leans on no Zellij-only pipe or tmux-only feature.

### One feed, three audiences

Anything that wants to participate publishes or resolves through one CLI — `rimz event …` to announce, `rimz feed …` to ask and answer. An agent hook, a `terraform apply`, a CI gate, and you all write to the same feed and read the same room. The sidebar is one renderer of that feed; `rimz feed list` is another. Agent integrations are adapters layered on the same primitives a shell script uses, so a script reaches every surface an agent does.

### Observe by default; resolve when you opt in

Out of the box, Rimz observes and routes: an agent asks in its own UI, Rimz writes the feed item, wakes the sidebar, and points you at the pane. The default loop answers nothing on your behalf — it gets you to the question fast and lets you answer where the full context lives.

When the routine answers start repeating, enrol a **resolver** — an external process you trust on this machine — to handle them ahead of you. Resolvers form an ordered chain (a fast policy, then a slower human escalation, then you) that always ends with you, and they are the path to continuous, unattended agent work. Rimz ships the protocol rather than a policy: the contract is the product. Two reference resolvers ship as examples to prove it — **hook-bridge**, a permission policy that answers routine read-only tool calls on the bridge, and **pane-send**, which answers well-known terminal prompts through the pane primitives. The chain, the heartbeat, and the two examples live in [docs/internals/resolvers.md](./docs/internals/resolvers.md).

## The three operating paths

Every actionable feed item is created with one `surface`, and the surface decides who holds the agent's hook open and which answers are legal.

| Surface | Source | Hook blocks? | Behaviour |
| --- | --- | --- | --- |
| `native_ui` | Agent hook, no fresh resolver enrolled | no | The hook writes the feed item, wakes sidebars, and exits. The agent's own UI asks the human. |
| `bridge` | Agent hook with a fresh enrolled resolver | yes | The hook writes the item, binds a per-request socket, and waits up to the agent's hook cap for a resolver answer — falling back to `native_ui` on timeout. |
| `script` | A script that called `rimz feed ask` | yes | The script blocks until a human or resolver answers, or its own timeout fires. No agent involved. |

These three are the whole contract. Unattended auto-approve is one of these paths made permissive — the agent's own bypass flag (`native_ui`) or a permissive resolver (`bridge`) — and both leave a record in the ledger. The wire-level surfaces, sockets, and CAS rules are in [docs/internals/ledger.md](./docs/internals/ledger.md).

## Commitments

Each line is a decision a reader might challenge, with the reason on the same line.

- **One repo, one room.** A project repo maps to one workspace, one multiplexer session, one ledger, one sidebar; worktrees of the repo group inside it. A repo with five branches and ten agents stays scannable as one room.
- **The ledger owns durability.** Detach, sidebar reload, sidebar crash, or no-client mode never lose feed state. The sidebar is a renderer over the ledger; correctness lives one layer down.
- **A reborn room comes back, not empty.** When a session must be reborn — reboot, multiplexer crash, or a rebirth of a stuck room — Rimz re-seeds the prior agents from the durable rollup, each restored idle in its own pane (`claude --resume`, `codex resume`, `pi --session`). Continuity is Rimz-owned and transcript-based, not multiplexer serialization (which resurrects suspended, unhealthy panes); the clean rebirth is guaranteed, the resume is best-effort enrichment over it.
- **One feed, many renderers.** The `rimz sidebar snapshot` JSON is the shared view-model; the native pane and CLI listings are projections of it, and any future renderer joins the same way. None owns state; none gates correctness.
- **Interactive attach is opportunistic.** `rimz` enters the selected mux only when stdin/stdout are TTYs and the caller is not already inside it; non-interactive callers get a printed attach command, and explicit flags override.
- **Resolvers are explicit and per-machine.** A resolver engages the bridge only when it is on the local allowlist *and* heartbeating freshly. Same-UID file access is not the trust boundary.
- **Transcripts and panes enrich display.** Pane contents and transcripts decorate rows; the ledger and explicit events decide permissions, state, and correctness. Core reads a pane to render, never to decide.
- **Pane I/O is a resolver primitive.** `pane capture` and `pane send` are public primitives for humans and resolvers; core treats panes as opaque and types into none.
- **Headless works.** Hooks, the bridge, and `rimz feed ask` run with no sidebar and no attached client. The sidebar is a UI over a workspace that runs fine without one.

## Non-goals

- Not a cloud control plane or cross-workspace orchestrator.
- Agents are optional — scripts and humans are first-class citizens of the feed.
- Ships no resolver as core product. The protocol is the contract; two reference resolvers ship as examples (see [resolvers.md](./docs/internals/resolvers.md)).
- Process resurrection across host restart is the host's job. The ledger survives a reboot; running sessions need tmux-resurrect, Zellij resurrect, systemd, or another supervisor.

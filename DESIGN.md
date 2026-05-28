# Design

Rimz turns a project into one durable multiplexer room with a shared feed. The product is narrow on purpose: wrap Zellij or tmux, persist workspace state in a ledger, surface attention in a sidebar, and let humans, scripts, CI, and coding agents participate through one CLI.

> **Invariant.** Rimz routes attention. By default, it does not answer for you.

The agent's own UI stays the answer surface. This is the call that separates Rimz from every other agent multiplexer. Silently auto-approving a tool call hides the only moment a human can stop something destructive. Rimz refuses that move by default. When you want help, you enrol a resolver explicitly; the resolver chain ends with you.

## The three operating paths

Every actionable feed item is created with one `surface`. The surface decides whether anyone is holding the hook open and what answer paths are legal.

| Surface | Source | Hook blocks? | Runtime behaviour |
| --- | --- | --- | --- |
| `native_ui` | Agent hook with no fresh enrolled resolver | no | Hook writes the feed item, wakes sidebars, prints the neutral payload, and exits. The agent's own UI asks the human. |
| `bridge` | Agent hook with a fresh enrolled resolver | yes | Hook writes the feed item, binds a per-request socket, and waits up to the agent's hook cap for a resolver answer. Falls back to native UI on timeout. |
| `script` | A script that called `rimz feed ask` | yes | Script blocks until a human or resolver answers, or until the script's own timeout fires. No agent involved. |

These three paths are the product contract. There is no fourth path; Rimz core never adds a hidden auto-approve. Unattended auto-approve is `native_ui` with the agent's own bypass flag, or `bridge` with a permissive resolver — both visible in the ledger.

## Commitments

Each commitment is a decision a reader might challenge. The reason is on the same line.

- **One repo, one room.** A project repo maps to one workspace, one multiplexer session, one ledger, one sidebar. Worktrees of the repo group inside it. A repo with five branches and ten agents stays scannable as one room with internal subdivisions.
- **The feed is the shared surface.** Agents, scripts, CI shims, and humans publish or resolve through `rimz event ...` and `rimz feed ...`. One concept to learn, three audiences to serve.
- **The ledger owns durability.** Detach, sidebar reload, sidebar crash, or no-client mode never lose feed state. The sidebar is a renderer over the ledger; correctness lives one layer down.
- **One feed, many renderers.** The `rimz sidebar snapshot` JSON is the shared view-model; every sidebar is a projection of it. The default renderer is the native binary in a pane, identical on Zellij and tmux and across detach/reattach. Zellij users may opt in to a docked plugin rail for nicer placement. Renderers are interchangeable presentation; none owns state and none gates correctness.
- **Interactive attach is opportunistic.** `rimz` enters the selected mux only when stdin/stdout are TTYs and the caller is not already inside that mux. Non-interactive callers get a printed attach command; explicit flags override the default.
- **Observe and route by default.** Without an enrolled resolver, Rimz never answers an agent prompt. It tells the human which pane needs attention and gets out of the way.
- **Resolvers are explicit and per-machine.** A resolver engages the bridge only if it is on the local allowlist *and* it is heartbeating freshly. Same-UID file access is not the trust boundary.
- **No transcript correctness.** Pane contents and transcripts may enrich display only. They never decide permissions, state transitions, or correctness. Core code never scrapes a pane.
- **No core auto-type.** `pane capture` and `pane send` are public primitives for humans and resolvers. Rimz core does not type into panes on anyone's behalf.
- **Both multiplexers are first-class.** Zellij and tmux run the same ledger, bridge, CLI, and sidebar model. Core behaviour cannot depend on a Zellij-only pipe or a tmux-only feature. The optional Zellij plugin rail is an enhancement only; tmux reaches the same surface through the native pane.
- **Headless works.** Hooks, the bridge, and `rimz feed ask` work with no sidebar and no attached client. The sidebar is a UI; the workspace runs without one.

## Sidebar shape

The sidebar is a **worktree-keyed presence and attention map**, not a feed reader. Three laws govern it:

- **Worktree-first.** A worktree is total isolation — only same-worktree agents collaborate — so the sidebar groups by worktree before anything else.
- **Notify, don't answer.** The sidebar routes you to the pane that needs you; it never reproduces the agent's question. You read and answer in the agent's own UI. (A script's `feed ask`, which chose Rimz as its surface, is the exception.)
- **Presence and attention, never history.** It shows what is *running now* — every pane's foreground process, with agents enriched from the ledger — and what *needs you* (waiting/failed agents, pending items). Nothing resolved or historical; that lives in `rimz feed list`.

**Presence is a live view; the ledger is truth.** Row presence is read live from the multiplexer's pane list — a pane running `zsh` is a row, and it becomes the agent's row when that pane runs an agent. The ledger stays the source of attention and of every durable fact; the pane list never decides correctness. A hook-driven agent that exits is gone the moment its pane reverts to a shell or closes — liveness is the live process, not a status the ledger has to retract.

Agent statuses (exactly five), highest attention first. Each maps to one glyph and color — the canonical vocabulary every renderer paints. The glyph carries the status by **shape** (so it survives `NO_COLOR`); color reinforces it. A pane running a non-agent process (a bare shell, an editor) renders as a dim process row with no status glyph and never counts as attention.

| rank | status    | glyph | color       |
|------|-----------|-------|-------------|
| 1    | `waiting` | `◆`   | yellow bold |
| 2    | `failed`  | `✗`   | red bold    |
| 3    | `running` | `▸`   | green       |
| 4    | `idle`    | `○`   | gray / dim  |
| 5    | `success` | `✓`   | green dim   |

Agent modes (observed from the agent, not set by Rimz) render as a dim pill; `interactive` and `unknown` are omitted, `bypass` is warn-colored:

```text
interactive   plan   auto   bypass   unknown
```

Renderer details — the two-line row anatomy, attention ranking, and the jump interaction — live in [docs/internals/sidebar.md](./docs/internals/sidebar.md).

## Non-goals

- Not a cloud control plane or cross-workspace orchestrator.
- Not agent-required — scripts and humans are first-class.
- Ships no resolver as product. The protocol is the contract; resolver code is user code.
- Does not own process resurrection across host restart. The ledger survives a reboot; running sessions need tmux-resurrect, Zellij resurrect, systemd, or another host supervisor.

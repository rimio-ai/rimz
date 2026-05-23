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
- **The ledger owns durability.** Detach, plugin reload, sidebar crash, or no-client mode never lose feed state. The sidebar is a renderer over the ledger; correctness lives one layer down.
- **Observe and route by default.** Without an enrolled resolver, Rimz never answers an agent prompt. It tells the human which pane needs attention and gets out of the way.
- **Resolvers are explicit and per-machine.** A resolver engages the bridge only if it is on the local allowlist *and* it is heartbeating freshly. Same-UID file access is not the trust boundary.
- **No transcript correctness.** Pane contents and transcripts may enrich display only. They never decide permissions, state transitions, or correctness. Core code never scrapes a pane.
- **No core auto-type.** `pane capture` and `pane send` are public primitives for humans and resolvers. Rimz core does not type into panes on anyone's behalf.
- **Both multiplexers are first-class.** Zellij and tmux run the same ledger, bridge, CLI, and sidebar model. Core behaviour cannot depend on a Zellij-only pipe or a tmux-only feature.
- **Headless works.** Hooks, the bridge, and `rimz feed ask` work with no sidebar and no attached client. The sidebar is a UI; the workspace runs without one.

## Sidebar shape

Four display groups: **Needs your attention** · **Resolver is working** · **Recently answered** · **Recent activity**.

Agent statuses (exactly five):

```text
running   waiting   idle   success   failed
```

Agent modes (observed from the agent, not set by Rimz):

```text
interactive   plan   auto   bypass   unknown
```

Renderer details and action rules live in [docs/internals/sidebar.md](./docs/internals/sidebar.md).

## Non-goals

- Not a cloud control plane or cross-workspace orchestrator.
- Not agent-required — scripts and humans are first-class.
- Ships no resolver as product. The protocol is the contract; resolver code is user code.
- Does not own process resurrection across host restart. The ledger survives a reboot; running sessions need tmux-resurrect, Zellij resurrect, systemd, or another host supervisor.

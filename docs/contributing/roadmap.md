# Roadmap

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Build order keeps the highest-risk runtime work ahead of agent-specific behaviour. Live milestone status lives in [status.md](../../status.md); the gate stack is in [rust-conventions.md](./rust-conventions.md).

## M0 — Bridge spike

No Claude, no Codex, no notifications, no sound, no sub-agents, no real UI.

- **M0a** — multiplexer-neutral ledger and bridge with a synthetic hook driver.
- **M0b** — Zellij backend spike on top of M0a.
- **M0c** — tmux backend spike on top of M0a.

M0 closes only when all three pass the same synthetic-hook matrix in [testing.md](./testing.md).

## M1 — Useful without agents

Workspace start/attach/list, setup and doctor, trust gate, `event emit`, `feed push/ask/wait`, `pane split/focus/list`, the sidebar renderer (worktree-keyed attention map), resolver allowlist commands, first-run setup status.

At M1, scripts and remote workflows work without any agent integrations.

## M2 — Codex adapter

Internal proof of the agent integration interface. Lifecycle hooks, the blocking-feed permission path (default and bridge), neutral and decision goldens, both backends. Mapping in [hooks.md → Appendix Codex](../internals/hooks.md#appendix--codex).

Codex alone is not the public agent-coding launch bar.

## M3 — Claude Code adapter

Public agent-coding bar. Lifecycle hooks and all three blocking-feed kinds (permission, plan approval, user question), default and bridge paths for each, modified-input decision goldens, both backends. Mapping in [hooks.md → Appendix Claude](../internals/hooks.md#appendix--claude-code).

At M3, Claude and Codex are peers on the integration interface.

## M3.5 — Reference resolvers

Test artifacts, not product. A hook-bridge resolver and a pane-send resolver. They prove external clients can resolve feed items through the public CLI alone.

## M4 — Remote durability

Detach/reattach polish, sidebar reload recovery, protocol-version doctor checks, minimum mux version checks, trust-stale auto-revoke, `workspace migrate`, GC (the global janitor that also prunes dead workspaces) and event-log rotation.

## M5 — Attention polish

OS notifications, sounds, user/project notification policy.

## Follow-up — Sidebar interaction polish

The approved design in [sidebar.md](../internals/sidebar.md) now drives the native renderer: the snapshot carries worktree-grouped rows, capability fields, attention ranking, per-worktree caps, and history stays out of the sidebar. Remaining interaction work:

- Make rows jump targets: rail click/keys plus a native-pane key handler, both calling `focus_pane`.
- Expand renderer parity for any future Zellij rail projection of the same enrichment grammar.

## M6 — Sub-agent observability

Done: both agents' subagents flow through subagent-start/stop observations, keyed by child `agent_id` with the root captured as `parent_agent_id`; the snapshot nests each child under its parent and the expanded card lists them with turn-scoped retention (mapping in [hooks.md](../internals/hooks.md); rollup in [agent.md](../internals/agent.md) and [sidebar.md → Sub-agent lists](../internals/sidebar.md#sub-agent-lists)).

Remaining: gated auto-open of a subagent surface only when preconditions are met.

## M7 — Additional agents

Done: the Pi adapter — a Rimz-authored in-process extension forwarding pi's lifecycle events with the payload-stamped context gauge, gating tools through the blocking `tool_call` bridge, whole-file install, resume, spend, and the account probe (mapping in [hooks.md → Appendix Pi](../internals/hooks.md#appendix--pi); upstream surface in [adapter/pi-reference.md](../internals/adapter/pi-reference.md)).

Remaining: the OpenCode adapter — upstream surface mirrored and live-verified in [adapter/opencode-reference.md](../internals/adapter/opencode-reference.md), with the proposed mapping and the unsupportable surfaces in its [Mapping feasibility](../internals/adapter/opencode-reference.md#mapping-feasibility) — other agents when their extension APIs and decision contracts are stable enough to test, plus Pi's one unwired increment (a model-change marker — [pi-reference.md → Mapping feasibility](../internals/adapter/pi-reference.md#mapping-feasibility)).

## Follow-up — Fleet-room depth

Done: non-git roots are first-class — typed root classes with session-pinned identity ([cli.md → Workspace resolution](../reference/cli.md#start-and-attach-a-workspace)), and the directory room groups panes by depth-1 child repos with a name-only root pod ([sidebar.md → Worktree groups](../internals/sidebar.md#worktree-groups)).

Done: repo-backed rooms can launch agents through Rimz-owned worktrees and backend-native tabs — `rimz worktree`, `rimz tab`, and `rimz agents` share the same marker and layout IR ([worktrees.md](../internals/worktrees.md)).

Deferred, in design-readiness order:

- **Depth >1 enumeration.** A repo at `<root>/org/repo` folds into the root pod today (the v1 depth rule). A deeper scan needs a depth/ignore policy before it pays its `read_dir` fan-out.
- **Child-repo worktrees parked outside the room.** A child repo's own `git worktree list` is not consulted, so a checkout it parked elsewhere folds into `external`. Enumerating per-child worktrees multiplies the probe fan-out; wants real demand first.
- **Cross-session aggregation.** A parent room never ingests a nested live room's ledger — overlap stays notice-and-doctor ([cli.md](../reference/cli.md#start-and-attach-a-workspace)). If aggregation is ever built it is read-side only (one ledger per workspace stays the write invariant), and the jump cannot cross Zellij sessions — both are why it waits.

## Optional — Zellij docked plugin rail

A UX upgrade, not a milestone gate. A `wasm32-wasip1` plugin (`crates/rimz-sidebar-zellij`) that projects the snapshot view-model as a docked, persistent left rail for Zellij users who opt in (`[layout.zellij] sidebar_plugin`). In-sandbox snapshot-JSON ingress off the `zellij pipe --name rimz::feed` wakeup, idempotent `launch-or-focus-plugin`. The native pane stays the default and the fallback; the rail never gates correctness.

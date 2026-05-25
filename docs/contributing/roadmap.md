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

Internal proof of the agent integration interface. Lifecycle hooks, `PermissionRequest` default and bridge paths, neutral and decision goldens, mode observation, both backends.

Codex alone is not the public agent-coding launch bar.

## M3 — Claude Code adapter

Public agent-coding bar. Lifecycle hooks, `PermissionRequest`, `PreToolUse: ExitPlanMode`, `PreToolUse: AskUserQuestion`, default and bridge paths for each, `updatedInput` goldens, both backends.

At M3, Claude and Codex are peers on the integration interface.

## M3.5 — Reference resolvers

Test artifacts, not product. A hook-bridge resolver and a pane-send resolver. They prove external clients can resolve feed items through the public CLI alone.

## M4 — Remote durability

Detach/reattach polish, sidebar reload recovery, protocol-version doctor checks, minimum mux version checks, trust-stale auto-revoke, `workspace migrate/prune`, GC and event-log rotation.

## M5 — Attention polish

OS notifications, sounds, user/project notification policy.

## Follow-up — Sidebar attention redesign

The approved design in [sidebar.md](../internals/sidebar.md) reflows the renderer for the narrow (30%) column and reframes it as a worktree-keyed attention map. The M1 renderer diverges from it; tracked work to converge:

- Reflow `rimz-sidebar` to the spec — two-line agent cells, per-row activity age, attention-ranked sort, per-worktree cap, no "updated" footer — with narrow-width (24/28-col) snapshot fixtures replacing the 80/96-col ones.
- Make rows jump targets: rail click/keys plus a native-pane key handler, both calling `focus_pane`.
- Extend the snapshot — `AgentState.task`, `.model`, `.effort`, a last-activity timestamp, and (future) `.token_budget` — fed by the agent hooks.
- Drop the `recently_answered` / `recent_activity` projection from the sidebar; history stays in `rimz feed list`.

## M6 — Sub-agent observability

Codex thread events, parent/child rendering, gated auto-open only when preconditions are met.

## M7 — Additional agents

OpenCode, Pi, and other agents when their extension APIs and decision contracts are stable enough to test.

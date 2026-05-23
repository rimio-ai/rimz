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

Workspace start/attach/list, setup and doctor, trust gate, `event emit`, `feed push/ask/wait`, `pane split/focus/list`, sidebar renderer groups, resolver allowlist commands, first-run setup status.

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

## M6 — Sub-agent observability

Codex thread events, parent/child rendering, gated auto-open only when preconditions are met.

## M7 — Additional agents

OpenCode, Pi, and other agents when their extension APIs and decision contracts are stable enough to test.

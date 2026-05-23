# Sidebar

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

The sidebar is the product surface. It's a UI client over the workspace ledger; it owns no durable state. Read the ledger through `rimz sidebar snapshot`, write liveness through `rimz sidebar heartbeat`, and never import a ledger-writer module.

## What it looks like

```
┌─ billing-service ─────────────────────────────────────────┐
│                                                           │
│  Worktree: main                                           │
│  Needs your attention                                     │
│  ▶ claude    waiting · permission: psql DROP TABLE        │
│                                                           │
│  Worktree: feature-migration                              │
│  Resolver is working                                      │
│    ▸ opus-policy active · 18s left → slack-on-call (5m)   │
│                                                           │
│  Workspace                                                │
│  Needs your attention                                     │
│    deploy.sh · staging → prod ?     [yes] [no] [abort]    │
│                                                           │
│  Recently answered                                        │
│    codex     feature-migration · success            (hook)│
│    build     ✓ tests pass                           (cli) │
│                                                           │
│  Recent activity                                          │
│    SessionStart  claude   main              12s ago       │
│    Stop          codex    feature-migration 48s ago       │
└───────────────────────────────────────────────────────────┘
```

> Product invariant lives in [DESIGN.md](../../DESIGN.md).

Agent items in **Needs your attention** show focus/dismiss, not approve/deny. Script items may show declared options as buttons — that script chose Rimz as its decision surface.

## State access

On load and tick:

```text
rimz sidebar snapshot --workspace-id <id>
rimz sidebar heartbeat --workspace-id <id> --instance-id <id> \
  --mux <zellij|tmux> --session-name <name> --wakeup-socket <path>
```

The heartbeat binds `sock/sidebar.<instance_id>.sock` and writes:

- workspace ID,
- session name,
- mux backend,
- sidebar instance ID,
- protocol version,
- wakeup socket path,
- last-seen timestamp.

On wakeup, the sidebar refetches the snapshot. Missed wakeups are closed by polling (~2s tick).

## Information architecture

The four groups, in render order:

- **Needs your attention.** Items waiting on a human, grouped by worktree.
  - `native_ui`: an agent prompt is waiting in its pane. Action: **focus pane** or **dismiss** (local ack; does not answer the agent).
  - `script`: a script asked. Action: **answer buttons** for declared options, or **focus the script's pane**.
  - `bridge`: shown here only when the chain is exhausted and the item has not yet downgraded.
- **Resolver is working.** Bridge-held items past a short threshold. Renders the active resolver chain: ticked links, current link with remaining budget, queued links. Humans can override with `feed resolve --override-chain`.
- **Recently answered.** Resolved, dismissed, or agent-moved-on items. Resolution method labelled — `hook_bridge`, `pane_send`, `cli`, `sidebar`.
- **Recent activity.** Lifecycle, completions, failures, telemetry, sub-agent events. Read-only.

## Agent rollup

Agents are grouped by worktree. The five-value status set and the five-value mode pill are defined in [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape); the renderer maps each value to its column verbatim.

## Action rules

- `native_ui` items never show approve/deny. Rimz cannot deliver an answer to the agent's own UI.
- `script` items render declared options as buttons because the script committed to Rimz as the answer surface.
- `bridge` items can be resolved manually with `--override-chain` when a human wants to preempt a slow Slack escalation.
- Focus reconciles pane ID *and* process start time so a reused pane ID never silently focuses a stranger.

## Notifications

Native notifications are best-effort polish; the ledger remains authoritative. Opt-in per workspace via `[notifications]` in project or per-machine config.

Notify on:

- agent enters `waiting`,
- resolver picks up or hands off an item,
- bridge falls back to native prompt,
- item is answered,
- agent resumes after waiting,
- agent stays `waiting` past a configured threshold.

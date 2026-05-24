# Rimz

Run one coding agent and you flip tabs. Run four and you lose them.

Rimz pins every repo to one durable room — a Zellij or tmux session with a sidebar that tells you which pane needs you, and a ledger that survives detach, sidebar reload, and reattach from anywhere. Humans, scripts, CI, and coding agents share the same feed through one CLI.

```
  ┌─ billing-service ───────────────────────────────────┐
  │                                                     │
  │  Needs your attention                               │
  │  ▶ claude  · main          · waiting · permission   │
  │    deploy  · deploy.sh     · staging → prod?        │
  │                                                     │
  │  Resolver is working                                │
  │    ▸ opus-policy active · 18s left → slack-on-call  │
  │                                                     │
  │  Recently answered                                  │
  │    codex   · feature-migration · success     (hook) │
  │    build   · ✓ tests pass                    (cli)  │
  │                                                     │
  │  Recent activity                                    │
  │    SessionStart   claude#1     12s ago              │
  │    Stop           codex        48s ago              │
  └─────────────────────────────────────────────────────┘
```

> Product invariant lives in [DESIGN.md](./DESIGN.md). The short version: the agent's own UI stays the answer surface unless you explicitly enrol a resolver. Nothing in Rimz silently approves a tool call.

## Quickstart

```sh
cd ~/code/billing-service
rimz                                                # open or reattach the room

rimz event emit --kind build.started --title "web"  # any script can post
rimz feed ask --title "Promote staging → prod?" \
              --options yes,no --timeout 1h         # block on a human or resolver

ssh dev-box rimz attach billing-service             # reattach from anywhere
rimz pane split --view new && claude                # start an agent in a new view
```

That's the whole loop. Everything else is variations on those five commands.

## Why Rimz

- **Agent users.** See at a glance which of four parallel Claude or Codex sessions needs you, and answer in the agent's own UI. No silent auto-approve. No tab roulette.
- **Remote developers.** Start the room on a host or container, walk away, reattach from a laptop, tablet, or SSH client on a phone. Pickup is zero-cost.
- **Script and tool authors.** Make a `terraform apply`, a 4-hour migration, or a CI gate a first-class citizen of the sidebar through `rimz event emit` and `rimz feed ask`. No UI to build.

## How it works

One repo maps to one Rimz workspace and one multiplexer session. Git worktrees of that repo group inside it. Every event — agent hooks, script announcements, build results, blocking questions — writes through one CLI to a durable file-backed ledger. The sidebar is a renderer over that ledger; the ledger doesn't care whether anyone is watching.

Design commitments and the three operating paths (`native_ui`, `bridge`, `script`) live in [DESIGN.md](./DESIGN.md). The wire-level state machine, surfaces, and CAS rules live in [docs/internals/ledger.md](./docs/internals/ledger.md).

## Status

Documentation-first. Implementation lands in milestones — M0 spikes the ledger and bridge across Zellij and tmux; agent adapters follow at M2 (Codex) and M3 (Claude Code). See [docs/roadmap.md](./docs/contributing/roadmap.md).

## Read next

- **Use it.** [docs/product.md](./docs/guide/product.md) — five-minute tour of the sidebar, the feed, and the three audiences.
- **CLI surface.** [docs/cli.md](./docs/reference/cli.md) — every command, grouped by intent.
- **Understand the design.** [DESIGN.md](./DESIGN.md) — commitments and the three operating paths.
- **Contribute.** [AGENTS.md](./AGENTS.md) — engineering rules and the docs map. `cargo xtask ci` runs every quality gate locally; the gate stack is in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).

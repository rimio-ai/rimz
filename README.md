# Rimz

Run one coding agent and you flip tabs. Run four and you lose them.

Rimz pins every repo to one durable room — a Zellij or tmux session with a sidebar that tells you which pane needs you, and a ledger that survives detach, sidebar reload, and reattach from anywhere. Humans, scripts, CI, and coding agents share the same feed through one CLI.

```
┌ query-engine ──────────────┐
│ ◆2  ✗1                     │
│                            │
│ ▌main             2◐ 1◆    │
│ ◆ claude  fix auth flow 12m│
│   Opus · xhigh █▉░░░ 38%   │
│ ◐ claude  add tests     8s │
│   Sonnet · high            │
│ ◐ codex   refactor api 30s │
│   GPT-5.5 · high           │
│                            │
│ ▌feature-migration 1✗ 1○   │
│ ✗ claude  db migrate    4m │
│   Opus · xhigh · yolo      │
│ ○ codex   —             1h │
│   GPT-5.5 · low            │
│                            │
│ ▌workspace         1◆      │
│ ◆ deploy  promote?      5m │
│                            │
│ ↵ focus                    │
└────────────────────────────┘
```

> Product invariant lives in [DESIGN.md](./DESIGN.md). The short version: the agent's own UI stays the answer surface unless you explicitly enrol a resolver. Nothing in Rimz silently approves a tool call.

## Quickstart

```sh
cd ~/code/query-engine
rimz                                                # open or reattach the room

rimz event emit --kind build.started --title "web"  # any script can post
rimz feed ask --title "Promote staging → prod?" \
              --options yes,no --timeout-seconds 3600

ssh dev-box rimz attach query-engine                # reattach from anywhere
rimz pane split && claude                           # start an agent in a new pane
```

That's the whole loop. Everything else is variations on those five commands.

## Why Rimz

- **Agent users.** See at a glance which of four parallel Claude or Codex sessions needs you, and answer in the agent's own UI. No silent auto-approve. No tab roulette.
- **Remote developers.** Start the room on a host or container, walk away, reattach from a laptop, tablet, or SSH client on a phone. Pickup is zero-cost.
- **Script and tool authors.** Make a `terraform apply`, a 4-hour migration, or a CI gate a first-class citizen of the sidebar through `rimz event emit` and `rimz feed ask`. No UI to build.

## How it works

One repo maps to one Rimz workspace and one multiplexer session. Git worktrees of that repo group inside it. Every event — agent hooks, script announcements, build results, blocking questions — writes through one CLI to a durable file-backed ledger. The sidebar is a renderer over that ledger; the ledger doesn't care whether anyone is watching.

Design commitments and the three operating paths (`native_ui`, `bridge`, `script`) live in [DESIGN.md](./DESIGN.md). The wire-level state machine, surfaces, and CAS rules live in [docs/internals/ledger.md](./docs/internals/ledger.md).

## Development

The Rust toolchain is pinned by [rust-toolchain.toml](./rust-toolchain.toml). Zellij or tmux is needed to try the room and pane flows.

```sh
make build      # cargo xtask build
make install    # install rimz and rimz-sidebar
make test       # cargo xtask test
make ci         # full quality gate
```

`make` is a thin wrapper around `cargo xtask <task>`; `xtask` is the source of truth for automation. `make install` writes binaries to `${CARGO_HOME:-$HOME/.cargo}/bin`, so that directory must be on `PATH`.

After installing, smoke-test the CLI:

```sh
rimz ping
rimz doctor
rimz start --print .
```

Contributor rules, gate details, and task names live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).

## Status

Documentation-first. Implementation lands in milestones — M0 spikes the ledger and bridge across Zellij and tmux; agent adapters follow at M2 (Codex) and M3 (Claude Code). See [docs/roadmap.md](./docs/contributing/roadmap.md).

## Read next

- **Use it.** [docs/product.md](./docs/guide/product.md) — five-minute tour of the sidebar, the feed, and the three audiences.
- **CLI surface.** [docs/cli.md](./docs/reference/cli.md) — every command, grouped by intent.
- **Understand the design.** [DESIGN.md](./DESIGN.md) — commitments and the three operating paths.
- **Contribute.** [AGENTS.md](./AGENTS.md) — engineering rules and the docs map. `cargo xtask ci` runs every quality gate locally; the gate stack is in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).

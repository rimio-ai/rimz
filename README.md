# Rimz

Run one coding agent and you flip tabs. Run four and you lose them.

Rimz pins every repo to one durable room — a Zellij or tmux session with a sidebar that tells you which pane needs you, and a ledger that survives detach, sidebar reload, and reattach from anywhere. Humans, scripts, CI, and coding agents share one feed through one CLI.

```
 ⌘ query-engine                    ~/code/query-engine

 ◎ 91                          ◇ 32M ↘ 28M ↗ 3M ◌ 472M
 ¤ 16                                          $420.00
 ─────────────────────────────────────────────────────
 ? 3   ! 0   ⏸ 0   ✓ 8                       ⢿ 3   ○ 2

▏⑂ feature ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ +127 -43
▌⣾ claude · Opus 4.8 · xhigh · 1m                $1.27
▌  ledger refactor
▌  ▣ ━━━━━━━━━━━━━━━━─────────────────────────── 38.2%
▌  ▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k                   ◔ 1m
▌  ⧉ subagents (2)
▌    ✓ Explore — locate the render seam
▌      ◇ 12k · Opus 4.8                          ◔ 10m
▌    ✻ Explore — audit the trust hash
▌      ◇  3k · Opus 4.8                          ◔  3m

 ─────────────────────────────────────────────────────
  Claude v2.1.169 · Claude Max                    ⇅ rc

  ▐▛███▜▌  ◎ 53  ◇ 16M ↘ 13M ↗ 2M ◌ 198M       $188.88
 ▝▜█████▛▘ 5h ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱   ↻ 1h47m
   ▘▘ ▝▝   7d ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱   ↻ 5d22h

 W: ◎ 420  ◇ 202.9M ↘ 175.1M ↗ 27.8M ◌  5.2B $3,888.88
 M: ◎ 860  ◇ 420.0M ↘ 366.0M ↗ 54.0M ◌ 10.8B $8,666.66

                      ? for help
```

> Product invariant lives in [DESIGN.md](./DESIGN.md). The short version: Rimz shows you which agent needs you and takes you straight to its pane, where you answer in the agent's own UI. Enrol a resolver — a small process you trust on this machine — when you want routine answers handled ahead of you; the chain still ends with you.

Every glyph, meter, and frame above is broken down in the [interface reference](./docs/interface/sidebar.md).

## Quickstart

```sh
cd ~/code/query-engine
rimz                                                # open or reattach the room

rimz event emit --kind build.started --title "web"  # any script can post
rimz feed ask --title "Promote staging → prod?" \
              --options yes,no --timeout 1h

rimz remote connect dev-box:query-engine           # reattach from anywhere; the link reconnects itself
rimz pane split && claude                           # start an agent in a new pane
```

That's the whole loop. Everything else is variations on those five commands.

## Why Rimz

- **Agent users.** See at a glance which of four parallel Claude or Codex sessions needs you, jump straight to its pane, and answer in the agent's own UI — no more flipping tabs to find the blocked one.
- **Remote developers.** Start the room on a host or container, walk away, reattach from a laptop, tablet, or SSH client on a phone. Pickup is zero-cost.
- **Script and tool authors.** Make a `terraform apply`, a 4-hour migration, or a CI gate a first-class citizen of the sidebar through `rimz event emit` and `rimz feed ask`. No UI to build.

## How it works

One repo maps to one Rimz workspace and one multiplexer session, and the repo's git worktrees group inside it. Every event — agent hooks, script announcements, build results, blocking questions — writes through one CLI to a durable file-backed ledger. The sidebar renders that ledger, and the room keeps its state whether or not anyone is attached.

The design commitments and the three operating paths (`native_ui`, `bridge`, `script`) live in [DESIGN.md](./DESIGN.md). The wire-level state machine, surfaces, and CAS rules live in [docs/internals/ledger.md](./docs/internals/ledger.md).

## Development

The Rust toolchain is pinned by [rust-toolchain.toml](./rust-toolchain.toml). Zellij or tmux is needed to try the room and pane flows.

```sh
make build      # cargo xtask build
make install    # install the single rimz binary
make test       # cargo xtask test
make ci         # full CI gate
```

`make` is a thin wrapper around `cargo xtask <task>`; `xtask` is the source of truth for automation. Use focused tasks for routine validation and the full CI gate when the change calls for it. `make install` writes `rimz` to `${CARGO_HOME:-$HOME/.cargo}/bin`, so that directory must be on `PATH`. `sudo make install` builds as the invoking user and installs to `/usr/local/bin`; set `PREFIX=/opt/rimz` or `DESTDIR=...` to change the root-owned destination.

After installing, smoke-test the CLI:

```sh
rimz ping
rimz doctor
rimz start --print .
```

Contributor rules, gate details, and task names live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).

## Status

Documentation-first. Implementation lands in milestones — M0 spikes the ledger and bridge across Zellij and tmux; agent adapters follow at M2 (Codex) and M3 (Claude Code). See [docs/contributing/roadmap.md](./docs/contributing/roadmap.md).

## Read next

- **Use it.** [docs/guide/product.md](./docs/guide/product.md) — five-minute tour of the sidebar, the feed, and the three audiences.
- **See it.** [docs/interface/sidebar.md](./docs/interface/sidebar.md) — the sidebar on screen, zone by zone, with the frames it draws.
- **CLI surface.** [docs/reference/cli.md](./docs/reference/cli.md) — every command, grouped by intent.
- **Understand the design.** [DESIGN.md](./DESIGN.md) — the attention problem and the design choices that answer it.
- **Contribute.** [AGENTS.md](./AGENTS.md) — engineering rules and the docs map. Contributor commands and the gate stack live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).
```

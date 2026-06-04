# Rimz

Run one coding agent and you flip tabs. Run four and you lose them.

Rimz pins every repo to one durable room — a Zellij or tmux session with a sidebar that tells you which pane needs you, and a ledger that survives detach, sidebar reload, and reattach from anywhere. Humans, scripts, CI, and coding agents share the same feed through one CLI.

```
 ⌘ query-engine            ~/code/query-engine
 ✦ 6   ✧ 2                              $4.20
 ──────────────────────────────────────────────
 ? 2   ! 1   ○ 1            ✽ 1   ⢿ 1   ✓ 0
 ◷ 41m · ◇ 486.0k · ◆ 4

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━──────────────── 41%
▏
▏✽ claude · Sonnet · high · plan
▏  add tests
▏  ▣ ━━━━━─────────────────────  18%
▏
▏⢿ codex · GPT-5.5 · high
▏  refactor api
▏  ▣ ━━━━━━━━━━━━━━━──────────── 63%

 feature-migration                     +230 -23
 ! claude · Opus · 1M
   db migrate

 ○ codex · GPT-5.5 · low
   —

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ? 1
 ? deploy
   promote?

 ──────────────────────────────────────────────
 Claude Code v2.1.158 · Claude Max          ⇅ rc
  ▐▛███▜▌  $4.20 · ◇ 486.0k
 ▝▜█████▛▘ 5h ▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱ ↻ 2h06m
   ▘▘ ▝▝   7d ▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱ ↻ 1d02h
 ──────────────────────────────────────────────
            ␣ next ?!   ? for help
```

> Product invariant lives in [DESIGN.md](./DESIGN.md). The short version: Rimz shows you which agent needs you and takes you to its pane, where you answer in the agent's own UI. Enrol a resolver when you want routine answers handled ahead of you — the chain still ends with you.

Every glyph, meter, and frame above is broken down in the [interface reference](./docs/interface/sidebar.md).

## Quickstart

```sh
cd ~/code/query-engine
rimz                                                # open or reattach the room

rimz event emit --kind build.started --title "web"  # any script can post
rimz feed ask --title "Promote staging → prod?" \
              --options yes,no --timeout 1h

ssh dev-box rimz attach query-engine                # reattach from anywhere
rimz pane split && claude                           # start an agent in a new pane
```

That's the whole loop. Everything else is variations on those five commands.

## Why Rimz

- **Agent users.** See at a glance which of four parallel Claude or Codex sessions needs you, jump straight to its pane, and answer in the agent's own UI — no more flipping tabs to find the blocked one.
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
- **See it.** [docs/interface/sidebar.md](./docs/interface/sidebar.md) — the sidebar on screen, zone by zone, with the frames it draws.
- **CLI surface.** [docs/cli.md](./docs/reference/cli.md) — every command, grouped by intent.
- **Understand the design.** [DESIGN.md](./DESIGN.md) — commitments and the three operating paths.
- **Contribute.** [AGENTS.md](./AGENTS.md) — engineering rules and the docs map. `cargo xtask ci` runs every quality gate locally; the gate stack is in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).

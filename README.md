# Rimz

Run one coding agent and you flip tabs. Run ten and you lose track of which one is blocked, which errored, and which is quietly burning your rate limit.

Rimz pins every project to one room: a Zellij or tmux session with a sidebar that tells you which agent needs you and takes you straight to its pane. Agents keep working while you are away, locally or over SSH from a laptop, tablet, or phone, and the room comes back exactly as you left it.

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

> Rimz shows you which agent needs you and takes you straight to its pane, where you answer in the agent's own UI. Enrol a resolver, a small process you trust on this machine, to handle routine answers ahead of you so agents keep moving while you are gone; the chain still ends with you. The full invariant lives in [DESIGN.md](./DESIGN.md).

Every glyph, meter, and frame above is broken down in the [interface reference](./docs/interface/sidebar.md).

## Quickstart

```sh
cd ~/code/query-engine
rimz                                       # open or reattach the room

rimz pane split && claude                  # start an agent in a new pane
rimz remote connect dev-box:query-engine   # reattach from anywhere; the link reconnects itself
```

That loop is the product; the sidebar carries everything from there.

## Why Rimz

If you run several Claude Code or Codex sessions at once, the hard part is no longer any single agent. It is knowing which of them needs you. Rimz watches every agent in the room and surfaces the one that is blocked, the one that errored, and the one burning toward a rate limit, then takes you straight to its pane in one keystroke so you answer in the agent's own UI. Triage goes from staring at five terminals to answering the questions that actually need you, when they need you.

Rimz also lets agents keep working while you are not there. Start the room on a host or container, detach, and reattach later from a laptop, a tablet, or an SSH client on a phone: the sidebar rebuilds from the ledger with every agent where you left it and every pending question still waiting. Enrol a resolver, a small local process you trust, and routine prompts get answered ahead of you in an ordered chain that always ends with you, so an overnight or unattended run does not stall the moment it hits a permission prompt.

## How it works

One repo maps to one Rimz workspace and one multiplexer session, and the repo's git worktrees group inside it. Everything an agent reports through its hooks (sessions, tool calls, completions, failures, blocking questions) writes through one CLI to a durable file-backed ledger. The sidebar renders that ledger, and the room keeps its state whether or not anyone is attached.

The design commitments and the operating paths a question can take (`native_ui`, `bridge`, `script`) live in [DESIGN.md](./DESIGN.md). The wire-level state machine, surfaces, and CAS rules live in [docs/internals/ledger.md](./docs/internals/ledger.md).

Once you are running agents in the room, the same CLI lets a script post to the same sidebar. A long migration, a deploy gate, or a CI step can call `rimz event emit` to announce itself and `rimz feed ask` to put a yes/no question on the column with answer buttons, answered by you or a resolver just like an agent's prompt. It is the same feed seen from a script instead of an agent, with no UI to build. And `rimz run "<prompt>"` lets a script launch a whole agent turn the same way: it blocks for the final message and an exit code while the agent works in a visible pane that you, a resolver, or a supervising script can inspect, nudge, stream, and stop.

## Development

The Rust toolchain is pinned by [rust-toolchain.toml](./rust-toolchain.toml). Zellij or tmux is needed to try the room and pane flows.

```sh
cargo xtask build      # build rimz and the Zellij presence plugin
cargo xtask install    # install the single rimz binary
cargo xtask test       # run the nextest suite
cargo xtask ci         # full CI gate
```

`cargo xtask <task>` is the source of truth for automation. Use focused tasks for routine validation and the full CI gate when the change calls for it. `cargo xtask install` writes `rimz` to `${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin`, so that directory must be on `PATH`.

After installing, smoke-test the CLI:

```sh
rimz ping
rimz doctor
rimz start --print .
```

Contributor rules, gate details, and task names live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).

## Status

Pre-release. The ledger, sidebar, multiplexer backends, and Claude/Codex/Pi adapters are implemented in-tree; upcoming adapter and renderer work is documented beside the owning internals pages.

## Read next

- [docs/guide/product.md](./docs/guide/product.md) is the five-minute tour of the sidebar, the fleet, and how a blocked agent reaches you.
- [docs/interface/sidebar.md](./docs/interface/sidebar.md) walks the sidebar on screen, zone by zone, with the frames it draws.
- [docs/reference/cli.md](./docs/reference/cli.md) lists every command, grouped by intent.
- [DESIGN.md](./DESIGN.md) lays out the attention problem and the design choices that answer it.
- [AGENTS.md](./AGENTS.md) holds the engineering rules and the docs map; contributor commands and the gate stack live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).
```

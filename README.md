# Rimz

**A control room for your coding agents, inside the terminal you already use.**

Run one Claude Code session and you flip tabs. Run ten and you lose track of which one is blocked, which one errored, and which one is quietly draining your 5-hour window.

Rimz gives every project one room: a Zellij or tmux session with a live sidebar of every coding agent in it. Each card shows what the agent is doing, what it costs, and how much context it has left. One key drops you into the pane that needs you, and you answer in the agent's own UI.

Agents keep working while you are away. Attach from a laptop, a tablet, or a phone over SSH, and the room comes back exactly as you left it — even after a reboot.

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

Every glyph and meter in this frame is live, and the [interface reference](./docs/interface/sidebar.md) breaks down each one.

## Quickstart

```sh
cargo install rimz   # or: brew install rimz

cd ~/code/query-engine
rimz                                          # open or reattach the room

rimz tab --layout peer --worktree feat/x      # Claude + Codex side by side in a fresh worktree
rimz remote connect dev-box:query-engine      # the same room, from any machine
```

That loop is the product; the sidebar carries everything from there.

**Your attention, routed.** The cockpit line reads the whole fleet in one glance — `? 3` waiting on you, `! 0` failed, `⏸ 0` paused, `✓ 8` done — and a row of zeros means nothing needs you. The column underneath is already triaged: the agent that has waited longest rises to the top, a result you have not read blinks until you look, and one keypress lands you in its pane, where you answer in Claude's or Codex's own prompt. When the routine questions start repeating, [enrol a resolver](./docs/guide/product.md#resolvers-scale-your-attention) to answer them ahead of you, in a chain that always ends with you.

**A beautiful realtime card for every agent**, carrying the details you care about when you run a team of them in parallel:

- working state and the task it is on, animated while the agent thinks, edits, or compacts
- model and effort level, with the context window it is running in
- context-window health: a meter that ramps toward red as it fills, plus the compaction count
- token mix, down to cache reads and writes, and the live dollar cost of the session
- the subagent tree: each child's task, status, model, tokens, and elapsed time

The provider dashboard pins your plan underneath: today's sessions, tokens, and dollars, the 5h and 7d budget bars draining in real time, and week and month totals priced from your full transcript history. One look and you know your pace.

And when a run hits the 5-hour wall, its row parks as `⏸` while the dashboard counts down the reset. Enrol the bundled [resolver example](./docs/internals/resolvers.md) and it types the resume the moment `↻` hits zero, so overnight runs pick themselves back up. You care about the task; Rimz takes care of the noise.

**One binary, zero learning curve.** Rimz wraps the tools you already run — Claude Code, Codex, and Pi inside Zellij or tmux — and the agents run stock: your keybinds, your layouts, your terminal (Ghostty, Warp, the VS Code terminal), and the official web, desktop, and mobile apps all keep working. Underneath, it reads what the agents already emit (hooks, `.jsonl` session transcripts, the Claude statusline, the Codex app-server) into a directory of flat files you can read with `cat` — no daemon, nothing to relearn. Hooks install at `rimz start`, with your consent and a diff preview. It is still your tmux.

**Close the laptop.** Start the room on a server, detach, and reattach from anywhere: `rimz remote connect dev-box:query-engine` rebuilds the sidebar from the ledger — every agent where you left it, every pending question still waiting — over a link that reconnects itself and reports its health (`⇅ 42ms 0%`). The room even survives a reboot: it comes back populated, every prior agent re-seeded idle in its own pane via `claude --resume`, `codex resume`, or `pi --session`, one prompt from where it stopped.

**Fix the 3 a.m. CI failure from your bed.** Your nightly job hits a failing migration, the question lands on the feed, your phone buzzes, you type the fix, and the run finishes while the script is still blocking. `rimz run "<prompt>"` gives scripts `claude -p` ergonomics — a blocking call, real exit codes, `--detach` and `--stream` for orchestration — over an agent in a real pane you can watch and steer the whole time. `rimz steer` types into a live agent now, `rimz queue --on done` delivers the next instruction when the turn finishes, and `rimz feed ask` puts any script's question on the sidebar with answer buttons. The full scenarios live in [the product tour](./docs/guide/product.md#put-your-pipeline-on-the-feed).

**Your beloved `--worktree`, for every agent at once.** `rimz tab --layout peer --worktree feat/x` opens Claude and Codex side by side in a fresh worktree on its own branch: one plans and implements, the other reviews, in one tab. The layout DSL composes any grid (`claude,codex+term` is a Claude column beside a stacked Codex and shell), the sidebar groups every card by the worktree it lives in with per-tree diff churn, and cleanup is supervised: Rimz removes a worktree only after proving its work landed on the base branch ([worktrees](./docs/internals/worktrees.md)).

## How it works

One repo maps to one Rimz workspace and one multiplexer session, and the repo's git worktrees group inside it. Everything an agent reports through its hooks — sessions, tool calls, completions, failures, blocking questions — writes through one CLI to a durable file-backed ledger. The sidebar renders that ledger, and the room keeps its state whether or not anyone is attached.

The design commitments and the operating paths a question can take live in [DESIGN.md](./DESIGN.md). The wire-level state machine, surfaces, and CAS rules live in [docs/internals/ledger.md](./docs/internals/ledger.md).

## Development

The Rust toolchain is pinned by [rust-toolchain.toml](./rust-toolchain.toml). Zellij or tmux runs the room.

```sh
cargo xtask build      # build rimz and the Zellij presence plugin
cargo xtask install    # install the single rimz binary
cargo xtask test       # run the nextest suite
cargo xtask ci         # full CI gate
```

`cargo xtask <task>` is the entry point for every quality gate; contributor rules and task names live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).

## Status

Claude Code, Codex, and Pi adapters, the Zellij and tmux backends, the ledger, and the sidebar are implemented in-tree. Rimz is pre-release; upcoming adapter and renderer work is documented beside the owning internals pages.

## Read next

- [docs/guide/product.md](./docs/guide/product.md) tours the room and the four ways people run it: a local fleet, a server room, two agents on one feature, and a pipeline on the feed.
- [docs/guide/experience.md](./docs/guide/experience.md) walks the first run to a ten-agent fleet, moment by moment.
- [docs/interface/sidebar.md](./docs/interface/sidebar.md) walks the sidebar on screen, zone by zone, with the frames it draws.
- [docs/reference/cli.md](./docs/reference/cli.md) maps every command to grouped references and examples.
- [DESIGN.md](./DESIGN.md) lays out the attention problem and the design choices that answer it.

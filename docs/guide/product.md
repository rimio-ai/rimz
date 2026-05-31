# Product

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz gives every project one room — a Zellij or tmux session with a sidebar — where humans, scripts, CI, and coding agents share one feed. This doc is the five-minute tour: what the sidebar looks like, who it's for, how the three operating paths feel in practice, and the commands you'd actually run today. For the full phase-by-phase walk-through — first keystroke to a ten-agent fleet — see [experience.md](./experience.md).

## The sidebar

```
 ⌘ query-engine            ~/code/query-engine
 ✦ 5   ✧ 2                              $4.20
 ──────────────────────────────────────────────
 ? 2   ! 1   ○ 0            ✽ 1   ⢿ 1   ✓ 0
 ◷ 41m · ◇ 486k · ◆ 4

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━──────────────── 41%
▏
▏✽ claude · Sonnet · high
▏  add tests
▏  ▣ ━━━━━─────────────────────  18%
▏
▏⢿ codex · GPT-5.5 · high
▏  refactor api
▏  ▣ ━━━━━━━━━━━━━━━──────────── 63%

 feature-migration                     +230 -23
 ! claude · Opus · yolo
   db migrate

 ○ codex · GPT-5.5 · low
   —

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ? 1
 ? deploy
   promote?

 ──────────────────────────────────────────────
 Claude Code v2.1.158 · Claude Max          ⇅ rc
  ▐▛███▜▌  $4.20 · ◇ 486k
 ▝▜█████▛▘ 5h ▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱ ↻ 2h06m
   ▘▘ ▝▝   7d ▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱ ↻ 1d02h
 ──────────────────────────────────────────────
 ↵ jump   ␣ next ?!   ? keys
```

> Product invariant lives in [DESIGN.md](../../DESIGN.md).

The sidebar is a worktree-keyed presence and attention map: every pane is a row — a bare shell shows as `· zsh`, and becomes the agent's row the moment you run one — and each agent shows its status (a colored glyph), the task it's on, and its context meter, grouped by the worktree it lives in. Each worktree is a bold header, and the worktree you have selected reads as one bracketed lane — a thin accent spine down its whole height with a faint dotted seal capping its header — while the selected card inside it lights up with a bolder spine. Other worktrees stay quiet, so the lane is the only marker on screen and the selection is unmistakable. Account-scoped usage budgets (5-hour / 7-day) leave the rows for the pinned per-provider dashboard at the bottom — one block per provider with its plan, spend, and draining "mana" bars. When an agent exits, its row reverts to the shell, so the column always mirrors what's actually running. It **routes you to the pane that needs you** — select a row to jump there — and you read the actual prompt and answer in the agent's own UI. That safety net never breaks. Resolvers can slot ahead of you when you want help; the chain ends with you.

## Three audiences, one room

- **Agent users.** You have four Claude Code or Codex sessions in flight across two worktrees. One hits a permission prompt. The sidebar surfaces it, you focus the pane, read what it actually wants to run, and approve in Claude's own prompt. Triage goes from "stare at five terminals" to "answer the questions that need me, when they need me."
- **Remote developers.** Your dev box is on a server. You start agents, close the laptop, reopen from a tablet on the train. `ssh dev-box rimz attach query-engine` and the sidebar reconstructs from the ledger — every agent where you left it, every pending question still waiting.
- **Script and tool authors.** Your deploy pipeline runs 40 minutes unattended, then pauses at the staging-to-prod gate. The script calls `rimz feed ask --title "Promote build 2026.05.18-rc.4?"` and a teammate sees it from their phone over Tailscale alongside everything the agents are doing.

## How a question reaches you

Every actionable item travels one of three paths. The schema names (`native_ui`, `bridge`, `script`) and the wire-level details live in [ledger.md](../internals/ledger.md); the human story is:

1. **Default — the agent asks in its own UI.** No resolver enrolled. Rimz writes the feed item, wakes the sidebar, and gets out of the way. You see "claude · waiting · permission" in the sidebar; you focus that pane and answer Claude's prompt. The sidebar clears when the agent moves on.
2. **Bridge — a resolver answers first, you're the fallback.** You've enrolled a resolver (e.g. an Opus-backed policy script) on this machine. When an agent hook fires, Rimz holds the hook open and asks the resolver. If the resolver answers, the decision flows back to the agent and you don't see it. If it abstains or runs out its budget, the chain advances — Slack, on-call, you — until someone answers. The agent's native UI is the last fallback if the whole chain misses the agent's hook cap.
3. **Script — your script chose Rimz as its decision surface.** A deploy script calls `rimz feed ask --title "Promote?"` and blocks. The question lands in the sidebar with answer buttons (because the script declared its options). Anyone with shell access can answer through the CLI; resolvers can answer too. No agent is involved.

## Five-minute tour

```sh
# 1. Start (or attach to) the workspace for this project
cd ~/code/query-engine
rimz

# 2. Emit a workspace event from any pane — no agent required
rimz event emit --kind build.started --title "Building web"

# 3. Ask the human a yes/no question from a script
rimz feed ask --title "Deploy staging?" \
              --options yes,no --timeout 1h

# 4. From another terminal, resolve without the sidebar
rimz feed list
rimz feed resolve <request-id> --decision '{"choice":"yes"}'

# 5. Detach. Everything keeps running.
#    Zellij: Ctrl-O then d. tmux: prefix then d.

# 6. Come back later from anywhere
ssh dev-box rimz attach query-engine

# 7. Launch a coding agent in a new pane
rimz pane split
claude   # or: codex
```

That's the loop. Every other feature in Rimz is a variation on those primitives.

## Many agents, many worktrees

Rimz groups panes by worktree, so a fleet spread across `../query-engine` (main), `../query-engine-feature-migration`, and `../query-engine-feature-frontend` renders as three groups inside one room. Agents in the same worktree share file space; sibling worktrees keep their own. The sidebar shows you which worktree each agent is in. Two write-capable agents in the *same* worktree trigger a one-time advisory; running them in *sibling* worktrees is the recommended pattern.

## Survive overnight, survive reboot

The workspace and its ledger outlive the terminal session. Two things a long-running agent run depends on:

**Reboot.** The ledger survives a host restart because it's a directory of flat files under `~/.local/state/rimz/`. Running processes do not. To make an overnight agent run survive a reboot, wire a host supervisor — a systemd unit, tmux-resurrect, or Zellij's resurrect mode. Rimz does not own this; the host does. Detail in [DESIGN.md](../../DESIGN.md) under "Non-goals."

**Overnight without a human.** By default, an agent that hits a permission prompt with nobody attached blocks on its own native UI until you return or its own timeout fires. Two ways to scale past your sleep schedule: enrol a resolver chain so routine answers happen without you (next section), or run the agent with its own bypass flag (`claude --dangerously-skip-permissions`, `codex --ask-for-approval never`) — that's the unattended-CI pattern covered in "Unattended runs" below.

## Resolver chains — the morning after

You've been answering routine permissions all day. "Can I run `cargo check`?" for the eighth time, plus six more agents waking up tomorrow morning. Write a small process that wraps a smarter model (Opus, GPT-class), enrol it once, and it handles the routine ones.

```sh
rimz resolver add opus-policy   --order 10 --budget 30s --binary ~/bin/opus-resolver
rimz resolver add slack-on-call --order 20 --budget 5m
rimz resolver add pagerduty     --order 30 --budget 30m
```

The chain:

```
[ Opus resolver ]  →  [ Slack on-call ]  →  [ PagerDuty ]  →  [ you ]
      30s                    5m                  30m            always
```

Each link has its own budget. When the budget elapses (or the resolver explicitly abstains), the chain advances. Whoever answers first wins. The chain ends with you — a resolver only ever slots ahead of you, never instead of you.

The morning after six overnight agents: "Opus answered 47 routine permissions, abstained on 3 architecture-shaped questions, Slack pinged my co-lead who answered two of them in his timezone, and one fell through to me in the sidebar." Your team's attention bandwidth scales with the chain, not with the agent count.

Chain mechanics and the heartbeat protocol live in [resolvers.md](../internals/resolvers.md). Trust gates and the allowlist are in [security.md](./security.md).

## Unattended runs in CI / sandbox

Inside a sandboxed CI runner there's no human to ask. Two patterns work:

**Agent-native bypass.** Launch each agent with its own bypass flag — `claude --dangerously-skip-permissions`, `codex --ask-for-approval never --sandbox danger-full-access`. The agent never blocks. Rimz still observes everything the agent reports through lifecycle hooks (sessions, completions, failures); the sidebar's permission posture renders `yolo` so the audit record is clear. The tradeoff: the agent skips permission events at the source, so the ledger records only what other hooks report — not a complete per-decision audit trail.

**Permissive resolver.** Enrol a resolver that answers `allow` to anything (or anything matching a policy). Every permission request still flows through Rimz, gets a decision attributed to that resolver, and lands in the ledger as a real audit record. Prefer this when you need full audit fidelity.

The two patterns compose: a permissive resolver for routine cases with the agent's bypass flag as the ultimate fallback for anything the resolver doesn't catch in time. `rimz doctor` reports the permission posture it sees per agent so the audit story stays unambiguous.

## Product principles (recap)

- **Observability is the default.** The agent's own prompt stays yours to answer in the agent's own tab.
- **The ledger owns durability, and works headless.** Every feed item survives detach, sidebar reload, and no-client mode.
- **Generic primitives, agent adapters on top.** Anything an agent integration can do, a shell script can do through the same CLI.
- **The agent's native UI is the safety net.** Whatever else gets wired into Rimz later, the agent's own prompt always works as the answer surface of last resort.
- **State comes from explicit events.** Transcripts and pane content enrich display only — they never drive blocking decisions, permissions, or correctness-critical state.

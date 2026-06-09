# Product

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz gives every project one room: a Zellij or tmux session with a sidebar where humans, scripts, CI, and coding agents share one feed. This is the five-minute tour: what the sidebar shows, who it serves, how a blocked agent reaches you, and the commands you'd run today. For the felt walk-through from first keystroke to a ten-agent fleet, see [experience.md](./experience.md).

## The sidebar

```
 ⌘ query-engine            ~/code/query-engine

 ◎ 12                  ◇ 88k ↘ 24k ↗ 64k ◌ 68k
 ¤ 6                                    $4.20
 ──────────────────────────────────────────────
 ? 2   ! 1   ⏸ 0   ✓ 0            ⢿ 2   ○ 1

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━──────────────── 41%
▏
▏✻ claude · Sonnet · high · plan
▏  add tests
▏  ▣ ━━━━━─────────────────────  18%
▏
▏⢿ codex · GPT 5.5 · high
▏  refactor api
▏  ▣ ━━━━━━━━━━━━━━━──────────── 63%

 feature-migration                     +230 -23
 ! claude · Opus · 1m
   db migrate

 ○ codex · GPT 5.5 · low
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

> The product invariant lives in [DESIGN.md](../../DESIGN.md); every glyph and meter in this frame is broken down, zone by zone, in the [interface reference](../interface/sidebar.md).

The sidebar is a worktree-keyed presence and attention map. Every pane is a row: a bare shell reads as `○ zsh` and becomes the agent's row the moment you launch one, carrying its status, the task it's on, and its context meter, grouped by the worktree it lives in. Ranking does the triage for you: the most overdue work rises to the top, calm work settles below, and a one-line cockpit summarizes the whole fleet (`? 2  ! 1 …`). Account-scoped usage budgets lift off the rows into the provider dashboard pinned at the bottom, one tab per provider showing its plan, spend, and draining budget bars. Select a row and you land in that pane, where you read the prompt and answer in the agent's own UI. The column always mirrors what's running: when an agent exits, its row reverts to the shell.

## Three audiences, one room

- **Agent users.** You have four Claude Code or Codex sessions in flight across two worktrees. One hits a permission prompt. The sidebar surfaces it, you focus the pane, read what it wants to run, and approve in Claude's own prompt. Triage goes from "stare at five terminals" to "answer the questions that need me, when they need me."
- **Remote developers.** Your dev box is on a server. You start agents, close the laptop, and reopen from a tablet on the train. `ssh dev-box rimz attach query-engine` reconstructs the sidebar from the ledger: every agent where you left it, every pending question still waiting.
- **Script and tool authors.** Your deploy pipeline runs 40 minutes unattended, then pauses at the staging-to-prod gate. The script calls `rimz feed ask --title "Promote build 2026.05.18-rc.4?"` and a teammate answers it from their phone over Tailscale, alongside everything the agents are doing.

## How a question reaches you

Every actionable item travels one of three paths. The schema names (`native_ui`, `bridge`, `script`) and the wire-level details live in [ledger.md](../internals/ledger.md); the human story is:

1. **Default: the agent asks in its own UI.** The everyday path, with nothing extra enrolled. Rimz writes the feed item, wakes the sidebar, and points you at the pane. You see "claude · waiting · permission", focus that pane, and answer Claude's prompt; the sidebar clears when the agent moves on.
2. **Bridge: a resolver answers ahead of you.** Enrol a resolver on this machine and Rimz holds the agent's hook open long enough for the resolver to take the routine items first; anything it passes on falls through to you, with the agent's own UI as the final fallback. This is the opt-in upgrade; see [resolver chains](#resolver-chains-the-morning-after) below.
3. **Script: your script chose Rimz as its decision surface.** A deploy script calls `rimz feed ask --title "Promote?"` and blocks. Because it declared its options, the question lands in the sidebar with answer buttons, and anyone with shell access (or a resolver) answers through the CLI. The script owns the question end to end.

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

Rimz groups panes by worktree, so a fleet spread across `../query-engine` (main), `../query-engine-feature-migration`, and `../query-engine-feature-frontend` renders as three groups inside one room. Agents in the same worktree share file space; sibling worktrees keep their own, and the sidebar shows you which worktree each agent is in. Running two write-capable agents in sibling worktrees is the recommended pattern; two in the *same* worktree trigger a one-time advisory.

## Many repos, one room

A room doesn't need a repo. Run `rimz start` in any directory (`~/code` holding a dozen clones, or a headless server with agents and no source control at all) and that directory is the room ([directory workspace](../reference/cli.md#start-and-attach-a-workspace)). Each child repo renders as its own pod with its own branch label and per-repo churn, exactly like a worktree pod; panes at the root sit under a name-only header; a scratch directory with no repos is one flat group. One sidebar triages the whole machine: an agent blocking in `~/code/query-engine` surfaces in the `~/code` room's feed, and the jump takes you to its pane.

## Survive overnight, survive reboot

The workspace and its ledger outlive the terminal session. Two things a long-running agent run depends on:

**Reboot.** The ledger survives a host restart because it's a directory of flat files under `~/.local/state/rimz/`. Running processes don't. To carry an overnight agent run across a reboot, the host keeps them alive with a systemd unit, tmux-resurrect, or Zellij's resurrect mode. Detail in [DESIGN.md](../../DESIGN.md) under "Non-goals."

**Overnight without a human.** An agent that hits a permission prompt with nobody attached waits on its own native UI until you return or its timeout fires. Two ways to scale past your sleep schedule: enrol a resolver chain so routine answers happen without you (next section), or run the agent with its own bypass flag (`claude --dangerously-skip-permissions`, `codex --ask-for-approval never`). The unattended-CI pattern is covered in "Unattended runs" below.

## Resolver chains: the morning after

You've answered routine permissions all day. "Can I run `cargo check`?" for the eighth time, with six more agents waking up tomorrow morning. Write a small process that wraps a smarter model (Opus, GPT-class), enrol it once, and it handles the routine ones.

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

Each link has its own budget. When the budget elapses (or the resolver abstains), the chain advances; whoever answers first wins. A resolver only ever slots ahead of you, so the chain always ends with you.

The morning after six overnight agents: "Opus answered 47 routine permissions, abstained on 3 architecture-shaped questions, Slack pinged my co-lead who answered two in his timezone, and one fell through to me in the sidebar." Your team's attention bandwidth scales with the chain, not with the agent count.

Two reference resolvers ship with Rimz, ready to enrol and adapt:

- **hook-bridge** (`hook_bridge_resolver.py`): answers routine permission requests against a policy you set, read-only tools out of the box. It is the audited form of yolo mode: every approval flows through the bridge and lands in the ledger as a real decision, so you keep a per-decision record while the prompts stop interrupting you.
- **pane-send** (`pane_send_resolver.py`): captures well-known terminal prompts in the agent's own pane, matches a bounded pattern list, types the reply, and confirms. The same skeleton adapts into a rate-limit resumer that nudges a stalled run the moment the `↻` countdown on the provider dashboard resets, so long runs pick themselves back up overnight.

Both are starting points you copy and edit. The chain mechanics, the heartbeat protocol, and the two examples live in [resolvers.md](../internals/resolvers.md); trust gates and the allowlist are in [security.md](./security.md).

## Unattended runs in CI / sandbox

Inside a sandboxed CI runner there's no human to ask. Two patterns work:

**Agent-native bypass.** Launch each agent with its own bypass flag: `claude --dangerously-skip-permissions`, `codex --ask-for-approval never --sandbox danger-full-access`. The agent runs straight through. Rimz still observes everything it reports through lifecycle hooks (sessions, completions, failures). The tradeoff: the agent skips permission events at the source, so the ledger records what other hooks report rather than a complete per-decision audit trail.

**Permissive resolver.** Enrol a resolver that answers `allow` to anything (or anything matching a policy); the bundled `hook_bridge_resolver.py` example is exactly this. Every permission request still flows through Rimz, gets a decision attributed to that resolver, and lands in the ledger as a real audit record. Prefer this when you need full audit fidelity.

The two patterns compose: a permissive resolver for routine cases, with the agent's bypass flag as the ultimate fallback for anything the resolver doesn't catch in time.

## The design behind it

The principles these flows rest on (observe by default, ledger-owned durability, one set of primitives shared by agents and scripts, and transcripts that enrich display without driving decisions) are spelled out in [DESIGN.md](../../DESIGN.md).

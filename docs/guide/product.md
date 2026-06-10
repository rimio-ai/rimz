# Product

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz gives every project one room: a Zellij or tmux session with a sidebar where you watch and steer every coding agent. This is the five-minute tour: what the sidebar shows, who it serves, how a blocked agent reaches you, and the commands you'd run today. For the felt walk-through from first keystroke to a ten-agent fleet, see [experience.md](./experience.md).

## The sidebar

> The product invariant lives in [DESIGN.md](../../DESIGN.md); every glyph and meter in this frame is broken down, zone by zone, in the [interface reference](../interface/sidebar.md).

The sidebar is a worktree-keyed presence and attention map. Every pane is a row: a bare shell reads as `○ zsh` and becomes the agent's row the moment you launch one, carrying its status, the task it's on, and its context meter, grouped by the worktree it lives in. Ranking does the triage for you: the most overdue work rises to the top, calm work settles below, and a one-line cockpit summarizes the whole fleet (`? 2  ! 1 …`). Account-scoped usage budgets lift off the rows into the provider dashboard pinned at the bottom, one tab per provider showing its plan, spend, and draining budget bars. Select a row and you land in that pane, where you read the prompt and answer in the agent's own UI. The column always mirrors what's running: when an agent exits, its row reverts to the shell.

## Who it serves

Rimz is built for developers running many coding agents in parallel, both locally and remotely.

Running a fleet locally, you might have four Claude Code or Codex sessions in flight across two worktrees when one hits a permission prompt. The sidebar surfaces it, you focus the pane, read what the agent wants to run, and approve in Claude's own prompt. Triage goes from "stare at five terminals" to "answer the questions that need me, when they need me."

Running agents remotely, your dev box lives on a server. You start agents, close the laptop, and reopen from a tablet on the train. `ssh dev-box rimz attach query-engine` reconstructs the sidebar from the ledger: every agent where you left it, every pending question still waiting. With a resolver chain enrolled, the routine prompts get answered while you are gone, so the fleet keeps moving and only the questions that genuinely need you are still waiting when you reattach.

## How a question reaches you

When an agent needs a decision, Rimz gets you to it. The two everyday paths (the schema names `native_ui` and `bridge`, with wire-level details in [ledger.md](../internals/ledger.md)) are:

1. The agent asks in its own UI. The everyday path, with nothing extra enrolled. Rimz writes the feed item, wakes the sidebar, and points you at the pane. You see "claude · waiting · permission", focus that pane, and answer Claude's prompt; the sidebar clears when the agent moves on.
2. A resolver answers ahead of you. Enrol a resolver on this machine and Rimz holds the agent's hook open long enough for the resolver to take the routine items first; anything it passes on falls through to you, with the agent's own UI as the final fallback. This is the opt-in upgrade that keeps a fleet moving while you step away; see [resolver chains](#resolver-chains-the-morning-after) below.

A third path exists for scripts: a process can call `rimz feed ask` and route its own decision into the same sidebar (the `script` surface), covered under [scripts on the same feed](#scripts-on-the-same-feed).

## Five-minute tour

```sh
# 1. Start (or attach to) the room for this project
cd ~/code/query-engine
rimz

# 2. Launch a coding agent in a new pane
rimz pane split
claude   # or: codex

# 3. Work. When an agent needs you, the sidebar surfaces it;
#    press ␣ to jump straight to the pane that is waiting.

# 4. Detach. Everything keeps running.
#    Zellij: Ctrl-O then d. tmux: prefix then d.

# 5. Come back later from anywhere
ssh dev-box rimz attach query-engine
```

That's the loop. Every other feature in Rimz is a variation on those primitives.

## Many agents, many worktrees

Rimz groups panes by worktree, so a fleet spread across `../query-engine` (main), `../query-engine-feature-migration`, and `../query-engine-feature-frontend` renders as three groups inside one room. Agents in the same worktree share file space; sibling worktrees keep their own, and the sidebar shows you which worktree each agent is in. Running two write-capable agents in sibling worktrees is the recommended pattern; two in the same worktree trigger a one-time advisory.

## Many repos, one room

A room doesn't need a repo. Run `rimz start` in any directory (`~/code` holding a dozen clones, or a headless server with agents and no source control at all) and that directory is the room ([directory workspace](../reference/cli.md#start-and-attach-a-workspace)). Each child repo renders as its own pod with its own branch label and per-repo churn, exactly like a worktree pod; panes at the root sit under a name-only header; a scratch directory with no repos is one flat group. One sidebar triages the whole machine: an agent blocking in `~/code/query-engine` surfaces in the `~/code` room's feed, and the jump takes you to its pane.

## Survive overnight, survive reboot

The workspace and its ledger outlive the terminal session. A long-running agent run depends on two things.

The ledger survives a host restart because it is a directory of flat files under `~/.local/state/rimz/`. Running processes do not. To carry an overnight agent run across a reboot, the host keeps them alive with a systemd unit, tmux-resurrect, or Zellij's resurrect mode (detail in [DESIGN.md](../../DESIGN.md) under "Non-goals").

To run overnight without a human, plan for the prompts. An agent that hits a permission prompt with nobody attached waits on its own native UI until you return or its timeout fires. Two ways scale past your sleep schedule: enrol a resolver chain so routine answers happen without you (next section), or run the agent with its own bypass flag (`claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`). The unattended pattern is covered in [Unattended runs](#unattended-runs) below.

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

Two reference resolvers ship with Rimz, ready to enrol and adapt.

The `hook_bridge_resolver.py` example answers routine permission requests against a policy you set, with read-only tools allowed out of the box. It is the audited form of yolo mode: every approval flows through the bridge and lands in the ledger as a real decision, so you keep a per-decision record while the prompts stop interrupting you.

The `pane_send_resolver.py` example captures well-known terminal prompts in the agent's own pane, matches a bounded pattern list, types the reply, and confirms. The same skeleton adapts into a rate-limit resumer that nudges a stalled run the moment the `↻` countdown on the provider dashboard resets, so long runs pick themselves back up overnight.

Both are starting points you copy and edit. The chain mechanics, the heartbeat protocol, and the two examples live in [resolvers.md](../internals/resolvers.md); trust gates and the allowlist are in [security.md](./security.md).

## Unattended runs

`rimz run "<prompt>"` launches one supervised agent turn in the room: it opens a real agent pane, waits for the turn to finish, prints the final assistant message, and exits `0` on success, `1` on failure, `124` on timeout, `130` on cancel — script ergonomics over an agent you can still see and steer.

```sh
# cron, 02:00 — refresh the deps and open a PR
rimz run --worktree deps --timeout 4h "update dependencies, run the test suite, open a PR"
```

Because the agent runs in a real pane, a run that stops to ask survives the stop: the question takes the normal path — a resolver answers the routine ones, anything left pops to the cockpit — and you attach from anywhere, answer in the agent's own UI, and the run picks up and finishes while the script is still blocking. A run launched from cron while you work joins the room as one more row, triaged and jumpable like the agents you started by hand.

How a run answers permissions on its own is a posture you choose, and two patterns work.

The first is the agent's own bypass flag. Launch each agent with `claude --dangerously-skip-permissions` or `codex --dangerously-bypass-approvals-and-sandbox` and it runs straight through (`rimz run --yolo` passes the adapter's flag for you; `--ask` leaves the provider's prompts in place). Rimz still observes everything it reports through lifecycle hooks (sessions, completions, failures). The tradeoff is that the agent skips permission events at the source, so the ledger records what other hooks report rather than a complete per-decision audit trail.

The second is a permissive resolver. Enrol a resolver that answers `allow` to anything (or anything matching a policy); the bundled `hook_bridge_resolver.py` example is exactly this. Every permission request still flows through Rimz, gets a decision attributed to that resolver, and lands in the ledger as a real audit record. Prefer this when you need full audit fidelity.

The two patterns compose: a permissive resolver for routine cases, with the agent's bypass flag as the fallback for anything the resolver does not catch in time.

For orchestration, `--detach` prints the run id and returns immediately; `rimz run status <id>` reports the durable result with live phase when the run is active; `rimz run stream <id>` or `rimz run --stream` streams the turn as it happens; `rimz run send <id> --enter -- "continue"` is the first-class nudge for wrapper scripts; and `rimz pane send` / `rimz pane capture` remain the universal pane fallback. Flags and selection rules live in [the CLI reference](../reference/cli.md#run-agents-in-tabs-and-worktrees); the run record and completion mechanics live in [run.md](../internals/run.md).

## Scripts on the same feed

Once the room is running agents, the same CLI lets a script join the feed. A deploy pipeline that runs unattended and then pauses at a staging-to-prod gate can call `rimz feed ask --title "Promote build 2026.05.18-rc.4?"`, and the question lands in the sidebar with answer buttons alongside everything the agents are doing; you or a resolver answers it from anywhere. The script owns the question end to end. It is the same primitives an agent integration uses, reached from a shell script instead, so anything an agent can surface, a script can too.

## The design behind it

The principles these flows rest on (observe by default, ledger-owned durability, one set of primitives shared by agents and scripts, and transcripts that enrich display without driving decisions) are spelled out in [DESIGN.md](../../DESIGN.md).

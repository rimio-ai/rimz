# Product

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz is built for developers running ten or twenty Claude Code, Codex, or Pi sessions in parallel — on a laptop, or on a server you reach over SSH. This page is the working tour: the room and the sidebar, the everyday loop, and the four ways people run it — triaging a local fleet, running a room on a server, pairing two agents on one feature, and putting a pipeline on the feed. For the felt walk-through from first keystroke to a ten-agent fleet, see [experience.md](./experience.md).

## The room and the sidebar

> Every glyph and meter in the sidebar is broken down, zone by zone, in the [interface reference](../interface/sidebar.md).

Rimz gives every project one room: a Zellij or tmux session with a sidebar that is a worktree-keyed presence and attention map. Every pane is a row: a bare shell reads as `○ zsh` and becomes the agent's card the moment you launch one, carrying its status, the task it's on, its model and effort, its context meter, and its running cost, grouped by the worktree it lives in. Ranking does the triage for you: the most overdue work rises to the top, calm work settles below, and a one-line cockpit summarizes the whole fleet (`? 2  ! 1 …`). Account-scoped usage budgets lift off the rows into the provider dashboard pinned at the bottom, one tab per provider showing its plan, spend, and draining 5h/7d budget bars. Select a row and you land in that pane, where you read the prompt and answer in the agent's own UI. The column always mirrors what's running: when an agent exits, its row reverts to the shell.

## The loop

```sh
# 1. Start (or attach to) the room for this project
cd ~/code/query-engine
rimz

# 2. Launch a coding agent and start using
claude   # or: codex

# 3. Work. When an agent needs you, the sidebar surfaces it;
#    press ␣ to jump straight to the pane that is waiting.

# 4. Detach. Everything keeps running.
#    Zellij: Ctrl-O then d. tmux: prefix then d.

# 5. Come back later, from any machine
rimz remote connect dev-box:query-engine
```

That's the loop. Every other feature in Rimz is a variation on those primitives.

## How a question reaches you

When an agent needs a decision, Rimz gets you to it. The two everyday paths (the schema names `native_ui` and `bridge`, with wire-level details in [ledger.md](../internals/sidebar/ledger.md)) are:

1. The agent asks in its own UI. The everyday path, with nothing extra enrolled. Rimz writes the feed item, wakes the sidebar, and points you at the pane. You see "claude · waiting · permission", focus that pane, and answer Claude's prompt; the sidebar clears when the agent moves on.
2. A resolver answers ahead of you. Enrol a resolver on this machine and Rimz holds the agent's hook open long enough for the resolver to take the routine items first; anything it passes on falls through to you, with the agent's own UI as the final fallback. This is the opt-in upgrade that keeps a fleet moving while you step away; see [resolvers](#resolvers-scale-your-attention) below.

A third path exists for scripts: a process can call `rimz feed ask` and route its own decision into the same sidebar (the `script` surface), covered under [the pipeline scenario](#put-your-pipeline-on-the-feed).

## Triage a local fleet

Running a fleet locally, you might have four Claude Code or Codex sessions in flight across two worktrees when one hits a permission prompt. The sidebar surfaces it, you focus the pane, read what the agent wants to run, and approve in Claude's own prompt. Triage goes from "stare at five terminals" to "answer the questions that need me, when they need me."

Between questions, you steer. `rimz steer claude -- "focus on the failing parser test"` types into the agent's pane immediately, and holds off when a pending ask owns the next keystroke. `rimz queue codex --on done -- "open a PR summary"` parks the next instruction durably and delivers it the moment the agent finishes its turn, so you hand off follow-up work without watching for the turn boundary. Targets are agent kinds, session-id prefixes, or pane ids, with `--worktree` to narrow; the grammar and delivery gates live in [the agent-control reference](../reference/cli/agents.md#steer-a-live-agent).

## Run it on a server

Your dev box lives on a server. You start agents, close the laptop, and reopen from a tablet on the train: `rimz remote connect dev-box:query-engine` reconstructs the sidebar from the ledger, with every agent where you left it and every pending question still waiting. Saved aliases carry the target and reconnect defaults (`rimz remote add dev dev-box:~/code/query-engine`, then `rimz remote connect dev`), the link supervises itself with automatic reconnects, and a `⇅ 42ms 0%` badge in the sidebar footer reads link health at a glance ([remote internals](../internals/reach/remote.md)).

The room outlives the host too. The ledger is a directory of flat files under `~/.local/state/rimz/`, and when the session must be reborn after a reboot or a multiplexer crash, Rimz re-seeds every prior agent idle in its own pane (`claude --resume`, `codex resume`, `pi --session`), so the fleet is one prompt away from where it stopped. `--no-resume` starts a clean room instead.

Long runs meet provider limits, and the room keeps them legible: an agent that stops on the 5-hour wall parks as `⏸` while the provider dashboard counts down the window reset (`↻ 1h47m`). Enrol the bundled pane-send resolver as a rate-limit resumer and it nudges the run the moment the countdown clears, so overnight work picks itself back up. Resolvers are the general answer to running past your attention, next.

## Resolvers: scale your attention

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

Both are starting points you copy and edit. The chain mechanics, the heartbeat protocol, and the two examples live in [resolvers.md](../internals/agents/resolvers.md); trust gates and the allowlist are in [security.md](./security.md).

## Two agents, one feature

`rimz tab --layout peer --worktree feat/great` opens Claude and Codex side by side in a fresh worktree on its own branch: one plans and implements while the other reviews, both in the same files, isolated from your checkout. `peer` is a built-in layout, and inline specs compose any grid — `rimz tab --layout 'claude,codex+term'` opens a Claude column beside a stacked Codex and shell — while named layouts in `[agents.layouts]` carry per-agent launch flags. `rimz agents claude claude --worktree --prompt "take separate approaches and report back"` fans one prompt out to parallel attempts, each in its own fresh worktree ([agent-control reference](../reference/cli/agents.md#open-agent-tabs)).

Rimz groups panes by worktree, so a fleet spread across `../query-engine` (main), `../query-engine-feature-migration`, and `../query-engine-feature-frontend` renders as three groups inside one room. Agents in the same worktree share file space; sibling worktrees keep their own, and the sidebar shows you which worktree each agent is in. Running two write-capable agents in sibling worktrees is the recommended pattern; two in the same worktree trigger a one-time advisory.

Cleanup is supervised. When a worktree's agent exits, Rimz inspects the tree: work proven landed on the base branch is swept away with its branch, and anything dirty or unmerged is kept, with a keep/remove/shell prompt when you're watching. `rimz gc` sweeps the leftovers later, under the same proof ([worktrees.md](../internals/agents/worktrees.md)).

## Many repos, one room

A room doesn't need a repo. Run `rimz start` in any directory (`~/code` holding a dozen clones, or a headless server with agents and no source control at all) and that directory is the room ([directory workspace](../reference/cli.md#start-and-attach-a-workspace)). Each child repo renders as its own pod with its own branch label and per-repo churn, exactly like a worktree pod; panes at the root sit under a name-only header; a scratch directory with no repos is one flat group. One sidebar triages the whole machine: an agent blocking in `~/code/query-engine` surfaces in the `~/code` room's feed, and the jump takes you to its pane.

## Put your pipeline on the feed

`rimz run "<prompt>"` launches one supervised agent turn in the room: it opens a real agent pane, waits for the turn to finish, prints the final assistant message, and exits `0` on success, `1` on failure, `124` on timeout, `130` on cancel — script ergonomics over an agent you can still see and steer.

```sh
# cron, 02:00 — refresh the deps and open a PR
rimz run --worktree deps --timeout 4h "update dependencies, run the test suite, open a PR"
```

Because the agent runs in a real pane, a run that stops to ask survives the stop: the question takes the normal path — a resolver answers the routine ones, anything left pops to the cockpit and your notification channel — and you attach from anywhere, answer in the agent's own UI, and the run picks up and finishes while the script is still blocking. A failing migration at 3 a.m. becomes a push on your phone, a one-line fix typed over SSH, and a green pipeline by morning. The same shape runs while you watch: a PR-review job launched from CI joins your room as one more row, where you inspect the work as it happens and it asks its design questions right in your workspace.

For orchestration, `--detach` prints the run id and returns immediately; `rimz run status <id>` reports the durable result with live phase while the run is active; `rimz run stream <id>` or `rimz run --stream` streams the turn as it happens; `rimz run send <id> --enter -- "continue"` is the first-class nudge for wrapper scripts; and `rimz pane send` / `rimz pane capture` remain the universal pane fallback. Flags and selection rules live in [the agent-control reference](../reference/cli/agents.md#run-one-supervised-agent-turn); the run record and completion mechanics live in [run.md](../internals/agents/run.md).

Scripts join the feed directly too. A deploy pipeline that pauses at a staging-to-prod gate calls `rimz feed ask --title "Promote build 2026.05.18-rc.4?"`, and the question lands in the sidebar with answer buttons alongside everything the agents are doing; you or a resolver answers it from anywhere, and the script owns the question end to end. `rimz event emit` announces milestones to the same column. It is the same primitives an agent integration uses, reached from a shell script, so anything an agent can surface, a script can too.

### Choosing the permission posture

How an unattended run answers permissions on its own is a posture you choose, and two patterns work.

The first is the agent's own bypass flag. Launch each agent with `claude --dangerously-skip-permissions` or `codex --dangerously-bypass-approvals-and-sandbox` and it runs straight through (`rimz run --yolo` passes the adapter's flag for you; `--ask` leaves the provider's prompts in place). Rimz still observes everything it reports through lifecycle hooks (sessions, completions, failures). The tradeoff is that the agent skips permission events at the source, so the ledger records what other hooks report rather than a complete per-decision audit trail.

The second is a permissive resolver. Enrol a resolver that answers `allow` to anything (or anything matching a policy); the bundled `hook_bridge_resolver.py` example is exactly this. Every permission request still flows through Rimz, gets a decision attributed to that resolver, and lands in the ledger as a real audit record. Prefer this when you need full audit fidelity.

The two patterns compose: a permissive resolver for routine cases, with the agent's bypass flag as the fallback for anything the resolver does not catch in time.

## The design behind it

The principles these flows rest on (observe by default, ledger-owned durability, one set of primitives shared by agents and scripts, and transcripts that enrich display without driving decisions) are spelled out in [DESIGN.md](../../DESIGN.md).

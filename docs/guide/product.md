# Product

> The design these flows rest on — attention routing, store-owned durability, one CLI shared by agents and scripts — is [DESIGN.md](../../DESIGN.md).
Rimz runs tens or hundreds of Claude Code, Codex, and Pi sessions in parallel, on a laptop or on a server you reach over SSH, inside the Zellij or tmux you already run, with your keybinds and the official apps untouched. This page is the working tour, ordered the way people scale, each step moving your leverage further from the keyboard: triage a fleet in one room, put a team on a feature, move it to a server, engineer the loop past your attention span, and script agents like any other CLI. The first session, install to a working fleet, is walked step by step in [experience.md](./experience.md).

## The room and the sidebar

Every project gets one room: a Zellij or tmux session with a sidebar where every pane is a row. A bare shell reads `○ zsh`; launch an agent and the row becomes its card (status, current task, model and effort, context meter, running cost), grouped by the worktree it lives in. Ranking triages the column, overdue work rising and calm work settling, and the cockpit line sums the fleet: `? 2  ! 1 …`. The provider dashboard pinned at the bottom reads the pace: each provider's plan, spend for today, the week, and the month, included-window bars draining in real time.

Select a row and you land in its pane, where you answer in the agent's own UI. The column mirrors what runs: when an agent exits, its row reverts to the shell.

Every glyph and meter, zone by zone: [the interface reference](../interface/sidebar.md). Why cards rank the way they do: [attention.md](./attention.md).

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

That's the loop. Everything else in Rimz is a variation on those primitives.

## When an agent needs you

The everyday path has nothing extra wired: the agent asks in its own UI, the hook flips its row to waiting, the sidebar wakes, and Rimz points you at the pane. You see `claude · waiting · permission`, jump, answer, and the row clears when the agent moves on.

Waiting is agent state, so everything downstream reads it the same way: ranking lifts the row ([attention.md](./attention.md)), notifications carry it off-screen, and `rimz message` holds a queued prompt until your answer lands. How blocking prompts become lifecycle signals is in [agent.md](../internals/agents/agent.md).

## Triage a local fleet

Four sessions across two worktrees, and one hits a permission prompt: the sidebar surfaces it, you jump, read what the agent wants to run, and approve in its own prompt. Triage goes from staring at five terminals to answering the questions that need you, when they need you.

Between questions you steer. `rimz message --steer @claude "focus on the failing parser test"` types into the pane now, holding off only while the agent is waiting on your answer. `rimz message --on done @codex "open a PR summary"` delivers at the turn boundary, parking durably until Codex is free, so you hand off follow-up work without watching for it.

Addresses read like Slack. `@codex` names a kind, `@planner` a profile you defined, `@swift-otter` one specific agent, `@all` everyone; `#channel` names the lane, defaulting to the one you are in. Reaching several takes an explicit `--all`, and `--create` launches an agent straight from its address. The grammar and delivery gates: [the agent-control reference](../reference/cli/agents.md#message-an-agent).

## Put a team on a feature

`rimz agents peer --worktree=feat/great` opens the built-in `peer` team, Claude and Codex side by side, in a fresh worktree on its own branch: one plans and implements, the other reviews, both in the same files, isolated from your checkout.

A named team in `agents.toml` makes that shape yours. `[agents.teams]` binds each role to a profile (an agent preset with its own mode, model, effort, and system prompt), and an optional `layout` arranges the roles with the same grammar inline specs use (`rimz agents 'claude,codex+term'` is a Claude column beside a stacked Codex and shell). Every member answers to its role in the team's lane, so steering reads like a standup: `rimz message @planner "split the migration into two PRs"` reaches the right member wherever its pane sits. The config shape is in [configuration.md → Agent profiles, commands, and teams](../reference/configuration.md#agent-profiles-commands-and-teams).

```sh
rimz agents pcr --worktree=feat/great   # your team: planner, coder, reviewer
rimz agents pcr.reviewer                # re-add one role, same handle and lane
rimz agents pcr -w feat/great --resume  # reopen that exact worktree's team
```

The room treats a team as one line of work. The sidebar keeps its members as one contiguous block in role order with one derived state, so one member asking for you marks the whole block blocked and lifts it together ([attention.md → Teams read as one](./attention.md#teams-read-as-one)). Relaunching a team into the same worktree reconciles instead of duplicating: a live team focuses its tab, a closed one with work in progress offers a resume, and a clean merged worktree offers cleanup before a fresh start ([agent-control reference](../reference/cli/agents.md#agents)).

The sidebar groups panes by worktree, so main plus two feature trees render as three groups in one room. Agents in one worktree share files and siblings keep their own: write-capable agents in sibling worktrees is the recommended pattern, and two in the same tree trigger a one-time advisory. Two `rimz agents claude "…" --worktree` launches race parallel attempts, each in its own fresh tree.

Cleanup is supervised. When a worktree agent is reclaimed, work proven landed on the base branch is swept away with its branch; anything dirty, pending, or unknown is kept, behind a keep/remove/shell prompt when you're watching, and `rimz gc` sweeps leftovers later under the same proof ([worktree.md](../internals/harness/worktree.md#cleanup)).

## Run it on a server

Your dev box lives on a server: start agents, close the laptop, reopen from a tablet on the train. `rimz remote connect dev-box:query-engine` reconstructs the sidebar from the store — every agent where you left it, every pending question still waiting. Saved aliases carry the target and reconnect defaults (`rimz remote add dev dev-box:~/code/query-engine`, then `rimz remote connect dev`), the link supervises itself with automatic reconnects, and a `⇄ remote 210ms` badge in the sidebar footer reads link health at a glance ([remote internals](../internals/reach/remote.md)).

The room outlives the host. The store is a directory of flat files under `~/.local/state/rimz/`, and after a reboot or mux crash Rimz offers the fleet back — prior agents idle in their `#channel` tabs (`claude --resume`, `codex resume`, `pi --session`), one prompt from where they stopped. The offer defaults yes, non-interactive starts recover, `rimz reset` or `--no-resume` starts clean, and a room you closed deliberately stays closed.
Long runs meet provider limits, and the room keeps them legible: an agent on the 5-hour wall parks as `⏸` while the provider dashboard counts down the reset (`↻ 1h47m`), in the same place the week's spend reads. That pause is the first thing loop engineering automates, next.

## Engineer the loop

The [everyday loop](#the-loop) needs you at step 3. Loop engineering removes that dependency: you define the goal and the stopping condition, and a cycle of act, observe, decide, repeat keeps the fleet moving when you are not there. Rimz supplies four layers to build with: built-in recovery, scheduled turns, notification wakeups, and a deliberate permission posture. The intelligence in the loop stays yours.

Built-ins first. `[resume] auto_continue = true` (off by default) resumes rate-limit and spend-limit parks the moment the provider window resets and retries overload or transient API errors on a bounded backoff; context compaction rides the same loop, so a long agent keeps its footing with no babysitter process ([configuration.md → Resume](../reference/configuration.md#resume), [provider.md → Auto-continue](../internals/agents/provider.md#auto-continue)).

Then the clock. `rimz loop` drives supervised turns on a schedule (calendar, interval, cron, or poll-until), and a check-guarded task watches a command and wakes an agent on the result, so a failing test suite becomes a fix prompt instead of a red morning ([harness.md → Scheduled turns](../internals/harness/harness.md#scheduled-turns-loop)):

```sh
rimz loop add morning --spec claude-ping --prompt ping --at 07:00 --days weekdays   # prime the 5h window
rimz loop add watchdog --check "cargo test" --on fail \
    --spec codex --prompt "fix the failing test" --every 15m
```

Then wakeups. A notification handler is a per-machine command that fires on the room's attention cues (an agent going waiting, a failure, a park) with the agent, kind, pane id, and workspace root in its environment ([notifications](../internals/sidebar/notifications.md)):

```toml
[[notifications.handler]]
when = { kind = ["waiting"] }
command = "python3 ~/bin/waiting_handler.py"
```

What the handler runs is up to you, and the primitives that drive the room are public CLI: `rimz pane capture` reads the prompt, `rimz pane send` types into the agent's own UI, `rimz message` hands work to another agent, and `rimz agents <kind> -p` runs a supervised one-shot. Composed from them, a handler can clear the routine permission prompt you've approved eight times today: a bounded-pattern script, a one-shot agent delegate, or a standing in-room guardian reached with `rimz message --steer @guardian`. Anything the handler leaves alone stays waiting in the sidebar and still routes to you, so attention bandwidth scales with what you automate rather than with the agent count. The safety posture for handlers is in [security.md](./security.md).

### Choose the permission posture

How an unattended run answers permissions is a posture you choose, and two patterns compose. The posture is the guardrail layer of the harness you are building: a constraint the room and the agent's own prompts enforce, rather than a rule the model is asked to follow.

The agent's own bypass flag runs straight through: `claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`, or `rimz agents <kind> "<prompt>" -p --yolo` to pass the adapter's flag for you (`--ask` keeps the provider's prompts in place). Rimz still observes sessions, completions, and failures through lifecycle hooks; the tradeoff is that the agent skips permission events at the source, so the store holds what other hooks report rather than a per-decision audit trail.

Answering in the agent's own UI keeps the per-decision record: the prompt, the answer, and the tool run all land in the agent's transcript, whether you typed the answer or a handler you wired sent it with `rimz pane send`. Prefer that path when handled prompts belong on the record, and reserve the bypass flag for runs where you accept the tradeoff.

## Put your pipeline in the room

`rimz agents <kind> "<prompt>" -p` runs one supervised agent turn: a real pane opens in the room, the turn runs, the final assistant message prints, and the exit code carries the outcome (`0` success, `1` failure, `124` timeout, `130` cancel). Script ergonomics over an agent you can still see and steer.

```sh
# cron, 02:00 — refresh the deps and open a PR
rimz agents codex --worktree=deps --timeout 4h -p "update dependencies, run the test suite, open a PR"
```

Because the agent runs in a real pane, a run that stops to ask survives the stop: the question takes the normal path (the row goes waiting, pops to the cockpit and your notification channel) and you answer from anywhere while the script is still blocking. A failing migration at 3 a.m. becomes a push on your phone, a one-line fix typed over SSH, and a green pipeline by morning. The same shape runs while you watch: a PR-review job launched from CI joins the room as one more row and asks its design questions in your workspace.

For orchestration:

- `--bg` prints the agent's pet name and returns; `rimz agents wait <ref> --stream` blocks on it later, streaming the turn as it happens.
- `rimz agents show <ref>` reports activity, context, placement, the attached run, the message queue, and recent transcript.
- `rimz message --steer @<agent> "continue"` is the first-class nudge for wrapper scripts; `rimz pane send` and `rimz pane capture` remain the universal fallback.

Flags and selection rules: [the agent-control reference](../reference/cli/agents.md#agents). Run records and completion mechanics: [harness.md](../internals/harness/harness.md#supervised-runs).

Scripts drive the room with the same commands you type. A pipeline that needs a judgment call runs one supervised turn and reads the exit code (`rimz agents claude -p "review the canary metrics and reply SHIP or HOLD"`); a wrapper hands follow-up work to a running agent with `rimz message --on done`; `rimz pane capture` and `rimz transcript` read back what happened. These are the primitives the interactive room runs on, so anything you do at the keyboard, a script can do on a schedule.

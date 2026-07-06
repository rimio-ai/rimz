# Product

> The design these flows rest on — attention routing, ledger-owned durability, one CLI shared by agents and scripts — is [DESIGN.md](../../DESIGN.md).

Rimz is a harness for running tens or hundreds of Claude Code, Codex, and Pi sessions in parallel — on a laptop, or on a server you reach over SSH — inside the Zellij or tmux you already run, with your keybinds and the official apps untouched. This page is the working tour, ordered the way people scale: triage a fleet in one room, pair agents on a feature, spread the room over many repos, move it to a server, engineer the loop past your attention span, and script agents like any other CLI. The felt walk-through, first keystroke to a ten-agent fleet, is [experience.md](./experience.md).

## The room and the sidebar

Every project gets one room: a Zellij or tmux session with a sidebar where every pane is a row. A bare shell reads `○ zsh`; launch an agent and the row becomes its card — status, current task, model and effort, context meter, running cost — grouped by the worktree it lives in. Ranking triages the column, overdue work rising and calm work settling, and the cockpit line sums the fleet: `? 2  ! 1 …`. The provider dashboard pinned at the bottom reads the pace — each provider's plan, spend for today, the week, and the month, included-window bars draining in real time.

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

The everyday path has nothing extra wired: the agent asks in its own UI, Rimz records the ask, wakes the sidebar, and points you at the pane. You see `claude · waiting · permission`, jump, answer, and the row clears when the agent moves on.

Two more answer paths build on the same record. A handler you wire can answer recognised routine prompts ahead of you, in the same UI, on the audit trail ([Engineer the loop](#engineer-the-loop)). And a script can raise its own question into the same column with `rimz feed ask` ([the pipeline scenario](#put-your-pipeline-in-the-room)). The schema names the two surfaces `native_ui` and `script`; wire-level detail lives in [ledger.md](../internals/sidebar/ledger.md).

## Triage a local fleet

Four sessions across two worktrees, and one hits a permission prompt: the sidebar surfaces it, you jump, read what the agent wants to run, and approve in its own prompt. Triage goes from staring at five terminals to answering the questions that need you, when they need you.

Between questions you steer. `rimz message --steer @claude "focus on the failing parser test"` types into the pane now, holding off only while a pending ask owns the keyboard. `rimz message --on done @codex "open a PR summary"` delivers at the turn boundary, parking durably until Codex is free, so you hand off follow-up work without watching for it.

Addresses read like Slack. `@codex` names a kind, `@planner` a profile you defined, `@swift-otter` one specific agent, `@all` everyone; `#channel` names the lane, defaulting to the one you are in. Reaching several takes an explicit `--all`, and `--create` launches an agent straight from its address. The grammar and delivery gates: [the agent-control reference](../reference/cli/agents.md#message-an-agent).

## Two agents, one feature

`rimz agents peer --worktree=feat/great` opens Claude and Codex side by side in a fresh worktree on its own branch: one plans and implements, the other reviews, both in the same files, isolated from your checkout. `peer` is a built-in team; inline specs compose any grid (`rimz agents 'claude,codex+term'` is a Claude column beside a stacked Codex and shell), and `[agents.profiles]` with `[agents.teams]` bind roles like `planner` and `coder` to agent variants. Two `rimz agents claude "…" --worktree` launches race parallel attempts, each in its own fresh tree ([agent-control reference](../reference/cli/agents.md#agents)).

The sidebar groups panes by worktree, so main plus two feature trees render as three groups in one room. Agents in one worktree share files and siblings keep their own: write-capable agents in sibling worktrees is the recommended pattern, and two in the same tree trigger a one-time advisory.

Cleanup is supervised. When a worktree agent is reclaimed, work proven landed on the base branch is swept away with its branch; anything dirty, pending, or unknown is kept, behind a keep/remove/shell prompt when you're watching, and `rimz gc` sweeps leftovers later under the same proof ([worktree.md](../internals/agents/worktree.md#cleanup)).

## Many repos, one room

A room doesn't need a repo. `rimz start` in any directory — `~/code` holding a dozen clones, or a headless server with no source control at all — makes that directory the room ([directory workspace](../reference/cli.md#start-and-attach-a-workspace)). Each git-backed agent groups under the checkout it works in, however deeply nested; panes at the root sit under a name-only header; a scratch directory with no repos is one flat group.

One sidebar triages the whole machine: an agent blocking in `~/code/query-engine` surfaces in the `~/code` room, and the jump takes you to its pane.

## Run it on a server

Your dev box lives on a server: start agents, close the laptop, reopen from a tablet on the train. `rimz remote connect dev-box:query-engine` reconstructs the sidebar from the ledger — every agent where you left it, every pending question still waiting. Saved aliases carry the target and reconnect defaults (`rimz remote add dev dev-box:~/code/query-engine`, then `rimz remote connect dev`), the link supervises itself with automatic reconnects, and a `⇄ remote 210ms` badge in the sidebar footer reads link health at a glance ([remote internals](../internals/reach/remote.md)).

The room outlives the host. The ledger is a directory of flat files under `~/.local/state/rimz/`, and after a reboot or mux crash Rimz offers the fleet back — prior agents idle in their `#channel` tabs (`claude --resume`, `codex resume`, `pi --session`), one prompt from where they stopped. The offer defaults yes, non-interactive starts recover, `rimz reset` or `--no-resume` starts clean, and a room you closed deliberately stays closed.

Long runs meet provider limits, and the room keeps them legible: an agent on the 5-hour wall parks as `⏸` while the provider dashboard counts down the reset (`↻ 1h47m`), in the same place the week's spend reads. That pause is the first thing loop engineering automates, next.

## Engineer the loop

The [everyday loop](#the-loop) needs you at step 3. Loop engineering is how the fleet keeps moving when you are not there, in three layers: built-in recovery, a handler for the prompts that repeat, and a deliberate permission posture.

Built-ins first. `[resume] auto_continue = true` (off by default) resumes rate-limit and spend-limit parks the moment the provider window resets and retries overload or transient API errors on a bounded backoff; context compaction rides the same loop, so a long agent keeps its footing with no babysitter process ([configuration.md → Resume](../reference/configuration.md#resume), [provider.md → Auto-continue](../internals/agents/provider.md#auto-continue)).

Then a handler, for the prompts Rimz should not answer by itself. You've approved "Can I run `cargo check`?" eight times today, and six more agents wake up tomorrow. Wire a notification handler once and it answers the routine ones in the agent's own pane:

```toml
[[notifications.handler]]
when = { kind = ["waiting"] }
command = "python3 ~/bin/loop_permission_handler.py"
```

```
[ waiting row ] → [ handler inspects ] → [ pane send ] → [ record --by handler ]
```

Unknown shapes stay pending and still route to you. The morning after six overnight agents reads: 47 routine permissions answered, 3 architecture-shaped questions skipped, one waiting in the sidebar for you. Attention bandwidth scales with the handler, not with the agent count.

The handler is whatever intelligence you give it — a bounded-pattern script, a supervised one-shot agent (`rimz agents codex -p …`), or a standing in-room guardian reached with `rimz message --steer @guardian`. Reference handlers live in `examples/resolvers/` with the pattern in [resolvers.md](../internals/agents/resolvers.md); handler security lives in [security.md](./security.md).

### Choose the permission posture

How an unattended run answers permissions is a posture you choose, and two patterns compose.

The agent's own bypass flag runs straight through: `claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`, or `rimz agents <kind> "<prompt>" -p --yolo` to pass the adapter's flag for you (`--ask` keeps the provider's prompts in place). Rimz still observes sessions, completions, and failures through lifecycle hooks; the tradeoff is that the agent skips permission events at the source, so the ledger holds what other hooks report rather than a per-decision audit trail.

A permissive handler keeps the audit trail: it recognises a narrow prompt, answers in the agent's own UI with `rimz pane send`, and records the row with `rimz feed resolve --method pane-send --by <name>`. Prefer it when handled prompts belong on the record, and reserve the bypass flag for runs where you accept the tradeoff.

## Put your pipeline in the room

`rimz agents <kind> "<prompt>" -p` runs one supervised agent turn: a real pane opens in the room, the turn runs, the final assistant message prints, and the exit code carries the outcome — `0` success, `1` failure, `124` timeout, `130` cancel. Script ergonomics over an agent you can still see and steer.

```sh
# cron, 02:00 — refresh the deps and open a PR
rimz agents codex --worktree=deps --timeout 4h -p "update dependencies, run the test suite, open a PR"
```

Because the agent runs in a real pane, a run that stops to ask survives the stop: the question takes the normal path — your handler answers the routine ones, the rest pops to the cockpit and your notification channel — and you answer from anywhere while the script is still blocking. A failing migration at 3 a.m. becomes a push on your phone, a one-line fix typed over SSH, and a green pipeline by morning. The same shape runs while you watch: a PR-review job launched from CI joins the room as one more row and asks its design questions in your workspace.

For orchestration:

- `--detach` prints the agent's pet name and returns; `rimz agents wait <ref> --stream` blocks on it later, streaming the turn as it happens.
- `rimz agents show <ref>` reports activity, context, placement, the attached run, the message queue, and recent transcript.
- `rimz message --steer @<agent> "continue"` is the first-class nudge for wrapper scripts; `rimz pane send` and `rimz pane capture` remain the universal fallback.

Flags and selection rules: [the agent-control reference](../reference/cli/agents.md#agents). Run records and completion mechanics: [harness.md](../internals/agents/harness.md#supervised-runs).

Scripts join the room directly too. A deploy pipeline that pauses at a staging-to-prod gate calls `rimz feed ask --title "Promote build 2026.05.18-rc.4?"` and blocks; the question lands in the sidebar with answer buttons, and you or another process answers it from anywhere. `rimz event emit` announces milestones to the same column. These are the same primitives agent integrations use, so anything an agent can surface, a script can too.

# Teams

A team launches several agents as one unit, each in a named role with its own context window, model, and instructions, cooperating over messages in a shared channel.

<p align="center">
  <img src="../rimz-team.png" alt="A RimZ room running forge teams: the coder messages the planner about a gap in the plan, and the planner takes the design call" width="100%">
  <br/><sub>The loop mid-conversation: <code>@coder</code> finds a gap in the plan and messages <code>@planner</code> with evidence; the planner (center pane) verifies and updates the plan.</sub>
</p>

Define the roles once in `agents.toml`, then launch the whole set with one name; each member answers to its own role handle. For a one-off pairing, put [roles directly in the layout spec](./fleet.md#compose-a-layout) with `cell:role`; a role set earns a named team when it recurs. You compose the roles the way the work splits — the shipped `forge` team, one split that works really well, pairs a planner, a coder, and a reviewer on one feature.

```sh
rimz teams forge -w feat-complex          # planner, coder, reviewer on one feature
rimz message @planner "ship the plan"     # each member answers to its role handle
rimz teams resume forge                   # reopen the newest closed forge team
```

## Why split the work

Every agent works inside one context window, and everything it does fills it: files read, tool output, discussion, dead ends. A filling window costs more per turn and reasons less sharply, so the window is the real budget a long task runs against. And inside the window, attention is the scarcer resource still: a model weighs everything it holds against everything else, and when everything claims importance, nothing receives it.

Subagents are the first tool against that, and a good one. The parent dispatches an explore subagent to locate the relevant code and folds back a summary instead of the whole search; plan subagents draft competing directions in parallel. But a subagent runs one way: the parent spawns it, collects its report, and still carries the whole task in its own window.

A team splits the task itself across independent windows:

- Each member keeps its own context and its own attention, the way specialists on a human team do. Each member still uses its own subagents, so a team stacks on that architecture rather than replacing it.
- Each member can run a different provider. Model capability is jagged (brilliant at one kind of work, mediocre at the next), the peaks and valleys sit in different places per model, and each model has a comfort zone shaped by its size and training. Matching phases of the work to models lets one model's peak cover another's valley: better results for fewer tokens.
- Members talk both ways, over as many rounds as the work needs: a downstream role can ask, push back, and escalate, which a subagent can never do to its parent.

That is what a team manages: which window holds which part of the problem, and what each window's attention is spent on. The split itself is yours to design: two roles or five, one provider or several, whatever shape the work divides into. The rest of this page walks one split that has proven itself.

## The forge loop

`forge` is the team RimZ builds itself with, shipped ready to copy under [`examples/teams/forge/`](https://github.com/rimio-ai/rimz/tree/main/examples/teams): three roles that carry one change through plan, code, and review, each role a profile with its own model, effort, and system-prompt file.

- **@planner** (Claude, on Fable) talks with you. It explores the code through subagents, drafts directions, confirms the design choices with you, and writes the plan. Planning is where a deep model earns its price: Fable reads the intention behind a question, holds a large design in view, and writes implementation plans precise enough to execute. On a complex problem that conversation alone fills 200k to 300k tokens, and by hand-off the window holds the whole design history: the exploration, the alternatives weighed, your decisions. That context is exactly what good design calls for, and exactly what execution doesn't need. Asking the same window to also type the code would push it toward 400k to 600k tokens, where every turn gets slower and pricier and a big model's reasoning starts to dull.
- **@coder** (Codex, on Sol, the current GPT 5.6) starts fresh from the plan: a clean window, the exact files to touch, the decisions already made. Sol is fast, cheap, and writes robust code once the details are pinned down, and a precise plan makes its context loading precise too: it pulls in just the code the change touches and implements from there. It verifies the plan against the real code as it goes rather than trusting it.
- **@reviewer** (Claude, on Opus) is a third fresh window. It reads the plan, reviews the full diff blind before opening the coder's report, so its findings form from the code rather than the coder's narrative, then reconciles that report claim by claim.

By the time implementation starts, two windows understand the problem from different sides: the planner's holds the design history, the coder's holds the code as it stands, and each catches what the other misses. So the roles keep talking. The coder hits a choice the plan left open and takes it to the planner, whose full design context makes it the right desk for the call. Coder and reviewer argue findings with `file:line` evidence, fix or push back, and escalate to the planner when a dispute turns out to be a design call. Three independent windows, each with its own focus, cooperating as one team.

The split reads like it should multiply cost; in practice it divides it. Building RimZ with forge, a complex change that a single Fable window would carry to 400k or 500k tokens and well past $100 lands around $10 of planning, $20 of coding, and $10 of review, and the quality rises as the price falls, because each window spends its whole budget inside its comfort zone.

## Set up forge

Install the release-matched bundle from GitHub, then launch it into a worktree:

```sh
rimz teams install forge
rimz teams forge -w feat-complex            # the whole team, one isolated worktree
```

From a repository checkout, copy the fragment instead when you want to edit that checkout's version directly:

```sh
mkdir -p ~/.agents/teams
cp -r examples/teams/forge ~/.agents/teams/
```

Then hand the task to the planner and let the loop carry it: type into the planner's pane, or message it.

```sh
rimz message @planner "add rate limiting to the ingest API"
```

The planner comes back to you at its design gates; the sidebar lifts the whole team the moment any role needs you ([one team, one line of work](#one-team-one-line-of-work)). The `claude` and `codex` CLIs must be on `PATH`; models, feature flags, and the rest of the install fine print are in the [examples README](../../examples/README.md#forge-agent-team--teamsforge).

## See and drive your teams

The team catalogue merges configured definitions with the cohorts alive in this room:

```sh
rimz teams                              # every definition and live instance
rimz teams show forge                   # roles, models, validation, and live members
rimz teams forge -w feat-query          # launch or reconcile one cohort
rimz teams resume forge                 # reopen its newest closed cohort
rimz teams focus forge                  # jump to the role that needs attention
rimz teams restart forge                # restart every role in declared order
rimz teams stop forge                   # close the whole live cohort
```

Add `--json` to `rimz teams` or `show` for the structured report.
The bare team name and the longer `launch` form use the same reconciliation engine as `rimz agents <team>`, while `resume`, `focus`, `restart`, and `stop` keep the cohort lifecycle together.
When the same team is live in several lanes, run the lifecycle command inside the lane you mean or select it with `-w NAME`.
The `COST` in `rimz teams show`, the team's attribution footer, and its collapsed finished sidebar receipt all use the same lifetime fold across every resumed session of each role, so their dollars and token totals agree. Expanding that finished receipt puts each role's lifetime cost on its card, and those cards add back to the receipt; live cards remain scoped to the current provider session.
Use [`rimz agents attribution --md`](../reference/cli/agents.md#attribution) when the team's pull request is ready; it credits every contributing role from durable lane history, including members that exited before the PR opened.
The full flag surface lives in the [teams CLI reference](../reference/cli/teams.md).

## Define your own team

A team in `agents.toml` (or a drop-in fragment like forge's `team.toml`) is a list of roles, each bound to a configured profile or registered agent kind, with an optional `layout` using the same row and column operators as inline specs (commas split columns, plus signs tile rows, slashes stack them). A same-named machine profile overrides the kind's implicit base. Its cells name declared roles or roleless cells, while ad-hoc `cell:role` suffixes stay exclusive to inline specs:

```toml
# Copy-ready drop-in fragment for ~/.agents/teams/forge/.
# See examples/README.md for install and launch commands.

[agents.teams.forge]
layout = "planner,coder+reviewer"
scratch-files = ["/plan.md", "/result.md", "/review.md"]

[[agents.teams.forge.roles]]
role = "planner"
profile = "claude"
mode = "auto"
model = "fable"
effort = "high"
args = "--strict-mcp-config --tools 'Bash,Read,Edit,Write,AskUserQuestion,WebFetch,WebSearch,Skill,Agent(Explore,Plan)'"
system-prompt-file = "planner.md"

[[agents.teams.forge.roles]]
role = "coder"
profile = "codex"
model = "gpt-5.6-sol"
effort = "xhigh"
args = "--strict-config -c 'web_search=\"cached\"' -c 'features.goals=false' -c 'features.multi_agent=false' -c 'features.shell_snapshot=true' -c 'features.shell_tool=true' -c 'features.skill_mcp_dependency_install=false' -c 'features.tool_call_mcp_elicitation=false' -c 'features.unified_exec=true' -c 'features.browser_use=false' -c 'features.browser_use_external=false' -c 'features.computer_use=false' -c 'features.in_app_browser=false' -c 'features.image_generation=false' -c 'features.tool_suggest=false' -c 'features.memories=false' -c 'features.default_mode_request_user_input=false' -c 'skills.include_instructions=true'"
system-prompt-file = "coder.md"

[[agents.teams.forge.roles]]
role = "reviewer"
profile = "claude"
mode = "auto"
model = "opus"
effort = "xhigh"
args = "--strict-mcp-config --tools 'Bash,Read,Edit,Write,WebFetch,WebSearch,Skill'"
system-prompt-file = "reviewer.md"
```

`scratch-files` declares the workflow's ephemeral team memory as verbatim gitignore patterns. On every launch or resume, RimZ appends missing patterns to the repository's `.git/info/exclude`; the leading `/` anchors these names at the checkout root. Linked worktrees commonly share that exclude file with the main checkout, so a declared name is ignored as untracked everywhere in the repository, not only in this team's worktree. To reverse it, delete those pattern lines from `.git/info/exclude`.

Launching the team name opens every member in that layout, and each answers to its role handle: `@reviewer` inside the team's channel, `forge.reviewer` from anywhere in the workspace. The optional `leader` names the role that receives a trailing launch prompt; without it, the first declared role leads. `rimz teams forge -w feat-x "task"` therefore seeds the planner directly, while the rest of the team starts ready for its hand-offs. `rimz agents forge.reviewer` launches or re-adds that one role with the same identity it has inside the full team, and from a pane in the team's own channel the bare `rimz agents reviewer` means the same thing.

Start from the forge directory or from scratch: rename the roles, add or drop some, swap the models and prompts — a pair, a trio, or a whole bench of specialists. The role prompts do the heavy lifting: each one states the role's craft, how the roles hand work to each other, and who owns which decision, which is what turns co-launched agents into a team instead of a row of panes. The full config shape, override fields included, is in [configuration → agent profiles, commands, and teams](./configuration.md#agent-profiles-commands-and-teams).

## Relaunch reconciles instead of duplicating

Point any co-launched layout — a named team or an inline multi-agent spec — at an explicit worktree name, and RimZ reads the state first: a live cohort focuses its tab, a closed cohort with work in progress offers to resume it, and a clean merged tree offers to remove it and start fresh.

`rimz agents claude:planner,codex:coder -w feat-once` focuses the existing pair when the same command runs again.

`--resume` (alias `--continue`) forces the resume path, reopening the newest matching set of sessions: by team name and role for a team, or by cell order for an inline spec. Resume takes identity, working directory, and channel from RimZ's durable records and each role's model, effort, system prompt, and permission mode from its profile, so a resumed team comes back configured exactly as it launched. It stands alone: no prompt, model, or channel flags ride with it.

```sh
rimz teams resume forge            # reopen the newest closed forge team
rimz agents claude,codex --resume  # reopen the newest matching inline pair
rimz agents claude --resume        # resume the freshest closed Claude session
```

When the place is easier to name than the spec, `rimz agents resume '#feat-x'` restores the lane's saved team layout and stray agents without requiring the team name. This place-first form converges a partially live team by adding only its closed members; the spec-first `--resume` form selects a cohort by team or layout.

## One team, one line of work

The room treats a team as a single line of work: the sidebar names the active group with `· <team>`, keeps its members as one contiguous block with one derived state, and lets one member asking for you lift the whole block ([the sidebar guide → Teams read as one](./sidebar.md#teams-read-as-one)).

## See also

- [Agents](./fleet.md) — launch agents by name and compose the layout a team fills.
- [Worktrees](./worktrees.md) — isolate a team on its own branch for parallel work.
- [Messaging](./messaging.md) — reach a role by handle: park, steer, schedule, and channels.
- [Examples → forge](../../examples/README.md) — the shipped forge fragment: install, prerequisites, and try-before-install.
- [Configuration → profiles and teams](./configuration.md#agent-profiles-commands-and-teams) — the `agents.toml` shape behind every profile and team.
- [Teams CLI reference](../reference/cli/teams.md) — discover, inspect, install, launch, resume, and drive named teams.
- [Agent-control reference](../reference/cli/agents.md) — the complete `rimz agents`, `worktree`, and `gc` surface.

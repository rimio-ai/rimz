# Teams

Copy-ready RimZ team fragments: named agent teams you drop into `~/.agents/teams/` and launch with one word. A team gives each role its own context window, model, and system prompt, cooperating over messages in a shared channel; the concept and the config shape live in the [teams guide](../../docs/guide/teams.md).

One team ships today: **forge**, the plan → code → review team RimZ builds itself with.

## Forge — `forge/`

Forge carries one change from idea to open PR through three roles, each with its own model and prompt, so every member spends its whole window inside its comfort zone:

- **@planner** (Claude, on Fable) talks with you. It explores the code through subagents, confirms design choices at user gates, and writes the plan to `plan.md`. It owns design and intent: every design question during the loop lands on its desk.
- **@coder** (Codex, on the current GPT) starts fresh from the plan: a clean window, the exact files to touch, the decisions already made. It treats the plan as a hypothesis to verify against the real code, implements, resolves review findings, and opens the PR.
- **@reviewer** (Claude, on Opus) is a third fresh window. It reviews the full diff blind — findings drafted before it ever opens the coder's report — then reconciles that report claim by claim, conceding only to the code.

The fragment is four files: [`team.toml`](./forge/team.toml) declares the team and each role's agent kind, model, effort, launch args, prompt, and layout; [`planner.md`](./forge/planner.md), [`coder.md`](./forge/coder.md), and [`reviewer.md`](./forge/reviewer.md) are the role prompts, each stating the role's craft plus the shared team protocol.

### How the loop runs

```
user → @planner —plan→ @coder —result→ @reviewer —review→ @coder ⇄ @reviewer —clear→ @coder —PR→ done
```

The team shares one worktree; @coder works on a feature branch so commits accumulate for the reviewer's diff and the final PR. Three git-ignored scratch files at the worktree root carry the substance — `plan.md`, `result.md`, `review.md` — and hand-offs are short `rimz message` lines that point at them:

1. **Plan.** @planner drafts the design with you, gates included, and writes `plan.md`. Its `# <Title>` H1 becomes the PR title.
2. **Implement.** @coder implements and verifies the plan, taking open design calls back to @planner, and reports to `result.md`.
3. **Review.** @reviewer reviews the full merge-base diff blind, drafts `review.md`, then reconciles the coder's report and hands down a verdict: blocking or clear.
4. **Resolve.** On a blocking verdict, @coder and @reviewer argue each finding with `file:line` evidence — fix, push back, or escalate a design clash to @planner — until a re-review comes back clear.
5. **PR.** @coder opens the PR: `plan.md` becomes the description, `result.md` the first comment. The open PR ends the loop.

State lives in the scratch files and git, never in the chat: a member restarted or compacted mid-loop re-derives its step from the files and carries on. The sidebar treats the team as one block, lifting all three the moment any role needs you.

Why this split pays for itself — independent windows, matched model strengths, two-way escalation — is the [teams guide](../../docs/guide/teams.md#the-forge-loop).

### Install

From a checkout of this repository, copy the fragment into the agents home:

```sh
mkdir -p ~/.agents/teams
cp -r examples/teams/forge ~/.agents/teams/
```

A same-named directory in `~/.agents/teams` is overwritten; remove it first for a clean copy. Entries in `~/.config/rimz/agents.toml` override fragment entries with the same names.

**Prerequisites:**

- RimZ installed with hooks set up ([installation](../../docs/guide/installation.md) · [setup](../../docs/guide/setup.md)).
- The `claude` and `codex` CLIs on `PATH`, each logged in — the planner and reviewer run Claude Code, the coder runs Codex.
- Optional: the coder's PR step expects a `pr` skill and falls back to plain `gh` or `tea` without one.

Try the team before installing by pointing RimZ at the checkout:

```sh
RIMZ_AGENTS_HOME="$PWD/examples" rimz agents forge
```

### Launch and work

Launch the team into an isolated worktree and hand the task to the planner — type into its pane, or message it:

```sh
rimz agents forge -w feat-complex
rimz message @planner "add rate limiting to the ingest API"
```

The planner comes back to you at its design gates; after your sign-off the loop carries the change through code, review, and PR on its own. Useful moves while it runs:

```sh
rimz agents '#feat-complex'        # the team's cards in its lane
rimz message @coder --wait "status? one line"
rimz agents forge --resume         # reopen the newest closed forge team
```

### Customize

`team.toml` is the tuning surface: swap models, change effort, or adjust the Codex feature flags in the role's `args`. The role prompts do the heavy lifting, so renaming roles, dropping one, or adding a fourth means editing the prompts and the `[[agents.teams.forge.roles]]` list together. Forge also makes a solid skeleton for a team of your own: copy the directory under a new name and reshape the roles to how your work splits. The full config shape is in [configuration → profiles and teams](../../docs/guide/configuration.md#agent-profiles-commands-and-teams).

## See also

- [Teams guide](../../docs/guide/teams.md) — why split the work, and the forge loop in depth.
- [Worktrees](../../docs/guide/worktrees.md) — the isolated checkout `-w` gives the team.
- [Messaging](../../docs/guide/messaging.md) — handles, park/steer delivery, and channels.
- [Examples index](../README.md) — every shipped fragment.

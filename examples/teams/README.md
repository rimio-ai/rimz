# Teams

Copy-ready RimZ team fragments: named agent teams you drop into `~/.agents/teams/` and launch with one word. A team gives each role its own context window, model, and system prompt, cooperating over messages in a shared worktree; the concept and the config shape live in the [teams guide](../../docs/guide/teams.md).

Three teams ship, one per shape of work:

| Team | Use it for | Roles |
| --- | --- | --- |
| [**forge**](#forge--forge) | a change worth designing before it is built: a feature, a fix whose shape is a design call | @planner → @coder → @reviewer |
| [**mill**](#mill--mill) | accreted structural debt: a behavior-preserving pass that removes surface | @architect → @coder → @reviewer |
| [**spot**](#spot--spot) | a small change with no design left to do: a bug, a wrong default, a stale doc line | @coder → @reviewer |

Forge is the team RimZ builds itself with; mill and spot are the same machinery with a different pipeline and roster.

## What every team shares

The three fragments differ in their roles and their pipeline. Everything below is common to all of them, so learning one teaches the others.

**A pipeline of stages.** `team.toml` declares an ordered `stages` list, and each role owns the stages its prompt names. A stage is an obligation to leave specific information behind, not a turn: the owner does the work, writes its product, then hands off. `rimz teams show <team>` displays the pipeline and brackets the current stage.

**Memory in files, never in the chat.** Two kinds of file live at the worktree root, both git-excluded through `scratch-files` and never committed:

- `blackboard.md` — the run's state and history, and where you look to see where the run stands. It carries the `Stage:` line, the Goal, an append-only Decisions list, an append-only Progress log, and the Result.
- `<stage>-notes.md` — one per stage, written only by that stage's owner: `explore-notes.md`, `plan-notes.md`, `implement-notes.md`, `review-notes.md`, `reflect-notes.md`. The narrative lives here; the board carries state.

Because state lives in the files and in git, a member that crashed, restarted, or compacted mid-run re-derives its position from the `Stage:` line and the Progress log and carries on.

**One channel to you.** The leader (`@planner`, `@architect`, or `@coder`) is the only seat that talks to you: it restates your request as the board's Goal and raises the calls only you can make. The other roles end their turns silently and reach each other with short `rimz message` lines that point at a file — `plan-notes.md is ready, read and implement` — never with the content itself.

**A blind review.** Every team's reviewer builds its own model of the change from the intent and the diff, with the implementer's report embargoed until its findings are drafted. Blocking findings go back to the coder, who fixes, pushes back with `file:line`, or refuses with a reason; two rounds, then the reviewer rules.

**CI comes back to the coder.** Each fragment binds the `ci.failed` signal to its coder role, so a failing run after submission reaches the agent that can fix it, with its evidence, rather than whoever pushed.

**The run ends in Reflect.** The final stage distills what the run learned into `reflect-notes.md`: a ranked list of fixes, each marked landed, proposed, or yours to decide. That file is the one to read before the next run.

## Forge — `forge/`

Three roles carry one change from idea to open PR, each in its own window and on the model that suits its stage.

```
Explore → Plan → Implement → Review → Submit → Reflect
@planner   @planner  @coder    @reviewer  @reviewer  @planner
```

- **@planner** (Claude, on Fable) is the leader. It aims the exploration, sizes the code through subagents, and designs the change with you at its gates, locking each decision into the board. `plan-notes.md` is its product and stays its file for the whole run: when an assumption breaks, the planner amends it and steers whoever is building on it.
- **@coder** (Codex, on GPT) starts fresh from the plan: a clean window, the files named, the decisions already made. It treats the plan as a strong proposal rather than ground truth, takes a deviation the code justifies and records it with evidence, and stops only when a broken assumption invalidates a large part of the plan.
- **@reviewer** (Claude, on Opus) is a third fresh window. It gates the pass on a clean tree, reviews the full merge-base diff blind, and then owns Submit: the PR body is synthesized from the board and the stage files, advisories included.

## Mill — `mill/`

The refactor team. What the run delivers is surface removed — counted in source lines, interfaces, flags, and files — so a pass that only moves code sideways has failed even when every stage ran clean.

```
Explore → Plan → Implement → Review → Submit → Reflect
@architect  @architect  @coder   @reviewer  @reviewer  @architect
```

- **@architect** (Claude, on Fable) leads. It censuses the target, reads each module for what the code does rather than what its name claims, and weighs the result against a from-scratch design rather than against the current shape. Its plan is one self-standing ordered pass with a line budget, and "nothing here is worth a pass" is a real outcome it will bring you.
- **@coder** (Codex, on GPT) executes the pass. Moves land in their own commits, separate from the edits to the moved code, so the review reads a move as a move.
- **@reviewer** (Claude, on Opus) reviews for behavior preserved and for surface actually gone: it measures the diff against the plan's line budget itself, and calls a rename a rename.

Point it at a repository, a subtree, or a single module's interface. It is the wrong team for a feature or a bug.

## Spot — `spot/`

Two roles, no design stage, for a change that needs none.

```
Implement → Review → Submit → Reflect
  @coder     @reviewer  @reviewer  @coder
```

- **@coder** (Codex, on GPT) leads and implements. Your request is the board's Goal, and the coder builds from its own read of the code, fixing the cause rather than the symptom. A fix that outgrows the Goal comes back to you instead of quietly widening.
- **@reviewer** (Claude, on Opus) reviews blind and ships the PR, the same craft forge and mill use.

Its coder keeps Codex's user-input tooling enabled, since it is the seat you talk to.

## Install

Install the bundle matching the running RimZ release:

```sh
rimz teams install forge
rimz teams install mill
rimz teams install spot
```

From a checkout of this repository, copy a fragment when you want that checkout's version:

```sh
mkdir -p ~/.agents/teams
cp -r examples/teams/spot ~/.agents/teams/
```

The plain install preserves a same-named directory in `~/.agents/teams`; pass `--force` to replace its files. Entries in `~/.config/rimz/agents.toml` override fragment entries with the same names. To try a team straight from this checkout without installing anything, point RimZ's agent library at it:

```sh
RIMZ_AGENTS_HOME="$PWD/examples" rimz teams spot -w fix-x
```

**Prerequisites:**

- RimZ installed with hooks set up ([installation](../../docs/guide/installation.md) · [setup](../../docs/guide/setup.md)).
- The `claude` and `codex` CLIs on `PATH`, each logged in. Spot needs both too: its coder runs Codex, its reviewer Claude.
- The crafts name helper skills for the mechanical steps — `commit`, `rebase`, `fix-ci`, `branch-diff`, `pr`, and `reflect`. They are not shipped here. Each mention states the outcome as well as the skill, so a role without one does the step with plain `git` or `gh`; install your own equivalents to make those steps sharper.
- Subagent delegation is optional. The prompts ask for exploration, design, and defect-hunting children where the work parallelizes, and `rimz subagents profiles` tells the role what is actually configured — with no `[subagents.profiles]` entries, each role does that reading itself.

## Launch and work

Launch a team into an isolated worktree and hand it the task — type into the leader's pane, or message it:

```sh
rimz teams forge -w feat-complex
rimz message @planner#feat-complex "add rate limiting to the ingest API"
```

The team shares that one worktree, and the coder works on a feature branch so commits accumulate for the reviewer's diff and the final PR. `rimz teams show forge#feat-complex` gives the live cohort: current stage, memory files, PR and CI facts, and a row per member. The sidebar treats the team as one block, lifting every member the moment any role needs you.

The `ci.failed` binding is armed when the coder registers and is scoped to its worktree, so launching it on the root checkout is refused without an explicit branch or path match: use `-w`, or launch from a linked worktree.

## Customize

`team.toml` is the tuning surface: swap models, change effort, adjust the Codex feature flags in a role's `args`, or rename the pipeline's stages. The role prompts do the heavy lifting, so renaming a role, dropping one, or adding a fourth means editing the prompts and the `[[agents.teams.<name>.roles]]` list together — the prompts carry their own roster table and pipeline, and a role's `owns` column must keep matching `stages`.

Any of the three also makes a solid skeleton for a team of your own: copy the directory under a new name, rename the team in `team.toml`, and reshape the roles to how your work splits. The full config shape is in [configuration → profiles and teams](../../docs/guide/configuration.md#agent-profiles-commands-and-teams).

## See also

- [Teams guide](../../docs/guide/teams.md) — why split the work, and the forge loop in depth.
- [Teams CLI reference](../../docs/reference/cli/teams.md) — launch, resume, focus, stop, and install.
- [Worktrees](../../docs/guide/worktrees.md) — the isolated checkout `-w` gives the team.
- [Messaging](../../docs/guide/messaging.md) — handles, park/steer delivery, and channels.
- [Examples index](../README.md) — every shipped fragment.

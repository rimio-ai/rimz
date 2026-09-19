# Teams

`rimz teams` lists, inspects, launches, resumes, and drives named teams, hands a team's board from stage to stage, and installs team bundles.

A team is a configured set of roles and a layout. Each role keeps its own model, prompt, and `@role` handle, and every member of one launch shares one lane; that group of live members is a cohort, addressed as `team#lane` (for example `forge#feat-rate-limits`). The [teams guide](../../guide/teams.md) explains how to design and run a team, and [configuration → teams](../../guide/configuration.md#teams) owns the definition fields (`roles`, `leader`, `layout`, `stages`, `scratch-files`, `consensus-file`, `append-system-prompt-files`, and each role's `owns`, `signals`, and `flip-compact`). This page owns the command forms.

| Command | Does |
| --- | --- |
| `rimz teams`, `list`, `ls` | [List teams](#list-teams) and their live cohorts. |
| `rimz teams show`, `inspect` | [Inspect one team](#inspect-one-team), or every team live in a lane. |
| `rimz teams profiles` | [List team role profiles](#list-team-role-profiles). |
| `rimz teams <team>`, `launch` | [Launch a team](#launch-a-team). |
| `rimz teams resume` | [Resume a team](#resume-a-team)'s closed cohort. |
| `rimz teams focus`, `stop`, `restart` | [Drive a live team](#drive-a-live-team). |
| `rimz teams wait` | [Wait for a cohort to finish](#wait-for-a-cohort-to-finish). |
| `rimz teams flip` | [Flip the board to the next stage](#flip-the-board-to-the-next-stage). |
| `rimz teams install` | [Install a team bundle](#install-a-team-bundle). |

`rimz teams` sets where a cohort runs, whether it resumes, and what each member may spend. Per-agent shaping (model, effort, prompts, permission mode, name, pane placement, supervised runs) stays on [`rimz agents`](./agents.md), which also launches one role of a team as `rimz agents forge.reviewer`. Every form takes the [global flags](../cli.md#global-flags).

## List teams

```sh
rimz teams
rimz teams ls --json
```

The bare command, `list`, and `ls` print one row per live cohort, and one row for each defined team with no live cohort:

| Column | Live cohort | No live cohort |
| --- | --- | --- |
| `TEAM` | Team name. | Team name. |
| `LANE` | `#<lane>`. | `-` |
| `STAGE` | The board stage, with ` (@owner)` when the board names one. | `-` |
| `PR` | `#<number>` from the room's cached PR facts, followed by the CI glyph unless the PR is closed; `-` when nothing is cached. | `-` |
| `STATUS` | The cohort [state](#cohort-state). | `ready` |

A broken definition replaces `STATUS` with `broken: <error>` on every row of that team. A live cohort whose team is no longer defined keeps its state with ` · not defined` appended. Resolved roles and models appear in [`show`](#inspect-one-team).

The catalogue merges machine [Markdown definitions](../definitions.md) under `<agents_home>/teams/<name>.md` with the repository's `.rimz/config.toml` when trusted. Invalid definitions fail with their source errors; unknown frontmatter keys are errors, not ignored settings. Run `rimz agents validate` to inspect the complete library. The built-in roleless `peer` team remains unless replaced. With no user teams, the listing includes an install hint.

`--json` prints an array of [team records](#json-report). `--json` works only on the list forms and `show`: `rimz teams forge --json` is refused with a pointer to `rimz teams show forge --json`.

## List team role profiles

```sh
rimz teams profiles
rimz teams profiles --path          # add each profile's defining file
rimz teams profiles --json --path
```

`rimz teams profiles` lists the resolved seat profiles named `<team>.<role>` for a configured team, such as `forge.coder`, as the same cards and JSON as [`rimz agents profiles`](./agents.md#discover-agent-profiles). The launch grammar resolves those names as team roles, so `rimz agents profiles` leaves them out unless it gets `--teams`. The listing reads the machine configuration; with none configured it prints `No team profiles configured.` and the install command.

## Inspect one team

```sh
rimz teams show forge                    # the definition, then one row per live cohort
rimz teams show forge#feat-rate-limits   # one cohort's status, then the definition
rimz teams show '#feat-rate-limits'      # every team live in that lane
rimz teams show forge --json
```

`show` (alias `inspect`) takes one target:

| Target | Reports |
| --- | --- |
| `<team>` | The definition, then a table of the team's live cohorts. The team must be defined: a removed team's live cohort shows only in the list and in lane-only forms. |
| `<team>#<lane>`, or `<team> -w <lane>` | The lane block of the cohort whose lane is `<lane>` or whose members run in worktree `<lane>`, then the definition. With no match it prints `no live instance in #<lane>` before the definition and exits 0. |
| `'#<lane>'`, or `-w <lane>` alone | For every team live in that lane, defined or not, its lane block then its definition. With none it prints `no live team in #<lane>` and exits 0. |

Give the lane once: the fused form and `-w` together are refused. Quote `'#lane'` so the shell does not read it as a comment.

Each form leads with what it is asked for. A lane form, here `rimz teams show forge#feat-x`, checks on a run:

```text
forge#feat-x · idle 48s
  stage:    Implement (@coder) for 28m
  pipeline: Explore → Plan → [Implement] → Review → Submit → Reflect → Done
  pr:       #412 · open · ci pending · https://github.com/acme/api/pull/412

  MEMBER     STATUS   AGE  ACTIVITY           CTX   COST
  @planner   success  28m  -                  61%  $1.84
  @coder     success  48s  editing ingest.rs  42%  $0.95
  @reviewer  idle     31m  -                   8%  $0.12

  signals: ci.failed → @coder · never fired · team-forge-feat-x-coder-ci-failed

  worktree:  /repo-worktrees/feat-x
  isolation: host · tmp /tmp
  memory:    blackboard.md   41 lines · 2m ago
             plan-notes.md  120 lines · 18m ago

forge
  source: ~/.rimz/teams/forge.md
  layout: planner,coder+reviewer

  @planner   claude  fable        <- leader
  @coder     codex   gpt-6-astra
  @reviewer  claude  opus
  signals   ci.failed → @coder
  (prompt stack: rimz teams show forge --json)
```

The team form, `rimz teams show forge`, describes the team and lists its cohorts:

```text
forge
  source: ~/.rimz/teams/forge.md
  layout: planner,coder+reviewer

  @planner   claude  fable        <- leader
  @coder     codex   gpt-6-astra
  @reviewer  claude  opus
  signals   ci.failed → @coder
  (prompt stack: rimz teams show forge --json)

  LANE     STAGE                       PR                        STATUS
  #feat-x  Implement (@coder) for 28m  #412 · open · ci pending  idle 48s
  #feat-y  Plan (@planner) for 4m      -                         working
```

The definition block names `source` and `layout`, and adds an `error` line only when the definition is broken. The roster lists each role's handle, provider, and model (`-` when unset), marks the leader with `<- leader`, and ends with a `signals` line when roles declare [signal bindings](../../guide/configuration.md#team-signal-bindings), each shown as `signal (match filters) → @role` and clipped to the terminal width. When any role has system-prompt files, a pointer to `--json` follows, since the JSON carries the resolved prompt stack.

The cohort table's `STAGE` and `STATUS` cells carry the same text as a lane block's `stage` line (`-` when there is none) and header state; `PR` is the `pr` line without the URL.

A lane block reads top to bottom:

| Line | Shows |
| --- | --- |
| Header | `<team>#<lane> · <state>`, the [cohort state](#cohort-state) followed by the time since any member's last activity, except while `working`. |
| `stage` | Always present. The [board stage](#board-stage) with ` (@owner)` when the board names one, then ` for <age>`, the time since the flip into that stage; `Done for <age>` once the board is flipped to `Done`; `none` without a board stage. The age is omitted when no recorded flip matches (a hand-edited board, or a flip rotated out of the event log). |
| `pipeline` | The declared `stages` with the implicit `Done` last, bracketing the board stage when it matches a name exactly. Omitted when the team declares no `stages`. |
| `pr` | Cached PR number, state (`open`, `merged`, `closed`), `ci passing`, `ci pending`, or `ci failing`, and URL, as far as known; `none` when nothing is cached. |
| Member table | `MEMBER STATUS AGE ACTIVITY CTX COST`, members in the team's declared role order. `STATUS` uses the [agent status words](./agents.md#list-and-manage-agents), `AGE` is the time since the member's last activity, and `COST` is each role's lifetime spend in this worktree. |
| `signals` | One line per subscription RimZ armed for a member from its role bindings: `<signal> → @<member> · <fire> · <loop task name>`. `<fire>` is `never fired`, `fired <age> ago` when the last firing delivered, `skipped <age> ago` when it matched the family but not the subscription, or the [loop run result](./loop.md#read-run-history) with its age; ` ×<n>` gives the total run count once it has run more than once. Omitted when nothing is armed. The roster's `signals` line shows what is declared; this one shows what is armed now and whether it fired. |
| `worktree` | The members' absolute checkout path, with ` · branch <name>` when the branch differs from the directory name; `-` when members disagree. |
| `isolation` | `host · tmp /tmp`, or `sandbox · tmp <room tmp dir> (as /tmp)`: the `--isolation` recorded at launch or replaced on resume, else the machine's `agents.isolation`. Members that disagree show the machine setting. |
| `memory` | Files matching the team's effective scratch patterns (defaults or explicit `scratch-files`), relative to the worktree, with line counts and modification ages. Omitted when none exist. |

A script that blocks until the work is done runs [`rimz teams wait`](#wait-for-a-cohort-to-finish) rather than polling `.stage.name` from `--json`. The report ends with the definition, with no launch, resume, or focus hints.

The definition includes a `(prompt stack: rimz teams show <team> --json)` hint when roles have prompt files or the team has a consensus.

### Cohort state

A cohort's state is the first rule that matches its members' statuses:

| State | When |
| --- | --- |
| `blocked` | Any member is `waiting` or `failed`. |
| `paused` | Any member is `paused`. |
| `working` | Any member is `running`. |
| `sleeping` | Any member is `sleeping`: at rest with a one-shot wait or delivery armed. Standing subscriptions do not count. |
| `idle` | Otherwise, including members whose last turn succeeded. Whether the pipeline finished is the [board stage](#board-stage), never member status. |

### Board stage

The stage comes from the first line starting `Stage:` in `<worktree>/blackboard.md`, for example `Stage: Implement (@coder)`. Only a final ` (@owner)` splits off as the owner; other parenthesized text stays part of the stage name. A missing or unreadable board shows no stage, even when the team declares a pipeline. [`flip`](#flip-the-board-to-the-next-stage) writes this line, and a hand-edited board parses the same way. The stage is advisory: RimZ never infers it from member status, and it is not proof that work finished.

PR and CI facts come from the room's sidebar cache. `rimz teams` and `show` never contact the forge, so `pr none` means nothing is cached, not that no PR exists. Before the room publishes its first snapshot, live cohorts still appear without PR facts or activity.

### JSON report

`rimz teams --json` prints an array of team records, `show <team> --json` one record, and lane-only `show --json` an array (empty when no team is live there). Fields marked optional are omitted when unset; the rest are always present, with `null` where a value is unavailable.

| Field | Meaning |
| --- | --- |
| `name`, `defined`, `valid` | Team name; whether a definition exists; whether it resolves and validates. |
| `source`, `layout`, `leader`, `error` | Optional. Definition file (`built-in` for `peer`), layout spec, effective leader role, validation error. |
| `consensus` | Staged teams only: `"builtin"` or the absolute replacement-file path. RimZ keeps a [read-only copy](../definitions.md#the-built-in-consensus-copy) of the built-in text on disk. |
| `append_system_prompt_files` | Team-level files composed after the consensus, as absolute paths. Omitted when empty. |
| `roles[]` | `role`, `profile`, and `signals`, plus optional `kind`, `model`, `effort`, `mode`, `system_prompt_file`, and `append_system_prompt_files`. |
| `roles[].append_system_prompt_files` | The role's own resolved profile-chain and role fragments, excluding the team layer. Omitted when empty. |
| `roles[].signals[]` | Declared bindings: `signal`, `match` (an object), and `prompt` (nullable). |
| `instances[]` | One record per live cohort, below. |

| Instance field | Meaning |
| --- | --- |
| `channel`, `state` | Lane name without `#`, and the [cohort state](#cohort-state). |
| `status_counts` | Object mapping each member status to its count. |
| `last_activity_at` | The newest member `last_activity_at` (UTC timestamp). |
| `worktree`, `branch` | Absolute checkout path and branch; `null` when unavailable or when members disagree. |
| `isolation`, `tmp_dir` | `host` or `sandbox`; `/tmp` for host, the absolute room tmp directory mounted at `/tmp` for sandbox. |
| `stages` | Declared stage names in order; `[]` when undeclared or the team is no longer defined. |
| `stage` | `{ "name": "Implement", "owner": "coder", "since": "2026-09-15T08:33:23Z" }`; `null` when the board has no stage. `name` is `Done` once the board is flipped to `Done`, so `.stage.name` is the field to script against. `owner` is without `@` and nullable; `since` is the UTC time of the flip into this stage, `null` when no recorded flip matches. |
| `pr` | `number`, `state` (`open`, `merged`, `closed`), `ci` (`passing`, `pending`, `failing`), and `url`, each nullable; `null` when nothing is cached. |
| `memory[]` | `path` (absolute), `lines`, and `modified_at` (UTC timestamp or `null`). |
| `members[]` | In declared role order: `role` (nullable), `handle`, `kind`, `status`, `phase`, `activity` (nullable), `last_activity_at` (UTC timestamp), `signals`, and optional `context_fill_pct` and `cost_usd`. |
| `members[].signals[]` | Armed subscriptions: `name` (the loop task name), `selector`, `matches`, and optional `fired`, the task's latest loop run: `at` (UTC timestamp), `result` (the snake_case run result, `delivered` for a delivered firing), and `runs`. These keys differ from the declared `roles[].signals[]`. |

## Launch a team

```sh
rimz teams forge -w feat-rate-limits "add rate limiting"
rimz teams forge#feat-rate-limits "add rate limiting"
rimz teams forge#feat-rate-limits --fresh
rimz teams forge --from-pr 91 --bg
rimz teams peer --channel triage
rimz teams launch forge -w feat-rate-limits
```

`rimz teams <team> [PROMPT]` and `rimz teams launch <team> [PROMPT]` launch a configured team and send the optional prompt to its leader: the `leader` role, else the first declared role. The team's tab opens with the leader's pane focused. Both run the same launch as `rimz agents <team>`: worktree creation, channel placement, pull-request checkout, and [reconciliation with an existing cohort](./agents.md#relaunch-into-a-named-worktree) (a live cohort is focused; a closed one prompts to resume, start fresh, or remove). A name that is not a defined team is refused with the list of configured teams, and `forge.reviewer` is refused with a pointer to `rimz agents forge.reviewer`. A team launched by an agent is a top-level cohort, not a child of that agent.

| Flag | Effect |
| --- | --- |
| `-w`, `--worktree [NAME]` | Reuse or create the RimZ-owned worktree `NAME`, on lane `#NAME`; bare `-w` generates a name. `team#NAME` is the same selection; give one or the other. Name rules and prompts are on [`rimz agents`](./agents.md#channel-worktree-and-placement). |
| `--channel NAME` | Launch into the durable named lane `NAME` in the room root. Conflicts with `-w` and `team#worktree`. |
| `--from-pr PR` | Create or reuse a worktree from a pull-request number or URL. |
| `--description TEXT` | Seed the member cards' description until the agents name their sessions. |
| `--resume` (alias `--continue`) | Resume the matching closed cohort instead of launching; the same as [`rimz teams resume`](#resume-a-team). |
| `--fresh` | Start new sessions in an existing named worktree instead of resuming its closed cohort, keeping the checkout and its files. Needs `-w NAME` or `team#NAME`. |
| `--budget AMOUNT[/day]` | Cap each member's spend for the session (`5`) or the local day (`20/day`). Each member is capped separately; there is no pooled team cap. See [`rimz budget`](./budget.md). |
| `--isolation host\|sandbox` | Run every member under this isolation instead of the machine's `agents.isolation`, recorded per member. |
| `--bg` | Keep focus where it is. |
| `--new-tab` | Open the launch in a new tab or tmux window. |

`--resume` accepts `--isolation host|sandbox`, replacing each member's recorded isolation while keeping its conversation. It refuses `PROMPT`: send it with `rimz message` after the session opens, or choose fresh at the named-worktree prompt. `--from-pr`, `--channel`, `--description`, and `--budget` still conflict with resume. `--fresh` conflicts with `--resume` and `--from-pr`. Launch flags without a team name are refused with `team launch options require a team name`.

A launch that opens new panes prints a receipt:

```text
launched forge in worktree #feat-rate-limits
  path      /repo-worktrees/feat-rate-limits
  board     blackboard.md
  prompt    → @planner  "add rate limiting"

  @planner   claude  fable        <- leader
  @coder     codex   gpt-6-astra
  @reviewer  claude  opus
  signals   ci.failed → @coder
```

The header names the lane as `in worktree #<lane>`, for a `--channel` lane too. `path` is the absolute checkout, and `board` is `blackboard.md` relative to it, whether or not the file exists yet. The `prompt` line appears only when you gave one: whitespace collapsed, quotes escaped, clipped to the terminal width. Member rows show handle, provider, and model (`-` when unset), and a `signals` line closes them when roles declare bindings. The receipt has no command hints and no JSON form.

The receipt records what was launched. Members start asynchronously and may not yet appear in `rimz teams show`, which is the source of live status. To block until the board reaches `Done`, run [`rimz teams wait forge#feat-rate-limits`](#wait-for-a-cohort-to-finish); the cohort is resolvable as soon as the launch command returns. To be told when the cohort next goes idle instead, arm a one-shot [signal subscription](./loop.md#signals):

```sh
rimz loop add team-idle --wait @me --signal team.idle --match instance=forge#feat-rate-limits --once
```

It fires on the next transition to idle only. Signals never replay, so a cohort that was already idle when you armed it does not wake you; check `show` as well. Idle does not mean the task is done.

### Signal bindings at launch

A role's declared `signals` become live subscriptions when its member registers: at launch, resume, restart, and when the role is re-added. Subagents do not inherit them. Ending, losing, or stopping the session removes its subscriptions, and signals missed in between are not replayed. The binding list is part of project trust.

A launch from the root checkout with no named worktree (`-w NAME` or `team#NAME`) and no `--from-pr` refuses any `ci.*` or `pr.*` binding that lacks `match.branch` or `match.path`, before it creates panes or worktrees, because RimZ watches CI only for worktree branches. Launch with `-w NAME`, launch from a linked worktree, or add an explicit match.

## Resume a team

```sh
rimz teams resume forge
rimz teams resume forge#feat-rate-limits
rimz teams resume forge -w --bg
```

`resume` reopens the newest closed cohort of the team, with the identity, directory, and lane recorded in the store and each role's launch settings from its current profile. `team#worktree` or `-w NAME` limits the match to that worktree. Bare `-w`, or no flag while you stand in a linked worktree, scopes to the current worktree; from the root checkout the newest match anywhere in the room wins. A matched member that is still live refuses the command. Matching rules are on [`rimz agents` → Resume a cohort](./agents.md#resume-a-cohort).

`--isolation host|sandbox` replaces each resumed member's recorded isolation; for example, `rimz teams resume forge --isolation host`. Omitting it preserves each member's recorded override, falling back to machine policy only when none is recorded. The replacement survives restart, rebirth, and child launches. For one-shot permission, model, or effort changes, use [`rimz agents forge --resume`](./agents.md#resume-a-cohort).

`--bg` keeps focus where it is. `resume` prints one line per member, `resumed <kind>:<name> (<session id>)` or `started fresh <member>` for a member with nothing to resume, followed by `Check:`, `Reach:`, and `Wait:` command hints for the cohort.

## Drive a live team

```sh
rimz teams focus forge
rimz teams restart forge
rimz teams stop forge#feat-rate-limits
rimz teams stop forge -w feat-rate-limits
```

| Verb | Does | Prints |
| --- | --- | --- |
| `focus` | Jumps to the member that most needs attention, else the declared `leader`, else the first member. | Nothing on success. |
| `stop` | Closes every live member, stopping each member's own subagents first. | `stopped @<handle>` per member it stopped, `error @<handle>: <reason>` per failure. |
| `restart` | Relaunches every live member in declared role order, resuming each provider session where supported. | One restart line per member, `error @<handle>: <reason>` per failure. |

`stop` and `restart` continue past a failed member and exit `1` when any member failed.

Each verb acts on one live cohort. Without a lane, RimZ takes the cohort in your current lane, or the only live cohort; when several are live elsewhere it refuses and lists their lanes. `team#worktree` or `-w NAME` selects by lane or worktree name. With no live cohort, the error suggests `rimz teams resume <team>`.

## Wait for a cohort to finish

```sh
rimz teams wait forge#feat-rate-limits
rimz teams wait forge -w feat-rate-limits --timeout 4h
rimz teams wait forge#feat-a forge#feat-b --json
rimz teams wait forge#feat-a forge#feat-b --any
```

`rimz teams wait <NAME>... [-w NAME] [--any] [--timeout DURATION] [--json]` blocks until each named cohort's board reaches the `Done` stage. Each `NAME` selects one live cohort the way the [drive verbs](#drive-a-live-team) do, once, when the command starts; `-w NAME` is accepted only with a single `NAME`. The team must be configured.

The wait reads the [board stage](#board-stage) every 500 ms and settles a cohort when:

| Outcome | When | Exit |
| --- | --- | --- |
| completed | The board's stage is exactly `Done` (`Done (delta)` is not). A board already at `Done` settles at once. | `0` |
| failed | Every member has ended while the board is not at `Done`. | `1` |

A missing board, or a member waiting on you or failed, keeps the wait blocked: only the board says the work is done. With several names the wait settles when all of them have, exiting `0` when all completed and otherwise with the code of the first name, in argument order, that did not. `--any` settles on the first cohort to finish and exits with its code. `--timeout` caps the whole wait and exits `124`, leaving the cohorts running. The wait writes nothing.

With one name, a completed wait prints the board's `## Result` section on stdout and nothing else; a board without one prints a note on stderr instead, and a failed wait prints `rimz: <team>#<lane>: cohort ended before Done (stage: <stage>)` on stderr. With several names, or `--any`, each cohort prints as it settles under a `--- <team>#<lane> ---` header, suffixed with its status when it did not complete; a timeout prints `--- <team>#<lane> (timed out) ---` on stderr for each cohort still pending.

`--json` prints one object for a single name, and an object keyed by `<team>#<lane>` for several once the wait ends:

```json
{"status": "completed", "exit": 0, "stage": "Done", "board": "/repo-worktrees/feat-rate-limits/blackboard.md", "result": "PR: https://github.com/acme/api/pull/42"}
```

`status` is `completed`, `failed`, or `timed_out`; `stage` is the board's stage when the wait ended (`null` without a board); `result` is the `## Result` section, `null` unless completed with one; a failed entry adds `error`.

## Flip the board to the next stage

```sh
rimz teams flip Explore "board opened; sweep aimed at rate-limit handling"
rimz teams flip Implement "plan ready in plan-notes.md; three advisories carried in"
rimz teams flip Review "implementation committed; report in implement-notes.md" --team forge
rimz teams flip Done "reflection recorded in reflect-notes.md; run complete"
```

`rimz teams flip <STAGE> <NOTE> [--team NAME]` sets the board's stage, records the note in its progress ledger, and wakes the stage's owner. `NOTE` is required for every stage, `Done` included, and must not be blank. Write it as where the work stands; it is not an instruction to the owner.

### Which cohort a flip acts on

`flip` has no worktree flag: run it inside the team's worktree.

1. The worktree is `RIMZ_WORKTREE_PATH` when set, else the Git toplevel of the current directory. Outside Git with no `RIMZ_WORKTREE_PATH`, the flip is refused.
2. The cohort is the live one whose members all run in that worktree, limited to team `NAME` by `--team`. With none, the error names the worktree. With cohorts of several teams, it lists them for `--team`. With several cohorts of one team, it asks you to stop the extras with `rimz teams stop <team> -w <lane>`.
3. A caller in the selected cohort flips as its role. A caller outside any cohort flips as `@user`. An agent in a different cohort is refused.

A project team that is not trusted is refused, as for any command that reads an untrusted project config.

### Stage rules

Stages come from the team definition ([configuration → teams](../../guide/configuration.md#teams)). `STAGE` must match a declared stage exactly and case-sensitively: a name in `stages`, or in the roles' `owns` lists when `stages` is empty. It must have an owning role. `Done` is always accepted, is implicit and last, and cannot be declared or owned. Put qualifiers such as "delta round" in the note. The command does not enforce pipeline order, and flipping out of `Done` reopens the work.

A member handing off needs a clean worktree. A hand-off is a member leaving a stage its role owns for one it does not own, `Done` included. When `git status --untracked-files=all` lists any path under the worktree, the flip is refused before the board changes and names the paths. The root `blackboard.md` never counts, and ignored files (including the patterns the team's `scratch-files` registers) never appear; other memory files count. User flips, same-stage re-fires, moves between stages the member owns, and flips made while another role owns the current stage skip the check, so a human can always force a hand-off.

### What a flip writes

The first flip creates `<worktree>/blackboard.md` when absent. Each flip, under a per-worktree lock:

1. Replaces the first line starting `Stage:` with `Stage: <stage> (@<owner>)`, or `Stage: Done`. With no such line, it inserts one after a leading `# ` heading, else at the top.
2. Appends a ledger line to the `## Progress` section (an existing `## Progress log` is used too), creating the section when absent: `- YYYY-MM-DD HH:MM:SS @by: from -> to — note`, or `opened <to>` instead of the transition when there was no stage. The time is in the configured local time zone with seconds, and a multiline note is joined onto one line. The sidebar's [run clock](../../interface/sidebar.md#worktree-headers) reads this ledger and still accepts older minute-form stamps.
3. Appends a durable `team.stage` signal and fires its subscriptions.
4. Delivers a `STAGE` notice to the owner, as described below.

Other board text is left as it is; the leader writes the rest of the board.

```text
- 2026-09-12 14:02 @planner: opened Explore — board opened; sweep aimed at rate-limit handling
- 2026-09-12 14:20 @planner: Plan -> Implement — plan ready in plan-notes.md; three advisories carried in
```

To undo a mistaken flip, flip back to the intended stage; both entries stay in the ledger. A flip to the current stage repeats the ledger line, signal, and delivery. The board and ledger mechanics are in [team memory and stages](../../internals/harness/teams.md).

### The receipt

```text
Flipped Plan -> Implement by @planner  (forge#feat-rate-limits · feat-rate-limits)
  Explore → Plan → [Implement] → Review → Submit → Reflect → Done
  note     plan ready in plan-notes.md; three advisories carried in
  owner    @coder, woken at its next turn boundary
  compact  queued for you: 204k tokens, over 180k
```

The header reads `Opened <stage>` on a board with no prior stage and names the flipper, the cohort, and the worktree directory. The pipeline line appears only when the team declares `stages`, and a multiline note prints on one line.

| `owner` row | Meaning |
| --- | --- |
| `@<owner>, sent now` | The owner was at rest and received the notice immediately. |
| `@<owner>, woken at its next turn boundary` | The notice is parked until the owner's turn ends. |
| `@<owner>, not live; woken on resume` | No live member holds the role. The board and signal still land. |
| `you, carry on` | You own the destination stage; no notice is sent. |
| (omitted) | The stage is `Done`; no notice is sent. |

The `compact` row appears only when [flip compaction](#flip-compaction) acted: `queued for you: <n>k tokens, over <threshold>k`, `sent`, or `skipped: <reason>`.

### The owner's notice

The owner receives a message with `Type: STAGE` and `From: @rimz`, delivered at its next `done` turn boundary; `flip` has no interrupt option. The body is prose:

```text
@planner flipped the stage Plan -> Implement. Implement is yours: pick it up from blackboard.md.

Note: plan ready in plan-notes.md; three advisories carried in
```

A board with no prior stage reads `@<by> opened the stage <stage>.`, and a same-stage flip reads `@<by> re-opened <stage>. It is still yours: pick it up from blackboard.md.` When the team declares a `leader` and the owner is another role, the body ends with that seat's channel rule:

```text
Your report goes in your stage file and anything for the user to @planner; end the turn with the flip and no pane text.
```

When the current stage's owner registers after resume, restart, a single-member restart, or room rebirth, RimZ re-wakes it without touching the board or ledger: a `team.stage` signal with `from` equal to `to` and `by` set to `rimz`, and this notice:

```text
The team resumed at stage Implement, which is yours. Nothing flipped since the board's last Progress line: reread blackboard.md and continue from where it stops.
```

Explicit `team.stage` subscriptions receive the loop's signal message independently of the notice. `rimz transcript` shows each flip in the lane's conversation ([transcript](./transcript.md)); re-wakes are not shown.

### The team.stage signal

| Payload field | Value |
| --- | --- |
| `team` | Team name. |
| `instance` | `team#lane`. |
| `from` | Previous stage; absent when the flip opened the board. |
| `to` | Destination stage. |
| `owner` | Owner role; absent for `Done`. |
| `by` | Flipping role name, `user`, or `rimz` for a re-wake. |
| `note` | The flip's note; absent on a re-wake. |
| `board` | Absolute path of `blackboard.md`. |
| `at` | RFC 3339 time. |

The signal's source is `team`, and `rimz events emit` cannot produce it.

### Flip compaction

A flip can compact the flipper's own context once it hands off. Set `[harness] flip_compact = "180k"` for a machine default, or `flip-compact` on a role to override it (`"off"` disables it for that role); both accept token counts or percentages such as `"70%"`, and unset means off. The fields are documented in [configuration → profiles](../../guide/configuration.md#profiles).

Only a hand-off (the same predicate as the clean-worktree rule) to a stage another role owns is eligible; a flip to `Done` never compacts, because no member works after it. RimZ checks the member's occupied context against the threshold first; below it, or with occupancy unknown, nothing happens and nothing is recorded. At or above it, RimZ queues or sends the compact command for the flipper's next turn boundary. A missing pane or a compact-command error prints `skipped` and never fails the flip. Each attempt writes a `flip compaction` assist record shown in `rimz stats`. A launch refuses a team whose effective threshold falls on an adapter with no compact command; set `flip-compact = "off"` on that role.

### When a flip fails

| Error | Fix |
| --- | --- |
| ``team `<team>` has no stage owners`` | Add `owns` to the team's roles. |
| ``unknown stage `<stage>`; choose one of [...] or Done`` | Use an exact declared name. |
| ``stage `<stage>` has no owner`` | Add the stage to a role's `owns`. |
| `worktree <path> has uncommitted changes: <paths>` | Commit or discard the listed paths, then flip again. |
| `no matching live team cohort in worktree <path>` | Run from the team's worktree. |
| `multiple live team cohorts in worktree <path>; select a team with --team <name>` | Add `--team`. |
| `calling agent belongs to a different team cohort` | Flip from a member of the selected cohort, or from outside every cohort. |
| ``completed <steps>; <error>; re-run `rimz teams flip <stage> "<progress note>"` to repeat the signal and delivery`` | The board changed but appending the signal or delivering the notice failed. Re-run the flip rather than editing the board by hand. |

A failure to fire subscriptions after the signal is appended is logged and does not fail the flip. Launch and flip also reject invalid stage configuration (blank or padded names, control characters, duplicate owners, a declared or owned `Done`, owned names missing from a nonempty `stages`); the rules are in [configuration → teams](../../guide/configuration.md#teams).

## Install a team bundle

```sh
rimz teams install
rimz teams install forge
rimz teams install forge --force
rimz teams install forge --ref main
```

Bare `install` lists the bundles under `examples/teams/` in the RimZ GitHub repository as a `TEAM REF` table. `install <name>` fetches `<name>.md` and the agent definitions selected by its roster, writing `<agents_home>/teams/<name>.md` and `<agents_home>/agents/<agent>.md`. The bundle's kind bases (`agents/claude.md`, `agents/codex.md`) are written only where the machine has none; an existing base is never replaced, `--force` included. The default agents home is `~/.rimz`, which `RIMZ_HOME` relocates; `RIMZ_AGENTS_HOME` overrides only the definition trees and skill library.

| Flag | Effect |
| --- | --- |
| `--ref TAG\|BRANCH` | Fetch from this tag or branch. The default is the release tag of the running binary, `v<version>`, so the bundle matches the command. When that tag is missing (a development build), the error suggests `--ref main`. |
| `--force` | Overwrite existing target definition files. Unrelated files stay. Requires a name. |

Without `--force`, any existing target is refused. Each file is written atomically. Network, GitHub API, parsing, and filesystem failures stop the install and name the failing source or destination. Run `rimz agents validate` after installation to check the resulting definitions and skill library.

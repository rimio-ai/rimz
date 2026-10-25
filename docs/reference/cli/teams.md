# Teams

`rimz teams` lists, inspects, launches, resumes, and drives named teams, hands a team's board from stage to stage, and installs team bundles.

Bare `rimz teams` exits 2 with the list command first, followed by the same orientation as `rimz teams --help`: See (`list show wait`), Drive (`focus stop restart flip record`), Start (`launch resume profiles`), and Define (`install`). Launch flags live under `rimz teams launch --help`.

A positional word that is neither a command nor a configured team exits 2 and suggests up to three names at the closest distance within two edits (an adjacent transposition counts as one), verbs first. Without a close match it lists commands and configured teams. For example, `rimz teams pee` suggests `rimz teams peer`. A lone prompt matching a teams command or alias is also refused with exit 2: `rimz teams forge stop` suggests `rimz teams stop forge`. To send the literal prompt instead, use `rimz teams launch forge "stop"`. Explicit `launch` keeps exit 1 for an unknown team and suggests only teams.

A team is a configured set of roles and a layout. Each role keeps its own model, prompt, and `@role` handle, and every member of one launch shares one lane; that group of live members is a cohort, addressed as `team#lane` (for example `forge#feat-rate-limits`). The [teams guide](../../guide/teams.md) explains how to design and run a team, [configuration → teams](../../guide/configuration.md#teams) covers where the definition files live, and the [definition reference](../definitions.md#teams-and-seats) owns every field (`roles`, `leader`, `layout`, `stages`, `scratch-files`, `consensus-file`, `append-system-prompt-files`, and each role's `owns`, `signals`, and `flip-compact`). This page owns the command forms.

| Command | Does |
| --- | --- |
| `rimz teams list`, `ls` | [List teams](#list-teams) and their live cohorts. |
| `rimz teams show`, `inspect` | [Inspect one team](#inspect-one-team), or every team live in a lane. |
| `rimz teams profiles` | [List team role profiles](#list-team-role-profiles). |
| `rimz teams <team>`, `launch` | [Launch a team](#launch-a-team). |
| `rimz teams resume` | [Resume a team](#resume-a-team)'s closed cohort. |
| `rimz teams focus`, `stop`, `restart` | [Drive a live team](#drive-a-live-team). |
| `rimz teams wait` | [Wait for a cohort to finish](#wait-for-a-cohort-to-finish). |
| `rimz teams flip` | [Flip the board to the next stage](#flip-the-board-to-the-next-stage). |
| `rimz teams record` | [Record an entry on the board](#record-an-entry-on-the-board). |
| `rimz teams install` | [Install a team bundle](#install-a-team-bundle). |

`rimz teams` sets where a cohort runs, whether it resumes, and what each member may spend. Per-agent shaping (model, effort, prompts, permission mode, name, pane placement, supervised runs) stays on [`rimz agents`](./agents.md), which also launches one role of a team as `rimz agents forge.reviewer`. Every form takes the [global flags](../cli.md#global-flags).

## List teams

```sh
rimz teams list
rimz teams ls --json
```

Bare `rimz teams`, with no verb and no team name (an empty name counts as none), refuses with exit 2 and prints the `list` command to run. `list` and `ls` open with cohort counts and a `needs you` block using the same attention rule and age columns as [`agents list`](./agents.md#list-and-manage-agents). A missing stage owner adds a `not live` attention row unless the board is at `Done`; it does not change the cohort's state. With no recorded flip, that row's age falls back to the newest member activity. Definitions with no live cohort collapse into a trailing `no live cohort: …` line.

| Column | Live cohort |
| --- | --- |
| `TEAM` | Team name. |
| `LANE` | `#<lane>`. |
| `STAGE` | The board stage and owner, with `not live` for an absent owner, followed by the time since its recorded flip. No age when no flip is recorded. |
| `STATUS` | The cohort [state](#cohort-state), followed by the time since the newest member activity. |
| `PR` | `#<number>` from the room's cached PR facts, followed by the CI glyph unless the PR is closed; `-` when no pull request is cached. |

Cohorts needing attention sort first, then blocked, paused, idle, working, and sleeping cohorts; `Done` cohorts sort last. Within each group, the longest time since member activity comes first. The summary omits zero counts and counts attention rows, not blocked cohorts, as `need(s) you`.

A broken definition keeps a row at the top, with `STATUS` replaced by `broken: <error>`. A live cohort whose team is no longer defined keeps its state with ` · not defined` appended. Resolved roles and models appear in [`show`](#inspect-one-team).

The catalogue merges machine [Markdown definitions](../definitions.md) under `<agents_home>/teams/<name>.md` with the repository's `.rimz/config.toml` when trusted. Invalid definitions fail with their source errors; unknown frontmatter keys are errors, not ignored settings. Run `rimz agents validate` to inspect the complete library. The built-in roleless `peer` team remains unless replaced. With no user teams, the listing includes an install hint.

`--json` prints an array of [team records](#json-report). `--json` works only on the list forms and `show`: `rimz teams forge --json` is refused with a pointer to `rimz teams show forge --json`.

## List team role profiles

Tiered role listings and `show` rosters display the tier beside the concrete model and mark a fallback. JSON adds `tier` and `tier_fallback` only for tiered roles; `model` remains the resolved provider model.

On a fresh launch, `--model M` runs that exact model for every Markdown role, normally on M's family's render, without tier routing or a stamp. Pi roles keep Pi and do not require M's native-family render; `--agent pi` also runs the model on Pi. For other engines, if that family cannot build a role, the launch refuses with the role, model, family and build error. A conflicting `--agent` family refuses too. Use `--tier` to allow routing, including across families on a Pi role or when a role declares a concrete `model:`.

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
| `<team>#<lane>`, or `<team> -w <lane>` | The lane block of the cohort whose lane is `<lane>` or whose members run in worktree `<lane>`, then the definition. With no match it exits 1 and names the live lanes, or the resume command when none are live. A root-checkout cohort launched without `--channel` has lane `<dir>/<team>`; `main`, the project directory's name, and the root path select none, and a miss under any of them prints as `#main`, with a resume command that names `<team>#<dir>/<team>`. |
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
  isolation: host · shared ~/.rimz/ws/repo-3f2a/shared
  memory:    .rimz/team/blackboard.md   41 lines · 2m ago
             .rimz/team/plan-notes.md  120 lines · 18m ago

forge
  source: ~/.rimz/teams/forge.md
  layout: planner,coder+reviewer

  @planner    claude  fable        <- leader
  @coder      codex   gpt-6-astra
  @reviewer   claude  opus
  signals   ci.failed → @coder
  (prompt stack: rimz teams show forge --json)
```

The team form, `rimz teams show forge`, describes the team and lists its cohorts:

```text
forge
  source: ~/.rimz/teams/forge.md
  layout: planner,coder+reviewer

  @planner    claude  fable        <- leader
  @coder      codex   gpt-6-astra
  @reviewer   claude  opus
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
| `pr` | The cached pull request: number, state (`open`, `merged`, `closed`), its `ci passing`, `ci pending`, or `ci failing` verdict, and URL, as far as known; `none` when no pull request is cached. A trunk lane has no pull request, so it reads `none` however its branch CI stands; that verdict shows on the [sidebar](../../interface/sidebar.md). |
| Member table | `MEMBER STATUS AGE ACTIVITY CTX COST`, members in the team's declared role order. Absent declared roles show `not live` and dashes. `STATUS` uses the [agent status words](./agents.md#list-and-manage-agents), `AGE` is the time since the member's last activity, and `COST` is each role's lifetime spend in this worktree. |
| `signals` | One line per subscription RimZ armed for a member from its role bindings: `<signal> → @<member> · <fire> · <loop task name>`. `<fire>` is `fired <age> ago` when the last fire delivered, or the [loop run result](./loop.md#read-run-history) with its age; ` ×<n>` gives the fire count once it has fired more than once. A subscription that has not fired reads `heard <signal> <age> ago` when it has heard another signal of its family, otherwise `never fired`. Heard signals are never counted as fires. Omitted when nothing is armed. The roster's `signals` line shows what is declared; this one shows what is armed now and whether it fired. |
| `worktree` | The members' absolute checkout path, with ` · branch <name>` when the branch differs from the directory name; `-` when members disagree. |
| `isolation` | `host · shared <dir>`, `scoped · shared <dir>`, or `sandbox · shared <dir>`: the room's shared directory (`$RIMZ_SHARED`, the same host path in all isolation levels), and where each member ran: the isolation its launch enforced, else the `--isolation` recorded at launch or replaced on resume, else the machine's `agents.isolation`. The enforced isolation already folds in a profile `isolation` default. Members that disagree show the machine setting. |
| `memory` | A staged team's Markdown files under `.rimz/team/` (the board, the stage notes, and any other `.md` a seat keeps there), relative to the checkout, with line counts and modification ages. Extra `scratch-files` excludes are not listed. Omitted when none exist. |

When the stage owner is absent, its stage label reads `Implement (@coder, not live) for <age>`.

A script that blocks until the work is done runs [`rimz teams wait`](#wait-for-a-cohort-to-finish) rather than polling `.stage.name` from `--json`. The report ends with the definition, with no launch, resume, or focus hints.

The definition includes a `(prompt stack: rimz teams show <team> --json)` hint when roles have prompt files or the team has a consensus.

Roles with a resolved [keep-warm](../definitions.md#keep-warm) policy also show their `max` and `while` values. These resolve each field from the role, then the team, with `off` cancelling inheritance.

### Cohort state

A cohort uses the sidebar's shared unit verdict. Attention from any member takes precedence, including the sidebar's attention lift when a current child of either origin waits or a provider-native child fails. A staged board at `Done` gives `done`, including without an owner. Otherwise the owner's projected status decides the calm state; without a usable owner, the first matching member state wins:

| State | When |
| --- | --- |
| `blocked` | Any member is `waiting` or `failed`. |
| `paused` | Any member is `paused`. |
| `working` | The stage owner is `running`, or any member is `running` without a usable owner. In-flight waits and listed background shells count as running. |
| `sleeping` | The stage owner is `sleeping`, or any member is `sleeping` without a usable owner and none is running. Sleeping means resting on a timer, signal, condition, or file watch, including through a child. |
| `idle` | The stage owner is `idle` or `success` before `Done`, or an ownerless team's members are all `idle` or `success` without a board at `Done`. |
| `done` | The board is at `Done`. |

With no projected rows, the same ownerless rule uses the live members' display statuses. An all-resting stage past the [stall window](../../guide/configuration.md#sidebar-rendering) makes its owner `failed`, so the cohort reads `blocked`.

### Board stage

The stage comes from the first line starting `Stage:` in `<worktree>/.rimz/team/blackboard.md`, for example `Stage: Implement (@coder)`. Only a final ` (@owner)` splits off as the owner; other parenthesized text stays part of the stage name. A missing or unreadable board shows no stage, even when the team declares a pipeline. [`flip`](#flip-the-board-to-the-next-stage) writes this line, and a hand-edited board parses the same way. The stage is advisory: RimZ never infers it from member status, and it is not proof that work finished.

PR and CI facts come from the room's sidebar cache. `rimz teams list` and `show` never contact the forge, so `pr none` means no pull request is cached, not that no PR exists. Before the room publishes its first snapshot, live cohorts still appear without PR facts or activity.

### JSON report

`rimz teams list --json` prints an array of team records, `show <team> --json` one record, and lane-only `show --json` an array (empty when no team is live there). Fields marked optional are omitted when unset; the rest are always present, with `null` where a value is unavailable.

| Field | Meaning |
| --- | --- |
| `name`, `defined`, `valid` | Team name; whether a definition exists; whether it resolves and validates. |
| `source`, `layout`, `leader`, `error` | Optional. Definition file (`built-in` for `peer`), layout spec, effective leader role, validation error. |
| `consensus` | Staged teams only: `"builtin"` or the absolute replacement-file path. RimZ keeps a [read-only copy](../definitions.md#the-built-in-consensus-copy) of the built-in text on disk. |
| `append_system_prompt_files` | Team-level files composed after the consensus, as absolute paths. Omitted when empty. |
| `roles[]` | `role`, `profile`, and `signals`, plus optional `kind`, `model`, `effort`, `tier`, `tier_fallback`, `mode`, `system_prompt_file`, and `append_system_prompt_files`. |
| `roles[].append_system_prompt_files` | The role's own resolved profile-chain and role fragments, excluding the team layer. Omitted when empty. |
| `roles[].keep_warm` | Resolved `{ "max": "<duration>" or null, "while": "<condition>" or null }`. Omitted when both are unset. |
| `roles[].signals[]` | Declared bindings: `signal`, `match` (an object), and `prompt` (nullable). |
| `instances[]` | One record per live cohort, below. |

| Instance field | Meaning |
| --- | --- |
| `channel`, `state` | Lane name without `#`, and the [cohort state](#cohort-state). |
| `status_counts` | Object mapping each member status to its count. |
| `last_activity_at` | The newest member `last_activity_at` (UTC timestamp). |
| `worktree`, `branch` | Absolute checkout path and branch; `null` when unavailable or when members disagree. |
| `isolation`, `shared_dir` | `host`, `scoped`, or `sandbox`; the absolute host path of the room's shared directory. |
| `stages` | Declared stage names in order; `[]` when undeclared or the team is no longer defined. |
| `stage` | `{ "name": "Implement", "owner": "coder", "owner_live": true, "since": "2026-09-15T08:33:23Z" }`; `null` when the board has no stage. `name` is `Done` once the board is flipped to `Done`, so `.stage.name` is the field to script against. `owner` is without `@` and nullable; `owner_live` is `true` or `false` by the same role-holder rule as flip delivery, and `null` when no owner is named. `since` is the UTC time of the flip into this stage, `null` when no recorded flip matches. |
| `pr` | `number`, `state` (`open`, `merged`, `closed`), `ci` (`passing`, `pending`, `failing`), and `url`, each nullable; `null` when no pull request is cached. |
| `memory[]` | `path` (absolute), `lines`, and `modified_at` (UTC timestamp or `null`). |
| `members[]` | In declared role order: `role` (nullable), `handle`, `kind`, `status`, `phase`, `activity` (nullable), `last_activity_at` (UTC timestamp), `signals`, and optional `context_fill_pct` and `cost_usd`. |
| `members[].signals[]` | Armed subscriptions: `name` (the loop task name), `selector`, `matches`, optional `fired`, the task's latest fire: `at` (UTC timestamp), `result` (the snake_case run result, `delivered` for a delivered firing), and `runs` (the fire count), and optional `heard`, the newest sibling signal heard without firing: `signal` and `at`. A subscription that only heard siblings has `heard` and no `fired`. These keys differ from the declared `roles[].signals[]`. |

## Launch a team

In both the bare and `launch` forms, `PROMPT` may start with `-`; a value that is itself one of the command's flags (`--isolation`, `--isolation=…`, `-h`, `--help`, etc.) needs `--` before it. A prompt starting with `-w` is refused, even after `--`, to avoid mistaking an attached worktree option for a prompt; use `-w NAME` with a space.

```sh
rimz teams forge -w feat-rate-limits "add rate limiting"
rimz teams forge#feat-rate-limits "add rate limiting"
rimz teams forge#feat-rate-limits --fresh
rimz teams forge --from-pr 91 --bg
rimz teams peer --channel triage
rimz teams launch forge -w feat-rate-limits
```

A team launched from an agent's pane starts in that agent's own checkout unless a worktree is selected. If the selected account has not trusted the launch directory, the launch refuses with the grant command to ask the user to run. Human team launches warn on an undecided folder; saved-untrusted folders refuse for either caller.

A checkout holding another team (a live member, or team history with a board stage other than `Done`) [refuses a fresh launch](../../internals/harness/fleet.md#one-team-per-checkout); `rimz teams flip <team>#<lane> Done "<note>"` releases a stopped run without removing its checkout. A channel held at a different checkout [also refuses a fresh launch](../../internals/harness/fleet.md#one-team-per-channel), even for the same team, whether the channel is explicit or derived from the checkout.

`rimz teams <team> [PROMPT]` and `rimz teams launch <team> [PROMPT]` launch a configured team and send the optional prompt to its leader: the `leader` role, else the first declared role. The team's tab selects the leader's pane internally; whether the client enters it follows the [agent launch focus rule](./agents.md#channel-worktree-and-placement). Both run the same launch as `rimz agents <team>`: worktree creation, channel placement, pull-request checkout, and [reconciliation with an existing cohort](./agents.md#relaunch-into-a-named-worktree). A live cohort receives the prompt through its leader, or is just focused when no prompt is supplied; an agent caller or `--bg` preserves focus. A missing leader refuses with exit 1 and commands to message live members. A closed cohort prompts to resume, start fresh, or remove; cancellation and non-terminal refusals exit 1 with prompt-preserving next commands. A name that is not a defined team is refused with the list of configured teams, and `forge.reviewer` is refused with a pointer to `rimz agents forge.reviewer`. A team launched by an agent is a top-level cohort, not a child of that agent.

| Flag | Effect |
| --- | --- |
| `-w`, `--worktree [NAME]` | Create the RimZ-owned worktree `NAME`, or check before entering an existing one, on lane `#NAME`; bare `-w` generates a name. New trees branch from the remote default branch, fetched at launch, unless `agents.worktree.base` selects `head` or a literal ref. A configured `fresh` also fetches. The fetch never prompts and gives up after 60 seconds, so an ssh origin whose key needs a passphrase with no agent running waits out that budget. A failed fetch warns and uses the last-fetched tip, or local `HEAD` if none exists; with no `origin`, an unset base uses local `HEAD`. Reuse and `--from-pr` do not fetch. `team#NAME` is the same selection; give one or the other. Name rules and prompts are on [`rimz agents`](./agents.md#channel-worktree-and-placement). |
| `--channel NAME` | Launch into the durable named lane `NAME` in the selected checkout. Conflicts with `-w` and `team#worktree`. |
| `--from-pr PR` | Create or reuse a worktree from a pull-request number or URL. |
| `--description TEXT` | Seed the member cards' description until the agents name their sessions. |
| `--continue` | Resume the matching closed cohort instead of launching; the same as [`rimz teams resume`](#resume-a-team). |
| `--fresh` | Start new sessions in an existing named worktree without asking to reuse it or resuming its closed cohort, keeping the checkout and its files. Needs `-w NAME` or `team#NAME`. |
| `--budget AMOUNT[/day]` | Cap each member's spend for the session (`5`) or the local day (`20/day`). Each member is capped separately; there is no pooled team cap. See [`rimz budget`](./budget.md). |
| `--isolation host\|scoped\|sandbox` | Run every member under this isolation instead of the machine's `agents.isolation`, recorded per member. |
| `--bg` | Keep focus where it is, as agent callers always do. See the [focus rule](./agents.md#channel-worktree-and-placement). |
| `--new-tab` | Open the launch in a new tab or tmux window. |
| `--detach` | Launch the prompt without a [team report](#team-reports) and without holding the launching agent. Refused without a prompt and with `--continue`. |

`--continue` accepts `--isolation host|scoped|sandbox`, replacing each member's recorded isolation while keeping its conversation, and queues `PROMPT` to the leader after reopening. For model, effort, agent-base, tier, or permission overrides, use the `resume` verb below. `--from-pr`, `--channel`, `--description`, and `--budget` still conflict with resume. `--fresh` conflicts with `--continue` and `--from-pr`. Without `--continue` or `--fresh`, a launch into a named worktree whose cohort has closed offers the choice only at a terminal: run unattended (stdin not a terminal), it prints the resume and fresh commands to stderr and exits 1 without launching. An existing unoccupied tree with no matching cohort also asks before entry, or refuses unattended with the facts and fresh command; see the [entry table](./agents.md#relaunch-into-a-named-worktree). This applies to every team launch form, including `team#NAME`. Launch flags without a team name are refused with `team launch options require a team name`.

A launch that opens new panes prints a receipt:

```text
launched forge in worktree #feat-rate-limits
  path      /repo-worktrees/feat-rate-limits
  board     .rimz/team/blackboard.md
  prompt    → @planner  "add rate limiting"

  @planner (stable-haven)   claude  fable        <- leader
  @coder (round-binder)     codex   gpt-6-astra
  @reviewer (quiet-otter)   claude  opus
  signals   ci.failed → @coder
```

The header names the lane as `in worktree #<lane>`, for a `--channel` lane too. `path` is the absolute checkout, and `board` is `.rimz/team/blackboard.md` relative to it, whether or not the file exists yet. The `prompt` line appears only when you gave one: whitespace collapsed, quotes escaped, clipped to the terminal width. Member rows show the handle `rimz agents list` gives the member, its pet name beside it when the two differ, then provider and model (`-` when unset), and a `signals` line closes them when roles declare bindings. The receipt has no command hints and no JSON form; an agent's prompted launch adds the [team report](#team-reports) lines.

A member moved by tier routing adds its tier and every passed entry with a reason after its model. Each fresh move also prints one stderr warning with the role, tier, chosen model and every reason, including full build errors. Unmoved rows stay unchanged. Inspect a member's recorded passes with [`rimz agents show`](./agents.md#show--inspect); resumed sessions print no new routing warning.

The receipt records what was launched. Members start asynchronously and may not yet appear in `rimz teams show`, which is the source of live status. To block until the board reaches `Done`, run [`rimz teams wait forge#feat-rate-limits`](#wait-for-a-cohort-to-finish); the cohort is resolvable as soon as the launch command returns. To be told when the cohort next goes idle instead, arm a one-shot [signal subscription](./loop.md#signals):

```sh
rimz loop add team-idle --wait @me --signal team.idle --match instance=forge#feat-rate-limits --once
```

It fires on the next transition to idle only. Signals never replay, so a cohort that was already idle when you armed it does not wake you; check `show` as well. Idle does not mean the task is done.

### Signal bindings at launch

A role's declared `signals` become live subscriptions when its member registers: at launch, resume, restart, and when the role is re-added. Subagents do not inherit them. Ending, losing, or stopping the session removes its subscriptions, and signals missed in between are not replayed. The binding list is part of project trust.

A launch from the root checkout with no named worktree (`-w NAME` or `team#NAME`) and no `--from-pr` refuses any `ci.*` or `pr.*` binding that lacks `match.branch` or `match.path`, before it creates panes or worktrees: the root checkout requires an explicit scope. Launch with `-w NAME`, launch from a linked worktree, or add an explicit branch or path match. `match.branch = "*"` accepts every checkout RimZ watches; see [signal matching](./loop.md#signals).

## Resume a team

```sh
rimz teams resume forge
rimz teams resume forge#feat-rate-limits
rimz teams resume forge -w --bg
```

`resume` reopens the newest closed cohort of the team, with the identity, directory, and lane recorded in the store and each role's saved launch record over its current profile. `team#lane` or `-w NAME` selects by the recorded lane, just like the drive verbs: a named-worktree cohort's lane is its worktree name, and a branch-style name such as `feat/rate-limits` also matches the dashed lane `feat-rate-limits`. Root-checkout lanes (`<dir>/forge`, where `<dir>` is the project directory's name) and explicit `--channel` lanes use the same rule, without requiring a git repository or a valid worktree name. Bare `-w`, or no flag while you stand in a linked worktree, scopes to the current worktree; from the root checkout the newest match anywhere in the room wins. A matched member that is still live refuses the command; a successful stop that returned without naming a still-running member leaves none live. If no member matches but the team's board still holds a checkout in that scope, `resume` reopens the newest such checkout and its channel with every member started fresh, continuing the unfinished board. This includes teams stopped before any member registered a conversation. Spec-matching rules are on [`rimz agents` → Continue a cohort](./agents.md#continue-a-cohort).

`--model MODEL`, `--effort LEVEL`, `--fast` or `--no-fast`, `--agent PROFILE|KIND`, `--tier TIER`, `--ask` or `--yolo`, and `--isolation host|scoped|sandbox` become the saved launch settings, with the same provider and tier restrictions as [`rimz agents forge --continue`](./agents.md#continue-a-cohort). A positional `PROMPT` is queued to the team's leader as a user message after reopening. Omitting isolation preserves each member's recorded override; a member with none recorded takes its profile's `isolation` default, then machine policy. The replacement survives restart, rebirth, and child launches.

`--bg` and agent callers keep focus where it is under the [focus rule](./agents.md#channel-worktree-and-placement). `resume` prints one line per member, `resumed <kind>:<name> (<session id>)` or `started fresh <member>` for a member with nothing to resume, followed by `Check:`, `Reach:`, and `Wait:` command hints for the cohort.

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
| `stop` | Closes every live member, stopping each member's own subagents first, then waits up to 3 seconds total for their processes to exit. | `stopped TEAM#LANE (N members)`, then `stopped @<handle>` per member it stopped, `error @<handle>: <reason>` per failure, and an address-and-pid line for each closed agent whose process is still running. |
| `restart` | Relaunches every live member in declared role order, resuming each provider session where supported. | `restarted TEAM#LANE (N members)`, then one restart line per member, `error @<handle>: <reason>` per failure. |

`stop` and `restart` continue past a failed member and exit `1` when any member failed. A member whose account fails the [account check](./agents.md#the-account-check-at-launch) is one such `restart` failure; a team launch and `rimz teams resume` refuse whole on it.

On return from a successful stop, each closed agent's process is no longer live, or stdout names its address and pid and says a resume refuses until it exits; a still-running process does not change the exit status. A pending board keeps an existing checkout held. The receipt names its stage and prints the addressed resume and `flip ... Done` release commands, but omits the resume command while any member's process is still running. Stopping does not finish the board: until it reaches `Done`, agent-exit cleanup, `worktree sweep`, and `gc` keep a RimZ-owned checkout ([worktree sweep](./worktree.md#sweep-landed-worktrees)), while an explicit `worktree remove` still removes it once no agent process is live.

Each verb acts on one live cohort. Without a lane, RimZ takes the cohort in your current lane, or the only live cohort; when several are live elsewhere it refuses and lists their lanes. `team#lane` or `-w NAME` selects by the recorded lane, which for a named-worktree cohort is its worktree name and for a root-checkout cohort launched without `--channel` is `<dir>/<team>`, where `<dir>` is the project directory's name (`forge#api/forge`); `main` does not select such a cohort. Branch-style names such as `feat/rate-limits` also match the dashed lane `feat-rate-limits`. A recorded lane takes precedence over the checkout's name. With no live cohort, the error suggests `rimz teams resume <team>`.

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

A missing board, or a member waiting on you or failed, keeps the wait blocked: only the board says the work is done. With several names the wait settles when all of them have, exiting `0` when all completed and otherwise with the code of the first name, in argument order, that did not. `--any` settles on the first cohort to finish and exits with its code. `--timeout` caps the whole wait and exits `124`, leaving the cohorts running. The wait changes no durable state.

With one name, a completed wait prints the board's `## Result` section on stdout and nothing else; a board without one prints a note on stderr instead, and a failed wait prints `rimz: <team>#<lane>: cohort ended before Done (stage: <stage>)` on stderr. With several names, or `--any`, each cohort prints as it settles under a `--- <team>#<lane> ---` header, suffixed with its status when it did not complete.

In both human and `--json` modes, stderr reports each cohort's stage, owner, and state at the start, then changes to the stage, owner availability, or set of members needing you. Progress uses the published sidebar verdict, like `teams list`; completion still reads the board directly. A missing owner reads `@<owner> not live`, with no focus command, since focusing the cohort does not reach an absent owner; attention changes include a `rimz teams focus` command. Age changes alone print nothing. These diagnostics do not end the wait when a person is needed. On timeout, each pending cohort gets a line naming the elapsed limit and stage, ending `cohort left running`.

`--json` prints one object for a single name, and an object keyed by `<team>#<lane>` for several once the wait ends:

```json
{"status": "completed", "exit": 0, "stage": "Done", "board": "/repo-worktrees/feat-rate-limits/.rimz/team/blackboard.md", "result": "PR: https://github.com/acme/api/pull/42"}
```

`status` is `completed`, `failed`, or `timed_out`; `stage` is the board's stage when the wait ended (`null` without a board); `result` is the `## Result` section, `null` unless completed with one; a failed entry adds `error`.

## Team reports

When an agent launches a team with a nonempty prompt, that agent gets one `TEAM_REPORT` message from `@rimz` each time [`rimz teams flip Done`](#flip-the-board-to-the-next-stage) moves the board into `Done`. It reports the prompted leader the way a [fleet report](./subagents.md#the-fleet-report) row reports one agent:

```text
Type: TEAM_REPORT
From: @rimz
Content:
Team forge#feat-rate-limits reached Done; its leader reports:
- @planner: completed in 2h14m, task: "Add rate limiting to the public API", response: /home/me/.rimz/ws/myrepo-f89e/out/boss/planner.<run_id>.output (~1.2k tokens, 31 lines)
Memory: /home/me/.rimz/ws/myrepo-f89e/team-notes/<run>/blackboard.md
```

| Part | Meaning |
| --- | --- |
| Elapsed | From the launch, or from the flip that last left `Done`, to this `Done` |
| Task | The launch `--description`, falling back to the first line of the launch prompt |
| Response | The leader's last final message, as a file the launcher can read, with its size; `no response` when the leader never finished a turn |
| Memory | The archived run's absolute board path; the checkout's `.rimz/team/blackboard.md` spelling if the link no longer resolves |

The launching agent's receipt names the leader's response file and `rimz teams wait <team>#<lane>`, and, unless the launch is detached, `rimz teams stop <team>#<lane>` for once the report arrives. The report is parked and delivered at the launcher's next turn boundary. The flip receipt adds a `report` line naming the launcher. A flip out of `Done` prints `report   TEAM_REPORT again at the next Done`, and the next `Done` sends a new report.

A team launched with `--detach` sends no `TEAM_REPORT`. Its run still follows the board: `Done` and cohort death settle it and write the leader's response file, a flip out of `Done` opens the next detached stretch, and each flip receipt prints `report   none: the team was launched detached` in place of the report line. The launcher is not held open or shown as waiting for it; `rimz teams wait` still blocks on `Done`.

The seats do not report their own turns, including turns the launcher opens with `rimz message`, and they add nothing to the launcher's fleet report. When every member ends before `Done`, the launcher receives one failure `TEAM_REPORT` with the last board stage and the leader's last response; if the board already reads `Done` at settlement, it receives the completed report instead. No report is sent when you launched the team yourself, when the launch had no prompt, when the leader's provider lacks native turn hooks (the launch receipt says so), after stop, restart, or room recovery settled the run, when the launcher has ended, or when the board is edited to `Done` by hand while the cohort stays live. Resume opens no new run, so a cohort resumed after death settlement reports nothing at its next `Done`.

## Record an entry on the board

`rimz teams record [TEAM#LANE] <SECTION> [TEXT] [--file PATH] [--stdin]` appends one timestamped entry to `.rimz/team/blackboard.md`. SECTION is Goal, Decisions, or Result, case-insensitive. Supply exactly one of TEXT, `--file PATH`, or `--stdin`; files and stdin are read to EOF. Text loses carriage returns and surrounding whitespace, but keeps internal blank lines. TEXT may start with `-`; only a value that is itself one of the command's flags (`--file`, `--stdin`, `-h`, `--help`) needs `--` before it, as in `rimz teams record Decisions -- --stdin`.

An address such as `forge#feat-rate-limits` selects the live cohort, or the stopped team holding that lane, just as `flip` does. Without it, the command uses `RIMZ_WORKTREE_PATH` when set, otherwise the nearest directory from the current one up to the Git toplevel whose `.rimz/team` link resolves, else the Git toplevel. No live cohort is required and there is no `--team` flag, but `.rimz/team` must already link a run: relaunch or resume the team to repair a missing or dangling link. `record` and `flip` never create a run. A member of the selected cohort records under its role; other callers record as `user`. A member of another cohort is refused. The linked manifest must name the writing team (or, for an unscoped human entry, an owning team). A foreign or unowned run refuses with its owner and link: stop the writing team, then resume it to relink its own run.

Entries have the following shape (stamp in the configured machine time zone):

```text
- 2026-09-26T15:20:59+02:00 @planner: first line
  second line

  a paragraph after a blank line
```

Continuation lines are indented two spaces. The command appends after the section's last nonblank line. A missing section is inserted in Goal, Decisions, Progress, Result order, before the first later section present or at EOF. A missing or empty board gets this template, with the entry under the selected heading:

```markdown
# Blackboard

## Goal

## Decisions

## Progress

## Result
```

There is no Stage line until a flip. Success exits 0 and prints one line naming the section and author, such as `recorded Decisions @planner` or `recorded Goal @user`; the entry itself is not echoed. The section prints in its canonical capitalization whatever the input casing. A TEXT argument inside double quotes is expanded by the shell first, so pass text holding backticks or `$` through `--stdin` with a quoted heredoc (`<<'EOF'`). Recording does not flip the stage, send messages, compact agents, or require a clean worktree or trusted team definition. To amend or remove an existing line, edit the board directly; direct edits take no board lock.

| Error | Result |
| --- | --- |
| Stage, Progress, or Progress log | Exit 1: names `rimz teams flip` as the owning command. |
| Unknown section | Exit 1: lists Goal, Decisions, Result. |
| Blank text | Exit 1: `provide nonblank text for the board entry`. |
| Missing or multiple text sources | Exit 2: supply exactly one text source. |
| Caller belongs to a cohort elsewhere | Exit 1: `calling agent belongs to a team cohort in another worktree`. |
| No worktree, unreadable input or board, lock timeout, write failure | Error with the underlying failure. |

## Flip the board to the next stage

```sh
rimz teams flip Explore "board opened; sweep aimed at rate-limit handling"
rimz teams flip Implement "plan ready in .rimz/team/plan-notes.md; three advisories carried in"
rimz teams flip Review "implementation committed; report in .rimz/team/implement-notes.md" --team forge
rimz teams flip Done "reflection recorded in .rimz/team/reflect-notes.md; run complete"
```

`rimz teams flip [TEAM#LANE] <STAGE> <NOTE> [--team NAME]` sets the board's stage, records the note in its progress ledger, and wakes the stage's owner. `NOTE` is required for every stage, `Done` included, and must not be blank. Write it as where the work stands; it is not an instruction to the owner. `NOTE` may start with `-`, as in `rimz teams flip Review "--no-tests: docs only"`; only a note that is itself one of the command's flags (`--team`, `--team=…`, `-h`, `--help`) needs `--` before it.

### Which cohort a flip acts on

`flip` has no worktree flag. Give `TEAM#LANE` to select a cohort from elsewhere in the room, or run it inside the team's worktree. The `#` distinguishes an address from a stage.

1. With `TEAM#LANE`, the cohort supplies its recorded worktree. Without it, the worktree is `RIMZ_WORKTREE_PATH` when set, else the nearest directory from the current one up to the Git toplevel whose `.rimz/team` link resolves, so a `--cwd` team's subdirectory finds its own board, else the Git toplevel. Outside Git with neither an address nor `RIMZ_WORKTREE_PATH`, the flip is refused.
2. Select the live cohort in that lane or checkout, otherwise the team holding it under the launch hold rules. `LANE` matches as it does for `stop`, `focus`, and `restart`: the lane or its worktree name, and `<dir>/<team>` for a root-checkout cohort. `--team NAME` filters the current checkout and cannot be combined with `TEAM#LANE`. With several live cohorts, the error lists the choices.
3. A caller in the selected cohort flips as its role. A caller outside any cohort flips as `@user`. An agent in a different cohort is refused.

A project team that is not trusted is refused, as for any command that reads an untrusted project config.

With no live members, a user can still flip the held board. The receipt says `no live members`; a `Done` flip that ends the board's hold adds `checkout released`, and one made while a recorded member still holds the checkout does not. Release ends the board's hold, not the checkout itself: no worktree is removed. Afterwards, automatic cleanup or `gc` may reclaim a clean, landed RimZ-owned checkout. Other stages name the absent owner as `not live; woken on resume`.

### Stage rules

Stages come from the team definition ([configuration → teams](../../guide/configuration.md#teams)). `STAGE` must match a declared stage exactly and case-sensitively: a name in `stages`, or in the roles' `owns` lists when `stages` is empty. It must have an owning role. `Done` is always accepted, is implicit and last, and cannot be declared or owned. Put qualifiers such as "delta round" in the note. The command does not enforce pipeline order, and flipping out of `Done` reopens the work.

A member handing off needs a clean worktree. A hand-off is a member leaving a stage its role owns for one it does not own, `Done` included. When `git status --untracked-files=all` lists any path under the worktree, the flip is refused before the board changes and names the paths. The `.rimz/team` archive link never counts, even without its Git exclude line. Ignored extras never appear; other modified or untracked files still count. User flips, same-stage re-fires, moves between stages the member owns, and flips made while another role owns the current stage skip the check, so a human can always force a hand-off.

### What a flip writes

Within an existing linked run, the first flip creates `<worktree>/.rimz/team/blackboard.md` when absent. Each flip, under a per-worktree lock:

1. Replaces the first line starting `Stage:` with `Stage: <stage> (@<owner>)`, or `Stage: Done`. With no such line, it inserts one after a leading `# ` heading, else at the top.
2. Appends a ledger line to the `## Progress` section (an existing `## Progress log` is used too), creating the section when absent: `- YYYY-MM-DDTHH:MM:SS±HH:MM @by: from -> to — note`, or `opened <to>` instead of the transition when there was no stage. The time is RFC 3339 with its UTC offset, written in the configured time zone, and a multiline note is joined onto one line. The sidebar's [run clock](../../interface/sidebar.md#worktree-headers) reads this ledger and still accepts older stamps without an offset (seconds or minute form), read in the configured zone.
3. Appends a durable `team.stage` signal and fires its subscriptions.
4. Delivers a `STAGE` notice to the owner, as described below.

Other board text is left as it is; the leader writes the rest of the board.

```text
- 2026-09-12T14:02:11+02:00 @planner: opened Explore — board opened; sweep aimed at rate-limit handling
- 2026-09-12T14:20:36+02:00 @planner: Plan -> Implement — plan ready in .rimz/team/plan-notes.md; three advisories carried in
```

To undo a mistaken flip, flip back to the intended stage; both entries stay in the ledger. A flip to the current stage repeats the ledger line, signal, and delivery. The board and ledger mechanics are in [team memory and stages](../../internals/harness/teams.md).

### The receipt

```text
Flipped Plan -> Implement by @planner  (forge#feat-rate-limits · feat-rate-limits)
  Explore → Plan → [Implement] → Review → Submit → Reflect → Done
  owner    @coder, woken at its next turn boundary
  compact  queued for you: 204k tokens, over 180k
```

The header reads `Opened <stage>` on a board with no prior stage and names the flipper, the cohort, and the worktree directory. The pipeline line appears only when the team declares `stages`. The note is not echoed; it lands in the board's ledger line.

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
@planner flipped the stage Plan -> Implement. Implement is yours: pick it up from .rimz/team/blackboard.md.

Note: plan ready in .rimz/team/plan-notes.md; three advisories carried in
```

A board with no prior stage reads `@<by> opened the stage <stage>.`, and a same-stage flip reads `@<by> re-opened <stage>. It is still yours: pick it up from .rimz/team/blackboard.md.` When the team declares a `leader` and the owner is another role, the body ends with that seat's channel rule:

```text
Your report goes in your stage file and anything for the user to @planner; end the turn with the flip and no pane text.
```

When the current stage's owner is resumed (by a team resume, a restart, a single-member restart, or room rebirth), RimZ re-wakes it without waiting for a prompt and without touching the board or ledger. An owner launched fresh into an open stage is re-woken when it registers instead ([resume re-wake](../../internals/harness/teams.md#resume-re-wake)). A re-wake is a `team.stage` signal with `from` equal to `to` and `by` set to `rimz`, and this notice:

```text
The team resumed at stage Implement, which is yours. Nothing flipped since the board's last Progress line: reread .rimz/team/blackboard.md and continue from where it stops.
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
| `board` | Canonical run-directory path of `blackboard.md`, retained after checkout removal. |
| `at` | RFC 3339 time. |

The signal's source is `team`, and `rimz events emit` cannot produce it.

### Flip compaction

A flip can compact the flipper's own context once it hands off. Set `[harness] flip_compact = "180k"` for a machine default, or `flip-compact` on a role to override it; both accept token counts, percentages such as `"70%"`, or `"off"`. An explicit role value wins, including over a machine `"off"`. With neither set, the built-in default is `120k` for a role owning `Plan`, `180k` otherwise. The machine default is documented in [configuration → hand-off compaction](../../guide/configuration.md#hand-off-compaction) and the role field in the [definition reference](../definitions.md#flip-compaction).

Only a hand-off (the same predicate as the clean-worktree rule) to a stage another role owns is eligible; a flip to `Done` never compacts, because no member works after it. RimZ checks the member's occupied context against the threshold first; below it, or with occupancy unknown, nothing happens and nothing is recorded. At or above it, RimZ queues or sends the compact command for the flipper's next turn boundary. A missing pane or a compact-command error prints `skipped` and never fails the flip. Each attempt writes a `flip compaction` assist record shown in `rimz stats`. A launch refuses a team whose effective threshold falls on an adapter with no compact command; set `flip-compact = "off"` on that role, or `[harness] flip_compact = "off"` when the role inherits the machine setting.

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
| ``completed <steps>; <error>; re-run `rimz teams flip <team>#<lane> <stage> "<progress note>"` to repeat the signal and delivery`` | The board changed but appending the signal or delivering the notice failed. Re-run the flip rather than editing the board by hand. |

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

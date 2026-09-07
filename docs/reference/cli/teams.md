# Teams

`rimz teams` discovers, inspects, installs, launches, resumes, and drives named teams.

A team is a configured set of role bindings and a layout.
Each role keeps its own model, prompt, context window, and address while the team shares one lane.
The definition may set `leader`, `layout`, `stages`, `scratch-files`, and `signals` alongside its `roles`. `stages` declares an optional ordered pipeline, such as `["Explore", "Plan", "Implement", "Review", "Submit", "Reflect"]`; names must be nonblank and unique, and an empty list means undeclared. `scratch-files` is a list of verbatim gitignore patterns for ephemeral team memory, registered on launch and resume.
The [teams guide](../../guide/teams.md) explains how to design a team; this page owns the command forms.

## List teams

```sh
rimz teams
rimz teams --json
rimz teams list
rimz teams ls --json
```

The bare command and `list`/`ls` merge the effective team definitions with live team instances. The columns are `TEAM LANE STAGE PR STATUS`, with one row per live cohort; `PR` includes the projected PR number and CI indicator when available. A definition with no live cohort gets one row with `-` for lane, stage, and PR, and `ready` or a definition error for status. Definition errors remain visible on live rows; a cohort whose definition was removed keeps its live status with `not defined` appended. Resolved roles, models, and effort stay in `show`.

The effective catalogue merges the machine `agents.toml`, fragments under `~/.agents/teams/`, and a trusted repository overlay.
An unreadable or invalid effective config fails at entry with the source error.
Unknown fields print a warning, are ignored, and can be removed with `rimz setup`.
`--json` emits the same catalogue as structured team records with definitions, resolved roles, validation, and live instances.

## Inspect one team

```sh
rimz teams show forge
rimz teams show forge#feat-rate-limits
rimz teams show forge -w feat-rate-limits
rimz teams inspect forge
rimz teams show forge --json
```

`show` and its `inspect` alias name the best-effort definition source, layout, leader, and validation result, then list each resolved role's profile or kind, model, effort, and mode.
When a role has system-prompt files, the human report points to `--json`, whose `system_prompt_file` and `append_system_prompt_files` fields expose the complete resolved stack.

Each live cohort gets its own block: lane and advisory board stage with optional owner; absolute worktree path and branch; declared stages; cached PR/CI facts and URL; and matching memory files with absolute paths, line counts, and modification ages. The member table is `MEMBER STATUS ACTIVITY CTX COST AGE`; `AGE` measures time since last activity, not the last heartbeat. Undeclared stages and empty memory scans omit their lines. If members disagree on the worktree or branch, that value is unavailable rather than chosen from an arbitrary member.

The current stage comes from the first `Stage:` line in `<worktree>/blackboard.md`, for example `Stage: Plan (@planner)`. A terminal ` (@owner)` suffix supplies the owner; other parenthesized text stays part of the stage name. This is advisory text maintained by the team, never inferred from member status and never proof of completion. The stages line brackets the declared name equal to the board stage's first whitespace-delimited word, case-sensitively; an unknown stage brackets nothing. A missing or unreadable board omits the header's stage suffix, even when a pipeline is declared.

PR/CI comes from the sidebar-refreshed cache; `teams` and `show` do not contact the forge. Only available facts are shown, and `pr none` means nothing is projected, not that RimZ verified there is no PR. Before a room snapshot is published, live cohorts can still be inspected without projected PR or activity enrichment.

Use `team#worktree` or `-w NAME` to narrow the live section by exact lane or member worktree; an ended or not-yet-live lane reports that no instance is live and still exits successfully.
`Declared signals` lists ROLE, SIGNAL, MATCH, and PROMPT from config even before launch. `Live signals` lists LANE, MEMBER, NAME, SIGNAL, and MATCH from materialized workspace rows whose team instance and pinned kind/session match that member. JSON exposes `roles[].signals` and `instances[].members[].role`/`signals`; an invalid definition stays visible with its validation error.

The report ends with copy-ready launch and resume forms when no instance is live, or lane-qualified reach and focus forms when exactly one live cohort is shown.

`rimz teams --json` emits an array of team records; `rimz teams show <team> --json` emits one team record. Both retain the definition, resolved `roles`, validation, and `instances` array. Each instance retains `channel`, `state`, `status_counts`, and `members` and adds:

| Field | Meaning |
| --- | --- |
| `worktree`, `branch` | Absolute checkout path and branch, or `null` when unavailable or conflicting. |
| `stages` | Ordered declared stage names; `[]` when undeclared or the definition is gone. |
| `stage` | `{ "name": "Plan", "owner": "planner" }`, with the owner stored without `@`; `owner` can be `null`, and absent board stage is `null`. |
| `pr` | `number`, `state`, `ci`, and `url`, each nullable; the whole value is `null` when no facts are projected. |
| `memory` | An array of `{ "path": …, "lines": …, "modified_at": … }` records with absolute paths and UTC timestamps; unavailable modification times are `null`. |

Each member also exposes `phase`, nullable `activity`, and `last_activity_at` as a UTC timestamp alongside its handle, kind, status, context fill, and cost. The new optional values serialize as `null`, and arrays remain present even when empty.

## Launch a team

```sh
rimz teams forge -w feat-rate-limits
rimz teams forge#feat-rate-limits "add rate limiting"
rimz teams forge -w feat-rate-limits "add rate limiting"
rimz teams forge#feat-rate-limits --fresh
rimz teams peer --channel triage
rimz teams forge --from-pr 91 --bg
rimz teams launch forge -w feat-rate-limits
```

The bare-name form and `launch` verb accept a configured team name and send an optional trailing prompt to its configured leader.
It uses the same launch and relaunch-reconciliation path as `rimz agents <team>`, including worktree creation, channel placement, pull-request checkout, and existing-cohort focus or recovery.
When an agent launches a team, its members are top-level peers rather than children of the caller.

After opening new panes, a fresh launch prints the team and lane, absolute worktree path and actual branch when available, declared stages when present, and the absolute `<worktree>/blackboard.md` path even if the board does not exist yet. Each member row shows its minted handle, provider, and resolved model (`-` when unset), marking the effective leader rather than printing `starting`. An optional `prompt` line names its recipient and echoes the supplied prompt, before reminders, with whitespace collapsed, quotes escaped, and text clipped to terminal width. The receipt records launch inputs, not confirmation that a provider received or acted on the prompt.

The receipt ends with these lane-qualified hints (shown here for `forge#feat-rate-limits`):

```text
Check: rimz teams show forge#feat-rate-limits
Reach: rimz message @planner#feat-rate-limits '<text>'
Wait:  rimz loop add --wake @me --signal team.idle --match instance=forge#feat-rate-limits --once
```

Startup remains asynchronous: the receipt is not a readiness barrier, and members may not yet appear in `teams show`. Inspect the cohort for live status. `Wait` arms a one-shot subscription on a future transition to `team.idle`; it does not block until readiness or completion, and idle does not mean the task is done. Signals do not replay: if the cohort was already idle before the subscription was armed, that transition will not wake you. Inspect current state as well as arming the subscription; [signal delivery](./loop.md#signal-triggers) applies. Launch has no JSON receipt; `--json` is for list and inspection.

For a configured binding the receipt also prints `signals: ci.failed → coder`; this describes intent, not an already-armed row. Root members arm their bindings when their real sessions register, including resume, restart, and role re-add; children do not. End, loss, or stop retires the session's subscriptions, and missed signals are never replayed.

A fresh launch on the root checkout refuses CI/PR bindings with no explicit `match.path` or `match.branch`, before creating panes or worktrees. Use `-w <worktree>`, launch from a linked worktree, or set an explicit match. The full ordered binding list enters project trust.

Members report their live status asynchronously, so `rimz teams show team#worktree` remains the source of truth rather than the receipt.

`rimz teams` sets where a cohort runs, whether it resumes, and what each member may spend.
`rimz agents` sets what an agent is — model, effort, prompts, permission posture, name, pane placement, supervised runs.

The team surface carries these cohort-level controls:

- `-w, --worktree [NAME]` creates or reuses a RimZ-owned worktree in the current Git repository; a bare `-w` chooses a fresh name. Spell the name without the channel's leading `#`, as `-w feat-rate-limits` for channel `#feat-rate-limits`: a quoted `-w '#feat-rate-limits'` is an invalid worktree name, and with shell comments enabled, an unquoted `#feat-rate-limits` is dropped as a comment, leaving a bare `-w` that generates a name. The fused `forge#feat-rate-limits` form remains valid; its `#` separates the team and worktree names. Cross-repository room launches use the same confirmation and `--root` rules as [`rimz agents`](./agents.md#channel-worktree-and-placement).
- `--channel NAME` launches in a durable named lane instead of a worktree.
- An existing user-owned linked worktree in the configured directory can also be entered with `-w NAME` after terminal confirmation (default no), without adoption, seeding, or automatic cleanup. It must belong to the launch repository; non-terminal launches refuse. See [`rimz agents`](./agents.md#channel-worktree-and-placement).
- `--from-pr PR` creates or reuses a worktree from a pull-request number or URL.
- `--description TEXT` seeds the member-card description until agents name their sessions.
- `--resume` reopens a matching closed cohort instead of launching a fresh one.
- `--fresh` launches new sessions into a named worktree instead of resuming or removing it, keeping the checkout and its files. It needs the worktree named, as `team#worktree` or `-w NAME`.
- `--budget AMOUNT[/day]` caps each member separately; it is not a pooled team cap.
- `--bg` leaves focus where it is.
- `--new-tab` opens the launch in a new tab or window.

Because resume takes identity from the store, it conflicts with `PROMPT`, `--from-pr`, `--channel`, `--description`, and `--budget`. `--fresh` answers the same reconciliation the other way, so it conflicts with `--resume` and `--from-pr`; the reconciliation it answers is described in [`rimz agents`](./agents.md#channel-worktree-and-placement).

Per-agent model, prompt-file, permission, supervised-run, and pane-placement overrides stay on [`rimz agents`](./agents.md).
Put stable role-specific choices in the team definition.

## Resume a team

```sh
rimz teams resume forge
rimz teams resume forge#feat-rate-limits
rimz teams resume forge -w feat-rate-limits
rimz teams resume forge -w --bg
```

`resume` reopens the newest matching closed cohort with the same identity, directory, and lane from durable state.
Current role profiles supply the launch configuration.
`team#worktree` and `-w NAME` limit selection to one worktree, while bare `-w` uses the current worktree.
Use either the fused form or `-w`, not both.
`--bg` leaves focus where it is.

## Drive a live team

```sh
rimz teams focus forge
rimz teams stop forge#feat-rate-limits
rimz teams restart forge
rimz teams stop forge -w feat-rate-limits
```

`focus` jumps to the selected cohort member that needs attention, falling back to the configured leader and then the first member.
`stop` closes every live member and reports one result per role.
`restart` relaunches every live member in declared role order, resuming its provider session where supported.

When one team has live cohorts in several lanes, RimZ prefers the cohort in the current lane.
From outside those lanes, select one with `team#worktree` or `-w NAME`; use either form, not both.

## Install a team bundle

```sh
rimz teams install
rimz teams install forge
rimz teams install forge --force
rimz teams install forge --ref main
```

The bare form lists bundles under `examples/teams/` in the RimZ GitHub repository.
The named form downloads every file in that bundle into `~/.agents/teams/<name>/`.
The default Git ref is the release tag matching the running binary, `v<CARGO_PKG_VERSION>`, so the examples and command stay version-aligned.
`--ref TAG|BRANCH` selects another tag or branch; development builds whose tag is unavailable report the `--ref main` recovery command.

An existing destination is preserved unless `--force` is present.
Bundle files use durable temp-file-plus-rename writes.
Network, API, validation, and filesystem failures stop the install with the failing URL or path and a recovery cue.

Use `rimz teams` for configured-cohort placement, resume, spend caps, and lifecycle control.
Use [`rimz agents`](./agents.md) for inline layouts, one role from a team, or per-agent launch shaping.

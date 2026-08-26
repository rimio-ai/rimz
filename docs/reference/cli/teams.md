# Teams

`rimz teams` discovers, inspects, installs, launches, resumes, and drives named teams.

A team is a configured set of role bindings and a layout.
Each role keeps its own model, prompt, context window, and address while the team shares one lane.
The definition may set `leader`, `layout`, and `scratch-files` alongside its `roles`; `scratch-files` is a list of verbatim gitignore patterns for ephemeral team memory, registered on launch and resume.
The [teams guide](../../guide/teams.md) explains how to design a team; this page owns the command forms.

## List teams

```sh
rimz teams
rimz teams --json
rimz teams list
rimz teams ls --json
```

The bare command and `list`/`ls` merge the effective team definitions with live team instances.
It shows the resolved role, model, and effort summary, each live lane and member count, and either the live state or the definition error.
Live instances remain visible when their definition has since been removed.

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

`show` and its `inspect` alias name the best-effort definition source, layout, leader, and validation result, then list each resolved role's profile or kind, model, effort, mode, and prompt files.
Live instances include the lane, member handle and status, context fill, and tracked session cost.
Use `team#worktree` or `-w NAME` to narrow the live section to one lane; an ended or not-yet-live lane reports that no instance is live and still exits successfully.
The report ends with copy-ready launch and resume forms.

## Launch a team

```sh
rimz teams forge
rimz teams launch forge
rimz teams launch forge#feat-rate-limits "add rate limiting"
rimz teams forge -w feat-rate-limits "add rate limiting"
rimz teams forge --channel triage
rimz teams forge --from-pr 91 --bg
```

The bare-name form and `launch` verb accept a configured team name and send an optional trailing prompt to its configured leader.
It uses the same launch and relaunch-reconciliation path as `rimz agents <team>`, including worktree creation, channel placement, pull-request checkout, and existing-cohort focus or recovery.
When an agent launches a team, its members are top-level peers rather than children of the caller.
After opening the panes, a fresh launch prints the minted member handles as `starting` and copy-ready `Check` and `Reach` commands.
Members report their live status asynchronously, so `rimz teams show team#worktree` remains the source of truth rather than the receipt.

`rimz teams` sets where a cohort runs, whether it resumes, and what each member may spend.
`rimz agents` sets what an agent is — model, effort, prompts, permission posture, name, pane placement, supervised runs.

The team surface carries these cohort-level controls:

- `-w, --worktree [NAME]` creates or reuses a RimZ-owned worktree in the current Git repository; a bare `-w` chooses a fresh name. Cross-repository room launches use the same confirmation and `--root` rules as [`rimz agents`](./agents.md#channel-worktree-and-placement).
- `--channel NAME` launches in a durable named lane instead of a worktree.
- `--from-pr PR` creates or reuses a worktree from a pull-request number or URL.
- `--description TEXT` seeds the member-card description until agents name their sessions.
- `--resume` reopens a matching closed cohort instead of launching a fresh one.
- `--budget AMOUNT[/day]` caps each member separately; it is not a pooled team cap.
- `--bg` leaves focus where it is.
- `--new-tab` opens the launch in a new tab or window.

Because resume takes identity from the store, it conflicts with `PROMPT`, `--from-pr`, `--channel`, `--description`, and `--budget`.

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

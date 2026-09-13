# Teams

`rimz teams` discovers, inspects, installs, launches, resumes, and drives named teams.

A team is a configured set of role bindings and a layout.
Each role keeps its own model, prompt, context window, and address while the team shares one lane.
The definition may set `leader`, `layout`, `stages`, and `scratch-files` alongside its `roles`; each role may declare its own `signals` array of inline tables, `owns` stage names, and `flip-compact` threshold override. `stages` declares an optional ordered pipeline, such as `["Explore", "Plan", "Implement", "Review", "Submit", "Reflect"]`; names must be nonblank and unique, and an empty list means undeclared. `scratch-files` is a list of verbatim gitignore patterns for ephemeral team memory, registered on launch and resume.
The [teams guide](../../guide/teams.md) explains how to design a team; this page owns the command forms.

## List teams

```sh
rimz teams
rimz teams --json
rimz teams list
rimz teams ls --json
```

The bare command and `list`/`ls` merge the effective team definitions with live team instances. The columns are `TEAM LANE STAGE PR STATUS`, with one row per live cohort; `PR` includes the projected PR number and CI indicator when available. A definition with no live cohort gets one row with `-` for lane, stage, and PR, and `ready` or a definition error for status. Definition errors remain visible on live rows; a cohort whose definition was removed keeps its live status with `not defined` appended. Resolved roles and models stay in `show`; effort is available in JSON.

The effective catalogue merges the machine `agents.toml`, fragments under `~/.agents/teams/`, and a trusted repository overlay.
An unreadable or invalid effective config fails at entry with the source error.
Unknown fields print a warning, are ignored, and can be removed with `rimz setup`.
`--json` emits the same catalogue as structured team records with definitions, resolved roles, validation, and live instances.

## Inspect one team

An instance's `state` follows member-status priority: `blocked` (any waiting or failed member), then `paused`, `working` (running), `sleeping`, `done` (success), and finally `idle`. Thus a resting member with an armed one-shot delivery keeps the cohort `sleeping` rather than `done`, unless a higher-priority member state wins. Standing subscriptions do not make members sleep. Any live member's pending one-shot wait withholds `team.idle`; cancellation is reevaluated on the caller's following lifecycle boundary, not immediately on row removal.

```sh
rimz teams show forge
rimz teams show forge#feat-rate-limits
rimz teams show forge -w feat-rate-limits
rimz teams show '#feat-rate-limits'
rimz teams show -w feat-rate-limits
rimz teams inspect forge
rimz teams show forge --json
```

`show` and its `inspect` alias name the best-effort definition `source` and `layout`, with an `error` line only when the definition is broken. The roster matches the launch receipt: each row shows a role handle, provider kind, and raw model ID, with `<- leader` marking the leader. When configured, a muted `signals` line closes the roster, showing bindings such as `ci.failed → @coder`, including match filters in parentheses. Both inspection and launch clip this line to terminal width; JSON inspection retains every binding and filter.
When a role has system-prompt files, the human report points to `--json`, whose `system_prompt_file` and `append_system_prompt_files` fields expose the complete resolved stack.

Each live cohort gets its own block headed by lane, cohort state, and advisory board stage with optional owner. The absolute worktree path appears once, with a branch suffix only when the branch differs from the checkout directory's name. The block also shows isolation, declared stages, cached PR/CI facts and URL, and matching memory files with paths relative to the worktree, line counts, and modification ages. The member table is `MEMBER STATUS ACTIVITY CTX COST AGE`; `AGE` measures time since last activity, not the last heartbeat. Undeclared stages and empty memory scans omit their lines. If members disagree on the worktree or branch, that value is unavailable rather than chosen from an arbitrary member.

The `isolation` line reports the room's current machine-wide `agents.isolation` setting, not a durable per-agent launch record. Host isolation shows `host · tmp /tmp`. Sandbox isolation shows the room's state tmp directory, home-relative where possible, with `(as /tmp)` indicating where it is mounted inside the sandbox.

The current stage comes from the first `Stage:` line in `<worktree>/blackboard.md`, for example `Stage: Plan (@planner)`. A terminal ` (@owner)` suffix supplies the owner; other parenthesized text stays part of the stage name. `flip` writes this advisory text from configured ownership; hand-edited boards still parse the same way. It is never inferred from member status and never proof of completion. The stages line includes implicit `Done` last and brackets the name matching the board stage exactly and case-sensitively; an unknown stage brackets nothing. A missing or unreadable board omits the header's stage suffix, even when a pipeline is declared.

PR/CI comes from the sidebar-refreshed cache; `teams` and `show` do not contact the forge. Only available facts are shown, and `pr none` means nothing is projected, not that RimZ verified there is no PR. Before a room snapshot is published, live cohorts can still be inspected without projected PR or activity enrichment.

Use `team#worktree` or a team name with `-w NAME` to narrow the live section by exact lane or member worktree; an ended or not-yet-live lane reports `no live instance in #lane` and still exits successfully. Use either the fused form or `-w`, not both. Without a team name, `show '#lane'` or `show -w lane` prints a full report for every team live in that lane. No matches prints `no live team in #lane` and exits successfully.
The roster's `signals` line describes configured bindings even before launch. `Live signals` lists LANE, MEMBER, NAME, SIGNAL, and MATCH from materialized workspace rows whose team instance and pinned kind/session match that member. JSON exposes `roles[].signals` and `instances[].members[].role`/`signals`; an invalid definition stays visible with its error.

The report has no trailing launch, resume, reach, or focus command hints.

`rimz teams --json` emits an array of team records; `rimz teams show <team> --json` emits one team record. Lane-only inspection (`show '#lane' --json` or `show -w lane --json`) emits an array, empty when no teams are live there. All forms retain the definition, resolved `roles`, validation, and `instances` array. `roles[]` keeps `profile`, `effort`, and `mode`, and `roles[].signals[]` keeps `prompt` and `match`, even though the human roster omits those launch details except match filters. Each instance retains `channel`, `state`, `status_counts`, and `members` and adds:

| Field | Meaning |
| --- | --- |
| `worktree`, `branch` | Absolute checkout path and branch, or `null` when unavailable or conflicting. |
| `isolation` | Current machine setting: `host` or `sandbox`, not a per-agent launch record. |
| `tmp_dir` | `/tmp` for host isolation; the absolute room state tmp directory mounted at `/tmp` for sandbox isolation. |
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

After opening new panes, a fresh launch prints `launched <team> in worktree #<channel>`, the absolute worktree `path`, and `board blackboard.md` relative to that path even if the board does not exist yet. Each member row shows its role handle, provider, and resolved model (`-` when unset), marking the effective leader with `<- leader`. Branch, declared stages, and command hints are omitted. An optional `prompt` line names its recipient and echoes the supplied prompt, before reminders, with whitespace collapsed, quotes escaped, and text clipped to terminal width. The receipt records launch inputs, not confirmation that a provider received or acted on the prompt.

Inspect, message, or subscribe to the cohort with lane-qualified commands (shown here for `forge#feat-rate-limits`):

```sh
rimz teams show forge#feat-rate-limits
rimz message @planner#feat-rate-limits '<text>'
rimz loop add team-idle --wait @me --signal team.idle --match instance=forge#feat-rate-limits --once
```

Startup remains asynchronous: the receipt is not a readiness barrier, and members may not yet appear in `teams show`. Inspect the cohort for live status. The `loop add` command above arms a one-shot subscription on a future transition to `team.idle`; it does not block until readiness or completion, and idle does not mean the task is done. Signals do not replay: if the cohort was already idle before the subscription was armed, that transition will not wait you. Inspect current state as well as arming the subscription; [signal delivery](./loop.md#signals) applies. Launch has no JSON receipt; `--json` is for list and inspection.

For configured bindings, a `signals` line closes the receipt's member list, for example `signals   ci.failed → @coder`; this describes intent, not an already-armed row. Root members arm their bindings when their real sessions register, including resume, restart, and role re-add; children do not. End, loss, or stop retires the session's subscriptions, and missed signals are never replayed.

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

## Flip the board to the next stage

```sh
rimz teams flip Explore "board opened; sweep aimed at rate-limit handling"
rimz teams flip Implement "plan ready in plan-notes.md; three advisories carried in"
rimz teams flip Review "implementation committed; report in implement-notes.md" --team forge
rimz teams flip Done "reflection recorded in reflect-notes.md; run complete"
```

`rimz teams flip <STAGE> <NOTE> [--team NAME]` records progress and hands the board to the configured owner. Both positional arguments are required, including the note for `Done`; empty or whitespace-only notes are refused. The note records what is done or where the work stands, not an instruction to the receiver.

Selection is worktree-based:

1. RimZ uses `RIMZ_WORKTREE_PATH` when set, otherwise the Git toplevel of the current directory, with lexical path normalization.
2. It selects the live cohort whose members record that worktree, limited by `--team` when supplied. No match reports the worktree; several teams reports their names so you can select one with `--team`. There is no worktree-selection flag: run the command in the intended worktree.
3. A calling member of another cohort is refused. A caller outside the selected cohort acts as `@user`; a selected member acts as its role.

Declare ownership on roles, for example `owns = ["Explore", "Plan", "Reflect"]` on the planner and `owns = ["Implement"]` on the coder. Stage names match exactly and case-sensitively against `stages`, or against the owned names when `stages` is empty. Put qualifiers such as “delta round” in the note, not in the stage name. The command does not enforce pipeline order or a Review gate.

The first flip creates `<worktree>/blackboard.md` if absent. Under a per-worktree lock, RimZ atomically replaces its first column-zero `Stage:` line with `Stage: <stage> (@owner)`. If the line is missing, it inserts it after a leading `# ` heading line, otherwise at the top. It appends the note to `## Progress`, creating the section if absent; existing `## Progress log` sections remain accepted. Other board text stays intact, and the leader writes the other sections. The ledger uses the configured local time zone:

```text
- 2026-09-12 14:02 @planner: opened Explore — board opened; sweep aimed at rate-limit handling
- 2026-09-12 14:20 @planner: Plan -> Implement — plan ready in plan-notes.md; three advisories carried in
```

After the board write, RimZ appends a durable `team.stage` signal with source `team`, fires explicit subscriptions, and directly delivers to the owner without requiring a role signal binding. The payload carries string fields `team`, `instance` (`team#channel`), `from` (absent when opening a board), `to`, `owner` (omitted for `Done`), `by` (role name, `user`, or `rimz`), optional `note`, `board` (absolute path), and `at` (RFC 3339). Flip signals always include the required note; registration re-wakes have no note. `rimz transcript` renders each flip inside the lane's conversation ([transcript](./transcript.md)).

Direct delivery uses `Type: STAGE` / `From: @rimz` with prose only, not payload JSON. These are the flip, same-stage re-fire, and registration re-wake bodies:

```text
@planner flipped the stage Plan -> Implement. Implement is yours: pick it up from blackboard.md.

Note: plan ready in plan-notes.md; three advisories carried in
```

```text
@user re-opened Implement. It is still yours: pick it up from blackboard.md.

Note: implementation paused after the first check
```

```text
The team resumed at stage Implement, which is yours. Nothing flipped since the board's last Progress line: reread blackboard.md and continue from where it stops.
```

If the leader opens a board at a stage another role owns, the first sentence instead starts `@planner opened the stage Explore. Explore is yours: pick it up from blackboard.md.` Explicit signal subscriptions still receive the loop's signal body, independently of this direct notice.

Delivery always parks at the owner's next done boundary; flip has no interrupt option. A flip to a stage the caller owns sends no message: carry on. An owner with no live member is not an error: the board and signal land, and the receipt says the owner will be woken on resume. When the current owner registers after resume, restart, single-member restart, or room rebirth, RimZ emits and delivers a re-wait with `from == to` and `by = "rimz"`, without editing the board or ledger.

`Done` is implicit and always last in the pipeline display; declaring it in either `stages` or `owns` is refused. It writes `Stage: Done`, appends the required note, and emits the signal without delivering a message. Flipping out of `Done` is allowed: keep the board and flip to an owned stage for a follow-up run. Same-stage flips repeat the ledger entry, signal, and eligible delivery. To correct a mistaken flip, flip back to the intended stage; both actions stay in the ledger.

The receipt names the flipper and cohort, brackets the current stage in the declared pipeline, and reports the note, owner delivery, and any compaction action. With no declared `stages`, it omits the pipeline strip. Multiline notes stay intact in the Stage notice and signal but collapse to one line on the board and receipt. For example:

```text
Flipped Plan -> Implement by @planner  (forge#teams-flip · teams-flip)
  Explore → Plan → [Implement] → Review → Submit → Reflect → Done
  note     plan ready in plan-notes.md; three advisories carried in
  owner    @coder, woken at its next turn boundary
  compact  queued for you: 204k tokens, over 180k
```

A first flip says `Opened <stage>`. The owner row can also say `sent now`, `not live; woken on resume`, or `you, carry on`; it is omitted for `Done`. The compact row appears only for sent, queued, or skipped attempts, not when unconfigured, ineligible, or below threshold.

No role declares `owns`? Add ownership before using `flip`. Unknown stage? Use an exact declared name; a declared but unowned stage needs an owner. Launch and flip reject blank stage names, surrounding whitespace or control characters, duplicate owners, explicit `Done`, or owned names missing from a nonempty `stages` list. Stage-owner role names must omit parentheses and control characters. A signal or delivery failure after the board write reports completed steps and exits nonzero; repeat `rimz teams flip <stage> "<progress note>"` to retry rather than undoing the board by hand.

Set `[harness] flip_compact = "180k"` to compact a flipper's own context at its next turn boundary once it reaches that threshold; unset means off. A role's `flip-compact = "220k"` overrides the default, and `flip-compact = "off"` disables it for that role. Thresholds accept token counts or percentages such as `"70%"`. Compaction requires a cohort member leaving a stage its role owns for one it does not own. `Done` counts as not owned, and a non-live destination owner does not prevent compaction. User flips, same-stage re-fires, moves between self-owned stages, and flips of another role's stage never compact. There is no first-leave exemption: every eligible flip checks current occupied context before pane availability. Below-threshold flips, including those with unknown occupancy, make no attempt and write no assist record.

Compaction is best-effort enrichment: an unavailable pane or compact-command error appears as `skipped` without failing the flip. Attempts append a `flip compaction` assist record and appear in `rimz stats`. Launch refuses an effective threshold on adapters without a compact command; set `flip-compact = "off"` on that role or choose a supported adapter. This is separate from native `auto-compact`, smart compaction of a message target, and idle compaction.

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

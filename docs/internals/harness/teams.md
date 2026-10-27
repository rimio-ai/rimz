# Team boards, stages, and reports

> How a staged team's shared files work: the run archive behind `.rimz/team`, the board and its `Stage:` line, `rimz teams record` and `rimz teams flip`, the owner re-wake on resume, and the `TEAM_REPORT` an agent-launched team owes its launcher. Launching a team is [fleet.md](./fleet.md). The `team.stage` signal and the `STAGE` message body are [loops.md § Team signals](./loops.md#team-signals). Users read the [teams guide](../../guide/teams.md) and [cli/teams.md](../../reference/cli/teams.md).

A team cooperates through files in its checkout. The members write the content. RimZ links the files to room state, reads the board's current stage, and owns three kinds of board write: registration of the link, a recorded Goal, Decisions, or Result entry, and the stage flip. Neither the board nor member status is lifecycle authority: a stage is advisory progress for the agents, the reports, and the displays, and a hand-edited `Stage:` line is read like any other.

A team is staged when it declares `stages` or any role `owns` a stage (`config::Team::staged`). An unstaged team gets no archive, no memory listing, and no board.

## Modules

| Path under `crates/rimz/src/` | Owns |
|---|---|
| `harness/team_notes.rs` | `register`: the dated run directory, its `run.json`, and the checkout link, before a launch or resume |
| `harness/scratch.rs` | Read side: link resolution (`run_dir`, `run_team`), board paths, the memory scan, `board_stage`, `board_section`, `board_run`, and `holding_team` |
| `harness/board.rs` | Write side: `LockedBoard` (canonical worktree, board lock, owner check, read), stamping, section insertion, `Stage:` rewriting, atomic publication, and `record` |
| `harness/team_stage.rs` | `flip`, the two re-wake producers (`rewake_resumed`, the registration branch of `react_lifecycle`), owner delivery, flip compaction, and `stage_flips` |
| `harness/run/team.rs` | The team run: `create_team_run`, `reopen_team_run`, `settle_team_run`, `rekey_team_run` |
| `cli/agents_cmd/team_report.rs` | `on_flip` and `settle_ended_teams`: composing, queueing, and settling the `TEAM_REPORT` |
| `cli/teams/board_context.rs` | Worktree and caller resolution shared by `flip` and `record` |
| `cli/teams/list.rs`, `cli/teams/show.rs`, `cli/teams/wait.rs` | The board's CLI readers |

## The run archive

A staged team's board and notes live in room state, under `team-notes/<run>/`, and the checkout reaches them through the symlink `.rimz/team`. Removing the checkout therefore keeps the notes, and retention and reset of the archive belong to the [store catalog](../store.md#what-is-on-disk). A `blackboard.md` at the checkout root is never read or moved into a run.

### Registration

`team_notes::register` runs before every launch that seats a staged team:

| Caller | Mode |
|---|---|
| A fresh team launch (`crates/rimz/src/launcher/mod.rs::settle_layout`) | `Fresh` |
| A cohort resume (`crates/rimz/src/launcher/mod.rs::launch_resume_layout`) | `Resume` |
| A restored team tab in lane restore or rebirth (`crates/rimz/src/harness/resume.rs::materialize_team_restore_tab`) | `Resume` |

It canonicalizes the checkout and takes the [board lock](#the-board-lock) before it inspects or replaces the link. An existing link counts only when it resolves to a directory directly inside this room's `team-notes/` and that directory's `run.json` names the same team. A missing, unreadable, or invalid manifest is never a match. Then the mode decides:

- `Fresh` reuses the linked run unless its board reads `Done`, and otherwise creates a new run. It never searches unlinked archives.
- `Resume` reuses the linked run whatever its stage. Without one, it relinks the newest run by `created_at` whose manifest names this team and this canonical checkout, and creates a run only when none does.

A new run is named `<UTC %Y%m%dT%H%M%SZ>-<team>-<checkout basename>`. Each name component keeps its first 80 characters, maps every character outside `[A-Za-z0-9_-]` to `-`, trims `-`, and falls back to `run`. A collision appends `-2`, `-3`, and so on. `run.json` is written once: `version`, `team`, `created_at`, `checkout`, and, for a RimZ worktree, its marker's `name`, `branch`, `base_branch`, `from_pr`, and `repo_root`. The link is then replaced atomically with the run's canonical absolute path. Replacing a link never changes the run it pointed at.

`register` refuses, with the path and the fix, in four cases (`RegisterErr`):

| Variant | Condition |
|---|---|
| `Occupied` | A real file or directory sits at `.rimz/team` |
| `SharedRimz` | The checkout's `.rimz` is itself a symlink: seed it through `.worktreeinclude` instead of `.worktreelink`, or make it a directory |
| `RimzHome` | The checkout's `.rimz` is the RimZ home (`disk::paths::holds_rimz_home`). A home nested below `.rimz` is allowed. |
| `Io`, `Atomic` | Creating the directory, the manifest, or the link failed |

A refusal stops that launch or that restored tab.

### Git excludes

Two pattern sets exist, and they never mix:

| Set | Source | Contents |
|---|---|---|
| Extra excludes | `Team::scratch_patterns`, from the definition's `scratch-files` | Declared patterns only, empty when unset |
| Memory patterns | `scratch::memory_patterns` | `.rimz/team/*.md` for a staged team, nothing otherwise |

Registration publishes the extra excludes plus `/.rimz/team`, and, when the checkout sits below its Git root, the glob-escaped root-relative path of the link. It hands them to the best-effort exclude publisher ([worktrees.md § Team scratch patterns](./worktrees.md#team-scratch-patterns)), so a failed exclude warns and never refuses. An unstaged team publishes only its extra excludes. Extra excludes and `run.json` never appear as memory.

## The memory listing

`scratch::scan(root, patterns)` resolves the memory patterns against the checkout root for three consumers: the member launch reminder, the [prompt-submit Environment block](./fleet.md#environment-at-prompt-submit), and the `teams list` and `teams show` instance, so all three see the same files. It strips a leading `/` before rooting each glob, matches files only, and returns absolute paths sorted and deduplicated across patterns. Each file carries its line count (zero when unreadable) and its modification time when metadata allows. An invalid glob or a failed traversal sets `probe_failed` and leaves the rest of the scan intact.

The agent-facing form is one fence from `launch_context::files_listing`: `$ ls .rimz/team/*.md`, then the matching names alone, two spaces apart, or `(no such files)`. A file behind the link is named without its directory, since the command line already spells it. Line counts are omitted, and an unstaged team gets no fence. A failed probe adds the line `RimZ could not inspect every pattern.` A launch carries the fence in exactly one place, its reminder's Environment section or the prompt-submit block, and in the prompt-submit block it comes before the Git state ([fleet.md § Launch reminders](./fleet.md#launch-reminders)).

`teams show` scans again on every call and prints each file's line count and modification age. Its human output shows paths relative to the worktree, and its JSON keeps them absolute.

## The board

The board is `blackboard.md` in the run directory, reached as `<checkout>/.rimz/team/blackboard.md` (`board::BOARD_FILE`, `scratch::board_path`). Its parts:

| Part | Rule |
|---|---|
| `Stage:` line | The first line starting `Stage:`. Only a terminal ` (@owner)` suffix splits off, and only when the owner is nonempty and holds no whitespace or parenthesis. `Stage: Implement (delta) (@coder)` is stage `Implement (delta)` owned by `coder`. |
| Section | The trimmed text under a `## <heading>` line, up to the next ATX heading outside a fenced code block. A blank section reads as absent. |
| Progress ledger | `## Progress`, or an existing `## Progress log`. Each flip appends one line. |
| Goal, Decisions, Result | Written by `rimz teams record` or by the members directly. |

`scratch::board_stage` and `scratch::board_section` read through the link wherever it points and treat a missing or unreadable board as having no stage. Writers are stricter: `board::LockedBoard::open` requires the link to resolve inside this room's archive and the manifest to name an owner (and, when the caller names a team, that team). A board can therefore be readable but refuse writes.

### The board lock

`RuntimePaths::board_lock` is one lock file per canonical worktree, under the shared runtime root (`board-write/<sha256 of the path>.lock`), so it serializes writers across rooms. Registration, `record`, `flip`, and both re-wake producers take it. `LockedBoard` holds it until it is dropped, so a flip keeps it through the signal append, owner delivery, and flip compaction, and releases it before the CLI settles the team report.

### Readers

| Reader | What it takes from the board |
|---|---|
| `teams list`, `teams show` (`cli/teams/list.rs`) | The stage and owner. The stage's age is the `at` of the newest `team.stage` flip in `stage_flips` for this instance whose destination is the board's stage. A flip rotated out of the active event log leaves the age unknown, and a hand edit back to a stage the cohort once flipped to shows that older flip's age. |
| `CohortState::for_cohort` (`cli/teams/list.rs`) | Only `done` reads the board: a Success headline maps to `done` only when the unit's board is at `Done` (below) |
| `rimz teams wait` (`cli/teams/wait.rs`) | `Done` settles the wait, and the `Result` section is the JSON `result` (below) |
| The sidebar's pipeline line | `scratch::board_run` folds the ledger into run and stage times ([sidebar.md § Team units and pipelines](../sidebar/sidebar.md#team-units-and-pipelines)) |
| The launcher's owed wake and `Team` pending wait | The stage of each open team run's worktree. `Done` ends both ([Launcher debt and the team wait](#launcher-debt-and-the-team-wait)) |
| `scratch::holding_team` | Automatic worktree removal ([worktrees.md § Protection facts](./worktrees.md#protection-facts)) |
| `resume::inspect_team_hold` | A fresh launch's one-team-per-checkout rule ([fleet.md § One team per checkout](./fleet.md#one-team-per-checkout)) |

`teams list` and `teams show` share one instance build. Beside the board stage it carries the pipeline (the team's declared `stages`, else the owned stages in role order, then the implicit `Done`), and each armed signal subscription's newest acting run-log record and newest heard signal ([loops.md § The display commands](./loops.md#the-display-commands)). Its PR and CI line comes from the published sidebar projection (the snapshot's `worktree_groups`) with no forge call of its own, so an absent PR line is not proof that no PR exists.

`CohortState::for_cohort` maps the published group's shared unit headline, matched by member row identity, to `blocked` (Waiting or Failed), `paused`, `working`, `sleeping`, `idle`, or `done`. A cohort with no published row uses `UnitStatus::from_members` without a pipeline. A Success headline without a `Done` board maps to `idle`, even though the cockpit and tab suffixes can show the turn verdict's ✓. Member `needs_you` details are diagnostic and never override the state.

`rimz teams wait` resolves each cohort once, then polls `board_stage` at its worktree every 500 ms. An exact `Done` settles it completed. Any other stage, or no board, reads the published snapshot for progress and liveness. When the cohort is gone from `address::team_cohorts`, the wait reads the board once more and settles completed on `Done` and failed (`cohort ended before Done`) otherwise, so a flip that races the members' exit is never reported as a failure. It never scans the event log or subscribes to `team.stage`: signals do not replay, and a blocking CLI cannot receive one.

### Ledger stamps

A ledger line is `- <stamp> @<by>: <from> -> <to> — <note>`, or `opened <to>` in place of the transition when the board had no stage. The stamp is RFC 3339 with its UTC offset, written in the machine's configured time zone (`board::stamp`). `scratch::board_run` also reads stamps written without an offset, in seconds or minute form, as times in the configured zone.

`board_run` starts the run at the first parseable entry and moves the start to each later flip out of `Done`. It records the end at the last flip into `Done`, and only while the board stage is `Done`. A `Done -> Done` re-flip moves neither. Malformed entries are skipped, and the note after ` — ` never supplies a transition.

## Recording an entry

`rimz teams record` calls `board::record` for Goal, Decisions, or Result. Progress and Stage refuse with `FlipSection`, since only a flip writes them. The text comes from the argument, `--file`, or stdin.

`board::record` normalizes the text (carriage returns removed, trimmed), refuses blank input, and writes `- <stamp> @<by>: <text>`, indenting each nonblank continuation line two spaces so it cannot become a heading or a `Stage:` line. Under the board lock it reads, inserts the entry after the last nonblank line of the section, and publishes atomically inside the run directory. A missing section is created in Goal, Decisions, Progress, Result order. A missing or empty board gets all four under `# Blackboard`, without a `Stage:` line. It emits no store record, signal, delivery, or compaction, and prints only `recorded <Section> @<by>`.

## Flipping a stage

`rimz teams flip [TEAM#LANE] <stage> "<note>"` resolves its target through `board_context::resolve`, the same as `record`:

- An explicit `TEAM#LANE` selects its live cohort, falling back to the audit-store hold (`resume::inspect_cohort_hold`).
- Without one, the worktree is `RIMZ_WORKTREE_PATH`, or else the nearest directory between the cwd and the Git root whose team link resolves (`nearest_team_checkout`). Its live cohort is selected, falling back to `resume::inspect_team_hold`.
- Several live cohorts of one team in the worktree refuse with a `rimz teams stop` hint. Cohorts of different teams refuse and ask for `--team`.

`flip` needs a live or held cohort. `record` without an address needs neither, only a linked run with a named owner: an unscoped human entry accepts that owner, and a member writes as its own team.

The caller is a member (`Flipper::Member`) when it belongs to the selected cohort, and the user (`Flipper::User`) otherwise. An agent from another cohort is refused. `flip` loads the team definition and checks its trust. `record` loads neither. A held cohort flips with no members, as the user. A flip to `Done` ends its board-stage hold (the receipt prints `checkout released`) without removing the checkout. Stop and restart have already settled such a cohort's team run, so the report step finds nothing to send.

`team_stage::flip` then runs six steps:

1. **Resolve the owner.** The team must have a role that owns a stage (`NoOwners`). The destination must be a declared pipeline stage or `Done` (`UnknownStage`), and a stage other than `Done` must have an owning role (`UnownedStage`).
2. **Lock and read.** `LockedBoard::open` takes the board lock and resolves the link to this room's run. A missing or foreign link refuses (`BoardErr::Unlinked`, `BoardErr::WrongTeam`) with the link, the owner, and the fix: stop and resume the writing team. A missing board inside a matching run reads as empty. The current stage is the flip's `from`.
3. **Check a hand-off.** A hand-off is a member leaving a stage its role owns for one it does not own, `Done` included. Before any write it requires `git status --porcelain=v1 --untracked-files=all` over the checkout, with `:(exclude).rimz/team`, to be empty, and refuses with the dirty paths (`DirtyWorktree`). The link is exempt even when exclude registration failed, and ignored files never appear. A worktree outside Git has nothing to commit and passes. Any other `git` failure, dubious ownership included, refuses (`GitStatus`).
4. **Rewrite the board.** The first `Stage:` line becomes `Stage: <to> (@<owner>)`, or `Stage: Done`. Without one, the line goes after a leading `# ` heading or at the top. The ledger line goes at the end of the Progress section, which is created when absent. Newlines in the note become spaces. The board is published atomically.
5. **Open the stage.** The flip appends the `team.stage` signal and fires its subscriptions, then delivers a `STAGE` message to the owner at the `done` boundary ([loops.md § Team signals](./loops.md#team-signals)). `Done` stops the shared language servers of the locked checkout instead (`lsp::registry::stop_checkout`), and a self-owned stage or an owner with no live seat sends nothing. A failed signal append or owner dispatch returns `PartialFlip`, which names what completed and prints the flip command to re-run. A failed subscription fire or language-server stop only logs.
6. **Compact the flipper.** On a member's hand-off to a stage another role owns, the flipper's pane gets its compaction command once its context reaches the threshold. The threshold is the role's `flip-compact`, else `[harness] flip_compact`, else the built-in default (`config::Team::flip_compact`). This is best-effort enrichment recorded in the assist log (`Assist::FlipCompact`), and it never fails the flip. A flip to `Done` never compacts, because no member works after it. [Idle compaction](./messaging.md#idle-compaction) is separate and reads no board.

After `flip` returns, the CLI settles the team report ([Team reports](#team-reports)). A report failure prints a `report` line and leaves the landed flip standing.

## Resume re-wake

A re-wake opens the board's current stage again for its owner: it appends `team.stage` with `from == to` and `by = "rimz"`, and delivers a `STAGE` notice telling the owner to continue from the board's last Progress line. It reads the board under the lock but never edits it, never checks the worktree, and never compacts. `stage_flips`, which `rimz transcript` renders, skips re-wakes.

`team_stage::with_rewake` holds the shared rule. It acts only for a root member whose role owns the board's current stage, and only when that stage is not `Done`. A missing or foreign link makes it a no-op. Two producers call it:

| Producer | When | Notices that already cover the member |
|---|---|---|
| `team_stage::rewake_resumed`, from the exec wrapper | Every `Resume` action that runs through `rimz agents exec`: cohort resume, lane restore, the partial-live lane split, `agents restart`, `teams restart`, and rebirth. It runs after the wrapper stamps `rimz.agent-resumed`, which revives the ended card ([model.md](../agents/model.md)), and binds the pane. The wrapper triggers queued-head delivery after the re-wake. | One still queued, or one delivered at or after the cutoff the wrapper takes just before the resume stamp, which precedes the provider |
| The registration branch of `team_stage::react_lifecycle` | A root member registers: a resume, a fresh launch into an open stage, or `/clear`, since a registration does not say which it is | One still queued, or one delivered within one prompt delivery window before the registration |

The wrapper is the trigger for providers whose definition sets `registers_lazily` (built-ins and plugins that declare `registers-lazily`), which emit no registration until their first turn, or never. A lazy provider's notice is typed as soon as the pane is bound, in the same paste as a prompt queued ahead of it. A non-lazy provider's notice is held as provider start pending until it registers or one delivery window passes. A fresh lazy launch into an open stage is re-woken only at its first registration.

The cutoffs are why both producers can run for one resume without a double notice. A non-lazy provider can register and take the reactor's notice before the wrapper's check runs, and the wrapper's pre-spawn cutoff counts that notice. A notice from an earlier attempt is older than the cutoff, so a member that dies again right after its notice is still re-woken. The reactor's window covers Codex registering in the same turn the typed notice started. Both checks run under the board lock, so a wrapper and a registration racing for one member cannot both enqueue.

The check reads the live queue and the message history together under the workspace lock (`Store::list_messages_and_history`). An acknowledgement moves a record from one to the other under that lock, so an ack landing mid-check cannot hide it. The history append is best-effort: when it fails, a delivered notice is in neither list and the next producer re-wakes once more. When the hook replays a lifecycle receipt after a crash (`team_stage::replay_lifecycle`), the registration branch delivers the notice without appending or firing the signal again.

## Team reports

An agent-launched team reports its leader to the launcher once per `Done`, with one `TEAM_REPORT` from `@rimz` (`HarnessNotice::TeamReport`). The report rides a team run: a `RunRecord` whose `team` field holds the leader's `launch_id` and the `<team>#<channel>` instance. [scripting.md](./scripting.md#completion-folding-lifecycle-observations) owns the record and its lifecycle fold.

`run::create_peer_prompt` opens a team run for the seat that receives the launch prompt, the run's leader, when an agent launched it, the prompt is nonempty, and the leader's adapter has native root turn hooks. A user launch, a promptless launch, and a resume open none. While open, the run takes `fold_team_lifecycle`, which keeps the leader's newest final message at each turn end, follows the leader onto a new conversation row, and never settles the run. A molt moves the open run to the successor (`rekey_team_run`). Team seats open no per-turn peer runs, so launcher messages to them report nothing.

### Settlement

| Event | Run | Message |
|---|---|---|
| A flip into `Done` from any other stage (`on_flip`) | Completed, with the report's message id | `TEAM_REPORT`, gate `Done`, to the launcher's current row |
| A flip out of `Done` | `reopen_team_run` opens a fresh run with the previous run's task, leader, reader, and report policy, but only when the cohort's newest run is terminal | None now. The next `Done` reports. |
| The cohort ends: no live members, or every member's recorded process dead (`fleet::ended_team_runs`) | Completed when the board reads `Done`, otherwise failed with `cohort ended before Done` | `TEAM_REPORT` as a success or a failure |
| `agents stop` or `teams stop` reaches the leader | Canceled, as the leader's newest run (`crates/rimz/src/cli/agents_cmd/stop.rs::stop_run`) | None |
| Restart or rebirth of the leader | Failed (`run::fail_peer_run`) | None |
| The launcher or the leader row is gone at settlement | Completed or failed by the board | None |

Only one flip sees a transition into `Done`, because the board lock serializes flips. The report is queued before the terminal write, so a crash between the two re-reports at the next `Done` rather than losing this one. `settle_team_run` is a compare-and-set: a reporter that loses it cancels its own message, so the flip and the death backstop never both report.

Cohort death is settled by `settle_ended_teams`, called from the sidebar's digest backstop and from the launcher's parked exec wrapper ([subagents.md § Backstops](./subagents.md#backstops)). Both need a live launcher. A hand-edited `Done` with a live cohort reports nothing at the edit, and reports completed when the cohort later ends. A resumed cohort has no run, so after death settlement its next `Done` reports nothing.

### Detached teams

A team launched with `--detach` carries `report_to: nobody` on its team run. Settlement still selects it: `on_flip` and the death backstop publish the leader's response and mark the run completed or failed exactly as for an attached run, but queue no message, store no `report_message_id`, and return `TeamReportOutcome::Detached`, so the flip receipt says the team was launched detached. `reopen_team_run` copies the policy from the newest run, so the stretch after a flip out of `Done` stays detached.

### Launcher debt and the team wait

Debt does not select a detached run. `fleet::owed_team_runs` keeps only runs that report to the launcher, and it feeds the owed-wake team term (`harness/owed.rs`) and the `Team` pending wait (`harness/schedule/pending.rs`). `ended_team_runs` reads every open run, so a dead detached cohort is still settled.

An owed team run whose board is not at `Done` projects a `Team` pending wait on its launcher: a resting launcher displays working until `Done` or cohort-death settlement. Its operational status stays sleeping for delivery and [prompt-cache keepalive](./messaging.md#prompt-cache-keepalive). Both the debt and the wait end when the board reads `Done`, even while the run is still open.

### The memory line

Each report ends with `Memory: <board path>`, a host path. A flip passes its receipt's board, the canonical file in the run directory, so the path survives checkout removal. The death backstop passes `scratch::durable_board_path`, which falls back to the checkout's link path when the link no longer resolves.

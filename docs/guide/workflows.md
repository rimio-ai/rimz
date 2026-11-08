# Workflows

A [loop task](./loops.md) is a cron line for agents. On a clock or a signal it fires one turn, and `--verify` can re-prompt that turn until a command passes. That covers "every morning, sweep the PRs" and "fix until the tests pass". It does not cover work in stages that waits on the world between them: keep this PR alive until it merges, launch a fixer each time CI fails, stop after five tries, and call me when something needs a human. Written as loop tasks, that plan lives in prompts and in an agent's context, and a dead or compacted agent loses it.

A workflow writes the plan down as a small graph in `~/.rimz/workflows/<name>/workflow.toml`. Each node is a check or a step. A check reads the world (ordered conditions, a shell command, a script you write, or a model call), or sleeps, and answers with an outcome word. A step does work: it launches a fresh agent, optionally with companions, or runs a command. Edges send each outcome to the next node. RimZ keeps the run's position in a record on disk. When an agent dies or the machine reboots, the run continues from that record. An interrupted check can run again; a command step whose host died goes to its next check rather than repeating the action.

## Write a workflow

The smallest useful workflow runs the test suite, sends an agent to fix it when it fails, and gives up after three tries. A loop task's `--verify` could do this one alone. It is shown first because every larger graph uses the same parts. Create two files:

```text
~/.rimz/workflows/fix-tests/
  workflow.toml
  prompts/fix.md
```

`workflow.toml` is the graph:

```toml
[workflow]
on-break = "@failed"
scope = "worktree"
start = "tests"

[checks.tests]
shell = "cargo test"
on = { pass = "@done", fail = "fix" }

[steps.fix]
agent = "claude"
prompt-file = "prompts/fix.md"
lifecycle = "turn"
then = "tests"
max-visits = 3
timeout = "1h"
```

Every fireable failure edge needs an explicit route or on-break; validation and launch refuse `@user` targets. For failures, set `[workflow] on-break = { agent = "debugger" }` to open your chosen agent profile with a debug trace, or `on-break = "@failed"` to end silently (`"@done"` is also supported). Explicit failure routes override this catch-all. The agent is a staying step named `@break`: it may fix the cause and run `rimz workflow resume <run> [--at <node>]`, cancel, ask urgently through its native UI, or end its turn and wait for you. Resume defaults to the node that failed and leaves the debugger's pane open; cancel also leaves it open. Closing its pane ends the run as failed. Its deadline cannot interrupt it. See [on-break](../reference/cli/workflow.md#failure-handling-with-on-break) for custom prompts, failure input, and legacy runs.

`shell` runs the command in the checkout through `sh -c`. Exit 0 answers `pass`; any other exit or signal answers `fail`. RimZ saves both output streams in a per-evaluation log and gives its absolute path to the next node as `input.log`. Replace `cargo test` with your suite's command. A shell check requires both `on.pass` and `on.fail`; its only other routes are `on.timeout` and `on.invalid`. Use `run` instead when a script needs to answer more than pass or fail.

`prompts/fix.md` is what the agent receives each time the step launches. RimZ sets `RIMZ_WORKFLOW_DIR` in the agent's environment, and the agent reads it there. RimZ does not expand variables in the prompt text.

`RIMZ_WORKFLOW_DEFINITION` names the workflow's absolute definition folder, distinct from its work directory and the checkout. Steps, judges, script and shell checks, and command steps all receive it. Every workflow agent's Environment section includes a `- workflow:` line naming that folder, even with `env_reminder` off.

```markdown
The test suite in this checkout fails. Run `rimz workflow show --history` to read the check's answer and log path, and read your working-memory file named in the Workflow note. Fix the code until the suite passes, then commit. An earlier attempt may have left uncommitted changes: inspect them and continue from them. Do not report that the suite passes. The workflow runs it again after your turn.
```

Read the graph from `start`. The run begins at the `tests` check. An answer of `pass` goes to `@done`, which ends the run. An answer of `fail` goes to the `fix` step, which starts Claude Code in the checkout with the prompt file. When that agent's turn ends cleanly, RimZ stops the agent and follows `then` back to `tests`. The check decides whether the work is done, never the agent. The check's whole answer, `outcome` and `reason` included, becomes the next node's `input`. That is how `log` reaches the agent.

`start` may name a check or a step. To do work before the first check, name the agent or command step directly; no placeholder check is needed. A first step's `context.json` has `input`, `key`, and `prev_attempt` set to null, and `visit` and `visits` both set to 1. That launch counts toward `max-visits-per-outcome` under the null outcome and null key. An agent step still waits for its window, checkout occupancy, and any applicable capacity admission.

Two fields bound the loop. `max-visits = 3` refuses a fourth visit to `fix` (a launch or an `until` push-back), and the run follows `on.cap` (explicit, else on-break). `timeout = "1h"` ends an attempt that runs longer than an hour and follows `then` as if the turn had ended. Time the step's agent, or a subagent it launched, spends waiting on a blocking question, permission prompt, provider limit, error, or budget park does not count toward that hour. Overlapping waits count once. Silence and an interrupt do not pause the timeout; the run deadline never pauses. Durations take `s`, `m`, `h`, and `d`, at most `365d`.

The `365d` maximum covers `timeout`, `sleep`, `for`, route `delay`, and workflow or step `stagger`, including when a live run rereads its sealed definition. Overlong optional values warn and are ignored; an overlong `sleep` refuses validation of a new definition, while in a live run's sealed definition it routes to `invalid` when the sleep is reached. A command step whose timeout is ignored refuses as missing. `capacity-wait` is separate and has no such maximum.

For distinct failures to get their own tries while the same failure stops retrying, add `max-visits-per-outcome = 2` to the step. Visits count separately for each routed outcome and key. Two checks answering the same outcome share a count; a script or agent check can split it further by answering different string or integer keys. Shell and condition checks inherit the run's key. The bucket uses the step's routed outcome and `key`: forwarded input and `resume --at` retain that outcome. A cap park preserves the refused bucket, but resuming it resets the capped step's total and every outcome/key count. An engine route into a different step counts there under its own outcome, such as `cap`.

To give a later failure new tries after a success, use `max-visits-per-outcome = { limit = 2, reset-on = "queued" }` when a check answers `queued`. `reset-on` also accepts a nonempty list of outcomes. That answer clears this step's outcome counts even when it routes to `@wait` or `@done`; the total is unchanged by this automatic reset. A `for`-held answer resets only when its hold completes. Switching between failure outcomes does not reset anything. Without `reset-on`, counts last until a manual restart or cap resume.

Keep `max-visits` to cap the total: a check naming a new outcome or key each time must not retry forever, and a per-outcome bound alone does not bound a cycle. Both limits must be positive, and the per-outcome limit must be below the total when both are set. `reset-on` requires a total bound and outcomes that a check can answer. A cap without `reset-on` warns when only one outcome reaches a non-start step with no incoming `then`: it repeats `max-visits` unless a check answers keys. The total cap is checked first and wins if both trip. Either cap follows `on.cap` (on-break, else `@failed`; legacy default `@user`), and its `input.bound` names the field that tripped, `"max-visits"` or `"max-visits-per-outcome"`, so a `run` or agent check can route the two apart. The reference gives the [whole cap input](../reference/cli/workflow.md#json-shapes).

Select exactly one of `run`, `shell`, `agent`, `sleep`, or nonempty `cases` on a check. Paths in `run` and `prompt-file` are relative to the workflow's directory and must stay inside it. `run` is a command line split into words, and no shell reads it. A pipe or a variable belongs inside the script, so start the command with `sh`, `uv run`, or another interpreter. `shell` accepts shell syntax and arbitrary paths; it is not confined to the workflow directory. `agent` takes an agent kind such as `claude` or `codex`, or the name of a [profile](./fleet.md#profiles-shape-an-agent-for-one-job) you defined. `sleep` waits for a duration, optionally ending early on a condition.

Run, shell, and agent checks accept an optional `wake` list of signal subscriptions. Condition checks derive their subscriptions from their terms, so an explicit `wake` warns and is ignored; see [waiting on the outside world](#wait-on-the-outside-world).

## Check it and start a run

Validate the definition first. The command prints every node with the edges it resolved, including the defaults the engine adds:

```console
$ rimz workflow validate ~/.rimz/workflows/fix-tests
WORKFLOW   NODE   KIND   RUNNER      BACKGROUND  EDGES
fix-tests  tests  check  cargo test  —           fail → fix (explicit), pass → @done (explicit), timeout → @failed (on-break, never: no timeout), invalid → @failed (on-break), deadline → @failed (on-break, only with a deadline)
fix-tests  fix    step   claude      yes         then → tests (explicit), invalid → @failed (on-break), cap → @failed (on-break), capacity → @failed (default, never: capacity-wait is forever), deadline → @failed (on-break, only with a deadline)
warning: fix-tests: steps.fix: auto-continue is off; a step paused on a provider limit waits for you
```

Failure edges use on-break when set, or an explicit node route. The `tests` row says a command that cannot start or a log that cannot open (`invalid`) follows that failure route, but its timeout cannot fire because this check has no timeout. A deadline needs a grant on the run. The `fix` row says the cap (`cap`) uses its failure route too; a question stays live in the agent's pane. Its capacity wait never gives up by default. `BACKGROUND yes` means RimZ can see the agent's background work, so a turn that ends with background work still running does not end the attempt.

The warning appears while [auto-continue](./loops.md#auto-continue) is off, which is the default. A provider rate or spend limit keeps the step open on every lifecycle, waiting for auto-continue or for you in the agent's pane. It spends no visit and does not run `then`. A budget park also keeps the agent open. A budget refusal before launch still ends the attempt as `limited` and follows `then`.

Definitions can outlive a field rename or the version of RimZ that wrote them. Unknown fields and invalid optional values warn with their dotted path and are ignored, even limits such as `window`, `throttle`, and `max-visits`. Read those warnings: an ignored limit no longer bounds the run. Required fields and invalid live route targets still refuse, and graph rules run after ignoring, so an invalid timeout on a command step still refuses as missing. The [validation reference](../reference/cli/workflow.md#validate-and-schema) lists the rules.

A Claude Console API usage cap (`You have reached your specified API usage limits`) leaves the leader's card reading `failed`, but keeps the workflow agent open on every lifecycle. Failed and interrupted turns wait for a later clean turn; an `until` check does not judge those stopped turns. An interrupt does not pause a turn step's timeout. Some providers report an interrupt as a clean end, so RimZ follows that report and lets the check judge. Provider exit still follows `then`, as `died` for a turn run or `ended` for a registered staying run. A headless Claude judge that hits this cap still fails as `died`.

An agent step's leader is a [supervised run](./scripting.md#what-a-run-does-on-your-machine), so it has the same prerequisites. The leader's reporting hooks must be installed and trusted, its program must be on `PATH`, and its account must trust the checkout. An unset leader permission mode defaults to `auto`. Companion agents use the ordinary layout checks: an undecided folder warns, but saved-untrusted folders and untrusted hooks refuse the workflow launch. `rimz doctor` reports hooks and folder trust, and names the `rimz trust grant` command for an undecided folder.

Then start the run:

1. Open the project's room. The room is the multiplexer session RimZ manages for the project, and `rimz start` opens it and attaches to it. Run the next commands from the project's directory, in a shell inside that room or in another terminal. A shell in another project's room carries that project's root in its environment, and `workflow run` would look for the worktree there.
2. Create a RimZ worktree, so the agent's edits stay off your branch. The [worktrees guide](./worktrees.md) covers what this creates.

   ```sh
   rimz worktree new feat-x
   ```

3. Start the run in that worktree, and keep its id:

   ```sh
   RUN=$(rimz workflow run fix-tests -w feat-x | sed -n 1p)
   ```

`run` validates again, prints its warnings to stderr, creates the run, and prints the run id (`wf_` and 32 hexadecimal digits), the run's work directory, then a `workflow show` hint for that run. Warnings alone do not stop it, and the first two stdout lines keep their scripting format. The first node starts in the background. In this example, if the suite already passes, the run ends at `@done` at once. Break a test first to watch the loop.

Without `-w` the run uses the checkout you are in. `-w` selects an existing RimZ worktree and never creates one. A workflow holds at most one active or parked run per checkout, so a second `run` there refuses and names the run that holds it. `--prompt 'keep the public API unchanged'` adds text to the end of every agent step's prompt for this run.

## Watch a run

Before starting a larger workflow, read its routes without jumping between TOML tables. `rimz workflow show fix-tests` draws the graph from `start`, hangs a returning step beneath its check, and keeps the limits beside the node they bound. Downward routes use `→`, returns use `↑`. Conditions show their cases in order, including how long a case must hold. Uniform defaults appear once below the rail; `validate` also lists edges that cannot fire, with the reason. The runs of this workflow follow the graph.

```console
$ rimz workflow show fix-tests
fix-tests
one run per worktree · no loop task starts it; by hand: rimz workflow run fix-tests -w '<worktree>'

○ tests  check  sh: cargo test
│   fail → fix · pass @done
│
├─○ fix  step  claude · 1 turn ≤1h · ↑ tests · 3 visits, then @failed
│

A turn step goes on to its next check when its turn ends cleanly or its provider exits.
one node in full: rimz workflow show fix-tests tests · source ~/.rimz/workflows/fix-tests/workflow.toml

no active or parked runs of fix-tests
```

When a route needs a closer look, open its node rather than guessing which defaults apply: `rimz workflow show fix-tests fix`. This view includes the prompt path, layout, limits (including `reset-on`) and every resolved edge, even one that cannot fire. A condition node also shows its wake subscriptions and each case's hold. This reads the definition for future runs; a run address still inspects that run's sealed graph and history.

```console
$ rimz workflow show fix-tests fix
fix · step in fix-tests · agent

reached from: tests by fail

edges:
  then → tests (explicit)
  cap → @failed (on-break)
  capacity → @failed (default, never: capacity-wait is forever)
  deadline → @failed (on-break, only with a deadline)

agent:       claude
prompt-file: prompts/fix.md
timeout:     1h
max-visits:  3
```

```sh
rimz workflow list         # every active and parked run, with progress and what it waits on
rimz workflow show "$RUN"  # one run: painted graph, next moves, and a short trail
rimz workflow show "$RUN" --history  # checks and attempts, repeats folded; --json keeps every entry
rimz workflow show fix-tests  # definition rail, then live runs
```

From a workflow step or one of its subagents, omit the target: `rimz workflow show` finds that run, as do `rimz workflow show --history` and `rimz workflow show --json`. Outside a workflow, name a run or use `rimz workflow list`. The compact view prints the absolute notes folder when the run has one.

`list` puts parks, blocking questions, and capacity holds needing a fix under NEEDS YOU, longest waiting first, with a full address and a hint to inspect the first run. The other rows group by workflow, with active and held counts and self-clearing capacity holds last. Each row has a status glyph, a lane such as `#feat-x`, what it is doing, and its age, followed by a horizontal progress rail. Staying sessions name their agent card as `with you in @<handle>`; a cached PR adds ` · PR #N`. A shared origin appears in the group heading; mixed origins appear on each row. Combine the workflow name and lane, such as `fix-tests#feat-x`, instead of `$RUN` in `show`, `resume`, and `cancel`; the part after `#` is the checkout directory's name (the project directory for project scope). Several rooms get separate headings.

Legacy parks and capacity holds needing a fix use `⊘`, not an agent's `?` question or paused mark. The same held glyph appears in `show` and at the current rail node; its [theme role](./theme.md#glyphs) is `workflow.held` in both glyph sets.

A row marked `· N ignored` has fields RimZ ignored in that run's sealed definition. `show` names each path and reason, even after the run finishes. These notices reflect how the current version reads the sealed definition, so an upgrade can add them to an existing run. A clean run has no mark. Start-time warnings about auto-continue or background reporting do not mark runs.

`show` on an address or id starts with where the run is and what it is waiting for, then paints its sealed graph: `●`/`○` for reached/unreached steps, `◆`/`◇` for checks, and the headline's activity glyph at the current node. Reached glyphs are green, current nodes bold in their state tone, and future nodes muted. The header adds a cached PR number for active or parked runs when known; a live agent uses its full card handle. Past nodes keep their last outcome or attempt, time and repeat count. One line below the headline summarizes the current check's readings and when they were read. Its cases put `▸` in the gutter at the winner or held case and `?` where unread evidence stopped it. The current check says `last checked`, not an advance count; completed attempts omit pet names and say `HEAD <a>, no new commits` when unchanged. Those marks work without color. Visit counts and a short trail follow, then the work directory and a command for full history. When parked, it explains why and gives the exact commands to resume, grant more bounds, choose another node, or cancel, followed by a horizontal rail. Finished runs use the horizontal rail without a current node. While an agent step runs, its agent has a card in the sidebar and a pane you can watch. The [workflow reference](../reference/cli/workflow.md#inspect-runs) lists every field and the rules for choosing a run by name or address.

Here the suite passed without needing `fix`. The finished run keeps that distinction visible:

```console
$ rimz workflow show fix-tests#feat-x
fix-tests#feat-x
✓ done 3m
started 09:12 by hand · 0 attempts

◆ tests
trail  09:12 tests pass → @done · 09:12 done
work dir ~/.rimz/ws/demo-1234/owned/workflows/wf_00000000000000000000000000000004/work
all of it: rimz --root ~/project workflow show 'fix-tests#feat-x' --history
```

For a live or parked run, `show` adds a `now <check> · <outcome>` line when the current check, or an agent step's first listed check, reads conditions. It shows what that check would answer from current cached readings and the board, not its last journal answer. A winning `for` case includes the hold's remaining time, or its full duration without an open hold. Forge readings include their probe time; stale readings show `unread` and the evaluator's reason. This reads only: no check child, record write, or probe refresh. CI conditions retain their read-only Git fence. A conditional sleep also has a now line; script, shell, judge and plain-sleep checks do not. JSON and history stay unchanged.

An attempt can end as `died` when an agent fails to start or crashes, or when a command fails. Use `show --history` for checks and attempts in order, folding adjacent repeated evaluations with their count, span, newest answer, and summed judge cost (`--json` keeps every journal entry): the handle or exit code, commits before and after, duration, known cost, recorded `reason:`, and log path. A message step's attempt instead names the agent it reached and its outcome, or `—` and why no agent took it. A command's log holds its output; an agent's own UI carries errors that it did not report to RimZ.

## What a run does on your machine

`rimz workflow run` creates the run directory, `~/.rimz/ws/<workspace-dir>/owned/workflows/<run-id>/`, with four entries:

- `record.json`, the run's current position, which is the truth the engine reads.
- `journal.jsonl`, the append-only history behind `show --history`'s folded rows; `show --json` includes every entry.
- `workflow.toml`, a sealed copy of the definition. Editing the original changes later runs, never this one. Scripts and prompt files are not copied: each check and launch reads them fresh from `~/.rimz/workflows/<name>/`.
- `work/`, the work directory, which `RIMZ_WORKFLOW_DIR` names. It holds `context.json`, `notes/`, `signs/`, script stderr and shell-check logs (`<seq>.check.log`), command-step logs (`<seq>.step.log`), and judge answers (`<seq>.check.json`). Your nodes keep temporary work here and kept memory through the `notes/` link; the run record and journal sit outside it.

RimZ rewrites `work/context.json` before every check and every attempt. It names the run, the node, and this visit, carries the `input`, and describes how the last attempt ended. [The context file](#the-context-file) lists its fields.

A script or shell check runs as a child process in the checkout, with no stdin, and with `RIMZ_WORKFLOW_DIR`, `RIMZ_WORKFLOW_RUN`, and `RIMZ_WORKFLOW_DEFINITION` set. RimZ reads a script's stdout as its answer and saves its complete stderr to `work/<seq>.check.log`, even when empty; a shell command's combined output is a log at that same path. A condition check reads the same [condition keys](./loops.md#wait-for-done-and-green) as a loop, without a child process. A check's optional `timeout` spans evaluations and waits; shell checks do not inherit the loop's default timeout. Timeout, deadline, and cancellation stop the shell's whole process group.

An agent step's `agent` accepts the same layout grammar as `rimz agents`: a kind, profile, virtual cell such as `claude-auto`, or a layout such as `claude,nvim` or `yagni,reflect-notes`. It needs an unambiguous prompt leader and cannot name a team. Judges still take one profile. The layout opens in an unfocused new tab titled `run <leader kind>`. Every agent cell gets the same workflow environment variables; only the leader gets the composed prompt and owns the supervised run. Its prompt is the prompt file, then `signs/<step>.md` when that file exists, then the run's `--prompt`. The room's turn, room, and account [budgets](./budget.md) apply to every agent. Attempt cost counts the leader only; companion spend still appears on their cards and in stats.

Only the leader also gets a `### Workflow` launch note (including the check's ordered cases, run or shell commands and summaries, or judge summaries, and that push-backs count as visits): what a workflow is, a compact graph rail marking this step, who started the run, why it arrived here, applicable visit caps, the previous attempt, and what follows its turn. It names the step's working-memory file and points to `rimz workflow show` and `rimz workflow show --history`. Automatic runs name the loop task and its current trigger when available. `rimz workflow restart`, and `rimz agents restart` on a step agent, compose a new note for the relaunched step, saying the user restarted it. Ordinary agent resume refuses a current workflow step, and room rebirth leaves its recovery to the workflow. Providers without a system-text channel receive no launch note.

Set `lifecycle = "turn"` for a one-clean-turn agent step. When the leader's turn ends cleanly, RimZ by default stops every agent cell and its subagents, closes the command cells, records the commit before and after, and follows `then`. Failed or interrupted turns and provider or budget limits keep the agent open; a later clean end completes the attempt. Provider exit before that end still follows `then` as `died`. A companion's question or limit does not end the leader's attempt. A peer or team launched later by a cell keeps running until you stop it.

The note describes every listed check in order. For `run`, it shows the command as written, any summary, and the rule that slash-containing words name files inside the definition folder while the command runs from the checkout. For `shell`, it shows the command and any summary, says it runs through `sh -c` from the checkout, and names `$RIMZ_WORKFLOW_DEFINITION` for scripts in the definition folder. In both cases the step agent may run the command itself. Judges show only their summary, never their prompt filename; an older sealed judge without one is described simply as an agent judge.

Use `lifecycle = "stay"` when you want to work with the agent across turns. The run reads `with you in @<handle>` in `workflow list` and `<step> is with you` in `show`, or `asking you: …` and `<step> is asking you` while a blocking question is open. Questions stay on the agent's card: answer in its pane. Turn ends, interruptions, provider limits, and provider session-end hooks do not end the attempt. Exiting the leader's provider process ends it as `ended` after registration, or `died` before registration, and follows `then`; a crash after registration also reads `ended`, so the check judges the work. Every layout cell closes. A staying step warns and ignores `timeout` and `on.asked`; the workflow deadline, cancel, restart, reset, and rebirth still end it as `canceled`.

Agent steps default to `lifecycle = "until"`: work across turns until the check says you are done. At each clean leader turn end, once the leader is at rest with no pending work, blocking question, provider limit, error, interrupt, or budget park, RimZ runs `then` once. A route back to this step sends the answer back as your next prompt in the same session, updates the input and key, and counts one visit in that answer's bucket. The prompt names the check and outcome, explains false condition terms and unread readings or the script/judge's reason, and says the run moves on when the check passes. A push-back that would exceed either visit cap follows `on.cap` instead, closing the agent by default. A route to `@wait` keeps the session held at the check without spending a visit; subscribed signals and the minute tick re-sample it while the leader remains at rest on that turn. Every route outside the listed checks, other than this step or `@wait`, leaves the step without repeating the check, closing the layout by default. While the check runs, `list` reads `with you · checking <check>`, with any waiting detail appended; a blocking question still takes precedence. A new leader turn abandons a held wait and any check in flight without stopping the session; a newer turn also discards a saved closing verdict. The next clean rest is judged again. Provider exit, deadline, cancel, and restart still end the session as for `stay`. Both `stay` and `until` are staying steps; omitting `lifecycle` therefore also warns and ignores `timeout` and `on.asked`. Set `lifecycle = "turn"` to use those fields. An `until` step cannot use a sleep as its `then` check; use `turn` for that too.

Use `then = ["tests", "review"]` to put a cheap check before a costly judge without closing the agent between them. Each check except the last must have a direct route to the next listed check. The run enters the first check at every eligible turn end; routes, not list order alone, decide where it goes. A route between listed checks keeps the session open and preserves the step's input and key, so a later check reads earlier answers from their output files or the world, not from `context.json` input. A later check can push back to the step, wait, or close just like the first. Empty lists, non-check entries and sleeps on an until step refuse. A list of two or more checks on a `turn`, `stay` or command step warns: only an until step keeps its agent open across it; other steps enter the first check and let routes decide the rest.

A child check's push-back names the check that actually answered, including a later listed check. Its `reason`, when present, comes before `output: <path> (<size label>)`: script stderr or shell output at `<seq>.check.log`, or the judge's whole saved answer at `<seq>.check.json`. An empty or missing file reads `no output`. The prompt never copies a tail or file content; condition explanations stay unchanged.

Set `keep = true` on a `turn` or `until` agent step to leave its leader and companions open outside the run when the attempt finishes naturally. The workflow moves on and no longer owns those agents. With `turn`, a check that returns to the step leaves the old agent beside the next launch; normal occupancy still applies, and `takeover = true` may stop that old agent once idle. Cancel, restart, deadline, a turn step's timeout, and a push-back cap that parks the run still stop the layout. A cap routed to another node detaches it instead. `keep` defaults to false; on `stay` steps and non-agent nodes it warns and is ignored.

To leave time for another turn before an `until` session closes, give the closing route a delay:

```toml
[checks.ruled]
run = "uv run --script checks/notes.py after"
on = { open = "@wait", settled = { to = "@done", delay = "30m" }, left = "@failed" }
timeout = "5m"
```

Here an until step must name `ruled` as its `then` check. A `settled` answer starts the 30-minute delay without rerunning the check; the check timeout does not cut that saved verdict short. A new leader turn discards it, and the next eligible turn end checks again with a fresh delay. Cancel, restart, deadline, and provider exit still interrupt it; a signal wake or `resume --extend` does not shorten it. Closing uses the usual minute tick, so it can be about a minute late. `list` and `show` read `checking ruled: closing in 23m → @done` while the delay remains. `workflow validate` and the node view print the route as `settled → @done after 30m`. Without a held until session, including `resume --at ruled`, it goes at once. Delays use nonzero `s`, `m`, `h`, or `d`, at most `365d`; invalid delays or delays on a check not used by an until step, a `@wait` route, a route between listed checks, or a return to an until step using that check warn and are ignored, keeping the target.

An agent step does not start in a checkout another agent is using. It waits until the checkout is free. With `takeover = true` it stops an idle agent there to make room, but an agent in the middle of a turn still makes it wait. Agents in other RimZ worktrees do not count.

A step that only reads the checkout, such as a reviewer, does not need it to itself. Set `share = true` and it launches beside whoever is there, idle or mid-turn, and never stops anyone. A sharing agent still occupies the checkout, so a later step without `share` waits for it to leave. `share` and `takeover` exclude each other.

```toml
[steps.review]
agent = "reviewer"
prompt-file = "prompts/review.md"
share = true
then = "verdict"
```

When a loop starts the run automatically, its agent steps also wait for [capacity](./loops.md#limit-agent-starts), before stopping any occupant. A held step uses no visit and retries about once a minute. `list` separates its reason and held age; `show` prints `held 4m` and a bounded wait's give-up time. On an agent step, set `capacity-wait = "6h"` to give up after six hours, or leave the default `forever`. Giving up ends the run as failed unless `on.capacity` names `@done` or another node. It cannot park at `@user` or `@wait`. A window or occupancy wait restarts this clock, and the run deadline still applies. `throttle = "off"` skips the machine turn, stagger, resource limits, and capacity check, not the machine caps or workflow and step limits below. Runs you start with `rimz workflow run` or `rimz loop fire`, and runs detached by removing their loop task, skip capacity waits but still count and restart any applicable workflow or step stagger, never machine stagger. Command steps, checks, and an already-open staying session never ask for admission.

A judge runs headless, with no pane, and [the judge section](#let-a-model-decide) covers what it may touch.

While the room is open, it checks every active run about once a minute. With no room open for the project, only the [loop timer](./loops.md#who-keeps-time) does those checks, so keep the room open while a run works. Your `~/.rimz/workflows/` directory is never written.

### Limit one workflow or step

The [machine caps](./configuration.md#agent-start-limits) protect the whole fleet. When triage needs a tighter cap than sweep, put that rule on triage. Add a workflow limit when all its agent steps also need a shared ceiling:

```toml
[workflow.limit]
max-parallel = 4
stagger = "2m"

[steps.triage.limit]
max-parallel = 3
stagger = "10m"
```

Both tables accept `max-parallel`, `stagger`, or both. `max-parallel` is a positive integer; `stagger` is a positive duration in `s`, `m`, `h`, or `d`, at most `365d`. `rimz workflow validate` warns and ignores invalid values or a limit on a command step; a table left with neither field is ignored whole. A workflow limit covers open agents started by that workflow name across rooms; a step limit covers only agents started for that step. Each run obeys its sealed limits, so editing the definition changes new runs, not waiting ones.

Every open agent takes a slot, including idle staying panes, companions, and RimZ-launched children and peers. Waiting, paused, hibernated, compacting, and live agents whose last turn errored count too. Ended or dead agents, failed launches, and provider-built-in subagents do not. An open agent keeps its slot even after its workflow run finishes. A layout must fit as a whole, including starts already admitted but not yet launched. An automated step whose layout is larger than a cap fails with the key and the fix; it does not wait or take `on.capacity`. These are start limits, never a reason to stop an existing turn or pane. Starts you request by hand skip the wait even at a full cap, recording any skipped limit hold in `workflow show`, but count afterwards and restart the workflow and step stagger gaps. `throttle = "off"` does not bypass these limits or the two machine caps.

A manual step start, including a restart or a later step of a manual run, records the first workflow or step hold an automatic start would have met at that claim: an in-flight launch, earlier waiter, full cap, or stagger gap. `workflow show` prints `<hold>; started anyway: manual starts do not wait` while that session is current, and `--history` keeps it with the settled attempt. A clear limit or an unreadable limit adds no note. This is read when the step claims admission, not when you type `workflow run` or `loop fire`; neither command prints the line.

After the window and checkout clear, an automated start must pass workflow and step limits, then the machine caps. Earlier capacity waiters of the same workflow go first: a workflow-limit waiter holds all steps, a step-limit waiter only its own step. A freed slot can wait until that run's next minute retry. `workflow list` puts these starts last in their workflow group, and `loop list` puts them in HELD, with reasons such as `step triage: 3 of 3 agents open (@builder, +2)`, `workflow land: 4 of 4 agents open (@reviewer, +3)`, `step triage: next start in 7m`, `workflow land: 2 earlier starts waiting`, or `workflow land: 1 start launching` while another start of the workflow is still coming up. Cap reasons name reservations separately (`1 more starting`) and a multi-agent layout's demand (`this start opens 2`). The same `capacity-wait` and `on.capacity` give-up apply to temporary shortages, including on an `off` step.

Workflow and step `stagger` set minimum gaps between launches, not between admissions or user turns. They survive process restarts and are shared across rooms. They cannot guarantee the usage window has caught up with the last start's spend: normal direct usage probes refresh no more than once every five minutes, while a live Claude statusline can update on the next heavy refresh while a sidebar is subscribed. The next start still uses the window reading available when it asks.

## Do mechanical work without an agent

Once a check sees green CI, sending the PR to merge is a command, not a reasoning task. A command step does it without launching or paying for an agent:

```toml
[steps.merge]
run = "gh pr merge --auto"
then = "pr"
timeout = "1m"
max-visits = 3
```

Route the green outcome to `merge`, and let the `pr` check decide whether the PR merged or still needs to wait. Select exactly one of `agent`, `run`, `shell`, or [`message`](#hand-work-to-an-agent-that-is-already-running) on a step. `run` passes words literally, without a shell; a quoted word with spaces stays one argument, and files named by slash-containing words must stay inside the workflow directory. Use `shell` instead for `sh -c` syntax and arbitrary paths. Both command kinds require an explicit `timeout`, so a hung command cannot hold the run.

The child runs in the checkout, in its own process group, with no stdin, the room's workspace environment, `RIMZ_WORKFLOW_DIR`, `RIMZ_WORKFLOW_RUN`, and `RIMZ_WORKFLOW_DEFINITION`. Both output streams go to `work/<seq>.step.log`. It opens no pane, waits for no agent to leave the checkout, and takes no prompt. `prompt-file`, `lifecycle`, `window`, `takeover`, `share`, `on.asked`, and `on.invalid` belong only to agent steps, apart from a message step's `prompt-file`; command steps read `context.json` instead. Signs and the run's `--prompt` compose only agent prompts and message-step messages.

Exit 0 records `ended`; another exit or a signal records `died`. A spawn or log-open failure also records `died`, with a reason. The step timeout stops the group and records `timeout`. These outcomes follow `then`, with the original routed input unchanged and the outcome in `prev_attempt`. A run deadline stops the group, records `canceled`, and follows `on.deadline`; `workflow cancel` stops the group and ends the run. If the host dies, the next advance stops any survivor, records `canceled`, and follows `then`, without repeating the action. A child that already exited keeps its exit outcome even if a bound became due before settlement.

Make the action safe to repeat on a later visit, just like an agent step. A cycle still needs `max-visits`, and it should go through a waiting check: a command returns at once, so an immediate route back can consume the whole visit cap in one advance.

`workflow validate` and `workflow run` warn for each command or message step bounded by `max-visits` on a cycle that never waits, naming that step's cap. An agent step or a check with a non-deadline `@wait` route breaks this warning, including a sleep or a condition's implicit wait. A case's `for` hold is not a wait edge. The warning does not refuse the run; loop-started runs do not print it.

### Hand work to an agent that is already running

A planner or a lead often already sits in the room with the context a workflow's result needs. Launching a fresh agent for it costs a cold start and loses that context, and `rimz message` from a `shell` step leaves no record in the run. A message step queues one message for that agent and moves on:

```toml
[steps.tell]
message = "@planner"
prompt-file = "merged.md"
then = "@done"
```

`message` takes an agent address, `@<handle>` or `@<handle>#<channel>`, resolved like `rimz message` from the run's checkout among the living agents. Each visit queues one `Type: WAIT` message from `@rimz` for the agent's next turn boundary, sent at once when it is resting. The message opens with a line naming the workflow, the step and the run, so the agent can run `rimz workflow show <run>`, then carries `prompt-file`, the signs, and the run's `--prompt`, as an agent step's prompt would. `prompt-file` is optional; without any of them the message is that line alone. The step records `ended` with the target's handle and follows `then` once the message is queued, whether the agent is mid-turn or resting. `then` names exactly one node, `@done`, or `@failed`.

When no single living agent matches, the step records `died` with the reason and follows `on.undelivered`, which defaults to the workflow's `on-break`; it never follows `then`. `@user`, `@me`, and pane ids are refused, since a message step hands work to an agent. If the advancing process dies after queuing, the next advance sends the same message again, and the queue keeps it once. A message step opens no pane and takes no checkout, so the agent-step fields it cannot use (`lifecycle`, `window`, `takeover`, `share`, `subscribe`, `keep`, `limit`, `throttle`, `capacity-wait`, `notes`, `timeout`, `on.asked`, `on.invalid`, `on.capacity`) warn and are ignored. Like a command step it returns at once, so a cycle through it needs `max-visits` and a waiting check.

## Wait on the outside world

Some checks can only say "not yet". A PR whose CI still runs is one. Route that outcome to `@wait`, and the run stays at the check and asks again later. This workflow keeps one worktree's PR alive until it merges:

```toml
# ~/.rimz/workflows/babysit-pr/workflow.toml
[workflow]
on-break = "@failed"
scope = "worktree"
start = "pr"

[checks.pr]
summary = "Wait for merge or sustained CI trouble"
else = "pending"
on = { merged = "@done", queued = "@wait", red = "sweep", pending = "@wait" }
timeout = "2d"
cases = [
  { when = ["pr=merged"], outcome = "merged" },
  { when = ["pr.queue=queued"], outcome = "queued" },
  { when = ["ci=failed || pr.queue=dequeued"], outcome = "red", for = "2m" },
]

[steps.sweep]
summary = "Repair CI and re-arm the PR"
agent = "claude"
prompt-file = "prompts/sweep.md"
lifecycle = "turn"
takeover = true
then = "pr"
max-visits = 5
max-visits-per-outcome = { limit = 2, reset-on = "queued" }
timeout = "6h"
```

`cases` is an ordered list, one case per line. Each `when` array is ANDed, and its strings take the loop grammar (`!`, `&&`, `||`, parentheses, and branch selectors). The first true case wins; all false answers `else`. Every case outcome and `else` needs an `on` edge. Here a merged PR ends the run; a queued PR waits and resets `sweep`'s outcome counts; a failing CI or dequeued PR held for two minutes launches `sweep`. Two sweeps for `red` without a queued answer use up that outcome's tries, while five sweeps in total use up the step's total bound. `takeover = true` lets it stop an idle agent you left in that worktree. However the agent's turn ends, `then` sends the run back to `pr`.

A sweep that repairs a queue removal must also re-arm the PR (enable auto-merge or add it to the queue); a push alone does not clear `pr.queue=dequeued`.

A landing check can route conflicts and review state with `pr.mergeable` and `pr.review` instead of a script; the [condition keys](../reference/cli/loop.md#conditions) list their readings.

Unread is not false: an unread case before the first true case answers `unknown`, even for `!pr=open`. The default `unknown → @wait` waits for evidence; for `until`, it holds the session and re-samples on subscribed signals and the minute tick while the leader remains at rest. Override `on.unknown` to route elsewhere. Do not write `pr=unknown` in a case: unread terms answer the `unknown` outcome instead. An ordinary check needs a successful forge probe at or after entry to that check (including after resume). An `until` session's check instead accepts a successful probe at or after the session's start, including a probe from an earlier turn. Until a probe meets that floor, `show` names the unread keys. Stage and usage-window readings have no freshness floor. Keep the room open for fresh forge data: an idle repository's next probe can take up to five minutes. Two consecutive ordinary forge checks each need a post-entry probe, so put related decisions in cases of one check. Closing the room leaves its last facts that pass the relevant floor in force. An `until` check samples the latest probe at each eligible leader turn end, and while held at `@wait` also on subscribed signals and the minute tick.

On an open GitHub PR, when the commits are known, bare `ci` also requires CI for the PR's head and the checkout's current HEAD to be on that PR. A local commit not yet on the PR, or a verdict for another PR head, makes `ci` unread. The check waits by default, and `show` says which commits differ. Someone else's push can count even when the checkout has not fetched it. Missing commit data skips only the comparison it prevents. Without a known PR head, the time floor alone applies, as it does for `pr`, `pr.queue`, `pr.mergeable`, `pr.review`, branch selectors such as `ci[main]`, merged or closed PRs, Gitea, and PR-less checkouts. Forge propagation lag can still affect those readings. Loop conditions use the same commit fence, and hold instead of answering `unknown` ([forge freshness](../reference/cli/loop.md#forge-freshness)).

A case's optional `for` is a nonzero duration in `s`, `m`, `h`, or `d`. A held case must route its outcome away from `@wait`; otherwise `for` warns and is ignored. While that case keeps winning, the run shows `holding red 1m/2m`; the next evaluation at or after two minutes follows `on.red`. A different answer closes the hold, and a later true starts a new one. Subscribed signals and the minute tick re-sample waiting checks, including a held `until` wait while the leader rests on its judged turn. Changes entirely between samples go unseen. Condition answers inherit the run's key.

A condition reading `window.*` requires `provider = "claude"` (an agent kind, not a profile) and accepts an optional `account = "work"`. An unknown provider or a provider without a window term warns and is ignored; an account without a remaining provider does too. Window terms still require a provider after ignoring. `team.stage` values are checked against the project's effective teams plus `Done`; validation outside a workspace warns that stage names were not checked.

A waiting check without `recheck_at` runs again about once a minute. A signal matching the current check's subscription clears `recheck_at` and runs it at once. A check with no subscription gets only its recheck time and the minute tick. A signal never wakes a step, but a staying step can [receive one as a message](#hand-signals-to-a-staying-agent).

A condition derives these subscriptions from all its cases, including negated terms:

| Term | Signal subscription |
| --- | --- |
| `ci` | `ci.*` for the run's checkout |
| `pr`, `pr.queue`, `pr.mergeable`, `pr.review` | `pr.*` for the run's checkout |
| `team.stage` | `team.stage` for the run's board |
| Branch-selector terms such as `ci[main]`, and `window.*` | None; recheck time and the minute tick only |

For a run, shell, or agent check, declare the signals that should re-run it:

```toml
[checks.review]
run = "checks/review.py"
on = { ready = "@done", pending = "@wait" }
timeout = "2d"
wake = ["ci.failed", { signal = "deploy.finished", match = { env = "prod" } }]
```

Each entry is a selector string or a table with a required string `signal` and optional `match`; unknown table keys warn and are ignored. Selectors and match values use the [loop signal grammar](../reference/cli/loop.md#signals). `ci` and `pr` default to the run's checkout `path`, unless the entry explicitly matches `path` or `branch`. `team.stage` defaults to the run's current board, unless the entry matches `team`, `instance`, or `board`; relinking the checkout changes which board wakes it. Other families, such as `review.ready`, are room-wide unless given match filters. For example, `{ signal = "ci.failed", match = { path = "*" } }` accepts any checkout. Project-scoped runs use the project root for these defaults. An invalid selector or match, an agent lifecycle selector, or a team selector other than exact `team.stage` warns and drops only that entry, keeping the others.

### Hand signals to a staying agent

A check's `wake` re-runs a script; it tells the agent nothing. A fixer that stays open across turns often needs the news itself: CI failed again, a reviewer asked for changes. Without a subscription it learns only when the check sends the run back. `subscribe` on a `stay` or `until` agent step delivers each matching signal to that step's agent as a message:

```toml
[steps.fix]
summary = "Keep the PR green"
agent = "claude"
prompt-file = "prompts/fix.md"
lifecycle = "until"
then = "pr"
subscribe = ["ci.failed", { signal = "review.changes", match = { repo = "acme/app" } }]
```

Entries take the same forms and the same checkout and board defaults as `wake`, and an invalid entry drops alone the same way. Each match is queued for the agent as a `Type: SIGNAL` message from `@rimz` at its next turn boundary, sent at once when it is resting. The message opens with the signal's subject and payload details, as a loop's signal delivery does, then names the workflow, the step and the run, so the agent can run `rimz workflow show <run>`. A forward moves the run nowhere: when the current check also subscribes to the same signal, both take effect. Subscriptions last while the step's attempt does: once it ends, a `keep = true` agent that lives on receives nothing more, and a message still queued when the attempt ends is canceled rather than delivered. The agent's launch brief lists what it subscribes to, and `rimz workflow show --history` prints one `signal` row per forward. `subscribe` on a turn or command step warns and is ignored, since a turn ends where the message would land.

A check that can wait must have a `timeout`, except a sleep, which has its own duration. The timeout starts each time the run enters the check and runs across every wait there. When it passes, the run follows `on.timeout` or on-break; legacy runs park by default. Here each return from `sweep` starts a new two days.

Keep `run` for custom, many-outcome answers. A rate limit is another thing to wait for: when the last attempt ended as `limited`, `context.json` carries `prev_attempt.limited.reset_at`, and a script can answer a waiting outcome with that time as `recheck_at`. The [sample `pr.py` in the RimZ repository](../../crates/rimz/tests/fixtures/workflows/babysit-pr/checks/pr.py) does this, and waits ten minutes after an attempt that died.

## Sleep between nodes

Use a sleep check for a pause without an agent or a script. Its required `then` is where to go when the time runs out, or when its optional `when` becomes true:

```toml
[checks.cooldown]
sleep = "30m"
then = "fix"

[checks.cooldown2]
sleep = "30m"
when = ["ci=passed"]
for = "2m"
then = "fix"

[checks.drain]
sleep = "6h"
when = ["!pr.queue=queued"]
then = "next"
```

The first pauses for 30 minutes. The second waits at most 30 minutes, waking early once CI has stayed passed for two minutes. The third sleeps while the PR is queued, for at most six hours. `when` uses the same ANDed term array and unread rules as condition cases: missing data never wakes it, including under `!`, and forge readings must be probed at or after this sleep's entry. It derives the same signal subscriptions; otherwise conditions are sampled about once a minute. The full duration wins even if a `for` hold is still running, and a conditional sleep can end about a minute late.

`sleep` is a nonzero duration in `s`, `m`, `h`, or `d`, at most `365d`. `for` needs `when` and must be nonzero and shorter than `sleep`; otherwise it warns and is ignored. `then` may name a node, `@done`, or `@failed`, never `@wait` or `@user`. The example's `fix` and `next` are nodes you define. An until step cannot use a sleep as its turn-end `then` check; use a route delay to postpone closing that session instead. A sleep accepts `on.invalid` for evaluation errors; `wake`, other `on` keys, `timeout`, `else`, `window`, and `prompt-file` warn and are ignored. Window terms use `provider` and optional `account` just as conditions do.

A sleep forwards the input and key it received, so the next step still reads the preceding check's answer and counts visits under that outcome. The exception is a failure routed to `@break`: the debugger receives the failure object with its source and edge. Re-entering or resuming a parked sleep starts its clock again. A plain sleep has no signal subscription and does not re-evaluate between entry and its end. `workflow list` and `show` read `sleeping 12m/30m → fix`, adding `when ci=passed` before the arrow when configured. A cycle such as `poll → cooldown → poll` needs no extra timeout: the sleep supplies its wait.

## Let a model decide

Some decisions need judgment that a script cannot give: whether review comments need a code change, or whether a diff matches its issue. A check with `agent` in place of `run` is a judge:

```toml
[checks.review]
summary = "Review the diff for correctness"
agent = "claude"
prompt-file = "checks/review.md"
window = "5h.left>=40"
on = { approve = "@done", revise = "fix" }
timeout = "1d"
```

The judge gets a shorter `### Workflow` launch note naming its check, the graph position and the run's origin, and explaining that its answer chooses where the run goes. The prompt still carries the JSON answer contract.

`checks/review.md` says what to judge and what each outcome means:

```markdown
Read the review comments on this branch's PR with `gh pr view --comments`.
Answer "revise" with a one-line reason when any comment asks for a code
change that is not already on the branch. Otherwise answer "approve".
```

A judge runs headless in the checkout. Its permission mode is automatic unless its profile sets one, so it does not stop on prompts. Its Judge note points to `rimz workflow show $RIMZ_WORKFLOW_RUN --history`; its prompt requires one JSON object of the same shape a script prints, written to the supplied `output_path`. A judge must leave the checkout alone. Under [scoped isolation](./security.md#scoped-isolation), RimZ mounts the checkout and its Git directories read-only for the judge. Under host isolation, which is the default, only the prompt tells it so. `rimz workflow validate` refuses a judge whose agent has no headless mode.

A judge that fails (its process fails, or it writes no answer or a bad one) runs once more, with `retry` in `context.json` saying what went wrong. A second failure follows `on.invalid`, falling back to on-break (legacy runs still park). An [unsupported agent version](../reference/agent-support.md) refuses before the judge runs and follows the same retry and failure-route policy; `workflow show` includes the upgrade instruction.

`window` holds a judge until its provider's usage window has room, or an agent step until every agent cell's provider has room. `5h.left` and `7d.left` are the percent left in the 5-hour and 7-day windows, compared with a whole percent, as in [loop conditions](./loops.md#wait-for-room-in-a-usage-window). While it holds, `rimz workflow list` shows detail such as `window 5h.left=12<40`, with its relative age and progress rail.

Put the cheap case in a script and the unclear case in a judge. A script that answers `unsure`, routed to a judge, pays for a model only when it has to.

## When a run needs you

New runs send failures to explicit routes or `[workflow] on-break`. With an agent handler, the run waits at `@break` in that agent's pane: ask there, or fix the cause and resume to the source node. Closing the pane ends the run as failed; resume and cancel leave the pane open.

A legacy run, one whose record has no `route_defaults`, parks when it would otherwise have to guess:

- a step reached `max-visits` or `max-visits-per-outcome`, and `on.cap` defaults or routes to `@user`,
- a check's `timeout` passed,
- a run check exited nonzero or printed a bad answer, or a shell check could not start or its log could not open,
- a judge failed twice,
- a turn step's leader or child blocked on a question or permission prompt, and `on.asked = "@user"` routes it to you,
- the run's `deadline` passed,
- a check routed an outcome to `@user`, with its `reason` as the text.

A condition's unread data normally waits rather than breaking. An explicit `on.unknown` can choose another node or end the run.

A blocking question or permission prompt from a turn step's leader or its subagent keeps the attempt live: answer in the agent's pane. `workflow list` reads `asking you: <handle> (<kind>): <text>`, and the step's `timeout` pauses from when the ask opened until RimZ sees it answered. A subagent's ask or answer is noticed within about a minute. Set `on.asked` to end the attempt on a question instead: RimZ stops the layout and follows that edge, with the question in `prev_attempt.asked` in `context.json`. For a legacy edge to `@user`, fix what made the agent ask (its permission mode, the prompt, or a [sign](#rules-for-writing-nodes)), then resume to relaunch it.

`[workflow] deadline = "2026-11-01"` bounds the whole run. It takes a date (local midnight), a local date and time, or an RFC 3339 instant. When it passes, RimZ stops a live attempt and follows `on.deadline` or on-break; the debugger at `@break` is exempt.

Legacy parked runs raise no sidebar notice; the worktree's workflow strip keeps its yellow `parked` word. They remain under NEEDS YOU in `rimz workflow list` with their addresses and reasons, and in `rimz loop list` when a loop task started them. Blocking questions and capacity holds needing a fix also appear under NEEDS YOU; a staying session that is simply `with you` stays in its workflow group. The [park reasons table](../reference/cli/workflow.md#park-reasons) gives the exact word for each legacy park.

Active and parked runs keep their checkout's sidebar group even while no agent pane exists. A [workflow strip](../interface/sidebar.md#workflow-strips) heads its own section above the team: workflow badge, rail with the current node named, a state word when no step card stands for it, that state's duration, and the run total pinned right. Click it to reach the current step's card when present. The header keeps PR, CI and queue facts live, including the cockpit's open-PR count. The strip and a rowless group vanish when the run ends; a team whose panes closed leaves no finished cards.

Fix what the reason names, then send the run back to work:

```sh
rimz workflow resume "$RUN"                          # the same node, with a fresh timeout
rimz workflow resume "$RUN" --at tests               # another node
rimz workflow resume "$RUN" --extend visits=+2       # raise both visit caps for every step and bucket
rimz workflow resume "$RUN" --extend deadline=2026-11-08
```

The receipt names the resumed node, its visit limit when capped, and any new grant or armed deadline, then gives a `show` command. Resuming a cap park resets that step's total and every outcome/key count. Extending an active run says `extended` and does not warn about visits its current attempt has already used. Either receipt warns if the armed deadline is already past. A deadline fires only once; plain resume after a deadline park continues without one until you grant a new deadline.

A run that is still active takes `--extend` too, without `--at`. An extension is a grant on this run only, and the definition does not change. A visit grant raises both `max-visits` and `max-visits-per-outcome` for every step and bucket. The grant survives resets; the per-outcome bound compares it against counts since the last reset. `show` includes the grant in the limit, such as `visits  fix 3 of 5`. If the step declares `max-visits-per-outcome`, it adds rows such as `  fix 2 of 4 for red` or `  fix 2 of 4 for red key "lint"`, in recorded order. A missing outcome prints `null`, and keys, when set, render as JSON. Otherwise it prints no outcome rows. A per-outcome cap's park view lists only that bucket's session ends and push-backs since its last reset; a total cap lists session ends and push-backs since the last full reset. The trail and full history show `<step> visits reset on <outcome>` when an answer clears counts.

## Stop a run and clean up

To replace an agent step without leaving it, run `rimz workflow restart "$RUN"` for a fresh agent, or add `--continue` to retain its conversation. Both modes work while it is running, held at an `until` check, or parked at the agent step. From a cap or asked park, `--continue` resumes that step's previous session, stopping its card first if it is still live. It keeps the routed input and key, resets the step's visits to 1, skips window and capacity waits, and still respects checkout occupancy. `--continue` refuses before stopping if the conversation cannot resume or belongs to another account's history pool. Switch accounts with `rimz accounts use`, or restart without `--continue` to start fresh. If resuming fails after the old agent stops, new runs follow step `on.invalid`. Legacy runs park at the step, and the park reason in `rimz workflow show` names the fresh restart command. `rimz agents restart @<step-agent>` routes through the same continued workflow restart.

```sh
rimz workflow cancel "$RUN"
```

`cancel` ends the run as `canceled` and stops whatever it is running: the check's script or shell command, the judge, a command step's process group, or every cell of an agent step's layout and its subagents. Its receipt names the address and gives a `show` hint. An already-finished run id prints `<address> is already <state>; unchanged`. The commits and uncommitted edits in the checkout stay, and so does the run directory.

A run started by a condition loop task stays finished, even while its condition holds. To start another, run `rimz loop fire <name>` from that worktree, or start an independent run with `rimz workflow run`. Schedule, signal, and watch tasks can start another run on their next fire; disable the task with `rimz loop disable <name>` to prevent that.

An active or parked workflow run holds its checkout even between steps with no agent there, so automatic cleanup and sweeping leave it in place. When you are done with the run and worktree, `rimz worktree remove feat-x` removes the tree. It refuses while an unfinished run holds it, while the tree has uncommitted changes, while the branch holds commits that have not landed, or while an agent or pane is still in the tree, and names the fix. For a run, the fix is `rimz workflow cancel <run-id>`, or `--force`, which also cancels unfinished runs after removing their checkout. Their history records `worktree removed`. A failed cancellation prints the fix and exits non-zero; the tree stays removed. A check runs in the checkout, so files your suite writes there (a `__pycache__/`, a coverage report) count as changes unless `.gitignore` covers them. `--force` removes it anyway and deletes those commits with the branch. `rimz worktree merge` can still land a run's work without removing its checkout. The [cleanup section](./worktrees.md#cleanup-once-work-lands) covers landing the work first.

A run also ends on its own: as `done` at `@done`, or as `failed` at `@failed`. A finished run leaves `rimz workflow list`, and `rimz workflow show` still reads it for seven days. After that, its run directory becomes eligible for garbage collection and is removed at the next sweep, which runs about once a day per open room. At any ending, including cancellation, RimZ saves the colorless `--history` view as `history.txt` in the notes folder. The headline states when the run ended, and every row clock includes its date, so the saved text stays reproducible and readable later. A failed copy prints a warning without failing the transition. Garbage collection backfills a missing copy before removing the run directory, and keeps that directory if writing fails. The notes and history survive run-directory cleanup and default to permanent retention. `rimz reset` keeps both; `rimz reset --hard` removes both.

## Start runs from a loop

A loop task can start the runs for you. This one, in `~/.rimz/loop.toml`, starts a `babysit-pr` run in every RimZ worktree of one project that has an open PR:

```toml
[tasks.babysit-pr]
root          = "/home/you/code/app"
workflow      = "babysit-pr"
each-worktree = true
when          = ["pr=open"]
for           = "3m"
```

This condition task starts its workflow once per checkout. An active, parked, or finished run prevents another automatic launch there, even if the condition turns false and true again, the room restarts, or you edit the task under the same name. Use `rimz loop fire babysit-pr` from the worktree to start another run after it ends, or `rimz workflow run babysit-pr` for an independent run. A workflow with project scope covers every checkout of the task, including new ones. Schedule, signal, and watch workflow tasks instead admit a new run on each fire unless a live run holds the scope. Write this task by hand, since `rimz loop add` has no flag for it. The task's own `budget` does not apply; agent steps run under the room's budgets, and command steps use no provider. The [loop task reference](../reference/cli/workflow.md#loop-tasks) lists the fields a workflow task takes.

## One run per project

Some work is project-wide: a monthly dependency sweep, or a loop that takes the next item off a shared backlog. Set `[workflow] scope = "project"` for it. Every node then runs at the project root, and the workflow holds one active or parked run for the whole project. Starting it from a worktree still selects the root, and `-w` prints a notice that says so. A loop task with `each-worktree` starts one run and skips the other worktrees.

## Rules for writing nodes

A `summary` describes the whole graph in `[workflow]`, or one check or step in its own table. It is required for a judge check and optional elsewhere. It must be one nonempty line, not instructions to the runner. Blank or multiline summaries warn and are ignored; a judge then refuses as missing at `checks.<name>.summary`, with the fix `add summary = "..."`. Admission checks this for both manual and loop-started runs. Older sealed judges keep running with a fixed agent-judge description.

The engine stays simple because nodes follow four rules.

1. **Make every node safe to repeat.** An agent step launches the same prompt on every visit; a command step runs the same command. Both learn how far things got from the world: the repository, the PR, and the run's notes folder. A check can run twice for one decision, because the process hosting it can die after the script ran.
2. **Say what to do with a dirty tree.** A stopped attempt can leave uncommitted work behind. An agent step's prompt tells the agent to keep it, commit it, or discard it; a command must handle that state itself.
3. **Tune an agent step through its sign, never its prompt file.** A sign is a file, `$RIMZ_WORKFLOW_DIR/signs/<step>.md`, that RimZ adds after the agent step's prompt at every launch until someone deletes it. A judge can write one to steer the next attempt, and so can you before a resume. The prompt file is shared by every run of the workflow.
4. **Keep judges read-only.** A judge reads and answers. Work belongs in a step.

Use a sleep check, not a command that sleeps, for a pause between nodes. It does not count a visit or replace the previous attempt, input, or key.

`$RIMZ_WORKFLOW_DIR/notes/` links to the run's kept notes folder, under the room's `automation-notes/<yyyymmdd>/<HHMMSS>Z-workflow-<workflow>-<lane>/`. Each agent step's launch brief names `<step>-notes.md` there as its working memory across visits, restarts, and its subagents. Write it for someone arriving cold, including changes outside the repository: pushes, PR or issue comments, databases, and remote services. Only that step's agent and its subagents write its file. Set `notes = false` on an agent step to omit that instruction; command steps warn and ignore the field. Runs created before notes support keep their old scratch directory and receive no notes instruction.

RimZ never reads the notes. A check that walks a list can record there the items it finished too. Name each file after the item it records, so a repeat overwrites its own file and never adds a second one. Notes survive run-home cleanup and default to permanent retention; `gc.automation_notes_retention` can expire closed runs' folders, and a hard reset removes them.

### Reflect before a failed ending

A hand-written retry loop often leaves only its last error when it gives up. Add an ordinary agent step to leave a cold-reader account instead. If your status script distinguishes `retry`, `done`, and `failed`, this graph sends a failed answer, an exhausted fix cap, or the run's deadline through `reflect`. Success still ends directly. Save it as `~/.rimz/workflows/fix-and-reflect/workflow.toml`, with your existing status script at `checks/status.sh` and the fixer prompt at `prompts/fix.md`; choose a deadline for your job.

```toml
[workflow]
on-break = "@failed"
scope = "worktree"
start = "result"
deadline = "2026-11-08T18:00:00Z"
on.deadline = "reflect"

[checks.result]
run = "sh checks/status.sh"
on = { retry = "fix", done = "@done", failed = "reflect", invalid = "reflect" }

[steps.fix]
agent = "claude"
prompt-file = "prompts/fix.md"
lifecycle = "turn"
then = "result"
max-visits = 3
on.cap = "reflect"
timeout = "1h"

[steps.reflect]
agent = "reflector"
prompt-file = "prompts/reflect.md"
lifecycle = "turn"
then = "finish-failed"
max-visits = 1
on.cap = "@failed"
timeout = "10m"

[checks.finish-failed]
shell = "true"
on = { pass = "@failed", fail = "@failed" }
```

Use a cheap [profile](./fleet.md#profiles-shape-an-agent-for-one-job) for the reflection, for example `~/.rimz/agents/reflector.md`, on your configured [Claude kind base](../reference/definitions.md#kind-bases):

```markdown
---
description: Summarize a workflow's failed work
agent: claude
model: haiku
tools: [Bash, Read, Write]
---
```

Save this prompt as `~/.rimz/workflows/fix-and-reflect/prompts/reflect.md`:

```markdown
Run `rimz workflow show --history`. Read every step's notes in the folder named by `rimz workflow show`, including your own earlier notes if present. Write only your own reflect-notes.md there. Explain for someone arriving cold what was tried, what failed, what remains, and which changes happened outside the repository. Suggest a concrete improvement to the next run's prompt or checks. Do not modify other steps' notes or change the code.
```

Validate with `rimz workflow validate fix-and-reflect` before starting it. Reflection is part of the graph, not a special RimZ phase: it writes its own notes, then the final check ends the run as failed, preserving the original result. A deadline fires only once, so it does not interrupt the reflection it routed to. The run's `history.txt` is written after that ending; the reflection reads the live history command, not a file that does not exist yet.

### The check answer

A script prints, and a judge writes, exactly one JSON object. A script must also exit 0: a nonzero exit routes to `invalid`, so a script that uses `set -e` must catch the failures it expects.

A shell check constructs its answer for you: `outcome` is `pass` or `fail`, `code` is the exit code (null after a signal), `log` is the absolute log path, and `key` is inherited when present. Only `fail` includes `reason`, either `exited N` or `killed by signal`. Output tails stay out of the history. A check timeout routes `timeout`, and the run's deadline routes `deadline`, never `fail`.

| Field | Required | Meaning |
| --- | --- | --- |
| `outcome` | yes | The edge to follow, from the check's `on` table. |
| `reason` | no | Text for the history, and for legacy parks when the outcome routes to `@user`. |
| `recheck_at` | no | An RFC 3339 time. On `@wait`, nothing runs the check again before it, unless a subscribed signal arrives. |
| `key` | no | A string or integer naming the item or failure the run now works on, such as an issue number or `"lint"`. Later context files carry it. |

Any other field travels too: the whole object becomes the next node's `input`. An edge targets a node name, `@wait`, `@done`, or `@failed`; `@user` is refused. Set `[workflow] on-break` or explicitly route every fireable failure edge (check timeout/invalid, agent launch invalid, message undelivered, visit cap, and workflow deadline). An agent or command step's `then` must name a check or a nonempty list of checks; a message step's `then` names exactly one node, `@done`, or `@failed`. `rimz workflow schema` prints the JSON Schema of `workflow.toml`.

A condition answer carries `outcome`, the zero-based winning `case` (absent for `else` and `unknown`), `readings` (null for unread), and its inherited `key` when present. `unknown` also carries a `reason` naming each unread key. During a hold, `outcome` is `hold` until the case's duration completes. `timeout`, `invalid`, `cap`, `capacity`, `deadline`, `unknown`, and `hold` are reserved: cases, `else`, scripts, and judges cannot choose them; a condition's `on.unknown` may override the default, but `on.hold` is never an edge.

### The context file

Scripts and command steps can read `$RIMZ_WORKFLOW_DIR/context.json` for structured input and the previous attempt. Models use the workflow views instead. The Workflow note's visit numbers match this attempt's `visit` and `visits` below.

| Field | Meaning |
| --- | --- |
| `run`, `node`, `seq` | The run id, the current node, and this evaluation's sequence number. |
| `prompt` | The run's `--prompt`, or null. |
| `input` | The answer that routed here, the engine's object after a cap, timeout, or deadline, or null at the start. |
| `key` | The current item key, or null. |
| `visit`, `visits` | Steps only: the latest launch or push-back's outcome/key bucket number, counted since the last reset at admission, and the step's total since its last manual restart or cap resume. A reset alone does not change the stamped visit; a push-back stamps its newly admitted bucket number. `max-visits-per-outcome` caps `visit`; `max-visits` caps `visits`. |
| `output_path` | Judges only: the file to write the answer to. Null for other checks and steps. |
| `retry` | Judges only: set when the judge runs again after a failed first try, with that try's outcome and reason. |
| `signs_path` | For a step, its sign file, `signs/<step>.md`. For a check, the `signs/` directory. |
| `prev_attempt` | How the last step attempt ended, or null. |
| `live_attempt` | The open `until` session, during checks too: its step, sequence, visits, current answer key and session start time, handle, run id, claim path, starting commit, and last checked turn end. Not published for other lifecycles or staged endings. |

`prev_attempt` remains the last attempt that ended; checking an open `until` session does not replace it. Its `routed_by` names the outcome that admitted the attempt (null for a start step), beside its `key`; its `visit` keeps the latest admitted launch or push-back's bucket number, unchanged by resets alone. During a check, the top-level fields describe that check, while `live_attempt` describes the session you are working with, using the same visit-count rule. A push-back or discard restores the step view; a held `@wait` keeps the check view; a close leaves the file until the next publication.

`prev_attempt.outcome` is `ended`, `died`, `timeout`, `asked`, `limited`, or `canceled`. `asked` occurs only on a turn agent step with `on.asked` set. A staying agent step records no ask or step timeout; a terminal budget failure still reads `limited`. A command step ends as `ended`, `died`, `timeout`, or `canceled`. The object also carries the attempt's start and end times (`started_at`, `ended_at`), the leader's handle, the commit before and after (`head_start`, `head_end`), its cost, and, when they apply, the question it asked (`asked`) and the provider's reset time (`limited.reset_at`). For a command step, `code` is its exit code (null after a signal or an unobserved exit) and `log` is its output file when one exists; agent attempts have both fields null. Agent-only evidence is null for a command step. A command's `head_end` is null when another process, rather than the host that reaped the child, settles the attempt.

## See also

- [Workflow CLI](../reference/cli/workflow.md): every command and flag, the waiting line, park reasons, and the JSON shapes.
- [Loops](./loops.md): loop tasks that start runs, signals, and window conditions.
- [Scripting](./scripting.md): the supervised run each agent step's leader owns.
- [Workflow internals](../internals/harness/workflow.md): how the engine decides, for readers of the code.

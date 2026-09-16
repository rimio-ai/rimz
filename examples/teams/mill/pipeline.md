# The pipeline

Explore → Plan → Implement → Review → Submit → Reflect, owners per the roster.

This is the pipeline for a behavior-preserving pass over code that already works: structure changes, observable behavior does not. What the run delivers is surface removed, counted in source lines, interfaces, flags, and files, so a pass that only moves code sideways has failed even when every stage ran clean.

The plan speaks four terms, and every seat reads them the same way. Module: anything with an interface and an implementation, at any scale. Interface: everything a caller must know or do to reach the common case, measured at the call site. Deep: much behavior behind a small interface; shallow: an interface nearly as complex as what it hides. Seam: where an interface lives, a place behavior can change without editing in place. Each candidate is led by the verb that fixes it: `collapse` two parallel implementations into the one that wins, `delete` vestigial weight, `deepen` a module so callers stop assembling or flagging their way to the common case, `rehome` knowledge or a seam to the one place a change should touch.

The Implement owner commits atomically as it goes, and a move lands in its own commit with the edits to the moved code in the next, so the review reads a move as a move and the three lines changed inside it stand out.

What each stage must leave behind:

## Explore

`explore-notes.md` is what the team knows about the code before anyone proposes a change, and every later stage reads it.

The file carries what downstream cannot get elsewhere, densest first, every entry anchored by path and symbol: the census and the locked target with why; the module map, one line per module, its lines, its interface, and what it hides; how the target behaves today, each claim with the `file:line` or command that shows it; the load-bearing oddities, with the commit that paid for each; the commands that build, run, and test the area; and what was not read, since silence downstream reads as clearance.

Under one owner, the file lands in two steps, each before the ask it feeds: the census and the lock before the target is carried to the user, the deep read before the candidates are.

## Plan

`plan-notes.md` holds one self-standing ordered pass, produced by the owner's craft with its user gates intact, first line `Title:`, then the target in full, a page, since the pass, its review, and the PR's Target shape all read against one destination and not a slice of it. Downstream reads against two more of its parts: the line budget, which Review measures the diff against, and the tests that move, since Review holds any assertion change that list leaves unnamed. A bug found on the way is recorded in the file with its `file:line` for the PR's Advisories, never fixed here.

Nothing worth a pass is a real outcome, not a failure to find one: no plan, what holds and why in the board's Result, and since nothing ships for the user to judge, one ask carrying that verdict and whether to widen the target. A widened target reopens Explore; otherwise the run closes through Reflect, whose file records what the census covered and why nothing earned a pass.

The Plan owner may also amend the upstream file: add what the read missed, correct what the code contradicts, and impose structure so the information is right.

Record the choices the user locked in under the board's Decisions. Then flip Stage and ping the Implement owner.

The plan stays the Plan owner's file for the whole run. When a question or a broken assumption changes it, the owner edits the file, appends a Decision to the board, and pings whoever builds on it, with line ranges when that helps.

Every message the Plan owner sends goes with `--steer`: it carries user intent or a changed plan, and parked it buys a full turn of work against the old truth.

## Implement

Read the board and the upstream files, verify the pass against the real code, and build it; the plan is a strong proposal, not ground truth, and a justified deviation beats faithful implementation of a flaw. Behavior is the one thing that never deviates: a change a caller, a test, or a user can observe leaves the pass and goes to the Plan owner instead, and a bug you find is recorded in `implement-notes.md` with its `file:line` and left standing. A deviation the code justifies, take: record it in `implement-notes.md` with its evidence and build on, no ask. A broken assumption that invalidates a large part of the plan is the one stop: ping the Plan owner and rest until the plan is amended.

The tests the plan pins land as the first commit, green on the base before any structural commit, so the pin proves the old shape and not the new one. The tests the plan moves go in the commit that removes the internals they reached, rewritten or deleted as the plan says.

Before handing off: your craft's verification done, the target's existing suite green on the same commands the plan names, every change committed and the branch rebased onto the current trunk with Skill(rebase), so review diffs against a fresh base.

Your craft's report goes to `implement-notes.md`, never the blackboard, so the Review owner can read the board and the upstream files before their blind pass. It opens with the line count from `git diff --shortstat` against the merge base, source and test paths counted separately, and where tests live inside source files, say so and split those hunks by hand. Then the surface removed: the interfaces, flags, options, and files that are gone. Source lines that grew, or a count short of the plan's budget, are argued there, with the alternative you weighed, or the pass is not ready to hand off. Then flip Stage and ping the Review owner. When a deviation you took changes the design or the intent, ping the Plan owner instead, Stage unflipped, one check for all of them: they record the Decision, flip Stage, and hand off to the Review owner themselves, or amend the plan and ping you back.

After Submit the branch is yours to keep green and current, alone. When the repo carries CI, a failing run reaches you as a signal message with its evidence: run Skill(fix-ci) on it. A trunk that moved under the branch: Skill(rebase), the conflicts resolved by you. Either way: commit, push, a line in `implement-notes.md`, rest. A ping goes out only when the fix changed what a teammate ruled on: behavior the Review owner judged opens their delta round; a design choice the plan made goes to the Plan owner.

## Review

The tree gates the pass: nothing uncommitted beyond the team's memory files. Any other uncommitted change means the hand-off never really happened, so reject it and review nothing: poke the Implement owner to commit and rest. The delta round opens on the same gate. A rejection is not a finding and does not count as a blocking round.

Past the gate the craft runs as written, with two questions this pipeline puts ahead of its angles, both answered from the diff and your own runs, never from the author:

- Did observable behavior change. Run the target's suite yourself first, on the commands `explore-notes.md` names, before any draft: the author's run is a claim in the embargoed file. So is the pin's green on the base: read that commit for a test that reaches what the pass introduced, since one written to the new shape pins nothing about the old one. Then read the diff for moves apart from edits, since a refactor's bug is the three lines changed inside a five-hundred-line move: Skill(branch-diff) marks the lines that moved unchanged `M-`/`M+`, so the plain `-` or `+` inside a run of them is the edit to read. On a rename-heavy diff, separate the mechanical renames from the edits before reading hunks, by whatever normalization the language allows, and check attributes and annotations removed against added: a lost one shows there when no hunk shows it. A difference a caller, a test, or a user can see is blocking, not a judgment call; the removed-behavior audit is the angle that finds it, and an assertion that changed is one however far its test moved, unless the plan's tests-that-move list names that test; a named one you check landed where the plan said.
- Did surface actually shrink. Measure it yourself with `git diff --shortstat` against the merge base, discounting test lines wherever they live, and read the diff for what a reader no longer has to hold. Judge the count against the plan's line budget: growth in source lines is a blocking finding unless a reason holds, and the fix asked for is a cheaper alternative or a narrower scope, never the same shape rewritten; a pass that landed well short of its budget is drift, and the finding names what the plan promised and the diff left standing. A pass that moved code sideways is a rename, and saying so is the review's job.

Its inputs are the board plus `explore-notes.md`, `plan-notes.md`, and the full merge-base diff; the board's Goal is the request. `implement-notes.md` is the author narrative the craft embargoes, its line count and its argument included, so your own measurement comes first and that file is read against it. Verdict and findings go to `review-notes.md`; advisories ride in the PR body.

- Blocking: flip Stage to Implement and ping the Implement owner. They counter-review each finding against the code: fix what holds, push back with `file:line` where one is wrong, refuse one not worth making with the reason, never silently; the Review owner holds a refusal blocking or downgrades it to an advisory. Fixes and rejections land in `implement-notes.md`, commits named by subject, and their ping back opens the delta round. A fix commit that changes the measured deltas re-measures them with the baseline commands on the commit that ships and updates the figures in `implement-notes.md` in the same commit.
- Clear (advisories at most): straight to Submit.

## Submit

The PR is what the run delivers and the one thing the user reads to judge it. Synthesize the body from the board and the stage files, each fact stated once: **Context**, **Target shape** (the module the pass moved toward and what its callers stop needing to know), **Implementation** (deviations from the plan, with why), **Surface removed** (source lines net with tests stated apart, plus the interfaces, flags, options, and files gone), **Advisories** when any are open (advisory findings, accepted refusals, weak spots, the bugs found and left with their `file:line`).

Factual and concise, weak spots left in, no commit hashes. Title: the plan's `Title:` line, refined only if the shipped change outgrew it. Ship with Skill(pr), the synthesized doc as the body: create the PR, or edit an open one in place. Record the PR link, the verification evidence, and the line delta in the board's Result, flip Stage to Reflect and ping its owner; a clear verdict with no PR means Submit is still owed. Advisories ride in the PR body; the Reflect owner may reopen Implement once for the ones worth fixing, all of them in one commit that re-measures the deltas, and the Review owner's delta round pushes and edits the body. A second reopening for a number the first should have re-measured is not made.


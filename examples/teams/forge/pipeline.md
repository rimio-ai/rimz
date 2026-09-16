# The pipeline

Explore → Plan → Implement → Review → Submit → Reflect, owners per the roster.

This is the pipeline for a change worth designing before it is built: a feature, a refactor, a fix whose shape is a design call rather than a one-liner.

The Implement owner commits atomically as it goes.

What each stage must leave behind:

## Explore

`explore-notes.md` is the team's index into the code, written before anyone knows what the diff looks like, every later stage reads it.

Explore opens aimed, never from the raw request: read the request first, then aim the sweep, where to look and the questions it must answer.

Go fast and wide, read shallow, stop when those questions are answered. The file carries what downstream cannot get elsewhere, densest first, every entry anchored by path and symbol: the map, one line per file or symbol in scope and what it owns; how those paths work today, each behavior claim with the command that shows it; the traps, the name that lies, the doc that drifted; the commands that build, run, and test the area; the open questions and whose call each is; and what was not checked, since silence downstream reads as clearance.

Under one owner, the file lands before the gate where you brief the user.

## Plan

`plan-notes.md` holds the plan, produced by the owner's craft with its user gates intact, each raised as an ask, first line `Title:`.

The Plan owner may also amend the upstream files: add what the sweep missed, correct what the code contradicts, and impose structure so the information is right.

Record the choices the user locked in under the board's Decisions. Then `rimz teams flip Implement "plan ready in plan-notes.md; agreed scope recorded"`.

The plan stays the Plan owner's file for the whole run. When a question or a broken assumption changes it, the owner edits the file, appends a Decision to the board, and pings whoever builds on it, with line ranges when that helps.

Every ordinary message the Plan owner sends goes with `rimz message --steer`: it carries user intent or a changed plan, and parked it buys a full turn of work against the old truth. Stage hand-offs use `rimz teams flip` and always wait for the owner's next turn boundary; flip has no interrupt option.

## Implement

Read the board and the upstream files, verify the plan against the real code, and build it; the plan is a strong proposal, not ground truth, and a justified deviation beats faithful implementation of a flaw. A deviation the code justifies, take: record it in `implement-notes.md` with its evidence and build on, no ask. A broken assumption that invalidates a large part of the plan is the one stop: ping the Plan owner and rest until the plan is amended.

Before handing off: your craft's verification done, every change committed and the branch rebased onto the current trunk with Skill(rebase), so review diffs against a fresh base.

Your craft's report goes to `implement-notes.md`, never the blackboard, so the Review owner can read the board and the upstream files before their blind pass. Then `rimz teams flip Review "implementation committed; report in implement-notes.md"`. When a deviation you took changes the design or the intent, `rimz message` the Plan owner instead and leave the stage where it is, one check for all of them: they record the Decision and flip to Review themselves, or amend the plan and ping you back.

After Submit the branch is yours to keep green and current, alone. When the repo carries CI, a failing run reaches you as a signal message with its evidence: run Skill(fix-ci) on it. A trunk that moved under the branch: Skill(rebase), the conflicts resolved by you. Either way: commit, push, a line in `implement-notes.md`, rest. A ping goes out only when the fix changed what a teammate ruled on: behavior the Review owner judged opens their delta round; a design choice the plan made goes to the Plan owner.

## Review

The tree gates the pass: nothing uncommitted beyond the team's memory files. Any other uncommitted change means the hand-off never really happened, so reject it and review nothing: poke the Implement owner to commit — a message, not a flip, the stage stays Review — and rest. The delta round opens on the same gate. A rejection is not a finding and does not count as a blocking round.

Past the gate the craft runs as written. Its inputs are the board plus `explore-notes.md`, `plan-notes.md`, and the full merge-base diff; the board's Goal is the request. `implement-notes.md` is the author narrative the craft embargoes. Verdict and findings go to `review-notes.md`; advisories ride in the PR body.

- Blocking: `rimz teams flip Implement "blocking findings recorded in review-notes.md"`. They counter-review each finding against the code: fix what holds, push back with `file:line` where one is wrong, refuse one not worth making with the reason, never silently; the Review owner holds a refusal blocking or downgrades it to an advisory. Fixes and rejections land in `implement-notes.md`, commits named by subject, and their flip back to Review, `delta round` in the note, opens it.
- Clear (advisories at most): straight to Submit.

## Submit

The PR is what the run delivers and the one thing the user reads to judge it. Synthesize the body from the board and the stage files, each fact stated once: **Context**, **Design choices**, **Implementation** (deviations from the plan, with why), **Advisories** when any are open (advisory findings, accepted refusals, weak spots, follow-ups).

Factual and concise, weak spots left in, no commit hashes. Title: the plan's `Title:` line, refined only if the shipped change outgrew it. Ship with Skill(pr), the synthesized doc as the body: create the PR, or edit an open one in place. Record the PR link and the verification evidence in the board's Result, then `rimz teams flip Reflect "PR submitted: <pr link>; verification recorded in Result"`; a clear verdict with no PR means Submit is still owed. Advisories ride in the PR body; the Reflect owner may reopen Implement once for the ones worth fixing, all of them in one commit, and the Review owner's delta round pushes and edits the body.


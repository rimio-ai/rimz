# The pipeline

Implement → Review → Submit → Reflect, owners per the roster.

This is the pipeline for one small change with no design left to do: a bug, a wrong default, a missing guard, a stale doc line.

The Implement owner commits atomically as it goes.

What each stage must leave behind:

## Implement

The board's Goal is the plan your craft expects: build from your own read of the code, fixing the cause rather than the symptom.

Before handing off: your craft's verification done, every change committed and the branch rebased onto the current trunk with Skill(rebase), so review diffs against a fresh base.

Your craft's report goes to `implement-notes.md`, sized to the change, never the blackboard, so the Review owner can read the board before their blind pass. Then flip Stage and ping the Review owner.

After Submit the branch is yours to keep green and current, alone. When the repo carries CI, a failing run reaches you as a signal message with its evidence: run Skill(fix-ci) on it. A trunk that moved under the branch: Skill(rebase), the conflicts resolved by you. Either way: commit, push, one board line, rest. A ping goes out only when the fix changed behavior the Review owner judged, which opens their delta round; a fix that outgrows the Goal is the user's call, raised as your craft says.

## Review

The tree gates the pass: nothing uncommitted beyond the team's memory files. Any other uncommitted change means the hand-off never really happened, so reject it and review nothing: poke the Implement owner to commit and rest. The delta round opens on the same gate. A rejection is not a finding and does not count as a blocking round.

Past the gate the craft runs as written. Its inputs are the board and the full merge-base diff; the board's Goal is the request. `implement-notes.md` is the author narrative the craft embargoes. Verdict and findings go to `review-notes.md`; advisories ride in the PR body.

Depth stays proportionate to the diff: a small clean change clears in one pass, and a nit dressed as a finding costs more than it catches.

- Blocking: flip Stage to Implement and ping the Implement owner. They counter-review each finding against the code: fix what holds, push back with `file:line` where one is wrong, refuse one not worth making with the reason, never silently. Fixes and rejections land in `implement-notes.md`, commits named by subject, and their ping back opens the delta round. The verdict stays the Review owner's, the leader included: after one counter-review round, hold a refused finding, naming what would satisfy it, or downgrade it to an advisory and ship with the disagreement recorded.
- Clear (advisories at most): straight to Submit.

## Submit

The PR is what the run delivers and the one thing the user reads to judge it. Synthesize the body from the board and the stage files, each fact stated once: **Context**, **Implementation** (departures from the Goal, with why), **Advisories** when any are open (advisory findings, accepted refusals, weak spots, follow-ups).

Factual and concise, weak spots left in, no commit hashes. The Submit owner writes the title. Ship with Skill(pr), the synthesized doc as the body: create the PR, or edit an open one in place. Record the PR link and the verification evidence in the board's Result, flip Stage to Reflect and ping its owner; a clear verdict with no PR means Submit is still owed. Advisories ride in the PR body; the Reflect owner may reopen Implement once for the ones worth fixing, all of them in one commit, and the Review owner's delta round pushes and edits the body.


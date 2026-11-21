# `when` conditions

`when` terms: `key=a,b` matches either value, and `key!=a,b` a known reading outside them; terms join with `&&`, `||`, `!`, and parentheses. Keys:
- `ci`: passed, failed, pending, none
- `pr`: open, merged, closed, none
- `pr.queue`: queued, dequeued (left the queue unmerged), armed (auto-merge on, not yet queued), none
- `pr.review`: draft, changes_requested, required (a required review not yet given), approved, none
- `pr.mergeable`: mergeable, conflicting, none
- `team.stage`: a stage your team defines (case-sensitive), or Done
- `window.5h.left`, `window.7d.left`: the percent left in the room account's usage window, compared with `>=`, `<=`, `>`, or `<`, never `=`; it fires at the window's reset when spent. Use 50 to 80, not 100, which a spend before the check can miss.
A bare `ci`, `pr`, `pr.<field>`, or `team.stage` reads the checkout you arm from; `[branch]` after the key reads that branch and takes whatever PR it finds there, so a reused branch name can read an old PR as merged. `none` means nothing to read: no PR or CI, an open PR outside the queue, a closed PR for a `pr.<field>` term, or no review decision. A PR in the merge queue still reads `pr=open`, so pair a queue term with it. `pr.queue`, `pr.review`, and `pr.mergeable` are GitHub only: elsewhere `pr.queue` reads `none`, and `pr.review` and `pr.mergeable` stay unread until the PR closes. Where the base requires no review, GitHub may report no decision, so `pr.review=approved` may never fire. Forge terms trail the forge by up to five minutes. For a state no term reads, poll the forge's own CLI with `check`.

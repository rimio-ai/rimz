# `when` expressions

- `ci`: `passed`, `failed`, `pending`, `none`
- `pr`: `open`, `merged`, `closed`, `none`
- `pr.queue`: `queued`, `dequeued` (removed from the queue without merging), `armed` (auto-merge on, not yet queued), `none`; GitHub only
- `pr.review`: `draft`, `changes_requested`, `required` (a required review not yet given), `approved`, `none`; GitHub only
- `pr.mergeable`: `mergeable`, `conflicting`, `none`; GitHub only
- `team.stage`: a stage your team defines (case-sensitive), or `Done`
- `window.5h.left`, `window.7d.left`: the remaining percentage (0 to 100) of the usage window of the room's current account for your provider kind, which can differ from the account your own session runs on; compared with `>=`, `<=`, `>`, or `<`, never `=`

Each key but the two `window` keys also takes a branch, as `pr[<branch>]`. The forge keys (`ci`, `pr`, `pr.<field>`) read that branch on the forge and take whatever pull request they find there, so a reused branch name can read an old PR as `merged`; `team.stage[<branch>]` reads the board of the local checkout on that branch. For your own PR, arm from its checkout with bare `pr`.

A `schedule` booking reads its condition at the project root: its bare keys read the root's branch, so name a feature branch as `pr[<branch>]`, and its window terms read the provider of the agent the booking launches.

`none` means the probe found nothing to read: no PR, no CI, an open PR outside the queue, a merged or closed PR for the `pr.<field>` terms, or no review decision. GitHub may report no decision when the base branch requires no review, so `pr.review=approved` may never fire there. A PR in the merge queue still reads `pr=open`, so pair a queue term with it: `pr=open && pr.queue=dequeued`.

A window term fires at the window's reset when it is spent, and at once when enough is left. Use 50 to 80, not 100, which a spend before the check can miss.

`key=a,b` matches either value, and `key!=a,b` matches a known reading outside them. Terms combine with `!`, `&&`, `||`, and parentheses. Each line below is one `wait` call's arguments:

    {"trigger": "when", "value": "pr[feat-a]=merged && pr[feat-b]=merged", "label": "start the integration"}
    {"trigger": "when", "value": "pr.review=approved,changes_requested", "label": "merge or answer the review"}
    {"trigger": "when", "value": "window.5h.left>=60", "label": "start the storage refactor"}

On Gitea, `pr.queue` always reads `none`, and `pr.review` and `pr.mergeable` stay unknown while the PR is open, so a wait that needs one of them never fires before the PR closes.

For a state no term reads, poll the forge's own CLI with a `check` wait.

The forge terms (`ci`, `pr`, `pr.<field>`) come from the room's forge poll, so a wake can trail the forge by up to five minutes, and a PR already merged when you arm fires only after the next poll.

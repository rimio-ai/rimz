# Signals

A signal reaches only the tasks that exist when it is emitted, and is never replayed, so book it before the work that emits it.

## Names

The room emits these families:

- `ci.passed`, `ci.failed`: CI settles on a checkout's commit.
- `pr.opened`, `pr.merged`, `pr.closed`, and on GitHub `pr.behind` (the base moved ahead), `pr.conflicted`, `pr.queued`, and `pr.dequeued`: a checkout's PR changes state.
- `trunk.moved`, `worktree.created`, `worktree.removed`.
- `agent.started|idle|waiting|failed|ended`: one agent's lifecycle.
- `team.idle`, `team.ended`: the whole team settles. `team.waiting`, `team.failed`: one member waits or errors. `team.stage`: the team hands a stage off.
- `wait.<name>`: a `wait` watch settles or checks in.

A lowercase dotted name in any other family is a signal anything can send once the project has a room, open or closed: `rimz events emit deploy.finished --json '{"env":"prod"}'`.

`'ci.*'` matches every member of the family. A booking on an exact name records each sibling as `skipped`, without running its command.

## Scope

A booking gets no default scope: an unscoped `ci.failed` booking fires for every polled checkout. `match` compares top-level payload fields exactly, and all must hold. The usual keys:

- `ci` and `pr`: `branch` or `path`. The payload also carries `repo`, `number`, and `head`.
- `agent`: `handle` (`@planner`, without the channel) or `session`.
- `team`: `team` or `instance`.
- A custom signal: any field of its JSON.

Only the whole value `'*'` is a wildcard: it matches any payload that carries the key.

## CI and PR signals

`ci` and `pr` signals come from the room's forge poll. It polls each linked worktree of the project with a pane in the room, and each checkout whose `ci` or `pr` state a live condition task or an active workflow reads. A `ci` or `pr` signal booking whose `match` names a `branch` literally (not `'*'`) holds that branch in the poll; any other signal booking adds nothing to it.

The poll's first read of a checkout records its state and emits nothing, so a state that already holds then, such as a red CI, never arrives as a signal. For a branch no polled checkout holds, or a state that may already hold, use a `when` trigger (`ci[feat-x]=failed`, guide `rimz-wait/conditions`).

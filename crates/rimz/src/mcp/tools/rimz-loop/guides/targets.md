# What differs at a fire

Every fire runs its target through the `rimz` CLI rather than in-process, so NUL in any string and values that would parse as a flag or a verb (such as a prompt starting with `-` other than a `- ` bullet, or a `rimz agents` verb as `profile`) are refused at booking. So are an agent's `stay: false`, `schema`, and `timeout` and a team's `timeout`: a fire's `rimz agents` or `rimz teams` launch carries none of them.

A missing worktree is created in the project root's repository rather than your checkout.

A `signal` or `when` fire appends its evidence line (``Rule `ci-fix` fired here. ci.failed on feat-x #91 @88824c2``) to an agent's or team leader's prompt and to workflow agent and message steps, and passes it as `RIMZ_LOOP_TRIGGER` to workflow commands and checks, so the prompt need not restate what fired it.

## Agent

A relative `cwd` resolves at the booking call and is stored absolute. A worktree booking already launches fresh into a closed worktree an earlier fire left, so `fresh` is accepted beside `worktree` and stored once. `from_pr` works on a plain profile for both one-shot and repeating bookings; a repeating booking whose target resolves to a team seat (such as `team.role` or a bare role qualified by the root's lane) refuses `from_pr` because a later fire strikes on the closed pull-request worktree.

The fire records `launched` once the launch places the agent, so only a failed launch strikes. A headless run (`-p`), a fresh worktree per fire (a bare `-w`), and permission flags stay with `rimz loop add <name> --agent <profile> ... -- <arguments>`.

## Team

A `#lane` suffix on `team` is refused. The fire records `launched` once the team's agents are placed. Each member runs in the permission mode its role or profile sets. The team stays running past `Done` until `rimz teams stop <team>#<lane>`, holding the fire live until every member ends. In a folder whose trust is undecided, the fired team stops at its folder-trust prompt until someone answers it (`rimz trust grant --agents <kind>` before the first fire settles it).

## Workflow

The fire ends when the workflow run ends: done, or a strike. With no `deadline` in the definition or `arguments`, only each step's timeout bounds the run.

## Not offered here

Messages and `--each-worktree` fan-out with `--once-per-checkout` also stay with `rimz loop add`.

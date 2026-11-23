# What a fire launches

## Agent

`rimz agents profiles` lists the profiles a booking can launch. The profile is the agent's job description, the same on every launch; the `prompt` carries the task for this fire. A signal or condition fire appends its evidence line to the prompt (``Rule \`ci-fix\` fired here. ci.failed on feat-x #91 @88824c2``), so the prompt need not say what fired it.

The fire records `launched` once the launch places the agent, so only a failed launch strikes. The agent then runs in its own pane like any other agent in the room: the user can watch it and type into it. Until it ends, even idle in its pane, the fire stays live, so on a repeating booking say in the prompt when it ends its session, such as once its PR merges. To let every fire's agent stay, set `overlap` to `allow`.

A headless run (`-p`), a fresh worktree per fire (a bare `-w`), and permission flags are launch arguments this tool does not take: the user books those with `rimz loop add <name> --agent <profile> ... -- <arguments>`.

## Team

The `goal` goes to the team's leader; write it like an agent's prompt. The fire records `launched` once the team's agents are placed. The team keeps running after that, past `Done`, until `rimz teams stop <team>#<lane>`, and until every member ends the fire stays live. On a repeating booking, `overlap: replace` stops the earlier fire's team before the new one launches. Each member runs in the permission mode its role or profile sets. In a folder whose trust is undecided, the fired team stops at its folder-trust prompt until someone answers it; `rimz trust grant --agents <kind>` before the first fire settles it.

## Workflow

`rimz workflow show <name>` prints what a run does. The fire ends when the booking's own run ends: done, or a strike. The workflow's own `deadline` bounds the run, and `deadline` in `run.workflow` sets one for this booking; with neither, only each step's timeout applies. A signal or condition fire's evidence reaches each command and check of the run as `RIMZ_LOOP_TRIGGER`, and is appended to the prompt of each agent and message step.

## Not offered here

Each of these stays with `rimz loop add`, which the user runs: a shell command or message (the kinds without a target), `--each-worktree` fan-out with `--once-per-checkout`, and `--project`, a task committed with the repository.

# Debug a workflow run

1. Find it. `rimz workflow list` shows every unfinished run with its node and what it waits on or asks. A run missing there has finished: `rimz workflow list --all` adds the finished runs, and `rimz workflow show '<workflow>#<worktree>'` picks the live run there, else the newest finished one.
2. Read why it stopped. For a waiting run, look its waiting line up below: it clears on its own, waits on the user in an agent's pane, or needs a fix it names. For a failure, `rimz workflow show <run> --history` gives each attempt's exit, log, and reason, and the failed node's cause, which the table below explains.
3. Recover a live run, with the user's approval:
   - `visits`: each entry into a step spends one of the visits `max-visits` caps, and `--at` resets none. Grant more with `rimz workflow extend <run> <step> --visits +N`, or `restart` when the run is at that step.
   - `deadline`: `rimz workflow extend <run> --deadline <when>`.
   - `timeout`, or a node to retry: `resume --at <node>` restarts its timeout at the definition's length; a longer one is an edit to the definition.
   - An agent step held on an error or an interrupted turn, a judge's check over it included: `restart`, with `--continue` to keep its conversation.
   - `died` with a reason naming a fix: the user makes the fix, then `resume --at <node>`.
   - `died` with no `reason:` line: launch the same agent by hand in that checkout (`rimz agents claude -w feat-x -p 'say hello'`) and read what it prints.
   - `invalid`, or `waits on its definition`: fix the script or the definition through skill(rimz-workflow-write).
4. A finished run, `failed` included, stays finished. Make the fix its cause needs first, then start a new run with the old run's flags (propose any change), which gets full visits again, and name the old run's notes folder in its `--prompt` so its steps read what was tried; for `ralph` or `steward`, hand the notes folder to the agent that starts it. The notes folder, which `show` names, holds each step's `<step>-notes.md` and `history.txt`, and outlives the run.

## What each waiting line means

What each waiting or held line of `rimz workflow list` and `show` means, and what to do about it.

- `asking you: <handle> (<kind>): <text>`, under NEEDS YOU, or `with you in @<handle>`: the answer is the user's. Tell them which agent asks what, and that `rimz agents focus <handle>` takes them to its pane.
- `<handle> held <age>: <stop>` (`<step> is held` in `show`): a step's or judge's agent is stopped in its pane.
  - `provider error`, `unknown error`, or `plan lapsed` stays until someone acts, so the run shows under NEEDS YOU. `rimz agents show <handle>` shows the error; the user continues the agent in its pane, or approves a `rimz workflow restart`, with or without `--continue`.
  - `rate limit`, `spend limit`, or `provider overloaded` clears on its own. `budget park` clears on its own only at its cap's `/day` or `/month` reset with auto-continue on; otherwise the user raises the cap or messages the agent.
- A step agent whose last turn was interrupted: its attempt stays open until a later clean turn. Continue it in its pane, or restart it as above.
- `checking <check> <age> in <handle>`: a judge's agent is working. It answers through `rimz workflow submit`, and the run closes its pane.
- `occupied by <handles>`: idle agents' panes hold the step's checkout, and the step starts once those sessions close. When one of them is yours, tell the user.
- `takeover`: an agent in the step's checkout is mid-turn, and `show` names it.
- `unknown: <term> (<why>)`: the check waits for a reading. A CI or PR term reads only once the room's sidebar has probed the forge after the run reached the check, up to five minutes with the room open; bare `ci` reads only once the checkout's head is on its PR and CI has run for that head.
- `window <term>`: waits for the provider's usage window to have the share the definition asks.
- `budget <reason>`: launches once the user raises or clears that budget.
- `needs trust: <problems>`, under NEEDS YOU: an agent step's or judge's launch needs reporting hooks or folder trust fixed. The user makes each fix the detail names, and the next advance launches it; only the run deadline ends the wait.
- `retry: <reason>`: clears on its own at the next advance.
- `runs: <n> of <total> started runs ended`: a runs step waits on its started runs; debug a stuck one as its own run.
- `held · <reason>`: waits for capacity and clears on its own. Under NEEDS YOU with `⊘` (a failing capacity check, low disk, an unreadable store) it needs the fix its reason names.
- `waits on its definition: <error>`: an edit left the definition invalid, and the run holds until it is fixed through skill(rimz-workflow-write).

## Failure causes

A node that fails carries one cause, which the definition can route to another node; where it routes none, the run ends `failed`.

| cause | raised when |
| --- | --- |
| `died` | a process could not start or ended abnormally: an agent launch failed (a judge's included), a turn step's provider exited before its clean turn end, a session exited away from rest, a command or script exited nonzero, a test was killed, a judge's session ended without an answer; or a runs step refused to start its runs, or one of them did not end `done` |
| `invalid` | a script answered something unusable (not one JSON object, an outcome its `on` does not name, a reserved word), or a definition edit left a judge's accepted answer unroutable |
| `timeout` | the node's `timeout` passed |
| `errored` | a judge's own turn ended on a provider error that waiting will not clear |
| `undelivered` | a message step found no single living agent, could not compose its prompt, or found the wrong session |
| `visits` | an entry would exceed `max-visits` plus the step's `extend` grants; nothing launches |
| `capacity` | an agent step's capacity wait gave up, or its start can never fit |
| `deadline` | the run's deadline passed while the run was at the node |

A step that sets `on.errored` routes an error stop as a failure instead of holding.

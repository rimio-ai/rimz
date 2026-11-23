Your scheduler: book a launch for later, and go on at once. A booking has a `name`, a `trigger` that says when, and a `run` that says what: each time the trigger arrives, the booking fires and launches a fresh agent, team, or workflow run in your room. Use it when work should start later, or each time an event or a state arrives. To wake yourself instead, use `wait`. A shell command or a project-wide task is the user's, through `rimz loop add`.

Booking starts work that runs and spends while no one watches, so book what the user asked for, and name its trigger and target back to them.

`trigger` is exactly one of:
{{trigger-values}}
- `every`: a repeating interval (`15m`, `4h`, `1d`), measured from the last fire, so it drifts.
- `cron`: five fields or a macro (`0 9 * * mon-fri`, `@daily`).
- `signal`: an event name or `<family>.*` (`ci.failed`, `pr.merged`), with `match` narrowing it on payload fields. A signal is never replayed and carries no default scope, so an unscoped `ci.failed` fires for every polled checkout: call `guide` with `rimz-loop/signals`.

`when` takes `for`, a duration it must hold. Before you write a `when` beyond `ci=passed,failed` or `pr=merged`, call `guide` with `rimz-wait/conditions`. A booking reads its condition at the project root, so a bare `ci`, `pr`, or `team.stage` reads the root's branch: name a feature branch as `pr[<branch>]`. A window term reads the provider of the agent the booking launches. `at` and `in` fire once and remove the booking. `signal` and `when` fire once unless `repeat` is set. A repeating clock never replays a missed fire; a time that passed with no room open fires late.

`run` is one of `agent` (`profile`, `prompt`), `team` (`team`, `goal`), or `workflow` (the `workflow` tool's arguments, without `input` or `detach`). Write `prompt` or `goal` as a handoff: the goal, and where the result goes. Each file it names must exist at the fire. For what each target does at a fire, call `guide` with `rimz-loop/targets`.

A booking runs at the project root, which may not be the checkout you are in. Give one that writes files a `worktree`. A fire stays live until what it launched ends, an idle agent included, and a trigger during a live fire is skipped unless `overlap` says otherwise: `coalesce` or `queue` hold it, `replace` stops the live fire first, `allow` launches beside it (agents only). On a repeating agent booking, say in the prompt when the agent ends its session. `max_strikes` failed fires in a row disable the booking.

Booking a name you hold replaces it, canceling its live runs that have not acted yet (one past its first step goes on), clearing its strikes and history; a name another starter holds is refused. The answer is the id `schedule:<name>`, with any `warnings`. No message reaches you when it fires. `tasks` lists it and `stop` removes it, leaving runs it started going. To check, pause, test-fire, or debug one, call `guide` with `rimz-loop/managing`.

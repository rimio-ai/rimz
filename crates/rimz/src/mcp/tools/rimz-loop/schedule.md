Your scheduler: book a launch for later, or each time a cron schedule, signal, or condition arrives, and go on at once. Each fire launches a fresh `agent`, `team`, or `workflow` run in your room. To wake yourself instead, use `watch`. A shell command or a project-wide task stays with `rimz loop add`.

Booking starts work that runs and spends while no one watches, so book what the user asked for, and name its trigger and target back to them.

Pick `trigger`; `value` is its argument:
- `when`: a condition, firing when it reads true, already true when set included.
- `in`: a delay, one number and unit (`90s`, `20m`, `2h`, `1d`), rounded up to the next whole minute.
- `at`: a clock time in the configured time zone (the `timezone` setting), the next one ahead: `HH:MM`, `<weekday> HH:MM`, or `YYYY-MM-DD HH:MM`.
- `every`: a repeating interval (`15m`, `4h`, `1d`), measured from the last fire, so it drifts.
- `cron`: five fields or a macro (`0 9 * * mon-fri`, `@daily`).
- `signal`: an event name or `<family>.*` (`ci.failed`, `pr.merged`); never replayed and carries no default scope, so an unscoped `ci.failed` fires for every polled checkout (narrow with `match`; call `guide` with `rimz-loop/signals`).

`at` and `in` fire once and remove the booking; a repeating clock never replays a missed fire, while a time that passed with no room open fires late. Before writing a `when` beyond `ci=passed,failed` or `pr=merged`, call `guide` with `rimz-watch/conditions`.

Required `arguments` keys by `tool`: `agent` (`profile`, `prompt`; no `stay: false`, `schema`, or `timeout`), `team` (`team`, `prompt`, plus top-level `worktree` or one-shot `from_pr`; no `timeout`), or `workflow` (`name`; no `input` or `detach`). Call `guide` with `rimz-loop/targets` for what differs at a fire from a direct launch.

A booking always reads `when` at the project root, which may not be your checkout, and runs there unless `worktree`, `from_pr`, or `cwd` places it: give one that writes files a `worktree`, and branch-scope `when` keys (`pr[<branch>]`). Each file a prompt names must exist at the fire. A fire stays live until what it launched ends, an idle agent included; on a repeating booking, say in `arguments.prompt` when the session ends or set `overlap`.

No message reaches you when a booking fires, so write `arguments.prompt` as a handoff naming the goal and where the result goes. To check, pause, test-fire, or debug one, call `guide` with `rimz-loop/managing`.

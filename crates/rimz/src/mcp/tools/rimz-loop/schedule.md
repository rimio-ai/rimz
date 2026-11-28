Your scheduler: book a launch or a command for later, on a cron schedule, or when a condition comes to hold, and go on at once. Each fire launches a fresh `agent`, `team`, or `workflow` run in your room, or runs a `shell` command. To wake yourself instead, use `watch`. A project-wide task stays with `rimz loop add`.

Booking starts work that runs and spends while no one watches, and a booked command runs outside your sandbox, so book what the user asked for, and name its trigger and target back to them.

Pick `trigger`; `value` is its argument:
- `when`: a condition, firing when it reads true, already true when set included.
- `at`: a delay, one number and unit (`90s`, `20m`, `2h`, `1d`), rounded up to the next whole minute; or a clock time in the configured time zone (the `timezone` setting), the next one ahead: `HH:MM`, `<weekday> HH:MM`, or `YYYY-MM-DD HH:MM`.
- `cron`: five fields or a macro (`*/30 * * * *`, `0 9 * * mon-fri`, `@daily`).

`at` fires once and removes the booking; a repeating clock never replays a missed fire, while a time that passed with no room open fires late. Before writing a `when` beyond `ci=passed,failed` or `pr=merged`, call `guide` with `conditions`.

Required `arguments` keys by `tool`: `agent` (`profile`, `prompt`; no `stay: false`, `schema`, or `timeout`), `team` (`team`, `prompt`, plus top-level `worktree` or one-shot `from_pr`; no `timeout`), `workflow` (`name`; no `input` or `detach`), or `shell` (`command`; `timeout` defaults to the `loop.default-timeout` setting, else `2h`; no `input`, `check_in`, or `on`). Call `guide` with `rimz-loop/targets` for what differs at a fire from a direct launch.

A booking always reads `when` at the project root, which may not be your checkout, and runs there unless `worktree`, `from_pr`, or `cwd` places it: give one that writes files a `worktree`, and branch-scope `when` keys (`pr[<branch>]`). Each file a prompt names must exist at the fire. A fire stays live until what it launched ends, an idle agent included; on a repeating booking, say in `arguments.prompt` when the session ends or set `overlap`.

No message reaches you when a booking fires, so write `arguments.prompt` as a handoff naming the goal and where the result goes. To check, pause, test-fire, or debug one, call `guide` with `rimz-loop/managing`.

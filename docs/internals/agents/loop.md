# Loop tasks

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. Loop tasks ride the supervised-run path in [harness.md](./harness.md#supervised-runs) and the budget-window model in [provider.md](./provider.md).

Loop tasks run one supervised agent turn on an OS schedule. Rimz keeps no daemon; systemd user timers or cron keep time and fire `rimz loop run <name>`, which resolves the recorded project `root`, brings the room up when needed, spawns one transient agent pane through the same path `rimz agents <spec> -p` uses, waits for the root turn, closes the pane, and exits with the run status.

Each task names exactly one agent cell in `spec`: a built-in kind, a profile, or an adapter-supported virtual cell such as `claude-auto`, `codex-yolo`, or `claude-ping`. Team specs, multi-cell layouts, and command cells are rejected at add time because a scheduled task owns one supervised pane.

## Schedule forms

Rimz stores schedule intent in per-machine `agents.toml` and installs scheduler artifacts after a consent preview.

- **Calendar:** `at = "07:00"` with optional `days = "weekdays"`, `daily`, `weekends`, `mon-fri`, or `mon,wed,fri`.
- **Interval:** `every = "15m"`, `2h`, or `1d`; cron uses clean divisor expressions and systemd uses `OnBootSec` plus `OnUnitActiveSec`.
- **Raw cron:** `cron = "*/15 * * * *"`; cron backend only.
- **One-shot:** `once = true` with a calendar or cron schedule. `rimz loop add --in 30m` resolves to a local `at` time and implies `once`.

One-shot tasks remove their scheduler artifact and config row immediately before the supervised run. The run exits the process with the agent status, so cleanup cannot happen afterward. A one-shot removed pre-fire that then fails to launch is not retried.

## Scheduler artifacts

Two backends are supported, selected by `--scheduler` (default auto: systemd when its user manager answers, else cron):

- a **systemd user timer** under `~/.config/systemd/user/rimz-loop-<name>.{timer,service}`, enabled with `systemctl --user enable --now`. Enable lingering (`loginctl enable-linger`) so timers fire while you are logged out.
- the **user crontab**, where each entry is a `# rimz-loop:<name>` fence plus its command line, spliced in idempotently and reclaimed exactly so foreign lines are untouched.

Both run the command through your login shell (`$SHELL -lc`) so the mux and agent binaries resolve on the interactive PATH, with the absolute `rimz` path baked in.

## Carrying the workspace

A scheduler process runs outside any pane, so it has no mux identity pin. Each entry records the absolute project `root` at add time; `loop run` resolves the workspace from that root deterministically, with no pin to read ([ARCHITECTURE.md](../../../ARCHITECTURE.md)).

## Window-priming pings

A task whose `spec` is a `<kind>-ping` virtual cell starts a provider's budget window at a time you choose. The task defaults `prompt = "ping"` and uses lowest effort unless configured otherwise; the virtual cell supplies the adapter's ping arguments.

The window is account-scoped, shared by every session of a provider kind ([provider.md](./provider.md)), so one ping per provider primes the whole account. Before spawning the turn, `loop run` reads the shared rate-limit cache and skips when the shortest window is already counting down. The read is best-effort: an unknown or cold cache falls through to the ping, since missing a window-start defeats the feature while an occasional extra token is cheap.

## Config and code

Loop tasks live in per-machine `[agents.loop.tasks.*]`, outside the trust hash, and each entry runs the rimz-owned `loop run` rather than arbitrary shell. The config shape is in [configuration.md → Loop tasks](../../reference/configuration.md#loop-tasks); the `rimz loop add` / `install` / `uninstall` / `remove` / `list` commands are in [agents.md → Schedule turns with loop](../../reference/cli/agents.md#schedule-turns-with-loop).

The pure schedule parsing, artifact rendering, and crontab reclaim live in `schedule.rs`; the CLI handler in `cli/loop_cmd.rs` owns config editing and OS scheduler glue.

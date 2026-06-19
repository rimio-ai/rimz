# Auto-ping

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. Auto-ping rides the supervised-run path in [harness.md](./harness.md#supervised-runs) and the budget-window model in [provider.md](./provider.md).

Auto-ping starts a provider's budget window at a time you choose. A provider's 5h/7d window is a sliding clock that starts on the first billable token, so a window you open at 10:00 only resets at 15:00. Schedule a ping for 07:00 and the window starts at 07:00 and resets at 12:00 — by the time you sit down it is already two hours from a fresh window.

The window is account-scoped, shared by every session of a provider kind ([provider.md](./provider.md)), so one ping per provider primes the whole account.

## The ping

`rimz autoping run <name>` drives one lowest-effort `ping`→`pong` supervised turn through the same path `rimz agents <kind> -p` uses ([harness.md → Supervised runs](./harness.md#supervised-runs)). It brings the room up if it is closed (`ensure_session` plus the sidebar and presence plugin), spawns a transient agent pane, waits for the root turn, closes the pane, and exits with the run's status code. The card appears, pongs, and clears.

The turn is `<kind>` at `--effort low` with the prompt `ping`, which each adapter renders into its own flag — the same lowest-effort posture as the `<kind>-ping` virtual cell ([agent.md](./agent.md)). Installed and trusted hooks are the completion signal, so the ping needs them just as any supervised run does.

### Skipping a running window

A ping only *starts* a window, so the run first reads the provider's account-scoped budget and skips when the window is already counting down — the token would buy nothing. It reads the shared rate-limit cache the dashboard publishes and projects it to now exactly as the dashboard does ([provider.md → Not-started windows](./provider.md#not-started-windows)), keying on the shortest window: a fresh or idle-refilled window reads as ready to start, so the ping proceeds; one already counting down prints a skip line and exits `0` without spawning a turn. The shared predicate is [`RateLimitWindow::not_started`](../../../crates/rimz/src/agents/context.rs), the same one that paints the dashboard's "ready to start" bar. The read is best-effort: an unknown or cold cache falls through to the ping, since missing a window-start defeats the feature while an occasional extra token is cheap.

## The schedule

Rimz keeps no daemon, so the OS scheduler keeps time. Each entry installs one scheduler job that runs `rimz autoping run <name>`. Two backends are supported, selected by `--scheduler` (default auto: systemd when its user manager answers, else cron):

- a **systemd user timer** under `~/.config/systemd/user/rimz-autoping-<name>.{timer,service}`, enabled with `systemctl --user enable --now`. Enable lingering (`loginctl enable-linger`) so timers fire while you are logged out.
- the **user crontab**, where each entry is a `# rimz-autoping:<name>` fence plus its command line, spliced in idempotently and reclaimed exactly so foreign lines are never touched.

Both run the command through your login shell (`$SHELL -lc`) so the mux and agent binaries resolve on the interactive PATH, with the absolute `rimz` path baked in.

### Carrying the workspace

A scheduler process runs outside any pane, so it has no mux identity pin. Each entry records the absolute project `root` at add time; `autoping run` resolves the workspace from that root, deterministically, with no pin to read ([ARCHITECTURE.md](../../../ARCHITECTURE.md)).

## Config and commands

Schedules live in the per-machine config, outside the trust hash — the only thing an entry runs is the rimz-owned `autoping run`, never arbitrary shell:

```toml
[agents.loop.autoping.schedules.morning]
kind = "claude"        # must support a ping turn (claude, codex)
root = "/home/you/code/app"
at = "07:00"           # 24h local wall-clock
days = "weekdays"      # daily | weekdays | weekends | mon-fri | mon,wed,fri
# cron = "0 7 * * 1-5" # raw cron escape hatch (cron backend only; replaces at/days)
```

- `rimz autoping add <name> --kind <kind> --at <HH:MM> [--days …] [--root .] [--worktree …]` writes an entry (comment-preserving).
- `rimz autoping install [name]` previews the scheduler artifacts, takes consent, and installs them — refusing up front unless the kind's hooks are installed and trusted, so the scheduled run cannot fail silently later.
- `rimz autoping uninstall [name]` reclaims the scheduler entry; `rimz autoping remove <name>` also drops the config entry.
- `rimz autoping list` shows each schedule and whether it is installed; `rimz doctor` carries the same configured-schedule surface.

The pure schedule parsing, artifact rendering, and crontab reclaim live in `autoping.rs`; the CLI handler in `cli/autoping.rs` owns config editing and the OS scheduler glue.

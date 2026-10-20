# Dependabot repair

- [dependabot.py](./dependabot.py): read-only planning and locked `spot` team launches after a six-hour quiet period.
- [prompt.md](./prompt.md): leader instructions, maintainer decisions, and replacement PR body contract.
- [../../.rimz/config.toml](../../.rimz/config.toml): project schedule (every hour; fifteen-minute coordinator timeout; three failure strikes).
- [tests/test_dependabot.py](./tests/test_dependabot.py): duplicate prevention and the attempt-checkout lifecycle against a real Git fixture.

From any checkout of the repository:

```sh
uv run --script loops/dependabot/tests/test_dependabot.py
uv run --script loops/dependabot/dependabot.py plan
```

Both scripts require Python >=3.14; `uv` selects a compatible interpreter from their inline metadata. `run` is the scheduled entry point, started read-only at the project root. Each batch gets a fresh RimZ worktree named after its branch (`deps/repair-…`) and one `spot` team; the fire exits after launch. The launch drops `RIMZ_LOOP_TASK` and keeps the workspace pin, because RimZ refuses a `--bg` launch inside a loop check and the coordinator owns the lane's lifecycle itself. Later fires idle on unfinished lanes, or stop resting seats and sweepers when the board is `Done` and no open PR heads the branch, then reclaim before planning. A tree still in use returns idle for the next fire to retry. Unpushed or uncommitted work is never forced away.

A new batch waits until the newest open Dependabot PR is six hours old, counted from creation. Existing batches are ungated. `plan --quiet-period 0` or a hand-run `run --quiet-period 0` bypasses the delay.

Enable only after the machine-local `spot` team and `sweep` merger loop are ready, with GitHub access and writable cargo caches under the seats' isolation policy. See the [runbook](../../docs/contributing/dependabot-loop.md) for trust, enablement, recovery, and stopping.

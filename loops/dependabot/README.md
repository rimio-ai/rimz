# Dependabot repair

- [dependabot.py](./dependabot.py): read-only planning and locked worker launches, each attempt in a fresh RimZ worktree.
- [prompt.md](./prompt.md): repair-worker instructions and replacement PR identity.
- [../../.rimz/config.toml](../../.rimz/config.toml): project schedule (every 8 hours; two-hour timeout; three failure strikes).
- [tests/test_dependabot.py](./tests/test_dependabot.py): duplicate prevention and the attempt-checkout lifecycle against a real Git fixture.

From any checkout of the repository:

```sh
uv run --script loops/dependabot/tests/test_dependabot.py
uv run --script loops/dependabot/dependabot.py plan
```

Both scripts require Python >=3.14; `uv` selects a compatible interpreter from their inline metadata. `run` is the scheduled entry point, started read-only at the project root. Each repair attempt runs in a fresh RimZ worktree named after the batch branch (`deps/repair-…`), reclaimed by RimZ once its work is pushed.

Enable the project task only after worker GitHub access and writable tool caches are available. See the [runbook](../../docs/contributing/dependabot-loop.md) for project trust, enablement, isolation, recovery, and stopping.

# Dependabot repair

- [dependabot.py](./dependabot.py): read-only planning and locked, worktree-isolated worker dispatch.
- [prompt.md](./prompt.md): repair-worker instructions and replacement PR identity.
- [../../.rimz/config.toml](../../.rimz/config.toml): project schedule (every 8 hours; two-hour timeout; three failure strikes).
- [tests/test_dependabot.py](./tests/test_dependabot.py): duplicate prevention and dispatch verification.

From the dedicated `dependabot-loop` control worktree:

```sh
uv run --script loops/dependabot/tests/test_dependabot.py
uv run --script loops/dependabot/dependabot.py plan
```

Both scripts require Python >=3.14; `uv` selects a compatible interpreter from their inline metadata. The project task uses `dispatch` to locate the `dependabot-loop` control worktree through Git, then replaces itself with `uv run --no-project --script loops/dependabot/dependabot.py run` there. No machine-specific worktree path is committed.

Enable the project task only after worker GitHub access and writable tool caches are available. See the [runbook](../../docs/contributing/dependabot-loop.md) for project trust, enablement, isolation, recovery, and stopping.

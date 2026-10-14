# Repository loop tasks

Each loop task owns one directory under `loops/<task>/`, containing its coordinator, prompt, and focused tests. Declare schedules directly in [.rimz/config.toml](../.rimz/config.toml); no per-task installer is needed. Keep task-specific code inside its directory rather than sharing a scripts or prompts directory. Python helpers run through `uv` and declare Python >=3.14 in their inline script metadata.

Directory separation organizes task code; it does not replace Git worktree isolation. Project check commands start at the project root, where coordinators run read-only. Editing teams get a fresh RimZ worktree per attempt (`rimz worktree new <branch> --base origin/<branch>`, then `rimz teams launch <team> -w <branch>`). Use task-specific schedule names, branch prefixes, and locks so unrelated tasks do not share execution state. Project tasks require trust and machine-local enablement; copying the config into team worktrees does not create another schedule.

Tests live at `loops/<task>/tests/test_*.py`, directly inside `tests/` (not recursively). Each file is a self-contained PEP 723 script. `cargo xtask loop-tests` discovers these files in sorted path order and runs each as `uv run --script <path>` from the repository root, with the inherited environment. A nonzero exit fails the task; all discovered files run even after a failure. No matching files or a missing `uv` is an error. The task runs in `cargo xtask gate`, `cargo xtask ci`, and both forges' required `tests` jobs.

## Tasks

- [Dependabot repair](./dependabot/README.md) — combine failed dependency updates into one replacement PR and resume it without duplicates.

# Repository loop tasks

Each loop task owns one directory under `loops/<task>/`, containing its coordinator, prompt, and focused tests. Declare schedules directly in [.rimz/config.toml](../.rimz/config.toml); no per-task installer is needed. Keep task-specific code inside its directory rather than sharing a scripts or prompts directory. Python helpers run through `uv` and declare Python >=3.14 in their inline script metadata.

Directory separation organizes task code; it does not replace Git worktree isolation. Project check commands start at the project root, where coordinators run read-only. Editing workers get a fresh RimZ worktree per attempt (`rimz worktree new <branch> --base origin/<branch>`, then `rimz agents <profile> -w <branch>`), reclaimed by RimZ once the work is pushed. Use task-specific schedule names, branch prefixes, and locks so unrelated tasks do not share execution state. Project tasks require trust and machine-local enablement; copying the config into worker worktrees does not create another schedule.

## Tasks

- [Dependabot repair](./dependabot/README.md) — combine failed dependency updates into one replacement PR and resume it without duplicates.

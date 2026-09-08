# Repository loop tasks

Each loop task owns one directory under `loops/<task>/`, containing its coordinator, prompt, and focused tests. Declare schedules directly in [.rimz/config.toml](../.rimz/config.toml); no per-task installer is needed. Keep task-specific code inside its directory rather than sharing a scripts or prompts directory. Python helpers run through `uv` and declare Python >=3.14 in their inline script metadata.

Directory separation organizes task code; it does not replace Git worktree isolation. Project check commands start at the project root, so their read-only dispatch step must enter a dedicated linked control worktree before running the coordinator. Coordinators reject the primary checkout and launch editing workers with `rimz agents <profile> -w <task-specific-branch>`. Use task-specific schedule names, branch prefixes, and locks so unrelated tasks do not share execution state. Project tasks require trust and machine-local enablement; copying the config into worker worktrees does not create another schedule.

## Tasks

- [Dependabot repair](./dependabot/README.md) — combine failed dependency updates into one replacement PR and resume it without duplicates.

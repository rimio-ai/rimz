Title: Summarize broken definitions by cause in the `rimz start` notices

## Context

`rimz start` prints one `rimz: <file> cannot be used: ...` line per broken Markdown definition, each 274–402 chars in the handoff's repro (7 lines; 22 in the user's real case), repeating the same boilerplate tail and the same absolute path twice. The user: "I assume the error message shall be more friendly." Intended outcome: with N broken definitions, stderr shows a few short lines that say why (grouped by cause) and which files, and still points at `rimz agents validate` for the full list.

Findings the design rests on (all anchored in `explore-notes.md`, read from the code at f2db685f6):

- `cli/room/start_notice.rs::report_start_notices` maps every `config::broken_machine_files()` entry through `broken_config_notice`; for `ConfigErr::Definition` that is `render::definition_notice(home_relative(path), one_line_error(err))`, and `one_line_error` falls back to the Display `"{path}: {message}"` because `Definition` has no `source` (`config.rs::ConfigErr`). That is the duplicated path.
- Definition errors are strings: `config/definitions/mod.rs::DefinitionErr { path, message }`, copied field-for-field into `config.rs::DefinitionError` for `ConfigNotices.definition_errors`, then into `ConfigErr::Definition { path, message }` by `config.rs::broken_machine_files_in`. Nothing records the cause.
- The two cascade shapes and the missing-skill shape each have one producing site: `definitions/agent.rs::Resolver::resolve_one` (`follows ..., which failed to load`; `allows subagent ..., which failed to load`), `definitions/team.rs::SeatLoader::load` (`selects unknown or failed agent`, one error per role, unknown and failed not distinguished though the site can: the agent is in `self.agents.definitions` or `self.agents.failed` when it failed), and `definitions/agent.rs::skills` (`lists skill '{name}', missing at {candidates}` where the candidates are `<provider skills home>/<name>/SKILL.md` and `<agents_home>/skills/<name>/SKILL.md`; the loop returns at the first missing skill, so one definition yields at most one error).
- `render::definition_notice` has exactly two callers: the start notice and `cli/doctor/render.rs::render_machine_config`; doctor's detail comes from `cli/doctor.rs::collect_machine_config` via the same `one_line_error` fallback, so doctor duplicates the path too.
- `refactor-target.toml` holds a `[[verdict]] key = "config::DefinitionError"` and `[[module]] path = "crates/rimz/src/config" surface-budget = 168`; the pre-commit hook runs `cargo xtask atlas conform --ratchet`.
- Handoff claims checked against the code: all hold. Its "team files emit one error per role" is confirmed at `team.rs::SeatLoader::load` (called per role; errors collected per file).

## Decisions

- **Grouped-by-cause summary** over (a) shortened per-file lines (still N lines) and (b) a single count line (hides the missing skill, which is the fix in the common case). Locked by the planner; the user set no format.
- **Cause typed in the config layer.** `DefinitionErr` gains a `cause` enum set at the three producing sites; the CLI groups on it. Rejected: matching message strings in the CLI (house style: structured over string work; the messages are free to change).
- **Collapse `config::DefinitionError` into `definitions::DefinitionErr`.** The cause has to travel through `ConfigNotices.definition_errors` and `ConfigErr::Definition` anyway, and the two structs are identical; keeping both would mean copying a third field. `ConfigErr::Definition` becomes a wrapper of the whole `DefinitionErr`.
- **Team-role error split.** `SeatLoader::load` distinguishes a failed agent (cause `DependsOnFailed`) from an unknown one (cause `Invalid`); the failed case gets its own message. The existing test `definitions/tests.rs:~581` asserts the unknown-case wording; it moves with whatever wording the implementer picks.
- **Doctor changes too, minimally.** It stays one line per file (it is a detailed surface) but the line loses the duplicated path and the 150-char tail; one `fix:` detail line follows the list. `render::definition_notice` is inlined into doctor and deleted from `render/`. Rejected: leaving doctor as is (same duplicated path, same wall for 22 files, and the shared helper would keep a tail the start notice no longer uses).
- **Names in the summary** are the path relative to the agents home with `.md` dropped (`agents/coder`, `subagents/finder`, `teams/forge`): unambiguous across the three trees, matches what `rimz agents validate` and the file system show. A path not under the agents home (an unreadable tree directory, the agents-home validation error) prints home-relative. A team file's per-role errors dedupe to one name.
- **Name lists are capped** at 6 names then `+N more`: with 22 definitions listing the same skill, the cause and the count are the message; the full list is one `rimz agents validate` away. Rejected: no cap (a 22-name line is the wall again).
- **Out of scope, unchanged:** the "flash" (notices print right before attach; changing attach behaviour is a separate product call, flag it to the user in the PR body), the remote-attach interleaving (not reproduced), `cli/mod.rs::report_definition_errors` for the list commands, the launch-time refusal in `cli/agents_cmd/exec.rs`, non-definition notices (TOML parse/semantic, unknown keys, root class keep their current lines), `rimz agents validate` output (its Display `path: message` and `--json {path, message}` stay; `--json` does not gain the cause in this change).

## Contracts

### Config layer (shared by config, start notice, doctor, validate)

```rust
// crates/rimz/src/config/definitions/mod.rs
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {message}")]
pub struct DefinitionErr {
    pub path: PathBuf,
    pub message: String,
    pub cause: DefinitionCause,
}

/// Why a definition could not load, for surfaces that group failures.
/// `message` stays the full human text; the cause carries only what grouping needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefinitionCause {
    /// `skills:` names a skill absent from every root searched (the provider's skills home, then the library).
    MissingSkill { skill: SkillName, roots: Vec<PathBuf> },
    /// The file is sound; it names a definition that itself failed to load (`agent:`, `subagents:`, or a team role's `agent:`).
    DependsOnFailed { name: String },
    /// Anything else; the message says what.
    Invalid,
}
```

`DefinitionErr::new` keeps its signature and sets `Invalid`; the three producing sites set the other two variants (constructor shape is the implementer's). `roots` are the directories the message's candidates were built from (`<provider skills home>` and `<library>`), not the `SKILL.md` paths.

`config::DefinitionError` is deleted; `ConfigNotices.definition_errors: Vec<definitions::DefinitionErr>` (so `DefinitionErr` needs `PartialEq, Eq`, which `ConfigNotices` derives). `ConfigErr::Definition(DefinitionErr)` with `#[error("{0}")]`; `ConfigErr::path()` and `validation_message()` read through it. `broken_machine_files_in` moves the `DefinitionErr` in whole. Binary-crate constructors (`cli/agents_cmd/validate.rs::load`, `harness/plan/tests.rs`, `cli/agents_cmd/tests.rs`, `start_notice.rs` tests) build the new struct.

### `rimz start` stderr (what the user sees)

For the definition subset of `broken_machine_files()`, replacing the per-file lines; every other notice keeps its current line and order (definition summary first, where the per-file lines were). Shape, with the repro fixture (5 missing `rimz-lsp`, 2 cascades) plus a hypothetical team:

```
rimz: 5 definitions list skill 'rimz-lsp', which is not installed in ~/.claude/skills or ~/.rimz/skills: agents/coder, agents/planner, subagents/finder, subagents/newcomer, subagents/surveyor
rimz: 3 definitions depend on one that failed: agents/astra, agents/reviewer, teams/forge
rimz: 1 definition has another error: agents/broken
rimz: launches that select them are refused until the files are fixed; `rimz agents validate` lists every error
```

Fixed by this contract: one line per distinct missing skill (count, skill name, the union of `roots` across the group home-relativized, names); one line for all `DependsOnFailed` (count, names); one line for all `Invalid` (count, names); one tail line that names the consequence and `rimz agents validate`; a line whose group is empty is omitted; names are agents-home-relative without `.md`, sorted, deduplicated per path, at most 6 then `+N more`; counts are distinct files, not errors. Exact wording is the implementer's. `rimz:` prefix and stderr as today.

### `rimz doctor` MACHINE CONFIG

Per definition problem: `<home-relative path> cannot be used: <message>` (message only, no absolute path repeated, no tail). After the list, when at least one definition problem exists, one `detail` line in the `fix:` idiom already used under HOME: names `rimz agents validate` and that launches selecting these definitions are refused until they pass. Parse/Semantic lines unchanged. `--json` (`MachineConfigProblem.error`) carries the message only for definitions.

## Rules

- No start-notice or doctor line for a definition contains the definition's absolute path twice: grep the captured repro output for the sandbox home path count per line.
- With N definition errors over D distinct missing skills, `report_start_notices` emits at most D + 3 definition lines. Proven by a unit test on the pure summary builder (factored out of the stderr write in `start_notice.rs`; signature is the implementer's) with ~7 errors: 5 `MissingSkill` on one skill (mixed `agents/` and `subagents/` paths), 2 `DependsOnFailed`, and a `teams/x.md` path carrying two `DependsOnFailed` errors: asserts 3 lines, the skill line names the skill, the roots home-relativized, and all 5 names; the dependency line names `teams/x` once; the tail names `rimz agents validate`; no line exceeds ~200 chars.
- Cap test: 9 files on one missing skill → 6 names then `+3 more`.
- Classification tests in `config/definitions/tests.rs` (extend the existing missing-skill, `follows ... failed`, `allows subagent ... failed`, and team-role tests): each asserts the `cause` variant and its payload (`skill`, `roots`; `name`). The team test that today expects `selects unknown or failed agent 'missing'` asserts `Invalid` for the unknown case, and a new case (role selecting an agent whose own file fails) asserts `DependsOnFailed { name }`.
- `start_notice.rs` tests: `broken_config_notice_is_one_line_and_names_the_fallback` unchanged; `broken_definition_notice_names_launch_precondition_not_fallback` is replaced by the summary tests above (there is no per-definition notice any more).
- `doctor/render/tests.rs::machine_config_definition_problem_names_launch_precondition`: asserts `cannot be used`, the message, the `fix:` line naming `rimz agents validate`, and the absence of the old tail.
- `rg 'definition_notice' crates/` matches nothing after the change (helper deleted); `rg 'DefinitionError\b' crates/ refactor-target.toml` matches nothing (type and its verdict gone).
- `cargo xtask atlas conform --ratchet` passes: `DefinitionCause` is a new `pub` item in `config/`; if the module budget trips, paste the block the ratchet prints for `crates/rimz/src/config`, and drop the `config::DefinitionError` verdict.
- Docs: `docs/guide/troubleshooting.md` "RimZ cannot parse a config file" gains the definition summary sentence (start prints a few lines grouped by cause and `rimz agents validate` lists each file and error); no other page quotes the notice. `cargo xtask docs-links` passes.

## Reuse

- `cli/render/mod.rs::home_relative` / `home_relative_path` for every path in the summary and doctor lines.
- `rimz::disk::paths::agents_home()` for the base the summary names files against (same base `validate.rs::run` uses).
- `config/skills.rs::SkillName` for the typed skill in the cause.
- `doctor/render.rs::note` and `detail` for the doctor lines.

## Order

1. Config layer: `DefinitionCause`, `DefinitionErr.cause`, the three producing sites (including the team-role split), delete `DefinitionError`, rewrap `ConfigErr::Definition`, fix constructors and tests; `refactor-target.toml` adjustments in the same commit so the hook passes.
2. Start notice: summary builder + tests; delete the per-definition path in `broken_config_notice_for` (the `fragment` flag goes with it).
3. Doctor: message-only detail, inline and delete `render::definition_notice`, `fix:` line, test.
4. Guide sentence.

One PR; commit split is the implementer's.

## Verification

- `cargo xtask check`, `cargo xtask lint`, `cargo xtask docs-links`, then `cargo xtask gate` before hand-off.
- Focused: `cargo xtask test 'cli::room::start_notice'`, `cargo xtask test 'cli::doctor::render::tests'`, `cargo xtask test 'config::definitions::tests'`, `cargo xtask test 'config::tests'`.
- End to end: point `/var/tmp/handoff-start-notice-repro-script.md` at this worktree's `target/debug/rimz`, run `cargo xtask sandbox -- sh <script> > /tmp/scratchpad/out.txt 2>&1`, and check the first lines match the start-notice contract (2 definition lines + tail, then the root-class line); then `rimz doctor` in the same sandbox home for the MACHINE CONFIG shape.

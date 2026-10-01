# Explore: `rimz start` definition notices

Aim: make the per-definition `rimz: <file> cannot be used: ...` lines that `rimz start` prints on stderr short and friendly. Handoff with the verified repro: /var/tmp/handoff-start-definition-notices.md (script at /var/tmp/handoff-start-notice-repro-script.md, captured output at /var/tmp/handoff-start-notice-repro-output.md). Every claim below was read from the code at f2db685f6 unless marked `handoff`.

## Map

- `crates/rimz/src/cli/room/start_notice.rs::report_start_notices` — the `rimz start` stderr notices: every `rimz::config::broken_machine_files()` entry through `broken_config_notice`, then unknown-key notices, then the root-class line; one `rimz: {notice}` line each, no grouping, no cap. Called once, `cli/room/mod.rs::run` (the start handler) right before `enter_room`, so the lines scroll away when the mux attaches.
- `start_notice.rs::broken_config_notice_for` — for `ConfigErr::Definition` the detail is `render::one_line_error(err)`; `Definition` has no `source`, so that is the Display `"{path}: {message}"` (`config.rs::ConfigErr`, `#[error("{path}: {message}")]`). That is the second, absolute copy of the path after the home-relative one. Non-definition errors render `"{path} is unparseable/invalid — ... built-in defaults apply: {detail}; fix the file, then restart"`.
- `crates/rimz/src/cli/render/mod.rs::definition_notice` (`pub(super)`) — `"{path} cannot be used: {detail}; `rimz agents`, `rimz subagents`, and `rimz teams` refuse to launch it until the definition is fixed; run `rimz agents validate`"`. Two callers: `start_notice.rs::broken_config_notice_for` and `cli/doctor/render.rs::render_machine_config` (MACHINE CONFIG section, one `note` per problem).
- `crates/rimz/src/config.rs::broken_machine_files` / `broken_machine_files_in` — strict parses of core/theme/loop TOML, then `MachineConfig::load_lenient_from(..).notices.definition_errors` mapped to `ConfigErr::Definition { path, message }`. Doc comment: "Runtime loading remains lenient; this feeds the start notice and `rimz doctor`". Callers: `start_notice.rs::report_start_notices`, `cli/doctor.rs::collect_machine_config` (grep `broken_machine_files(`).
- `config.rs::ConfigErr::validation_message` (private) — returns `message` alone for `Definition`.
- `config.rs::ConfigNotices.definition_errors: Vec<DefinitionError>` and `config.rs::DefinitionError { path, message }` — a field-for-field copy of `config/definitions/mod.rs::DefinitionErr { path, message }`; `MachineConfig` builds one from each `LoadedDefinitions.errors` entry (config.rs ~703) and one more from `validate_agents_file` (config.rs ~708, path = agents home, message = `validation_message()`). `refactor-target.toml` carries a `[[verdict]] key = "config::DefinitionError"` ("named by the binary crate's test modules, so it stays `pub`"); the binary-side constructors are `harness/plan/tests.rs:~407` and `cli/agents_cmd/tests.rs:~2495`.
- `crates/rimz/src/config/definitions/mod.rs::DefinitionErr` — `{ path: PathBuf, message: String }`, `#[error("{path}: {message}")]`, constructor `DefinitionErr::new(path, message)` collapses whitespace. 58 `DefinitionErr::new` sites: agent.rs 26, team.rs 15, frontmatter.rs 7, mod.rs 7, traits.rs 3. Strings only: nothing records why a definition failed.
- `definitions/mod.rs::LoadedDefinitions.failed: BTreeMap<String, BTreeSet<PathBuf>>` — name → source paths of failed definitions; `MachineConfig::definition_failure_for` joins it with `definition_errors` for the launch refusal.
- Cascade error sites (a sound file naming a definition that itself failed):
  - `definitions/agent.rs::Resolver::resolve_one` (~169): `follows `{parent}`, which failed to load` when the parent is in `tree.definitions` or `tree.failed` but does not resolve.
  - `agent.rs` (~260): `allows subagent '{name}', which failed to load` when the name is in the foreign namespace (`definitions` or `failed`) but not in `allowed_children`.
  - `definitions/team.rs::SeatLoader::load` (~276): `team '{name}' role '{handle}' selects unknown or failed agent '{agent}'; team roles can select definitions from agents only` — one error per role, and it does not distinguish unknown from failed. The split is available at the site: `self.agents.definitions.contains_key(&role.agent) || self.agents.failed.contains(&role.agent)` means failed, else unknown. `definitions/tests.rs:~581` asserts the exact message for the unknown case (`'missing'`).
- Missing-skill site: `definitions/agent.rs::skills` (~467): `lists skill '{name}', missing at {a} and {b}` where the candidates are `<provider skills home>/<name>/SKILL.md` (from `AgentDefinition::skills_home(env)`, e.g. `~/.claude/skills`) then `<library>/<name>/SKILL.md` (`<agents_home>/skills`). The loop returns at the first missing skill, so one definition yields at most one error. `SkillName` is `config/skills.rs::SkillName`.
- `crates/rimz/src/cli/doctor.rs::collect_machine_config` — classifies each `ConfigErr` into `MachineConfigProblemKind::{Definition, Parse, Semantic}` and stores `config_file_error_detail(&err, ..)`, which for `Definition` is `one_line_error` → the Display, so doctor's per-file line also carries the duplicated absolute path. `doctor/render.rs::render_machine_config` renders Definition through `definition_notice`; `doctor/render/tests.rs::machine_config_definition_problem_names_launch_precondition` asserts `cannot be used` and the full tail text. `doctor/render.rs::detail` writes an indented follow-up line (used as `fix: ...` under HOME).
- `crates/rimz/src/cli/agents_cmd/validate.rs::run` — the detailed surface: prints each `DefinitionErr` via Display (`path: message`) on stdout, `--json` emits `{path, message}`; `validate.rs::load` merges `machine.notices.definition_errors` into `loaded.errors` by constructing `DefinitionErr { path, message }` when path+message is unseen. Unchanged by this work except the constructor.
- `crates/rimz/src/cli/mod.rs::report_definition_errors` — the list commands' stderr line per error (`rimz: {path}: {message}`), no tail. Out of scope.
- `cli/render/mod.rs::home_relative` / `home_relative_path` — `~/` abbreviation used by the current notice for the leading path only.
- Docs: `docs/guide/troubleshooting.md` "RimZ cannot parse a config file" (~352-354) is the only guide text describing start-time notices: TOML files warn on stderr; "Markdown definitions fail differently ... `rimz agents validate` names the file and the error". `docs/guide/configuration.md:~96` says `rimz doctor` reports both kinds and points at validate. No doc quotes the `cannot be used` wording (grep `cannot be used|refuse to launch` in docs: none).

## Behaviour today (from the captured repro output)

7 definition lines of 274–402 chars, then the root-class line. Each: `rimz: ~/.rimz/subagents/finder.md cannot be used: /tmp/.../home/.rimz/subagents/finder.md: lists skill 'rimz-lsp', missing at /tmp/.../.claude/skills/rimz-lsp/SKILL.md and /tmp/.../.rimz/skills/rimz-lsp/SKILL.md; `rimz agents`, `rimz subagents`, and `rimz teams` refuse to launch it until the definition is fixed; run `rimz agents validate``. Five are the same missing skill; two are cascades (`allows subagent 'finder', which failed to load`).

## Traps

- `ConfigErr::Definition`'s Display already carries the path; `one_line_error` falls back to Display when there is no `source`, which is what duplicates it. `validation_message` is the message-only accessor but private.
- `config::DefinitionError` and `definitions::DefinitionErr` are the same shape under two names; validate.rs builds one from the other.
- A team file emits one error per role (team.rs `SeatLoader::load` per role, errors collected per file), so a broken team with 3 roles is 3 entries for one path.
- Adding a `pub` item to `config/` trips `cargo xtask atlas conform --ratchet` (pre-commit hook) until `refactor-target.toml` admits it (`[[module]] path = "crates/rimz/src/config" surface-budget = 168`); removing `config::DefinitionError` leaves a stale `[[verdict]]` for it.
- Handoff claim checked: the "flash" (notices printed right before attach) is real by placement (`cli/room/mod.rs::run` line before `enter_room`); remote interleaving was not reproduced by the handoff and not traced here.

## Commands

- Build/tests: `cargo xtask check`, then `cargo xtask lint`; `cargo xtask docs-links` for Markdown; `cargo xtask gate` before hand-off.
- Focused: `cargo xtask test --name <exact test name>`; module: `cargo xtask test 'cli::room::start_notice'`, `cargo xtask test 'cli::doctor::render::tests'`, `cargo xtask test 'config::definitions::tests'`.
- Repro: `cargo xtask sandbox -- sh /var/tmp/handoff-start-notice-repro-script.md > "$TMPDIR/out.txt" 2>&1` after pointing the script's binary path at this worktree's `target/debug/rimz`.

## Open questions

- None for the user; presentation format is the planner's design (handoff).

## Not checked

- Remote-attach interleaving of the notice with `rimz: remote room ... ended`.
- Whether Zellij's attach clears the pre-attach stderr differently from tmux (the "flash"); out of scope per handoff.
- `frontmatter.rs` and `traits.rs` error sites individually (all go through `DefinitionErr::new`, so a default cause covers them).

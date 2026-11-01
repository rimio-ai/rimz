# CLI scenes

> See [rust-conventions.md](./rust-conventions.md#cli-shape) for how the CLI is parsed and [the CLI layer contract](../../crates/rimz/src/cli/AGENTS.md) for what a handler may own. Its [human-history rule](../../crates/rimz/src/cli/AGENTS.md#human-history-lists) covers folded repeats and one fact stated once.

A scene is a top-level command that owns one kind of thing and the verbs over it: `rimz agents`, `rimz teams`, and `rimz subagents`. `rimz agents` is the reference design; `rimz teams` follows it. A change to a scene keeps the six rules below true, and a new scene starts from them. The command forms each scene prints live in its reference page ([agents.md](../reference/cli/agents.md), [teams.md](../reference/cli/teams.md)); the shared exit codes live in [cli.md](../reference/cli.md#exit-codes).

## The list opens with attention

A scene's `list` leads with what needs a person, before the inventory. The rows come from one library projection, `crates/rimz/src/store/snapshot/view/model.rs::needs_you`: the sidebar's attention status (waiting, failed, or paused, lifted when a current child of either origin waits or a provider-native child fails), else the rowless status under the room's park demotion. Its age is the open ask's `since`, else the row's last activity. `rimz agents list` and `rimz teams list` render those rows with `crates/rimz/src/cli/render/mod.rs::needs_you`. `rimz teams show` takes each member's row from the same catalog as `teams list`, and `rimz teams wait` reads the projection on every poll. All four therefore agree with each other and with the sidebar by construction.

A scene never filters on `AgentStatus::is_attention()` itself, and never classifies by counting status strings. A team whose board owner holds no live seat adds a `not live` row; owner liveness is `crates/rimz/src/address.rs::role_holders` over the cohort's live members, the rule flip delivery already applies.

## One described address shape

Each scene names its target one way and says so in help. Agents take `@handle[#channel]`, and their root lane spells `#main`. A team cohort is `team#lane` (`forge#feat-x`); a root-checkout cohort launched without `--channel` has lane `<dir>/<team>` (`forge#api/forge`), not `#main`. Every verb that acts on one cohort takes that positional, and a flag that selects the same thing (`-w NAME`) is described as the same selection rather than as a second address. Help prints the positional's meaning, not a bare metavariable.

## Wrong input exits 1, a bad command line exits 2

- **Exit 2** (`crates/rimz/src/cli/usage.rs::UsageError`, mapped in `crates/rimz/src/main.rs::error_exit_code`): the command line itself cannot mean anything. A bare scene, a positional that is neither a verb nor a configured target, a launch prompt that is exactly a verb (`rimz teams forge stop`), conditional argument shapes clap cannot express.
- **Exit 1**: the command line parsed, and the thing it names is not there. An unknown lane, a cohort with no live members in that lane, a leader that is not live. The error lists the valid values (`live cohorts are in #a, #b`), or the command that would make one exist (`rimz teams resume forge`).
- **Exit 0** only when the requested effect happened. A launch that started no agent and delivered no prompt exits 1, whether the reconciler refused unattended, the user canceled, or the target was already gone; the one exception is focusing a live cohort when no prompt was given.
- A command that answers a question (`rimz loop explain`, a future `--check`) exits 0 when the answer is yes, 1 when it is no, and 2 when the question itself does not parse, so a script can tell "false" from "typo" by status alone.

A bare scene's refusal puts the list command on line 1, since a truncated read keeps the first line, followed by the scene's grouped orientation (`crates/rimz/src/cli/usage.rs::UsageError::missing_scene_verb`, `crates/rimz/src/cli/help.rs::scene_orientation`). Scene help and that refusal share the one orientation, grouped See, Drive, Start, Define; launch flags live under `<scene> launch --help` (`crates/rimz/src/cli/help.rs::customize_scene`).

## Actions report what they hit and the next command

A verb that changes something prints the target it acted on, by address, and the command a person would run next. A prompt delivered to a live cohort prints the shared dispatch receipt (`crates/rimz/src/cli/send.rs::report_dispatch`) plus the line saying the cohort was already running; stop and restart head with `stopped forge#lane (N members)`; a stop that leaves a pending board names its stage and the addressed resume and `flip ... Done` commands. A refusal that preserves the user's input carries it into the suggested command, quoted with `crates/rimz/src/cli/usage.rs::shell_command`.

Progress for a long wait goes to stderr in both human and `--json` modes, so stdout stays the result.

## A bare word never silently launches

A scene's positional launch (`rimz agents <SPEC>`, `rimz teams <TEAM>`) is a shortcut for `launch`, and a word that could mean something else refuses with exit 2 and both readings. A word naming a live agent points at `show` and at the explicit launch. An unresolved agents-scene word refuses with exit 2 and nearby verbs or launch names, or the compact launch catalog when none is close (`crates/rimz/src/cli/agents_cmd/mod.rs::run`). A team-scene word that is neither a command nor a configured team suggests nearby verbs or teams, otherwise lists both sets (`crates/rimz/src/cli/teams/mod.rs::guard_launch`). Explicit `launch` excludes verbs from suggestions, keeps exit 1 for an unknown target, and is the escape that sends any prompt literally.

## One vocabulary

User-facing text in a scene uses the scene's nouns and nothing older. For teams: a *cohort* is one team's run on a lane, addressed `team#lane`; a *lane* is `#lane`. "Instance" stays in JSON keys and serde names, which scripts read, and leaves every human string. A hint addresses a cohort as `team#lane`, never as `-w <worktree>`. When a change renames a noun, grep the old one by stem across `crates/rimz/src/cli`, `docs/`, `crates/**/AGENTS.md`, and rustdoc in the same change.

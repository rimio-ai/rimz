# Markdown definitions

RimZ reads machine agent, subagent, and team definitions directly from Markdown with YAML frontmatter. Edit the source, then run `rimz agents validate`; there is no generation step. Machine preferences and command shortcuts remain in `config.toml`. Trusted project `.rimz/config.toml` keeps its TOML schema and trust rules.

## Trees and format

The definitions root is the RimZ home, `~/.rimz` by default, which `RIMZ_HOME` relocates; `RIMZ_AGENTS_HOME` overrides it for these trees and the skill library alone:

```text
agents/<name>.md       direct profiles and kind bases
subagents/<name>.md    supervised-child profiles
teams/<name>.md        team roster and pipeline
traits/<name>.md       plain Markdown fragments
skills/<name>/SKILL.md shared skill library
```

The three definition trees are scanned non-recursively for `.md` files, in sorted order. Subdirectories and `AGENTS.md`, `CLAUDE.md`, and `README.md` are skipped. A missing tree is empty. Traits are read when referenced. Symlinked trees work too.

The first line must be exactly `---`; another exact `---` line closes a YAML mapping. Unknown keys and wrong value types fail. The remaining Markdown is the prompt body, trimmed at its edges. String-list keys accept YAML lists; an explicit null list means `[]`, not inheritance. Signals have their own rules below.

For example, `agents/planner.md`:

```markdown
---
description: Plan changes without editing files
agent: claude
model: fable
tools: [Bash, Read, Grep, Glob, AskUserQuestion]
subagents: []
---
Read the surrounding code and propose a bounded plan before implementation.
```

Because this example has a body, it also needs `agents/claude.md`:

```markdown
---
description: Shared Claude instructions
---
Follow the project's instructions and report what you verified.
```

## Agent and subagent keys

| Key | Meaning |
| --- | --- |
| `name` | Optional public name; defaults to the filename stem. |
| `description` | Required nonempty, single-line listing description; not inherited. |
| `agent` | Registered kind or another definition in the same tree. May be omitted when `model` identifies a kind. |
| `model` | Model alias or provider model ID. |
| `mode` | `ask`, `auto`, `plan`, or `yolo`. |
| `isolation` | `host` or `sandbox`; overrides machine policy unless `--isolation` is recorded. Inherited through definitions, unavailable on team roles or repository profiles. |
| `effort` | Provider-specific effort string. |
| `auto-compact` | Native compaction window: integer or string token count from `100k` through `1M`, inclusive; no percentages or decimals. |
| `budget` | Dollar cap as string or number; a string such as `20/day` selects a daily cap. |
| `model-reminder` | Boolean controlling the model identity in the launch reminder. Defaults on. |
| `traits` | Trait names substituted into this definition's body. |
| `tools` | Tool-name list, translated to provider arguments below. |
| `subagents` | Direct profiles only: allowed RimZ child profiles; `[]` permits none. |
| `skills` | Bare skill-directory names allowed for model invocation. Sandbox views and host Claude keep unlisted skills user-only; host Codex hides them. Other host providers warn and run unrestricted. `[]` lists none. |

Names must match `[A-Za-z0-9_-]+` and be unique across both definition trees. Registered kinds are reserved for kind bases. `general` and model aliases are not reserved by the definition parser. The final namespace checks also reject command-verb and address collisions, such as `all`, kind ordinals, or a profile colliding with a team.

### Chains and defaults

`agent:` follows only definitions in its own namespace: an agent cannot inherit a subagent or vice versa. The chain must end at a registered kind. Without `agent:`, a recognized model alias, model ID, or prefix determines the kind: Claude's roster and `claude-` prefix, and Codex's roster and `gpt-` prefix. Unknown models need an explicit kind. A model implying a different kind from the chain is an error.

Children inherit `model`, `mode`, `isolation`, `effort`, `auto-compact`, `budget`, `tools`, `skills`, `subagents`, and `model-reminder`. An explicit child field replaces the inherited value; lists never concatenate. Descriptions and trait lists are local. Parent prompt bodies are retained in order before the child's body. A bodyless definition adds no craft, so it can serve as a model-only preset on its kind base.

Defaults apply after inheritance: Claude uses mode `auto` and effort `xhigh`, except `fable` defaults to `high`; Codex and Pi default to effort `xhigh`. Codex aliases expand as `astra` → `gpt-6-astra`, `sol` → `gpt-6-sol`, `luna` → `gpt-6-luna`, and `terra` → `gpt-5.6-terra`. A kind whose adapter declares native auto-compaction support defaults to `258k`; other kinds have no default window. Definitions are rejected at load if their resolved kind cannot render a supplied or inherited `model`, `effort`, or `auto-compact` field. Defaults are filled only when supported.

### Kind bases

`agents/<registered-kind>.md` is a special base, not an ordinary definition. It accepts only `description`, requires a nonempty body, and supplies the replacement system prompt in both namespaces. It cannot pin a model or tools. Every team seat, every definition with a craft, and every definition of a kind that takes a system prompt (claude, codex, pi, qwen, and plugins that declare one) requires the base of the kind it runs on, body or not; a home with no definitions needs no base.

### Traits

`traits: [careful, concise]` reads `traits/careful.md` then `traits/concise.md`, deduplicating first-seen names. Names cannot be empty, `.` or `..`, or contain path separators. These files are plain Markdown: their complete trimmed contents are joined with blank lines and replace the first `${traits}` in the body. Later tokens collapse to a blank line. With no traits, the token disappears. Naming traits without a token is an error. Other `${...}` placeholders survive unchanged; the team pipeline is not trait-substituted.

## Tools to provider arguments

Tool entries use Claude-style names. The base before `(` is trimmed and deduplicated in first-seen order; it must begin with an ASCII letter and contain only ASCII letters, digits, or underscores. Parentheses must balance without nesting. Only `Agent(...)` interprets its suffix, as a comma-separated native child-type allowlist. Other suffixes are discarded; this is not a permission-pattern language.

Quote entries containing commas in a YAML flow list: `tools: ["Agent(Explore, Plan)", Bash]`.

| Kind | Rendering |
| --- | --- |
| Claude | Requires `tools`, even if empty. Emits `--strict-mcp-config`, then optional `--disallowedTools`, then `--tools` with comma-joined bases. A nonempty `Agent(...)` allowlist denies every unnamed type from `Explore`, `Plan`, `general-purpose`, `statusline-setup`, `fork`, in that order. Unknown native types fail. Bare `Agent` adds no type restriction. |
| Codex | Requires `tools`, even if empty. Emits `--strict-config` and the ordered `-c` settings below. Native `Agent(...)` type names do not restrict Codex child types. |
| Pi | Accepts or omits `tools`; generates no tool arguments. |
| Other kinds | Omit `tools`; a supplied list is unsupported. |

Codex emits these settings in order. Boolean values are unquoted; `web_search` is a quoted string.

| Setting | Value |
| --- | --- |
| `web_search` | `"cached"` with `WebSearch` or `WebFetch`, otherwise `"disabled"` |
| `agents.enabled` | Whether `Agent` is present |
| `features.goals` | `false` |
| `features.multi_agent`, `features.multi_agent_v2` | Whether `Agent` is present |
| `features.shell_snapshot`, `features.shell_tool` | Whether `Bash` is present |
| `features.skill_mcp_dependency_install`, `features.tool_call_mcp_elicitation` | `false` |
| `features.browser_use`, `features.browser_use_external`, `features.computer_use`, `features.in_app_browser` | `false` |
| `features.image_generation`, `features.tool_suggest`, `features.memories` | `false` |
| `features.default_mode_request_user_input`, `tools.experimental_request_user_input.enabled` | Whether `AskUserQuestion` is present |
| `skills.include_instructions` | Whether `Skill` is present |

Other tool bases do not toggle Codex settings. Markdown definitions do not accept raw `args`, `system-prompt-file`, or `append-system-prompt-files`: tools generate arguments and bodies supply prompt pieces. Project TOML and launch flags retain their separate surfaces.

## Delegation and skills

Omitting `subagents` leaves RimZ delegation unrestricted; a list replaces that policy with named children, and `[]` denies all. Names are trimmed and deduplicated, must be nonempty, and must name a subagent definition that loads, an `[agents.commands]` entry, a registered kind, or the built-in `general` fallback. Naming a subagent that failed to load fails the listing definition. A subagent definition cannot set or inherit this field. A direct profile cannot combine any `subagents` list with the native `Agent` tool: choose the RimZ doorway or native delegation.

A `skills` list requires the `Skill` tool except on Pi. Entries are trimmed and deduplicated bare directory names, never paths, whitespace, or `<name>:<mode>` strings. At config load from a host process, a list is checked only when the definition resolves to sandbox isolation or to a host provider with a skill switch (Claude or Codex). For a checked list, each skill must have a readable `<name>/SKILL.md` in the provider's skill root or the RimZ skill library, searched in that order, the same roots and precedence as the [sandbox skill view](../internals/sandbox.md#skill-roots-and-user-only-markers); the first copy found is the one whose marker is checked, and a name in neither root fails naming both paths. A skill marked user-only for the selected provider is rejected: frontmatter `disable-model-invocation: true` for frontmatter-based providers, or `policy.allow_implicit_invocation: false` in `agents/openai.yaml` for Codex. The error names the marked file and both fixes: drop the skill from `skills:` or remove the marker. Invalid marker metadata or unreadable files also fail.

Host isolation enforces the list through Claude's or Codex's per-launch switch; providers without a switch warn and run unrestricted. The load-time decision uses the inherited profile isolation default, then machine policy; host definitions without a switch skip both skill lookup and marker checks. The check reads the host's skill roots, so it runs only from a host process: inside a sandboxed pane, where an agent's own unlisted skills appear as RimZ's user-only copies, config loading and `rimz agents validate` skip it (validate says so on stderr). Validation resolves the provider root from the ambient environment; a named account whose login home holds a different skill set is checked when that account launches. Sandbox launch also requires provider support for skill views. See [configuration: skills](../guide/configuration.md#skills) and [host switch limits](../internals/sandbox.md#host-mode).

## Teams and seats

`teams/<name>.md` contains YAML roster metadata and a required Markdown pipeline body. Roles select successful ordinary definitions from `agents/`, not subagents or bare kinds. Each seat becomes a resolved `<team>.<role>` profile, launchable alone or with the team.

| Team key | Meaning |
| --- | --- |
| `name` | Optional name, default filename stem; `[A-Za-z0-9_-]+`, unique among teams. |
| `description` | Optional string, accepted as metadata. |
| `layout` | Optional layout using role names and roleless cells; comma columns, plus tiled rows, slash stacked rows (tiled on tmux). |
| `leader` | Required declared role handle; receives the initial task and retains `AskUserQuestion`. |
| `stages` | Required nonempty ordered list of unique, nonblank stage names. |
| `traits` | Traits added to every seat's leaf craft. |
| `roles` | Required nonempty list of role mappings. |

| Role key | Meaning |
| --- | --- |
| `agent` | Required direct definition name. |
| `role` | Optional handle, defaulting to `agent`; unique within the team. |
| `owns` | Stages owned by this role; omitted or null means none. |
| `model`, `mode`, `effort`, `auto-compact`, `budget`, `model-reminder`, `tools`, `subagents`, `skills` | Overrides with the same types as agent fields. |
| `traits` | Additional traits for this seat's leaf craft. |
| `signals` | Optional nonempty signal-binding list. |
| `flip-compact` | Stage-handoff compaction threshold or `off`. |
| `idle-compact` | Idle team compaction: `off`, `on`, or a duration; inherits the machine setting when omitted. |

Role fields override inherited definition fields. Unlike standalone chains, a seat's recognized model may select a different runtime kind; an unrecognized model keeps the original kind. That kind needs its own base. Tools are rendered again for the selected kind. Traits combine leaf-definition → role → team, deduplicated; ancestor crafts remain unchanged. Added traits require `${traits}` in the leaf body. Nonleaders lose `AskUserQuestion`. Every nonempty seat skill list gains `reflect`, which must pass the same library checks. Empty and omitted skill lists stay unchanged.

Prompt order is kind base → ancestor crafts → seat craft → built-in consensus → team pipeline, separated by blank lines. Markdown teams do not expose custom consensus or scratch-file keys. Their staged workflow uses `/blackboard.md` and `/*-notes.md` as default ephemeral-memory patterns; see [teams](../guide/teams.md).

### The built-in consensus copy

RimZ writes a read-only copy of the built-in consensus to `teams/consensus.md` under the definitions root, for reading and linking. `rimz setup` writes or refreshes it, and launching a team seat that runs under the built-in consensus refreshes it too, so an upgrade replaces a stale copy at the next team launch. A header line names the RimZ version that wrote it. RimZ never reads the file back: teams always run under the text compiled into the binary, and an edit to the copy changes nothing and is overwritten at the next refresh. To change a team's consensus, set `consensus-file` in a project TOML team.

Because the definitions loader skips it, `teams/consensus.md` is never a team definition, and `consensus` is not available as a Markdown team file name.

### Stages and handles

Every declared stage must have exactly one owner. Ownership of an undeclared stage or duplicate ownership fails. `Implement` and `Review`, when both present, must have different owners. `Done` is implicit and cannot appear in `stages` or `owns`. Stage names match exactly; declaration order is not an enforced transition order.

The leader must name a declared handle. Handles must satisfy the shared address grammar and cannot shadow broadcast, reserved sender, kind, or kind-ordinal addresses. A layout must place every declared role exactly once. The pipeline and built-in consensus are scanned for `@` mentions beginning with an ASCII letter; each must name a declared role, `all`, `rimz`, or a registered kind. See [addresses](./cli/agents.md#addressing-agents).

### Signals

Signals belong on roles, never standalone definitions or the team root:

```yaml
signals:
  - ci.failed
  - signal: agent.idle
    match: {handle: reviewer}
    prompt: Read the review and continue the plan.
```

A selector is `family.event` or `family.*`: the family begins with a lowercase ASCII letter, and words contain only lowercase ASCII letters, digits, `_`, or `-`. No multi-dot or partial-wildcard selectors are accepted. Mapping entries accept only `signal`, `match`, and `prompt`. `match` maps payload field names to nonempty strings; all fields must match. An `agent.*` binding requires `handle` or `session`. A supplied prompt must be nonblank and is trimmed. An omitted signals field has no bindings; a supplied field must be a nonempty list, so null and `[]` are refused. Launch also validates event scope. See [team signal delivery](../guide/teams.md#send-events-to-the-responsible-role).

### Idle compaction

`idle-compact` accepts `off`, `on`, or a duration such as `25m` (units `s`, `m`, `h`, `d`). YAML booleans `false` and `true` mean `off` and `on`; integers without units are refused. Omission inherits `harness.idle_compact`, whose default is `on`. An explicit role value overrides it, including a machine opt-out.

`on` uses the provider prompt-cache lifetime minus three minutes; a duration supplies the threshold directly. Unknown lifetimes skip the role under `on`, and an adapter without a native compact command always skips. See [idle compaction](../guide/configuration.md#idle-compaction) for the provider table and eligibility.

This field is valid only on a team role. A standalone definition is rejected because a solo seat is never idle-compacted.

### Flip compaction

`flip-compact` accepts nonnegative integer token counts, strings such as `120k` or `1m`, percentages from `0%` through `100%`, or case-insensitive `off`; decimals, repeated suffixes, and overflowing counts fail. Omission inherits `harness.flip_compact`; when that is unset, a seat owning `Plan` gets `120k` and other seats get `180k`. An explicit role value overrides both, including a machine `off`. A provider without a manual compact command requires `off` on the role or machine-wide.

```yaml
flip-compact: off
```

YAML boolean `false` is also accepted as `off`; `true` is refused.

This is separate from native `auto-compact`. A role leaving its stage for another owner's stage compacts at its next turn boundary only when occupied context reaches the threshold. Flips to `Done`, user flips, same-stage re-fires, and moves between self-owned stages do not compact. See [stage handoff](../guide/teams.md#hand-off-with-one-command).

## Validation and failures

```sh
rimz agents validate
rimz agents validate --json
```

Validation reads the definition trees without launching agents or writing generated files. Human output groups agents, subagents, and teams; team rows include stages and leader, followed by seats. Errors appear last as `path: message`. Warnings for unenforced host lists follow the errors and do not change the exit status. JSON returns `{rows, errors, warnings}`; rows carry namespace, name, kind, model, effort, source, and team-seat metadata where applicable. Any error makes the exit status nonzero. From a host shell, listed skills are checked against the provider's skill root and the skill library in every isolation mode; an absent library is simply one empty root. Inside a sandboxed pane the check is skipped and validate says so on stderr. It also reports every error the launch path's config load records (commands from `config.toml` included), so a set that passes validation is a set that launches.

The loader collects independent errors rather than stopping at the first file, and one broken definition never fails the config load. Every command keeps the definitions that loaded; only a launch that selects a failed name refuses, with that definition's `path: message`, instead of silently substituting another profile. That covers `rimz agents` launch, resume, restart, and fork, `rimz subagents`, `rimz teams` launch and flip, loop tasks, and the exec wrapper. A definition that selects a failed one (an agent allowing a failed subagent, a team with a failed seat) fails too. A failure no single definition owns, such as a team named like an `[agents.commands]` entry, still refuses every launch until it is fixed. Resuming a lane degrades a member whose recorded profile failed to a bare resume with a warning, as it does for a deleted profile. `rimz doctor` and startup notices direct you here.

| Failure class | What to fix |
| --- | --- |
| Files and YAML | Unreadable trees/files, missing or unclosed frontmatter, malformed or nonmapping YAML, unknown keys, or wrong types. Traits and skill metadata can also be unreadable or malformed. |
| Retired or misplaced fields | Replace `soul` with body text and `meka` with `agent`; role `meka` is invalid because the model selects runtime. Move `signals`, `flip-compact`, and `idle-compact` onto roles. Raw prompt-path/argv keys are not Markdown fields. |
| Identity | Unsafe, duplicate, reserved, or address-shadowing names; cross-namespace duplicates; team/profile/command collisions. |
| Required content | Missing or multiline definition/base description, empty kind base, missing team pipeline, roster, stages, or leader. Ordinary definition bodies may be empty. |
| Resolution | Missing kind inference, unknown or failed parent, cross-namespace inheritance, cycles, excessive shared chain depth, model/kind mismatch, missing kind base, or an unknown/failed seat definition. |
| Traits | Invalid names, unreadable fragments, or named traits without a substitution token. |
| Tools | Required list missing, malformed entry, unsupported kind, unknown Claude native child type, or an argument that cannot be rendered. |
| Delegation | Empty/unknown child names, delegation on a child, native `Agent` combined with a RimZ child list, or a child reference rejected by final namespace validation. |
| Skills | Invalid names or missing `Skill` tool; for enforced lists, a listed skill in neither the provider root nor the library, invalid policy metadata, or a listed user-only skill (including automatically added `reflect`). |
| Compaction and presets | Invalid native token count or handoff threshold; shared validation/launch rejects unsupported provider capabilities and invalid provider-specific values. |
| Team topology | Duplicate/missing/invalid role handles; unknown leader; missing, duplicate, unknown, blank, or reserved stages/owners; same Implement/Review owner; undeclared prompt handle; invalid layout, missing or repeated role placement. |
| Signals | Empty list, invalid selector or map, blank match value/prompt, unscoped agent selector, or a binding rejected by the shared signal-selector checks. Worktree-dependent scope is checked at launch. |

Validation does not prove that provider executables, accounts, sandbox preconditions, or a particular live launch will work. Those remain launch checks.

## Moving existing machine configuration

Move agent launch preferences, `[agents.commands]`, `[agents.worktree]`, attention settings, and `[agents.subagents] timeout` into `config.toml`. Put direct profiles, children, teams, and traits in the trees above. Old `agents.toml`, `profiles/*/agent.toml`, and `teams/*/team.toml` are no longer read; setup does not convert or delete them. `agents.d/*/agent.toml` plugin manifests are a separate, unchanged format. Project `.rimz/config.toml` remains TOML. Validate the Markdown sources before launching.

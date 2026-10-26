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
| `agent` | Registered kind (a family preference for tiers) or another definition in the same tree. May be omitted with `tier` or when `model` identifies a kind. |
| `model` | Concrete model alias or provider model ID. OpenCode checks a full `provider/model` name or a unique bare ID before launch. Tier names are refused; use `tier:` instead. |
| `tier` | [Model tier](../guide/configuration.md#model-tiers): `intern`, `junior`, `senior`, or `principal`. Mutually exclusive with `model`. |
| `mode` | `ask`, `auto`, `plan`, or `yolo`. |
| `isolation` | `host`, `scoped`, or `sandbox`; overrides machine policy unless `--isolation` is recorded. Inherited through definitions, unavailable on team roles or repository profiles. |
| `empty-dir` | Boolean opting a solo agent or subagent into its own empty working directory. Inherited through `agent:` chains; `false` clears it. Also accepted in repository TOML `[profiles]` and `[subagents.profiles]`; machine profiles remain Markdown definitions. Team roles refuse it, including through a selected definition or profile, because teams share the worktree. |
| `effort` | Provider-specific absolute effort (YAML integer or string), or omit for the selected model's default. Relative `+N` / `-N` values are refused. |
| `fast` | Boolean requesting the provider's [fast mode](./agent-support.md#fast-mode). Off by default; a kind with no fast switch warns at launch and runs without it. |
| `auto-compact` | Native compaction window: integer or string token count from `100k` through `1M`, inclusive; no percentages or decimals. |
| `budget` | Dollar cap as string or number; a string such as `20/day` selects a daily cap. |
| `keep-warm` | Prompt-cache [keep-warm](#keep-warm) maximum for a solo seat: `off`, a duration such as `2h`, or a block with `max` only. |
| `idle-compact` | [Idle compaction](#idle-compaction) token floor: `off` or a positive token count such as `100k`. Refused on a subagent definition. |
| `model-reminder` | Boolean controlling the model identity in the launch reminder for solo agents and subagents. Defaults on; team members never show model names. |
| `traits` | Trait names substituted into this definition's body. |
| `tools` | Tool-name list, translated to provider arguments below. |
| `subagents` | Direct profiles only: allowed RimZ child profiles; `[]` permits none. |
| `skills` | Bare skill-directory names allowed for model invocation. Claude also adds exact invocation permissions for listed names without `*`, in all isolation levels and independently of `allow-routine-rimz`. Scoped views and host Claude keep unlisted skills user-only; host Codex hides them. Other host providers warn and run unrestricted. `[]` lists none. |
| `allowed-tools` | Permission rules such as `["Bash(git *)", Read]`. Claude pre-approves matching calls in all isolation levels, independently of `allow-routine-rimz`; other kinds warn and do not apply them. Available only in Markdown definitions, not repository TOML. `[]` or null clears inherited rules. |

Names must match `[A-Za-z0-9_-]+` and be unique across both definition trees. Registered kinds are reserved for kind bases. `general` and model aliases are not reserved by the definition parser. The final namespace checks also reject command-verb and address collisions, such as `all`, kind ordinals, or a profile colliding with a team.

### Chains and defaults

The nearest definition setting `tier:`, `model:`, or a registered `agent:` kind decides the runtime. A `tier:` needs no `agent:`. A concrete `model:` normally runs exactly on its own family, whether listed in the machine [tier table](../guide/configuration.md#model-tiers) or not, with no tier stamp or usage fallback. Definitions on Pi or OpenCode keep their engine even when a concrete model names another family, so `agent: pi` with `model: sonnet` runs Sonnet on Pi. Otherwise, if the model's family cannot build the definition, it fails with the path, build error and fix; a listed model also gets a `tier:` hint for allowing a move. Only a `tier:` routes: an explicit kind on the deciding definition prefers that family's entries first, followed by remaining entries in list order, then higher tiers with the preferred family first, never lower. Families unable to render the definition's tools, skills, supported fields, or kind base are excluded with reasons reported by `rimz agents validate`; passes ahead of the chosen entry are recorded with their full errors. No eligible entry at or above the requested tier is an error.

`tier` and `model` inherit as one setting: setting either inherits neither. A child still inherits its parent's absolute effort if present; otherwise the chosen model's default applies. Relative effort is refused even when inherited, with the source named. Project TOML profiles and roles cannot set `tier` or tier-named `model`: define the tiered profile in Markdown or name a concrete model in TOML. Project models run exactly, even when listed in the table. For tier selection guidance, see [choosing a model tier](../guide/configuration.md#choosing-a-model-tier).

`agent:` follows only definitions in its own namespace: an agent cannot inherit a subagent or vice versa. Without `agent:`, a recognized model alias, model ID, or prefix determines the kind: Claude's roster and `claude-` prefix, and Codex's roster and `gpt-` prefix. Unknown models use the chain's kind or require `agent:` when none exists. Except on Pi and OpenCode, a model implying a different kind from a registered `agent:` in the same definition is an error, and a child model may change its ancestor's kind. Pi and OpenCode chains keep their engine under concrete models, including a team role's explicit `model:`; use `agent:` or `--agent` to select another engine. OpenCode strictly matches a full `provider/model` name or a unique bare model ID against its configured catalog. A `tier:` still walks the tier table's families.

Children inherit `tier` or `model`, `mode`, `isolation`, `empty-dir`, `effort`, `fast`, `auto-compact`, `budget`, `keep-warm`, `idle-compact`, `tools`, `allowed-tools`, `skills`, `subagents`, and `model-reminder`. An explicit child field replaces the inherited value; lists never concatenate. Descriptions and trait lists are local. Parent prompt bodies are retained in order before the child's body. A bodyless definition adds no craft, so it can serve as a model-only preset on its kind base.

Defaults apply after inheritance, for the kind and model the launch finally runs on, including `--agent`, `--tier`, and `--model` overrides. Written mode and effort carry across an override; kind defaults do not. Claude uses mode `auto` and effort `xhigh`, except `fable` defaults to `high`; Codex and Pi default to effort `xhigh`. Codex aliases `astra`, `sol`, `luna`, and `terra` follow their account catalog's newest family model at fresh launch, subject to effort support; definitions retain the requested alias. Machine [model alias pins](../guide/configuration.md#model-aliases) take precedence. Recorded launches resolve the requested alias again on relaunch; legacy sessions without a launch record replay their observed model when available. Pi and OpenCode instead restore their native session's model on plain resume or restart; Pi also does so on fork. A kind whose adapter declares native auto-compaction support uses the machine [`[harness] auto_compact`](../guide/configuration.md#native-auto-compaction) default (`272k` built in, `"off"` passes none); other kinds have no default window. Definitions are rejected at load if their resolved kind cannot render a supplied or inherited `model` or `effort`. Unsupported `fast: true` and `auto-compact` load normally and warn at launch, which runs without the setting. Invalid auto-compact counts still refuse. Defaults are filled only when supported and silently dropped on a switch to an unsupported kind.

### Kind bases

`agents/<registered-kind>.md` is a special base, not an ordinary definition. It accepts only `description`, requires a nonempty body, and supplies the system prompt in both namespaces. It cannot pin a model or tools. A kind takes a system prompt when its adapter declares a prompt channel: claude, codex, opencode, pi, omp, qwen, minimax (appended to its own prompt), and process plugins declaring `system-prompt-file-flag`. See [launch-prompt replacement](./agent-support.md#launch-prompt-replacement) for how each receives it. Every definition and every team seat on such a kind requires that kind's base, body or not; a home with no definitions needs no base.

A kind that takes no system prompt carries no RimZ prompt. Validation refuses its kind base (delete the file), a definition with a craft body on it (remove the body or choose a kind that takes a system prompt), and a team seat on it (choose a definition on a kind that takes a system prompt). A bodyless standalone definition on such a kind needs no base. A definition on a kind whose base failed to load fails too, naming that base rather than asking you to create it.

Overriding a definition's kind with `--agent` does not discard its prompt. If the chosen kind cannot receive it, launch refuses, names the first definition file supplying the prompt (base, then crafts, then team pipeline), and asks you to choose a kind that takes a system prompt.

### Traits

`traits: [careful, concise]` reads `traits/careful.md` then `traits/concise.md`, deduplicating first-seen names. Names cannot be empty, `.` or `..`, or contain path separators. These files are plain Markdown: their complete trimmed contents are joined with blank lines and replace the first `${traits}` in the body. Later tokens collapse to a blank line. With no traits, the token disappears. Naming traits without a token is an error. Other `${...}` placeholders survive unchanged; the team pipeline is not trait-substituted.

## Tools to provider arguments

Tool entries use Claude-style names; the Pi aliases below normalize to those names before deduplication. The base before `(` is trimmed and deduplicated in first-seen order; it must begin with an ASCII letter and contain only ASCII letters, digits, or underscores. Parentheses must balance without nesting. Only `Agent(...)` interprets its suffix, as a comma-separated native child-type allowlist. Other suffixes do not restrict calls; Pi warns when it cannot enforce a suffix on a mapped tool. This is not a permission-pattern language.

Quote entries containing commas in a YAML flow list: `tools: ["Agent(Explore, Plan)", Bash]`.

`tools` decides which tools exist; `allowed-tools` pre-approves calls to them. Rules are trimmed and deduplicated, keeping the first occurrence. A rule starts with an ASCII letter, followed by ASCII letters, digits, `_`, `*`, or `-`, optionally followed by a nonempty `(specifier)` closing the rule. Control characters are refused. MCP names such as `mcp__my-server__*` are accepted. Claude auto mode drops some broad rules, including `Bash`, `Bash(*)`, wildcarded interpreters, package-manager run commands, `Agent`, and `Monitor`; use narrow rules such as `Bash(git *)`. Claude's drop list is best-effort and may change.

| Kind | Rendering |
| --- | --- |
| Claude | Requires `tools`, even if empty. Emits `--strict-mcp-config`, then optional `--disallowedTools`, then `--tools` with comma-joined bases. A nonempty `Agent(...)` allowlist denies every unnamed type from `Explore`, `Plan`, `general-purpose`, `statusline-setup`, `fork`, in that order. Unknown native types fail. Bare `Agent` adds no type restriction. |
| Codex | Requires `tools`, even if empty. Emits `--strict-config` and the ordered `-c` settings below. Native `Agent(...)` type names do not restrict Codex child types. |
| Pi | Optional. Emits `--tools` with the mapped names below, or `--no-tools` for an empty or all-dropped list, then `--no-skills` when the list omits `Skill`. An absent list emits no flag. |
| OMP | Accepts or omits `tools`; ignores the list silently. |
| Other kinds | Omit `tools`; a supplied list is unsupported. |

The Markdown `tools` list follows the original role through every `--agent` switch, including Claude ↔ Codex, and renders for the final provider. An explicit `[]` wins over the replacement's list. A provider that cannot apply a carried list warns and launches without it; OMP stays silent. Generated tool flags are not part of a profile's `args` in `rimz config get`. The injected machine auto-compact window is also omitted from `config get`; `rimz agents explain` shows its effective launch value. A profile's declared auto-compact value stays visible in `config get`.

Pi maps these names in first-occurrence order and deduplicates the result:

| RimZ name | Pi tool |
| --- | --- |
| `Bash` | `bash` |
| `Read` | `read` |
| `Edit`, `MultiEdit` | `edit` |
| `Write` | `write` |
| `Grep` | `grep` |
| `Glob` | `find` |
| `LS` | `ls` |
| `AskUserQuestion` | `ask_user_question` |

The Pi names in the right column normalize to their RimZ equivalents (`edit` to `Edit`) before rendering for any provider. Pi's `powershell` stays as written. Pi has no Skill tool: it lists skills in the system prompt while `read` or `bash` is enabled, and the model loads them with that tool. So `Skill` beside `Read` or `Bash` keeps skills on and adds no flag, `Skill` without either is dropped, and a list that omits `Skill` turns skills off with `--no-skills`. On Pi, other names are dropped with one warning naming them all; a separate warning names only mapped tools whose `(…)` restrictions Pi cannot enforce. A plain `--tools` list keeps MCP tools callable, so this is not a strict allowlist across MCP. `--no-tools` disables built-in and extension tools, including MCP. A supplied list that omits `AskUserQuestion` (or its Pi spelling) removes the rpiv question tool.

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

At launch, callable `rimz-subagents` plus an available child catalog switches off provider-native delegation wherever the provider has a verified switch, even when `tools:` lists `Agent`. Omitting `skills` makes every skill callable unless an applied `tools:` list excludes `Skill` (Claude, Codex, and Pi); then none is callable, skill reminder sections disappear, and native delegation and LSP tools stay as the tools list renders them. Tools lists on other kinds do not change skill callability. To retain native delegation, omit `rimz-subagents` from an explicit `skills` list, or have no launchable child: a [kind base](#kind-bases) counts as one, beside subagent definitions and `[agents.commands]` entries. A disabled catalog (`subagents: []` included) does not switch native delegation off. Children still have native delegation disabled. See [delegation policy](../internals/harness/subagents.md#native-delegation-policy).

A `skills` list requires the `Skill` tool on Claude and Codex, and on Pi whenever the definition supplies a `tools` list. Entries are trimmed and deduplicated bare directory names, never paths, whitespace, or `<name>:<mode>` strings. At config load from a host process, a list is checked only when the definition resolves to scoped isolation or to a host provider with a skill switch (Claude or Codex). For a checked list, each skill must have a readable `<name>/SKILL.md` in the provider's skill root or the RimZ skill library, searched in that order, the same roots and precedence as the [scoped skill view](../internals/sandbox.md#skill-roots-and-user-only-markers); the first copy found is the one whose marker is checked, and a name in neither root fails naming both paths. A skill marked user-only for the selected provider is rejected: frontmatter `disable-model-invocation: true` for frontmatter-based providers, or `policy.allow_implicit_invocation: false` in `agents/openai.yaml` for Codex. The error names the marked file and both fixes: drop the skill from `skills:` or remove the marker. Invalid marker metadata or unreadable files also fail.

Host isolation enforces the list through Claude's or Codex's per-launch switch; providers without a switch warn and run unrestricted. The load-time decision uses the inherited profile isolation default, then machine policy; host definitions without a switch skip both skill lookup and marker checks. The check reads the host's skill roots, so it runs only from a host process: inside a scoped pane, where an agent's own unlisted skills appear as RimZ's user-only copies, config loading and `rimz agents validate` skip it (validate says so on stderr). Validation resolves the provider root from the ambient environment; a named account whose login home holds a different skill set is checked when that account launches. Scoped launch also requires provider support for skill views. See [configuration: skills](../guide/configuration.md#skills) and [host switch limits](../internals/sandbox.md#host-mode).

## Teams and seats

`teams/<name>.md` contains YAML roster metadata and a required Markdown pipeline body. Roles select successful ordinary definitions from `agents/`, not subagents or bare kinds. Each seat becomes a resolved `<team>.<role>` profile, launchable alone or with the team.

| Team key | Meaning |
| --- | --- |
| `name` | Optional name, default filename stem; `[A-Za-z0-9_-]+`, unique among teams. |
| `description` | Optional string, accepted as metadata. |
| `layout` | Optional layout using role names and roleless cells; comma columns, plus tiled rows, slash stacked rows (one expanded member on both backends). |
| `leader` | Required declared role handle; receives the initial task and retains `AskUserQuestion`. |
| `stages` | Required nonempty ordered list of unique, nonblank stage names. |
| `traits` | Traits added to every seat's leaf craft. |
| `roles` | Required nonempty list of role mappings. |
| `keep-warm` | Default [keep-warm](#keep-warm) `max` and `while` for every role. |

| Role key | Meaning |
| --- | --- |
| `agent` | Required direct definition name. |
| `role` | Optional handle, defaulting to `agent`; unique within the team. |
| `owns` | Stages owned by this role; omitted or null means none. |
| `tier`, `model`, `mode`, `effort`, `fast`, `auto-compact`, `budget`, `model-reminder`, `tools`, `allowed-tools`, `subagents`, `skills` | Overrides with the same types and inheritance as agent fields, including mutually exclusive `tier` and `model`, and absolute effort only. |
| `traits` | Additional traits for this seat's leaf craft. |
| `signals` | Optional nonempty signal-binding list. |
| `flip-compact` | Stage-handoff compaction threshold or `off`. |
| `idle-compact` | [Idle compaction](#idle-compaction) token floor for this seat: `off` or a token count; inherited from the definition when omitted. |
| `keep-warm` | [Keep-warm](#keep-warm) shorthand or `{ max, while }` block. Inherits each field from the team, never the definition. |

Role fields override inherited definition fields. Unlike standalone chains, a seat's recognized model may select a different runtime kind; an unrecognized model keeps the original kind. That kind needs its own base. Tools are rendered again for the selected kind. Traits combine leaf-definition → role → team, deduplicated; ancestor crafts remain unchanged. Added traits require `${traits}` in the leaf body. Nonleaders lose `AskUserQuestion`. Every nonempty seat skill list gains `reflect`, which must pass the same library checks. Empty and omitted skill lists stay unchanged.

Prompt order is kind base → ancestor crafts → seat craft → built-in consensus → team pipeline, separated by blank lines. Markdown teams do not expose custom consensus or scratch-file keys. Their staged workflow keeps its board and stage notes through `.rimz/team/`, a link to the run's archive in room state; see [teams](../guide/teams.md).

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

`idle-compact` accepts `off` or a positive token count: an integer or a string in digits or `k` such as `100k`, `250000`, or `1000k`, with no `m` suffix and no percentages. A bare YAML `off` (the boolean `false`) means `off`; `true` and `0` are refused. An idle seat compacts once its occupied context reaches the floor; see [idle compaction](../guide/configuration.md#idle-compaction) for the timing, provider defaults, and eligibility.

On a standalone definition it sets the floor for an agent launched from that profile and is inherited through `agent`; a TOML profile inherits it through its `agent` base the same way, and an explicit `off` stops the walk. On a team role it overrides the floor the role's definition carries. Omitted everywhere, the seat uses `harness.idle_compact`, whose default is `100k`. A subagent definition that sets it is refused, since a `rimz subagents` child is never idle-compacted.

### Keep-warm

`keep-warm` accepts a block with optional `max` and `while`. `max` takes `off` or a positive duration (units `s`, `m`, `h`, `d`). `while` takes the [loop `--when` condition grammar](./cli/loop.md#conditions), restricted to `team.stage`, or `off`. The stage values are this team's stages plus `Done` and `unknown`; stage words use ASCII letters, digits, `_`, `.`, or `-`, so names with spaces cannot appear in a condition. Unknown readings (missing or unreadable board, no `Stage:` line, or no checkout path) permit the hold, including before the first flip. A known reading is evaluated anew on every pass; reopening a stage can restore a hold only while the cache remains warm.

Shorthand `6h` means `{ max: 6h }`; shorthand `off` means `{ max: off, while: off }`. Bare YAML `off` or `false` works at the top level and in either field. `true`, numbers, and unitless durations are refused. TOML accepts the same string shorthand or an inline table, for example `keep-warm = { max = "6h", while = "team.stage!=Done" }`.

For team seats, resolve each field separately: role, then team, then unset. An explicit `off` cancels that field. A role duration still inherits the team's `while`. Neither field reads the profile behind the seat. No resolved fields means no hold. `max` alone continues through `Done`; add `while: team.stage!=Done` to stop there. `while` alone uses `harness.cache_keepalive_max` as a finite maximum; with that cap `off` it holds nothing, and validate warns to set `max`.

Standalone definitions and TOML profiles accept shorthand or a `max`-only block, inherited through `agent`. Any `while` field is refused there, or on an unstaged team: they have no board. Other keys (forge or window readings), undeclared stage values, and malformed conditions are refused at load for teams and roles too. The horizon holds only when the provider's prompt-cache lifetime is known and at least `harness.keep_warm_min_ttl`; `rimz agents validate` warns at the definition when it will not: the lifetime is unknown or under the floor, or `harness.cache_keepalive` is off; it also notes a resolved `max` longer than `harness.cache_keepalive_max`, which the pings stop at. A `subagents` definition may carry `max` but no seat reads it, and validate says so. See [keep-warm](../guide/configuration.md#keep-warm) for what a horizon does and costs.

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

Validation reads the definition trees without launching agents or writing generated files. Human output groups agents, subagents, and teams; team rows include stages and leader, followed by seats. Errors appear last as `path: message`. Warnings for unenforced host lists follow the errors and do not change the exit status. JSON returns `{rows, errors, warnings}`; rows carry namespace, name, kind, model, effort, source, and team-seat metadata where applicable. Any error makes the exit status nonzero. From a host shell, listed skills are checked against the provider's skill root and the skill library in every isolation mode; an absent library is simply one empty root. Inside a scoped pane the check is skipped and validate says so on stderr. It also reports every error the launch path's config load records (commands from `config.toml` included), so a set that passes validation is a set that launches.

The loader collects independent errors rather than stopping at the first file, and one broken definition never fails the config load. Every command keeps the definitions that loaded; only a launch that selects a failed name refuses, with that definition's `path: message`, instead of silently substituting another profile. That covers `rimz agents` launch, resume, restart, and fork, `rimz subagents`, `rimz teams` launch and flip, loop tasks, and the exec wrapper. A definition that selects a failed one (a definition on a failed kind base, an agent allowing a failed subagent, a team with a failed seat) fails too. A failure no single definition owns, such as a team named like an `[agents.commands]` entry, still refuses every launch until it is fixed. Resuming a lane degrades a member whose recorded profile failed to a bare resume with a warning, as it does for a deleted profile. `rimz doctor` and startup notices direct you here.

| Failure class | What to fix |
| --- | --- |
| Files and YAML | Unreadable trees/files, missing or unclosed frontmatter, malformed or nonmapping YAML, unknown keys, or wrong types. Traits and skill metadata can also be unreadable or malformed. |
| Retired or misplaced fields | Replace `soul` with body text and `meka` with `agent`; role `meka` is invalid because the model selects runtime off Pi. Move `signals` and `flip-compact` onto roles. Raw prompt-path/argv keys are not Markdown fields. |
| Identity | Unsafe, duplicate, reserved, or address-shadowing names; cross-namespace duplicates; team/profile/command collisions. |
| Required content | Missing or multiline definition/base description, empty kind base, missing team pipeline, roster, stages, or leader. Ordinary definition bodies may be empty. |
| Resolution | Missing kind inference, unknown or failed parent, cross-namespace inheritance, cycles, excessive shared chain depth, model/kind mismatch, missing or failed kind base, or an unknown/failed seat definition. |
| Prompt support | A kind base, definition craft, or team seat on a kind that takes no system prompt. Delete the base, remove the craft, or choose a kind that takes a system prompt. |
| Model tiers | Both `tier` and `model` set, a tier name in `model`, an unknown tier (with a suggestion), relative effort, an unbuildable concrete model, or no eligible model at or above the requested tier. Tier exclusions identify each family's reason. Configure ordered lists in machine `config.toml`; project TOML cannot set `[tiers]`, `tier`, or tier-valued models. |
| Traits | Invalid names, unreadable fragments, or named traits without a substitution token. |
| Tools | Required list missing, malformed entry, unsupported kind, unknown Claude native child type, or an argument that cannot be rendered. |
| Delegation | Empty/unknown child names, delegation on a child, native `Agent` combined with a RimZ child list, or a child reference rejected by final namespace validation. |
| Skills | Invalid names or missing `Skill` tool; for enforced lists, a listed skill in neither the provider root nor the library, invalid policy metadata, or a listed user-only skill (including automatically added `reflect`). |
| Compaction and presets | Invalid native token count or handoff threshold; shared validation/launch rejects unsupported provider capabilities and invalid provider-specific values. |
| Team topology | Duplicate/missing/invalid role handles; unknown leader; missing, duplicate, unknown, blank, or reserved stages/owners; same Implement/Review owner; undeclared prompt handle; invalid layout, missing or repeated role placement. |
| Signals | Empty list, invalid selector or map, blank match value/prompt, unscoped agent selector, or a binding rejected by the shared signal-selector checks. Worktree-dependent scope is checked at launch. |

To check a definition by hand without touching your own home, validate it in a scratch one. Point `RIMZ_HOME` and `RIMZ_AGENTS_HOME` at a temporary directory, write nonempty [kind bases](#kind-bases) `agents/claude.md` and `agents/codex.md`, and give every Claude or Codex definition a `tools:` list such as `tools: [Bash]`:

```sh
export RIMZ_HOME=$(mktemp -d) RIMZ_AGENTS_HOME=$RIMZ_HOME
mkdir -p "$RIMZ_HOME/agents"
printf -- '---\ndescription: Claude base\n---\nBase prompt.\n' > "$RIMZ_HOME/agents/claude.md"
printf -- '---\ndescription: Codex base\n---\nBase prompt.\n' > "$RIMZ_HOME/agents/codex.md"
printf -- '---\ndescription: Scratch seat\nagent: claude\ntier: senior\ntools: [Bash]\n---\n' > "$RIMZ_HOME/agents/scratch.md"
rimz agents validate
```

Validation does not prove that provider executables, accounts, sandbox preconditions, or a particular live launch will work. Those remain launch checks.

## Moving existing machine configuration

Move agent launch preferences, `[agents.commands]`, `[agents.worktree]`, attention settings, and `[agents.subagents] timeout` into `config.toml`. Put direct profiles, children, teams, and traits in the trees above. Old `agents.toml`, `profiles/*/agent.toml`, and `teams/*/team.toml` are no longer read; setup does not convert or delete them. `agents.d/*/agent.toml` plugin manifests are a separate, unchanged format. Project `.rimz/config.toml` remains TOML. Validate the Markdown sources before launching.

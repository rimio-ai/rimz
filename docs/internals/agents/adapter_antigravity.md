# Antigravity adapter

This page maps the Antigravity CLI (`agy`) onto RimZ's types: launch and resume, the hooks RimZ installs, session discovery from the provider's local store, the transcript, subagents, statusline context, the account, and cost. The provider-neutral agent model is in [model.md](./model.md), the integration layer in [adapter.md](./adapter.md), and pane binding and supersession in [instances.md](./instances.md). Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The upstream protocol, pinned at `agy` 1.5.14 with transcript and subagent captures from 1.1.2, plugin and `/usage` captures from 1.5.12, custom-agent and `/compact` captures from 1.5.13, and permission, sandbox, and MCP captures from 1.5.14, is in [antigravity-reference.md](../../externals/agent-adapter/antigravity-reference.md).

Antigravity reaches RimZ through four sources, and each owns different facts. Command hooks carry turn boundaries and tool progress. The custom statusline carries the live model, account, and context gauge. The provider's local `brain/` store carries conversation identity, history, priced spend, and native questions between hooks. The statusline and a print-mode `agy -p /usage` run both carry the quota. RimZ installs no `PreToolUse` hook, because no documented `PreToolUse` decision is proven to leave native permission policy unchanged ([reference](../../externals/agent-adapter/antigravity-reference.md#decisions)).

## Files

| File | Owns |
| --- | --- |
| `crates/rimz/src/agents/adapters/antigravity/mod.rs` | The `static AgentSpec` (`ANTIGRAVITY_DESCRIPTOR`), the declared coverage, the hook catalog (`ANTIGRAVITY_HOOKS`), hook replies and decoding (`decode_lifecycle_fields`), resume argv, and the capability impls |
| `crates/rimz/src/agents/adapters/antigravity/permissions.rs` | The `allowed-tools` translation into agy permission rules and its merge into the launch's one `--settings` |
| `crates/rimz/src/agents/adapters/antigravity/payloads.rs` | Typed hook payloads and the failure rules for tool and `Stop` payloads |
| `crates/rimz/src/agents/adapters/antigravity/install.rs` | The plugin-directory install, the statusline record, preview, uninstall, the disabled-plugin check, and the launch's plugin-dir check (`launch_plugin_dir`) |
| `crates/rimz/src/agents/adapters/antigravity/agent_file.rs` | The launch agent file: the tool-name mapping and its notices, the frontmatter and body, and the content-hashed name |
| `crates/rimz/src/agents/adapters/antigravity/session.rs` | Provider home resolution, transcript path validation, local-session discovery, the transcript fold, final-message extraction, and the parent-transcript subagent join |
| `crates/rimz/src/agents/adapters/antigravity/spend.rs` | The spend source glob and the token-row parser with its exact-only pricing |
| `crates/rimz/src/agents/adapters/antigravity/statusline.rs` | The statusline payload and its `AgentContext` projection, quota windows included |
| `crates/rimz/src/agents/adapters/antigravity/usage.rs` | The `agy -p /usage` child, the credential-file account id, its mapping onto both account probes, the 60 s reuse, and the Gemini quota fold both quota sources share |

## Launch and resume

The launch spec maps RimZ's launch inputs onto `agy` flags:

| Input | Argv |
| --- | --- |
| fresh launch | `agy` |
| startup prompt | `agy --prompt-interactive <prompt>` |
| profile `model` | `--model <model>`, a catalog short name as its `gemini-` ID |
| profile `effort` | `--effort <effort>`, from `low`, `medium`, `high`, and `xhigh` |
| permission `ask` | none (the native default) |
| permission `auto` | `--mode accept-edits` |
| permission `plan` | `--mode plan` |
| permission `yolo` | `--dangerously-skip-permissions --mode accept-edits` |
| exact resume | `agy --conversation <id>` |
| definition `allowed-tools` | one `--settings` whose `permissions.allow` holds the [translated rules](#permission-rules-sandbox-and-added-dirs) |
| `sandbox` isolation | `--sandbox` |
| added directory | `--add-dir <dir>`, once per directory |
| profile system prompt, fresh launch | `--agent rimz-<label>-<hash>` after the rest of the argv, selecting [the agent file](#the-agent-file) |

The 1.3.3 CLI accepts `low`, `medium`, `high`, `xhigh`, and `max` for `--effort` ([reference](../../externals/agent-adapter/antigravity-reference.md#flags)), but agy 1.5.13 refuses `max` on every model and takes a per-model subset of the rest: hydrogen and neon take `low` to `xhigh`, carbon, argon, flash, and `claude-` IDs `low` to `high`. The adapter declares no version floor, so an older `agy` that lacks the flag rejects a profile that sets effort at its own startup.

`crates/rimz/src/agents/adapters/antigravity/mod.rs::MODELS` is the one table of Gemini model facts: each short name, its base ID, its default effort (`high`), and the level `max` launches at. agy knows no short name, so `AntigravityAdapter::resolve_model_alias` rewrites one to its ID at fresh launch, below a machine pin. RimZ sends a base ID plus `--effort`, and sets no kind-wide default effort, because agy exits on an effort-suffixed ID (`gemini-4-hydrogen-high`) beside a differing `--effort`: an uncataloged or suffixed ID launches as written. The effort matcher refuses a level outside `low` to `xhigh` at launch, naming the levels. `crates/rimz/src/agents/definition.rs::LaunchSpec::lower_max_effort` launches `max` at the entry's level, `high` for a `claude-` ID (the ceiling beside that prefix in `foreign_prefixes`), or `xhigh` for another ID outside the catalog, with a `SkippedField` warning. `crates/rimz/src/harness/plan.rs::finalize_agent_cell` settles `max` and the built-in default for the launch's final model, a `--model` override or a machine pin included, and re-renders the one `--effort` arg to match. A level the model lacks within that list is left to agy's own refusal. The `gemini-` prefix routes a definition to Antigravity; a `claude-` ID runs here only under `agent: antigravity` or `agent: agy` (`DefinitionSpec::foreign_prefixes`). Under `yolo`, implementation-plan approval still follows the user's `artifactReviewPolicy`, which RimZ leaves untouched ([reference](../../externals/agent-adapter/antigravity-reference.md#execution-modes)).

Resume takes a conversation id only when `session.rs::valid_conversation_id` accepts it: nonblank after trimming, at most 256 bytes, not `.` or `..`, no leading `-`, and no control character, `/`, or `\`. The same rule gates every path built from an id. On a live pane, `session.rs::resumed_session_id` reads the first `--conversation <id>` or `--conversation=<id>` after a program whose basename is `agy`. `-c` and `--continue` resume the workspace's latest conversation, so they prove no exact identity and bind nothing. The process matcher (`process_names`, `bin_names`) recognizes only `agy`, so the desktop app's `antigravity` processes stay outside this adapter.

Some launch inputs have no Antigravity channel:

| Input | Behaviour |
| --- | --- |
| `rimz agents fork` | unsupported: native `/fork` switches identity inside the TUI and has no source-id launch flag ([reference](../../externals/agent-adapter/antigravity-reference.md#fork-rewind-and-clear)) |

### Permission rules, sandbox, and added dirs

`permissions.rs::translate` maps each `allowed-tools` rule into agy's grammar by the table in [definitions.md](../../reference/definitions.md#tools-to-provider-arguments), and `permissions.rs::render` unions the results into `permissions.allow` through `crates/rimz/src/agents/launch_settings.rs::merge_settings`, the merge Claude uses. agy keeps only the last `--settings` ([reference](../../externals/agent-adapter/antigravity-reference.md#permission-resources)), so a profile's own `--settings`, inline or a file, merges into the one RimZ emits; a file source becomes a private `settings.<digest>.json` artifact, and a malformed or non-object source fails the launch. The emitted `permissions.allow` replaces the account's saved global allow list for the session, as a passthrough `--settings` does, while saved global and project deny rules stay in force; RimZ does not copy the saved allow rules in. A list with no translatable rule emits no `--settings`, which leaves the saved lists alone. A rule with no translation never reaches agy, which accepts any rule string and matches nothing; `rimz agents validate` and the launch both name it, because both ask `ToolRules::untranslated` the same `translate`. The argv is recompiled on every launch, so a resume carries the rules again, which agy needs.

`configure_room_permissions` appends `--sandbox` at sandbox isolation unless the argv already has it. Yolo keeps `--dangerously-skip-permissions`, which agy lets override its sandbox, so `native_sandbox_overridden` reports that flag in the compiled argv and the launch and `rimz agents validate` warn that sandbox isolation is not in effect. RimZ never writes `toolPermission`.

`allow_added_dirs_args` adds one `--add-dir` per directory the argv lacks. agy opens each as a workspace and loads its instructions, rules, and skills and runs its hooks, so `added_dirs` answers `AddedDirs::Workspace`: the launch warns, the supervised undecided-cwd fallback never chooses Antigravity, and the empty-directory reminder says the target was loaded.

### The agent file

agy publishes no system-prompt flag, so a fresh launch whose definition carries prompt sources (a kind base, a craft body, or a team layer) writes its composed system prompt into an agy custom agent and selects it with `--agent <name>`. `compile` plans the file through `agent_file.rs::plan`, and `apply` writes it through `crates/rimz/src/agents/agent_file.rs::publish_agent_file`. The file is `agents/<name>.md` in RimZ's plugin directory, resolved by the installer's rule from the launch env's `HOME` or `RIMZ_ANTIGRAVITY_HOOKS`. Its frontmatter holds `name`, a quoted `description`, `mainAgent: true`, `tools`, and the [MCP keys](#mcp-servers) when a launch has any; its body is the composed prompt, a blank line, and the launch reminder, so the file is also the reminder channel. The name is `rimz-<label>-<hash>`: the label is the profile name with every character outside `[A-Za-z0-9._-]` replaced by `-`, or `antigravity` without a profile, and the hash is 12 hex characters of SHA-256 over the rest of the file, so equal content shares one file and any change gets its own. Publishing gives the file a fresh mtime and removes `rimz-*.md` regular files beside it older than seven days.

Whenever a prompt is composed, a profile's own `--agent` is removed, on every action. A resume selects no file and passes no `--agent`: the conversation keeps the prompt, tools, and MCP servers it started with. A launch with no prompt sources keeps its argv and writes no file; its launch reminder is not delivered, and a subagent's no-delegation body arrives through the user-prompt fallback ([fleet.md](../harness/fleet.md#launch-reminders)). Such a fresh launch that carries `tools`, or an `mcps` list that limits servers (one without `*`), has no file to write the list into, so it is refused before any pane opens (`crates/rimz/src/harness/plan.rs::validate_fresh_agent_file_lists`): at layout, and before a molt or a restart whose conversation cannot resume reserves its swap, through `crates/rimz/src/harness/plan.rs::ResumeLaunchPosture::validate_fresh_agent_file_lists` on the posture the replacement launches with. The refusal names the field and the fix: add the kind base `agents/antigravity.md` or another prompt source, or remove the field. A cell carrying both lists is refused for `tools`. `compile` refuses the launch when the plugin directory does not hold RimZ's manifest, with the fix `rimz hooks install antigravity`, or when agy has the plugin disabled, with the fix `agy plugin enable rimz`.

agy needs a `tools` list on every definition. `agent_file.rs::map_tools` maps RimZ's names once, for the file and for the launch notices:

| RimZ name | agy names |
| --- | --- |
| `Read`, `Grep`, `Glob`, `LS` | `read_file` |
| `Edit`, `MultiEdit` | `edit_file` |
| `Write` | `write_file` |
| `Bash` | `run_command` |
| `WebSearch`, `WebFetch` | `search_web` |
| `AskUserQuestion` | `ask_question` |
| `Agent` | `invoke_subagent`, `define_subagent`, `manage_subagents`, `run_workflow` |
| `Skill` | none |

An agy name passes as written: the 13 defaults (`agent_file.rs::DEFAULT_TOOLS`) and `generate_image`. Any other name is dropped with the dropped-tools notice and never reaches the file, and a restriction suffix (`Bash(git *)`) is reported with the restrictions notice while its base name is mapped. A launch with no list writes the 13 defaults. The four subagent names, `run_workflow` among them since it fans work out to subagents, are written only when the launch keeps native subagents on (`crates/rimz/src/harness/launch_reminders.rs::LaunchReminders::disable_native_subagents`). On agy 1.5.13 `read_file` refuses a directory under a custom agent (`path is a directory, not a file`) and agy registers no other listing tool, so `LS` maps to `read_file` and grants no directory listing: a definition that needs one lists `Bash`.

#### MCP servers

`takes_mcp_servers` is on and `mcp_server_args` keeps the trait default, so no MCP argument reaches argv. RimZ's server is the rimz plugin's own `mcp_config.json` entry (`rimz mcp serve --launch-env`, written by [install](#install)), which agy starts for every session of the account and names `rimz_tools` (`<plugin>_<server>`, the adapter's `mcp_server_name`). `serves_mcp_from_launch_env` is on, so `compile` hands the server over on every launch that serves a capability (fresh with or without a prompt, and resume) and records the launch directory as the agent sees it (`LaunchReminders::mcp_cwd`); `crates/rimz/src/harness/launch.rs::compile_agent_process_with_extra_env` then sets `RIMZ_MCP_SERVE` (the served capabilities and their marks, `crates/rimz/src/mcp/launch_env.rs::render`) and `RIMZ_MCP_CWD`, and removes and unsets both on every launch that serves nothing, of every kind. A mark on `rimz` marks every served capability that has no mark of its own (`crates/rimz/src/config/mcps.rs::served_marks`). The server reads the two variables when agy starts it, so a resumed conversation serves what its launch names, and a session RimZ did not launch gets a server that writes one stderr line and exits, which `/mcp` shows as that server's error ([mcp.md](../../reference/cli/mcp.md#launch-registration)). The quota read's `agy -p /usage` child removes both variables (`crates/rimz/src/agents/adapters/antigravity/usage.rs::command`).

A profile's `mcps` list rides the agent file, after `tools` and inside the hashed part. `crates/rimz/src/agents/adapters/antigravity/agent_file.rs::mcp_lines` writes nothing without a limit; with one, `inheritMcp: false`, then `mcpServers: [<name>...]` when the list names a user server. `<name>` is `{"name": "<server>"}` for each `crates/rimz/src/config/mcps.rs::McpName::user_server` in list order, with no check that the server exists; agy ignores an unknown one. `inheritMcp: false` does not withhold the plugin's own server: on agy 1.5.14, `rimz_tools` stayed listed and answered a first-turn call beside `inheritMcp: false` and a name-only user item, whether or not the list also named `rimz_tools` ([antigravity-reference.md](../../externals/agent-adapter/antigravity-reference.md)). agy keeps the user servers a conversation started with across a resume, even after the file changes or is pruned. A mark on one of the user's servers, or any mark while the server is not handed over, is returned in `AgentFile::warnings`, and `compile` adds each to the launch warnings and `mcp_warnings`; the name-only item is still written.

agy binds a plugin server's tools from the first turn, so a scoped Antigravity launch hides the `rimz-*` skill of each capability it serves, as Claude and Codex launches do (`crates/rimz/src/harness/launch_reminders.rs::LaunchReminders::hidden_skills`); a host launch hides none, since agy's `HostSkills::Unsupported` has no per-launch switch, and the reminder's `lsp` bullet names the `lsp` tool of the `rimz_tools` server. With `allow-routine-rimz` on, install saves `mcp(rimz_tools/*)` in the account's `permissions.allow`, so the tools run unasked. A launch whose definition has `allowed-tools` replaces the saved allow list for its session ([permission rules](#permission-rules-sandbox-and-added-dirs)), so its emitted list carries the same rule while it serves and the switch is on, when at least one of the definition's rules translates (a list none of whose rules agy reads, such as `[Glob]` or `[]`, emits nothing and leaves the saved allows in force); an `allowed-tools` rule naming `mcp__rimz` translates to `mcp(rimz_tools/...)`.

### Skills

`skills_home` is `$HOME/.gemini/config/skills`, the global root agy's built-in skills name, and `manual_skill` is `Frontmatter`: on 1.5.13 `disable-model-invocation: true` hides a skill from the model and keeps it in the `/` menu, and symlinked skill directories and a read-only root are discovered ([reference](../../externals/agent-adapter/antigravity-reference.md#remote-control-mcp-plugins-and-custom-agents)). Host launches link RimZ skills there, creating the root, and a scoped profile `skills` list overlays it ([sandbox.md](../sandbox.md#skill-roots-and-user-only-markers)). agy also reads `~/.gemini/antigravity-cli/skills`, a plugin's `skills/`, and the workspace `.agents/skills`, which RimZ leaves native, so a list does not restrict a skill kept there. On a duplicate name the `~/.gemini/antigravity-cli/skills` copy wins. agy never reads `~/.agents/skills`, so a skill kept only there does not reach it. Nothing about skills goes in the agent file. `host_skills` stays `Unsupported`: a host launch with a list warns and runs unrestricted.

## Hooks

### Install

`rimz hooks install antigravity` writes an agy plugin and edits one settings file, and the preview shows each diff:

| File | Override | Change |
| --- | --- | --- |
| `~/.gemini/config/plugins/rimz/hooks.json` | beside `RIMZ_ANTIGRAVITY_HOOKS` | one top-level hook named `rimz` holding the six handlers below |
| `~/.gemini/config/plugins/rimz/mcp_config.json` | beside `RIMZ_ANTIGRAVITY_HOOKS` | RimZ's MCP server: `{"mcpServers": {"tools": {"command": "rimz", "args": ["mcp", "serve", "--launch-env"]}}}`, which agy names `rimz_tools` |
| `~/.gemini/config/plugins/rimz/plugin.json` | beside `RIMZ_ANTIGRAVITY_HOOKS` | the manifest: name `rimz`, author `RimZ`, version, description |
| `~/.gemini/antigravity-cli/settings.json` | `RIMZ_ANTIGRAVITY_SETTINGS` | `statusLine` set to `rimz statusline feed --source antigravity`, and `mcp(rimz_tools/*)` in `permissions.allow` beside the user's rules while [`allow-routine-rimz`](../../guide/configuration.md#routine-rimz-commands) is on; with it off, install removes that rule, and uninstall always does, dropping a `permissions` object only the rule held |
| `~/.gemini/config/plugins/rimz/agents/rimz-*.md` | beside `RIMZ_ANTIGRAVITY_HOOKS` | not written by install: launches write [agent files](#the-agent-file) there, install keeps them, and uninstall removes them with the directory |
| `<rimz home>/antigravity-statusline.json` | the RimZ home | the displaced `statusLine`, written when install displaced one and removed when `statusLine` is absent |

Every path resolves from the process `HOME` (and the RimZ home) through `crates/rimz/src/agents/locate.rs::agent_config_path`, and the installer ignores the login env map. A launch resolves the same rule from its launch env instead (`install.rs::launch_plugin_dir`). The plugin directory is `plugins/rimz` beside the global `hooks.json` that `RIMZ_ANTIGRAVITY_HOOKS` names, and agy's enablement file is `config.json` there (`install.rs::Paths`). agy 1.5.12 loads a plugin's `hooks.json` with no install step and enables it by default ([reference](../../externals/agent-adapter/antigravity-reference.md#files-and-format)).

Ownership of the directory is the manifest, as for MiniMax: `crates/rimz/src/agents/adapters/antigravity/install.rs::check_plugin` accepts an absent or empty path, or a real directory whose `plugin.json` names `rimz` and author `RimZ`, or one holding only files install writes before the manifest (`hooks.json` and `mcp_config.json`), its `hooks.json` holding RimZ's handlers (an install the manifest has not yet followed). A file, a symlink, a plugin file that is not a regular file, a foreign manifest, a manifest-less `hooks.json` without RimZ's handlers, or any other content refuses install with a move-aside fix naming the path, and uninstall leaves that path in place while it still cleans the rest. Such a path is not a RimZ artifact either, so a home holding only it reads as untouched. Install writes the plugin last, after the settings pair below, so a failed settings, global-hooks, or record write leaves the legacy global handlers as the one hook set. It writes `hooks.json` and `mcp_config.json` before `plugin.json`, so agy never sees a manifest whose hooks or server are missing, and writes only files whose bytes differ. `installed` is every plugin file byte-equal, no owned handler left in the global file, and the statusline command equal to RimZ's; the allow rule does not count, since without it agy only asks. A plugin an older release installed lacks `mcp_config.json` or holds a `hooks.json` with older matchers (before the 1.5.14 tool names), so it reads not installed and `ProviderLogin::health` answers `HooksMissing`: each launch is refused with the hooks-missing text ("RimZ hooks are missing for antigravity account `default` at `<home>`; run `rimz hooks install antigravity`", `rimz accounts add antigravity <name>` for a named account, and "`rimz agents -p` requires antigravity hooks ..." for a supervised run) until `rimz hooks install antigravity` runs. `rimz doctor` shows the plugin's server in its hooks section, read from `mcp_config.json` and agy's enablement file (`ManagedIntegration::shipped_mcp_server`).

An older RimZ wrote its handlers into the global `~/.gemini/config/hooks.json`. agy runs a hook name defined there and in a plugin twice, so a surviving owned handler makes the global file outdated, and install removes it. `install.rs::strip_owned_hooks` removes every handler whose command contains `rimz hooks feed --source antigravity`, under any hook name, and drops a hook name, event, or entry only when that strip emptied it; containers the user left empty stay. A `rimz` hook of the user's own in the global file stays untouched. Install commits settings and the stripped global file as one pair (`crates/rimz/src/agents/settings_json.rs::commit_pair`): a failed second write restores both files to their original bytes. Every handler is `{"type":"command","command":...,"timeout":5}` and starts with `RIMZ_AGENT_PID=$PPID exec`, so the hook carries its owner pid ([instances.md](./instances.md#session-death)). agy runs plugin hooks with the plugin directory as cwd, so no handler reads its cwd. Uninstall removes the plugin directory, strips owned global handlers, restores the statusline, and deletes the record. Its report names only what it changed: the removed events only when the plugin's `hooks.json` held RimZ's handlers, and `settings.json` only when it was written.

A plugin the user disabled (`plugins.rimz.enabled == false` in `config.json`, read through `crates/rimz/src/agents/jsonc.rs`) is reported by `hooks_disabled` with the fix `agy plugin enable rimz`; an absent, `true`, unreadable, or malformed value reads as enabled. RimZ never writes `config.json`, and `installed` stays a statement about files.

The statusline follows the marker-free ownership Cursor uses (`crates/rimz/src/agents/adapters/owned_statusline.rs::OwnedStatusLine`), because agy 1.5.12 rewrites `settings.json` keeping only `type`, `command`, and `stack_with_default` under `statusLine`, so no marker key survives a start. Installed means the command equals RimZ's; owned means it contains RimZ's statusline marker. A `statusLine` that exists and is not a JSON object refuses install. The displaced object goes whole to `antigravity-statusline.json` and is restored on uninstall. After each install the record describes what that install displaced: another statusline (an empty-command object included) overwrites it, a RimZ-owned one keeps it, and an absent `statusLine` removes it, which the dry run shows as `removes file`. When install would not write `settings.json`, the preview shows it unchanged. A legacy inline `_rimz_wrapped` value migrates there on reinstall. While RimZ is installed, the active entry keeps only one of the user's rendering options, `stack_with_default`. When `statusLine` was absent, install sets `stack_with_default: true` so Antigravity keeps its built-in line. A statusline the user replaced after install is left by uninstall and wrapped again by the next install.

### Events

The catalog holds four native events, with `PostToolUse` split by three disjoint matchers. The matchers name the documented tools and the names agy 1.5.14 was captured sending, and keep the legacy step names because the adapter declares no version floor ([reference](../../externals/agent-adapter/antigravity-reference.md#tool-vocabulary)). A tool outside all three sends no `PostToolUse` at all: there is no catch-all, so an unknown tool is never read as an edit or a mutation by guess. `run_workflow` and `call_mcp_tool` count as mutating because either can change anything, as `run_command` can.

| RimZ event | Native event and matcher | Signal | Reply |
| --- | --- | --- | --- |
| `PreInvocation` | `PreInvocation` | when `invocationNum` is `0`, `Compacting` for a bare `/compact` and `TurnStarted` otherwise; routing only for a later invocation | `{}` |
| `PostToolUse:edit` | `PostToolUse`, `write_file`, `edit_file`, and the legacy `write_to_file`, `replace_file_content`, `multi_replace_file_content` | `ToolUsed`, mutating and editing | `{}` |
| `PostToolUse:mutating` | `PostToolUse`, `run_command`, `run_workflow`, `call_mcp_tool` | `ToolUsed`, mutating | `{}` |
| `PostToolUse:observed` | `PostToolUse`, the remaining known tools (`view_file`, `read_file` through `browser_.*`) | `ToolUsed` | `{}` |
| `PostInvocation` | `PostInvocation` | routing only | `{}` |
| `Stop` | `Stop` | `CompactionEnded` for a bare `/compact`, `TurnEnded` otherwise | `{"decision":""}` |

Hook stdout is Antigravity's decision channel, and every wired event must answer with JSON. `{}` is the documented no-op, and `{"decision":""}` is a `Stop` decision other than `continue`, so the stop proceeds ([reference](../../externals/agent-adapter/antigravity-reference.md#events)). Any other event name, such as a hand-fed `PreToolUse` or `SessionStart`, gets no output and records nothing. For every Antigravity event the feed waits up to 1.2 s for the drainer to apply the frame and prints the drainer's reply ([adapter.md](./adapter.md#the-feed)). That early return also skips the deadline rung a `PostToolUse` could otherwise carry.

Every payload carries the conversation id (`conversationId`, or `conversation_id`), `workspacePaths`, `transcriptPath`, and `modelName`. A payload without a nonblank id records nothing. The first nonblank workspace path becomes the worktree path, and `modelName` becomes the launch model hint. Four decoding rules in `mod.rs::decode_lifecycle_fields` are easy to break:

- Only `invocationNum: 0` opens a turn. Later invocations of the same execution follow tool calls and must not reopen it, and a payload without `invocationNum` opens nothing.
- A failed tool (a nonempty `error`) keeps its `ToolUsed` progress and tool name but clears the mutating and editing flags. The tool name is the trimmed `toolCall.name`. A release that omits `toolCall` sends unnamed progress.
- An execution whose `lastUserInput`, trimmed, is exactly `/compact` is a compaction, not a turn ([Compaction](#compaction)). Any other input, `/compact <message>` included, and a payload without `lastUserInput` decode by the other rules.
- A turn's `Stop` needs `fullyIdle`, which upstream marks required. A turn's `Stop` without it records no lifecycle event and no final message. `terminationReason` `ERROR`, `MAX_INVOCATIONS`, `MAX_FORCED_INVOCATIONS`, `MAX_TOKEN_BUDGET_EXCEEDED`, or the 1.3.3 `max_steps_exceeded`, compared without case, or a nonempty `error`, ends the turn errored (`payloads.rs::FAILED_REASONS`). Every other reason is a clean end: `NO_TOOL_CALL`, `TERMINAL_CUSTOM_HOOK`, and `USER_CANCELED`, which agy 1.5.14 was not seen to send, since Escape fires no hook. A clean `Stop` with `fullyIdle: false` parks the turn on background work, and a failed one never parks ([model.md](./model.md#turn-endings-and-parked-turns)).

A supervised run (`rimz agents antigravity -p`) completes from the `Stop` hook. Its final message is the newest completed assistant text in the hook's transcript ([Transcript](#transcript)).

### Compaction

The compact command is a bare `/compact`, and Antigravity takes no compaction instruction: `/compact <message>` compacts and then answers the message as an ordinary turn, so RimZ refuses an instruction. agy 1.5.13 brackets a bare `/compact` with a `PreInvocation` (`invocationNum: 0`) and a `Stop` (`terminationReason: "TERMINAL_CUSTOM_HOOK"`), both carrying `lastUserInput: "/compact"`. The opener decodes to `Compacting`, which acknowledges a sent compact command, and reads no prompt. The closer decodes to `CompactionEnded` with `auto: Some(false)`, failed when the `Stop` reads as failed by the turn rule, whether or not it carries `fullyIdle`. It sets no final message, since the newest assistant row predates the compaction.

Two cases leave the bracket without a closer. An Escape during the compaction fires only the opener, so the compacting head stays until the next lifecycle signal. A `/compact` in a conversation with nothing to compact fires no hook, so a sent command times out unconfirmed. A user's own `/compact <message>` stays a turn, though agy compacts before it answers.

The transcript is append-only across a compaction: a `USER_INPUT` row for `/compact`, then a `SYSTEM` / `CHECKPOINT` row with the summarizing call's tokens. History drops a user row whose request is exactly `/compact`, so a sent command never shows as the user's words. The statusline keeps its pre-compaction context reading until the next turn ends, and RimZ's compacted-at guards (the command's recorded token count and the compacted-awaiting-prompt mark) keep idle compaction and compact-first from firing again on it. agy compacts on its own near the context limit by its docs, but no hook for that was observed, so `auto-compact` stays unsupported.

### Interrupt

The interrupt key is Escape. agy fires no hook for it, so no event confirms the stop, and a user's own Escape leaves the row Running until the next turn or a manual compaction. Escape also moves a message queued behind the running turn back into the composer, so the descriptor sets `interrupt_returns_queued_text` and the interrupt presses Enter 500 ms after the key to submit it ([the interrupt key](../harness/messaging.md#the-interrupt-key)). With no hook context, a supervised child's deadline stop goes out from the host at the deadline as that interrupt.

A stale Running row would make that interrupt cancel the turn its own text opened: the pre-write row reads Running, the paste starts a turn on the resting agy, and the Escape three seconds later cancels it. The same happens when the running turn ends during the delay and agy starts the queued text. agy opens no turn for text queued behind a running one until it dequeues it (no invocation-0 `PreInvocation`, probes P9 and P10), so after the delay `crates/rimz/src/message/send.rs::send_batch_to_live_pane` reads the row again and, when its `turn_started_at` is newer than the one on the row the write decided on, presses neither Escape nor Enter and reports the key unsent (`InterruptConfirmation::NotSent`). Residual: nothing proves the paste's `PreInvocation` is applied before that read. The feed waits up to 1.2 s for the drainer inside the 3 s delay, but under load a later apply leaves the old behavior, and the key cancels the paste's turn.

## Sessions

Antigravity registers lazily (`registers_lazily`). A live `agy` pane renders as the idle row of [instances.md](./instances.md#before-a-session-binds) until a hook or local discovery binds a conversation. Upstream documents `SessionStart`, but RimZ leaves it unwired until its behaviour on resume, `/clear`, and child conversations is captured ([reference](../../externals/agent-adapter/antigravity-reference.md#events)). The first `PreInvocation` of an unseen conversation creates it.

One live `agy` follows its latest conversation, so the spec declares `SamePaneSessionPolicy::FollowLatest`. Pane adoption, ownership, and the in-place-switch supersession follow from that policy ([instances.md](./instances.md#binding-a-session), [same-pane ownership](./instances.md#same-pane-ownership), [supersession](./instances.md#supersession)). RimZ records no `Fresh` origin for a switch, because the hook wire does not say whether `/clear` or `/fork` created the new conversation.

### Local discovery

Antigravity declares `local_session_discovery = FullScan`. `session.rs::discover` reads the provider home: `RIMZ_ANTIGRAVITY_HOME` when set, else `$HOME/.gemini/antigravity-cli`, both from the process environment. `discover_local_sessions` ignores the login env map. One pass serves every admitted workspace:

1. Catalog. `enumerate_conversations` lists every directory under `brain/` whose name passes `valid_conversation_id`. The shared `IncrementalCatalog` rescans when the stamped topology changes or after the 30-second backstop (`crates/rimz/src/agents/local_session_cache.rs::LOCAL_SESSION_DISCOVERY_BACKSTOP`).
2. Selection. A conversation counts when `.system_generated/logs/transcript_full.jsonl`, or else `transcript.jsonl`, is a regular file. The 512 newest by that file's mtime are kept (`MAX_DISCOVERED_SESSIONS`).
3. Fold. Each selected conversation is validated and folded from the shared 64 KiB record-aligned tail. The fold is cached against five stamped paths: the conversation, `.system_generated`, and `logs` directories and both transcript files. A transcript change or a fallback promotion refolds that conversation alone. A tail with no visible record yields no observation, and the first visible record in the tail stands in for the creation time.
4. Index. `cache/last_conversations.json` maps a workspace path to its current conversation id. It is parsed once per pass and cached on its own stamp, so an index-only change rematerializes without a rescan or a refold.
5. Materialize. Every folded conversation becomes one `Lifecycle` observation for each admitted workspace. Only the conversation the index names for that exact workspace path gets `fresh_binding_at`.

The store is provider-global, so an observation's workspace proves nothing about where the conversation ran. A conversation without `fresh_binding_at` binds only through an exact durable row or a `--conversation` command line. The fresh fallback is reserved for the index's pairing ([instances.md](./instances.md#local-session-observations) owns the binding order). Because hooks bind first, a lagging index cannot pull a native question away from a conversation the hooks already identified.

A transcript path is accepted only through `session.rs::valid_transcript_under`. `brain/` and `brain/<id>` must be real directories, not symlinks, and `brain/<id>` must canonicalize directly under `brain/`. The path itself must be a regular, non-symlink file that canonicalizes to `brain/<id>/.system_generated/logs/` plus one of the two basenames. Discovery, `session_transcript`, the first-prompt read, and the subagent join all check this. The `Stop` final-message read does not ([Known gaps](#known-gaps)).

## Transcript

The 1.1.2 fixtures define the record shape: JSONL with `step_index`, `source`, `type`, `status`, `created_at`, and optional `content` and `tool_calls` ([reference](../../externals/agent-adapter/antigravity-reference.md#transcripts)). A line missing a required field is skipped. `session.rs::fold` reads records in physical order, and only these records move the projection:

| Record | Fold effect |
| --- | --- |
| `USER_EXPLICIT` / `USER_INPUT`, `DONE` | `running`, phase `reasoning`, prompt set, native question cleared. This is the turn start |
| `MODEL` / `PLANNER_RESPONSE`, `DONE`, with an `ask_question` call holding a nonblank question | `waiting`, phase idle, the first nonblank question as the native prompt detail, `waiting_since` from `created_at` |
| `MODEL` / `PLANNER_RESPONSE`, `DONE`, with any other tool call (a blank `ask_question`, `invoke_subagent`, and `run_workflow` included) | `running`, phase `reasoning`, native question cleared. The requested tool has no later planner row yet, so the turn is open |
| `MODEL` / `PLANNER_RESPONSE`, `DONE`, with no tool call | `success`, phase idle, native question cleared |
| `MODEL` / `PLANNER_RESPONSE`, `ERROR` | `failed`, phase idle, native question cleared. The content is never shown |
| anything else (`SYSTEM` history and checkpoints, other statuses, unknown sources or types, unparseable `created_at`) | ignored |

While a tool waits at agy's native permission prompt or runs, the transcript already ends in its planner row, so the fold reads `running` and a newer statusline `NativeWait` raises the card to `waiting` ([Statusline context](#statusline-context)). A turn stopped with Escape during a tool call fires no hook and writes no later row, so it reads `running` until the next turn. The `ERROR` row is an assumption: no failure capture has shown which source and type carry a terminal model error. A later turn start or completed planner response supersedes it. User content wrapped in `<USER_REQUEST>` contributes only the text up to the matching `</USER_REQUEST>`, so the trailing metadata and settings blocks stay private. An unclosed or empty wrapper yields no prompt. Unwrapped text passes through as it is. Both forms then pass through `sanitize_user_prompt`.

`ask_question.args.questions` is an array in `transcript_full.jsonl`, and `transcript.jsonl` may carry it as a JSON-encoded string. The parser accepts either form in either file. The native question stays pane-only: it creates no durable RimZ ask, and an accepted fold clears any older routable ask ([model.md](./model.md#waiting-and-asks)). A fold overlays a durable row only when its activity is at least as recent, compared in whole seconds (`crates/rimz/src/store/snapshot/view/live.rs::local_observation_is_current`). A fold written in the same second as a newer hook can therefore replace that hook's state until the next hook ([instances.md](./instances.md#local-session-observations)).

Four readers use the transcript:

| Reader | Path source | Read | Rule |
| --- | --- | --- | --- |
| history (`session.rs::messages`) | `session_transcript`, validated | the lines the shared transcript reader supplies | `DONE` visible records with content, user text normalized as above, less a user request that is exactly `/compact` |
| first prompt (`session.rs::latest_prompt`) | hook `transcriptPath`, validated | 64 KiB tail | on `TurnStarted` only. The latest completed visible user prompt. Other hooks read no prompt |
| final message (`session.rs::last_assistant_message`) | hook `transcriptPath`, not validated | 64 KiB tail, then whole file | on every `Stop` that decodes to `TurnEnded`. The newest `DONE` planner text after the last turn start. When the tail holds neither a turn start nor an answer, it reads the whole file without a bound, and a file without a turn start counts as one turn |
| subagent join | parent row's path, or rebuilt from the parent id | 64 KiB tail | [Subagents](#subagents) |

A turn with no answer returns no final text, never the previous turn's reply. A torn final record stays out of every tail until it completes. Once a hook records the prompt, a later discovery tail with no visible user record supplies no newer prompt, so the card keeps its launch description or hook prompt through a long turn.

## Subagents

Antigravity fires the ordinary hook set for each asynchronous or nested child, with the child's own `conversationId` and transcript and no parent id ([reference](../../externals/agent-adapter/antigravity-reference.md#subagents-and-background-tasks)). RimZ recovers the parent from the parent's transcript, where a completed `MODEL` / `PLANNER_RESPONSE` record holds an `invoke_subagent` call with ordered `Subagents` specs, and a later completed `MODEL` / `INVOKE_SUBAGENT` record's `content` returns one `{conversationId, logAbsoluteUri, workspaceUris?}` object per spec in the same order. This shape is live-verified, not documented upstream.

### The forward join

`crates/rimz/src/cli/hooks/lifecycle/observe.rs::correlate_subagent_observation` runs on a root `TurnStarted`, `TurnEnded`, or `ToolUsed`:

1. Known child. A prior row of this id with a parent supplies the relation, with no transcript read.
2. Trigger. Otherwise only a `TurnStarted` or an unparked `TurnEnded` tries a join, and the hook needs a pane and a worktree path.
3. Candidates. The candidates are the other Antigravity rows stamped on the same pane, newest first. Above 64 (`MAX_SUBAGENT_PARENT_CANDIDATES`) the join is skipped.
4. Match. `AntigravityAdapter::correlate_subagent` tests each candidate against its validated transcript. Exactly one candidate must match, and a match that resolves to the child itself is discarded.
5. Flatten. A candidate that is itself a child stands in for its root parent, so nested children attach to the root card.

A match sets the child's name from `TypeName`, its role from `Role`, its task from `Role` or else `Prompt`, and its prompt from `Prompt`. It also clears the hook's model hint, because the correlation carries no model. Later child hooks reuse the stored relation and keep their `modelName`. Child observations take no root launch environment or configured identity.

| Child signal | Becomes |
| --- | --- |
| `TurnStarted` | `SubagentStarted` |
| `TurnEnded`, unparked | `SubagentStopped { errored }` |
| `TurnEnded`, parked on background | unchanged, until the terminal `Stop` |
| `ToolUsed` | unchanged, keyed to the child |

### Parent validation

`session.rs::validated_subagents_under` reads the parent's 64 KiB tail. A request whose result falls inside the tail but whose `invoke_subagent` record lies before it leaves a result with no request, which voids the record. Some failures void every child of the parent transcript, and others drop one child:

| Scope | Failure |
| --- | --- |
| whole record | the parent path fails [path validation](#local-discovery), the parent workspace does not canonicalize, or the tail ends in a torn record |
| whole record | any line fails to parse as a transcript record |
| whole record | an `invoke_subagent` call does not parse, has no specs, or has a spec with both `Prompt` and `Role` blank or a blank `Workspace` |
| whole record | an `INVOKE_SUBAGENT` record is not `DONE`, has no pending request, or holds fewer objects than specs or a further object after them |
| whole record | a valid child id repeats, even when either copy would be dropped later |
| whole record | a request is still unmatched at the end of the tail |
| one child | the returned id fails `valid_conversation_id` |
| one child | `logAbsoluteUri` is not a `file:` URI, or not a valid transcript of that child id |
| one child | the spec's `Workspace` is relative (such as `branch` or `share`) or does not canonicalize. An absent `Workspace` or the literal `inherit` means the parent's workspace |
| one child | `workspaceUris` is present but empty, holds a URI that does not canonicalize, or does not contain the expected workspace |

A child then joins only when its id equals the result's id and its canonical hook workspace equals the expected workspace. A worktree child whose hook workspace differs stays a root.

### The reverse pass

1.1.2 buffers the parent transcript. In a two-child capture, both children's `PreInvocation` and `Stop` hooks ran before the parent's request and result records reached disk, and the file flushed before the parent's own `Stop`. The forward join therefore often fails, and a reverse pass catches up.

On every root `ToolUsed` or `TurnEnded`, `crates/rimz/src/cli/hooks/lifecycle/observe.rs::record_mapped_lifecycle_observation` calls `AntigravityAdapter::spawned_subagents`. It returns every validated child in the parent's tail. The store writer (`crates/rimz/src/store/writer/lifecycle.rs`) then emits a `SubagentStopped` for each child, errored when the child row is `failed`:

- A child with no parent is adopted (`SubagentAdopted`), unless its row is stamped on another pane. Adoption also restores a child that a supersession reap already hid ([instances.md](./instances.md#supersession)).
- A child already under this parent is reconciled (`SubagentReconciled`) while its row is still `running`, or when its model or tokens changed. A settled child with no change is skipped.

The pass treats a returned result as proof that the child finished, and no capture confirms that for an asynchronous child. A child still `running` is closed by its parent's next tool hook. Until the reverse pass runs, the supersession open-turn guard keeps the active parent from being reaped by a provisional child root ([instances.md](./instances.md#supersession)).

## Statusline context

The wrapper runs `RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source antigravity`, which forwards the user's prior command and prints nothing when there is none. `statusline.rs::StatuslinePayload` needs `conversation_id` (or `conversationId`) and maps the rest into `AgentContext`:

| Payload field | `AgentContext` |
| --- | --- |
| `model.id` | `model_id`, unchanged |
| `model.display_name` | `model_display_name`, with a recognized trailing qualifier removed |
| `model.effort`, else the display-name qualifier | `effort`, trimmed and lowercased |
| `(Thinking)` qualifier | `thinking_enabled` |
| `version` | `agent_version` |
| `plan_tier`, `email` | `account` plan and id, `metered: true` |
| `context_window` size, percentages, `current_usage` (`null` before the first model turn) | `tokens`. `current_context_tokens` stays empty, so the gauge comes from the percentages and window size alone |
| `quota`, its `gemini-` buckets only | `rate_limits`: `BestEffort` 5h and weekly windows stamped at `observed_at` ([Quota windows](#quota-windows)) |
| `tool_confirmation_pending: true` | a `NativeWait` settle stamped at observation time |

Antigravity puts the selector label, such as `Gemini 3.5 Flash (Medium)`, in both `model.id` and `model.display_name` ([reference](../../externals/agent-adapter/antigravity-reference.md#payload)). The recognized qualifiers are `(Low)`, `(Medium)`, `(High)`, `(XHigh)`, `(Extra High)`, `(Max)`, and `(Thinking)`, matched without case. `Extra High` maps to `xhigh`. An unknown parenthetical stays in the label. The `(Extra High)` and `(Max)` spellings come from the 1.3.3 effort list and have no label capture yet.

The `NativeWait` settle projects a `running` card to `waiting` while it is newer than the row's activity ([model.md](./model.md#waiting-and-asks)). It creates no ask and sends nothing to Antigravity. Any later hook advances the row's activity past the marker, so a missed `false` refresh clears itself.

RimZ reads no other statusline field. `cost` has no published shape and RimZ prices the transcript instead ([Cost](#cost)), `subagents` carries no stable ids, and `conversation_title` would replace RimZ's launch description on the card.

The plan, the account id, and `metered` reach the login's account record through the producer's live merge, and the windows reach the rate-limit cache, so every surface shows them from there ([the account record](./providers.md#the-account-record)). One session's payloads fill in stages: the first carry neither `quota` nor `plan_tier`. A payload without them is no reading, and what the record and the cache already hold stays.

## Account and balance

One print-mode run, `agy -p /usage --output-format json`, answers both account probes. It spends no model turn, fires no hook, and leaves no conversation ([reference](../../externals/agent-adapter/antigravity-reference.md#exit-timeout-and-read-only-commands)). RimZ reads no OAuth token and sends no pane input. The version probe returns `None` instead of running `agy --version` ([providers.md](./providers.md#the-out-of-band-probe)).

`crates/rimz/src/agents/adapters/antigravity/usage.rs::run` starts the `agy` that `locate_binary` finds, with the inherited environment plus:

- `SSH_CONNECTION`, and one newline on stdin. A logged-out `agy -p` otherwise waits 60 s for a sign-in and opens a browser. The SSH variable selects agy's URL-and-code flow, and the newline answers its code prompt, so the run exits within a second.
- the login env's `HOME`, when it carries one.
- a 10 s deadline (`DEADLINE`), after which the process group is killed and reaped. An offline run took 7.3 s on 1.5.12, a logged-in one at most 2.8 s.

`usage::classify` reads stdout as one JSON envelope, and `Read::account` and `Read::usage` map the one reading onto both probes:

| Outcome of the run | `probe_account` | `probe_account_usage` |
| --- | --- | --- |
| exit 0, `status: "SUCCESS"`, `command.name: "usage"`, windows fold | `Found`, `metered: true`, and the credential file's account id | `Found`, default identity, the windows, no plan |
| the same envelope with no bucket | `Found` | `NoCredentials` |
| the same envelope whose buckets fail the fold, a type mismatch included | `Found` | `Failed` |
| `status: "ERROR"` whose `error` contains `authentication` in any case, at any exit | `LoggedOut` | `NoCredentials` |
| any other `ERROR` envelope, at any exit | `Unavailable(SourceUnavailable)` | `Failed` |
| a non-zero exit without an `ERROR` envelope | `Unavailable(ExitNonZero)` | `Failed` |
| the deadline | `Unavailable(TimedOut)` | `Failed` |
| no binary, or a failed spawn | `Unavailable(SpawnFailed)` | `Failed` |
| exit 0 without that envelope | `Unavailable(UnparseableOutput)` | `Failed` |
| `RIMZ_OAUTH_USAGE_OFFLINE` set, `agy` never started | `Unavailable(SourceUnavailable)` | `Failed` |

The `/usage` reading names no email, so the usage identity carries no owner key and its default `KindWide` scope ([providers.md](./providers.md#the-credits-cache-and-usage-identity)). Where the CLI stores tokens in files rather than the keyring, `Read::account` adds the account id from the `id_token_email` field of `<provider home>/antigravity-oauth-token`, stamped at the file's mtime; it reads no other field and never logs the value. `rimz accounts show` and `rimz accounts list --json` print it as the account id, as they do Cursor's email. On a keyring install only the statusline names the account. The plan comes only from the statusline, through the account record, so `rimz accounts list` and the sidebar show it once a session has reported it, with no expiry.

`usage::read` keeps each reading for 60 s (`REUSE`) per provider home (`config_home`) within one process, whichever probe asks and whatever the reading was, so one `rimz accounts list` starts `agy` once. The reuse is per process, and the sidebar's two lanes run in different ones: the account lane probes in the sidebar host (`crates/rimz/src/sidebar/refresh/accounts.rs`), while the usage read runs in a detached `rimz agents refresh-usage` helper (`crates/rimz/src/sidebar/refresh/usage.rs`). A cold pass therefore starts `agy` twice, and the steady state is the usage claim cadence (every 5 min while an agent is in use, 60 min once a read settles on a login failure, 3 h idle) plus one account start per `ACCOUNTS_TTL` (10 min), where a failed account probe retries after `ACCOUNTS_RETRY_TTL` (10 s), doubling up to that window. The reuse stamp is taken before the run, and the windows carry the run's start as `observed_at`.

### Quota windows

`usage.rs::rate_limits` folds quota buckets into at most two windows, for both sources: the buckets of every `/usage` group, and the statusline's `quota` map. Only a bucket whose id starts with `gemini-` counts, and the id's suffix gives its period: `5h` means five hours and `weekly` a week. Every other bucket, the `3p-` buckets of Claude and GPT models included, is never parsed, so its content cannot change or fail a reading.

- Each enabled Gemini bucket with a remaining fraction joins its period. A disabled bucket, or one without a fraction, marks the period present and contributes nothing.
- The period's window is the bucket with the smallest remaining fraction. A tie takes the later reset.
- A present period with no usable bucket publishes a window with unknown fill and reset. An absent period publishes no window, so a weekly-only response yields one window.
- Used percentage is `100` only at a remaining fraction of exactly `0`. Otherwise it is rounded and capped at `99`.
- A Gemini bucket of the wrong shape, a usable bucket's fraction outside `0..=1`, or a reset that is missing, unparseable, or not in the future, rejects the whole reading. So does a `/usage` response whose buckets hold no Gemini one. A `/usage` response with no bucket, and a statusline payload whose map holds no valid Gemini window, are no reading.

The `/usage` windows are `Authoritative` and the statusline's are `BestEffort`, as Claude's are ([providers.md](./providers.md#window-fusion)), and RimZ invents no credits or dollars from them. They never arm auto-continue, because no Antigravity stop class is certified recoverable ([providers.md](./providers.md#auto-continue)).

## Cost

`spend.rs` reads `brain/<conversation>/.system_generated/logs/transcript_full.jsonl` under the login's provider home (`config_home`). `transcript.jsonl` repeats the same rows and is never a source. Each row carrying `input_tokens` (a completed `MODEL` / `PLANNER_RESPONSE`, or a compaction's `SYSTEM` / `CHECKPOINT`) becomes one entry: message id `<conversation>:<step_index>`, thread id the conversation, time from `created_at`. `input_tokens` excludes cache reads and `output_tokens` already counts thinking, so `input_tokens`, `cache_read_tokens`, and `output_tokens` are disjoint inputs to `Pricing::cost_of`. The model is the row's slug with its effort suffix (`-low`, `-medium`, `-high`, `-xhigh`, `-max`) removed. Pricing tries the raw slug, then that base name, with `PriceBook::exact_price` only, so a related model never lends its rates ([spending.md](./spending.md#resolving-a-model)). A model neither prices keeps its tokens without dollars and is recorded unknown under its base name. A `CHECKPOINT` row names no model, so its tokens count at $0 and record no unknown model. Rows name no working directory, so Antigravity spend carries no workspace attribution.

The dollars are a list-price estimate: Antigravity quota is measured by work, and a plan bills no tokens. That is why `account_spend` is partial, which keeps Antigravity out of account-day caps.

The statusline carries no dollars into RimZ. At each turn end the hook path sets the card's cost to the conversation's transcript total (`supplement_realtime_cost` through `session_cost_usd`) with `Session` coverage, and later statusline payloads keep it ([spending.md](./spending.md#live-cost-coverage)). So the card shows no price until the first turn ends, and the figure moves only at turn ends. It counts toward room spend and per-agent and room budget caps.

## Known gaps

`rimz coverage --wiring` prints the declared concern and lifecycle verdicts. These gaps have a reason worth recording:

- No recoverable failure. No 1.3.3 error class carries a provider-owned typed discriminator captured twice, so every errored `Stop` is terminal. Message text, error-shaped objects, quota exhaustion, and a stalled pane never arm auto-continue, and a supervised run or loop attempt does not survive an Antigravity limit.
- Subagents are partial. Child hooks are documented, but the parent's request and result records are live-verified only. An errored invocation may write an `INVOKE_SUBAGENT` record with status `ERROR` ([reference](../../externals/agent-adapter/antigravity-reference.md#transcripts)). Its shape is uncaptured, and the non-`DONE` status voids every sibling in that record.
- The reverse pass closes children on a returned result, which may arrive before an asynchronous child finishes ([The reverse pass](#the-reverse-pass)).
- The `Stop` final-message read takes the hook's `transcriptPath` without path validation, and its fallback reads the whole file.
- No billed ledger. Machine-readable credits, API keys or BYOK, and account-day dollar caps are unsupported. Spend is a list-price estimate from the transcripts, and carries no workspace.
- Permission wait detail, durable asks, artifact-review waits, and structured answers are unsupported. The user answers native prompts in the Antigravity pane.
- Remote control is unsupported. Upstream offers a daemon (`agy remote-control`) and session-scoped connections (`--remote-control`, `/remote-control`), and RimZ wires neither ([reference](../../externals/agent-adapter/antigravity-reference.md#remote-control-mcp-plugins-and-custom-agents)).
- Tool statistics are partial. Names come only from live `PostToolUse.toolCall`, on releases that send it and for tools the three matchers cover. A lazily loaded MCP tool counts only as `call_mcp_tool`, without its server or tool name, an eager one (`mcp_<server>_<tool>`) not at all, and transcript history is not counted.
- Registration is derived. `SessionStart` stays unwired until a live capture shows its behaviour on resume, `/clear`, and child conversations.

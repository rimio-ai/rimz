# Codex protocol reference

This page mirrors the upstream surfaces of OpenAI's Codex CLI that RimZ binds to: hook events and their decision schema, project directory trust, launch flags and config overrides, the TUI input and notification channels, the app-server JSON-RPC API and its daemon, rollout and session-index files, and the ChatGPT auth file and usage endpoints. It records what upstream ships. How RimZ maps each surface onto its own types, and which events and methods it wires, is in [adapter_codex.md](../../internals/agents/adapter_codex.md); the provider-neutral model is [model.md](../../internals/agents/model.md), and accounts and spend are [providers.md](../../internals/agents/providers.md).

## Baseline and sources

The page describes Codex CLI **0.154.0** (tag `rust-v0.154.0`, commit `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`, released 2026-09-09), read on 2026-09-13. That is the GitHub latest release and the npm `@openai/codex` `latest` dist-tag. A source reference written as `path:line` is relative to `codex-rs/` at that tag. Generated app-server shapes come from `codex app-server generate-json-schema` run on the 0.154.0 binary, and live rollout samples come from the same build. A fact that exists only on `main` is marked unreleased with its pull request.

| Surface | Source |
| --- | --- |
| Hooks: events, payloads, output, trust, limits | <https://learn.chatgpt.com/docs/hooks>; [`hooks/src/schema.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/hooks/src/schema.rs), [`hooks/src/engine/`](https://github.com/openai/codex/tree/rust-v0.154.0/codex-rs/hooks/src/engine), generated schemas in [`hooks/schema/generated/`](https://github.com/openai/codex/tree/rust-v0.154.0/codex-rs/hooks/schema/generated) |
| Hook event enum and runtime dispatch | [`protocol/src/protocol.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/protocol/src/protocol.rs) `HookEventName`, [`core/src/hook_runtime.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/core/src/hook_runtime.rs) |
| Hook tool names | [`core/src/tools/hook_names.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/core/src/tools/hook_names.rs) |
| Project directory trust | [`tui/src/lib.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/lib.rs), [`config/src/loader/mod.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/config/src/loader/mod.rs), [`git-utils/src/trust.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/git-utils/src/trust.rs) |
| Config keys (`notify`, `[tui]`, credential store, `projects`, compaction) | <https://learn.chatgpt.com/docs/config-file/config-reference>, <https://learn.chatgpt.com/docs/config-file/config-advanced>; [`config/src/config_toml.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/config/src/config_toml.rs), [`config/src/types.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/config/src/types.rs) |
| CLI commands and `-c` parser | <https://learn.chatgpt.com/docs/developer-commands?surface=cli>; `codex <command> --help`; [`utils/cli/src/config_override.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/utils/cli/src/config_override.rs), [`cli/src/login.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/cli/src/login.rs) |
| Auto-compaction ceiling | [`protocol/src/openai_models.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/protocol/src/openai_models.rs), [`protocol/src/config_types.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/protocol/src/config_types.rs) |
| TUI paste burst, plan prompt, questionnaire | [`tui/src/bottom_pane/paste_burst.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/bottom_pane/paste_burst.rs), [`tui/src/chatwidget/plan_implementation.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/chatwidget/plan_implementation.rs), [`tui/src/bottom_pane/request_user_input/mod.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/bottom_pane/request_user_input/mod.rs) |
| App-server protocol | <https://learn.chatgpt.com/docs/app-server>; [`app-server/README.md`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/app-server/README.md), [`app-server-protocol/src/protocol/`](https://github.com/openai/codex/tree/rust-v0.154.0/codex-rs/app-server-protocol/src/protocol) |
| App-server daemon and updater | [`app-server-daemon/README.md`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/app-server-daemon/README.md), [`app-server-daemon/src/`](https://github.com/openai/codex/tree/rust-v0.154.0/codex-rs/app-server-daemon/src) (`lib.rs`, `backend/pid.rs`, `update_loop.rs`, `managed_install.rs`) |
| Rollout files, persistence policy, compression, session index | [`rollout/src/`](https://github.com/openai/codex/tree/rust-v0.154.0/codex-rs/rollout/src) (`recorder.rs`, `policy.rs`, `rollout_file_name.rs`, `compression.rs`, `session_index.rs`), [`protocol/src/protocol.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/protocol/src/protocol.rs), [`protocol/src/items.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/protocol/src/items.rs) |
| `auth.json` | [`login/src/auth/storage.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/login/src/auth/storage.rs) |
| ChatGPT usage and reset-credit endpoints | [`backend-client/src/client/rate_limit_resets.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/backend-client/src/client/rate_limit_resets.rs), [`backend-client/src/types.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/backend-client/src/types.rs), [`codex-backend-openapi-models/src/models/`](https://github.com/openai/codex/tree/rust-v0.154.0/codex-rs/codex-backend-openapi-models/src/models) |
| Release notes | <https://github.com/openai/codex/releases> |

The app-server protocol publishes no version string. The binary generates its exact schema; `--experimental` adds the methods and fields gated as experimental:

```bash
codex app-server generate-ts --out DIR
codex app-server generate-json-schema --out DIR [--experimental]
```

## Hooks

A Codex hook is a handler Codex runs at a lifecycle point. A command handler receives a JSON payload on stdin and answers on stdout. The event set, payloads, and output rules follow Claude Code's hook shape with the differences recorded below; never reuse a Claude decision without checking this section.

### Config shape

Hooks are declared as `[[hooks.<Event>]]` groups in a `config.toml`, or in a `hooks.json` beside it. Codex discovers both beside the user config (`$CODEX_HOME/config.toml`, default `~/.codex/config.toml`), beside each trusted project's `.codex/` layer, and in plugin and managed layers. Sources merge; a layer holding both `hooks.json` and TOML hooks gets a startup warning. Hooks are enabled by default: `[features].hooks = false` disables them, and `codex_hooks` is a deprecated alias for that feature (`features/src/lib.rs:1166`, `features/src/legacy.rs:49`).

```toml
[[hooks.PreToolUse]]
matcher = "^Bash$"

[[hooks.PreToolUse.hooks]]
type = "command"
command = '/usr/bin/python3 "path/to/script.py"'
commandWindows = 'py -3 C:\path\script.py'
timeout = 30
statusMessage = "Checking Bash command"
```

| Field | Meaning |
| --- | --- |
| `matcher` (group) | Regex over the tool name, or over the source or trigger for lifecycle events. `UserPromptSubmit`, `Stop`, and `Interrupt` ignore it (`hooks/src/events/common.rs:112`). |
| `type` | `command` and `mcp_tool` run. `prompt` and `agent` parse and are skipped with a warning (`hooks/src/engine/discovery.rs:635`, `:645`). `mcp_tool` is unsupported on `SessionEnd`. Handler fields are in `HookHandlerConfig` (`config/src/hook_config.rs:163`). |
| `command`, `commandWindows` | Shell command; the Windows variant (alias `command_windows`) replaces it on Windows. |
| `server`, `tool`, `input` | The MCP server, tool, and TOML-representable input of an `mcp_tool` handler. |
| `timeout` | Seconds. Default 600. `SessionEnd` and `Interrupt` default to 1 and clamp to 1 through 3 (`discovery.rs:740` to `:763`). |
| `async` | A command with `async = true` runs in the background and cannot apply control effects. At most eight background hooks run per session; the rest queue (`MAX_CONCURRENT_ASYNC_HOOKS`, `hooks/src/engine/command_runner.rs:45`). `async` on `SessionEnd` runs synchronously with a warning. |
| `statusMessage` | Text the TUI shows while the hook runs. |
| `additionalContextLimit` | Approximate token threshold for spilling a command hook's `additionalContext` to disk; unset means 2,500 and `0` disables spilling. Overflow is saved under `<temp_dir>/hook_outputs/<session_id>/` and the model sees a head and tail preview ([hooks docs, "Large hook output"](https://learn.chatgpt.com/docs/hooks)). |

Managed hooks come from `requirements.toml` `[hooks]` (`managed_dir`, `windows_managed_dir`); `allow_managed_hooks_only` makes Codex ignore every other source.

### Execution

Codex starts all matching handlers for one event concurrently (`hooks/src/engine/dispatcher.rs:125`), with the session cwd as working directory. `build_command` (`command_runner.rs:390` to `:426`) clears the child environment, replays the environment snapshot `Hooks::new` captured from the Codex process (`hooks/src/registry.rs:79`), applies the source's environment overlay, and then scrubs non-inheritable credential variables. Only plugin hooks have an overlay (`PLUGIN_ROOT`, `CLAUDE_PLUGIN_ROOT`, `PLUGIN_DATA`, `CLAUDE_PLUGIN_DATA`); a handler has no `env` field.

A plain TUI launch runs its session inside the shared per-user app-server daemon ([App-server daemon](#app-server-daemon)), so a hook child's parent process is the daemon, and the environment snapshot is the daemon's.

### Trust state

Codex runs a non-managed hook only after the user trusts its current definition. Trust is recorded per handler as a hash, so a new or changed handler is skipped until the user reviews it in `/hooks`; the TUI raises a startup review listing hooks that need it (`tui/src/startup_hooks_review.rs`), and the engine never runs them in the meantime. `--dangerously-bypass-hook-trust` runs enabled hooks without trust for one invocation. Managed hooks, and the bundled cleanup-plugin hooks Codex treats as built-ins, need no trust (`discovery.rs:713` to `:735`).

State lives in the user config as `[hooks.state]` entries, `{ enabled, trusted_hash }`, keyed `"<key_source>:<event_label>:<group>:<handler>"` (`hooks/src/lib.rs:113`, `config/src/hook_config.rs:28`). For a config-file hook `key_source` is the config path; for a plugin hook it is `"<plugin_id>:<relative_path>"`. The event label is the event name in lower snake case, and the hash carries a `sha256:` prefix (`config/src/fingerprint.rs:61`):

```toml
[hooks.state."/home/user/.codex/config.toml:permission_request:0:0"]
trusted_hash = "sha256:…"
```

### Common input

Every command hook except `SessionEnd` receives this envelope (`hooks/src/schema.rs`):

| Field | Type | Notes |
| --- | --- | --- |
| `session_id` | string | The root thread id; also the rollout UUID. |
| `transcript_path` | string or null | Rollout path. |
| `cwd` | string | Session cwd. |
| `hook_event_name` | string | Event name. |
| `model` | string | Active model slug. |
| `permission_mode` | string | `default`, `acceptEdits`, `plan`, `dontAsk`, or `bypassPermissions` (`schema.rs:843`). Absent on `PreCompact` and `PostCompact`. |
| `turn_id` | string | Turn-scoped events only; absent on `SessionStart`. |
| `agent_id`, `agent_type` | string, optional | Present on `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, and `PostCompact` when the hook fires inside a child thread. The hooks docs omit them on these events; the schema carries them. |

A child observation carries an `agent_id` distinct from the root `session_id`. Hooks carry no child nickname, task path, token usage, or assignment prompt; those are in the child's [rollout header](#rollout-records).

### Events

Codex 0.154.0 has twelve hook events (`HookEventName`, `protocol/src/protocol.rs:1576`) and no `Notification` or dedicated plan-approval event.

| Event | Fires | Event-specific input |
| --- | --- | --- |
| `SessionStart` | a root session starts, resumes, clears, compacts, or forks; dispatched at the start of the next turn (`run_pending_session_start_hooks`, `core/src/hook_runtime.rs:124`) | `source`: `startup`, `resume`, `clear`, `compact`, or `fork` |
| `UserPromptSubmit` | the user submits a prompt | `turn_id`, `prompt` |
| `SubagentStart` | a spawned child thread starts | `turn_id`, `agent_id`, `agent_type`, `permission_mode` |
| `PreToolUse` | before a hooked tool call ([tool names](#tool-names)) | `turn_id`, `tool_name`, `tool_use_id`, `tool_input` |
| `PermissionRequest` | an approval is needed (shell escalation, network) | `turn_id`, `tool_name`, `tool_input` (with optional `description`) |
| `PostToolUse` | after a hooked tool produces output | `turn_id`, `tool_name`, `tool_use_id`, `tool_input`, `tool_response` |
| `SubagentStop` | a child thread stops | `turn_id`, `agent_id`, `agent_type`, `agent_transcript_path`, `stop_hook_active`, `last_assistant_message` |
| `Stop` | a turn completes | `turn_id`, `stop_hook_active`, `last_assistant_message` |
| `Interrupt` | a root turn is interrupted (`TurnAbortReason::Interrupted` only) | `turn_id`, plus common `model` and `permission_mode` |
| `PreCompact` | before compaction | `turn_id`, `trigger` (`manual` or `auto`) |
| `PostCompact` | after compaction | `turn_id`, `trigger` |
| `SessionEnd` | the root session runtime shuts down | `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `reason` (always `other`); no `model`, `permission_mode`, or `turn_id` |

`Interrupt` and `SessionEnd` fire only for root threads, each after Codex flushes the rollout (`hook_runtime.rs:455` to `:526`). `Interrupt` fires before the `turn_aborted` record is emitted (`core/src/tasks/mod.rs:807`, `:965`). `SessionEnd` runs from the shutdown handler, which the app-server also reaches when it unloads an idle thread (`unload_thread_without_subscribers`, `app-server/src/request_processors/thread_lifecycle.rs:422`), so the event does not prove the interactive session ended. Codex 0.152.0 added executor-plugin `Interrupt` hooks (#41432); they run async and leave the command-hook payload unchanged.

Compaction runs in place: Codex writes a `compacted` record into the same rollout, keeps the session id, and queues `SessionStart` with `source = "compact"` for the next turn (`core/src/session/mod.rs:3831` to `:3847`). Local 0.154.0 rollouts confirm it: sessions with several compactions keep one file and a parentless header.

`/btw` (alias `/side`) forks an ephemeral side thread: `side_fork_config` sets `ephemeral = true` ([`tui/src/app/side.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/app/side.rs)). No rollout is persisted, so `hook_transcript_path` yields a null `transcript_path` ([`core/src/hook_runtime.rs`](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/core/src/hook_runtime.rs)). Its `SessionStart` carries `source: "fork"`; later `UserPromptSubmit` and `Stop` hooks carry the new session id and null transcript path, but no source or ephemeral marker. A persistent fork has a materialized rollout; an ephemeral root instead starts with `source: "startup"`.

### Tool names

Hook `tool_name` values are canonical names, distinct from the function names a rollout records (`core/src/tools/hook_names.rs`):

| Hook `tool_name` | Covers | Matcher aliases |
| --- | --- | --- |
| `Bash` | simple and unified-exec shell commands, including the final `write_stdin` poll that completes a unified exec | none |
| `apply_patch` | file edits | `Edit`, `Write` |
| `spawn_agent` | child thread spawn | `Agent` |
| `mcp__<server>__<tool>` | MCP tool calls | none |
| `request_user_input` | a blocking questionnaire ([questions](#questions-and-plan-approval)) | none |
| `request_user_input_async` | a non-blocking question set; returns `{"accepted":true}` at once | none |
| `update_plan` | the todo-plan tool; non-blocking | none |

Hooks are a partial interception boundary: web search and other unhooked tool paths bypass them. `update_plan` is off by default since 0.152.0; `[tools.update_plan] enabled = true` turns it on (`core/src/config/mod.rs:2659`). `request_user_input_async` is registered only for root threads whose model catalog entry advertises it (0.153.0).

For function tools without a dedicated hook contract, `tool_response` is the tool's text output as a JSON **string** (`core/src/tools/registry.rs:95` to `:125`). `request_user_input` is such a tool, so its `tool_response` is a string holding `{"answers":{…}}`, which a consumer must parse a second time.

### Hook output

Every output type is parsed with `deny_unknown_fields` (`hooks/src/engine/output_parser.rs`): an unrecognized field marks the hook run failed. The universal fields are `continue`, `stopReason`, `systemMessage`, and `suppressOutput`; `hookSpecificOutput` carries event-specific fields.

| Event | Accepted | Rejected (hook fails) |
| --- | --- | --- |
| `PreToolUse` | `hookSpecificOutput.permissionDecision` `allow` with `updatedInput`, or `deny` with `permissionDecisionReason`; `hookSpecificOutput.additionalContext`; legacy `decision: "block"` with `reason`; `systemMessage` | `ask`; `allow` without `updatedInput`; `updatedInput` without `allow`; `deny` without a reason; `decision: "approve"`; `continue: false`; `stopReason`; `suppressOutput` |
| `PermissionRequest` | `hookSpecificOutput.decision` `{ behavior: "allow" \| "deny", message }`; a deny without `message` gets "PermissionRequest hook denied approval"; `systemMessage` | `updatedInput`, `updatedPermissions`, `interrupt: true`, `continue: false`, `stopReason`, `suppressOutput` |
| `PostToolUse` | `decision: "block"` with `reason`; `additionalContext`; `continue: false` stops and sends feedback; `systemMessage`, `stopReason` | `updatedMCPToolOutput`, `suppressOutput`, `reason` without `decision` |
| `UserPromptSubmit` | `decision: "block"` with `reason`; `additionalContext`; plain stdout becomes context | |
| `SessionStart`, `SubagentStart` | `additionalContext`; plain stdout becomes context; `continue: false` stops only on `SessionStart`; `suppressOutput` is ignored on `SessionStart` | |
| `Stop`, `SubagentStop` | universal fields; `decision: "block"` with `reason` | `additionalContext` |
| `PreCompact`, `PostCompact` | universal fields; `suppressOutput` ignored | `additionalContext` |
| `Interrupt` | `systemMessage` only, shown as a warning; non-JSON stdout or a non-zero exit is a failure | everything else |
| `SessionEnd` | output ignored | |

The decision shapes a permission or pre-tool hook returns:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow", "message": "string" } } }
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": { "command": "string" } } }
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "string" } }
```

A `PermissionRequest` answer carries only `decision.behavior` and `decision.message`. Claude Code's `updatedInput`, `updatedPermissions`, and `interrupt` fail it.

Exit codes: exit 0 with JSON applies the output, and exit 0 with empty stdout continues. Exit 2 with non-empty stderr blocks, using stderr as the reason, on `PreToolUse`, `PermissionRequest`, `PostToolUse`, `UserPromptSubmit`, `Stop`, and `SubagentStop`; exit 2 with empty stderr, or on the compact events and `Interrupt`, is a failure.

## Project directory trust

Codex shows "Do you trust the contents of this directory?" at TUI startup whenever the resolved project has no trust level: `should_show_trust_screen` is `config.active_project.trust_level.is_none()` (`tui/src/lib.rs:2055`). A recorded `untrusted` is a decision and suppresses the screen. The screen comes before the first prompt, so an unattended launch in an undecided directory stops there and never starts a turn.

Decisions live in the merged configuration, keyed by absolute path; the user writes them to `$CODEX_HOME/config.toml`, and a managed layer can supply the same keys:

```toml
[projects."/home/user/src/app"]
trust_level = "trusted"   # or "untrusted"
```

`decision_for_dir` (`config/src/loader/mod.rs:1062`) takes the first candidate that carries a level, in this order:

1. The session cwd.
2. The nearest ancestor holding a `project_root_markers` entry (default `[".git"]`); a `.git` directory counts only if it holds `HEAD` (`find_project_root`, `loader/mod.rs:1548`).
3. The main repository root from `resolve_root_git_project_for_trust` (`git-utils/src/trust.rs:13`). For a linked worktree it reads the `.git` file, follows `gitdir` into `worktrees/<name>`, and returns the checkout owning the canonical `commondir`, so a worktree of a trusted repository is trusted through its main root.

Each candidate is looked up as its normalized canonical path and as written, so a symlinked directory and its target share one decision. Windows folds ASCII case; other platforms compare exactly (`loader/mod.rs:1450`, `:1463`).

Answering "Yes, continue" writes `trust_level = "trusted"` for the trust target: the main git root when there is one, the cwd otherwise (`tui/src/onboarding/onboarding_screen.rs:172`, `tui/src/config_update.rs:62`).

| Related behaviour | Upstream |
| --- | --- |
| What trust gates | Project-scoped `.codex/` layers: project-local config, hooks, and rules ([config reference](https://learn.chatgpt.com/docs/config-file/config-reference), `projects.<path>.trust_level`). User-level `$CODEX_HOME/config.toml` hooks run regardless. |
| Skipping the screen | No CLI flag skips it. Approval and sandbox flags leave trust untouched. |
| `codex exec --skip-git-repo-check` | Covers the separate not-a-git-repository refusal, which `--dangerously-bypass-approvals-and-sandbox` also bypasses (`exec/src/lib.rs:964` to `:969`). |
| Unparseable `config.toml` | The TUI exits at startup instead of prompting (`tui/src/lib.rs:2034` to `:2049`). |
| Helpers before trust | Since 0.154.0 (#42324) startup resolves helper executables from trusted system directories instead of `PATH`, and `codex doctor` inspects executables without running them. |

## Launch and config overrides

### Launch prompt, resume, and fork

| Command | Behaviour |
| --- | --- |
| `codex [OPTIONS] [PROMPT]` | Starts the interactive TUI, submitting `PROMPT` as the first message when given. The CLI parses arguments with clap (`cli/src/main.rs`), so everything after `--` is positional: `codex -- "<prompt>"` passes a prompt even when it begins with `-`. |
| `codex resume [SESSION_ID] [PROMPT]` | Reopens a session in place. `SESSION_ID` is a UUID or a session name (a parseable UUID wins); `--last` picks the most recent. |
| `codex fork [SESSION_ID] [PROMPT]` | Copies the conversation into a new session id and leaves the source untouched; `--last` forks the most recent. |
| `codex exec resume`, `codex exec fork` | Non-interactive equivalents. |

Both interactive commands accept an optional initial `PROMPT` in 0.154.0 (`codex resume --help`, `codex fork --help`). A fork's rollout header names its source in `forked_from_id`. Codex 0.154.0 also adds experimental `--worktree` and `/worktree` for new or forked sessions.

### CLI config overrides

Each `-c key=value` or `--config key=value` overrides one loaded configuration key for that launch, and wins over the same key in `~/.codex/config.toml`. A dotted key reaches nested tables. Codex parses the value as TOML and falls back to the raw string when TOML parsing fails (`utils/cli/src/config_override.rs`), so a caller that needs an exact string round trip emits a TOML-quoted value. `--enable <FEATURE>` and `--disable <FEATURE>` are shorthand for `-c features.<name>=true|false`.

| Key | Type and behaviour |
| --- | --- |
| `developer_instructions` | String, added as a developer-role message separate from the user prompt (`config_toml.rs:235`). |
| `model_instructions_file` | Path; replaces the built-in instructions (`config_toml.rs:253`). |
| `model_reasoning_effort` | Reasoning effort for the session model. |
| `model_auto_compact_token_limit` | `Option<i64>`, an absolute token threshold (`config_toml.rs:168`). The effective limit is `min(configured, 90% of the model context window)`, and 90% is the default when unset (`openai_models.rs:515`). Compaction triggers at `>=` the limit (`core/src/session/context_window.rs:105`), so zero or a negative value compacts at once. |
| `model_auto_compact_token_limit_scope` | `total` (default) counts all active context; `body_after_prefix` counts growth after the carried compaction prefix and compares against the configured value unclamped (`config_toml.rs:172`, `config_types.rs:49`). |

`model_auto_compact_token_limit` is a top-level key only: it is not a field of `[profiles.<name>]` (`config/src/profile_toml.rs`), and it has no dedicated flag or environment variable. The override parser requires a TOML integer for it, so `-c model_auto_compact_token_limit=200000` works and `200k` does not.

### Login status

`codex login status` prints one line to **stderr** and exits 0 when credentials are present (`cli/src/login.rs:443` to `:500`). It reports the login kind, never the plan or a token. Workload identity is checked first.

| Output line | Login kind |
| --- | --- |
| `Logged in using ChatGPT` | ChatGPT OAuth |
| `Logged in using an API key - <masked>` | OpenAI API key |
| `Logged in using Amazon Bedrock API key` | Bedrock API key |
| `Logged in using Amazon Bedrock AWS access keys` | Bedrock AWS credentials |
| `Logged in using access token` | access token |
| `Logged in using personal access token` | personal access token |
| `Logged in using workload identity` | workload identity |
| `Not logged in` | none |

### Per-launch skill overrides

[`skills.config`](https://developers.openai.com/codex/skills) entries accept `name` or `path` selectors and `enabled=false`; disabled skills are also unavailable to explicit `$skill` invocation. [CLI `-c` overrides](https://developers.openai.com/codex/config-advanced) can supply this array for one run. Local probes on 0.157.0 found that `name` selects the frontmatter name (directory fallback), there is no wildcard, and file `[[skills.config]]` entries remain in effect alongside CLI entries.

## TUI input and notifications

### Paste burst

The composer treats plain characters arriving at most 8 ms apart as a suspected paste once at least 3 arrive (`paste_burst.rs:159`, `:163`). It flushes the buffered burst after 8 ms of inactivity, or 60 ms on Windows (`:167` to `:170`), and appends Enter received during the burst or within the following 120 ms as a literal newline instead of submitting (`PASTE_ENTER_SUPPRESS_WINDOW`, `:160`). A bracketed paste clears the burst state through `clear_after_explicit_paste` (`:458`). A client typing raw keystrokes therefore needs a gap of more than 120 ms before a separate Enter; a bracketed paste does not.

`tui.disable_paste_burst = true` turns the heuristic off (`config/src/types.rs:753`, default false). Since 0.153.0 that is the key's home; the top-level `disable_paste_burst` still works as a fallback.

### `notify`

`notify` runs an external program when a turn completes, independent of hooks. It must be set in the user-level config; a project-local `notify` is ignored with a startup warning (`PROJECT_LOCAL_CONFIG_DENYLIST`, `config/src/loader/mod.rs:74` to `:87`).

```toml
notify = ["python3", "/path/to/notify.py"]
```

The program receives one JSON document as its last argument (`hooks/src/legacy_notify.rs:16`). `agent-turn-complete` is the only type:

```json
{
  "type": "agent-turn-complete",
  "thread-id": "string",
  "turn-id": "string",
  "cwd": "string",
  "client": "string (optional)",
  "input-messages": ["user messages preceding the turn"],
  "last-assistant-message": "string"
}
```

In-terminal notifications are a separate `[tui]` setting (`config/src/types.rs:633` to `:671`), and approval prompts appear only there:

| Key | Values |
| --- | --- |
| `tui.notifications` | `true`, `false`, or an array of event types such as `agent-turn-complete` and `approval-requested` |
| `tui.notification_method` | `auto`, `osc9`, or `bel` |
| `tui.notification_condition` | `unfocused` or `always` |

## App-server API

`codex app-server` is a bidirectional JSON-RPC 2.0 service with the `"jsonrpc":"2.0"` member omitted on the wire. It exposes thread, turn, account, config, and filesystem operations and streams notifications; the TUI itself runs on it. The protocol has three primitives: an **item** (one input or output unit with a `started`, optional `delta`, `completed` lifecycle), a **turn** (the items from one unit of agent work), and a **thread** (the durable session).

### Transport and handshake

`--listen <URL>` selects the transport (`codex app-server --help`):

| URL | Transport |
| --- | --- |
| `stdio://` | JSONL over stdin and stdout (default; `--stdio` is the same) |
| `unix://`, `unix://PATH` | WebSocket HTTP upgrade over a Unix domain socket, then JSON-RPC text frames. Bare `unix://` is `$CODEX_HOME/app-server-control/app-server-control.sock` (`app-server-transport/src/transport/mod.rs:55`). |
| `ws://IP:PORT` | WebSocket over TCP; experimental |
| `off` | no listener |

A client sends one `initialize` request per connection, then an `initialized` notification, before any other method. The `initialize` result carries `userAgent` (which embeds the Codex version), `codexHome`, `platformFamily`, and `platformOs`.

```jsonc
{ "method": "initialize", "params": { "clientInfo": { "name": "my-client", "version": "x.y.z" } } }
{ "method": "initialized", "params": {} }
```

Ingress queues are bounded. When one is full the server rejects the request with retryable JSON-RPC error `-32001` on stdio, WebSocket, and Unix-socket connections (`enqueue_incoming_message`, `transport/mod.rs:205` to `:250`); the docs describe this for WebSocket mode only.

### Read methods

These are the read-only methods whose shapes RimZ depends on, from the 0.154.0 generated schema. None of them subscribes to a thread; `thread/resume` and `turn/start` join and own a thread.

**`thread/loaded/list`** returns the thread ids the app-server holds in memory. Params are `{ cursor?, limit? }`.

```jsonc
// v2/ThreadLoadedListResponse
{ "data": ["thread-id", …], "nextCursor": "string | null" }
```

A thread stays loaded until it has had no subscriber for `thread_unload_delay_secs`, default 60, where 0 unloads at once (`config_toml.rs:322`, `core/src/config/mod.rs:3829`; configurable since 0.154.0, a fixed 30 minutes before). An idle daemon's list is therefore a loaded-in-memory set, not a set of attached terminals, and a freshly spawned app-server returns an empty list.

**`account/rateLimits/read`** returns included-usage windows, plan, credits, and reset credits for a ChatGPT login. Fields are camelCase.

```jsonc
// v2/GetAccountRateLimitsResponse
{
  "rateLimits": {                         // required; the single-bucket view
    "limitId": "codex",                   // string | null
    "limitName": null,                    // string | null
    "primary":   { "usedPercent": 19, "windowDurationMins": 300,   "resetsAt": 1789290347 }, // or null
    "secondary": { "usedPercent": 4,  "windowDurationMins": 10080, "resetsAt": 1789805400 }, // or null
    "planType": "plus",                   // PlanType | null
    "credits": { "hasCredits": true, "unlimited": false, "balance": "12.50" },           // or null
    "rateLimitReachedType": null,         // RateLimitReachedType | null
    "individualLimit": null,              // { limit, used, remainingPercent, resetsAt } | null
    "spendControlReached": null,          // boolean | null
    "normalModelSlug": null               // string | null
  },
  "rateLimitsByLimitId": { "codex": { … } },  // object | null; RateLimitSnapshot per metered limit id
  "rateLimitResetCredits": {                  // object | null
    "availableCount": 2,
    "credits": [                              // array | null; null means only the count is known
      { "id": "opaque", "status": "available", "resetType": "codexRateLimits",
        "grantedAt": 1789000000, "expiresAt": 1789600000, "title": null, "description": null }
    ]
  },
  "ordinaryUsageAllowed": true,  // boolean | null; null means unavailable
  "accountId": "string | null",
  "rateLimitUpsell": {}          // optional backend banner; nested keys stay snake_case
}
```

| Type | Values |
| --- | --- |
| `RateLimitWindow` | `usedPercent` (int, required), `windowDurationMins` (int or null), `resetsAt` (epoch seconds or null) |
| `CreditsSnapshot` | `hasCredits` (required), `unlimited` (required), `balance` (string or null) |
| `PlanType` | `free`, `go`, `plus`, `pro`, `prolite`, `team`, `self_serve_business_prolite`, `self_serve_business_usage_based`, `business`, `ent26`, `enterprise_cbp_automation`, `enterprise_cbp_usage_based`, `enterprise`, `edu`, `edu_plus`, `edu_pro`, `unknown` |
| `RateLimitReachedType` | `rate_limit_reached`, `workspace_owner_credits_depleted`, `workspace_member_credits_depleted`, `workspace_owner_usage_limit_reached`, `workspace_member_usage_limit_reached` |
| `RateLimitResetCredit.status` | `available`, `redeeming`, `redeemed`, `unknown` |
| `RateLimitResetCredit.resetType` | `codexRateLimits`, `unknown` |

`CreditsSnapshot` has no `overageLimitReached` field at 0.150.1 or 0.154.0 (no `overage` string exists under `codex-rs` at either tag), and the 0.154.0 response has no root-level `credits` object. `accountId`, `ordinaryUsageAllowed`, `rateLimitUpsell`, and `normalModelSlug` are new since 0.150.1. `account/rateLimitResetCredit/consume` takes `{ idempotencyKey, creditId? }` and returns `{ outcome }` with `reset`, `nothingToReset`, `noCredit`, or `alreadyRedeemed`.

**`model/list`** with `{ "includeHidden": true }` returns the model catalog in `data[]`. Each `Model` carries `id`, `model`, `displayName`, `description`, `hidden`, `isDefault`, `defaultReasoningEffort`, `supportedReasoningEfforts`, service tiers, input modalities, and upgrade metadata. `defaultReasoningEffort` is the catalog default, not a session's live effort.

**`thread/read`** with `{ "threadId": "<id>", "includeTurns": false }` returns `{ "thread": Thread }`. **`thread/list`** returns `{ data: Thread[], nextCursor, backwardsCursor }` and filters on `archived`, `cwd`, `searchTerm`, `sourceKinds`, `sortKey`, and more. A `Thread` always carries `id`, `sessionId`, `preview`, `cwd`, `cliVersion`, `createdAt`, `updatedAt`, `ephemeral`, `modelProvider`, `projectId`, `source`, `status`, and `turns`, and optionally `name`, `model`, `reasoningEffort`, `forkedFromId`, `parentThreadId`, `agentNickname`, `agentRole`, `threadSource`, `historyMode`, `path`, and `gitInfo`. `name` is the thread title ([session index](#session-index)); `preview` is derived from the first user message. `includeTurns: true` is deprecated for paginated threads in favour of `thread/turns/list` and `thread/items/list`.

```jsonc
{ "thread": { "id": "01a09a03-fd61-7eb3-9f97-0b24ec37cde7", "sessionId": "01a09a03-fd61-7eb3-9f97-0b24ec37cde7",
              "preview": "Trace the isolation override", "name": "Trace isolation override flow", "updatedAt": 1789290354, … } }
```

Token and context-window usage has no read method. It arrives only as the `thread/tokenUsage/updated` notification to a subscribed client; `account/usage/read` is an account-level activity summary, not per-thread context.

### Method index

The 0.154.0 stable schema, grouped. `--experimental` adds the methods listed last.

| Group | Methods |
| --- | --- |
| Thread | `thread/start`, `thread/resume`, `thread/fork`, `thread/read`, `thread/list`, `thread/loaded/list`, `thread/unsubscribe`, `thread/archive`, `thread/unarchive`, `thread/delete`, `thread/name/set`, `thread/metadata/update`, `thread/goal/{set,get,clear}`, `thread/compact/start`, `thread/revert`, `thread/rollback` (deprecated; removed on `main` by #44915, unreleased), `thread/inject_items`, `thread/shellCommand`, `thread/turns/list`, `thread/items/list` (both stable since 0.154.0), `thread/approveGuardianDeniedAction`, `thread/section/move`, `threadSection/{create,list,update,delete}` |
| Turn and review | `turn/start`, `turn/steer`, `turn/interrupt`, `review/start` |
| Account | `account/read`, `account/login/{start,cancel}`, `account/logout`, `account/rateLimits/read`, `account/rateLimitResetCredit/consume`, `account/usage/read`, `account/workspaceMessages/read`, `account/sendAddCreditsNudgeEmail` |
| Exec and filesystem | `command/exec`, `command/exec/{write,resize,terminate}`, `fs/{readFile,writeFile,createDirectory,getMetadata,readDirectory,remove,copy,watch,unwatch}`, `fuzzyFileSearch` |
| Config, models, features | `config/read`, `config/value/write`, `config/batchWrite`, `config/mcpServer/reload`, `configRequirements/read`, `model/list`, `modelProvider/capabilities/read`, `permissionProfile/list`, `experimentalFeature/list`, `experimentalFeature/enablement/set` |
| Skills, hooks, plugins, apps, MCP | `skills/list`, `skills/config/write`, `skills/extraRoots/set`, `hooks/list`, `plugin/{list,read,install,installed,uninstall,reconcile}`, `plugin/skill/read`, `plugin/share/{list,save,delete,checkout,updateTargets}`, `marketplace/{add,remove,upgrade}`, `app/{list,read,installed}`, `mcpServerStatus/list`, `mcpServer/oauth/login`, `mcpServer/resource/read`, `mcpServer/tool/call` |
| Other | `externalAgentConfig/{detect,import}`, `externalAgentConfig/import/{readHistories,recordHistory}`, `feedback/upload`, `windowsSandbox/{readiness,setupStart}` |
| Experimental only | `process/{spawn,writeStdin,resizePty,kill}`, `thread/queue/*`, `thread/realtime/*`, `thread/search`, `thread/timeline/list`, `thread/settings/update`, `turn/settings/update`, `thread/backgroundTerminals/*`, `thread/memoryMode/set`, `remoteControl/*`, `project/*`, `environment/*`, `collaborationMode/list`, `userVerification/*`, `account/bedrock/{discover,setup}`, `server/diagnostics`, `memory/reset` |

Server requests to the client: `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, `item/tool/requestUserInput`, `item/tool/call`, `mcpServer/elicitation/request`, `account/chatgptAuthTokens/refresh`, `attestation/generate`, and the legacy `execCommandApproval` and `applyPatchApproval`.

Notifications include `thread/{started,status/changed,tokenUsage/updated,name/updated,compacted,archived,unarchived,deleted,closed,goal/updated,goal/cleared,settings/updated,queue/changed}`, `turn/{started,completed,diff/updated,plan/updated}`, `item/{started,completed}` with the `item/agentMessage/delta`, `item/reasoning/*`, `item/commandExecution/outputDelta`, and `item/fileChange/*` streams, `hook/{started,completed}`, `account/{updated,rateLimits/updated,login/completed}`, `model/rerouted`, `remoteControl/status/changed`, `error`, and `warning`. The generated `ServerNotification.json` is the full list.

`ThreadItem` types: `userMessage`, `hookPrompt`, `agentMessage`, `functionCallOutput`, `plan`, `reasoning`, `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `collabAgentToolCall`, `subAgentActivity`, `webSearch`, `imageView`, `sleep`, `imageGeneration`, `enteredReviewMode`, `exitedReviewMode`, `contextCompaction`.

`TurnError.codexErrorInfo` is one of `contextWindowExceeded`, `sessionBudgetExceeded`, `usageLimitExceeded`, `rateLimitExceeded`, `serverOverloaded`, `cyberPolicy`, `misalignmentPolicyViolation`, `internalServerError`, `unauthorized`, `badRequest`, `threadRollbackFailed`, `sandboxError`, `other`, or an object variant `httpConnectionFailed`, `responseStreamConnectionFailed`, or `responseStreamDisconnected`, each with an optional `httpStatusCode`. The [rollout spelling](#rollout-records) is snake case.

### App-server daemon

`codex remote-control start` enables remote control and starts the persistent per-user app-server; `codex remote-control stop` stops it, and `pair` prints a pairing code. `codex app-server daemon` exposes `start`, `restart`, `stop`, `bootstrap`, `enable-remote-control`, `disable-remote-control`, and `version`. Windows uses the same backend and file names since 0.154.0.

| Item | Upstream |
| --- | --- |
| Managed binary | `$CODEX_HOME/packages/standalone/current/bin/codex`, falling back to the legacy `current/codex` (`managed_install.rs:19`; the `bin/` layout is from #42318). The 0.154.0 installer keeps `current/codex` as a symlink to `bin/codex`. |
| PID records | `$CODEX_HOME/app-server-daemon/app-server.pid` and `app-server-updater.pid`, each `{ "pid", "processStartTime" }` (`lib.rs:35`, `backend/pid.rs:38`). `processStartTime` is the `lstart` value from `ps -p PID -o stat= -o lstart=` and guards against pid reuse (`pid.rs:645`). |
| Other state | `daemon.lock` (lifecycle lock), `settings.json`, and per-process `.stderr.log` files in the same directory. |
| App-server argv | `codex app-server --remote-control --listen unix://`, or `codex app-server --listen unix://` without remote control (`pid.rs:340` to `:344`). |
| Updater argv | `codex app-server daemon pid-update-loop` |
| Shutdown | A request to `ws://localhost/daemon/shutdown` over the control socket (#43308). |

The updater waits five minutes before its first pass and one hour between passes (`update_loop.rs:34`, `:36`). Each pass runs the standalone installer and compares its own executable with the managed binary; when they differ it restarts a running app-server from the managed binary and then replaces its own process image with it.

`stop` stops only the app-server (`lib.rs:435`), and `start` never touches the updater (`lib.rs:313`), so a stop and start pair leaves an updater running an old binary. `codex app-server daemon bootstrap --remote-control` takes `daemon.lock` (`lib.rs:516`), restarts the app-server, stops any live updater, and starts a new one from the managed binary (`bootstrap_locked`, `lib.rs:616` to `:641`).

A zombie app-server counts as inactive and is reaped with a non-blocking `waitpid` (`pid.rs:183`, `:508` to `:516`; #43504). Up to 0.144.4 the PID backend treated a recorded process as active when `kill(pid, 0)` succeeded and its start time matched, so a zombie child of the updater looked alive: `start` waited ten seconds for an absent control socket, and `stop` signalled the zombie through a 60-second grace period and failed at its 70-second timeout. RimZ's recovery for those releases is in [adapter_codex.md → Remote control](../../internals/agents/adapter_codex.md#remote-control).

## Session files

### Rollout files

Codex writes one rollout per thread at `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<thread_id>[_<rollout_id>].jsonl` (`precompute_new_rollout_path`, `rollout/src/recorder.rs:1635`; `rollout_file_name.rs:62`). The optional rollout id appears when a reverted thread keeps its thread id but starts a new file; the newest timestamp and UUIDv7 rollout id win. Archiving moves a rollout into the flat `$CODEX_HOME/archived_sessions/` (`recorder.rs:1457`). Readers accept `.jsonl` and zstd-compressed `.jsonl.zst` (`compression.rs:41`); writing compressed files is behind the `local_thread_store_compression` feature, which is under development and off by default (`features/src/lib.rs:1112`).

Each line is `{ "timestamp", "ordinal", "type", "payload" }`, where `type` is `session_meta`, `turn_context`, `event_msg`, `response_item`, `compacted`, or one of the newer item kinds. Which `event_msg` records persist depends on the thread's history mode (`should_persist_event_msg`, `rollout/src/policy.rs`):

| Record | Legacy | Paginated |
| --- | --- | --- |
| `item_completed` with a turn item | `Plan`, `Sleep`, `FunctionCallOutput`, and completed `SubAgentActivity` items only | every turn item, including `UserMessage` and `AgentMessage` |
| `user_message`, `agent_message`, `agent_reasoning`, review, patch, MCP, web-search, image, and context-compacted end events | persisted | omitted |
| `token_count`, `task_started`, `task_complete`, `turn_aborted`, `thread_goal_updated`, `thread_rolled_back`, thread settings | persisted | persisted |
| `error`, `stream_error`, `warning`, exec begin and delta events, approval requests | never persisted | never persisted |
| `response_item` | persisted | persisted |

Non-ephemeral TUI sessions use Paginated history (`tui/src/app_server_session.rs:2038`), and the header records it as `history_mode`. `codex migrate-rollouts` converts legacy sessions. `response_item` rows are model transport, including developer and system messages, and are not a second copy of the visible transcript.

### Rollout records

The header is the first line. `SessionMeta` (`protocol.rs:3040`) carries `id`, `session_id`, `timestamp`, `cwd`, `originator`, `cli_version`, `source`, `model_provider`, `history_mode`, `context_window`, git metadata, and these lineage fields:

| Field | Meaning |
| --- | --- |
| `forked_from_id` | Source thread of a user fork (`/fork`, `codex fork`; an ephemeral `/side` or `/btw` thread writes no rollout); absent on a fresh root such as `/new` or `/clear`. |
| `forked_from_ordinal_exclusive` | Ordinal in the source where the copy stops; new since 0.150.1. |
| `thread_source` | `user`, `subagent`, `guardian_review`, `memory_consolidation`, or a feature string (`protocol.rs:2760`). `subagent` identifies a child; `forked_from_id` alone does not. |
| `parent_thread_id` | Immediate parent of a child. |
| `agent_nickname`, `agent_path`, `agent_role` | Child display name, root-relative task path such as `/root/research/explore_hooks`, and role; `agent_role` also accepts `agent_type`. |
| `multi_agent_version` | `disabled`, `v1`, or `v2`. |
| `source.subagent.thread_spawn` | Older structured child source `{ parent_thread_id, depth, agent_path, agent_nickname, agent_role }` (`protocol.rs:2828`), still found in copied headers. |

```jsonc
{ "type": "session_meta", "payload": {
    "id": "<thread_id>", "cwd": "/repo", "cli_version": "0.154.0", "history_mode": "paginated",
    "thread_source": "subagent", "parent_thread_id": "<parent>", "forked_from_id": "<fork_source>",
    "agent_nickname": "Atlas", "agent_path": "/root/research/explore_hooks",
    "agent_role": "explorer", "multi_agent_version": "v2" } }
```

A fork or child rollout begins with a copy of the parent's history, including its `token_count` records, before its own work.

Turn and usage records, as a 0.154.0 build writes them:

```jsonc
// model and effort for the turn; collaboration_mode.mode is "plan" or "default"
{ "type": "turn_context", "payload": { "model": "gpt-5.5-codex", "effort": "xhigh", "collaboration_mode": { "mode": "default" }, … } }

{ "type": "event_msg", "payload": { "type": "task_started", "turn_id": "01a09a03-fdd7-…", "started_at": 1789290347,
    "model_context_window": 372400, "collaboration_mode_kind": "default" } }

{ "type": "event_msg", "payload": { "type": "token_count",
    "info": { "model_context_window": 372400,
              "last_token_usage":  { "input_tokens": 10152, "cached_input_tokens": 0, "cache_write_input_tokens": 0,
                                     "output_tokens": 481, "reasoning_output_tokens": 41, "total_tokens": 10633 },
              "total_token_usage": { … } },
    "rate_limits": { "limit_id": "codex", "primary": { "used_percent": 19.0, "window_minutes": 10080, "resets_at": … }, … } } }
```

`TokenUsage` (`protocol.rs:2216`): `input_tokens` includes both cache slices, `output_tokens` includes `reasoning_output_tokens`, and `cache_write_input_tokens` defaults to 0 when absent (older rollouts omit it). `last_token_usage` is the most recent request; `total_token_usage` is cumulative for the rollout, including any copied parent prefix.

Visible messages in Paginated mode:

```jsonc
// user message; content is UserInput, snake-tagged: text, image, local_image, audio, local_audio, skill, mention
{ "type": "event_msg", "payload": { "type": "item_completed", "turn_id": "turn-1",
    "item": { "type": "UserMessage", "content": [ { "type": "text", "text": "prompt", "text_elements": [] } ] } } }

// assistant message; the content tag is capitalized; the item may also carry phase, memory_citation, delivery, questions
{ "type": "event_msg", "payload": { "type": "item_completed", "turn_id": "turn-1",
    "item": { "type": "AgentMessage", "content": [ { "type": "Text", "text": "assistant update" } ] } } }
```

Legacy mode writes the same text as `{ "type": "user_message", "message": … }` and `{ "type": "agent_message", "message": … }` event payloads.

Turn endings (`TurnCompleteEvent`, `protocol.rs:2141`, serialized as `task_complete` with alias `turn_complete`; `TurnAbortedEvent`, `protocol.rs:4154`). Timestamps inside the payload are epoch seconds:

```jsonc
// clean completion
{ "type": "event_msg", "payload": { "type": "task_complete", "turn_id": "…", "last_agent_message": "patch is correct",
    "started_at": 1789290347, "completed_at": 1789290628, "duration_ms": 280548 } }

// failed turn; the only persisted error record
{ "type": "event_msg", "payload": { "type": "task_complete", "turn_id": "…", "last_agent_message": null,
    "error": { "message": "This content was flagged for possible cybersecurity risk. …", "codex_error_info": "cyber_policy" } } }

// interrupted turn
{ "type": "event_msg", "payload": { "type": "turn_aborted", "turn_id": "…", "reason": "interrupted",
    "started_at": 1789043828, "completed_at": 1789043832, "duration_ms": 4223 } }
```

| Field | Values |
| --- | --- |
| `turn_aborted.reason` | `interrupted`, `replaced`, `review_ended`, `budget_limited`; Esc and `/clear` during a turn were observed writing `interrupted` on 0.144.x |
| `task_complete.error` | `ErrorEvent { message, codex_error_info }` (`protocol.rs:2063`) |
| `codex_error_info` | snake case (`protocol.rs:1851`): `context_window_exceeded`, `session_budget_exceeded`, `usage_limit_exceeded`, `rate_limit_exceeded`, `server_overloaded`, `cyber_policy`, `misalignment_policy_violation`, `http_connection_failed`, `response_stream_connection_failed`, `internal_server_error`, `unauthorized`, `bad_request`, `sandbox_error`, `response_stream_disconnected`, `response_too_many_failed_attempts`, `active_turn_not_steerable`, `thread_rollback_failed`, `other` |

`other` is a catch-all and can accompany a transient failure such as an HTTP 503. There is no `turn_error` record, and `error` and `stream_error` events are never persisted.

Some provider failures leave no error in the rollout. Observed on 0.142.x, a serving-capacity failure (TUI: `⚠ Selected model is at capacity. Please try a different model.`) and a usage-limit failure (TUI: `■ You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage … try again at 6:35 AM.`) both end in a `task_complete` with `last_agent_message: null` and no `error`, with no `Stop` hook and no error on the app-server thread; see openai/codex #22277, #19579, #28507, and #29760. This has not been re-observed on 0.154.0.

### Session index

`$CODEX_HOME/session_index.jsonl` maps a thread id to its title (`rollout/src/session_index.rs:25`). The TUI generates a title of at most 36 characters after the first turn (`THREAD_TITLE_MAX_CHARS`, `tui/src/app/thread_title.rs:25`) and appends one row with it (`append_thread_name`, `session_index.rs:33`). Renames append further rows, and the newest valid row for an `id` wins; deleting a thread rewrites the file without its rows (`remove_thread_name_entries`, `session_index.rs:74`). Since 0.154.0 (#42749) the TUI writes only the generated title; earlier builds first appended a provisional row made from the whitespace-normalized first prompt.

```json
{ "id": "01a09a03-fd61-7eb3-9f97-0b24ec37cde7", "thread_name": "Trace isolation override flow", "updated_at": "2026-09-13T09:05:54.102581041Z" }
```

The same title is the app-server `Thread.name`.

### Questions and plan approval

A Plan-mode turn records `turn_context.payload.collaboration_mode.mode = "plan"` and `task_started.payload.collaboration_mode_kind = "plan"`, and its hooks report `permission_mode = "plan"`. The plan itself is a completed `Plan` item followed by a clean completion of the same turn:

```jsonc
{ "type": "event_msg", "payload": { "type": "item_completed", "turn_id": "turn-1",
    "item": { "type": "Plan", "id": "turn-1-plan", "text": "# Plan\n\n..." } } }
{ "type": "event_msg", "payload": { "type": "task_complete", "turn_id": "turn-1", "last_agent_message": "Codex says:" } }
```

The model's response wraps the plan in `<proposed_plan>…</proposed_plan>`, which `last_agent_message` excludes. After the turn the TUI asks "Implement this plan?" with three rows (`tui/src/chatwidget/plan_implementation.rs:9` to `:13`): "Yes, implement this plan" switches to Default mode and submits `Implement the plan.`; "Yes, clear context and implement" starts a fresh thread seeded with the plan; "No, stay in Plan mode" continues planning. The prompt is client-side and emits no hook or `notify` event ([openai/codex#19921](https://github.com/openai/codex/issues/19921)), so the ordinary `Stop` hook and the rollout are the only signals.

Plan clarifications and default-mode questionnaires use the blocking `request_user_input` tool:

```jsonc
// PreToolUse.tool_input
{ "questions": [
  { "id": "path", "header": "Migration", "question": "Pick a path?",
    "options": [
      { "label": "Blue", "description": "Safer rollout" },
      { "label": "Green", "description": "Faster rollout" }
    ] }
] }

// PostToolUse.tool_response: a JSON string
"{\"answers\":{\"path\":{\"answers\":[\"Blue\"]}}}"
```

The questionnaire (`tui/src/bottom_pane/request_user_input/mod.rs`) opens with the first option of each question selected. Down moves the selection, and the submit key (Enter by default, remappable in the keymap) commits the answer, advances to the next question, and submits after the last. Each question also accepts notes and free-form text.

`request_user_input_async` (0.153.0) is the non-blocking variant: its input is `{ "questions": [ { "title", "options"?: [string] } ] }`, it returns `{"accepted":true}` at once, and the questions appear as an `AgentMessage` item with `delivery: "async"` and `questions` that the user answers inline while the turn continues (`core/src/tools/handlers/request_user_input_async.rs`).

## Auth and usage endpoints

### `auth.json`

`cli_auth_credentials_store` selects where Codex keeps credentials: `file` (default), `keyring`, `auto`, or `ephemeral`, which keeps them in memory for the current process (`config/src/types.rs:109`). With `file`, credentials are in `$CODEX_HOME/auth.json` (`login/src/auth/storage.rs:41`):

| Key | Meaning |
| --- | --- |
| `OPENAI_API_KEY` | API-key login when non-empty. |
| `tokens.access_token` | ChatGPT OAuth access token; the bearer token for the usage endpoints below. |
| `tokens.account_id` | ChatGPT account id, sent as the `ChatGPT-Account-Id` header. |

With keyring or ephemeral storage there is no file; `codex login status` still reports the login kind.

### Responses rate-limit headers

Codex also reads rate-limit state from response headers on model requests (`codex-api/src/rate_limits.rs`). For a limit id `L` (default `codex`, underscores written as hyphens) the headers are:

| Header | Value |
| --- | --- |
| `x-L-primary-used-percent`, `x-L-secondary-used-percent` | window usage, float |
| `x-L-primary-window-minutes`, `x-L-secondary-window-minutes` | window length in minutes |
| `x-L-primary-reset-at`, `x-L-secondary-reset-at` | reset time, epoch seconds |
| `x-L-limit-name` | display name of a non-default limit |
| `x-codex-credits-has-credits`, `x-codex-credits-unlimited`, `x-codex-credits-balance` | credit state |
| `x-codex-rate-limit-reached-type` | a `RateLimitReachedType` value |
| `x-codex-promo-message` | backend promotional text |

Every `x-*-primary-used-percent` header names one limit id, so `parse_all_rate_limits` returns one snapshot per limit present. The same state reaches rollouts as `token_count.rate_limits` and app-server clients as `account/rateLimits/updated`. The public docs do not describe these headers.

### ChatGPT usage endpoints

Codex's backend client reads account usage over HTTPS with the OAuth bearer token and, when known, the `ChatGPT-Account-Id` header. The base URL is `chatgpt_base_url` from config, default `https://chatgpt.com/backend-api/` (`core/src/config/mod.rs:4283`), with trailing slashes trimmed. A `https://chatgpt.com` or `https://chat.openai.com` base without `/backend-api` gets it appended, and the path style follows from whether the base contains `/backend-api` (`PathStyle::from_base_url`, `backend-client/src/client.rs:126`, `:184` to `:196`):

| Call | Base contains `/backend-api` | Other bases |
| --- | --- | --- |
| Usage, `GET` | `/wham/usage` | `/api/codex/usage` |
| Reset credits, `GET` | `/wham/rate-limit-reset-credits` | `/api/codex/rate-limit-reset-credits` |
| Consume a reset credit, `POST` | `/wham/rate-limit-reset-credits/consume` | `/api/codex/rate-limit-reset-credits/consume` |

The usage response is snake case (`RateLimitStatusPayload` and `RateLimitStatusWithResetCredits`):

```jsonc
{
  "plan_type": "plus",                    // required; guest, free, go, plus, pro, prolite, free_workspace, team, business, education, edu, enterprise, k12, quorum, … unknown
  "rate_limit": {                         // optional
    "allowed": true,
    "limit_reached": false,
    "primary_window":   { "used_percent": 19, "limit_window_seconds": 18000,  "reset_after_seconds": 123, "reset_at": 1789290347 },
    "secondary_window": { "used_percent": 4,  "limit_window_seconds": 604800, "reset_after_seconds": 456, "reset_at": 1789805400 }
  },
  "credits": {                            // optional
    "has_credits": true, "unlimited": false, "balance": "12.50",
    "approx_local_messages": null, "approx_cloud_messages": null
  },
  "spend_control": { "reached": false, "individual_limit": { "limit": "…", "used": "…", "remaining_percent": 80, "reset_at": 1789805400 } }, // optional
  "additional_rate_limits": [ { "metered_feature": "…", "limit_name": "…", "rate_limit": { … }, "normal_model_slug": "…" } ],      // optional
  "rate_limit_reached_type": { "type": "rate_limit_reached" }, // optional
  "rate_limit_reset_credits": { "available_count": 2 },        // optional
  "account_id": "…",                                           // optional
  "user_id": "…",                                              // optional
  "rate_limit_upsell": {}                                      // optional
}
```

Windows carry `limit_window_seconds`; primary and secondary order carries no meaning beyond position. Codex converts this payload into the app-server `account/rateLimits/read` shape: the main bucket gets `limitId: "codex"`, each additional limit becomes a further bucket, and `rate_limit.allowed` becomes `ordinaryUsageAllowed`.

The reset-credit list is `{ "available_count", "credits": [ { "id", "reset_type", "status", "granted_at", "expires_at"?, "title"?, "description"? } ] }`, with the timestamps as strings (`RateLimitResetCreditDetails`, `types.rs:34`). The consume request is `{ "redeem_request_id": "<id>", "credit_id": "<optional id>" }`, and the response is `{ "code", "windows_reset" }` with `code` one of `reset`, `nothing_to_reset`, `no_credit`, or `already_redeemed` (`types.rs:106` to `:116`). `nothing_to_reset` leaves the credit available.

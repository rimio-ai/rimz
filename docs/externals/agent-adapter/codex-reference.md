# Codex protocol reference

This page records Codex CLI's hooks, launch behavior, app-server API, session files, folder trust, TUI input, authentication, and usage endpoints. The RimZ mapping is in [adapter_codex.md](../../internals/agents/adapter_codex.md). The provider-neutral model is [model.md](../../internals/agents/model.md), and accounts and spend are [providers.md](../../internals/agents/providers.md).

## Baseline and sources

This page targets Codex CLI **0.163.0** (tag `rust-v0.163.0`, commit `83086188ae0614bd882818ff1aa23f3194962012`, dated 2026-10-15). Permission profiles, Guardian review, and execution-policy loading were checked in source on 2026-10-18; the remaining protocol facts carry forward the 0.162.1 baseline checked 2026-10-12. Source paths are relative to [`codex-rs/` at that tag](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs).

App-server shapes come from the release's [precomputed schema exports](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs/app-server-protocol/schema/precomputed), which its schema commands emit. The linked TypeScript files match the corresponding stable exports. Live captures retain their original version labels. The [async-question observations](#async-questions-observed-on-01580) remain scoped to 0.158.0.

The table lists main sources. Additional links appear beside the corresponding wire details. Historical notes describe compatibility and observations, without setting an adapter support floor.

| Surface | Source |
| --- | --- |
| Hooks: events, payloads, output, trust, limits | <https://learn.chatgpt.com/docs/hooks>. [`hooks/src/schema.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/hooks/src/schema.rs), [`hooks/src/engine/`](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs/hooks/src/engine), generated schemas in [`hooks/schema/generated/`](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs/hooks/schema/generated) |
| Hook event enum and runtime dispatch | [`protocol/src/protocol.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/protocol.rs) `HookEventName`, [`core/src/hook_runtime.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/hook_runtime.rs) |
| Hook tool names | [`core/src/tools/hook_names.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/tools/hook_names.rs) |
| Project directory trust | [`tui/src/lib.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/lib.rs), [`config/src/loader/mod.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/config/src/loader/mod.rs), [`git-utils/src/trust.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/git-utils/src/trust.rs) |
| Config keys (`notify`, `[tui]`, credential store, `projects`, compaction) | <https://learn.chatgpt.com/docs/config-file/config-reference>, <https://learn.chatgpt.com/docs/config-file/config-advanced>. [`config/src/config_toml.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/config/src/config_toml.rs), [`config/src/types.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/config/src/types.rs) |
| CLI commands and `-c` parser | <https://learn.chatgpt.com/docs/developer-commands?surface=cli>. `codex <command> --help`. [`utils/cli/src/config_override.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/utils/cli/src/config_override.rs), [`cli/src/login.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/cli/src/login.rs) |
| Auto-compaction ceiling | [`protocol/src/openai_models.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/openai_models.rs), [`protocol/src/config_types.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/config_types.rs) |
| TUI paste burst, plan prompt, questionnaire | [`tui/src/bottom_pane/paste_burst.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/bottom_pane/paste_burst.rs), [`tui/src/chatwidget/plan_implementation.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/chatwidget/plan_implementation.rs), [`tui/src/bottom_pane/request_user_input/mod.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/bottom_pane/request_user_input/mod.rs) |
| App-server protocol | <https://learn.chatgpt.com/docs/app-server>. [`app-server/README.md`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server/README.md), [`app-server-protocol/src/protocol/`](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs/app-server-protocol/src/protocol) |
| App-server daemon and updater | [`app-server-daemon/README.md`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/README.md), [`app-server-daemon/src/`](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs/app-server-daemon/src) (`lib.rs`, `backend/pid.rs`, `update_loop.rs`, `managed_install.rs`) |
| Rollout files, persistence policy, compression, session index | [`rollout/src/`](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs/rollout/src) (`recorder.rs`, `policy.rs`, `rollout_file_name.rs`, `compression.rs`, `session_index.rs`), [`protocol/src/protocol.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/protocol.rs), [`protocol/src/items.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/items.rs) |
| `auth.json` | [`login/src/auth/storage.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/login/src/auth/storage.rs) |
| ChatGPT usage and reset-credit endpoints | [`backend-client/src/client/rate_limit_resets.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/backend-client/src/client/rate_limit_resets.rs), [`backend-client/src/types.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/backend-client/src/types.rs), [`codex-backend-openapi-models/src/models/`](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs/codex-backend-openapi-models/src/models) |
| Release notes | <https://github.com/openai/codex/releases> |

The app-server protocol publishes no version string. The binary generates its exact schema. `--experimental` adds the methods and fields gated as experimental:

```bash
codex app-server generate-ts --out DIR
codex app-server generate-json-schema --out DIR [--experimental]
```

## Hooks

A Codex hook is a handler Codex runs at a lifecycle point. A command handler receives a JSON payload on stdin and answers on stdout. The event set, payloads, and output rules follow Claude Code's hook shape with the differences recorded below. Never reuse a Claude decision without checking this section.

### Config shape

Hooks are declared as `[[hooks.<Event>]]` groups in a `config.toml`, or in a `hooks.json` beside it. Codex discovers both beside the user config (`$CODEX_HOME/config.toml`, default `~/.codex/config.toml`), beside each trusted project's `.codex/` layer, and in plugin and managed layers. Sources merge. A layer holding both `hooks.json` and TOML hooks gets a startup warning. Hooks are enabled by default: `[features].hooks = false` disables them, and `codex_hooks` is a deprecated alias for that feature (`features/src/lib.rs`, `features/src/legacy.rs`).

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
| `matcher` (group) | Regex over the tool name, or over the source or trigger for lifecycle events. `UserPromptSubmit`, `Stop`, and `Interrupt` ignore it (`hooks/src/events/common.rs`). |
| `type` | `command` and `mcp_tool` run. `prompt` and `agent` parse and are skipped with a warning (`hooks/src/engine/discovery.rs`). `mcp_tool` is unsupported on `SessionEnd`. Handler fields are in `HookHandlerConfig` (`config/src/hook_config.rs`). |
| `command`, `commandWindows` | Shell command. The Windows variant (alias `command_windows`) replaces it on Windows. |
| `server`, `tool`, `input` | The MCP server, tool, and TOML-representable input of an `mcp_tool` handler. |
| `timeout` | Seconds. Default 600. `SessionEnd` and `Interrupt` default to 1 and clamp to 1 through 3 (`discovery.rs`). |
| `async` | A command with `async = true` runs in the background and cannot apply control effects. At most eight background hooks run per session. The rest queue (`MAX_CONCURRENT_ASYNC_HOOKS`, `hooks/src/engine/command_runner.rs`). `async` on `SessionEnd` runs synchronously with a warning. |
| `statusMessage` | Text the TUI shows while the hook runs. |
| `additionalContextLimit` | Approximate token threshold for spilling a command hook's `additionalContext` to disk. Unset means 2,500 and `0` disables spilling. Overflow is saved under `<temp_dir>/hook_outputs/<session_id>/` and the model sees a head and tail preview ([hooks docs, "Large hook output"](https://learn.chatgpt.com/docs/hooks)). |

Managed hooks come from `requirements.toml` `[hooks]` (`managed_dir`, `windows_managed_dir`). `allow_managed_hooks_only` makes Codex ignore every other source.

### Execution

Codex runs ordinary synchronous handlers concurrently and schedules asynchronous handlers separately (`hooks/src/engine/dispatcher.rs`). Executor-scoped plugin hooks run after synchronous results, subject to their control outcome. Tool hooks use the local environment cwd, with the turn cwd as fallback. They run on the host even with a remote workspace (`core/src/hook_runtime.rs`, `tool_hook_cwd`). `build_command` (`command_runner.rs`) clears the child environment, replays the environment snapshot `Hooks::new` captured from the Codex process (`hooks/src/registry.rs`), applies the source's environment overlay, and then scrubs non-inheritable credential variables. Only plugin hooks have an overlay (`PLUGIN_ROOT`, `CLAUDE_PLUGIN_ROOT`, `PLUGIN_DATA`, `CLAUDE_PLUGIN_DATA`). A handler has no `env` field.

An eligible TUI launch automatically starts or attaches to the shared [app-server daemon](#app-server-daemon) for its `CODEX_HOME`. Hook children inherit the environment captured when the daemon started. A new terminal does not replace it. `--no-daemon`, introduced in 0.156.0, keeps the session embedded and cannot combine with `--remote` ([startup checks](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/startup_orchestration.rs)).

[Daemon exclusions](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/daemon_startup.rs) include `--oss`, workload identity, `CODEX_EXEC_SERVER_URL`, `--profile`, `--strict-config`, hook-trust bypass, and unsupported configuration overrides. Overrides outside the allowlist select embedded execution. The allowlist accepts boolean `suppress_unstable_features_warning`, `tui.fullscreen_transcript`, and selected boolean `features.*` keys. The complete feature list is `allowed_feature` in that source. `--no-daemon` provides deterministic embedded execution. Automatic startup also excludes Windows-mounted WSL homes. In 0.162.1, compatibility checks compare the daemon's shared services only against explicit CLI feature overrides, rather than client defaults.

For a normal TUI session, server selection follows these rules ([startup orchestration](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/startup_orchestration.rs)):

| Condition | Result |
| --- | --- |
| Explicit `--remote` endpoint | Connect to that app-server. Connection failure is an error. `--no-daemon` conflicts, and workload identity must be configured on the remote host. |
| Local daemon exclusion | Use embedded execution. Other exclusions include a custom configuration loader and required Bedrock sign-in. Elevated Windows sessions also run embedded. |
| Eligible local session, `features.daemon_auto_start = true` (default) | Start or reuse the managed daemon. Startup or connection failure exits with the `--no-daemon` recovery hint. A Windows launcher that forbids detached processes falls back to embedded. |
| Automatic startup disabled | Attach to an eligible daemon already discovered at the default socket, otherwise run embedded. This optional attachment can fall back if connection or compatibility checks fail. |

Automatic startup requires daemon compatibility. If stdin or stdout is not a terminal, a compatibility failure returns an error. The TUI offers embedded execution or a confirmed managed restart with the requested settings ([recovery choices](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/daemon_recovery.rs)). Restart can interrupt other clients. Codex checks compatibility once more after restart. A failed second check returns an error. Daemon logs are listed under [App-server daemon](#app-server-daemon).

### Trust state

Codex runs a non-managed hook only after the user trusts its current definition. Trust is recorded per handler as a hash, so a new or changed handler is skipped until the user reviews it in `/hooks`. The TUI raises a startup review listing hooks that need it (`tui/src/startup_hooks_review.rs`), and the engine never runs them in the meantime. `--dangerously-bypass-hook-trust` runs enabled hooks without trust for one invocation. Managed hooks, and the bundled cleanup-plugin hooks Codex treats as built-ins, need no trust (`discovery.rs`).

State lives in the user config as `[hooks.state]` entries, `{ enabled, trusted_hash }`, keyed `"<key_source>:<event_label>:<group>:<handler>"` (`hooks/src/lib.rs`, `config/src/hook_config.rs`). For a config-file hook `key_source` is its source path, including `hooks.json` when that file declares it. Group and handler indices are zero-based within that source. For a plugin hook it is `"<plugin_id>:<relative_path>"`. The event label is lower snake case. The hash input is a flattened object with `event_name`, `hooks = [<one normalized handler>]`, and optional `matcher`. Keys are sorted recursively, serialized as compact JSON, then SHA-256 hashed with a `sha256:` prefix ([hash construction](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/hooks/src/engine/discovery.rs), `hook_hash`, and [fingerprinting](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/config/src/fingerprint.rs)):

```toml
[hooks.state."/home/user/.codex/config.toml:permission_request:0:0"]
trusted_hash = "sha256:…"
```

For a command handler, the hashed fields are `type = "command"`, the platform-selected `command`, normalized `timeout`, `async` (default false), and `statusMessage` when present (including an empty string). `commandWindows` selects the command on Windows and is then removed; Unix uses `command`. Missing timeout becomes 600 seconds, except `Interrupt` and `SessionEnd`, which default to 1 and clamp to 1..=3 seconds; other events clamp to at least 1. `additionalContextLimit` is retained only for `PreToolUse`, `PostToolUse`, `SessionStart`, `UserPromptSubmit`, and `SubagentStart`, and omitted at its default of 2500. Unknown keys are ignored. The group matcher is dropped for `UserPromptSubmit`, `Stop`, and `Interrupt`; otherwise an absent matcher is omitted and a present string is preserved verbatim. Neither sibling handlers nor source paths, slot indices, or trust state contribute to the hash (`append_matcher_groups`, `normalize_command_hook`, and `openai/codex@rust-v0.163.0:codex-rs/hooks/src/events/common.rs::matcher_pattern_for_event`).

### Common input

These fields form the hook input envelope. Availability varies by event as noted below (`hooks/src/schema.rs`). `SessionEnd` has its own reduced envelope.

| Field | Type | Notes |
| --- | --- | --- |
| `session_id` | string | The shared session-tree id, equal to the root thread id. A child has its own `agent_id`. |
| `transcript_path` | string or null | Rollout path. |
| `cwd` | string | Session cwd. |
| `hook_event_name` | string | Event name. |
| `model` | string | Active model slug. |
| `permission_mode` | string | Runtime emits `bypassPermissions` for approval policy `never`, otherwise `default`. Absent on `PreCompact` and `PostCompact`. The schema also lists `acceptEdits`, `plan`, and `dontAsk`, which this runtime does not emit (`core/src/hook_runtime.rs`, `hook_permission_mode`). |
| `turn_id` | string | Turn-scoped events only. Absent on `SessionStart`. |
| `agent_id`, `agent_type` | string, optional | Present on `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, and `PostCompact` when the hook fires inside a child thread. The release schema carries these fields. |

A child observation carries an `agent_id` distinct from the root `session_id`. Hooks carry no child nickname, task path, token usage, or assignment prompt. See [rollout records](#rollout-records) for lineage and usage.

### Events

Codex 0.162.1 has twelve hook events (`HookEventName`, `protocol/src/protocol.rs`) and no `Notification` or dedicated plan-approval event.

| Event | Fires | Event-specific input |
| --- | --- | --- |
| `SessionStart` | a root session starts, resumes, clears, compacts, or forks. Dispatched at the start of the next turn (`run_pending_session_start_hooks`, `core/src/hook_runtime.rs`) | `source`: `startup`, `resume`, `clear`, `compact`, or `fork` |
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
| `SessionEnd` | the root session runtime shuts down | `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `reason` (always `other`). No `model`, `permission_mode`, or `turn_id` |

`Interrupt` and `SessionEnd` fire only for root threads, each after Codex flushes the rollout (`hook_runtime.rs`). `Interrupt` fires before the `turn_aborted` record is emitted (`core/src/tasks/mod.rs`). `SessionEnd` runs from the shutdown handler, which the app-server also reaches when it unloads an idle thread (`unload_thread_without_subscribers`, `app-server/src/request_processors/thread_lifecycle.rs`), so the event does not prove the interactive session ended. Codex 0.152.0 added executor-plugin `Interrupt` hooks (#41432). They run async and leave the command-hook payload unchanged.

Compaction runs in place: Codex writes a `compacted` record into the same rollout, keeps the session id, and queues `SessionStart` with `source = "compact"` for the next turn (`core/src/session/mod.rs`). Local 0.154.0 rollouts confirm it: sessions with several compactions keep one file and a parentless header.

`/btw` (alias `/side`) forks an ephemeral side thread: `side_fork_config` sets `ephemeral = true` ([`tui/src/app/side.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/app/side.rs)). No rollout is persisted, so `hook_transcript_path` yields a null `transcript_path` ([`core/src/hook_runtime.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/hook_runtime.rs)). Its `SessionStart` carries `source: "fork"`. Later `UserPromptSubmit` and `Stop` hooks carry the new session id and null transcript path, but no source or ephemeral marker. A persistent fork has a materialized rollout. An ephemeral root instead starts with `source: "startup"`.

### Tool names

Hook `tool_name` values are canonical names, distinct from the function names a rollout records (`core/src/tools/hook_names.rs`):

| Hook `tool_name` | Covers | Matcher aliases |
| --- | --- | --- |
| `Bash` | simple and unified-exec shell commands, including the final `write_stdin` poll that completes a unified exec | none |
| `apply_patch` | file edits | `Edit`, `Write` |
| `spawn_agent` | child thread spawn | `Agent` |
| `mcp__<server>__<tool>` | MCP tool calls | none |
| `request_user_input` | a blocking questionnaire ([questions](#questions-and-plan-approval)) | none |
| `request_user_input_async` | a [non-blocking question set](#questions-and-plan-approval) | none |
| `update_plan` | the todo-plan tool. Non-blocking | none |

Hooks are a partial interception boundary: web search and other unhooked tool paths bypass them. `update_plan` is off by default since 0.152.0. `[tools.update_plan] enabled = true` turns it on (`core/src/config/mod.rs`). `request_user_input_async` is registered only for root threads whose model catalog entry advertises it (0.153.0).

For function tools without a dedicated contract, `tool_response` is a string for one text item and a content-item array otherwise ([output projection](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/tools/context.rs)). `request_user_input` returns a string containing `{"answers":{…}}`, which requires a second JSON parse. `request_user_input_async` also returns a JSON string.

### Hook output

Every output type is parsed with `deny_unknown_fields` (`hooks/src/engine/output_parser.rs`): an unrecognized field marks the hook run failed. The universal fields are `continue`, `stopReason`, `systemMessage`, and `suppressOutput`. `hookSpecificOutput` carries event-specific fields.

| Event | Accepted | Rejected (hook fails) |
| --- | --- | --- |
| `PreToolUse` | `hookSpecificOutput.permissionDecision` accepts `allow` with `updatedInput`, or `deny` with `permissionDecisionReason`. Optional `hookSpecificOutput.additionalContext` and `systemMessage`. Legacy `decision: "block"` requires `reason`. | `ask`, `allow` without `updatedInput`, `updatedInput` without `allow`, `deny` without a reason, `decision: "approve"`, `continue: false`, `stopReason`, `suppressOutput` |
| `PermissionRequest` | `hookSpecificOutput.decision` `{ behavior: "allow" \| "deny", message }`. A deny without `message` gets "PermissionRequest hook denied approval". `systemMessage` | `updatedInput`, `updatedPermissions`, `interrupt: true`, `continue: false`, `stopReason`, `suppressOutput` |
| `PostToolUse` | `decision: "block"` with `reason`. `hookSpecificOutput.additionalContext`, with `hookSpecificOutput.hookEventName: "PostToolUse"` required (`hooks/src/schema.rs`, `PostToolUseHookSpecificOutputWire`). `continue: false` stops and sends feedback. `systemMessage`, `stopReason` | top-level `additionalContext`, `updatedMCPToolOutput`, `suppressOutput`, `reason` without `decision` |
| `UserPromptSubmit` | `decision: "block"` with `reason`. `hookSpecificOutput.additionalContext`, with `hookSpecificOutput.hookEventName: "UserPromptSubmit"` required ([`hooks/src/schema.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/hooks/src/schema.rs), `UserPromptSubmitCommandOutputWire` and `UserPromptSubmitHookSpecificOutputWire`, both `deny_unknown_fields`). Plain stdout becomes context | a top-level `additionalContext` |
| `SessionStart`, `SubagentStart` | `hookSpecificOutput.additionalContext`, with matching `hookSpecificOutput.hookEventName`. Plain stdout becomes context. `continue: false` stops only on `SessionStart`. `suppressOutput` is ignored on `SessionStart` | |
| `Stop`, `SubagentStop` | Universal fields, plus `decision: "block"` with `reason` | `additionalContext` |
| `PreCompact`, `PostCompact` | Universal fields. `suppressOutput` is ignored | `additionalContext` |
| `Interrupt` | `systemMessage` only, shown as a warning. Non-JSON stdout or a non-zero exit is a failure | everything else |
| `SessionEnd` | output ignored | |

The decision shapes a permission or pre-tool hook returns:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow", "message": "string" } } }
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": { "command": "string" } } }
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "string" } }
```

The `PermissionRequest` decision object carries only `behavior` and `message`. `systemMessage` remains a separate top-level field. Claude Code's `updatedInput`, `updatedPermissions`, and `interrupt` fail it.

Exit codes: exit 0 with JSON applies the output, and exit 0 with empty stdout continues. Exit 2 with non-empty stderr blocks, using stderr as the reason, on `PreToolUse`, `PermissionRequest`, `PostToolUse`, `UserPromptSubmit`, `Stop`, and `SubagentStop`. Exit 2 with empty stderr, or on the compact events and `Interrupt`, is a failure.

A hook run marked `Failed` contributes diagnostics, without a block or stop decision. Timeouts, process errors, and invalid output take this path. Successful output from another handler can still block or stop the flow ([event handlers](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs/hooks/src/events)). A failed `PermissionRequest` hook supplies no approval decision, so normal approval processing continues. Asynchronous hooks cannot apply control effects, including exit-2 blocking.

Other nonzero exits fail the handler. Exit 2 also fails on `SessionStart` and `SubagentStart`. For exit-zero non-JSON stdout, these rules apply:

| Events | Plain stdout |
| --- | --- |
| `SessionStart`, `SubagentStart`, `UserPromptSubmit` | Added as context |
| `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact` | Ignored unless it looks like malformed JSON |
| Synchronous `Stop`, `SubagentStop`, and `Interrupt` | Fails the handler |
| `SessionEnd` | Ignored |

JSON-looking text starts with `{` or `[`. Malformed JSON fails parsing. Public hook-run summaries expose failures through `hook/completed` and the TUI's hook details.

## Async questions (observed on 0.158.0)

The captures below describe 0.158.0. In 0.162.1, an accepted ordinary composer prompt clears pending questions, while question-editor answers preserve unanswered siblings ([submission handling](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/chatwidget/input_submission.rs)). Escape collapses the expanded editor when you are not editing text. This behavior defaults on through `tui.question_esc_back`. Ctrl+C follows the interrupt path ([key handling](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/chatwidget/questions.rs)). The earlier Escape behavior below is historical.

In the 0.158.0 capture, `request_user_input_async` takes `questions: [{title, options?: [string]}]`, returns `{"accepted":true}` immediately, and leaves the turn running. Both PreToolUse and PostToolUse fire. PostToolUse carries `tool_use_id`, the input, and `tool_response` as a JSON string (`"{\"accepted\":true}"`). Each question's `questionItemId` is the JSON-encoded string `["request_user_input_async", call_id, index]`. A committed answer fires UserPromptSubmit during the same turn with a prompt containing `<send_user_message_question_reply>`, a JSON array of `{answer, question, questionItemId}`, and the closing tag. Ordinary user prompts do not clear questions. Replies clear only the named questions, and completion, interruption, failure, or thread reset clears the queue. The widget displays a 30-second countdown, but questions remain after it expires. Skipping a question with Ctrl+] sends no hook.

A live trial on 0.158.0 found that pasting a reply envelope into the ordinary composer and pressing Enter cleared the whole pending-question widget, including the unanswered sibling. The UserPromptSubmit hook arrived about 26 seconds later, after four intervening tool boundaries. Composer submission is therefore not equivalent to answering through the question editor.

On 0.158.0, the default editor keys are Shift+Left to expand/advance, Shift+Right to move backward and then collapse at the first question, digits 1 through 9 to select and submit, and paste plus Enter for free text. Answering leaves the editor open on the next pending question (wrapping to the first after answering the last). The current index survives collapse. Escape interrupts the turn. A reply may be held until the next tool boundary. Sources: [`request_user_input_async.rs`](https://github.com/openai/codex/blob/rust-v0.158.0/codex-rs/core/src/tools/handlers/request_user_input_async.rs), [`async_questions/state.rs`](https://github.com/openai/codex/blob/rust-v0.158.0/codex-rs/tui/src/bottom_pane/async_questions/state.rs), [`chatwidget/questions.rs`](https://github.com/openai/codex/blob/rust-v0.158.0/codex-rs/tui/src/chatwidget/questions.rs), and [`answered_question.rs`](https://github.com/openai/codex/blob/rust-v0.158.0/codex-rs/context-fragments/src/answered_question.rs).

## Project directory trust

Codex checks folder access after selecting the destination and reading the app-server's effective configuration ([directory consent](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/onboarding/directory_trust.rs)). An undecided project can stop an unattended launch at the "Folder access" screen. Local directories without a project can skip consent.

A saved local `untrusted` decision shows "Open restricted", or "Open existing task" for a connected resume. Accepting it keeps the saved decision unchanged. Existing tasks can retain configuration and hooks loaded while trusted. Remote checks retain their explicit-cwd scope and skip the screen for saved `untrusted` projects. Local-daemon resumes check selected and saved thread directories, then recheck after consent.

Decisions live in the merged configuration, keyed by absolute path. The user writes them to `$CODEX_HOME/config.toml`, and a managed layer can supply the same keys:

```toml
[projects."/home/user/src/app"]
trust_level = "trusted"   # or "untrusted"
```

`decision_for_dir` (`config/src/loader/mod.rs`) takes the first candidate that carries a level, in this order:

1. The session cwd.
2. The nearest ancestor holding a `project_root_markers` entry (default `[".git"]`). A `.git` directory counts only if it holds `HEAD` (`find_project_root`, `loader/mod.rs`).
3. The main repository root from `resolve_root_git_project_for_trust` (`git-utils/src/trust.rs`). For a linked worktree it reads the `.git` file, follows `gitdir` into `worktrees/<name>`, and returns the checkout owning the canonical `commondir`, so a worktree of a trusted repository is trusted through its main root.

Each candidate is looked up as its normalized canonical path and as written, so a symlinked directory and its target share one decision. Windows folds ASCII case. Other platforms compare exactly (`loader/mod.rs`).

Choosing "Trust and continue" writes `trust_level = "trusted"` for the resolved trust target ([consent persistence](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/onboarding/onboarding_screen.rs)). Restricted-opening choices do not write `trusted`.

| Related behaviour | Upstream |
| --- | --- |
| What trust gates | Project-scoped `.codex/` layers: project-local config, hooks, and rules ([config reference](https://learn.chatgpt.com/docs/config-file/config-reference), `projects.<path>.trust_level`). User-level hook discovery is independent of directory trust. Hook-definition trust still applies. |
| Skipping the screen | No CLI flag skips it. Approval and sandbox flags leave trust untouched. |
| `codex exec --skip-git-repo-check` | Covers the separate not-a-git-repository refusal, which `--dangerously-bypass-approvals-and-sandbox` also bypasses (`exec/src/lib.rs`). |
| Unparseable `config.toml` | The TUI exits at startup instead of prompting (`tui/src/lib.rs`). |
| Helpers before trust | Since 0.154.0 (#42324) startup resolves helper executables from trusted system directories instead of `PATH`, and `codex doctor` inspects executables without running them. |

## Launch and config overrides

### Launch prompt, resume, and fork

| Command | Behaviour |
| --- | --- |
| `codex [OPTIONS] [PROMPT]` | Starts the interactive TUI, submitting `PROMPT` as the first message when given. The CLI parses arguments with clap (`cli/src/main.rs`), so everything after `--` is positional: `codex -- "<prompt>"` passes a prompt even when it begins with `-`. |
| `codex resume [SESSION_ID] [PROMPT]` | Reopens a session in place. `SESSION_ID` is a UUID or a session name (a parseable UUID wins). `--last` picks the most recent. |
| `codex fork [SESSION_ID] [PROMPT]` | Copies the conversation into a new session id and leaves the source untouched. `--last` forks the most recent. |
| `codex exec resume`, `codex exec fork` | Non-interactive equivalents. |

Both interactive commands accept an optional initial `PROMPT` ([CLI definitions](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/cli/src/main.rs)). A fork's rollout header names its source in `forked_from_id`. `--worktree` and `/worktree` create worktrees for new or forked sessions. The `worktrees` feature defaults to enabled ([feature registry](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/features/src/lib.rs)).

`codex exec --json` emits JSONL events: `thread.started`, `turn.started`, `turn.completed`, `turn.failed`, `item.started`, `item.updated`, `item.completed`, and `error`. This is a separate wire from app-server JSON-RPC and rollout files ([event types](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/exec/src/exec_events.rs)). Web-search items carry `query`, `action`, and optional structured `results`. `turn.completed.usage` carries input, cached-input, cache-write-input, output, and reasoning-output token counts.

[Exec flags](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/exec/src/cli.rs) include `--ephemeral`, `--output-schema FILE`, and `--output-last-message FILE`. `--ignore-user-config` skips the user config while authentication still uses `CODEX_HOME`. `--ignore-rules` skips user and project execution-policy files.

### CLI config overrides

Each `-c key=value` or `--config key=value` overrides one loaded configuration key for that launch, and wins over the same key in `~/.codex/config.toml`. A dotted key reaches nested tables. Codex parses the value as TOML and falls back to the raw string when TOML parsing fails (`utils/cli/src/config_override.rs`), so a caller that needs an exact string round trip emits a TOML-quoted value. `--enable <FEATURE>` and `--disable <FEATURE>` are shorthand for `-c features.<name>=true|false`.

| Key | Type and behaviour |
| --- | --- |
| `developer_instructions` | String, added as a developer-role message separate from the user prompt (`config_toml.rs`). |
| `model_instructions_file` | Path. Replaces the built-in instructions (`config_toml.rs`). |
| `model_reasoning_effort` | Reasoning effort for the session model. |
| `model_auto_compact_token_limit` | `Option<i64>`, an absolute token threshold (`config_toml.rs`). The effective limit is `min(configured, 90% of the model context window)`, and 90% is the default when unset (`openai_models.rs`). Compaction triggers at `>=` the limit (`core/src/session/context_window.rs`), so zero or a negative value compacts at once. |
| `service_tier` | `fast` requests priority serving; it lands in the same `cfg.service_tier` as `config.toml` for a fresh TUI, `codex resume`, and `codex exec` alike (`openai/codex@rust-v0.162.1:codex-rs/utils/cli/src/config_override.rs::parse_overrides`, `exec/src/lib.rs`, read 2026-10-13). Core keeps the tier only while feature `fast_mode` is on (`core/src/config/mod.rs`). The rollout records the applied tier as `payload.thread_settings.service_tier` (`"priority"` when fast) on the `thread_settings_applied` event. |
| `features.fast_mode` | Boolean; defaults on (`features/src/lib.rs`), so a user-level `fast_mode = false` drops `service_tier=fast` silently unless the launch also sets it. A managed feature pin wins over this override without an error (`openai/codex@rust-v0.163.0:codex-rs/core/src/config/managed_features.rs::normalize_candidate`). |
| `model_auto_compact_token_limit_scope` | `total` (default) counts all active context. `body_after_prefix` counts growth after the carried compaction prefix and compares against the configured value unclamped (`config_toml.rs`, `config_types.rs`). |

`model_auto_compact_token_limit` is a top-level key only: it is not a field of `[profiles.<name>]` (`config/src/profile_toml.rs`), and it has no dedicated flag or environment variable. The override parser requires a TOML integer for it, so `-c model_auto_compact_token_limit=200000` works. `200k` becomes a string and fails configuration type validation.

`-p NAME` or `--profile NAME` layers `$CODEX_HOME/NAME.config.toml` over the base user config ([shared CLI options](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/utils/cli/src/shared_options.rs)). This full-config file can carry top-level compaction settings. Legacy `[profiles.NAME]` tables are a separate configuration surface.

`--approve-for-me` (alias `--not-so-yolo`) sets automatic approval review, `approval_policy="on-request"`, and `sandbox_mode="workspace-write"`. It conflicts with explicit sandbox and bypass flags, plus the TUI's explicit approval-policy flag.

### Permission profiles and approval review

`-c default_permissions="<name>"` selects `[permissions.<name>]`. A complete profile can be supplied as one CLI inline table, for example `-c 'permissions.room={ extends=":workspace", filesystem={ "/absolute/room"="write" }, network={ enabled=false } }'`. The CLI profile may extend a file-defined profile that extends a built-in. Only `:workspace` and `:read-only` are supported built-in parents; undefined parents and inheritance cycles are startup errors ([profile resolution](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/config/src/permissions_toml.rs)).

`:workspace` grants writes to the workspace, `/tmp`, and the process `TMPDIR`, while protecting `.git` and resolved Git metadata. Missing writable roots are skipped when the sandbox is built ([filesystem policy](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/permissions.rs), [Linux mounts](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/linux-sandbox/src/bwrap.rs)). `network.enabled=false` explicitly restricts network even if the parent enabled it; a default profile without network configuration is restricted. On Linux that restriction denies socket `connect`, `bind`, and `listen`, including AF_UNIX ([network policy](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/config/permissions.rs), [seccomp](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/linux-sandbox/src/seccomp.rs)).

A session-flags `default_permissions` override selects profile syntax even beside legacy `sandbox_mode` and `[sandbox_workspace_write]` settings; the legacy settings, including their network flag, are then ignored. An explicit typed `--sandbox` with a typed `default_permissions` override is refused ([selection and validation](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/config/mod.rs)).

`--ask-for-approval on-request -c approvals_reviewer="auto_review"` routes escalations to Guardian. The `PermissionRequest` hook runs before Guardian, and its payload has no reviewer field ([approval routing](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/tools/approvals.rs), [hook schema](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/hooks/src/schema.rs)). `codex exec` preserves the requested policy for automatic review but forces `never` for the human reviewer; an automatic-review fallback requiring a human is rejected rather than waiting for input ([headless config](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/exec/src/lib.rs)).

### Login status

`codex login status` prints one line to **stderr** and exits 0 when credentials are present (`cli/src/login.rs`). It reports the login kind, never the plan or a token. Workload identity is checked first. Missing credentials and lookup errors exit 1.

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

### Execution policy rules

At the pinned release, [`execpolicy/README.md`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/execpolicy/README.md) defines the Starlark `prefix_rule` syntax. [`core/src/exec_policy.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/exec_policy.rs), `load_exec_policy`, collects `*.rules` from each enabled config layer's `rules/` directory. A user rule file can therefore be `~/.codex/rules/build.rules`, or `$CODEX_HOME/rules/build.rules` with a custom home. Managed policy can disable user/project rules.

```starlark
prefix_rule(
    pattern = ["cargo", "test"],
    decision = "allow",
    match = ["cargo test", "cargo test --locked"],
    not_match = ["cargo publish"],
)
```

Patterns match ordered command-token prefixes, not shell globs. An element may be a list of alternative tokens. `decision` is `allow` (the default), `prompt`, or `forbidden`. Optional `match` and `not_match` examples are validated at load time. Across matching rules the strictest decision wins: `forbidden` over `prompt` over `allow`. These rules belong to the Codex user configuration.

Files in each layer are sorted before loading; `#` comments are legal. A parse error in any file discards the file-loaded rules with a warning, retaining managed requirements. Codex writes its own “don't ask again” amendments into `default.rules`, so a separately owned rules file need not compete for that path ([loader and amendments](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/exec_policy.rs)).

An absolute executable path matches by basename. A plain `bash -lc 'rimz message @a hi'` can match the inner command, but a redirection makes the wrapper the command, so `rimz agents list --json > file` does not match a `rimz agents` prefix ([shell parsing](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/shell-command/src/bash.rs)). An allow match normally skips permission hooks and Guardian and runs unsandboxed; deny-read entries retain restrictions, and strict automatic review can still require review ([orchestrator](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/tools/orchestrator.rs)).

### Per-launch skill overrides

[`skills.config`](https://developers.openai.com/codex/skills) entries accept `name` or `path` selectors and `enabled=false`. Disabled skills are also unavailable to explicit `$skill` invocation. [CLI `-c` overrides](https://developers.openai.com/codex/config-advanced) can supply this array for one run. Local probes on 0.157.0 found that `name` selects the frontmatter name (directory fallback), there is no wildcard, and file `[[skills.config]]` entries remain in effect alongside CLI entries.

## TUI input and notifications

### Paste burst

The composer treats plain characters arriving at most 8 ms apart as a suspected paste once at least 3 arrive (`paste_burst.rs`). It flushes the buffered burst after 8 ms of inactivity, or 60 ms on Windows (`paste_burst.rs`), and appends Enter received during the burst or within the following 120 ms as a literal newline instead of submitting (`PASTE_ENTER_SUPPRESS_WINDOW`). A bracketed paste clears the burst state through `clear_after_explicit_paste` (`paste_burst.rs`). A client typing raw keystrokes therefore needs a gap of more than 120 ms before a separate Enter. A bracketed paste does not.

`tui.disable_paste_burst = true` turns the heuristic off (`config/src/types.rs`, default false). Since 0.153.0 that is the key's home. The top-level `disable_paste_burst` still works as a fallback.

### `notify`

`notify` runs an external program when a turn completes, independent of hooks. It must be set in the user-level config. A project-local `notify` is ignored with a startup warning (`PROJECT_LOCAL_CONFIG_DENYLIST`, `config/src/loader/mod.rs`).

```toml
notify = ["python3", "/path/to/notify.py"]
```

The program receives one JSON document as its last argument (`hooks/src/legacy_notify.rs`). `agent-turn-complete` is the only type:

```json
{
  "type": "agent-turn-complete",
  "thread-id": "string",
  "turn-id": "string",
  "cwd": "string",
  "client": "string (optional)",
  "input-messages": ["user messages preceding the turn"],
  "last-assistant-message": "string or null"
}
```

In-terminal notifications are a separate `[tui]` setting (`config/src/types.rs`), and approval prompts appear only there:

| Key | Values |
| --- | --- |
| `tui.notifications` | `true`, `false`, or an array of event types containing `agent-turn-complete`, `approval-requested`, `plan-mode-prompt`, or `async-question` |
| `tui.notification_method` | `auto`, `osc9`, or `bel` |
| `tui.notification_condition` | `unfocused` or `always` |

## App-server API

`codex app-server` is a bidirectional JSON-RPC 2.0 service with the `"jsonrpc":"2.0"` member omitted on the wire. It exposes thread, turn, account, config, and filesystem operations and streams notifications. The TUI itself runs on it. The protocol has three primitives: an **item** (one input or output unit with a `started`, optional `delta`, `completed` lifecycle), a **turn** (the items from one unit of agent work), and a **thread** (the durable session).

### Transport and handshake

`--listen <URL>` selects the transport (`codex app-server --help`):

| URL | Transport |
| --- | --- |
| `stdio://` | JSONL over stdin and stdout (default, also selected by `--stdio`) |
| `unix://`, `unix://PATH` | WebSocket HTTP upgrade over a Unix domain socket, then JSON-RPC text frames. Bare `unix://` is `$CODEX_HOME/app-server-control/app-server-control.sock` (`app-server-transport/src/transport/mod.rs`). |
| `ws://IP:PORT` | WebSocket over TCP. Experimental |
| `off` | no listener |

A client sends one `initialize` request per connection, then an `initialized` notification, before any other method. The `initialize` result carries `userAgent` (which embeds the Codex version), `codexHome`, `platformFamily`, and `platformOs`.

```jsonc
{ "id": 1, "method": "initialize", "params": { "clientInfo": { "name": "my-client", "version": "x.y.z" } } }
{ "method": "initialized", "params": {} }
```

`initialize.params.capabilities` can negotiate `experimentalApi`, `requestAttestation`, exact-name `optOutNotificationMethods`, and MCP `extensions`. Legacy `mcpServerOpenaiFormElicitation` remains accepted. `explicitGatewayOauth` selects explicit gateway login for the app-server process, and later connections cannot undo it ([capabilities](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/schema/typescript/InitializeCapabilities.ts)).

Ingress queues are bounded. When one is full the server rejects the request with retryable JSON-RPC error `-32001` on stdio, WebSocket, and Unix-socket connections (`enqueue_incoming_message`, `transport/mod.rs`). The docs describe this for WebSocket mode only.

### Read methods

These reads use the 0.162.1 stable schema and do not subscribe to a thread. Mutating methods appear in the [method index](#method-index).

**`thread/loaded/list`** returns the thread ids the app-server holds in memory. Params are `{ cursor?, limit? }`.

```jsonc
// v2/ThreadLoadedListResponse
{ "data": ["thread-id", …], "nextCursor": "string | null" }
```

An inactive thread with no subscribers unloads after `thread_unload_delay_secs`, default 60. The delay starts at the later transition to inactivity or no subscribers. Zero removes that delay, but active turns stay loaded ([unload condition](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server/src/request_processors/thread_lifecycle.rs)). The list reports loaded threads, which can outlive attached terminals. Managed restart recovery can repopulate it after startup.

**`account/rateLimits/read`** returns included-usage windows, plan, credits, and reset credits for a ChatGPT login. Fields are camelCase. Optional request booleans `supportsLunaReserve` and `excludeResetCreditDetails` default to false ([request type](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/schema/typescript/v2/GetAccountRateLimitsParams.ts)). `excludeResetCreditDetails` skips the separate detail fetch while retaining the available count. `supportsLunaReserve` declares support for automatic reserve fallback and can record backend experiment exposure.

```jsonc
// v2/GetAccountRateLimitsResponse
{
  "rateLimits": {                         // required. The single-bucket view
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
  "rateLimitsByLimitId": { "codex": { … } },  // object | null. RateLimitSnapshot per metered limit id
  "rateLimitResetCredits": {                  // object | null
    "availableCount": 2,
    "credits": [                              // array | null. null means only the count is known
      { "id": "opaque", "status": "available", "resetType": "codexRateLimits",
        "grantedAt": 1789000000, "expiresAt": 1789600000, "title": null, "description": null }
    ]
  },
  "ordinaryUsageAllowed": true,  // boolean | null. null means unavailable
  "accountId": "string | null",
  "rateLimitUpsell": {}          // optional backend banner. nested keys stay snake_case
}
```

| Type | Values |
| --- | --- |
| `RateLimitWindow` | `usedPercent` (int, required), `windowDurationMins` (int or null), `resetsAt` (epoch seconds or null) |
| `CreditsSnapshot` | `hasCredits` (required), `unlimited` (required), `balance` (string or null) |
| `PlanType` | `free`, `go`, `plus`, `pro`, `prolite`, `promax`, `team`, `self_serve_business_prolite`, `self_serve_business_usage_based`, `business`, `ent26`, `enterprise_cbp_automation`, `enterprise_cbp_usage_based`, `enterprise`, `edu`, `edu_plus`, `edu_pro`, `unknown` |
| `RateLimitReachedType` | `rate_limit_reached`, `workspace_owner_credits_depleted`, `workspace_member_credits_depleted`, `workspace_owner_usage_limit_reached`, `workspace_member_usage_limit_reached` |
| `RateLimitResetCredit.status` | `available`, `redeeming`, `redeemed`, `unknown` |
| `RateLimitResetCredit.resetType` | `codexRateLimits`, `unknown` |

`CreditsSnapshot` has no `overageLimitReached` field, and the response has no root-level `credits` object. `accountId`, `ordinaryUsageAllowed`, `rateLimitUpsell`, and `normalModelSlug` are new since 0.150.1.

**`model/list`** with `{ "includeHidden": true }` returns the model catalog in `data[]`. Each `Model` carries `id`, `model`, `displayName`, `description`, `hidden`, `isDefault`, `defaultReasoningEffort`, `supportedReasoningEfforts`, service tiers, input modalities, and upgrade metadata. It also carries `modelSpecialty`, `multiAgentVersion`, `defaultServiceTier`, and `availableAccessPrograms` ([model type](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/schema/typescript/v2/Model.ts)). `defaultReasoningEffort` is the catalog default, not a session's live effort.

**`thread/read`** with `{ "threadId": "<id>", "includeTurns": false }` returns `{ "thread": Thread }`. **`thread/list`** returns `{ data: Thread[], nextCursor, backwardsCursor }` and filters on `archived`, `cwd`, `searchTerm`, `sourceKinds`, `sortKey`, and more. A `Thread` always carries `id`, `sessionId`, `preview`, `cwd`, `cliVersion`, `createdAt`, `updatedAt`, `ephemeral`, `modelProvider`, `projectId`, `source`, `status`, `turns`, and `historyMode`. Nullable fields include `name`, `model`, `reasoningEffort`, `forkedFromId`, `parentThreadId`, `agentNickname`, `agentRole`, `threadSource`, `path`, and `gitInfo`. The [thread type](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/schema/typescript/v2/Thread.ts) also carries nullable `originator`, `recencyAt`, `section`, and `sectionEnteredAt`. `name` is the thread title ([session index](#session-index)). `preview` is derived from the first user message. `includeTurns: true` is deprecated for paginated threads in favour of `thread/turns/list` and `thread/items/list`.

```jsonc
{ "thread": { "id": "01a09a03-fd61-7eb3-9f97-0b24ec37cde7", "sessionId": "01a09a03-fd61-7eb3-9f97-0b24ec37cde7",
              "preview": "Trace the isolation override", "name": "Trace isolation override flow", "updatedAt": 1789290354, … } }
```

`thread/items/list.cursor` accepts an opaque string or `{ "type": "item", "itemId": "<id>" }`. An item anchor requires nonempty `turnId` and excludes that item in the requested direction. Returned entries carry nullable `startedAtMs` and `completedAtMs` ([paging contract](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/schema/typescript/v2/ThreadItemsListParams.ts)).

Live context-window usage arrives through `thread/tokenUsage/updated`, with `{ threadId, turnId, tokenUsage }`. There is no equivalent context-window read method. `account/usage/read` accepts optional `threadId` for backend billing estimates. Its nullable `threadUsage` contains credit and USD estimates plus model/effort/speed groups ([usage response](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/schema/typescript/v2/GetAccountTokenUsageResponse.ts)). Billing estimates do not measure live context occupancy.

`account/gatewayOAuth/read` returns `{ providerId, providerName, required, status, error }`, without credentials. `status` is null or `notReady`, `started`, `succeeded`, or `failed`. `account/gatewayOAuth/changed` reports changes ([types](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/schema/typescript/v2/GatewayOAuthReadResponse.ts)).

`thread/attachment/list` pages a thread's attachments. `thread/attachmentOwner/list` finds owners by `{ attachmentType, identityKey, archived?, cursor?, limit? }`.

### Method index

The 0.162.1 stable schema, grouped. `--experimental` adds the methods listed last. The release's [schema exports](https://github.com/openai/codex/tree/rust-v0.163.0/codex-rs/app-server-protocol/schema/precomputed) include complete request and notification types.

| Group | Methods |
| --- | --- |
| Thread | `thread/start`, `thread/resume`, `thread/fork`, `thread/read`, `thread/list`, `thread/loaded/list`, `thread/unsubscribe`, `thread/archive`, `thread/unarchive`, `thread/delete`, `thread/name/set`, `thread/metadata/update`, `thread/goal/{set,get,clear}`, `thread/compact/start`, `thread/revert`, `thread/inject_items`, `thread/shellCommand`, `thread/turns/list`, `thread/items/list` (both stable since 0.154.0), `thread/approveGuardianDeniedAction`, `thread/attachment/{add,list,remove}`, `thread/attachmentOwner/list`, `thread/section/move`, `threadSection/{create,list,update,delete}` |
| Turn and review | `turn/start`, `turn/steer`, `turn/interrupt`, `review/start` |
| Account | `account/read`, `account/gatewayOAuth/{read,login,cancel}`, `account/login/{start,cancel}`, `account/logout`, `account/rateLimits/read`, `account/rateLimitResetCredit/consume`, `account/usage/read`, `account/workspaceMessages/read`, `account/sendAddCreditsNudgeEmail` |
| Exec and filesystem | `command/exec`, `command/exec/{write,resize,terminate}`, `fs/{readFile,writeFile,createDirectory,getMetadata,readDirectory,remove,copy,watch,unwatch}`, `fuzzyFileSearch` |
| Config, models, features | `config/read`, `config/value/write`, `config/batchWrite`, `config/mcpServer/reload`, `configRequirements/read`, `model/list`, `modelProvider/capabilities/read`, `permissionProfile/list`, `experimentalFeature/list`, `experimentalFeature/enablement/set` |
| Skills, hooks, plugins, apps, MCP | `skills/list`, `skills/config/write`, `skills/extraRoots/set`, `hooks/list`, `plugin/{list,read,install,installed,uninstall,reconcile}`, `plugin/skill/read`, `plugin/share/{list,save,delete,checkout,updateTargets}`, `marketplace/{add,remove,upgrade}`, `app/{list,read,installed}`, `mcpServerStatus/list`, `mcpServer/oauth/login`, `mcpServer/resource/read`, `mcpServer/tool/call` |
| Other | `externalAgentConfig/{detect,import}`, `externalAgentConfig/import/{readHistories,recordHistory}`, `feedback/upload`, `windowsSandbox/{readiness,setupStart}`, legacy `getConversationSummary`, `gitDiffToRemote`, `getAuthStatus` |
| Experimental only | `process/{spawn,writeStdin,resizePty,kill}`, `thread/queue/*`, `thread/realtime/*`, `thread/search`, `thread/searchOccurrences`, `thread/prediction/request`, `thread/{increment_elicitation,decrement_elicitation}`, `thread/timeline/list`, `thread/settings/update`, `turn/settings/update`, `thread/backgroundTerminals/*`, `thread/memoryMode/set`, `remoteControl/*`, `project/*`, `environment/*`, `collaborationMode/list`, `userVerification/*`, `account/bedrock/{discover,setup,checkGovCloudRequirements}`, `server/diagnostics`, `memory/{reset,status}`, `rollout/compress`, `plugin/search`, `mcpServer/event/stream/{start,stop}`, `fuzzyFileSearch/{sessionStart,sessionUpdate,sessionStop}` |

`account/rateLimitResetCredit/consume` takes `{ idempotencyKey, creditId? }` and returns `{ outcome }` with `reset`, `nothingToReset`, `noCredit`, or `alreadyRedeemed`.

`account/gatewayOAuth/login` and `cancel` control authentication. `thread/attachment/add` takes `{ threadId, attachmentType, identityKey, payload }` ([attachment types](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/schema/typescript/v2/ThreadAttachmentAddParams.ts)).

`thread/rollback` was removed in 0.156.0 ([release notes](https://github.com/openai/codex/releases/tag/rust-v0.156.0), #44915). Use `thread/revert`. Legacy `thread_rolled_back` rollout records remain readable.

Server requests to the client: `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, `item/tool/requestUserInput`, `item/tool/call`, `mcpServer/elicitation/request`, `account/chatgptAuthTokens/refresh`, `attestation/generate`, and the legacy `execCommandApproval` and `applyPatchApproval`. Experimental `currentTime/read` asks for `{ currentTimeAt }` in whole Unix seconds ([request registry](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/src/protocol/common.rs)).

Notifications include `thread/{started,status/changed,tokenUsage/updated,name/updated,compacted,archived,unarchived,deleted,closed,goal/updated,goal/cleared,settings/updated,queue/changed}`, `turn/{started,completed,diff/updated,plan/updated}`, `item/{started,completed}` with the `item/agentMessage/delta`, `item/reasoning/*`, `item/commandExecution/outputDelta`, and `item/fileChange/*` streams, `hook/{started,completed}`, `account/{updated,rateLimits/updated,login/completed}`, `model/rerouted`, `remoteControl/status/changed`, `account/gatewayOAuth/changed`, `thread/attachment/updated`, experimental `thread/prediction/updated`, `thread/reverted`, `serverRequest/resolved`, `model/verification`, `modelProvider/{authRecoveryStarted,authRecoveryCompleted}`, `error`, and `warning`. The generated `ServerNotification.json` contains the full list. The schema-generation commands in [Baseline and sources](#baseline-and-sources) emit it.

`item/tool/requestUserInput` carries `threadId`, `turnId`, `itemId`, `questions`, and `isBlocking`. Missing `isBlocking` defaults to true. Deprecated `autoResolutionMs` must not determine blocking ([request type](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/src/protocol/v2/item.rs)).

`ThreadItem` types: `userMessage`, `hookPrompt`, `agentMessage`, `functionCallOutput`, `plan`, `reasoning`, `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `collabAgentToolCall`, `subAgentActivity`, `webSearch`, `imageView`, `sleep`, `imageGeneration`, `enteredReviewMode`, `exitedReviewMode`, `contextCompaction`.

`TurnError.codexErrorInfo` uses camelCase. String variants include `contextWindowExceeded`, `sessionBudgetExceeded`, `usageLimitExceeded`, `rateLimitExceeded`, `flexUnavailable`, `serverOverloaded`, `cyberPolicy`, `misalignmentPolicyViolation`, `tooManyDenials`, `internalServerError`, `unauthorized`, `badRequest`, `threadRollbackFailed`, `sandboxError`, and `other`. Object variants `httpConnectionFailed`, `responseStreamConnectionFailed`, `responseStreamDisconnected`, and `responseTooManyFailedAttempts` carry nullable `httpStatusCode`. `activeTurnNotSteerable` carries `turnKind`, either `review` or `compact`. The [exported type](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/schema/typescript/v2/CodexErrorInfo.ts) also permits unknown strings and objects. The [rollout spelling](#rollout-records) is snake case. Rollout `bio_policy` and `invalid_prompt` map to app-server `other` ([conversion](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-protocol/src/protocol/v2/shared.rs)).

### App-server daemon

`codex remote-control start` enables remote control and starts the persistent app-server for its `CODEX_HOME`. `codex remote-control stop` stops it, and `pair` prints a pairing code. `codex app-server daemon` exposes `start`, `restart`, `stop`, `bootstrap`, `update`, `enable-remote-control`, `disable-remote-control`, and `version`. Successful lifecycle commands print one JSON object ([daemon reference](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/README.md)).

| Item | Upstream |
| --- | --- |
| Managed binary | Codex selects one package root ([installation selection](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/src/managed_install.rs)): dedicated `$CODEX_HOME/packages/app-server-daemon` when its `current` exists or any of `daemon.pid`, `daemon.stderr.log`, `daemon-updater.pid`, or `daemon-updater.stderr.log` exists; otherwise `$CODEX_HOME/packages/standalone` when a legacy `app-server` counterpart of those files exists; otherwise dedicated. Within the selected root the binary is `current/bin/codex` (`codex.exe` on Windows), else the legacy `current/codex`. `install.sh` installs the CLI into `packages/standalone` by default (`CODEX_INSTALL_DAEMON_ONLY=1` targets the dedicated root), and that install alone does not select the standalone root, so a default CLI home provisions the dedicated root on its first daemon start. The dedicated root was introduced in 0.156.0. |
| PID records | `$CODEX_HOME/app-server-daemon/daemon.pid` and `daemon-updater.pid`. Legacy installations use `app-server.pid` and `app-server-updater.pid`. JSON records contain `pid`, `processStartTime`, optional `processIdentity`, and optional `executableIdentity` ([PID backend](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/src/backend/pid.rs)). |
| Process identity | Linux uses `{ bootId, startTicks }`. Current macOS records use `{ bootId, uniqueId, startSeconds, startMicroseconds }`. Older timestamp-only macOS records remain readable. `processStartTime` retains the platform start-time string for legacy readers ([native identity](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/src/backend/pid_identity.rs)). |
| Executable identity | `digest` and optional `path_digest` are 32-byte arrays. The path digest distinguishes release directories containing identical executable bytes (`managed_install.rs`). |
| Other state | `daemon.lock`, `settings.json`, `daemon.stderr.log`, `daemon-updater.stderr.log`, and `daemon-updater.sock`. Legacy names use `app-server` in place of `daemon` (`app-server-daemon/src/lib.rs`). |
| Control socket | `$CODEX_HOME/app-server-control/app-server-control.sock`, using the [Unix WebSocket transport](#transport-and-handshake). |
| App-server argv | `codex app-server --remote-control --listen unix://`, or without `--remote-control`. The daemon adds `--analytics-default-enabled`. Saved feature overrides add `-c features.<name>=<bool>` (`backend/pid.rs`). |
| Updater argv | `codex app-server daemon pid-update-loop`, optionally with internal restoration arguments. |
| Shutdown | Unix sends SIGTERM, then forces termination after the grace period. Windows sends the PID as a text frame over the control socket at `ws://localhost/daemon/shutdown` and expects the same PID back ([shutdown client](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/src/client.rs)). |

[Updater settings](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/src/settings.rs) live in `settings.json`: `updater.autoUpdateEnabled` defaults to true and `updater.updateIntervalMinutes` defaults to 60. The interval must be positive. `shutdownGraceSeconds` defaults to 60 and accepts 0 through 300. Zero requests graceful shutdown and then forces it immediately.

The [update loop](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/src/update_loop.rs) normally waits five minutes before its first scheduled check. Managed-binary handoff and restoration use shorter waits. Each scheduled pass can install a release, restart the app-server, and replace the updater process.

`daemon update` selects the latest stable release even when automatic updates are disabled. It can restore pinned or local packages to the production update channel. It reports `updated`, `noUpdate`, or `unsupported`. A running daemon restarts, while a stopped daemon stays stopped. Active or queued work can be interrupted. CLI `-c` overrides do not reach the updater.

Managed restarts can restore persisted root threads from `$CODEX_HOME/app-server-daemon/loaded-threads.json`. A managed shutdown writes the file, and startup removes it before the daemon serves requests; restoration then runs in the background, so readiness does not mean restoration has finished, and no upstream marker says when it has ([recovery](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server/src/daemon_thread_recovery.rs)). An explicit `stop`, even with no daemon running, and a fresh `start` discard the file. The running daemon can use a different release from the invoking CLI. `daemon version` and the connection's `initialize.userAgent` expose its version.

`start` and `restart` ensure an eligible updater. `stop` stops the app-server and clears pending thread recovery. `bootstrap --remote-control` stops the previous updater and app-server, then starts the selected managed installation ([lifecycle implementation](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/src/lib.rs)).

Zombie app-servers count as inactive. Codex 0.144.4 and earlier could treat a zombie as active because its PID and start time still matched. That behavior could make startup wait for an absent control socket and shutdown exceed its timeout.

## Session files

### Rollout files

Codex writes a current rollout for each persistent thread at `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<thread_id>[_<rollout_id>].jsonl` (`precompute_new_rollout_path`, `rollout/src/recorder.rs`, `rollout_file_name.rs`). The optional rollout id appears when a reverted thread keeps its thread id but starts a new file. The newest timestamp and UUIDv7 rollout id select the current revision. Archiving moves a rollout into the flat `$CODEX_HOME/archived_sessions/` (`recorder.rs`). Readers accept `.jsonl` and zstd-compressed `.jsonl.zst` (`compression.rs`). Writing compressed files is behind the `local_thread_store_compression` feature, which is under development and off by default (`features/src/lib.rs`).

Each line carries `timestamp`, `type`, and `payload`. Paginated records carry increasing `ordinal` values, starting at zero or the inherited exclusive boundary. Legacy records omit `ordinal` ([ordinal state](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/rollout/src/ordinal.rs)). A `response_item` can also carry top-level `metadata`. The [wire enum](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/history/src/rollout_payload.rs) defines `session_meta`, `turn_context`, `event_msg`, `response_item`, `compacted`, `token_usage_record`, `world_state`, `retained_context`, `security_risk_score`, `inter_agent_communication`, `inter_agent_communication_metadata`, and `realtime_item`. Which `event_msg` records persist depends on the thread's history mode (`persisted_event_msg`, `rollout/src/policy.rs`):

| Record | Legacy | Paginated |
| --- | --- | --- |
| `item_completed` with a turn item | `Plan`, sleep extension items, `FunctionCallOutput`, and completed `SubAgentActivity` items only | every turn item, including `UserMessage` and `AgentMessage` |
| `user_message`, `agent_message`, `agent_reasoning`, review, patch, MCP, web-search, image, and context-compacted end events | persisted | omitted |
| `token_count`, `task_started`, `task_complete`, `turn_aborted`, `thread_goal_updated`, `thread_rolled_back`, thread settings | persisted | persisted |
| `error`, `stream_error`, `warning`, exec begin and delta events, approval requests | never persisted | never persisted |
| `response_item` | persisted except `compaction_trigger` and unknown `other` items | same |
| `realtime_item` | omitted | persisted |

Paginated persistence truncates command output and MCP results using 64 KiB limits ([persistence policy](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/rollout/src/policy.rs)). Persisted output can therefore differ from the live stream.

Experimental `rollout/compress` takes no parameters and returns `{}` after scheduling a cold-file compression pass. It works independently of the startup compression flag. Its acknowledgement does not imply completion, and it has no progress or cancellation API ([compression API](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server/README.md#local-rollout-compression)).

`history_mode` uses `legacy` or `paginated`. Missing values default to `legacy`. Non-ephemeral TUI sessions use Paginated history (`tui/src/app_server_session.rs`), and the header records it as `history_mode`. `codex migrate-rollouts` converts legacy sessions. `response_item` rows are model transport, including developer and system messages, and are not a second copy of the visible transcript.

### Rollout records

The header is the first line. Its `id` identifies the thread, while `session_id` identifies the shared session tree. Child hooks point `transcript_path` at the child's rollout. `SessionMeta` (`protocol.rs`) carries `id`, `session_id`, `timestamp`, `cwd`, `originator`, `cli_version`, `source`, `model_provider`, `history_mode`, `context_window`, git metadata, and these lineage fields:

| Field | Meaning |
| --- | --- |
| `creator_user_id`, `creator_account_id` | Creator identity when available. The account id stays fixed across resumes. |
| `selected_capability_roots` | Capability roots selected by the hosting platform. |
| `runtime_workspace_roots` | Runtime workspace roots at creation. Absent means unknown, and an empty list means no roots. |
| `history_base` | Inherited prefix: `thread_id` identifies its rollout, with boundaries `end_ordinal_exclusive` and `end_byte_offset`. A reverted thread can have a distinct rollout id. |
| `subagent_history_start_ordinal` | First ordinal belonging to the child's own projected history. |
| `forked_from_id` | Source thread of a user fork (`/fork`, `codex fork`). An ephemeral `/side` or `/btw` thread writes no rollout. Absent on a fresh root such as `/new` or `/clear`. |
| `forked_from_ordinal_exclusive` | Ordinal in the source where the copy stops. New since 0.150.1. |
| `thread_source` | Open analytics label: `user`, `subagent`, `guardian_review`, `memory_consolidation`, or any feature string ([`protocol.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/protocol.rs)). Not authority for root status. |
| `parent_thread_id` | Immediate parent of a child. |
| `agent_nickname`, `agent_path`, `agent_role` | Child display name, root-relative task path such as `/root/research/explore_hooks`, and role. `agent_role` also accepts `agent_type`. |
| `multi_agent_version` | `disabled`, `v1`, or `v2`. |
| `source` | `"cli"`, `"vscode"`, `"exec"`, `"mcp"`, `"unknown"`, `{"custom":s}`, `{"internal":"guardian"}` or `{"internal":"memory_consolidation"}`, `{"subagent":"review"}`, `{"subagent":"compact"}`, `{"subagent":"memory_consolidation"}`, `{"subagent":{"other":s}}`, or `{"subagent":{"thread_spawn":{parent_thread_id,depth,agent_path,agent_nickname,agent_role}}}`. `SessionSource::is_non_root_agent` is `Internal` or `SubAgent` ([`protocol.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/protocol.rs)). A saved Guardian thread records `{"subagent":{"other":"guardian"}}` ([`thread_manager.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/thread_manager.rs)). |

```jsonc
{ "type": "session_meta", "payload": {
    "id": "<thread_id>", "cwd": "/repo", "cli_version": "0.154.0", "history_mode": "paginated",
    "thread_source": "subagent", "parent_thread_id": "<parent>", "forked_from_id": "<fork_source>",
    "agent_nickname": "Atlas", "agent_path": "/root/research/explore_hooks",
    "agent_role": "explorer", "multi_agent_version": "v2" } }
```

Forks and children inherit parent history, which can include `token_count` records. Paginated forks can reference a shared prefix through `history_base` instead of copying it into the new file ([session creation](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/session/session.rs)). A raw child-file scan can therefore omit inherited records.

Turn and usage records, as a 0.154.0 build writes them:

```jsonc
// model and effort for the turn. collaboration_mode.mode is "plan" or "default"
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

`TokenUsage` (`protocol.rs`): `input_tokens` includes both cache slices, `output_tokens` includes `reasoning_output_tokens`, and `cache_write_input_tokens` defaults to 0 when absent (older rollouts omit it). `last_token_usage` is the most recent request. `total_token_usage` is cumulative for the rollout, including inherited parent usage. Do not sum cumulative totals across records or count inherited usage as new child work.

Visible messages in Paginated mode:

```jsonc
// user message. content is UserInput, snake-tagged: text, image, local_image, audio, local_audio, skill, mention
{ "type": "event_msg", "payload": { "type": "item_completed", "turn_id": "turn-1",
    "item": { "type": "UserMessage", "content": [ { "type": "text", "text": "prompt", "text_elements": [] } ] } } }

// assistant message. The content tag is capitalized. The item may also carry phase, memory_citation, delivery, questions
{ "type": "event_msg", "payload": { "type": "item_completed", "turn_id": "turn-1",
    "item": { "type": "AgentMessage", "content": [ { "type": "Text", "text": "assistant update" } ] } } }
```

Legacy mode writes the same text as `{ "type": "user_message", "message": … }` and `{ "type": "agent_message", "message": … }` event payloads.

Turn endings (`TurnCompleteEvent` and `TurnAbortedEvent`, `protocol.rs`). Completion serializes as `task_complete` with alias `turn_complete`. Timestamps inside the payload are epoch seconds:

```jsonc
// clean completion
{ "type": "event_msg", "payload": { "type": "task_complete", "turn_id": "…", "last_agent_message": "patch is correct",
    "started_at": 1789290347, "completed_at": 1789290628, "duration_ms": 280548 } }

// failed turn
{ "type": "event_msg", "payload": { "type": "task_complete", "turn_id": "…", "last_agent_message": null,
    "error": { "message": "This content was flagged for possible cybersecurity risk. …", "codex_error_info": "cyber_policy" } } }

// interrupted turn
{ "type": "event_msg", "payload": { "type": "turn_aborted", "turn_id": "…", "reason": "interrupted",
    "started_at": 1789043828, "completed_at": 1789043832, "duration_ms": 4223 } }
```

| Field | Values |
| --- | --- |
| `turn_aborted.reason` | `interrupted`, `replaced`, `review_ended`, `budget_limited`. Esc and `/clear` during a turn were observed writing `interrupted` on 0.144.x |
| `task_complete.error` | `ErrorEvent { message, codex_error_info }` (`protocol.rs`) |
| `codex_error_info` | snake case (`protocol.rs`): `context_window_exceeded`, `session_budget_exceeded`, `usage_limit_exceeded`, `rate_limit_exceeded`, `flex_unavailable`, `server_overloaded`, `cyber_policy`, `bio_policy`, `misalignment_policy_violation`, `too_many_denials`, `http_connection_failed`, `response_stream_connection_failed`, `internal_server_error`, `unauthorized`, `bad_request`, `invalid_prompt`, `sandbox_error`, `response_stream_disconnected`, `response_too_many_failed_attempts`, `active_turn_not_steerable`, `thread_rollback_failed`, `other` |

`token_usage_record.payload` carries `thread_id`, `turn_id`, `session_id`, `root_turn_id`, `response_id`, `usage`, `turn_token_usage`, and `thread_token_usage`. It records best-effort usage for a completed response. The latter two usage fields are cumulative.

`task_started`, `task_complete`, and `turn_aborted` can carry `root_turn_id`. `task_complete` can also carry `time_to_first_token_ms`, and `task_started` can carry `trace_id`. `task_started.turn_attribution` adds `turn_id`, `turn_trigger`, `parent_turn_id`, `initiating_agent_path`, and `root_turn_id` ([turn input types](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/turn_input.rs)). `turn_aborted.error` can contain the interruption error.

`other` is a catch-all and can accompany a transient failure such as an HTTP 503. There is no `turn_error` record, and `error` and `stream_error` events are never persisted.

Some provider failures leave no error in the rollout. Observed on 0.142.x, a serving-capacity failure (TUI: `Selected model is at capacity. Please try a different model.`) and a usage-limit failure (TUI: `You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage … try again at 6:35 AM.`) both end in a `task_complete` with `last_agent_message: null` and no `error`, with no `Stop` hook and no error on the app-server thread. See openai/codex #22277, #19579, #28507, and #29760. This has not been re-observed on 0.162.1.

### Session index

`$CODEX_HOME/session_index.jsonl` maps a thread id to its title (`rollout/src/session_index.rs`). The TUI generates a title of at most 36 characters after the first turn (`THREAD_TITLE_MAX_CHARS`, `tui/src/app/thread_title.rs`) and appends one row with it (`append_thread_name`, `session_index.rs`). Renames append further rows, and the newest valid row for an `id` wins. Deleting a thread rewrites the file without its rows (`remove_thread_name_entries`, `session_index.rs`). Since 0.154.0 (#42749) the TUI writes only the generated title. Earlier builds first appended a provisional row made from the whitespace-normalized first prompt.

```json
{ "id": "01a09a03-fd61-7eb3-9f97-0b24ec37cde7", "thread_name": "Trace isolation override flow", "updated_at": "2026-09-13T09:05:54.102581041Z" }
```

The same title is the app-server `Thread.name`.

### Questions and plan approval

A Plan-mode turn records `turn_context.payload.collaboration_mode.mode = "plan"` and `task_started.payload.collaboration_mode_kind = "plan"`. Hook `permission_mode` describes approval policy and cannot identify Plan mode. The plan itself is a completed `Plan` item followed by a clean completion of the same turn:

```jsonc
{ "type": "event_msg", "payload": { "type": "item_completed", "turn_id": "turn-1",
    "item": { "type": "Plan", "id": "turn-1-plan", "text": "# Plan\n\n..." } } }
{ "type": "event_msg", "payload": { "type": "task_complete", "turn_id": "turn-1", "last_agent_message": "Codex says:" } }
```

The model's response wraps the plan in `<proposed_plan>…</proposed_plan>`, which `last_agent_message` excludes. After the turn the TUI asks "Implement this plan?" with three rows (`tui/src/chatwidget/plan_implementation.rs`): "Yes, implement this plan" switches to Default mode and submits the prompt `Implement the plan.` "Yes, clear context and implement" starts a fresh thread seeded with the plan. "No, stay in Plan mode" continues planning. The prompt is client-side and emits no lifecycle hook or external `notify` event ([openai/codex#19921](https://github.com/openai/codex/issues/19921)), and requests a terminal `plan-mode-prompt` notification. Blocking questionnaires use the same notification type. Async questions use `async-question` ([notification types](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/chatwidget/notifications.rs)).

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

`request_user_input_async` is the non-blocking variant: its input is `{ "questions": [ { "title", "options"?: [string] } ] }`, it returns `{"accepted":true}` at once, and the questions appear as an `AgentMessage` item with `delivery: "async"` and `questions` that the user answers inline while the turn continues ([handler](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/tools/handlers/request_user_input_async.rs)). The [historical observations](#async-questions-observed-on-01580) distinguish current editor changes from 0.158.0 captures.

## Auth and usage endpoints

### `auth.json`

`cli_auth_credentials_store` selects where Codex keeps credentials: `file` (default), `keyring`, `auto`, or `ephemeral`, which keeps them in memory for the current process (`config/src/types.rs`). With `file`, credentials are in `$CODEX_HOME/auth.json` (`login/src/auth/storage.rs`):

| Key | Meaning |
| --- | --- |
| `OPENAI_API_KEY` | API-key login when non-empty. |
| `tokens.access_token` | ChatGPT OAuth access token. The bearer token for the usage endpoints below. |
| `tokens.account_id` | ChatGPT account id, sent as the `ChatGPT-Account-Id` header. |

Keyring and ephemeral credentials need not exist in `auth.json`. `codex login status` still reports the login kind.

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

Every `x-*-primary-used-percent` header names one limit id, so `parse_all_rate_limits` returns one snapshot per limit present. It also returns the default `codex` snapshot even with empty headers: `limit_id: "codex"`, with the other fields null ([`codex-api/src/rate_limits.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/codex-api/src/rate_limits.rs)). The session's latest snapshot reaches rollouts as `token_count.rate_limits` and app-server clients as `account/rateLimits/updated`.

The snapshot carries nullable `plan_type`, `primary`, `secondary`, and `credits`. A window carries `used_percent`, `window_minutes`, and `resets_at`; credits carry `has_credits`, `unlimited`, and nullable `balance` ([`RateLimitSnapshot`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/protocol/src/protocol.rs)). Only WebSocket `codex.rate_limits` messages set `plan_type`; HTTP header parsing writes it as null ([`parse_rate_limit_event` and `parse_rate_limit_for_limit`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/codex-api/src/rate_limits.rs)). Before publication, `merge_rate_limit_fields` carries missing `plan_type` and `credits` forward from the prior session snapshot, but not the windows or `rate_limit_reached_type` ([`core/src/state/session.rs`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/state/session.rs)). A null `plan_type` therefore does not prove API-key authentication: a ChatGPT session with only HTTP readings can also lack it.

### ChatGPT usage endpoints

Codex's backend client reads account usage over HTTPS with the OAuth bearer token and, when known, the `ChatGPT-Account-Id` header. The base URL is `chatgpt_base_url` from config, default `https://chatgpt.com/backend-api/` (`core/src/config/mod.rs`), with trailing slashes trimmed. A `https://chatgpt.com` or `https://chat.openai.com` base without `/backend-api` gets it appended, and the path style follows from whether the base contains `/backend-api` (`PathStyle::from_base_url`, `backend-client/src/client.rs`):

| Call | Base contains `/backend-api` | Other bases |
| --- | --- | --- |
| Usage, `GET` | `/wham/usage` | `/api/codex/usage` |
| Reset credits, `GET` | `/wham/rate-limit-reset-credits` | `/api/codex/rate-limit-reset-credits` |
| Consume a reset credit, `POST` | `/wham/rate-limit-reset-credits/consume` | `/api/codex/rate-limit-reset-credits/consume` |

The usage response is snake case (`RateLimitStatusPayload` and `RateLimitStatusWithResetCredits`):

```jsonc
{
  "plan_type": "plus",                    // required. guest, free, go, plus, pro, prolite, promax, free_workspace, team, business, education, edu, enterprise, k12, quorum, … unknown
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

Windows carry `limit_window_seconds`. Primary and secondary order carries no meaning beyond position. Codex converts this payload into the app-server `account/rateLimits/read` shape: the main bucket gets `limitId: "codex"`, each additional limit becomes a further bucket, and `rate_limit.allowed` becomes `ordinaryUsageAllowed`.

The reset-credit list is `{ "available_count", "credits": [ { "id", "reset_type", "status", "granted_at", "expires_at"?, "title"?, "description"? } ] }`, with the timestamps as strings (`RateLimitResetCreditDetails`, `types.rs`). The consume request is `{ "redeem_request_id": "<id>", "credit_id": "<optional id>" }`, and the response is `{ "code", "windows_reset" }` with `code` one of `reset`, `nothing_to_reset`, `no_credit`, or `already_redeemed` (`types.rs`). `nothing_to_reset` leaves the credit available.

Account analytics use the same authentication and path prefix, `/wham` or `/api/codex` ([client](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/backend-client/src/client/analytics.rs)). Each route below uses `GET`. Except for credit events, requests include inclusive UTC `start_date` and `end_date`.

| Report | Route after the prefix | Other query fields |
| --- | --- | --- |
| Token usage | `/usage/daily-token-usage-breakdown` | `group_by=day` |
| Credit events | `/usage/credit-usage-events` | No date query |
| Workspace token usage | `/usage/daily-workspace-user-token-usage-breakdown` | `group_by=day`. Enterprise token reports also send `breakdown_by=model` and repeated `modes=codex`, `modes=work`. |
| Enterprise credit usage | `/usage/daily-workspace-user-credit-usage` | `breakdown` |
| Workspace messages | `/analytics/daily-workspace-usage-counts` | `group_by=day`, `workspace_user=true` |
| Plugin activity | `/analytics/daily-plugin-usage-metrics` | `group_by=day`, `workspace_user=true`, `top_plugin_limit` |
| Skill activity | `/analytics/daily-skill-usage-metrics` | `group_by=day`, `workspace_user=true`, `top_skill_limit` |

Responses use [report-specific types](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/codex-backend-openapi-models/src/models/analytics.rs). Missing amounts remain unavailable.

Thread costs use `POST /wham/usage/thread_usage/query`, or `/api/codex/usage/thread_usage/query`, with `{ "thread_ids": ["<id>"] }`. The [client](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/backend-client/src/client/thread_usage.rs) accepts 1 through 100 distinct ids. The response contains `threads[]` with `thread_id`, `estimated_usage_credits_micros`, optional estimated and native USD amounts, and optional model/effort/speed groups. Amounts are integer millionths of credits or USD. Omitted rows and missing credit estimates mean unavailable usage, not zero.

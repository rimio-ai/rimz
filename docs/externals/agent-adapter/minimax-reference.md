# MiniMax Code protocol reference

MiniMax Code is a terminal coding agent invoked as `mcode`. This page records its upstream integration surfaces for adapter authors. It makes no claim about RimZ support.

## Evidence baseline

Source claims use [MiniMax-AI/minimax-code at `9a935605956c4500e9cb415f29258e2887e97817`](https://github.com/MiniMax-AI/minimax-code/tree/9a935605956c4500e9cb415f29258e2887e97817), read on 2026-10-15. Runtime-source links below pin that commit.

| Evidence | Scope |
| --- | --- |
| Source snapshot | Root and TUI manifests say `0.6.5`. The README still calls it a `0.6.3` source preview. |
| Public documentation | Official CLI pages, read on 2026-10-15, are rolling and unversioned. They can describe a different release. |
| Published package | `@minimax-ai/code` provides `mcode`. No published npm artifact was inspected for this reference. |
| Live behavior | No provider session, login, paid probe, or installation was run. Source dispatch establishes implementation evidence only. |

The version discrepancy appears in the [root manifest](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/package.json), [TUI manifest](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/package.json), and [README](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/README.md). Matching version labels do not establish source-to-npm parity.

| Official source | What it owns |
| --- | --- |
| [Features and configuration](https://agent.minimax.io/docs/cli/features) | Public launch modes, slash commands, permissions, providers, plugins, and ACP. |
| [Troubleshooting](https://agent.minimax.io/docs/cli/faq) | Public configuration paths, resume forms, and headless failures. |
| [Hook messages](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/hooks.md) | Command-hook registration and TUI notice behavior. |
| [Native plugin format](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/plugins/package-format-v1.md) | MiniMax plugin package layout and hook declarations. |

The separate [MiniMax-Code-Plugins hook proposal](https://github.com/MiniMax-AI/MiniMax-Code-Plugins/blob/main/proposals/hooks-detailed-spec.md) describes a design target. Its event lists, manifest paths, and timeout defaults do not establish runtime behavior for this snapshot.

## Launch and resume

The public entry points are `mcode [prompt]` for the TUI, `mcode exec [prompt]` for headless execution, and `mcode acp` for ACP. ACP reserves stdin and stdout for protocol traffic. Diagnostics use stderr. Its full request and notification schema is outside this reference's verified subset. [Features and configuration](https://agent.minimax.io/docs/cli/features).

| Surface | Source-verified launch controls |
| --- | --- |
| TUI initial prompt | One positional `[prompt]`. No registered `-p` or `--prompt`. Change directory before launch to select the workspace. |
| TUI resume | `--session <id>` opens a session. Bare `--session` opens the picker. `-c` or `--continue` selects the latest workspace session. |
| TUI resume alias | Hidden `--resume <id>`. Resume selectors cannot combine. |
| TUI model | `-m` or `--model <provider/model[#variant]>` selects the session model. An untargeted session picker cannot combine with `--model`. |
| Headless input | Positional `[prompt]`, or `--input -`. They cannot combine. Stdin is read only with `--input -`. |
| Headless JSON input | `--input-format json` accepts a JSON string or an object with a string `prompt`. Default input format is `text`. |
| Headless workspace and resume | `--cwd <path>`, `--session <id>`, or `--continue`. No exec `--resume` alias. |
| Headless model and effort | `--model <provider/model[#variant]>`, `--effort <level>`. These apply to the run. A model variant is distinct from effort. |
| Headless limits and output | `--timeout <duration>`, `--max-steps <count>`, `--output-format text\|json\|stream-json`. Default output is `text`. |

The [CLI declarations](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/contract.ts), [invocation parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/invocation.ts), and [model selection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/model-selection.ts) own these forms. Headless resume rejects archived sessions, internal subagent sessions, and sessions from another workspace. [Session selection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/runner.ts).

Both TUI and exec accept `--system-prompt <text>`, `--system-prompt-file <path>`, `--append-system-prompt <text>`, and `--append-system-prompt-file <path>`. Inline and file forms conflict within each replacement or append slot. Prompt files resolve against the process directory, not exec `--cwd`. Replacement retains runtime rules and global and project instructions. [Prompt options](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/system-prompt-options.ts), [prompt composition](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/preparation/config/local-agent-config-builder.ts).

### Permission vocabularies

The TUI labels are `ask`, `auto`, and `full`. The configuration parser accepts `default`, `auto`, `bypassPermissions`, and `off`, with default `auto`. Headless uses a third vocabulary:

| Exec `--permission` | Runtime configuration mode |
| --- | --- |
| `smart` (default) | `auto` |
| `full` | `bypassPermissions` |
| `off` | `off` |
| `ask` | Rejected because headless has no interactive host. |

These names belong to different surfaces. The [public TUI labels](https://agent.minimax.io/docs/cli/features) differ from the [configuration parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/config/src/config.ts). The [exec translation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/run-exec-command.ts) establishes the table's mapping. The TUI registers no permission flag. Plan Mode is separate from permission mode. [CLI declarations](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/contract.ts), [public mode distinction](https://agent.minimax.io/docs/cli/faq).

Headless cannot wait for permission prompts or questionnaires. A session with a pending interaction requires the TUI or ACP before headless can continue. [Headless limits](https://agent.minimax.io/docs/cli/faq).

### Headless result and exit codes

`json` emits an `ExecResult`. `stream-json` emits newline-delimited events. A pre-run invocation or runtime-creation failure can write stderr without a JSON result. [Result contract](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/contract.ts), [command error handling](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/run-exec-command.ts).

| Result fields | Contents |
| --- | --- |
| `schemaVersion`, `type` | `1`, `exec.result`. |
| `runId`, `sessionId`, `turnId`, `durationMs` | Required identity and duration fields. |
| `status` | `succeeded`, `failed`, `timeout`, `cancelled`, or `limit_exceeded`. |
| `output`, `error` | Optional answer and failure detail. Error has `category`, `message`, optional `code` and `retryable`. |
| `model`, `usage` | Optional model identity and token buckets. |
| `usageSource`, `usageIncomplete` | Optional provenance (`completed_responses`, `analytics_fallback`, or `unavailable`) and completeness flag. |

Stream events share `schemaVersion`, `sequence`, `timestampMs`, `runId`, `sessionId`, `turnId`, and `type`. Types are `exec.started`, `session.started`, `session.resumed`, `turn.started`, `item.started`, `item.updated`, `item.completed`, `turn.completed`, `turn.failed`, and `exec.completed`. The final `exec.completed` carries `result`. [Event contract](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/events.ts).

| Exec exit | Meaning |
| --- | --- |
| `0` | Success. |
| `2` | Invocation or input error. |
| `3` | Configuration error. |
| `4` | Runtime or task failure. |
| `6` | Timeout. |
| `7` | Runtime limit exceeded. |
| `70` | Internal error. |
| `130` | Cancellation. |
| `141` | Broken pipe. |

The [exec exit policy](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/exit-policy.ts) owns these codes. They are separate from hook-command exit semantics.

## Configuration and authentication

Data-directory precedence is nonempty `MINIMAX_DATA_DIR`, then `MAVIS_DATA_DIR`, then `~/.minimax`. If the configuration layer selects a profile, the default becomes `~/.minimax-<profile>`. The TUI declares no data-directory or profile flag. [TUI directory resolution](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/runtime/data-dir.ts), [default path](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/config/src/data-dir.ts), [CLI declarations](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/contract.ts).

Configuration defaults to `<dataDir>/config.yaml`. Exec `--config <path>` selects another configuration file without relocating auth or session data. Exec permission policy overrides the file's permission mode. [Public configuration path](https://agent.minimax.io/docs/cli/faq), [exec runtime construction](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/run-exec-command.ts).

The public CLI supports MiniMax Token Plan, MiniMax API Key, and custom Anthropic-compatible or OpenAI-compatible providers. `mcode login` manages MiniMax OAuth. `mcode provider use token-plan` and `mcode provider use api-key` select the MiniMax credential source. [Features and configuration](https://agent.minimax.io/docs/cli/features).

`mcode login --region global` selects Global accounts. `mcode provider set-minimax-key` reads `MCODE_PROVIDER_API_KEY` by default. `--api-key-env <name>` selects another variable. These commands change provider state and were not run for this reference. [Authentication documentation](https://agent.minimax.io/docs/cli/faq).

OAuth uses file-backed credential storage beneath `<dataDir>/auth/<buildEnv>/<region>/<clientId>/`. The namespace contains `auth-state.json`, `auth.json`, and `auth.lock`. CLI auth projections also exist under `<dataDir>/cli-auth/`, with shared-state reconciliation. Changing one file is not a verified account-isolation procedure. [OAuth namespace](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/oauth-core/src/namespace.ts), [credential factory](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/oauth-core/src/credential-store/factory.ts), [CLI auth projection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/auth/storage.ts).

## Command-hook registration

The native manifest is `.minimax-plugin/plugin.json`, with `schemaVersion: 1`. Its `hooks` array references package-relative JSON files. The local package scanner reads `<dataDir>/plugins`. Only enabled hook contributions belong in the active runtime snapshot. [Native package reader](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/plugin-system/plugin/package/minimax-reader.ts), [package scanner](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/plugin-system/plugin/runtime/package-storage.ts).

The root `plugin.json` Agent Plugins V1 format has a separate reader, which returns `hooks: []`. A manifest accepted in that format does not establish command-hook installation. [Agent Plugins V1 reader](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/plugin-system/plugin/package/agent-plugin-reader.ts).

This registration fragment assumes an existing native plugin with `scripts/observe.cjs`. Its manifest references this file through `hooks`. It is a schema example, not a complete installable plugin:

```json
{
  "hooks": {
    "Stop": [{
      "hooks": [{
        "type": "command",
        "command": "node \"${PLUGIN_ROOT}/scripts/observe.cjs\"",
        "timeout": 5
      }]
    }]
  }
}
```

The shape follows the upstream [registration example](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/hooks.md). An observer consumes stdin, writes nothing to stdout, and exits `0`. The [response rules](#hook-responses) explain why silence matters.

| Native handler field or limit | Meaning |
| --- | --- |
| `type` | Only `command` executes. |
| `command` | Shell command. Windows can override it with `commandWindows` or `command_windows`. |
| `timeout` | Integer seconds, `1` through `10`. Default `5`. |
| `async` | `true` is rejected. Hooks execute synchronously with their event. |
| `args`, `shell`, `if`, `asyncRewake` | Compatibility-format extensions. The native parser rejects these fields. |
| Handler count | At most 64 executable handlers across a package's hook documents. |
| Event budget | 15 seconds normally, 3 seconds for `SessionEnd`. The remaining event budget can shorten a handler's timeout. |
| Input and output | Input limit is 1 MiB. Each captured output stream has a 64 KiB limit. |

The [parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/parser.ts) validates declarations. The [runner](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts) applies execution budgets and output limits. Multiple handlers can execute concurrently. Synchronous hooks do not imply serial handler execution.

Matchers use case-sensitive native tool names such as `bash`, `read`, `write`, `edit`, and `task`. Missing matchers and `*` match all. `UserPromptSubmit` and `Stop` ignore the matcher after validation. Public package guidance limits matchers to exact names or alternatives. The source parser also accepts restricted regular expressions, which exceeds that documented contract. [Package format](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/plugins/package-format-v1.md), [parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/parser.ts), [matching implementation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

## Hook input and environment

The runner starts each command in the current workspace directory with piped stdin, stdout, and stderr. It sends one JSON object on stdin and closes stdin. Native input uses these common fields:

| Field | Contents |
| --- | --- |
| `hook_event_name` | Registered event name. |
| `session_id` | Current session identity. Child events can identify the child session. |
| `turn_id` | Turn identity, when supplied. Omitted on SessionStart and SessionEnd. |
| `cwd` | Workspace directory. |
| `transcript_path` | Hook transcript projection path, or `null` when unavailable. See [Stored history](#stored-history). |
| `model` | Present when supplied, except on SessionEnd. |
| `permission_mode` | When supplied, except on SessionEnd and compact events. `off` projects to `bypassPermissions`. |

The runner projects internal event payloads into format-specific fields before serialization. Native input omits `prompt_id` and `effort`, although the shared serializer supports them for compatibility formats. Common fields override payload keys with the same name. [Input projection, serializer, and process launch](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

The runner inherits a fixed environment allowlist, then adds plugin roots, plugin data directories, and project directories. `PLUGIN_ROOT` and `MINIMAX_PLUGIN_ROOT` identify the package. `PLUGIN_DATA` identifies writable plugin state. `MINIMAX_PROJECT_DIR` identifies the workspace. The allowlist excludes `RIMZ_*`, `MINIMAX_DATA_DIR`, and arbitrary parent variables. Hook children therefore do not automatically inherit RimZ room pins. [Environment construction](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

## Hook events

The command-hook registry contains these eleven events. Protocol enums and internal event buses are separate surfaces. The registry contains no `Notification`, `StopFailure`, or permission-resolved command hook. [Event registry](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/contracts.ts).

| Event | Event-specific payload and boundary |
| --- | --- |
| `SessionStart` | `source`: `startup`, `resume`, `clear`, `compact`, or `fork`. Plugin activation projects to `startup`. |
| `SessionEnd` | `reason`: `clear`, `logout`, `resume`, or `other`. Archive and idle timeout project to `other`. |
| `UserPromptSubmit` | `prompt`. A prompt-admission boundary, not proof that the model has started. |
| `PreToolUse` | `tool_name`, `tool_input`, `tool_use_id`. Before tool execution. |
| `PermissionRequest` | `tool_name`, `tool_input`. Before the final approval decision. Native projection drops the internal tool-use ID and reason. |
| `PostToolUse` | Tool fields plus `tool_response`, projected from the internal tool result. No separate native `is_error` or `duration_ms` field. |
| `SubagentStart` | `agent_id`, `agent_type`. The native session and turn fields identify the child. |
| `SubagentStop` | Child fields plus `agent_transcript_path`, `stop_hook_active`, and `last_assistant_message`. Normal child close attempt. |
| `Stop` | `stop_hook_active`, `last_assistant_message`. Normal root close attempt. |
| `PreCompact` | `trigger` (`auto` or `manual`). Native projection omits `custom_instructions`. |
| `PostCompact` | `trigger`. Native projection omits `compact_summary`. |

The table describes native stdin after [wire projection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts). Internal payload construction lives in the [lifecycle coordinator](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/coordinator.ts), [tool lifecycle](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/execution/plugin-hook-tool-lifecycle.ts), and [permission hook](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/assembly/local-turn-execution-preparation.ts).

## Hook responses

For neutral observation, exit `0` with empty stdout. `PermissionRequest` then produces internal `permissionDecision: "abstain"`, which preserves normal approval handling. Its default internal `decision: "allow"` means the hook does not block. It does not authorize the tool. [Default decisions and output parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

Plain stdout on `SessionStart` or `UserPromptSubmit` can enter model context. For structured output, write one JSON object. Unknown keys or mismatched `hookSpecificOutput.hookEventName` values invalidate the response. Diagnostics belong on stderr.

| Response | Native effect |
| --- | --- |
| `continue: false`, optional `stopReason` | Requests termination of the active run. |
| `systemMessage` | User-facing notice where the runtime supports notices. It does not add model context by itself. |
| `hookSpecificOutput.additionalContext` | Adds context on supported events. On `Stop` or `SubagentStop`, it requests continuation. |
| `decision: "block"`, `reason` on `UserPromptSubmit` | Rejects the prompt. |
| `hookSpecificOutput.permissionDecision` on `PreToolUse` | `allow`, `deny`, `ask`, or `defer`. The same object can carry `permissionDecisionReason` and `updatedInput`. |
| `hookSpecificOutput.decision.behavior` on `PermissionRequest` | Explicit `allow` or `deny`. Allow can bypass eligible ordinary prompts. Deny can carry `message` and `interrupt`. |
| `decision: "block"`, `reason` on `Stop` or `SubagentStop` | Requests another model step using the reason as continuation input. |
| `decision: "block"`, `reason` on `PreCompact` | Defers compaction. |

These are event-specific shapes, not interchangeable decision envelopes. A `hookSpecificOutput` object must include the matching `hookEventName`. Native `PermissionRequest` rejects `updatedPermissions`. [Response validation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

Timeouts, process errors, and invalid output produce diagnostics and fail open. Native exit code `2` has no special blocking meaning. The runner can still honor valid structured stdout from a nonzero native command exit. Do not use exit status as the native decision channel. [Command result handling](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

TUI `systemMessage` notices persist in display history but stay outside canonical model history, compaction input, and default Markdown export. PreCompact and manual PostCompact do not currently display these notices. SessionEnd notice delivery before exit is best-effort. [Hook messages](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/hooks.md).

## Lifecycle limits

`Stop` establishes a normal close attempt. Another handler can supply continuation input, so the event alone does not establish terminal success. The executor allows up to eight hook continuations. Failed and cancelled turns skip normal Stop. [Close handling](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/execution/user-input-control.ts), [failure and abort guards](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-core/src/pi-turn-runner/events.ts).

`PermissionRequest` establishes an approval attempt. The permission gate runs command hooks before choosing between explicit hook approval and native prompting. Another hook can approve the request without showing the UI. The actual `permission.ask` and `permission.resolved` events belong to an internal event bus. Pending requests live in process-local maps. No stable external subscription or durable waiting-state contract was verified. [Permission gate](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/runner/policy/local-turn-permission-gate.ts), [approval service](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime/src/api/local-permission-approval-service.ts).

`SessionEnd` establishes logical session closure. The coordinator supports archive, clear, logout, session switching, and a 30-minute idle rule. It does not establish per-turn completion or guaranteed process-exit notification. [Session coordinator](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/coordinator.ts), [delivery limits](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/hooks.md).

## Sessions, transcripts, and usage

### Stored history

Default session IDs are `mvs_` followed by a UUID with hyphens removed. Session metadata lives in `local_runtime_sessions` within `<dataDir>/v2/sqlite/runtime-state.sqlite`. [Identity creation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/sessions/lifecycle/record-service.ts), [session schema](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/schema/sessions.ts), [database path](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/client.ts).

Canonical history is `<dataDir>/v2/sessions/<historyRelativeDir>/messages.jsonl`. Resolve the directory from the session record's `historyRelativeDir`. New paths use UTC date and time plus an encoded session ID. Siblings include `manifest.json`, `snapshots/`, and `reports/`. Display history is separate, in SQLite `local_runtime_message_rows`. These are private implementation layouts. [History paths](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/messages/history/session-history-paths.ts), [display schema](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/schema/messages.ts).

Each canonical JSONL envelope requires `message_id`, `turn_id`, and `message`. Optional fields are `turn_config` and `history_artifact`. Only user messages can carry `turn_config`. The nested message schema requires separate parser work before an adapter can consume it. [Envelope decoder](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/file/canonical-history-jsonl.ts).

Hook `transcript_path` can identify a temporary compatibility projection instead of canonical history. The executor substitutes this path, or `null` if projection creation fails. Projection cleanup deletes its files. A hook path therefore does not establish durable transcript location. [Executor projection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/execution/executor.ts), [projection cleanup](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-tools/src/plugin-hooks/transcript.ts).

### Durable turn outcomes

The private SQLite store includes `local_runtime_turn_ingress`. Its `turn_id` is the primary key, and `session_id` identifies the owning session. Status is `accepted`, `completed`, `failed`, or `aborted`. Settlement writes the outcome and `completed_at_ms` under session-owner checks. [Turn schema](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/schema/turn.ts), [settlement transaction](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/persistence/turn.repository.ts).

These records provide implementation evidence for terminal outcomes beyond Stop. They are private storage, with no verified external compatibility or complete crash-recovery guarantee. They do not establish native waiting state.

The session-status vocabulary is `idle`, `started`, `error`, `aborted`, and `interrupted`. A separate continuation inspector recognizes an `ask_user` tool-result tail or `details.waiting_for_user: true`. That narrower signal does not establish general permission waiting. Recovery marks selected stale sessions `interrupted` with `errorSource: "crash-recovery"`. No crash test was run. [Session statuses](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/sessions/repo/contract.ts), [continuation inspector](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/history/continuation-history.ts), [recovery service](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/sessions/recovery/recovery-service.ts).

### Usage and cost

Private `local_runtime_token_usage` records session, turn, model, timestamp, input, output, reasoning, cache-read, and cache-write tokens. `cost_usd` is nullable. Pi accounting takes a finite `usage.cost.total` as dollars and otherwise records `null`. Missing or all-zero token usage produces no row, and projection failures are caught. [Usage schema](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/schema/usage.ts), [usage projection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/usage/pi-usage.ts).

The private summary adds input, output, and reasoning for `totalTokens`. It excludes cache buckets and counts usage rows as `turns`. Its dollar sum converts missing costs to zero, so a zero aggregate does not prove free usage. Exec usage has its own optional buckets and completeness fields. [Private aggregation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/usage/repo/drizzle.ts), [exec usage contract](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/contract.ts).

## Compaction and child agents

The public manual command is `/compact [instructions]`. Native hooks expose PreCompact and PostCompact. Automatic compaction emits PostCompact and then SessionStart with `source: "compact"`. Manual compaction marks the session for a later compact-source SessionStart. [Slash commands](https://agent.minimax.io/docs/cli/features), [automatic lifecycle](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/execution/plugin-hook-compaction-lifecycle.ts), [manual executor](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/compaction/agent-host-compaction-executor.ts).

Compaction preserves the session identity. It snapshots the previous canonical history before replacing active history and recording generation lineage. Compaction `totalTokens` includes cache buckets, unlike the private ordinary-usage summary. [History replacement](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/messages/history/canonical-history-provider.ts), [compaction usage](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/compaction/token-usage.ts).

The coordinator registers parent session and turn identities alongside child session, turn, agent, and transcript identities. Native SubagentStart and SubagentStop serialize the child session identity. Inherited child events add `agent_id` and `agent_type`. Normal child closure uses SubagentStop instead of root Stop. The command-hook wire does not serialize the entire internal parent-child registration. [Child registration and dispatch](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/coordinator.ts), [registration contract](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/contracts.ts).

## Remaining verification

| Surface | Unverified boundary |
| --- | --- |
| Published releases | Source parity with a particular npm artifact and behavior in a live provider session. |
| Initial prompt | Live TUI submission, prompts beginning with `-`, and subcommand-name collisions. |
| Attention | Stable external access to actual ask and resolved events, including question prompts. |
| Completion | Outcome reconciliation after process death, forced termination, or partial writes. |
| Transcript | Real-session ordering, origin classification for injected hook context, and crash-tail handling. |
| Usage and account | Live token accounting, dollar semantics, account isolation, and quota-response contracts. |
| Child agents | Complete parent-child lineage on the external wire and terminal outcomes after child failure. |
| ACP | Full wire schemas, reconnect semantics, and permission-answer transport. |

These gaps require separate verification before an adapter can promise the corresponding behavior. Third-party storage reports and plugin proposals do not close them.

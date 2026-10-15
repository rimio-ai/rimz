# MiniMax Code protocol reference

MiniMax Code is a terminal coding agent invoked as `mcode`. This page records its upstream integration surfaces for adapter authors. It makes no claim about RimZ support.

## Evidence baseline

Source claims use [MiniMax-AI/minimax-code at `9a935605956c4500e9cb415f29258e2887e97817`](https://github.com/MiniMax-AI/minimax-code/tree/9a935605956c4500e9cb415f29258e2887e97817), read on 2026-10-15. Runtime-source links below pin that commit. Citations to `agent.minimax.io` describe rolling public documentation. Commit-pinned GitHub citations describe source implementation, without a stable external API guarantee.

| Evidence | Scope |
| --- | --- |
| Source snapshot | Root and TUI manifests say `0.6.5`. The README still calls it a `0.6.3` source preview. |
| Public documentation | Official CLI pages, read on 2026-10-15, are rolling and unversioned. They can describe a different release. |
| Published package | [The upstream README](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/README.md) names `@minimax-ai/code` and `mcode`. No published npm artifact was inspected. |
| Live behavior | No provider session, login, paid probe, or installation was run. Reading event-dispatch code establishes implementation evidence only. |

Baseline evidence: the [root manifest](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/package.json), [TUI manifest](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/package.json), and [README](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/README.md). Matching version labels do not establish source-to-npm parity.

| Official source | What it owns |
| --- | --- |
| [Features and configuration](https://agent.minimax.io/docs/cli/features) | Public launch modes, slash commands, permissions, providers, plugins, and ACP. |
| [Troubleshooting](https://agent.minimax.io/docs/cli/faq) | Public configuration paths, resume forms, and headless failures. |
| [Hook messages](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/hooks.md) | Command-hook registration and TUI notice behavior. |
| [Native plugin format](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/plugins/package-format-v1.md) | MiniMax plugin package layout and hook declarations. |

The separate [MiniMax-Code-Plugins hook proposal](https://github.com/MiniMax-AI/MiniMax-Code-Plugins/blob/6481e4ae1e0375a4ddb3d5e9b29845354ab1cead/proposals/hooks-detailed-spec.md) describes a design target. Its event lists, manifest paths, and timeout defaults do not establish runtime behavior for this snapshot.

## Launch and resume

The public documentation lists these entry points: `mcode [prompt]` for the TUI, `mcode exec [prompt]` for headless execution, and `mcode acp` for ACP. ACP reserves stdin and stdout for protocol traffic. Diagnostics use stderr. Its full request and notification schema is outside this reference's verified subset. [Features and configuration](https://agent.minimax.io/docs/cli/features).

| Surface | Source-verified launch controls |
| --- | --- |
| TUI initial prompt | One positional `[prompt]`. No registered `-p` or `--prompt`. Change directory before launch to select the workspace. |
| TUI resume | `--session <id>` opens a session. Bare `--session` opens the picker. `-c` or `--continue` selects the latest workspace session. |
| TUI resume alias | Hidden `--resume <id>`. `--session`, `--continue`, and `--resume` cannot combine. |
| TUI model | `-m` or `--model <provider/model[#variant]>` selects the session model. An untargeted session picker cannot combine with `--model`. |
| Headless input | Positional `[prompt]`, or `--input -`. They cannot combine. Stdin is read only with `--input -`. |
| Headless JSON input | `--input-format json` accepts a JSON string or an object with a string `prompt`. Default input format is `text`. |
| Headless workspace and resume | `--cwd <path>` selects the workspace. `--session <id>` selects a session. `--continue` selects the latest active session in that workspace. No exec `--resume` alias. |
| Headless model and effort | `--model <provider/model[#variant]>`, `--effort <level>`. Effort must match the model's options. `#variant` identifies a provider-defined model configuration, separately from effort. |
| Headless limits and output | `--timeout <duration>` (such as `30s` or `2m`), `--max-steps <count>`, `--output-format text\|json\|stream-json`. Default output is `text`. |

The [CLI declarations](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/contract.ts), [invocation parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/invocation.ts), and [model selection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/model-selection.ts) own these forms. Headless resume rejects archived sessions, internal subagent sessions, and sessions from another workspace. [Session selection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/runner.ts).

Both TUI and exec accept `--system-prompt <text>`, `--system-prompt-file <path>`, `--append-system-prompt <text>`, and `--append-system-prompt-file <path>`. Inline and file forms conflict within each replacement or append slot. Prompt files resolve against the process directory, not exec `--cwd`. Replacement retains runtime rules and global and project instructions. [Prompt options](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/system-prompt-options.ts), [prompt composition](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/preparation/config/local-agent-config-builder.ts).

## Permission modes

The configuration key `permissionMode` accepts `default`, `auto`, `bypassPermissions`, and `off`, with default `auto`. TUI commands and headless flags translate to these values:

| TUI command | Exec `--permission` | Runtime `permissionMode` |
| --- | --- | --- |
| `/permission ask` | `ask` is rejected | `default` |
| `/permission auto` | `smart` (default) | `auto` |
| `/permission full` | `full` | `bypassPermissions` |
| No `/permission off` command | `off` | `off` |

The [TUI command handler](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/tui/controller/product/command-flow.ts), [exec translation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/run-exec-command.ts), and [configuration parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/config/src/config.ts) establish this mapping. The TUI registers neither `--permission` nor `--config`. [CLI declarations](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/contract.ts).

Public documentation treats Plan Mode separately from permission mode. [Mode distinction](https://agent.minimax.io/docs/cli/faq).

Headless cannot wait for permission prompts or questionnaires. A session with a pending interaction requires the TUI or ACP before headless can continue. [Headless limits](https://agent.minimax.io/docs/cli/faq).

## Headless results and exit codes

`json` emits an `ExecResult`. `stream-json` emits newline-delimited events. A pre-run invocation or runtime-creation failure can write stderr without a JSON result. [Result contract](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/contract.ts), [command error handling](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/run-exec-command.ts).

| Result fields | Contents |
| --- | --- |
| `schemaVersion`, `type` | `1`, `exec.result`. |
| `runId`, `sessionId`, `turnId`, `durationMs` | Required identity and duration fields. |
| `status` | `succeeded`, `failed`, `timeout`, `cancelled`, or `limit_exceeded`. |
| `output`, `error` | Optional answer and failure detail. Error has `category`, `message`, optional `code` and `retryable`. |
| `model` | Optional identity, including `providerId`, `modelId`, and optional `variant`. |
| `usage` | Optional `totalTokens`, `inputTokens`, `outputTokens`, `reasoningTokens`, `cacheReadTokens`, and `cacheWriteTokens`. |
| `usageSource`, `usageIncomplete` | Optional provenance (`completed_responses`, `analytics_fallback`, or `unavailable`) and completeness flag. |

Stream events share `schemaVersion`, `sequence`, `timestampMs`, `runId`, `sessionId`, `turnId`, and `type`.

| Stream event | Meaning |
| --- | --- |
| `exec.started`, `session.started` or `session.resumed`, `turn.started` | Introduce the run, session selection, and turn. |
| `item.started`, `item.updated`, `item.completed` | Incremental `agent_message`, `reasoning`, or `tool_call` records. |
| `turn.completed` | Encodes the final successful run result, after execution returns. |
| `turn.failed` | Encodes a final unsuccessful run result, including timeout, cancellation, and limits. |
| `exec.completed` | Carries `result`. The projector rejects subsequent events. |

The terminal events derive from the final ExecResult, separately from tentative Stop hooks. The encoder permits one result per run. [Event projection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/events.ts), [output encoder](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/output.ts), [result construction](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/runner.ts).

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

Data-directory precedence is nonempty `MINIMAX_DATA_DIR`, then `MAVIS_DATA_DIR`, then `~/.minimax`. The source path helper also supports `~/.minimax-<profile>` for named profiles. Profile selection is outside the verified launch surface. `MAVIS_DATA_DIR` is the legacy compatibility variable. The TUI declares no data-directory or profile flag. [TUI directory resolution](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/runtime/data-dir.ts), [default path](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/config/src/data-dir.ts), [CLI declarations](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/contract.ts).

Public documentation places configuration at `<dataDir>/config.yaml`. [Configuration path](https://agent.minimax.io/docs/cli/faq).

Exec `--config <path>` selects another configuration file without relocating auth or session data. Exec always overrides the file's `permissionMode`, including when `--permission` defaults to `smart`. [Exec runtime construction](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/cli/run-exec-command.ts).

The public CLI supports MiniMax Token Plan, MiniMax API Key, and custom Anthropic-compatible or OpenAI-compatible providers. `mcode login` manages MiniMax OAuth. `mcode provider use token-plan` and `mcode provider use api-key` select the MiniMax credential source. [Features and configuration](https://agent.minimax.io/docs/cli/features).

`mcode login --region global` selects Global accounts. `mcode provider set-minimax-key` reads `MCODE_PROVIDER_API_KEY` by default. `--api-key-env <name>` selects another variable. These commands change provider state and were not run for this reference. [Authentication documentation](https://agent.minimax.io/docs/cli/faq).

The private OAuth implementation uses file-backed credential storage beneath `<dataDir>/auth/<buildEnv>/<region>/<clientId>/`. The namespace contains `auth-state.json`, `auth.json`, and `auth.lock`. CLI auth projections also exist under `<dataDir>/cli-auth/`, with shared-state reconciliation. Changing one file is not a verified account-isolation procedure. [OAuth namespace](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/oauth-core/src/namespace.ts), [credential factory](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/oauth-core/src/credential-store/factory.ts), [CLI auth projection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/auth/storage.ts).

## Command-hook registration

The shared hook implementation defines native `MINIMAX` handlers and `CLAUDE` and `CODEX` compatibility formats. The hook tables below describe `MINIMAX`. [Format contract](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/contracts.ts).

The native manifest is `.minimax-plugin/plugin.json`, with `schemaVersion: 1`. Its `hooks` array references package-relative JSON files. The local package scanner reads `<dataDir>/plugins`. Only enabled hook contributions belong in the active runtime snapshot. [Native package reader](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/plugin-system/plugin/package/minimax-reader.ts), [package scanner](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/plugin-system/plugin/runtime/package-storage.ts), [snapshot assembly](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/plugin-system/plugin/runtime/snapshot-builder.ts).

The root `plugin.json` Agent Plugins V1 format has a separate reader, which returns `hooks: []`. A manifest accepted in that format does not establish command-hook installation. [Agent Plugins V1 reader](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/plugin-system/plugin/package/agent-plugin-reader.ts).

For an existing plugin, the manifest fragment `"hooks": ["hooks/observe.json"]` references the following `hooks/observe.json` document. The script `scripts/observe.cjs` is relative to the package root, exposed as `PLUGIN_ROOT`. This is a registration example, not a complete plugin:

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

The shape follows the upstream [registration example](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/hooks.md). The [response rules](#hook-responses) explain why silence matters.

The public CLI enables a local plugin with `mcode plugin enable <name>@local`. `disable` reverses activation, and `remove` uninstalls it. [Plugin management](https://agent.minimax.io/docs/cli/features).

| Native handler field or limit | Meaning |
| --- | --- |
| `type` | Only `command` executes. |
| `command` | Shell command. Windows can override it with `commandWindows` or `command_windows`. |
| `timeout` | Integer seconds, `1` through `10`. Default `5`. |
| `async` | `true` is rejected. Hooks execute synchronously with their event. |
| `args`, `shell`, `if`, `asyncRewake` | Compatibility-format extensions. Native handlers with these fields are skipped with a diagnostic. |
| Handler count | At most 64 executable handlers across a package's hook documents. |
| Event budget | 15 seconds normally, 3 seconds for `SessionEnd`. The remaining event budget can shorten a handler's timeout. |
| Input and output | Input limit is 1 MiB. Each captured output stream has a 64 KiB limit. |

The [parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/parser.ts) validates declarations. The [runner](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts) applies execution budgets and output limits. Multiple handlers can execute concurrently. Synchronous hooks do not imply serial handler execution.

Matchers are limited to 256 characters. Tool matchers use case-sensitive native names such as `bash`, `read`, `write`, `edit`, and `task`. Missing matchers and `*` match all. `UserPromptSubmit` and `Stop` ignore the matcher after validation. Public package guidance limits matchers to exact names or alternatives. The source parser also accepts restricted regular expressions, which exceeds that documented contract. [Package format](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/plugins/package-format-v1.md), [parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/parser.ts), [matching implementation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

| Event matcher | Matched value |
| --- | --- |
| Tool events | Native tool name. |
| SessionStart, SessionEnd | Projected `source` or `reason`. |
| SubagentStart, SubagentStop | Agent type. |
| PreCompact, PostCompact | `trigger`: `auto` or `manual`. |
| UserPromptSubmit, Stop | Ignored after validation. |

## Hook input and environment

The runner starts each command in the current workspace directory with piped stdin, stdout, and stderr. It sends one JSON object on stdin and closes stdin. Native input uses these common fields:

| Field | Contents |
| --- | --- |
| `hook_event_name` | Registered event name. |
| `session_id` | Current session identity. |
| `turn_id` | Optional runtime turn identity. The traced UserPromptSubmit and Stop paths supply it. Omitted on SessionStart and SessionEnd. |
| `cwd` | Workspace directory. |
| `transcript_path` | Hook transcript projection path, or `null` when unavailable. See [Stored history](#stored-history). |
| `model` | Optional runtime model identity. Omitted on SessionEnd. |
| `permission_mode` | Optional runtime mode. Omitted on SessionEnd and compact events. `off` projects to `bypassPermissions`. |

The runner projects internal event payloads into format-specific fields before serialization. Native input omits `prompt_id` and `effort`, although the shared serializer supports them for compatibility formats. Common fields override payload keys with the same name. Turn-admission and normal-close paths supply the runtime turn ID. [Coordinator](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/coordinator.ts). [Input projection, serializer, and process launch](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

The runner inherits a fixed environment allowlist, then adds plugin roots, plugin data directories, and project directories. `PLUGIN_ROOT` and `MINIMAX_PLUGIN_ROOT` identify the package. `PLUGIN_DATA` identifies writable plugin state selected by the host, separately from the package root. `MINIMAX_PROJECT_DIR` identifies the workspace. The allowlist excludes `RIMZ_*`, `MINIMAX_DATA_DIR`, and arbitrary parent variables. [Environment construction](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

| Inherited environment | Names |
| --- | --- |
| General | `PATH`, `HOME`, `LANG`, `TERM`, `SHELL`, `USER`, `TMPDIR`, `TEMP`, `TMP`. |
| Windows-related | `PATHEXT`, `SystemRoot`, `ComSpec`, `USERPROFILE`, `HOMEDRIVE`, `HOMEPATH`, `APPDATA`, `LOCALAPPDATA`. |

Only values present in the parent environment are inherited.

Without a host-supplied data path, `PLUGIN_DATA` defaults to `<pluginRoot>/../.plugin-data/<name>`. The runner replaces name characters outside `[A-Za-z0-9._-]` with `_` and truncates the result to 80 characters. [Plugin data path](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

## Hook events

The command-hook registry contains these eleven events. Protocol enums and internal event buses are separate surfaces. The registry contains no `Notification`, `StopFailure`, or permission-resolved command hook. [Event registry](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/contracts.ts).

| Event | Event-specific payload and boundary |
| --- | --- |
| `SessionStart` | `source`: `startup`, `resume`, `clear`, `compact`, or `fork`. Plugin activation projects to `startup`. |
| `SessionEnd` | `reason`: `clear`, `logout`, `resume`, or `other`. Archive and idle timeout project to `other`. |
| `UserPromptSubmit` | `prompt`. A prompt-admission boundary, not proof that the model has started. |
| `PreToolUse` | `tool_name`, `tool_input`, `tool_use_id`. Before tool execution. |
| `PermissionRequest` | `tool_name`, `tool_input`. Before the final approval decision. Native projection drops the internal tool-use ID and reason. |
| `PostToolUse` | `tool_name`, `tool_input`, `tool_use_id`, `tool_response`. No separate native `is_error` or `duration_ms` field. |
| `SubagentStart` | `agent_id`, `agent_type`. The native session and turn fields identify the child. |
| `SubagentStop` | `agent_id`, `agent_type`, `agent_transcript_path`, `stop_hook_active`, `last_assistant_message`. Normal child close attempt. |
| `Stop` | `stop_hook_active`, `last_assistant_message`. Normal root close attempt. |
| `PreCompact` | `trigger` (`auto` or `manual`). Native projection omits `custom_instructions`. |
| `PostCompact` | `trigger`. Native projection omits `compact_summary`. |

The table describes native stdin after [wire projection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts). Internal payload construction lives in the [lifecycle coordinator](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/coordinator.ts), [tool lifecycle](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/execution/plugin-hook-tool-lifecycle.ts), and [permission hook](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/assembly/local-turn-execution-preparation.ts).

SessionStart can repeat within a session during plugin activation or compaction. Its `source` field does not establish a unique process launch.

## Hook responses

A neutral observer reads stdin, writes nothing to stdout, and exits `0`. On PermissionRequest, silence abstains and preserves normal approval handling. [Default decisions and output parser](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

Plain stdout on `SessionStart` or `UserPromptSubmit` can enter model context. For structured output, write one JSON object. Unknown keys or mismatched `hookSpecificOutput.hookEventName` values invalidate the response. Diagnostics belong on stderr.

| Response | Native effect |
| --- | --- |
| `continue: false`, optional `stopReason` | Requests termination of the active agent turn. |
| `systemMessage` | User-facing notice where the runtime supports notices. It does not add model context by itself. |
| `hookSpecificOutput.additionalContext` | Adds context on supported events. On `Stop` or `SubagentStop`, it requests continuation. |
| `decision: "block"`, `reason` on `UserPromptSubmit` | Rejects the prompt. |
| `hookSpecificOutput.permissionDecision` on `PreToolUse` | `allow`, `deny`, `ask`, or `defer`. Ask requests approval. Defer blocks the tool because deferred execution is unsupported by this host. |
| `hookSpecificOutput.decision.behavior` on `PermissionRequest` | Explicit `allow` or `deny`. Allow can bypass eligible ordinary prompts. Deny can carry `message` and `interrupt`. |
| `decision: "block"`, `reason` on `Stop` or `SubagentStop` | Requests another model step using the reason as continuation input. |
| `decision: "block"`, `reason` on `PreCompact` | Defers compaction. |

PreToolUse can also carry `permissionDecisionReason` and `updatedInput`. An explicit PermissionRequest allow applies only when policy marks the prompt eligible for hook auto-approval. Forced approval requirements can still apply. [Permission eligibility](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/runner/policy/local-turn-permission-gate.ts).

These are event-specific shapes, not interchangeable decision envelopes. A `hookSpecificOutput` object must include the matching `hookEventName`. Native `PermissionRequest` rejects `updatedPermissions`. [Response validation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

Timeouts, process errors, and invalid output produce diagnostics and fail open. Native exit code `2` has no special blocking meaning. The runner can still honor valid structured stdout from a nonzero native command exit. Do not use exit status as the native decision channel. [Command result handling](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/runner.ts).

TUI `systemMessage` notices persist in display history but stay outside canonical model history, compaction input, and default Markdown export. PreCompact and manual PostCompact do not currently display these notices. SessionEnd notice delivery before exit is best-effort. [Hook messages](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/hooks.md).

## Lifecycle limits

`Stop` establishes a normal close attempt. A handler can supply continuation input, so the event alone does not establish terminal success. The executor allows up to eight hook continuations per turn. Failed and cancelled turns skip normal Stop. A hook-only observer has no verified terminal event covering those outcomes. [Close handling](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/execution/user-input-control.ts), [failure and abort guards](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-core/src/pi-turn-runner/events.ts).

`stop_hook_active` starts as `false` for each turn. After a hook requests continuation, later Stop attempts carry `true`. This field records prior continuation, not whether the current attempt will settle. [Stop state tracking](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/execution/user-input-control.ts).

For terminal outcomes, see [durable turn records](#durable-turn-outcomes) or [headless results](#headless-results-and-exit-codes).

`PermissionRequest` establishes an approval attempt. The permission gate runs command hooks before choosing between explicit hook approval and native prompting. A hook can approve the request without showing the UI. The actual `permission.ask` and `permission.resolved` events belong to an internal event bus. Pending requests live in process-local maps. No stable external subscription or durable waiting-state contract was verified. [Permission gate](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/runner/policy/local-turn-permission-gate.ts), [approval service](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime/src/api/local-permission-approval-service.ts).

Policy denial and direct approval can return before PermissionRequest in the traced gate path. Some side-session approvals call the native approval service directly. Mode alone therefore does not determine hook emission, and this hook is not an exhaustive waiting hint. [Gate paths](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/runner/policy/local-turn-permission-gate.ts).

`SessionEnd` establishes logical session closure. The coordinator supports archive, clear, logout, session switching, and a 30-minute idle rule. It does not establish per-turn completion or guaranteed process-exit notification. [Session coordinator](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/coordinator.ts), [delivery limits](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/docs/hooks.md).

## Sessions, transcripts, and usage

### Stored history

By default, the private session implementation generates IDs as `mvs_` followed by a UUID with hyphens removed. For root sessions, the native hook `session_id` is the ID accepted by `--session`. Session metadata lives in `local_runtime_sessions` within `<dataDir>/v2/sqlite/runtime-state.sqlite`. [Identity creation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/sessions/lifecycle/record-service.ts), [session schema](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/schema/sessions.ts), [database path](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/client.ts).

Canonical history is `<dataDir>/v2/sessions/<historyRelativeDir>/messages.jsonl`. Resolve the directory from the session record's `historyRelativeDir`. New paths use UTC date and time plus an encoded session ID. Siblings include `manifest.json`, `snapshots/`, and `reports/`. Display history is separate, in SQLite `local_runtime_message_rows`. These are private implementation layouts. [History paths](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/messages/history/session-history-paths.ts), [display schema](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/schema/messages.ts).

Each canonical JSONL envelope requires `message_id`, `turn_id`, and `message`. Optional fields are `turn_config` and `history_artifact`. Only user messages can carry `turn_config`. Nested message schemas remain outside this reference's verified subset. [Envelope decoder](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/file/canonical-history-jsonl.ts).

Hook `transcript_path` can identify a temporary compatibility projection instead of canonical history. The executor substitutes this path, or `null` if projection creation fails. Projection cleanup deletes its files. A hook path therefore does not establish durable transcript location. [Executor projection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/execution/executor.ts), [projection cleanup](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-tools/src/plugin-hooks/transcript.ts).

### Durable turn outcomes

The private SQLite store includes `local_runtime_turn_ingress`. Its `turn_id` is the primary key, and `session_id` identifies the owning session. Status is `accepted`, `completed`, `failed`, or `aborted`. Settlement writes the outcome and `completed_at_ms` under session-owner checks. [Turn schema](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/schema/turn.ts), [settlement transaction](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/persistence/turn.repository.ts).

These records provide implementation evidence for terminal outcomes beyond Stop. They are private storage, with no verified external compatibility or complete crash-recovery guarantee. They do not establish native waiting state.

The session-status vocabulary is `idle`, `started`, `error`, `aborted`, and `interrupted`. A separate continuation inspector recognizes an `ask_user` tool-result tail or `details.waiting_for_user: true`. That narrower signal does not establish general permission waiting. Recovery marks selected stale sessions `interrupted` with `errorSource: "crash-recovery"`. No crash test was run. [Session statuses](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/sessions/repo/contract.ts), [continuation inspector](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/history/continuation-history.ts), [recovery service](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/sessions/recovery/recovery-service.ts).

### Usage and cost

Private `local_runtime_token_usage` records session, turn, model, timestamp, input, output, reasoning, cache-read, and cache-write tokens. `cost_usd` is nullable. The usage projection takes a finite `usage.cost.total` as dollars and otherwise records `null`. Missing or all-zero token usage produces no row, and projection failures are caught. [Usage schema](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/infra/db/schema/usage.ts), [usage projection](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/usage/pi-usage.ts).

The private summary adds input, output, and reasoning for `totalTokens`. It excludes cache buckets and counts usage rows as `turns`. Its dollar sum converts missing costs to zero, so a zero aggregate does not prove free usage. Exec usage has its own optional buckets and completeness fields. [Private aggregation](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/usage/repo/drizzle.ts), [exec usage contract](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/tui/src/headless/contract.ts).

## Compaction and child agents

The public manual command is `/compact [instructions]`. Native hooks expose PreCompact and PostCompact. On the normal automatic path, PostCompact precedes SessionStart with `source: "compact"`. Manual compaction runs PreCompact, then PostCompact after history replacement. It marks a compact-source SessionStart for the next turn admission. [Slash commands](https://agent.minimax.io/docs/cli/features), [automatic lifecycle](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/execution/plugin-hook-compaction-lifecycle.ts), [manual executor](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/compaction/agent-host-compaction-executor.ts).

Compaction preserves the session identity. It snapshots the previous canonical history before replacing active history and recording generation lineage. The private compaction usage accumulator's `totalTokens` includes cache buckets, unlike the private ordinary-usage summary. [History replacement](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/session-system/messages/history/canonical-history-provider.ts), [compaction usage](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/local-runtime-v2/src/service/turn-system/agent-host/compaction/token-usage.ts).

The coordinator registers parent session and turn identities alongside child session, turn, agent, and transcript identities. Native SubagentStart and SubagentStop serialize the child session identity. Inherited child tool events include `agent_id` and `agent_type`. Normal child closure uses SubagentStop instead of root Stop. Native hook stdin has no dedicated parent session or parent turn field. The parent-child registration remains internal. [Child registration and dispatch](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/coordinator.ts), [registration contract](https://github.com/MiniMax-AI/minimax-code/blob/9a935605956c4500e9cb415f29258e2887e97817/packages/agent-modules/plugin-hooks/src/contracts.ts).

## Remaining verification

| Surface | Unverified boundary |
| --- | --- |
| Published releases | Source parity with a particular npm artifact and behavior in a live provider session. |
| Launch and resume | Live TUI submission, leading-dash prompts, subcommand collisions, TUI resume eligibility, and profile selection. |
| Attention | Stable external access to actual ask and resolved events, including question prompts. |
| Completion | Outcome reconciliation after process death, forced termination, or partial writes. |
| Transcript | Nested message schemas, real-session ordering, origin classification for injected hook context, and crash-tail handling. |
| Usage and account | Live token accounting, dollar semantics, account isolation, and quota-response contracts. |
| Child agents | Complete parent-child lineage on the external wire and terminal outcomes after child failure. |
| ACP | Full wire schemas, reconnect semantics, and permission-answer transport. |

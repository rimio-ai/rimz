# OMP protocol reference

OMP (Oh My Pi) is a fork of Pi, published as `@oh-my-pi/pi-coding-agent` with the executable `omp`. This page records its upstream integration surface for contributors evaluating an adapter. It does not establish RimZ support or select an integration design. The [adapter playbook](../../contributing/agent-adapters.md#step-2--map-the-protocol-onto-the-model) owns the mapping worksheet for later integration work. The [Pi reference](./pi-reference.md) records Pi's independent contract.

## Verification baseline

Checked 2026-10-15 against official source commit [`13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74`](https://github.com/can1357/oh-my-pi/commit/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74). The pinned [package manifest][package] declares version `18.11.0`. The [npm version metadata](https://registry.npmjs.org/@oh-my-pi/pi-coding-agent/18.11.0) also identifies `18.11.0`, with `omp` mapped to `dist/cli.js` and Bun `>=1.3.14`. Matching version strings do not prove the published bundle matches this commit.

Verification is static: official documentation and source were read, but no OMP executable, authenticated turn, extension, or recorded session was run. All upstream links below pin the inspected commit unless explicitly labeled as registry metadata. This page follows source definitions when upstream prose differs.

## Executable and launch arguments

`omp` and `omp launch` start a session. A registered subcommand takes precedence over an initial prompt word. `omp launch` makes that choice explicit. Outside protocol modes, piped input selects print mode when `--mode` is absent. `--` ends option parsing and makes later arguments literal messages. The table selects adapter-relevant arguments from the [CLI reference][cli] and [settings reference][settings].

| Surface | Arguments or behavior |
| --- | --- |
| Detection | `omp --version` / `-v`, `omp --help`, `omp launch --help` |
| Interactive/print input | Positional messages and `@path` attachments. Stdin, text attachments, and the first positional message form the initial prompt. Later messages become later turns. |
| Workspace | `--cwd <dir>`, repeatable `--add-dir <dir>`. Without `--allow-home`, starting in the home directory can switch to a temporary directory. |
| Resume | `--continue` / `-c` continues the previous session. `--resume [id]` / `-r` / `--session [id]` takes an ID prefix or path, or opens a picker. |
| Recovery | `--restore [id]` also recovers interrupted work. It is interactive-only and accepts no prompt. |
| Fork | `--fork <session>` creates a new session from an ID prefix or path. |
| Storage | `--session-dir <dir>` changes storage and lookup. `--no-session` disables persistence and cannot combine with resume, restore, fork, or imports. |
| Model | `--model <id-or-role>`, `--models <patterns>`, legacy `--provider <name>`, and role overrides `--smol`, `--slow`, `--plan`. |
| Startup planning | `--plan` selects a model, not a permission mode. `plan.defaultOnStartup` enables startup planning for fresh interactive sessions. `--plan-yolo` plans, then auto-approves execution. |
| Thinking | `--thinking off\|minimal\|low\|medium\|high\|xhigh\|max\|auto`. `--hide-thinking` affects display only. |
| System prompt | `--system-prompt <text-or-file>`, `--append-system-prompt <text-or-file>`, or `--system-prompt-template <path>`. The template and replacement flags are mutually exclusive. |
| Extension | Repeatable `--extension` / `-e <path>`, `--hook <path>`, `--no-extensions`, or `--trusted-extension <absolute-file>`. |
| Tool policy | `--tools <names>`, `--no-tools`, `--approval-mode always-ask\|write\|yolo`, `--auto-approve`, `--yolo`. |
| Skills and rules | `--skills <globs>`, `--no-skills`, `--no-rules`. |
| Headless | `--print` / `-p`, `--mode text\|json\|rpc\|rpc-ui\|acp`. Explicit `--mode text` is non-interactive. `--no-ui` disables extension UI in RPC modes. |

With terminal stdin and neither `--mode` nor `--print`, OMP uses the interactive TUI. Process names under Bun and prebuilt binaries were not observed. The package manifest establishes the executable name, not a process-detection rule.

## Configuration roots and extension discovery

The default agent directory is `~/.omp/agent`. The following paths assume the default profile and no overrides. OMP's [configuration reference][config], [environment reference][environment], and [path helpers][dirs] define relocation.

| Input | Resolution |
| --- | --- |
| `--profile <name>` | Selects `~/.omp/profiles/<name>/agent` for native user settings, auth, sessions, and extensions. |
| `OMP_PROFILE`, legacy `PI_PROFILE` | `OMP_PROFILE` wins when defined, including an empty value. Empty, whitespace, and `default` select the default profile. |
| `PI_CONFIG_DIR` | Changes the user config root name, normally `.omp`. |
| `PI_CODING_AGENT_DIR` | Overrides agent-directory consumers in the default profile. Named profiles ignore it. Generic config lookup still uses its own base root. See [configuration lookup][config]. |
| Linux `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | Relocate classified paths under `omp/`. For example, sessions use `$XDG_DATA_HOME/omp/sessions`. Run `omp config migrate` to move existing data before relying on XDG roots. |
| `PI_CODING_AGENT_SESSION_DIR` | Sets the initial session directory. `--session-dir` overrides it. |
| `--config <file>` | Adds a settings overlay. Repeatable. |
| Native settings | User `config.yml`, with `config.yaml` fallback. Project `.omp/config.yml` and legacy `.omp/settings.json` also supply settings. |

Native extension discovery scans `<cwd>/.omp/extensions` and `<agentDir>/extensions`, plus legacy settings entries. The project extension root is cwd-only. It does not walk ancestors. `.pi/extensions` is not a native extension root. See [extension loading][loading] and the [native discovery provider][native].

| Extension source | Contract |
| --- | --- |
| Native directories | Direct `.ts` / `.js` files and one-level extension directories. |
| Hook directories | JS/TS factories under `.omp/hooks/pre/` and `.omp/hooks/post/`, with equivalent agent-directory roots. A file directly under `hooks/` is not discovered. |
| Installed plugins | Enabled package entries from `omp.extensions`, or legacy `pi.extensions`. `omp` takes precedence. |
| Explicit paths | CLI paths, then effective settings `extensions`. Relative paths resolve against cwd. Explicit files also support `.mjs` and `.cjs`. |
| `--no-extensions` | Excludes ambient factories. Explicit `-e` and `--hook` paths still load. It does not disable independent skill, MCP, tool, prompt, or rule discovery. |
| `--trusted-extension` | Loads only named absolute, existing module files. Resolves symlinks, rejects combination with `-e` or `--hook`, and aborts startup on load failure. It grants no package-sibling discovery. |

Project inputs load without a per-directory trust prompt. Extension `ctx.isProjectTrusted()` always returns `true`. Extensions execute in-process with host permissions and are not sandboxed. A default-exported factory receives `ExtensionAPI` and registers handlers through `pi.on(...)`. Bun loads modules through compatibility rewriting for legacy Pi package imports. Those import shims do not establish event or payload equivalence ([loader documentation][loading]).

The normal CLI uses `ExtensionRunner` and `ExtensionToolWrapper`. `--hook` is an extension-path alias. [hooks.md][hooks] also describes a legacy runner whose context, error, and timeout rules differ. Those rules apply to consumers explicitly using the legacy library classes. The default CLI loads discovered JS/TS hook factories through the extension runner.

Skills expose metadata to the model and load content through `read` on `skill://...`. Conventional discovery uses `skills/<name>/SKILL.md`, without recursive grouping. Native skill roots include `.omp/skills` and `<agentDir>/skills`. `--skills` filters names with globs. `--no-skills` disables skill discovery and loading. Skills have independent discovery controls, so `--no-extensions` is insufficient ([skills][skills]).

## Lifecycle and blocking decisions

Extension events, session subscription events, and RPC frames are separate surfaces. The table selects adapter-relevant extension events, not the complete event union. In particular, `session_settled` is an RPC frame, not an extension event in the inspected union. The [RPC settlement predicate](#headless-output-and-rpc) defines when no pending work can restart the session. See [extension types][extension-types], [shared event types][shared-events], [session events][session-events], and [RPC settlement][settle].

| Extension event | Payload or decision |
| --- | --- |
| `session_start` | The event contains only its type. Session identity comes from `ctx.sessionManager`. |
| `session_before_switch`, `session_switch` | `reason: new\|resume\|fork`. Before-switch can cancel and can identify the target session file. After-switch identifies the previous file. |
| `session_before_branch`, `session_branch` | `reason: branch\|fork\|btw`. Before-branch identifies the cut entry and can cancel. Branch discards the selected user message. Fork keeps the selected entry. `btw` promotes a side-question answer. |
| `session_before_compact` | Preparation, branch entries, optional instructions, and an abort signal. Can cancel or supply a compaction result. |
| `session.compacting`, `session_compact` | Customize summary context or prompt, then observe the stored compaction entry. |
| `session_before_tree`, `session_tree` | Cancel or customize tree navigation, then observe navigation. |
| `session_shutdown` | Teardown notification. |
| `input` | Submission interception. Its coverage depends on the transport. Print and ACP input are outside the documented interception contract. |
| `before_agent_start` | Transformed prompt, images, and `systemPrompt: string[]`. Also runs for dequeued batches containing user messages, possibly without another `agent_start`. |
| `agent_start`, `agent_end` | Agent-loop notifications. `agent_end` alone does not prove session quiescence. |
| `session_stop` | Awaited main-session stop decision. Return `{ continue: true, additionalContext }` or `{ decision: "block", reason }` to prevent stopping and request more model work. It does not fire for task/subagent sessions. |
| `turn_start`, `turn_end` | Turn notifications. |
| `message_start`, `message_update`, `message_end` | Message notifications. `message_end` receives a detached snapshot. |
| `tool_call` | Tool name, call ID, and input. Return `{ block: true, reason }` to refuse, or revised `input` and optional `additionalContext`. |
| `tool_result` | Content, details, error state, and call identity. Returned patches can change `content`, `details`, and `isError`. |
| `tool_approval_requested`, `tool_approval_resolved` | Approval observability. Emitted only when approval is required and a handler is registered. Requested carries session ID, tool ID/name, reason, and mode. |
| `plan_review_requested`, `plan_review_resolved` | Interactive review identity, plan, choices, optional model tiers, then choice and answering source. `ctx.resolvePlanReview(...)` can answer. |
| `before_provider_request` | Request `payload`. Return a replacement payload directly. |
| `after_provider_response` | `status`, `headers`, optional `requestId` and `metadata`, before body consumption. `ctx.model` identifies that request's model. |
| `before_subagent_spawn` | Agent name, `invocationKind: task\|eval`, role, model patterns, and optional spawn key. Can block or replace model patterns. No return leaves that handler neutral. |

Handler returns carry decisions. An extension that invokes a helper must read and apply the helper's answer itself. There is no shell-hook stdout decision contract in this extension interface. Returning nothing from `tool_call` leaves it unblocked by that handler, but native approval policy still applies. See the [extension runner][runner] and [tool wrapper][wrapper].

The inspected extension union has no Pi-style `ui_prompt_start` or `ui_prompt_end` event. Tool approvals and plan reviews have dedicated events, listed above. Generic extension dialogs still need transport-specific verification. See [extension types][extension-types].

The runner defaults to a 30-second handler budget, with 2 seconds for `session_shutdown`. `extensionHandlers.toolCallTimeoutMs` changes the tool-call budget. Tool-call errors and timeouts block execution. Dialogs opened through the handler's `ctx.ui` pause its budget while awaiting an answer. These limits do not contain arbitrary detached extension work. See the pinned [runner source][runner].

`session_stop` advisory continuations require nonempty context and are capped at eight. Explicit blocking refusals take precedence and do not consume that allowance. Background work must become idle before this stop hook runs. Returning nothing requests no continuation from that handler. This hook can prevent stopping, so it is not a completed-session notification ([extension lifecycle semantics][extensions]).

## Tool names and manual compaction

| Surface | Upstream vocabulary |
| --- | --- |
| Built-ins | The [canonical name list][tool-names] defines IDs such as `read`, `edit`, `write`, `ast_edit`, `bash`, `eval`, `task`, and `ask`. Availability depends on settings and capabilities. |
| File editing | [`edit`][edit-tool] changes existing files and can remove or move them. [`write`][write-tool] creates or overwrites files and writable internal resources. [`ast_edit`][ast-tool] previews or applies structural rewrites. |
| Other mutation | `bash` and [`eval`][eval-tool] can run arbitrary code. Eval can also launch children through its `agent()` API. A tool-name list alone cannot bound file mutation, especially with extensions or MCP tools. |
| Tool selection | `--tools <names>` enables the selected names. `--no-tools` disables built-in tools. Neither establishes process containment or disables arbitrary extension code. See [CLI tools][cli]. |
| Manual compaction | `/compact [instructions]` requests compaction with optional summary guidance. See [compaction entry points][compaction]. |

## Session files and durable history

Default file sessions use `~/.omp/agent/sessions/<encoded-cwd>/<timestamp>_<sessionId>.jsonl`. The bucket derives from canonical cwd, so symlink aliases share it. Home-relative and temporary paths use shortened encodings. Other paths use an encoded absolute form. Resolve the actual root with the [path helpers][dirs], including profile and Linux XDG rules. See [session storage][session].

| Record | Contract |
| --- | --- |
| Physical first line | Current files start with a mutable, fixed-width 256-byte `type: "title"` slot, including its newline. Legacy files can start directly with the header. |
| Logical header | `type: "session"`, `version: 3`, string `id`, ISO timestamp, `cwd`, and optional title, extra directories, prior paths, cache identity, and lineage. |
| Session identity | New IDs are UUIDv7 strings. Older string IDs remain readable. `parentSession` can be a path or an ID, depending on the operation. |
| Entry base | `type`, `id`, nullable `parentId`, and ISO timestamp. Entry IDs are usually eight hexadecimal characters, but have a longer collision fallback. |
| `message` | Contains an `AgentMessage`. Assistant content uses `text`, `thinking`, and `toolCall` blocks. Tool results use the role `toolResult`. |
| State and accounting | `model_change`, `thinking_level_change`, `service_tier_change`, `model_usage`, and `credential_pin`. |
| Context and navigation | `compaction`, `branch_summary`, and `reset_boundary`. |
| Extension and UI state | `custom`, `custom_message`, `label`, `title_change`, `ttsr_injection`, `session_init`, and `mode_change`. |

The header and title-slot contracts are defined by [session entries][entries] and [title-slot handling][title-slot]. Raw readers must distinguish the physical slot from the logical header. The title slot is rewritten in place. Targeted history rewrite/discard helpers can also change existing records. Consumers must handle file replacement or truncation.

History is a tree. Appended entries can belong to abandoned branches. Context reconstruction follows the active leaf and compaction rules. `/tree` changes navigation. `/branch` rewinds into a new session, while `/fork` can preserve a full conversation or a selected prefix. See [session storage][session] and [session operations][operations].

`/new` changes session identity and materializes a new header in persistent mode. `/clear` retains the ID, file, and history, but clears live context and appends `reset_boundary`. It is TUI-only and refuses during streaming or foreground shell/Python execution. See [session operations][operations].

Large content can be externalized under the agent's content-addressed `blobs/` store. A fresh session can delay creating its JSONL file. Terminal breadcrumb files record cwd and the intended session path during that delay. The session manager's explicit `newSession()` operation writes a header before returning. A directory scan can therefore miss a newly launched session. See [session storage][session].

## Headless output and RPC

| Transport | Contract |
| --- | --- |
| Print | `omp -p` processes prompts, prints the final assistant response, and exits. Progress goes to stderr. `--print-thoughts` includes thinking text. |
| JSON | `omp --mode json` writes a logical session header followed by newline-delimited session events. Incremental updates omit full partial-message snapshots and opaque provider replay payloads. `message_end` carries completed messages. |
| RPC | `omp --mode rpc` accepts newline-delimited JSON commands on stdin and writes responses, events, and side-channel frames on stdout. This is not JSON-RPC 2.0. |
| RPC UI | `--mode rpc-ui` adds tool UI traffic. Extension dialogs use `extension_ui_request` and client responses. |
| Headless extensions | `--no-ui` makes extension `ctx.hasUI` false and dialogs resolve to defaults. In `rpc-ui`, tool UI such as `ask` still sends requests. Host-issued login can still request UI in headless RPC. |
| ACP | `omp acp` or `--mode acp` serves Agent Client Protocol over stdio. See [ACP approval rules][approval]. |
| SDK | `createAgentSession(...)` embeds the runtime. Its options and extension authority are documented separately. |

The [CLI modes][cli], [RPC specification][rpc], [print-mode source][print], and [SDK documentation][sdk] define these transports. Text and JSON print modes return nonzero when the final turn fails. An RPC prompt acknowledgment can still report `success: true` before a provider turn later fails.

RPC rejects CLI `@file` attachments. CLI RPC startup also disables automatic titles. See [RPC startup][rpc].

RPC starts with a `ready` frame advertising protocol versions and limits. Physical stdout frames are capped at 1 MiB. Negotiating protocol v2 enables base64 `rpc_chunk` sequences for oversized output, with a 64 MiB reassembly limit. V1 can elide oversized event data. Even in v2, `agent_end.messages` can omit messages already delivered through `message_end`. Clients must retain streamed messages and drain stdout after closing stdin ([RPC framing][rpc]).

RPC commands have a `type` and an optional correlation `id`. Responses use `type: "response"`, `command`, `success`, and optional data or error. The acknowledgment admits the command. For agent-invoking prompts, a later `prompt_result` reports `status: completed|aborted|error` and `sessionSettled`. Local-only handling can report `agentInvoked: false` without a later prompt result. See [RPC prompting][rpc].

| RPC surface | Commands or fields |
| --- | --- |
| Prompting | `prompt`, `steer`, `follow_up`, `abort`, `remove_queued_message`, `promote_queued_message`, `abort_and_restore_queue`. |
| State | `get_state` includes session ID/file, model, thinking level, context usage, queue state, `hasPendingAsyncWork`, and `isSettled`. |
| Accounting | `get_session_stats`. |
| Native children | `get_subagents`, `get_subagent_messages`, and `set_subagent_subscription`. Forwarding defaults to `off`. |
| Models | `set_model`, `get_available_models`, `set_thinking_level`. |
| History | `get_entries` returns append history and `leafId`, including abandoned branches. `get_tree` returns tree structure. |
| Session control | `new_session`, `switch_session`, `open_session`, `fork`, `set_session_name`. |
| Command discovery | `get_available_commands` returns `{ commands }`. `available_commands_update` pushes changes. Pi's `get_commands` is not served. |

OMP's command catalog includes built-in, skill, extension, custom, MCP-prompt, and file sources. History entries remain OMP-native even when command names match Pi. For example, `model_change` uses a combined `model` field, and side-call accounting uses `model_usage`. A strict Pi decoder cannot assume payload equivalence. See [RPC compatibility boundaries][rpc] and the [RPC handler][rpc-source].

A yield ends the agent's current work, while background results can still restart it. RPC `session_settled` follows the final yield's prompt results when no admitted, scheduled, queued, or background work can restart the session. `get_state.isSettled` and `prompt_result.sessionSettled` use the same predicate. Session `agent_end` can carry `isTerminal`, `yielded`, and `awaitingAsyncWork`. A yielded answer can precede background completion. See the [session-event union][session-events] and [settlement implementation][settle].

## Model, context, and accounting

| Source | Data and limits |
| --- | --- |
| `ctx.model`, RPC `get_state.model` | Current model metadata. Provider request/response handlers receive the model used by that request. |
| `ctx.getContextUsage()`, RPC `contextUsage` | `{ tokens, contextWindow, percent }`, or unavailable. Context use is distinct from cumulative billable tokens. |
| Assistant messages | Concrete `provider`, `model`, API, stop reason, and `usage`. |
| `usage` | `input`, `output`, `cacheRead`, `cacheWrite`, `totalTokens`, and `cost` with per-bucket values and `total`. |
| Optional usage | Authoritative `contextTokens`, provider orchestration tokens, premium requests, reasoning tokens, server-tool counts, and credit meters. |
| `model_usage` entries | Non-conversation model calls, with purpose, optional role, provider/model/API, usage, and stop reason. Late accounting can identify its `activeLeafId`. |
| `get_session_stats` | Message/tool counts, token totals, cost, premium requests, optional credits, routed models, and context usage. |
| Thinking records | `thinking_level_change` can retain both effective `thinkingLevel` and the configured selector, including `auto`. |

These shapes come from [catalog usage types][usage], [session entries][entries], [session statistics types][stats-types], and [context calculation][stats]. Reasoning tokens are a subset of output tokens. An absent reasoning count means unknown, not zero. A stored cost field is OMP's accounting value, not proof of a provider invoice or subscription debit.

Model discovery combines bundled metadata, native `models.yml`, cached or live discovery, and extensions. Availability checks configuration rather than validating credentials. Provider-specific context, price, and billing behavior requires that provider's contract. See [providers][providers] and [models][models].

## Credentials, trust, and approvals

| Surface | Upstream contract |
| --- | --- |
| Local credential store | Default `~/.omp/agent/agent.db`, a SQLite database shared with other agent state. Profile and XDG relocation apply. |
| Credential rows | The `auth_credentials` table stores provider, credential type, plaintext JSON credential data, disabled cause, and identity key. A provider can have multiple credentials. |
| Credential types | API key (`type: "api_key"`, `key`, optional login source) or provider-specific OAuth data (`type: "oauth"`). |
| Local permissions | Storage attempts directory mode `0700` and database mode `0600`. Failures warn. POSIX permission hardening is skipped on Windows. |
| Login | `omp login <provider>` and interactive `/login`. Stored OAuth accounts can refresh and rotate. |
| Broker/gateway | Optional broker-backed credentials and gateway routing have separate network and authentication contracts. Local SQLite is not the only credential authority. |

These facts are defined by the [agent store][agent-store], [SQLite credential store][credential-store], [credential types][credential-types], and [broker documentation][broker]. They do not establish a Pi `auth.json` reader contract.

OMP exposes account limits through `omp usage` and interactive `/usage`. The optional broker offers `/v1/usage` and `/v1/usage/history`. Provider support and units vary. See [CLI usage][cli], [providers][providers], and [broker endpoints][broker].

Provider key precedence is: runtime override, custom-provider `models.yml` key, stored OAuth, login-sourced API key, extension config fallback, then provider environment variables. Custom keys can intentionally override stored OAuth for a custom endpoint. See [credential precedence and provider-specific fallbacks][providers].

Project trust and extension authority are defined under [configuration roots and extension discovery](#configuration-roots-and-extension-discovery).

| Approval mode | Automatically permits | Prompts for |
| --- | --- | --- |
| `always-ask` | Read tier | Write and execution tiers |
| `write` | Read and write tiers | Execution tier |
| `yolo` (default) | Read, write, and execution tiers | Explicit policies and special safety checks can still require approval. |

`--approval-mode` overrides configuration. `--auto-approve` and `--yolo` force yolo even when a mode is also supplied. Explicit denies and prompt policies still apply. Approval evaluates revised tool arguments after extension interception. A required prompt without an interactive UI fails closed.

ACP also requires client grants for unmatched bash and destructive file operations unless yolo is explicitly selected. See [approval policy][approval].

The native `ask` tool supports single or multiple questions with structured results. It requires a prompt-capable UI. Cancellation and headless execution throw instead of returning successful answers. See the pinned [ask tool contract][ask].

The native approval UI has Approve/Deny actions. It keeps composer input separate from approval. Extension UI also offers selection, confirmation, input, editor, and optional structured ask dialogs. Protocol clients must implement the relevant UI request/response surface. These are upstream interfaces, not evidence that RimZ can answer them. See [approval UI][approval], [extension UI][extensions], and [RPC UI][rpc].

## Native children and platforms

OMP includes native task agents and eval `agent()` children. Task children execute in-process on the main thread. Definitions carry model, tool, spawn, and skill policies. Extensions receive `ctx.agent` with `kind: main|sub`, ID, name, depth, and optional parent ID. Depth counts task nesting, so it is not a general main-versus-child discriminator. Background bash, task, and eval results can wake a parent after it yields.

Task definitions follow [task discovery][tasks]. Extension identity follows [extension context][extensions].

Task children use `yolo` approval mode because the parent task approval is their authorization boundary. User `tools.approval` policies still apply. See the pinned [task executor][executor].

The [upstream README][readme] documents macOS, Linux, and Windows installation. The [package manifest][package] requires Bun `>=1.3.14` for the package runtime. Prebuilt binaries provide a separate installation route. On Alpine/musl, the documented prebuilt requires `libstdc++` and `libgcc`. Platform availability does not establish identical terminal, extension-reload, or permission behavior.

## Evidence limits

| Question | Remaining verification |
| --- | --- |
| Process detection and containment | Observe Bun and prebuilt process identity. No native sandbox or delegation-lockdown flag contract was verified. |
| Published artifact parity | Compare the installed or unpacked `18.11.0` artifact with the pinned source. Version equality alone is insufficient. |
| Actual lifecycle order | Capture native interactive, print, RPC, retry, cancellation, compaction, and background-child sessions. Static event unions do not prove observed order. |
| Blocking asks | Exercise native approval, plan review, ask dialogs, cancellation, and headless defaults on each intended transport. |
| Durable history | Capture title-slot updates, forks, branch navigation, blob references, and crash recovery. No real transcript was inspected here. |
| Account and cost mapping | Validate each target provider's credential rotation, usage refresh, quota windows, and monetary units. No account was accessed. |
| Pi compatibility | Test any proposed reuse against OMP's roots, runtime API, event sets, and payloads. Compatibility import rewriting alone is insufficient. |

[package]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/package.json
[cli]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/cli-reference.md
[config]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/config-usage.md
[dirs]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/utils/src/dirs.ts
[loading]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/extension-loading.md
[native]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/discovery/builtin.ts
[hooks]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/hooks.md
[extension-types]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/extensibility/extensions/types.ts
[shared-events]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/extensibility/shared-events.ts
[session-events]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/session/agent-session-events.ts
[settle]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/modes/rpc/rpc-session-settle.ts
[runner]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/extensibility/extensions/runner.ts
[wrapper]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/extensibility/extensions/wrapper.ts
[extensions]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/extensions.md
[session]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/session.md
[entries]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/session/session-entries.ts
[title-slot]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/session/session-title-slot.ts
[operations]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/session-operations-export-share-fork-resume.md
[rpc]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/rpc.md
[print]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/modes/print-mode.ts
[sdk]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/sdk.md
[rpc-source]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/modes/rpc/rpc-mode.ts
[usage]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/catalog/src/types.ts
[stats-types]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/session/agent-session-types.ts
[stats]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/session/session-stats.ts
[providers]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/providers.md
[models]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/models.md
[agent-store]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/session/agent-storage.ts
[credential-store]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/ai/src/auth/sqlite-credential-store.ts
[credential-types]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/ai/src/auth/types.ts
[broker]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/auth-broker-gateway.md
[approval]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/approval-mode.md
[skills]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/skills.md
[tasks]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/task-agent-discovery.md
[readme]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/README.md

[executor]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/task/executor.ts

[ask]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/tools/ask.md

[environment]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/environment-variables.md

[tool-names]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/packages/coding-agent/src/tools/builtin-names.ts
[edit-tool]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/tools/edit.md
[write-tool]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/tools/write.md
[ast-tool]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/tools/ast-edit.md
[compaction]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/compaction.md
[settings]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/settings.md

[eval-tool]: https://github.com/can1357/oh-my-pi/blob/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74/docs/tools/eval.md

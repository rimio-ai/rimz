# OMP protocol reference

OMP (Oh My Pi) is a fork of Pi, published as `@oh-my-pi/pi-coding-agent` with the executable `omp`. This page records its upstream integration surface for contributors evaluating an adapter. It does not establish RimZ support or select an integration design. The [adapter playbook](../../contributing/agent-adapters.md) owns that later workflow. The [Pi reference](./pi-reference.md) records Pi's independent contract.

## Verification baseline

Checked 2026-10-15 against official source commit [`13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74`](https://github.com/can1357/oh-my-pi/commit/13a3d52ea5cc8a2a210e4881e22bbe473ec9ed74). The pinned [package manifest][package] declares version `18.11.0`. The [npm version metadata](https://registry.npmjs.org/@oh-my-pi/pi-coding-agent/18.11.0) also identifies `18.11.0`, with `omp` mapped to `dist/cli.js` and Bun `>=1.3.14`. Matching version strings do not prove the published bundle matches this commit.

Verification is static: official documentation and source were read, but no OMP executable, authenticated turn, extension, or recorded session was run. All upstream links below pin the inspected commit unless explicitly labeled as registry metadata. Source contracts take precedence where prose differs.

## Executable and launch arguments

`omp` and `omp launch` start a session. A registered subcommand takes precedence over an initial prompt word. `omp launch` makes that choice explicit. Outside protocol modes, piped input selects print mode when `--mode` is absent. `--` ends option parsing and makes later arguments literal messages. These are OMP arguments, not RimZ commands. See the pinned [CLI reference][cli].

| Surface | Arguments or behavior |
| --- | --- |
| Detection | `omp --version` / `-v`, `omp --help`, `omp launch --help` |
| Initial input | Positional messages and `@path` attachments. Stdin, text attachments, and the first positional message form the initial prompt. Later messages become later turns. |
| Workspace | `--cwd <dir>`, repeatable `--add-dir <dir>`. Without `--allow-home`, starting in the home directory can switch to a temporary directory. |
| Resume | `--continue` / `-c` continues the previous session. `--resume [id]` / `-r` / `--session [id]` takes an ID prefix or path, or opens a picker. |
| Recovery | `--restore [id]` also recovers interrupted work. It is interactive-only and accepts no prompt. |
| Fork | `--fork <session>` creates a new session from an ID prefix or path. |
| Storage | `--session-dir <dir>` changes storage and lookup. `--no-session` disables persistence and cannot combine with resume, restore, fork, or imports. |
| Model | `--model <id-or-role>`, `--models <patterns>`, legacy `--provider <name>`, and role overrides `--smol`, `--slow`, `--plan`. |
| Thinking | `--thinking off\|minimal\|low\|medium\|high\|xhigh\|max\|auto`. `--hide-thinking` affects display only. |
| System prompt | `--system-prompt <text-or-file>`, `--append-system-prompt <text-or-file>`, or `--system-prompt-template <path>`. The template and replacement flags are mutually exclusive. |
| Extension | Repeatable `--extension` / `-e <path>`, `--hook <path>`, `--no-extensions`, or `--trusted-extension <absolute-file>`. |
| Tool policy | `--tools <names>`, `--no-tools`, `--approval-mode always-ask\|write\|yolo`, `--auto-approve`, `--yolo`. |
| Skills and rules | `--skills <globs>`, `--no-skills`, `--no-rules`. |
| Headless | `--print` / `-p`, `--mode text\|json\|rpc\|rpc-ui\|acp`. Explicit `--mode text` is non-interactive. |

No process-name or executable alias compatibility with Pi is established here. The package name and executable come from OMP's manifest, not its ancestry.

## Configuration roots and extension discovery

The default agent directory is `~/.omp/agent`. The following paths assume the default profile and no overrides. OMP's [configuration reference][config] and [path helpers][dirs] define relocation.

| Input | Resolution |
| --- | --- |
| `--profile <name>` | Selects `~/.omp/profiles/<name>/agent` for native user settings, auth, sessions, and extensions. |
| `OMP_PROFILE`, legacy `PI_PROFILE` | `OMP_PROFILE` wins when defined, including an empty value. Empty, whitespace, and `default` select the default profile. |
| `PI_CONFIG_DIR` | Changes the user config root name, normally `.omp`. |
| `PI_CODING_AGENT_DIR` | Overrides agent-directory consumers in the default profile. Named profiles ignore it. Generic config lookup has separate rules. |
| Linux `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | Relocate classified paths under `omp/`. For example, sessions use `$XDG_DATA_HOME/omp/sessions`. The helpers trust configured roots without checking migration. |
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

Extensions execute in-process with host permissions. They are not sandboxed. A default-exported factory receives `ExtensionAPI` and registers handlers through `pi.on(...)`. Bun loads modules through compatibility rewriting for legacy Pi package imports. Those import shims do not establish event or payload equivalence. See the pinned [loader documentation][loading].

The normal CLI uses `ExtensionRunner` and `ExtensionToolWrapper`. `--hook` is an extension-path alias. [hooks.md][hooks] also describes a legacy runner whose context, error, and timeout rules differ. Its standalone behavior must not be used as the normal runtime contract.

## Lifecycle and blocking decisions

Extension events, session subscription events, and RPC frames are separate surfaces. In particular, `session_settled` is an RPC frame, not an extension event in the inspected union. See [extension types][extension-types], [shared event types][shared-events], [session events][session-events], and [RPC settlement][settle].

| Extension event | Payload or decision |
| --- | --- |
| `session_start` | The event contains only its type. Session identity comes from `ctx.sessionManager`. |
| `session_before_switch`, `session_switch` | `reason: new\|resume\|fork`. Before-switch can cancel and can identify the target session file. After-switch identifies the previous file. |
| `session_before_branch`, `session_branch` | `reason: branch\|fork\|btw`. Before-branch identifies the cut entry and can cancel. Branch discards the selected user message. Fork keeps the selected entry. |
| `session_before_compact` | Preparation, branch entries, optional instructions, and an abort signal. Can cancel or supply a compaction result. |
| `session.compacting`, `session_compact` | Customize summary context or prompt, then observe the stored compaction entry. |
| `session_before_tree`, `session_tree` | Cancel or customize tree navigation, then observe navigation. |
| `session_shutdown` | Teardown notification. |
| `input` | Submission interception. Its coverage depends on the transport. Print and ACP input are outside the documented interception contract. |
| `before_agent_start` | Transformed prompt, images, and `systemPrompt: string[]`. Also runs for dequeued user-containing batches, possibly without another `agent_start`. |
| `agent_start`, `agent_end` | Agent-loop notifications. `agent_end` alone does not prove session quiescence. |
| `session_stop` | Awaited main-session stop decision. Can request continuation or return `{ decision: "block", reason }`. It does not fire for task/subagent sessions. |
| `turn_start`, `turn_end` | Turn notifications. |
| `message_start`, `message_update`, `message_end` | Message notifications. `message_end` receives a detached snapshot. |
| `tool_call` | Tool name, call ID, and input. Return `{ block: true, reason }` to refuse, or revised `input` and optional `additionalContext`. |
| `tool_result` | Content, details, error state, and call identity. Returned patches can change `content`, `details`, and `isError`. |
| `before_provider_request` | Request `payload`. Return a replacement payload directly. |
| `after_provider_response` | `status`, `headers`, optional `requestId` and `metadata`, before body consumption. `ctx.model` identifies that request's model. |
| `before_subagent_spawn` | Agent name, `invocationKind: task\|eval`, role, model patterns, and optional spawn key. Can block or replace model patterns. |

Handler returns carry decisions. An extension that invokes a helper must read and apply the helper's answer itself. There is no shell-hook stdout decision contract in this extension interface. Returning nothing from `tool_call` leaves it unblocked by that handler, but native approval policy still applies. See the [extension runner][runner] and [tool wrapper][wrapper].

The runner defaults to a 30-second handler budget, with 2 seconds for `session_shutdown`. `extensionHandlers.toolCallTimeoutMs` changes the tool-call budget. Tool-call errors and timeouts block execution. Managed UI dialogs pause the handler budget while awaiting an answer. These limits do not contain arbitrary detached extension work. See the pinned [runner source][runner].

`session_stop` advisory continuations are capped at eight. Explicit blocking refusals take precedence and do not consume that allowance. Background work must become idle before this stop hook runs. See [extension lifecycle semantics][extensions].

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

The header and title-slot contracts are defined by [session entries][entries] and [title-slot handling][title-slot]. Raw readers must distinguish the physical slot from the logical header. Current files can change existing bytes, so treating the entire file as append-only is incorrect.

History is a tree. Appended entries can belong to abandoned branches. Context reconstruction follows the active leaf and compaction rules. `/tree` changes navigation. `/branch` rewinds into a new session, while `/fork` can preserve a full conversation or a selected prefix. `parentSession` alone is not a typed foreign key. See [session storage][session] and [session operations][operations].

Large content can be externalized under the agent's content-addressed `blobs/` store. Fresh lazy sessions can have terminal breadcrumbs before a JSONL file exists. Explicit `newSession()` boundaries materialize a header before returning. A directory scan can therefore miss a newly launched session. See [session storage][session].

## Headless output and RPC

| Transport | Contract |
| --- | --- |
| Print | `omp -p` processes prompts, prints the final assistant response, and exits. Progress goes to stderr. `--print-thoughts` includes thinking text. |
| JSON | `omp --mode json` writes a logical session header followed by newline-delimited session events. Incremental updates omit full partial-message snapshots and opaque provider replay payloads. `message_end` carries completed messages. |
| RPC | `omp --mode rpc` accepts newline-delimited JSON commands on stdin and writes responses, events, and side-channel frames on stdout. This is not JSON-RPC 2.0. |
| RPC UI | `--mode rpc-ui` adds tool UI traffic. Extension dialogs use `extension_ui_request` and client responses. |
| Headless extensions | `--no-ui` makes extension `ctx.hasUI` false and dialogs resolve to defaults. In `rpc-ui`, tool UI such as `ask` still sends requests. Host login UI remains separate. |
| ACP | `omp acp` or `--mode acp` serves Agent Client Protocol over stdio. Its approval contract differs from stdio RPC. |
| SDK | `createAgentSession(...)` embeds the runtime. Its options and extension authority are documented separately. |

The [CLI mode reference][cli], [RPC specification][rpc], [print-mode source][print], and [SDK documentation][sdk] own the transport details. Text and JSON print modes return nonzero on terminal turn failures. An RPC prompt acknowledgment can still report `success: true` before a provider turn later fails.

RPC commands have a `type` and an optional correlation `id`. Responses use `type: "response"`, `command`, `success`, and optional data or error. The acknowledgment admits the command. For agent-invoking prompts, a later `prompt_result` reports `status: completed|aborted|error` and `sessionSettled`. Local-only handling can report `agentInvoked: false` without a later prompt result. See [RPC prompting][rpc].

| RPC surface | Commands or fields |
| --- | --- |
| Prompting | `prompt`, `steer`, `follow_up`, `abort`, and queue-control commands. |
| State | `get_state` includes session ID/file, model, thinking level, context usage, queue state, `hasPendingAsyncWork`, and `isSettled`. |
| Accounting | `get_session_stats`. |
| Models | `set_model`, `get_available_models`, `set_thinking_level`. |
| History | `get_entries` returns append history and `leafId`, including abandoned branches. `get_tree` returns tree structure. |
| Session control | `new_session`, `switch_session`, `open_session`, `fork`, `set_session_name`. |
| Command discovery | `get_available_commands` returns `{ commands }`. `available_commands_update` pushes changes. Pi's `get_commands` is not served. |

OMP's command catalog includes built-in, skill, extension, custom, MCP-prompt, and file sources. History commands share some Pi reconciliation semantics, but entries remain OMP-native. For example, `model_change` uses a combined `model` field, and side-call accounting uses `model_usage`. A strict Pi decoder cannot assume payload equivalence. See [RPC compatibility boundaries][rpc] and the [RPC handler][rpc-source].

RPC `session_settled` follows the final yield's prompt results when no admitted, scheduled, queued, or background work can restart the session. `get_state.isSettled` and `prompt_result.sessionSettled` use the same predicate. Session `agent_end` can carry `isTerminal`, `yielded`, and `awaitingAsyncWork`. A yielded answer can precede background completion. See the [session-event union][session-events] and [settlement implementation][settle].

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

Provider key precedence is: runtime override, custom-provider `models.yml` key, stored OAuth, login-sourced API key, extension config fallback, then provider environment variables. Additional provider-specific fallback rules exist. Custom keys can intentionally override stored OAuth for a custom endpoint. See [credential precedence][providers].

OMP does not ask for per-directory trust before loading project inputs. Extension `ctx.isProjectTrusted()` always returns `true`. `--trusted-extension` limits extension modules, but does not sandbox them or disable other discovery subsystems. See [extension context][extensions] and [extension loading][loading].

| Approval mode | Automatically permits | Prompts for |
| --- | --- | --- |
| `always-ask` | Read tier | Write and execution tiers |
| `write` | Read and write tiers | Execution tier |
| `yolo` (default) | Read, write, and execution tiers | Explicit policies and special safety checks can still require approval. |

`--approval-mode` overrides configuration. `--auto-approve` and `--yolo` force yolo even when a mode is also supplied. Explicit denies and prompt policies still apply. Approval evaluates revised tool arguments after extension interception. A required prompt without an interactive UI fails closed. See [approval policy][approval].

The native approval UI has Approve/Deny actions. It keeps composer input separate from approval. Extension UI also offers selection, confirmation, input, editor, and optional structured ask dialogs. Protocol clients must implement the relevant UI request/response surface. These are upstream interfaces, not evidence that RimZ can answer them. See [approval UI][approval], [extension UI][extensions], and [RPC UI][rpc].

## Skills, native children, and platforms

Skills expose metadata to the model and load content through `read` on `skill://...`. Conventional discovery uses `skills/<name>/SKILL.md`, without recursive grouping. `--skills` filters names with globs. `--no-skills` disables skill discovery and loading. Skills have independent discovery controls, so `--no-extensions` is insufficient. See [skills][skills].

OMP includes native task agents and eval `agent()` children. Task children execute in-process on the main thread. Definitions carry model, tool, spawn, and skill policies. Extensions receive `ctx.agent` with `kind: main|sub`, ID, name, depth, and optional parent ID. Depth counts task nesting, so it is not a general main-versus-child discriminator. Background bash, task, and eval results can wake a parent after it yields. See [task discovery][tasks], [extension context][extensions], and [RPC settlement][rpc].

Task children use `yolo` approval mode because the parent task approval is their authorization boundary. User `tools.approval` policies still apply. See the pinned [task executor][executor].

The [upstream README][readme] documents macOS, Linux, and Windows installation. The [package manifest][package] requires Bun `>=1.3.14` for the package runtime. Prebuilt binaries provide a separate installation route. On Alpine/musl, the documented prebuilt requires `libstdc++` and `libgcc`. Platform availability does not establish identical terminal, extension-reload, or permission behavior.

## Evidence limits

| Question | Remaining verification |
| --- | --- |
| Published artifact parity | Compare the installed or unpacked `18.11.0` artifact with the pinned source. Version equality alone is insufficient. |
| Actual lifecycle order | Capture native interactive, print, RPC, retry, cancellation, compaction, and background-child sessions. Static event unions do not prove observed order. |
| Blocking asks | Exercise native approval, plan review, ask dialogs, cancellation, and headless defaults on each intended transport. |
| Durable history | Capture title-slot updates, forks, branch navigation, blob references, and crash recovery. No real transcript was inspected here. |
| Account and cost mapping | Validate each target provider's credential rotation, usage refresh, quota windows, and monetary units. No account was accessed. |
| Pi compatibility | Test any proposed reuse against OMP's roots, runtime API, event sets, and payloads. Compatibility import rewriting alone is insufficient. |

No implementation defect in RimZ is established by this upstream-only investigation. Adapter coverage, installation ownership, permission-mode mapping, and the choice between a built-in adapter and a plugin remain undecided.

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

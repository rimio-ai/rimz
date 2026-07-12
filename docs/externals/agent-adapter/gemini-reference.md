# Gemini CLI protocol reference

> The agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md), the landed mapping is [gemini.md](../../internals/agents/gemini.md), and the account, balance, spend, and pricing contract is [providers.md](../../internals/agents/providers.md).

This is the single home for the **Gemini CLI upstream protocol surface** a RimZ adapter can bind to: command hooks, tool-confirmation notifications, session JSONL, authentication and quota state, subagents, resume behavior, and the headless stream. It mirrors Google's published documentation and the open-source `google-gemini/gemini-cli` types, with source links pinned for implementation work.

Coverage is **depth on viable RimZ inputs, breadth as an index**. The hook and session shapes are detailed enough to implement typed parsers. Adjacent surfaces are indexed so an implementer can deliberately choose them rather than rediscover them.

## Refresh target and upstream sources

This mirror was refreshed against Gemini CLI `0.52.0-nightly.20260707.g27a3da3e8` at source commit [`f354eebaf43b25bacb176007e449bb9a638fd101`](https://github.com/google-gemini/gemini-cli/tree/f354eebaf43b25bacb176007e449bb9a638fd101). Re-fetch the documentation and compare the linked types before implementation because hooks, session persistence, subagents, plan mode, and quota reporting are active development surfaces.

| Surface | Source |
| --- | --- |
| Hooks overview, configuration, execution, and security | <https://geminicli.com/docs/hooks/> |
| Hook event payloads and output schema | <https://geminicli.com/docs/hooks/reference/> |
| Hook wire types | [`packages/core/src/hooks/types.ts`](https://github.com/google-gemini/gemini-cli/blob/f354eebaf43b25bacb176007e449bb9a638fd101/packages/core/src/hooks/types.ts) |
| Hook command runner and environment | [`packages/core/src/hooks/hookRunner.ts`](https://github.com/google-gemini/gemini-cli/blob/f354eebaf43b25bacb176007e449bb9a638fd101/packages/core/src/hooks/hookRunner.ts) |
| Session management and retention | <https://geminicli.com/docs/cli/session-management/> |
| Session JSONL types and reader | [`chatRecordingTypes.ts`](https://github.com/google-gemini/gemini-cli/blob/f354eebaf43b25bacb176007e449bb9a638fd101/packages/core/src/services/chatRecordingTypes.ts), [`chatRecordingService.ts`](https://github.com/google-gemini/gemini-cli/blob/f354eebaf43b25bacb176007e449bb9a638fd101/packages/core/src/services/chatRecordingService.ts) |
| CLI flags | <https://geminicli.com/docs/cli/cli-reference/> |
| Headless JSON and stream JSON | <https://geminicli.com/docs/cli/headless/> |
| Headless stream types | [`packages/core/src/output/types.ts`](https://github.com/google-gemini/gemini-cli/blob/f354eebaf43b25bacb176007e449bb9a638fd101/packages/core/src/output/types.ts) |
| Tools and canonical names | <https://geminicli.com/docs/reference/tools/> |
| Plan mode | <https://geminicli.com/docs/cli/plan-mode/> |
| Subagents | <https://geminicli.com/docs/core/subagents/> |
| Model selection and routing | <https://geminicli.com/docs/cli/model/>, <https://geminicli.com/docs/cli/model-routing/> |
| Authentication | <https://geminicli.com/docs/get-started/authentication/> |
| Quotas and pricing | <https://geminicli.com/docs/resources/quota-and-pricing/> |
| OAuth storage and Code Assist quota types | [`oauth-credential-storage.ts`](https://github.com/google-gemini/gemini-cli/blob/f354eebaf43b25bacb176007e449bb9a638fd101/packages/core/src/code_assist/oauth-credential-storage.ts), [`types.ts`](https://github.com/google-gemini/gemini-cli/blob/f354eebaf43b25bacb176007e449bb9a638fd101/packages/core/src/code_assist/types.ts), [`server.ts`](https://github.com/google-gemini/gemini-cli/blob/f354eebaf43b25bacb176007e449bb9a638fd101/packages/core/src/code_assist/server.ts) |
| Settings and folder trust | <https://geminicli.com/docs/cli/settings/>, <https://geminicli.com/docs/cli/trusted-folders/> |
| ACP JSON-RPC mode | <https://geminicli.com/docs/cli/acp-mode/> |

## Adapter feasibility at a glance

The stock interactive CLI exposes enough durable identity and lifecycle evidence for a first-class adapter:

| RimZ need | Gemini surface | Verdict |
| --- | --- | --- |
| Session identity and registration | `SessionStart.session_id`, present on every hook | direct |
| Turn start and prompt | `BeforeAgent.prompt` | direct |
| Turn completion | `AfterAgent.prompt_response` | direct for clean completed turns |
| Tool activity and acting phase | `BeforeTool` / `AfterTool`; `write_file` and `replace` are edit tools | direct |
| Permission wait | `Notification` with `notification_type = "ToolPermission"` and typed `details` | direct observation; answer still goes through pane input |
| User question and plan approval | `BeforeTool` with `tool_name = "ask_user"` / `"exit_plan_mode"` | direct observation; answer still goes through the native dialog |
| Compaction start | `PreCompress.trigger` | direct |
| Compaction end | no post-compression hook | infer from the next lifecycle event; trigger comes from the opener |
| Session end | `SessionEnd.reason` | direct but best-effort and asynchronous |
| Model and context | newest Gemini session record: `model` plus `tokens.total`; model limit is 1,048,576 for current Gemini families | direct durable enrichment |
| Session spend | session records contain token categories but no dollars | price from RimZ's model table for metered API/Vertex use |
| Quota balance and plan | Code Assist `retrieveUserQuota` plus `LoadCodeAssistResponse`; `/stats model` uses the same in-process data | source-backed, no stable public API contract |
| Subagents | nested session JSONL and parent `invoke_agent` tool records | correlatable, but no dedicated start/stop hook |
| Supervised `-p` runs | `--output-format stream-json` and process exit code | direct |
| Native resume | `--resume [latest|index|UUID]` | direct |
| Native fork | no CLI fork operation | gap; do not represent checkpoint restore or rewind as a fork |

The first adapter should use hooks as lifecycle truth and session JSONL as context/spend enrichment. ACP is an alternate stdio-owned operating mode and does not observe a stock TUI pane; keep it out of the initial interactive adapter.

## Session resume, clear, rewind, and fork

Gemini stores sessions per project and resumes them with `gemini --resume`, `gemini --resume latest`, `gemini --resume <index>`, or `gemini --resume <full UUID>`. An optional positional query starts a new turn immediately after resume. `gemini --list-sessions` lists the current project's records, and `gemini --delete-session <index|id>` deletes one.

`/resume` opens the session browser. `/resume save <tag>` and `/resume resume <tag>` save and restore a named chat checkpoint; `/chat` is a compatibility alias. `/rewind` moves the active conversation back to an earlier interaction and can separately restore conversation history, file changes, or both. `/clear` ends the old session and creates a new session identity.

Gemini CLI exposes no native session fork that copies a conversation into a new provider-assigned session while preserving the source. A RimZ `fork` capability must remain false until upstream adds one or RimZ defines and validates an explicit import/copy workflow. A tagged checkpoint, rewind, or Git worktree is not equivalent to a session fork.

## Hooks

A command hook runs at a lifecycle point, receives one JSON object on **stdin**, and returns one JSON object on **stdout**. Logs and diagnostics go to stderr. Hooks are synchronous unless the event explicitly documents best-effort/asynchronous behavior; Gemini waits for all matching hooks before continuing the agent loop.

### Configuration

Hooks live under `hooks.<EventName>[]` in `settings.json`:

```json
{
  "hooks": {
    "BeforeTool": [
      {
        "matcher": "write_file|replace",
        "sequential": true,
        "hooks": [
          {
            "type": "command",
            "name": "rimz-before-tool",
            "command": "rimz hooks handle gemini BeforeTool",
            "timeout": 5000,
            "description": "Forward Gemini lifecycle state to RimZ"
          }
        ]
      }
    ]
  }
}
```

`matcher` is a regular expression for `BeforeTool` and `AfterTool`, and an exact trigger string for lifecycle events. `"*"` and `""` match every occurrence. MCP tool names use `mcp_<server_name>_<tool_name>`. `sequential: true` passes supported modifications from one hook into the next; false or absent permits parallel execution within the group. The default timeout is 60,000 ms.

Gemini merges hook definitions from project `.gemini/settings.json`, user `~/.gemini/settings.json`, system settings, and installed extensions. The hooks guide states the hook precedence as project, user, system, then extensions. System policy can still constrain the broader merged settings surface; refresh both the hook guide and configuration reference when implementing installation.

`hooksConfig.enabled` is the canonical global toggle. `hooksConfig.disabled` names individual disabled hooks, and `hooksConfig.notifications` controls the CLI's visual hook-running indicators. `/hooks panel`, `/hooks enable-all`, `/hooks disable-all`, `/hooks enable <name>`, and `/hooks disable <name>` manage the effective set.

### Execution environment

The command runner selects the user's platform shell, runs with `cwd` equal to the hook input's current working directory, sends compact JSON without a trailing newline to stdin, and captures stdout and stderr. It terminates a timed-out process, then force-kills it after five seconds if required.

The child begins with the Gemini process environment after optional sensitive-variable redaction, then receives these overlays:

| Variable | Value |
| --- | --- |
| `GEMINI_PROJECT_DIR` | current hook `cwd` in the current implementation |
| `GEMINI_PLANS_DIR` | effective plans directory |
| `GEMINI_SESSION_ID` | current session id |
| `GEMINI_CWD` | current hook `cwd` |
| `CLAUDE_PROJECT_DIR` | compatibility alias for current hook `cwd` |

The public guide describes `GEMINI_PROJECT_DIR` as the project root, while the pinned runner assigns `input.cwd`. Treat the pinned implementation as the observed wire and avoid using that variable as the workspace identity until upstream reconciles the discrepancy. Hook-specific `env` overlays are an internal/config type surface and are not listed in the public hook configuration schema.

Environment redaction removes credential-like names and values when `security.environmentVariableRedaction.enabled` is active and always applies a strict allowlist on GitHub surfaces. A RimZ hook must recover its pane/workspace binding from durable session and process identity rather than assuming RimZ's mux-stamped variables survive sanitization.

### Trust and executable surface

Hooks execute with the user's privileges. Workspace hooks load only in a trusted folder; an untrusted workspace ignores `.gemini/settings.json` and project hooks. Folder trust lives in `~/.gemini/trustedFolders.json` when `security.folderTrust.enabled` is active. Headless callers can grant trust for one invocation with `--skip-trust` or `GEMINI_CLI_TRUST_WORKSPACE=true`.

The pinned source also records seen project hook keys in `~/.gemini/trusted_hooks.json`, keyed by absolute project path with values derived as `<name>:<command>`. On first sight it warns, records the keys, and still executes them; this is a warning acknowledgement rather than an approval gate. Changing either name or command produces a new key and warning. RimZ's trust hash must include every installed Gemini hook command and must not mistake `trusted_hooks.json` for proof that execution is blocked pending approval.

### Common input

Every event receives:

```jsonc
{
  "session_id": "UUID",
  "transcript_path": "/absolute/path/to/session-....jsonl",
  "cwd": "/current/working/directory",
  "hook_event_name": "BeforeAgent",
  "timestamp": "ISO 8601"
}
```

`transcript_path` is an empty string if chat recording is unavailable at hook time. The session id is the durable identity. Unlike Claude and Codex hook payloads, the base payload carries no model, permission mode, parent agent id, subagent name, turn id, prompt id distinct from the session id, or context count.

### Common output and exit codes

Most events accept:

```jsonc
{
  "continue": true,
  "stopReason": "message shown when continue is false",
  "suppressOutput": false,
  "systemMessage": "message shown immediately",
  "decision": "allow | approve | ask | deny | block",
  "reason": "decision explanation",
  "hookSpecificOutput": { "hookEventName": "BeforeTool" }
}
```

The public reference documents `allow`, `deny`, and the `block` alias. The pinned type also accepts `approve` and `ask`; `ask` on `BeforeTool` forces the ordinary confirmation flow. Use only event-documented fields unless the adapter deliberately binds to the pinned source behavior.

Exit handling:

- `0` parses stdout as JSON and applies the event's decision.
- `2` is a system block; stderr becomes the rejection reason, with event-specific behavior below.
- Any other code is a non-fatal warning and the original action continues.

The runner attempts to parse `stdout.trim() || stderr.trim()`. Plain text is converted into a structured hook result, but the official contract requires stdout to contain only the final JSON object. RimZ's neutral result is `{}` on stdout with exit 0; logs stay on stderr.

### Events and event-specific wire

| Event | Fires | Input beyond common fields | Output relevant to an adapter |
| --- | --- | --- | --- |
| `SessionStart` | startup, resume, or after `/clear` | `source`: `startup` \| `resume` \| `clear` | `hookSpecificOutput.additionalContext`, `systemMessage`; flow control ignored |
| `SessionEnd` | exit, clear, logout, prompt-input exit, or other shutdown | `reason`: `exit` \| `clear` \| `logout` \| `prompt_input_exit` \| `other` | `systemMessage`; best-effort, not awaited, flow control ignored |
| `BeforeAgent` | once after a user prompt and before the agent loop | `prompt` | add context, deny/discard prompt, or stop while retaining prompt |
| `AfterAgent` | once after the outer agent loop has a final response and no pending tool calls | `prompt`, `prompt_response`, `stop_hook_active` | deny to retry, stop without retry, optionally clear model context |
| `BeforeTool` | before a built-in or MCP tool executes | `tool_name`, `tool_input`, optional `mcp_context`, optional `original_request_name` | deny/block, force ask, or replace input fields |
| `AfterTool` | after tool execution | `tool_name`, `tool_input`, `tool_response`, optional `mcp_context`, optional `original_request_name` | hide/replace result, append context, or request a tail tool call |
| `Notification` | immediately before the CLI enters a tool-confirmation wait | `notification_type = "ToolPermission"`, `message`, `details` | observation only; `systemMessage` and `suppressOutput` |
| `PreCompress` | before automatic or manual history compression | `trigger`: `auto` \| `manual` | advisory `systemMessage`; asynchronous, flow control ignored |
| `BeforeModel` | before an LLM request | `llm_request` | patch request, synthesize response, or block turn |
| `BeforeToolSelection` | before model tool selection | `llm_request` | constrain tool mode and allowed names; common flow controls unsupported |
| `AfterModel` | for every streamed model response chunk | `llm_request`, `llm_response` | replace current chunk, discard it, or stop loop |

The pinned `Notification.details` serializer emits these discriminated shapes:

| `details.type` | Additional fields on the current wire |
| --- | --- |
| `edit` | `title`, `fileName`, `filePath`, `fileDiff`, `originalContent`, `newContent`, optional `isModifying` |
| `exec` | `title`, `command`, `rootCommand` |
| `mcp` | `title`, `serverName`, `toolName`, `toolDisplayName` |
| `info` | `title`, `prompt`, optional `urls` |
| `sandbox_expansion`, `ask_user`, `exit_plan_mode` | `title` only in the current hook serializer |

The broader confirmation-bus type contains more fields than the hook serializer exposes. Parse the hook's smaller shape and tolerate additions. Classify `ask_user` as a question and `exit_plan_mode` as plan approval; every other current `details.type` is an ordinary permission prompt.

#### Lifecycle event decisions

`BeforeAgent` supports turn-local additional context:

```json
{ "hookSpecificOutput": { "hookEventName": "BeforeAgent", "additionalContext": "text" } }
```

`decision: "deny"` blocks the turn and removes the user's message from history. `continue: false` blocks the turn but retains the message. Exit 2 behaves like deny.

`AfterAgent` denial rejects the response and automatically runs another attempt with `reason` as the new corrective prompt. Gemini sets `stop_hook_active: true` on the retry path so a hook can avoid an infinite loop. `continue: false` stops without retry. `hookSpecificOutput.clearContext: true` clears LLM memory while leaving the UI transcript visible. Exit 2 requests a retry with stderr as feedback.

For RimZ, a neutral `AfterAgent` is the clean `turn_ended` signal. The payload has no error bit or finish reason, so provider/API failures that bypass `AfterAgent` need a transcript/error or process-state backstop before the adapter can distinguish `failed`, `paused`, and interrupted-idle outcomes.

#### Tool decisions

`BeforeTool` can block or rewrite:

```jsonc
{ "decision": "deny", "reason": "sent to the model as a tool error" }
{ "decision": "ask", "systemMessage": "shown with the confirmation" }
{ "hookSpecificOutput": { "hookEventName": "BeforeTool", "tool_input": { "file_path": "replacement" } } }
```

Exit 2 blocks the tool, sends stderr to the agent as the tool error, and lets the turn continue. `continue: false` terminates the whole agent loop.

`AfterTool` can append model-visible context or replace the result:

```jsonc
{ "hookSpecificOutput": { "hookEventName": "AfterTool", "additionalContext": "text" } }
{ "decision": "deny", "reason": "replacement result shown to the model" }
{ "hookSpecificOutput": { "hookEventName": "AfterTool", "tailToolCallRequest": { "name": "read_file", "args": { "file_path": "x" } } } }
```

Exit 2 hides the real result, substitutes stderr, and lets the turn continue. A tail call's result replaces the original tool response; `original_request_name` identifies the source request on the tail call's own hook payload.

`ask_user` carries `tool_input.questions`, an array of one to four objects. Each has `question`, a short `header`, and `type` (`choice`, `text`, or `yesno`); choice questions carry two to four `{label, description}` options and optional `multiSelect`, while text inputs can carry `placeholder`. Its result returns `{"answers":{"0":"..."}}` to the model. Preserve this typed input if RimZ adds native structured-answer rendering.

#### Stable model-hook schema

The hook layer translates SDK-specific request/response objects into this reduced surface:

```jsonc
{
  "model": "string",
  "messages": [{ "role": "user | model | system", "content": "text-only content" }],
  "config": { "temperature": 1 },
  "toolConfig": { "mode": "AUTO | ANY | NONE", "allowedFunctionNames": ["read_file"] }
}
```

The stable response carries `candidates[].content` (`role`, text `parts`), `finishReason`, and `usageMetadata.totalTokenCount`. `BeforeToolSelection` combines multiple hooks' whitelists and gives `NONE` precedence. `AfterModel` fires per streaming chunk, so it is too hot for ordinary lifecycle ingestion and should remain unwired unless RimZ needs model-level filtering.

## Native-event mapping for a first adapter

The initial mapping onto RimZ's existing signal vocabulary is:

| Gemini observation | RimZ signal/enrichment | Notes |
| --- | --- | --- |
| `SessionStart` | `registered` | carry `session_id`, `transcript_path`; `source = clear` is a new session id |
| `BeforeAgent` | `turn_started` | carry `prompt`; this is the authoritative prompt boundary |
| `BeforeTool` / `AfterTool` for ordinary tools | `tool_used` | use `AfterTool`; completed non-mutating tools clear resolved waits without forcing durable activity, and mutating tools advance the phase |
| first `AfterTool` for `write_file` or `replace` | `tool_used { edits: true }` | moves reasoning → acting |
| `BeforeTool` with `ask_user` | `awaiting_input(Question)` | the tool opens its own interactive dialog |
| `BeforeTool` with `exit_plan_mode` | `awaiting_input(PlanApproval)` | current stable CLIs send `tool_input.plan_filename`; the pinned nightly surface uses `plan_path`, so tolerate both |
| `Notification.ToolPermission` | `awaiting_input(Permission)` | `ask_user` and `exit_plan_mode` duplicate the richer `BeforeTool` payload and should not open a second ask; every other current type is permission |
| `AfterAgent` neutral completion | `turn_ended { errored: false }` | response validation hooks can retry; RimZ's hook must stay neutral |
| `PreCompress` | `compacting` | retain `trigger` to close correctly later |
| next lifecycle observation after an open bracket | implicit bracket close | Gemini has no `PostCompress`; emit `compaction_ended` first using the stored opener trigger |
| `SessionEnd` | `ended` | best-effort cleanup; pane/process liveness remains the backstop |

Classify only `write_file` and `replace` as native file-edit tools. `run_shell_command` can mutate files but remains work without structured edit proof, matching RimZ's cross-provider phase rule. `write_todos`, tracker tools, and `update_topic` are progress, not human waits. `enter_plan_mode` asks for confirmation and is observed through `Notification` as an ordinary permission; `exit_plan_mode` is the formal plan-approval wait. `invoke_agent` is the subagent tool; it does not itself provide a child lifecycle id in the hook payload.

The waiting clear follows the shared model: a subsequent tool event, new turn, completion, or pane interruption proves the native prompt resolved. Gemini's `Notification` hook observes permission prompts but cannot grant them. Structured `rimz answer` support must target Gemini's native TUI dialog separately; ordinary text and choice answers continue through pane send.

## Session transcript JSONL

Gemini automatically records a project-scoped session under:

```text
~/.gemini/tmp/<project-identifier>/chats/session-<YYYY-MM-DDTHH-MM>-<first-8-session-id>.jsonl
```

The public docs call `<project-identifier>` a project hash. Current source obtains it through the storage/project registry and stores a SHA-256 `projectHash` inside the record. Use the hook-provided absolute `transcript_path` for a live session instead of reconstructing the directory.

The file is append-only JSONL in current releases. The first line carries session metadata:

```jsonc
{
  "sessionId": "full UUID",
  "projectHash": "SHA-256",
  "startTime": "ISO 8601",
  "lastUpdated": "ISO 8601",
  "kind": "main | subagent — optional; commonly absent for a main session",
  "directories": ["additional workspace directories — optional"]
}
```

Later lines are message records, metadata patches, or rewind markers. Load them in file order:

- A normal message has `id`, `timestamp`, `type`, `content`, and optional `displayContent`.
- `{ "$set": { ... } }` merges metadata. A `$set.messages` array is a full checkpoint that replaces the accumulated message map.
- `{ "$rewindTo": "message-id" }` removes that message and every later active message. A tail reader that ignores rewind markers can report abandoned-branch context.
- Legacy `.json` whole-record sessions migrate to `.jsonl` on resume. Parse both when walking historical spend.

Message `type` is `user`, `gemini`, `info`, `error`, or `warning`. `content` and `displayContent` use the Google GenAI `PartListUnion` shape, commonly a string or an array such as `[{"text":"..."}]`.

A Gemini message adds:

```jsonc
{
  "id": "message UUID",
  "timestamp": "ISO 8601",
  "type": "gemini",
  "content": [{ "text": "assistant text" }],
  "model": "gemini-3-pro-preview",
  "tokens": {
    "input": 12000,
    "output": 800,
    "cached": 5000,
    "thoughts": 300,
    "tool": 450,
    "total": 12800
  },
  "thoughts": [{ "subject": "...", "description": "...", "timestamp": "ISO 8601" }],
  "toolCalls": []
}
```

Token fields map directly from Google `usageMetadata`: `input = promptTokenCount`, `output = candidatesTokenCount`, `cached = cachedContentTokenCount`, `thoughts = thoughtsTokenCount`, `tool = toolUsePromptTokenCount`, and `total = totalTokenCount`. `cached` is part of prompt input, `thoughts` is reported alongside candidate usage, and `tool` is a prompt-component diagnostic; do not add the component fields together to derive `total`.

Gemini CLI itself restores the live context count from the newest active Gemini message's `tokens.total`. Current hosted Gemini models use a 1,048,576-token limit in `tokenLimits.ts`; Gemma 4 local models use 256,000. Unknown models currently default to 1,048,576. A RimZ adapter should retain the table as a refreshable model-limit rule and tolerate model routing changing `model` between messages.

Tool call records have this shape:

```jsonc
{
  "id": "tool-call-id",
  "name": "write_file",
  "args": { "file_path": "src/lib.rs", "content": "..." },
  "result": [{ "text": "..." }],
  "status": "success | error | validating | scheduled | executing | awaiting_approval | cancelled",
  "timestamp": "ISO 8601",
  "agentId": "subagent UUID — optional",
  "displayName": "WriteFile",
  "description": "...",
  "resultDisplay": "UI-specific optional payload",
  "renderOutputAsMarkdown": false
}
```

The exact scheduler status union can grow; deserialize it as a string and classify known terminal values. A message can be appended again with the same `id` when token or tool-call details arrive later. The official loader replaces the prior map entry, so a spend/context walk must deduplicate messages by id after applying rewinds and checkpoints.

### Context and spend rules

For the live context gauge, resolve the active transcript, apply its log semantics, select the newest active `type = "gemini"` message with tokens, use `tokens.total` as context tokens, and use that message's `model` for the divisor and row label.

For historical token/spend aggregation, sum each deduplicated active Gemini message's usage once. Metered Gemini API key and Vertex AI sessions require RimZ pricing-table multiplication because the transcript stores no dollar cost. Price uncached input as `input - cached`, cached input as `cached`, and output according to the provider pricing rule; confirm whether a model's billable output already includes thought tokens before adding a separate reasoning category. Google-login Code Assist sessions are quota/subscription traffic rather than API-key pay-as-you-go traffic, so token counts remain insight while dollar spend should not be inferred without an upstream billing contract.

## Subagents

Gemini subagents run as tools inside the parent session with independent context windows and restricted tool registries. Built-ins include `codebase_investigator`, `cli_help`, and `generalist`; custom definitions live in `.gemini/agents/*.md` or `~/.gemini/agents/*.md`. The main agent invokes them through `invoke_agent` with `agent_name` and a task prompt. Subagents cannot recursively invoke subagents.

Each local subagent receives a random UUID and records its conversation at:

```text
~/.gemini/tmp/<project-identifier>/chats/<full-parent-session-id>/<subagent-session-id>.jsonl
```

Its metadata uses `kind: "subagent"` and `sessionId: <subagent UUID>`. The parent transcript's `invoke_agent` tool call can carry the same UUID in `toolCalls[].agentId`; `toolCalls[].args.agent_name` identifies the type and task. This provides the correlation chain `(parent transcript path, invoke_agent.agentId) → child transcript`.

There is no `SubagentStart` or `SubagentStop` command-hook event and the common hook input contains no parent id or agent name. Before declaring `Capabilities.subagents`, verify live behavior for hooks inside local subagent contexts. If child hooks fire, recover the parent from the nested transcript path and enrich the type from the parent's `invoke_agent` record. If they do not, a bounded transcript watcher can discover child creation and terminal tool-call status, but its latency and failure modes must be explicit.

## Authentication, account, and quota

Gemini CLI selects one of these auth types, persisted as `security.auth.selectedType` in settings:

| Value | User-facing method | Metering implication |
| --- | --- | --- |
| `oauth-personal` | Sign in with Google / Gemini Code Assist | quota/tier-backed |
| `gemini-api-key` | Gemini API key from `GEMINI_API_KEY` | pay-as-you-go or API free tier |
| `vertex-ai` | Vertex AI via ADC, service account, or environment | Google Cloud billing/quota |
| `cloud-shell` | legacy Cloud Shell | environment-provided |
| `compute-default-credentials` | Compute metadata ADC | environment-provided |
| `gateway` | custom `GOOGLE_GEMINI_BASE_URL` gateway | gateway-defined |

Environment selection checks `GOOGLE_GENAI_USE_GCA=true`, then `GOOGLE_GENAI_USE_VERTEXAI=true`, then `GOOGLE_GEMINI_BASE_URL`, then `GEMINI_API_KEY`. Vertex setup also uses `GOOGLE_CLOUD_PROJECT` / `GOOGLE_CLOUD_PROJECT_ID`, `GOOGLE_CLOUD_LOCATION`, and optionally `GOOGLE_APPLICATION_CREDENTIALS`.

Google-login OAuth credentials historically live at `~/.gemini/oauth_creds.json` as Google `Credentials` fields such as `access_token`, `refresh_token`, `token_type`, `scope`, and `expiry_date` in epoch milliseconds. Current releases migrate them into secure storage under service `gemini-cli-oauth`, account `main-account`, and delete the legacy file. On Linux without a working keyring, the shared hybrid token storage may use its encrypted-file backend; inspect the pinned storage implementation before writing an idle credential probe. `~/.gemini/google_accounts.json` is non-secret identity metadata:

```json
{ "active": "user@example.com", "old": ["prior@example.com"] }
```

The active email can label the account but does not prove that a usable credential exists. API-key and Vertex auth are primarily environment/ADC surfaces, so a logged-in probe must evaluate the selected auth type together with its required credential source.

### Code Assist quota surface

For `oauth-personal`, Gemini CLI calls the source-only Code Assist API at `POST https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota` with OAuth authorization and:

```json
{ "project": "projects-or-managed-project-identifier", "userAgent": "optional" }
```

The response is:

```jsonc
{
  "buckets": [{
    "modelId": "gemini model id",
    "tokenType": "quota bucket type — optional",
    "remainingAmount": "integer encoded as a string — optional",
    "remainingFraction": 0.0,
    "resetTime": "timestamp — optional"
  }]
}
```

Gemini derives a limit as `remainingAmount / remainingFraction` when both are present. With only `remainingFraction`, it normalizes the bucket to a 0–100 scale. It pools the active Pro and Flash buckets for the UI and uses the furthest reset time. `/stats` refreshes this quota and `/stats model` displays model usage and limits.

The project identifier and plan tier come from `loadCodeAssist`. `LoadCodeAssistResponse` contains `currentTier`, `paidTier`, `allowedTiers`, `ineligibleTiers`, and `cloudaicompanionProject`; a tier contains `id`, `name`, descriptive/eligibility fields, and optional Google One AI credits. Known tier ids are `free-tier`, `legacy-tier`, and `standard-tier`, while the type deliberately accepts unknown strings.

`retrieveUserQuota`, `loadCodeAssist`, tier fields, and secure credential storage are official open-source implementation surfaces without a public stability guarantee. A RimZ account probe should fail soft on unknown buckets and tiers, preserve valid windows independently, never expose tokens, and keep API-key/Vertex quota handling separate from Code Assist quota handling.

## Headless and automation stream

`gemini -p <prompt>` forces non-interactive mode. `--output-format json` returns one object with `session_id`, `response`, `stats`, optional `error`, and optional `warnings`. `--output-format stream-json` emits JSONL events suitable for supervised-run streaming:

```jsonc
{ "type": "init", "timestamp": "ISO", "session_id": "UUID", "model": "model id" }
{ "type": "message", "timestamp": "ISO", "role": "user | assistant", "content": "text", "delta": true }
{ "type": "tool_use", "timestamp": "ISO", "tool_name": "write_file", "tool_id": "id", "parameters": {} }
{ "type": "tool_result", "timestamp": "ISO", "tool_id": "id", "status": "success | error", "output": "text", "error": { "type": "...", "message": "..." } }
{ "type": "error", "timestamp": "ISO", "severity": "warning | error", "message": "..." }
{ "type": "result", "timestamp": "ISO", "status": "success | error", "error": { "type": "...", "message": "..." }, "stats": {} }
```

Final stream stats carry `total_tokens`, `input_tokens`, `output_tokens`, `cached`, uncached `input`, `duration_ms`, `tool_calls`, and `models[model]` with the same token split. The non-stream JSON `stats` uses the richer `SessionMetrics` shape: per-model API request/error/latency and token totals, per-tool results/decisions/duration, and lines added/removed.

Documented process exits are 0 success, 1 general/API failure, 42 invalid input/arguments, and 53 maximum session turns exceeded. Preserve the actual process code as the supervised-run verdict; a stream `error` event can be a non-fatal warning, and the terminal `result.status` is the structured completion.

## CLI and environment surface

| Surface | Meaning for RimZ |
| --- | --- |
| `gemini --version` | adapter version probe |
| `gemini [query]` | interactive pane; a query starts the first turn |
| `gemini -i <query>` | run a prompt, then remain interactive |
| `gemini -p <query>` | headless turn |
| `gemini -r [latest|index|UUID] [query]` | resume a project session, optionally start a turn |
| `gemini --model <id>` | startup model; wins over environment/settings |
| `GEMINI_MODEL` | model override below the CLI flag |
| `model.name` | persisted model selection; default is `auto` |
| `gemini --approval-mode <default|auto_edit|yolo|plan>` | permission-mode mapping |
| `gemini --yolo` | auto-approve all tools |
| `gemini --allowed-tools <names>` | bypass confirmation for selected tools |
| `gemini --sandbox` | sandboxed tool execution |
| `gemini --include-directories <paths>` | additional workspace roots |
| `gemini --worktree [name]` | upstream experimental worktree creation; keep distinct from RimZ-owned worktrees |
| `gemini --list-sessions` / `--delete-session` | project session maintenance |
| `gemini --output-format <text|json|stream-json>` | supervised output selection |
| `gemini --acp` | alternate Agent Client Protocol server over stdio |
| `NO_COLOR` | disable ANSI output where supported |

Model selection precedence is `--model`, `GEMINI_MODEL`, `model.name`, experimental local routing, then `auto`. Auto routing and fallback can change the model used between requests, so the transcript message's model is authoritative for token pricing and live display.

Permission modes map naturally to RimZ launch suffixes: `default` → ask, `auto_edit` → auto for edit tools, `plan` → plan, and `yolo` → bypass. Gemini plan mode can automatically transition to implementation and route between Pro and Flash; model display must follow observed transcript data rather than the launch flag alone.

## ACP mode index

`gemini --acp` turns the CLI into a JSON-RPC 2.0 server over stdin/stdout for IDE clients. It supports `initialize`, `authenticate`, `newSession`, `loadSession`, `prompt`, `cancel`, `setSessionMode`, and the unstable session-model setter, plus a client-proxied filesystem.

ACP is useful if RimZ later builds a dedicated programmatic Gemini surface. It is not an out-of-band read API for a separately running TUI session: starting ACP creates an owned server process and moves prompt, approval, and filesystem control to the client. The first interactive adapter therefore stays on hooks, pane I/O, and durable session records.

## Implementation checklist

1. Add a typed `gemini` descriptor with eager `SessionStart` registration, hook support, resume support, and subagents disabled until child-hook behavior is verified.
2. Install one RimZ-owned command hook for `SessionStart`, `SessionEnd`, `BeforeAgent`, `AfterAgent`, `BeforeTool`, `AfterTool`, `Notification`, and `PreCompress`; keep model hooks unwired.
3. Include the full installed hook definitions and command paths in project trust hashing, and preflight `hooksConfig.enabled`, disabled hook names, folder trust, and merged settings.
4. Parse the common hook envelope strictly around `session_id`; keep neutral stdout as `{}` and all logging on stderr.
5. Map lifecycle events with the table above, close the one-sided compression bracket on the next event, and add golden tests for question, plan, permission, edit, retry-safe completion, clear, resume, and session end.
6. Implement the JSONL fold with message-id replacement, `$set.messages`, `$rewindTo`, legacy `.json`, and optional fields before using it for context or spend.
7. Use the hook's `transcript_path` for live context and scan project chat trees only for historical spend and subagent correlation.
8. Treat OAuth secure storage and Code Assist quota as a separate, fail-soft account probe; do not read or log secret values beyond the minimum bearer-token operation.
9. Drive supervised runs with `-p --output-format stream-json`, preserve native exit codes, and test stdin-plus-prompt behavior against the target Gemini version.
10. Live-verify the remaining upstream gaps: `AfterAgent` coverage on API error, Esc interruption evidence, hook firing inside subagents, notification variants through the native UI, shell/background-tool completion, and session-id behavior across `/clear` and `/rewind`.

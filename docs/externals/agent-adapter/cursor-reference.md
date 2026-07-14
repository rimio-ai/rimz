# Cursor CLI protocol reference

> This document records the upstream surface behind RimZ's Cursor adapter; the implemented mapping and explicit deferrals are in [cursor.md](../../internals/agents/cursor.md), the agent-agnostic lifecycle and enrichment contracts are [model.md](../../internals/agents/model.md), and the account/spend contract is [providers.md](../../internals/agents/providers.md).

This is the single home for the **Cursor CLI upstream protocol surface** relevant to RimZ — local hooks and their decision channel, conversation identity, context and subagent payloads, interactive and headless launch modes, stream JSON, ACP, authentication, configuration, permissions, and the documented local-state boundary. It is an implementation research record, not a claim that RimZ currently supports Cursor.

Refresh baseline: the official Cursor documentation and CLI changelog available on **2026-07-10**, plus the installed **2026.07.09-a3815c0** build used to capture the command-statusline wire. Cursor auto-updates by default and identifies installed builds with `agent --version`; re-capture the exact supported binary when this record is refreshed because the public docs are rolling rather than versioned.

Coverage is **depth on surfaces an adapter should wire, breadth as an index**. Hook inputs and outputs are recorded in implementation detail because they are the strongest stock-UI seam. The `2026.07.09-a3815c0` transcript capture pins only file identity and terminal rows; assistant text, authentication JSON, local chat storage, historical cost accounting, and native permission prompts remain opaque where Cursor publishes no safe schema.

## Upstream sources

Re-fetch these pages and compare the latest CLI changelog before implementing or refreshing the adapter.

| Surface | Official source |
| --- | --- |
| CLI overview, modes, sessions, worktrees | <https://cursor.com/docs/cli/overview.md> · <https://cursor.com/docs/cli/using.md> |
| CLI changelog | <https://cursor.com/docs/cli/changelog.md> |
| Installation and updates | <https://cursor.com/docs/cli/installation.md> |
| Commands and launch options | <https://cursor.com/docs/cli/reference/parameters.md> |
| Slash commands | <https://cursor.com/docs/cli/reference/slash-commands.md> |
| Hooks, payloads, outputs, configuration | <https://cursor.com/docs/hooks.md> |
| Third-party hook compatibility | <https://cursor.com/docs/reference/third-party-hooks.md> |
| Headless mode and structured output | <https://cursor.com/docs/cli/headless.md> · <https://cursor.com/docs/cli/reference/output-format.md> |
| ACP server and Cursor extensions | <https://cursor.com/docs/cli/acp.md> · <https://agentclientprotocol.com/> |
| Authentication | <https://cursor.com/docs/cli/reference/authentication.md> |
| CLI configuration and permissions | <https://cursor.com/docs/cli/reference/configuration.md> · <https://cursor.com/docs/cli/reference/permissions.md> |
| Subagent behavior and definitions | <https://cursor.com/docs/subagents.md> |
| Agent run modes and security | <https://cursor.com/docs/agent/security/run-modes.md> · <https://cursor.com/docs/agent/security.md> |
| Models and pricing | <https://cursor.com/docs/models-and-pricing.md> |

The authoritative local companion to these rolling pages is the installed executable:

```sh
agent --version
agent --help
agent status --format json
agent about --format json
agent models
agent mcp list
```

The current primary executable is `agent`. `cursor-agent` is a backward-compatible alias introduced before `agent` became the primary entry point; probe both names during discovery and launch the resolved executable rather than assuming either path.

## Adapter feasibility at a glance

Use **command hooks** for local interactive lifecycle and context observations. They preserve Cursor's stock terminal UI, carry stable conversation and generation IDs, expose model parameters and transcript paths, and run as local child processes over JSON stdin/stdout.

Use **pane liveness** for presence and process exit, as with every standalone CLI. The docs describe hooks as spawned processes and provide no daemon-routing exception. Confirm with a process-tree fixture that a Cursor CLI hook inherits the pane's RimZ environment and that `$PPID` or ancestor recovery reaches the in-pane `agent` process.

Use **headless `-p` mode** for supervised runs. Its `stream-json` transport has a published schema, stock process exit semantics, session IDs, complete tool brackets, and a terminal result. Use **ACP** only when a supervised integration needs structured permission, question, or plan-approval requests; ACP makes RimZ the client UI and is therefore the wrong primary seam for an ordinary interactive pane.

The decisive limitation for a first adapter is **awaiting-user coverage**. Cursor's local hook catalog publishes no permission-request, question, plan-approval, or notification hook. Generic `preToolUse` is documented to fire before tools, but its payload says what the model wants to call, not whether Cursor's native permission UI is open. Returning `"ask"` is either unsupported or actively changes policy depending on the event. Do not infer `waiting` from a pre-tool event. ACP exposes the missing asks, but only when RimZ hosts the ACP client.

The candidate transport matrix is:

| RimZ concern | Primary upstream surface | Backstop / gap |
| --- | --- | --- |
| session start and identity | `sessionStart` hook; common `conversation_id` | pane presence before the hook |
| turn start and prompt | `beforeSubmitPrompt` | no documented event for a queued follow-up before submission |
| tool activity and acting phase | `postToolUse`; `tool_name` and `tool_use_id` | specialized after-hooks are redundant enrichment |
| turn completion, abort, error | `stop.status` | pane liveness if a process dies before `stop` |
| session end | `sessionEnd` plus pane liveness | `sessionEnd` is fire-and-forget |
| permission ask | none for stock local CLI | ACP `session/request_permission` only in hosted mode |
| user question | none for stock local CLI | ACP `cursor/ask_question` only in hosted mode |
| plan approval | none for stock local CLI | ACP `cursor/create_plan` only in hosted mode |
| compaction start and live context | command `statusLine` payload; `preCompact` | no documented post-compaction hook |
| subagent start | `subagentStart.subagent_id` | strong unique child identity |
| subagent stop | `subagentStop` | published stop payload omits `subagent_id`; capture correlation before wiring |
| model and effort | common hook `model_id` and `model_params` | `model` is the legacy slug |
| transcript | common `transcript_path`, exact per-conversation JSONL | captured `turn_ended` tail only; assistant text is privacy-unsafe |
| supervised streaming | `-p --output-format stream-json` | failures may end without a terminal JSON event |
| auth/account | `status --format json`, `about --format json` | official docs publish no JSON response schema or credential path |
| tokens, cost, quota | statusline and stop-hook input/output/cache split | API-equivalent live-session estimate only; historical spend, billing, and quota remain absent |

## Session identity, resume, fork, and clear

The stable root session key is `conversation_id`. Every agent-session hook receives it, and `sessionStart.session_id` is explicitly the same value. `generation_id` changes with each user message and is the natural turn correlation key; never use it as the RimZ `agent_id`.

Cursor exposes these interactive session operations:

| Operation | CLI surface | Identity implication |
| --- | --- | --- |
| resume a chosen conversation | `agent --resume <chat-id>` or the `agent ls` picker | preserves `conversation_id` |
| resume latest | `agent resume` | preserves the chosen conversation ID |
| continue previous | `agent --continue` (`--resume=-1`) | preserves the chosen conversation ID |
| clear/new | `/clear` (`/new`, `/new-chat`, `/newchat` aliases) | starts a new conversation; expect a new `conversation_id` |
| fork | `/fork` | starts a new session lineage; capture the new ID and any parent evidence |
| summarize | `/summarize` (`/compress` alias) | preserves the conversation ID and triggers `preCompact` when covered |
| rewind | `/rewind` | restores an earlier message in the same session; no hook identity contract is published |
| create empty chat | `agent create-chat` | returns a chat ID; output schema is undocumented |

`agent ls`, `agent --resume`, and `/resume` can browse chats across workspaces in current releases. A resumed conversation may therefore report workspace roots different from the pane's launch cwd. Bind by the stamped pane/session relationship first and treat hook `workspace_roots` as context, not as the owner identity.

The docs publish no parent-conversation field for `/fork`, no structured list output for `agent ls`, and no local chat-store schema. Capture all three before implementing session restoration or supersession from provider state.

## Hooks

Cursor command hooks are spawned processes. Each receives one JSON object on **stdin** and returns one JSON object on **stdout**. RimZ's helper must reserve stdout for the native decision response and send diagnostics to stderr or RimZ state logs.

### Discovery, priority, and trust

Cursor loads all matching hooks from every active source and merges responses. When values conflict, source priority is:

```text
Enterprise → Team → Project → User
```

When the account feature **Third-party skills** is enabled, Cursor also loads Claude Code hook files below every native Cursor tier. The complete documented order is Enterprise → Team → Cursor project → Cursor user → Claude project-local → Claude project → Claude user. Supported Claude events map as `PreToolUse` → `preToolUse`, `PostToolUse` → `postToolUse`, `UserPromptSubmit` → `beforeSubmitPrompt`, `Stop` → `stop`, `SubagentStop` → `subagentStop`, `SessionStart` → `sessionStart`, `SessionEnd` → `sessionEnd`, and `PreCompact` → `preCompact`; Claude `Notification` and `PermissionRequest` have no Cursor mapping.

This compatibility path creates a RimZ collision to test explicitly: a machine with RimZ's Claude hooks installed may run those commands inside Cursor as well. The official compatibility page documents event, tool-name, exit-code, and response translation but does not promise that hook **input** is rewritten into Claude's schema. A Cursor adapter install must detect this setting and prove whether existing `--source claude` hooks fire on Cursor sessions; suppress, distinguish, or safely ignore cross-fired payloads before both adapters can be enabled together.

| Source | Location | Working directory | RimZ implication |
| --- | --- | --- | --- |
| Enterprise | macOS `/Library/Application Support/Cursor/hooks.json`; Linux/WSL `/etc/cursor/hooks.json`; Windows `C:\ProgramData\Cursor\hooks.json` | enterprise config directory | managed, highest priority |
| Team | distributed from the Cursor dashboard | managed hooks directory | enterprise-only managed executable surface |
| Project | `<project>/.cursor/hooks.json` | project root | runs in trusted workspaces; include in project trust |
| User | `~/.cursor/hooks.json` | `~/.cursor/` | suitable for RimZ's machine-level install |

Project hooks are committed executable configuration and run automatically in a trusted workspace. RimZ's trust hash must cover `.cursor/hooks.json`, hook commands and scripts, `.cursor/cli.json` permission policy, MCP commands, worktree setup commands, plugins, rules, and every other Cursor configuration field that can execute or load code.

Use a bounded RimZ-owned entry in the user's `~/.cursor/hooks.json`. The file may already contain user hooks, so install and uninstall through a structured JSON merge with a visible diff. Cursor watches the file and reloads it on save; a restart is the documented troubleshooting fallback.

The docs specify that all matching hooks run but publish no stable execution order among multiple definitions at one source. Cursor's changelog says hooks execute in parallel with merged responses. Observation hooks must therefore be independent, idempotent, and neutral; never rely on ordering against another hook.

### Configuration shape

The config schema version is `1`:

```json
{
  "version": 1,
  "hooks": {
    "sessionStart": [{ "command": "rimz hooks feed --source cursor" }],
    "beforeSubmitPrompt": [{ "command": "rimz hooks feed --source cursor" }],
    "postToolUse": [{ "command": "rimz hooks feed --source cursor" }],
    "postToolUseFailure": [{ "command": "rimz hooks feed --source cursor" }],
    "afterAgentResponse": [{ "command": "rimz hooks feed --source cursor" }],
    "stop": [{ "command": "rimz hooks feed --source cursor" }],
    "sessionEnd": [{ "command": "rimz hooks feed --source cursor" }],
    "preCompact": [{ "command": "rimz hooks feed --source cursor" }]
  }
}
```

Install an absolute, safely quoted RimZ path in production. Cursor accepts a shell string, absolute path, or relative path, but the docs do not name the shell, quoting rules, or Windows command interpreter. Verify spaces, quotes, symlinks, and Windows paths on every supported platform.

Per-hook fields are:

| Field | Type / default | Contract |
| --- | --- | --- |
| `command` | string, required for command hooks | executable path or shell command |
| `type` | `"command"` or `"prompt"`, default `"command"` | prompt hooks invoke an LLM and are unsuitable for deterministic RimZ observation |
| `timeout` | seconds, platform default | kill/failure behavior follows fail-open or `failClosed` |
| `loop_limit` | number or `null`, default `5` | bounds auto-follow-ups from `stop` and `subagentStop` |
| `failClosed` | boolean, default `false` | blocks the guarded action when a hook crashes, times out, or returns invalid JSON |
| `matcher` | documented as a filter; examples use a string | matches an event-specific value |

Leave `failClosed` false for RimZ observation. A dashboard outage must not block Cursor's tools. Leave `loop_limit` unused and return no follow-up message; a lifecycle observer must not create turns.

### Common input

Every agent-session hook receives these base fields in addition to its event-specific fields:

```json
{
  "conversation_id": "string",
  "generation_id": "string",
  "model": "string",
  "model_id": "string",
  "model_params": [{ "id": "string", "value": "string" }],
  "hook_event_name": "string",
  "cursor_version": "string",
  "workspace_roots": ["/path"],
  "user_email": "string | null",
  "transcript_path": "string | null"
}
```

| Field | Adapter use |
| --- | --- |
| `conversation_id` | required root `agent_id`; quarantine a lifecycle event that omits it |
| `generation_id` | per-user-message turn correlation and dedupe input |
| `model` | legacy configured model slug; fallback label only |
| `model_id` | preferred canonical model ID when present |
| `model_params` | structured options such as `thinking`, `context`, and `effort`; ignore unknown IDs |
| `hook_event_name` | native event discriminator |
| `cursor_version` | capture and diagnostics; use for deliberate version gates |
| `workspace_roots` | enrichment for single- or multi-root workspaces |
| `user_email` | account enrichment; sensitive and unnecessary in lifecycle records |
| `transcript_path` | optional sidecar source after its file schema is captured |

`workspaceOpen` fires outside a session and omits `conversation_id`, `generation_id`, `model`, `session_id`, and `transcript_path`. It is not a lifecycle event.

Hook processes also receive `CURSOR_PROJECT_DIR`, `CURSOR_VERSION`, optional `CURSOR_USER_EMAIL`, optional `CURSOR_TRANSCRIPT_PATH`, `CURSOR_CODE_REMOTE="true"` in a remote workspace, and the Claude-compatibility alias `CLAUDE_PROJECT_DIR`. Environment returned by `sessionStart` becomes available to later hooks in that session.

Do not persist `user_email`, prompts, tool inputs, tool outputs, file contents, thought text, or transcript contents merely because the wire exposes them. RimZ's privacy surface decides which content enters durable records.

### Events a first adapter should wire

| Event | Fires | Event-specific fields | RimZ mapping |
| --- | --- | --- | --- |
| `sessionStart` | a new conversation is created | `session_id`, `is_background_agent`, optional `composer_mode` | `Registered`; verify `session_id == conversation_id` |
| `beforeSubmitPrompt` | after send, before backend request | `prompt`, `attachments[]` | `TurnStarted`, task/prompt subject to privacy policy |
| `postToolUse` | a tool succeeds | `tool_name`, `tool_input`, `tool_output`, `tool_use_id`, `cwd`, `duration` | `ToolUsed`; heartbeat; `edits` from a pinned tool-name table |
| `postToolUseFailure` | a tool errors, times out, or is denied | `tool_name`, `tool_input`, `tool_use_id`, `cwd`, `error_message`, `failure_type`, `duration`, `is_interrupt` | heartbeat/diagnostic only; a tool failure is not necessarily turn death |
| `afterAgentResponse` | the assistant produces its final visible response | `text` | safe assistant content only; never a turn boundary |
| `stop` | the main agent loop ends | `status: completed | aborted | error`, input/output/cache token fields, `loop_count` | completed/error end the turn; aborted is an interruption |
| `sessionEnd` | a conversation ends | `session_id`, `reason`, `duration_ms`, `is_background_agent`, `final_status`, optional `error_message` | `Ended` tombstone |
| `preCompact` | automatic or manual summarization begins | trigger and live context fields | `Compacting` plus `AgentContext` refresh |
| `subagentStart` | before a Task subagent spawns | unique child ID, type, task, parent, call ID, model, parallel bit, optional branch | child `SubagentStarted` |
| `subagentStop` | a subagent completes, errors, or aborts | type, status, task, description, summary, metrics, modified files, transcript path | candidate `SubagentStopped` after identity correlation is proven |

`postToolUse` is the preferred heartbeat because it proves completed activity. `preToolUse` is useful for governance but touches no heartbeat: the tool may still be blocked or awaiting approval.

Use a conservative, captured `tool_edits_files` table. The matcher docs name `Shell`, `Read`, `Write`, `Grep`, `Delete`, `Task`, and `MCP:<tool_name>`. At minimum `Write` and `Delete` edit files; do not classify `Shell` or an arbitrary MCP tool as editing even though it may mutate the workspace.

`stop.status == "aborted"` is a native turn-interruption certificate and lands the shared lifecycle at idle. `postToolUseFailure.is_interrupt` is tool-grained and must not by itself end the turn.

### Compaction and context

`preCompact` is the richest documented context payload:

```json
{
  "trigger": "auto",
  "context_usage_percent": 85,
  "context_tokens": 120000,
  "context_window_size": 128000,
  "message_count": 45,
  "messages_to_compact": 30,
  "is_first_compaction": true
}
```

Map `context_usage_percent`, `context_tokens`, and `context_window_size` directly into a context sidecar after range validation. Record the compaction start with its `auto` or `manual` trigger.

Cursor publishes no `postCompact` event. Do not close the RimZ compaction bracket merely because the `preCompact` command returned: the hook is called before summarization. Before implementation, capture whether the next `beforeSubmitPrompt`, response, or tool event proves compaction completion and define one provider-specific close rule with tests. Until then, capability reporting must mark compaction completion unsupported rather than leaving a permanent pulsing head.

Cursor CLI `2026.07.09-a3815c0` supports a command statusline in `~/.cursor/cli-config.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "rimz statusline feed --source cursor"
  }
}
```

The command receives structured JSON on stdin with `session_id`, `session_name`, `model.{id,display_name,param_summary,max_mode}`, `version`, `output_style`, `vim`, and `context_window.{context_window_size,used_percentage,remaining_percentage,current_usage}`. Current usage separates input, output, cache-create, and cache-read tokens. Before the first prompt, `session_name` can contain Cursor-owned presentation text; use prompt lifecycle evidence rather than matching that text to decide whether the session is named. Treat every nested field as optional and field-locally lossy because the rolling CLI schema is not versioned.

An explicit-model capture from `2026.07.09-a3815c0` reported `model.display_name = "GPT-5.6 Sol 272K Medium"`, `model.param_summary = "272K Medium"`, and `context_window.context_window_size = 200000`. The `272K` parameter describes the nominal model selector while the independent `200000` field is the live usable window and fill denominator; consumers separate the summary suffix into model qualifiers and reasoning effort without promoting its nominal magnitude to live token usage.

Cursor invokes the configured command as direct argv. Split shell-style quotes, expand a leading `~` in the program, and preserve shell metacharacters as literal arguments; do not insert `sh -c`. The statusline is the live context authority, while `preCompact` remains the compaction signal and a fallback source for window occupancy. Pane reads stay out of producer enrichment.

### Subagents

`subagentStart` provides enough identity for a proper child row:

```json
{
  "subagent_id": "abc-123",
  "subagent_type": "explore",
  "task": "Explore the authentication flow",
  "parent_conversation_id": "conv-456",
  "tool_call_id": "tc-789",
  "subagent_model": "claude-sonnet-4-20250514",
  "is_parallel_worker": false,
  "git_branch": "feature/auth"
}
```

Use `subagent_id` as the child `agent_id`, `parent_conversation_id` as the parent, `subagent_type` as the durable child label, and `task` as the child task. `tool_call_id` is a useful correlation key but not child identity.

The published `subagentStop` shape omits `subagent_id`, `parent_conversation_id`, and `tool_call_id`. Its common `conversation_id` may name the parent or the child; the docs do not say. Do not correlate concurrent children by type or task text. Capture start and stop payloads for sequential and concurrent same-type children; wire stop only after one stable unique key or transcript-path join is proven.

Cursor supports foreground and background subagents, parallel execution, preserved checkpoints across resume, and nested children within a depth limit. Background subagent files live under `~/.cursor/subagents/`, but the docs publish no file schema. Treat that directory as opaque until fixtures establish durability and identity.

### Full hook catalog and why the rest stay out of lifecycle

| Event | Relevant content | Adapter treatment |
| --- | --- | --- |
| `preToolUse` | generic tool request, model options, agent message | optional audit; never infer waiting |
| `beforeShellExecution` | command, cwd, sandbox state | governance decision seam; generic post-hook already covers activity |
| `afterShellExecution` | command, full output, duration, sandbox | redundant heartbeat; sensitive output |
| `beforeMCPExecution` | tool name, JSON params, server URL or command | governance; sensitive inputs |
| `afterMCPExecution` | tool name/input, full JSON result, duration | redundant heartbeat; sensitive result |
| `beforeReadFile` | absolute path, full contents, attachments | access-control seam; avoid persisting content |
| `afterFileEdit` | absolute path and old/new strings | redundant acting proof; avoid persisting source text |
| `afterAgentResponse` | final assistant text | sole safe assistant-text source; content only, never a turn boundary |
| `afterAgentThought` | aggregated thinking text and optional duration | exclude from lifecycle and durable transcript by default |
| `beforeTabFileRead`, `afterTabFileEdit` | editor inline-completion activity | not Cursor CLI agent lifecycle |
| `workspaceOpen` | workspace roots and optional plugin paths | app lifecycle and executable loading, not session lifecycle |

Specialized before/after hooks may remain useful for trust enforcement, but installing all of them for observation duplicates events and increases latency and content exposure. A first RimZ adapter needs only the minimal lifecycle set.

### Matchers

Cursor documents these matcher inputs:

| Hook | Matcher input |
| --- | --- |
| `preToolUse`, `postToolUse`, `postToolUseFailure` | tool type such as `Shell`, `Read`, `Write`, `Grep`, `Delete`, `Task`, or `MCP:<tool_name>` |
| `subagentStart`, `subagentStop` | subagent type |
| `beforeShellExecution`, `afterShellExecution` | full command text |
| `beforeReadFile` | tool type |
| `afterFileEdit` | tool type |
| `beforeSubmitPrompt` | literal `UserPromptSubmit` |
| `stop` | literal `Stop` |
| `afterAgentResponse` | literal `AgentResponse` |
| `afterAgentThought` | literal `AgentThought` |

The docs call the field an object in one options table but every example supplies a string. The compatibility page calls tool matchers regular-expression patterns, but Cursor does not specify the dialect, anchoring, case sensitivity, or invalid-pattern behavior. Prefer no matcher for RimZ's bounded lifecycle hooks and filter parsed event names in the adapter.

### Hook outputs and neutral decisions

The native neutral response for a RimZ observation hook is `{}` with exit `0`. Golden-test this exact stdout for every event. Empty stdout is not explicitly documented as valid JSON, so prefer `{}` over silence.

Decision-capable outputs include:

| Event | Output | Important boundary |
| --- | --- | --- |
| `preToolUse` | `permission: allow | deny`, optional messages, optional `updated_input` | `ask` parses but is not enforced today |
| `beforeShellExecution`, `beforeMCPExecution` | `permission: allow | deny | ask`, optional user/agent messages | returning `ask` changes behavior; RimZ returns neutral |
| `beforeReadFile` | `permission: allow | deny`, optional user message | observation returns neutral |
| `beforeSubmitPrompt` | `continue`, optional user message | `false` blocks prompt submission |
| `subagentStart` | `permission: allow | deny`, optional user message | `ask` is treated as deny |
| `stop`, `subagentStop` | optional `followup_message` | creates another turn; RimZ never emits it for observation |
| `sessionStart` | optional `env`, `additional_context` | fire-and-forget; blocking fields are accepted but not enforced |
| `preCompact` | optional `user_message` | observational; cannot block or modify compaction |
| `workspaceOpen` | optional `pluginPaths[]` | loads executable plugin directories |

`postToolUse` may return `updated_mcp_tool_output` for MCP results and `additional_context`; other post-events are observational. RimZ must not alter results or inject context through an observation hook.

### Exit and failure semantics

| Result | Behavior |
| --- | --- |
| exit `0` with valid JSON | merge and apply the hook response |
| exit `2` | block the action, equivalent to `permission: "deny"` |
| other non-zero | hook failure; action proceeds by default |
| crash, timeout, invalid JSON | action proceeds by default |
| any failure with `failClosed: true` | guarded action is blocked |

The docs do not publish stderr display behavior, stdout size limits, signal behavior, timeout defaults, merge semantics for arrays/objects, or what happens when one parallel hook returns invalid JSON while others succeed. Keep RimZ output tiny, finish quickly, and test the supported binary under conflicting third-party hooks.

### Awaiting-user gap

The official hook catalog has no event corresponding to RimZ's `AskKind::Permission`, `AskKind::Question`, or `AskKind::PlanApproval`.

`preToolUse` fires before a requested tool but cannot say whether the action was allowlisted, sandboxed, auto-reviewed, automatically denied, or presented in the native terminal UI. `beforeShellExecution` and `beforeMCPExecution` accept an `ask` output, but returning it would create a policy decision rather than observe Cursor's own decision. `beforeSubmitPrompt` observes the user's prompt, not an agent question.

A stock-pane adapter therefore declares `native_ask_ui` only after a supported, captured waiting detector exists. Until then it must abstain from `AwaitingUser` instead of painting false `?` rows. Pane capture remains a rendering or explicit-user primitive and must not become the producer's truth source.

ACP has first-class permission, question, and plan requests, described below. Those apply only to a RimZ-hosted ACP run and do not fill the stock interactive CLI hook gap.

## Transcript and local state

Cursor CLI `2026.07.09-a3815c0` writes per-conversation JSONL at `~/.cursor/projects/<workspace>/agent-transcripts/<conversation_id>/<conversation_id>.jsonl`. Hooks expose the same file through `transcript_path` and `CURSOR_TRANSCRIPT_PATH`; subagent stop separately exposes `agent_transcript_path`.

The authenticated native `--print --resume` capture rewrote that same path as a whole-conversation snapshot instead of appending a suffix: the original user/assistant/terminal three-row file became two user/assistant pairs followed by one terminal row, and the saved prefix hash changed. The minimized capture-backed row shapes are:

```json
{"role":"user","message":{"content":[{"type":"text","text":"<redacted>"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"<redacted>"}]}}
{"type":"turn_ended","status":"success"}
```

The terminal subset also admits installed-writer statuses `aborted` and `error` plus optional error text. RimZ models only the top-level terminal fields, stats and reads the bounded whole-file tail, and recovers an outcome only when the complete recognized terminal row is the last meaningful record with no torn suffix. A later nonterminal, unknown, malformed, or partial record suppresses the older outcome until a new complete terminal row or whole-file snapshot arrives. File mtime supplies the observation timestamp only after that at-rest proof. Path discovery joins the exact conversation directory and filename under each immediate project directory and rejects zero or multiple matches.

The same JSONL carries user, assistant, thinking, and tool records, but assistant `message.content[type=text]` blocks merge visible commentary and model thinking without a safe discriminator. RimZ never normalizes, pages, streams, persists, or uses those text blocks as final output. `afterAgentResponse.text` is the sole assistant-text authority.

Cursor still publishes no transcript enablement, append/rotation/durability, cross-surface compatibility, child-identity, or local chat-history contract. Resumed, continued, cleared, forked, summarized, and concurrent-subagent behavior remains a capture target; terminal-tail support does not imply full native-history support.

## Headless `--print` mode

Use `agent -p` or `agent --print` for one non-interactive run. The current default output format is `text`; select `--output-format json` or `stream-json` explicitly rather than depending on a default that has changed in earlier releases.

Print mode can access write and shell tools. `--force` or `--yolo` allows commands unless explicitly denied; permission allow/deny rules still apply. `--trust` accepts the workspace trust prompt in headless mode. RimZ must map its permission profiles deliberately rather than silently adding `--force` to every supervised run.

One authenticated `2026.07.09-a3815c0` `--mode=ask --print --resume` capture completed with the exact requested text but invoked only two byte-identical `sessionEnd` hook payloads. It emitted no `beforeSubmitPrompt`, `afterAgentResponse`, or `stop` payload, so this native headless transport provided no live response-hook or token-counter evidence. RimZ's Cursor hook coverage is scoped to ordinary interactive sessions; RimZ supervised runs use that interactive transport with a positional prompt and do not pass `-p` or `--print`. The installed hook bundle and deterministic fixtures, rather than this native-headless capture, pin the response and stop field schemas.

### JSON terminal result

Successful `--output-format json` prints one newline-terminated object:

```json
{
  "type": "result",
  "subtype": "success",
  "is_error": false,
  "duration_ms": 1234,
  "duration_api_ms": 1234,
  "result": "<full assistant text>",
  "session_id": "<uuid>",
  "request_id": "<optional request id>"
}
```

On failure the process exits non-zero, writes an error to stderr, and emits no guaranteed well-formed JSON result.

### Stream JSON

`--output-format stream-json` emits one JSON object per line. The published event sequence is:

| Type / subtype | Core fields | Supervised-run use |
| --- | --- | --- |
| `system/init` | auth source, cwd, session ID, model, permission mode | registration and launch metadata |
| `user` | role/content message and session ID | turn input |
| `assistant` | complete message segment and session ID | streamed answer segments |
| `tool_call/started` | call ID and tagged tool-call object | open tool bracket |
| `tool_call/completed` | same call ID plus tagged result | close bracket and heartbeat |
| `result/success` | durations, full result, session ID, optional request ID | terminal success |

Without `--stream-partial-output`, each `assistant` event is one complete message between tool calls. With it, only assistant events with `timestamp_ms` present and `model_call_id` absent contain new text. Events with both are pre-tool buffered duplicates; events with neither are final duplicates. Ignore unknown fields because Cursor explicitly permits backward-compatible additions.

Tool calls use tagged shapes such as `readToolCall` and `writeToolCall`; other tools may use `tool_call.function` with a name and JSON arguments. Parse the outer lifecycle structurally, retain unknown tagged calls, and never make an exhaustive enum that rejects a new tool.

The stream may end early on failure without a terminal result. Process exit, stderr, timeout, and the last complete record jointly decide the supervised-run outcome. Thinking events are suppressed in all print formats.

The headless `session_id` is documented as stable for one execution. Capture `--resume` in print mode before assuming it equals hook `conversation_id` across executions.

## ACP server

`agent acp` runs a newline-delimited JSON-RPC 2.0 server on stdio. Client requests go to stdin, protocol responses and notifications come from stdout, and logs may go to stderr. This separation matches RimZ's stdout-discipline requirements.

The normal flow is:

```text
initialize
authenticate(methodId = "cursor_login")
session/new or session/load
session/prompt
session/update …
session/request_permission …
optional session/cancel
```

ACP supports `agent`, `plan`, and `ask` modes, project/user `.cursor/mcp.json`, and session resume through `session/load`. Team-dashboard MCP servers are not supported in ACP mode.

Use the upstream ACP specification for standard request and update schemas. Cursor documents these extension methods:

| Method | Direction / blocking | Key contract |
| --- | --- | --- |
| `session/request_permission` | server request, blocking | client returns `allow-once`, `allow-always`, or `reject-once` |
| `cursor/ask_question` | server request, blocking | multiple questions/options; answer, skip, or cancel |
| `cursor/create_plan` | server request, blocking | markdown plan and todos; accept, reject, or cancel |
| `cursor/update_todos` | notification | replace or merge typed todo states |
| `cursor/task` | notification | subagent type, prompt, optional model/agent ID/duration |
| `cursor/generate_image` | notification | description, optional paths, generated result |

ACP is a strong candidate for RimZ `-p` runs that need native structured asks. It is not the default interactive adapter: hosting ACP would replace Cursor's stock terminal UI and move question, plan, permission, file, and terminal presentation responsibilities into RimZ.

ACP authentication advertises `cursor_login`; a process may also be pre-authenticated with `agent login`, `--api-key` / `CURSOR_API_KEY`, or the ACP-documented `--auth-token` / `CURSOR_AUTH_TOKEN`. The ACP page also shows endpoint and insecure-TLS options that are absent from the public global-options table; probe `agent acp --help` and the root help before relying on those flags.

## Authentication and account surface

Cursor CLI supports browser login and API keys:

```sh
agent login
agent status
agent logout

CURSOR_API_KEY=… agent -p "task"
agent --api-key … -p "task"
```

`NO_OPEN_BROWSER=1 agent login` prints the login URL instead of opening a browser. Current releases also support a QR-code login flow for remote terminals.

`agent status --format json` and `agent about --format json` are the documented machine-readable probes. The docs say status reports authentication, account information, and endpoint configuration, but publish no JSON schema, exit-code table, latency contract, or distinction between browser credentials and API-key auth. An authenticated browser-login capture on `2026.07.09-a3815c0` produced this sanitized `status` shape:

```json
{
  "status": "authenticated",
  "isAuthenticated": true,
  "hasAccessToken": true,
  "hasRefreshToken": true,
  "userInfo": {
    "email": "<redacted>",
    "userId": 0,
    "firstName": "<redacted>",
    "lastName": "<redacted>",
    "createdAt": "<redacted>"
  }
}
```

The same authenticated arm produced this sanitized `about --format json` shape. `subscriptionTier` and `userEmail` are non-empty strings for this browser-login arm, while `lastRequestId` was null:

```json
{
  "cliVersion": "2026.07.09-a3815c0",
  "model": "<redacted>",
  "subscriptionTier": "<redacted>",
  "osPlatform": "<redacted>",
  "osArch": "<redacted>",
  "userEmail": "<redacted>",
  "terminalProgram": "<redacted>",
  "shell": "<redacted>",
  "lastRequestId": null
}
```

The unauthenticated, expired, API-key, service-account, proxy, and server-error arms remain unverified. RimZ therefore recognizes only explicit authenticated and logged-out facts, rejects contradictory or schema-unknown status, requires a successful JSON `about` call after positive authentication, and treats every other arm as retryable unavailable. Matching status/about emails produce account identity; `subscriptionTier` and `cliVersion` remain optional, and a missing tier never implies API-key auth.

Cursor does not document the credential file or secure-store schema. Treat browser credentials as opaque and use the CLI probe; never read or copy secrets directly. The changelog documents `AGENT_CLI_CREDENTIAL_STORE=file` for sandboxed environments, where credentials are stored unencrypted in an owner-only file, but does not publish that file's schema or path.

The common hook field `user_email` can enrich a known logged-in account but is neither an auth probe nor safe to use as session identity.

## Usage, tokens, pricing, and quota

The installed interactive `stop` hook schema carries `input_tokens`, `output_tokens`, `cache_read_tokens`, and `cache_write_tokens`, and the bundle repeats them on `afterAgentResponse`. `input_tokens` includes both cache classes, so fresh input is `input_tokens - cache_read_tokens - cache_write_tokens` with saturating subtraction; RimZ preserves the other three classes independently and keeps the per-turn counters out of cumulative token totals. Deterministic hook fixtures pin the regression shape `22,725 - 8,704 = 14,021` fresh tokens. The authenticated native `--print` capture emitted neither event and therefore supplied no live token values. The headless terminal result still has durations but no usage, and `preCompact.context_tokens` remains occupancy rather than billable fresh input.

The CLI changelog mentions a human `/usage` display, while the current slash-command reference does not list a machine-readable usage command or schema. Do not scrape the TUI or undocumented output.

Cursor publishes Auto API-equivalent rates of `$1.25/M` uncached input, `$6.00/M` output, and `$0.25/M` cached input. RimZ prices cache creation at the uncached-input rate, prices explicit model IDs through its shared model table, and accepts the repeated token counters from `afterAgentResponse` or a completed, aborted, or errored `stop` once per `generation_id`. Response delivery advances local pricing early; `stop` remains the lifecycle and per-turn-token authority. This is a locally priced live-session value for the card, cockpit, and live agent/room budgets; it resets with the local session sidecar and does not claim Cursor billing, account-day spend, or historical spend.

Before implementing provider account spend, look for a newly documented JSON command or official account API and capture its account scoping, timezone, reset windows, included usage, on-demand spend, and model prices. The general model catalog and pricing page is an index, not a per-account usage feed.

Two adjacent surfaces exist but do not fill the stock per-user CLI gap. The team [Admin API](https://cursor.com/docs/account/teams/admin-api) exposes usage events with `inputTokens`, `outputTokens`, `cacheWriteTokens`, and `cacheReadTokens`, but it is team-admin-scoped behind an admin token rather than a per-user CLI credential. Including token counts in `--output-format stream-json` remains an open upstream feature request as of the 2026-07 refresh, so the headless transport still carries no usage.

## CLI configuration, modes, and permissions

### Configuration files

| Scope | Path | Contents relevant to RimZ |
| --- | --- | --- |
| global macOS/Linux | `~/.cursor/cli-config.json` | all CLI settings and permissions |
| global Windows | `%USERPROFILE%\.cursor\cli-config.json` | all CLI settings and permissions |
| custom global | `$CURSOR_CONFIG_DIR/cli-config.json` | explicit override |
| XDG global | `$XDG_CONFIG_HOME/cursor/cli-config.json` | Linux/BSD override |
| project | `<project>/.cursor/cli.json` | permissions only |

The schema version is `1`, pure JSON. Cursor self-repairs missing fields and backs corrupted configs up as `.bad`. Some fields are CLI-managed and may be overwritten. Concurrent writes use temp-file plus atomic rename in current releases.

Relevant global fields include `model`, `maxMode`, `approvalMode` (`allowlist`, `auto-review`, or `unrestricted`), `sandbox.mode`, `sandbox.networkAccess`, `statusLine`, notifications, display controls, release channel, and network/proxy settings. A statusline object accepts `type`, `command`, `padding`, `updateIntervalMs`, and `timeoutMs`; RimZ carries those rendering siblings when wrapping a user command. Read the remaining config as launch-policy enrichment, not lifecycle truth: slash commands and flags can change the effective session state.

### Modes and launch mapping inputs

| Cursor mode | Launch surface | Semantics |
| --- | --- | --- |
| Agent | default | full tool set subject to approvals/sandbox |
| Plan | `--plan` or `--mode=plan` | plans and asks clarifying questions before coding |
| Ask | `--mode=ask` | read-only exploration |

Permission-related launch flags are `--force` / `--yolo`, `--sandbox enabled|disabled`, `--approve-mcps`, `--trust`, and `--auto-review`. The installed 2026.07.09 help confirms all of these flags. RimZ maps Ask to the default, Plan to `--mode=plan`, Auto to the classifier-backed `--auto-review`, and Yolo to `--force --sandbox disabled`; neutral-hook behavior still needs live fixtures because sandbox, allowlist, auto-review, and unrestricted are separate axes rather than one linear permission enum.

### Permission tokens

Cursor permission lists support:

| Token | Controls |
| --- | --- |
| `Shell(commandBase)` or `Shell(command:args)` | shell commands with glob support |
| `Read(pathOrGlob)` | file reads |
| `Write(pathOrGlob)` | file writes |
| `WebFetch(domainOrPattern)` | web-fetch domains |
| `Mcp(server:tool)` | MCP server/tool pairs |

Relative paths are workspace-scoped, absolute paths may target outside it, glob patterns use `**`, `*`, and `?`, and deny rules override allow rules. These project permission entries change executable and data-access behavior and belong in RimZ trust review.

### Workspace and worktree launch

`--workspace <path>` chooses the workspace. Repeatable `--add-dir` support in current releases creates multi-root sessions. `-w` / `--worktree [name]` creates a Cursor-owned Git worktree under `~/.cursor/worktrees/<repo>/<name>`, with `--worktree-base` and `--skip-worktree-setup` controls.

RimZ should launch Cursor inside the RimZ-owned pane/worktree and avoid nesting a Cursor-owned worktree unless the user explicitly requests it. `.cursor/worktrees.json` setup commands are an executable trust surface.

## Implementation checklist

Before declaring Cursor supported:

1. Pin a Cursor CLI build and capture `agent --version`, root help, ACP help, status/about JSON, and the model list.
2. Install one neutral user hook entry through structured JSON merge, visible diff, uninstall, and `hooks_installed` checks.
3. Verify hook ancestry, inherited RimZ pane/workspace environment, cwd, stdout/stderr behavior, timeouts, and paths containing spaces on macOS, Linux, WSL, and Windows.
4. Enable third-party skills with RimZ's Claude hooks installed and prove Cursor cannot cross-fire a Cursor payload into the Claude adapter or double-record one native event.
5. Golden-capture every wired hook for new, resumed, continued, cleared, forked, summarized, interrupted, errored, and process-killed sessions.
6. Prove that `conversation_id` is stable, `generation_id` changes per prompt, and `sessionStart.session_id` equals the common conversation ID in CLI mode.
7. Capture empty `{}` output on every wired event and prove it preserves Cursor's native behavior under allowlist, sandbox, auto-review, unrestricted, Plan, and Ask modes.
8. Build a tool-name corpus and pin the file-editing subset without treating arbitrary shell or MCP activity as an edit.
9. Establish a compaction-close certificate or report compaction completion unsupported.
10. Establish `subagentStop` identity for concurrent same-type children before emitting child stop observations.
11. Confirm whether any supported local event exposes permission, question, or plan waits; otherwise declare awaiting-user unsupported and emit no false waiting state.
12. Keep transcript parsing limited to the fixture-backed terminal subset until privacy-safe schemas for any additional records exist.
13. Exercise `-p` text, JSON, stream JSON, partial streaming, non-zero exit, signal interruption, timeout, missing terminal result, stdin prompts, and resume.
14. Exercise ACP initialization, auth, session new/load, streaming updates, permission replies, questions, plans, cancellation, and child task notifications if ACP backs supervised runs.
15. Define explicit RimZ permission-profile mappings across mode, approval mode, sandbox, force, MCP approval, and workspace trust.
16. Leave account spend and quota capabilities off until an official machine-readable source exists.

## Known upstream gaps

- Local hooks publish no dedicated native permission, user-question, plan-approval, or attention event.
- `preToolUse.permission = "ask"` is accepted by schema but not enforced, while specialized before-hooks use `ask`; the surfaces are not interchangeable.
- There is no documented post-compaction event or continuous context query.
- The published `subagentStop` payload omits the unique `subagent_id` provided at start.
- The captured transcript terminal subset is unpublished upstream, and the enablement/durability contract remains undocumented.
- Local CLI chat storage and fork lineage have no official schema.
- Status/about JSON commands exist and one sanitized authenticated browser-login arm is captured, but unauthenticated, expired, API-key, service-account, proxy, and server-error semantics remain unverified.
- Interactive stop hooks expose per-turn token composition through installed-bundle and deterministic-fixture evidence; spend, balance, quota, rate-limit reset, and native-headless token usage remain absent.
- Hook parallel-response merge details, command-shell rules, stdout limits, and timeout defaults are unpublished.
- Third-party compatibility may execute existing Claude hooks in Cursor, while the official docs do not define input-payload translation or a source discriminator.
- ACP documents richer asks but changes RimZ from observing the stock CLI into hosting its UI protocol.

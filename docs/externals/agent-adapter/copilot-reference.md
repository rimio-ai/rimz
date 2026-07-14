# GitHub Copilot CLI protocol reference

> RimZ ships a hooks-first Copilot adapter documented in [copilot.md](../../internals/agents/copilot.md). This reference retains the live-verification and enrichment gaps that bound the current integration; the agent-agnostic lifecycle and enrichment contracts are [model.md](../../internals/agents/model.md), and the account/spend contract is [providers.md](../../internals/agents/providers.md).

This is the single home for the **GitHub Copilot CLI upstream protocol surface** relevant to RimZ — lifecycle hooks and their decision channel, session identity and persistence, the statusline command, OpenTelemetry, programmatic and ACP modes, authentication, remote control, configuration, and permission modes. It is an implementation research record, not a claim that RimZ currently supports Copilot.

Refresh baseline: GitHub Copilot CLI **1.0.70** (`758c2c9`, released 2026-07-10) and the official GitHub documentation available on **2026-07-10**. GitHub publishes the CLI executable and changelog in [`github/copilot-cli`](https://github.com/github/copilot-cli), but the runtime implementation and its session-event types are not published there. Treat the official hook, configuration, ACP, and OTel references as contracts; undocumented `events.jsonl`, `--output-format=json`, and custom-statusline fields are compatibility evidence only where a version-pinned capture below names them.

Coverage is **depth on the surfaces an adapter should wire, breadth as an index**. The hook inputs and outputs are documented in full because they are the stable lifecycle and blocking seam. The local state, telemetry, launch, authentication, and remote-control sections identify the exact source of every RimZ concern and mark gaps where GitHub publishes no schema.

## Upstream sources

Re-fetch these pages and compare the latest stable release before implementing or refreshing the adapter.

| Surface | Source |
| --- | --- |
| CLI repository, install entry, license | <https://github.com/github/copilot-cli> |
| Stable releases and version changelog | <https://github.com/github/copilot-cli/releases> · <https://github.com/github/copilot-cli/blob/main/changelog.md> |
| CLI commands, options, environment, permissions, OTel | <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference> |
| Hook discovery, events, payloads, outputs, exit semantics | <https://docs.github.com/en/copilot/reference/hooks-reference> |
| Configuration directory, settings merge, session files | <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference> |
| Programmatic `-p` mode | <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference> |
| ACP server | <https://docs.github.com/en/copilot/reference/copilot-cli-reference/acp-server> |
| Session storage, syncing, resume | <https://docs.github.com/en/copilot/concepts/agents/copilot-cli/chronicle> · <https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/chronicle> |
| Remote control | <https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/steer-remotely> |
| Authentication and credential behavior | <https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli> · <https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/troubleshoot-copilot-cli-auth> |
| Tool allow/deny behavior | <https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/allowing-tools> |
| Agent modes and autonomous continuation | <https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-copilot-cli> · <https://docs.github.com/en/copilot/concepts/agents/copilot-cli/autopilot> |

The authoritative local companion to these pages is the installed executable:

```sh
copilot version
copilot help
copilot help config
copilot help environment
copilot help logging
copilot help monitoring
copilot help permissions
copilot help providers
```

## Recommended adapter shape

Use **command hooks** as the lifecycle and decision seam. They are local, session-scoped, carry the session ID and cwd on every event, expose synchronous permission decisions, and preserve Copilot's stock interactive UI. This matches RimZ's existing pane-first contract.

Use the **custom statusline command** or **file-exported OTel** for live context, model, cost, and token enrichment. Statusline is the lighter UI-owned transport, but GitHub currently documents only that it receives session JSON, not that JSON's schema. OTel has a published schema and includes tokens, cost, model, session ID, compaction, errors, and subagents, but it is asynchronous telemetry and must remain enrichment rather than lifecycle truth. A separate bounded read of the undocumented `/copilot_internal/user` endpoint supplies best-effort plan and named-quota enrichment when a documented environment token is available.

Use **`-i, --interactive <prompt>`** for RimZ prompt-seeded panes and supervised runs: it submits the initial prompt while preserving the stock interactive UI, native asks, and the hook-driven completion path shared with other adapters. Native `-p` prompt mode is a future alternative if RimZ adds a process-output supervised backend. Evaluate **ACP** when RimZ needs structured streaming, permission requests, or session control for a supervised run. Do not replace the interactive pane with ACP: ACP changes RimZ from observing the user's stock CLI into hosting the agent protocol itself.

The candidate transport matrix is:

| RimZ concern | Primary upstream surface | Backstop / gap |
| --- | --- | --- |
| start, prompt, work, stop, end | command hooks | pane liveness for missed `sessionEnd` |
| permission ask | `permissionRequest` hook + neutral stdout | `notification(permission_prompt)` is asynchronous evidence only |
| user question | `preToolUse` on `ask_user` plus `notification(elicitation_dialog)` | hook protocol has no dedicated synchronous elicitation-answer event |
| compaction start | `preCompact` | OTel has start and complete events; hooks have no post-compact event |
| compaction end | OTel `session.compaction_complete`, or derived next activity | no native post-compact hook |
| subagents | `subagentStart` / `subagentStop` | built-in `general-purpose` emits neither event |
| model, effort, context | statusline if its captured schema is suitable | OTel plus config; footer display proves fields exist but is not a read API |
| plan and named quotas | bounded `/copilot_internal/user` read with an environment token | undocumented compatibility surface; treat availability as best-effort |
| tokens, cost, AI units | OTel | session `events.jsonl` is undocumented |
| session resume | `--resume`, `--continue`, `--session-id` | session ID is present in hooks |
| authentication | environment and `copilot login` | no documented machine-readable auth-status command |
| remote control | `--remote`, `/remote on`, `remoteSessions` | requires a GitHub-hosted repository and eligible account |

## Hooks

Copilot CLI hooks are external commands or HTTP endpoints invoked at lifecycle points. A command receives one JSON object on **stdin** and may return one JSON decision object on **stdout**. RimZ's hook helper must reserve stdout for the decision and send diagnostics to stderr or RimZ logs.

### Discovery and merge order

Copilot combines hook entries from all active sources; entries for the same event all run. The official discovery list is:

| Source | Location / shape | Order and trust implication |
| --- | --- | --- |
| machine policy | `/etc/github-copilot/policy.d/*.json` on Linux/macOS; platform policy locations on Windows | first, alphabetical; root-owned policy hooks cannot be disabled by `disableAllHooks` |
| user hook files | `$COPILOT_HOME/hooks/*.json`, default `~/.copilot/hooks/*.json` | user tier; suitable for one RimZ-owned file |
| user settings | `$COPILOT_HOME/settings.json`, top-level `hooks` | user tier; global inline alternative |
| repository files | `.github/hooks/*.json` | project tier; repository executable surface |
| repository settings | `.github/copilot/settings.json` and `.github/copilot/settings.local.json`, top-level `hooks` | project and local tiers; repository hooks run after user hooks |
| cross-tool repository settings | `.claude/settings.json` and `.claude/settings.local.json`, shared hook subset | project tier; repository executable surface |
| installed plugins | plugin `hooks.json` or `hooks/hooks.json` | plugin tier, after project hooks |

Prefer one RimZ-owned user hook file such as `$COPILOT_HOME/hooks/rimz.json`. It avoids rewriting the user's JSONC settings and gives install, diff, uninstall, and trust hashing one bounded file. Hook commands, cwd, environment overlays, HTTP targets, and any executable plugin path belong in RimZ's project-trust review.

`disableAllHooks: true` disables user and repository hooks but does not disable policy hooks. The key can exist in user, repository, local, or cross-tool settings; the adapter preflight must resolve the effective merged value rather than checking one file.

Prompt mode adds another trust gate: repository hooks load when the folder is already trusted, `COPILOT_ALLOW_ALL` is set, or `GITHUB_COPILOT_PROMPT_MODE_REPO_HOOKS=true`. A global RimZ user hook does not need that repository opt-in.

### Command configuration

Hook files use version `1` JSON:

```json
{
  "version": 1,
  "hooks": {
    "sessionStart": [
      {
        "type": "command",
        "bash": "rimz hook copilot",
        "powershell": "rimz hook copilot",
        "timeoutSec": 30
      }
    ]
  }
}
```

| Field | Contract |
| --- | --- |
| `type` | optional for command hooks; defaults to `"command"` |
| `bash` | Unix command; one of `bash`, `powershell`, or `command` is required |
| `powershell` | Windows command |
| `command` | cross-platform fallback copied to an absent platform-specific field |
| `cwd` | absolute, or relative to the repository root |
| `env` | environment overlay with variable expansion |
| `timeoutSec` | seconds; default `30` |
| `timeout` | alias used only when `timeoutSec` is absent |
| `matcher` | event-specific anchored regex; see [Matchers and tool names](#matchers-and-tool-names) |

The docs do not state whether command strings are tokenized directly or passed through a shell. Install an absolute, safely quoted path and verify paths containing spaces on every supported platform.

HTTP hooks receive the same input as a JSON `POST`. HTTPS is required for `preToolUse` and `permissionRequest`; other HTTP hooks require HTTPS by default, with loopback HTTP enabled only by `COPILOT_HOOK_ALLOW_LOCALHOST=1`. HTTP errors, timeouts, and non-2xx responses are fail-open. RimZ should prefer command hooks because per-session local routing must stay available offline and blocking decisions must not depend on a network endpoint.

### Payload dialects

The event name selects one of two payload dialects:

- camelCase event names such as `sessionStart` produce camelCase fields and millisecond Unix timestamps.
- VS Code-compatible PascalCase names such as `SessionStart` produce snake_case fields, an ISO-8601 timestamp, and `hook_event_name`.

Use the native camelCase names for a new adapter. They cover every event and avoid Claude-name translation. A tolerant parser may accept both dialects because Copilot explicitly supports both and plugins may install either.

Every documented payload carries `sessionId`, `timestamp`, and `cwd` (or `session_id`, ISO `timestamp`, and `cwd`). Unlike Claude and Codex, the common payload has no model, effort, permission mode, or transcript path; only selected events add `transcriptPath`.

### Events an adapter should wire

| Event | Fires | Event-specific camelCase fields | RimZ use |
| --- | --- | --- | --- |
| `sessionStart` | new or resumed session begins | `source: "startup" \| "resume" \| "new"`, `initialPrompt?` | prompt-seeded duplicate turn edge or promptless start/resume identity |
| `userPromptSubmitted` | user submits a prompt | `prompt` | turn start |
| `preToolUse` | before every tool | `toolName`, `toolArgs` | proof of work; classify `ask_user`; optional blocking policy |
| `postToolUse` | tool succeeds | `toolName`, `toolArgs`, `toolResult: { resultType: "success", textResultForLlm }` | silent activity / audit |
| `postToolUseFailure` | tool fails | `toolName`, `toolArgs`, `error` | activity and error evidence |
| `permissionRequest` | before permission rules, session approvals, auto decisions, and UI prompt | official docs describe matching and decisions but do not publish a distinct input shape beyond `toolName` matching | synchronous awaiting-user decision channel |
| `agentStop` | main agent finishes a turn | `transcriptPath`, `stopReason: "end_turn"` | turn completed |
| `subagentStart` | a non-`general-purpose` subagent starts | `transcriptPath`, `agentName`, `agentDisplayName?`, `agentDescription?` | child start; upstream publishes no child instance ID |
| `subagentStop` | a non-`general-purpose` subagent completes | `transcriptPath`, `agentName`, `agentDisplayName?`, `stopReason: "end_turn"` | child stop |
| `preCompact` | manual or automatic compaction begins | `transcriptPath`, `trigger: "manual" \| "auto"`, `customInstructions` | open compaction bracket |
| `errorOccurred` | runtime error | `error: { message, name, stack? }`, `errorContext: "model_call" \| "tool_execution" \| "system" \| "user_input"`, `recoverable` | turn error / diagnostic |
| `notification` | asynchronous attention notification | `message`, `title?`, `notification_type` | attention enrichment; never a blocking decision source |
| `sessionEnd` | session terminates | `reason: "complete" \| "error" \| "abort" \| "timeout" \| "user_exit"` | explicit end |

The hook event catalog is exactly the table above; GitHub documents no post-compaction, model-change, effort-change, rate-limit, usage, or streaming-message hook.

The 1.0.44 changelog says `userPromptSubmitted` hooks can handle a request directly and bypass the model, while the current hooks reference still marks that event's output as unprocessed and publishes no return schema. Treat direct handling as unavailable until GitHub documents the output contract or a pinned fixture establishes a deliberately version-gated shape.

`agentStop` and `subagentStop` use `transcriptPath`, but the hooks reference does not define the transcript format or promise that the path is the local `session-state/<id>/events.jsonl`. Capture and compare before binding them.

The built-in `general-purpose` agent emits neither `subagentStart` nor `subagentStop`. Other documented built-ins — `explore`, `task`, `code-review`, `rubber-duck`, `research`, and `security-review` — and user custom agents emit both. The payload exposes an agent definition name, not a unique child-run ID; concurrent children with the same `agentName` cannot be durably distinguished from hooks alone. OTel spans provide invocation structure but still require a captured trace/span-to-row identity strategy.

### Hook outputs and decision channel

The neutral path is empty stdout or `{}` with exit `0`. That delegates permission and interaction to Copilot's own UI.

`sessionStart` may inject context:

```json
{ "additionalContext": "context added to the session" }
```

`subagentStart` accepts the same field and prepends it to the child's prompt; it cannot block child creation.

`preToolUse` controls the call:

```json
{
  "permissionDecision": "allow",
  "permissionDecisionReason": "optional unless denying",
  "modifiedArgs": {}
}
```

`permissionDecision` is `allow`, `deny`, or `ask`; `permissionDecisionReason` is required for `deny`; `modifiedArgs` replaces the original arguments. In a non-interactive cloud-agent job, `ask` becomes `deny`. For the local CLI, empty output preserves the native permission path.

`permissionRequest` short-circuits the permission service:

```json
{ "behavior": "allow" }
```

```json
{ "behavior": "deny", "message": "reason returned to the model", "interrupt": false }
```

All matching permission hooks run and their outputs merge, with later hook outputs overriding earlier ones. `behavior` is `allow` or `deny`; `interrupt: true` plus `deny` stops the agent. Empty output falls through to Copilot's rule engine and prompt. `read` and `hook` permission kinds short-circuit before permission hooks, so they cannot be intercepted here.

`agentStop` and `subagentStop` can force continuation:

```json
{ "decision": "block", "reason": "continue with this instruction" }
```

`decision` is `block` or `allow`. A block starts another agent turn with `reason` as the prompt. RimZ observation hooks must return neutral output; using stop blocking for ordinary telemetry would change product behavior.

`postToolUse` may replace a successful result or append model context:

```json
{
  "modifiedResult": {
    "resultType": "success",
    "textResultForLlm": "replacement result"
  },
  "additionalContext": "guidance appended after the tool output"
}
```

Multiple `additionalContext` values join with blank lines and are capped at 10 KB. A replacement marked as failure routes into `postToolUseFailure`.

`notification` is asynchronous and fire-and-forget, but may inject a user message:

```json
{ "additionalContext": "prepended user message" }
```

That injection can restart processing when the session is idle. RimZ must return neutral output unless the user explicitly routes a queued answer through this channel.

### Exit and failure semantics

| Result | General behavior | Special cases |
| --- | --- | --- |
| exit `0` | parse stdout as one output JSON object | empty stdout is neutral |
| exit `2` | warning; surface stderr and continue | `permissionRequest` merges stdout with `behavior: deny`; `postToolUseFailure` treats stdout as added recovery context; as of CLI 1.0.70, `preToolUse` exit `2` denies |
| other non-zero | log failure and continue | command `preToolUse` fails closed and denies |
| timeout | kill after timeout, warn, continue | command `preToolUse` timeout is explicitly fail-open |
| HTTP error / timeout / non-2xx | fail open | includes HTTP `preToolUse` |

Command-hook stdout supports progress objects while a hook runs:

```json
{"type":"progress","message":"Checking policy...","temporary":true}
```

Each progress object must occupy one complete JSON line. Copilot strips recognized progress lines, concatenates every remaining stdout line, trims them, then parses the remainder with one `JSON.parse`. Emit at most one final decision object. RimZ's hook should emit no progress lines unless the user-facing delay warrants a timeline entry.

### Matchers and tool names

Native camelCase matchers are case-sensitive regular expressions anchored as `^(?:PATTERN)$`. Invalid expressions skip the hook entry.

| Event | Matcher input |
| --- | --- |
| `preToolUse`, `postToolUse`, `permissionRequest` | `toolName` |
| `preCompact` | `trigger` (`manual` or `auto`) |
| `subagentStart` | `agentName` |
| `notification` | `notification_type` |

Documented native tools are `ask_user`, `bash`, `powershell`, `create`, `edit`, `glob`, `grep`, `task`, `view`, and `web_fetch`. Runtime releases may add tools; use a catch-all hook and tolerate unknown names.

PascalCase `PreToolUse` and `PermissionRequest` use Claude-compatible matching and names: `Bash`, `Read`, `Write`, `Edit`, `Grep`, `Glob`, `WebFetch`, `WebSearch`, `AskUserQuestion`, `TodoWrite`, and `Agent` (`Task` is also accepted). `*`, `**`, or empty matches all; literal `|` alternation matches native or Claude aliases; other strings are anchored regexes over the Claude name.

### Notifications

The asynchronous `notification` payload uses `hook_event_name: "Notification"` even in the otherwise camelCase shape. `notification_type` values are:

| Type | Meaning |
| --- | --- |
| `shell_completed` | background shell command finished |
| `shell_detached_completed` | detached shell session finished |
| `agent_completed` | background subagent completed or failed |
| `agent_idle` | background agent finished a turn and waits for `write_agent` |
| `permission_prompt` | native permission UI requires attention |
| `elicitation_dialog` | agent asks the user for information |

These signals are excellent sidebar wakeups and ask classifiers, but they are not truth: the hook is fire-and-forget, failures are skipped, and its output cannot directly answer the dialog. The synchronous permission decision remains `permissionRequest`; a human response to `ask_user` should use RimZ's pane send path unless a tested native answer protocol becomes available.

## Session identity, resume, and local store

Session identity is a UUID-like string supplied as `sessionId` by every hook. Launch and selection options:

| Option | Behavior |
| --- | --- |
| `--session-id ID` | resume exact existing session/task; if absent, create only when `ID` is a valid UUID |
| `--resume[=VALUE]`, `-r` | picker, ID, ID prefix, or exact case-insensitive session name; falls back to generated summary |
| `--continue` | most recent session in cwd, falling back to most recent globally |
| `--name NAME`, `-n` | name a new session for later selection |
| `/resume [ID]` | switch sessions inside the interactive UI |
| `/rename NAME` | rename current session |
| `/fork [NAME]` | create an independent child conversation; experimental in the 1.0.70 command reference |

Do not combine `--session-id` with `--resume`, `--continue`, or `--connect`; they compete for session selection. A resumed session restores its saved working directory unless `-C DIRECTORY` overrides it.

`COPILOT_HOME` selects the configuration and state root; default is `~/.copilot`. Relevant contents:

```text
$COPILOT_HOME/
├── config.json                 # managed application state and plaintext auth fallback
├── settings.json               # user JSONC settings
├── hooks/                      # user hook JSON files/scripts
├── logs/process-<time>-<pid>.log
├── permissions-config.json     # saved approvals by location
├── session-state/<session-id>/
│   ├── events.jsonl
│   ├── checkpoints/
│   ├── plan.md
│   └── files/
└── session-store.db            # cross-session Chronicle index/search
```

GitHub documents `events.jsonl` as the per-session event log and says the session directory is the complete record used for resume. It does **not** publish event discriminants, field schemas, append/durability guarantees, or compatibility rules. `session-store.db` is an automatically managed Chronicle derivative and can be rebuilt with `/chronicle reindex`; captured `turns` rows lagged the live event file until session shutdown, so it is unsuitable for live conversation streaming.

Copilot CLI 1.0.70 capture shows root `user.message` and `assistant.message` records appended in conversation order with RFC3339 `timestamp` and visible text in `data.content`. The same turn includes `system.message`, hook, turn-boundary, and session records; user records can also carry `transformedContent`, while assistant records can carry encrypted and reasoning fields. These are captured compatibility facts, not an upstream schema guarantee.

The captured hook order for a fresh prompt-seeded turn is `userPromptSubmitted`, `sessionStart`, `agentStop`, `sessionEnd`; `sessionStart.initialPrompt` repeats the already-submitted prompt. RimZ uses a non-empty `initialPrompt` only to normalize that start as the duplicate turn edge, while promptless starts retain ordinary registration semantics.

The captured `agentStop` hook input names the same file in `transcriptPath`. RimZ validates the filename and session-ID parent, normalizes only visible user/assistant content, and ignores transformed/encrypted content, reasoning, systems, hooks, tools, and unknown events. Child-session filtering stays deferred until a child capture proves its identity shape.

Local session data contains prompts, responses, tools, and modified-file details. It syncs to the user's GitHub account by default when policy permits. `remoteExport: false` opts out. Reading local files for RimZ enrichment stays local and read-only; diagnostics must not copy prompt content unless the user enabled an explicit privacy setting.

## Custom statusline

User setting `statusLine` runs a command that receives **session JSON on stdin** and prints status content on stdout:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/absolute/path/to/status-command",
    "padding": 0
  }
}
```

The footer can display model/effort, directory, branch, context window, quota, agent, AI used, code changes, username, sandbox, yolo state, and custom content. This proves the TUI maintains the main enrichment values, but the official docs do not publish the statusline input fields, update triggers, timeout, environment, exit handling, ANSI rules, or command-chaining semantics.

A sanitized 1.0.70 capture observed stable top-level `session_id`, `version`, `model`, `context_window`, `cost`, and `ai_used` objects. After one auto-model turn, `model` was `{id: "auto", display_name: "Auto → gpt-5-mini"}` and `context_window` added `current_context_tokens`, `displayed_context_limit`, and `current_context_used_percentage`, while its nominal `context_window_size`, `used_percentage`, `remaining_percentage`, and `remaining_tokens` stayed null. The payload later added latest-call and cumulative token fields. This binds a candidate payload to lifecycle identity, but does not choose stable denominator semantics or prove a lossless wrapper under concurrent sessions, so RimZ leaves the Copilot statusline untouched.

Before selecting this transport, complete the remaining capture matrix: tool execution, permission wait, `ask_user`, model/effort and context-tier changes, compaction, subagent, rate-limit warning, remote mode, concurrent sessions, and exact wrapper stdin/stdout/stderr/exit/timeout behavior. OTel remains the only selected structured enrichment schema until those gates pass.

## OpenTelemetry

Copilot CLI can export traces and metrics through OTLP or a local JSONL file. It activates when `COPILOT_OTEL_ENABLED=true`, `OTEL_EXPORTER_OTLP_ENDPOINT` is set, or `COPILOT_OTEL_FILE_EXPORTER_PATH` is set. The file exporter is the least invasive RimZ prototype:

```sh
COPILOT_OTEL_FILE_EXPORTER_PATH=/path/owned/by/rimz/copilot-otel.jsonl copilot
```

Relevant configuration:

| Variable | Meaning |
| --- | --- |
| `COPILOT_OTEL_EXPORTER_TYPE` | `otlp-http` or `file`; file auto-selected when file path is set |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/json` default or `http/protobuf` |
| `OTEL_SERVICE_NAME` | default `github-copilot` |
| `OTEL_RESOURCE_ATTRIBUTES` | extra percent-encoded `key=value` attributes; candidate place for a RimZ pane/session nonce if process env is preserved |
| `COPILOT_OTEL_FILE_EXPORTER_PATH` | JSONL output for all signals |
| `COPILOT_OTEL_SOURCE_NAME` | instrumentation scope, default `github.copilot` |
| `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` | full prompt/response/tool content; default `false`; RimZ must leave false |

One user interaction produces an `invoke_agent` root span with `chat` and `execute_tool` descendants. Top-level invocations are `CLIENT`; subagents are `INTERNAL`.

The `invoke_agent` span carries `gen_ai.conversation.id` (session), agent ID/name/description/version, requested model, finish reason, cumulative input/output/cache tokens, `github.copilot.turn_count`, monetary `github.copilot.cost`, AI units, and error type. The top-level default agent ID is `github.copilot.default`.

Each `chat` span carries requested and resolved model, session ID, response ID and finish reasons, per-request input/output/cache tokens, turn cost, AI units, server duration, initiator, `turn_id`, `interaction_id`, and error type. Each `execute_tool` span carries tool name, type, call ID, description, and error type.

Lifecycle span events relevant to RimZ:

| Event | Attributes / use |
| --- | --- |
| `github.copilot.session.truncation` | token limit, pre/post tokens/messages, removed counts, performer |
| `github.copilot.session.compaction_start` | native compaction opener |
| `github.copilot.session.compaction_complete` | success, pre/post tokens, removed counts, optional captured summary |
| `github.copilot.session.shutdown` | shutdown type, total premium requests, lines added/removed, modified-file count |
| `github.copilot.session.abort` | abort reason |
| `exception` | Copilot error type, HTTP status, provider call ID |
| `github.copilot.hook.start/end/error` | hook type and invocation ID; diagnostics only |

OTel is asynchronous and exporters can drop data, so lifecycle still comes from hooks/store. The RimZ 1.0.70 reader deliberately accepts only `chat` spans with an exact `gen_ai.conversation.id`, chooses the newest captured timestamp, prefers resolved over requested model, and maps latest-call input/output/cache counts. Captured input counts include the cache-read slice, so normalized fresh input is the saturating `input - cache_read` difference. It ignores `invoke_agent`, inference/agent-turn logs, metrics, costs, quotas, and account data.

The 1.0.70 concurrency gate ran three overlapping turns in each of two direct processes against one file. It produced 84 complete JSON records and 93,721 bytes without truncation or interleaving; the bounded final 64 KiB retained two complete `chat` spans for each exact conversation ID. In a separate flush probe, the completed `chat` span was visible by `agentStop` (2,743 bytes), while `invoke_agent` and metric records appended during shutdown (15,357 bytes at exit); stat-gated Tick/Watch refresh observes later growth without another turn. Live rotation remains disabled because exporter reopen behavior is not verified.

New RimZ rooms set a private room-runtime file exporter for direct and managed launches and pin `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=false`. An ambient `COPILOT_OTEL_FILE_EXPORTER_PATH` is preserved. An OTLP endpoint or explicitly non-file exporter is not redirected; file enrichment remains unavailable unless the user also selects a file exporter. Exact conversation filtering isolates concurrent sessions.

Captured `github.copilot.cost` was finite but `0.0` on each `chat` span, and the aggregate `invoke_agent` span exposed no session-cumulative dollar. This fails the positive cumulative-value and replacement/dedup gates, so RimZ publishes no live session dollars and keeps historical/account Copilot spend unsupported.

The docs publish no rate-limit/quota attributes in the OTel schema. Quota is visible in the native footer; RimZ reads it separately through the bounded undocumented account-usage surface below and does not infer it from telemetry.

## Programmatic mode

`copilot -p PROMPT` runs one non-interactive task and exits. Adapter-relevant options:

| Option | Use |
| --- | --- |
| `-p`, `--prompt` | one programmatic task |
| `-s`, `--silent` | assistant response only; suppress stats and decoration |
| `--output-format=json` | JSONL, one object per line; GitHub publishes no object schema |
| `--stream=on\|off` | progressive response or buffered output |
| `--model`, `--effort` | pin model and reasoning effort |
| `--mode=interactive\|plan\|autopilot`, `--plan`, `--autopilot` | initial behavior |
| `--context=default\|long_context` | context-window tier; overrides the persisted setting, and bounds the gauge denominator once a transport reports the resolved model |
| `--allow-tool`, `--deny-tool`, `--allow-url`, `--deny-url` | scoped permission rules |
| `--allow-all`, `--yolo` | all tools, paths, and URLs; may be disabled by managed policy |
| `--no-ask-user` | remove the `ask_user` tool |
| `--share PATH` | export Markdown transcript after completion; contains sensitive content |
| `--agent NAME` | select a custom agent |
| `-C DIRECTORY` | change cwd before session selection/creation |

Programmatic mode requires enough pre-approved permissions to avoid an unanswered prompt. `permissionRequest` hooks are explicitly supported for CI and pipe mode and can supply policy decisions. For a user-facing RimZ `-p` pane, preserve the ability to answer in the pane rather than granting `--allow-all` by default.

`copilot -i PROMPT` instead starts interactive mode and automatically executes the prompt. RimZ uses this form for its supervised pane because RimZ's `-p` surface supervises an interactive agent turn rather than adopting each provider's similarly named non-interactive mode.

The official docs promise JSONL for `--output-format=json` but publish no event names, completion record, usage object, or compatibility contract. RimZ supervised runs should initially use process exit plus `--silent` text output, or pin a captured JSON schema behind a minimum CLI version.

## ACP server

`copilot --acp --stdio` starts the public-preview Agent Client Protocol server over NDJSON stdio. `copilot --acp --port PORT` uses TCP. Tool filters and reasoning-effort flags on the server process apply to every client-created session.

The documented client flow is standard ACP:

1. spawn `copilot --acp --stdio` with stdin/stdout piped;
2. initialize with the ACP protocol version and client capabilities;
3. create a session with `cwd` and MCP server definitions;
4. send `prompt` with the returned session ID;
5. consume `sessionUpdate`, including `agent_message_chunk` text;
6. implement `requestPermission` and return an ACP permission outcome.

ACP is attractive for a future structured supervised-run backend because it exposes permission requests and streaming without scraping terminal output. It is public preview and versioned by ACP rather than RimZ's canonical agent-plugin wire; gate it by Copilot version and protocol initialization. It does not observe an independently running stock Copilot pane.

## Permission and launch mapping

Copilot has three interactive modes: standard, plan, and autopilot. `Shift+Tab` cycles modes; `--mode`, `--plan`, and `--autopilot` select at launch. Standard mode can execute and prompt. Plan mode builds a structured plan before editing. Autopilot continues until the `task_complete` tool, subject to `--max-autopilot-continues`.

Candidate RimZ permission mapping:

| RimZ mode | Copilot launch |
| --- | --- |
| ask/default | stock `copilot` |
| plan | `copilot --plan` |
| auto | `copilot --autopilot` with explicit bounded continuation policy where configured |
| yolo | `copilot --allow-all` / `--yolo` |

Validate the final mapping against RimZ's cross-agent permission semantics before implementation. `--allow-all` can be suppressed by managed `permissions.disableBypassPermissionsMode = "disable"`; fail fast when the user requested yolo but policy makes it unavailable.

Tool permission patterns use categories `memory`, `read`, `shell`, `url`, `write`, and MCP server/tool names. Deny rules always win, including over `--allow-all`. Saved approvals live in `$COPILOT_HOME/permissions-config.json` by location. RimZ must not edit this managed file to simulate a mode; use documented launch flags and hooks.

## Authentication and account surface

`copilot login` uses GitHub's browser/device OAuth flow and accepts `--host`. Credentials normally land in the system credential store. When no credential store exists, Copilot can store plaintext state under `$COPILOT_HOME/config.json` if the user consents or `storeTokenPlaintext` is enabled.

Environment-token precedence is:

1. `COPILOT_GITHUB_TOKEN`
2. `GH_TOKEN`
3. `GITHUB_TOKEN`

Supported tokens are fine-grained PATs with the **Copilot Requests** permission, Copilot CLI OAuth tokens, and GitHub CLI OAuth tokens. Classic `ghp_` PATs are unsupported. `COPILOT_GH_HOST` overrides `GH_HOST` for Copilot only.

The official CLI command list has no `copilot auth status --json` equivalent. `/user show`, `/user list`, and `/user switch` are interactive slash commands. Captured `$COPILOT_HOME/config.json` state exposes the non-secret current identity as `lastLoggedInUser: {host, login}` and the known identity list as `loggedInUsers: [{host, login}]`. RimZ uses the last valid identity, falling back to the first valid list entry, and qualifies enterprise identities as `login@host` so equal logins on different hosts remain distinct.

Model only these identity fields. Leave keychain entries and `config.json` token fields such as `copilotTokens` untouched and unmodeled. Presence of an environment variable is not proof that the token is valid or that the account has an enabled Copilot CLI policy.

### Undocumented account-usage response

[CodexBar's Copilot usage fetcher](https://github.com/steipete/CodexBar/blob/main/Sources/CodexBarCore/Providers/Copilot/CopilotUsageFetcher.swift) demonstrates a bounded `GET /copilot_internal/user` request authenticated as `Authorization: token <credential>`, with GitHub JSON accept/version headers and Copilot editor/plugin user-agent headers. The public endpoint is `https://api.github.com/copilot_internal/user`; an enterprise host maps to its `api.<host>` authority while retaining an explicit port. This surface is compatibility evidence rather than an official GitHub API contract.

The response can expose `copilot_plan`, `token_based_billing`, `quota_reset_date`, modern `quota_snapshots`, and legacy `monthly_quotas` plus `limited_user_quotas`. Premium interactions and Chat may report an explicit percentage, entitlement and remaining counts, or unlimited state; reset dates appear as RFC 3339 timestamps or calendar dates. Business responses may carry zero-entitlement placeholders and no usable quota while still providing the plan.

RimZ resolves only the documented environment-token precedence above and host precedence `COPILOT_GH_HOST`, `GH_HOST`, then `github.com`. It fingerprints the normalized host and selected token for cache invalidation, stores no credential, and treats a successfully decoded response as authoritative quota data while treating missing credentials and expected authentication or unsupported-endpoint responses as quiet best-effort absence. It maps Premium and Chat to durationless named lanes `prm` and `cht`; no monthly duration is inferred from the reset date.

GitHub AI Credits represent cost at **$0.01 per credit**, but model usage and plan quota behavior can change. OTel publishes `github.copilot.aiu` and `github.copilot.cost`; verify units with fixtures and current billing docs before mapping `cost` to dollars or AI units to credits.

## Remote control

Remote control lets GitHub.com and GitHub Mobile view the running local session, answer permission requests, and continue the conversation. The host machine and terminal session must remain online.

Enable with `copilot --remote`, `/remote on`, or `remoteSessions: true`; disable per launch with `--no-remote`. `/remote` reports the current session status and `/remote off` disconnects it. `remoteExport` controls view-only session sync; remote control also enables export.

The official prerequisite says the cwd must be a Git repository hosted on GitHub.com for remote control; account/organization availability also applies. Preflight both rather than launching a degraded remote mode. Ordinary RimZ pane launches should preserve the user's configured `remote` / `remoteSessions` behavior unless RimZ adds an explicit provider toggle.

## Implementation footprints and open gaps

The hooks-first adapter implements the lifecycle, transcript, and narrow OTel subset documented in [copilot.md](../../internals/agents/copilot.md). Version-pinned Copilot CLI 1.0.70 fixtures cover a clean prompt-mode turn, hook/system noise, final output, and metadata-only OTel; the remaining live-verification items follow.

1. Install a user hook file containing every native camelCase event; prove discovery in interactive and `-p` modes, `disableAllHooks`, `COPILOT_HOME`, path quoting, cwd, environment inheritance, timeout, stderr, neutral stdout, and uninstall.
2. Record hook payloads for new/resume/new-session switching, prompt, success/failure tool calls, every permission choice, `ask_user`, stop, errors, manual/auto compaction, all built-in subagents, simultaneous same-name subagents, and every session-end reason.
3. Prove whether `permissionRequest` input contains permission kind, tool arguments, path/URL/command subject, or a stable request ID; the current official reference omits its input schema.
4. Determine whether `preToolUse(ask_user)` carries the question/options and whether any hook output can answer it; otherwise map answers exclusively through pane send.
5. Capture statusline input and invocation behavior, including chaining an existing command, before using it for `AgentContext`.
6. Capture child and resumed `events.jsonl` shapes plus `--output-format=json`; keep the reader on guarded visible root messages until those identities are proven.
7. Extend OTel only after shared-file interactive concurrency, cumulative-versus-turn replacement, `github.copilot.cost` units, subagent span identity, compaction close, and long-running flush behavior are pinned.
8. Confirm process names, argv, parent/child tree, cwd, and environment stamping under normal, resume, remote, `-p`, ACP, worktree, and auto-update/restart paths for PID attribution.
9. Decide whether RimZ owns a statusline wrapper; OTel stays user-opt-in unless a managed exporter path and its trust/privacy contract become product behavior.
10. Keep coverage honest: transcript/history and the internal usage endpoint are captured compatibility, usage/model/plan/quota are partial enrichment, and cost/spend/subagent remain unsupported until their own sources are proven.

The expected initial lifecycle mapping is:

```text
sessionStart(initial)   -> duplicate PromptSubmitted edge
sessionStart(promptless)-> SessionStarted / SessionResumed
userPromptSubmitted     -> PromptSubmitted
preToolUse              -> ToolStarted (proof of work)
permissionRequest       -> AwaitingUser (permission)
preToolUse(ask_user)    -> AwaitingUser (question, if payload is sufficient)
agentStop               -> TurnCompleted
errorOccurred           -> TurnError
preCompact              -> CompactionStarted
OTel compaction_complete-> CompactionEnded (enrichment-backed close)
subagentStart/Stop      -> child lifecycle (partial identity)
sessionEnd              -> SessionEnded
```

Keep the neutral hook response empty, preserve unknown fields and tool names, and make every transcript/statusline/OTel read best-effort. Hooks establish lifecycle truth; pane liveness confirms death; local files and telemetry enrich the row.

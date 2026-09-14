# GitHub Copilot CLI protocol reference

This page mirrors the GitHub Copilot CLI surfaces a RimZ adapter binds to: lifecycle hooks and their decision channel, session identity and the local session store, the custom statusline command, OpenTelemetry, interactive, programmatic, and ACP launch, modes and permissions, authentication and account usage, and remote control. It records what upstream ships. How RimZ maps these surfaces, and which it wires, lives in [adapter_copilot.md](../../internals/agents/adapter_copilot.md); the provider-neutral contracts are [model.md](../../internals/agents/model.md) and [providers.md](../../internals/agents/providers.md).

Refresh baseline: GitHub Copilot CLI **1.0.83** (tag [`v1.0.83`](https://github.com/github/copilot-cli/releases/tag/v1.0.83), commit `be82101`, released 2026-09-04, npm `@github/copilot` `latest`), with the official GitHub documentation and the installed 1.0.83 binary's help read on **2026-09-13**. GitHub publishes the executable, installer, and changelog in [`github/copilot-cli`](https://github.com/github/copilot-cli) but not the runtime source, and the binary is a compressed single file with no readable strings, so the official docs and `copilot help` output are the wire evidence. Where the two disagree, the page follows the binary and flags the claim.

Four surfaces have no published schema and carry their own pins: the hook fields observed beyond the reference ([Captured hook wire](#captured-hook-wire), 1.0.70 and 1.0.71), the `events.jsonl` records ([Session event log](#session-event-log), 1.0.70 and 1.0.71), the statusline input ([Statusline input](#statusline-input), 1.0.70 and 1.0.71), and the file exporter's flush behaviour ([Captured file-exporter behaviour](#captured-file-exporter-behaviour), 1.0.70). Nothing in 1.0.72 through 1.0.83's changelog announces a change to them, and no newer capture exists.

## Upstream sources

| Surface | Source |
| --- | --- |
| CLI repository, install, license | <https://github.com/github/copilot-cli> |
| Releases and changelog | <https://github.com/github/copilot-cli/releases> · <https://github.com/github/copilot-cli/blob/main/changelog.md> |
| Commands, options, slash commands, environment, permission patterns, OTel | <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference> |
| Hook locations, entry types, events, payloads, outputs, exit codes | <https://docs.github.com/en/copilot/reference/hooks-reference> |
| Configuration directory, settings, session files | <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference> |
| Programmatic `-p` mode | <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference> |
| ACP server | <https://docs.github.com/en/copilot/reference/copilot-cli-reference/acp-server> |
| Session data, sync, and Chronicle | <https://docs.github.com/en/copilot/concepts/agents/copilot-cli/chronicle> · <https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/chronicle> |
| Remote control | <https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/steer-remotely> |
| Authentication | <https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli> · <https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/troubleshoot-copilot-cli-auth> |
| Tool allow and deny rules | <https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/allowing-tools> |
| Modes and autopilot | <https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-copilot-cli> · <https://docs.github.com/en/copilot/concepts/agents/copilot-cli/autopilot> |
| Billing usage REST API | <https://docs.github.com/en/rest/billing/usage> |

The installed executable carries the same references as help topics. Set `COPILOT_AUTO_UPDATE=false` (or pass `--no-auto-update`) on read-only probes so evidence collection never downloads a build.

```sh
copilot --help
copilot help config         # settings.json keys
copilot help environment    # environment variables
copilot help monitoring     # OpenTelemetry
copilot help permissions    # tool, URL, and path rules
copilot help commands       # slash commands
copilot help billing        # AI credit surfaces
copilot help limits         # session AI credit limits
copilot login --help
```

The two version probes print different banners. `copilot --version` prints `GitHub Copilot CLI 1.0.83.` (with a trailing period) followed by `Run 'copilot update' to check for updates.`; `copilot version` prints `GitHub Copilot CLI 1.0.83` without the period, a blank line, and an update-status line.

## Hooks

Copilot CLI hooks run external commands, HTTP endpoints, or auto-submitted prompts at lifecycle points. A command hook receives one JSON object on stdin and may print one JSON output object on stdout; everything else it wants to say belongs on stderr.

### Discovery and merge order

Copilot loads hook entries from every active source and runs all entries registered for an event. The [hooks reference](https://docs.github.com/en/copilot/reference/hooks-reference#hooks-locations) lists the sources in this order:

| Source | Location | Notes |
| --- | --- | --- |
| Policy hook files | `/etc/github-copilot/policy.d/*.json` (Linux, macOS); `C:\ProgramData\GitHub\Copilot\policy.d\*.json` and `HKLM\Software\Policies\GitHub\Copilot` (Windows) | alphabetical; POSIX files must be root-owned and not group- or world-writable; not disabled by `disableAllHooks`; load regardless of folder trust |
| Repository hook files | `.github/hooks/*.json` | the only source Copilot cloud agent reads |
| User hook files | `$COPILOT_HOME/hooks/*.json`, default `~/.copilot/hooks/*.json` | |
| Repository settings | top-level `hooks` in `.github/copilot/settings.json`, `.github/copilot/settings.local.json`, `.claude/settings.json`, `.claude/settings.local.json` | the `.claude` files contribute a shared cross-tool subset |
| User settings | top-level `hooks` in `$COPILOT_HOME/settings.json` | |
| Plugins | each installed plugin's `hooks.json` or `hooks/hooks.json` | |

The same page's prose summary says "policy, then user, then project, then plugins", which contradicts its own list on whether user or repository hook files come first. The order matters where outputs merge (`permissionRequest`, `subagentStop.modifiedResponse`); no source settles it.

A malformed item in a directory-sourced hook file is dropped and logged while its siblings load; invalid JSON, a bad `version`, or a non-array event list rejects the whole file. An inline `hooks` block in `settings.json` is strict: any item error rejects the whole field.

`disableAllHooks: true` skips hooks at two scopes. Inside one `.github/hooks/*.json` file it skips that file's hooks. At the top level of repository `settings.json` it skips every hook from every source except policy hooks. `copilot help config` and the [settings table](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference#configuration-file-settings) also list it as a user setting that disables repository and user hooks, and repository settings take precedence over user settings for this key.

Prompt mode (`-p`) loads repository hooks only when the folder is already trusted, `COPILOT_ALLOW_ALL` is set, or `GITHUB_COPILOT_PROMPT_MODE_REPO_HOOKS=true` ([environment variables](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference#environment-variables)). User hook files carry no such gate.

### Hook entry types

A hook file is version `1` JSON with a `hooks` object keyed by event name. Each event holds an array of entries, and each entry is one of three types.

```json
{
  "version": 1,
  "hooks": {
    "agentStop": [
      { "type": "command", "bash": "/usr/local/bin/on-stop", "timeoutSec": 30 }
    ]
  }
}
```

Command entries run a shell command or, with `exec`, an executable directly:

| Field | Contract |
| --- | --- |
| `type` | `"command"`; optional, the default |
| `bash` | Unix shell command; one of `bash`, `powershell`, or `command` is required unless `exec` is set |
| `powershell` | Windows shell command |
| `command` | cross-platform fallback copied to whichever of `bash` and `powershell` is absent |
| `exec` | executable name or path run without a shell; CLI only; never combined with `bash`, `powershell`, or `command` |
| `args` | string array passed to `exec` without shell interpretation; CLI only |
| `cwd` | absolute, or relative to the repository root |
| `env` | environment overlay with variable expansion |
| `timeoutSec` | seconds; default `30` |
| `timeout` | alias read only when `timeoutSec` is absent |
| `matcher` | anchored regex on an event-specific value; see [Matchers and tool names](#matchers-and-tool-names) |

The docs do not name the shell that runs `bash` strings. Since 1.0.72, lifecycle and subagent hook commands run in the session's current directory after `/cd` ([changelog 1.0.72](https://github.com/github/copilot-cli/releases/tag/v1.0.72)).

HTTP entries (`type: "http"`) `POST` the same input JSON to `url`, with optional `headers`, `allowedEnvVars` (names that may expand inside `headers`, which forces `https://`), `timeoutSec`, and `timeout`. `url` must be `https://` for `preToolUse` and `permissionRequest`. Other events reject plain `http://` except loopback (`localhost`, `127.*`, `[::1]`) when `COPILOT_HOOK_ALLOW_LOCALHOST=1`. HTTP `preToolUse` hooks fail open on network errors, timeouts, and non-2xx responses.

Prompt entries (`type: "prompt"`, with `prompt`) auto-submit text or a slash command as if the user typed it. They are valid only on `sessionStart`, CLI only, and fire only for new interactive sessions: never on resume and never under `-p`.

### Payload dialects

The configured event name selects the payload dialect:

| Dialect | Event names | Fields | `timestamp` | Event name in payload |
| --- | --- | --- | --- | --- |
| camelCase | `sessionStart`, `preToolUse`, … | camelCase | Unix milliseconds (number) | absent |
| VS Code compatible | `SessionStart`, `PreToolUse`, `Stop`, `UserPromptSubmit`, … | snake_case | ISO 8601 string | `hook_event_name` |

Only the camelCase dialect covers every event: `userPromptTransformed`, `subagentStart`, and `notification` document no PascalCase form. Every payload carries `sessionId`, `timestamp`, and `cwd` (`session_id`, `timestamp`, `cwd` in the VS Code dialect). No common field names the model, effort, permission mode, or event; a command that serves several events must learn the event from its own configuration.

Two camelCase payloads break the casing rule: `agentStop` carries `stop_hook_active`, and `notification` carries `hook_event_name` and `notification_type`.

### Events

The reference documents thirteen events. The fields column lists the camelCase input beyond `sessionId`, `timestamp`, and `cwd`.

| Event | Fires | Input fields | Output processed |
| --- | --- | --- | --- |
| `sessionStart` | a new or resumed session begins | `source: "startup" \| "resume" \| "new"`, `initialPrompt?` | `additionalContext` |
| `userPromptSubmitted` | the user submits a prompt | `prompt` | `modifiedPrompt`, honoured only for SDK programmatic hooks; command and HTTP output is dropped |
| `userPromptTransformed` | after the runtime transforms a prompt into model-facing content, for each message in a batched submission | `prompt`, `transformedPrompt` | `modifiedTransformedPrompt` |
| `preToolUse` | before each tool | `toolName`, `toolArgs` | allow, deny, ask, or modify |
| `postToolUse` | a tool succeeds | `toolName`, `toolArgs`, `toolResult: { resultType: "success", textResultForLlm }` | `modifiedResult`, `additionalContext` |
| `postToolUseFailure` | a tool fails | `toolName`, `toolArgs`, `error: string` | recovery context through exit `2` |
| `permissionRequest` | before the permission service (rules, session approvals, auto decisions, prompt) | no published schema; matchers see `toolName`, and the decision rules name `toolInput.requestSandboxBypass` | allow or deny |
| `agentStop` | the main agent finishes a turn | `transcriptPath`, `stopReason: "end_turn"`, `stop_hook_active: boolean` | block to continue |
| `subagentStart` | a subagent is spawned, before it runs | `transcriptPath`, `agentName`, `agentDisplayName?`, `agentDescription?` | `additionalContext` prepended to the child prompt; cannot block |
| `subagentStop` | a subagent completes normally, before its result returns | `transcriptPath`, `agentId`, `agentType`, `agentName`, `agentDisplayName?`, `response`, `stopReason: "end_turn"` | block to continue, or `modifiedResponse` |
| `preCompact` | manual or automatic compaction begins | `transcriptPath`, `trigger: "manual" \| "auto"`, `customInstructions` | none |
| `errorOccurred` | a runtime error | `error: { message, name, stack? }`, `errorContext: "model_call" \| "tool_execution" \| "system" \| "user_input"`, `recoverable` | none |
| `notification` | the CLI emits a system notification (asynchronous) | `hook_event_name: "Notification"`, `message`, `title?`, `notification_type` | `additionalContext` |
| `sessionEnd` | the session terminates | `reason: "complete" \| "error" \| "abort" \| "timeout" \| "user_exit"` | none |

No hook fires after compaction, on a model or effort change, on a rate limit, or per streamed message. OTel carries compaction start and completion as span events ([Span events](#span-events)).

`stop_hook_active` is `true` when a prior `block` from the same hook already forced this turn to continue. `subagentStop.response` carries the full final subagent text (`last_assistant_message` in the VS Code dialect), because the hook fires before large-response spill handling.

The built-in `general-purpose` agent emits neither `subagentStart` nor `subagentStop`. The other built-in agents (`explore`, `task`, `code-review`, `rubber-duck`, `research`, `security-review`) and user custom agents emit both. The docs do not say whether `subagentStop.agentId` is unique per child run or names the definition. In a Copilot CLI 1.0.83 capture, a `general-purpose` child and an `explore` child each emitted both events: `subagentStart` carried the parent `sessionId` and `agentName`, and `subagentStop.agentId` was that child run's UUID.

The reference does not define the file behind `transcriptPath` or promise that it is `session-state/<sessionId>/events.jsonl`; the 1.0.70 capture below shows that it is for a root `agentStop`.

For a run whose prompt is piped over stdin, as under `-p`, `sessionEnd` fires once per completed agent turn with `reason` `complete` or `error`, and a run that exits before completing a turn fires none ([changelog 1.0.78](https://github.com/github/copilot-cli/releases/tag/v1.0.78)).

The 1.0.81 changelog says hook inputs gain `traceparent` (plus `tracestate` when the span has vendor state) and that command hooks also receive the trace context in environment variables ([changelog 1.0.81](https://github.com/github/copilot-cli/releases/tag/v1.0.81)). Neither the hooks reference nor `copilot help` names the fields' placement or the variables.

### Captured hook wire

Live captures show payload shapes the reference does not describe. They are compatibility evidence at the version named, not an upstream contract.

Copilot CLI 1.0.71 sent `preToolUse`, `postToolUse`, and `postToolUseFailure` tool detail as a batch, `toolCalls: [{name, args}]`, in place of the documented singular `toolName`/`toolArgs`. `args` arrived either as an object or as a JSON-encoded string, and one batch could hold `ask_user` beside other calls. Selecting or dismissing an `ask_user` question emitted `postToolUse` before the next assistant output. `postToolUseFailure.error` is a string, unlike the object in `errorOccurred`.

Copilot CLI 1.0.71 fired the ordinary `userPromptSubmitted` and `agentStop` hooks once per `general-purpose` child. Each child payload set `sessionId` to the parent's `task` tool `toolCallId` (a `toolu_…` value), kept the parent `cwd`, and carried an empty `transcriptPath`; two simultaneous children produced distinct IDs.

Copilot CLI 1.0.83 gives each child a UUID `sessionId` instead, equal to the top-level `agentId` on the parent's `subagent.started`, `subagent.completed`, and child tool records, while `toolCallId` becomes a `call_…` value. The child also fires `userPromptTransformed`, `preToolUse`, and `postToolUse` under that UUID, and its `agentStop.transcriptPath` names the parent's `events.jsonl`. A child's `permissionRequest` carries the parent `sessionId`, with extra `hookName`, `toolInput`, and `permissionSuggestions` fields. A 1.0.83 `-p` capture on 2026-09-14 showed a child `bash` request with `toolInput: {command}`, while the same call's child `preToolUse` and `postToolUse` `toolArgs` held `command`, `description`, `initial_wait`, and `mode`; no hook of the three carried a `toolCallId`. A `permissionRequest` hook `deny` fired neither `postToolUse` nor `postToolUseFailure` for the child call, and the child went straight to `agentStop`.

Copilot CLI 1.0.70 ran the hooks of a fresh prompt-seeded (`-i`) turn in the order `userPromptSubmitted`, `sessionStart`, `agentStop`, `sessionEnd`, and `sessionStart.initialPrompt` repeated the already-submitted prompt. A root `agentStop.transcriptPath` named `$COPILOT_HOME/session-state/<sessionId>/events.jsonl`.

### Hook outputs

Empty stdout or `{}` with exit `0` is neutral: the event proceeds as if no hook ran, and permission and question UI stay native. Hook output (stdout for command hooks, the response body for HTTP hooks) is capped at 10 MiB per invocation and truncated beyond that. A non-string `modifiedPrompt`, `modifiedTransformedPrompt`, or `responseContent` is ignored with a `session.warning`, an empty-string override is rejected, and a `null` `additionalContext` counts as absent.

`sessionStart`, `subagentStart`, and `notification` accept additional context:

```json
{ "additionalContext": "text" }
```

On `sessionStart` it joins the session context, on `subagentStart` it is prepended to the child's prompt, and on `notification` it is injected as a prepended user message that can restart processing when the session is idle.

`preToolUse` decides the call:

```json
{ "permissionDecision": "deny", "permissionDecisionReason": "required for deny", "modifiedArgs": {} }
```

| Field | Contract |
| --- | --- |
| `permissionDecision` | `allow`, `deny`, or `ask`; empty output keeps the default flow; cloud agent treats `ask` as `deny` |
| `permissionDecisionReason` | shown to the agent; required for `deny` |
| `modifiedArgs` | replaces the tool arguments |

If any `preToolUse` hook returns `deny`, the tool is blocked. When the CLI shows its hook-permission prompt, feedback the user types with a denial reaches the agent as `Denied by user via preToolUse hook prompt: <reason>. The user provided the following feedback: <feedback>`.

`permissionRequest` short-circuits the permission service:

```json
{ "behavior": "deny", "message": "reason returned to the model", "interrupt": false }
```

| Field | Contract |
| --- | --- |
| `behavior` | `allow` or `deny`; empty output falls through to rules and the prompt |
| `message` | reason fed to the model on deny |
| `interrupt` | with `deny`, `true` stops the agent |

All matching `permissionRequest` hooks run, and later outputs override earlier ones. `read` and `hook` permission kinds short-circuit before hooks run. A request with `requestSandboxBypass: true` in `toolInput` (a shell command asking to leave the sandbox, or a `web_fetch` the sandbox network policy denies) ignores a hook `allow` and still prompts the user; only `deny` propagates.

`agentStop` and `subagentStop` can force another turn:

```json
{ "decision": "block", "reason": "prompt for the next turn" }
```

| Field | Contract |
| --- | --- |
| `decision` | `block` starts another agent turn with `reason` as its prompt; `allow` lets it end |
| `reason` | prompt for the forced turn |
| `modifiedResponse` | `subagentStop` only: replaces the response returned to the parent; a valid `block` discards it, and with several hooks the last one wins |

After 8 consecutive `block` continuations the CLI ends the turn regardless ([runaway guard](https://docs.github.com/en/copilot/reference/hooks-reference#agentstop--subagentstop-decision-control)).

`postToolUse` can replace a successful result or append model context:

```json
{
  "modifiedResult": { "resultType": "success", "textResultForLlm": "replacement result" },
  "additionalContext": "guidance appended after the tool output"
}
```

A `modifiedResult` with `resultType: "failure"` routes to `postToolUseFailure`. Multiple `additionalContext` values join with a blank line and are capped at 10 KB. Command and HTTP hooks both honour `modifiedResult`.

### Exit codes and stdout parsing

| Result | Behaviour |
| --- | --- |
| exit `0` | stdout, if any, is the output JSON |
| exit `2` | warning with stderr surfaced; `preToolUse` and `permissionRequest` deny, merging any stdout JSON with the deny even when it says `allow`; `postToolUseFailure` appends stdout to the failure shown to the agent |
| other non-zero | logged, run continues; `preToolUse` denies with `Denied by preToolUse hook (hook errored)` |
| timeout | killed after `timeoutSec` and logged; fail-open for every event, including `preToolUse` and policy hooks |

A command hook may print progress lines while it runs:

```json
{"type":"progress","message":"Checking policy...","temporary":true}
```

The CLI removes every stdout line that trims to one complete JSON object with `"type": "progress"`, concatenates and trims the remaining lines, and parses them with one `JSON.parse`. Progress objects must each fit on one line; the final output object may span lines. Empty or unparseable leftovers count as no output, so two final objects on stdout cancel each other. A `temporary` line replaces the previous temporary line and clears when the assistant responds.

### Matchers and tool names

A camelCase `matcher` is a case-sensitive regex compiled as `^(?:PATTERN)$`; an invalid regex skips the entry.

| Event | Matcher input |
| --- | --- |
| `preToolUse`, `postToolUse`, `permissionRequest` | `toolName` |
| `preCompact` | `trigger` |
| `subagentStart` | `agentName` |
| `notification` | `notification_type` |

The reference lists these native tool names for matching: `ask_user`, `bash`, `powershell`, `create`, `edit`, `glob`, `grep`, `task`, `view`, `web_fetch`. Other runtime tools exist (the Claude mapping below names `str_replace_editor`, `apply_patch`, `rg`, `web_search`, and `update_todo`), so a hook should tolerate unknown names.

PascalCase `PreToolUse` and `PermissionRequest` apply Claude matcher rules and report Claude tool names in `tool_name`. `*`, `**`, or an empty matcher fires for every tool; a literal name or `|` alternation matches either the runtime or the Claude name; anything else is an anchored regex over the Claude name.

| Runtime tool | Claude tool name |
| --- | --- |
| `bash`, `powershell` | `Bash` |
| `view` | `Read` |
| `create` | `Write` |
| `edit`, `str_replace_editor`, `apply_patch` | `Edit` |
| `grep`, `rg` | `Grep` |
| `glob` | `Glob` |
| `web_fetch` | `WebFetch` |
| `web_search` | `WebSearch` |
| `ask_user` | `AskUserQuestion` |
| `update_todo` | `TodoWrite` |
| `task` | `Agent` (`Task` also accepted) |

### Notifications

The `notification` hook is fire-and-forget: it never blocks the session, and its errors are logged and skipped. It does not fire under cloud agent. Its output cannot answer the dialog it reports.

| `notification_type` | Fires when |
| --- | --- |
| `shell_completed` | a background shell command finishes |
| `shell_detached_completed` | a detached shell session completes |
| `agent_completed` | a background subagent completes or fails |
| `agent_idle` | a background agent finishes a turn and waits for `write_agent` |
| `permission_prompt` | the agent requests permission to run a tool |
| `elicitation_dialog` | the agent asks the user for information |

## Sessions and local state

A session is identified by the UUID that every hook carries as `sessionId`. Each session keeps its event log and workspace artifacts under `$COPILOT_HOME/session-state/<sessionId>/`, and that directory is what resume reads.

### Selecting a session

| Option | Behaviour |
| --- | --- |
| `--session-id ID` | resume the session or task whose ID matches exactly; if none matches, create a session only when `ID` is a valid UUID |
| `-r`, `--resume[=VALUE]` | resume by session ID, task ID, ID prefix, or exact case-insensitive name, falling back to the generated summary; bare `--resume` opens a picker that needs a TTY, and without one (under `-p`, a non-TTY `-i`, or piped stdin) the CLI exits with an error when several sessions exist |
| `--continue` | most recent session in the cwd, else the most recent globally |
| `--connect[=ID]` | connect to a remote session or task; requires the remote sessions feature |
| `-n`, `--name NAME` | name a new session for `--resume` and `/resume` |
| `-C DIRECTORY` | change directory before anything else; overrides the directory a resumed session restores |
| `-w`, `--worktree[=NAME]` | start in an isolated Git worktree under `<repo>.worktrees/`; experimental mode only |
| `/resume [ID]`, `/rename [NAME]` | switch or rename sessions inside the UI |
| `/fork [NAME]`, `/branch [NAME]` | fork the current session into a new one |

`--session-id` conflicts with `--resume`, `--continue`, and `--connect`; `--continue` and `--resume` reject each other; `--worktree` conflicts with all three. `--name` with `--session-id` for an existing session is an error. A resumed session restores its working directory and its interactive, plan, or autopilot mode.

### Configuration directory

`COPILOT_HOME` selects the configuration and state root, default `~/.copilot`. The [configuration directory reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference#directory-overview) lists more items; these are the ones an adapter touches.

```text
$COPILOT_HOME/
├── settings.json               # user settings, JSONC
├── config.json                 # managed application state, including auth
├── hooks/                      # user hook files
├── permissions-config.json     # saved tool and directory approvals per location
├── session-state/<sessionId>/
│   ├── events.jsonl            # per-session event log
│   └── …                       # plans, checkpoints, tracked files
├── session-store.db            # SQLite cross-session index and search
└── logs/process-<timestamp>-<pid>.log
```

User settings live in `settings.json`, and the CLI migrates any user settings it finds in `config.json` there at startup. Application state, such as `loggedInUsers`, stays in `config.json`. Settings apply in this order, later winning: built-in defaults, MDM managed settings, user `settings.json`, repository `.github/copilot/settings.json`, local `.github/copilot/settings.local.json`, environment variables, command-line flags.

`session-store.db` is a derived Chronicle index; `/chronicle reindex` rebuilds it and also syncs session data to the account. In a 1.0.70 capture its `turns` rows lagged the live event file until session shutdown.

### Session event log

GitHub documents `events.jsonl` as the per-session event log and publishes no record discriminants, field schema, append guarantee, or compatibility rule. Each line is one JSON record with a top-level `type`, an RFC 3339 `timestamp`, an `id`, and a `data` object. The records below come from 1.0.70 and 1.0.71 captures.

| `type` | `data` fields observed |
| --- | --- |
| `session.start` | `sessionId`, `version`, `producer`, `copilotVersion`, `startTime`, `contextTier`, `alreadyInUse`, `remoteSteerable`, `context: { cwd, gitRoot, branch, headCommit, baseCommit }` |
| `user.message` | `content` (visible text), `transformedContent`, `interactionId` |
| `assistant.turn_start` | `turnId`, `model` |
| `assistant.message` | `content` (visible text), `encryptedContent`, `reasoningOpaque`, `requestId` |
| `system.message` | `role`, `content` |
| `hook.start`, `hook.end` | `hookInvocationId`, `hookType`, `input` |
| `tool.execution_start` | `toolCallId`, `toolName`, `arguments`, `model`, `turnId` |
| `subagent.started` | `toolCallId`, `agentName`, `agentDisplayName`, `agentDescription`, `model` |
| `subagent.completed` | `toolCallId`, `agentName`, `agentDisplayName`, `model`, `totalTokens`, `totalToolCalls`, `durationMs` |
| `session.shutdown` | `shutdownType`, `modelMetrics`, `tokenDetails`, `totalNanoAiu`, `totalPremiumRequests`, `currentModel`, `codeChanges`, `totalApiDurationMs`, and context-size counters |

Records also seen without their fields pinned: `assistant.turn_end`, `session.model_change`, `session.auto_mode_resolved`. Since 1.0.81, `hook.start` and `hook.end` from hooks inside a subagent are recorded on the subagent's session and re-emitted on the parent ([changelog 1.0.81](https://github.com/github/copilot-cli/releases/tag/v1.0.81)).

A delegated `task` call writes its parent-side records in a fixed order in 1.0.71: `tool.execution_start` with `toolName: "task"` and `arguments.name`, `description`, `prompt`, and `agent_type`; then `subagent.started` with the same `toolCallId`, before the child's `userPromptSubmitted` hook; then, after the child's `agentStop` hook, `subagent.completed` with the same `toolCallId`. The parent's next `postToolUse` hook is the first hook after the completed record.

`session.shutdown.data.modelMetrics.<model>` holds cumulative per-model counters. `usage` has `inputTokens`, `cacheReadTokens`, `cacheWriteTokens`, `outputTokens`, and `reasoningTokens`; `tokenDetails` has disjoint `input`, `cache_read`, `cache_write`, and `output` entries, each with a `tokenCount`. `usage.inputTokens` includes both cache categories, and `outputTokens` includes `reasoningTokens`. A resumed session writes a further `session.shutdown` on each exit, repeating cumulative totals. `totalNanoAiu` is nano AI units (see [OpenTelemetry](#traces)); premium-request counts carry no published conversion.

### Sync and privacy

Session data holds prompts, responses, tool calls, and modified-file details, and it syncs to the user's GitHub account by default where policy allows ([session data](https://docs.github.com/en/copilot/concepts/agents/copilot-cli/chronicle)). For Copilot Business and Enterprise, an organization admin must set the "Store local sessions in the Cloud" policy to at least "View from cloud" before anything syncs. `remoteExport: false`, `--no-remote-export`, or `remote: "off"` keep sessions local. Deleting local files does not delete synced copies.

## Custom statusline

The `statusLine` user setting runs a command that receives the session status as JSON on stdin and prints the status line on stdout ([settings table](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference#configuration-file-settings), `copilot help config`).

```json
{
  "statusLine": {
    "type": "command",
    "command": "/absolute/path/to/status-command",
    "padding": 0,
    "refreshInterval": 5
  }
}
```

| Field | Contract |
| --- | --- |
| `type` | optional; when set, must be `"command"` |
| `command` | executable path or shell command; expands `~`, `$VAR`, `${VAR}`, and `${VAR:-default}`; plain commands run under `/bin/sh` on Unix and `cmd.exe` on Windows |
| `padding` | spaces of left padding per line |
| `refreshInterval` | integer seconds, `1` to `2147483`, to rerun on a timer; omitted, the command reruns only when session state changes |

If the command fails to spawn, exits non-zero, or fails to receive its stdin JSON, the CLI logs one warning per continuous failure episode and leaves the line blank; `--log-level all` shows the detail. The docs publish no input schema, timeout, environment, or ANSI rules.

The built-in footer is configured separately under `footer` (`showModelEffort`, `showDirectory`, `showBranch`, `showContextWindow`, `showQuota`, `showAgent`, `showAiUsed`, `showCodeChanges`, `showUsername`, `showSandbox`, `showYolo`, `showCustom`), managed by `/statusline`. `showCustom` places the custom statusline.

### Statusline input

The stdin object is snake_case. These fields come from 1.0.70 and 1.0.71 captures, preserved as a sanitized 1.0.71 fixture.

| Field | Observed content |
| --- | --- |
| `session_id`, `session_name`, `version`, `cwd`, `transcript_path`, `username` | scalars |
| `model.id`, `model.display_name` | selected model; after an auto-model turn `id` is `auto` and `display_name` names the resolved target, for example `Auto → claude-sonnet-4.6 (1x) (medium)` |
| `context_window.displayed_context_limit`, `current_context_tokens`, `current_context_used_percentage` | the window the footer shows, once a model call has resolved it |
| `context_window.context_window_size`, `used_percentage`, `remaining_percentage`, `remaining_tokens` | nominal window fields; null after the 1.0.70 auto-model turn |
| `context_window.current_usage.{input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens}` | latest model call |
| `context_window.last_call_input_tokens`, `last_call_output_tokens`, `total_tokens` | latest-call and aggregate scalars |
| `context_window.total_input_tokens`, `total_output_tokens`, `total_cache_write_tokens`, `total_cache_read_tokens`, `total_reasoning_tokens` | cumulative session counters (1.0.71) |
| `cost.total_duration_ms`, `total_api_duration_ms`, `total_lines_added`, `total_lines_removed`, `total_premium_requests` | session counters; no currency field |
| `ai_used.total_nano_aiu`, `ai_used.formatted` | AI credits used this session |
| `remote.connected` | remote control state |

Cumulative `total_input_tokens` includes both cache categories: the fixture reports `82000` as `7000` cache write plus `69000` cache read plus `6000` fresh input. `total_output_tokens` includes `total_reasoning_tokens`. In the 1.0.71 display name, the resolved model follows the rightmost arrow and may carry a multiplier and effort suffix.

## OpenTelemetry

Copilot CLI exports traces and metrics through OTLP HTTP or a local JSON-lines file, following the OTel GenAI semantic conventions ([OTel monitoring](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference#opentelemetry-monitoring)). OTel is off by default and activates when `COPILOT_OTEL_ENABLED=true`, `OTEL_EXPORTER_OTLP_ENDPOINT` is set, or `COPILOT_OTEL_FILE_EXPORTER_PATH` is set. Enterprise managed settings and VS Code can also enable it.

```sh
COPILOT_OTEL_FILE_EXPORTER_PATH=/path/to/copilot-otel.jsonl copilot
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `COPILOT_OTEL_ENABLED` | `false` | enable OTel explicitly |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | | OTLP endpoint; enables OTel |
| `COPILOT_OTEL_EXPORTER_TYPE` | `otlp-http` | `otlp-http` or `file`; `file` is selected when the file path is set and this variable is unset |
| `COPILOT_OTEL_FILE_EXPORTER_PATH` | | JSON-lines file for all signals; enables OTel |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/json` | `http/json` or `http/protobuf`; `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` and `OTEL_EXPORTER_OTLP_METRICS_PROTOCOL` override per signal |
| `OTEL_EXPORTER_OTLP_HEADERS` | | OTLP auth headers |
| `OTEL_EXPORTER_OTLP_CERTIFICATE`, `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE`, `OTEL_EXPORTER_OTLP_CLIENT_KEY` | | TLS trust and mTLS files, with per-signal variants; `copilot help monitoring` only |
| `OTEL_SERVICE_NAME` | `github-copilot` | `service.name` resource attribute |
| `OTEL_RESOURCE_ATTRIBUTES` | | extra comma-separated, percent-encoded `key=value` resource attributes |
| `COPILOT_OTEL_SOURCE_NAME` | `github.copilot` | instrumentation scope name |
| `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` | `false` | capture prompts, responses, system instructions, tool definitions, and tool arguments and results |
| `OTEL_LOG_LEVEL` | | OTel diagnostic log level |

`copilot help monitoring` adds a behaviour the docs omit: when the `otlp-http` exporter resolves to a plain `http://` endpoint, including the default `http://localhost:4318`, export is disabled without a console message, leaving only a warning in the process log.

### Traces

Each agent interaction produces an `invoke_agent` span with `chat` and `execute_tool` children. Top-level and subagent `invoke_agent` spans are both `INTERNAL`, `chat` spans are `CLIENT`, and `execute_tool` spans are `INTERNAL`; subagent invocations join the parent trace. The 1.0.83 `copilot help monitoring` tree also shows a `plan` span, for plan-mode decomposition, between `invoke_agent` and its `chat` and `execute_tool` children; the docs do not list it.

| Span | Attributes |
| --- | --- |
| `invoke_agent` | `gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.agent.id` (`github.copilot.default` at top level), `gen_ai.agent.name`, `gen_ai.agent.description`, `gen_ai.agent.version`, `gen_ai.conversation.id` (session ID), `enduser.pseudo.id`, `gen_ai.request.model`, `gen_ai.response.finish_reasons`, cumulative `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.cache_creation.input_tokens`, `github.copilot.turn_count`, `github.copilot.cost`, `github.copilot.nano_aiu`, `server.address` and `server.port` (top level only), `error.type` |
| `chat` | `gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.request.stream`, `gen_ai.conversation.id`, `gen_ai.response.finish_reasons`, `gen_ai.response.id`, `gen_ai.response.model` (resolved model), `gen_ai.response.time_to_first_chunk`, per-request token counts under the same four `gen_ai.usage.*` names, `github.copilot.cost`, `github.copilot.nano_aiu`, `github.copilot.server_duration`, `github.copilot.initiator`, `github.copilot.turn_id`, `github.copilot.interaction_id`, `server.address`, `server.port`, `error.type` |
| `execute_tool` | `gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.tool.name`, `gen_ai.tool.type`, `gen_ai.tool.call.id`, `gen_ai.tool.description`, `error.type` |

`github.copilot.cost` is a per-request model multiplier used in billing and is not a currency value. `github.copilot.nano_aiu` counts AI units in billionths (1 AIU = 1,000,000,000 nano AIU); read it from the root `invoke_agent` span only, because summing it across `chat` children double-counts. Content-capture attributes (`gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.system_instructions`, `gen_ai.tool.definitions`, `gen_ai.tool.call.arguments`, `gen_ai.tool.call.result`) appear only when capture is on.

### Span events

Lifecycle events are recorded on the active `chat` or `invoke_agent` span.

| Event | Key attributes |
| --- | --- |
| `github.copilot.hook.start`, `github.copilot.hook.end` | `github.copilot.hook.type`, `github.copilot.hook.invocation_id` |
| `github.copilot.hook.error` | the same, plus `github.copilot.hook.error_message` |
| `github.copilot.session.truncation` | `token_limit`, `pre_tokens`, `post_tokens`, `pre_messages`, `post_messages`, `tokens_removed`, `messages_removed`, `performed_by` (each under `github.copilot.`) |
| `github.copilot.session.compaction_start` | none |
| `github.copilot.session.compaction_complete` | `github.copilot.success`, `pre_tokens`, `post_tokens`, `tokens_removed`, `messages_removed`, `message` (content capture only) |
| `github.copilot.skill.invoked` | `github.copilot.skill.name`, `skill.path`, `skill.plugin_name`, `skill.plugin_version` |
| `github.copilot.session.shutdown` | `github.copilot.shutdown_type`, `total_premium_requests`, `lines_added`, `lines_removed`, `files_modified_count` |
| `github.copilot.session.abort` | `github.copilot.abort_reason` |
| `exception` | `github.copilot.error_type`, `error_status_code`, `error_provider_call_id` |

No span or event attribute carries rate-limit or quota state.

### Metrics

Metrics carry no session identity beyond resource attributes; they are indexed here for completeness.

| Metric | Type |
| --- | --- |
| `gen_ai.client.operation.duration`, `gen_ai.client.operation.time_to_first_chunk`, `gen_ai.client.operation.time_per_output_chunk`, `gen_ai.invoke_agent.duration` | histogram, seconds |
| `gen_ai.client.token.usage` | histogram, tokens by type |
| `gen_ai.invoke_agent.inference_calls`, `gen_ai.invoke_agent.tool_calls` | histogram by `gen_ai.agent.name` |
| `github.copilot.tool.call.count`, `github.copilot.mcp.server.connection.count`, `github.copilot.code.lines_added`, `github.copilot.code.lines_removed` | counter |
| `github.copilot.tool.call.duration`, `github.copilot.agent.turn.count` | histogram |
| `github.copilot.sandbox.operation.count`, `github.copilot.sandbox.spawn.duration`, `github.copilot.sandbox.policy.path.count` | sandbox counters and histograms; `copilot help monitoring` only |

### Captured file-exporter behaviour

A 1.0.70 capture pinned how the file exporter writes. Two direct processes running three overlapping turns each against one file produced 84 complete JSON lines (93,721 bytes) with no truncation or interleaving. The completed `chat` span was on disk by the time the `agentStop` hook ran (2,743 bytes), while `invoke_agent` and the metric records were appended during shutdown (15,357 bytes at exit). Every captured `chat` span carried `github.copilot.cost` as `0.0`, and `gen_ai.usage.input_tokens` included the cache-read tokens. Whether the exporter reopens a rotated file is unknown.

## Launch modes

`copilot` starts the interactive UI. `copilot -i PROMPT` (`--interactive`) starts the same UI and submits `PROMPT` at once, keeping native questions and permission prompts. `copilot -p PROMPT` (`--prompt`) runs one non-interactive task and exits; its exit summary prints a `copilot --resume=<id>` hint.

| Option | Contract |
| --- | --- |
| `-s`, `--silent` | print only the agent response, without usage statistics |
| `--output-format text\|json` | `json` is JSONL, one object per line; no object schema is published |
| `--stream on\|off` | progressive or buffered response; default `on` |
| `--model MODEL` | model ID, or `auto` for automatic selection; `COPILOT_MODEL` also sets it |
| `--effort`, `--reasoning-effort LEVEL` | the 1.0.83 binary accepts `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`; the docs list `low` through `max` |
| `--context default\|long_context` | context-window tier for tiered-pricing models; overrides the `contextTier` setting |
| `--mode interactive\|plan\|autopilot`, `--plan`, `--autopilot` | initial mode; see [Modes and permissions](#modes-and-permissions) |
| `--agent NAME` | custom agent, including a plugin agent as `<plugin>:<agent>` |
| `--no-ask-user` | remove the `ask_user` tool |
| `--allow-tool`, `--deny-tool`, `--allow-url`, `--deny-url`, `--allow-all-tools`, `--allow-all-paths`, `--allow-all-urls`, `--allow-all`, `--yolo` | permission rules; `--allow-all-tools` (or `COPILOT_ALLOW_ALL=true`) is required for tool use without prompts |
| `--available-tools`, `--excluded-tools` | which tools the model sees at all |
| `--share[=PATH]`, `--share-gist` | export the session after a `-p` run, default `./copilot-session-<id>.md`; a failed export exits non-zero |
| `--usage-output-file FILE` | write final usage statistics as JSON, with per-agent metrics since 1.0.81 |
| `--max-ai-credits CREDITS` | soft AI credit limit, minimum 30 |
| `--attachment PATH` | attach a file to the initial prompt; `-p` only |
| `--no-auto-update` | skip the automatic update download (also `COPILOT_AUTO_UPDATE=false`; off by default in CI) |

`-p` exits with a failure code when a prompt is blocked before any response. `permissionRequest` hooks apply under `-p` and can supply the decisions no prompt can. No flag writes a session ID or completion record in machine-readable form other than the unschematized JSONL and `--usage-output-file`.

The binary and the docs disagree on `--max-ai-credits`: `copilot help limits` scopes the limit to the whole session (reset by `/clear`), while the [command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference#command-line-options) says it resets per user message.

## ACP server

`copilot --acp` starts the Agent Client Protocol server, in public preview ([ACP server](https://docs.github.com/en/copilot/reference/copilot-cli-reference/acp-server)). stdio is the default transport and `--stdio` names it; `--port PORT` listens on TCP, bound to `127.0.0.1`. The two are mutually exclusive and both carry NDJSON. A stdio server serves the one client that spawned it and exits when stdin closes; a TCP server accepts many connections and runs until stopped. Neither flag appears in `copilot --help`.

`session/new` sets only per-session parameters such as `cwd` and MCP servers. Tool filters (`--available-tools`, `--excluded-tools`) and `--effort` passed to the server process apply to every session it creates or loads. BYOK provider configuration lets ACP sessions run without a GitHub login.

A client spawns `copilot --acp --stdio`, sends `initialize` with its protocol version and capabilities, creates a session with `cwd` and MCP servers, sends `prompt`, consumes `sessionUpdate` notifications (text arrives as `agent_message_chunk`), and answers `requestPermission`. Since 1.0.78 prompt results and live `usage_update` notifications carry token usage and clients can send `closeSession`; since 1.0.81 clients receive subagent IDs, raw event subscriptions, and live title, mode, command, and plan updates ([changelog 1.0.78](https://github.com/github/copilot-cli/releases/tag/v1.0.78), [1.0.81](https://github.com/github/copilot-cli/releases/tag/v1.0.81)). ACP hosts the agent; it cannot observe a separately running interactive session.

## Modes and permissions

Copilot has three agent modes: interactive (standard), plan, and autopilot. `Shift+Tab` cycles them, and `--mode`, `--plan`, and `--autopilot` choose one at launch.

| Mode | Behaviour |
| --- | --- |
| interactive | the agent works and prompts for permissions and questions |
| plan | builds a plan; built-in tools that would modify the workspace are blocked outside the session folder, while MCP and external tools stay allowed |
| autopilot | continues without waiting for the user until the agent calls `task_complete`, up to `--max-autopilot-continues` continuation messages |

`--plan --mode autopilot` (or `COPILOT_PLAN_THEN_AUTOPILOT=1`) starts in plan mode and moves to autopilot once the plan is ready; `--plan` rejects any other `--mode` and cannot combine with `--autopilot`. `stayInAutopilot` (default `true`) keeps autopilot selected after `task_complete`. The 1.0.83 binary documents a `--max-autopilot-continues` default of `5`, as does the [autopilot page](https://docs.github.com/en/copilot/concepts/agents/copilot-cli/autopilot); the command reference says unlimited.

Two settings choose how new interactive sessions start; neither applies to resumed sessions, `-p`, or `--acp`, and launch flags win.

| Setting | Values |
| --- | --- |
| `defaultMode` | `interactive` (default), `plan`, `autopilot` |
| `defaultPermissionMode` | `manual` (default: prompt for writes and commands, auto-approve reads), `assisted` (an LLM safety check approves what it judges safe; needs the experimental auto-approval feature), `allow-all` |

`--allow-all` and `--yolo` equal `--allow-all-tools --allow-all-paths --allow-all-urls`. Managed `permissions.disableBypassPermissionsMode` restricts them: `"disable"` suppresses every allow-all flag and the `/permissions allow-all`, `/allow-all`, and `/yolo` commands at startup, and `"allow-auto-only"` blocks full allow-all while permitting assisted approval. An unrecognized value is enforced as `"disable"`. An MDM value of `"disable"` always wins over user settings.

Tool rules take `kind(argument)` patterns ([permission patterns](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference#tool-permission-patterns)):

| Kind | Matches |
| --- | --- |
| `shell(command)` | a shell command; `:*` matches a prefix, as in `shell(git:*)` |
| `write(path)` | file-creating and file-editing tools; a relative path matches trailing components |
| `read(path)` | file reads |
| `url(domain-or-url)` | URL access by shell and `web_fetch`; protocol-aware, defaulting to `https://` |
| `memory` | storing facts to agent memory |
| `<mcp-server>(tool)` | an MCP server's tool, or all its tools |

Deny rules always win, including over `--allow-all-tools`. Saved approvals live in `$COPILOT_HOME/permissions-config.json`, keyed by location, and do not carry across repositories after `/cd`.

## Authentication and account

`copilot login` authenticates through GitHub OAuth. It uses the browser flow by default on local terminals and desktop subprocesses and the device-code flow on remote or headless terminals; `--web-flow` and `--device-code` force one, `--with-token` reads a token from stdin, and `--host` selects a GitHub host. Credentials go to the system credential store. Without one, tokens are stored in plain text in `$COPILOT_HOME/config.json` only if the user consents or `storeTokenPlaintext` is `true`.

Environment tokens take precedence over stored credentials, in the order `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`. In Codespaces the auto-injected `GITHUB_TOKEN` does not override a `/login` account, though an explicitly exported one does. Supported tokens are fine-grained PATs with the **Copilot Requests** permission, Copilot CLI OAuth tokens, and GitHub CLI OAuth tokens; classic `ghp_` PATs are not. `GH_HOST` selects the host for both GitHub CLI and Copilot, and `COPILOT_GH_HOST` overrides it for Copilot only. `COPILOT_OFFLINE=true` with a BYOK provider (`COPILOT_PROVIDER_BASE_URL`) skips GitHub authentication entirely.

No command reports auth status in machine-readable form; `/user show`, `/user list`, and `/user switch` are interactive. The config directory reference names `loggedInUsers` as application state in `config.json`. A captured `config.json` holds the current identity as `lastLoggedInUser: {host, login}`, the known identities as `loggedInUsers: [{host, login}]`, and plaintext-stored tokens under `copilotTokens`, keyed `<host>:<login>`.

### Account usage response

`GET /copilot_internal/user` is an undocumented endpoint that returns the signed-in account's Copilot plan and quotas. [CodexBar's Copilot usage fetcher](https://github.com/steipete/CodexBar/blob/main/Sources/CodexBarCore/Providers/Copilot/CopilotUsageFetcher.swift) calls it at `https://api.github.com/copilot_internal/user` (an enterprise host maps to `api.<host>`) with `Authorization: token <credential>`, GitHub JSON accept and API-version headers, and Copilot editor and plugin user-agent headers. It is compatibility evidence, not a GitHub API contract.

| Field | Content |
| --- | --- |
| `copilot_plan` | plan name |
| `token_based_billing` | whether the account bills by AI credits |
| `quota_reset_date` | RFC 3339 timestamp or calendar date |
| `quota_snapshots.{chat, premium_interactions, completions}` | per-scope `entitlement`, `remaining`, `percent_remaining`, `unlimited` |
| `monthly_quotas`, `limited_user_quotas`, `limited_user_reset_date` | legacy per-scope entitlements and remaining counts |

Business responses can carry zero-entitlement placeholders and no usable quota alongside a valid plan. The response has no rolling windows; quotas are monthly.

### Billing and AI credits

Copilot measures usage in AI credits, priced at $0.01 per credit (the billing usage API's `pricePerUnit`); accounts on the legacy billing platform see premium requests instead (`copilot help billing`, [billing concepts](https://docs.github.com/en/copilot/concepts/billing)). In the CLI, the footer shows the remaining budget, `/usage` the session's credits and token breakdown, and `/limits` or `--max-ai-credits` a soft session limit (minimum 30 credits), which a single model call may overshoot because usage is known only after the call returns.

GitHub's [billing usage API](https://docs.github.com/en/rest/billing/usage) reports billing-account aggregates:

| Endpoint | Access |
| --- | --- |
| `GET /users/{username}/settings/billing/ai_credit/usage`, `GET /users/{username}/settings/billing/premium_request/usage` | usage billed to a personal account; fine-grained tokens need user **Plan: read** |
| `GET /organizations/{org}/settings/billing/ai_credit/usage`, `GET /organizations/{org}/settings/billing/premium_request/usage` | organization-billed usage; caller must administer the organization; fine-grained tokens need **Administration: read** |
| `GET /users/{username}/settings/billing/usage`, `…/usage/summary`, and the organization equivalents | general billing usage and summary reports; the summary is in public preview |

These endpoints report per account and period, need broader access than a CLI login, and carry no session or turn identity.

## Remote control

Remote control lets GitHub.com and GitHub Mobile view a running local session, answer its permission requests, and continue the conversation ([steer remotely](https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/steer-remotely)). The host machine must stay online with the session running in a terminal. It needs no GitHub-hosted repository: sessions outside one appear with no repository.

Enable it with `copilot --remote`, `/remote on`, or a setting; disable it per launch with `--no-remote` or in session with `/remote off`. `/remote` alone shows the status and the access links. `--remote` with `--resume <task-id>` resumes a remote task locally.

The docs disagree on the setting. The how-to enables it with `"remoteSessions": true` in `settings.json`, while the [settings table](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference#configuration-file-settings) documents `remote: "on" | "off"` (default `"on"`, controlling sync and remote access) and mentions `remoteSessions` only inside the `remoteExport` row. `remoteExport` (default `true`) and `--remote-export` control read-only export; `--no-remote-export` also disables remote control.

All remote flags require the remote sessions feature on the account. Enterprise managed settings can disable remote control of sessions on a device or restrict it to SSO-authorized controlling clients: the managed `remoteControl.mode` is `enabled`, `disabled`, or `requireSSO` with `githubDotComOrganizations`.

## Upstream scope

These surfaces have no published contract at 1.0.83, so anything built on them rests on the captures above or on nothing:

- the `permissionRequest` input schema, including any permission kind, subject, or request ID;
- whether any hook output can answer an `ask_user` question or `elicitation_dialog`;
- the `events.jsonl` record schema and its compatibility rules;
- the `--output-format json` record schema;
- the statusline input schema, its timeout, and its environment;
- the shell that runs command-hook `bash` strings, and the placement of the 1.0.81 `traceparent` fields;
- the `copilot_internal/user` response.

RimZ's mapping of these surfaces and the gaps it records are in [adapter_copilot.md](../../internals/agents/adapter_copilot.md#known-gaps).

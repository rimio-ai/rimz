# Muse Code protocol reference

Meta Muse Code is a terminal and CI coding agent, launched with `muse`. It runs on macOS, Linux, and Windows. Agent-run shell commands use PowerShell on Windows. See the [official overview](https://dev.meta.ai/docs/muse-code).

This page records upstream contracts for contributors. It makes no claim about RimZ support. The [adapter playbook](../../contributing/agent-adapters.md#step-1--land-the-protocol-reference-first) owns the integration sequence.

## Evidence baseline and sources

Sources read on 2026-10-15. The newest entry in the rolling [changelog](https://dev.meta.ai/docs/muse-code/changelog#v1-4-8) is 1.4.8. The initial documentation baseline was followed by the [1.4.9 capture](#149-capture) below, using an installed binary and disposable echo sessions. No credentials were inspected or network model calls made.

The [official overview](https://dev.meta.ai/docs/muse-code#sdk) links the Muse Code SDK site and names `@muse-code/sdk`. That link establishes the SDK site's upstream provenance. SDK links below pin the `1.4` documentation series. Its hook pages say their examples and values were checked against Muse Code 1.3.0. Later changelog additions appear separately below where those tables are incomplete. These are documentation baselines, not a claim of runtime conformance on 1.4.8.

| Surface | Owning upstream source |
| --- | --- |
| Installation, platforms, SDK discovery | [Overview](https://dev.meta.ai/docs/muse-code) |
| Version changes | [Changelog](https://dev.meta.ai/docs/muse-code/changelog) |
| Launch flags and settings | [Configuration and context](https://dev.meta.ai/docs/muse-code/configuration) |
| Approval and confinement | [Permissions and safety](https://dev.meta.ai/docs/muse-code/permissions) |
| Hook configuration and execution | [SDK 1.4: Hooks](https://meta-models.github.io/muse-code-sdk/1.4/guides/extend/hooks/) |
| Native hook wire | [SDK 1.4: Hook events and payloads](https://meta-models.github.io/muse-code-sdk/1.4/guides/plugins/reference/hook-events/) |
| Session commands and exports | [Working with the agent](https://dev.meta.ai/docs/muse-code/interactive) |
| Served session identity and turns | [SDK 1.4: Sessions and turns](https://meta-models.github.io/muse-code-sdk/1.4/guides/msp-concepts/sessions-and-turns/) |
| Authentication and billing | [Authentication and billing](https://dev.meta.ai/docs/muse-code/auth) |
| Headless runs, skills, MCP, subagents | [Extending and automating](https://dev.meta.ai/docs/muse-code/extending) |
| Plugin package schema | [SDK 1.4: Manifest](https://meta-models.github.io/muse-code-sdk/1.4/guides/plugins/reference/manifest/) |

## Launch and settings

The [overview](https://dev.meta.ai/docs/muse-code#install) provides native installers and the version probe `muse --version`. The interactive `muse` and headless `muse exec` commands have separate flag sets. This subset follows [Configuration and context](https://dev.meta.ai/docs/muse-code/configuration).

| Surface | Flags or setting |
| --- | --- |
| Both launch modes | `--model <id>`, `--reasoning-effort <level>`, `--workspace <path>`, `--no-session-log` |
| Permission flags in both launch modes | `--approval-mode <mode>`, `--approval-judge <on\|off>`, `--sandbox-network <mode>`, `--disable-sandbox`, `--disable-approval`, `--yolo`, `--trust-workspace` |
| Headless only | `--json`, `--prompt-file <path>`, `--max-model-steps <n>` |
| Model default | `muse-spark-1.2` |
| Reasoning levels | `none`, `minimal`, `low`, `medium`, `high` (default), `xhigh`, `max`, `ultra` |
| User settings | `~/.config/muse/settings.json` |

The [SDK hook guide](https://meta-models.github.io/muse-code-sdk/1.4/guides/extend/hooks/) resolves user settings through `$XDG_CONFIG_HOME/muse/settings.json`. It requires top-level `schema_version: 1`. Other data and credential paths need separate resolution evidence.

`muse exec` documents a positional prompt. The [1.4.9 capture](#149-capture) establishes interactive positional prompts, including the double-dash form for a leading hyphen and its subcommand-name limitation.

`--no-session-log` disables retained state, including resume and recovery of saved workflow runs. [Configuration and context](https://dev.meta.ai/docs/muse-code/configuration) documents this for both launch modes.

For an existing retained session, the [1.4.0 changelog](https://dev.meta.ai/docs/muse-code/changelog#v1-4-0) documents a narrower exception. `muse resume <id> --no-session-log` replays it read-only. `resume --last` and the startup picker still require logging.

## Permissions and sandbox

Profiles combine approval mode and judge settings. `permissions.default_profile` selects the startup profile. Ask me and Auto-review both use `on-request`. Ask me disables the judge. These controls follow [Permissions and safety](https://dev.meta.ai/docs/muse-code/permissions).

The [1.4.8 changelog](https://dev.meta.ai/docs/muse-code/changelog#v1-4-8) also names `--permission-profile`. The [1.4.9 capture](#149-capture) establishes the four built-in IDs and refusal when combined with approval mode or yolo.

| Control | Effect |
| --- | --- |
| Default profile | Auto-review. The approval judge handles eligible requests and falls back to the user. |
| Ask me | Human review of eligible requests. |
| Read-only | Workspace reads without writes or side-effecting commands. |
| `--approval-mode on-request` | Default. Safe parsed shell stages pass policy checks. Dangerous patterns need review. |
| `--approval-mode untrusted` | Shell stages without an allow rule need review. In-workspace file writes still pass. |
| `--approval-mode never` | No approval stops. Sandbox remains. |
| `--approval-judge off` | Human decisions replace automated review. |
| `--disable-approval` | Sandbox remains. |
| `--disable-sandbox` | Approval remains. File tools lose workspace confinement. Network becomes unrestricted. |
| `--yolo` | Disables approval and sandbox, and trusts the workspace for this run. |
| `--sandbox-network` | `proxy-only` by default, `restricted` blocks network. The configuration documentation calls full egress `unrestricted`; 1.4.9 help instead lists `enabled`. See the [capture](#149-capture). |

Shell confinement uses Seatbelt on macOS, bubblewrap on Linux, and a Windows sandbox. Muse refuses shell execution when it cannot verify confinement. Workspace trust enables project rules, skills, and hooks. Hook execution has a separate boundary, described below.

## Hook configuration and execution

The following contract comes from [SDK 1.4: Hooks](https://meta-models.github.io/muse-code-sdk/1.4/guides/extend/hooks/).

| Source, in application order | Location | Trust |
| --- | --- | --- |
| Managed | `managed_hooks_path` in settings. Relative paths use the settings directory. | No workspace-trust step. |
| User | Settings `hooks` object | No workspace-trust step. |
| Project | `<project-root>/.muse/hooks.json` | Requires workspace trust. |

Sources load at session start. Changes require a new session. A missing managed file contributes no hooks or warning. Malformed hook files contribute no handlers. Malformed user settings prevent startup.

Unsupported events, matchers, or handlers skip the affected entry with a warning. There is no active `muse hooks` command family.

All three sources use event keys, matcher groups, and command handlers. This POSIX example belongs in a trusted project's `.muse/hooks.json`. It uses the documented shape and has not been executed here.

```json
{
  "hooks": {
    "PermissionRequest": [
      {
        "matcher": "*",
        "hooks": [
          { "type": "command", "command": "cat >/dev/null", "timeout": 5 }
        ]
      }
    ]
  }
}
```

| Handler field | Contract |
| --- | --- |
| `type` | Required, `"command"`. |
| `command` | Nonblank shell string. |
| `timeout` | Integer seconds. See [limits](#hook-replies-neutral-behavior-and-limits). |
| `statusMessage` | TUI progress label. |
| `async` | Observation only. Output and exit status cannot change the turn. |
| `onFailure` | Fallback handler, at most three levels. Cannot set `async` or `if`. |
| `commandWindows`, `command_windows` | Windows command override. |
| `outputCapabilities` | Structured skill output, only `["skills.v1"]`, on foreground `UserPromptSubmit` or `PostToolUse`. |

The handler schema is closed, with explicit compatibility exceptions documented upstream. Unknown fields skip the handler. Wrong field types reject the source. Managed files also have a stricter alternative schema. Adding `name` or `timeout_ms` selects that schema for the whole file.

Each group can set `matcher`. An omitted, empty, or `*` value selects everything. ASCII alphanumeric, underscore, and pipe selectors match exact alternatives, such as `Bash|Edit|Write`. Other selectors are case-sensitive, unanchored regular expressions.

`UserPromptSubmit` and `Stop` accept only all-event matching. `PostToolBatch` ignores its matcher. Tool events match tool names. `SessionStart` matches `source`. Other event selectors are in the upstream guide.

Commands run through `$SHELL -c`, or `/bin/sh` when unset. Windows uses `%COMSPEC% /C`, defaulting to `cmd.exe`. This differs from the PowerShell surface for agent-run tools. The process cwd equals payload `cwd`. Matching handlers start concurrently, then answers apply in configured source and file order.

Hooks run outside tool approval and sandbox confinement, with the user's privileges. Their environment is cleared, then these variables pass through when set:

```text
HOME PATH USER LOGNAME TMPDIR TEMP TMP SHELL LANG LC_ALL TERM
COMSPEC PATHEXT SystemRoot WINDIR (Windows only)
```

Managed hooks additionally receive settings `managed_hooks_env_vars`. Provider credential names are prohibited there.

Every hook source receives `MUSE_SESSION_ID` for the top-level session and `MUSE_THREAD_ID` for the current session, including a child session. Arbitrary inherited variables, including integration identity variables, have no documented passthrough for user or project hooks. No emitter PID field is documented.

## Hook events and stdin

This table records the 1.3.0-checked subset in [SDK 1.4: Hook events and payloads](https://meta-models.github.io/muse-code-sdk/1.4/guides/plugins/reference/hook-events/). Tool fields below mean `tool_name`, `tool_input`, and `tool_use_id`. "Block" on post-tool events means model feedback. It cannot undo a completed tool call.

| Event | Native boundary and event-specific fields | Reply effects |
| --- | --- | --- |
| `SessionStart` | Start, resume, clear, fork, or post-compaction. `source` is `startup`, `resume`, `clear`, `compact`, or `fork`. | Context, stop. |
| `UserPromptSubmit` | Before the model receives `prompt`. | Reject prompt, context. |
| `PreToolUse` | Validated call before execution. `tool_name`, `tool_input`, `tool_use_id`. | Deny, force approval, rewrite input, context. |
| `PermissionRequest` | Pending approval. `tool_name`, `tool_input`. No `tool_use_id`. | Approve or deny. |
| `PostToolUse` | Successful tool result. Tool fields plus `tool_response`. | Feedback, context, stop. |
| `PostToolUseFailure` | Failed tool result. Tool fields plus `error`, `is_interrupt`, `duration_ms`. | Feedback, context, stop. |
| `PostToolBatch` | Committed batch. Ordered `tool_calls` entries contain tool fields and responses. | Feedback, context, stop. |
| `PreLLMCall` | Before provider request. `provider`, `request_id`, `attempt`, `step`, message/tool summaries, counts, `options`. | Block, context. |
| `PostLLMCall` | Provider response. Request fields plus `status`, `response_id`, `usage`, `finish_reason`, `error`, `output_text_preview`, `tool_call_count`. | Block, context. |
| `PreCompact` | Before compaction. `trigger`. | Veto compaction with `continue: false`. |
| `PostCompact` | After compaction. `trigger`. | Observation. |
| `SubagentStart` | Child starts within a turn. `subagent_id`, `child_session_id`. | Context. |
| `SubagentStop` | Child wants to finish. `subagent_id`, `child_session_id`, `stop_hook_active`, `last_assistant_message`. | Block keeps child working. |
| `Stop` | Model wants to finish. `stop_hook_active`, `last_assistant_message`. | Block keeps model working. Can accept stop. |
| `StopFailure` | Final model failure. `error` category, optional `error_details`, optional `last_assistant_message`. | Observation. |
| `Notification` | Approval waits six seconds. `notification_type: "permission_prompt"`, `title`, `message`. | Observation. |
| `SessionEnd` | Orderly shutdown. `reason`. | Observation. |

Common stdin fields are `hook_event_name`, `session_id`, `cwd`, `transcript_path` (string or null), `model`, and `permission_mode`. `turn_id` is omitted on `SessionStart` and `SessionEnd`. `model_provider` appears when known. Only child invocations of `PostToolBatch` and `StopFailure` receive `agent_id` and `agent_type`, together. The `permission_mode` value set and its mapping to CLI modes are not established here.

The SDK reference places hooks in the shared session runtime for TUI, `muse exec`, and `muse serve`. This is a documented cross-mode contract, not a capture of each mode.

`tool_input` can be parsed JSON, a raw string, or `{}` for empty arguments. `tool_response` can be a string or structured object. No complete tool-input, response, or `usage` schema is established here.

The SDK table lists accepted source values, not proof of their runtime assignment on every boundary. The [1.4.8 changelog](https://dev.meta.ai/docs/muse-code/changelog#v1-4-8) says `/clear` now reports source `clear`, and compaction reports `compact`. A `startup` matcher therefore excludes those boundaries. It also stops runtime-owned child runs from repeating root `SessionStart` hooks. The [1.4.9 capture](#149-capture) confirms `clear`, observes `startup` after `/new`, and observes no `SessionStart` on CLI resume or `/fork`. Earlier runtime source assignments remain unverified.

The [1.4.0 changelog](https://dev.meta.ai/docs/muse-code/changelog#v1-4-0) adds observation-only `Interrupt` when Escape cancels a turn. Both the manual's event list and the SDK table omit it. The [1.4.9 capture](#149-capture) records its payload and async-only admission in the TUI; other source and selector combinations remain unverified. Treat the table above as the SDK's documented subset.

## Hook replies, neutral behavior, and limits

The [hook wire reference](https://meta-models.github.io/muse-code-sdk/1.4/guides/plugins/reference/hook-events/) defines these replies.

| Reply | Meaning |
| --- | --- |
| Exit 0, empty stdout | No requested effect. Native approval remains undecided. The documented reply rules imply neutrality. Native UI preservation is untested. |
| Exit 0, plain stdout | Context on `SessionStart`, `SubagentStart`, `UserPromptSubmit`. Ignored elsewhere. |
| Exit 2, nonempty stderr | Blocks where the event permits it. Stderr supplies the reason. Post-failure feedback leaves the failed result unchanged. |
| Other nonzero exit, or exit 2 without a valid block | Hook failure. Turn continues without its output. |
| Timeout | Process tree killed. Hook becomes `timed_out`, and its answer is discarded. |

Leading `{` or `[` selects JSON parsing. The result must be an object. Unknown keys, wrong types, invalid event fields, or a mismatched `hookEventName` reject the entire answer.

| JSON output | Accepted effect |
| --- | --- |
| `{"decision":"block","reason":"Run the tests first"}` | Block on eligible events except `PermissionRequest`, which uses the decision object below. Reason must be nonempty. |
| `{"continue":false,"stopReason":"Check failed"}` | Stop where supported. Rejected on `PreToolUse`, `PermissionRequest`, and `Notification`. |
| `hookSpecificOutput.hookEventName` | Required inside `hookSpecificOutput`, equal to the current event. |
| `hookSpecificOutput.additionalContext` | Context on events listed above. |
| `hookSpecificOutput.permissionDecision` | `PreToolUse`: `deny` needs `permissionDecisionReason`. `ask` forces review. `allow` requires `updatedInput` and still runs approval. |
| `hookSpecificOutput.updatedInput` | `PreToolUse`: replacement object, still subject to approval. |
| `hookSpecificOutput.decision` | `PermissionRequest`: `{"behavior":"allow"}` or `{"behavior":"deny","message":"Check failed"}`. Other keys are rejected. |
| Top-level `decision` object | Older spelling of the same `PermissionRequest` decision. |
| `systemMessage` | User-visible text, capped at 1000 characters. |
| `suppressOutput` | Accepted without effect. |

`continue: false` has no effect on `UserPromptSubmit`, `PreLLMCall`, `PostLLMCall`, `PostCompact`, `SubagentStart`, `SubagentStop`, `StopFailure`, or `SessionEnd`. `Stop` and `SubagentStop` blocks continue work, subject to `max_consecutive_stop_hook_continuations` (default 8). These hooks do not establish task correctness.

The [1.4.0 changelog](https://dev.meta.ai/docs/muse-code/changelog#v1-4-0) extends the older reply table. `PostToolUseFailure` and `onFailure` successors can propose `updatedInput` corrections. Each correction becomes a new call through hooks, policy, and approval. Fallbacks cannot widen the original call or grant approval. The exact newer output nesting is not established by that release note.

| Resource | Limit and failure behavior |
| --- | --- |
| Serialized stdin | 256 KiB. Oversize payload skips the hook invocation. |
| Stdout and stderr | 16 KiB each. Either overflow kills the process tree and discards both streams. |
| Settings timeout | 600 seconds by default. Configured zero becomes one second. |
| Plugin timeout | `timeoutMs`, default 600000 ms, minimum 1000 ms. |

`PermissionRequest` identifies pending approval. Its ordering relative to the approval judge and native prompt rendering is not established here. `PreToolUse` alone does not identify an approval wait. The delayed `Notification` is unsuitable as evidence of immediate ask timing.

A synchronous empty reply requests no decision. An `async` handler cannot apply a decision at all. Neither documented rule proves when the native UI draws. A live test must check prompt visibility and latency with an observing hook.

Muse also has a native clarification-question UI that pauses a turn. Answers provide conversational input without granting tool authority. [Working with the agent](https://dev.meta.ai/docs/muse-code/interactive#clarify) documents that distinction. This baseline establishes no hook event for those questions.

## Session identity, resume, and exports

The [SDK session contract](https://meta-models.github.io/muse-code-sdk/1.4/guides/msp-concepts/sessions-and-turns/) defines `sessionId` as UUIDv7. The runtime stream and session directory use that same ID. This determines the directory name, not its parent path.

`muse serve` hosts programmatic sessions. Its returned Session has an absolute durable-log `path`, or an empty string under the ephemeral (no durable log) profile. Never treat that empty value as a filesystem path. This baseline does not verify the profile's equivalence to CLI `--no-session-log`. One host owns each loaded session through an OS-released writer lease. Competing hosts receive `sessionInUse`.

The interactive commands follow [Working with the agent](https://dev.meta.ai/docs/muse-code/interactive).

| Command | Session behavior |
| --- | --- |
| `/new` | Fresh session, retains terminal scrollback. Resets conversation, context, counters, goal, and tasks. |
| `/clear` | Same reset as `/new`, also clears terminal scrollback. |
| `/resume`, `/resume --last` | Retained session in the current workspace. Relaunches the session. |
| `/fork` | New session from the latest stable point. Source remains active. |
| `muse resume <id>` | Opens a retained session, including a fork, interactively. |
| `/compact` | Replaces model-visible history with a summary. Automatic compaction also exists. |
| `/export transcript` | On-screen conversation as text. |
| `/export trajectory` | Session log as JSON. Supports `--out <path>` and `--redacted`. |

`session_retention_days` selects cleanup after that many unopened days, currently on macOS and Linux. Unset means indefinite retention ([configuration](https://dev.meta.ai/docs/muse-code/configuration)).

Retained runs use append-only event logs containing model calls, tool results, and approval decisions. `muse trace inspect` provides post-hoc inspection. The [1.4.9 capture](#149-capture) establishes the XDG data-home store path and transaction-framed records, not a complete published local record schema. Hook `transcript_path` was always null in those runs, so it cannot be the only session identity source.

## Authentication, usage, and context

Authentication supports browser sign-in and API keys. `/login` reopens the choice. Credential precedence is `META_API_KEY`, stored API key, then stored browser session. `muse auth set` stores a key. `muse logout` removes stored credentials but leaves exported environment keys intact. [Authentication and billing](https://dev.meta.ai/docs/muse-code/auth) describes token-based API billing.

That page does not specify the credential path, keyring backend, or a local balance endpoint.

`PostLLMCall.usage` is a documented hook field, but its provider-specific contents are not established here. No verified per-turn dollar field, cache-token accounting, or context-window denominator is recorded in this baseline. Token counter resets do not specify a usage record format. Pricing requires a separate current source and cannot be inferred from a successful turn.

The [1.4.0 changelog](https://dev.meta.ai/docs/muse-code/changelog#v1-4-0) names the `/usage` UI and served subscription-usage methods. `usage/read` returns last-seen five-hour and weekly windows without a prompt. `usage/changed` pushes updates. These windows describe subscription allowance, not per-turn dollar cost.

The SDK marks [account/read](https://meta-models.github.io/muse-code-sdk/1.4/generated/msp/methods/account-read/) as experimental. It reports the current authentication source, an optional label, and whether credentials are required. The sources reviewed do not map each authentication method to every subscription tier.

[UsageReadResult](https://meta-models.github.io/muse-code-sdk/1.4/generated/msp/types/usagereadresult/) omits `usage` when nothing has been observed. Otherwise [SubscriptionUsage](https://meta-models.github.io/muse-code-sdk/1.4/generated/msp/types/subscriptionusage/) contains `observedAtMs`, `tier`, `window`, and `weekly`. The observation timestamp records host receipt. These are last-observed values, not guaranteed live measurements.

[Configuration and context](https://dev.meta.ai/docs/muse-code/configuration) lists telemetry options in settings without defining their schema. Exact telemetry controls remain unverified here.

## Headless and served sessions

The [headless contract](https://dev.meta.ai/docs/muse-code/extending#headless) provides these surfaces.

| Command or exit | Meaning |
| --- | --- |
| `muse exec --json <prompt>` | JSONL on stdout. |
| `muse exec --session-id <uuid> <prompt>` | Continue non-interactively. Workspace mismatch requires `--allow-workspace-switch`. |
| `muse export --session <uuid> --out run.json` | Export includes the producing CLI version. |
| Exit 0 | Turn completed. Tests or task results can still be wrong. |
| Exit 1 | Failure or cancellation, including model-step limit. |
| Exit 2 | Usage error. |
| Exit 130, 143 | SIGINT, SIGTERM. |

Headless runs have no approval UI. `--disable-approval` removes approval prompts while retaining confinement. The documentation also describes `--approval-mode never` as sandbox-only execution. No further difference is established here. A held action can remain unresolved. Its timeout and terminal exit behavior need a capture.

`muse resume` opens the TUI. Full JSONL event schemas and ordering remain outside this baseline.

The [1.4.9 capture](#149-capture) establishes the root resume ID as `stream.id` where `stream.kind == "session"` in `muse exec --json`, equal to root hook `session_id` and `MUSE_SESSION_ID`. Both headless continuation and interactive exact-ID resume succeeded. Served `Session.sessionId` equivalence remains a documented contract rather than a served-session capture.

`muse serve` runs the session protocol. `muse schema` prints its JSON schema ([overview](https://dev.meta.ai/docs/muse-code#sdk)). The [SDK session contract](https://meta-models.github.io/muse-code-sdk/1.4/guides/msp-concepts/sessions-and-turns/) describes a v1 stdio host with one client connection and an `initialize` handshake. `session/start`, `session/resume`, and `session/fork` subscribe that connection.

`turn/completed` distinguishes `completed`, `failed`, and `cancelled`. Queued turns can terminate with `turn/unqueued`, without `turn/completed`. A command acknowledgement proves admission, not completion. Closing stdin cancels active work and drains durable session state. After host death, a later durable-session load synthesizes the missing turn terminal. Ephemeral sessions have no retained log for that repair.

## Plugins, skills, and child agents

Native plugin packages use `.muse-plugin/plugin.json`, with `schemaVersion: 1`. The [manifest reference](https://meta-models.github.io/muse-code-sdk/1.4/guides/plugins/reference/manifest/) defines skills, commands, hooks, MCP servers, and an advanced reminder capability. The reminder decision and context schemas are not documented by that page.

Hooks require plugin approval through `muse plugins approve`. A native hook `command` is an argv array, executed directly, unlike settings shell strings. Native hooks share the event and reply wire above. `muse plugins hook test` delivers fixture stdin verbatim, so it cannot prove runtime identity stamping or native prompt timing.

[Extending and automating](https://dev.meta.ai/docs/muse-code/extending) lists built-in, user, project, and plugin skills. User discovery includes `$XDG_CONFIG_HOME/muse/skills` and `~/.agents/skills`, plus foreign-agent roots subject to settings and rollout gates. Project discovery includes `.agents/skills`, `.codex/skills`, and `.claude/skills`. `muse skills list`, `inspect`, and `validate` expose discovery and validation.

Children share the checkout unless the lead requests worktree isolation per child through its native subagent tools. The request schema is outside this baseline. `--subagent-worktree-isolation` is a compatibility flag and does not isolate every child.

MCP supports stdio and streamable HTTP. Required MCP startup failures abort the run.

## 1.4.9 capture

Captured on 2026-10-15 with installed `Muse Code 1.4.9 (1.4.9-R6297.1)` on Linux, using tmux 3.7c for the TUI. These are runtime observations, distinct from the upstream documentation above. Evidence is retained outside the checkout at `/home/marvin/.rimz/ws/rimz-f89e/shared/muse-adapter/`: `live-capture.md` and `static-capture.md`, verbatim help under `help/`, first-occurrence stdin under `payloads/`, and raw artifacts under `evidence/` and `static/`.

All session runs selected `--provider echo` with disposable HOME and XDG directories and an isolated RIMZ_HOME. The TUI harness constructed its environment with `env -i` and disabled the approval judge. No credentials or network models were used. Commands below reproduce the recorded argv or name the recorded harness action; `<ROOT>` denotes the disposable capture root, not a literal runnable path. The recording hook saved stdin, environment, and ancestry, then exited 0 with empty stdout. Its PermissionRequest branch would sleep three seconds, but was never entered.

### Settings, environment, and process identity

The live harness command `node <ROOT>/probe.mjs launch <case> <args>` ran `/usr/local/bin/muse --provider echo --trust-workspace --approval-judge off --workspace <ROOT>/project <args>` in a private tmux pane. Fixture changes named below were made before each launch.

| Probe command or fixture | 1.4.9 observation |
| --- | --- |
| `/usr/local/bin/muse --provider echo --version` | Reports `Muse Code 1.4.9 (1.4.9-R6297.1)`. The launcher execs `/usr/local/lib/muse-code/muse-bin-1.4.9-R6297.1`. |
| `node <ROOT>/probe.mjs launch missing-schema hello`; `launch schema2 hello` | Settings containing hooks but no `schema_version`, or version 2, refuse startup with exit 1. Version 1 runs. |
| `node <ROOT>/probe.mjs launch unset-xdg --probe-unset-xdg hello` | With XDG_CONFIG_HOME unset, reads `$HOME/.config/muse/settings.json`. A version-2 fixture there refuses despite valid settings in the unused XDG location. |
| `node <ROOT>/probe.mjs launch managed-key hello`; `launch sync-key hello`; `launch handler-key hello`; `launch handler-sync-key hello` | Adding `_rimz_managed: true` or `_rimz_sync: true` to a matcher group or command handler skips that group or handler with an `UnsupportedHandler: D66` warning. The session still echoes the prompt; the skipped hook does not run. These two keys were tested, not every possible extension key. |
| `node <ROOT>/probe.mjs launch lifecycle --echo-delay-ms 2500 hello` | The user hook shell received only `HOME LANG LOGNAME MUSE_SESSION_ID MUSE_THREAD_ID PATH PWD SHELL TERM TMPDIR USER`. PWD may be shell-supplied. Explicitly inherited `RIMZ_*`, XDG roots, `ZELLIJ_PANE_ID`, and `TMUX_PANE` markers were absent. |
| Same lifecycle launch, handler `exec <ROOT>/record.sh SessionStart` | The hook's direct parent was the real binary, with kernel comm `muse-bin-1.4.9-` (15-byte truncation). Reading its `/proc/<pid>/environ` recovered all requested room, run, pane, home, and runtime markers. There was no intervening hook-runner child. This is a Linux ancestry observation, not a cross-platform PID contract. |
| `RIMZ_WORKSPACE_ID=static-workspace muse exec --provider echo --json -c 'managed_hooks_path="/tmp/muse-static/static.XLhDI6rz/managed-hooks.json"' -c 'managed_hooks_env_vars=["RIMZ_WORKSPACE_ID"]' hi` | The managed recorder received `RIMZ_WORKSPACE_ID=static-workspace`. The absolute managed path worked. The list passes inherited values; it does not assign them. |
| `muse exec --provider echo -c 'managed_hooks_env_vars={"RIMZ_WORKSPACE_ID":"static-workspace"}' hi` | A name/value map refuses with exit 1 because the JSON type does not fit. The accepted setting is an array of names. |
| `muse exec --provider echo -c 'hooks={}' hi`; `-c 'managed_hooks_path="/tmp/muse-static/static.XLhDI6rz/missing-hooks.json"'`; `-c 'managed_hooks_env_vars=["RIMZ_WORKSPACE_ID"]'` | Each override was accepted with exit 0. A populated `-c hooks=...` recorder also ran but did not inherit RIMZ_WORKSPACE_ID. Overrides did not create a settings file. |
| `muse config --help`; `muse config validate --plane defaults --file <flat-settings.json>`; `muse exec --provider echo hi` against the same fixture | `config validate` checks enterprise plane documents, not flat user settings. Even a valid flat hook file fails that validator; actual startup accepts it. `muse config status` reporting absent enterprise planes does not validate user hooks. |

### Lifecycle, children, and native input

`node <ROOT>/probe.mjs launch events --echo-delay-ms 1500 hello`, followed by native `/clear`, `after-clear`, `/compact`, `/fork`, `after-fork`, `/new`, `after-new`, `cancel-me`, Escape, and `/quit`, supplied the lifecycle observations. The composer was cleared before the final `/quit` because Escape restored the canceled prompt. Additional `lifecycle` and explicit-shell probes supplied the first-occurrence payloads.

| Boundary or command | 1.4.9 observation |
| --- | --- |
| Completed echo model turns | Root recorder-entry order was `SessionStart` (when a boundary fires), `UserPromptSubmit`, `PreLLMCall`, `PostLLMCall`, `Stop`. All 12 completed root turns had a Stop sharing the submitted turn_id. Internal child hooks interleaved. Recorder-entry timestamps do not prove exact emission order for concurrent handlers. |
| `/clear`, then next prompt | Ends the old session with `reason: "other"`; the next prompt emits `SessionStart(source=clear)` with a new ID, not at the immediate reset display. |
| `/new`, then next prompt | Ends the old session; the next prompt emits `SessionStart(source=startup)` with a new ID. |
| `/compact` | Prints `manual context compaction is not available` under echo. No compact boundary, PreCompact/PostCompact, or compact-induced ID change was captured. This does not establish behavior with another provider. |
| `/fork`, then another prompt; `muse ... resume <fork-id>` | Prints a new fork ID and keeps the source active. The following source prompt retains its ID. Resuming the fork uses the new ID. No SessionStart(fork) was captured at creation or continuation. |
| `muse ... resume <root-id>`; `muse ... resume --last` | Reopens retained sessions; a new explicit-ID resumed turn completes in the same session. No SessionStart(resume) was captured on CLI resume, including fork continuation, although other turn hooks ran. |
| Escape during Thinking | Synchronous Interrupt registration was skipped with `Interrupt hooks must declare async: true`. After adding `"async": true` and relaunching, Escape emitted Interrupt with session_id and turn_id, restored the prompt to the composer, and emitted no Stop for that turn. |
| `/quit` | Exits 0 and emits SessionEnd. All 13 SessionEnd captures, including resets, had `reason: "other"`; it did not distinguish quit from reset. |
| All captured stdin | `transcript_path: null`, `permission_mode: "default"`, and `model: "unknown"`; no model_provider. SessionStart and SessionEnd omitted turn_id; other captured events included it. These values do not establish other providers' models or a permission-profile mapping. |
| `node <ROOT>/probe.mjs launch lifecycle --echo-delay-ms 2500 hello` | Internal `skill-reminder`, `goal-reminder`, and `verify-reminder` children emitted SubagentStart/SubagentStop and PreLLMCall/PostLLMCall. Payload session_id and MUSE_THREAD_ID identified the child; MUSE_SESSION_ID remained the root. No captured payload carried agent_id or agent_type. This does not test model-spawned children or the uncaptured child PostToolBatch/StopFailure fields. |
| `node <ROOT>/probe.mjs launch bang --approval-mode untrusted '!touch <ROOT>/project/probe-approval'` | Explicit shell input emitted PreToolUse then PostToolUseFailure: bubblewrap could not mount proc on this host. It did not emit PermissionRequest. |
| `node <ROOT>/probe.mjs launch bang-review --approval-mode untrusted --disable-sandbox '!touch <ROOT>/project/probe-approval'` | The same explicit shell action ran without an approval prompt and emitted PreToolUse then PostToolUse. This is not a model-issued tool call or proof of neutral-hook approval preservation. |

Twelve event types were captured: SessionStart, UserPromptSubmit, PreLLMCall, PostLLMCall, SubagentStart, SubagentStop, Stop, SessionEnd, Interrupt, PreToolUse, PostToolUseFailure, and PostToolUse. PermissionRequest, Notification, StopFailure, PreCompact, PostCompact, and PostToolBatch had zero invocations. Natural-language and tool-call-shaped prompts were echoed, not executed. `node <ROOT>/probe.mjs launch clarification 'Call request_user_input now to ask me which option I want.'` likewise produced only echoed text. No native approval or clarification UI, judge ordering, or hook-return versus prompt-render timing was measured.

### Launch, profiles, and durable sessions

| Probe command | 1.4.9 observation |
| --- | --- |
| `muse --help`; `muse resume --help`; `muse exec --help` | Root accepts `[PROMPT]`. Resume supports exact UUID, Session Name, `--last`, or a picker with no argument. No CLI fork flag appears in these help outputs. Root network help lists `restricted\|enabled\|proxy-only`, contradicting the documentation's `unrestricted` spelling. |
| `muse ... hello`; `muse ... -- '-x starts with hyphen'`; `muse ... '-x hyphen'`; `muse ... -- resume` | Ordinary positional prompts submit automatically. Double dash permits a leading hyphen; the bare hyphen form exits 2 as an option error. However, `-- resume` still opens the resume picker, just like `muse ... resume`, rather than submitting that word. Here `...` is the isolated TUI prefix above. |
| `muse exec --provider echo --permission-profile <id> hi` | Each of `:auto-review`, `:ask-me`, `:read-only`, and `:unrestricted` exited 0. `read-only` without the colon exited 1. The experimental schema's SessionSetPermissionProfileParams profileId description also lists these four native IDs. |
| `muse exec --provider echo --permission-profile :read-only --approval-mode never hi`; same profile with `--yolo` | Each exits 2: profile selection cannot combine with approval-mode or yolo, rather than one overriding the other. `:read-only --disable-sandbox` was accepted, but echo requested no tools, so read-only enforcement remains untested. |
| `muse exec --provider echo --json hi`, with managed recorders as above | Root JSONL `stream.id` where `stream.kind == "session"` equaled root hook session_id, MUSE_SESSION_ID, and MUSE_THREAD_ID. Top-level `id` was a record ID, not the session ID. Child hook session_id differed from the root. |
| `muse exec --provider echo --session-id <captured-root-id> --json ... again` with the same managed overrides | Exits 0 with `echo: again` under the same stream.id. The retained session_resumed record reports prior_turn_count 1. No SessionStart(resume) recorder appeared. |
| `fd -t f . <ROOT>/data/muse/sessions`; `muse trace inspect --session-log <session.jsonl> --render-mode compact --max-text-bytes 100`; `muse export --session <root-id> --out <export.json>` | Retained logs are at `$XDG_DATA_HOME/muse/sessions/YYYY/MM/DD/<session-id>/session.jsonl`, with some child logs under `subagent/<child-id>/session.jsonl`. Trace and export exit 0. The HOME data-path fallback was not exercised. |
| Inspection of that retained log after two echo turns | Physical lines include `retained_frame: "session_permission_transaction"` frames containing JSON-encoded `children[].record_json`, not only flat event envelopes and not the same format as headless stdout. Model-completion usage had cached_tokens, input_tokens, output_tokens, and reasoning_tokens, all zero. No cost or context-window field was found in this decoded echo log. The full local schema remains unpublished. |
| `muse schema generate-json-schema --out <dir>`; same with `--experimental` | Both exit 0 offline. Stable export includes usage/read and turn/completed; account/read is experimental-only. Schema fields for token usage, optional cumulative USD cost, and optional context window are contracts, not real-provider measurements. |

These captures narrow seven documentation gaps or claims: network mode spelling is `enabled` in help; documented resume/fork SessionStart sources did not fire; `/new` used `startup`; `/compact` was unavailable under echo; profile IDs require colons; transcript_path stayed null despite a known retained-store path; and Interrupt requires async registration. None establishes behavior on earlier versions, another platform, or a network provider.

## Validation boundary

Closed rows apply only to the 1.4.9 echo capture above. Missing evidence does not establish that a capability is absent.

| Evidence | Status and integration consequence |
| --- | --- |
| Installed version and exported schema | Closed for 1.4.9: version probe and stable/experimental schema generation succeeded. Served RPC conformance was not exercised. |
| Start, clear, new, resume, fork, cancellation, and orderly end | Closed for captured TUI echo paths: lifecycle commands above establish IDs, ordering, async Interrupt, and absent resume/fork SessionStart. |
| Tool failure and children | Partial: explicit `!` input captured tool success/failure; reminder children captured child identity. Final model StopFailure and model-spawned children remain open. |
| Permission wait with exit 0 and empty stdout | Open: no PermissionRequest fired, so native approval UI preservation is unverified. |
| Approval-judge ordering and sync/async prompt timing | Open: no pending approval or native prompt was obtained. The delayed Notification was also uncaptured. |
| Headless hook and JSONL identity | Closed for echo ID extraction and continuation: session stream.id matches root hook identity, and exec --session-id continues it. Headless pending-approval behavior remains open. |
| Clarification questions and corresponding events | Open: the request_user_input prompt was echoed without a question UI. |
| Parent process and sanitized environment | Closed on this Linux host: direct versioned-binary parent, cleared user-hook env, and list-based managed passthrough captured. Other platforms remain unverified. |
| Retained-log path and format | Closed for XDG_DATA_HOME relocation and observed framed records. Transcript parsing, author classification, and full local schema compatibility remain open. |
| Compaction, real-provider usage, context, and cost | Open: echo could not compact and yielded only zero counters and unknown model. Schema exports do not close these runtime gaps. |
| Credential and data-home overrides | Partial: XDG config/data relocation works; data-home fallback untested. Launcher source mentions MUSE_AUTH_PATH, but the CLI credential override and account isolation were not validated. No general Muse-specific home override was established. |
| Live prompt parsing and permission profiles | Closed for the tested argv forms, four colon IDs, and flag conflicts. A subcommand-named prompt remains ambiguous even after double dash. Read-only enforcement at a real tool call remains open. |

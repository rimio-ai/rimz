# Antigravity CLI protocol reference

> The landed adapter mapping is [antigravity.md](../../internals/agents/antigravity.md), the agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md), the adapter implementation playbook is [agent-adapters.md](../../contributing/agent-adapters.md), and account, balance, spend, and pricing semantics are in [providers.md](../../internals/agents/providers.md).

This is the single home for the official **Antigravity CLI upstream surface** a RimZ adapter can bind to: the `agy` process and launch flags, JSON command hooks, custom statusline state, conversations, local persistence, permission and artifact waits, subagents, headless runs, authentication, models, and quota presentation.

Coverage is **depth on viable RimZ inputs, breadth as an index**. Google publishes the hook and statusline payloads but does not publish the CLI implementation or a schema for its conversation databases and transcripts. This reference keeps documented facts, release-note evidence, implementation inferences, and live-verification requirements visibly separate.

## Refresh target and upstream sources

This reference was refreshed on 2026-07-13 against an installed Antigravity CLI `1.1.2`, its embedded hook/statusline documentation, and Google's living documentation. The [`1.1.1`](https://github.com/google-antigravity/antigravity-cli/releases/tag/1.1.1) tag at commit [`b5578c4bbeae95fd9be14d14ac61563bd9f20363`](https://github.com/google-antigravity/antigravity-cli/tree/b5578c4bbeae95fd9be14d14ac61563bd9f20363) remains the latest public source snapshot used for repository examples; the distributed CLI implementation is not published there.

RimZ's typed fixtures target Antigravity CLI 1.1.2. The adapter keeps tolerant readers for additive fields; refresh this reference and its fixtures when Google changes hook decisions, config locations, payload semantics, or the private local-service wire. The local-service account probe deliberately abstains from running `agy --version`, because a version command is not a safe idle-enrichment precondition for this TUI.

Google's website documentation is a living surface without versioned snapshots. The installed 1.1.2 embedded documentation wins for executable hook behavior, while the 1.1.1 tagged examples remain provenance for unchanged examples; every known conflict stays visible in [Documentation drift](#documentation-drift).

| Surface | Official source |
| --- | --- |
| CLI release and version evidence | installed `agy 1.1.2`, [`1.1.1` source release](https://github.com/google-antigravity/antigravity-cli/releases/tag/1.1.1), pinned [`CHANGELOG.md`](https://github.com/google-antigravity/antigravity-cli/blob/b5578c4bbeae95fd9be14d14ac61563bd9f20363/CHANGELOG.md) |
| Product boundary and installation | [CLI overview](https://antigravity.google/docs/cli-overview), [installation and auth](https://antigravity.google/docs/cli-install), pinned [README](https://github.com/google-antigravity/antigravity-cli/blob/b5578c4bbeae95fd9be14d14ac61563bd9f20363/README.md) |
| Launch flags and TUI commands | [CLI reference](https://antigravity.google/docs/cli-reference), the installed `agy 1.1.2 --help` output summarized below |
| Settings and keybindings | [settings](https://antigravity.google/docs/cli-settings) |
| Command hooks and tool vocabulary | [hooks](https://antigravity.google/docs/hooks) |
| Custom statusline payload | [statusline](https://antigravity.google/docs/cli-statusline), pinned [example script](https://github.com/google-antigravity/antigravity-cli/blob/b5578c4bbeae95fd9be14d14ac61563bd9f20363/examples/statusline/statusline.sh) |
| Custom terminal-title payload | [terminal title](https://antigravity.google/docs/cli-title), pinned [example script](https://github.com/google-antigravity/antigravity-cli/blob/b5578c4bbeae95fd9be14d14ac61563bd9f20363/examples/title/title.sh) |
| Conversations, resume, and fork | [managing conversations](https://antigravity.google/docs/cli-conversations), [`/resume` command](https://antigravity.google/docs/cli/commands/resume) |
| Permission engine and sandbox | [CLI permissions](https://antigravity.google/docs/cli-permissions), [settings](https://antigravity.google/docs/cli-settings) |
| Artifact review | [CLI artifacts](https://antigravity.google/docs/cli-artifacts), [artifact model](https://antigravity.google/docs/artifacts) |
| Subagents and background tasks | [subagents](https://antigravity.google/docs/cli-subagents) |
| Headless `-p` | [best practices](https://antigravity.google/docs/cli-best-practices), pinned [1.1.1 release notes](https://github.com/google-antigravity/antigravity-cli/releases/tag/1.1.1) |
| Models, plans, quota, and credits | [models](https://antigravity.google/docs/models), [plans](https://antigravity.google/docs/plans), [`/usage`](https://antigravity.google/docs/cli/commands/usage), [CLI credits](https://antigravity.google/docs/cli-credits) |
| Authentication | [installation and auth](https://antigravity.google/docs/cli-install) |
| Plugins and shared configuration | [CLI plugins and skills](https://antigravity.google/docs/cli-plugins), [MCP](https://antigravity.google/docs/mcp) |
| Gemini CLI transition | [migration guide](https://antigravity.google/docs/gcli-migration), [official transition announcement](https://github.com/google-gemini/gemini-cli/discussions/27274) |
| Programmatic sibling surface | [Antigravity SDK](https://antigravity.google/docs/sdk-overview), [official SDK repository](https://github.com/google-antigravity/antigravity-sdk-python) |

## Product boundary and migration

Antigravity CLI is the terminal TUI in the Antigravity product family. It shares an agent harness and settings with Antigravity 2.0 but has its own binary, CLI app-data directory, rendering, conversation cache, and terminal interaction surface.

Google explicitly transitions the consumer, free-tier, and Google AI Pro/Ultra terminal experience from Gemini CLI to Antigravity CLI. Antigravity offers one-time import of Gemini CLI settings and converts Gemini extensions into Antigravity plugins. Compatibility and migration do not make the two products wire-compatible: do not reuse Gemini hook names, payload structs, session discovery, auth probing, permission flags, model limits, or transcript parsing.

The initial RimZ integration target is **Antigravity CLI**, because it owns a stock terminal pane that `rimz pane capture`, `rimz pane send`, and `rimz message` can drive. Antigravity 2.0, the IDE, and the SDK are adjacent surfaces, not alternate observation channels for an `agy` pane.

## Adapter feasibility at a glance

Antigravity exposes enough official surface for a useful adapter, plus a version-sensitive private local service observed in a running CLI. The landed adapter owns process/launch/resume, validated local conversation discovery, transcript history and question waits, safe command hooks, custom-statusline context and live API-rate estimates, background parking, supervised completion, and read-only account/quota enrichment. Policy-changing pre-tool decisions, artifact waits, stable child identities, credits, provider-history/account spend, and remote control remain outside the verified boundary.

| RimZ need | Antigravity surface | Verdict |
| --- | --- | --- |
| Process discovery and launch | `agy`; stable interactive and prompt flags in 1.1.2 help | direct |
| Session identity | exact `--conversation`; workspace-latest cache; hook `conversationId`; statusline `conversation_id` | direct hook binding with validated local fallback |
| Registration before work | first `PreInvocation` identity plus local discovery | create-on-miss/derived; no session-only event |
| Turn start | first `PreInvocation` plus captured `USER_INPUT` fallback | native realtime edge |
| Turn completion and error | `Stop.terminationReason`, `Stop.error`, `Stop.fullyIdle` | native success/failure/background edge |
| Session end | no session-end hook | pane/process presence only |
| Tool activity and acting phase | disjoint `PostToolUse` matchers over the published tool vocabulary | native after execution, without changing permission policy |
| Native permission wait | statusline `tool_confirmation_pending` | read-only card attention; native pane owns detail and decision |
| Question wait | completed planner-response transcript record with typed `ask_question` questions | derived native-pane wait; no durable ask or out-of-band answer |
| Plan/artifact review wait | statusline `artifacts` and artifact-review UI | schema/status enum incomplete; derived after capture |
| Model and context | custom statusline `model` and `context_window` | direct live enrichment landed |
| Background tasks | `Stop.fullyIdle`; statusline arrays remain identity-poor | native foreground parking, no task rows |
| Subagents | statusline `subagents`; tool calls for define/invoke/manage | visible, but documented child entries omit conversation IDs |
| Transcript/history | official hook path plus captured 1.1.1 text records | basic root user/assistant history landed; all other records remain unknown |
| Durable conversation store | 1.1.1 changelog says SQLite is the CLI conversation format | format known, schema and authoritative path unpublished |
| Compaction | no documented hook, command, or marker | unsupported until verified |
| Account identity | statusline `email` and `plan_tier`; private local `GetUserStatus` | direct live plus version-sensitive idle enrichment; treat email as private |
| Quota and credits | private local `RetrieveUserQuotaSummary`; interactive `/usage`/`/quota` and `/credits` panels | conservative 5h/weekly quota windows landed; credits remain unsupported |
| Session spend | statusline current token split plus exact model ID; no documented cumulative billing record | partial live API-rate estimate only |
| Supervised `-p` runs | stock interactive prompt, `Stop`, and transcript final response | RimZ supervised hook transport landed; native headless mode remains separate |
| Native resume | `--conversation <UUID>`; `-c`/`--continue` for workspace latest | direct |
| Native fork in a new pane | `/fork` clones in the current TUI, but no launch flag forks a supplied source ID | unsupported for RimZ fork |
| Structured answer | native TUI keys and artifact/question panels | pane-send fallback; no out-of-band answer API |
| Remote control | no CLI remote-control host documented | unsupported |

The current adapter combines installed command hooks and a wrapped custom statusline with the workspace conversation cache and validated JSONL history. Spend, permission/question/artifact waits, subagent rows, compaction, fork, and structured answers stay disabled until their verification items pass.

## Launch and process surface

The official installer places the executable at `~/.local/bin/agy` on macOS and Linux and under the per-user local `agy\bin` directory on Windows. RimZ should detect and launch `agy`, not `antigravity`, `gemini`, or a desktop application process.

The pinned README publishes these installers:

```text
# macOS and Linux
curl -fsSL https://antigravity.google/cli/install.sh | bash

# Windows PowerShell
irm https://antigravity.google/cli/install.ps1 | iex

# Windows CMD
curl -fsSL https://antigravity.google/cli/install.cmd -o install.cmd && install.cmd && del install.cmd
```

Use `agy update` as the stale-version fix. `agy install` configures shell paths and aliases for an existing installation; its shipped flags are `--dir`, `--skip-path`, and `--skip-aliases`, so it is not the binary downloader RimZ should prescribe for a missing install.

The installed binary reports `1.1.2` from `agy --version` and this top-level surface from `agy --help`:

| Flag/subcommand | Shipped meaning | RimZ use |
| --- | --- | --- |
| `--add-dir <path>` | add a workspace directory; repeatable | render an explicit multi-root preset when supported |
| `--agent <name>` | choose a custom agent for this session | provider-native agent profile, not a RimZ kind |
| `-c`, `--continue` | continue the most recent conversation | convenience only; prefer an exact ID for restart |
| `--conversation <id>` | resume a conversation by ID | native resume |
| `--dangerously-skip-permissions` | auto-approve tool permission requests | RimZ `yolo` |
| `-i`, `--prompt-interactive <prompt>` | send an initial prompt, then stay interactive | fresh pane with startup prompt |
| `--log-file <path>` | override the CLI log path | diagnostics only |
| `--mode <accept-edits\|plan>` | select execution mode | RimZ `auto`/`plan`; see below |
| `--model <name>` | choose the session model | launch preset model |
| `--new-project` | create a project for the session | leave user-controlled by default |
| `-p`, `--print`, `--prompt <prompt>` | run one prompt non-interactively and print the response | provider-native alternative; RimZ keeps the pane/UI hook transport |
| `--print-timeout <duration>` | bound print-mode wait; default `5m0s` | unused by the interactive hook transport |
| `--project <id>` | select a project | raw profile arg until project semantics are implemented |
| `--sandbox` | enable terminal restrictions | optional launch hardening; orthogonal to approval mode |
| `agent`, `agents` | list available custom agents | optional discovery, not lifecycle |
| `models` | list available models | optional model discovery |
| `plugin`, `plugins` | manage plugins | unused by direct named-hook installation |
| `changelog`, `update`, `install` | release notes and client maintenance | outside ordinary adapter launches |

A bare interactive launch is:

```text
agy
```

An interactive launch with an initial task is:

```text
agy --prompt-interactive "task"
```

The provider owns the current working directory, so RimZ launches `agy` with the pane/worktree directory as the child cwd. The living best-practices page shows `--cwd`, but 1.1.2 help exposes no such flag; do not emit it.

### Permission-mode mapping

The shipped native modes support this closest mapping:

| RimZ mode | Provider argv | Boundary |
| --- | --- | --- |
| `ask` | no flag | keeps native default review and permission policy |
| `auto` | `--mode accept-edits` | accepts edits; it does not promise approval of every command, URL, or MCP action |
| `plan` | `--mode plan` | starts plan mode |
| `yolo` | `--dangerously-skip-permissions` | bypasses tool permission requests; preserve the dangerous naming in help and docs |

`--sandbox` changes execution containment rather than permission posture. The persistent `proceed-in-sandbox` policy auto-runs sandboxed terminal commands and asks for unsandboxed ones, but the shipped help exposes no one-shot flag that selects that policy. Do not silently add `--sandbox` to `accept-edits` or claim the pair is a provider-defined permission mode without a launch test.

### Presets and prompts

`--model` is the direct model preset. The official launch surface has no reasoning-effort flag and no flag that replaces or appends the system prompt. `--agent` chooses an Antigravity custom agent, which can carry its own instructions, and rules/skills/plugins provide other instruction surfaces; the adapter rejects unsupported preset fields rather than translating system prompts into unrelated flags.

The CLI supports multiple workspaces with repeated `--add-dir`. Ordinary RimZ worktree isolation remains a process-cwd concern; Antigravity's separate project and desktop worktree concepts do not replace RimZ worktrees.

## Conversation identity, resume, clear, rewind, and fork

Antigravity calls its durable session a **conversation**. Hook payloads name it `conversationId`; the statusline names the same concept `conversation_id`. Official examples use UUIDs. Parse a non-empty opaque string and do not enforce UUID syntax unless upstream publishes that invariant.

Conversation pickers are scoped to the current working directory. `/resume` opens the picker, and CLI 1.1.1 can also import an Antigravity 2.0 conversation by cloning it into the CLI. The shell launch forms are:

```text
agy --continue
agy -c
agy --conversation <conversation-id>
agy --conversation=<conversation-id>
```

`-c` reads `~/.gemini/antigravity-cli/cache/last_conversations.json`, a documented map from absolute workspace path to latest conversation ID, and verifies the selected conversation with the backend. Live 1.1.2 can leave this cache pointing at an older conversation while a newer bare `agy` process is active. RimZ restart retains the exact hook/statusline conversation ID and uses `--conversation`; a matching hook-bound row authorizes local transcript enrichment before this latest-workspace fallback.

`/fork` (alias `/branch`) clones conversation history up to the current turn, allocates a new conversation ID, and switches the current TUI to that clone. It does not clone the Git checkout. Because no `agy --fork <source-id>` launch surface exists, it cannot implement `rimz agents fork`, whose contract opens a provider-native copy beside an untouched source.

`/rewind` (alias `/undo`) rewinds conversation history. `/clear` resets the terminal and active conversation context; the official reference does not say whether it allocates a new `conversationId`, rewrites the transcript, or changes the SQLite row. Capture all three before mapping either command to identity or compaction behavior.

The CLI prints an exact resume command on exit. That text is useful to humans but the hook/statusline ID remains the durable machine identity.

## JSON command hooks

Hooks execute custom commands at five agent-loop points. They receive one camelCase JSON object on stdin and return one JSON object on stdout. Hook stdout is the decision channel, so RimZ logs go to stderr and any helper process starts with fresh stdio.

### Configuration and discovery

The global shared hook file is:

```text
~/.gemini/config/hooks.json
```

The workspace hook file is:

```text
<workspace>/.agents/hooks.json
```

The official 1.1.2 hook documentation describes `hooks.json` as a map from stable hook names to definitions and runs multiple named hooks sequentially. CLI 1.1.1 fixed workspace hook loading after a folder becomes trusted. RimZ installs one global named hook, `rimz`, preserves every other name, and refuses to replace a user-owned `rimz` definition.

A shortened RimZ-owned shape is:

```jsonc
{
  "rimz": {
    "PreInvocation": [
      {
        "type": "command",
        "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event PreInvocation",
        "timeout": 5
      }
    ],
    "Stop": [
      {
        "type": "command",
        "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source antigravity --event Stop",
        "timeout": 5
      }
    ]
  }
}
```

The complete installer also adds three disjoint `PostToolUse` matcher entries and one `PostInvocation` entry. Synthetic `--event` labels preserve the matcher class because `PostToolUse` input omits the tool name. The published handler object has `type` (only `command`, optional), `command` (required), and `timeout` in seconds (optional, default 30); RimZ uses five seconds.

Plugins may also carry `hooks.json`. CLI 1.1.1's changelog establishes `~/.gemini/config/` as the active shared global customization directory even though older/living plugin prose still shows some assets under `~/.gemini/antigravity-cli/`. RimZ uses the direct named global entry because a plugin adds packaging without improving the wire.

Workspace trust is a launch precondition for workspace-local hooks. RimZ's project trust preview must show the exact command and every config file it will edit; the trust hash includes the hook command, the custom statusline command, and any wrapped prior command.

### Hook schema

Every hook input carries:

```jsonc
{
  "conversationId": "conversation UUID/opaque id",
  "workspacePaths": ["/absolute/workspace"],
  "transcriptPath": "/absolute/app-data/brain/<id>/.system_generated/logs/transcript.jsonl",
  "artifactDirectoryPath": "/absolute/app-data/brain/<id>"
}
```

The documented app-data roots are `~/.gemini/antigravity-cli` for CLI and `~/.gemini/antigravity` for Antigravity 2.0. Use the absolute hook path rather than reconstructing either root.

The CLI 1.1.2 hook capture instead names `transcript_full.jsonl`. RimZ accepts that observed basename and the documented `transcript.jsonl`, validates either against the same conversation-root boundary, and prefers the observed full transcript when it must reconstruct a path.

| Event | Fires | Event fields | Output |
| --- | --- | --- | --- |
| `PreToolUse` | before a tool executes | `toolCall.name`, `toolCall.args`, `stepIdx` | required `decision`; optional `reason`, `permissionOverrides[]` |
| `PostToolUse` | after a tool completes | `stepIdx`, optional/empty `error` | `{}` |
| `PreInvocation` | before each model call | `invocationNum` (0-indexed), `initialNumSteps` | optional `injectSteps[]` |
| `PostInvocation` | after tool calls finish | same documented input as `PreInvocation` | optional `injectSteps[]`, `terminationBehavior` |
| `Stop` | when the execution loop terminates | `executionNum`, `terminationReason`, optional `error`, required `fullyIdle` | required `decision`; optional `reason` |

An injected step has exactly one of `toolCall`, `userMessage`, or `ephemeralMessage`. `PostInvocation.terminationBehavior` is `force_continue`, `terminate`, or empty/omitted.

`Stop.terminationReason` examples are `model_stop`, `max_steps_exceeded`, and `error`. `fullyIdle = false` means background commands or asynchronous tasks remain active even though the foreground loop is stopping.

### Pre-tool decision channel

`PreToolUse.decision` accepts:

| Value | Meaning |
| --- | --- |
| `allow` | auto-allow execution |
| `deny` | hard-block execution |
| `ask` | prompt the user while respecting an Always Allow grant |
| `force_ask` | prompt regardless of cached permissions |

None of those four values is documented as a behavior-preserving observer result. `allow` can bypass the provider's native policy, while `ask` can introduce a prompt that policy would have skipped. The 1.0.16 release notes say the permission manager now handles an empty decision string safely, but they do not define whether `{}`, `{"decision":""}`, and absent stdout are equivalent, nor how exit codes and malformed JSON behave.

This remains the permission-integration gate. RimZ leaves `PreToolUse` uninstalled and returns no output if it is fed manually; native policy and the provider UI retain the decision.

`Stop` documents `decision = "continue"` as the only value that prevents stopping and injects `reason`; any other value allows the stop. RimZ returns the golden `{"decision":""}` shape.

### Canonical tool vocabulary

The hook reference publishes these names and argument keys. Parse the tool name strictly and the args tolerantly so additions do not break lifecycle ingestion.

| Category | Tool | Documented arguments |
| --- | --- | --- |
| file | `view_file` | `AbsolutePath`, optional `StartLine`, `EndLine`, `IsSkillFile` |
| file | `write_to_file` | `TargetFile`, `Overwrite`, `CodeContent`, `Description`, optional `IsArtifact`, `ArtifactMetadata` |
| file | `replace_file_content` | `TargetFile`, `Instruction`, `Description`, `AllowMultiple`, `TargetContent`, `ReplacementContent`, `StartLine`, `EndLine`, optional `TargetLintErrorIds` |
| file | `multi_replace_file_content` | `TargetFile`, `Instruction`, `Description`, `ReplacementChunks[]`, optional `TargetLintErrorIds`, `ArtifactMetadata` |
| file | `list_dir` | `DirectoryPath` |
| file | `find_by_name` | `SearchDirectory`, `Pattern`, optional `Type`, `Excludes`, `Extensions`, `FullPath`, `MaxDepth` |
| search | `grep_search` | `SearchPath`, `Query`, optional `IsRegex`, `CaseInsensitive`, `Includes`, `MatchPerLine` |
| search | `search_web` | `query`, optional `domain` |
| search | `read_url_content` | `Url` |
| execution | `run_command` | `CommandLine`, `Cwd`, `WaitMsBeforeAsync`, optional `RunPersistent`, `RequestedTerminalID` |
| execution | `manage_task` | `Action` (`list`, `kill`, `status`, `send_input`), optional `TaskId`, `Input` |
| execution | `schedule` | optional `DurationSeconds`, `CronExpression`, `MaxIterations`; `Prompt` |
| permission | `list_permissions` | none |
| permission | `ask_permission` | `Action`, `Target`, `Reason` |
| agent | `invoke_subagent` | `Subagents[]`, each with `Prompt`, `Role`, `TypeName`, optional `Workspace` |
| agent | `define_subagent` | `name`, `description`, `system_prompt`, optional `enable_mcp_tools`, `enable_write_tools`, `enable_subagent_tools` |
| agent | `send_message` | `Recipient`, `Message` |
| agent | `manage_subagents` | `Action` (`list`, `kill`, `kill_all`), optional `ConversationIds[]` |
| interaction | `ask_question` | `questions[]`, each with `question`, `options[]`, `is_multi_select` |
| media | `generate_image` | `Prompt`, `ImageName`, optional `ImagePaths[]` |

The matcher is a regular expression over `toolCall.name`; empty and `*` match all tools. The reference shows `browser_.*` as a matcher example without publishing the complete browser-tool catalog, so unknown browser names remain ordinary non-edit tools.

For RimZ phase semantics, only `write_to_file`, `replace_file_content`, and `multi_replace_file_content` are structured native file-edit proof. `run_command` may mutate the repository but remains generic work, matching the cross-provider rule.

## Custom statusline state channel

Antigravity CLI can execute one custom statusline command whenever agent state changes. It sends detailed snake_case JSON on stdin, reads the command's stdout, and renders that stdout with ANSI support.

Configuration lives in `~/.gemini/antigravity-cli/settings.json`:

```jsonc
{
  "statusLine": {
    "type": "command",
    "command": "rimz statusline feed --source antigravity"
  }
}
```

CLI 1.0.6 added `stack_with_default` to render the built-in and custom lines together. That option does not preserve a pre-existing user custom command. A RimZ installer must wrap and forward an existing command's output, exactly as other statusline adapters do, and uninstall must restore the previous object byte-for-byte where possible.

The terminal-title command receives the same JSON, but it runs only when title customization is active and strips ANSI/non-printable output. Use the statusline as the primary feed and leave the title untouched.

### Published payload

| Field | Documented shape and meaning |
| --- | --- |
| `cwd` | current working directory |
| `conversation_id` | current conversation identity |
| `model` | `{id, display_name}`; live 1.1.2 can put the selected human label, such as `Gemini 3.5 Flash (Medium)`, in `id` |
| `product` | product name, for example `antigravity-cli` |
| `workspace` | `{current_dir, project_dir}`; the example uses a `file://` URI for `project_dir` |
| `version` | CLI version string |
| `plan_tier` | authenticated subscription tier |
| `email` | authenticated account email/LDAP identity |
| `agent` | active custom-agent profile object/name; nested schema is not published |
| `context_window` | totals, limit, percentages, and current usage; detailed below |
| `agent_state` | `idle`, `thinking`, `working`, `tool_use`, or `initializing` |
| `vcs` | `{type, branch, client, dirty}`; documented types include `git`, `jj`, and `fig` |
| `sandbox` | `{enabled, allow_network}` with optional fields tolerated |
| `subagents` | array of active entries with `name`, `role`, `status` |
| `artifacts` | array with `uri`, `status`, `type` |
| `pending_input_count` | queued user-message count |
| `background_tasks` | array with `name`, `status`, `index` |
| `tool_confirmation_pending` | whether a tool-confirmation dialog is visible |
| `terminal_width` | live terminal width |

The context object is:

```jsonc
{
  "total_input_tokens": 88244,
  "total_output_tokens": 61074,
  "context_window_size": 1048576,
  "used_percentage": 8.415603637695312,
  "remaining_percentage": 91.58439636230469,
  "current_usage": {
    "input_tokens": 63382,
    "output_tokens": 346,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 20857
  }
}
```

Treat every field as optional, validate finite nonnegative numbers, and prefer upstream `used_percentage`/`context_window_size` rather than a hard-coded model limit. The example does not define whether the total token fields are current-window or cumulative session totals; `current_usage` clearly names one current usage object, but its refresh cadence is unpublished.

### Release-example drift

The pinned official 1.1.1 statusline example reads `artifact_count` and `task_count`, while the living schema documents `artifacts[]` and `background_tasks[]`. The script reads `subagents[]` as documented. RimZ's landed context parser ignores those drifting identity-poor fields and tolerates additive keys; task, artifact, and child-row claims wait for stable identities and enums.

### Lifecycle projection

The statusline is a sidecar state feed, not a durable event log. RimZ persists model, version, plan/account identity, and context usage without converting refreshes into lifecycle churn:

| Statusline observation | RimZ projection | Constraint |
| --- | --- | --- |
| `model`, `version` | model and CLI identity | sidecar only |
| `plan_tier`, `email` | provider account identity | private sidecar; no diagnostic logging |
| `context_window` | live context gauge and token composition | sidecar only |
| `agent_state` | ignored for lifecycle | command hooks own durable edges |
| `tool_confirmation_pending` | timestamped display-only permission wait | raises the card while newer than hook activity; no durable ask or detail |
| `subagents`, `artifacts`, `background_tasks` | ignored for row identity | published entries lack the stable IDs RimZ requires |

`PreInvocation` fires for every model call inside an execution. RimZ emits `registered` and `turn_started` only when `invocationNum = 0`; later model calls do not reset acting to reasoning or create false prompt boundaries.

## Turn completion, errors, and background work

`Stop` is the authoritative documented foreground-loop terminal signal:

```jsonc
{
  "executionNum": 1,
  "terminationReason": "model_stop",
  "error": "",
  "fullyIdle": true,
  "conversationId": "...",
  "workspacePaths": ["/workspace/project"],
  "transcriptPath": ".../transcript_full.jsonl",
  "artifactDirectoryPath": ".../brain/<id>"
}
```

The initial mapping is:

| Stop payload | RimZ signal |
| --- | --- |
| `terminationReason = model_stop`, empty error, `fullyIdle = true` | `turn_ended { errored: false }` |
| `terminationReason = error` or non-empty error, `fullyIdle = true` | `turn_ended { errored: true }`; no current error shape supplies a recovery certificate |
| clean stop with `fullyIdle = false` | clean `turn_ended` with background work in flight, leaving the row running/parked |
| error with `fullyIdle = false` | foreground failure wins; do not paint a success-shaped park |
| `max_steps_exceeded` | failed |

The stop hook can itself force another loop. RimZ returns the documented non-`continue` empty decision, so observation does not extend the execution.

Antigravity documents no process/session-end hook. Pane process presence, shell reversion, and ordinary RimZ reaping remove the row. `Stop` ends an execution loop, not the conversation.

### Recovery evidence gate

The supported installed version reports `1.1.2`. A recoverable classifier requires two independent captures of the same provider-limit Stop with a stable provider-owned typed discriminator after removing conversation IDs, paths, account/model identity, user text, request metadata, and dynamic values. The available evidence does not satisfy that gate: no rate-limit, spend-limit, overload, or transient class has a repeated typed discriminator, and no sanitized positive recovery payload exists. Raw Stop bodies stay out of the repository and logs.

The classification table is therefore closed:

| 1.1.2 Stop shape | Classification |
| --- | --- |
| exact `model_stop`, empty error, required `fullyIdle` | terminal clean/background mapping |
| `max_steps_exceeded`, required `fullyIdle` | terminal error |
| `error` with a string, message-only object, code-like object, empty object, or another unverified value | terminal error according to the existing failure mapping; no turn-error marker |
| missing `fullyIdle` or malformed payload | no lifecycle observation |
| future typed rate-limit, spend-limit, overload, or transient discriminator | unclassified until the same supported-version shape is captured twice and sanitized |

Account quota is timing evidence only. A `100%` window, a stalled pane, or a keyword-similar error cannot establish why the current turn stopped, so Antigravity supervised runs and loop attempts remain terminal on every current error Stop.

## Human waits and native answer surfaces

Antigravity has three visible human-in-the-loop families:

- Tool permissions open a TUI card; the statusline exposes `tool_confirmation_pending`, while the preceding `PreToolUse` carries the proposed action.
- `ask_question` carries one or more questions with options and multi-select state. The exact result wire is not published because the answer stays inside Antigravity's UI.
- Artifacts include implementation plans, code diffs, and media. Depending on `artifactReviewPolicy`, the agent pauses at milestones for approval, rejection, or inline comments before changes reach disk.

Root tool confirmations accept `y` and `n`. `Ctrl+K` fast-approves the pending subagent action surfaced by the status alert, and `Alt+J` jumps to the next subagent that needs approval. The Artifact Review panel opens with `Ctrl+R` and owns its own approve/reject/comment flow.

Ordinary `rimz message` text continues through pane send. A future `rimz answer` planner may drive stable native keys after its dialog-state preconditions are captured, but there is no official out-of-band answer API. Never answer by returning `allow` from the observation hook: that changes provider behavior before the user acts and bypasses the invariant that the provider UI is the answer surface.

RimZ's current `AskKind` has permission, plan approval, and question. Map a pending implementation-plan artifact to plan approval only after the payload's artifact `type` and pending `status` values are captured. Treat pending code diffs as permission until the shared model gains an artifact-review kind; do not hide them as generic idle.

## Subagents and background tasks

Antigravity supports nested asynchronous subagents and non-agent background tasks.

The parent can call `define_subagent`, `invoke_subagent`, `send_message`, and `manage_subagents`. The `/agents` panel shows identifier, role, status (`running`, `done`, `killed`, or `error` in the published prose), and current step. Nested descendants and their tool confirmations relay to the root conversation in CLI 1.1.1.

The statusline schema exposes active subagents with `name`, `role`, and `status`, but it does not document a child `conversationId`, parent ID, start time, task text, token usage, or terminal result identity. The tagged example only counts the array. `invoke_subagent` inputs describe requested children but do not carry the provider-assigned IDs returned after spawn.

Keep RimZ `subagents` capability off until one of these is proven:

- child hook callbacks carry their own `conversationId` and a recoverable parent identity;
- the real statusline entry contains an undocumented stable child ID and parent relation;
- a documented transcript/store relation supplies stable IDs without polling private implementation state.

Background shell work appears through `run_command` with `RunPersistent`, `manage_task`, the `/tasks` panel, and the statusline task surface. `Stop.fullyIdle` is enough to keep a clean foreground completion parked while work remains. Rich per-task rows wait for the array/count drift and stable task IDs to be captured.

## Transcript and durable local state

Every hook carries an absolute `transcriptPath`. The official hook page says it points to:

```text
<app_data_dir>/brain/<conversationId>/.system_generated/logs/transcript.jsonl
```

For CLI, `<app_data_dir>` is `~/.gemini/antigravity-cli`; for Antigravity 2.0 it is `~/.gemini/antigravity`.

Google publishes no JSONL record schema, append/replace guarantee, retention rule, file-locking contract, rewind semantics, or relationship between a root transcript and child transcripts. The path is safe to retain as provider identity evidence. Keep context and spend disabled, and derive visible history only from record shapes captured against the one supported release.

### Live 1.1.2 transcript probe

A stock root conversation confirms `transcript.jsonl` and `transcript_full.jsonl` as newline-delimited JSON with these top-level fields; the 1.1.2 hook payload points at `transcript_full.jsonl` even though the hook documentation still names `transcript.jsonl`:

| Field | Captured shape |
| --- | --- |
| `step_index` | integer physical step index |
| `source` | string source enum |
| `type` | string record-type enum |
| `status` | string status enum |
| `created_at` | RFC 3339 timestamp |
| `content` | optional string |

The captured simple text turn contains `USER_EXPLICIT` / `USER_INPUT` / `DONE` with visible user content, `MODEL` / `PLANNER_RESPONSE` / `DONE` with visible assistant content, and `SYSTEM` `CONVERSATION_HISTORY`/`CHECKPOINT` records that stay internal. Provider-authored user content may wrap the request in exact `<USER_REQUEST>...</USER_REQUEST>` tags followed by `<ADDITIONAL_METADATA>` and settings blocks; only the request body is user-visible.

The captured native question turn adds `tool_calls` to a completed `MODEL` / `PLANNER_RESPONSE` record. `transcript_full.jsonl` carries `ask_question.args.questions` as a JSON array of typed question objects; `transcript.jsonl` carries the same array as a JSON-encoded string. The first nonblank `question` is sufficient to project a native waiting card at the record timestamp; answer state remains inside the TUI.

The landed parser accepts only those two visible source/type pairs, ignores system and unknown records, tolerates malformed complete lines, and retains a torn final line for the next incremental read. Ordinary completed planner responses supply partial pulled turn completion; the validated `ask_question` shape supplies a read-only question wait. These records do not prove failure, cancel, compaction, subagent, or historical spend semantics. Re-capture those before broadening the parser.

The official CLI changelog adds a second persistence fact:

- 1.0.4 adds SQLite `.db` conversations and says SQLite will be the CLI conversation format.
- 1.0.5 makes `/resume` scan `.db` and `.db-wal` files.
- 1.0.16 uses a shared SQLite summary store for background synchronization.

The database path, table schema, transaction mode, row identity, and relationship to `transcript.jsonl` are not published. Treat the hook transcript as an agent-loop log and SQLite as conversation persistence until a live trace proves a stronger relationship. A future row-store parser follows RimZ's durability rules: open read-only, tolerate WAL, select by typed conversation ID, and never mutate or checkpoint the provider database.

### Live 1.1.1 persistence probe

A read-only probe of the locally installed latest CLI confirms one conversation database at `~/.gemini/antigravity-cli/conversations/<conversation-id>.db` and a shared `~/.gemini/antigravity-cli/conversation_summaries.db`. These are implementation observations, not published compatibility promises, so fixtures must be regenerated for every latest-version advance.

The per-conversation database reports SQLite `user_version = 1`. Its visible schema is:

| Table | Columns visible in SQLite schema | Implementation value |
| --- | --- | --- |
| `trajectory_meta` | `trajectory_id`, `cascade_id`, `trajectory_type`, `source` | possible root identity; enum meanings unpublished |
| `steps` | `idx`, `step_type`, `status`, `has_subtrajectory`, `metadata`, `error_details`, `permissions`, `task_details`, `render_info`, `step_payload`, `step_format` | ordered activity shell; most payloads are opaque blobs |
| `gen_metadata` | `idx`, `data`, `size` | opaque generation metadata |
| `executor_metadata` | `idx`, `data` | opaque executor metadata |
| `parent_references` | `idx`, `data` | possible fork/subagent relation; blob wire unpublished |
| `trajectory_metadata_blob` | `id`, `data` | opaque trajectory metadata |
| `battle_mode_infos` | `idx`, `data` | opaque battle-mode state |

The shared summary database also reports `user_version = 1`. Its `conversation_summaries` row exposes `conversation_id`, title/preview, step count, modification and last-user-input times, workspace URIs, status/source/project/agent fields, parent conversation ID, nesting depth, battle/winner IDs, `not_fully_idle`, `killed`, last-user-input step index, and `app_data_dir`. Indexes cover the two time fields.

This schema makes exact resume discovery and candidate parent/nesting recovery plausible, but it does not make the blob payloads a supported transcript wire. Validate enum values, concurrent WAL behavior, parent semantics, and schema drift before parsing; keep titles, previews, workspace paths, and account-bearing app-data paths out of diagnostics.

Documented cache files relevant to identity are:

| Path | Role | Adapter use |
| --- | --- | --- |
| `~/.gemini/antigravity-cli/cache/last_conversations.json` | absolute workspace → latest conversation ID | optional resume fallback only |
| `~/.gemini/antigravity-cli/cache/projects.json` | centralized workspace → project mapping | do not use for session identity |
| `~/.gemini/antigravity-cli/updater/` | updater lock/timestamp state | ignore |

## Model, context, account, quota, and spend

### Model and context

The statusline is the authoritative live model/context surface. Preserve `model.id` byte-for-byte as provider identity even when 1.1.2 supplies the human selector label rather than the canonical-shaped hook hint. A terminal case-insensitive `(Low)`, `(Medium)`, or `(High)` display qualifier supplies lowercase effort; `(Thinking)` supplies the thinking flag; unknown parenthetical suffixes remain presentation. Model choice is sticky for the current turn: changing the selector while a turn runs applies after that turn finishes or is canceled.

Antigravity is multi-model. The captured 1.1.2 selector lists `Gemini 3.5 Flash (Medium)`, `Gemini 3.5 Flash (High)`, `Gemini 3.5 Flash (Low)`, `Gemini 3.1 Pro (Low)`, `Gemini 3.1 Pro (High)`, `Claude Sonnet 4.6 (Thinking)`, `Claude Opus 4.6 (Thinking)`, and `GPT-OSS 120B (Medium)`. Current and selected markers are selector UI state rather than part of these labels. Availability changes by plan. Do not infer provider, context window, or pricing from the `antigravity` kind; use the exact live model and upstream-reported context limit.

### Account and authentication

The CLI authenticates through the OS secure keyring (Apple Keychain, Linux Secret Service/D-Bus, or Windows Credential Manager), silently reusing a session and falling back to browser Google Sign-In. SSH launches use a URL-and-code OAuth flow. `/logout` purges the saved authentication profile.

No credential file or stable machine-readable auth command is documented. Do not scrape or export keyring tokens. While a pane is live, statusline `email` and `plan_tier` populate best-effort account identity; while the same user's `agy` process is already running, its private local service can return email and the native user tier or plan label through `GetUserStatus`. Discard or redact email outside the account cache and diagnostics according to RimZ privacy policy. `AGY_CLI_HIDE_ACCOUNT_INFO` hides header presentation but the official docs do not say it removes those statusline or local-service fields, so verify rather than assuming.

### Quota and credits

`/usage` (alias `/quota`) refreshes model configuration and backend quota state, then opens an interactive panel. `/credits` opens credit details and purchase/upgrade links. The built-in statusline displays quota and remaining AI credits in current releases, but the documented custom-statusline JSON does not publish quota-window or credit fields.

Plans provide baseline quota with plan-dependent five-hour and/or weekly refresh behavior, and optional AI-credit overages for eligible paid plans. Google explicitly says quota is capacity-dependent and measured by work rather than a stable prompt or token count. Do not synthesize RimZ `RateLimitWindow`s from plan prose.

The distributed CLI exposes a private Connect-over-HTTPS service on process-owned loopback sockets. This surface is undocumented by Google and therefore version-sensitive; its wire and discovery were cross-checked against CodexBar commit [`b41715f`](https://github.com/steipete/CodexBar/tree/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8), specifically the pinned [`AntigravityStatusProbe`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityStatusProbe.swift), [`AntigravityStatusProbe+PortDetection`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityStatusProbe%2BPortDetection.swift), and [`AntigravityQuotaSummaryParser`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityQuotaSummaryParser.swift).

RimZ POSTs `{}` to `/exa.language_server_pb.LanguageServerService/GetUserStatus` and `{"forceRefresh":true}` to `/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary`, with `Content-Type: application/json` and `Connect-Protocol-Version: 1`. It accepts only an exact current-uid `agy` executable and `argv[0]`, intersects that process's owned sockets with loopback listeners, discovers candidates once newest-first, and revalidates the process start identity before each RPC. One direct usage attempt pairs status and quota on the same candidate endpoint; once status identifies an owner, quota failure returns that owner's failed result instead of falling back to another process or endpoint. The client starts no process, reads no credential, follows no redirect, applies bounded deadlines and body size, and accepts the service's self-signed certificate only after those process/socket checks.

The direct usage owner key is SHA-256 over the trimmed ASCII-lowercased email under the versioned `rimz:antigravity-account:v1` domain, rendered with an `antigravity:v1:` prefix. Only that digest reaches the account-usage cache. A known owner switch invalidates the prior windows even when the paired quota call fails; an ownerless early failure retains prior truth. The separate display account probe may retain plan-only status and stays independently cached.

The quota response can wrap its summary at the root, under `response`, or under `summary`, and can encode a remaining fraction directly, nested, or through an observed oneof shape. RimZ recognizes only explicit five-hour and weekly period labels/IDs; every enabled native model bucket in a period folds to the smallest remaining fraction, with later reset and stable model identity breaking ties. A missing or disabled recognized period stays an unknown window, unknown periods are ignored, and any malformed recognized fraction or nonfuture reset rejects the reading. The normalized `5h` and `7d` windows are authoritative account quota; AI credits and dollars remain unknown.

### Spend

The statusline exposes current input, output, cache-creation, and cache-read tokens plus a model value, but no dollars. Live 1.1.2 may express that value as the selected human label. Baseline plan quota and AI-credit overages are not equivalent to API-token billing, and Antigravity can route multiple model providers. No official per-session cost field or cumulative usage ledger is published.

RimZ may price those four disjoint current-usage classes through its local public API price book. A canonical ID uses the shared resolver; a captured selector label must carry a recognized reasoning qualifier, and its qualifier-free normalized candidate must resolve by exact table key. Render the result as an ordinary dollar value with current-usage coverage: room/provider aggregates, budgets, full-history spend, provider/account totals, and `rimz stats` exclude it because the replace-style value is non-additive. Never present the price as subscription billing or synthesize it from the plan or agent kind.

## Headless and supervised runs

The stock one-shot form is:

```text
agy --print "prompt"
agy -p "prompt"
```

`--prompt` is an alias for `--print`. `--print-timeout` defaults to five minutes. Resume composes with print mode:

```text
agy --conversation <conversation-id> -p "next prompt"
agy --conversation=<conversation-id> -p "next prompt"
agy -c -p "next prompt"
```

CLI 1.1.1 fixes two contract-critical behaviors: a server-side request failure writes its error to stderr and exits nonzero instead of returning empty success, and a flagged prompt no longer causes the process to read stdin and hang inside scripts/subprocesses. The response is plain stdout text.

No JSON result mode, streaming JSON mode, event envelope, or documented token/cost footer exists in the shipped help or official headless prose. A first supervised adapter can support plain text and process exit status; its normalized JSON mode must be RimZ's wrapper around that text rather than a claimed provider-native format.

Before enabling supervised runs, verify hook/statusline behavior in `-p`, timeout exit codes, signal exits, empty final responses, permission-required failures, and whether `--print-timeout` accepts Go duration syntax beyond the shown default.

## Settings, permissions, trust, and privacy

CLI preferences live in sparse JSON at:

```text
~/.gemini/antigravity-cli/settings.json
```

The parser preserves unknown fields in current releases. RimZ still edits it with typed JSON, temp-file plus rename, conflict detection, and a preview; it never regenerates the whole file from a partial schema.

Implementation-relevant documented keys are:

| Key | Values/default | Relevance |
| --- | --- | --- |
| `toolPermission` | `request-review` default; `proceed-in-sandbox`, `always-proceed`, `strict` | native permission posture |
| `artifactReviewPolicy` | `asks-for-review` default; `agent-decides`, `always-proceed` | plan/code review waits |
| `permissions.allow/deny/ask` | resource strings | exact tool policy; living docs use plural `permissions` |
| `allowNonWorkspaceAccess` | `false` | workspace boundary |
| `enableTerminalSandbox` | `false` | persistent sandbox |
| `enableTelemetry` | `true` | privacy-visible data collection setting |
| `altScreenMode` | `default`, `always`, `never` | multiplexer rendering; inline `never` is designed for tmux/SSH |
| `statusLine` | command object | RimZ live state wrapper |
| `notifications` | `false` | native desktop/bell notifications |

Permission resources use `action(target)` with actions `read_file`, `write_file`, `read_url`, `execute_url`, `command`, `unsandboxed`, and `mcp`. Conflict precedence is Deny > Ask > Allow. Workspace reads/writes are auto-allowed by default; web, commands, MCP, browser actuation, and non-workspace access default to Ask.

Project-specific configuration under `~/.gemini/config/projects/` takes precedence over global CLI settings in the 1.1.1 changelog. The broader Antigravity product also merges shared user and project permissions. A launch flag overrides persistent settings for that process. RimZ should read enough effective config to describe a mode mismatch, but leave provider policy evaluation to Antigravity.

The executable trust surface includes at least:

- every command inserted into `~/.gemini/config/hooks.json`;
- the custom `statusLine.command` and any pre-existing command RimZ wraps;
- any plugin path or command if installation moves to a plugin;
- raw profile arguments that select `--dangerously-skip-permissions`, hooks, MCP servers, or project behavior.

The official CLI README warns about autonomous execution, data exfiltration, prompt injection, and supply-chain risk and says interaction-data collection can be disabled in settings. Hook payloads contain workspace paths, transcript locations, tool arguments, account identity through the separate statusline, and potentially source code inside edit arguments. RimZ stores only the normalized fields its product surfaces require and keeps raw payloads out of ordinary logs.

## Documentation drift

These official sources disagree as of the refresh. Treat the pinned release as the shipped 1.1.1 contract and retain tolerant parsers where the real payload may carry both shapes.

| Surface | Living documentation | Pinned 1.1.1 evidence | Implementation rule |
| --- | --- | --- | --- |
| Plan commands | CLI reference still lists `/planning` and `/fast` | 1.1.0 removes both and adds `/plan`; `--mode plan` ships | use `--mode plan`; do not emit removed slash commands |
| Working directory | best-practices example passes `--cwd` | 1.1.1 `--help` has no `--cwd` | set child cwd at spawn |
| Statusline artifacts/tasks | `artifacts[]`, `background_tasks[]` | tagged example reads `artifact_count`, `task_count` | tolerate both; capture before capability claims |
| Hook global path | general prose mentions customization directories and older CLI plugin paths | 1.0.8 fixes `/hooks` to shared `~/.gemini/config/hooks.json` | install into shared config after a clean-room probe |
| Plugin location | CLI plugin page shows `~/.gemini/antigravity-cli/plugins/` in places | 1.0.2 moves installed plugins to `~/.gemini/config/`; 1.1.0 fixes global agents to shared config | avoid plugin installation initially |
| Pre-tool neutral result | hook page requires `decision` and lists only behavioral decisions | 1.0.16 accepts an empty decision string without the former error | live-verify exact neutral bytes |
| Permission key spelling | permission page shows `permissions.{allow,deny,ask}` | 1.1.1 notes say `permission.allow` in one line | preserve unknown fields and capture the actual settings file |
| Conversation record | hook page promises per-conversation `transcript.jsonl` | 1.1.2 hooks point at `transcript_full.jsonl`; changelog says SQLite is the CLI conversation format; live capture confirms both visible JSONL text records | retain and validate either verified hook basename, prefer `transcript_full.jsonl` for reconstruction, parse only captured visible shapes, and keep SQLite blobs opaque |

## Adjacent surfaces kept out of the initial adapter

The Antigravity SDK exposes a richer programmatic lifecycle with session start/end, pre/post turn, tool calls, user interaction, compaction, streaming, token usage, and structured output. It starts and owns its own agent runtime; it does not observe a stock `agy` TUI session. Replacing the CLI with an SDK-hosted agent would change the product boundary and the provider UI, so it is not an implementation shortcut for RimZ's pane adapter.

Antigravity 2.0 shares the harness and can import/export conversations, but its desktop process is not the pane child and its app-data root differs. Do not join its background conversations to a local `agy` pane solely because both use a conversation ID namespace.

MCP, skills, rules, custom agents, and plugins affect tool vocabulary, prompts, and executable trust. The current pulled adapter ignores unknown transcript records and does not manage or report those customizations.

## Native-event mapping for the live-channel promotion

The landed adapter uses this conservative mapping:

| Antigravity observation | RimZ signal/enrichment | Notes |
| --- | --- | --- |
| first `PreInvocation`, `invocationNum = 0` | `turn_started` | create-on-miss establishes identity and carries transcript/workspace/model enrichment plus the latest completed visible user prompt from the bounded validated transcript tail |
| later `PreInvocation` | activity only | do not reopen the turn after tool use |
| successful edit-matcher `PostToolUse` | `tool_used { mutates: true, edits: true }` | acting begins only after execution succeeds |
| successful `run_command` matcher `PostToolUse` | `tool_used { mutates: true, edits: false }` | durable proof of generic mutation |
| remaining documented-tool matcher `PostToolUse` | `tool_used { mutates: false, edits: false }` | progress without durable churn unless it changes state |
| failed `PostToolUse` | `tool_used { mutates: false, edits: false }` | failure does not claim a completed edit |
| statusline `tool_confirmation_pending = true` | display card as waiting | read-only marker; native pane remains the answer surface |
| `Stop`, clean and fully idle | `turn_ended { errored: false }` | terminal success |
| `Stop`, error | `turn_ended { errored: true }` | no 1.1.2 error class passes the repeated typed-discriminator recovery gate |
| `Stop`, clean and not fully idle | `turn_ended { errored: false, background work }` | shared fold leaves running/parked |
| pane process exits/reverts | `ended` through presence reconciliation | no native session-end event |
| statusline model/context/account | `AgentContext` sidecar | no event-log churn |

An edit moves to acting only after successful execution. The installer selects the tool class through disjoint `PostToolUse` matchers, so no pre-tool cache or policy callback is required. Newly added upstream tool names remain unclassified until the vocabulary fixture advances.

Compaction stays unsupported. `/clear`, `/rewind`, and implicit context management are not substitutes for an opener/closer signal.

## Implementation verification checklist

Run the remaining probes with a temporary HOME and throwaway Git workspace against the current latest `agy` release. Record sanitized payloads as typed test fixtures before expanding the adapter; older-release fixtures and speculative compatibility fallbacks stay out of the implementation.

### Process and launch

- Capture `agy --version`, `agy --help`, `agy agents`, and `agy models`; pin the one supported latest version and tolerate model-list failure while logged out.
- Verify process name, parent/child tree, cwd, and whether hooks inherit RimZ's mux-stamped environment on macOS and Linux.
- Verify `--prompt-interactive`, `--conversation`, `--mode accept-edits`, `--mode plan`, `--sandbox`, and `--dangerously-skip-permissions` independently.
- Prove `--conversation <id>` and `--conversation=<id>` resume the same hook/statusline ID and `-c` remains workspace-scoped.
- Prove `/clear`, `/rewind`, and `/fork` identity and persistence behavior; keep unsupported claims until then.

### Hook installation and decisions

- Create global-only, workspace-only, same-name global/workspace, different-name global/workspace, and plugin hook configurations; capture merge and order behavior after workspace trust.
- Re-check command cwd, environment, timeout, signal, and malformed-output behavior when a release changes the hook executor.
- For `PreToolUse`, compare no hook, absent/empty decision, `allow`, `ask`, `force_ask`, and `deny` under each native policy before ever expanding into permission observation; defer the event unless one result is behavior-preserving.
- Re-probe the non-`continue` `Stop` decision whenever the documented decision contract changes.
- Verify hooks in root agents, nested subagents, resumed sessions, forked sessions, print mode, and after `/clear`.
- Install/uninstall/preview must preserve unrelated named hooks and unknown fields and write with temp-file plus rename.

### Statusline

- Capture the first payload before any prompt and every transition through initializing, idle, thinking, working, tool use, root permission, root question, artifact review, background task, subagent wait, stop, error, cancel, resume, rewind, clear, and fork.
- Record absent vs null fields and exact nested schemas/enums for `agent`, `subagents`, `artifacts`, `background_tasks`, `sandbox`, `vcs`, and `context_window`.
- Check whether 1.1.2 emits array fields, count fields, or both; verify whether `tool_confirmation_pending` covers subagent, question, and artifact waits.
- Verify callback coalescing, maximum payload size, concurrent invocation, timeout, stdout forwarding, and whether a custom command runs in print mode.
- Re-test a real prior statusline command's ANSI output across CLI releases; fixture tests prove structural preservation and uninstall restoration.
- Confirm whether `email` disappears when account-info hiding is enabled and keep raw identity out of logs.

### Lifecycle and transcripts

- Expand the post-tool matcher fixture when Google publishes new tool names; retain non-overlap among edit, generic-mutation, and observed-only sets.
- Capture every `Stop.terminationReason`, provider-limit/network error text, `fullyIdle` combination, and statusline state before/after Stop.
- Inspect the hook-provided transcript with append, tool use, question, artifact, error, cancel, resume, rewind, clear, fork, and subagent activity; define a parser only from stable fixtures.
- Re-capture the observed SQLite `user_version`, tables, columns, indexes, `.db-wal` behavior, and blob encodings; map conversation IDs to rows and test concurrent writes, rewind, fork/import, and retention before claiming history, parentage, or spend.
- Search for a real compaction command/event/record in the supported release; otherwise leave smart compaction unavailable.

### Headless, account, and quota

- Verify `-p` stdout/stderr and exit codes for success, provider error, permission required, timeout, SIGINT, empty answer, resume, and sandbox.
- Verify whether a prompt supplied by stdin has any supported form; RimZ should pass the prompt flag until a contract exists.
- Confirm the final answer contains only the new response on resumed print runs.
- Re-capture `GetUserStatus` and `RetrieveUserQuotaSummary` envelopes, success codes, period labels, fractions, and reset fields against each supported CLI release; keep `/credits` TUI-only and do not scrape pane text into account truth.
- Verify statusline context token semantics across turns, model changes, cache reads, and any implicit compaction before using totals for anything beyond live context.

### Conformance target

The landed descriptor claims only what the current fixtures prove: lazy pulled registration; interactive launch; exact resume; model preset; ask/auto/plan/yolo launch mappings; basic text transcript history and streaming; partial pulled text-turn state; safe hook-driven lifecycle/tool/wait signals; live context; supervised runs; and private-service account/quota enrichment. Cumulative session/account spend, credits, native fork, compaction, remote control, structured answers, and subagent rows remain unsupported. Promote each capability in the same commit that adds its typed parser, fixture, mapping, and conformance case.

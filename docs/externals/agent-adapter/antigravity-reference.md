# Antigravity CLI protocol reference

> RimZ's mapping of this surface is [adapter_antigravity.md](../../internals/agents/adapter_antigravity.md). The provider-neutral lifecycle contract is [model.md](../../internals/agents/model.md), accounts and spend are [providers.md](../../internals/agents/providers.md), and the adapter playbook is [agent-adapters.md](../../contributing/agent-adapters.md).

This page mirrors the upstream surface of Google's Antigravity CLI (`agy`) that an adapter can bind to: the process and its flags, JSON command hooks, the custom statusline payload, conversations and transcripts, permissions, subagents, print mode, authentication, models and quota, and the adjacent remote-control, MCP, and plugin surfaces. It records what upstream ships and makes no claim about what RimZ supports.

## Baseline and evidence

The page mirrors **Antigravity CLI 1.2.2**, tag [`1.2.2`](https://github.com/google-antigravity/antigravity-cli/releases/tag/1.2.2) at commit `ba985e6b5de2ac8aa09860a154a102831eb7722b`, released 2026-09-12. Docs, changelog, and the installed binary were read on 2026-09-13. Repository paths below (`CHANGELOG.md`, `README.md`, `examples/`) are at that commit; the repository does not publish the CLI implementation.

Google publishes no CLI source, so each claim rests on one of these tiers, strongest first, and the tier is named where it is not the official docs:

| Tier | Evidence | How the page marks it |
| --- | --- | --- |
| Official docs | the `antigravity.google/docs` pages in [Upstream sources](#upstream-sources), a living site without versioned snapshots | cited by URL |
| Release notes | `CHANGELOG.md` at the tag, which `agy changelog` prints verbatim | "1.1.28 changelog" |
| Installed 1.2.2 binary | `agy --help` and subcommand help; the hook contract the binary embeds for its own `/hooks` answer; Go struct tags and protobuf message names in the binary's strings | "1.2.2 help", "embedded hook doc", "1.2.2 binary" |
| Live captures | a stock 1.1.2 session (hook payloads, transcripts, subagents), a 1.1.1 SQLite schema probe, and a first-run 1.2.2 session in an untrusted workspace (hook payloads and statusline only, no completed tool step) | "1.1.2 capture", "1.1.1 probe", "1.2.2 capture" |
| Third-party source | CodexBar commit [`b41715f`](https://github.com/steipete/CodexBar/tree/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8) for the private local service | cited by path |

Where two tiers disagree, the claim follows the wire and [Documentation drift](#documentation-drift) records the disagreement.

### Upstream sources

| Surface | Source |
| --- | --- |
| Releases and changelog | [releases](https://github.com/google-antigravity/antigravity-cli/releases), [`CHANGELOG.md`](https://github.com/google-antigravity/antigravity-cli/blob/ba985e6b5de2ac8aa09860a154a102831eb7722b/CHANGELOG.md) |
| Product boundary and install | [CLI overview](https://antigravity.google/docs/cli/overview/), [install](https://antigravity.google/docs/cli/install/), [`README.md`](https://github.com/google-antigravity/antigravity-cli/blob/ba985e6b5de2ac8aa09860a154a102831eb7722b/README.md), [`install.sh`](https://antigravity.google/cli/install.sh) |
| Slash commands, keybindings, settings | [CLI reference](https://antigravity.google/docs/cli/reference/), [settings](https://antigravity.google/docs/cli/settings/), [execution modes](https://antigravity.google/docs/cli/modes/) |
| Command hooks and tool vocabulary | [hooks](https://antigravity.google/docs/hooks/) |
| Statusline and terminal title | [statusline](https://antigravity.google/docs/cli/statusline/), [terminal title](https://antigravity.google/docs/cli/title/), [`examples/statusline/statusline.sh`](https://github.com/google-antigravity/antigravity-cli/blob/ba985e6b5de2ac8aa09860a154a102831eb7722b/examples/statusline/statusline.sh), [`examples/title/title.sh`](https://github.com/google-antigravity/antigravity-cli/blob/ba985e6b5de2ac8aa09860a154a102831eb7722b/examples/title/title.sh) |
| Conversations | [managing conversations](https://antigravity.google/docs/cli/conversations/), [`/resume`](https://antigravity.google/docs/cli/commands/resume/) |
| Permissions and sandbox | [permissions](https://antigravity.google/docs/cli/permissions/), [sandbox](https://antigravity.google/docs/cli/sandbox/) |
| Artifacts | [CLI artifacts](https://antigravity.google/docs/cli/artifacts/), [artifact model](https://antigravity.google/docs/artifacts/) |
| Subagents and tasks | [subagents](https://antigravity.google/docs/cli/subagents/), [`/agents`](https://antigravity.google/docs/cli/commands/agents/) |
| Print mode | [headless](https://antigravity.google/docs/cli/headless/), [best practices](https://antigravity.google/docs/cli/best-practices/) |
| Models, plans, quota, credits | [models](https://antigravity.google/docs/models/), [plans](https://antigravity.google/docs/plans/), [`/usage`](https://antigravity.google/docs/cli/commands/usage/), [`/credits`](https://antigravity.google/docs/cli/commands/credits/) |
| Remote control | [remote control](https://antigravity.google/docs/remote-control/) |
| Plugins, skills, MCP | [CLI plugins](https://antigravity.google/docs/cli/plugins/), [MCP](https://antigravity.google/docs/mcp/) |
| Gemini CLI transition | [migration guide](https://antigravity.google/docs/cli/gcli-migration/), [transition announcement](https://github.com/google-gemini/gemini-cli/discussions/27274) |
| SDK | [SDK overview](https://antigravity.google/docs/sdk/overview/), [`antigravity-sdk-python`](https://github.com/google-antigravity/antigravity-sdk-python) |

## Upstream scope

Antigravity CLI is the terminal TUI of the Antigravity product family. It shares an agent harness and shared configuration (`~/.gemini/config/`) with the Antigravity 2.0 desktop app and the IDE, but has its own binary, its own app-data directory (`~/.gemini/antigravity-cli`), and its own conversation cache.

Google moves the consumer terminal experience from Gemini CLI to Antigravity CLI. `agy plugin import gemini` converts Gemini extensions to plugins, and first launch offers a one-time settings import; skills must move from `.gemini/skills/` to `.agents/skills/` by hand ([migration guide](https://antigravity.google/docs/cli/gcli-migration/)). The two CLIs are not wire-compatible: hook names, payloads, session discovery, auth, flags, and transcripts all differ.

The SDK hosts its own agent runtime and does not observe a stock `agy` process. Antigravity 2.0 shares the conversation ID namespace and can export conversations into the CLI, but its process and app-data root (`~/.gemini/antigravity`) are separate.

Several upstream surfaces on this page have no RimZ binding at this baseline: print-mode `json` and `stream-json` output, `PreToolUse` decisions, the statusline `quota`, `cost`, and `conversation_title` fields, remote control, and the undocumented `SessionStart` hook variant. What RimZ leaves unsupported, and why, is in the internals page's [Known gaps](../../internals/agents/adapter_antigravity.md#known-gaps).

## Install and launch

The official installer places the executable at `~/.local/bin/agy` on macOS and Linux and under the per-user local `agy\bin` directory on Windows. The README publishes three installers:

```text
# macOS and Linux
curl -fsSL https://antigravity.google/cli/install.sh | bash

# Windows PowerShell
irm https://antigravity.google/cli/install.ps1 | iex

# Windows CMD
curl -fsSL https://antigravity.google/cli/install.cmd -o install.cmd && install.cmd && del install.cmd
```

`install.sh` pins no version: it reads the `version` key of a per-platform download manifest, and the installed CLI updates itself in the background during ordinary runs. `agy update` updates on demand. `agy install` only configures an existing installation's shell `PATH` and aliases (`--dir`, `--skip-path`, `--skip-aliases`).

### Flags

`agy --help` in 1.2.2 lists these top-level flags. The [headless](https://antigravity.google/docs/cli/headless/) page documents the print-mode subset; no docs page lists the interactive set.

| Flag | Meaning (1.2.2 help) |
| --- | --- |
| `--add-dir <path>` | add a workspace directory; repeatable |
| `--agent <name>` | custom agent for the session (`agy agents` lists them) |
| `-c`, `--continue` | continue the most recent conversation; see [Resume](#resume) |
| `--conversation <id>` | resume a conversation by ID |
| `--dangerously-skip-permissions` | auto-approve every tool permission request |
| `--disable-slash-commands` | disable slash-command and skill expansion in print mode |
| `--effort <low\|medium\|high>` | reasoning-effort variant of the selected model |
| `-i`, `--prompt-interactive <prompt>` | send an initial prompt, then stay interactive |
| `--input-format <text\|stream-json>` | print-mode input; `stream-json` requires `--output-format stream-json` |
| `--json-schema <schema-or-path>` | enforce structured output; for `stream-json` it applies to the final result |
| `--log-file <path>` | override the CLI log path |
| `--mode <accept-edits\|plan>` | execution mode; see [Execution modes](#execution-modes) |
| `--model <name>` | model by slug, name, or label (`agy models` lists them) |
| `--new-project` | create a new project for the session |
| `--output-format <text\|json\|stream-json>` | print-mode output, default `text` |
| `-p`, `--print`, `--prompt <prompt>` | run one prompt non-interactively; see [Print mode](#print-mode) |
| `--print-timeout <duration>` | print-mode wait ceiling, default `5m0s` |
| `--project <id-or-name>` | project for the session |
| `--sandbox` | enable terminal sandbox restrictions |

A prompt argument outside `-p` or `-i` is an error: 1.2.2 prints `Prompts are read only from -p/--print, -i/--prompt-interactive, or stdin` and exits. Since 1.1.18 a valueless prompt flag no longer swallows the next flag as its prompt. The binary also accepts an unlisted `--remote-control` launch flag, which the 1.1.19 changelog names; its semantics are undocumented.

No flag sets the working directory; the process cwd is the workspace. The [best-practices](https://antigravity.google/docs/cli/best-practices/) page shows `--cwd`, which 1.2.2 does not accept. No flag replaces or appends the system prompt; custom agents, rules, skills, and plugins carry instructions.

### Subcommands

| Subcommand | Meaning (1.2.2 help) |
| --- | --- |
| `agent`, `agents` | list available agents; `--output-format json\|stream-json` since 1.1.12 |
| `models` | list models; same output flag |
| `plugin`, `plugins` | `list`, `import [gemini\|claude]`, `install <target>` (supports `plugin@marketplace`), `uninstall`, `enable`, `disable`, `validate [path]`, `link <mp> <target>` |
| `mcp` | `add`, `remove`, `list`, `enable`, `disable` over the user-level `mcp_config.json`; `add` takes `--type stdio\|http`, `--env`, `--header` |
| `remote-control` | `start` (`--name`, `--session`), `status`, `stop`; see [Remote control](#remote-control-mcp-plugins-and-custom-agents) |
| `mic-serve` | serve this machine's microphone to a CLI on another host (`--addr`, default `127.0.0.1:4713`) |
| `changelog` | print release notes |
| `update` | update the CLI |
| `install` | configure `PATH` and aliases |
| `help <subcommand>` | subcommand help |

### Execution modes

The [execution modes](https://antigravity.google/docs/cli/modes/) page defines three modes, cycled in the TUI with `Shift+Tab` (`default` → `accept-edits` → `plan`) and selectable at launch with `--mode`:

| Mode | Behavior |
| --- | --- |
| `default` | pauses for diff review before creating or modifying a file (`y`, `n`, `f` full diff, `Ctrl+G` edit) |
| `accept-edits` | auto-approves `write_to_file`, `replace_file_content`, and `multi_replace_file_content`; subagents inherit the mode |
| `plan` | prepends the `/plan` instruction prefix so the agent outlines before writing code |

Tool permission rules and `--dangerously-skip-permissions` govern `run_command` in every mode. `--sandbox` changes containment, not approval: it enables the terminal sandbox for the session, and the persistent `toolPermission: proceed-in-sandbox` policy (auto-run sandboxed commands, ask for unsandboxed ones) has no launch flag. A launch flag overrides the persistent setting for that process, and `/config` marks the overridden row ([settings](https://antigravity.google/docs/cli/settings/)).

## Conversations

Antigravity calls a durable session a **conversation**. Hooks name its ID `conversationId`, the statusline `conversation_id`, and print-mode JSON `conversation_id`. Examples use UUIDs, but no page states UUID syntax as an invariant.

Conversation lists are scoped to the launch directory ([managing conversations](https://antigravity.google/docs/cli/conversations/)). `/resume` opens a picker with a flat or workspace-grouped view (`Ctrl+F`, default from the `pickerGrouping` setting since 1.1.26), `f2` rename, and `f4` delete. The picker can also import an Antigravity 2.0 conversation by cloning it into the CLI. Since 1.1.21 the CLI titles a conversation when it is created, and since 1.1.27 the title reaches the statusline as `conversation_title`.

### Resume

```text
agy --conversation <conversation-id>
agy --conversation=<conversation-id>
agy --continue
agy -c
```

`--conversation` resumes one exact ID and warns on stderr when the ID is not found. `-c` reads `~/.gemini/antigravity-cli/cache/last_conversations.json`, a map from absolute workspace path to latest conversation ID, and verifies the selection with the backend. Since 1.2.1, when that entry is missing or stale (launch from a subdirectory, after a crash, or with another session open in the workspace), `-c` falls back to the most recent non-empty conversation in the workspace or its parent and child directories. A 1.1.2 capture also showed the cache still naming an older conversation while a newer bare `agy` process was active. On exit the CLI prints the exact resume command.

Since 1.1.10, opening a conversation that is already open in another CLI instance on the machine shows a non-blocking advisory that points at `/fork`.

### Fork, rewind, and clear

`/fork` (alias `/branch`) clones the conversation up to the current turn into a new conversation ID and switches the current TUI to the clone; it does not clone the Git checkout. No launch flag forks a supplied source ID. Since 1.2.2 forks and snapshot reverts skip the internal `.system_generated/subagents` and `.system_generated/worktrees` directories.

`/rewind` (alias `/undo`) reverts conversation history to an earlier step. `/clear` resets the terminal and the active conversation context. A 1.1.2 capture showed the first `PreInvocation` after `/clear` carrying a new `conversationId` while the same `agy` process kept running in the same pane. No page defines the lineage between the old and new IDs for `/clear` or `/fork`. In print mode, `-p "/clear"` and other interactive-only commands fail and name the flag or subcommand that replaces them (1.1.11 changelog).

## Command hooks

Hooks run shell commands at five points of the agent loop. Each handler receives one camelCase (protojson) JSON object on stdin and returns one JSON object on stdout. Handlers run synchronously and block the loop.

### Files and format

| File | Scope |
| --- | --- |
| `~/.gemini/config/hooks.json` | global, shared with Antigravity 2.0 |
| `<workspace>/.agents/hooks.json` | workspace; loads once the folder is trusted |
| `hooks.json` inside a plugin | plugin; a disabled plugin's hooks do not run (1.1.7 changelog) |

`hooks.json` maps a hook name to its event configuration. Named hooks from every source that target the same event are merged and run sequentially. `PreToolUse` and `PostToolUse` take matcher groups; `PreInvocation`, `PostInvocation`, and `Stop` take a flat list of handlers and ignore any matcher:

```json
{
  "my-linter-hook": {
    "PostToolUse": [
      {
        "matcher": "run_command",
        "hooks": [
          { "type": "command", "command": "./scripts/lint.sh", "timeout": 10 }
        ]
      }
    ]
  },
  "safety-gate": {
    "enabled": false,
    "PreToolUse": [
      { "matcher": "run_command", "hooks": [{ "command": "./scripts/safety-check.sh" }] }
    ]
  },
  "reminder": {
    "PreInvocation": [
      { "type": "command", "command": "./scripts/reminder.sh" }
    ]
  }
}
```

| Field | Level | Meaning |
| --- | --- | --- |
| `enabled` | named hook | optional boolean, default `true`; `false` disables every handler of the hook |
| `PreToolUse`, `PostToolUse`, `PreInvocation`, `PostInvocation`, `Stop` | named hook | handler arrays |
| `matcher` | tool-event group | regular expression over the tool name; `""` and `*` match all tools |
| `hooks` | tool-event group | the group's handlers |
| `type` | handler | optional, default `command`, the only supported value |
| `command` | handler | required; run through `sh -c` on Unix and `cmd /c` on Windows, with `~` expanded and the cwd set to the directory holding `hooks.json` (embedded hook doc) |
| `timeout` | handler | optional seconds, default `30` |

The matcher documents `run_command|view_file` and `browser_.*` as examples. Tool names are step types lowercased with the `CORTEX_STEP_TYPE_` prefix removed (embedded hook doc), so the browser tool names exist but are not catalogued.

### Common input

| Field | Meaning ([hooks](https://antigravity.google/docs/hooks/)) |
| --- | --- |
| `conversationId` | active conversation ID |
| `workspacePaths` | absolute workspace directories |
| `transcriptPath` | absolute path of the conversation transcript; documented as `<app_data_dir>/brain/<conversationId>/.system_generated/logs/transcript.jsonl` |
| `artifactDirectoryPath` | absolute path of the conversation's artifact directory, `<app_data_dir>/brain/<conversationId>` |
| `modelName` | model handling the invocation, for example `gemini-3.6-flash-medium` (the embedded doc's example is `auto`) |

The 1.2.2 capture sent `workspacePaths: []` from a workspace that was not yet trusted. `<app_data_dir>` is `~/.gemini/antigravity-cli` for the CLI, `~/.gemini/antigravity` for 2.0, and `antigravity-ide` for the IDE. The 1.1.2 capture's `transcriptPath` named `transcript_full.jsonl`, not the documented `transcript.jsonl`; see [Transcripts](#transcripts).

### Events

| Event | Fires | Event input | Output |
| --- | --- | --- | --- |
| `PreToolUse` | before a tool executes | `toolCall.name`, `toolCall.args` (the 1.2.2 capture's args also carried `toolAction` and `toolSummary` strings), `stepIdx` (0-based) | required `decision`; optional `reason`, `permissionOverrides[]`, `overwrite` |
| `PostToolUse` | after a tool completes, tool steps only (1.1.9) | `toolCall` (`name`, `args`), `stepIdx`, optional `error` string, empty on success | `{}` |
| `PreInvocation` | before each model call | `invocationNum` (0 for the first call of an execution), `initialNumSteps` | optional `injectSteps[]` |
| `PostInvocation` | after each model invocation completes | same as `PreInvocation` | optional `injectSteps[]`, `terminationBehavior` |
| `Stop` | when the execution loop terminates | `executionNum`, `terminationReason`, optional `error`, required `fullyIdle` | required `decision`; optional `reason` |

`PostToolUse.toolCall` is documented on the web page and present in the 1.2.2 binary's hook message, but absent from the embedded hook doc and not yet live-captured. `Stop.terminationReason` examples are `model_stop`, `max_steps_exceeded`, and `error`. `fullyIdle = false` means background commands or asynchronous tasks are still running while the foreground loop stops.

Since 1.1.10, `hooks.json` hooks run before the built-in termination checks, so `PostInvocation` observes the final invocation of a turn and `Stop` hooks always run. Since 1.1.9, a `Stop` hook that keeps answering `continue` loses its block after a configured number of consecutive continuations and the turn ends.

### Decisions

`PreToolUse.decision` accepts five values ([hooks](https://antigravity.google/docs/hooks/)); the binary's JSON schema for the field enumerates the same five:

| Value | Meaning |
| --- | --- |
| `allow` | run the tool without prompting |
| `deny` | block the tool |
| `ask` | prompt, honoring an Always Allow grant |
| `force_ask` | prompt regardless of cached grants |
| `deny_unless_prior_grant` | block unless the resource was approved by an earlier user grant |

`permissionOverrides` is an array of permission resources such as `command(npm test)` that override default tool permissions. `overwrite` (embedded hook doc) is an object shallow-merged into the tool call's arguments before it runs; the tool result then tells the agent which keys a hook rewrote.

No value is documented as leaving native policy unchanged. `allow` can skip a prompt that policy would show, and `ask` can add one it would skip. The 1.0.16 changelog says an empty decision string no longer errors, but no source defines whether `{}`, `{"decision":""}`, empty stdout, a non-zero exit, or malformed JSON are equivalent.

An injected step (`injectSteps[]`) carries exactly one of `toolCall` (`{name, args}`), `userMessage` (string), or `ephemeralMessage` (string, a transient system message). `terminationBehavior` is `force_continue`, `terminate`, or empty.

`Stop.decision = "continue"` blocks the stop, re-enters the loop, and injects `reason` as a system message; any other value, including `""`, allows the stop.

### Undocumented hook fields in the 1.2.2 binary

The binary's hook protobuf messages define more than the docs publish. These names come from generated getters in the binary's strings; no page documents them. The 1.2.2 capture settles one row: a `SessionStart` handler in `hooks.json` ran once, just before the first `PreInvocation` of a session launched with `-i`, with the common input and no event-specific fields. No capture shows whether the other fields are serialized.

| Message | Undocumented fields |
| --- | --- |
| common arguments | `agentName`, `executionId`, `isBattleMode`, `lastUserInput` |
| `PostInvocation` arguments | `modelOutput`, `modelThinking` |
| `PostToolUse` arguments | `result` |
| `PostToolUse` result | `overwriteResult` |
| `Stop` arguments | `finalModelOutput` |
| hook arguments oneof | a `SessionStart` variant (`sessionStartHookArgs`) whose result carries `injectSteps`; the string `SessionStart` also sits beside the five documented event names in the binary |
| handler config | a `prompt` handler variant beside `command`, while the embedded doc says prompt hooks are unsupported |

### Tool vocabulary

The [hooks](https://antigravity.google/docs/hooks/) page publishes these tool names and argument keys:

| Category | Tool | Arguments |
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
| agent | `invoke_subagent` | `Subagents[]`, each `Prompt`, `Role`, `TypeName`, optional `Workspace` |
| agent | `define_subagent` | `name`, `description`, `system_prompt`, optional `enable_mcp_tools`, `enable_write_tools`, `enable_subagent_tools` |
| agent | `send_message` | `Recipient`, `Message` |
| agent | `manage_subagents` | `Action` (`list`, `kill`, `kill_all`), optional `ConversationIds[]` |
| interaction | `ask_question` | `questions[]`, each `question`, `options[]`, `is_multi_select` |
| media | `generate_image` | `Prompt`, `ImageName`, optional `ImagePaths[]` |

The table is not exhaustive. Browser tools (`browser_*`), MCP tools, and tools the changelog names without arguments (`manage_inbox` in 1.1.13, `read_resource` in 1.1.16) also reach matchers.

## Statusline and terminal title

The custom statusline runs a command whenever agent state changes, pipes a snake_case JSON payload to its stdin, and renders its stdout with ANSI color. It is configured in `~/.gemini/antigravity-cli/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "~/.gemini/antigravity-cli/statusline.sh"
  }
}
```

| Key | Meaning ([statusline](https://antigravity.google/docs/cli/statusline/)) |
| --- | --- |
| `type` | `command` |
| `command` | the command to run |
| `padding` | blank lines above the status line |
| `enabled` | `false` suspends the script while keeping the command |
| `stack_with_default` | `true` renders the script below the built-in line instead of replacing it |

The terminal-title command ([terminal title](https://antigravity.google/docs/cli/title/)) receives the same payload but runs only while title customization is active, and strips ANSI and non-printable output.

### Payload

| Field | Shape and meaning | Source |
| --- | --- | --- |
| `cwd` | launch directory | docs |
| `session_id` | backward-compatibility alias of `conversation_id` | docs |
| `conversation_id` | current conversation ID | docs |
| `conversation_title` | current conversation title | 1.1.27 changelog, 1.2.2 binary |
| `transcript_path` | absolute transcript path, optional; see [Documentation drift](#documentation-drift) | docs |
| `model` | `{id, display_name, effort}`; the docs example and the 1.1.2 and 1.2.2 captures put the selector label, such as `Gemini 3.5 Flash (Medium)`, in both `id` and `display_name`; `effort` (`low` under `--effort low`) is undocumented | docs, 1.2.2 capture |
| `workspace` | `{current_dir, project_dir}` | docs |
| `version` | CLI version | docs |
| `context_window` | token totals, limit, percentages, and `current_usage`; below | docs |
| `exceeds_200k_tokens` | `true` once context exceeds 200k tokens; `null` before the first API call | docs |
| `product` | application name, for example `antigravity` | docs |
| `quota` | map from model or bucket ID to `{remaining_fraction, reset_time, reset_in_seconds}`, optional | docs |
| `agent_state` | `idle`, `thinking`, `working`, `tool_use`, or `initializing` | docs |
| `vcs` | `{type, branch, client, dirty}`; `type` is `git`, `jj`, or `hg` | docs |
| `sandbox` | `{enabled, allow_network}` | docs |
| `artifact_count` | artifacts produced in the conversation | docs |
| `task_count` | running background tasks | docs |
| `pending_input_count` | queued user messages | docs |
| `tool_confirmation_pending` | `true` while a tool-confirmation dialog is showing | docs |
| `plan_tier` | subscription tier, optional | docs |
| `email` | account email or LDAP identity | docs |
| `terminal_width` | live terminal width | docs |
| `execution_mode` | active prompt execution mode | docs |
| `vim` | `{mode}`: `NORMAL`, `INSERT`, `VISUAL`, or `VISUAL LINE`; present only in Vim editor mode | docs |
| `cost` | unrounded estimated cost of the current session; nested shape unpublished | 1.1.21 changelog, 1.2.2 binary |
| `agent` | active custom agent; shape unpublished | 1.2.2 binary |
| `subagents` | array of active subagents; the example script reads its length | example script, 1.2.2 binary |
| `artifacts`, `background_tasks` | arrays beside the two count fields; shape unpublished | 1.2.2 binary |

Most of these fields carry `omitempty` tags in the binary, so treat a missing field and a zero value alike. The docs example:

```json
{
  "context_window": {
    "total_input_tokens": 88244,
    "total_output_tokens": 61074,
    "context_window_size": 1048576,
    "used_percentage": 14.24,
    "remaining_percentage": 85.76,
    "current_usage": {
      "input_tokens": 63382,
      "output_tokens": 346,
      "cache_creation_input_tokens": 0,
      "cache_read_input_tokens": 20857
    }
  },
  "quota": {
    "gemini-weekly": {
      "remaining_fraction": 0.9378,
      "reset_time": "2026-07-06T07:50:32Z",
      "reset_in_seconds": 560580
    }
  }
}
```

No source says whether the `total_*` token fields count the current window or the whole session, or how often `current_usage` refreshes. The 1.1.12 changelog fixed quota that lagged one fetch; the full set of `quota` bucket IDs is unpublished.

## Transcripts and local state

### Transcripts

Each conversation keeps two JSONL transcripts under `<app_data_dir>/brain/<conversationId>/.system_generated/logs/`. The binary's embedded agent instructions describe them: `transcript.jsonl` is compact and truncates long fields, listing them in `truncated_fields`; `transcript_full.jsonl` is never truncated. Each line is one step:

| Field | Meaning (embedded instructions, 1.1.2 capture) |
| --- | --- |
| `step_index` | step index in the trajectory |
| `source` | `USER_EXPLICIT`, `MODEL`, `SYSTEM`, and others |
| `type` | step type, including `USER_INPUT`, `PLANNER_RESPONSE`, `INVOKE_SUBAGENT`, `CONVERSATION_HISTORY`, `CHECKPOINT` |
| `status` | `DONE`, `ERROR`, and others |
| `created_at` | RFC 3339 timestamp |
| `content` | optional text |
| `thinking` | model reasoning on `PLANNER_RESPONSE` steps |
| `tool_calls` | optional array of tool calls with arguments |
| `truncated_fields` | compact file only: the fields cut on this line |

No page publishes the full enums, an append guarantee, a locking contract, retention, or what `/rewind` does to either file. The 1.1.13 changelog fixed context compaction corrupting the transcript by rewriting it while a background message appended, so compaction rewrites the file in place.

The 1.1.2 capture observed these record shapes:

- A text turn is `USER_EXPLICIT` / `USER_INPUT` / `DONE` with the user's text, then `MODEL` / `PLANNER_RESPONSE` / `DONE` with the reply, around `SYSTEM` `CONVERSATION_HISTORY` and `CHECKPOINT` records. User content can wrap the request in `<USER_REQUEST>...</USER_REQUEST>` followed by `<ADDITIONAL_METADATA>` and settings blocks.
- A question turn adds an `ask_question` entry to a completed `MODEL` / `PLANNER_RESPONSE` record's `tool_calls`. `transcript_full.jsonl` carries `args.questions` as a JSON array; `transcript.jsonl` carries the same array JSON-encoded as a string.
- A subagent turn adds the `invoke_subagent` request and `INVOKE_SUBAGENT` result described in [Subagents](#subagents-and-background-tasks).

### Conversation databases

The CLI stores conversations in SQLite: the 1.0.4 changelog makes SQLite the conversation format, 1.0.5 makes `/resume` scan `.db` and `.db-wal` files, and 1.1.26 checkpoints the WAL on exit. A 1.1.1 probe found one database per conversation at `~/.gemini/antigravity-cli/conversations/<conversation-id>.db` and a shared `~/.gemini/antigravity-cli/conversation_summaries.db`, both at SQLite `user_version = 1`. Google publishes no schema; the tables below are that probe's.

| Table (per-conversation) | Columns |
| --- | --- |
| `trajectory_meta` | `trajectory_id`, `cascade_id`, `trajectory_type`, `source` |
| `steps` | `idx`, `step_type`, `status`, `has_subtrajectory`, `metadata`, `error_details`, `permissions`, `task_details`, `render_info`, `step_payload`, `step_format` |
| `gen_metadata` | `idx`, `data`, `size` |
| `executor_metadata` | `idx`, `data` |
| `parent_references` | `idx`, `data` |
| `trajectory_metadata_blob` | `id`, `data` |
| `battle_mode_infos` | `idx`, `data` |

Most payload columns are opaque blobs. The summary database's `conversation_summaries` table carries the conversation ID, title and preview, step count, modification and last-user-input times (both indexed), workspace URIs, status, source, project, agent, parent conversation ID, nesting depth, battle and winner IDs, `not_fully_idle`, `killed`, last-user-input step index, and `app_data_dir`.

### Other files

| Path under `~/.gemini/antigravity-cli/` | Role |
| --- | --- |
| `settings.json` | CLI preferences; see [Settings and permissions](#settings-and-permissions) |
| `keybindings.json` | action → key-sequence map; delete to restore defaults |
| `cache/last_conversations.json` | absolute workspace → latest conversation ID |
| `cache/projects.json` | workspace → project mapping |
| `brain/<conversationId>/` | artifacts, `scratch/`, and `.system_generated/` logs, subagent metadata, and worktrees |
| `updater/` | updater lock and timestamp state |
| `cli.log` | default CLI log (`--log-file` overrides) |

Shared customization lives in `~/.gemini/config/`: `hooks.json`, `config.json` (plugin enablement since 1.1.11, remote-control settings), `mcp_config.json`, and project-specific configuration under `projects/`.

## Subagents and background tasks

The agent drives subagents through `define_subagent`, `invoke_subagent`, `send_message`, and `manage_subagents`. Subagents run asynchronously and can nest. The `/agents` panel shows each one's identifier, role, status (`running`, `done`, `killed`, `error`), and current step; `K` kills, `A` and `D` approve or deny inline, and `Enter` expands a group ([`/agents`](https://antigravity.google/docs/cli/commands/agents/)). Globally, `Ctrl+K` fast-approves the pending subagent action and `Alt+J` jumps to the next subagent that needs approval. Descendants' tool confirmations relay to the root conversation. Custom agents declare subagents in Markdown frontmatter (`agents`, 1.1.27) and choose a model tier with `model`, default `inherit` (1.1.5).

A child is an ordinary conversation, and the 1.1.2 two-child capture showed how it links to its parent:

- Every child receives the ordinary command hooks with its own stable `conversationId`, its first workspace path, and its own transcript path. Child hooks carry no parent ID.
- The parent transcript holds a completed `MODEL` / `PLANNER_RESPONSE` record whose `invoke_subagent` `args.Subagents[]` entries carry `Prompt`, `Role`, `TypeName`, and optional `Workspace` in request order. Nested children that inherit the workspace use the literal `inherit`.
- The next completed `MODEL` / `INVOKE_SUBAGENT` record's `content` holds consecutive JSON objects inside prose, one per child in request order. Each carries `conversationId`, a `file:` `logAbsoluteUri` naming the child's transcript under `brain/<conversationId>`, and optional `workspaceUris`.
- The parent transcript flushed late: both children's `PreInvocation` and `Stop` hooks ran before the request and result records reached disk, and the records flushed before the parent's own `Stop`.
- `manage_subagents` with `Action=list` returned the same child IDs after completion, with no lifecycle state.

Print mode publishes the same relation directly: a `stream-json` `step_update` for an `invoke_subagent` step carries `subagent_info.subagents[]` with `type_name`, `role`, `conversation_id`, `log_uri`, and `workspace_uris` ([headless](https://antigravity.google/docs/cli/headless/)).

Background shell work starts through `run_command` with `RunPersistent` and is managed with `manage_task` and the `/tasks` panel. The statusline counts it in `task_count`, and `Stop.fullyIdle` is `false` while it runs. Stopping a subagent tree stops every descendant and the tasks they own (1.1.10 changelog).

## Human waits

Antigravity has three wait families, and each is answered only inside the TUI:

| Wait | Signal | Native answer |
| --- | --- | --- |
| tool permission | a prompt card (`Run this command?`, `Allow access to this URL?`, `Allow calling this tool?`, with a `Reason:` line when a hook or cross-project path caused it, since 1.1.28); statusline `tool_confirmation_pending`; the preceding `PreToolUse` carries the proposed call | `y` / `n`; file, URL, and MCP targets can be edited to widen the grant before allowing |
| question | `ask_question` with `questions[]` of options and `is_multi_select` | the question dialog; space or `x` toggles a multi-select option |
| artifact review | implementation plans, diffs, and media held for review per `artifactReviewPolicy` | the Artifact Review panel (`Ctrl+R`): approve, reject, or comment |

No API answers a wait out of band, and the answer to `ask_question` is not published as a wire. The artifact `status` and `type` enums are unpublished ([artifact model](https://antigravity.google/docs/artifacts/)).

Print mode has no one to answer: a tool needing unobtainable approval is soft-denied with a stderr notice (1.1.3) and listed in `denied_actions` (1.1.27), the agent settles questions itself (1.1.12), and plan review proceeds automatically (1.1.28).

## Print mode

`-p` runs one prompt and exits. Diagnostics, authentication prompts, progress, and permission notices go to stderr; stdout carries only the response or the event stream ([headless](https://antigravity.google/docs/cli/headless/)). Print mode uses cached credentials and fails with an authentication error rather than blocking when none exist. It honors `settings.json` policies (1.1.4), `--mode` (1.1.12), `--model` and `--effort` (1.1.10), and expands slash commands and skills unless `--disable-slash-commands` is set (1.1.9).

```text
agy -p "prompt"
agy -p "prompt" --output-format json
agy --conversation <conversation-id> -p "next prompt"
agy -c -p "next prompt"
```

### JSON result

`--output-format json` prints one object; `stream-json` ends with the same object as its `result` event:

| Field | Meaning |
| --- | --- |
| `conversation_id` | conversation to resume |
| `status` | `SUCCESS`, `ERROR`, `CANCELED`, `INTERRUPTED` (for example SIGINT), `INVALID`, `WAITING` (ended waiting on input), `RUNNING` (no terminal state) |
| `response` | free-text response |
| `error` | error message, only on failure |
| `duration_seconds` | wall-clock duration |
| `num_turns` | user turns in the conversation |
| `structured_output`, `json_schema` | parsed output and enforced schema under `--json-schema` |
| `usage` | `input_tokens`, `output_tokens`, `thinking_tokens`, `cache_read_tokens`, `total_tokens` |
| `denied_actions` | actions soft-denied during the run (1.1.27 changelog and a 1.2.2 binary tag; not on the docs page) |

No field reports dollars.

### Stream events

`--output-format stream-json` emits NDJSON with an `event` discriminator:

| Event | Payload |
| --- | --- |
| `init` | once: `conversation_id` and `init` `{cwd, tools[], permission_mode, model?, agent?, json_schema?}`; `permission_mode` is `request-review`, or `always-proceed` under `--dangerously-skip-permissions` |
| `step_update` | per step transition or text delta: `step_update` `{conversation_id, step_index, state, step_type, tool_name?, text_delta?, duration_seconds?, usage?, tool_info?, subagent_info?}` |
| `result` | once, at the end: `result`, the [JSON result](#json-result) |

`state` is `ACTIVE` or `DONE`. `step_type` is the closed set `user_input`, `agent_response`, `tool`, `checkpoint`. `tool_info` is `{name, parameters, output, error: {type, message}}`. A docs example:

```json
{"event":"init","conversation_id":"9ec58bfd-4d67-4f5e-83a5-9d907e9c6b1f","init":{"cwd":"/home/user/project","tools":["ask_permission","run_command","write_to_file","..."],"permission_mode":"request-review"}}
{"event":"step_update","step_update":{"conversation_id":"9ec58bfd-4d67-4f5e-83a5-9d907e9c6b1f","step_index":0,"state":"DONE","step_type":"user_input"}}
{"event":"step_update","step_update":{"conversation_id":"9ec58bfd-4d67-4f5e-83a5-9d907e9c6b1f","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"apple"}}
```

### Stream input

`--input-format stream-json` reads one message per stdin line and runs a turn for each in one conversation until stdin closes:

```json
{"event":"user","message":{"content":"string or [{\"type\":\"text\",\"text\":\"string\"}]"}}
```

| Input | Result |
| --- | --- |
| unrecognized `event` name | skipped, warning on stderr |
| `control_request` or `control_response` | `ERROR` result, session ends, exit 2 |
| a slash command the CLI answers itself, such as `/model` | `ERROR` result, session ends, exit 2 |
| missing `event`, invalid JSON, or a non-text content block | `ERROR` result, session ends, exit 1 |

### Exit, timeout, and read-only commands

A run that produces a response exits 0; a run that fails exits non-zero with the reason on stderr and, in JSON modes, in `status` and `error`. An unknown `--model` exits 1 with an error envelope that lists the available models. Fatal errors carry a stable `error:` marker on stderr (1.1.28). Tool errors and permission denials inside a run do not change the exit code (1.1.20).

`--print-timeout` bounds the wait and accepts durations such as `15m`. Since 1.1.28 an expired timeout returns the partial output and exits 0 with a stderr warning; an interrupt such as `Ctrl+C` still exits non-zero. The run also waits for running background tasks and timers within that bound, and leaves daemon tasks such as dev servers running.

Read-only slash commands answer without an agent turn, quota, or conversation: `-p "/usage"`, `/quota`, `/credits`, `/model`, `/effort`, `/skills` (1.1.11), and `/permissions`, `/hooks`, `/help`, `/changelog`, `/config` (1.1.12). Text output is one tab-separated record per line; the JSON formats give a structured payload whose shape is unpublished.

## Settings and permissions

`~/.gemini/antigravity-cli/settings.json` is sparse: it stores only non-default values. Since 1.1.16 a file the CLI cannot parse is left byte-identical and the status line names it.

| Key | Values and default ([settings](https://antigravity.google/docs/cli/settings/), [CLI reference](https://antigravity.google/docs/cli/reference/)) |
| --- | --- |
| `toolPermission` | `request-review` (default), `proceed-in-sandbox`, `strict`, `always-proceed` |
| `artifactReviewPolicy` | `asks-for-review` (default), `agent-decides`, `always-proceed` |
| `permissions` | `{allow[], deny[], ask[]}` of permission resources |
| `enableTerminalSandbox` | terminal sandbox on or off |
| `allowNonWorkspaceAccess` | off by default; since 1.1.14 grants read access only |
| `altScreenMode` | `default` (adaptive: inline over SSH), `always`, `never` (inline, for tmux, screen, and SSH) |
| `statusLine` | see [Statusline](#statusline-and-terminal-title) |
| `notifications` | desktop notification and terminal bell when a task completes or needs attention |
| `enableTelemetry` | usage statistics and crash reports |
| `modelProvider` | `gemini` routes to the Gemini API with `GEMINI_API_KEY` (1.1.13) |
| `pickerGrouping` | `/resume` default view, flat or by workspace (1.1.26) |
| `copyOnSelect` | copy mouse selections in alt-screen mode, default on (1.1.8) |
| `useG1Credits` | spend AI credits once plan quota is exhausted |
| `colorScheme`, `verbosity`, `runningLightSpeed`, `editor`, `editorMode`, `vimInsertFirst`, `showTips`, `showFeedbackSurvey` | presentation and editor preferences |

### Permission resources

A permission resource is `action(target)` ([permissions](https://antigravity.google/docs/cli/permissions/)). Precedence is Deny > Ask > Allow, and `*` as the target matches the whole action.

| Action | Target | Default |
| --- | --- | --- |
| `read_file` | absolute or workspace-relative path, recursive | Ask; auto-allowed in the workspace |
| `write_file` | same; implies `read_file` on the target | Ask; auto-allowed in the workspace |
| `read_url` | hostname, covering subdomains | Ask (always-allowed before 1.1.28) |
| `execute_url` | hostname, for browser actuation | Ask |
| `command` | word-by-word prefix, or `regex:<pattern>` | Ask |
| `unsandboxed` | command prefix or `regex:` pattern allowed to run outside the sandbox | Ask; deprecated, see below |
| `mcp` | `server/tool` or `server/*` | Ask |

Denying `read_file` on a path also denies `write_file` there. The system temp directory is readable and writable by default (1.1.6, 1.1.9). `always-proceed` also auto-approves MCP calls and page reads (1.1.21). A pattern approved at a prompt holds for the rest of the conversation (1.1.9).

The 1.2.2 changelog calls `unsandboxed` rules deprecated: at startup the CLI warns about each one in CLI, shared, and project configuration and explains migrating it to a `command` rule. The permissions page still documents `unsandboxed` without a deprecation note.

Project configuration under `~/.gemini/config/projects/` takes precedence over the global CLI settings (1.1.1 changelog).

## Authentication

The CLI signs in with Google in a browser and stores the session in the OS keyring (Apple Keychain, Secret Service over D-Bus, Windows Credential Manager); SSH sessions use a URL-and-code flow ([install](https://antigravity.google/docs/cli/install/)). On Linux without a D-Bus session bus, or for an hour after a keyring timeout, the CLI bypasses the keyring and stores tokens in files (1.1.3, 1.1.26). A 1.2.2 installation on this host has an `antigravity-oauth-token` file in the app-data root; its format is undocumented. `/logout` removes the stored session.

Other sign-in paths exist: Business sign-in for Gemini Enterprise with a Google Cloud project, Workforce Identity Federation, and Application Default Credentials (1.1.10), and a `GEMINI_API_KEY` environment variable with `modelProvider: "gemini"` (1.1.13), for which `/logout` has nothing to clear. No credential file or machine-readable auth-status command is documented.

| Environment variable | Effect |
| --- | --- |
| `GEMINI_API_KEY` | Gemini API credential, with `modelProvider: "gemini"` |
| `GOOGLE_GEMINI_BASE_URL` | custom Gemini API endpoint |
| `AGY_CLI_HIDE_ACCOUNT_INFO` | hides email and plan tier from the header; not documented to remove them from the statusline payload |
| `AGY_CLI_HIDE_LOGO` | hides the banner art |
| `AGY_CLI_DISABLE_ESCAPE_SEQUENCE_OPTIMIZATIONS` | disables renderer diffing |

## Models, quota, and cost

### Models

Antigravity routes several model families. The [models](https://antigravity.google/docs/models/) page lists Gemini 3.8 Flash, Gemini 3.7 Flash, Gemini 3.6 Flash, Gemini 3.1 Pro, Claude Sonnet 4.6 (thinking), Claude Opus 4.6 (thinking), GPT-OSS-120b, and the Nano Banana 2 image model; availability depends on plan and sign-in path. `agy models` prints the slugs `--model` accepts, and `--effort` or `/effort` picks a reasoning variant. `/model <name>` switches and saves the default (1.1.22), and `/model <name> <prompt>` runs one prompt on another model and returns (1.1.27). A model change during a turn applies after the turn.

The selector label format, seen in `model.id` and `model.display_name`, is a base name plus a parenthesized variant. The 1.1.2 capture listed `Gemini 3.5 Flash (Medium)`, `(High)`, `(Low)`, `Gemini 3.1 Pro (Low)`, `(High)`, `Claude Sonnet 4.6 (Thinking)`, `Claude Opus 4.6 (Thinking)`, and `GPT-OSS 120B (Medium)`. Hook `modelName` uses slug form, for example `gemini-3.6-flash-medium`.

### Quota and credits

Plans grant baseline quota that refreshes on plan-dependent five-hour and weekly windows, with optional AI-credit overage on eligible paid plans ([plans](https://antigravity.google/docs/plans/)). Google says quota is measured by work, not by a fixed prompt or token count. `/usage` (alias `/quota`) refreshes and shows quota; `/credits` shows credit balance and purchase links. The statusline `quota` map carries per-bucket remaining fractions and resets; credits have no documented machine-readable field outside print-mode `/credits`.

### Cost

The statusline `cost` field (1.1.21 changelog) is an unrounded estimated cost of the current session; its unit and nested shape are unpublished. Print-mode `usage` reports tokens without dollars. Plan quota and AI credits are not API-token billing.

### Private local service

A running `agy` exposes an undocumented Connect-over-HTTPS service on process-owned loopback ports with a self-signed certificate. Google does not publish it; the 1.2.2 binary still contains both paths below, and the wire was cross-checked against CodexBar's [`AntigravityStatusProbe`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityStatusProbe.swift), [`AntigravityStatusProbe+PortDetection`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityStatusProbe%2BPortDetection.swift), and [`AntigravityQuotaSummaryParser`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityQuotaSummaryParser.swift).

| RPC (POST) | Body | Returns |
| --- | --- | --- |
| `/exa.language_server_pb.LanguageServerService/GetUserStatus` | `{}` | account email and user tier or plan label |
| `/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary` | `{"forceRefresh":true}` | per-model quota buckets grouped by period |

Both take `Content-Type: application/json` and `Connect-Protocol-Version: 1`. The quota summary can sit at the root, under `response`, or under `summary`, and a bucket's remaining fraction can be direct, nested, or in a oneof wrapper. Period labels include explicit five-hour and weekly periods; the complete label set is unpublished.

## Remote control, MCP, plugins, and custom agents

These surfaces change tool vocabulary, prompts, or executable configuration, so they are indexed here without depth.

| Surface | What ships |
| --- | --- |
| Remote control | `agy remote-control start\|status\|stop` registers a headless daemon, built into the CLI, as a systemd user service (boot), LaunchAgent (login), or Scheduled Task, reachable from a browser. `--name` sets the instance name, stored as `cliRemoteControlHostname` in `~/.gemini/config/config.json`; `--session` scopes it to the login session. The daemon uses the CLI's sign-in ([remote control](https://antigravity.google/docs/remote-control/), 1.2.0 changelog). |
| MCP | servers in the user-level `mcp_config.json` (comments and trailing commas allowed since 1.1.24), managed with `agy mcp` or `/mcp`; per-server `disabled`, `disabledTools[]`, `enabledTools`, `timeoutSeconds`, `url`, and `authProviderType: "google_credentials"`; plugin-bundled servers are namespaced `<plugin>_<server>` (1.2.2) ([MCP](https://antigravity.google/docs/mcp/)) |
| Plugins | installed and enabled with `agy plugin`; enablement lives in `~/.gemini/config/config.json`; a plugin can ship `hooks.json`, skills, and `rules.json` |
| Skills | `SKILL.md` under `.agents/skills/`; `disable-slash-command: true` hides a skill from the `/` menu (1.1.12) |
| Custom agents | Markdown `agent.md` with YAML frontmatter (`mainAgent`, `subagent`, `hidden`, `model`, `skills`, `rules`, `agents`, `inheritCustomizations`, `excludeDefaultComponents`, `commandExecutionPolicy`); since 1.1.25 they inherit ambient skills, rules, and subagents by default |
| SDK | Python SDK that hosts its own agent runtime with lifecycle hooks, streaming, persistence, and structured output ([SDK overview](https://antigravity.google/docs/sdk/overview/)); it does not attach to a running `agy` |

## Documentation drift

Official sources disagree with each other or with the 1.2.2 binary at these points:

| Surface | Official docs | Other evidence | Rule this page follows |
| --- | --- | --- | --- |
| Plan commands | [CLI reference](https://antigravity.google/docs/cli/reference/) lists `/planning` and `/fast`; the statusline page gives `planning` and `fast` as `execution_mode` examples | 1.1.0 changelog removes both and adds `/plan`; the [modes](https://antigravity.google/docs/cli/modes/) page uses `/plan` and `--mode plan` | use `--mode plan`; treat `execution_mode` values as unknown strings |
| Working directory | best-practices example passes `--cwd` | 1.2.2 help has no `--cwd` | set the process cwd |
| Transcript basename | hooks page names `transcript.jsonl` | 1.1.2 and 1.2.2 hooks sent `transcript_full.jsonl`; embedded instructions define both files | expect either basename |
| Statusline `transcript_path` | statusline page: absolute transcript path | the 1.2.2 capture named `~/.gemini/antigravity/brain/<id>/.system_generated/logs/transcript.jsonl`, under the 2.0 app-data root, while the same session's hooks named the CLI root's `transcript_full.jsonl` | take the transcript path from hooks |
| Statusline fields | statusline table lists counts (`artifact_count`, `task_count`) and omits `agent`, `subagents`, `artifacts`, `background_tasks`, `conversation_title`, `cost` | the binary tags all of them; the example script reads `subagents`; changelog adds `conversation_title` and `cost` | tolerate every field as optional |
| `PostToolUse` input | hooks page lists `toolCall` | embedded hook doc lists only `stepIdx` and `error`; not captured | treat `toolCall` as optional |
| `PostInvocation` timing | "immediately after each model invocation completes" | embedded hook doc: "after tool calls finish" | not settled without a capture |
| `PreToolUse` decisions | hooks page lists `deny_unless_prior_grant` | binary schema agrees; embedded hook doc lists four values | five values |
| `unsandboxed` rules | permissions page documents them | 1.2.2 changelog deprecates them in favor of `command` | treat as deprecated |
| Plugin location | CLI plugins page shows `~/.gemini/antigravity-cli/plugins/` | 1.0.2 changelog moves plugins to `~/.gemini/config/`; 1.1.11 moves enablement to `config.json` | shared config directory |
| Model catalog | models page starts at Gemini 3.6 Flash | the statusline payload example and the settings page's `--model="Gemini 3.5 Flash"` example still use Gemini 3.5 Flash | read the model from the payload |

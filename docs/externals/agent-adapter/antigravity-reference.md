# Antigravity CLI protocol reference

> RimZ's mapping of this surface is [adapter_antigravity.md](../../internals/agents/adapter_antigravity.md). The provider-neutral lifecycle contract is [model.md](../../internals/agents/model.md), accounts and spend are [providers.md](../../internals/agents/providers.md), and the adapter playbook is [agent-adapters.md](../../contributing/agent-adapters.md).

This page records Antigravity CLI (`agy`) protocols for adapter authors. It covers launch, hooks, statusline payloads, transcripts, permissions, headless output, authentication, and adjacent automation surfaces.

## Baseline and evidence

The release baseline is **Antigravity CLI 1.3.3**, tag [`1.3.3`](https://github.com/google-antigravity/antigravity-cli/releases/tag/1.3.3), released 2026-10-10, checked 2026-10-12. The tag resolves to commit `800852ea61796e9fe53077a2f945e74daacb4be6`. Repository links below use that commit. Google does not publish the CLI implementation.

The release delta covers 1.2.3 through 1.3.3. The released Linux x64 binary confirms the flags and subcommands below. Earlier captures and observations remain historical evidence, not 1.3.3 runtime verification. Bare version citations refer to the pinned changelog.

Evidence comes from these sources. Binary observations and live captures carry version labels.

| Tier | Evidence | How the page marks it |
| --- | --- | --- |
| Official docs | the `antigravity.google/docs` pages in [Upstream sources](#upstream-sources), a living site without versioned snapshots | cited by URL |
| Release notes | `CHANGELOG.md` at the tag, which `agy changelog` prints verbatim | "1.1.28 changelog" |
| Released 1.3.3 binary | [`agy_cli_linux_x64.tar.gz`](https://github.com/google-antigravity/antigravity-cli/releases/download/1.3.3/agy_cli_linux_x64.tar.gz), `--version`, `--help`, and subcommand help in an isolated home, plus embedded hook-schema strings | "1.3.3 help", "1.3.3 binary" |
| Installed 1.2.2 binary | `agy --help` and subcommand help. The hook contract the binary embeds for its own `/hooks` answer. Embedded agent instructions, Go struct tags, and protobuf message names in the binary's strings | "1.2.2 help", "embedded hook doc", "1.2.2 binary" |
| Live captures | a historical 1.2.2 credential-file observation, a stock 1.1.2 session (hook payloads, transcripts, subagents), a 1.1.1 SQLite schema probe, and a first-run 1.2.2 session in an untrusted workspace (hook payloads and statusline only, no completed tool step) | "1.1.2 capture", "1.1.1 probe", "1.2.2 capture" |
| Third-party source | CodexBar commit [`b41715f`](https://github.com/steipete/CodexBar/tree/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8) for the private local service | cited by path |

Where two tiers disagree, the claim follows the wire and [Documentation drift](#documentation-drift) records the disagreement.

### Upstream sources

| Surface | Source |
| --- | --- |
| Releases and changelog | [releases](https://github.com/google-antigravity/antigravity-cli/releases), [`CHANGELOG.md`](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md) |
| Product boundary and install | [CLI overview](https://antigravity.google/docs/cli/overview/), [install](https://antigravity.google/docs/cli/install/), [`README.md`](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/README.md), [`install.sh`](https://antigravity.google/cli/install.sh) |
| Slash commands, keybindings, settings | [CLI reference](https://antigravity.google/docs/cli/reference/), [settings](https://antigravity.google/docs/settings?tab=cli), [execution modes](https://antigravity.google/docs/cli/modes/) |
| Command hooks and tool vocabulary | [hooks](https://antigravity.google/docs/hooks/) |
| Statusline and terminal title | [statusline](https://antigravity.google/docs/cli/statusline/), [terminal title](https://antigravity.google/docs/cli/title/), [`examples/statusline/statusline.sh`](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/examples/statusline/statusline.sh), [`examples/title/title.sh`](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/examples/title/title.sh) |
| Conversations | [managing conversations](https://antigravity.google/docs/cli/conversations/), [`/resume`](https://antigravity.google/docs/cli/commands/resume/) |
| Permissions and sandbox | [permissions](https://antigravity.google/docs/permissions?tab=cli), [sandbox](https://antigravity.google/docs/cli/sandbox/) |
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

Google moves the consumer terminal experience from Gemini CLI to Antigravity CLI. `agy plugin import gemini` converts Gemini extensions to plugins, and first launch offers a one-time settings import. Skills must move from `.gemini/skills/` to `.agents/skills/` by hand ([migration guide](https://antigravity.google/docs/cli/gcli-migration/)). The two CLIs are not wire-compatible: hook names, payloads, session discovery, auth, flags, and transcripts all differ.

The SDK hosts its own agent runtime and does not observe a stock `agy` process. Antigravity 2.0 shares the conversation ID namespace and can export conversations into the CLI, but its process and app-data root (`~/.gemini/antigravity`) are separate.

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

`agy --help` in 1.3.3 lists these top-level flags. The [headless](https://antigravity.google/docs/cli/headless/) page documents the print-mode subset. No docs page lists the interactive set.

| Flag | Meaning (1.3.3 help) |
| --- | --- |
| `--add-dir <path>` | add a workspace directory. Repeatable, default empty list |
| `--agent <name>` | custom agent for the session (`agy agents` lists them) |
| `-c`, `--continue` | continue the most recent conversation. See [Resume](#resume) |
| `--conversation <id>` | resume a conversation by ID |
| `--dangerously-skip-permissions` | auto-approve every tool permission request |
| `--disable-slash-commands` | disable slash-command and skill expansion in print mode |
| `--effort <low\|medium\|high\|xhigh\|max>` | reasoning-effort variant of the selected model |
| `-i`, `--prompt-interactive <prompt>` | send an initial prompt, then stay interactive |
| `--input-format <text\|stream-json>` | print-mode input, default `text`. `stream-json` requires `--output-format stream-json` |
| `--json-schema <schema-or-path>` | enforce structured output. For `stream-json` it applies to the final result |
| `--log-file <path>` | override the CLI log path |
| `--mode <accept-edits\|plan>` | execution mode. See [Execution modes](#execution-modes) |
| `--model <name>` | model by slug, name, or label (`agy models` lists them) |
| `--new-project` | create a new project for the session |
| `--output-format <text\|json\|stream-json>` | print-mode output, default `text` |
| `-p`, `--print`, `--prompt <prompt>` | run one prompt non-interactively. See [Print mode](#print-mode) |
| `--print-timeout <duration>` | print-mode wait ceiling, default `0s` (unlimited) since 1.2.6 |
| `--project <id-or-name>` | project for the session |
| `--remote-control` | open a remote connection for this CLI session |
| `--sandbox` | enable terminal sandbox restrictions |

A prompt argument outside `-p` or `-i` is an error: 1.2.2 prints `Prompts are read only from -p/--print, -i/--prompt-interactive, or stdin` and exits. Since 1.1.18 a valueless prompt flag no longer swallows the next flag as its prompt. See [Remote control](#remote-control-mcp-plugins-and-custom-agents) for the connection lifecycle.

No flag sets the working directory. The process cwd is the workspace. The [best-practices](https://antigravity.google/docs/cli/best-practices/) page shows `--cwd`, which 1.3.3 help does not list. No flag replaces or appends the system prompt. Custom agents, rules, skills, and plugins carry instructions.

### Subcommands

| Subcommand | Meaning (1.3.3 help) |
| --- | --- |
| `agent`, `agents` | list available agents. The 1.1.12 changelog adds `--output-format json\|stream-json`, omitted from 1.3.3 subcommand help |
| `models` | list models. Same historically documented output flag, omitted from 1.3.3 help |
| `plugin`, `plugins` | `list`, `import [gemini\|claude]`, `install <target>` (supports `plugin@marketplace`), `uninstall`, `enable`, `disable`, `validate [path]`, `link <mp> <target>` |
| `mcp` | `add`, `remove`, `list`, `enable`, `disable` over the user-level `mcp_config.json`. `add` takes `--type stdio\|http`, `--env`, `--header` |
| `remote-control` | `start` (`--name`, `--session`), `status`, `stop`. See [Remote control](#remote-control-mcp-plugins-and-custom-agents) |
| `mic-serve` | serve this machine's microphone to a CLI on another host (`--addr`, default `127.0.0.1:4713`) |
| `changelog` | print release notes |
| `update` | update the CLI |
| `install` | configure `PATH` and aliases |
| `help <subcommand>` | subcommand help |

`mcp add [flags] <name> <commandOrUrl> [args...]` requires flags before the name. `--type` defaults to `stdio`, and HTTP URLs are detected automatically. Aliases are `-t` for `--type`, `-e` for `--env`, and `-H` for `--header` (1.3.3 help).

### Execution modes

The [execution modes](https://antigravity.google/docs/cli/modes/) page defines three modes, cycled in the TUI with `Shift+Tab` (`default` → `accept-edits` → `plan`) and selectable at launch with `--mode`:

| Mode | Behavior |
| --- | --- |
| `default` | pauses for diff review before creating or modifying a file (`y`, `n`, `f` full diff, `Ctrl+G` edit) |
| `accept-edits` | auto-approves `write_to_file`, `replace_file_content`, and `multi_replace_file_content`. Subagents inherit the mode |
| `plan` | prepends the `/plan` instruction prefix so the agent outlines before writing code |

Tool permission rules and `--dangerously-skip-permissions` govern `run_command` in every mode. The 1.3.2 changelog says that flag no longer auto-approves implementation plans in `/plan` mode. `artifactReviewPolicy` controls artifact approval ([1.3.2 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#132)).

`--sandbox` enables terminal containment for the session. The persistent `toolPermission: proceed-in-sandbox` policy auto-runs sandboxed commands and asks for unsandboxed ones. It has no launch flag. `/config` marks settings overridden by launch flags ([settings](https://antigravity.google/docs/settings?tab=cli)).

## Conversations

Antigravity calls a durable session a **conversation**. Hooks name its ID `conversationId`, the statusline `conversation_id`, and print-mode JSON `conversation_id`. The [hooks](https://antigravity.google/docs/hooks/) page describes `conversationId` as a UUID.

Conversation lists are scoped to the launch directory ([managing conversations](https://antigravity.google/docs/cli/conversations/)). `/resume` opens a picker with a flat or workspace-grouped view (`Ctrl+F`, default from the `pickerGrouping` setting since 1.1.26), `f2` rename, and `f4` delete. The picker can also import an Antigravity 2.0 conversation by cloning it into the CLI. Since 1.1.21 the CLI titles a conversation when it is created, and since 1.1.27 the title reaches the statusline as `conversation_title`.

### Resume

```text
agy --conversation <conversation-id>
agy --conversation=<conversation-id>
agy --continue
agy -c
```

`--conversation` resumes one exact ID and warns on stderr when the ID is not found (1.1.12 changelog). The sources do not establish whether a missing ID exits or starts a fresh conversation. `-c` reads `~/.gemini/antigravity-cli/cache/last_conversations.json`, a map from absolute workspace path to latest conversation ID, and checks that the selected conversation exists. Since 1.2.1, when that entry is missing or stale (launch from a subdirectory, after a crash, or with another session open in the workspace), `-c` falls back to the most recent non-empty conversation in the workspace or its parent and child directories. A 1.1.2 capture also showed the cache still naming an older conversation while a newer bare `agy` process was active. On exit the CLI prints the exact resume command.

Since 1.1.10, opening a conversation that is already open in another CLI instance on the machine shows a non-blocking advisory that points at `/fork`.

### Fork, rewind, and clear

`/fork` (alias `/branch`) clones the conversation up to the current turn into a new conversation ID and switches the current TUI to the clone. It does not clone the Git checkout. No launch flag forks a supplied source ID. Since 1.2.2 forks and snapshot reverts skip the internal `.system_generated/subagents` and `.system_generated/worktrees` directories.

`/rewind` (alias `/undo`) reverts conversation history to an earlier step. `/clear` resets the terminal and the active conversation context. A 1.1.2 capture showed the first `PreInvocation` after `/clear` carrying a new `conversationId` while the same `agy` process kept running in the same pane. No page defines the lineage between the old and new IDs for `/clear` or `/fork`. In print mode, `-p "/clear"` and other interactive-only commands fail and name the flag or subcommand that replaces them (1.1.11 changelog).

Since 1.2.6, stored edits larger than 1 MiB omit the raw diff body. `/rewind` skips those elided diffs ([1.2.6 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#126)). Since 1.3.1, the rewind picker disables messages cleared to save space ([1.3.1 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#131)).

## Command hooks

The [hooks](https://antigravity.google/docs/hooks/) page documents six events, including `SessionStart`. Each handler receives one camelCase (protojson) JSON object on stdin and returns one JSON object on stdout. Handlers run synchronously and block the loop.

Since 1.3.3, `/btw` (a side question during an active conversation) do not run user or workspace hooks ([1.3.3 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#133)). Since 1.2.4, customization token-budget truncation no longer drops `hooks.json` configurations ([1.2.4 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#124)).

### Files and format

| File | Scope |
| --- | --- |
| `~/.gemini/config/hooks.json` | global, shared with Antigravity 2.0 |
| `~/.gemini/antigravity-cli/settings.json` | alternative global location listed by the hooks docs. Nesting format not specified there or verified here |
| `<workspace>/.agents/hooks.json` | workspace. Loads once the folder is trusted |
| `hooks.json` inside a plugin | plugin. A disabled plugin's hooks do not run (1.1.7 changelog) |

`hooks.json` maps a hook name to its event configuration. Named hooks from every source that target the same event are merged and run sequentially. `PreToolUse` and `PostToolUse` take matcher groups. `SessionStart`, `PreInvocation`, `PostInvocation`, and `Stop` take a flat list of handlers and ignore any matcher:

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
| `enabled` | named hook | optional boolean, default `true`. `false` disables every handler of the hook |
| `SessionStart`, `PreToolUse`, `PostToolUse`, `PreInvocation`, `PostInvocation`, `Stop` | named hook | handler arrays |
| `matcher` | tool-event group | regular expression over the tool name. `""` and `*` match all tools |
| `hooks` | tool-event group | the group's handlers |
| `type` | handler | optional, default `command`, the only supported value |
| `command` | handler | required. Run through `sh -c` on Unix and `cmd /c` on Windows, with `~` expanded and the cwd set to the directory holding `hooks.json` (embedded hook doc) |
| `timeout` | handler | optional seconds, default `30` |

The matcher documents `run_command|view_file` and `browser_.*` as examples. Tool names are step types lowercased with the `CORTEX_STEP_TYPE_` prefix removed (embedded hook doc), so the browser tool names exist but are not catalogued.

### Common input

| Field | Meaning ([hooks](https://antigravity.google/docs/hooks/)) |
| --- | --- |
| `conversationId` | active conversation ID |
| `workspacePaths` | absolute workspace directories |
| `transcriptPath` | absolute path of the conversation transcript. Documented as `<app_data_dir>/brain/<conversationId>/.system_generated/logs/transcript.jsonl` |
| `artifactDirectoryPath` | absolute path of the conversation's artifact directory, `<app_data_dir>/brain/<conversationId>` |
| `modelName` | model handling the invocation, for example `gemini-3.6-flash-medium` (the embedded doc's example is `auto`) |

The 1.2.2 capture sent `workspacePaths: []` from a workspace that was not yet trusted. `<app_data_dir>` is `~/.gemini/antigravity-cli` for the CLI, `~/.gemini/antigravity` for 2.0, and `~/.gemini/antigravity-ide` for the IDE. See [Transcripts](#transcripts) for captured basename differences and path selection.

### Events

| Event | Fires | Event input | Output |
| --- | --- | --- | --- |
| `SessionStart` | once when a new conversation session starts, before the first model invocation | common input only | optional `injectSteps[]` |
| `PreToolUse` | before a tool executes | `toolCall.name`, `toolCall.args` (the 1.2.2 capture's args also carried `toolAction` and `toolSummary` strings), `stepIdx` (0-based) | required `decision`. Optional `reason`, `permissionOverrides[]`, `overwrite` |
| `PostToolUse` | after a tool completes, tool steps only (1.1.9) | `toolCall` (`name`, `args`), `stepIdx`, optional `error` string, empty on success | `{}` |
| `PreInvocation` | before each model call | `invocationNum` (0 for the first call of an execution), `initialNumSteps` | optional `injectSteps[]` |
| `PostInvocation` | after each model invocation completes | same as `PreInvocation` | optional `injectSteps[]`, `terminationBehavior` |
| `Stop` | when the execution loop terminates | `executionNum`, `terminationReason`, optional `error`, required `fullyIdle` | required `decision`. Optional `reason` |

The 1.2.2 capture observed `SessionStart` before the first `PreInvocation` of a fresh `-i` session. Its behavior on resume, `/clear`, and child conversations remains unverified.

`PostToolUse.toolCall` is documented on the web page and present in the 1.2.2 binary's hook message, but absent from the embedded hook doc and not yet live-captured. `Stop.terminationReason` examples are `model_stop`, `max_steps_exceeded`, and `error`. `fullyIdle = false` means background commands or asynchronous tasks are still running while the foreground loop stops.

Since 1.1.10, `hooks.json` hooks run before the built-in termination checks, so `PostInvocation` observes the final invocation of a turn and `Stop` hooks always run. The 1.1.9 changelog describes a cap on consecutive `continue` decisions. It names neither the setting nor its default.

### Decisions

The [hooks](https://antigravity.google/docs/hooks/) page documents these six `PreToolUse.decision` values. The 1.3.3 binary schema string lists only five, omitting `auto_approve`. Runtime acceptance of the sixth value is unverified.

| Value | Meaning in the living hooks docs |
| --- | --- |
| `allow` | delegate to the next hook or default policy |
| `auto_approve` | approve when default policy would prompt, while honoring explicit user rules. Absent from the 1.3.3 embedded schema |
| `deny` | block the tool |
| `ask` | prompt, honoring an Always Allow grant |
| `force_ask` | prompt regardless of cached grants |
| `deny_unless_prior_grant` | block unless the resource was approved by an earlier user grant |

`permissionOverrides` is an array of permission resources such as `command(npm test)` that override default tool permissions. `overwrite` is an object shallow-merged into the tool call's arguments before it runs. The tool result then tells the agent which keys a hook rewrote.

The embedded hook doc describes `allow` as automatic approval, unlike the living page. Neither source establishes its runtime behavior in 1.3.3. An observe-only handler must not assume that `allow` preserves native policy.

The 1.0.16 changelog says an empty decision string no longer errors. It does not define equivalence between `{}`, `{"decision":""}`, empty stdout, non-zero exit, malformed JSON, or handler timeout.

An injected step (`injectSteps[]`) carries one of `toolCall` (`{name, args}`), `userMessage` (string), `ephemeralMessage` (string), or `systemMessage` (object). The [hooks](https://antigravity.google/docs/hooks/) page gives the latter as `{systemMessage: "Remember to lint", metadata: {source: "policy"}}`. `terminationBehavior` is `force_continue`, `terminate`, or empty.

`Stop.decision = "continue"` blocks the stop, re-enters the loop, and injects `reason` as a system message. Any other value, including `""`, allows the stop.

### Undocumented hook fields in the 1.2.2 binary

These historical names come from generated getters in the 1.2.2 binary. The hooks page does not document them. No capture establishes whether they serialize.

| Message | Undocumented fields |
| --- | --- |
| common arguments | `agentName`, `executionId`, `isBattleMode`, `lastUserInput` |
| `PostInvocation` arguments | `modelOutput`, `modelThinking` |
| `PostToolUse` arguments | `result` |
| `PostToolUse` result | `overwriteResult` |
| `Stop` arguments | `finalModelOutput` |
| handler config | a `prompt` handler variant beside `command`, while the embedded doc says prompt hooks are unsupported |

The [1.3.3 binary](https://github.com/google-antigravity/antigravity-cli/releases/download/1.3.3/agy_cli_linux_x64.tar.gz) adds generated getters absent from 1.2.2: `HookArgsCommon.GetParentConversationId`, `GetRootConversationId`, and `GetCallerContext`. These suggest parent linkage but do not establish serialized fields or populated values. `HookInjectedStep.GetCheckpoint` and `HookCheckpoint.GetSessionSummary` also appear. Their command-hook wire behavior is unverified.

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
| execution | `schedule` | `Prompt`, optional `DurationSeconds`, `CronExpression`, `MaxIterations` |
| permission | `list_permissions` | none |
| permission | `ask_permission` | `Action`, `Target`, `Reason` |
| agent | `invoke_subagent` | `Subagents[]`, each `Prompt`, `Role`, `TypeName`, optional `Workspace` |
| agent | `define_subagent` | `name`, `description`, `system_prompt`, optional `enable_mcp_tools`, `enable_write_tools`, `enable_subagent_tools` |
| agent | `send_message` | `Recipient`, `Message` |
| agent | `manage_subagents` | `Action` (`list`, `kill`, `kill_all`), optional `ConversationIds[]` |
| interaction | `ask_question` | `questions[]`, each `question`, `options[]`, `is_multi_select` |
| media | `generate_image` | `Prompt`, `ImageName`, optional `ImagePaths[]` |

The table is not exhaustive. Browser tools (`browser_*`), MCP tools, and tools the changelog names without arguments (`manage_inbox` in 1.1.13, `read_resource` in 1.1.16) also reach matchers.

`find_by_name`, `grep_search`, and `list_dir` left the default agent and subagent toolset in 1.2.7. Custom agents can still list them explicitly in `tools` ([1.2.7 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#127)). Since 1.2.16, image requests run through the built-in `image-generator` subagent ([1.2.16 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1216)). The legacy `generate_image` row above describes the published hook vocabulary, not a guaranteed default tool.

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
| `transcript_path` | absolute transcript path, optional. See [Documentation drift](#documentation-drift) | docs |
| `model` | `{id, display_name, effort}`. The docs example and the 1.1.2 and 1.2.2 captures put the selector label, such as `Gemini 3.5 Flash (Medium)`, in both `id` and `display_name`. `effort` (`low` under `--effort low`) is undocumented | docs, 1.2.2 capture |
| `workspace` | `{current_dir, project_dir}` | docs |
| `version` | CLI version | docs |
| `context_window` | token totals, limit, percentages, and `current_usage`. Below | docs |
| `exceeds_200k_tokens` | `true` once context exceeds 200k tokens. `null` before the first API call | docs |
| `product` | application name, for example `antigravity` | docs |
| `quota` | map from model or bucket ID to `{remaining_fraction, reset_time, reset_in_seconds}`, optional | docs |
| `agent_state` | `idle`, `thinking`, `working`, `tool_use`, or `initializing` | docs |
| `vcs` | `{type, branch, client, dirty}`. `type` is `git`, `jj`, or `hg` | docs |
| `sandbox` | `{enabled, allow_network}` | docs |
| `artifact_count` | artifacts produced in the conversation | docs |
| `task_count` | background tasks, including timers, recurring jobs, and other background tools since 1.3.1 | docs, [1.3.1 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#131) |
| `pending_input_count` | queued user messages | docs |
| `tool_confirmation_pending` | `true` while a tool-confirmation dialog is showing | docs |
| `plan_tier` | subscription tier, optional | docs |
| `email` | account email or LDAP identity | docs |
| `terminal_width` | live terminal width | docs |
| `execution_mode` | active prompt execution mode | docs |
| `vim` | `{mode}`: `NORMAL`, `INSERT`, `VISUAL`, or `VISUAL LINE`. Present only in Vim editor mode | docs |
| `cost` | unrounded estimated cost of the current session. Nested shape unpublished | 1.1.21 changelog, 1.2.2 binary |
| `agent` | active custom agent. Shape unpublished | 1.2.2 binary |
| `subagents` | array of active subagents. The example script reads its length | example script, 1.2.2 binary |
| `artifacts`, `background_tasks` | arrays beside the two count fields. Shape unpublished | 1.2.2 binary |

The 1.2.2 binary marks most fields `omitempty`. Accept absent fields. Omission alone does not establish a measured zero. The docs example:

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

No source says whether the `total_*` token fields count the current window or the whole session, or how often `current_usage` refreshes. The 1.1.12 changelog fixed quota that lagged one fetch. The full set of `quota` bucket IDs is unpublished.

## Transcripts and local state

### Transcripts

Use the hook-provided `transcriptPath`. Historical statusline captures pointed at the wrong app-data root, as detailed in [Documentation drift](#documentation-drift).

Historical embedded instructions describe two JSONL transcripts per conversation under `<app_data_dir>/brain/<conversationId>/.system_generated/logs/`. The 1.2.2 embedded instructions distinguish them: `transcript.jsonl` is compact and truncates long fields, listing them in `truncated_fields`. `transcript_full.jsonl` is never truncated. Each line is one step:

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

No page publishes the full enums, an append guarantee, a locking contract, retention, or what `/rewind` does to either file. The 1.1.13 changelog describes transcript rewriting during compaction. It provides no append-only guarantee, so an offset-only reader cannot assume stable prior content.

Since 1.2.4, `transcript.jsonl` includes terminal `ERROR` steps that older versions omitted, including failed tool executions and timeouts ([1.2.4 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#124)).

The 1.1.2 capture observed these record shapes:

- A text turn is `USER_EXPLICIT` / `USER_INPUT` / `DONE` with the user's text, then `MODEL` / `PLANNER_RESPONSE` / `DONE` with the reply, around `SYSTEM` `CONVERSATION_HISTORY` and `CHECKPOINT` records. User content can wrap the request in `<USER_REQUEST>...</USER_REQUEST>` followed by `<ADDITIONAL_METADATA>` and settings blocks.
- A question turn adds an `ask_question` entry to a completed `MODEL` / `PLANNER_RESPONSE` record's `tool_calls`. `transcript_full.jsonl` carries `args.questions` as a JSON array. `transcript.jsonl` carries the same array JSON-encoded as a string.
- A subagent turn adds the `invoke_subagent` request and `INVOKE_SUBAGENT` result described in [Subagents](#subagents-and-background-tasks).

### Conversation databases

The CLI stores conversations in SQLite: the 1.0.4 changelog makes SQLite the conversation format, 1.0.5 makes `/resume` scan `.db` and `.db-wal` files, and 1.1.26 checkpoints the WAL on exit. A 1.1.1 probe found one database per conversation at `~/.gemini/antigravity-cli/conversations/<conversation-id>.db` and a shared `~/.gemini/antigravity-cli/conversation_summaries.db`, both at SQLite `user_version = 1`. Google publishes no schema. The tables below are that probe's.

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
| `settings.json` | CLI preferences. See [Settings and permissions](#settings-and-permissions) |
| `keybindings.json` | action → key-sequence map. Delete to restore defaults |
| `cache/last_conversations.json` | absolute workspace → latest conversation ID |
| `cache/projects.json` | workspace → project mapping |
| `brain/<conversationId>/` | artifacts, `scratch/`, and `.system_generated/` logs and subagent metadata |
| `worktrees/` | subagent Git worktrees since 1.2.10, moved out of the artifact tree ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1210)) |
| `updater/` | updater lock and timestamp state |
| `cli.log` | default CLI log (`--log-file` overrides) |

Shared customization lives in `~/.gemini/config/`: `hooks.json`, `config.json` (plugin enablement since 1.1.11, remote-control settings), `mcp_config.json`, and project-specific configuration under `projects/`.

## Subagents and background tasks

The agent drives subagents through `define_subagent`, `invoke_subagent`, `send_message`, and `manage_subagents`. Subagents run asynchronously and can nest. The `/agents` panel shows each one's identifier, role, status (`running`, `done`, `killed`, `error`), and current step. `K` kills, `A` and `D` approve or deny inline, and `Enter` expands a group ([`/agents`](https://antigravity.google/docs/cli/commands/agents/)). Globally, `Ctrl+K` fast-approves the pending subagent action and `Alt+J` jumps to the next subagent that needs approval. Descendants' tool confirmations relay to the root conversation. Custom agents declare subagents in Markdown frontmatter (`agents`, 1.1.27) and choose a model tier with `model`, default `inherit` (1.1.5).

A child is an ordinary conversation, and the 1.1.2 two-child capture showed how it links to its parent:

- Every child receives the ordinary command hooks with its own stable `conversationId`, its first workspace path, and its own transcript path. Those captured child hooks carried no parent ID. Later binary getters are described under [Undocumented hook fields](#undocumented-hook-fields-in-the-122-binary).
- The parent transcript holds a completed `MODEL` / `PLANNER_RESPONSE` record whose `invoke_subagent` `args.Subagents[]` entries carry `Prompt`, `Role`, `TypeName`, and optional `Workspace` in request order. Nested children that inherit the workspace use the literal `inherit`.
- The next completed `MODEL` / `INVOKE_SUBAGENT` record's `content` holds consecutive JSON objects inside prose, one per child in request order. Each carries `conversationId`, a `file:` `logAbsoluteUri` naming the child's transcript under `brain/<conversationId>`, and optional `workspaceUris`.
- The parent transcript flushed late: both children's `PreInvocation` and `Stop` hooks ran before the request and result records reached disk, and the records flushed before the parent's own `Stop`.
- `manage_subagents` with `Action=list` returned the same child IDs after completion, with no lifecycle state.

Print mode publishes the same relation directly: a `stream-json` `step_update` for an `invoke_subagent` step carries `subagent_info.subagents[]` with `type_name`, `role`, `conversation_id`, `log_uri`, and `workspace_uris` ([headless](https://antigravity.google/docs/cli/headless/)).

Background shell work starts through `run_command` with `RunPersistent` and is managed with `manage_task` and the `/tasks` panel. The statusline counts it in `task_count`, and `Stop.fullyIdle` is `false` while it runs. Stopping a subagent tree stops every descendant and the tasks they own (1.1.10 changelog).

`@<subagent> <message>` sends a message directly to a child conversation, with autocomplete for running and completed children ([1.2.9 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#129)). Since 1.3.1, subagent failures appear as errors until the child makes further progress. The task panel also includes timers, recurring jobs, and other background tools ([1.3.1 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#131)).

The 1.3.3 changelog names `branch` and `share` as subagent workspace modes. Both now isolate each repository in a multi-repository workspace. It does not define their full semantics or serialized request format. Stopping a child clears its pending approvals immediately ([1.3.3 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#133)).

## Human waits

The CLI exposes three interactive wait types:

| Wait | Signal | Native answer |
| --- | --- | --- |
| tool permission | a prompt card (`Run this command?`, `Allow access to this URL?`, `Allow calling this tool?`, with a `Reason:` line when a hook or cross-project path caused it, since 1.1.28). Statusline `tool_confirmation_pending`. The preceding `PreToolUse` carries the proposed call | `y` / `n`. File, URL, and MCP targets can be edited to widen the grant before allowing |
| question | `ask_question` with `questions[]` of options and `is_multi_select` | the question dialog. Space or `x` toggles a multi-select option |
| artifact review | implementation plans, diffs, and media held for review per `artifactReviewPolicy` | the Artifact Review panel (`Ctrl+R`): approve, reject, or comment |

The cited hook and headless contracts define no API for answering these interactive waits. They publish no wire format for `ask_question` answers. The artifact `status` and `type` enums are unpublished ([artifact model](https://antigravity.google/docs/artifacts/)).

Print mode has no one to answer: a tool needing unobtainable approval is soft-denied with a stderr notice (1.1.3) and listed in `denied_actions` (1.1.27), the agent settles questions itself (1.1.12), and the 1.1.28 changelog says plan review proceeds automatically. That headless rule was not recaptured after the 1.3.2 plan-approval change. The interaction remains unverified.

## Print mode

`-p` runs one prompt and exits. Diagnostics, authentication prompts, progress, and permission notices go to stderr. Stdout carries only the response or the event stream ([headless](https://antigravity.google/docs/cli/headless/)). Print mode uses cached credentials and fails with an authentication error rather than blocking when none exist. It honors `settings.json` policies (1.1.4), `--mode` (1.1.12), `--model` and `--effort` (1.1.10), and expands slash commands and skills unless `--disable-slash-commands` is set (1.1.9).

```text
agy -p "prompt"
agy -p "prompt" --output-format json
agy --conversation <conversation-id> -p "next prompt"
agy -c -p "next prompt"
```

### JSON result

`--output-format json` prints one object. `stream-json` ends with the same object as its `result` event:

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
| `denied_actions` | actions soft-denied during the run (1.1.27 changelog and a 1.2.2 binary tag. Not on the docs page) |

No field reports dollars.

`--json-schema` requires a JSON schema string or existing schema file with root `"type": "object"`. Since 1.2.14, primitive names, plain text, missing files, and non-object roots fail at startup with exit 1 ([1.2.14 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1214)). The headless docs still advertise primitive names.

### Stream events

`--output-format stream-json` emits NDJSON with an `event` discriminator:

| Event | Payload |
| --- | --- |
| `init` | once: `conversation_id` and `init` `{cwd, tools[], permission_mode, model?, agent?, json_schema?}`. `permission_mode` is `request-review`, or `always-proceed` under `--dangerously-skip-permissions` |
| `step_update` | per step transition or text delta: `step_update` `{conversation_id, step_index, state, step_type, tool_name?, text_delta?, duration_seconds?, usage?, tool_info?, subagent_info?}` |
| `result` | at the end of each turn: `result`, the [JSON result](#json-result) |

`state` is `ACTIVE` or `DONE`. Documented observed `step_type` values include `user_input`, `agent_response`, `tool`, and `checkpoint`. No exhaustive enum is published. `tool_info` is `{name, parameters, output, error: {type, message}}`. A docs example:

```json
{"event":"init","conversation_id":"9ec58bfd-4d67-4f5e-83a5-9d907e9c6b1f","init":{"cwd":"/home/user/project","tools":["ask_permission","run_command","write_to_file","..."],"permission_mode":"request-review"}}
{"event":"step_update","step_update":{"conversation_id":"9ec58bfd-4d67-4f5e-83a5-9d907e9c6b1f","step_index":0,"state":"DONE","step_type":"user_input"}}
{"event":"step_update","step_update":{"conversation_id":"9ec58bfd-4d67-4f5e-83a5-9d907e9c6b1f","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"apple"}}
```

### Stream input

`--input-format stream-json` reads one message per stdin line and runs a turn for each in one conversation until stdin closes. `init` appears once per session, and `result` appears once per turn. `response` is turn-local. `num_turns`, `usage`, and `duration_seconds` accumulate across the session ([headless](https://antigravity.google/docs/cli/headless/)).

```json
{"event":"user","message":{"content":"Summarize the latest commit."}}
{"event":"user","message":{"content":[{"type":"text","text":"List the tests it changes."}]}}
```

| Input | Result |
| --- | --- |
| unrecognized `event` name | skipped, warning on stderr |
| `control_request` or `control_response` | `ERROR` result, session ends, exit 2 |
| a slash command the CLI answers itself, such as `/model` | `ERROR` result, session ends, exit 2 |
| missing `event`, invalid JSON, or a non-text content block | `ERROR` result, session ends, exit 1 |

### Exit, timeout, and read-only commands

A successful run exits 0. An unknown `--model` exits 1 with an error envelope listing available models. Tool errors and permission denials inside a run do not change the exit code (1.1.20).

Since 1.2.6, an agent or model API failure exits 3 and writes `AGY_ERROR: {...}` to stderr. That JSON reports canonical status, HTTP or gRPC error code, retryability, and error ID ([1.2.6 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#126)). Since 1.2.10, this also applies after a partial response. JSON error output preserves that response. Multi-turn `stream-json` sessions warn and continue instead ([1.2.10 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1210)). The 1.1.28 changelog described an `error:` stderr prefix. The newer `AGY_ERROR` record covers agent and model failures. Its exact JSON field names are not published by the cited changelog.

`--print-timeout` accepts durations such as `15m`. The default `0s` waits until the turn completes. Since 1.2.6, the response wait is unlimited unless you set this flag ([1.2.6 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#126)). The 1.1.28 changelog specifies partial output, exit 0, and a stderr warning on timeout. Its exact warning and JSON status remain unverified in 1.3.3. Timeout expiry and model failure are distinct outcomes. An interrupt such as `Ctrl+C` exits non-zero, with no numeric code established here.

The 1.2.9 changelog caps background-task waiting at 30 minutes, independently of the unlimited foreground response wait. An explicit shorter `--print-timeout` bounds that wait. Daemon processes terminate when the headless run ends ([1.2.9 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#129)). Since 1.2.16, headless runs wait for the agent to respond to background work that finishes after its first turn ([1.2.16 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1216)).

This reference does not establish numeric exit codes for authentication failure or a stream-input session containing a recovered model error.

Read-only slash commands answer without an agent turn, quota, or conversation: `-p "/usage"`, `/quota`, `/credits`, `/model`, `/effort`, `/skills` (1.1.11), and `/permissions`, `/hooks`, `/help`, `/changelog`, `/config` (1.1.12). Text output is one tab-separated record per line. The JSON formats give a structured payload whose shape is unpublished.

Since 1.2.15, terminal `/usage` output shows reset times in local time. Piped and JSON output retain UTC ([1.2.15 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1215)).

## Settings and permissions

`~/.gemini/antigravity-cli/settings.json` is sparse: it stores only non-default values. Since 1.1.16 a file the CLI cannot parse is left byte-identical and the status line names it.

| Key | Values and default ([settings](https://antigravity.google/docs/settings?tab=cli), [CLI reference](https://antigravity.google/docs/cli/reference/)) |
| --- | --- |
| `toolPermission` | `request-review` (default), `proceed-in-sandbox`, `strict`, `always-proceed` |
| `artifactReviewPolicy` | `asks-for-review` (default), `agent-decides`, `always-proceed` |
| `permissions` | `{allow[], deny[], ask[]}` of permission resources |
| `enableTerminalSandbox` | terminal sandbox on or off |
| `allowNonWorkspaceAccess` | off by default. Since 1.1.14 grants read access only |
| `altScreenMode` | `default` (adaptive: inline over SSH), `always`, `never` (inline, for tmux, screen, and SSH) |
| `statusLine` | see [Statusline](#statusline-and-terminal-title) |
| `notifications` | desktop notification and terminal bell when a task completes or needs attention |
| `enableTelemetry` | usage statistics and crash reports |
| `modelProvider` | `gemini` routes to the Gemini API with `GEMINI_API_KEY` (1.1.13) |
| `pickerGrouping` | `/resume` default view, flat or by workspace (1.1.26) |
| `queuedMessages` | `send-immediately` delivers follow-ups mid-turn. The default Queue setting holds them until the turn ends ([1.2.14 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1214)) |
| `verbosity` | `high`, `medium`, `low`. Default `medium` since 1.3.0 ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#130)) |
| `copyOnSelect` | copy mouse selections in alt-screen mode, default on (1.1.8) |
| `useG1Credits` | spend AI credits once plan quota is exhausted |
| `colorScheme`, `runningLightSpeed`, `editor`, `editorMode`, `vimInsertFirst`, `showTips`, `showFeedbackSurvey` | presentation and editor preferences |

### Permission resources

A permission resource is `action(target)` ([permissions](https://antigravity.google/docs/permissions?tab=cli)). Precedence is Deny > Ask > Allow, and `*` as the target matches the whole action.

| Action | Target | Default |
| --- | --- | --- |
| `read_file` | absolute or workspace-relative path, recursive | Ask, auto-allowed in the workspace |
| `write_file` | same. Implies `read_file` on the target | Ask, auto-allowed in the workspace |
| `read_url` | hostname, covering subdomains | Ask (always-allowed before 1.1.28) |
| `execute_url` | hostname, for browser actuation | Ask |
| `command` | word-by-word prefix, or `regex:<pattern>` | Ask |
| `unsandboxed` | command prefix or `regex:` pattern allowed to run outside the sandbox | Ask, deprecated, see below |
| `mcp` | `server/tool` or `server/*` | Ask |

Denying `read_file` on a path also denies `write_file` there. The system temp directory is readable and writable by default (1.1.6, 1.1.9). `always-proceed` also auto-approves MCP calls and page reads (1.1.21). A pattern approved at a prompt holds for the rest of the conversation (1.1.9).

The 1.2.2 changelog calls `unsandboxed` rules deprecated: at startup the CLI warns about each one in CLI, shared, and project configuration and explains migrating it to a `command` rule. The permissions page still documents `unsandboxed` without a deprecation note.

Project configuration under `~/.gemini/config/projects/` takes precedence over the global CLI settings (1.1.1 changelog).

Since 1.3.3, the macOS terminal sandbox blocks reads under `~/Library/Application Support` by default. Explicit sandbox rules can grant access ([1.3.3 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#133)).

## Authentication

The CLI signs in with Google in a browser and stores the session in the OS keyring (Apple Keychain, Secret Service over D-Bus, Windows Credential Manager). SSH sessions use a URL-and-code flow ([install](https://antigravity.google/docs/cli/install/)). On Linux without a D-Bus session bus, or for an hour after a keyring timeout, the CLI bypasses the keyring and stores tokens in files (1.1.3, 1.1.26). A historical 1.2.2 installation had an `antigravity-oauth-token` file in the app-data root. Its format is undocumented. `/logout` removes the stored session.

Other sign-in paths exist: Business sign-in for Gemini Enterprise with a Google Cloud project, Workforce Identity Federation, and Application Default Credentials (1.1.10), and a `GEMINI_API_KEY` environment variable with `modelProvider: "gemini"` (1.1.13), for which `/logout` has nothing to clear. No credential file or machine-readable auth-status command is documented.

Since 1.2.5, definitive sign-in rejection clears stale tokens. Temporary network failures, server errors, and rate limits retain credentials ([1.2.5 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#125)). In 1.3.3, expired or revoked sign-in reports `Authentication expired. Please log back in.` ([1.3.3 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#133)).

| Environment variable | Effect |
| --- | --- |
| `GEMINI_API_KEY` | Gemini API credential, with `modelProvider: "gemini"` |
| `GOOGLE_GEMINI_BASE_URL` | custom Gemini API endpoint |
| `AGY_CLI_HIDE_ACCOUNT_INFO` | hides email and plan tier from the header. Not documented to remove them from the statusline payload |
| `AGY_ADC_AUTH` | Application Default Credentials sign-in. Remote Control rejects this path in 1.3.3 ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#133)) |
| `CLI_GRAPHICS` | `kitty` forces image mode, including multiplexer setups with image passthrough ([1.2.12 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1212)) |
| `AGY_CLI_HIDE_LOGO` | hides the banner art |
| `AGY_CLI_DISABLE_ESCAPE_SEQUENCE_OPTIMIZATIONS` | disables renderer diffing |

## Models, quota, and cost

### Models

Antigravity routes several model families. The [models](https://antigravity.google/docs/models/) page lists Gemini 3.8 Flash, Gemini 3.7 Flash, Gemini 3.6 Flash, Gemini 3.1 Pro, Claude Sonnet 5.5 (thinking), Claude Opus 5.5 (thinking), Claude Sonnet 4.6 (thinking), Claude Opus 4.6 (thinking), GPT-OSS-120b, and the Nano Banana 2 image model. Availability depends on plan and sign-in path. `agy models` prints the slugs `--model` accepts, and `--effort` or `/effort` picks a reasoning variant. `/model <name>` switches and saves the default (1.1.22), and `/model <name> <prompt>` runs one prompt on another model and returns (1.1.27). A model change during a turn applies after the turn.

The selector label format, seen in `model.id` and `model.display_name`, is a base name plus a parenthesized variant. The 1.1.2 capture listed `Gemini 3.5 Flash (Medium)`, `(High)`, `(Low)`, `Gemini 3.1 Pro (Low)`, `(High)`, `Claude Sonnet 4.6 (Thinking)`, `Claude Opus 4.6 (Thinking)`, and `GPT-OSS 120B (Medium)`. Hook `modelName` uses slug form, for example `gemini-3.6-flash-medium`.

### Quota and credits

Plans grant baseline quota that refreshes on plan-dependent five-hour and weekly windows, with optional AI-credit overage on eligible paid plans ([plans](https://antigravity.google/docs/plans/)). Google says quota is measured by work, not by a fixed prompt or token count. `/usage` (alias `/quota`) refreshes and shows quota. `/credits` shows credit balance and purchase links. The statusline `quota` map carries per-bucket remaining fractions and resets. Credits have no documented machine-readable field outside print-mode `/credits`.

### Cost

The statusline `cost` field (1.1.21 changelog) is an unrounded estimated cost of the current session. Its unit and nested shape are unpublished. Print-mode `usage` reports tokens without dollars. Plan quota and AI credits are not API-token billing.

### Private local service

A running `agy` exposes an undocumented Connect-over-HTTPS service on process-owned loopback ports with a self-signed certificate. Google does not publish it. The historical 1.2.2 binary contains both paths below, and the wire was cross-checked against CodexBar's [`AntigravityStatusProbe`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityStatusProbe.swift), [`AntigravityStatusProbe+PortDetection`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityStatusProbe%2BPortDetection.swift), and [`AntigravityQuotaSummaryParser`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityQuotaSummaryParser.swift).

| RPC (POST) | Body | Returns |
| --- | --- | --- |
| `/exa.language_server_pb.LanguageServerService/GetUserStatus` | `{}` | account email and user tier or plan label |
| `/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary` | `{"forceRefresh":true}` | per-model quota buckets grouped by period |

Both take `Content-Type: application/json` and `Connect-Protocol-Version: 1`. The quota summary can sit at the root, under `response`, or under `summary`, and a bucket's remaining fraction can be direct, nested, or in a oneof wrapper. Period labels include explicit five-hour and weekly periods. The complete label set is unpublished.

## Remote control, MCP, plugins, and custom agents

These surfaces change tool vocabulary, prompts, or executable configuration, so they are indexed here without depth.

| Surface | What ships |
| --- | --- |
| Remote control | `agy remote-control start\|status\|stop` registers a headless daemon, built into the CLI, as a systemd user service (boot), LaunchAgent (login), or Scheduled Task, reachable from a browser. `--name` sets the instance name, stored as `cliRemoteControlHostname` in `~/.gemini/config/config.json`. `--session` scopes it to the login session. Since 1.2.14, Linux without a systemd user manager uses a background process with no automatic restart or boot launch ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1214)). The daemon uses the CLI's sign-in ([remote control](https://antigravity.google/docs/remote-control/), 1.2.0 changelog). |
| Active-session remote control | `--remote-control` or `/remote-control` opens a session-scoped connection. `/remote-control off` or session exit removes it ([1.2.6 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#126)). ADC authentication is unsupported ([1.3.3 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#133)). |
| MCP | servers in the user-level `mcp_config.json` (comments and trailing commas allowed since 1.1.24), managed with `agy mcp` or `/mcp`. Per-server `disabled`, `disabledTools[]`, `enabledTools`, `timeoutSeconds`, `url`, and `authProviderType: "google_credentials"`. plugin-bundled servers are namespaced `<plugin>_<server>` (1.2.2) ([MCP](https://antigravity.google/docs/mcp/)). Since 1.3.3, tools with `_meta: {"antigravity/alwaysLoad": true}` in `tools/list` load directly instead of on-demand discovery ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#133)). |
| Plugins | installed and enabled with `agy plugin`. Enablement lives in `~/.gemini/config/config.json`. A plugin can ship `hooks.json`, skills, and `rules.json`. Since 1.3.2, manifests can live in `.antigravity-plugin/` or `.gemini-plugin/`, `PLUGIN_DATA` names per-plugin storage, and `bin/` tools run by name ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#132)). Root-level `agents.json` declares subagents since 1.3.3 ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#133)). |
| Skills | `SKILL.md` under `.agents/skills/`. `disable-slash-command: true` hides a skill from the `/` menu (1.1.12). `/skills reload` refreshes discovery without restarting since 1.2.4 ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#124)). |
| Custom agents | Markdown `agent.md` with YAML frontmatter (`mainAgent`, `subagent`, `hidden`, `model`, `skills`, `plugins`, `rules`, `agents`, `hooks`, `inheritCustomizations`, `excludeDefaultComponents`, `commandExecutionPolicy`). Since 1.1.25 they inherit ambient skills, rules, and subagents by default |
| SDK | Python SDK that hosts its own agent runtime with lifecycle hooks, streaming, persistence, and structured output ([SDK overview](https://antigravity.google/docs/sdk/overview/)). It does not attach to a running `agy` |

Customization manifests (`skills.json`, `rules.json`, `agents.json`, `plugins.json`) load direct directory children since 1.2.10. Nested items require `include_only` ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1210)). Since 1.2.16, discovery reads each `.agents/` directory between cwd and the project root ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1216)). Since 1.3.1, paths in Markdown agent customizations resolve relative to the agent file ([changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#131)).

## Documentation drift

These disagreements use release notes, 1.3.3 help, and the dated binary observations listed below.

| Surface | Official docs | Other evidence | Rule this page follows |
| --- | --- | --- | --- |
| Agent/model output flags | 1.1.12 changelog adds `--output-format` to `agents` and `models` | 1.3.3 subcommand help lists only help flags | acceptance was not tested in this refresh |
| Reasoning effort | headless docs list `low`, `medium`, `high` | 1.3.3 help also lists `xhigh`, `max` | effort depends on model support ([1.2.11 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1211)) |
| Headless timeout | [headless](https://antigravity.google/docs/cli/headless/) says five minutes | [1.2.6 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#126) changes the default to unlimited | unlimited response wait unless explicitly bounded |
| Structured output | headless docs accept primitive schema names | [1.2.14 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1214) rejects them and requires an object root | object-root JSON schema or existing file |
| Partial-response failure | headless docs describe non-zero exit when no response is produced | [1.2.10 changelog](https://github.com/google-antigravity/antigravity-cli/blob/800852ea61796e9fe53077a2f945e74daacb4be6/CHANGELOG.md#1210) assigns exit 3 even after partial output | use exit status and `AGY_ERROR`, not response presence |
| Plan commands | [CLI reference](https://antigravity.google/docs/cli/reference/) lists `/planning` and `/fast`. The statusline page gives `planning` and `fast` as `execution_mode` examples | 1.1.0 changelog removes both and adds `/plan`. The [modes](https://antigravity.google/docs/cli/modes/) page uses `/plan` and `--mode plan` | use `--mode plan`. Treat `execution_mode` values as unknown strings |
| Working directory | best-practices example passes `--cwd` | 1.3.3 help has no `--cwd` | set the process cwd |
| Transcript basename | hooks page names `transcript.jsonl` | 1.1.2 and 1.2.2 hooks sent `transcript_full.jsonl`. Embedded instructions define both files | expect either basename |
| Statusline `transcript_path` | statusline page: absolute transcript path | the 1.2.2 capture named `~/.gemini/antigravity/brain/<id>/.system_generated/logs/transcript.jsonl`, under the 2.0 app-data root, while the same session's hooks named the CLI root's `transcript_full.jsonl` | take the transcript path from hooks |
| Statusline fields | statusline table lists counts (`artifact_count`, `task_count`) and omits `agent`, `subagents`, `artifacts`, `background_tasks`, `conversation_title`, `cost` | the binary tags all of them. The example script reads `subagents`. Changelog adds `conversation_title` and `cost` | tolerate every field as optional |
| `PostToolUse` input | hooks page lists `toolCall` | embedded hook doc lists only `stepIdx` and `error`. Not captured | treat `toolCall` as optional |
| `PostInvocation` timing | "immediately after each model invocation completes" | embedded hook doc: "after tool calls finish" | not settled without a capture |
| `PreToolUse` decisions | hooks page lists six values and makes `allow` delegate to policy | 1.3.3 embedded schema lists five values without `auto_approve`. Embedded prose describes `allow` as automatic approval | retain the disagreement. Runtime acceptance and policy neutrality need a capture |
| `unsandboxed` rules | permissions page documents them | 1.2.2 changelog deprecates them in favor of `command` | treat as deprecated |
| Plugin location | CLI plugins page shows `~/.gemini/antigravity-cli/plugins/` | 1.0.2 changelog moves plugins to `~/.gemini/config/`. 1.1.11 moves enablement to `config.json` | shared config directory |
| Model catalog | models page lists Gemini 3.8 Flash and Claude 5.5 variants | the statusline payload example and the settings page's `--model="Gemini 3.5 Flash"` example still use Gemini 3.5 Flash | read the model from the payload |

# Antigravity CLI protocol reference

> RimZ's mapping of this surface is [adapter_antigravity.md](../../internals/agents/adapter_antigravity.md). The provider-neutral lifecycle contract is [model.md](../../internals/agents/model.md), accounts and spend are [providers.md](../../internals/agents/providers.md), and the adapter playbook is [agent-adapters.md](../../contributing/agent-adapters.md).

This page records Antigravity CLI (`agy`) protocols for adapter authors. It covers launch, hooks, statusline payloads, transcripts, permissions, headless output, authentication, and adjacent automation surfaces.

## Baseline and evidence

The release baseline is **Antigravity CLI 1.5.14**, tag [`1.5.14`](https://github.com/google-antigravity/antigravity-cli/releases/tag/1.5.14), released 2026-11-21, checked 2026-11-22. The tag resolves to commit `b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a`. Repository links below use that commit. Google does not publish the CLI implementation.

The release delta covers 1.3.4 through 1.5.14. The installed 1.5.14 Linux x64 binary confirms the flags, subcommands, embedded hook documentation, protobuf descriptors, and JSON tags below. Earlier captures remain dated runtime evidence where not re-captured on 1.5.14. Bare version citations refer to the pinned changelog.

Evidence comes from these sources. Binary observations and live captures carry version labels.

| Tier | Evidence | How the page marks it |
| --- | --- | --- |
| Official docs | the `antigravity.google/docs` pages in [Upstream sources](#upstream-sources), a living site without versioned snapshots, read 2026-11-22 | cited by URL |
| Release notes | `CHANGELOG.md` at tag `1.5.14`, which `agy changelog` prints verbatim | "1.5.14 changelog" |
| Installed 1.5.14 binary | `agy --version`, `--help`, and subcommand help, the embedded `/hooks` contract, protobuf descriptors (`exa.hooks_pb`, `ExecutorTerminationReason`), Go JSON struct tags, and binary strings | "1.5.14 help", "1.5.14 embedded hook doc", "1.5.14 binary" |
| Live captures | a 1.1.1 SQLite schema probe, a stock 1.1.2 session (hook payloads, transcripts, subagents), a historical 1.2.2 credential-file observation and untrusted-workspace session (hook payloads and statusline only, no completed tool step), 1.5.12 scratch-home probes (plugin hooks, `settings.json`, `/usage`, statusline stages, transcripts, file-store token), 1.5.13 scratch-home probes (custom agents, skills, effort ceilings, `/compact`, queued-message timing), and 1.5.14 scratch-home probes (permissions, `--settings`, sandbox, `--add-dir`, plugin and inline MCP servers, and on 2026-11-22 statusline `cost`, `/usage` under `GEMINI_API_KEY`, skill roots, Orchestrator ids, child hook fields, `PreToolUse`, background commands, and `SessionStart`), 1.5.15 scratch-home probes (Esc over an unsubmitted draft, and on 2026-11-23 prompt-cache lifetime and background-command continuation), 1.5.16 scratch-home probes on 2026-11-24 (workspace trust; background commands, follow-up turns, `manage_task` kill, `PostToolUse` `stepIdx`, and `/tasks` panel kill), 1.5.17 scratch-home probes on 2026-11-25 (`subagents[]` and `task_count` for native subagents, nested grandchildren, `image-generator`, `/agents` kill, `schedule`, recurring cron, `run_workflow`, and Escape with a running child), and a 52-transcript scan across 1.5.15 and 1.5.16 sessions | "1.1.1 probe", "1.1.2 capture", "1.2.2 capture", "1.5.12 probe", "1.5.13 capture", "1.5.14 capture", "1.5.15 capture", "1.5.16 capture", "1.5.17 capture" |
| Third-party source | CodexBar commit [`b41715f`](https://github.com/steipete/CodexBar/tree/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8) for the private local service | cited by path |

Where two tiers disagree, the claim follows the wire and [Documentation drift](#documentation-drift) records the disagreement.

### Upstream sources

| Surface | Source |
| --- | --- |
| Releases and changelog | [releases](https://github.com/google-antigravity/antigravity-cli/releases), [`CHANGELOG.md`](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/CHANGELOG.md) |
| Product boundary and install | [overview](https://antigravity.google/docs/overview?tab=cli), [authenticate](https://antigravity.google/docs/install?tab=cli), [`README.md`](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/README.md), [`install.sh`](https://antigravity.google/cli/install.sh) |
| Slash commands, keybindings, settings | [CLI commands](https://antigravity.google/docs/cli/reference/), [settings](https://antigravity.google/docs/settings?tab=cli), [execution modes](https://antigravity.google/docs/cli/modes/) |
| Command hooks and tool vocabulary | [hooks](https://antigravity.google/docs/hooks/) |
| Statusline and terminal title | [statusline](https://antigravity.google/docs/cli/statusline/), [terminal title](https://antigravity.google/docs/cli/title/), [`examples/statusline/statusline.sh`](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/examples/statusline/statusline.sh), [`examples/title/title.sh`](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/examples/title/title.sh) |
| Conversations, compaction, side questions | [managing conversations](https://antigravity.google/docs/cli/conversations/), [`/resume`](https://antigravity.google/docs/cli/commands/resume/), [`/compact`](https://antigravity.google/docs/commands/compact/), [`/btw`](https://antigravity.google/docs/commands/btw/) |
| Permissions and sandbox | [permissions](https://antigravity.google/docs/permissions?tab=cli), [sandbox](https://antigravity.google/docs/sandbox?tab=cli) |
| Artifacts | [CLI artifacts](https://antigravity.google/docs/cli/artifacts/), [artifact model](https://antigravity.google/docs/artifacts/) |
| Subagents, swarms, and tasks | [subagents](https://antigravity.google/docs/subagents?tab=cli), [`/agents`](https://antigravity.google/docs/cli/commands/agents/), [swarm](https://antigravity.google/docs/swarm/) |
| Print mode | [headless](https://antigravity.google/docs/cli/headless/), [best practices](https://antigravity.google/docs/cli/best-practices/) |
| Models, plans, quota, credits | [models](https://antigravity.google/docs/models/), [plans](https://antigravity.google/docs/plans/), [`/usage`](https://antigravity.google/docs/cli/commands/usage/), [`/credits`](https://antigravity.google/docs/cli/commands/credits/) |
| Remote control, remote agents, ACP | [remote control](https://antigravity.google/docs/remote-control/), [remote agents](https://antigravity.google/docs/remote-agents/), [ACP](https://antigravity.google/docs/cli/acp/) |
| Plugins, skills, rules, MCP | [plugins](https://antigravity.google/docs/plugins?tab=cli), [build plugins](https://antigravity.google/docs/build-plugins/), [skills](https://antigravity.google/docs/skills/), [customization discovery](https://antigravity.google/docs/customization-discovery/), [MCP](https://antigravity.google/docs/mcp/) |
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

`install.sh` pins no version: it reads the `version` key of a per-platform download manifest, accepts `-d`/`--dir <path>`, and the installed CLI updates itself in the background during ordinary runs. `agy update` updates on demand. `agy install` only configures an existing installation's shell `PATH` and aliases (`--dir`, `--skip-path`, `--skip-aliases`).

### Flags

`agy --help` in 1.5.14 lists these top-level flags. The [CLI commands](https://antigravity.google/docs/cli/reference/) page lists the interactive flags (omitting `--settings`) and the [headless](https://antigravity.google/docs/cli/headless/) page lists the print-mode subset.

| Flag | Meaning (1.5.14 help) |
| --- | --- |
| `--add-dir <path>` | add a workspace directory. Repeatable, default empty list. 1.5.14 capture: with a trusted primary cwd, the added directory's `AGENTS.md`, `.agents/rules` (`trigger: always_on`), and `.agents/skills` descriptions reach the model, and its `.agents/hooks.json` handler runs, with no trust prompt for the directory, which stays out of `trustedWorkspaces` |
| `--agent <name>` | custom agent for the session (`agy agents` lists them). Since 1.3.4, an unknown name exits 1 listing available agents in `-p`, and warns (`Agent Not Found`) before falling back to the default agent in interactive mode (1.3.4 changelog, 1.5.13 capture) |
| `-c`, `--continue` | continue the most recent conversation. See [Resume](#resume) |
| `--conversation <id>` | resume a conversation by ID |
| `--dangerously-skip-permissions` | auto-approve every tool permission request without prompting |
| `--disable-slash-commands` | disable slash-command and skill expansion in print mode |
| `--effort <low\|medium\|high\|xhigh\|max>` | reasoning effort for the session. 1.5.13 capture: `max` is refused on every model, and valid levels depend on the model. See [Models](#models) |
| `-i`, `--prompt-interactive <prompt>` | send an initial prompt interactively and continue the session |
| `--input-format <text\|stream-json>` | print-mode input, default `text`. `stream-json` reads one NDJSON message per stdin line and requires `--output-format stream-json` |
| `--json-schema <schema-or-path>` | enforce structured output. For `stream-json` it applies only to the final result |
| `--log-file <path>` | override the CLI log file path |
| `--mode <accept-edits\|auto\|plan>` | agent execution mode for this session. See [Execution modes](#execution-modes) |
| `--model <name>` | model for the session by slug, name, or label (`agy models` lists them). Since 1.3.4, a base model name without `--effort` selects the model's default thinking level |
| `--new-project` | create a new project for this session |
| `--output-format <text\|json\|stream-json>` | print-mode output, default `text` |
| `-p`, `--print`, `--prompt <prompt>` | run a single prompt non-interactively and print the response. See [Print mode](#print-mode) |
| `--print-timeout <duration>` | print-mode time limit, default `0s` (waits until the turn completes) |
| `--project <id-or-name>` | project ID or project name for this session |
| `--remote-agent` | run this session's conversations on remote agents. `--project` then names a remote project by ID or name, otherwise they open in this machine's default remote project ([1.5.0 changelog](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/CHANGELOG.md#150)). Exits 2 with `GEMINI_API_KEY` (1.5.12) |
| `--remote-control` | create a remote connection for this CLI session at startup. Exits 2 with `GEMINI_API_KEY` (1.5.12) |
| `--sandbox` | run in a sandbox with terminal restrictions enabled. No effect when the OS sandbox is unavailable (1.4.4) |
| `--settings <json-or-path>` | session-only settings, never saved: a JSON object of `settings.json` keys, inline or as a file path. When passed twice, only the last object applies (1.5.14 capture) |

A prompt argument outside `-p` or `-i` is an error: 1.5.14 prints `Prompts are read only from -p/--print, -i/--prompt-interactive, or stdin, so this argument would have been ignored.` and exits. Combining `--print` with `--prompt-interactive` exits with code 2 (1.3.4 changelog). Since 1.1.18 a valueless prompt flag no longer swallows the next flag as its prompt.

No flag sets the working directory. The process cwd is the workspace. No flag replaces or appends the system prompt. Custom agents, rules, skills, and plugins carry instructions.

### Subcommands

| Subcommand | Meaning (1.5.14 help) |
| --- | --- |
| `acp` | start the Agent Client Protocol (ACP) stdio JSON-RPC server ([1.5.0 changelog](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/CHANGELOG.md#150)). Accepts `--stdio` (default), `--login`, `--add-dir`, `--agent`, `--sandbox`, `--dangerously-skip-permissions`, `--process-per-session`, `--log-file`, `--app-data-dir`, `--gemini-dir`, `--include-default-customizations`, `--disable-telemetry`, and `--ide-mcp-port` |
| `agent`, `agents` | list available agents. The 1.1.12 changelog adds `--output-format json\|stream-json`, omitted from 1.5.14 subcommand help |
| `models` | list available models. Same historically documented output flag, omitted from 1.5.14 help |
| `plugin`, `plugins` | `list [--json]` (1.3.4), `import [gemini\|claude]`, `install <target>` (supports `plugin[@marketplace]`, GitHub URL, or local directory), `uninstall <name>`, `enable <name>`, `disable <name>`, `validate [target] [--strict] [--json]` (directory, `plugin.json`, `.zip`, `https://` URL, GitHub URL, or `marketplace.json`), `link <mp> <target>` |
| `mcp` | `add`, `remove`, `list`, `enable`, `disable` over the user-level `mcp_config.json`. `add` takes `--type stdio\|http`, `--env`, `--header` |
| `remote-control` | `start` (`--name`, `--session`), `status`, `stop`. See [Remote control](#remote-control-mcp-plugins-and-custom-agents) |
| `mic-serve` | serve this machine's microphone to a CLI on another host (`--addr`, default `127.0.0.1:4713`) |
| `changelog` | show changelog and release notes |
| `update` | update the CLI |
| `install` | configure environment paths and shell settings (`--dir`, `--skip-path`, `--skip-aliases`) |
| `help <subcommand>` | show help for subcommands |

`mcp add [flags] <name> <commandOrUrl> [args...]` requires flags before `<name>`. `--type` defaults to `stdio`, and `http`/`https` URLs are detected automatically. Aliases are `-t` for `--type`, `-e` for `--env`, and `-H` for `--header`. Use `--` before commands or args starting with `-` (1.5.14 help).

### Execution modes

The [execution modes](https://antigravity.google/docs/cli/modes/) page defines four modes, cycled in the TUI with `Shift+Tab` (`default` -> `accept-edits` -> `auto` -> `plan`, skipping `auto` where unavailable) and selectable at launch with `--mode` or persistently via `agentMode` in `settings.json`:

| Mode | Behavior |
| --- | --- |
| `default` | pauses for inline diff review before creating or modifying a file (`1` accept, `2` reject, `Tab` on reject to instruct, `f` full diff) and asks before terminal commands per `toolPermission` |
| `accept-edits` | auto-approves workspace file creations and edits (`write_to_file`, `replace_file_content`, `multi_replace_file_content`), while commands still follow `toolPermission`. Subagents inherit the mode |
| `auto` | default startup mode since 1.5.7 where available (including `-p` headless runs). Available in 1.3.5+ on macOS and Linux and 1.4.4+ on Windows 11 24H2+ when the terminal sandbox works, for personal accounts and pay-as-you-go `AGY_ADC_AUTH` accounts (1.5.6), not Gemini Enterprise or business accounts. Auto-approves workspace edits, runs commands inside the terminal sandbox, and evaluates commands that leave the sandbox with a reviewer model so safe reversible commands run without prompting. Since 1.5.14, Auto also allows reading files outside the workspace without asking (writes still ask, and explicit `ask`/`deny` rules, protected config files, other projects' conversations, and org policies still apply) |
| `plan` | prepends the `/plan` instruction prefix so the agent outlines before writing code |

Where `auto` mode is unavailable, passing `--mode auto` or saving `"agentMode": "auto"` falls back to `default` mode (with a warning in headless mode). Starting in 1.4.4, when the OS terminal sandbox cannot run on the machine (Windows before 11 24H2, or Linux hosts and containers blocking unprivileged user namespaces), `--sandbox` and `enableTerminalSandbox` have no effect, `proceed-in-sandbox` behaves like `request-review`, and `auto` is hidden.

Explicit `allow`/`ask`/`deny` permission rules, `toolPermission: strict`, and `--dangerously-skip-permissions` (`always-proceed`) govern tool calls across every mode. Since 1.5.7, `toolPermission: strict` overrides both `auto` and `accept-edits` so file edits still ask. Since 1.3.2, `--dangerously-skip-permissions` no longer auto-approves implementation plans in `/plan` mode (`artifactReviewPolicy` controls artifact review). Since 1.5.2, `--dangerously-skip-permissions` blocks commands on the system command denylist, and in `-p` headless runs denies actions that Gemini Enterprise admin policy requires to be reviewed.

`--sandbox` enables terminal containment for the session. Outside `auto` mode, `toolPermission: proceed-in-sandbox` auto-runs sandboxed commands and asks for unsandboxed ones. It has no flag of its own, and `--settings` can carry it. `/config` marks settings overridden by launch flags ([settings](https://antigravity.google/docs/settings?tab=cli)).

1.5.14 capture: under `--sandbox` and the default `toolPermission`, a command saw an `--add-dir` directory readable and writable and an unadded sibling as absent (`No such file or directory`), with no prompt. With `proceed-in-sandbox` and `--mode accept-edits`, commands ran unprompted but contained: `curl` could not resolve a host and a write outside the workspace failed, while a native `write_file` outside the workspace and an MCP call still asked. With `--dangerously-skip-permissions` beside `--sandbox`, with or without `proceed-in-sandbox`, a command wrote outside the workspace and a native write outside it ran without a dialog: the skip flag wins over the sandbox.

1.5.14 capture of the modes (2026-11-22, on Linux, on the host and inside a scoped and a sandboxed RimZ room):

- With no mode flag and no saved `agentMode`, the TUI starts in `auto`. `--mode default` and `--mode ''` also end in `auto`.
- `--settings '{"agentMode":""}'` starts in `default`, which asked before a write and before a command, and is not saved. It wins over `--mode auto` in either argv order.
- A `--conversation` resume takes the mode from its own argv, in both directions.
- `--dangerously-skip-permissions` alone, beside `--mode auto`, and beside `--mode accept-edits` behaved the same: a write, a command, and a network command ran unprompted, with commands outside the terminal sandbox. The 1.5.2 system command denylist was not observed.
- Under `auto`, the terminal sandbox started at every isolation level. A network command inside it failed to resolve its host, and the model retried it with the sandbox bypassed. The retry ran without a prompt, also under `--sandbox`.

## Conversations

Antigravity calls a durable session a **conversation**. Hooks name its ID `conversationId`, the statusline `conversation_id`, and print-mode JSON `conversation_id`. The [hooks](https://antigravity.google/docs/hooks/) page describes `conversationId` as a UUID.

Conversation lists are scoped to the launch directory ([managing conversations](https://antigravity.google/docs/cli/conversations/)). `/resume` opens a picker with a flat or workspace-grouped view (`Ctrl+F`, default from `pickerGrouping`), `f2` rename, and `f4` delete. Deleting a conversation stops and cleans up its running subagents, history, and shared or branched workspaces ([1.4.8 changelog](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/CHANGELOG.md#148)). Since 1.5.9, `/resume <id>` accepts any unique prefix and prints `conversation <id> not found` on an unknown ID. The picker can also import an Antigravity 2.0 conversation by cloning it into the CLI. Since 1.1.21 the CLI titles a conversation when it is created (skipped in `-p` since 1.5.2), and since 1.1.27 the title reaches the statusline as `conversation_title`.

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

`/fork` (alias `/branch`) clones the conversation up to the current turn into a new conversation ID and switches the current TUI to the clone. It does not clone the Git checkout. No launch flag forks a supplied source ID. Since 1.2.2 forks and snapshot reverts skip `.system_generated/subagents` and `.system_generated/worktrees`. Since 1.4.8 forks cancel any inherited `/goal`. Since 1.5.12, for conversations with large artifact directories, `/fork` leaves those files in the original conversation's folder and points the forked agent at them instead of copying (deleting the original deletes those files).

`/rewind` (alias `/undo`) reverts conversation history to an earlier step. Since 1.2.6, stored edits larger than 1 MiB omit the raw diff body and `/rewind` skips them. Since 1.3.1, the rewind picker disables messages cleared to save space. `/clear` (alias `/new`) resets the terminal and active conversation context. A 1.1.2 capture showed the first `PreInvocation` after `/clear` carrying a new `conversationId` while the same `agy` process kept running in the same pane. No page defines the lineage between the old and new IDs for `/clear` or `/fork`. In print mode, `-p "/clear"` and other interactive-only commands fail and name the flag or subcommand that replaces them (1.1.11 changelog).

#### Compaction (`/compact`)

`/compact` summarizes earlier conversation turns into a checkpoint without starting a new conversation ([1.3.5 changelog](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/CHANGELOG.md#135), [`/compact`](https://antigravity.google/docs/commands/compact/)). Bare `/compact` writes the checkpoint and stops without running a model turn. `/compact <message>` uses `<message>` to guide summarization and then answers `<message>` as an ordinary turn in the same execution. Only a leading `/compact` triggers compaction. Compaction works in `-p` headless mode unless `--disable-slash-commands` is set, retains tool declarations for prompt-cache reuse (1.4.4), and keeps attached images, PDFs, and files (1.4.6).

1.5.13 capture: bare `/compact` brackets compaction with `PreInvocation` (`invocationNum: 0`, `lastUserInput: "/compact"`) and `Stop` (`terminationReason: "TERMINAL_CUSTOM_HOOK"`, `lastUserInput: "/compact"`). Pressing `Esc` while compaction runs cancels it, prints `Compaction cancelled. Conversation was not compacted.`, and fires only the opening `PreInvocation` with no closing `Stop`. Running bare `/compact` before any completed agent response prints `Nothing to compact yet.` and fires no hook. Across compaction, the transcript remains append-only: a `USER_INPUT` row with `/compact`, followed by a `SYSTEM` / `CHECKPOINT` row carrying the summarizing call's token counts with no `model` field. The statusline keeps its pre-compaction context reading until the next turn's first model call returns (1.5.16 capture). Automatic compaction near the context limit is documented upstream, with no hook observed for it.

1.5.16 capture: resuming a conversation whose background command died, with a bare `/compact` or a prompt as first input, fires `PreInvocation` with `initialNumSteps` one past that user row; the server-restart `SYSTEM_MESSAGE` row is absent at that hook and lands at that step, before the `CHECKPOINT` or `PLANNER_RESPONSE` row.

#### Interrupt and queued messages

Pressing `Esc` interrupts an active turn, fires no lifecycle hook, and moves any message queued behind the running turn back into the composer. Text typed but not submitted stays in the composer, unsent (1.5.15 capture). Since 1.5.14, pressing `Esc` during pre-send checks cancels the send (`Message not sent: canceled before it was sent`), and pressing `Esc` during turn teardown keeps the `Interrupted` hint visible and queues immediate follow-ups until cancellation completes. Under `queuedMessages: "queue"` (the default), a message typed while a turn runs does not fire an `invocationNum: 0` `PreInvocation` until `agy` dequeues it (1.5.13 probes). `Alt+M` sends one message using the opposite of `queuedMessages` (1.4.0). `queuedMessages: "send-immediately"` adds the message to the current turn after the current step rather than interrupting it (1.5.13 changelog).

1.5.14 capture (2026-11-22, Queue mode, two messages each typed with `Enter` then sent with `Alt+M` during a running turn): a message sent this way is taken in by the next model invocation, which writes its `USER_INPUT` row at the `step_index` equal to that invocation's `initialNumSteps`, at invocation numbers 2 and 5 in the capture. The row is absent at that invocation's `PreInvocation` and present at its `PostInvocation`, and every later invocation begins past it. A `PostToolUse` of the running command can show the row and carry it as `lastUserInput` about 0.5 s before that `PostInvocation`, but carries no step bound. The `invocationNum: 0` `PreInvocation` that continues the turn after a background command carries the latest such message as `lastUserInput`. A turn's first prompt sits below its invocation's `initialNumSteps` (step 0 against 1). `transcriptPath` names `transcript_full.jsonl`.

1.5.15 capture (2026-11-23, Queue mode, `--mode auto`, the transcript copied inside each `invocationNum: 0` `PreInvocation`): where the row that opens an execution lands against that opener's `initialNumSteps`. A fresh prompt (bound 1) and a message typed with `Enter` during a running turn and drained when it ended (bound 5) each wrote their `USER_EXPLICIT` / `USER_INPUT` row at the step just below the bound, already on disk when the hook ran; the drained opener carried the drained text as `lastUserInput`. A background command's result after a steer (bound 8) and a native subagent's result waking its parent (bound 4) wrote no user row: the step just below the bound was the `PLANNER_RESPONSE` that ended the previous execution, on disk at the hook, and the `SYSTEM` / `SYSTEM_MESSAGE` carrying the result landed at the bound itself, not yet on disk. Both continuations carried the older prompt as `lastUserInput`, the steer in the background case. A native child's first opener carried bound 0 and an empty `lastUserInput`, and its transcript file did not exist yet.

1.5.17 capture: an `Alt+M` read together with the `Enter` before it is lost and the message stays queued; the same `Alt+M` pressed later sends it.

1.5.17 capture (2026-11-25, Queue mode, messages sent with `Alt+M`): a message sent while `agy` summarizes a folded `/compact <message>` inside a turn, from 1 s in to 0.2 s before the `CHECKPOINT`, or 0.7 s after it, waits until the compaction's invocation is over. The rows run `/compact <message>` at that invocation's `initialNumSteps`, `CHECKPOINT`, `PLANNER_RESPONSE`, its tool result, then the waiting message at the next invocation's `initialNumSteps`; two such messages landed there together. A message sent while a bare `/compact` summarizes (which closes with a `TERMINAL_CUSTOM_HOOK` `Stop`) opens an `invocationNum: 0` invocation with its row at `initialNumSteps`, right after the `CHECKPOINT`. A message sent behind `/compact <message>` while the running step still holds both is taken in with it by one invocation, which writes both user rows and no `CHECKPOINT`: that `/compact` reaches the model as text and nothing is compacted.

## Command hooks

The [hooks](https://antigravity.google/docs/hooks/) page and the 1.5.14 embedded hook doc define six events: `SessionStart`, `PreToolUse`, `PostToolUse`, `PreInvocation`, `PostInvocation`, and `Stop`. Each handler receives one camelCase (protojson) JSON object on stdin and returns one JSON object on stdout. Handlers run synchronously and block the loop.

Since 1.3.3, `/btw` side questions do not run user or workspace hooks. Since 1.2.4, customization token-budget truncation does not drop `hooks.json` configurations. Since 1.5.8, `hooks.json` files with a UTF-8 BOM load normally. Since 1.5.14, timing out or canceling a command hook on Linux and macOS terminates its entire process group.

### Files and format

| File | Scope |
| --- | --- |
| `~/.gemini/config/hooks.json` | global, shared with Antigravity 2.0 |
| `~/.gemini/antigravity-cli/settings.json` | not loaded: the [hooks](https://antigravity.google/docs/hooks/) and [settings](https://antigravity.google/docs/settings?tab=cli) pages explicitly note that the CLI preserves a `hooks` key in `settings.json` without running it |
| `<workspace>/.agents/hooks.json` | workspace. Loads once the folder is trusted (or via `--add-dir` from a trusted primary cwd: 1.5.14 capture) |
| `hooks.json` inside a plugin | plugin. A disabled plugin's hooks do not run (1.1.7 changelog). 1.5.12 probe: `~/.gemini/config/plugins/<dir>/hooks.json` loads with no install step, enabled by default |

1.5.12 probe (scratch `HOME` under `env -i`): a plugin's `hooks.json` takes the same `{"<hook name>": {"<Event>": [handler]}}` shape as the global file and fires for every event. Hook cwd is the plugin directory for plugin hooks and `~/.gemini/config` for the global file, never the workspace. The same hook name in the global file and a plugin fires twice (`loaded 2 named hooks from 2 hooks.json file(s)`). With the plugin written and the global entry removed during a live session, the next turn's hooks fired once each, from the plugin. `agy` rewrote nothing inside the plugin directory across three starts. `agy plugin validate <dir> --json` accepts a `plugin.json` of `name`, `version`, `description`, and a string `author`, reports `"hooks": 1`, and raises one note-severity finding for a missing logo.

Since 1.5.3, command hooks inside a plugin receive `PLUGIN_ROOT` (the plugin directory) and, for installed plugins, `PLUGIN_DATA` (persistent plugin data directory) as environment variables. They are not expanded inside the `command` string before shell invocation, and `agy plugin validate` warns (failing only with `--strict`) when a hook command references `${PLUGIN_ROOT}` or `$PLUGIN_ROOT` instead of a relative path (1.5.6 changelog, 1.5.14 embedded hook doc).

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
| `command` | handler | required. Run through `sh -c` on Unix and `cmd /c` on Windows (double-quoted commands fixed in 1.5.8), with `~` expanded and the cwd set to the directory holding `hooks.json` (1.5.14 embedded hook doc) |
| `timeout` | handler | optional seconds, default `30` |

The matcher documents `run_command|view_file` and `browser_.*` as examples. Tool names are step types lowercased with the `CORTEX_STEP_TYPE_` prefix removed (1.5.14 embedded hook doc), so browser tool names exist but are not catalogued.

### Common input

| Field | Meaning ([hooks](https://antigravity.google/docs/hooks/), 1.5.14 embedded hook doc) |
| --- | --- |
| `conversationId` | active conversation ID |
| `workspacePaths` | absolute workspace directories |
| `transcriptPath` | absolute path of the conversation transcript. Documented as `<app_data_dir>/brain/<conversationId>/.system_generated/logs/transcript.jsonl` |
| `artifactDirectoryPath` | absolute path of the conversation's artifact directory, `<app_data_dir>/brain/<conversationId>` |
| `modelName` | model handling the invocation, for example `gemini-3.6-flash-medium` (custom models fixed to report their real name in 1.5.3) |
| `lastUserInput` | most recent user prompt in the conversation ([1.4.7 changelog](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/CHANGELOG.md#147), living hooks docs) |
| `hookEventName` | event name as written in `hooks.json`, for example `PreToolUse` ([1.5.4 changelog](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/CHANGELOG.md#154), 1.5.14 embedded hook doc, omitted from living web page) |
| `cwd` | agent's working directory where commands run by default, distinct from the hook process's cwd (1.5.4 changelog, 1.5.14 embedded hook doc, omitted from living web page) |

The 1.2.2 capture sent `workspacePaths: []` from a workspace that was not yet trusted. `<app_data_dir>` is `~/.gemini/antigravity-cli` for the CLI, `~/.gemini/antigravity` for 2.0, and `~/.gemini/antigravity-ide` for the IDE. See [Transcripts](#transcripts) for captured basename differences and path selection.

### Events

| Event | Fires | Event input | Output |
| --- | --- | --- | --- |
| `SessionStart` | once when a new conversation session starts, before the first model invocation | common input only | optional `injectSteps[]` |
| `PreToolUse` | before a tool executes | `toolCall` (`id` since 1.5.4, `name`, `args`, plus `toolAction` and `toolSummary` observed inside args in 1.2.2, and only `toolSummary` in a 1.5.14 capture of `run_command`), `stepIdx` (0-based) | required `decision`. Optional `reason`, `permissionOverrides[]`, `overwrite` |
| `PostToolUse` | after a tool completes, tool steps only (1.1.9) | `toolCall` (`id` matching `PreToolUse` since 1.5.4, `name`, `args`), `stepIdx`, optional `error` string, empty on success | `{}` |
| `PreInvocation` | before each model call | `invocationNum` (0 for the first call of an execution), `initialNumSteps` | optional `injectSteps[]`, `terminationBehavior` (`"terminate"` or `""`, added in 1.4.11) |
| `PostInvocation` | after each model invocation completes (web page) or after tool calls finish (1.5.14 embedded doc) | same as `PreInvocation` | optional `injectSteps[]`, `terminationBehavior` (`"force_continue"`, `"terminate"`, or `""`) |
| `Stop` | when the execution loop terminates | `executionNum`, `terminationReason`, optional `error`, required `fullyIdle`, optional `continuationActive` (boolean, added in 1.5.6) | required `decision`. Optional `reason` |

1.5.14 capture (2026-11-22): `SessionStart` fires before the first `PreInvocation` of a fresh `-i` session, as in the 1.2.2 capture. After `/clear` it fires with the new `conversationId` at the next prompt, before that prompt's `PreInvocation`, not when `/clear` runs. For a native subagent and a nested one, it fires with the child's `conversationId` after the parent's `invoke_subagent` `PostToolUse` and before the child's first `PreInvocation`. A `--conversation` resume fires none, at startup or on its first turn. The payload carries the common input only, with no `source` field.

Both the living web page and the 1.5.14 embedded hook doc now list `toolCall` (`id`, `name`, `args`) on `PostToolUse`. Since 1.4.3, a `PostToolUse` hook that fails to run (missing command, non-zero exit, timeout, or invalid JSON) is logged and no longer marks the completed tool call as failed.

In the 1.5.14 embedded hook doc and protobuf enum (`ExecutorTerminationReason` with the `EXECUTOR_TERMINATION_REASON_` prefix stripped), `Stop.terminationReason` takes uppercase values: `NO_TOOL_CALL`, `MAX_INVOCATIONS`, `MAX_FORCED_INVOCATIONS`, `MAX_TOKEN_BUDGET_EXCEEDED`, `ERROR`, `USER_CANCELED`, `TERMINAL_STEP_TYPE`, `TERMINAL_CUSTOM_HOOK`, and `INJECTED_RESPONSE` (the 1.5.13 `/compact` capture observed `"TERMINAL_CUSTOM_HOOK"`). The living web page still shows lowercase examples (`model_stop`, `max_steps_exceeded`, `error`). `fullyIdle = false` means background commands or asynchronous tasks are still running while the foreground loop stops. `continuationActive = true` means the agent is executing work requested by an earlier `Stop` hook in the same turn, resetting to `false` on the next user message (1.5.6 changelog, 1.5.14 embedded hook doc).

Since 1.1.10, `hooks.json` hooks run before the built-in termination checks, so `PostInvocation` observes the final invocation of a turn and `Stop` hooks always run. The 1.1.9 changelog describes a cap on consecutive `continue` decisions without naming the setting or default.

### Decisions

The [hooks](https://antigravity.google/docs/hooks/) page documents six `PreToolUse.decision` values. The 1.5.14 binary wires `hook_auto_approve` (and the 1.5.7 changelog fixes `auto_approve` against admin-enforced review), yet the 1.5.14 embedded jsonschema still lists only five values (omitting `auto_approve`) and the 1.5.14 embedded hook doc lists only four (`allow`, `deny`, `ask`, `force_ask`).

| Value | Meaning in the living hooks docs |
| --- | --- |
| `allow` | delegate to the next hook or default policy |
| `auto_approve` | approve when default policy would prompt, while honoring explicit user rules and organization admin-enforced review (1.5.7 changelog). Absent from the 1.5.14 embedded jsonschema and embedded hook doc |
| `deny` | block the tool |
| `ask` | prompt, honoring an Always Allow grant |
| `force_ask` | prompt regardless of cached grants |
| `deny_unless_prior_grant` | block unless the resource was approved by an earlier user grant |

`permissionOverrides` is an array of permission resources such as `command(npm test)` that override default tool permissions. `overwrite` is an object shallow-merged into the tool call's arguments before it runs. The tool result then tells the agent which keys a hook rewrote.

The 1.5.14 embedded hook doc still describes `allow` as automatic approval, unlike the living page. 1.5.14 capture (2026-11-22, one `run_command` under `--settings '{"agentMode":""}'`): a `PreToolUse` handler answering `{"decision":"allow"}` left the `Run this command?` prompt in place, and statusline `tool_confirmation_pending` turned `true` about half a second after the hook. Declining that prompt fired no `PostToolUse`, `PostInvocation`, or `Stop`. File-edit prompts were not captured.

The 1.0.16 changelog says an empty decision string no longer errors. It does not define equivalence between `{}`, `{"decision":""}`, empty stdout, non-zero exit, malformed JSON, or handler timeout.

An injected step (`injectSteps[]`) carries one of:

- `systemMessage` (object): persists across turns, for example `{"systemMessage": "Remember to lint", "metadata": {"source": "policy"}}`. Setting `metadata.user_markdown` renders inline Markdown to the user that the model never reads (leave `systemMessage` `""` to send the model nothing: 1.5.14 embedded hook doc).
- `userMessage` (string): injected user message.
- `errorMessage` (object, added in 1.4.11): `{"userMessage": "Prompt blocked by policy."}` displays an error to the user without sending it to the model, marks the turn errored with a non-zero exit in `-p`, and stops execution only when paired with `"terminationBehavior": "terminate"`.
- `ephemeralMessage` (string): deprecated in the 1.5.14 embedded hook doc (converted to a hidden system message).
- `toolCall` (`{name, args}`): documented in earlier hooks docs.

`PreInvocation` accepts `"terminationBehavior": "terminate"` to stop before calling the model (1.4.11). `PostInvocation` accepts `"force_continue"`, `"terminate"`, or `""`.

`Stop.decision = "continue"` blocks the stop, re-enters the loop, and injects `reason` as a system message. Any other value, including `""`, allows the stop.

### Undocumented hook fields in the 1.5.14 binary

These proto3 fields exist in `exa.hooks_pb` inside the 1.5.14 binary. Neither the web hooks page nor the embedded hook doc documents them, and no capture establishes whether they serialize or carry populated values on command hooks, except `parentConversationId` and `rootConversationId`: a 1.5.14 capture (2026-11-22) of a root, a native subagent, and a nested subagent found neither on any of the five events its plugin registered (`SessionStart`, `PreInvocation`, `PostInvocation`, `Stop`, and `PostToolUse`).

| Message | Undocumented fields (1.5.14 binary) |
| --- | --- |
| `HookArgsCommon` | `agentName`, `executionId`, `isBattleMode`, `parentConversationId`, `rootConversationId`, `callerContext`, `trajectorySource` |
| `PreToolHookResult` | `allowTool`, `denyReason` |
| `PostInvocationHookArgs` | `modelOutput`, `modelThinking` |
| `PostToolHookArgs` | `result` |
| `PostToolHookResult` | `overwriteResult` |
| `StopHookArgs` | `finalModelOutput` |
| `HookInjectedStep` | `checkpoint` (`sessionSummary`) |
| `HookHandlerConfig` | `prompt` (`prompt`, `model`) beside `command`, while the embedded doc says only `command` is supported |

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

The table is not exhaustive. Browser tools (`browser_*`), MCP tools (`call_mcp_tool` for lazy tools and `mcp_<server>_<tool>` for eager tools, plus `CORTEX_STEP_TYPE_MCP_TOOL`), `run_workflow` (added for everyone in 1.5.9, running Python workflow scripts via bundled Python), and tools the changelog names without arguments (`manage_inbox` in 1.1.13, `read_resource` in 1.1.16) also exist.

`find_by_name`, `grep_search`, and `list_dir` left the default agent and subagent toolset in 1.2.7. Custom agents can still list them explicitly in `tools` (1.2.7 changelog). Since 1.2.16, image requests run through the built-in `image-generator` subagent. The 1.5.13 custom-agent capture showed an agentless session declaring `ask_question`, `define_subagent`, `edit_file`, `invoke_subagent`, `manage_subagents`, `manage_task`, `read_file`, `run_command`, `run_workflow`, `schedule`, `search_web`, `send_message`, `swarm`, and `write_file`. A 1.5.14 capture of a catch-all `PostToolUse` matcher (2026-11-22) received the newer names in `toolCall.name`: `read_file`, `write_file`, `edit_file`, `run_command`, `search_web`, `call_mcp_tool` (a call to a user MCP server's tool), `run_workflow`, and `invoke_subagent`. It received none of the legacy step names above (`view_file`, `write_to_file`, `replace_file_content`), and no eager `mcp_<server>_<tool>` name.

## Statusline and terminal title

The custom statusline runs a command through `sh -c` (`cmd /c` on Windows) whenever agent state changes, pipes a snake_case JSON payload to its stdin, and renders its stdout with ANSI color and multi-line support. It is configured in `~/.gemini/antigravity-cli/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "~/.gemini/antigravity-cli/statusline.sh"
  }
}
```

| Key | Meaning ([statusline](https://antigravity.google/docs/cli/statusline/), [settings](https://antigravity.google/docs/settings?tab=cli)) |
| --- | --- |
| `type` | optional, default `command`, the only supported value |
| `command` | the shell command to run (leading `~` expands) |
| `padding` | integer, default `0`: blank lines above the status line (`statusLine` only) |
| `enabled` | boolean, default `true`. `false` suspends the script while keeping the command |
| `stack_with_default` | boolean, default `false`: `true` renders the script below the built-in line instead of replacing it (`statusLine` only) |

Set `statusLine` and `title` to objects, not booleans: the CLI reads a boolean as `{"enabled": true|false}` with no `command`, so no script runs. The CLI runs the statusline command at most once every 300 ms, kills runs exceeding 5 seconds, and disables the script for the rest of the session after 30 consecutive failures (`⚠ Statusline disabled after 30 consecutive failures.`, re-enabled with `/statusline enable`). Since 1.4.7, headless `-p` runs do not start custom statusline or title commands. Since 1.5.8, transient status notices (copy confirmations, save errors, permission reload notices) briefly replace the custom status line without shifting the layout.

1.5.12 probe (scratch `HOME`): `agy` rewrites `settings.json` at start. A `statusLine` holding `type`, `command`, `stack_with_default`, and two keys outside this table (`_rimz_managed`, `_rimz_wrapped`) kept only the first three after one start, so a tool cannot mark its entry with an extra key.

The terminal-title command (`title` in `settings.json`, [terminal title](https://antigravity.google/docs/cli/title/)) receives the same payload while title customization is active and strips ANSI and non-printable output.

Since 1.4.0, the CLI also emits OSC 7501 terminal status sequences (working, waiting for input, done, or failed) after probing terminal support. Since 1.5.10, OSC 7501 keeps reporting working while the agent waits on non-daemon background subagents or tasks. `AGY_CLI_OSC7501=off|on` and `AGY_CLI_OSC7501_MESSAGES=brief` control it.

### Payload

| Field | Shape and meaning | Source |
| --- | --- | --- |
| `cwd` | launch directory | docs |
| `session_id` | backward-compatibility alias of `conversation_id` | docs |
| `conversation_id` | current conversation ID (empty before the first starts) | docs |
| `conversation_title` | current conversation title (omitted until set) | docs, 1.1.27 changelog |
| `transcript_path` | absolute transcript path, optional (file may not exist yet). See [Documentation drift](#documentation-drift) | docs |
| `model` | `{id, display_name, effort}` (`null` until selected). The docs example and the 1.1.2 and 1.2.2 captures put the selector label, such as `Gemini 4 Argon (Medium)`, in both `id` and `display_name`. `effort` (`low` through `max`, only for models with multiple effort levels) is now documented | docs, 1.2.2 capture |
| `workspace` | `{current_dir, project_dir}` | docs |
| `version` | CLI version | docs |
| `context_window` | `{total_input_tokens, total_output_tokens, context_window_size, used_percentage, remaining_percentage, current_usage}`. `context_window_size` is `0` if unknown, and `current_usage` is `null` before the first model response | docs |
| `exceeds_200k_tokens` | `true` once context exceeds 200k tokens, `null` before the first model response | docs |
| `agent` | `{name}` of the active custom agent (omitted for the default agent) | docs, 1.5.14 binary |
| `product` | application name, `antigravity` | docs |
| `quota` | map from bucket ID to `{remaining_fraction?, remaining_amount?, reset_time, reset_in_seconds, disabled?}`, optional. A 1.5.12 capture held `gemini-5h`, `gemini-weekly`, `3p-5h`, and `3p-weekly`, with no `window` field | docs, 1.5.14 binary, 1.5.12 capture |
| `agent_state` | `idle`, `working`, `tool_use`, `reviewing`, `authenticating`, `initializing`, or `error` (example `title.sh` still checks `thinking`). See [Agent state in 1.5.15](#agent-state-in-1515) | docs, 1.5.15 capture |
| `vcs` | docs list `{type}` (`git`, `jj`, or `hg`, omitted outside a repo). Example `statusline.sh` also reads `branch` and `dirty`, and 1.5.14 binary retains `branch`, `dirty`, and `client` tags | docs, example script, 1.5.14 binary |
| `cycle_mode` | mode selected with `Shift+Tab`: `accept-edits`, `auto`, or `plan` (omitted in `default`) | docs, 1.5.14 binary |
| `sandbox` | `{enabled}` (1.5.14 binary also retains `allow_network`) | docs, 1.5.14 binary |
| `subagents` | array of running and finished subagents in the root conversation's whole tree (nested grandchildren and `image-generator` included), each `{name, role, status, model?}` (`status` is `running` or `completed`, `model` added in 1.5.3). In 1.5.17 captures: finished entries stay listed, order changes between payloads, and entries carry no id; a child reads `running` while thinking with no tool, while parked on its own command, and after an Escape on the parent turn until the child stops cleanly; killing a child in `/agents` flips its entry to `completed` with `agent_state: "idle"`, `task_count` absent, and no hook or parent turn; and the array lags a spawn (absent up to 0.9 s after `invoke_subagent`'s `PostToolUse`, and in one run `completed` for about 1 s before `running`, all while `agent_state` read `working`, while every first idle payload after a spawn read `running`, with the smallest spawn-to-idle gap 2.98 s). Because agy sends no further payload after the last one (one natural-finish park emitted its only `idle` payload 0.92 s after `Stop` with nothing after), RimZ applies no delay margin to a complete rest report | docs, 1.5.3 changelog, 1.5.14 binary, 1.5.17 capture |
| `artifact_count` | artifacts produced in the conversation (omitted when 0) | docs |
| `task_count` | running background tasks, including timers, recurring jobs, workflows, and other background tools since 1.3.1 (omitted when 0). Does not count native subagents. A 1.5.15 capture read `1` on every payload while a turn was parked on background work and omitted it after an Escape with no background work. In 1.5.17 captures: `task_count` stayed `1` while a one-shot `schedule` timer was pending, before, at, and between ticks of a recurring cron job (absent only after the last tick's turn began), during a `run_workflow` sleep (including before its wake), and between a background command's `PostToolUse` and the wake turn's `PreInvocation` (3 of 3); it was absent throughout while native subagents ran (including thinking children, children parked on their own command, nested grandchildren, `image-generator`, and after killing a parent command in `/tasks` while a child still ran) | docs, 1.3.1 changelog, 1.5.15 capture, 1.5.17 capture |
| `pending_input_count` | queued user messages (omitted when 0) | docs |
| `tool_confirmation_pending` | `true` while a tool-confirmation dialog is showing (omitted otherwise) | docs |
| `plan_tier` | subscription tier, optional | docs |
| `email` | account email or LDAP identity | docs |
| `terminal_width` | live terminal width | docs |
| `vim` | `{mode}`: `NORMAL`, `INSERT`, `VISUAL`, or `VISUAL LINE`. Present only in Vim editor mode | docs |
| `cost` | `{total_usd, subagent_usd, estimated}`: estimated USD spend based on list prices, not billed amounts (omitted until reported). Absent from every payload of a 1.5.14 capture on a Google AI Ultra sign-in. See [Cost](#cost) | docs, 1.5.14 binary, 1.5.14 capture |
| `artifacts`, `background_tasks` | arrays beside the two count fields, shape unpublished. No 1.5.15 payload carried `background_tasks` | 1.5.14 binary, 1.5.15 capture |

Most fields are `omitempty`, so omission alone does not establish a measured zero. The living docs example:

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
  },
  "cycle_mode": "plan",
  "cost": {
    "total_usd": 0.4213,
    "subagent_usd": 0,
    "estimated": false
  }
}
```

No source says whether `total_*` token fields count the current window or the whole session, or how often `current_usage` refreshes. The full set of `quota` bucket IDs is unpublished.

A 1.5.12 capture (2026-11-19, no model turn) fills the payload in stages within one session: the first three payloads carry neither `quota` nor `plan_tier`, the next two carry `quota` only, and later ones carry both, with `email` present from the second. While `agent_state` is `initializing`, and until the first model turn, `context_window.current_usage` is `null`.

#### Agent state in 1.5.15

Live probes on agy 1.5.15 (2026-11-23) emitted a payload only when a field changed, with no periodic refresh, and read `agent_state` as follows:

- Escape on a no-tool answer: `working` to `idle` 70, 75, 84, and 118 ms after the key (four tries), with no hook of any kind after it. Escape during a foreground `run_command`: `idle` 263 ms after the key, with no `PostToolUse`, `PostInvocation`, or `Stop`.
- A permission prompt reads `tool_use` with `tool_confirmation_pending: true`, and a native `ask_question` on screen reads `tool_use` for its whole duration. Neither read `idle`.
- A tool-call turn reads `working` across `PostInvocation` and `PreInvocation` boundaries, and `idle` comes about 200 ms after `Stop`. With a `Stop` hook that slept 2 s, the first `idle` came 323 to 474 ms after the hook returned (three of three).
- A bare `/compact` reads `working` until its `Stop`, and `idle` about 300 ms after.
- A turn parked on background work sends `Stop` with `fullyIdle: false`, then `idle` about 220 ms later with `task_count: 1` while the command runs. The only payload between the finished task's `PostToolUse` and the next `PreInvocation` read `idle` with `task_count: 1` (three of three).
- `idle` also arrives between a prompt's Enter and its invocation-0 `PreInvocation`, up to about 3 s before the hook in one trial.

## Transcripts and local state

### Transcripts

Use the hook-provided `transcriptPath`. Statusline docs and historical captures point at the 2.0 app-data root, as detailed in [Documentation drift](#documentation-drift).

Two JSONL transcripts live under `<app_data_dir>/brain/<conversationId>/.system_generated/logs/`. Embedded instructions distinguish them: `transcript.jsonl` is compact and truncates long fields, listing them in `truncated_fields`, while `transcript_full.jsonl` is never truncated. Each line is one step:

| Field | Meaning (embedded instructions, 1.1.2 capture, 1.5.12 transcripts) |
| --- | --- |
| `step_index` | step index in the trajectory |
| `source` | `USER_EXPLICIT`, `MODEL`, `SYSTEM`, and others |
| `type` | step type, including `USER_INPUT`, `PLANNER_RESPONSE`, `INVOKE_SUBAGENT`, `CONVERSATION_HISTORY`, `CHECKPOINT` |
| `status` | `DONE`, `ERROR`, and others |
| `created_at` | RFC 3339 timestamp |
| `content` | optional text |
| `thinking` | model reasoning on `PLANNER_RESPONSE` steps |
| `tool_calls` | optional array of tool calls with arguments |
| `model` | effort-suffixed model slug on completed `PLANNER_RESPONSE` steps, such as `gemini-4-neon-high` (1.5.12 transcripts) |
| `input_tokens`, `cache_read_tokens`, `output_tokens` | per-step token counts on completed `PLANNER_RESPONSE` and compaction `CHECKPOINT` rows (1.5.12 transcripts, 1.5.13 capture) |
| `truncated_fields` | compact file only: the fields cut on this line |

In 1.5.12 local transcripts (12 conversations read 2026-11-18 and 2026-11-19), each completed `MODEL` / `PLANNER_RESPONSE` step carries `model` in slug form with its effort suffix (`gemini-4-neon-high`, `gemini-3.8-flash-high`), `input_tokens`, `cache_read_tokens`, and `output_tokens`. `input_tokens` excludes cache reads, and one print-mode turn whose `usage` reported 31 thinking tokens wrote an `output_tokens` equal to its reported output, so `output_tokens` includes thinking. No file repeated a `step_index`, `transcript.jsonl` repeats the same token rows, and a context reset appends new rows without rewriting earlier ones. In 1.5.13 `/compact` captures, compaction appends a `USER_INPUT` row with `/compact` and a `SYSTEM` / `CHECKPOINT` row carrying the summarizing call's token counts with no `model` field.

In a 1.5.16 capture (2026-11-24, print mode `--output-format stream-json` across foreground `run_command`, backgrounded `run_command`, and `invoke_subagent`, plus the child conversation transcript), every `created_at` timestamp was a whole-second UTC floor matching `floor(stream_DONE - duration_seconds)` across all 14 unbuffered timed steps. A `MODEL` / `PLANNER_RESPONSE` row's `created_at` marks when the step was created and the model request was sent (not first token and not response end), while the immediately following `MODEL` / `GENERIC` row's `created_at` marks when the model response finished and the tool step was dispatched (not when the tool completed). Both `DONE` followers (foreground `run_command`, `invoke_subagent`, and child `send_message`) and permanent `RUNNING` followers (backgrounded `run_command`) were stamped at response end. On 1.5.16, `invoke_subagent` wrote `MODEL` / `GENERIC` / `DONE` in `transcript_full.jsonl` rather than the 1.1.2 capture's `INVOKE_SUBAGENT`, while `stream-json` still reported `step_type: "subagent"`, `tool_name: "invoke_subagent"`, and `subagent_info`. Snapshots across `--conversation` resumes were byte-for-byte prefixes of one another; tool calls waiting on user approval or `ask_question` were not captured.

No page publishes the full enums, an append guarantee, a locking contract, retention, or what `/rewind` does to either file. The 1.1.13 changelog describes transcript rewriting during compaction, so an offset-only reader cannot assume immutable prior content across all versions. Since 1.2.4, `transcript.jsonl` includes terminal `ERROR` steps (including failed tool executions and timeouts).

The 1.1.2 capture observed these record shapes:

- A text turn is `USER_EXPLICIT` / `USER_INPUT` / `DONE` with the user's text, then `MODEL` / `PLANNER_RESPONSE` / `DONE` with the reply, around `SYSTEM` `CONVERSATION_HISTORY` and `CHECKPOINT` records. User content can wrap the request in `<USER_REQUEST>...</USER_REQUEST>` followed by `<ADDITIONAL_METADATA>` and settings blocks.
- A question turn adds an `ask_question` entry to a completed `MODEL` / `PLANNER_RESPONSE` record's `tool_calls`. `transcript_full.jsonl` carries `args.questions` as a JSON array. `transcript.jsonl` carries the same array JSON-encoded as a string.
- A subagent turn adds the `invoke_subagent` request and `INVOKE_SUBAGENT` result described in [Subagents](#subagents-and-background-tasks).

### Conversation databases

The CLI stores conversations in SQLite (1.0.4 changelog), scans `.db` and `.db-wal` on `/resume` (1.0.5), checkpoints the WAL on exit (1.1.26), and lazily reloads older steps from disk in long sessions (1.5.10). A 1.1.1 probe found one database per conversation at `~/.gemini/antigravity-cli/conversations/<conversation-id>.db` and a shared `~/.gemini/antigravity-cli/conversation_summaries.db`, both at SQLite `user_version = 1`. Google publishes no schema. The tables below are that probe's.

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
| `settings.json` | CLI preferences (`0600`, sparse, UTF-8 BOM tolerated since 1.5.8). See [Settings and permissions](#settings-and-permissions) |
| `keybindings.json` | action -> key-sequence map (`[]` disables an action, except protected `cli.exit`, `cli.escape`, `cli.enter`). Delete to restore defaults |
| `cache/last_conversations.json` | absolute workspace -> latest conversation ID |
| `cache/projects.json` | workspace -> project mapping |
| `brain/<conversationId>/` | artifacts, `scratch/`, and `.system_generated/` logs and subagent metadata |
| `mcp/<server>/` | cached MCP tool schemas (`instructions.md` and `<tool>.json` per tool) |
| `worktrees/` | subagent Git worktrees since 1.2.10 |
| `updater/` | updater lock and timestamp state |
| `cli.log` | default CLI log (`--log-file` overrides) |

Shared customization lives in `~/.gemini/config/`: `hooks.json`, `config.json` (plugin enablement, remote-control hostname, `userSettings.enableAdc`), `mcp_config.json`, `plugins/`, `skills/`, `rules/`, `agents/`, and project-specific configuration under `projects/`. Since 1.5.5, `/teamwork` keeps per-run state under the CLI app-data directory rather than `.agents/teamwork/`.

## Subagents and background tasks

The agent drives subagents through `define_subagent`, `invoke_subagent`, `send_message`, and `manage_subagents`. Built-in subagents are `research`, `browser`, `self`, and `image-generator` ([subagents](https://antigravity.google/docs/subagents?tab=cli)). Subagents run asynchronously and can nest up to 10 levels deep. The `/agents` panel shows each one's identifier, role, model (1.5.3), status (`running`, `done`, `killed`, `error`), and current step. `K` kills, `A` and `D` approve or deny inline, and `Enter` opens the detail view ([`/agents`](https://antigravity.google/docs/cli/commands/agents/)). Globally, `Ctrl+K` fast-approves the pending subagent action or opens a subagent's question picker (1.3.6), and `Alt+J` jumps to the next subagent that needs approval. Descendants' tool confirmations and `ask_question` prompts relay to the root conversation (1.3.6). Approving a permission for the conversation applies across the main conversation and all its subagents (1.4.6). Custom agents declare subagents in Markdown frontmatter (`agents`) and choose a model tier with `model` (`inherit`, `flash`, or `pro`, default `inherit`).

A child is an ordinary conversation, and the 1.1.2 two-child capture showed how it links to its parent:

- Every child receives the ordinary command hooks with its own stable `conversationId`, its first workspace path, and its own transcript path. Those captured child hooks carried no parent ID. The 1.5.14 binary's `HookArgsCommon` carries `parentConversationId` and `rootConversationId`, and a 1.5.14 capture (2026-11-22) of a child and a nested child found neither on the wire (see [Undocumented hook fields](#undocumented-hook-fields-in-the-1514-binary)). In that capture a parent's `Stop` while its child ran carried `fullyIdle: false`, and the child's result woke the parent with a new execution (`PreInvocation` `invocationNum: 0`).
- The parent transcript holds a completed `MODEL` / `PLANNER_RESPONSE` record whose `invoke_subagent` `args.Subagents[]` entries carry `Prompt`, `Role`, `TypeName`, and optional `Workspace` in request order. Nested children that inherit the workspace use the literal `inherit`.
- The next completed `MODEL` / `INVOKE_SUBAGENT` record's `content` holds consecutive JSON objects inside prose, one per child in request order. Each carries `conversationId`, a `file:` `logAbsoluteUri` naming the child's transcript under `brain/<conversationId>`, and optional `workspaceUris`.
- The parent transcript flushed late: both children's `PreInvocation` and `Stop` hooks ran before the request and result records reached disk, and the records flushed before the parent's own `Stop`.
- `manage_subagents` with `Action=list` returned the same child IDs after completion, with no lifecycle state.

Print mode publishes the same relation directly: a `stream-json` `step_update` for an `invoke_subagent` step carries `subagent_info.subagents[]` with `type_name`, `role`, `conversation_id`, `log_uri`, and `workspace_uris` ([headless](https://antigravity.google/docs/cli/headless/)).

`invoke_subagent` supports three workspace modes: `inherit` (parent's workspace), `branch` (isolated Git worktree per repository in a multi-repo workspace, including staged, unstaged, and untracked changes since 1.5.5, or an isolated folder copy up to 100,000 entries and 4 GiB outside VCS), and `share` (shared directory storage). Subagent compaction preserves the parent's 10 most recent messages and parent ID (1.4.10), and parent compaction keeps each subagent's type, role, and result (1.4.3). Stopping a child clears its pending approvals immediately (1.3.3), and stopping a subagent tree stops every descendant and the tasks they own (1.1.10).

`@<subagent> <message>` sends a message directly to a child conversation, with autocomplete for running and completed children (1.2.9). Since 1.3.1, subagent failures appear as errors until the child makes further progress.

Beyond single subagents, the CLI ships three multi-agent orchestrators and a workflow tool: `/boost` (iterative coder/investigator pipeline on paid plans), `/swarm` (preview added in 1.5.5, with `/swarm board` / `Alt+S` and an `Overview` pane in 1.5.14, and agents no longer self-start swarms without `/swarm` since 1.5.13), `/teamwork` (multi-milestone team on paid plans), and `run_workflow` (added for everyone in 1.5.9, running a Python script via bundled Python to fan out subagents).

Background shell work starts through `run_command` when the command outruns `WaitMsBeforeAsync` (0 of 91 saved background starts across 52 saved 1.5.15 and 1.5.16 transcripts set `RunPersistent`), and is managed with `manage_task` and the `/tasks` panel (which also lists timers, recurring cron jobs, and other background tools since 1.3.1). Recurring intra-day cron schedules add a deterministic per-schedule jitter offset (1.4.5). The statusline counts background work in `task_count`, and `Stop.fullyIdle` is `false` while it runs, including at the end of later user turns started while an earlier turn's command still runs.

1.5.14 capture (2026-11-22, `--mode auto`, a `run_command` of `sleep 30 && echo SLEPT` the model was told not to wait for): the turn ran `PreInvocation` and `PostInvocation` twice with no `PostToolUse`, then `Stop` with `NO_TOOL_CALL` and `fullyIdle: false`, and the statusline showed `task_count: 1`. The transcript held the `run_command` request, a `MODEL` / `GENERIC` row with status `RUNNING` (`Tool is running as a background task with task id: <conversationId>/task-<n>`), and a no-tool `PLANNER_RESPONSE` `DONE`. When the command ended 30 s later, `PostToolUse` for the `run_command` fired, then a new execution (`PreInvocation` `invocationNum: 0`, `PostInvocation`, `Stop` with `fullyIdle: true`), and `task_count` was omitted again. The transcript appended two `SYSTEM` / `SYSTEM_MESSAGE` rows (the task result, then a system reminder) and a `PLANNER_RESPONSE`, with no `USER_INPUT`. The `GENERIC` row keeps its `RUNNING` status.

1.5.15 capture (2026-11-23), 1.5.16 captures (2026-11-24), and 52 saved 1.5.15 and 1.5.16 transcripts (91 background starts): the `MODEL` / `GENERIC` `RUNNING` start row carries `Task Description:` (matching a preceding `run_command` `CommandLine` in all 91 saved starts) and `Task logs are available at:`, and its task id is `<conversationId>/task-<step_index>` (matching the start row's `step_index` in all 91 saved starts). In the 1.5.15 and 1.5.16 live captures, a follow-up user turn started while a command still ran ended with `Stop` carrying `fullyIdle: false`, and normal completion fired `PostToolUse` for `run_command` with `stepIdx` equal to the start row's `step_index` before the transcript appended the `SYSTEM` / `SYSTEM_MESSAGE` finish row (`content=Task id "<id>" finished with result:`). In the 1.5.16 live captures, killing a task through `manage_task` fired `PostToolUse` for `manage_task` with `toolCall.args.Action: "kill"` and `toolCall.args.TaskId`, with no `PostToolUse` for the killed `run_command`, followed by a `SYSTEM` / `SYSTEM_MESSAGE` row carrying `content=Task id "<id>" was canceled with result:`. Across the 52 saved transcripts, all 89 finish and cancel rows carry `[Message]` `content=Task id "<id>"` (83 `finished with result:`, 6 `was canceled with result:`), and a server restart writes one `SYSTEM` / `SYSTEM_MESSAGE` row with `content=[Notice] All your subagents and background tasks have been stopped due to server restart` and no per-task finish row. In a separate 1.5.16 capture (2026-11-24, isolated scratch home, one running background `sleep 600`), pressing `k` in `/tasks` fired no hook and appended no transcript row across a 33 s wait, emitted a statusline payload 780 ms after `k` with `agent_state: "idle"` and `task_count` absent (whereas every payload while the command ran carried `task_count: 1`), and wrote the `SYSTEM` / `SYSTEM_MESSAGE` cancel row (`content=Task id "<id>" was canceled with result: Tool execution was canceled`) only when the next user prompt arrived.

## Human waits

The CLI exposes three interactive wait types:

| Wait | Signal | Native answer |
| --- | --- | --- |
| tool permission | a prompt card (`Run this command?`, `Allow access to this URL?`, `Allow calling this tool?`, `Accept this file edit?`, `Allow creation of this file?`, with a `Reason:` line when a hook or cross-project path caused it). Statusline `tool_confirmation_pending`. The preceding `PreToolUse` carries the proposed call | `y`/`n` or numbered choices. File, URL, and MCP targets can be edited to widen the grant before allowing (not terminal commands). `Esc` denies file creation but does not dismiss a pending file edit |
| question | `ask_question` with `questions[]` of options and `is_multi_select` (subagent questions relay to root via `<agent> has a question` since 1.3.6) | the question dialog (`Space` or `x` toggles multi-select, and `Ctrl+K` or `a` in subagent view opens a subagent's picker) |
| artifact review | implementation plans, diffs, and media held for review per `artifactReviewPolicy` | the Artifact Review panel (`Ctrl+R`): approve, reject, or comment |

The cited hook and headless contracts define no API for answering these interactive waits. They publish no wire format for `ask_question` answers. The artifact `status` and `type` enums are unpublished ([artifact model](https://antigravity.google/docs/artifacts/)).

Print mode has no one to answer: a tool needing unobtainable approval is soft-denied with a stderr notice and listed in `denied_actions`, the agent settles main-agent questions itself (1.1.12) and skips subagent questions (1.3.6), and `--dangerously-skip-permissions` no longer stalls on subagent file permissions (1.5.2) while still denying actions that Gemini Enterprise admin policy requires to be reviewed (1.5.2). Plan review behavior in print mode after the 1.3.2 plan-approval change remains unverified.

## Print mode

`-p` runs one prompt and exits. Diagnostics, authentication prompts, progress, and permission notices go to stderr. Stdout carries only the response or the event stream ([headless](https://antigravity.google/docs/cli/headless/)). Print mode uses cached credentials and fails with an authentication error rather than blocking when no terminal is attached. Since 1.5.7, headless runs without a saved `agentMode` start in `auto` mode where available and `default` otherwise (pass `--mode` to pin). Print mode honors `settings.json` policies, `--mode`, `--model`, `--effort`, `--settings`, expands slash commands and skills (including `/compact`) unless `--disable-slash-commands` is set, starts no custom statusline or title commands (1.4.7), and skips background title generation (1.5.2).

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
| `status` | `SUCCESS` (including runs that succeeded after model retries since 1.5.13), `ERROR`, `CANCELED`, `INTERRUPTED` (for example SIGINT), `INVALID`, `WAITING` (ended waiting on input), `RUNNING` (no terminal state) |
| `finished_reason` | why a turn ended early: `print_timeout`, `interrupted`, or `error`. Omitted on normal completion ([1.4.11 changelog](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/CHANGELOG.md#1411), 1.5.14 binary, omitted from living headless docs) |
| `response` | free-text response |
| `error` | error message, only on failure |
| `duration_seconds` | wall-clock duration |
| `num_turns` | user turns in the conversation |
| `structured_output`, `json_schema` | parsed output and enforced schema under `--json-schema` (follow-up turns on resumed `--conversation` runs keep schema reminders since 1.5.8) |
| `usage` | `input_tokens`, `output_tokens`, `thinking_tokens`, `cache_read_tokens`, `total_tokens` |
| `denied_actions` | actions soft-denied during the run (1.1.27 changelog and 1.5.14 binary tag, omitted from headless docs) |

No field reports dollars.

`--json-schema` requires a JSON schema string or existing schema file with root `"type": "object"`. Since 1.2.14, primitive names, plain text, missing files, and non-object roots fail at startup with exit 1. The headless docs still advertise primitive names.

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

A successful run exits 0. An unknown `--model` or unknown `--agent` (1.3.4) exits 1 with an error envelope listing available options. Combining `--print` with `--prompt-interactive` exits 2 (1.3.4). A `PreInvocation` hook injecting `errorMessage` with `"terminationBehavior": "terminate"` exits non-zero (1.4.11). Tool errors and soft-denied permissions inside a run do not change the exit code (1.1.20).

Since 1.2.6, an agent or model API failure exits 3 and writes `AGY_ERROR: {...}` to stderr, reporting canonical status, HTTP or gRPC error code, retryability, and error ID. Since 1.2.10, this also applies after a partial response (JSON error output preserves that partial response, while multi-turn `stream-json` sessions warn and continue). Exact JSON keys inside `AGY_ERROR` are not published by the changelog (`short_error` appears in the 1.5.14 binary).

`--print-timeout` accepts durations such as `15m`. The default `0s` waits until the turn and its background tasks complete (1.5.0 removed the 1.2.9 30-minute background-task cap, since 1.2.16 headless runs wait for the agent to respond to background work finishing after its first turn, daemon processes terminate when the headless run ends, and stuck shutdown tasks are capped at 30 seconds since 1.5.5). Since 1.5.0, when `--print-timeout` cuts a turn short, the CLI exits with code 3, prints an `AGY_ERROR` line naming terminated tasks, writes `[agy] print timeout after ...` to stderr, and sets `finished_reason: "print_timeout"` (1.4.11, 1.5.0 changelogs). An interrupt such as `Ctrl+C` sets `finished_reason: "interrupted"` and exits non-zero, with no numeric code established here.

This reference does not establish numeric exit codes for authentication failure outside the `/usage` probe below, or for a stream-input session containing a recovered model error.

Read-only slash commands answer without an agent turn, quota, or conversation: `-p "/usage"`, `/quota`, `/credits`, `/model`, `/effort`, `/skills` (1.1.11), and `/permissions`, `/hooks`, `/help`, `/changelog`, `/config` (1.1.12). Text output is one tab-separated record per line. Since 1.5.11, `/usage`, `/quota`, and `/credits` exit with code 2 when connected with `GEMINI_API_KEY`. 1.5.14 capture (2026-11-22, a fake key): with `modelProvider: "gemini"`, signed in or not, `-p /usage --output-format json` exits 2 with `{"status":"ERROR","response":"","error":"/usage failed: not available with a Gemini API key; see https://aistudio.google.com/usage for the key's usage","finished_reason":"error",...}` on stdout and the same message after `error: ` on stderr. Without `modelProvider`, a signed-in home ignores the key and prints its quota, and a signed-out home exits 1 with `"error":"authentication failed or timed out"`, as with no key. Since 1.2.15, terminal `/usage` shows reset times in local time while piped and JSON output retain UTC.

1.5.12 probes (scratch `HOME` under `env -i`): `agy -p /usage --output-format json` answers in 1.8 to 2.8 s with `num_turns: 0` and zero tokens, fires no hook, runs no statusline, and leaves no conversation. Stdout is one object: `{"status": "SUCCESS", "num_turns": 0, "command": {"name": "usage", "data": {"groups": [{"name", "buckets": [{"id", "window", "remaining_fraction", "reset_time"}]}]}}, ...}`. Observed bucket ids are `gemini-5h`, `gemini-weekly`, `3p-5h`, and `3p-weekly`. `window` is `5h` or `weekly`, and `reset_time` is RFC 3339 UTC. Buckets may also carry `disabled` or `remaining_amount` (1.5.14 binary).

1.5.12 probes, logged out: the same command prints a sign-in URL on stderr and waits 60 s whatever `--print-timeout` says, and calls `xdg-open` whatever `DISPLAY`, `WAYLAND_DISPLAY`, `BROWSER`, or `CI` say. Any one of `SSH_TTY`, `SSH_CONNECTION`, or `SSH_CLIENT` in its environment selects the URL-and-code flow, which opens no browser, and a newline on stdin answers the code prompt: it then exits 1 within a second with stdout `{"status":"ERROR","error":"authentication failed or timed out",...}`. Logged in, the variable and the newline change nothing. Offline (`unshare -rn`) and logged in, it exits 1 after 7.3 s with `status: "ERROR"` and an `error` beginning `/usage failed: retrieving quota summary`, and the login survives.

## Settings and permissions

`~/.gemini/antigravity-cli/settings.json` is sparse (`0600`): it stores only non-default values, normalizes at startup, preserves unknown top-level keys (such as `"$schema": "https://antigravity.google/schemas/v1/settings.json"`), and tolerates a UTF-8 BOM (1.5.8). When the file has invalid JSON or an unrecognized value, the CLI leaves it untouched and opens a Settings Error screen offering exit, session defaults, or backup-and-reset to `settings.json.corrupted.<timestamp>` ([settings](https://antigravity.google/docs/settings?tab=cli)).

| Key | Values and default ([settings](https://antigravity.google/docs/settings?tab=cli), [CLI commands](https://antigravity.google/docs/cli/reference/), 1.5.14 changelog) |
| --- | --- |
| `agentMode` | `"auto"` (default since 1.5.7 where available), `""` (`default`), `"accept-edits"`, `"plan"` |
| `toolPermission` | `"request-review"` (default), `"proceed-in-sandbox"`, `"strict"`, `"always-proceed"` |
| `artifactReviewPolicy` | `"asks-for-review"` (default), `"agent-decides"`, `"always-proceed"` |
| `permissions` | `{allow[], deny[], ask[]}` of permission resources |
| `enableTerminalSandbox` | boolean, default `false`. Auto mode turns the sandbox on automatically |
| `allowNonWorkspaceAccess` | boolean, default `false` |
| `sandboxProxy` | `{http: {address, certFile, username?, password?}}` custom HTTP proxy for sandboxed commands |
| `altScreenMode` | `"default"` (inline conversation, alt-screen overlays), `"always"` (`full screen (altscreen)`), `"never"` (inline only, `settings.json` only) |
| `statusLine`, `title` | command objects. See [Statusline](#statusline-and-terminal-title) |
| `notifications` | boolean, default `false`: desktop notification and terminal bell when a task completes or needs attention |
| `model` | string, default `""`: saved `/model` selection by label |
| `modelProvider` | `"gemini"` routes to the Gemini API with `GEMINI_API_KEY` |
| `customModelsConfig` | extra models for `modelProvider: "gemini"` (`customModels[]` with `modelName`, `label`, `modelFeatures` fixed in 1.5.13) |
| `orchestrator` | consumer-plan Orchestrator `Planner`, `Executor`, and effort selections ([1.5.14 changelog](https://github.com/google-antigravity/antigravity-cli/blob/b9f5bf40b9e9e66f417f7bc6e8c0b22a079bc51a/CHANGELOG.md#1514)) |
| `gcp` | `{project, location}` saved by Google Cloud sign-in |
| `pickerGrouping` | `"none"` (default) or `"grouped"` (`/resume` default view) |
| `queuedMessages` | `"queue"` (default) or `"send-immediately"` (joins the current turn after the current step, 1.5.13 changelog) |
| `verbosity` | `"high"`, `"medium"` (default), `"low"` |
| `showThinking` | boolean, default `false` (`/thinking [on\|off]`, 1.5.14 changelog) |
| `hideActiveItems` | boolean, default `false` (hides active subagents and background tasks below prompt) |
| `copyOnSelect` | boolean, default `true`: copy mouse selections in alt-screen mode |
| `useG1Credits` | boolean, default `false`: spend AI credits once plan quota is exhausted (hidden on business accounts and `GEMINI_API_KEY`) |
| `clearScrollbackOnResize`, `disableSlashCommands`, `historySize`, `trustedWorkspaces`, `useTerminalCursor` | `settings.json`-only preferences (`historySize` default `2000`, `-1` unlimited, `0` clears; `trustedWorkspaces` in [Workspace trust](#workspace-trust)) |
| `colorScheme`, `runningLightSpeed`, `editor`, `editorMode`, `vimInsertFirst`, `showTips`, `showFeedbackSurvey` | presentation and editor preferences |

Telemetry (`Enable Telemetry` in `/config`) is saved with the account rather than `settings.json`, and is forced off for Business and `AGY_ADC_AUTH` accounts (1.4.1 changelog).

Managed organization admin settings also govern the CLI: `pluginControls` (`pluginsDisabled`, `marketplaceDisabled`, 1.5.9), `gateway.routeWebFetch` and `featureControls.remoteControlEnabled` (1.5.8), and `models.list[].supportsThoughtCirculation` (1.5.6).

### Workspace trust

On first launch in a folder, agy asks `Do you trust the contents of this project?` and saves an accepted folder to `trustedWorkspaces`, an array of absolute paths in `settings.json`. A 1.5.16 capture (isolated home, `modelProvider: "gemini"`, a placeholder `GEMINI_API_KEY`, onboarding done, no model turn) showed that only an entry naming the launch directory exactly skips the prompt:

| `trustedWorkspaces` | Launch directory | Prompt |
| --- | --- | --- |
| none | a linked worktree `repo-worktrees/wt1` | shown |
| `repo-worktrees/wt1` | `repo-worktrees/wt1` | skipped |
| the parent `repo-worktrees` | `repo-worktrees/wt1` | shown |
| the main checkout `repo` | its subdirectory `repo/sub` | shown |
| the main checkout `repo` | its linked worktree `repo-worktrees/wt1` | shown |

A `trustedWorkspaces` list naming the launch directory in `--settings`, inline or as a file, also skipped the prompt, and `settings.json` kept its saved list unchanged: `--settings` trust lasts for the session only. Whether agy compares the path as written or resolved through symlinks was not captured.

### Permission resources

A permission resource is `action(target)` ([permissions](https://antigravity.google/docs/permissions?tab=cli)). Precedence is Deny > Ask > Allow, and `*` as the target matches the whole action. The CLI ignores rules it cannot parse.

| Action | Target | Default |
| --- | --- | --- |
| `read_file` | absolute or workspace-relative path, recursive | Ask, auto-allowed in the workspace (and outside the workspace in `auto` mode since 1.5.14) |
| `write_file` | same. Implies `read_file` on the target | Ask, auto-allowed in the workspace |
| `read_url` | hostname, covering subdomains. Also populates the sandbox's outbound network allowlist | Ask |
| `execute_url` | hostname, for browser actuation. Isolates `localhost:port` and sensitive Google subdomains (`mail.google.com`, `docs.google.com`, `drive.google.com`, `accounts.google.com`, `console.cloud.google.com` since 1.4.9) | Ask |
| `download_url` | hostname, for Antigravity App browser downloads (same subdomain and port rules as `execute_url`) | Ask |
| `command` | word-by-word prefix, or `regex:<pattern>` (each whitespace-separated token anchored as `^(?:pattern)$`). Covers execution inside and outside the sandbox | Ask (auto-allowed in sandbox under `auto` or `proceed-in-sandbox`) |
| `unsandboxed` | command prefix or `regex:` pattern allowed outside the sandbox | Ask, business and enterprise accounts only (other accounts ignore with a warning and use `command`) |
| `mcp` | `server/tool`, `server/*`, or `*` | Ask |

Prefix matching on `command(...)` is disabled whenever a command contains shell constructs that can hide arbitrary execution: `$(...)`, backticks, `<(...)`, `$((...))`, `{a,b}`, non-literal command names, network or fd redirects, subcommand-executing flags (`git -c`, `tar --to-command`), or commands run through Windows `cmd`, `powershell.exe`, or `pwsh.exe` (1.5.8 changelog, [permissions](https://antigravity.google/docs/permissions?tab=cli)). Such commands require an exact full-line or full-line `regex:` match. Standard pipelines, `&&`/`||`/`;` chains, quoted literals, plain `$VAR`, simple file redirects, and transparent wrappers (`timeout`, `nohup`, `nice`, `env`) still prefix-match. On Windows, UNC, device, and NT object paths are rejected across permission rules, `@` mentions, plugins, `sidecars.json`, and custom agents (1.5.14).

1.5.14 capture, under `--settings` in the TUI:

- `--settings` given twice keeps only the last object, even when the two set different keys, so every override belongs in one object.
- An allow rule removes the prompt: under `--mode accept-edits`, `command(echo)` ran `echo hi` unprompted while `uname -s` asked. The default mode already auto-runs commands it judges safe.
- `command(P)` matches P word by word with any trailing arguments (`command(git status)` covers `git status --short` and not `git log -1`), and `command(*)` matches every command. A compound line or a pipe asks for the whole line unless every part is covered. None of `command(git *)`, `command(git status:*)`, `command(git status *)` matched `git status`.
- A `read_file` or `write_file` target that names a directory, absolute or workspace-relative, covers its descendants. `/**` and `/*.md` match only path components with those literal names, `~/` does not expand, and `//abs` does not match `/abs`. `read_file(*)`, `write_file(*)`, and `mcp(*)` each remove the prompt for the whole action, and `write_file(*)` also admits a native read.
- A file allow rule also opens its path to the terminal sandbox: a sandboxed command read and wrote a directory named only by `read_file` and `write_file` rules, while an uncovered sibling stayed absent, and with `write_file(*)` it wrote outside the workspace. File rules do not approve a shell command.
- A rule holds on `--conversation` resume only when `--settings` passes it again.
- A `--settings` `permissions.allow` replaces the account's saved global allow list (`~/.gemini/antigravity-cli/settings.json`) for that session, while the saved global deny list stays listed and in force. A project-scope deny rule (`~/.gemini/config/projects/<project>.json`) also stays in force, though `-p /permissions` does not list it. A `--settings` without a `permissions` key leaves both saved lists alone.
- A permission rule matches an inline agent-file MCP server by its raw name: `mcp(<name>/*)` removed the prompt for a server the agent file named `<name>`, with no plugin or agent prefix.
- `read_url` was not exercised: the probing model reported no fetch tool and made no call.

1.5.16 capture (2026-11-24, TUI under `--settings` with `"allowNonWorkspaceAccess": false`, editing an existing file outside any Git repo and outside `/tmp`, reached through an in-workspace symlink):

- A `write_file(<real folder>)` rule in `--settings` silences the "outside workspace" prompt for a native edit reached through an in-workspace symlink, in both `--mode auto` and default mode (`"agentMode": ""`), and a rule on an ancestor directory covers descendants. In default mode the rule also removes the mode's own ask-before-write for that folder (the allow rule outranks the preset).
- agy matches `write_file` targets on the real (resolved) path only: a rule naming only the symlink path still prompts, with the prompt naming the real path.
- `"allowNonWorkspaceAccess": false` in `--settings` overrides a saved `"allowNonWorkspaceAccess": true` in `settings.json`, while omitting the key from `--settings` leaves the saved `true` in effect.
- Under `--mode auto`, editing an existing file under `/tmp` succeeded with no prompt and no allow rule (confirming the temp-tree exemption).

1.5.16 capture (2026-11-24, TUI under `--mode auto` with `"enableTerminalSandbox": true` and `"allowNonWorkspaceAccess": false`, stub `rimz` writing under a scratch home outside the workspace and `/tmp` and checking `~/.ssh` and `unshare -Ur`):

- A `command(<prefix>)` rule in `--settings` `permissions.allow` removes the reviewer-model permission prompt (`Run this command?` / `tool_confirmation_pending: true`) on both the first and second turn of a session, for two-word and three-word prefixes (`command(rimz agents)`, `command(rimz pane list)`, and all 39 `ROUTINE_RIMZ_PREFIXES` entries spelled as written). No-rule controls prompted on the same commands, and uncovered commands (`rimz pane send`, `rimz config list`) still prompted with the rules present.
- With the matching rule, a call that requests sandbox bypass (`BypassSandbox: true`) runs outside the terminal sandbox (writing outside the workspace and `/tmp`, reading `~/.ssh`, and running `unshare -Ur`) with no prompt. The rule does not exclude default (`BypassSandbox: false`) calls from the sandbox: on unhinted turns agy ran the first attempt inside the sandbox, observed it fail, and retried with `BypassSandbox: true`, which ran unprompted outside the sandbox.
- Not captured: bare prefixes with no trailing argument, shell constructs that disable prefix matching, modes other than `auto`, and what agy's "Persist to settings.json" option saves when a session allow list is active.

Denying `read_file` on a path also denies `write_file` there. The system temp directory is readable and writable by default. `always-proceed` also auto-approves MCP calls and page reads. A pattern approved for the conversation holds across the main conversation and all its subagents (1.4.6). Project configuration under `~/.gemini/config/projects/` takes precedence over global CLI settings.

The terminal sandbox ([sandbox](https://antigravity.google/docs/sandbox?tab=cli)) uses Linux namespaces, macOS `sandbox-exec`, and Windows 11 24H2+ (August 2026 update+) AppContainer process security environments. By default it blocks sensitive paths including `~/.ssh`, `.env`, macOS `~/Library/Application Support` (1.3.3), `~/.docker`, `~/.kube`, `~/_netrc` (1.4.6), `~/.config` (`XDG_CONFIG_HOME`), Windows `AppData\Roaming`, and `~/.git-credentials` (1.5.10), while keeping common developer tool settings readable. Explicit rules can grant access.

## Authentication

The CLI signs in with Google in a browser and stores the session in the OS keyring (Apple Keychain, Secret Service over D-Bus, Windows Credential Manager). SSH sessions use a URL-and-code flow ([authenticate](https://antigravity.google/docs/install?tab=cli)). On Linux without a D-Bus session bus, for an hour after a keyring timeout, after a macOS keychain write failure (1.5.9), or when a token exceeds OS keychain size limits on macOS or Windows (1.4.6, 1.4.7, 1.5.4), the CLI stores credentials in files. A historical 1.2.2 installation had an `antigravity-oauth-token` file in the app-data root, and on a 1.5.12 file-store installation it is JSON with a top-level `id_token_email` equal to the statusline `email`. `/logout` removes the stored session. `agy acp --login` runs interactive terminal Google sign-in and exits (1.5.0).

Other sign-in paths exist: Business sign-in with a Google Cloud project choosing either a Gemini Enterprise Subscription or Pay As You Go billing (1.4.11), Workforce Identity Federation (BYOID), Application Default Credentials (`AGY_ADC_AUTH=true`, where pay-as-you-go accounts use personal permission settings including Auto mode since 1.5.6), and `GEMINI_API_KEY` with `modelProvider: "gemini"` (for which `/logout` has nothing to clear). No credential file format or machine-readable auth-status command is documented.

Since 1.2.5, definitive sign-in rejection clears stale tokens while temporary network failures, server errors, and rate limits retain them. Expired or revoked sign-in reports `Authentication expired. Please log back in.` (1.3.3).

| Environment variable | Effect |
| --- | --- |
| `GEMINI_API_KEY` | Gemini API credential, with `modelProvider: "gemini"` |
| `GOOGLE_GEMINI_BASE_URL` | custom Gemini API endpoint |
| `AGY_ADC_AUTH` | Application Default Credentials sign-in (`true`, optionally with `GOOGLE_CLOUD_QUOTA_PROJECT` and `GOOGLE_CLOUD_LOCATION`). Unsupported by Remote Control (1.3.3) |
| `AGY_LLM_GATEWAY_MODELS` | model list for enterprise model gateway configurations (1.5.6). Remote gateway URLs require `https://` (1.5.5) |
| `AGY_CLI_OSC7501`, `AGY_CLI_OSC7501_MESSAGES` | control OSC 7501 terminal status reporting (`off` or `on`, and `brief` to omit prompt/command details: 1.4.0) |
| `AGY_CLI_MAC_OPTION_AS_ALT` | set to `0` on macOS to revert 1.5.2's default Option-as-Alt shortcuts |
| `ANTIGRAVITY_DISABLE_EMBEDDED_RIPGREP` | set to `true` to skip extracting bundled `ripgrep` and use `PATH` or built-in search (1.5.5) |
| `AGY_CLI_HIDE_ACCOUNT_INFO` | hides email and plan tier from the header (not documented to remove them from the statusline payload) |
| `CLI_GRAPHICS` | `kitty` forces image mode, including multiplexer setups with image passthrough (1.2.12) |
| `AGY_CLI_HIDE_LOGO` | hides the banner art |
| `AGY_CLI_DISABLE_ESCAPE_SEQUENCE_OPTIMIZATIONS` | disables renderer diffing |
| `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` | respected by `agy plugin` CLI output since 1.4.3 (and `LC_TERMINAL=iTerm2` enables 24-bit color over SSH since 1.3.5) |

## Models, quota, and cost

### Models

Antigravity routes several model families ([models](https://antigravity.google/docs/models/), [headless](https://antigravity.google/docs/cli/headless/)). Availability depends on plan and sign-in path:

| Model family | Base slug / ID | Effort levels | Notes |
| --- | --- | --- | --- |
| Gemini 4 Neon | `gemini-4-neon` | `low`, `medium`, `high`, `xhigh` | default for new conversations on eligible plans, up to 512K context. Available on `GEMINI_API_KEY` (1.5.12) and `AGY_ADC_AUTH` / enterprise gateway (1.5.13) |
| Gemini 4 Carbon | `gemini-4-carbon` | `low`, `medium`, `high` | Ultra and Enterprise, `AGY_ADC_AUTH` (1.5.5), `GEMINI_API_KEY` (1.5.8) |
| Gemini 4 Argon | `gemini-4-argon` | `low`, `medium`, `high` | Pro, Ultra, Enterprise, `GEMINI_API_KEY` (1.4.3) |
| Gemini 4 Hydrogen | `gemini-4-hydrogen` | `low`, `medium`, `high`, `xhigh` | Pro (paid), Ultra, Enterprise, `GEMINI_API_KEY` / `AGY_ADC_AUTH` / gateway (1.5.8) |
| Orchestrator | `gemini-pro-orchestrator-neon`, `gemini-pro-orchestrator` (Carbon), `gemini-pro-orchestrator-argon` | fixed (`--effort` and `/effort` do not apply) | pairs a planner model with an executor model in one turn. 1.5.14 capture (2026-11-22): each of the three ids, and the label `Orchestrator (Neon)`, launches with statusline `model` `Orchestrator (Neon)` (or `Carbon`, `Argon`) and no `effort` key. `orchestrator`, `orchestrator-neon`, and `gemini-pro-orchestrator-carbon` are not recognised. With `--effort` (`high` or `max`), `-p` exits 1 with `--effort is not supported for model "<id>"`, and the TUI warns and runs `Gemini 4 Neon (High)`. On consumer plans in local sessions, `/model` configures both `Planner` and `Executor` models and efforts under `orchestrator` in `settings.json` / `--settings` (1.5.14). On Enterprise and `AGY_ADC_AUTH`, runs Carbon (High) + Hydrogen (High) (1.5.10) |
| Gemini 3.8 / 3.7 / 3.6 Flash, Gemini 3.1 Pro | `gemini-3.8-flash`, `gemini-3.7-flash`, `gemini-3.6-flash`, `gemini-3.1-pro` | `low`, `medium`, `high` (`low`, `high` on 3.1 Pro) | 3.7 Flash, 3.6 Flash, and 3.1 Pro are Enterprise-only on the living models page |
| Claude Sonnet 5.5, Claude Opus 5.5 (thinking) | `claude-*` | `low`, `medium`, `high` (1.5.13 capture) | Pro (paid), Ultra, Enterprise, `AGY_ADC_AUTH` (1.3.6) |
| Claude Sonnet 4.6, Claude Opus 4.6, GPT-OSS-120b | legacy slugs | model-specific | scheduled for removal on 2026-11-30 ([models](https://antigravity.google/docs/models/)) |
| Nano Banana 2 | internal | none | image generation tool only |

`agy models` prints the slugs `--model` accepts (such as `gemini-4-neon-xhigh`). Since 1.3.4, `--model` and `/model` accept a base model ID without `--effort` and choose the model's default thinking level. `agy` knows no short aliases (`hydrogen`, `neon`), refuses an effort-suffixed ID (`gemini-4-hydrogen-high`) paired with a differing `--effort`, and refuses `--effort max` on every model (1.5.13 capture). `/model <name>` switches and saves the default, and `/model <name> <prompt>` runs one prompt on another model and returns. A model change during a turn applies after the turn.

The selector label format, seen in statusline `model.id` and `model.display_name`, is a base name plus a parenthesized variant such as `Gemini 4 Argon (Medium)` or `Claude Opus 5.5 (Thinking)`. Hook `modelName` and transcript `model` use effort-suffixed slug form, such as `gemini-4-neon-high`.

### Quota and credits

Plans grant baseline quota that refreshes on plan-dependent five-hour and weekly windows, with optional AI-credit overage on eligible paid plans ([plans](https://antigravity.google/docs/plans/)). Google says quota is measured by work, not by a fixed prompt or token count. `/usage` (alias `/quota`) refreshes and shows quota, and `/credits` shows credit balance and purchase links. Both are unavailable with `GEMINI_API_KEY` and exit 2 in `-p` (1.5.11, [Print mode](#print-mode) has the 1.5.14 capture). In a 1.5.14 capture with `GEMINI_API_KEY` and `modelProvider: "gemini"`, the statusline carried no `plan_tier`, `email`, or `quota`, and the start screen read `Gemini API key`, signed in or not. The statusline `quota` map carries per-bucket remaining fractions or amounts, resets, and `disabled` flags. Since 1.5.14, the built-in statusline shows AI credits only when balance is positive. Credits have no documented machine-readable field outside print-mode `/credits`.

### Cost

The statusline `cost` object (`{total_usd, subagent_usd, estimated}`, [statusline](https://antigravity.google/docs/cli/statusline/)) reports unrounded USD estimates based on list prices, not billed amounts. 1.5.14 capture (2026-11-22, Google AI Ultra sign-in): none of 142 statusline payloads carried `cost`, across a plain turn, a turn with a nested native subagent, a background command, `/clear`, a `--conversation` resume with a turn, and a Claude Sonnet 5.5 turn. When it appears, and whether it is cumulative, includes `subagent_usd`, or survives a resume, is uncaptured. A `GEMINI_API_KEY` or ADC session was not captured. Print-mode `usage` and transcript token rows report tokens without dollars. Plan quota and AI credits are not API-token billing.

### Prompt cache

Antigravity publishes no prompt-cache lifetime: in 1.5.15 none appears in print-mode JSON, the statusline, transcripts, logs, or the changelog (`agy acp` was not checked). Transcript rows carry per-step `cache_read_tokens` ([Transcripts](#transcripts)), and print-mode `usage` accumulates across a resumed conversation.

1.5.15 capture (2026-11-23, `gemini-4-neon-high`, print mode, scratch home): 16 conversations, each opened on its own block of about 32k tokens and followed at once by a second turn, were resumed after an idle gap, two per gap. After 5, 15, 30, and 45 minutes all eight resumes read 94% of their input from cache. After 60, 75, 90, and 120 minutes all eight lost the conversation's cache, reading 0% or about 20% (the roughly 8k-token prefix the conversations share). In one local transcript a 59-minute gap still read 124,638 of 126,687 input tokens, on a compaction row. Misses also come with no expiry: one probe conversation missed on its immediate second turn, and local history shows misses after 3 and 27 minutes. The lifetime lies between 45 and 60 minutes, and only that one 59-minute sample speaks for the upper end.

### Private local service

A running `agy` exposes an undocumented Connect-over-HTTPS service on process-owned loopback ports with a self-signed certificate. Google does not publish it. Both RPC paths below remain in the 1.5.14 binary, and the wire was cross-checked against CodexBar's [`AntigravityStatusProbe`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityStatusProbe.swift), [`AntigravityStatusProbe+PortDetection`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityStatusProbe%2BPortDetection.swift), and [`AntigravityQuotaSummaryParser`](https://github.com/steipete/CodexBar/blob/b41715f3e3fb85d01d807b9bd7a64d9bf384c6f8/Sources/CodexBarCore/Providers/Antigravity/AntigravityQuotaSummaryParser.swift).

| RPC (POST) | Body | Returns |
| --- | --- | --- |
| `/exa.language_server_pb.LanguageServerService/GetUserStatus` | `{}` | account email and user tier or plan label |
| `/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary` | `{"forceRefresh":true}` | per-model quota buckets grouped by period |

Both take `Content-Type: application/json` and `Connect-Protocol-Version: 1`. The quota summary can sit at the root, under `response`, or under `summary`, and a bucket's remaining fraction can be direct, nested, or in a oneof wrapper. Period labels include explicit five-hour and weekly periods. The complete label set is unpublished.

RimZ uses none of this service: it reads quota through print-mode `/usage` ([Exit, timeout, and read-only commands](#exit-timeout-and-read-only-commands)).

## Remote control, MCP, plugins, and custom agents

These surfaces change tool vocabulary, prompts, runtime transport, or executable configuration, so they are indexed here without depth.

| Surface | What ships |
| --- | --- |
| Remote control | `agy remote-control start\|status\|stop` registers a headless daemon, built into the CLI, as a systemd user service (boot), LaunchAgent (login), or Scheduled Task, reachable from a browser. `--name` sets the instance name, stored as `cliRemoteControlHostname` in `~/.gemini/config/config.json`. `--session` scopes it to the login session. Linux without a systemd user manager uses a background process with no automatic restart or boot launch (1.2.14). Since 1.3.4, the browser Settings modal edits CLI settings and daemon sessions read and save `settings.json`. `featureControls.remoteControlEnabled` lets admins disable it (1.5.8). See [remote control](https://antigravity.google/docs/remote-control/) |
| Active-session remote control | `--remote-control` or `/remote-control` opens a session-scoped connection. `/remote-control off` or session exit removes it. Unsupported with `AGY_ADC_AUTH` (1.3.3) and exits 2 with `GEMINI_API_KEY` (1.5.12) |
| Remote agents | `--remote-agent` runs conversations in an isolated Google-managed remote environment on Pro/Ultra personal accounts ([remote agents](https://antigravity.google/docs/remote-agents/), 1.5.0). `--project` / `--new-project` select or create remote projects. Supports `accept-edits` (1.5.10), `plan` mode and `Agent Mode` in `/config` (1.5.14), and cloud plugin skills (1.5.2). Exits 2 with `GEMINI_API_KEY` (1.5.12). Does not support `-p`, `/codesearch`, `/diff`, `/fork`, `/rewind`, `/btw`, `/voice`, `!`, `@<subagent>`, or local MCP servers, plugins, and skills |
| ACP | `agy acp` exposes an Agent Client Protocol JSON-RPC stdio server for editors such as Zed, JetBrains IDEs, Android Studio, and Xcode ([ACP](https://antigravity.google/docs/cli/acp/), 1.5.0). Supports `default`, `accept-edits`, `auto` (1.5.4), and `plan` modes, `session/resume`, context-window usage notifications (1.5.8), background task and subagent wakeups plus cleanup on cancel/close (1.5.10), all account models (1.5.1), and terminal auth via `agy acp --login` |
| MCP | servers in global `~/.gemini/config/mcp_config.json`, workspace `<workspace>/.agents/mcp_config.json`, or plugin `mcp_config.json` (comments and trailing commas allowed), managed with `agy mcp` or `/mcp`. Remote servers use Streamable HTTP (`serverUrl` or `url`, no legacy SSE/`httpUrl`), with OAuth 2.0 PKCE (CIMD at `https://antigravity.google/oauth/client-metadata.json`, DCR, or manual `oauth`), `authProviderType: "google_credentials"`, custom `headers` (origin-scoped on redirects since 1.5.9), `disabled`, `disabledTools[]`, `enabledTools[]`, `timeoutSeconds` (default 300 s connect, 180 s call, `-1` disables), and per-tool `tools.<name>.eager` and `tools.<name>.background` (`"off"` or `"always"`) ([MCP](https://antigravity.google/docs/mcp/)). Tools with `_meta: {"antigravity/alwaysLoad": true}` in `tools/list` or `"eager": true` load directly as `mcp_<server>_<tool>` instead of lazy `call_mcp_tool` (on-demand results inline up to 10 KB since 1.4.7). Since 1.4.4, `tools/call` carries `_meta["antigravity.google/root_conversation_id"]`. Since 1.5.8, eager MCP tools and `/browser` Chrome DevTools enforce `mcp(...)` permission rules. 1.5.13 capture: a custom agent's `mcpServers` takes name-only items for configured servers and inline `command`/`args` items, in JSON flow style on one line. Without `inheritMcp: false` an agent with no `mcpServers` key gets every configured server, while one that lists any item and omits `inheritMcp` gets only the listed ones, and `inheritMcp: true` keeps configured servers beside listed ones. With `inheritMcp: false` a call to an unlisted server's tool fails before `tools/call` (`tool <name> is not enabled for server <server>`), though that server's process still starts. A name-only item for an unconfigured server is ignored silently. An inline server runs in the workspace cwd with `agy`'s whole environment. A resume keeps the conversation's server set and inline commands even after the agent file changes or is deleted. In the TUI, an inline server's tools are missing for the whole first turn of a fresh or resumed conversation when no configured server is enabled for the agent, `--prompt-interactive` included, and present from the second turn, while print mode binds them on the first turn. 1.5.14 capture: from the second TUI turn, a tool carrying `antigravity/alwaysLoad: true` is called as `mcp_<server>_<tool>`, and an unmarked tool through `call_mcp_tool`. Default permission mode asks before every MCP call, and `--settings` `mcp(<server>/*)` removes the prompt. 1.5.14 capture of a plugin server (`tools` entry in plugin's `mcp_config.json`): `agy` names it `<plugin directory>_<server>` (`rimz_tools`), with the directory name winning over a different manifest `name`, and starts it with the plugin directory as cwd. Unlike an inline agent-file server, its tools bind on the first TUI turn: with `mcp(rimz_tools/*)` in `--settings`, Hydrogen called `wait_list` on the first turn without a permission card. A plugin server that exits during `initialize` shows in `/mcp` as `✗ rimz_tools  error: <its last stderr line>`, or `connection closed: calling "initialize": EOF` when it wrote none, whatever its exit status, and the start screen shows no MCP message. `agy` keeps each server's tool schemas under `~/.gemini/antigravity-cli/mcp/<server>/` (`instructions.md` and one `<tool>.json` per tool), one folder per server name for the account, and a server that fails `initialize` leaves that folder's files byte for byte as they were. 1.5.14 capture with a custom agent: `inheritMcp: false` does not withhold a plugin server. With `mcpServers: [{"name": "userx"}]`, and with `rimz_tools` added to that list, `/mcp` showed `✓ rimz_tools` beside `✓ userx` exactly as for an agent with neither key, and Hydrogen called `rimz_tools`'s `wait_list` on the first turn in all three |
| Plugins | installed under `~/.gemini/config/plugins/<dir>/` (or workspace `.agents/plugins/<dir>/`) and managed with `agy plugin` or `/plugin` ([plugins](https://antigravity.google/docs/plugins?tab=cli)). Enablement lives in `~/.gemini/config/config.json`: on 1.5.12, `agy plugin disable <dir>` writes `plugins.<dir>.enabled = false`, `enable` writes `true`, and no entry means enabled (`agy`'s bundled docs tell tools never to hand-edit the file). Manifests live in `plugin.json` (schema `https://antigravity.google/schemas/v1/plugin.json`, UTF-8 BOM tolerated since 1.5.11, Agent Plugins `$schema` layout fixed in 1.5.14) or `.antigravity-plugin/` / `.gemini-plugin/`. A plugin can ship `hooks.json`, `mcp_config.json`, `skills/`, `rules/` or `rules.json`, `agents/` or root `agents.json`, `bin/` tools, `auth.json` (`"optional": true` since 1.4.12), and `sidecar.json` (`${HOME}` since 1.5.0). `agy plugin validate` checks manifests, components, hooks, sidecars, logos, and lowercase kebab-case names (`--strict`, `--json`, and local/zip/URL/GitHub/marketplace targets). `agy plugin import claude` converts Claude marketplace plugins, commands, HTTP MCP servers, and hooks (1.4.11, 1.5.5). Organization `pluginControls` can disable plugins or the marketplace (1.5.8, 1.5.9) |
| Skills | `SKILL.md` under `.agents/skills/` (also `.agent/`, `_agents/`, `_agent/`), `~/.gemini/config/skills/`, `~/.gemini/antigravity-cli/skills/`, `~/.gemini/skills/`, and each plugin's `skills/` ([skills](https://antigravity.google/docs/skills/), [customization discovery](https://antigravity.google/docs/customization-discovery/)). Without frontmatter `name`, a skill takes its folder name (1.5.10). `disable-slash-command: true` hides a skill from `/`. `/skills reload` refreshes discovery without restarting. 1.5.13 capture: TUI and print mode discover `~/.gemini/config/skills`, `~/.gemini/antigravity-cli/skills`, workspace `.agents/skills`, and each plugin's `skills/`, and never `~/.agents/skills`. On a duplicate name, `~/.gemini/antigravity-cli/skills` wins in the TUI and ignores manifest `exclude`. 1.5.14 capture (2026-11-22, print-mode `/skills`): `~/.gemini/skills` is discovered too, and on a duplicate name `~/.gemini/antigravity-cli/skills` wins over `~/.gemini/config/skills`, which wins over `~/.gemini/skills`. `agy` creates neither global root. Frontmatter `disable-model-invocation: true` hides a skill from the model in all four roots and keeps it in the `/` menu. Symlinked skill directories and a read-only root are discovered |
| Custom agents | Markdown `agent.md` or `<name>.md` under `.agents/agents/`, `~/.gemini/config/agents/`, or plugin `agents/`, with YAML frontmatter (`name`, `description`, `mainAgent`, `subagent`, `hidden`, `model`, `tools`, `skills`, `plugins`, `rules`, `agents`, `hooks`, `mcpServers`, `inheritMcp`, `inheritCustomizations`, `excludeDefaultComponents`, `commandExecutionPolicy`). They inherit ambient skills, rules, and subagents by default. 1.5.13 capture: `--agent <name>` selects an agent by its frontmatter `name`, never its file name, including one a plugin ships under `agents/`. A global agent of the same name wins silently, and an unknown name exits 1 with `error: unknown agent "X" (available: ...)` before any turn in `-p`. Plugin hooks and workspace `AGENTS.md` rules still apply. The body replaces the `identity` section inside a smaller built-in scaffold, and frontmatter `model` is ignored on a main agent. `tools` is an allowlist, and without the key the agent gets a small read-only set. An agentless session reports `ask_question`, `define_subagent`, `edit_file`, `invoke_subagent`, `manage_subagents`, `manage_task`, `read_file`, `run_command`, `run_workflow`, `schedule`, `search_web`, `send_message`, `swarm`, and `write_file`. Under a custom agent, `read_file` refuses a directory (`path is a directory, not a file`) and `agy` registers no separate directory-listing tool. A custom agent's `tools` naming a tool `agy` has not registered (`swarm`, `TodoWrite`, or a made-up name) exits 3 before the first turn with `unknown component: tool "<name>" not found in registry`. `agy` reads the file once when a new conversation starts and snapshots its body and tool grants into the conversation: a resume keeps the snapshot and ignores `--agent`, while still failing on an unknown name |
| SDK | Python SDK (`google-antigravity`) hosting its own agent runtime with lifecycle hooks, streaming, persistence, structured output, MCP, and local models ([SDK overview](https://antigravity.google/docs/sdk/overview/)). It does not attach to a running `agy` |

Customization manifests (`skills.json`, `rules.json`, `agents.json`, `plugins.json`) load direct directory children, and nested items require `include_only` ([customization discovery](https://antigravity.google/docs/customization-discovery/)). Discovery reads each `.agents/` directory, `AGENTS.md`, and `GEMINI.md` between cwd (plus `--add-dir` roots) and the repository root, and supports `.gemini/config.json` `personal_customization_dir` (`people/{user}`). Paths in Markdown agent customizations resolve relative to the agent file (1.3.1). Since 1.5.2, workspace `.agents/plugins.json` and `.agents/skills.json` only load entries inside the workspace or repository root, skipping absolute paths, `../`, or symlinks pointing outside.

## Documentation drift

These disagreements compare the living `antigravity.google/docs` pages (read 2026-11-22) against release notes, the 1.5.14 binary, and dated live captures.

| Surface | Official docs | Other evidence | Rule this page follows |
| --- | --- | --- | --- |
| Agent/model output flags | 1.1.12 changelog adds `--output-format` to `agents` and `models` | 1.5.14 subcommand help lists only `-h` / `--help` | acceptance was not tested in this refresh |
| Reasoning effort `max` | 1.5.14 `--help`, [CLI commands](https://antigravity.google/docs/cli/reference/), and [headless](https://antigravity.google/docs/cli/headless/) list `max` | 1.5.13 capture: `agy` refuses `max` on every model and enforces per-model effort ceilings (`xhigh` on Neon/Hydrogen, `high` on Carbon/Argon/Flash/Claude) | treat `max` as rejected and respect per-model ceilings |
| `--settings` flag | omitted from [CLI commands](https://antigravity.google/docs/cli/reference/) flag table | present in 1.5.14 `--help` and verified in 1.5.14 capture | supported (last `--settings` wins) |
| Structured output | [headless](https://antigravity.google/docs/cli/headless/) still advertises primitive type names (`string`, `number`, `integer`, `boolean`) | 1.2.14 changelog rejects them and requires `"type": "object"` | object-root JSON schema or existing file |
| Headless error and timeout output | [headless](https://antigravity.google/docs/cli/headless/) describes non-zero exit when no response is produced and omits `finished_reason`, `denied_actions`, and `AGY_ERROR` | 1.2.10, 1.4.11, and 1.5.0 changelogs and 1.5.14 binary assign exit 3, `AGY_ERROR`, and `finished_reason` (`print_timeout`, `interrupted`, `error`) even after partial output | use exit status, `finished_reason`, and `AGY_ERROR`, not response presence |
| Transcript basename | [hooks](https://antigravity.google/docs/hooks/) names `transcript.jsonl` | 1.1.2 and 1.2.2 hooks sent `transcript_full.jsonl`, and embedded instructions define both files | expect either basename |
| Statusline `transcript_path` | [statusline](https://antigravity.google/docs/cli/statusline/) example still puts `~/.gemini/antigravity/brain/...` (2.0 app-data root) | 1.2.2 capture sent the 2.0 root in statusline while hooks sent the CLI root's `transcript_full.jsonl` | take the transcript path from hooks |
| Statusline extra fields | [statusline](https://antigravity.google/docs/cli/statusline/) lists `vcs.type` and `sandbox.enabled`, omits `artifacts`, `background_tasks`, and `agent_state: "thinking"` | `examples/statusline/statusline.sh` reads `vcs.branch`/`vcs.dirty`, `examples/title/title.sh` checks `thinking`, and 1.5.14 binary retains all tags | tolerate every field as optional and unknown enum values |
| Hook common fields | [hooks](https://antigravity.google/docs/hooks/) omits `hookEventName`, `cwd`, and `toolCall.id` | 1.5.4 changelog and 1.5.14 embedded hook doc define all three | present since 1.5.4 |
| `PostInvocation` timing | [hooks](https://antigravity.google/docs/hooks/): "immediately after each model invocation completes" | 1.5.14 embedded hook doc: "after tool calls finish" | not settled without a capture |
| `Stop.terminationReason` values | [hooks](https://antigravity.google/docs/hooks/) gives lowercase examples (`model_stop`, `max_steps_exceeded`, `error`) | 1.5.14 embedded hook doc, protobuf enum, and 1.5.13 `/compact` capture (`TERMINAL_CUSTOM_HOOK`) use uppercase enum names (`NO_TOOL_CALL`, `ERROR`, `MAX_INVOCATIONS`, `MAX_FORCED_INVOCATIONS`, `MAX_TOKEN_BUDGET_EXCEEDED`, `USER_CANCELED`, `TERMINAL_STEP_TYPE`, `TERMINAL_CUSTOM_HOOK`, `INJECTED_RESPONSE`) | expect uppercase enum names and tolerate lowercase |
| `Stop` continuation field | omitted from [hooks](https://antigravity.google/docs/hooks/), spelled `continuation_active` in 1.5.6 changelog | protojson wire and 1.5.14 embedded hook doc use camelCase `continuationActive` | expect `continuationActive` on the wire |
| `PreToolUse` decisions | [hooks](https://antigravity.google/docs/hooks/) lists 6 values and makes `allow` delegate to policy | 1.5.14 embedded jsonschema lists 5 without `auto_approve`, 1.5.14 embedded hook doc lists 4 and describes `allow` as automatic approval, while 1.5.14 binary wires `hook_auto_approve` | retain the disagreement. A 1.5.14 capture saw `allow` leave a command prompt in place (see [Decisions](#decisions)) |
| `queuedMessages: "send-immediately"` | [settings](https://antigravity.google/docs/settings?tab=cli) and [CLI commands](https://antigravity.google/docs/cli/reference/) say it interrupts the agent | 1.5.13 changelog corrects `/config` to say the message joins the current turn after the current step | does not interrupt the running step |
| Remote agent execution modes | [remote agents](https://antigravity.google/docs/remote-agents/) says `accept-edits`, `plan`, and `auto` are unavailable | 1.5.10 added `accept-edits` and 1.5.14 added `plan` and `Agent Mode` in `/config` | `default`, `accept-edits`, and `plan` are available on 1.5.14 |
| Remote Control browser settings | [remote control](https://antigravity.google/docs/remote-control/) says settings cannot be toggled from the remote web UI for interactive CLI sessions | 1.3.4 changelog enabled the browser Settings modal for CLI sessions | check live behavior when integrating Remote Control |
| Model examples in docs | [hooks](https://antigravity.google/docs/hooks/) uses `gemini-3.6-flash-medium` and [settings](https://antigravity.google/docs/settings?tab=cli) uses `Gemini 3.8 Flash` | [models](https://antigravity.google/docs/models/) makes Gemini 4 Neon the default on eligible plans | read the model from the payload |

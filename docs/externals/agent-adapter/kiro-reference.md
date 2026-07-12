# Kiro CLI protocol reference

> RimZ's Kiro adapter maps the first verified lifecycle milestone in [kiro.md](../../internals/agents/kiro.md). This document records the upstream surface and the gaps that remain; the agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md), and the account, balance, spend, and pricing contract is [providers.md](../../internals/agents/providers.md).

This is the single home for the **Kiro CLI upstream protocol surface** relevant to RimZ: the v3 command-hook seam, session and launch identity, capability permissions, the ACP server, authentication, model/context/credit reporting, subagents, local state, and the gaps that require live fixtures before implementation.

Refresh baseline: **2026-07-11**, Kiro CLI **2.12.1** with its latest **v3 early-access engine** selected by `kiro-cli chat --v3`. This reference intentionally targets that engine only. It does not describe the embedded lowercase-hook protocol, trust flags, agent JSON, or session format from the older engine. Kiro ships v3 inside the current 2.x executable, so distinguish the package version from the selected engine in discovery and diagnostics.

Coverage is **depth on viable RimZ inputs, breadth as an index**. Official v3 documentation publishes the hook configuration and trigger semantics, but not the command-hook stdin schema, permission-prompt event, transcript schema, or machine-readable usage response. Those boundaries are called out instead of filling them with behavior from the older engine.

### Lineage: Kiro CLI is the rebranded Amazon Q Developer CLI

The installed 2.12.1 binaries build from `crates/q_cli/` and carry the Fig/figterm shell-integration heritage (the `kiro-cli-term` PTY daemon, `should-figterm-launch`, `_ pre-cmd` shell hooks). Kiro CLI is the rebranded **Amazon Q Developer CLI** (`aws/amazon-q-developer-cli`, open source). Its hook payload conventions (`conversation_id`, `cwd`, `tool_name`, `hook_event_name`) descend from Amazon Q, which is why the tolerant parser accepts `conversation_id`/`conversationId` session aliases. Treat the Amazon Q CLI source and docs as a strong secondary reference for the wire, but keep the v3 warning in force: the v3 engine capitalizes triggers (`SessionStart`, `UserPromptSubmit`, `PostToolUse`, `Stop`) where the older Q engine used lowercase (`agentSpawn`, `userPromptSubmit`, `preToolUse`, `postToolUse`, `stop`), and a captured v3 fixture still governs before any field is trusted.

## Upstream sources

Re-fetch these rolling pages and compare the CLI changelog before implementation or refresh.

| Surface | Official source |
| --- | --- |
| Latest CLI release and v3 introduction | <https://kiro.dev/changelog/cli/> · <https://kiro.dev/changelog/cli/2-8/> |
| v3 overview and compatibility boundary | <https://kiro.dev/docs/cli/v3/> · <https://kiro.dev/docs/cli/v3/feature-overview/> |
| v3 hooks, triggers, actions, and exit behavior | <https://kiro.dev/docs/cli/v3/hooks/> · <https://kiro.dev/docs/hooks/> · <https://kiro.dev/docs/hooks/types/> |
| v3 capability permissions | <https://kiro.dev/docs/cli/v3/permissions/> |
| v3 agent configuration | <https://kiro.dev/docs/cli/v3/agent-config/> |
| CLI commands and settings | <https://kiro.dev/docs/cli/reference/cli-commands/> · <https://kiro.dev/docs/cli/reference/settings/> |
| Terminal UI and slash commands | <https://kiro.dev/docs/cli/terminal-ui/> · <https://kiro.dev/docs/cli/reference/slash-commands/> |
| Sessions and context | <https://kiro.dev/docs/cli/chat/session-management/> · <https://kiro.dev/docs/cli/chat/context/> |
| ACP server | <https://kiro.dev/docs/cli/acp/> · <https://agentclientprotocol.com/> |
| Authentication and headless operation | <https://kiro.dev/docs/cli/authentication/> · <https://kiro.dev/docs/cli/headless/> |
| Subagents | <https://kiro.dev/docs/cli/chat/subagents/> |
| Models, context windows, and credit multipliers | <https://kiro.dev/docs/cli/models/> |
| Billing and credit semantics | <https://kiro.dev/docs/cli/billing/related-questions/> · <https://kiro.dev/docs/billing/add-on-credits/> |
| Installation, update behavior, and logs | <https://kiro.dev/docs/cli/installation/> |
| Configuration scopes and `KIRO_HOME` | <https://kiro.dev/docs/cli/chat/configuration/> · <https://kiro.dev/docs/cli/reference/settings/> |

The authoritative local companion to the rolling pages is the installed executable:

```sh
kiro-cli --version
kiro-cli --help-all
kiro-cli chat --v3 --help
kiro-cli diagnostic --format json
kiro-cli whoami --format json
kiro-cli settings list --all --format json
kiro-cli chat --list-models --format json
```

Record the executable version and prove that the launched pane selected v3. Reject a binary without `--v3`; silently falling back would bind RimZ to a different hook, permission, session, and agent-config protocol.

## Recommended adapter shape

Use **v3 command hooks** as the stock-TUI lifecycle seam. They are local subprocesses, receive JSON on stdin, preserve Kiro's own terminal UI, and expose session start, prompt start, tool brackets, turn stop, and spec-task brackets. Install only command actions; agent actions add prompts to model context and are product automation rather than observation.

Use **pane presence and process liveness** for instance registration before the first hook and removal after process exit. Kiro publishes no `SessionEnd` hook: v3 `Stop` means the end of a turn, despite the CLI v3 trigger table's terse “session ends” wording. The shared IDE documentation and hook-type page define it as agent turn completion.

Use **ACP** as an optional structured sidecar or supervised-run transport only after a v3 fixture proves compatibility. ACP exposes session creation/loading, streaming text, tool-call updates, turn completion, cancellation, model selection, compaction notifications, and subagent termination. Running `kiro-cli acp` makes RimZ the protocol client rather than observing a user's stock TUI, so it is not the primary interactive adapter seam.

Do not implement `rimz agents kiro -p` by copying the older `--no-interactive` flow. The 2.12.1 `chat --v3` parser still accepts `--no-interactive` (the flag was not removed at the CLI surface, contrary to earlier read of the docs), but its v3 semantics — final assistant message on stdout, exit status, transcript retention, cancellation — are unverified. First establish whether v3 `--no-interactive` or ACP can satisfy RimZ's supervised-run contract, including authentication, permission requests, final output, exit status, transcript retention, and cancellation.

The candidate transport matrix is:

| RimZ concern | Latest upstream surface | Backstop / gap |
| --- | --- | --- |
| instance presence | pane process `kiro-cli --v3` | process tree and pane cwd |
| session registration | `SessionStart` command hook | v3 stdin schema and session ID require capture |
| turn start | `UserPromptSubmit` command hook | prompt content is privacy-sensitive |
| proof of work | `PostToolUse` command hook | capture canonical v3 tool names |
| acting phase | `PostToolUse` plus write-capability table | file events are redundant enrichment |
| clean turn completion | `Stop` command hook | no documented error/abort discriminator |
| session removal | pane/process death | no `SessionEnd` trigger |
| permission ask | no documented v3 hook | pane attention; investigate ACP permission requests |
| user question / plan approval | no documented v3 hook | pane attention; investigate tool identity and ACP |
| compaction | ACP `_kiro.dev/compaction/status` in hosted mode | no v3 compaction hook in the stock TUI |
| model and effort | launch/profile/settings plus ACP model state | hooks publish no fields; model may change in-session |
| context usage | terminal `/context show` display | no stock-TUI machine-readable feed; never scrape pane output |
| subagents | `subagent` tool activity and persisted parent metadata | hooks do not fire in subagents; no child lifecycle hooks |
| tokens and credits | `/usage` display and model credit table | no documented machine-readable live usage API |
| authentication | `whoami --format json` | JSON schema is unpublished; capture a fixture |
| supervised runs | `--no-interactive` (parse-accepted under v3) or candidate ACP client | v3 output/exit/transcript semantics unverified |

The viable first milestone is therefore **presence + root lifecycle + native resume**. Awaiting-user state, compaction, child rows, live context, spend, and supervised runs remain capability-gated until the live-verification items at the end are satisfied.

## Launch, discovery, and process binding

Launch the latest engine as:

```sh
kiro-cli chat --v3
```

Use the explicit `chat` subcommand before chat options. The 2.12.1 parser accepts `kiro-cli chat --v3 --model <model> --effort <level>` but rejects those chat-level flags when they follow the root-level `kiro-cli --v3` shortcut. The shortcut remains useful for a bare interactive launch; the explicit form composes correctly with RimZ profile and resume arguments.

**Process tree and presence names.** The install ships three binaries: `kiro-cli` (119 MB launcher/CLI, the `bin`), `kiro-cli-chat` (691 MB v3 chat engine; clap reports its `bin_name` as `kiro-cli-chat` under `chat --help`), and `kiro-cli-term` (86 MB figterm PTY/shell-integration daemon). During the device-login phase the pane process observed as `kiro-cli chat --v3`; the heavy engine binary makes it likely the launcher execs into `kiro-cli-chat` once the TUI is active (unverified past login without an account). RimZ therefore matches presence on **both** `kiro-cli` and `kiro-cli-chat`, and deliberately excludes `kiro-cli-term` — that daemon runs for every integrated shell, so matching it would bind non-agent panes.

**Trust flags still parse under v3 (correcting "removed").** Although the official v3 permissions page documents capability-rule files only, the 2.12.1 `chat --v3` parser still accepts `-a, --trust-all-tools`, `--trust-tools <TOOL_NAMES>`, and `--no-interactive` (verified: passing them under `--v3` proceeds to device login rather than a parse error). Whether they are honored at runtime by the v3 capability engine is unverified without an account, and the permissions page states restrictive rules win over any relaxation. Treat these as parse-accepted-but-unproven: do not map a RimZ permission posture onto `--trust-all-tools` until a live turn proves it overrides (or is overridden by) `permissions.yaml`.

The v3 engine requires the terminal UI; the classic interface does not support it. The package auto-updates in the background unless `app.disableAutoupdates` is enabled, so persist the reported version with diagnostics and make protocol drift visible.

`KIRO_HOME` overrides the default `~/.kiro` root for global agents, prompts, skills, steering, settings, and sessions. Resolve it from the launched process environment rather than assuming the home directory. Other implementation-relevant environment variables documented by Kiro include:

| Variable | Purpose |
| --- | --- |
| `KIRO_HOME` | relocates Kiro's global configuration and state root |
| `KIRO_API_KEY` | API-key authentication for non-interactive operation |
| `KIRO_CHAT_LOG_FILE` | overrides the CLI log path |
| `KIRO_LOG_LEVEL` | controls ACP/log verbosity |
| `KIRO_LOG_NO_COLOR`, `NO_COLOR` | disable colored log or TUI output |
| `KIRO_ASCII_MODE` | forces ASCII rendering |
| `KIRO_NO_SYNCHRONIZED` | disables synchronized terminal output |
| `KIRO_ACP_RECORD_PATH` | records TUI ACP wire traffic as JSONL for debugging |

The default log is `$TMPDIR/kiro-log/kiro-chat.log` on macOS, `$XDG_RUNTIME_DIR/kiro-log/kiro-chat.log` on Linux, and `%TEMP%\kiro-log\logs\kiro-chat.log` on Windows. Logs are diagnostic enrichment, not lifecycle truth; their format is not published.

The docs publish no hook environment variable that binds a command child to the originating pane. Before implementation, verify which RimZ pane/session stamps survive into the hook process and whether ancestor PID recovery reaches the in-pane Kiro process.

## Session identity, resume, rewind, and compaction

Kiro saves sessions after every conversation turn, scopes ordinary session browsing by working directory, and assigns each session a UUID. The current command surface documents:

| Operation | Surface | Identity implication |
| --- | --- | --- |
| resume latest in cwd | `kiro-cli chat --resume` | preserves selected session ID |
| resume exact | `kiro-cli chat --resume-id <UUID>` | preserves that session ID |
| choose session | `--resume-picker` or `/chat resume` | preserves selected session ID |
| show current ID | `/session-id` | human-readable identity fallback |
| new session | `/chat new [prompt]` | creates a fresh identity |
| rewind/fork | `/rewind [turn]` | creates a new session; original remains |
| export | `/chat save`, `/transcript save --json` | manual export, not a live sidecar contract |

The v3 overview says its session format is incompatible with the previous engine and only locates the state under `~/.kiro/sessions/`; it does not publish the v3 on-disk schema, filename mapping, database schema, or transcript event types. Do not reuse the older engine's SQLite or ACP JSONL assumptions for a stock v3 TUI without a captured v3 fixture.

The generic ACP documentation stores hosted ACP sessions under `~/.kiro/sessions/cli/` as `<session-id>.json` metadata plus `<session-id>.jsonl` events. Treat that as an ACP transport contract, not proof of the stock v3 TUI's local representation.

Current general context documentation says manual or automatic conversation compaction creates a new session. The v3 hook catalog has no pre- or post-compaction trigger. The ACP extension `_kiro.dev/compaction/status` reports compaction progress only when RimZ owns an ACP connection. A stock-TUI adapter must leave compaction unsupported until a v3 session-rotation fixture proves the old/new identity relationship and provides explicit start/end evidence.

## V3 hooks

V3 hooks are versioned standalone JSON files in `.kiro/hooks/` for a workspace and `~/.kiro/hooks/` for a user. The shared v3 engine also powers current Kiro IDE hook behavior. Prefer a bounded user-level RimZ file such as `~/.kiro/hooks/rimz.json`; this observes all workspaces without writing executable configuration into each repository.

Workspace hook files are executable project surface. Include `.kiro/hooks/*.json`, command strings, project agent profiles, inline MCP commands, skills, and other command-bearing Kiro configuration in the RimZ trust hash and install diff. V3 stores workspace permission decisions outside the repository, but that separation does not make repository hooks safe by itself.

### Configuration

One file may contain multiple hooks:

```json
{
  "version": "v1",
  "hooks": [
    {
      "name": "rimz-session-start",
      "trigger": "SessionStart",
      "action": {
        "type": "command",
        "command": "/absolute/path/to/rimz hooks feed --source kiro"
      },
      "timeout": 5,
      "enabled": true
    },
    {
      "name": "rimz-user-prompt",
      "trigger": "UserPromptSubmit",
      "action": {
        "type": "command",
        "command": "/absolute/path/to/rimz hooks feed --source kiro"
      },
      "timeout": 5,
      "enabled": true
    },
    {
      "name": "rimz-post-tool",
      "trigger": "PostToolUse",
      "action": {
        "type": "command",
        "command": "/absolute/path/to/rimz hooks feed --source kiro"
      },
      "timeout": 5,
      "enabled": true
    },
    {
      "name": "rimz-stop",
      "trigger": "Stop",
      "action": {
        "type": "command",
        "command": "/absolute/path/to/rimz hooks feed --source kiro"
      },
      "timeout": 5,
      "enabled": true
    }
  ]
}
```

| Field | Type / default | Contract |
| --- | --- | --- |
| top-level `version` | string, required | current schema is `"v1"` |
| top-level `hooks` | array, required | independent hook entries |
| `name` | string, required | identifier shown in telemetry |
| `description` | string, optional | documentation only |
| `trigger` | enum string, required | lifecycle point |
| `matcher` | regex string, optional | always-match when absent; event-dependent target |
| `action` | object, required | command or agent action |
| `timeout` | integer seconds, default 60 | `0` disables timeout; ignored for agent actions |
| `enabled` | boolean, default true | skips the entry when false |

Install an absolute, safely quoted executable path. The docs say a command action runs a shell command but do not name the shell, quoting rules, working directory, environment inheritance, concurrent ordering, or behavior when multiple files define the same hook name. Capture paths containing spaces and every supported platform before shipping installation.

### Action types and stdout discipline

Command actions receive hook context as JSON on stdin. Hook stdout is the decision/context channel:

| Exit | V3 behavior |
| --- | --- |
| `0` | success; stdout is added to model context for `SessionStart` and `UserPromptSubmit`, ignored for other triggers |
| `2` | blocks `PreToolUse`, `UserPromptSubmit`, or `PreTaskExec`; stderr is returned to the agent |
| other | warning shown; guarded work proceeds |

RimZ observation hooks return exit `0` with empty stdout and send all diagnostics to stderr or RimZ state logs. Even on events where stdout is ignored, keep it empty so future upstream changes cannot inject logging into model context.

Agent actions have shape `{ "type": "agent", "prompt": "..." }` and append their prompt to model context without spawning a process. They are useful for native Kiro automation but provide no observation wire and must not be used by the adapter.

### Trigger catalog

| Trigger | Fires | Matcher target | Blocks | Candidate RimZ use |
| --- | --- | --- | :---: | --- |
| `SessionStart` | a session begins | none | | `Registered` after identity capture |
| `UserPromptSubmit` | user submits a prompt | prompt text | ✓ | `TurnStarted` |
| `PreToolUse` | before tool execution | tool name | ✓ | governance only; never infer successful work or an open permission prompt |
| `PostToolUse` | after tool execution | tool name | | `ToolUsed`, heartbeat, acting classification |
| `Stop` | agent completes its turn | none | | clean `TurnEnded` candidate |
| `PreTaskExec` | before a spec task moves to in-progress | none | ✓ | spec enrichment, not a new root turn |
| `PostTaskExec` | after a spec task completes | none | | spec enrichment |
| `PostFileCreate` | agent creates a matching file | file path | | redundant edit evidence |
| `PostFileSave` | agent saves a matching file | file path | | redundant edit evidence |
| `PostFileDelete` | agent deletes a matching file | file path | | redundant edit evidence |
| `Manual` | user explicitly runs a hook | none | | no lifecycle meaning |

The CLI v3 page lists `Manual`, while the latest shared hook page says manual hooks have moved to manual steering files. Do not install or depend on it.

File hooks are narrower than filesystem watchers: current shared documentation says they fire for files modified by the agent. Use `PostToolUse` as the lifecycle heartbeat and file triggers only if a later implementation needs precise edit enrichment.

### Matcher semantics and tool names

Matchers are regex strings. V3 evaluates them against tool names for `PreToolUse`/`PostToolUse`, prompt text for `UserPromptSubmit`, and file paths for file triggers. Other triggers ignore matchers.

The unified hook UI documents category names `read`, `write`, `shell`, `web`, `spec`, and `*`, plus source prefixes `@mcp`, `@powers`, and `@builtin`. The CLI v3 page's example also uses internal-looking `fs_write|str_replace`. This is not enough to freeze an acting table: capture actual `tool_name` values from the CLI's command-hook stdin, then classify only proven file-writing tools. Shell, web, and arbitrary MCP calls remain non-editing even when they can mutate external state.

### Input schema gap

The v3 pages promise JSON hook context on stdin but publish no common object or event-specific field schema. In particular, the latest contract does not state whether command hooks receive:

- a session ID, cwd, timestamp, engine version, model, effort, transcript path, or parent ID;
- prompt text in JSON in addition to the documented `USER_PROMPT` environment variable;
- `tool_name`, tool input, tool output, call ID, success status, or error status;
- spec name/task ID for task triggers;
- file path in JSON in addition to the documented `{{filePath}}` command template;
- a stop reason, final response, error bit, cancellation bit, or rate-limit marker.

Do not import the older lowercase-hook payload into the v3 parser. Begin implementation by installing a capture hook against the exact supported binary, redacting content, and recording one fixture for every trigger. The production parser then uses typed optional fields, quarantines lifecycle events without a durable session identity, and tolerates unknown additions.

### Lifecycle mapping constraints

After fixtures establish identity, the minimum mapping is:

| Native observation | RimZ signal | Constraint |
| --- | --- | --- |
| `SessionStart` | `Registered` | requires one stable provider session ID |
| `UserPromptSubmit` | `TurnStarted` | record prompt only under RimZ privacy policy |
| successful `PostToolUse` | `ToolUsed` | `edits` only for captured file-writing tool names |
| `Stop` | `TurnEnded { errored: false }` | only if fixture proves error/abort paths do not emit an indistinguishable stop |
| pane/process exit | `Ended` | provider session tombstone only when pane binding is unambiguous |

V3 publishes no hook for permission requests, questions, plan approval, compaction, subagent start/stop, rate limits, model changes, or session end. Capability declarations must reflect those absences instead of synthesizing durable truth from terminal text.

## Permissions and awaiting-user behavior

V3 replaces command-line trust flags with capability rules. User rules live at `~/.kiro/settings/permissions.yaml`; per-user workspace rules live outside the repository at `~/.kiro/workspace-roots/<hash>/permissions.yaml`. `KIRO_HOME` may relocate these roots.

```yaml
rules:
  - capability: fs_read
    effect: allow
  - capability: fs_write
    effect: allow
    match:
      - src/**
      - tests/**
  - capability: shell
    effect: allow
    match:
      - "cargo *"
      - "git diff*"
```

Rules contain `capability`, optional `match`, optional `exclude`, and `effect` (`deny`, `ask`, or `allow`). Restrictiveness wins globally: `deny > ask > allow`; a permissive rule cannot override a restrictive one from another scope.

Capabilities are `fs_read`, `fs_write`, `filesystem`, `shell`, `web_fetch`, `web_search`, `mcp`, `subagent`, `skill`, `diagnostics`, `context`, and the meta-capabilities `builtin` and `all`. Filesystem patterns use path globs; shell, web, and MCP use `*` string matching. Kiro parses compound shell commands and evaluates each component independently.

Hardcoded policy always denies writes to Kiro's permission configuration and always asks for writes to `.git/**`, `.kiro/agents/**`, `.kiro/hooks/**`, and `.kiroignore`. With no user configuration, workspace reads, common read-only Git/system commands, and utility tools are allowed; everything else asks.

No v3 hook announces that an `ask` rule has opened the native approval UI, and `PreToolUse` occurs before execution regardless of whether policy allows, denies, or asks. Therefore `PreToolUse` cannot map to `awaiting_input`. A first adapter leaves permission waiting unsupported and relies on ordinary pane attention until ACP or another official event supplies a synchronous discriminator.

RimZ permission-mode launch mapping should write or select explicit v3 permission policy, not pass removed trust flags. Because user and workspace rules merge restrictively, preflight must evaluate conflicts and fail with the fixing path when a requested autonomous profile cannot work.

## Agent profiles and executable trust

V3 agent profiles are Markdown with YAML frontmatter (JSON is equivalent). Workspace profiles live in `.kiro/agents/`; user profiles live in `~/.kiro/agents/`. Workspace profiles load only after workspace trust. Nested names are relative paths without the extension.

```markdown
---
description: RimZ-managed coding profile
model: claude-sonnet-5
tools: [read, write, shell]
permissions:
  rules:
    - capability: fs_read
      effect: allow
    - capability: fs_write
      effect: ask
---

Follow the project's instructions and report verified results.
```

The `tools` tags are `read`, `write`, `shell`, `web`, `subagent`, `knowledge`, `todo_list`, `@mcp`, `@builtin`, and `*`. Profiles may also embed MCP servers, environment expansion, resources, skills, model IDs, and permission rules. Every MCP command, environment-bearing launch definition, resource/skill path, and permission rule is part of RimZ's executable trust surface.

Use `--agent <name>` to select a profile and `--effort low|medium|high|xhigh|max` where the current launch parser accepts it with v3. Verify both flags on the supported binary because the general CLI reference is shared with the older engine. The active model can change through `/model`, and effort through `/effort`; hooks expose no documented model-change event.

## ACP server

`kiro-cli acp [--agent <name>]` speaks JSON-RPC 2.0 over stdin/stdout. The official Kiro ACP page documents:

| Method / update | Purpose |
| --- | --- |
| `initialize` | protocol and capability negotiation |
| `session/new` | create session with cwd and optional MCP servers |
| `session/load` | load by session ID |
| `session/prompt` | submit prompt content |
| `session/cancel` | cancel active operation |
| `session/set_mode` | switch agent configuration/mode |
| `session/set_model` | change the active model |
| `AgentMessageChunk` | streamed agent content |
| `ToolCall`, `ToolCallUpdate` | structured tool start/progress/result updates |
| `TurnEnd` | terminal turn notification |

Kiro advertises `loadSession: true` and image prompt support. Its extensions are:

| Extension | Purpose |
| --- | --- |
| `_kiro.dev/commands/execute` | execute slash command |
| `_kiro.dev/commands/options` | command completion |
| `_kiro.dev/commands/available` | command catalog notification |
| `_kiro.dev/mcp/oauth_request` | MCP OAuth URL notification |
| `_kiro.dev/mcp/server_initialized` | MCP startup notification |
| `_kiro.dev/compaction/status` | compaction progress |
| `_kiro.dev/clear/status` | history-clear progress |
| `_session/terminate` | terminate a subagent session |

The Kiro page does not publish full parameter/result schemas, permission-request support, question/plan extensions, finish reasons, token usage, or error mapping; follow the pinned ACP specification and capture Kiro extension fixtures before writing a client.

The TUI uses ACP internally and `KIRO_ACP_RECORD_PATH` can record its JSONL traffic. That makes a read-only record a promising research source, but it is a debugging surface rather than a documented stable adapter API. Never parse a live record as lifecycle truth until Kiro publishes its stability and durability contract.

## Subagents

Kiro can run up to four subagents concurrently. Each child has isolated context, may use a named custom profile, returns a summary to its parent, and persists a parent session ID. The parent can expose live child activity in the crew monitor.

The current unified-engine documentation states that hooks do not trigger in subagents. V3 also publishes no `SubagentStart` or `SubagentStop` trigger. A parent's `PostToolUse` around the `subagent` capability can prove delegation activity but cannot supply a unique durable child ID, child terminal verdict, or per-child tool heartbeat unless the captured v3 payload contains undocumented fields.

Declare `subagents: false` for a first lifecycle adapter. Revisit child rows only if the ACP stream or documented v3 session metadata supplies a stable child session ID, parent ID, start bracket, terminal status, and process/session liveness semantics.

## Authentication, account, and credentials

Interactive authentication supports Google, GitHub, AWS Builder ID, and organizational IAM Identity Center, including device flow for remote terminals. `kiro-cli login`, `logout`, and `whoami --format json` are the official management surface.

API-key authentication uses `KIRO_API_KEY` and is officially limited to non-interactive operation. Credential precedence is active browser session, then `KIRO_API_KEY`, then interactive sign-in. RimZ must never read, copy, or log the API key or undocumented browser credential storage.

Use `whoami --format json` as the candidate account probe, but capture its JSON across Builder ID, social, Identity Center, API-key, expired, and logged-out states before defining a typed parser. The documentation promises structured output but does not publish its schema or stability.

One state is already captured: on 2.12.1 with no session, `kiro-cli whoami --format json` prints `{"account":null}` and exits `0`. That pins the logged-out shape (a null `account` field), enough to detect signed-out, but the signed-in envelope — plan, method, expiry, rate-limit windows — still needs a live capture before a typed probe is safe.

## Models, context, credits, and quota

`kiro-cli chat --list-models --format json` is the candidate model-discovery surface. The active choice persists in `~/.kiro/settings/cli.json` and may be changed by `/model`; launch effort levels are `low`, `medium`, `high`, `xhigh`, and `max`.

The rolling model page is the authoritative catalog of IDs, context-window sizes, region/plan availability, and credit multipliers. Do not duplicate that table in code from this reference: refresh it into RimZ's pricing data at implementation time. `Auto` may route dynamically and has no single fixed model identity or context window.

Kiro exposes context usage interactively through `/context show`, including a percentage and category breakdown. It exposes plan credits and remaining usage through `/usage`. Neither page documents JSON output, a statusline command, a local usage file, an API endpoint, or hook fields for these values. Pane reads remain outside producer enrichment, so a first adapter reports context and live credits as unavailable.

Kiro bills in credits rather than raw token dollars. A credit is a metered unit of work; complex prompts can consume multiple credits, and model multipliers scale consumption. Plan credits reset on the billing cycle, prepaid add-on credits are consumed after plan credits and expire separately. Without a machine-readable per-session usage source, RimZ cannot derive accurate session spend from token counts or model multipliers alone.

## Configuration and trust summary

Relevant roots under `${KIRO_HOME:-~/.kiro}` and the workspace are:

```text
~/.kiro/
├── agents/                         # user v3 profiles
├── hooks/                          # user v3 hooks
├── settings/
│   ├── cli.json                    # user CLI settings
│   ├── mcp.json                    # user MCP servers
│   └── permissions.yaml            # user v3 permissions
├── sessions/                       # v3/ACP session state; schema varies by transport
└── workspace-roots/<hash>/
    └── permissions.yaml            # per-user workspace decisions

<workspace>/.kiro/
├── agents/                         # trusted workspace profiles
├── hooks/                          # executable workspace hooks
├── settings/mcp.json               # workspace MCP servers
├── skills/                         # loadable instructions/resources
└── steering/                       # workspace instructions
```

The trust hash must cover every project-controlled value that can execute a command, load code/instructions, expand credentials into a child, or relax behavior: hooks, agents, MCP servers, skills, steering, specs that drive tool execution, and related settings. Permission decisions live outside the repository, but the preflight still validates that effective policy can satisfy the requested profile.

## Implementation checklist and live-verification gaps

Before declaring any Kiro capability supported:

1. Pin the exact `kiro-cli --version`, verify `--v3`, and archive `--help-all` plus `--v3 --help` output.
2. Install a temporary user command hook and capture redacted stdin, cwd, environment names, stdout handling, exit handling, timeout behavior, ordering, and process ancestry for every v3 trigger.
3. Prove a stable root session ID across `SessionStart`, prompt, tool, stop, resume, `/chat new`, `/rewind`, and process exit.
4. Capture success, model/API failure, user cancellation, rate limit, permission denial, and process death; only then decide whether `Stop` is always clean.
5. Capture canonical tool names and arguments for reads, writes, deletes, patches, shell, web, MCP, spec, questions, plan transitions, and subagent delegation.
6. Verify user-hook discovery under `KIRO_HOME`, workspace-hook trust, duplicate hook behavior, hot reload, and safe structured install/uninstall.
7. Verify whether permission, question, and plan dialogs emit any documented hook or ACP request before enabling `awaiting_input`.
8. Capture the v3 session store and `/rewind` lineage without binding to undocumented fields; keep transcript parsing disabled until a stable schema exists.
9. Run `kiro-cli acp` against the current ACP spec and capture permission requests, tool brackets, child sessions, compaction, cancellation, terminal output, and errors before using it for supervised runs.
10. Capture `whoami --format json`, `--list-models --format json`, and diagnostics across auth/account states with secrets redacted.
11. Verify whether any official machine-readable context, token, credit, quota, or per-session spend surface has appeared; otherwise keep those capabilities false.
12. Test macOS, Linux glibc and musl builds, Windows, tmux, Zellij, SSH/device login, paths with spaces, `KIRO_HOME`, and auto-update drift.

Known upstream gaps that block full parity are: no published v3 hook input schema; no stock-TUI permission/question/plan event; no session-end or compaction hook; no child lifecycle hooks; no published v3 transcript schema; no machine-readable live context/usage contract; and no documented v3 supervised mode. Keep these gaps explicit in the descriptor rather than papering them over with terminal scraping or older-engine behavior.

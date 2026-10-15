# TRAE CLI protocol reference

This page records the official `traecli` surface for contributors preparing an integration. It makes no claim about RimZ support. The [adapter playbook](../../contributing/agent-adapters.md) owns the integration sequence. The [externals index](../README.md#agent-adapters) lists this as a reference-only surface.

## Scope and evidence

Refresh baseline: the official Chinese TRAE CLI 2.0 documentation, read 2026-10-15. These rolling pages identify a product generation, without pinning a binary version or commit. No executable, hook payload, or provider-owned session was inspected. Documented forms remain untested. Gaps below distinguish missing contracts from conflicting documentation.

The [overview](https://docs.trae.cn/cli_about-trae-code-cli-2) describes a terminal coding agent with interactive TUI, noninteractive execution, and ACP. It restricts CLI 2.0 access to TRAE Enterprise flagship-plan customers. Access terms for other editions were not assessed. This page does not cover TRAE IDE or the separate `bytedance/trae-agent` project.

## Upstream sources

To refresh this unversioned baseline, re-fetch these pages and compare their CLI claims with this reference. Update the read date and index together. A binary-backed refresh must also record the build version and separate captures from documentation claims.

| Surface | Source |
| --- | --- |
| Product and edition | [TRAE CLI 2.0 overview](https://docs.trae.cn/cli_about-trae-code-cli-2) |
| Installation, launch, login | [Quick start](https://docs.trae.cn/cli_get-started-with-trae-code-cli-2) |
| Arguments, resume, fork, exec | [Command-line parameters](https://docs.trae.cn/cli_command-line-parameters) |
| Sandbox and project trust | [Permissions and sandbox](https://docs.trae.cn/cli_permission-and-sandbox) |
| Session, context, hooks, delegation UI | [Slash commands](https://docs.trae.cn/cli_slash-command) |
| External client launch | [Agent Client Protocol](https://docs.trae.cn/cli_agent-client-protocol) |
| Configuration | [Configuration file reference](https://docs.trae.cn/cli_config-file) |
| Environment | [Environment variables](https://docs.trae.cn/cli_environment-variables) |
| Compaction and diagnostics | [Status and troubleshooting](https://docs.trae.cn/cli_status-indication-and-troubleshooting) |
| Extensions | [Plugins, skills, and MCP](https://docs.trae.cn/cli_tools-and-extensions) |
| IDE-only hook contract | [IDE hook configuration](https://docs.trae.cn/ide_hook-configuration-reference) |

## Installation and launch

The [quick start](https://docs.trae.cn/cli_get-started-with-trae-code-cli-2) supports macOS, Linux, and Windows, with WSL2 recommended on Windows. It publishes installers at `https://trae.cn/trae-cli/install_v2.sh` and `https://trae.cn/trae-cli/install_v2.ps1`.

| Documented invocation | Effect |
| --- | --- |
| `traecli` | Open the TUI in the current directory. |
| `traecli --cd /path/to/project` | Open the TUI with a selected working directory. |
| `traecli "Explain this repository"` | Open the TUI with an initial task. |

Initial task submission needs a live check. Leading-dash prompts and separator handling have no contract in the cited launch examples.

## Authentication and configuration

The [quick start](https://docs.trae.cn/cli_get-started-with-trae-code-cli-2) describes TUI-guided login, including custom domains. It documents `traecli login status`, `traecli logout`, and PAT login through `traecli login --with-trae-pat`. The cited page does not specify credential storage, refresh behavior, or account-identity fields.

[Configuration](https://docs.trae.cn/cli_config-file) defaults to `~/.trae/traecli.toml`. Its examples are a minimal subset, not a complete schema or proof of shipped defaults.

| Example key | Documented values or purpose |
| --- | --- |
| `model` | Default model selection. |
| `approval_policy` | `untrusted`, `on-failure`, `on-request`, `never`. |
| `default_permissions` | `:read-only`, `:workspace`, `:danger-full-access`. Colon-prefixed configuration values. |
| `trae_login_base_url` | Custom login domain. |
| `projects."/path/to/project".trust_level` | Example value `trusted`. |
| `mcp_servers.<name>` | MCP server configuration, with `command` and `args` examples. |

The [environment reference](https://docs.trae.cn/cli_environment-variables) lists:

| Variable | Meaning |
| --- | --- |
| `TRAE_HOME` | Override CLI configuration and runtime directory. |
| `TRAECLI_PERSONAL_ACCESS_TOKEN` | PAT login token. |
| `TRAECLI_HOST` | API base URL. |
| `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY` | Proxy configuration. |

## Launch controls and permissions

Selected integration controls from [Command-line parameters](https://docs.trae.cn/cli_command-line-parameters):

| Argument | Values or effect |
| --- | --- |
| `-m`, `--model` | Model selection. |
| `-p`, `--profile` | Named configuration. |
| `-s`, `--sandbox` | `read-only`, `workspace-write`, `danger-full-access`. |
| `-a`, `--ask-for-approval` | `never`, `on-request`, `untrusted`. The source's interactive table adds `on-failure`. |
| `--permission-mode` | `default`, `auto`, `bypass_permissions`. |
| `--add-dir` | Additional writable directory, repeatable. |
| `-y`, `--dangerously-bypass-approvals-and-sandbox`, `--yolo` | Disable approvals and sandbox. |
| `-c`, `--config` | Repeatable `key=value` override. |
| `--allowed-tool`, `--disallowed-tool` | Session tool controls. |
| `--no-alt-screen` | Inline TUI. |
| `-C`, `--cd` | Working directory. |
| `-V`, `--version` | Print version. |

The command page's common and interactive approval tables disagree. The [configuration example](https://docs.trae.cn/cli_config-file) also includes `on-failure`. Acceptance outside interactive mode needs a binary check.

[Permissions and sandbox](https://docs.trae.cn/cli_permission-and-sandbox) describes three choices under `/permissions`:

| TUI mode | Boundary |
| --- | --- |
| Workspace Edit | Edit workspace files. Operations outside that scope require approval. |
| Auto-Review | Workspace Edit sandbox scope, with model review for key operations. |
| Full Access | Remove sandbox and approval restrictions. |

The cited pages do not specify the default mode or map TUI labels to `--permission-mode` values. Precedence between `--permission-mode`, `-a`, `-s`, and `-y` is unspecified. No plan-mode argument is documented in these sources.

[Slash commands](https://docs.trae.cn/cli_slash-command) also lists `/approval` to view or adjust approvals.

## Session identity and persistence

[Command-line parameters](https://docs.trae.cn/cli_command-line-parameters) documents:

| Form | Selection |
| --- | --- |
| `traecli --resume[=SESSION_ID]` | ID, or latest session when omitted. |
| `traecli resume [SESSION_ID] [--last] [--all]` | ID or picker. `--last` selects latest. `--all` removes directory filtering. |
| `traecli fork [SESSION_ID] [--last] [--all]` | Fork selection with the same documented options. |

[Slash commands](https://docs.trae.cn/cli_slash-command) also lists `/new`, `/resume`, and `/fork [name]`.

The cited CLI pages do not specify ID format, discovery, fork lineage, or ID changes after `/new` or compaction. They also leave the session-directory layout and transcript schema unspecified.

## Hooks and the decision channel

CLI hooks exist. [Slash commands](https://docs.trae.cn/cli_slash-command) lists `/hooks`. [Permissions and sandbox](https://docs.trae.cn/cli_permission-and-sandbox) gates project configuration, hooks, and execution policies until the directory is trusted.

For the separate ACP server mode, see [hook trust controls](#noninteractive-execution-and-acp) below.

The detailed [hook configuration reference](https://docs.trae.cn/ide_hook-configuration-reference) belongs to the IDE documentation. Its event catalog, hook paths, stdin fields, stdout decisions, and exit codes are not verified CLI contracts. Do not copy them into a CLI installer or decoder without CLI evidence.

| CLI contract still unverified | Evidence needed |
| --- | --- |
| Configuration paths, merge order, enablement, trust storage | CLI-specific reference or versioned implementation, checked against an isolated configuration. |
| Event catalog, payloads, session and process attribution | Redacted CLI hook captures, including resume and fork. |
| Synchronous blocking and approval behavior | Captures at the native prompt, including allow, deny, cancel, and error paths. |
| Neutral stdout and exit status | CLI-specific proof for each blocking hook. Empty exit-0 output is unverified. |
| Timeouts, cancellation, hook execution environment | CLI contract and controlled captures. |

## Noninteractive execution and ACP

[Command-line parameters](https://docs.trae.cn/cli_command-line-parameters) documents these exec controls:

| Form or option | Effect |
| --- | --- |
| `traecli exec [OPTIONS] [PROMPT]` | Noninteractive run. |
| `--json` | JSONL events on stdout. |
| `-o`, `--output-last-message` | Final message file. |
| `--output-schema` | JSON Schema constraint. |
| `--ephemeral` | No session files. |
| `--ignore-user-config` | Ignore user `traecli.toml`. |

The command page says most prompt-taking commands read stdin when the prompt is omitted or `-`. It does not identify every applicable command. JSONL fields, session identification, completion/error records, and exit-code semantics are unspecified. Streaming JSONL alone does not establish a native TUI lifecycle contract.

The [ACP reference](https://docs.trae.cn/cli_agent-client-protocol) gives two server launch forms: `traecli acp serve` and `traecli --acp`. The ACP client normally starts this server as a subprocess. Global options precede `acp serve`. Server options follow it.

| ACP control | Documented meaning |
| --- | --- |
| Global `--profile` | `$TRAE_HOME/<PROFILE>.traecli.toml`, for example `~/.trae/work.traecli.toml`. |
| Global `--permission-mode` | Initial `default`, `auto`, or `bypass_permissions` mode. |
| Global `-c` | TOML-parsed overrides, including `model` and `model_reasoning_effort`. |
| Server `-y`, `--yolo` | Disable approvals and sandbox. Initial mode equals `bypass_permissions`. |
| Server `--dangerously-bypass-hook-trust` | Skip persistent trust checks for enabled hooks this run. Does not enable hooks. |

The source gives `traecli --permission-mode auto acp serve` as an example. Option parity with `--acp` and TUI acceptance of the hook-trust flag need binary checks.

The command reference describes profiles inside `traecli.toml`, while the ACP page names separate files. Profile lookup across modes remains unresolved.

The cited ACP page does not specify negotiation, session loading, notifications, approval payloads, or transport framing. ACP serves an external client. Choosing it would require a separate integration decision about the native pane UI.

## Other integration surfaces

[Command-line parameters](https://docs.trae.cn/cli_command-line-parameters) also lists these transports:

| Surface | Documented role |
| --- | --- |
| `app-server` | Server for remote connections. |
| Interactive `--remote <ADDR>` | Connect to a remote app server, for example `ws://host:port`. |
| `mcp-server` | Expose the CLI as a stdio MCP server for external tools. |

Their lifecycle and context payloads are unspecified there. App-server context enrichment needs investigation before it can inform the worksheet.

The same source lists worktree isolation, feature switches, review, diff application, sandbox commands, diagnostics, and version/update controls. Consult that page for options outside this reference's integration subset.

## Context, usage, and native delegation

[Slash commands](https://docs.trae.cn/cli_slash-command) lists these selected UI surfaces:

| Command | Documented purpose |
| --- | --- |
| `/status` | Model, working directory, permissions, token usage. |
| `/context` | Current context use. |
| `/compact` | Compress conversation context. The source does not specify trailing instructions. |
| `/model` | Model and reasoning-effort selection. |
| `/agents`, `/agent-new` | Manage agent roles and create a role definition. |
| `/subagents` | View and switch subagent threads in the current session. |

The cited pages supply no machine-readable schema for context gauges, token categories, per-turn accounting, costs, or quota APIs.

[Status and troubleshooting](https://docs.trae.cn/cli_status-indication-and-troubleshooting) documents automatic compaction for long conversations, without a threshold. It also documents `traecli doctor`, `traecli doctor network`, and the default TUI log directory `~/.trae/log/`. It does not specify whether `TRAE_HOME` relocates logs. Log filenames depend on the CLI version.

[Plugins, skills, and MCP](https://docs.trae.cn/cli_tools-and-extensions) documents extension management through `/plugins`, `/skills`, and `/mcp`. That page does not specify whether plugins can carry hooks. The cited sources leave child identity, lifecycle events, transcript routing, and delegation-disable controls unspecified.

## Autonomous tasks

[Slash commands](https://docs.trae.cn/cli_slash-command) documents goals and repeated tasks:

| Command | Documented purpose |
| --- | --- |
| `/goal <objective>` | Set a long-term goal that advances while the agent is idle. |
| `/goal pause`, `/goal resume` | Pause or resume that goal. |
| `/loop [interval] <prompt>` | Repeat a task, minimum interval one minute. |

Their hook behavior and durable turn records need captures alongside ordinary prompted turns.

## Evidence required before implementation

These gaps describe this reference's evidence boundary. They do not establish absent upstream features. Before collecting evidence, confirm account entitlement and obtain approval from the person requesting the integration. Installation, provider execution, and captures need that approval.

| Evidence to collect | [Playbook worksheet](../../contributing/agent-adapters.md#step-2--map-the-protocol-onto-the-model) rows it informs |
| --- | --- |
| `traecli --version`, help, then prompt, permission, profile, resume, fork, and compaction checks | Launch surface, session identity. Include the plan-mode gap. |
| CLI hook configuration, event captures, and neutral replies at the native approval UI | Lifecycle signals, blocking asks, tool vocabularies. |
| Redacted provider-owned sessions, including errors, cancellation, usage, and children | Session identity, context sources, account and spend. |
| Exec, ACP, app-server, and MCP server contracts or captures, assessed separately | Context sources and integration concerns. |

Use the evidence to fill all worksheet rows before claiming coverage.

The [integration-shape gate](../../contributing/agent-adapters.md#step-0--confirm-a-built-in-is-the-right-shape) decides between a built-in and an experimental [process plugin](../../reference/agent-plugins.md). It needs the native configuration contract and any required protocol beyond the canonical plugin wire. This reference leaves that decision open.

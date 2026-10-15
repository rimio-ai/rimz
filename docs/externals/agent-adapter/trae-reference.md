# TRAE CLI protocol reference

This page records the official `traecli` surface for contributors preparing an integration. It makes no claim about RimZ support. The [adapter playbook](../../contributing/agent-adapters.md) owns the integration sequence, and [agent-support.md](../../reference/agent-support.md) owns support status.

## Scope and evidence

Refresh baseline: the official Chinese TRAE CLI 2.0 documentation, read 2026-10-15. These rolling pages identify a product generation, without pinning a binary version or commit. No executable, hook payload, or provider-owned session was inspected. All positive protocol claims below come from documentation.

The [overview](https://docs.trae.cn/cli_about-trae-code-cli-2) describes a terminal coding agent with interactive TUI, noninteractive execution, and ACP. It restricts CLI 2.0 access to TRAE Enterprise flagship-plan customers. That restriction applies to this documented edition. This page does not cover TRAE IDE or the separate `bytedance/trae-agent` project.

## Upstream sources

All sources below are official pages read on the refresh date. The IDE reference is included only to mark its scope boundary.

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
| Extensions | [Plugins, skills, and MCP](https://docs.trae.cn/cli_tools-and-extensions) |
| IDE-only hook contract | [IDE hook configuration](https://docs.trae.cn/ide_hook-configuration-reference) |

## Installation and launch

The [quick start](https://docs.trae.cn/cli_get-started-with-trae-code-cli-2) supports macOS, Linux, and Windows, with WSL2 recommended on Windows. It publishes installers at `https://trae.cn/trae-cli/install_v2.sh` and `https://trae.cn/trae-cli/install_v2.ps1`. Neither installer was executed for this reference.

| Documented invocation | Effect |
| --- | --- |
| `traecli` | Open the TUI in the current directory. |
| `traecli --cd /path/to/project` | Open the TUI with a selected working directory. |
| `traecli "Explain this repository"` | Open the TUI with an initial task. |

These forms are documented, without a live prompt-submission check. Prompts beginning with `-` and separator handling remain unverified.

## Launch controls and permissions

Selected integration controls from [Command-line parameters](https://docs.trae.cn/cli_command-line-parameters):

| Argument | Values or effect |
| --- | --- |
| `-m`, `--model` | Model selection. |
| `-p`, `--profile` | Named configuration. |
| `-s`, `--sandbox` | `read-only`, `workspace-write`, `danger-full-access`. |
| `-a`, `--ask-for-approval` | `never`, `on-request`, `untrusted`. Interactive table also lists `on-failure`. |
| `--permission-mode` | `default`, `auto`, `bypass_permissions`. |
| `--add-dir` | Additional writable directory, repeatable. |
| `-y`, `--dangerously-bypass-approvals-and-sandbox`, `--yolo` | Disable approvals and sandbox. |
| `-c`, `--config` | Repeatable `key=value` override. |
| `--allowed-tool`, `--disallowed-tool` | Session tool controls. |
| `--no-alt-screen` | Inline TUI. |

The approval lists disagree. Acceptance of `on-failure` outside interactive mode remains unverified.

[Permissions and sandbox](https://docs.trae.cn/cli_permission-and-sandbox) describes three TUI choices:

| TUI mode | Boundary |
| --- | --- |
| Workspace Edit | Edit workspace files. Operations outside that scope require approval. |
| Auto-Review | Workspace Edit sandbox scope, with model review for key operations. |
| Full Access | Remove sandbox and approval restrictions. |

The page does not map these labels to every CLI flag combination or specify precedence between presets and individual controls. No RimZ permission mapping follows from these labels alone.

## Session identity and persistence

[Command-line parameters](https://docs.trae.cn/cli_command-line-parameters) documents:

| Form | Selection |
| --- | --- |
| `traecli --resume[=SESSION_ID]` | ID, or latest session when omitted. |
| `traecli resume [SESSION_ID] [--last] [--all]` | ID or picker. `--last` selects latest. `--all` removes directory filtering. |
| `traecli fork [SESSION_ID] [--last] [--all]` | Fork selection with the same documented options. |

The [slash reference](https://docs.trae.cn/cli_slash-command) also lists `/new`, `/resume`, and `/fork [name]`.

Session ID format, discovery, fork lineage, and ID changes after `/new` or compaction remain unverified. The cited CLI pages provide no validated session-directory layout or transcript schema. No compatibility with another provider's rollout format is established here.

## Hooks and the decision channel

CLI hooks exist. The [slash reference](https://docs.trae.cn/cli_slash-command) lists `/hooks`. [Project trust](https://docs.trae.cn/cli_permission-and-sandbox) gates project configuration, hooks, and execution policies until the directory is trusted.

The [ACP reference](https://docs.trae.cn/cli_agent-client-protocol) documents `--dangerously-bypass-hook-trust` after `acp serve`. For that run, it skips persistent trust checks for enabled hooks. It does not enable hooks. This evidence does not establish that the TUI accepts the same flag.

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

JSONL event fields, session identification, completion/error records, and exit-code semantics remain unverified. Streaming JSONL alone does not establish a native TUI lifecycle contract.

The [ACP reference](https://docs.trae.cn/cli_agent-client-protocol) gives two server launch forms: `traecli acp serve` and `traecli --acp`. Global options precede `acp serve`. Server options follow it.

| ACP control | Documented meaning |
| --- | --- |
| `--profile` | `$TRAE_HOME/<PROFILE>.traecli.toml`, for example `~/.trae/work.traecli.toml`. |
| `--permission-mode` | Initial `default`, `auto`, or `bypass_permissions` mode. |
| `-c` | TOML-parsed overrides, including `model` and `model_reasoning_effort`. |
| Server `-y`, `--yolo` | Disable approvals and sandbox. Initial mode equals `bypass_permissions`. |

The command reference describes profiles inside `traecli.toml`, while the ACP page names separate files. Profile lookup across modes remains unresolved.

ACP negotiation, session loading, notifications, approval payloads, and transport framing were not validated. ACP serves an external client. Choosing it would require a separate integration decision about the native pane UI.

## Authentication and configuration

The [quick start](https://docs.trae.cn/cli_get-started-with-trae-code-cli-2) describes TUI-guided login, including custom domains. It documents `traecli login status`, `traecli logout`, and PAT login through `traecli login --with-trae-pat`. Credential storage, refresh behavior, and account-identity fields remain unverified.

[Configuration](https://docs.trae.cn/cli_config-file) defaults to `~/.trae/traecli.toml`. Its examples are a minimal subset, not a complete schema or proof of shipped defaults.

| Example key | Documented values or purpose |
| --- | --- |
| `model` | Default model selection. |
| `approval_policy` | `untrusted`, `on-failure`, `on-request`, `never`. |
| `default_permissions` | `:read-only`, `:workspace`, `:danger-full-access`. These spellings differ from sandbox arguments. |
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

That page does not document `TRAECLI_HOME`. This reference uses `TRAE_HOME` and makes no claim about undocumented aliases.

## Context, usage, and native delegation

The [slash reference](https://docs.trae.cn/cli_slash-command) lists these UI surfaces:

| Command | Documented purpose |
| --- | --- |
| `/status` | Model, working directory, permissions, token usage. |
| `/context` | Current context use. |
| `/compact` | Compress conversation context. Trailing instructions remain unverified. |
| `/model` | Model and reasoning-effort selection. |
| `/agents`, `/agent-new` | Manage agent roles and create a role definition. |
| `/subagents` | View and switch subagent threads in the current session. |

Machine-readable context gauges, token categories, per-turn accounting, dollar costs, and quota APIs remain unverified. These UI commands supply no validated record schema.

[Plugins, skills, and MCP](https://docs.trae.cn/cli_tools-and-extensions) documents extension management through `/plugins`, `/skills`, and `/mcp`. It supplies no hook plugin schema. Native delegation exists as a UI surface, but child identity, lifecycle events, transcript routing, and delegation-disable controls remain unverified.

## Evidence required before implementation

These gaps describe this reference's evidence boundary. They do not establish absent upstream features.

1. Pin an authorized CLI build and edition, then record its version and relevant help output.
2. Verify initial prompt submission, permission precedence, profile resolution, resume, fork, and manual compaction under an isolated home.
3. Capture native CLI hooks and prove neutral replies at the native approval UI.
4. Inspect redacted provider-owned sessions for identity, record ordering, usage, cancellation, errors, and subagent routing.
5. Capture exec and ACP wires separately before relying on their event schemas.

Provider execution and captures require separate authorization. This reference leaves the built-in versus process-plugin decision open.

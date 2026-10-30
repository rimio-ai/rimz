# Qoder adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The raw upstream protocol is in [qoder-reference.md](../../externals/agent-adapter/qoder-reference.md), and the user-facing limits are in [agent-support.md](../../reference/agent-support.md#qoder).

Qoder is a built-in adapter, `crates/rimz/src/agents/adapters/qoder/mod.rs::QoderAdapter`, with its spec in `crates/rimz/src/agents/adapters/qoder/mod.rs::QODER_DESCRIPTOR`. Qoder runs in its stock interactive pane, and native command hooks in Qoder's `settings.json` are the adapter's only source: they carry the lifecycle edges and the blocking asks. RimZ reads no transcript, context, account, or spend for Qoder. Only the registry names the concrete adapter, and other code reads it through the provider-neutral capabilities.

The upstream evidence is thinner than for most adapters. The reference records Qoder CLI 1.1.72 from probes run without a login, plus traces of the installed bundle. Model turns, tool calls, asks, errors, and children have never been observed under a login, so every hook mapping below other than session start, session end, and the idle notification rests on the bundle trace ([evidence boundary](../../externals/agent-adapter/qoder-reference.md#evidence-boundary-and-sources)).

| File | Owns |
| --- | --- |
| `mod.rs` | the spec, the hook catalog `QODER_HOOKS`, hook decoding, and the lifecycle mapping |
| `payloads.rs` | the consumed fields of the Claude-shaped hook envelope (`QoderHook`) and the `StopFailure` error enum |
| `install.rs` | the settings path, the managed hook spec, and the version floor |
| `settings.rs` | the launch refusals: hook-disabling settings and alternate config roots |
| `folder_trust.rs` | the folder-trust decision and the grant preview |
| `tests.rs` | unit tests for all of the above |

## Launch

RimZ launches `qodercli`, never the `qoder` dispatcher. `qoder` spawns an inner Node CLI, so a hook's `$PPID` under it is that child, while under `qodercli` it is the pane's own process ([upstream](../../externals/agent-adapter/qoder-reference.md#launch-and-configuration)). The installed hook command stamps that pid as the runtime owner ([Install](#install)).

| Launch | Argv |
| --- | --- |
| initial prompt | `qodercli -i <prompt>`, flag and value as separate arguments |
| resume | `qodercli --resume <id>`. A bare RimZ resume opens the native picker, `qodercli --resume` |
| fork | `qodercli --resume <id> --fork-session` |
| permission mode | Ask `--permission-mode default`, Auto `--permission-mode auto`, Plan `--permission-mode plan`, Yolo `--dangerously-skip-permissions` |
| model preset | `--model <model>` |
| effort preset | `--reasoning-effort <effort>` |
| launch reminders | `--append-system-prompt <text>` (`append_system_text_channel`) |
| manual compaction | `/compact <instructions>` typed into the pane. The instructions follow the command on one line (`CompactInstruction::Trailing`) |

Plan mode is an upstream compatibility value. `--permission-mode plan` parses in 1.1.72 but is absent from its help, and it means default permissions plus Qoder's Plan work state, so a hook's `permission_mode` field still reads `default`. Upstream also drops any non-default mode back to `default` in an untrusted folder ([permissions](../../externals/agent-adapter/qoder-reference.md#permissions-and-working-modes)). Both behaviours can change upstream without notice.

Some launch text and profile settings never reach Qoder:

| Launch input | What happens |
| --- | --- |
| system-prompt replacement | refused: `presets` declares no system-prompt matcher |
| auto-compact and fast | skipped with a launch warning: `presets` declares no switches for them |
| headless command | refused: `launch.headless` is `None`, so Qoder cannot run a headless judge or check |
| hook-context replies, including the deadline stop rung | not sent: `hook_context` is `None`, so a supervised run's stop rung goes through the pane (`crates/rimz/src/harness/deadline.rs::stop_channel`) |
| per-turn prompt context | not sent: `prompt_context` is `false` |
| a profile's tool rules | dropped with a launch warning (`ToolRules::Unsupported`) |
| a profile's `skills` list on the host | warns and runs unrestricted (`HostSkills::Unsupported`). Under scoped isolation the view applies the list ([sandbox.md](../sandbox.md)) |
| the `sandbox` isolation level | warns and launches without a provider sandbox: `has_native_sandbox` keeps its default `false` |

### Version floor

Qoder 1.1.72 or newer is required (`crates/rimz/src/agents/adapters/qoder/install.rs::MIN_VERSION`). It is the only bundle probed, and the first whose closed hook-event enum accepts `QueryEnd` ([hook events](../../externals/agent-adapter/qoder-reference.md#hook-events-and-payloads)). An older readable version refuses both the launch and `rimz hooks install` with the fix `qodercli update` (`crates/rimz/src/agents/adapters/qoder/install.rs::QoderAdapter::install_blocker`). An unreadable version abstains.

### Process identity

Pane classification and ancestor matching use two different name lists:

| List | Names | Used for |
| --- | --- | --- |
| `bin_names` | `qodercli`, `qoder` | recognizing a hand-launched pane by its command |
| `process_names` | `qodercli`, `node` | the ancestor-walk fallback that finds the agent process |

Node 26 reports the kernel `comm` `node-MainThread`, which `runs_as` does not match against `node`. The fallback therefore cannot recognize a Qoder process under Node 26 by name. Installed hooks never reach the fallback, because they stamp the pid themselves, so the gap stays unfixed in the shared process matching.

### Launch preconditions and folder trust

`crates/rimz/src/agents/adapters/qoder/settings.rs::hook_blocker` refuses a launch whose configuration would keep RimZ's hooks from running, and each refusal names the setting, the file, and the fix:

| Source | Refuses when |
| --- | --- |
| `QODER_CLI_HOME` or `QODER_CONFIG_DIR_NAME` in the launch env | set at all. Either one selects a config root other than the one RimZ installed into. The fix is to unset it and use `QODER_CONFIG_DIR` |
| `--config-dir` or `--config-dir=<path>` in the launch argv | present before `--`. The value of `-i` or `--prompt-interactive` is skipped, so prompt text that contains `--config-dir` does not count |
| `disableAllHooks` | `true` in the highest settings layer that sets it |
| `hooksConfig.enabled` | `false` in the highest settings layer that sets it |

The settings layers are, lowest precedence first, `<config home>/settings.json`, `<cwd>/.qoder/settings.json`, and `<cwd>/.qoder/settings.local.json`. These reads parse JSONC and skip a missing or malformed file without rewriting it.

Folder trust gates every hook. In an untrusted folder Qoder runs none of its user or `--settings` hooks, so the session's first `SessionStart` is lost and the card stays a launch placeholder ([permissions](../../externals/agent-adapter/qoder-reference.md#permissions-and-working-modes)). `crates/rimz/src/agents/adapters/qoder/folder_trust.rs::folder_trust` models Qoder's decision through `InstallationCapability::folder_trust`. It reads the user `settings.json` (JSONC) and returns `Decided` when one of these holds:

- `security.folderTrust.enabled` is `false`
- an entry of `permissions.trustDirectories` or `permissions.additionalDirectories`, canonicalized, is the canonical cwd or a component-wise ancestor of it

Anything else is `Undecided`, with a grant preview. The adapter has no `Restricted` arm, because the reference records no saved-distrust state for Qoder. The grant appends the canonical cwd, never the repository root, to `permissions.trustDirectories`, keeps every other key, and rewrites the file as strict JSON. A file that does not parse yields no preview and a repair message instead. A grant is published by `rimz trust grant --agents qoder`, by an attended `rimz start` once the user accepts its folder-trust offer, or automatically by a fresh eligible `empty-dir` profile launch when the grantable key is the resolved empty root.

Whether an undecided folder warns or refuses depends on who launched, as tabled in [fleet.md](../harness/fleet.md#every-path-that-builds-a-launch-layout): a human launch, team command, or resident loop warns, while an agent-started launch and a subagent resume refuse. A supervised pane refuses an undecided launch directory too (`crates/rimz/src/agents/mod.rs::preflight_launch_dir`). The grant covers only the exact folder and its descendants, so a fresh team worktree needs its own grant unless an ancestor is already trusted.

## Hooks

### Install

Install merges into `<config home>/settings.json`, where the config home is `QODER_CONFIG_DIR` when it is non-empty, else `$HOME/.qoder` (`crates/rimz/src/agents/adapters/qoder/mod.rs::QoderAdapter::config_home`). Skills live in the same home under `skills/`. With neither variable set, install fails with a fix. The merge keeps user hooks and every other setting, reclaims RimZ-owned entries, and rewrites the canonical set, so a missing event is repaired on the next install ([adapter.md](./adapter.md#hook-install)).

Each handler runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source qoder` with a timeout of 2, in seconds (`crates/rimz/src/agents/adapters/qoder/install.rs::SPEC`). Default hook ingress accepts the stamped pid unchanged as the runtime owner. `PermissionRequest` installs synchronous, and install refuses a RimZ-marked entry for it that is async (`crates/rimz/src/agents/managed_json_hooks.rs::ManagedJsonHookSpec::reject_async_blocking`).

RimZ installs no `PreToolUse` hook, on purpose. Qoder 1.1.72 reads non-JSON text on `PreToolUse` stdout as allow and malformed JSON as deny, so a stray byte would decide a tool call ([hook replies](../../externals/agent-adapter/qoder-reference.md#hook-replies-and-approval-decisions)). Every RimZ reply is empty stdout, including for a malformed payload and an unknown event. The bundle trace shows empty stdout makes no decision on `PermissionRequest` and leaves Qoder's own prompt to the user. No logged-in approval prompt has confirmed it.

### Event mapping

| Native event | RimZ signal |
| --- | --- |
| `SessionStart` | registers, through the shared `crates/rimz/src/agents/hook_types.rs::SessionSource::session_start_signal`. Source `compact` closes a compaction instead. Sources `startup`, `clear`, and `new` mark the session `Fresh`, and `resume` does not |
| `UserPromptSubmit` | turn started, with the sanitized prompt as the card's prompt and task |
| `Stop` | turn ended, clean. `last_assistant_message` is the supervised run's final output |
| `StopFailure` | turn ended, errored. `error` decides the class: `rate_limit` is `paused_rate_limit`, `server_error` and `overloaded` are `paused_overloaded`, and any other value classifies by the label taken from `last_assistant_message`. `error_type` is never read |
| `QueryEnd` with `reason` `abort` | turn interrupted. Every other reason maps to nothing ([Turn endings](#turn-endings)) |
| `PostToolUse` | tool activity, with the spec's mutating and editing tool lists, keyed by `tool_use_id` |
| `PostToolUseFailure` | tool activity, never mutating or editing |
| `PermissionRequest` | wait, keyed by `tool_use_id`. `AskUserQuestion` is a question, `ExitPlanMode` a plan approval, and every other tool a permission ask, with the shared `permission_detail` line |
| `SubagentStart`, `SubagentStop` | child started and stopped ([Subagents](#subagents)) |
| `PreCompact`, `PostCompact` | compaction bracket. `PostCompact` carries the `auto` or `manual` trigger |
| `SessionEnd` | ended |
| `Notification` | installed and decoded for audit, with no lifecycle or ask signal |

The mutating tools are `Bash`, `PowerShell`, `BashInput`, `Monitor`, `Write`, `Edit`, and `NotebookEdit`, and the editing tools are `Write`, `Edit`, and `NotebookEdit`. Session death has no native hook: the exec wrapper derives it. The asks reach `rimz asks`, but `rimz answer` does not drive Qoder's dialog, so the user answers in the pane.

### Turn endings

A turn ends one of two native ways, and both are native signals. A completed query fires `Stop` and then `QueryEnd` with `end_turn`, which maps to nothing, so the verdict `Stop` set stands. Esc or Ctrl-C mid-turn fires no `Stop`, `StopFailure`, or `PostToolUseFailure`. Only the undocumented `QueryEnd`, which runs for every main-agent query, reports the abort ([hook events](../../externals/agent-adapter/qoder-reference.md#hook-events-and-payloads)).

The descriptor declares `Stop` as the native `turn_ended` hook. Conformance groups turn interruption with turn ending and takes `Stop` as the witness, so `QueryEnd` needs no lifecycle annotation of its own. It is in the classification corpus through `QODER_HOOKS`.

No turn id ties a `QueryEnd` to its prompt. A late or mismatched abort therefore moves a settled row to `idle`, because the [late-report rule](./model.md#late-turn-reports) applies only to reports that carry an id. The adapter leaves that shared rule as it is.

### Compaction

Qoder reports one compaction's end twice: `PostCompact`, then `SessionStart` with source `compact`. The first closes the bracket, and the second finds it closed, which the shared state machine ignores ([the compaction bracket](./model.md#the-compaction-bracket)). Compaction hooks carry no child identity.

### Subagents

Child hooks share the parent's `session_id` and add `agent_id` and `agent_type`. Every event routes by `agent_id`, falling back to `session_id`, while its context stays keyed to `session_id` (`HookRouting::split`). `SubagentStart` and `SubagentStop` need a child id and a parent id that are non-empty and distinct, and are quarantined otherwise (`crates/rimz/src/agents/identity.rs::resolve_subagent_identity`). An ordinary hook a child fires carries an `agent_id` that differs from `session_id`, so `crates/rimz/src/agents/identity.rs::resolve_root_identity` drops its lifecycle observation and the root's state is untouched.

A child shows its `agent_type` as its task. `SubagentStop` also decodes `last_assistant_message` as the event's final message. The card has no per-child tokens and no transcript replay.

## Context and transcript

No context, transcript, or local-session capability is declared. The bundle writes Claude-shaped JSONL, but it zeroes the token counts for ordinary Qoder models unless the model is custom or BYOK, or `EXPOSE_TOKEN_USAGE` is set. No transcript exists before login, and no logged-in schema has been captured ([sessions and transcripts](../../externals/agent-adapter/qoder-reference.md#sessions-and-transcripts)). A transcript integration needs captures first: the Claude parser cannot be assumed to fit.

Hooks inject nothing into the model. Qoder's hook replies accept `additionalContext`, but no logged-in probe has shown how the model consumes it or what origin the transcript records for it. A `statusLine` gauge is deferred too: upstream documents no field schema for its stdin, and a wrapper would rewrite the user's status line setting.

## Account, cost, and history

All three are unsupported. Qoder exposes no readable login, plan, or Credits balance to RimZ, and the SDK's quota queries are a separate contract that says nothing about local files. The account capability keeps its defaults, so a managed launch resolves `Unsupported` ([account scope](./providers.md#account-scope)). Named logins still work through `QODER_CONFIG_DIR`. Cost is unsupported both live and from history: ordinary token counts are hidden, and Credits have no established dollar conversion.

## Known gaps

`rimz coverage --wiring` prints the current wired, partial, and unsupported grid. These gaps wait on upstream evidence ([live-validation gaps](../../externals/agent-adapter/qoder-reference.md#live-validation-gaps)):

- Every logged-in emission: turn, tool, ask, error, interruption, compaction, and subagent hooks, and their timing against the native UI.
- Approval behaviour under a real prompt: empty-stdout neutrality, conflicting hook replies, and hook timeouts.
- How the model consumes `additionalContext`, the `statusLine` stdin schema, and a captured transcript with its usage semantics.
- Releases before 1.1.72, and how per-run `--settings` hooks merge with the user's.

These native surfaces are outside the adapter: `qoder hooks migrate`, worker-runtime hooks, Qoder's command sandbox, the ACP and SDK transports, and the `TaskCreated`, `TaskCompleted`, `TeammateIdle`, and `Setup` events.

# Muse adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. The upstream protocol and the 1.4.9 captures are in [muse-reference.md](../../externals/agent-adapter/muse-reference.md#149-capture), and the user-facing limits are in [agent-support.md](../../reference/agent-support.md#muse).

Muse Code is a built-in adapter, `crates/rimz/src/agents/adapters/muse/mod.rs::MuseAdapter`, with its spec in `crates/rimz/src/agents/adapters/muse/mod.rs::MUSE_DESCRIPTOR`. Managed command hooks are its only source: they carry the turn lifecycle, tool activity, tool approvals, and session endings. RimZ reads no transcript, context, account, or spend for Muse. Of the nineteen integration concerns, `turn_lifecycle`, `permission`, `session_end`, and `hook_install` are wired, and the rest are unsupported. The user-facing verdicts are Full state, Partial ask, and Unsupported live, history, account, and subagents.

The evidence is thin. Muse Code 1.4.9 is both the floor and the only version captured, and every capture ran Muse's offline `echo` provider, which echoes the prompt and calls no tools. A model tool call, a native approval, a model failure, a model-spawned child, compaction, and real usage have never been observed, so the mappings for them rest on the documented payload shapes ([1.4.9 capture](../../externals/agent-adapter/muse-reference.md#149-capture)).

| File | Owns |
| --- | --- |
| `mod.rs` | the spec, the coverage verdicts, the hook catalog `MUSE_HOOKS`, the child-thread ingress filter, and hook decoding |
| `payloads.rs` | the consumed fields of the hook envelope (`HookPayload`) |
| `install.rs` | the two-file install, the version floor `MIN_SUPPORTED_CLI`, and the env passthrough list `passthrough_names` |
| `tests.rs` | unit tests for all of the above |
| `fixtures/` | one payload per event: documented shapes for model tools, approval, and failure, and captured samples for the rest |

## Hooks

`MUSE_HOOKS` is the one list of installed events, and it drives both the install and the conformance classification corpus. Every event runs the same handler, `rimz hooks feed --source muse`, with no matcher and a timeout of 4 seconds (`crates/rimz/src/agents/adapters/muse/install.rs::render_hooks`). The command carries no `RIMZ_AGENT_PID=$PPID` stamp, so the hook owner is found another way ([Hook environment and owner](#hook-environment-and-owner)). Every reply is silent: empty stdout with exit 0 is Muse's documented neutral reply.

Only `Interrupt` installs async, because Muse 1.4.9 skips a synchronous `Interrupt` handler with an error. `PermissionRequest` installs synchronous, so its Waiting cannot arrive after the tool has already finished.

### Event mapping

`crates/rimz/src/agents/adapters/muse/mod.rs::MuseAdapter::decode_hook` maps each event:

| Native event | Lifecycle signal | Evidence |
| --- | --- | --- |
| `SessionStart` | through `SessionSource::session_start_signal`: `startup` and `clear` register, `compact` closes a compaction | 1.4.9 capture of `startup` and `clear`. No `compact` source was seen |
| `UserPromptSubmit` | `TurnStarted` with the prompt, and lazy registration | 1.4.9 capture |
| model `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | `ToolUsed` | documented shape only |
| user-shell `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | none | 1.4.9 capture of `!` shell input |
| `PermissionRequest` | `AwaitingInput(Permission)` | documented shape only |
| `Stop` | `TurnEnded`, not errored, with the final assistant message | 1.4.9 capture |
| `StopFailure` | `TurnEnded`, errored, with the final assistant message | documented shape only |
| `Interrupt` | `TurnInterrupted` | 1.4.9 capture of Escape |
| `SessionEnd` | `Ended` | 1.4.9 capture of `/quit`, `/clear`, and `/new` |

Every signal carries Muse's native `turn_id`, and `ToolUsed` also carries the native tool id. `Interrupt` and `Stop` both record the closed turn id, so a late tool report for that turn cannot reopen Running. Muse emits no `Stop` for an interrupted turn.

A tool id that starts with `user_shell_` marks a `!` shell action the user typed. It has no model turn around it, so the decoder returns the classification without a lifecycle signal and a resting card stays unchanged. For model tools, `shell` counts as mutating and no tool counts as editing (`ToolClassification` in the spec).

### Asks

A tool approval raises Waiting and routes attention to Muse's pane, where the user answers it. `PermissionRequest` carries no question text or options that RimZ extracts, so the ask has no `rimz answer` path. Clarifying questions have no captured hook and do not raise Waiting. Three things about approvals are unverified: that the neutral reply leaves Muse's native approval UI in place, whether `PermissionRequest` fires before or after Muse's approval judge decides, and how long the hook runs before the prompt renders.

### Child threads

Muse runs internal reminder children (`skill-reminder`, `goal-reminder`, `verify-reminder`) on ordinary turns, and they fire hooks too. `crates/rimz/src/agents/adapters/muse/mod.rs::MuseAdapter::hook_ingress` drops a hook before any store work when `MUSE_SESSION_ID` and `MUSE_THREAD_ID` are both set and differ, with the ignore reason `MuseChildThread`. The env pair decides because a child's payload `session_id` names the child, while `MUSE_SESSION_ID` stays the root's. Model-spawned children have never been captured, so RimZ projects no child cards.

### Events not installed

`SubagentStart` and `SubagentStop` would report only the internal children. The per-LLM-call hooks, `PostToolBatch`, the compaction hooks, and the delayed `Notification` are not installed either. Each one lacks a capture or a consumer.

## Install

`crates/rimz/src/agents/adapters/muse/install.rs::MuseManagedIntegration` writes two files in the Muse config home, `$XDG_CONFIG_HOME/muse` when that is set and non-empty, else `$HOME/.config/muse` (`crates/rimz/src/agents/adapters/muse/mod.rs::MuseAdapter::config_home`). The home must be absolute. [The hook command reference](../../reference/cli/hooks-trust.md#what-install-writes) lists the edits as a user sees them.

| File | What install writes |
| --- | --- |
| `rimz-hooks.json` | the whole file: the ten handlers from `MUSE_HOOKS`, with no ownership marker |
| `settings.json` | `managed_hooks_path`, the absolute path of `rimz-hooks.json`, and `managed_hooks_env_vars`, the names from `passthrough_names`. A new file also gets `schema_version: 1`. Every other key, user hooks included, survives a strict JSON round-trip |

Ownership is structural: `managed_hooks_path` naming RimZ's file is what makes the install RimZ's (`owns_settings`). No `_rimz_managed` or `_rimz_sync` marker is written, because the 1.4.9 capture shows Muse skips a hook group or handler that carries either key. The marker-based shared JSON installer therefore cannot serve Muse.

`install_at` writes the hook file first and the settings second (`crates/rimz/src/agents/settings_json.rs::commit_pair`). When the second write fails, it restores both originals, deletes a file that did not exist before, and reports any rollback failure with the write error.

Install refuses, and names the fix, in these cases:

| Condition | Why |
| --- | --- |
| `muse --version` is below 1.4.9, unavailable, or unparseable | 1.4.9 is the first captured version (`installed_cli_version`) |
| an existing `settings.json` without exactly `schema_version: 1` | Muse 1.4.9 refuses to start on such a file, so RimZ does not repair it |
| `managed_hooks_path` names another file | RimZ does not replace another managed hook source |
| neither `XDG_CONFIG_HOME` nor `HOME` is absolute | the config home cannot be resolved |

The refusal is also the install blocker, so detected installs (`rimz start`, `rimz setup`, a bare `rimz hooks install`) skip Muse, and `rimz hooks install muse` reports it ([adapter.md](./adapter.md#hook-install)).

Detection (`MuseManagedIntegration::installed`) requires `schema_version: 1`, `managed_hooks_path` naming RimZ's file, every name from `passthrough_names` in `managed_hooks_env_vars`, and `rimz-hooks.json` byte-equal to the canonical render. Any difference while RimZ's artifacts are present offers the install again as an upgrade. Uninstall removes the two settings only while `managed_hooks_path` still names RimZ's file, and keeps `schema_version` and every other key. It deletes `rimz-hooks.json` whether or not the settings name it.

Install reads only the user `settings.json`. RimZ never reads Muse's enterprise policy planes, so a passing detection does not prove that the hooks run under an enterprise policy. Whether a policy can override these two keys is not established upstream either. `muse config status` is Muse's own policy inspection command.

### Hook environment and owner

Muse strips the environment of every hook. A user hook gets only `HOME LANG LOGNAME MUSE_SESSION_ID MUSE_THREAD_ID PATH PWD SHELL TERM TMPDIR USER`, so the room identity is lost. A managed hook also inherits each variable named in `managed_hooks_env_vars`. The list passes inherited values through and assigns none.

`crates/rimz/src/agents/adapters/muse/install.rs::passthrough_names` builds the list from the shared hook-env constants `HOOK_ENV_NAMES` and `HOOK_ENV_EXTRA_NAMES` in `crates/rimz/src/agents/hook_types.rs`, plus `MUSE_SESSION_ID` and `MUSE_THREAD_ID`. It removes three names:

- `RIMZ_AGENT_PID` and `RIMZ_HOOK_OWNER_KIND`, which the feed writes into the hook frame itself (`crates/rimz/src/harness/hook_drain.rs::capture_ingress_env`)
- `SHELL`, which Muse already passes to every hook

The list therefore carries workspace, pane, run, launch, isolation, home, and runtime identity through ordinary hook ingress, with no Muse-specific recovery path. `HookCapability::hook_env_keys` returns the same list, so `crates/rimz/src/harness/hook_drain.rs::capture_env` copies the two Muse ids into the frame as well.

A name added to either shared constant changes the list. Detection then fails for every existing Muse install, which reads as an available upgrade and is offered again. A name Muse already passes to every hook belongs in the removals of `passthrough_names` instead, which re-offers nothing.

Without a pid stamp, `crates/rimz/src/cli/hooks/owner.rs::hook_agent_pid` walks the hook's ancestors for a process that `AgentSpec::runs_as` claims for Muse. The `muse` launcher execs a versioned binary, `muse-bin-<version>`, and the kernel truncates its `comm` to 15 bytes (`muse-bin-1.4.9-`). The declared process name `muse-bin-<version>` matches that prefix followed by a digit, truncated or not, and never matches the launcher. `bin_names` keeps `muse` for recognizing a pane by its command.

Two tests guard the passthrough. `crates/rimz/src/harness/launch/tests.rs::muse_managed_hook_passthrough_covers_host_and_sandbox_launch_environment` checks that every identity key a host or scoped launch sets is in `hook_env_keys`, and `crates/rimz/src/agents/adapters/muse/tests.rs::default_config_home_install_survives_runtime_domain_xdg_pinning` checks that an install under the `HOME` default is still detected once a tmux server pins `XDG_CONFIG_HOME`.

## Launch

The spec's `LaunchSpec` renders this argv:

| Launch | Argv |
| --- | --- |
| initial prompt | `muse -- <prompt>`, so a prompt may start with a hyphen |
| resume | `muse resume <id>` |
| resume picker | `muse resume` |
| fork | none: Muse has no CLI fork flag |
| permission mode | Auto `--permission-profile :auto-review`, Ask `--permission-profile :ask-me`, Plan `--permission-profile :read-only`, Yolo `--yolo` |
| model preset | `--model <model>` |
| effort preset | `--reasoning-effort <effort>` |
| scoped isolation | adds `--disable-sandbox`, Yolo included (`MuseAdapter::disable_native_sandbox_args`) |
| manual compaction | `/compact` typed into the pane, with no instructions (`CompactInstruction::Unsupported`) |

A prompt exactly equal to a Muse subcommand still runs that subcommand: `muse -- resume` opens the resume picker. A prompt of more than one word avoids the collision.

Auto names `:auto-review` explicitly, so a launch never inherits the user's default profile. Muse refuses a permission profile together with `--approval-mode` or `--yolo`, which is why Yolo passes no profile. Whether `:read-only` blocks a real model tool call is unverified, and plan approval has no mapped event. The [permission table](../../reference/agent-support.md#permission-modes) is pinned against `LaunchSpec::permission_args`.

Scoped isolation turns Muse's own sandbox off. On Linux, Muse confines shell commands with bubblewrap and refuses a shell command when it cannot verify that confinement, which a nested bubblewrap inside the RimZ scoped view does not reliably give ([upstream](../../externals/agent-adapter/muse-reference.md#permissions-and-sandbox), [sandbox.md](../sandbox.md)). Muse is the only adapter that implements `disable_native_sandbox_args`.

Some launch inputs never reach Muse:

| Launch input | What happens |
| --- | --- |
| the `sandbox` isolation level | warns and launches without a provider sandbox: `has_native_sandbox` keeps its default `false` |
| a profile's tool rules | warn and are not enforced (`ToolRules::Unsupported`) |
| a profile's `skills` list on the host | warns and runs unrestricted (`HostSkills::Unsupported`) |
| a profile's `skills` list under scoped isolation | refused: Muse has no user-only skill marker (`ManualSkill::Unsupported`). The skill root is `<config home>/skills` |
| launch reminders, system-prompt replacement, and presets other than model and effort | no Muse channel or spelling |
| a headless command | none: `launch.headless` is unset, so Muse cannot run a headless judge or check |

The spec declares no default model and no context window.

### Sessions

Registration is lazy (`registers_lazily`). Muse 1.4.9 fires no `SessionStart` on CLI resume or on `/fork`, so the first `UserPromptSubmit` registers the resumed or forked row. `/clear` and `/new` each end the session, and the next prompt fires `SessionStart` with a new id, with source `clear` or `startup` respectively. Every `SessionEnd` capture carried `reason: "other"`, so a quit and a reset look the same. RimZ has no locator for a session it did not record.

## Context, account, and cost

Muse has no context, account, or cost source:

| Surface | State |
| --- | --- |
| hook context | every captured hook had a null `transcript_path`, model `unknown`, and zero usage |
| transcript and rich context | none. Muse keeps framed records at `$XDG_DATA_HOME/muse/sessions/YYYY/MM/DD/<session_id>/session.jsonl`, but the schema is unpublished, so no parser exists |
| live context fill and tokens | none: no nonzero usage or context-window source was captured |
| login, plan, balance, quota | none integrated |
| named accounts | unsupported: Muse has no config-home override of its own, and moving `XDG_CONFIG_HOME` would move every tool's config |
| spend and live dollars | none: no spend parser, and the documented Muse model has no verified PriceBook price |

Muse's exported JSON schemas define token usage, an optional USD cost, and an optional context window, but none of these has been measured against a real provider. Each surface needs a real-provider capture before RimZ claims it.

## Known gaps

- Model tool calls, native approvals, `StopFailure`, and model-spawned children map from documented shapes only.
- The neutral reply's effect on the native approval UI, and the order of `PermissionRequest` against the approval judge, are unmeasured.
- `/compact` was unavailable under `echo`. Whether Muse fires `SessionStart` with source `compact` is unknown, and no compaction hook is installed.
- `:read-only` enforcement at a real tool call is untested.
- Enterprise policy over the managed-hook settings is unchecked.
- A one-word prompt that names a Muse subcommand runs the subcommand.

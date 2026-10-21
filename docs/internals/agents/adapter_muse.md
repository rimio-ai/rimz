# Muse adapter

Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration boundary. The upstream protocol and the versioned echo captures are in [muse-reference.md](../../externals/agent-adapter/muse-reference.md#149-capture).

Muse Code support requires 1.4.9 or later, the first version captured locally. `crates/rimz/src/agents/adapters/muse/mod.rs::MUSE_DESCRIPTOR` declares Full State, Partial Ask, and Unsupported Live, History, Account, and Subagents. TurnLifecycle, Permission, SessionEnd, and HookInstall are Wired; the other fifteen concerns are Unsupported. The card follows root turns, tool activity, and session endings; approvals route attention to Muse's pane without exporting a question or answer path. The approval mapping is documented-shape coverage, not a live-verified native prompt.

## Hooks and lifecycle

`crates/rimz/src/agents/adapters/muse/mod.rs::MUSE_HOOKS` drives installation and the classification corpus. All ten events run `rimz hooks feed --source muse`, without matchers, with a four-second timeout. Only `Interrupt` is async, as Muse 1.4.9 requires; `PermissionRequest` is synchronous to avoid a delayed Waiting arriving after tool completion. Every reply is silent.

| Native event | Lifecycle signal | Evidence |
| --- | --- | --- |
| `SessionStart` | `SessionSource::session_start_signal`: `startup` and `clear` register | 1.4.9 echo capture |
| `UserPromptSubmit` | `TurnStarted`, with lazy registration | 1.4.9 echo capture |
| Model `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | `ToolUsed` | Documented-only fixtures; model tool calls uncaptured |
| User-shell `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | No lifecycle signal | 1.4.9 explicit `!` shell input capture |
| `PermissionRequest` | `AwaitingInput(Permission)` | Documented-only fixture; native approval uncaptured |
| `Stop` | `TurnEnded`, not errored | 1.4.9 echo capture |
| `StopFailure` | `TurnEnded`, errored | Documented-only fixture; final model failure uncaptured |
| `Interrupt` | `TurnInterrupted`, idle | 1.4.9 Escape capture |
| `SessionEnd` | `Ended` | 1.4.9 `/quit` and `/clear` capture |

The decoder retains native turn and tool ids. `Interrupt` records the interrupted turn so late tool reports for that turn cannot reopen Running. Explicit user `!` shell actions, identified by a `user_shell_` tool id, have no model-turn boundaries and produce no lifecycle signal, leaving a resting card unchanged. Model `shell` calls count as mutating; no editing tool is declared. `Stop` and `StopFailure` can supply the final assistant message. `PermissionRequest` produces no structured ask text or options, so answer in Muse's UI, not `rimz answer`. Empty exit-0 stdout is the documented neutral reply, but native approval UI preservation and approval-judge ordering remain unverified. A judge-approved call may briefly raise Waiting. Clarifying questions do not raise Waiting.

Muse emits hooks for internal reminder children on ordinary turns. `MuseAdapter` drops ingress when both `MUSE_SESSION_ID` and `MUSE_THREAD_ID` exist and differ: payload `session_id` can identify the child while `MUSE_SESSION_ID` remains the root. Model-spawned children have not been captured, so no child cards are projected. `SubagentStart`/`SubagentStop`, per-LLM hooks, delayed `Notification`, `PostToolBatch`, and compaction hooks are not installed.

### Managed installation and identity

`crates/rimz/src/agents/adapters/muse/install.rs::MuseManagedIntegration` owns a two-file installation under `$XDG_CONFIG_HOME/muse`, else `$HOME/.config/muse`. [The hook command reference](../../reference/cli/hooks-trust.md#what-install-writes) lists the user-visible edits.

- `rimz-hooks.json` is RimZ's whole file. Ownership is structural: no `_rimz_managed` or `_rimz_sync` marker is written. The capture shows either marker makes Muse skip the group or handler rather than reject the file, so the shared marker-based JSON installer cannot be used.
- `settings.json` receives `managed_hooks_path`, an absolute path to that file, and `managed_hooks_env_vars`, an array of environment-variable names. Other settings, including user hooks, survive the strict JSON round-trip. A new settings file also gets `schema_version: 1`; an existing file without that exact version is refused, not repaired.
- A version below 1.4.9, an unavailable or unparseable version, or a foreign `managed_hooks_path` refuses installation. Detected installs skip Muse; an explicit install names the fix.
- Detection requires canonical hook-file bytes, schema version 1, RimZ's path, and every required passthrough name. A missing name re-offers install. Uninstall deletes the hook file and removes the two settings only while the path still names RimZ's file; it retains the schema version and unrelated settings.

User hooks lose the room environment. Managed hooks receive the named passthrough, derived by `crates/rimz/src/agents/adapters/muse/install.rs::passthrough_names` from the shared hook-env constants, plus Muse's root/thread ids. The list excludes ingress-owned `RIMZ_AGENT_PID` and `RIMZ_HOOK_OWNER_KIND`, and `SHELL`, which Muse already passes to every hook. This carries workspace, pane, run, launch, isolation, home, and runtime identity through ordinary hook ingress without a core recovery path. Host/scoped completeness and tmux-pinned config-home resolution are tested. Interactive process matching recognizes versioned `muse-bin-<version>` names, including kernel truncation, rather than mistaking the `muse` launcher for the interactive process.

Enterprise policy can override user managed-hook settings. Install reads only the user settings, so successful detection does not prove an enterprise policy allows these hooks. This remains an unhandled gap; `muse config status` is the native policy inspection surface.

## Launch and resume

RimZ submits initial prompts as `muse -- <prompt>`, including leading hyphens. A prompt exactly equal to a Muse subcommand can still dispatch that subcommand: `muse -- resume` opens the picker. Multi-word prompts avoid that collision.

The picker is `muse resume`; a recorded id resumes through `muse resume <id>`. There is no CLI fork integration or unrecorded-session locator. On 1.4.9 CLI resume and `/fork` fire no `SessionStart`, so the first `UserPromptSubmit` lazily registers the resumed row. `/clear` starts a new id with source `clear`; `/new` starts a new id with source `startup`, both at the next prompt.

Permission modes render as [the permission table](../../reference/agent-support.md#permission-modes) states: Auto `--permission-profile :auto-review`, Ask `--permission-profile :ask-me`, Plan `--permission-profile :read-only`, and Yolo `--yolo`. Auto stays explicit rather than inheriting the user's default profile. Muse refuses a permission profile alongside `--approval-mode` or `--yolo`. Read-only enforcement at a real model tool call is unverified, and plan approval is unsupported.

Under RimZ scoped isolation the adapter adds `--disable-sandbox`, including with Yolo, to avoid nested provider bubblewrap. Model and effort render through `--model` and `--reasoning-effort`, with no default model or context window declared. Skills live at `<config_home>/skills`; there is no host-mode skill-selection switch. Launch reminders, prompt replacement, and headless runs have no integrated surface.

The manual command is `/compact`, without summary guidance. It was unavailable under echo. Compaction events and their lifecycle remain unsupported until captured against a real provider.

## Context and transcript

Captured hooks always had null `transcript_path`, unknown model, and zero usage. Muse stores framed records under `$XDG_DATA_HOME/muse/sessions/YYYY/MM/DD/<session_id>/session.jsonl`, but its unpublished schema has no integrated parser. Transcript replay, rich context, live context fill, and token counts are unsupported. Exported schemas and a known store path do not establish real-provider usage semantics.

## Account and balance

No login, plan, balance, or quota source is integrated. Named accounts are unsupported: no Muse-specific config-home override is established, and moving `XDG_CONFIG_HOME` would relocate every tool's config, not just Muse's.

## Cost

There is no spend parser, historical/account spend, or live dollar source. Echo's zero counters do not validate real usage, and the documented Muse model has no verified PriceBook price. Pricing and provider usage need a real-provider capture before these capabilities can be claimed.

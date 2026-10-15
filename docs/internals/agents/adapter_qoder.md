# Qoder adapter

Qoder runs stock in its pane. The private adapter is `crates/rimz/src/agents/adapters/qoder/mod.rs::QoderAdapter`; the [upstream reference](../../externals/agent-adapter/qoder-reference.md) distinguishes 1.1.72 no-login observations from installed-bundle traces. Model turns, tool calls, asks, and children have not been observed live under a login. The [support reference](../../reference/agent-support.md#qoder) owns the user-facing limits and launch flags.

## Hooks and lifecycle

The adapter launches `qodercli`, not its `qoder` dispatcher: the hook shell's `$PPID` is then the pane's CLI process. Installed commands stamp `RIMZ_AGENT_PID=$PPID` before `exec rimz hooks feed --source qoder`. Default hook ingress accepts that PID unchanged. Both executable names classify hand-launched panes.

Hooks merge into `<config_home>/settings.json`. `QODER_CONFIG_DIR` selects the home, otherwise `$HOME/.qoder`; skills live under its `skills/`. Installation preserves user hooks and other settings, reclaims owned commands, and repairs missing events. PermissionRequest is synchronous. There is deliberately no PreToolUse hook: 1.1.72 interprets stray text there as allow and malformed JSON as deny. Every RimZ hook response is empty stdout, including malformed payloads and unknown events. The bundle trace establishes no-decision neutrality; a logged-in approval probe remains outstanding.

| Native event | Lifecycle signal |
| --- | --- |
| SessionStart | shared source rule: compact closes compaction; otherwise Registered |
| UserPromptSubmit | TurnStarted, with sanitized prompt and task |
| Stop | successful TurnEnded; last_assistant_message supplies supervised final output |
| StopFailure | errored TurnEnded; error (not error_type) classifies rate-limit and overload pauses |
| QueryEnd with reason abort | TurnInterrupted; other reasons ignored |
| PostToolUse | ToolUsed, with mutation/edit flags |
| PostToolUseFailure | ToolUsed without mutation/edit claims |
| PermissionRequest | AwaitingInput: AskUserQuestion is Question, ExitPlanMode is PlanApproval, otherwise Permission |
| SubagentStart / SubagentStop | child start / stop, parented by session_id |
| PreCompact / PostCompact | compaction open / close, with trigger auto/manual |
| SessionEnd | Ended |
| Notification | audit only, no lifecycle or ask signal |

SessionStart marks startup, clear, and new as Fresh. Resume is not Fresh. Subagent ordinary hooks route by agent_id while context remains keyed to session_id; child start/stop require both identities. Children display their agent type, not per-child tokens or replay. PostCompact followed by SessionStart:compact closes one bracket: the shared state machine ignores the duplicate end. QueryEnd:abort settles a running row. A completed query instead emits Stop followed by QueryEnd:end_turn, which maps no signal and preserves the verdict. No turn id correlates QueryEnd to the prompt, so a late or mismatched abort can change a settled row to Idle. The adapter does not change shared reducer semantics for that limit.

`turn_ended` is declared Native Stop. QueryEnd:abort is another native end path, included in the classification corpus, not a derived substitute: conformance groups TurnInterrupted with TurnEnded and accepts Stop as its native witness. Notification's idle concern is wired for audit only, not interruption detection; idle_prompt is focus-gated and also fires after ordinary completion.

The minimum release is 1.1.72, the only probed bundle and the one whose closed enum admits undocumented QueryEnd. Older readable releases refuse with `qodercli update`. Plan permissions parse there but are absent from help: plan is default permissions plus Plan work state, so hook permission_mode reads default. This compatibility mode may disappear upstream. `/compact <text>` uses trailing custom instructions. Model and reasoning-effort presets are supported; every other preset field is refused.

### Launch preconditions and folder trust

Launch refuses when effective user, project, or local settings disable hooks (`disableAllHooks` or `hooksConfig.enabled`, later layers win). Read-only checks accept JSONC and abstain on malformed files without rewriting them. `--config-dir`, QODER_CLI_HOME, and QODER_CONFIG_DIR_NAME refuse with fixes because they select roots different from RimZ's installed hook home.

Folder trust is modeled through the existing installation capability by `crates/rimz/src/agents/adapters/qoder/folder_trust.rs::folder_trust`. An undecided cwd warns through shared preflight; only an explicit `rimz trust grant --agents qoder` publishes the guarded preview. The grant appends canonical cwd, not repo root, to user permissions.trustDirectories, preserving other keys with a strict JSON rewrite. Existing trustDirectories and additionalDirectories entries match canonical exact paths or separator-delimited ancestors; security.folderTrust.enabled=false also decides trust. No persisted distrust state was established. Headless launches do not require folder trust.

Untrusted folders block even user and per-run hooks, losing the first SessionStart. A fresh team worktree therefore needs its own grant unless an ancestor already covers it. No-login observations proved startup and shutdown hook delivery in trusted folders, not the full managed-room launch chain.

## Context and transcript

No live context or transcript capability is declared. The bundle writes Claude-shaped JSONL, but zeroes token counts for ordinary Qoder models unless EXPOSE_TOKEN_USAGE is set. Custom/BYOK behavior is not a reliable default. No transcript exists before login, and no logged-in schema was observed. A future transcript integration needs captures, not an assumed Claude parser.

There is no hook context reply or prompt context injection: although additionalContext is implemented upstream, model consumption was not authorized for a logged-in probe. Deadline stop therefore uses the Pane channel. A statusLine gauge is also deferred: its stdin schema was traced but not observed, and a wrapper would modify user settings.

## Account and balance

Unsupported. No readable login, plan, or Credits balance is integrated. SDK quota APIs are separate contracts, not evidence for a local account parser.

## Cost

Unsupported live and historically. Ordinary token counters are hidden and Credits have no established dollar conversion. Stop's final text supports supervised completion without claiming transcript or spending support.

## Remaining evidence gaps

- Logged-in turn, tool, ask, error, interruption, compaction, and subagent emissions and their timing at the native UI.
- Model consumption and recorded origin of additionalContext; statusLine stdin; a captured transcript schema and usage semantics.
- Earlier release compatibility, conflicting hook replies and timeouts under a real approval prompt, and per-run hook merge behavior.
- `qoder hooks migrate` interplay, worker-runtime hooks, native command sandboxing, and TaskCreated, TaskCompleted, TeammateIdle, and Setup integration remain outside this adapter.
- Node 26's node-MainThread comm does not match the declared node process name in the ancestor fallback. Installed hooks bypass that fallback with the stamped PID; no shared process-matching change is included.

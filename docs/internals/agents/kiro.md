# Kiro adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md). The pinned upstream surface and live evidence are in [kiro-reference.md](../../externals/agent-adapter/kiro-reference.md).

Kiro support targets the v3 engine selected by `kiro-cli chat --v3`. RimZ owns verified launch, exact resume, profile arguments, manual compaction input, and process identity. Kiro CLI 2.12.1 exposes no verified executable lifecycle, transcript, context, or usage channel to a stock interactive observer.

## Launch, resume, and presence

Fresh sessions run `kiro-cli chat --v3`. Profiles map `model` and `effort` to the chat-level `--model <model>` and `--effort low|medium|high|xhigh|max` flags. These flags stay after the `chat` subcommand because the installed parser rejects them after the root-level `--v3` shortcut.

Exact resume runs `kiro-cli chat --v3 --resume-id <session_id>`. Kiro's `/rewind` remains interactive-only, so `rimz agents fork` is unsupported. Manual smart compaction types `/compact` into the native composer.

Presence matches `kiro-cli` and `kiro-cli-chat`. The first is the launcher and the second is the v3 chat engine. RimZ excludes `kiro-cli-term`, the figterm shell-integration daemon that runs for ordinary integrated shells, so it cannot bind a non-agent pane.

Permission-mode suffixes add no flags. Kiro v3 expresses permissions through capability-policy files; RimZ does not claim that legacy `--trust-all-tools` or `--trust-tools` flags override those files.

## Lifecycle and hooks

Authenticated Kiro CLI 2.12.1 testing found no executable stock-v3 hook contract. The documented user `~/.kiro/hooks/rimz.json`, an auxiliary user hook file, a replacement canonical command, and a project `.kiro/hooks` file produced no command invocation or stdin payload. The CLI exposes no hook validation command that makes the configuration reproducible.

The adapter therefore classifies every manually fed Kiro event as unknown, produces no lifecycle observation, installs no hooks, and advertises no native registered, turn, tool, idle, ask, compaction, or session-end event. Pane liveness and the rollup reaper can still clear a process-owned row after exit, while the `rimz exec` wrapper derives a lost mux session.

`rimz hooks install kiro` and its dry run fail with the verified limitation and the condition for re-enabling the surface. `rimz hooks uninstall kiro` remains available as cleanup for a legacy RimZ-owned `${KIRO_HOME:-~/.kiro}/hooks/rimz.json`; ownership is the stable `hooks feed --source kiro` command marker, and unowned files stay untouched.

## Supervised runs

`rimz agents kiro -p` fails before pane creation or run-record creation. A supervised run needs an executable turn-completion signal, and opening a stock Kiro pane without one can only time out. Interactive `rimz agents kiro` remains available.

ACP is not an implicit fallback. Switching to `kiro-cli acp` would make RimZ the protocol client and would require a separately verified supervised transport, cancellation, permissions, final-output, and session-retention contract.

## Transcript, context, and spend

The observed stock root session used a `sess_<uuid>` identity and left only `~/.kiro/sessions/cli/<session-id>.history`. The final observed file held submitted prompts and slash commands, with no assistant text, timestamps, model, context usage, credits, or tool results. RimZ treats it as readline history rather than a provider transcript.

A UUID-only `.json`/`.jsonl` pair observed beside it belonged to an ACP-hosted non-interactive session whose metadata identified `session_created_reason: "subagent"`. The stock adapter does not parse that different session class. It also does not depend on manual `/transcript save --json`, pane capture, or opt-in `KIRO_ACP_RECORD_PATH` debug recordings.

Context usage, transcript replay, final assistant output, live credits, realtime dollars, and historical spend remain unsupported. Kiro is credit-metered, and RimZ does not convert credits into USD.

## Re-enabling lifecycle

Re-enable hooks only after a pinned Kiro v3 release executes a reproducible user or project configuration and a redacted capture proves stdin keys, event ordering, root identity, cwd and process attribution, success/error/cancellation boundaries, and stdout behavior. Add fixture-backed lifecycle and supervised tests with that implementation; do not infer the contract from v2 embedded hooks or Amazon Q lineage.

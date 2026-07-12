# Kiro adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md). The pinned upstream surface is in [kiro-reference.md](../../externals/agent-adapter/kiro-reference.md).

Kiro support targets the v3 engine selected by `kiro-cli --v3`. The first milestone carries root-session lifecycle, presence, launch, and native resume; undocumented prompt, transcript, context, usage, and child surfaces stay declared unsupported.

## Hooks and lifecycle

RimZ installs four user-level v3 command hooks. Each command includes `--event <Trigger>` because Kiro publishes no stdin field that identifies the trigger.

| Native event | `observe_lifecycle` signal | Normalized fields |
| --- | --- | --- |
| `SessionStart` | `Registered` | session identity and `cwd` when present |
| `UserPromptSubmit` | `TurnStarted` | sanitized prompt when present |
| `PostToolUse` for `fs_write` or `str_replace` | `ToolUsed { mutates: true, edits: true }` | documented file-writing tool name |
| `Stop` | `TurnEnded { errored, parked_on_background: false }` | tolerant shared error-bit sniff |

The v3 hook input schema is unpublished. The parser accepts optional snake-case and camel-case candidates for session identity, prompt, tool name, and cwd; an event without a stable session id keeps `agent_id: None` and the hook ingest path quarantines it rather than inventing identity.

`Stop` is a turn boundary, not a session end. Kiro documents no error or abort discriminator, so RimZ treats it as clean unless the payload carries one of the shared tolerant error bits; live verification must prove failure and cancellation shapes before the mapping grows more specific. Pane liveness and the rollup reaper derive session end, and the `rimz exec` wrapper derives mux-session loss.

Kiro draws permission prompts in its own pane, but v3 exposes no hook after policy decides to ask. `PreToolUse` runs before that decision, so the adapter records no `AwaitingInput`; blocking prompts surface only as ordinary pane attention in this milestone.

Hook stdout stays empty for every trigger. Kiro adds stdout to model context on `SessionStart` and `UserPromptSubmit`, so one silent contract prevents diagnostics or future event changes from injecting text into the conversation.

## Install

`rimz hooks install` owns `${KIRO_HOME:-~/.kiro}/hooks/rimz.json` as a whole file, with `RIMZ_KIRO_HOOKS` available for tests and tooling. The canonical JSON contains one enabled command action per trigger, a ten-second timeout, and an absolute shell-quoted path to `rimz hooks feed --source kiro --event <Trigger>`.

Ownership is the stable `hooks feed --source kiro` command substring on every hook entry. Install and preview reclaim an owned file, refuse an unowned file with the move-it-aside fix, and rewrite atomically; uninstall removes only an owned file. Any drift from the canonical schema, enabled trigger set, action, timeout, or current RimZ executable path reports not installed so setup re-offers the canonical write; a partial owned trigger set remains discoverable for cleanup.

## Launch and resume

Fresh sessions run `kiro-cli chat --v3`. Profiles map `model` and `effort` to the documented chat-level `--model <model>` and `--effort low|medium|high|xhigh|max` flags. Keeping these flags after the `chat` subcommand matters: the installed 2.12.1 parser rejects them after the root-level `--v3` shortcut.

Exact resume runs `kiro-cli chat --v3 --resume-id <session_id>`. Kiro's `/rewind` remains interactive-only, so `rimz agents fork` is unsupported. Manual smart compaction types `/compact` into the native composer.

Presence matches two process names, `kiro-cli` and `kiro-cli-chat`. The install ships a thin `kiro-cli` launcher plus the heavy `kiro-cli-chat` v3 engine binary the launcher execs into once the TUI is active, so a live pane can read as either. The third shipped binary, `kiro-cli-term`, is the figterm shell-integration daemon that runs for every integrated shell; it is excluded from `process_names` so it can never bind a non-agent pane.

Permission-mode suffixes currently add no flags. Kiro v3 expresses permissions through capability-policy files, and mapping RimZ postures onto those executable configuration surfaces is outside the first milestone. The 2.12.1 `chat --v3` parser still *accepts* the legacy `--trust-all-tools`, `--trust-tools`, and `--no-interactive` flags (they proceed to login rather than erroring), but the official v3 permissions surface is capability rules where restrictiveness wins, so whether a flag overrides a `permissions.yaml` `ask`/`deny` is unverified. RimZ keeps the no-flag behavior rather than mapping `yolo` onto a flag that may be a runtime no-op.

## Abstraction follow-ups

`AgentAdapter::permission_args` models a permission posture as launch argv. Kiro v3 needs a broader preparation contract before `kiro-{auto,yolo}` can be honest: preview and consent to a managed policy artifact, resolve the user and per-workspace rule scopes, detect a more restrictive conflicting rule, and fail before launch with the fixing path. Keep the current no-flag behavior until that contract exists rather than making the suffix appear to work.

Supervised `-p` currently relies on the same `Stop` boundary as an interactive turn, but Kiro's hook contract publishes no final assistant message. Treat useful text/JSON output as deferred until a captured Stop payload, a stable transcript export, or an ACP-owned run supplies the answer without pane scraping; the shared harness may eventually need to declare output capability separately from turn-completion capability.

## Context and transcript

Context usage and transcript replay are unsupported. Kiro exposes `/context show` interactively but publishes no machine-readable hook field or v3 transcript schema.

## Account and balance

Account probing is unsupported. `kiro-cli whoami --format json` exists, but its response schema is unpublished. One state is captured: signed out, 2.12.1 prints `{"account":null}` and exits `0` — enough to detect logged-out, but the signed-in envelope (plan, method, expiry, rate-limit windows) still needs a live capture before a typed probe is safe. Kiro CLI is the rebranded Amazon Q Developer CLI (`crates/q_cli/`, open source at `aws/amazon-q-developer-cli`), so the Amazon Q account and hook conventions are a strong secondary reference — this is why the payload parser already accepts `conversation_id` session aliases — but v3 capitalizes triggers and a captured v3 fixture still governs.

## Cost

Kiro is credit-metered and publishes no machine-readable live usage or durable credit ledger. The adapter reports neither realtime cost nor historical account spend.

## Live verification still open

- Capture redacted stdin, cwd, environment, ancestry, timeout, and stdout behavior for all four triggers on the pinned Kiro version. The Amazon Q lineage (`crates/q_cli/`) suggests `conversation_id`/`cwd`/`tool_name`/`hook_event_name`, but only a v3 capture confirms the actual keys.
- Prove session-id stability across prompts, tools, stop, exact resume, `/chat new`, `/rewind`, and process exit.
- Capture success, model/API failure, cancellation, rate limit, permission denial, and process death, with special attention to `Stop` error bits.
- Capture canonical `PostToolUse` tool names before widening the acting table beyond `fs_write` and `str_replace`.
- Confirm the live pane process name once past login: verify whether the foreground/hosted process is `kiro-cli` or `kiro-cli-chat` so `process_names` can drop the one that never appears.
- Capture `whoami --format json` signed in (the signed-out `{"account":null}` shape is already pinned) before a typed account probe.
- Verify whether `--trust-all-tools`/`--trust-tools` actually relax v3 capability policy at runtime, or are overridden by `permissions.yaml`, before mapping any permission suffix onto a flag.

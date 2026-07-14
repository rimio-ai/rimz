# Copilot adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md); the pinned upstream hook, launch, telemetry, and storage surfaces are in [copilot-reference.md](../../externals/agent-adapter/copilot-reference.md).

GitHub Copilot CLI reports through native camelCase command hooks installed as one RimZ-owned user file at `$COPILOT_HOME/hooks/rimz.json`, falling back to `~/.copilot/hooks/rimz.json`. Each command passes its event name to `rimz hooks feed --source copilot --event <event>` because native payloads carry no event-name field. Hook stdout stays empty, preserving Copilot's own permission engine and question UI.

## Hooks and lifecycle

| Native event | Channel | Normalized signal | Detail |
| --- | --- | --- | --- |
| `sessionStart` | lifecycle | `TurnStarted` with non-empty `initialPrompt`, otherwise `Registered` | `sessionId`, `cwd`; `startup`/`new` mark fresh identity while `resume` preserves the existing lineage |
| `userPromptSubmitted` | lifecycle | `TurnStarted` | sanitized `prompt` labels the row |
| `preToolUse(ask_user)` | awaiting-user (`Question`) | `AwaitingInput` | best-effort question text from object or JSON-string `toolArgs` |
| `preToolUse` (other tools) | lifecycle | `ToolUsed { false, false }` | proof-of-work for clearing a prior ask; excluded from activity to avoid the ask race |
| `postToolUse` / `postToolUseFailure` | lifecycle | `ToolUsed` for mutating tools | `create`/`edit` mark the acting phase; shell tools mutate without editing |
| `permissionRequest` | awaiting-user (`Permission`) | `AwaitingInput` | Copilot's native UI remains the answer surface |
| `agentStop` | lifecycle | `TurnEnded` | the native final turn boundary |
| `preCompact` | lifecycle | `Compacting` | opens the compaction bracket |
| `errorOccurred` | lifecycle enrichment | — | non-recoverable errors become display-only turn-error markers |
| `sessionEnd` | lifecycle | `Ended` | tombstones the session |

Copilot publishes no post-compaction hook. `preCompact` opens the bracket and the shared state machine closes it on the next lifecycle signal; the display window is the missed-edge backstop. This makes compaction partial rather than fully wired.

Copilot CLI 1.0.70 emits `userPromptSubmitted` before a prompt-seeded `sessionStart`, whose payload repeats the prompt in `initialPrompt`. The adapter treats a non-empty `initialPrompt` as the duplicate `TurnStarted` edge without copying its text; a promptless start remains `Registered`, preserving the shared registration reset used by clear and rebirth flows.

`errorOccurred` is a marker rather than a lifecycle end because recoverable model-call retries occur mid-turn. Only `recoverable: false` creates an `AgentTurnError`; `agentStop` remains turn truth and `sessionEnd` remains session truth.

The payload parser accepts both documented `toolArgs` shapes: the hooks reference leaves it as `unknown`, while GitHub's hook tutorial describes a JSON-encoded string. It also keeps the rest of `postToolUseFailure` intact when that event's `error` is a string rather than the object carried by `errorOccurred`.

Subagent hooks are deliberately absent from the installed file. Their payload identifies only an agent definition name, not a child invocation, and the built-in `general-purpose` agent emits neither event, so concurrent children cannot be durably keyed.

Install owns the whole file through its first-line `_rimz_managed` marker. Reinstall reclaims a marked file, install refuses an unmarked file at that path, and uninstall removes only the marked file. Copilot CLI 1.0.70 accepts the unknown top-level marker and discovered the marked file during an access-denied startup probe; if a later release stops doing so, move the marker into the first hook entry's `env` overlay.

## Launch and resume

`copilot` launches a fresh interactive session and `copilot --resume <session_id>` restores one. Ask mode adds no flags, plan uses `--plan`, auto uses `--autopilot`, and yolo uses `--allow-all`; model and effort profiles map to `--model` and `--effort`.

Copilot CLI 1.0.70 exposes `-i, --interactive <prompt>`, which starts the stock interactive UI and automatically executes the prompt. RimZ appends `--interactive <prompt>` after profile and permission arguments for prompt-seeded panes and supervised `rimz agents copilot -p` runs, preserving native asks and the hook-driven completion path. Copilot's native non-interactive `-p` mode remains deferred because adopting its process output would introduce a second supervised-run backend and remove the pane's interactive answer surface. Fork and ping stay unsupported: Copilot 1.0.70 exposes `/fork` only as an experimental interactive slash command with no launch flag, and no lowest-effort priming turn is defined.

A supervised `rimz agents copilot -p` run reports its lifecycle and exit status from `agentStop` and pane liveness. The hook's validated `transcriptPath` supplies the final visible `assistant.message`, so the run record, `agents wait --stream`, durable transcript, and `agents history` use the same provider-native conversation source.

`--resume <session_id>` binds the id as a space-separated value on 1.0.70; `--session-id <session_id>` is the unambiguous exact-match alternative (a required-argument flag) if a future Copilot argument parser stops consuming the optional `--resume` value.

## Context and transcript

`$COPILOT_HOME/session-state/<sessionId>/events.jsonl` is the append-only conversation source captured from Copilot CLI 1.0.70. RimZ derives this path as soon as a safe single-component session ID arrives, then accepts `agentStop.transcriptPath` only when its filename is `events.jsonl` and its parent matches that session ID.

The transcript reader recognizes only root `user.message` and `assistant.message` records, reads visible `data.content`, and parses the RFC3339 top-level timestamp. It ignores system, hook, tool, reasoning, transformed, encrypted, malformed, and unknown records. User text passes through the shared control-prompt sanitizer; assistant text is preserved.

Live model and latest-call token composition come from metadata-only OTel `chat` spans. A newly-born room supplies one private `agent-telemetry/copilot-otel.jsonl` cache to both the ordinary work shell and RimZ-managed panes, so typing stock `copilot` directly has the same enrichment source as `rimz agents copilot`. RimZ preserves an ambient `COPILOT_OTEL_FILE_EXPORTER_PATH`; an OTLP-only configuration also wins and leaves file enrichment unavailable. A RimZ-owned file always pins `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=false`.

Source precedence remains an already-anchored sidecar, the inherited/explicit file exporter, then the newest direct JSONL child of `$COPILOT_HOME/otel`. The bounded reader filters exact conversation IDs, selects the latest complete timestamped `chat` span, and anchors a managed file before it contains a usable span so a later asynchronous append is visible on Tick/Watch. Model display prefers `model_display_name`, then the OTel `model_id`, then the lifecycle scalar. A nonzero latest-call composition counts as session history and renders its fresh/cache/output line, but without an authoritative denominator it draws no percentage gauge or window token.

Copilot 1.0.70 concurrent capture proved complete append-only records and per-session visibility beyond the 64 KiB tail. It did not prove session-cumulative dollars: `github.copilot.cost` was per-chat zero and the aggregate span carried no dollar total. Its statusline exposed candidate display-limit fields while the nominal context-window fields stayed null, and lossless wrapper behavior remains incomplete. RimZ therefore leaves session/account spend and context-window fill unsupported and does not install or wrap Copilot's statusline.

The bounded tail reader requires an exact `gen_ai.conversation.id`, prefers `gen_ai.response.model` over `gen_ai.request.model`, and maps fresh input, cache read/write, and output counts. OTel provides no authoritative context-window denominator or fill percentage, so Context Usage and Rich Context remain partial even when the card shows a resolved model and token composition.

## Account and balance

The account probe reads the non-secret login identity in `$COPILOT_HOME/config.json` (`lastLoggedInUser`, falling back to the first valid `loggedInUsers` entry). The file is JSONC, so the read-only probe accepts comments and trailing commas through the shared comment-tolerant reader. It leaves `copilotTokens` unmodeled, reports a github.com login as `account_id`, and qualifies an enterprise login as `login@host`, keeping identities on different GitHub hosts distinct. Host normalization accepts the CLI's safe scheme, path, and port forms, folds case and trailing dots, and rejects malformed authorities.

Only a validated config identity establishes the account. A missing config or a valid config without an identity reports logged out, while an unreadable or malformed config remains unavailable for the short retry path. A found identity is metered with an unknown subscription allowance, so the provider panel can retain the login without misclassifying it as an API-key account.

Copilot plan, quota, extra-credit balance, API-key balance, and account spend are unsupported. Environment tokens do not establish an identity and RimZ performs no Copilot account-usage request; the captured internal response and official billing APIs remain research in the [external protocol reference](../../externals/agent-adapter/copilot-reference.md#account-usage-research).

## Cost

Realtime and historical cost remain unsupported. The narrow OTel reader deliberately ignores `invoke_agent`, inference-log, agent-turn-log, cost, AI-unit, quota, and account fields until their ordering, replacement, and units have captured fixtures. `events.jsonl` is conversation history, not a spending walker input.

## Deferred integration

Wired now: the turn lifecycle (`sessionStart`/`userPromptSubmitted`/`agentStop`), permission and `ask_user` asks, mutating-tool activity and the acting-phase edge, the `preCompact` bracket, the non-recoverable `errorOccurred` marker, `sessionEnd` tombstoning, install/uninstall of the whole `rimz.json` hook file, interactive and prompt-seeded launch with permission-mode and model/effort presets, `--resume` restore, provider-native transcript/history/final output, the host-safe login probe, and optional OTel model/token enrichment. Deferred: subagents, context-window fill, cost enrichment, plan and balance integration, spend, and remote control.

A logged-in Copilot CLI 1.0.70 prompt-mode capture verified successful-turn ordering, `agentStop.transcriptPath`, visible transcript message shapes, metadata-only OTel `chat` spans, resolved/requested model fields, token fields, and asynchronous exporter shutdown. Permission variants, `ask_user` options, resume PID ancestry, multi-turn interactive streaming, and remote sessions remain live-verification gaps.

Yolo maps to `--allow-all` with no policy preflight. Managed `permissions.disableBypassPermissionsMode = "disable"` can suppress the flag, so a `copilot-yolo` launch under that policy degrades to a normal permission posture instead of failing fast at the entry point. Resolving the merged policy value at launch and refusing when the requested bypass is unavailable is the fail-fast footprint, deferred until the merged-policy read lands.

Subagent coverage needs a stable child invocation identity. Copilot hooks expose the parent session and agent definition name, so concurrent children with the same definition collide; a future RimZ abstraction may admit provider-scoped synthetic child IDs only if OTel trace identity or another durable upstream key makes replay deterministic.

Context-window fill and cost need evidence beyond the current metadata-only `chat` span. Statusline wrapping stays deferred until its input and command-chaining behavior are documented or captured. The `default_context_window` stays `None` because the window depends on the live model and the `--context <tier>` selector (`default` or `long_context`, new in 1.0.70), so a denominator requires a provider-reported resolved tier rather than an invented table. RimZ does not read keychains, plaintext `copilotTokens`, environment tokens, browser cookies, billing budgets, or extra-credit balances; plan, quota, API-key balance, and `AccountSpend` stay unsupported because no suitable operational or dollar ledger is available.

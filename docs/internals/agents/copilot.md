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

Install owns the whole hook file through its first-line `_rimz_managed` marker and owns one reversible `statusLine` object in `$COPILOT_HOME/settings.json`. Preview validates both candidates and shows both diffs before writing either file. Reinstall reclaims a marked hook file, canonicalizes a managed statusline without nesting, and refuses an unmarked hook file or a non-command statusline value. The statusline preserves rendering options such as `padding`, stores the complete prior JSON value under `_rimz_wrapped`, and passes the provider payload byte-for-byte to that prior command while forwarding its stdout and exit status.

Hook readiness requires both the marked hook file and the canonical managed statusline command. Either artifact alone remains detectable as a partial install for repair or uninstall. Uninstall strictly restores or removes only the managed statusline first, then removes only the marked hook file; a hook-removal failure restores the original settings bytes atomically. Read-only readiness and wrapped-command probes accept later JSONC edits, while install and uninstall reject comments or trailing commas instead of discarding them. Copilot CLI 1.0.70 accepts the unknown top-level hook marker; if a later release stops doing so, move the marker into the first hook entry's `env` overlay.

## Launch and resume

`copilot` launches a fresh interactive session and `copilot --resume <session_id>` restores one. Ask mode adds no flags, plan uses `--plan`, auto uses `--autopilot`, and yolo uses `--allow-all`; model and effort profiles map to `--model` and `--effort`.

Copilot CLI 1.0.70 exposes `-i, --interactive <prompt>`, which starts the stock interactive UI and automatically executes the prompt. RimZ appends `--interactive <prompt>` after profile and permission arguments for prompt-seeded panes and supervised `rimz agents copilot -p` runs, preserving native asks and the hook-driven completion path. Copilot's native non-interactive `-p` mode remains deferred because adopting its process output would introduce a second supervised-run backend and remove the pane's interactive answer surface. Fork and ping stay unsupported: Copilot 1.0.70 exposes `/fork` only as an experimental interactive slash command with no launch flag, and no lowest-effort priming turn is defined.

A supervised `rimz agents copilot -p` run reports its lifecycle and exit status from `agentStop` and pane liveness. The hook's validated `transcriptPath` supplies the final visible `assistant.message`, so the run record, `agents wait --stream`, durable transcript, and `agents history` use the same provider-native conversation source.

`--resume <session_id>` binds the id as a space-separated value on 1.0.70; `--session-id <session_id>` is the unambiguous exact-match alternative (a required-argument flag) if a future Copilot argument parser stops consuming the optional `--resume` value.

## Context and transcript

`$COPILOT_HOME/session-state/<sessionId>/events.jsonl` is the append-only conversation source captured from Copilot CLI 1.0.70. RimZ derives this path as soon as a safe single-component session ID arrives, then accepts `agentStop.transcriptPath` only when its filename is `events.jsonl` and its parent matches that session ID.

The transcript reader recognizes only root `user.message` and `assistant.message` records, reads visible `data.content`, and parses the RFC3339 top-level timestamp. It ignores system, hook, tool, reasoning, transformed, encrypted, malformed, and unknown records. User text passes through the shared control-prompt sanitizer; assistant text is preserved.

Live context comes from Copilot's command statusline in `$COPILOT_HOME/settings.json`, which runs `RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source copilot`. The payload's `session_id` binds the sidecar to lifecycle identity. Non-empty session name, CLI version, model ID, and model display label map directly; a documented terminal effort suffix (`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`) separates from the display label while selector and multiplier text remain intact.

For the current window, `displayed_context_limit` wins over `context_window_size`, `current_context_used_percentage` wins over `used_percentage`, and the legacy `remaining_percentage` remains available. Published percentages clamp to the shared gauge range; a missing fill derives only from positive selected denominator plus `current_context_tokens`. `current_usage` maps component-for-component into the latest-call composition. The cumulative `total_input_tokens`, `total_output_tokens`, `total_cache_write_tokens`, `total_cache_read_tokens`, and `total_reasoning_tokens` stay in session usage and never establish occupancy; ambiguous `total_tokens` and `last_call_input_tokens` remain unmodeled.

Statusline `cost` contributes duration, API duration, and line-change counters without assigning dollars. `ai_used`, premium-request counters, and `remote.connected` remain unmodeled because they establish neither authoritative USD semantics nor a RimZ remote-control transport.

Metadata-only OTel `chat` spans remain the fallback when the managed statusline is absent, replaced, or unreadable. A healthy canonical statusline suppresses OTel refresh so a sparse asynchronous span cannot replace its richer token scopes. A newly-born room still supplies one private `agent-telemetry/copilot-otel.jsonl` cache, preserves an ambient `COPILOT_OTEL_FILE_EXPORTER_PATH`, respects OTLP-only configuration, and pins `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=false` for a RimZ-owned file.

Source precedence remains an already-anchored sidecar, the inherited/explicit file exporter, then the newest direct JSONL child of `$COPILOT_HOME/otel`. The bounded reader filters exact conversation IDs, selects the latest complete timestamped `chat` span, and anchors a managed file before it contains a usable span so a later asynchronous append is visible on Tick/Watch. Model display prefers `model_display_name`, then the OTel `model_id`, then the lifecycle scalar. A nonzero latest-call composition counts as session history and renders its fresh/cache/output line, but without an authoritative denominator it draws no percentage gauge or window token.

Copilot 1.0.70 concurrent capture proved complete append-only OTel records and per-session visibility beyond the 64 KiB tail. It did not prove session-cumulative dollars: `github.copilot.cost` was per-chat zero and the aggregate span carried no dollar total. Statusline AI credits and premium requests also lack authoritative session-USD semantics, so realtime and account spend remain unsupported.

The bounded OTel tail reader requires an exact `gen_ai.conversation.id`, prefers `gen_ai.response.model` over `gen_ai.request.model`, and maps fresh input, cache read/write, and output counts. It supplies metadata-only model/token composition only while the statusline bridge is unhealthy.

## Account and balance

The account probe reads the non-secret login identity in `$COPILOT_HOME/config.json` (`lastLoggedInUser`, falling back to the first valid `loggedInUsers` entry). The file is JSONC, so the read-only probe accepts comments and trailing commas through the shared comment-tolerant reader. It leaves `copilotTokens` unmodeled, reports a github.com login as `account_id`, and qualifies an enterprise login as `login@host`, keeping identities on different GitHub hosts distinct. Host normalization accepts the CLI's safe scheme, path, and port forms, folds case and trailing dots, and rejects malformed authorities.

Only a validated config identity establishes the account. A missing config or a valid config without an identity reports logged out, while an unreadable or malformed config remains unavailable for the short retry path. A found identity is metered with an unknown subscription allowance, so the provider panel can retain the login without misclassifying it as an API-key account.

Copilot plan, quota, extra-credit balance, API-key balance, and account spend are unsupported. Environment tokens do not establish an identity and RimZ performs no Copilot account-usage request; the captured internal response and official billing APIs remain research in the [external protocol reference](../../externals/agent-adapter/copilot-reference.md#account-usage-research).

## Cost

Realtime and historical cost remain unsupported. The narrow OTel reader deliberately ignores `invoke_agent`, inference-log, agent-turn-log, cost, AI-unit, quota, and account fields until their ordering, replacement, and units have captured fixtures. `events.jsonl` is conversation history, not a spending walker input.

## Deferred integration

Wired now: the turn lifecycle (`sessionStart`/`userPromptSubmitted`/`agentStop`), permission and `ask_user` asks, mutating-tool activity and the acting-phase edge, the `preCompact` bracket, the non-recoverable `errorOccurred` marker, `sessionEnd` tombstoning, two-file hook/statusline install and reversible uninstall, statusline context-window fill plus rich session context with OTel fallback, interactive and prompt-seeded launch with permission-mode and model/effort presets, `--resume` restore, provider-native transcript/history/final output, and the host-safe login probe. Deferred: subagents, dollar cost, plan and balance integration, spend, and remote control.

A logged-in Copilot CLI 1.0.70 prompt-mode capture verified successful-turn ordering, `agentStop.transcriptPath`, visible transcript message shapes, metadata-only OTel `chat` spans, resolved/requested model fields, token fields, and asynchronous exporter shutdown. Permission variants, `ask_user` options, resume PID ancestry, multi-turn interactive streaming, and remote sessions remain live-verification gaps.

Yolo maps to `--allow-all` with no policy preflight. Managed `permissions.disableBypassPermissionsMode = "disable"` can suppress the flag, so a `copilot-yolo` launch under that policy degrades to a normal permission posture instead of failing fast at the entry point. Resolving the merged policy value at launch and refusing when the requested bypass is unavailable is the fail-fast footprint, deferred until the merged-policy read lands.

Subagent coverage needs a stable child invocation identity. Copilot hooks expose the parent session and agent definition name, so concurrent children with the same definition collide; a future RimZ abstraction may admit provider-scoped synthetic child IDs only if OTel trace identity or another durable upstream key makes replay deterministic.

The `default_context_window` stays `None` because the window depends on the live model and the `--context <tier>` selector (`default` or `long_context`, new in 1.0.70); the statusline's provider-selected denominator remains authoritative. RimZ does not read keychains, plaintext `copilotTokens`, environment tokens, browser cookies, billing budgets, or extra-credit balances; plan, quota, API-key balance, realtime dollars, and `AccountSpend` stay unsupported because no suitable operational or dollar ledger is available.

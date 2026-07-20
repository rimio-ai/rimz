# Copilot adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts, balances, and spend are in [providers.md](./providers.md); the raw upstream protocol is in [copilot-reference.md](../../externals/agent-adapter/copilot-reference.md).

GitHub Copilot CLI reports through native camelCase command hooks installed as one RimZ-owned user file at `$COPILOT_HOME/hooks/rimz.json`, falling back to `~/.copilot/hooks/rimz.json`. Each command passes its event name to `rimz hooks feed --source copilot --event <event>` because native payloads carry no event-name field. Hook stdout stays empty, preserving Copilot's own permission engine and question UI.

## Hooks and lifecycle

| Native event | Channel | Normalized signal | Detail |
| --- | --- | --- | --- |
| `sessionStart` | lifecycle | `TurnStarted` with non-empty `initialPrompt`, otherwise `Registered` | `sessionId`, `cwd`; `startup`/`new` mark fresh identity while `resume` preserves the existing lineage |
| `userPromptSubmitted` | lifecycle | `TurnStarted` | sanitized `prompt` labels the row |
| `preToolUse(ask_user)` | awaiting-user (`Question`) | `AwaitingInput` | best-effort question text from singular `toolArgs` or a batched `toolCalls[].args` object/JSON string |
| `preToolUse` (other tools) | lifecycle | `ToolUsed { false, false }` | proof-of-work for clearing a prior ask; excluded from activity to avoid the ask race |
| `postToolUse` / `postToolUseFailure` | lifecycle | `ToolUsed` | every completed boundary clears Waiting immediately; known batched calls aggregate mutation/edit flags |
| `permissionRequest` | awaiting-user (`Permission`) | `AwaitingInput` | Copilot's native UI remains the answer surface |
| `agentStop` | lifecycle | `TurnEnded` | the native final turn boundary |
| child `userPromptSubmitted` | lifecycle | `SubagentStarted` after transcript correlation | child `sessionId` equals the parent task call's `toolCallId` |
| child `agentStop` | lifecycle | `SubagentStopped` after transcript correlation | the prior child relation supplies the parent after the first join |
| `preCompact` | lifecycle | `Compacting` | opens the compaction bracket |
| `errorOccurred` | lifecycle enrichment | — | non-recoverable errors become display-only turn-error markers |
| `sessionEnd` | lifecycle | `Ended` | stamps `ended_at`; runtime hides the retained resumable row |

Copilot publishes no post-compaction hook. `preCompact` opens the bracket and the shared state machine closes it on the next lifecycle signal; the display window is the missed-edge backstop. This makes compaction partial rather than fully wired.

Copilot CLI 1.0.70 emits `userPromptSubmitted` before a prompt-seeded `sessionStart`, whose payload repeats the prompt in `initialPrompt`. The adapter treats a non-empty `initialPrompt` as the duplicate `TurnStarted` edge without copying its text; a promptless start remains `Registered`, preserving the shared registration reset used by clear and rebirth flows.

`errorOccurred` is a marker rather than a lifecycle end because recoverable model-call retries occur mid-turn. Only `recoverable: false` creates an `AgentTurnError`; `agentStop` remains turn truth and `sessionEnd` remains session truth.

The payload parser accepts the legacy top-level `toolName`/`toolArgs` shape and Copilot CLI 1.0.71's batched `toolCalls: [{name, args}]` shape, with either argument field represented as an object or a JSON-encoded string. A contained `ask_user` wins blocking/detail selection over sibling calls, mutation and edit flags aggregate across the batch, and the rest of `postToolUseFailure` stays intact when its `error` is a string rather than the object carried by `errorOccurred`.

Copilot CLI 1.0.71 emits `postToolUse` as soon as an `ask_user` answer is selected or dismissed, before the next assistant output. Every post-tool success or failure therefore proves the boundary completed even when the tool is read-only, unnamed, or partially malformed; RimZ clears Waiting on that edge rather than requiring a later mutating call.

Copilot CLI 1.0.71 runs the standard installed `userPromptSubmitted` and `agentStop` hooks for each `general-purpose` child, using the parent task call's `toolCallId` as the child `sessionId` and leaving `transcriptPath` empty. RimZ joins that ID to the parent's `subagent.started` transcript record, recovers task metadata from the matching `tool.execution_start`, publishes the start model, and stores the child under the pane-local root. The child stop precedes `subagent.completed`; the parent's next `postToolUse` or final `agentStop` reconciles the completion model and exact `totalTokens` onto the established child without changing its parent, task, verdict, or pane. Repeated checkpoints are no-ops. The native `subagentStart` and `subagentStop` events still lack a child invocation ID and remain absent from the installed file.

**Install.** Install owns the whole hook file through its first-line `_rimz_managed` marker and owns one reversible `statusLine` object in `$COPILOT_HOME/settings.json`. Preview validates both candidates and shows both diffs before writing either file. Reinstall reclaims a marked hook file, canonicalizes a managed statusline without nesting, and refuses an unmarked hook file or a non-command statusline value. The statusline preserves rendering options such as `padding`, stores the complete prior JSON value under `_rimz_wrapped`, and passes the provider payload byte-for-byte to that prior command while forwarding its stdout and exit status.

Hook readiness requires both the marked hook file and the canonical managed statusline command. Either artifact alone remains detectable as a partial install for repair or uninstall. Uninstall strictly restores or removes only the managed statusline first, then removes only the marked hook file; a hook-removal failure restores the original settings bytes atomically. Read-only readiness and wrapped-command probes accept later JSONC edits, while install and uninstall reject comments or trailing commas instead of discarding them.

## Launch and resume

`copilot` launches a fresh interactive session and `copilot --resume <session_id>` restores one. Ask mode adds no flags, plan uses `--plan`, auto uses `--autopilot`, and yolo uses `--allow-all`; model and effort profiles map to `--model` and `--effort`.

Copilot CLI 1.0.70 exposes `-i, --interactive <prompt>`, which starts the stock interactive UI and automatically executes the prompt. RimZ appends `--interactive <prompt>` after profile and permission arguments for prompt-seeded panes and supervised `rimz agents copilot -p` runs, preserving native asks and the hook-driven completion path. Copilot's native non-interactive `-p` mode remains deferred because adopting its process output would introduce a second supervised-run backend and remove the pane's interactive answer surface. Fork and ping stay unsupported: Copilot 1.0.70 exposes `/fork` only as an experimental interactive slash command with no launch flag, and no lowest-effort priming turn is defined.

A supervised `rimz agents copilot -p` run reports its lifecycle and exit status from `agentStop` and pane liveness. The hook's validated `transcriptPath` supplies the final visible `assistant.message`, so the run record, `agents wait --stream`, durable transcript, and `agents history` use the same provider-native conversation source.

`--resume <session_id>` binds the id as a space-separated value on 1.0.70; `--session-id <session_id>` is the unambiguous exact-match alternative (a required-argument flag) if a future Copilot argument parser stops consuming the optional `--resume` value.

## Context and transcript

`$COPILOT_HOME/session-state/<sessionId>/events.jsonl` is the append-only conversation source captured from Copilot CLI 1.0.70. RimZ derives this path once the file exists for a safe single-component session ID, then accepts `agentStop.transcriptPath` only when its filename is `events.jsonl` and its parent matches that session ID; child hooks therefore publish no phantom transcript path.

The visible-message reader recognizes only root `user.message` and `assistant.message` records, reads visible `data.content`, and parses the RFC3339 top-level timestamp. Final-message extraction reads the shared record-aligned tail first and falls back to the full file only when that complete tail contains no assistant. The separate bounded subagent fold recognizes typed `tool.execution_start`, `subagent.started`, and `subagent.completed` records keyed by exact `toolCallId`, skips malformed and unknown lines, and reads no assistant output or parent token aggregates. User text and recovered child prompts pass through the shared control-prompt sanitizer; assistant text is preserved.

Live context comes from Copilot's command statusline in `$COPILOT_HOME/settings.json`, which runs `RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source copilot`. The payload's `session_id` binds the sidecar to lifecycle identity. Non-empty session name and CLI version map directly. A concrete model ID remains unchanged; an `auto` selector resolves the concrete target after the rightmost `→` or `->`, strips only recognized terminal effort/multiplier qualifiers, and publishes that target as both model identity and display label. Malformed or targetless selectors retain the provider's literal `auto` and display input.

For the current window, `displayed_context_limit` wins over `context_window_size`, `current_context_used_percentage` wins over `used_percentage`, and the legacy `remaining_percentage` remains available. `current_context_tokens` publishes the provider's occupied-window scalar, and a missing fill derives only from that scalar plus a positive selected denominator. `current_usage` maps component-for-component into the latest-call composition. The cumulative `total_input_tokens` includes cache creation and cache reads, so session fresh input is its saturating difference from those two counters. Copilot's `total_output_tokens` includes its `total_reasoning_tokens` subset, so RimZ publishes their saturating difference as ordinary output and the reasoning count as thinking; display and local pricing add those disjoint fields exactly once. Ambiguous `total_tokens` and `last_call_input_tokens` remain unmodeled.

Statusline `cost` contributes duration, API duration, and line-change counters. The statusline feed also prices cumulative fresh input, output plus thinking, cache creation, and cache reads against the local price book at the same normalized concrete model published to renderers, then publishes the positive finite result as an estimated session cost. Statusline `ai_used` is session context only and cannot reconstruct the account's monthly allowance; premium-request counters and `remote.connected` likewise establish neither the estimate's missing billing adjustments nor a RimZ remote-control transport.

**OTel fallback.** Metadata-only OTel `chat` spans remain the fallback when the managed statusline is absent, replaced, or unreadable. A healthy canonical statusline suppresses OTel refresh so a sparse asynchronous span cannot replace its richer token scopes. A newly-born room still supplies one private `agent-telemetry/copilot-otel.jsonl` cache, preserves an ambient `COPILOT_OTEL_FILE_EXPORTER_PATH`, respects OTLP-only configuration, and pins `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=false` for a RimZ-owned file.

Source precedence remains an already-anchored sidecar, the inherited/explicit file exporter, then the newest direct JSONL child of `$COPILOT_HOME/otel`. The bounded reader filters exact conversation IDs, selects the latest complete timestamped `chat` span, and anchors a managed file before it contains a usable span so a later asynchronous append is visible on Tick/Watch. Model display prefers `model_display_name`, then the OTel `model_id`, then the lifecycle scalar. A nonzero latest-call composition counts as session history and renders its fresh/cache/output line, but without an authoritative denominator it draws no percentage gauge or window token.

Copilot 1.0.70 concurrent capture proved complete append-only OTel records and per-session visibility beyond the 64 KiB tail. It did not prove session-cumulative dollars: `github.copilot.cost` was per-chat zero and the aggregate span carried no dollar total. Live dollars therefore use only the statusline token estimate. Historical `rimz stats` and provider-dashboard spend instead come from finalized 1.0.71 shutdown counters priced by the local book; authoritative account dollars remain unsupported.

The bounded OTel tail reader requires an exact `gen_ai.conversation.id`, prefers `gen_ai.response.model` over `gen_ai.request.model`, and maps fresh input, cache read/write, and output counts. It supplies metadata-only model/token composition only while the statusline bridge is unhealthy.

## Account and balance

The account probe reads only the non-secret login identity in `$COPILOT_HOME/config.json` (`lastLoggedInUser`, falling back to the first valid `loggedInUsers` entry). The file is JSONC, so the read-only probe accepts comments and trailing commas through the shared comment-tolerant reader. It reports a github.com login as `account_id` and qualifies an enterprise login as `login@host`, keeping identities on different GitHub hosts distinct. Host normalization accepts the CLI's safe scheme, path, and port forms, folds case and trailing dots, and rejects malformed authorities.

Only a validated config identity establishes the displayed account. A missing config or a valid config without an identity reports logged out, while an unreadable or malformed config remains unavailable for the short retry path; environment-token presence alone does not claim a login.

For a metered login, the shared account-usage refresh issues one bounded read-only `GET https://<api-host>/copilot_internal/user`. Credential precedence matches Copilot: `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, then the `copilotTokens` entry whose normalized `<host>:<login>` key exactly matches the active config identity. Host precedence is `COPILOT_GH_HOST`, `GH_HOST`, the active config host, then `github.com`; public GitHub maps to `api.github.com`, enterprise hosts gain one `api.` prefix, and valid explicit ports survive. Config tokens never fall across identities or host overrides.

The usage request retains only a versioned SHA-256 digest of normalized host plus credential as its cache owner; config-backed reads also carry `config.json`'s mtime. Headers, response bodies, plaintext credentials, and request paths stay out of errors and durable state. Missing or provider-rejected credentials settle quietly, while transport, unexpected HTTP, and unusable-response failures take the shared short retry/reporting path.

Modern `quota_snapshots` win per usable scope, with missing scopes filled from `monthly_quotas` plus `limited_user_quotas`. The provider's explicit remaining percentage wins; otherwise RimZ derives it from a positive entitlement and remaining count, clamps the result, accepts RFC 3339 or date-only resets, suppresses zero-entitlement placeholders, and renders explicit unlimited allowances as lifted rows. Chat is the Copilot CLI mana lane, labeled `cr` for token-based AI Credit billing and `cht` otherwise; genuine `premium_interactions` renders as `prm`. Both are authoritative named durationless scopes, so Copilot exposes no invented 5h/7d windows and these allowances do not enter pace, priming, roll-forward, auto-continue, or provider-capacity policy.

The cleaned `copilot_plan` survives a plan-only Business response.

## Cost

Realtime cost is partial and estimated. The statusline's cumulative disjoint token scopes are priced at the currently resolved model, including cache creation and cache-read rates; premium-request billing and model changes within one accumulated live session are not reconstructed. Historical spend discovers direct `$COPILOT_HOME/session-state/*/events.jsonl` files, carries `session.start.data.context.cwd`, and folds each shutdown's per-model cumulative fresh-input, cache-read, cache-write, and inclusive-output counters into timestamped deltas. Field omissions preserve the prior baseline; a regression resets only that field without replaying or fabricating usage. Detailed `tokenDetails` win, aggregate input subtracts both cache categories, and a concrete top-level model/usage record is the fallback when `modelMetrics` is absent. Known models use the shared price book; unknown models retain zero-dollar token entries for the normal price-refresh healing path. AI Credits remain opaque and do not enter dollars, so AccountSpend coverage is partial and account budgets remain ineligible.

## Known gaps

Run `rimz coverage` for the current wired/partial/unsupported matrix. The gaps below are the ones with a reason worth recording.

- **Authoritative dollars are unavailable.** RimZ does not read keychains or browser cookies, write plaintext `copilotTokens`, query billing budgets, or synthesize extra-credit balances. Provider dollars, extra credits, completions usage, official billing reports, and `AccountSpend` stay unsupported because no suitable dollar ledger exists. AI Credits remain opaque and do not enter dollars.
- **Subagent coverage is partial.** The child standard hooks expose only prompt and stop boundaries. Parent transcript records supply the child model at start and its exact total after completion, but child tool activity and permissions remain unavailable.
- **Yolo has no policy preflight.** `--allow-all` can be suppressed by managed `permissions.disableBypassPermissionsMode = "disable"`, so a `copilot-yolo` launch under that policy degrades to a normal permission posture instead of failing fast at the entry point. Resolving the merged policy value at launch and refusing when the requested bypass is unavailable is the fail-fast footprint, deferred until the merged-policy read lands.
- **Native `-p` stays deferred.** Adopting Copilot's non-interactive mode would introduce a second supervised-run backend and remove the pane's interactive answer surface.
- **No default context window.** `default_context_window` stays `None` because the window depends on the live model and the `--context <tier>` selector (`default` or `long_context`, new in 1.0.70); the statusline's provider-selected denominator remains authoritative.
- **Remote control is unsupported.** Statusline `remote.connected` establishes no RimZ remote-control transport.
- **Live verification gaps.** A logged-in Copilot CLI 1.0.70 prompt-mode capture verified successful-turn ordering, `agentStop.transcriptPath`, visible transcript message shapes, metadata-only OTel `chat` spans, resolved/requested model fields, token fields, and asynchronous exporter shutdown. Permission variants, `ask_user` options, resume PID ancestry, multi-turn interactive streaming, and remote sessions remain unverified.
- **The hook marker is version-dependent.** Copilot CLI 1.0.70 accepts the unknown top-level hook marker; if a later release stops doing so, move the marker into the first hook entry's `env` overlay.

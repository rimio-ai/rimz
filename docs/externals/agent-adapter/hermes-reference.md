# Hermes Agent protocol reference

Hermes Agent is Nous Research's coding agent. This page records its upstream terminal, hook, session, and account surfaces for contributors preparing an integration. It makes no claim that RimZ supports Hermes. The [adapter playbook](../../contributing/agent-adapters.md#step-1--land-the-protocol-reference-first) owns the integration sequence.

## Baseline and evidence

The baseline is [v0.22.2](https://github.com/NousResearch/hermes-agent/releases/tag/v0.22.2), published on 2026-10-15, at commit [`d923a5eb487a17c0a68801bba9ef0f56c81040a8`](https://github.com/NousResearch/hermes-agent/commit/d923a5eb487a17c0a68801bba9ef0f56c81040a8). Checked on 2026-10-15. All source links below use that immutable commit, including links to upstream documentation.

Source-verified means static inspection of that revision, including emission sites where named. Documentary means upstream prose at that revision. An inference is labeled explicitly. Neither establishes recorded-session behavior or reliable event delivery.

The source archive was downloaded without installing or running Hermes. No provider calls, hook installation, configuration changes, credential reads, or user-session reads formed this reference. No live terminal or recorded-session validation exists for this baseline.

The locally installed revision, `86e5efb0ae3acc1ae574e745a509803a53aba443` (2026-06-07), is a different protocol baseline. Earlier findings about its query mode, approval defaults, hook catalog, and schema do not apply here without rechecking.

## Source map

| Surface | Pinned upstream source |
| --- | --- |
| Launch and initial prompt | [Parser][parser], [chat dispatch][main], [classic CLI][cli], [single-query dispatch][single-query] |
| Plugin registration and delivery | [Plugin API][plugins], [discovery][discovery], [callback dispatch][dispatch], [plugin isolation][isolation] |
| Shell hook wire and consent | [Shell hooks][shell] |
| Hook meanings | [Observer hook documentation][observer-doc], [turn finalizer][finalizer], [human-input helper][human] |
| Session history | [SQLite schema][schema], [session store][state], [compression][compression] |
| Other transports | [Programmatic integration documentation][integration-doc] |

## Processes and transports

At this pin, classic `hermes chat --cli` calls `cli.main`, constructs `HermesCLI`, and runs the agent from a worker thread. The worker calls `self.agent.run_conversation` and binds native approval, secret, and sudo callbacks. The classic terminal owns those prompts. See [chat dispatch][main], [CLI construction][cli-main], and [the worker][worker].

`hermes --tui` selects the Ink UI through a separate launcher. The launcher passes the profile environment to the UI. The [pinned integration guide][integration-doc] identifies `tui_gateway/server.py` as its agent protocol endpoint. Classic CLI and TUI process identities must be checked separately. See [the TUI launcher][tui-launch].

General plugins default to `plugins.isolation: in_process`. With `host`, third-party plugins run in a separate host per profile. Bundled plugins remain in-process. A plugin callback's PID therefore need not identify the terminal process. The [isolation contract][isolation] also lists host-only restrictions, including unavailable approval-transport registration.

The [rolling CLI page](https://hermes-agent.nousresearch.com/docs/user-guide/cli) describes a gateway-attached classic CLI. That description differs from this pin's dispatch and worker code. This page follows the pinned source. Do not combine rolling process ownership with this revision's callback identity.

The following transport catalog is documentary. Full request and response schemas, transport parity, and live behavior remain outside the verified subset.

| Surface | Transport and documented capabilities |
| --- | --- |
| `hermes acp` | Stdio JSON-RPC. Session creation, prompts, streaming, tool events, permission requests, fork, cancel, and authentication. |
| TUI gateway | Stdio or WebSocket JSON-RPC. Session control, prompts, slash commands, streaming, and native input requests. |
| API server | HTTP and SSE. OpenAI-compatible inference and run APIs with events and approval replies. |
| `run_agent.AIAgent` | In-process Python embedding. The embedding host supplies UI callbacks. |

For gateway WebSocket clients, the pinned guide requires `client.capabilities` with `{"server_requests": true}` after `gateway.ready`. Native prompts are server-to-client JSON-RPC requests. Replies use the same request ID. Cancellation uses `request.cancel`, and reconnect results can carry `open_requests`. These are protocol-host obligations, separate from observing terminal hooks. See [programmatic integration][integration-doc].

## Launch, prompts, and resume

These forms come from the [argument parser][parser] and [dispatch][main]. They are source-derived command shapes, not commands run during this reference pass.

| Form or flag | Behavior at this pin |
| --- | --- |
| `hermes chat --cli` | Force the classic prompt_toolkit UI, regardless of `display.interface`. |
| `hermes --tui` | Select the Ink UI. |
| `hermes chat --cli -q "Review the working tree"` | Seed the first turn, then remain interactive when stdin and stdout are TTYs. |
| `--query-file PATH` | Read the initial query from a file. `-` reads stdin. Mutually exclusive with `-q`. |
| `hermes chat -q "Review the working tree" --oneshot` | Answer and exit. This chat-subcommand flag is boolean. |
| `-Q`, `--quiet` | Suppress presentation. A query answers and exits, including on a TTY. |
| `hermes -z "Review the working tree"` | Top-level one-shot mode. Print the final response and automatically bypass approvals. |
| `--usage-file PATH` | Write a JSON usage report for top-level `-z`, including failed runs. |
| `--format stream-json` | Query-only JSONL output. Implies quiet mode and rejects `--tui`. |
| `--resume ID`, `-r ID` | Resume by native session ID. Title resolution also exists. |
| `--resume latest`, `-c` | Resume the most recent session for the workspace. `-c NAME` selects a named session. |
| `--in DIR` | Change directory before session lookup. A resumed session stays in that directory. |
| `--no-restore-cwd` | Disable restoration of the resumed session's recorded working directory. |
| `--model MODEL`, `--provider PROVIDER` | Override the inference model or backend for this invocation. |
| `--reasoning LEVEL` | Invocation override: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`. Backend support is separate. |
| `--toolsets NAMES` | Select comma-separated toolsets. |
| `--skills NAMES`, `-s NAMES` | Preload skills. Repeat the flag or use commas. This does not establish an allow-only skill restriction. |
| `--accept-hooks` | Consent to unseen configured shell hooks. This does not approve tool actions. |
| `--yolo` | Bypass dangerous-command approval prompts. |
| `--safe-mode` | Disable customizations, including plugins and MCP servers. This changes the available integration surface. |

Interactive query seeding requires both `sys.stdin.isatty()` and `sys.stdout.isatty()`. `--oneshot`, quiet mode, or non-TTY stdio takes the finite query path. The seed is submitted literally, without slash-command interpretation. See [the predicate][cli] and [single-query dispatch][single-query]. A positional prompt after `--` is not the parser's documented launch surface.

The JSONL emitter writes `system` with subtype `init`, `text`, `tool_use`, `tool_result`, and terminal `result` records. Diagnostics use stderr. The protocol is distinct from hook stdin. See [the emitter][stream-json].

## Hook registration and delivery

Hermes has separate plugin, shell-hook, and messaging-gateway hook systems. The [plugin API][plugins] accepts `register(ctx)` and `ctx.register_hook(event_name, callback)`. User plugins live under `$HERMES_HOME/plugins/NAME/`, with `plugin.yaml` and `__init__.py`. Configuration must enable them through `plugins.enabled`. Project plugin discovery requires `HERMES_ENABLE_PROJECT_PLUGINS=1`. See [discovery][discovery].

Shell hooks use the `hooks` section of `config.yaml` and register callbacks through the same event machinery. Messaging gateway `HOOK.yaml` bundles are a separate surface. See [shell hooks][shell] and [gateway hooks][gateway-hooks].

### Event families

This is the terminal-integration subset. The complete accepted names are in [`VALID_HOOKS`][plugins]. Fields vary by emitter. The [observer guide][observer-doc] documents common payloads, and the source links identify critical boundaries.

| Events | Meaning and payload constraints |
| --- | --- |
| `on_session_start` | Fresh session after system-prompt construction. Carries `session_id` and model information. Resume can continue without refiring it. |
| `pre_llm_call` | Turn-level entry before the tool loop. Includes session/turn identity and the user message. It can contribute context. |
| `post_llm_call` | Turn-level assistant output. It is not an API-attempt event or sufficient evidence of a clean turn. |
| `on_session_end` | End of one conversation run. Finalizer fields include `completed`, `failed`, `interrupted`, and `turn_exit_reason`. |
| `on_session_finalize` | Teardown of an active session identity. Distinct from per-turn end. |
| `on_session_reset` | Identity transition. Can carry `old_session_id`, `new_session_id`, and `reason`. |
| `pre_api_request`, `post_api_request`, `api_request_error` | Main-loop provider requests and failures. Common correlations include `session_id`, `turn_id`, and `api_request_id`. Retries can reuse the request ID. |
| `pre_auxiliary_call`, `post_auxiliary_call` | Auxiliary model attempts, such as compression and titling. Carry `aux_task`. |
| `pre_tool_call`, `post_tool_call` | Tool execution boundaries. Correlate with `tool_call_id` and turn/session identity when present. |
| `pre_approval_request`, `post_approval_response` | Approval observation. Includes `session_key`, `surface`, command details, and the final `choice`. |
| `on_human_input_request`, `on_human_input_resolved` | Native human-wait bracket. Pair by `request_id`. See [native prompts](#native-prompts-and-permissions). |
| `subagent_start`, `subagent_stop` | Delegated child boundaries with explicit parent and child identities. |

`on_session_end` is turn-scoped. It must not retire a session after each answer. The [turn finalizer][finalizer] includes interrupted and failed outcomes. CLI teardown can add reduced lifecycle observations. Consumers need correlation and deduplication, not a one-hook-one-transition assumption. See [observer documentation][observer-doc].

`api_request_id` is opaque. The retry loop can reuse it, so retain retry metadata when identifying attempts. See [request orchestration][retry-loop]. Parent/child payloads can include `parent_session_id`, `parent_turn_id`, `child_session_id`, and `child_subagent_id`. Approval `session_key` and native `session_id` are separate fields. See [observer payload contracts][observer-doc].

### Callback effects and failures

| Surface | Return or failure behavior |
| --- | --- |
| Approval and human-input observers | Return values do not answer, approve, or veto the native prompt. |
| `pre_tool_call` | Supports `block`, `modify`, and `approve` directives. `approve` requests human approval, not automatic permission. |
| `pre_tool_call` policy resolution | A block takes precedence over escalation. Modified argument fields are merged. |
| `pre_llm_call` | Can add context to the turn. An observer must avoid returning unintended context. |
| Bounded plugin callbacks | `plugins.hook_callback_timeout` defaults to 30 seconds. A timed-out or still-running `pre_tool_call` callback fails closed. Other bounded hooks fail open. |

See [plugin dispatch][dispatch], [the public invocation contract][plugins], and [shell reply parsing][shell]. Callback isolation does not establish durable delivery, retries, or delivery after a process dies.

The synchronous dispatcher does not apply that timeout to every hook. Human-input, approval, finalize/reset, and subagent-start callbacks are outside its bounded set. `subagent_stop` runs on the caller thread. On bounded events, the default 30-second callback deadline can expire before a shell hook's separate 60-second deadline. See [the dispatch policy][dispatch].

### Shell hook wire

[Shell hook serialization][shell] sends one JSON object on stdin. The callback executes the parsed command directly with `shell=False`, piped stdin/stdout/stderr, and a timeout. Shell operators require an explicitly invoked shell.

| Field | Contents |
| --- | --- |
| `hook_event_name` | Event name. |
| `tool_name` | Native tool name, or null. |
| `tool_input` | The `args` object, or null. |
| `session_id` | Emitter `session_id`, otherwise `parent_session_id`, otherwise an empty string. |
| `cwd` | Current working directory at serialization. |
| `profile` | Active Hermes profile at firing time. |
| `extra` | Remaining emitter fields, including correlations, outcomes, and possibly `session_key`. |

Successful exit with empty stdout contributes no directive. This remains neutral with `fail_closed: true`. Optional stdout JSON can carry directives or context. Diagnostics belong on stderr. An approval observer cannot turn a stdout directive into an approval response. See `_evaluate_result` and `_parse_response` in [shell hooks][shell].

| Result | Effect |
| --- | --- |
| Spawn failure or timeout | Fail open by default. With `fail_closed`, block only a blocking-capable event. |
| Exit 2 on `pre_tool_call` | Block. Prefer a JSON block message, then stderr, then the default reason. |
| Other nonzero exit | Log the failure and parse stdout. A fail-closed blocking hook blocks without a directive. |
| Invalid nonempty stdout | No directive by default. A fail-closed blocking hook blocks malformed non-object output. |
| Timeout | Default 60 seconds, maximum 300. Hermes kills the hook's process tree. |

At this pin, `_BLOCKING_EVENTS` contains `pre_tool_call` only. `fail_closed` does not make an observation event into a veto. Tool matchers use a full regex match. See [shell hook validation and execution][shell].

Consent is per `(event, command)`, recorded in `$HERMES_HOME/shell-hooks-allowlist.json`. Editing a script's contents does not change that consent key. `--accept-hooks`, `HERMES_ACCEPT_HOOKS=1`, or `hooks_auto_accept: true` can accept unseen commands. Those switches change command-execution consent. This reference installs nothing. See [consent handling][shell] and [the parser][parser].

## Native prompts and permissions

The [human-input helper][human] emits a request immediately before entering a native wait and emits resolution in `finally`. Fields include `kind`, `request_id`, `session_id`, `session_key`, `platform`, and a force-redacted `prompt`. The resolution adds `outcome`, defaulting to `error` when the wait supplies none. The helper does not include the typed password or answer.

That redaction applies to this helper's payload. Other hook families can carry user messages, tool arguments, commands, or results. An integration must treat them as sensitive data.

Source brackets cover [`clarify`][clarify], [sudo][sudo], and [approval waits][approval-prompt]. The helper carries no turn or tool ID by default. Approval observers use another correlation shape and do not share the generic human-input UUID. Source-level brackets must be checked before claiming coverage of other panels. A `pre_tool_call` for `clarify` establishes an attempted call, not a rendered question.

The classic CLI renders approval choices through `_approval_callback` and batches questions through `_clarify_callback`. Approval choices can include `once`, `session`, `always`, and `deny`. Policy can remove session or permanent approval. Questions return submitted, timed-out, or cancelled outcomes. The native callback owns presentation and answer handling. See [modal callbacks][modal].

The [pinned security guide][security-doc] defines `approvals.mode` as `smart` (default), `manual`, or `off`, with a 300-second approval timeout. Smart approval can decide without a human prompt. The human-input guide excludes those automatic decisions from human-wait events. Cron, finite single-query, and unattended modes have separate policies, documented as `deny` by default.

`--yolo` and top-level `-z` bypass approvals. `--accept-hooks` accepts hook commands. None is a neutral substitute for ordinary interactive permissions. This reference defines no mapping onto RimZ permission modes.

## Session identity and history

`$HERMES_HOME/state.db` is the SQLite session store. The schema version is 31 at this pin. Sessions carry `parent_session_id`, source, model, working directory, start/end times, and an end reason. The store uses WAL where available. See [schema declarations][schema] and [database opening][state].

| Boundary | Identity behavior |
| --- | --- |
| Fresh conversation | Establish a native `session_id`. Fresh-session hooks run after prompt construction. |
| Resume | Resolve the requested ID or title and load history. A continued identity need not emit a fresh-session hook. |
| Reset or new session | Switch identities. Lifecycle payloads can identify the old and new IDs. |
| Compression, default in-place path | Keep the session ID. Archive summarized rows and publish the replacement active history. |
| Compression, rotation path | Publish a continuation with the old session as `parent_session_id`, then switch the agent to that ID. |
| Per-turn completion | Emit the run outcome. The same identity can accept another turn. |

See [session argument resolution][main], [session initialization][conversation], [in-place compression][in-place], and [rotation publication][compression]. A history reader must distinguish compression continuations from branches and delegated children. `parent_session_id` alone does not identify compression.

Compression marks named `compaction` and `compaction.attempt` use a separate Relay telemetry path. Emission requires an available Relay scope. Their existence does not establish an ordinary plugin or shell-hook subscription. See [mark selection][compaction-events] and [Relay emission][relay-compaction]. Compression engine and memory-provider callbacks are also separate interfaces.

### SQLite fields

This is the integration-relevant subset of [the schema][schema]. JSON-bearing columns are SQLite text, not a JSONL stream.

| Table | Fields relevant to an external reader |
| --- | --- |
| `sessions` identity | `id`, `source`, `created_source`, `session_key`, `parent_session_id`, `profile_name`, `transport_profile` |
| `sessions` execution | `model`, `model_config`, `cwd`, `started_at`, `ended_at`, `end_reason`, `message_count`, `tool_call_count` |
| `sessions` usage | `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `reasoning_tokens`, `api_call_count` |
| `sessions` billing | `billing_provider`, `billing_base_url`, `billing_mode`, `estimated_cost_usd`, `actual_cost_usd`, `cost_status`, `cost_source`, `pricing_version` |
| `messages` identity | `id`, `session_id`, `message_uid`, `timestamp`, `role` |
| `messages` content | `content`, `api_content`, `tool_calls`, `tool_call_id`, `tool_name`, `finish_reason`, reasoning fields |
| `messages` history and display | `active`, `compacted`, `_compressed_summary`, `display_kind`, `display_metadata`, `display_identity`, `display_order` |
| `session_model_usage` | Usage and cost split by session, model, billing backend/mode, and task. |

The `role` column has no SQL enum constraint. Normal conversation roles include `user`, `assistant`, `tool`, and `system`. Role alone does not prove a human authored the text. The pinned integration guide describes generated turns with `display_kind` and `display_metadata`, including hidden scaffolding and process-completion markers. A history reader must preserve those distinctions. See [the schema][schema], [persistence][persistence], and [history presentation contracts][integration-doc].

This reference does not supply a validated external-reader query. Active rows, compression summaries, rewinds, display metadata, and live writes require recorded-session fixtures before a history parser can claim correctness.

The inspected [persistence path][persistence] appends to SQLite. `/save json` is an explicit export, with optional lineage segments. A per-session JSON snapshot is not a required live transcript source. See [session export][export].

## Context, usage, and cost

Request usage, session totals, and context occupancy are different measurements.

| Measurement | Source and limits |
| --- | --- |
| Request tokens | `post_api_request` receives normalized usage, when the response supplies it. The helper adds `prompt_tokens` and `total_tokens`. |
| Session totals | SQLite counters accumulate input, output, cache-read, cache-write, and reasoning tokens, plus API-call counts. |
| Context occupancy | The classic status bar uses compressor usage and can anchor to the turn's first response plus an estimate for appended messages. |
| Context capacity | The compressor's `context_length` supplies the denominator. It is separate from session totals. |
| Cost | SQLite distinguishes estimated and actual dollars and records billing backend and cost provenance. Token availability alone does not establish a bill. |

See [API usage normalization][api-hooks], [usage schema][schema], [the status-bar snapshot][status-bar], and [pricing semantics][pricing]. The status bar clamps occupancy to the context window and marks estimated display sources. Its display anchoring does not replace the compressor's real last-request usage for compression decisions.

Canonical `prompt_tokens` adds uncached input, cache-read, and cache-write tokens. `total_tokens` adds output to that prompt total. Do not add reasoning again. Child cost can be rolled into the parent's in-memory estimate, so summing parent and children can double count. See [canonical usage][pricing] and [child cost rollup][child-results].

An inference backend can change during a session. Hermes itself is the agent, while `--provider` selects an inference backend. Model and billing dimensions must accompany usage. This page establishes no universal provider quota or subscription-balance endpoint.

## Home, profiles, authentication, and skills

The [home resolver][home] applies a context-local home override before `HERMES_HOME`, then the platform default. On ordinary Linux installs, that default is `~/.hermes`. Routed profiles can therefore differ from the process's original environment. Source readers must resolve the active profile before selecting its store or hooks.

| Surface | Location or control |
| --- | --- |
| Configuration | `$HERMES_HOME/config.yaml` |
| Session database | `$HERMES_HOME/state.db` |
| User plugins | `$HERMES_HOME/plugins/` and `plugins.enabled` |
| Shell-hook consent | `$HERMES_HOME/shell-hooks-allowlist.json` |
| Skills | `$HERMES_HOME/skills/`, plus configured discovery sources |
| Provider auth store | `$HERMES_HOME/auth.json` |
| Environment secrets | Hermes loads environment and dotenv sources. Exact precedence is backend and scope dependent. |

The [auth resolver][auth] also has a global-root auth-store fallback when the active profile uses another home. Inference: selecting a separate home alone does not prove credential isolation. Provider authentication can use API keys or OAuth, and the source supports credential pools. No credentials or account records were read to validate this reference.

At startup, the profile `.env` overrides shell exports. An explicitly supplied project dotenv fills gaps when the profile dotenv exists. Without it, the project dotenv can override shell values. Managed dotenv applies last with override. Multiplexed foreign-profile turns use scoped handling instead of copying that profile's secrets into process-global environment. See [environment loading][env].

`--skills` preloads named skills. The [skill configuration][skills-config] separately supports global and per-platform disabled sets, with essential skills excluded from disabling. These controls do not establish an allow-only view of every skill source.

## Native subagents

`delegate_task` creates child agents with explicit parent/child session and subagent identities. The [creation path][delegate] emits `subagent_start` after attaching the child, before execution. A child can still queue for execution after that event. The [observer guide][observer-doc] documents `subagent_stop` with status, summary, duration, and metadata-only tool history.

Child toolsets, model/provider selection, execution limits, and approval policy are separate delegation controls. Native child work is distinct from a separately launched terminal process. The [delegation guide][delegation-doc] documents monitoring, steering, and stop controls. Stop timing, parallel result order, and child approval routing need live validation before they can support attention-state claims.

The [stop emitter][child-results] runs during result processing, not at the child's last model token. It omits the start event's subagent IDs. Child approval callbacks default to automatic denial, with `delegation.subagent_auto_approve` selecting one-time approval. Child construction supplies no clarify callback. A child approval observation therefore does not establish a root-terminal wait. See [child configuration][child-config] and [construction][delegate].

## Verification gaps

| Required evidence | What remains unproved |
| --- | --- |
| Redacted recorded sessions at this pin | Hook order and outcomes for success, interruption, cancellation, errors, and resume. No user records were inspected. |
| Native prompt captures | Rendering order, answer selection, and coverage of every approval, secret, vault, and confirmation panel. |
| Compression and reset fixtures | Ordinary plugin compaction brackets, continuation selection, and history projection across resets and rewinds. Engine callbacks alone do not prove plugin events. |
| Process and isolation probes | Runtime ownership and environment stamps for classic CLI, TUI, and hosted plugins. Static paths establish intended ownership only. |
| Backend-specific runs | API hook parity and usage/cost completeness across inference transports, retries, auxiliary calls, and external runtimes. |
| Read-only history validation | Correct SQLite queries during concurrent writes, migration, pruning, compression, and recovery. |
| Delivery and crash tests | No verified hook replay, durable delivery, or process-crash completion guarantee. |
| Transport captures | Full ACP, gateway RPC, and HTTP/SSE schemas and parity. Their catalog above is documentary. |
| Home and skill isolation tests | Runtime credential precedence across routed profiles and a complete allow-only skill restriction. |

These gaps constrain later claims. They do not decide whether the eventual integration should use a built-in adapter or a process plugin. The next artifact is the [mapping worksheet](../../contributing/agent-adapters.md#step-2--map-the-protocol-onto-the-model), grounded in the pinned contract and recorded-session evidence.

[parser]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/_parser.py#L144-L310

[main]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/main.py#L1325-L1815

[cli]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/cli.py#L847-L869

[worker]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_chat_turn_mixin.py#L313-L367

[single-query]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_single_query.py#L489-L526

[stream-json]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/stream_json.py

[plugins]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/plugins.py

[discovery]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/plugins_discovery.py

[dispatch]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/plugins_dispatch.py

[isolation]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/plugin_isolation.py#L1-L87

[shell]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/shell_hooks.py

[gateway-hooks]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/gateway/hooks.py#L1-L70

[observer-doc]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/website/docs/developer-guide/observer-hooks.md

[integration-doc]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/website/docs/developer-guide/programmatic-integration.md

[finalizer]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/turn_finalizer.py#L453-L811

[human]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/human_input_hooks.py#L1-L72

[modal]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_modal_mixin.py#L1003-L1118

[security-doc]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/website/docs/user-guide/security.md

[conversation]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/conversation_loop.py#L773-L865

[schema]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_state_common.py#L283-L502

[state]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_state.py

[compression]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/conversation_compression.py#L3280-L3448

[persistence]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/session_persistence.py

[api-hooks]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/api_request_hooks.py#L32-L49

[status-bar]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_status_bar_mixin.py#L300-L345

[pricing]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/usage_pricing.py

[home]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_constants.py#L70-L119

[auth]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/auth.py#L490-L534

[skills-config]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/skills_config.py#L32-L59

[delegate]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/delegate_tool.py#L270-L334

[delegation-doc]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/website/docs/user-guide/features/delegation.md

[compaction-events]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/compaction_events.py#L129-L151

[relay-compaction]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/relay_compaction.py#L47-L93

[child-results]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/delegate_tool_results.py#L345-L405

[child-config]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/delegate_tool_config.py#L42-L60

[cli-main]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/cli.py#L1651-L1777
[retry-loop]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/conversation_loop.py#L1640-L1673
[clarify]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/clarify_tool.py#L111-L128
[sudo]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/terminal_tool_sudo.py#L185-L254
[approval-prompt]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/approval_prompt.py#L47-L54
[in-place]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/conversation_compression.py#L3738-L3801
[export]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_state_portability.py#L287-L317
[env]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/env_loader.py#L454-L590

[tui-launch]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/main_tui_launch.py#L350-L483

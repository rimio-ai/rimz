# Hermes Agent protocol reference

Hermes Agent is Nous Research's coding agent. This page records its upstream terminal, hook, session, and account surfaces for contributors preparing an integration. It makes no claim that RimZ supports Hermes. The [adapter playbook](../../contributing/agent-adapters.md#step-1--land-the-protocol-reference-first) owns the integration sequence. The [externals index](../README.md) lists other upstream contracts.

## Baseline and evidence

The baseline is [v0.22.2](https://github.com/NousResearch/hermes-agent/releases/tag/v0.22.2), published on 2026-10-15, at commit [`d923a5eb487a17c0a68801bba9ef0f56c81040a8`](https://github.com/NousResearch/hermes-agent/commit/d923a5eb487a17c0a68801bba9ef0f56c81040a8). Checked on 2026-10-15. All source links below use that immutable commit, including links to upstream documentation.

Source-verified means static inspection of that revision, including emission sites where named. Documentary means upstream prose at that revision. Documentary sections and inferences are labeled. Other descriptions cite inspected source. Neither evidence type establishes recorded-session behavior or reliable event delivery.

The source archive was downloaded without installing or running Hermes. No provider calls, hook installation, configuration changes, credential reads, or user-session reads formed this reference. No live terminal or recorded-session validation exists for this baseline.

## Source map

| Surface | Pinned upstream source |
| --- | --- |
| Launch and initial prompt | [Parser][parser], [chat dispatch][main], [classic CLI][cli-main], [single-query dispatch][single-query] |
| Plugin registration and delivery | [Plugin API][plugins], [discovery][discovery], [callback dispatch][dispatch], [plugin isolation][isolation] |
| Shell hook wire and consent | [Shell hooks][shell] |
| Hook meanings | [Observer hook documentation][observer-doc], [turn finalizer][finalizer], [human-input helper][human] |
| Native prompts | [Human-input helper][human], [classic UI callbacks][modal] |
| Session history | [SQLite schema][schema], [session store][state], [compression][compression] |
| Context and usage | [Request usage][api-hooks], [context display][status-bar], [pricing][pricing] |
| Profiles and authentication | [Home resolver][home], [profile paths][profiles], [auth stores][auth], [dotenv loading][env] |
| Subagents | [Creation][delegate], [result processing][child-results], [controls][delegate-actions] |
| Other transports | [Programmatic integration documentation][integration-doc] |

## Processes and transports

A provider in Hermes means an inference provider, selected with `--provider`. A Hermes profile groups configuration and session state under one home directory. See [profile paths][profiles].

At this pin, classic `hermes chat --cli` calls `cli.main`, constructs `HermesCLI`, and runs the agent from a worker thread. The worker calls `self.agent.run_conversation` and binds native approval, secret, and sudo callbacks. The classic terminal owns those prompts. See [chat dispatch][main], [CLI construction][cli-main], and [the worker][worker].

`hermes --tui` selects the Ink UI through a separate launcher. The launcher passes the profile environment to the UI. The [pinned integration guide][integration-doc] assigns native prompt requests to the attached UI through `tui_gateway/server.py`. Panel behavior and process identity need separate TUI validation. See [the TUI launcher][tui-launch].

General plugins default to `plugins.isolation: in_process`. With `host`, third-party plugins run in a separate host per profile. Bundled plugins remain in-process. A plugin callback's PID therefore need not identify the terminal process. The [isolation contract][isolation] also lists host-only restrictions, including unavailable approval-transport registration.

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

Flag effects below describe the classic CLI unless a row names another surface. Use `--cli` to select it independently of saved UI preferences.

| Form or flag | Behavior at this pin |
| --- | --- |
| `hermes chat --cli` | Force the classic prompt_toolkit UI, regardless of `display.interface`. |
| `hermes --tui` | Select the Ink UI. |
| `hermes chat --cli -q "Review the working tree"` | Seed the first turn, then remain interactive when stdin and stdout are TTYs. |
| `--query-file PATH` | Read the initial query from a file. `-` reads stdin. Mutually exclusive with `-q`. |
| `hermes chat --cli -q "Review the working tree" --oneshot` | Answer and exit. This chat-subcommand flag is boolean. |
| `-Q`, `--quiet` | Suppress presentation. A query answers and exits, including on a TTY. |
| `hermes -z "Review the working tree"` | Top-level one-shot mode. Print the final response, bypass tool approvals, and accept unseen configured shell hooks. |
| `--usage-file PATH` | Write a JSON usage report for top-level `-z`, including failed runs. |
| `--format stream-json` | Query-only JSONL output. Implies quiet mode and rejects `--tui`. |
| `--resume ID`, `-r ID` | Resume by native session ID. Title resolution also exists. |
| `--resume latest` | Prefer the workspace's most recent CLI session. Fall back to the global CLI session list when no session matches. |
| `-c`, `--continue` | Prefer an enabled, valid terminal breadcrumb, otherwise use latest-session lookup. `-c NAME` selects a named session. |
| `--in DIR` | Change directory before session lookup. A resumed session stays in that directory. |
| `--no-restore-cwd` | Disable restoration of the resumed session's recorded working directory. |
| `--model MODEL`, `--provider PROVIDER` | Override the inference model or backend for this invocation. |
| `--reasoning LEVEL` | Invocation override: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`. Backend support is separate. |
| `--profile NAME`, `-p NAME` | Select the profile home before ordinary argument parsing. |
| `--toolsets NAMES` | Select comma-separated toolsets. |
| `--skills NAMES`, `-s NAMES` | Preload skills. Repeat the flag or use commas. This does not establish an allow-only skill restriction. |
| `--accept-hooks` | Consent to unseen configured shell hooks. This does not approve tool actions. |
| `--yolo` | Bypass dangerous-command approval prompts. |
| `--pass-session-id` | Include the native session ID in the system prompt. |
| `--ignore-user-config` | Use built-in defaults instead of user configuration. Dotenv credentials still load. Configured integration behavior needs separate checking. |
| `--ignore-rules` | Skip automatic instruction, memory, and skill injection. Explicit `--skills` preloads still apply. |
| `--max-turns N` | Limit tool-calling iterations per conversation turn. |
| `--run-budget SECONDS` | Set a wall-clock budget for each conversation run. |
| `--safe-mode` | Disable customizations, including plugins and MCP servers. This changes the available integration surface. |

Interactive query seeding requires both `sys.stdin.isatty()` and `sys.stdout.isatty()`. `--oneshot`, quiet mode, or non-TTY stdio takes the finite query path. The seed is submitted literally, without slash-command interpretation. See [the predicate][cli] and [single-query dispatch][single-query]. The [chat parser][parser] has no positional prompt argument. Pass initial text through `-q` or `--query-file`.

The JSONL emitter writes `system` with subtype `init`, `text`, `tool_use`, `tool_result`, and terminal `result` records. Diagnostics use stderr. The protocol is distinct from hook stdin. See [the emitter][stream-json].

Latest-session lookup defines the workspace as the Git repository root, or the current directory outside Git. Bare `-c` and `--resume latest` can therefore choose different sessions. `--in DIR` sets the lookup location but does not remove the global fallback. See [resume selection][main].

### Planning and added system text

`/plan [task]` queues a planning prompt with the existing tools. The prompt permits a plan file under `.hermes/plans/` and instructs against implementation. The inspected handler activates no read-only tool policy. Do not treat `/plan` as enforced filesystem isolation. See [the handler][plan-handler] and [plan rules][plan-rules].

`hermes chat --cli -q "/plan Review the working tree"` submits that text literally. It does not invoke the slash command. The inspected launch parser supplies no dedicated planning-mode argument.

The inspected [chat parser][parser] exposes no plan or approval-mode enum flag. Approval policy comes from configuration, with `--yolo` as the bypass switch. Toolset selection is a separate control.

The [classic skill loader][skill-preload] suppresses `skills.auto_load` under `--ignore-rules`, while independently loading explicit `--skills` arguments.

`HERMES_EPHEMERAL_SYSTEM_PROMPT` selects additional system text during classic CLI initialization. The turn builder appends it to the cached system prompt at API time. This differs from `pre_llm_call` context, which enters the user message. See [initialization][system-overlay] and [prompt assembly][system-assembly].

### Tool names and side effects

Hermes's static classifications supply candidate names for a later mapping. They do not enumerate the tools enabled in a particular session.

| Classification | Names at this pin |
| --- | --- |
| Guardrail mutating set | `terminal`, `execute_code`, `write_file`, `patch`, `todo_list`, `memory`, `skill_manage`, `browser_click`, `browser_type`, `browser_press`, `browser_scroll`, `browser_navigate`, `send_message`, `cronjob_manage`, `delegate_task`, `process_manage` |
| Direct file-edit set | `write_file`, `patch` |

See [guardrail names][tool-classes] and [file-effect classification][file-classes]. `write_file` creates or replaces a whole file. `patch` edits matching text. Its OpenAI-family schema also exposes multi-file patch operations. See [file-tool schemas][file-tools].

The guardrail set includes `send_message`, whose module intentionally does not register it as a model-callable tool. Plugins, MCP servers, toolsets, and availability checks change the runtime catalog. Unknown tools remain effect-capable in the result classifier. Static set membership alone cannot establish runtime coverage. See [message-tool registration][message-tool] and [tool assembly][tool-assembly].

## Hook registration and delivery

The [pinned hooks guide][hooks-doc] lists plugin hooks, shell hooks, messaging-gateway hooks, and outbound webhooks. Outbound HTTP delivery through `hooks.outbound` is documentary here. Its wire and delivery behavior remain unchecked.

The [plugin API][plugins] accepts `register(ctx)` and `ctx.register_hook(event_name, callback)`. User plugins live under `$HERMES_HOME/plugins/NAME/`, with `plugin.yaml` and `__init__.py`. Configuration must enable them through `plugins.enabled`. Project plugin discovery requires `HERMES_ENABLE_PROJECT_PLUGINS=1`. See [discovery][discovery].

Shell hooks use `hooks.<event_name>` lists in `config.yaml` and register through the same event machinery. Each entry has these fields, checked by [the shell-hook parser][shell].

| Field | Default and meaning |
| --- | --- |
| `command` | Required command string. |
| `matcher` | Optional full-match regex for pre/post-tool names. |
| `timeout` | 60 seconds, capped at 300. |
| `fail_closed` | `false`. Also accepts `failClosed`. Applies to `pre_tool_call` failures. |

Messaging gateway `HOOK.yaml` bundles are a separate surface. See [gateway hooks][gateway-hooks].

### Event families

This documentary table summarizes the [pinned observer guide][observer-doc]. Fields vary by emitter. The source-verified boundaries below qualify that summary. The complete accepted names are in [`VALID_HOOKS`][plugins].

| Events | Meaning and payload constraints |
| --- | --- |
| `on_session_start` | Fresh session after system-prompt construction. Carries `session_id` and model information. Resume can continue without refiring it. |
| `pre_llm_call` | Turn-level entry before the tool loop. Includes session/turn identity and the user message. It can contribute context. |
| `post_llm_call` | Turn-level assistant output. It is not an API-attempt event or sufficient evidence of a clean turn. |
| `on_session_end` | Turn outcome, not session teardown. Finalizer fields include `completed`, `failed`, `interrupted`, and `turn_exit_reason`. |
| `on_session_finalize` | Teardown of an active session identity. Distinct from per-turn end. |
| `on_session_reset` | Identity transition. Classic CLI carries the new `session_id`. Gateway reset can also carry `old_session_id` and `new_session_id`. |
| `pre_api_request`, `post_api_request`, `api_request_error` | Main-loop provider requests and failures. Common correlations include `session_id`, `turn_id`, and `api_request_id`. Retries can reuse the request ID. |
| `pre_auxiliary_call`, `post_auxiliary_call` | Auxiliary model attempts, such as compression and titling. Carry `aux_task`. |
| `pre_tool_call`, `post_tool_call` | Tool execution boundaries. Correlate with `tool_call_id` and turn/session identity when present. |
| `pre_approval_request`, `post_approval_response` | Approval observation. Includes `session_key`, `surface`, command details, and the final `choice`. |
| `on_human_input_request`, `on_human_input_resolved` | Native human-wait bracket. Pair by `request_id`. See [native prompts](#native-prompts-and-permissions). |
| `subagent_start`, `subagent_stop` | Delegated child boundaries. Both carry parent/child session IDs. The stop event omits the start event's subagent IDs. |

Additional names come from [the source catalog][plugins]. These scopes follow its comments, not live captures.

| Events | Catalog scope |
| --- | --- |
| `pre_verify` | Stop-gate verification. Can request continued work. It is separate from a tool veto. |
| `agent_loop_stopped` | Gateway interruption observer. Does not establish a classic-CLI idle notification. |
| `on_stream_start`, `on_stream_delta`, `on_stream_end`, `on_interim_message` | Streaming/interim observations. They are not session teardown events. |

The table omits unrelated gateway-platform and board-automation events. `on_skill_lifecycle` exists, but its payload and emission coverage remain outside this reference's checked subset.

The [turn finalizer][finalizer] includes interrupted and failed outcomes in `on_session_end`. A [CLI shutdown helper][shutdown] can emit it with fewer fields for interrupted runs. Consumers must correlate repeated observations of one turn. RimZ's [lifecycle model](../../internals/agents/model.md) defines its own turn and session boundaries separately.

`api_request_id` is opaque. The retry loop can reuse it, so retain retry metadata when identifying attempts. See [request orchestration][retry-loop]. Parent/child payloads can include `parent_session_id`, `parent_turn_id`, `child_session_id`, and `child_subagent_id`. Approval `session_key` and native `session_id` are separate fields. See [observer payload contracts][observer-doc].

### Callback effects and failures

The synchronous dispatcher bounds selected hooks through `plugins.hook_callback_timeout`, which defaults to 30 seconds. Its bounded set includes turn, API, tool-policy, and selected observation events. Human-input, approval, finalize/reset, and subagent-start callbacks are outside that set. `subagent_stop` runs on the caller thread. See [timeout configuration][callback-timeout] and [the dispatch policy][dispatch].

| Surface | Return or failure behavior |
| --- | --- |
| Approval and human-input observers | Return values do not answer, approve, or veto the native prompt. |
| `pre_tool_call` | Supports `block`, `modify`, and `approve` directives. `approve` requests human approval, not automatic permission. |
| `pre_tool_call` policy resolution | A block takes precedence over escalation. Modified argument fields are shallow-merged. |
| `pre_verify` | `{"action":"continue","message":"..."}` or `{"decision":"block","reason":"..."}` requests more work, subject to stop-gate limits. |
| `pre_llm_call` | A plugin can return a string or `{"context":"text"}`. Shell stdout uses the JSON object. Context enters the user message. |
| Bounded plugin callbacks | A timed-out, still-running, or raising `pre_tool_call` callback fails closed. Other bounded hooks fail open. |

See [plugin dispatch][dispatch], [the public invocation contract][plugins], and [shell reply parsing][shell]. Callback isolation does not establish durable delivery, retries, or delivery after a process dies.

The [context collector][hook-context] skips `pre_llm_call` when `_persist_disabled` is set. It can spill oversized context to disk before injection. A returned context object does not make an observation event a prompt modifier.

On bounded events, the default 30-second callback deadline can expire before a shell hook's separate 60-second deadline.

### Shell hook wire

[Shell hook serialization][shell] sends one JSON object on stdin. The callback executes the parsed command directly with `shell=False`, piped stdin/stdout/stderr, and a timeout. Shell operators require an explicitly invoked shell.

The child environment comes from `build_subprocess_env`. That path selects the active profile's `HERMES_HOME` and requests secret scrubbing during multiplexed operation. Environment stamps for an integration still need runtime verification. See [the spawn path][shell].

| Field | Contents |
| --- | --- |
| `hook_event_name` | Event name. |
| `tool_name` | Native tool name, or null. |
| `tool_input` | The `args` object, or null. |
| `session_id` | Emitter `session_id`, otherwise `parent_session_id`, otherwise an empty string. |
| `cwd` | Current working directory at serialization. |
| `profile` | Active Hermes profile at firing time. |
| `extra` | Remaining emitter fields, including correlations, outcomes, and possibly `session_key`. |

Successful exit with empty stdout contributes no directive, including with `fail_closed: true`. Optional stdout JSON can carry directives or context. Diagnostics belong on stderr. See `_evaluate_result` and `_parse_response` in [shell hooks][shell].

Only `pre_tool_call` belongs to `_BLOCKING_EVENTS` at this pin. `fail_closed` does not turn an observation event into a veto. A shell hook's timeout defaults to 60 seconds and is capped at 300.

| Result | Effect |
| --- | --- |
| Spawn failure or timeout | Fail open by default. With `fail_closed`, block only a blocking-capable event. |
| Exit 2 on `pre_tool_call` | Block. Prefer a JSON block message, then stderr, then the default reason. |
| Other nonzero exit | Log the failure and parse stdout. A fail-closed blocking hook blocks without a directive. |
| Invalid nonempty stdout | No directive by default. A fail-closed blocking hook blocks malformed non-object output. |
| Timeout cleanup | Hermes kills the hook's process tree. |

The shell parser also accepts a `pre_verify` continuation response. That requests more work, while the blocking table above governs tool execution. See [reply parsing][shell].

Consent is per `(event, command)`, recorded in `$HERMES_HOME/shell-hooks-allowlist.json`. Editing a script's contents does not change that consent key. `--accept-hooks`, `HERMES_ACCEPT_HOOKS=1`, or `hooks_auto_accept: true` can accept unseen commands. Top-level `-z` sets `HERMES_ACCEPT_HOOKS=1` automatically. This reference installs nothing. See [consent handling][shell], [the parser][parser], and [one-shot setup][oneshot-setup].

## Native prompts and permissions

The [human-input helper][human] emits a request immediately before entering a native wait and emits resolution in `finally`. Fields include `kind`, `request_id`, `session_id`, `session_key`, `platform`, and a force-redacted `prompt`. The resolution adds `outcome`, defaulting to `error` when the wait supplies none. The helper does not include the typed password or answer.

Session identity comes from approval context, then `HERMES_SESSION_ID`. It can be empty or null. The shell envelope normalizes that missing identity to an empty string. `request_id` pairs the wait events, independent of native session identity.

That redaction applies to this helper's payload. Other hook families can carry user messages, tool arguments, commands, or results. An integration must treat them as sensitive data.

Source brackets cover [`clarify`][clarify], [sudo][sudo], and [standard dangerous-command approval waits][approval-prompt]. The helper carries no turn or tool ID by default. Approval observers use another correlation shape and do not share the generic human-input UUID. Source-level brackets must be checked before claiming coverage of other panels. A `pre_tool_call` for `clarify` establishes an attempted call, not a rendered question.

| Checked `kind` | Resolution `outcome` |
| --- | --- |
| `clarify` | Callback outcome. Classic CLI returns `submitted`, `timed_out`, or `cancelled`. Callback exceptions produce `error`. |
| `sudo` | `provided`, `skipped`, `timeout`, `cancelled`, or `error`, depending on the callback or terminal fallback. |
| `approval` | The selected choice or wait failure. The standard callback path records the returned choice directly. |

See [clarify][clarify], [sudo][sudo], [approval][approval-prompt], and [classic callback outcomes][modal]. These are checked emitters, not a closed enum for every transport.

With `write_approval` enabled, the [inline memory-write prompt][write-approval] calls the CLI approval callback directly. That path has no generic human-wait bracket. Native UI ownership alone therefore does not establish hook coverage.

Secret-capture, vault, and confirmation panels remain unchecked. The generic helper's existence does not prove those panels emit human-wait events.

The classic CLI renders approval choices through `_approval_callback` and batches questions through `_clarify_callback`. Approval choices can include `once`, `session`, `always`, and `deny`. Policy can remove session or permanent approval. Questions return submitted, timed-out, or cancelled outcomes. The native callback owns presentation and answer handling. See [modal callbacks][modal].

The [pinned security guide][security-doc] defines `approvals.mode` as `smart` (default), `manual`, or `off`, with a 300-second approval timeout. Smart approval can decide without a human prompt. The [observer guide][observer-doc] excludes those automatic decisions from human-wait events. Cron, finite single-query, and unattended modes have separate policies, documented as `deny` by default.

Finite `chat -q` runs use the single-query policy, including `-Q` and chat-subcommand `--oneshot`. Top-level `-z PROMPT` enables approval bypass and accepts unseen configured shell hooks. Interactive `-q` seeding remains an interactive session. See [query dispatch][single-query] and [one-shot setup][oneshot-setup].

`--yolo` and top-level `-z` bypass approvals. `--accept-hooks` accepts hook commands. None is a neutral substitute for ordinary interactive permissions. This reference defines no mapping onto RimZ permission modes.

## Session identity and history

`$HERMES_HOME/state.db` is the SQLite session store. The schema version is 31 at this pin. Sessions carry `parent_session_id`, source, model, working directory, start/end times, and an end reason. The store uses WAL where available. See [schema declarations][schema] and [database opening][state].

| Boundary | Identity behavior |
| --- | --- |
| Fresh conversation | Establish a native `session_id`. Fresh-session hooks run after prompt construction. |
| Resume | Resolve the requested ID or title and load history. Resolution can select a compression descendant. A continued identity need not emit a fresh-session hook. |
| `/new`, `/reset`, or `/clear` after confirmation | Switch to a fresh identity and clear the active history. Classic CLI emits separate finalize/reset observations. |
| Compression, default in-place path | Keep the session ID. Archive summarized rows and publish the replacement active history. |
| Compression, rotation path | Publish a continuation with the old session as `parent_session_id`, then switch the agent to that ID. |
| Per-turn completion | Emit the run outcome. The same identity can accept another turn. |

See [session argument resolution][main], [session initialization][conversation], [in-place compression][in-place], and [rotation publication][compression]. A history reader must distinguish compression continuations from branches and delegated children. `parent_session_id` alone does not identify compression.

`compression.in_place` defaults to `true`. Setting it to `false` selects the rotating path. See [configuration defaults][compression-default] and [mode selection][compression-mode]. Upstream's [SQLite lineage predicates][lineage-predicates] identify compression children by the parent's `end_reason = 'compression'`. Branch classification separately uses a `_branched_from` marker or legacy `branched` endings. These predicates are inputs for fixture validation, not a complete external-reader query.

The [classic resume path][resume-chain] can follow an empty compression-chain head to the descendant holding its messages. Ordinary CLI resume refuses Kanban worker transcripts and selects a fresh ID instead. The active ID can therefore differ from the requested ID. See [the refusal path][resume-refusal].

The classic CLI's [`/new` and `/clear` handlers][session-controls] call `new_session` after destructive-action confirmation. `/reset` aliases `/new` in [the command registry][reset-alias]. Ordinary native IDs use `YYYYMMDD_HHMMSS_<six random hex digits>`. Other surfaces can use other forms. Treat the ID as opaque. See [ID construction][session-ids].

When an agent exists, [classic session reset][classic-reset] emits `on_session_finalize` with the old `session_id`, then `on_session_reset` with the new `session_id`. Neither payload carries the pair. [Gateway reset][gateway-reset] additionally supplies `old_session_id` and `new_session_id`. Event pairing needs transport-specific handling and recorded validation.

Both classic payloads include `platform` and `reason`. Finalize uses `reason: "session_boundary"`, and reset uses `reason: "new_session"`.

| Manual compression command | Effect |
| --- | --- |
| `/compress [focus text]` | Request compression, optionally naming what the summary should preserve. `/compact` is an alias. |
| `/compress here [N]` | Request a verbatim tail of `N` exchanges. The default is two. A degenerate split falls back to full compression. |
| `/compress --keep N` | Alternate partial form. Also accepts `-k N`, `--keep=N`, and `up to here [N]`. |
| `/compress --preview` | Preview without changing history. `--dry-run` is an alias. |

Manual compression remains available when automatic compression is disabled. The classic handler does nothing with fewer than four history messages. It refuses `--aggressive` execution. See [command names][session-command-names], [argument parsing][partial-compress], [minimum history][compression-manual], and [manual execution][manual-compress].

Numeric tail counts are clamped to 1 through 100. When no head remains to summarize, or no user turns exist, the split keeps no verbatim tail. Execution then compresses the full history. See [split selection][partial-split] and [manual fallback][compression-fallback].

The [plugin catalog][plugins] has no named compaction hook. The inspected [manual handler][manual-compress] and [rotation publisher][compression] emit no finalize/reset plugin event. A later payload's changed session ID is separate evidence from a lifecycle notification.

Compression marks named `compaction` and `compaction.attempt` use Hermes's Relay telemetry API. Emission requires an instrumented session or turn, called a Relay scope. These marks do not establish an ordinary plugin or shell-hook subscription. See [mark selection][compaction-events] and [Relay emission][relay-compaction]. Compression engine and memory-provider callbacks are also separate interfaces.

The separate [gateway hook catalog][hooks-doc] documents `session:compress`. The [compression callback][gateway-compaction] carries `session_id`, `old_session_id`, `in_place`, and `compression_count`. For in-place compression, `old_session_id` is empty. Gateway-hook availability does not establish a classic CLI plugin or shell-hook event.

Auxiliary calls tagged for compression observe model attempts, which can fail or leave history unchanged. They do not establish a committed compaction. A derivation from those calls needs a separate completion check. See [auxiliary hook contracts][observer-doc] and [committed-mark selection][compaction-events].

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

The inspected [persistence path][persistence] appends to SQLite. `/save json` explicitly exports one session through `export_session`. The separate `export_session_lineage` API can include ancestor segments. This reference establishes no automatic per-session JSON transcript feed. See [CLI export][cli-export] and [export APIs][export].

## Context, usage, and cost

Request usage, session totals, and context occupancy are different measurements.

| Measurement | Source and limits |
| --- | --- |
| Request tokens | `post_api_request` receives normalized usage, when the response supplies it. The helper adds `prompt_tokens` and `total_tokens`. |
| Session totals | SQLite counters accumulate input, output, cache-read, cache-write, and reasoning tokens, plus API-call counts. |
| Context occupancy | The classic status bar uses compressor usage and can anchor to the turn's first response plus an estimate for appended messages. |
| Context capacity | `post_api_request` carries `context_length` from the compressor, when available. It is separate from session totals. |
| Cost | SQLite distinguishes estimated and actual dollars and records billing backend and cost provenance. Token availability alone does not establish a bill. |

See [API usage normalization][api-hooks], [usage schema][schema], [the status-bar snapshot][status-bar], and [pricing semantics][pricing]. The status bar clamps occupancy to the context window and marks estimated display sources. Its display anchoring does not replace the compressor's real last-request usage for compression decisions.

The ordinary `post_api_request` payload carries both `usage` and `context_length`. Shell hooks receive them as `extra.usage` and `extra.context_length`. Request `usage.prompt_tokens` measures that request's input occupancy, not cumulative session tokens or the status bar's anchored display. Missing usage or capacity leaves the gauge incomplete. See [the emitter][post-api] and [shell serialization][shell].

The usage keys are `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `reasoning_tokens`, `request_count`, `prompt_tokens`, and `total_tokens`. The same event includes `response` and `assistant_message`, which can contain sensitive text and tool calls. Consumers need an explicit payload-size and redaction policy. See [usage projection][api-hooks] and [the response payload][post-api].

Canonical `prompt_tokens` adds uncached input, cache-read, and cache-write tokens. `total_tokens` adds output to that prompt total. Do not add reasoning again. Child cost can be rolled into the parent's in-memory estimate, so summing parent and children can double count. See [canonical usage][pricing] and [child cost rollup][child-results].

`cost_status` is `actual`, `estimated`, `included`, or `unknown`. Preserve that status when reporting dollars. Subscription-included and unknown usage do not establish a zero-cost account. See [cost types][pricing].

An inference backend can change during a session. Hermes itself is the agent, while `--provider` selects an inference backend. Model and billing dimensions must accompany usage. This page establishes no universal provider quota or subscription-balance endpoint.

## Home, profiles, authentication, and skills

The [home resolver][home] first uses the home selected for the current request, then `HERMES_HOME`, then the platform default. On ordinary Linux installs, that default is `~/.hermes`. A shared gateway can select a different profile for each request. Resolve that active profile before selecting its store or hooks.

Named profiles use `<root>/profiles/<name>`. The default profile uses the root home. See [profile paths][profiles].

| Surface | Location or control |
| --- | --- |
| Configuration | `$HERMES_HOME/config.yaml` |
| Session database | `$HERMES_HOME/state.db` |
| User plugins | `$HERMES_HOME/plugins/` and `plugins.enabled` |
| Shell-hook consent | `$HERMES_HOME/shell-hooks-allowlist.json` |
| Skills | `$HERMES_HOME/skills/`, plus configured discovery sources |
| Provider auth store | `$HERMES_HOME/auth.json` |
| Environment secrets | Environment, profile dotenv, explicit project dotenv, managed dotenv, and external secret sources. |

The [auth resolver][auth] also has a global-root auth-store fallback when the active profile uses another home. Inference: selecting a separate home alone does not prove credential isolation. Provider authentication can use API keys or OAuth, and the source supports credential pools. No credentials or account records were read to validate this reference.

At startup, the profile `.env` overrides shell exports. An explicitly supplied project dotenv fills gaps when the profile dotenv exists. Without a profile dotenv, the project dotenv can override shell values. Managed dotenv applies last with override. When a shared gateway handles another profile's turn, Hermes uses scoped secrets instead of changing the process environment. See [environment loading][env].

`--skills` preloads named skills. The [skill configuration][skills-config] separately supports global and per-platform disabled sets, with essential skills excluded from disabling. These controls do not establish an allow-only view of every skill source.

## Native subagents

`delegate_task` creates child agents with explicit parent/child session and subagent identities. The [creation path][delegate] emits `subagent_start` after attaching the child, before execution. A child can still queue for execution after that event. The [observer guide][observer-doc] documents `subagent_stop` with status, summary, duration, and metadata-only tool history.

Child toolsets, model/provider selection, execution limits, and approval policy are separate delegation controls. Native child work is distinct from a separately launched terminal process. The [delegation guide][delegation-doc] documents monitoring, steering, and stop controls. Stop timing, parallel result order, and child approval routing need live validation before they can support attention-state claims.

`delegate_task` accepts `action: spawn`, `list`, `steer`, or `stop`. Control actions run synchronously before spawn pause/depth gates. A stop requests interruption, and steering queues a message. These actions require parent ownership context. See [tool dispatch][delegate-actions] and [ownership checks][delegate-registry].

The [stop emitter][child-results] runs during result processing, not at the child's last model token. It omits the start event's subagent IDs. Child approval callbacks default to automatic denial, with `delegation.subagent_auto_approve` selecting one-time approval. Child construction supplies no clarify callback. A child approval observation therefore does not establish a root-terminal wait. See [child configuration][child-config] and [construction][delegate].

## Verification gaps

| Required evidence | What remains unproved |
| --- | --- |
| Redacted recorded sessions at this pin | Hook order and outcomes for success, interruption, cancellation, errors, and resume. No user records were inspected. |
| Native prompt captures | Rendering order, answer selection, and coverage of every approval, secret, vault, and confirmation panel. |
| Compression and reset fixtures | Detection without a named compaction hook, continuation selection, and history projection across resets and rewinds. |
| Process and isolation probes | Runtime ownership and environment stamps for classic CLI, TUI, and hosted plugins. Static paths establish intended ownership only. |
| Backend-specific runs | API hook parity and usage/cost completeness across inference transports, retries, auxiliary calls, and external runtimes. |
| Read-only history validation | Correct SQLite queries during concurrent writes, migration, pruning, compression, and recovery. |
| Delivery and crash tests | No verified hook replay, durable delivery, or process-crash completion guarantee. |
| Transport captures | Full ACP, gateway RPC, and HTTP/SSE schemas and parity. Their catalog above is documentary. |
| Outbound webhook traces | Payload schemas, signing, retries, and delivery behavior of `hooks.outbound`. Only its documentary catalog was read. |
| Home and skill isolation tests | Runtime credential precedence across routed profiles and a complete allow-only skill restriction. |
| File-edit approval traces | Whether `write_file` or `patch` prompts under each interactive policy. Command approval alone does not establish file-edit gating. |
| Other attention surfaces | Background-process parking, idle notifications, remote control, and terminal answer keystrokes were not traced in this pass. |
| Other session controls | `/branch`/`/fork`, `/undo`, `/handoff`, and in-session `/resume` transitions. CLI adoption timing after automatic rotation also remains untraced. |

These gaps constrain later claims. They do not decide whether the eventual integration should use a built-in adapter or a process plugin. The next artifact is the [mapping worksheet](../../contributing/agent-adapters.md#step-2--map-the-protocol-onto-the-model), grounded in the pinned contract and recorded-session evidence.

[parser]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/_parser.py#L144-L325

[main]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/main.py#L1325-L1815

[cli]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/cli.py#L847-L869

[worker]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_chat_turn_mixin.py#L313-L367

[single-query]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_single_query.py#L489-L526

[stream-json]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/stream_json.py

[plugins]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/plugins.py

[discovery]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/plugins_discovery.py

[dispatch]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/plugins_dispatch.py#L40-L357

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

[profiles]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/profiles.py#L282-L444

[delegate-actions]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/delegate_tool.py#L483-L527

[delegate-registry]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/delegate_tool_registry.py#L179-L309

[callback-timeout]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/plugins.py#L1125-L1150

[plan-handler]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_commands_mixin.py#L1607-L1630
[plan-rules]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/plan_prompt.py#L11-L71
[system-overlay]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_init_mixin.py#L249-L255
[system-assembly]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/turn_context.py#L1341-L1348
[tool-classes]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/tool_guardrails.py#L28-L32
[file-classes]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/tool_result_classification.py#L9-L23
[file-tools]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/file_tools.py#L1173-L1283
[message-tool]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/send_message_tool.py#L24-L26
[tool-assembly]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/model_tools.py#L316-L338
[session-controls]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_loops_mixin.py#L93-L203
[session-ids]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_state_ids.py#L50-L60
[session-command-names]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/commands.py#L106-L117
[partial-compress]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/partial_compress.py#L21-L47
[manual-compress]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_session_mixin.py#L885-L961
[post-api]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/turn_response_intake.py#L66-L98

[shutdown]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_shutdown.py#L185-L203
[oneshot-setup]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/oneshot.py#L270-L280
[cli-export]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/session_export.py#L222-L240

[hook-context]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/turn_context.py#L797-L853
[write-approval]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/tools/write_approval.py#L206-L248
[classic-reset]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_session_mixin.py#L415-L575
[gateway-reset]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/gateway/slash_commands_session.py#L210-L218
[compression-manual]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/conversation_compression_manual.py#L14-L20
[skill-preload]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/cli.py#L1559-L1583
[partial-split]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/partial_compress.py#L117-L158
[compression-fallback]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/conversation_compression_manual.py#L95-L105
[hooks-doc]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/website/docs/user-guide/features/hooks.md#L7-L104
[compression-default]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/config_defaults.py#L679-L686
[compression-mode]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/conversation_compression.py#L4149-L4154
[lineage-predicates]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_state_common.py#L167-L176
[resume-chain]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_agent_setup_mixin.py#L524-L533
[resume-refusal]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/cli_agent_setup_mixin.py#L766-L780
[reset-alias]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/hermes_cli/commands.py#L84-L92
[gateway-compaction]: https://github.com/NousResearch/hermes-agent/blob/d923a5eb487a17c0a68801bba9ef0f56c81040a8/agent/conversation_compression.py#L3457-L3469

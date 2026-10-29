# Antigravity adapter

This page maps the Antigravity CLI (`agy`) onto RimZ's types: launch and resume, the hooks RimZ installs, session discovery from the provider's local store, the transcript, subagents, statusline context, the account, and cost. The provider-neutral agent model is in [model.md](./model.md), the integration layer in [adapter.md](./adapter.md), and pane binding and supersession in [instances.md](./instances.md). Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The upstream protocol, pinned at `agy` 1.3.3 with transcript and subagent captures from 1.1.2, is in [antigravity-reference.md](../../externals/agent-adapter/antigravity-reference.md).

Antigravity reaches RimZ through four sources, and each owns different facts. Command hooks carry turn boundaries and tool progress. The custom statusline carries the live model, account, and context gauge. The provider's local `brain/` store carries conversation identity, history, and native questions between hooks. The private loopback service of a running `agy` carries the plan and quota. RimZ installs no `PreToolUse` hook, because no documented `PreToolUse` decision is proven to leave native permission policy unchanged ([reference](../../externals/agent-adapter/antigravity-reference.md#decisions)).

## Files

| File | Owns |
| --- | --- |
| `crates/rimz/src/agents/adapters/antigravity/mod.rs` | The `static AgentSpec` (`ANTIGRAVITY_DESCRIPTOR`), the declared coverage, the hook catalog (`ANTIGRAVITY_HOOKS`), hook replies and decoding (`decode_lifecycle_fields`), resume argv, and the capability impls |
| `crates/rimz/src/agents/adapters/antigravity/payloads.rs` | Typed hook payloads and the failure rules for tool and `Stop` payloads |
| `crates/rimz/src/agents/adapters/antigravity/install.rs` | The two-file install, preview, and uninstall |
| `crates/rimz/src/agents/adapters/antigravity/session.rs` | Provider home resolution, transcript path validation, local-session discovery, the transcript fold, final-message extraction, and the parent-transcript subagent join |
| `crates/rimz/src/agents/adapters/antigravity/statusline.rs` | The statusline payload, its `AgentContext` projection, and current-usage pricing |
| `crates/rimz/src/agents/adapters/antigravity/local_api.rs` | The loopback service client and the two account probes |
| `crates/rimz/src/agents/adapters/antigravity/local_api/process.rs` | Exact-process and owned-socket discovery for `agy` |
| `crates/rimz/src/agents/adapters/antigravity/local_api/wire.rs` | The service's JSON schemas, the hashed account key, and the quota fold |

## Launch and resume

The launch spec maps RimZ's launch inputs onto `agy` flags:

| Input | Argv |
| --- | --- |
| fresh launch | `agy` |
| startup prompt | `agy --prompt-interactive <prompt>` |
| profile `model` | `--model <model>` |
| profile `effort` | `--effort <effort>` |
| permission `ask` | none (the native default) |
| permission `auto` | `--mode accept-edits` |
| permission `plan` | `--mode plan` |
| permission `yolo` | `--dangerously-skip-permissions --mode accept-edits` |
| exact resume | `agy --conversation <id>` |

The 1.3.3 CLI accepts `low`, `medium`, `high`, `xhigh`, and `max` for `--effort` ([reference](../../externals/agent-adapter/antigravity-reference.md#flags)). The adapter declares no version floor, so an older `agy` that lacks the flag rejects a profile that sets effort at its own startup. Under `yolo`, implementation-plan approval still follows the user's `artifactReviewPolicy`, which RimZ leaves untouched ([reference](../../externals/agent-adapter/antigravity-reference.md#execution-modes)).

Resume takes a conversation id only when `session.rs::valid_conversation_id` accepts it: nonblank after trimming, at most 256 bytes, not `.` or `..`, no leading `-`, and no control character, `/`, or `\`. The same rule gates every path built from an id. On a live pane, `session.rs::resumed_session_id` reads the first `--conversation <id>` or `--conversation=<id>` after a program whose basename is `agy`. `-c` and `--continue` resume the workspace's latest conversation, so they prove no exact identity and bind nothing. The process matcher (`process_names`, `bin_names`) recognizes only `agy`, so the desktop app's `antigravity` processes stay outside this adapter.

Some launch inputs have no Antigravity channel:

| Input | Behaviour |
| --- | --- |
| profile system prompt | refused before launch: `presets.system_prompt_file` stays `None`, since upstream publishes no system-prompt flag |
| launch reminders | no append-system-text channel, so only the child no-delegation body arrives, through the user-prompt fallback ([fleet.md](../harness/fleet.md#launch-reminders)) |
| `rimz agents fork` | unsupported: native `/fork` switches identity inside the TUI and has no source-id launch flag ([reference](../../externals/agent-adapter/antigravity-reference.md#fork-rewind-and-clear)) |
| headless judge | refused: the spec has no `HeadlessForm`, so a workflow judge profile on Antigravity fails validation |
| scoped profile `skills` list | refused: the manual-skill marker is `Unsupported` ([sandbox.md](../sandbox.md#skill-roots-and-user-only-markers)) |
| `sandbox` isolation | warns and launches without a provider sandbox: `has_native_sandbox` keeps its `false` default |

## Hooks

### Install

`rimz hooks install antigravity` edits two files, and the preview shows both diffs:

| File | Override | Change |
| --- | --- | --- |
| `~/.gemini/config/hooks.json` | `RIMZ_ANTIGRAVITY_HOOKS` | one top-level hook named `rimz` holding the six handlers below |
| `~/.gemini/antigravity-cli/settings.json` | `RIMZ_ANTIGRAVITY_SETTINGS` | `statusLine` wrapped by `rimz statusline feed --source antigravity` |

Both paths resolve from the process `HOME` through `crates/rimz/src/agents/locate.rs::agent_config_path`, and the installer ignores the login env map. Install builds both candidates before it writes anything, then writes the settings file first and the hooks file second (`crates/rimz/src/agents/settings_json.rs::commit_pair`). A failed second write restores both files to their original bytes.

Ownership is by command marker. Before it rebuilds the `rimz` block, `install.rs::strip_owned_hooks` removes every handler whose command contains `rimz hooks feed --source antigravity`, under any hook name. A `rimz` key that survives the strip is user-owned, and install refuses with a request to rename it. Every handler is `{"type":"command","command":...,"timeout":5}` and starts with `RIMZ_AGENT_PID=$PPID exec`, so the hook carries its owner pid ([instances.md](./instances.md#session-death)). Uninstall removes only marked handlers and drops a hook name or event left empty.

The statusline wrap follows `crates/rimz/src/agents/managed_statusline.rs::upsert` with the `ObjectOnly` policy. A `statusLine` that exists and is not a JSON object refuses install. The prior object is saved under `_rimz_wrapped` and restored whole on uninstall. While RimZ is installed, the active entry keeps only one of the user's rendering options, `stack_with_default`, so other options such as `padding` take effect again only after uninstall. When `statusLine` was absent, install sets `stack_with_default: true` so Antigravity keeps its built-in line. An existing object keeps its own value or its absence.

### Events

The catalog holds four native events, with `PostToolUse` split by three disjoint matchers. The matchers name the documented tools ([reference](../../externals/agent-adapter/antigravity-reference.md#tool-vocabulary)), so a tool outside all three sends no `PostToolUse` at all.

| RimZ event | Native event and matcher | Signal | Reply |
| --- | --- | --- | --- |
| `PreInvocation` | `PreInvocation` | `TurnStarted` when `invocationNum` is `0`, otherwise routing only | `{}` |
| `PostToolUse:edit` | `PostToolUse`, `write_to_file`, `replace_file_content`, `multi_replace_file_content` | `ToolUsed`, mutating and editing | `{}` |
| `PostToolUse:mutating` | `PostToolUse`, `run_command` | `ToolUsed`, mutating | `{}` |
| `PostToolUse:observed` | `PostToolUse`, the remaining known tools (`view_file` through `browser_.*`) | `ToolUsed` | `{}` |
| `PostInvocation` | `PostInvocation` | routing only | `{}` |
| `Stop` | `Stop` | `TurnEnded` | `{"decision":""}` |

Hook stdout is Antigravity's decision channel, and every wired event must answer with JSON. `{}` is the documented no-op, and `{"decision":""}` is a `Stop` decision other than `continue`, so the stop proceeds ([reference](../../externals/agent-adapter/antigravity-reference.md#events)). Any other event name, such as a hand-fed `PreToolUse` or `SessionStart`, gets no output and records nothing. For every Antigravity event the feed waits up to 1.2 s for the drainer to apply the frame and prints the drainer's reply ([adapter.md](./adapter.md#the-feed)). That early return also skips the deadline rung a `PostToolUse` could otherwise carry.

Every payload carries the conversation id (`conversationId`, or `conversation_id`), `workspacePaths`, `transcriptPath`, and `modelName`. A payload without a nonblank id records nothing. The first nonblank workspace path becomes the worktree path, and `modelName` becomes the launch model hint. Three decoding rules in `mod.rs::decode_lifecycle_fields` are easy to break:

- Only `invocationNum: 0` opens a turn. Later invocations of the same execution follow tool calls and must not reopen it, and a payload without `invocationNum` opens nothing.
- A failed tool (a nonempty `error`) keeps its `ToolUsed` progress and tool name but clears the mutating and editing flags. The tool name is the trimmed `toolCall.name`. A release that omits `toolCall` sends unnamed progress.
- `Stop` needs `fullyIdle`, which upstream marks required. A `Stop` without it records no lifecycle event and no final message. `terminationReason` `error` or `max_steps_exceeded`, or a nonempty `error`, ends the turn errored. A clean `Stop` with `fullyIdle: false` parks the turn on background work ([model.md](./model.md#turn-endings-and-parked-turns)).

A supervised run (`rimz agents antigravity -p`) completes from the `Stop` hook. Its final message is the newest completed assistant text in the hook's transcript ([Transcript](#transcript)).

## Sessions

Antigravity registers lazily (`registers_lazily`). A live `agy` pane renders as the idle row of [instances.md](./instances.md#before-a-session-binds) until a hook or local discovery binds a conversation. Upstream documents `SessionStart`, but RimZ leaves it unwired until its behaviour on resume, `/clear`, and child conversations is captured ([reference](../../externals/agent-adapter/antigravity-reference.md#events)). The first `PreInvocation` of an unseen conversation creates it.

One live `agy` follows its latest conversation, so the spec declares `SamePaneSessionPolicy::FollowLatest`. Pane adoption, ownership, and the in-place-switch supersession follow from that policy ([instances.md](./instances.md#binding-a-session), [same-pane ownership](./instances.md#same-pane-ownership), [supersession](./instances.md#supersession)). RimZ records no `Fresh` origin for a switch, because the hook wire does not say whether `/clear` or `/fork` created the new conversation.

### Local discovery

Antigravity declares `local_session_discovery = FullScan`. `session.rs::discover` reads the provider home: `RIMZ_ANTIGRAVITY_HOME` when set, else `$HOME/.gemini/antigravity-cli`, both from the process environment. `discover_local_sessions` ignores the login env map. One pass serves every admitted workspace:

1. Catalog. `enumerate_conversations` lists every directory under `brain/` whose name passes `valid_conversation_id`. The shared `IncrementalCatalog` rescans when the stamped topology changes or after the 30-second backstop (`crates/rimz/src/agents/local_session_cache.rs::LOCAL_SESSION_DISCOVERY_BACKSTOP`).
2. Selection. A conversation counts when `.system_generated/logs/transcript_full.jsonl`, or else `transcript.jsonl`, is a regular file. The 512 newest by that file's mtime are kept (`MAX_DISCOVERED_SESSIONS`).
3. Fold. Each selected conversation is validated and folded from the shared 64 KiB record-aligned tail. The fold is cached against five stamped paths: the conversation, `.system_generated`, and `logs` directories and both transcript files. A transcript change or a fallback promotion refolds that conversation alone. A tail with no visible record yields no observation, and the first visible record in the tail stands in for the creation time.
4. Index. `cache/last_conversations.json` maps a workspace path to its current conversation id. It is parsed once per pass and cached on its own stamp, so an index-only change rematerializes without a rescan or a refold.
5. Materialize. Every folded conversation becomes one `Lifecycle` observation for each admitted workspace. Only the conversation the index names for that exact workspace path gets `fresh_binding_at`.

The store is provider-global, so an observation's workspace proves nothing about where the conversation ran. A conversation without `fresh_binding_at` binds only through an exact durable row or a `--conversation` command line. The fresh fallback is reserved for the index's pairing ([instances.md](./instances.md#local-session-observations) owns the binding order). Because hooks bind first, a lagging index cannot pull a native question away from a conversation the hooks already identified.

A transcript path is accepted only through `session.rs::valid_transcript_under`. `brain/` and `brain/<id>` must be real directories, not symlinks, and `brain/<id>` must canonicalize directly under `brain/`. The path itself must be a regular, non-symlink file that canonicalizes to `brain/<id>/.system_generated/logs/` plus one of the two basenames. Discovery, `session_transcript`, the first-prompt read, and the subagent join all check this. The `Stop` final-message read does not ([Known gaps](#known-gaps)).

## Transcript

The 1.1.2 fixtures define the record shape: JSONL with `step_index`, `source`, `type`, `status`, `created_at`, and optional `content` and `tool_calls` ([reference](../../externals/agent-adapter/antigravity-reference.md#transcripts)). A line missing a required field is skipped. `session.rs::fold` reads records in physical order, and only these records move the projection:

| Record | Fold effect |
| --- | --- |
| `USER_EXPLICIT` / `USER_INPUT`, `DONE` | `running`, phase `reasoning`, prompt set, native question cleared. This is the turn start |
| `MODEL` / `PLANNER_RESPONSE`, `DONE`, with an `ask_question` call holding a nonblank question | `waiting`, phase idle, the first nonblank question as the native prompt detail, `waiting_since` from `created_at` |
| `MODEL` / `PLANNER_RESPONSE`, `DONE`, otherwise | `success`, phase idle, native question cleared |
| `MODEL` / `PLANNER_RESPONSE`, `ERROR` | `failed`, phase idle, native question cleared. The content is never shown |
| anything else (`SYSTEM` history and checkpoints, other statuses, unknown sources or types, unparseable `created_at`) | ignored |

The `ERROR` row is an assumption: no failure capture has shown which source and type carry a terminal model error. A later turn start or completed planner response supersedes it. User content wrapped in `<USER_REQUEST>` contributes only the text up to the matching `</USER_REQUEST>`, so the trailing metadata and settings blocks stay private. An unclosed or empty wrapper yields no prompt. Unwrapped text passes through as it is. Both forms then pass through `sanitize_user_prompt`.

`ask_question.args.questions` is an array in `transcript_full.jsonl`, and `transcript.jsonl` may carry it as a JSON-encoded string. The parser accepts either form in either file. The native question stays pane-only: it creates no durable RimZ ask, and an accepted fold clears any older routable ask ([model.md](./model.md#waiting-and-asks)). A fold overlays a durable row only when its activity is at least as recent, compared in whole seconds (`crates/rimz/src/store/snapshot/view/live.rs::local_observation_is_current`). A fold written in the same second as a newer hook can therefore replace that hook's state until the next hook ([instances.md](./instances.md#local-session-observations)).

Four readers use the transcript:

| Reader | Path source | Read | Rule |
| --- | --- | --- | --- |
| history (`session.rs::messages`) | `session_transcript`, validated | the lines the shared transcript reader supplies | `DONE` visible records with content, user text normalized as above |
| first prompt (`session.rs::latest_prompt`) | hook `transcriptPath`, validated | 64 KiB tail | on `TurnStarted` only. The latest completed visible user prompt. Other hooks read no prompt |
| final message (`session.rs::last_assistant_message`) | hook `transcriptPath`, not validated | 64 KiB tail, then whole file | on every decoded `Stop`. The newest `DONE` planner text after the last turn start. When the tail holds neither a turn start nor an answer, it reads the whole file without a bound, and a file without a turn start counts as one turn |
| subagent join | parent row's path, or rebuilt from the parent id | 64 KiB tail | [Subagents](#subagents) |

A turn with no answer returns no final text, never the previous turn's reply. A torn final record stays out of every tail until it completes. Once a hook records the prompt, a later discovery tail with no visible user record supplies no newer prompt, so the card keeps its launch description or hook prompt through a long turn.

## Subagents

Antigravity fires the ordinary hook set for each asynchronous or nested child, with the child's own `conversationId` and transcript and no parent id ([reference](../../externals/agent-adapter/antigravity-reference.md#subagents-and-background-tasks)). RimZ recovers the parent from the parent's transcript, where a completed `MODEL` / `PLANNER_RESPONSE` record holds an `invoke_subagent` call with ordered `Subagents` specs, and a later completed `MODEL` / `INVOKE_SUBAGENT` record's `content` returns one `{conversationId, logAbsoluteUri, workspaceUris?}` object per spec in the same order. This shape is live-verified, not documented upstream.

### The forward join

`crates/rimz/src/cli/hooks/lifecycle/observe.rs::correlate_subagent_observation` runs on a root `TurnStarted`, `TurnEnded`, or `ToolUsed`:

1. Known child. A prior row of this id with a parent supplies the relation, with no transcript read.
2. Trigger. Otherwise only a `TurnStarted` or an unparked `TurnEnded` tries a join, and the hook needs a pane and a worktree path.
3. Candidates. The candidates are the other Antigravity rows stamped on the same pane, newest first. Above 64 (`MAX_SUBAGENT_PARENT_CANDIDATES`) the join is skipped.
4. Match. `AntigravityAdapter::correlate_subagent` tests each candidate against its validated transcript. Exactly one candidate must match, and a match that resolves to the child itself is discarded.
5. Flatten. A candidate that is itself a child stands in for its root parent, so nested children attach to the root card.

A match sets the child's name from `TypeName`, its role from `Role`, its task from `Role` or else `Prompt`, and its prompt from `Prompt`. It also clears the hook's model hint, because the correlation carries no model. Later child hooks reuse the stored relation and keep their `modelName`. Child observations take no root launch environment or configured identity.

| Child signal | Becomes |
| --- | --- |
| `TurnStarted` | `SubagentStarted` |
| `TurnEnded`, unparked | `SubagentStopped { errored }` |
| `TurnEnded`, parked on background | unchanged, until the terminal `Stop` |
| `ToolUsed` | unchanged, keyed to the child |

### Parent validation

`session.rs::validated_subagents_under` reads the parent's 64 KiB tail. A request whose result falls inside the tail but whose `invoke_subagent` record lies before it leaves a result with no request, which voids the record. Some failures void every child of the parent transcript, and others drop one child:

| Scope | Failure |
| --- | --- |
| whole record | the parent path fails [path validation](#local-discovery), the parent workspace does not canonicalize, or the tail ends in a torn record |
| whole record | any line fails to parse as a transcript record |
| whole record | an `invoke_subagent` call does not parse, has no specs, or has a spec with both `Prompt` and `Role` blank or a blank `Workspace` |
| whole record | an `INVOKE_SUBAGENT` record is not `DONE`, has no pending request, or holds fewer objects than specs or a further object after them |
| whole record | a valid child id repeats, even when either copy would be dropped later |
| whole record | a request is still unmatched at the end of the tail |
| one child | the returned id fails `valid_conversation_id` |
| one child | `logAbsoluteUri` is not a `file:` URI, or not a valid transcript of that child id |
| one child | the spec's `Workspace` is relative (such as `branch` or `share`) or does not canonicalize. An absent `Workspace` or the literal `inherit` means the parent's workspace |
| one child | `workspaceUris` is present but empty, holds a URI that does not canonicalize, or does not contain the expected workspace |

A child then joins only when its id equals the result's id and its canonical hook workspace equals the expected workspace. A worktree child whose hook workspace differs stays a root.

### The reverse pass

1.1.2 buffers the parent transcript. In a two-child capture, both children's `PreInvocation` and `Stop` hooks ran before the parent's request and result records reached disk, and the file flushed before the parent's own `Stop`. The forward join therefore often fails, and a reverse pass catches up.

On every root `ToolUsed` or `TurnEnded`, `crates/rimz/src/cli/hooks/lifecycle/observe.rs::record_mapped_lifecycle_observation` calls `AntigravityAdapter::spawned_subagents`. It returns every validated child in the parent's tail. The store writer (`crates/rimz/src/store/writer/lifecycle.rs`) then emits a `SubagentStopped` for each child, errored when the child row is `failed`:

- A child with no parent is adopted (`SubagentAdopted`), unless its row is stamped on another pane. Adoption also restores a child that a supersession reap already hid ([instances.md](./instances.md#supersession)).
- A child already under this parent is reconciled (`SubagentReconciled`) while its row is still `running`, or when its model or tokens changed. A settled child with no change is skipped.

The pass treats a returned result as proof that the child finished, and no capture confirms that for an asynchronous child. A child still `running` is closed by its parent's next tool hook. Until the reverse pass runs, the supersession open-turn guard keeps the active parent from being reaped by a provisional child root ([instances.md](./instances.md#supersession)).

## Statusline context

The wrapper runs `RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source antigravity`, which forwards the user's prior command and prints nothing when there is none. `statusline.rs::StatuslinePayload` needs `conversation_id` (or `conversationId`) and maps the rest into `AgentContext`:

| Payload field | `AgentContext` |
| --- | --- |
| `model.id` | `model_id`, unchanged |
| `model.display_name` | `model_display_name`, with a recognized trailing qualifier removed |
| `model.effort`, else the display-name qualifier | `effort`, trimmed and lowercased |
| `(Thinking)` qualifier | `thinking_enabled` |
| `version` | `agent_version` |
| `plan_tier`, `email` | `account` plan and id, `metered: true` |
| `context_window` size, percentages, `current_usage` | `tokens`. `current_context_tokens` stays empty, so the gauge comes from the percentages and window size alone |
| `tool_confirmation_pending: true` | a `NativeWait` settle stamped at observation time |

Antigravity puts the selector label, such as `Gemini 3.5 Flash (Medium)`, in both `model.id` and `model.display_name` ([reference](../../externals/agent-adapter/antigravity-reference.md#payload)). The recognized qualifiers are `(Low)`, `(Medium)`, `(High)`, `(XHigh)`, `(Extra High)`, `(Max)`, and `(Thinking)`, matched without case. `Extra High` maps to `xhigh`. An unknown parenthetical stays in the label. The `(Extra High)` and `(Max)` spellings come from the 1.3.3 effort list and have no label capture yet.

The `NativeWait` settle projects a `running` card to `waiting` while it is newer than the row's activity ([model.md](./model.md#waiting-and-asks)). It creates no ask and sends nothing to Antigravity. Any later hook advances the row's activity past the marker, so a missed `false` refresh clears itself.

RimZ reads no other statusline field. `cost` has no published shape, `subagents` carries no stable ids, `conversation_title` would replace RimZ's launch description on the card, and `quota` stays unread because the local service supplies quota ([Account and balance](#account-and-balance)).

## Account and balance

The plan and quota come from the private Connect-over-HTTPS service of an already-running `agy` ([reference](../../externals/agent-adapter/antigravity-reference.md#private-local-service)). RimZ starts no `agy`, reads no credential or OAuth token, and sends no pane input. The version probe returns `None` instead of running `agy --version` ([providers.md](./providers.md#the-out-of-band-probe)).

`local_api/process.rs::discover` finds the service. It takes processes of the caller's real uid whose executable basename and `argv[0]` basename are both `agy`, newest start first, and keeps the first four. For each it lists listening TCP sockets the process owns on exactly `127.0.0.1` or `::1`, from `/proc/<pid>/fd` and `/proc/<pid>/net/tcp{,6}` on Linux, or from `lsof` (400 ms bound) on macOS. Other platforms find nothing. Before every request `revalidate` checks the uid, both basenames, and the process start token again. The client disables certificate verification, since the service's certificate is self-signed, along with proxies and redirects. It relies on the process and socket checks for trust.

| Limit | Value |
| --- | --- |
| candidate processes | 4 |
| endpoints per process | 12 |
| whole probe | 2 s (`PROBE_TIMEOUT`) |
| one request | 750 ms (`ATTEMPT_TIMEOUT`), connect 300 ms |
| response body | 256 KiB (`MAX_RESPONSE_BYTES`) |

Two probes use the service:

| Probe | Calls | Needs | Returns |
| --- | --- | --- | --- |
| `probe_account` (display) | `GetUserStatus` on each endpoint until one parses | an email or a plan | plan and email, `metered: true`. Any failure is `SourceUnavailable` |
| `probe_account_usage` (direct account usage) | `GetUserStatus`, then `RetrieveUserQuotaSummary` with `{"forceRefresh":true}` on the same endpoint | an email | `Found` with plan and windows, or `Failed` |

The plan is `userStatus.userTier.name`, else the first nonblank of `planDisplayName`, `displayName`, `productName`, `planName`, and `planShortName` under `planStatus.planInfo`. A `code` other than absent, `0`, `"0"`, `"ok"`, or `"success"` rejects the response. The usage probe trims and ASCII-lowercases the email and hashes it as SHA-256 over `rimz:antigravity-account:v1\0` plus the email. It persists only `antigravity:v1:<hex>`, with `KindWide` scope. A failure before any status parse is `Failed` with no owner. A failure after the status read carries that owner, which lets the credits cache drop another owner's windows ([providers.md](./providers.md#the-credits-cache-and-usage-identity)).

### Quota windows

`wire.rs::parse_rate_limits` folds per-model buckets into at most two windows. A bucket's period comes from its `bucketId`, else its `displayName`, lowercased with `-` and `_` read as spaces. A `5h` token or the words `5 hour` or `five hour` mean five hours, and a `weekly` token means a week. Other buckets are ignored.

- Each enabled bucket with a remaining fraction joins its period. A disabled bucket, or one without a fraction, marks the period present and contributes nothing.
- The period's window is the bucket with the smallest remaining fraction. A tie takes the later reset, then the lower bucket identity.
- A present period with no usable bucket publishes a window with unknown fill and reset. An absent period publishes no window, so a weekly-only response yields one window.
- Used percentage is `100` only at a remaining fraction of exactly `0`. Otherwise it is rounded and capped at `99`.
- A usable bucket's fraction outside `0..=1`, or a reset that is missing, unparseable, or not in the future, rejects the whole reading. So does a response with no recognized period.

The windows are `Authoritative` ([providers.md](./providers.md#window-fusion)), and RimZ invents no credits or dollars from them. They never arm auto-continue, because no Antigravity stop class is certified recoverable ([providers.md](./providers.md#auto-continue)).

## Cost

`StatuslinePayload::cost` prices the statusline's `current_usage` as one request. Input, output, cache creation, and cache read are disjoint inputs to `Pricing::cost_of`. The result goes into `AgentCost.total_cost_usd` with `CostCoverage::CurrentUsage`, and only when it is finite and positive. `model.id` resolves through `PriceBook::price`, which tries the exact key and then the fuzzy scan ([spending.md](./spending.md#resolving-a-model)). A selector label rarely matches either, so a label with a recognized qualifier then tries two exact-only keys built from the base name: lowercase words joined by `-` (`gemini-3.5-flash`), then the same with `.` replaced by `-` (`gemini-3-5-flash`). Exact lookup keeps a selector from borrowing a related model's rates. A model without its own key, such as Claude Sonnet 5.5, stays unpriced.

`GPT-OSS 120B` has one alias, `vertex_ai/openai/gpt-oss-120b-maas`. Google's own hosting is the closest public rate, on the unverified assumption that Antigravity serves that model from Google's platform.

The card and `rimz agents show` render this figure as ordinary dollars. `CurrentUsage` coverage keeps it out of live spend, so room and provider aggregates, budgets, provider history, account spend, and `rimz stats` never count it ([spending.md](./spending.md#live-cost-coverage)). The provider headline shows the count of active Antigravity panes and dim placeholders in place of tokens and dollars ([spending.md](./spending.md#what-reads-the-totals)).

## Known gaps

`rimz coverage --wiring` prints the declared concern and lifecycle verdicts. These gaps have a reason worth recording:

- No recoverable failure. No 1.3.3 error class carries a provider-owned typed discriminator captured twice, so every errored `Stop` is terminal. Message text, error-shaped objects, quota exhaustion, and a stalled pane never arm auto-continue, and a supervised run or loop attempt does not survive an Antigravity limit.
- Subagents are partial. Child hooks are documented, but the parent's request and result records are live-verified only. An errored invocation may write an `INVOKE_SUBAGENT` record with status `ERROR` ([reference](../../externals/agent-adapter/antigravity-reference.md#transcripts)). Its shape is uncaptured, and the non-`DONE` status voids every sibling in that record.
- The reverse pass closes children on a returned result, which may arrive before an asynchronous child finishes ([The reverse pass](#the-reverse-pass)).
- The `Stop` final-message read takes the hook's `transcriptPath` without path validation, and its fallback reads the whole file.
- No cumulative ledger. Machine-readable credits, API keys or BYOK, account spend, and daily dollar caps are unsupported, and replace-style current usage cannot become history.
- Permission wait detail, durable asks, artifact-review waits, structured answers, and compaction are unsupported. The user answers native prompts in the Antigravity pane.
- Remote control is unsupported. Upstream offers a daemon (`agy remote-control`) and session-scoped connections (`--remote-control`, `/remote-control`), and RimZ wires neither ([reference](../../externals/agent-adapter/antigravity-reference.md#remote-control-mcp-plugins-and-custom-agents)).
- Tool statistics are partial. Names come only from live `PostToolUse.toolCall`, on releases that send it and for tools the three matchers cover. MCP tools and transcript history are not counted.
- Registration is derived. `SessionStart` stays unwired until a live capture shows its behaviour on resume, `/clear`, and child conversations.

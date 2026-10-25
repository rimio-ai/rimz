# Cursor adapter

This page maps the Cursor CLI (`cursor-agent`) onto RimZ's types: the binary and config directories, the hooks RimZ installs, subagents and waits derived from Cursor's local chat store, launch and resume, context and the transcript, the account, and cost. The provider-neutral agent model is in [model.md](./model.md), the integration layer in [adapter.md](./adapter.md), and pane binding and supersession in [instances.md](./instances.md). Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The upstream protocol, pinned at build `2026.10.01-e373342` with live captures from `2026.07.09-a3815c0`, is in [cursor-reference.md](../../externals/agent-adapter/cursor-reference.md).

Cursor reaches RimZ through five sources, and each owns different facts. Command hooks carry session, turn, tool, compaction-open, and exit boundaries. The command statusline carries the live model and context gauge. The chats store (`chats/` under Cursor's config directory) carries subagents and the two native waits, which have no hooks. The JSONL transcript under `~/.cursor/projects/` carries a missed turn ending. Two CLI subcommands carry the account.

## Files

| File | Owns |
| --- | --- |
| `crates/rimz/src/agents/adapters/cursor/mod.rs` | The `static AgentSpec` (`CURSOR_DESCRIPTOR`), the declared coverage, the hook catalog (`CURSOR_HOOKS`), hook decoding, config-directory resolution (`config_dirs`), launch and resume argv, local turn pricing, and the capability impls |
| `crates/rimz/src/agents/adapters/cursor/payloads.rs` | The tolerant hook payload, the stop outcome, and the per-turn token split |
| `crates/rimz/src/agents/adapters/cursor/install.rs` | The `hooks.json` and `cli-config.json` install, preview, and uninstall, and the displaced-statusline sidecar |
| `crates/rimz/src/agents/adapters/cursor/session.rs` | The chats-store readers: root waits, child chats, and the discovery cache |
| `crates/rimz/src/agents/adapters/cursor/statusline.rs` | The statusline payload, its `AgentContext` projection, and model normalization |
| `crates/rimz/src/agents/adapters/cursor/transcript.rs` | Transcript resolution and the terminal-row reader |
| `crates/rimz/src/agents/adapters/cursor/account.rs` | The `status` and `about` account probe |

## Binary and config directories

RimZ claims Cursor only as `cursor-agent` (`CURSOR_BIN`, the spec's `bin_names` and `process_names`). Cursor's installer also links a generic `agent`, and other providers' installers (Grok's among them) link the same name, so RimZ never discovers, launches, or classifies a process as Cursor under `agent`. `cursor` names the IDE and is outside binary discovery too. A session the user starts as `agent` still binds to its pane, because hook attribution never reads the process name: it uses the installer-stamped `$PPID` and the durable pane and session stamps. Source-known liveness never matches the `agent` process to that session.

Cursor splits its files across two roots. `LaunchCapability::config_home` resolves the config directory in Cursor's own order:

| Condition | Config directory |
| --- | --- |
| `CURSOR_CONFIG_DIR` is non-blank | `$CURSOR_CONFIG_DIR` |
| else `XDG_CONFIG_HOME` is non-blank | `$XDG_CONFIG_HOME/cursor`, on every platform |
| else | `~/.cursor` |

The config directory holds `cli-config.json` and `chats/`. User hooks (`~/.cursor/hooks.json`) and transcripts (`~/.cursor/projects/`) stay under `~/.cursor` whatever the config directory is.

The installer's environment is not always Cursor's. A tmux room pins an unset `XDG_CONFIG_HOME` to `$HOME/.config` for its panes, while Zellij panes inherit the shell's value. From a shell without that variable, Cursor therefore reads `~/.cursor` outside tmux and `~/.config/cursor` inside it. `config_dirs` returns both directories, the caller's own first, deduplicated when they agree. Install writes the statusline into each, `installed` requires every one, and uninstall restores each. The chats-store readers (`session.rs::CursorRoots`) scan the same list and then `~/.cursor`, which older builds used for `chats/`.

User hooks run with `~/.cursor` as their working directory. `HookCapability::hook_ingress` therefore takes a nonempty absolute `CURSOR_PROJECT_DIR` as the participant start path (`mod.rs::cursor_project_dir`), before the shared verified-pin and `WorkspaceResolver` flow. An absent, empty, or relative value falls back to `.`.

## Hooks and lifecycle

### Install

Install writes two files as one transaction (`install.rs::install_into`). Each `CURSOR_HOOKS` event in `~/.cursor/hooks.json` gains one entry whose command is `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source cursor`, and the file's `version` is set to `1`. The `statusLine` of `cli-config.json` becomes the command `rimz statusline feed --source cursor`. RimZ builds both JSON candidates before it writes, writes each by temp file and rename, and restores the hook file byte for byte if the config write fails (`settings_json::commit_pair`). Consent shows both diffs.

Ownership is by command text. A hook entry is RimZ's when its command contains `rimz hooks feed --source cursor`. Install strips those entries and appends the canonical set, so user entries and unknown events keep their place. A statusline is RimZ's when its command contains `rimz statusline feed --source cursor`, which governs displacement, artifact detection, and uninstall. `installed` is stricter and requires the exact canonical command (`install.rs::statusline_installed_at`). Cursor rewrites `cli-config.json` from its typed schema and drops unknown keys, so a private marker field would not survive the rewrite. The canonical object therefore carries only schema keys: `type`, `command`, and the user's `padding`, `updateIntervalMs`, and `timeoutMs` (`RETAINED_RENDERING_KEYS`).

A displaced user statusline goes to a RimZ-owned sidecar, `~/.rimz/cursor-statusline.json`, written by temp file and rename before the pair commits and shown at consent as a third file when the primary directory displaces. The sidecar holds one `statusLine` field with the exact displaced value. Statusline forwarding reads the user's command from it, and uninstall restores it and then deletes the sidecar. An inline `_rimz_wrapped` value from an older install migrates to the sidecar on reinstall and stays an uninstall fallback.

| Case | Behaviour |
| --- | --- |
| Two config directories | The transaction runs once per `cli-config.json`, primary last, so when both displace a statusline the primary's overwrites the secondary's in the one sidecar. A displacement in the secondary directory alone is written to the sidecar but missing from the consent preview (`install.rs::preview_all` keeps only the secondary's `cli-config.json`). A failure on the second run leaves the first installed and `installed` false until a rerun. |
| Uninstall with two directories | Each `cli-config.json` that carries RimZ's statusline receives the sidecar's one value, and the sidecar is deleted after the last. Every such file gets the same value, so a directory whose original statusline differed from the sidecar's gets the sidecar's, and one that had none gains it. |
| `installed` | Every catalog event has an owned entry and every config directory carries the exact statusline command, so `rimz hooks install cursor` repairs either half. |
| Path overrides | `RIMZ_CURSOR_HOOKS`, `RIMZ_CURSOR_CLI_CONFIG` (one path, replacing the directory list), and `RIMZ_CURSOR_STATUSLINE_STATE` |

### Event mapping

| Native event | Normalized signal | Fields RimZ reads |
| --- | --- | --- |
| `sessionStart` | `Registered` | `conversation_id`, model, `transcript_path` |
| `beforeSubmitPrompt` | `TurnStarted` | sanitized `prompt` |
| `postToolUse` for `Shell`, `Write`, or `Delete` | `ToolUsed { mutates: true }`, with `edits` for `Write` and `Delete` | `tool_name` |
| `postToolUse` for any other tool | none | |
| `postToolUseFailure` | none: progress only, for active time | |
| `afterAgentResponse` | none: the final assistant text | `text`, token counts |
| `stop` | `TurnEnded`, or `TurnInterrupted` for `aborted` | `status`, token counts |
| `preCompact` | `Compacting` | `context_usage_percent`, `context_window_size` |
| `sessionEnd` | `Ended` | |
| `subagentStart` | `SubagentStarted` | `subagent_id`, `parent_conversation_id`, `subagent_type`, `task`, `subagent_model`, `git_branch` |
| `subagentStop` | `SubagentStopped` | the same IDs, `status`, `task` with `description` as fallback, `agent_transcript_path` |

`conversation_id` is the session key for every root event. A `stop` with `completed` ends clean, `aborted` lands idle through `TurnInterrupted`, and any other status, absent included, ends errored (`payloads.rs::CursorHookPayload::stop_outcome`). Model and effort ride every event that carries them: `model_id`, else `model`, names the model, and the `model_params` entry with id `effort` supplies the effort.

### Replies

Every catalog event answers with `{}`, Cursor's documented neutral JSON (`HookCapability::payload_hook_reply`). The one exception is a supervised run's deadline rung: a `postToolUse` from the run's own session can carry the rung text as `{"additional_context":"..."}`, the `TopLevel` shape the spec's `hook_context` declares (`crates/rimz/src/cli/hooks.rs::attach_deadline_context`, [deadline ladder](../harness/subagents.md#the-deadline-ladder)).

### Conversation switches and exit

`/clear` fires no hook. The next `beforeSubmitPrompt` is the first event that carries the new `conversation_id`, in the same process and pane (captured on 2026.07.09). The spec declares the `FollowLatest` same-pane policy, so each superseded root conversation collapses into the latest ([instances.md](./instances.md#same-pane-ownership)). On exit, `sessionEnd` names the conversation the process started with. A later conversation without its own end hook leaves through same-process supersession or dead-process reaping. Pane death has no hook and is derived by the `rimz exec` wrapper.

### Compaction

`preCompact` opens the compaction bracket, and Cursor has no post-compaction hook. The next lifecycle signal closes the bracket in the shared `step` state machine, and the projection expires the compaction head after its display window ([model.md](./model.md#the-compaction-bracket)).

### Claude-compatible hooks

Cursor can also run Claude-compatible hook commands with Cursor-shaped payloads. The Claude adapter recognizes those by `cursor_version` and classifies them `Unknown`, so they open no catalog ask ([adapter_claude.md](./adapter_claude.md#hooks-and-lifecycle)).

## Subagents

Cursor defines `subagentStart` and `subagentStop` with exact `subagent_id` and `parent_conversation_id` fields, and RimZ installs and maps both. Neither has been seen to fire: they are requests the Cursor backend sends to the CLI, a live probe on 2026.07.09 saw no child hook across four child launches, and the reference baseline does not re-verify it ([reference](../../externals/agent-adapter/cursor-reference.md#subagent-hooks)). The native mapping stays so that a build that starts sending them folds onto the same child rows. It requires distinct trimmed IDs (`resolve_subagent_identity`), uses `subagent_type` as the child's name and role, sanitizes the task, and attaches the child transcript on stop only.

### The chats-store derivation

The working source is the chats store. After a root Cursor hook records a lifecycle observation, `crates/rimz/src/cli/hooks/lifecycle/mod.rs::derive_subagent_lifecycle` asks `HookCapability::derive_subagent_observations` for the children in the parent's workspace. `session.rs::discover_subagent_chats` scans the newest 32 directories (`MAX_SESSIONS_PER_WORKSPACE`) of each `chats/<md5(workspace)>/` bucket, config directories first, and keeps the first record per child. A directory is a child when every check passes:

- The directory name is a safe single path component, and the directory is a real directory directly under the bucket.
- `meta.json` is absent, or is a schema-1 file with `isSubagent: true`. Cursor's session lister backfills that file into child directories.
- `store.db` is a regular file with SQLite `user_version` 1, opened read-only with no busy wait.
- The hex-encoded JSON in `meta` row `'0'` has an `agentId` equal to the directory name, a nonnegative `createdAt`, and a `subagentInfo.parentAgentId` that is nonempty and differs from the child ID.

The optional `subagentInfo.typeName` becomes the child's name and role, and `createdAt` orders the observations. The task and the outcome come from the child transcript at `~/.cursor/projects/*/agent-transcripts/<child>/<child>.jsonl`, where the CLI's Task subagent writer puts it. The task is the first `<user_query>` block of the first user record within 256 KiB and 64 lines. The outcome is the last nonblank line:

| Last line | Child |
| --- | --- |
| no transcript, a torn suffix, or a record whose `type` is not `turn_ended` | still running |
| `turn_ended` with `success`, `completed`, or `aborted` | stopped clean |
| `turn_ended` with any other status, or a line that is not JSON | stopped errored |

The join then admits only a child whose parent already exists as a Cursor row in the snapshot. It emits `chatsStoreSubagentStart` when the child has no row, and `chatsStoreSubagentStop` when the child's row is not already `Success` or `Failed`, start before stop, with the transcript path on the stop only. Both go through the same enrichment and store append as native hooks (`cli/hooks/lifecycle/observe.rs::record_derived_lifecycle_observation`), which stamp the owner and pane of the hook process that triggered the scan. A child that fails a check is skipped, and a reader failure never fails the parent hook.

The scan is per workspace, not per parent: `derive_subagent_observations` ignores the triggering parent's ID, and the join checks only that the recorded parent exists. With two Cursor sessions in one workspace, one session's hook can record the other session's new child under its own owner and pane.

The cadence is the trap. Detection runs only when the next root hook lands, often the parent's `stop`, so a child has no prompt start or stop signal. The derivation reads uncached on every such hook.

## Waits

Cursor's local hooks expose no permission request, plan approval, question, or idle notification. Two native prompts leave traces in the chats store (captured on 2026.07.09): the `AskQuestion` tool commits its pending call while the question is on screen, and a completed `CreatePlan` call leaves the **Ready to build?** approval open after the plan turn ends. RimZ reads both through local session discovery (`SessionCapability::discover_local_sessions`, `session.rs::discover`) and projects the hook-bound card to `Waiting` with the pane as the answer surface. It emits no `AwaitingInput` hook event, durable ask, or answer operation, so `rimz asks` stays empty. When the pending call disappears or a later conversation message lands, the next pull restores the hook lifecycle.

### Root chat validation

The root reader admits a chat directory under these checks, for each absolute UTF-8 pane workspace that is a real directory. A failed check produces no wait.

- `meta.json` is at most 64 KiB, schema 1, with `hasConversation`, `isSubagent` false, `createdAtMs` no later than `updatedAtMs`, and an absolute `cwd` that normalizes to the workspace.
- `store.db` passes the child checks above, with `createdAt` equal to `createdAtMs`, no `subagentInfo`, and a 64-character lowercase hex `latestRootBlobId`.
- The root blob is at most 2 MiB and its SHA-256 equals that ID. `prost` decodes it as `ConversationStateStructure`: field 4 is the pending calls, and the last entry of field 1 is the latest message's 32-byte blob ID.
- The session ID resolves to exactly one transcript under `~/.cursor/projects/`, and `store.db-wal` and `store.db-journal` are absent or regular files.
- The wait's start time falls between `createdAtMs` and `updatedAtMs`.

### The two waits

| Wait | Condition | Detail and start |
| --- | --- | --- |
| Question | Exactly one synchronous `AskQuestion` tool call across at most 64 pending entries, each at most 256 KiB, with a nonempty `toolCallId` of at most 512 bytes and no control characters | the first sanitized nonempty question, from `pendingToolCallStartedAtMs` |
| Plan approval | No pending calls, store `mode` `plan`, a nonblank `currentPlanUri`, and a last message blob (at most 256 KiB, SHA-256-verified) that is a `tool` message with exactly one `tool-result` content for `CreatePlan` | the fixed text `Ready to build?`, from `updatedAtMs` |

An async, ambiguous, oversized, hash-invalid, or schema-drifted state produces no wait. A later message clears a plan wait. Dismissing the plan prompt with Esc or `p` writes nothing to the store, so the card stays `Waiting` until the next turn.

### Discovery cache

Discovery is pulled on every refresh, so it caches. `CursorDiscoverySnapshot` keeps both validated waits and validated absence for the newest 32 chat directories per workspace. Each candidate's dependency set is the directory, `meta.json`, `store.db`, `store.db-wal`, `store.db-journal`, and its transcript paths. `store.db-shm` is left out because it changes on every reader's coordination. The dependencies are stamped before and after each read, and a change between the two discards the read (`StableValueCache::refresh`). One full scan resolves every selected session ID's transcript at once, and a change to transcript topology, chat topology, or the inputs, or the 30-second backstop (`LOCAL_SESSION_DISCOVERY_BACKSTOP` in `agents/local_session_cache.rs`), rebuilds that index.

The observation carries only the session ID, workspace, the question or the fixed plan detail, provider timestamps, and the public transcript path. Raw pending JSON, reasoning, assistant text, option labels, arguments, provider blobs, plan paths, and `store.db` paths never enter RimZ state. `fresh_binding_at` is `None`, so disk history never creates a card.

## Launch and resume

| Input | Argv |
| --- | --- |
| fresh launch | the located `cursor-agent` path, else the bare name for `$PATH` lookup at spawn (`mod.rs::cursor_launch_binary`) |
| startup prompt | positional after `--` |
| profile `model` | `--model <model>` |
| fast mode | skipped with a warning naming a fast model ID for `--model` |
| permission `ask` | none (the native default) |
| permission `plan` | `--mode=plan` |
| permission `auto` | `--auto-review`: allowlisted and sandboxable calls run, and Cursor's classifier judges the rest |
| permission `yolo` | `--force --sandbox disabled` |
| resume | `cursor-agent --resume <conversation_id>` (`LaunchCapability::resume_command`) |
| fork | none declared |
| manual compaction | `/summarize`, with no instruction argument |

Fresh and supervised launches open the interactive CLI in a pane. RimZ never passes `-p` or `--print`: Cursor's headless print mode fires no configured hooks, so it sits outside the hook-driven coverage contract. Profile skills use a `Frontmatter` view under `$HOME/.agents/skills` ([sandbox.md](../sandbox.md)). Host-mode skill links and tool rules are unsupported.

## Context and transcript

### Statusline

The command statusline is the rich-context source (`ContextCapability::observe_context`). Its payload supplies the session name, model, agent version, output style, vim mode, context window and fill, and the current input, output, and cache composition. Every optional field parses on its own, so one malformed value does not discard the rest (`statusline.rs::StatuslinePayload`).

Model display follows these rules. Cursor's `default` model sentinel normalizes to `auto` (`statusline::normalize_model`), so a stock session reads `Auto`. For an explicit selection, `normalize_model_metadata` strips an exact `param_summary` suffix from the display name: a leading context magnitude (`272K`) is dropped, a recognized effort word becomes the effort, and qualifiers such as `Fast` and `Thinking` stay on the model name. A display name that does not end with the summary stays intact, and `max_mode` with no effort word reads as effort `max`. The window gauge uses the live `context_window_size`, never the summary's nominal magnitude.

Each statusline refresh also re-reads the transcript's terminal row (`transcript::statusline_turn_markers`) and restamps the settle and turn-error markers, because the whole-context write would otherwise drop them.

### Hook fallbacks

Hook fields fill in where no statusline has run. `preCompact`'s `context_usage_percent` and `context_window_size` set the gauge. `stop`'s `input_tokens` includes cache-read and cache-write tokens, so RimZ derives fresh input by saturating subtraction and keeps output, cache-read, and cache-write as reported (`CursorHookPayload::turn_usage`). Explicit zeroes stay visible. These per-turn counts never populate the cumulative `total_tokens`.

### Transcript

Cursor writes one JSONL file per conversation at `~/.cursor/projects/<project>/agent-transcripts/<conversation_id>/<conversation_id>.jsonl`. `transcript::resolve_transcript` prefers the hook's `transcript_path`, then the persisted path, then the one exact match beneath the immediate project directories. Two matches resolve to none.

The transcript recovers a missed turn ending and nothing else. `transcript::refresh` runs on every root hook except `sessionEnd` and the subagent hooks, and on producer ticks and transcript growth, behind the shared unchanged-stat gate. It reads the bounded tail and acts only on a complete last record:

| Last record | Effect |
| --- | --- |
| `turn_ended` with `success` or `completed` | a `Complete` settle marker |
| `turn_ended` with `aborted` | an `Interrupted` settle marker |
| `turn_ended` with `error` | a turn error, labelled with the first 500 characters of `error` |
| anything else, or a torn suffix | no marker: the turn keeps running |

The terminal row carries no tokens, so recovery restores state only. A native resume rewrites the whole file as a snapshot of the conversation instead of appending (captured on 2026.07.09), which the tail read tolerates. Root handling never models assistant, thinking, user, tool, or message content. Workspace ownership comes from the participant resolver (`CURSOR_PROJECT_DIR` and the verified room pin), and `workspace_roots` and `postToolUse.cwd` are enrichment only.

### Final text

`afterAgentResponse.text` is the only safe final-text source, because Cursor's JSONL merges visible commentary and model thinking into blocks nothing tells apart. Hook ingestion appends the trimmed response to RimZ's transcript (`record_assistant_response`, [transcript.md](../harness/transcript.md#writing-entries)) and seeds an active supervised run's output without ending the run. The later `stop` stays the delivery checkpoint and the terminal transition.

## Account and balance

`account.rs::probe` runs the located binary's `status --format json`, then `about --format json` only after status establishes a login. Each call is bounded by the shared informational-probe timeout. The probe reads no credential file, token, browser state, SQLite database, or web API.

| Outcome | Condition |
| --- | --- |
| `LoggedOut` | `status` is `logged_out` (or a spelling of it, or `unauthenticated`), or `isAuthenticated` is false, with no contradicting field |
| `Found` | a positive login fact and a parseable `about`. It carries the email (status first, about as fallback), the raw `subscriptionTier` as plan, and `cliVersion` |
| `Unavailable`, retryable | a spawn failure, timeout, or nonzero exit, malformed JSON, an unknown `status` value, `status` and `isAuthenticated` disagreeing, no login fact at all, or status and about emails differing beyond case |

A tier marks metering known. A tierless login leaves metering unknown rather than guessing API-key or subscription semantics. The identity keeps an idle provider block visible. Quota windows, paid usage, historical spend, and account-day caps are unsupported.

## Cost

Cursor's hooks report tokens but no dollars, so RimZ prices each turn locally (`ContextCapability::price_turn_locally`). Both `afterAgentResponse` and `stop` repeat the turn's token counts. A response needs no status, since the event certifies a completed response, while a stop prices only with `completed`, `aborted`, or `error`. Pricing also needs a nonempty `generation_id`, a model from `model_id` or `model`, and a price-book entry for it, and a `-fast` model ID takes the fast multiplier. Auto turns report the literal `default` and never name the serving model, so they keep their tokens without a dollar value. An unknown model or a zero price publishes nothing rather than a known zero.

The context sidecar keeps the last priced generation ID and the cumulative total (`AgentContextRecord::apply_locally_priced_turn` in `agents/context/record.rs`). A repeat of the last generation ID, the response-then-stop pair, adds nothing, and a new generation adds once. Statusline refreshes keep the total. The total has session coverage: it drives the live card, the cockpit add-back, and live agent and room budgets, and it ends with the session's sidecar ([spending.md](./spending.md)).

## Known gaps

Run `rimz coverage` for the current wired, partial, and unsupported matrix. These gaps have a reason worth recording.

- Account usage and historical spend. The only machine-readable usage feed is the team-scoped Admin API behind an admin token, which a per-user CLI login does not hold. The probe's expired, API-key, service-account, proxy, and server-error arms are unverified and stay retryable.
- Native history and streaming. Assistant-history replay and incremental reply streaming stay empty, because the JSONL cannot separate visible text from thinking.
- Headless print mode. A live `-p` run completes without firing configured hooks, so RimZ neither parses its result nor claims hook coverage for it.
- `CURSOR_DATA_DIR`. Cursor moves project data, transcripts included, to that directory when it is set ([reference](../../externals/agent-adapter/cursor-reference.md#transcripts)). Root recovery still finds a relocated transcript through the hook's `transcript_path` or the persisted path. The unique-match fallback, the statusline restamp, the child readers, and the root wait reader look only under `~/.cursor/projects/`, so such a session has no child task or outcome and no root wait.
- Live verification is still owed for hook command shell and `$PPID` semantics on each platform, the `--` prompt terminator, `{}` under every approval mode, `conversation_id` stability across resume, and the Claude-compatible hook cross-fire.

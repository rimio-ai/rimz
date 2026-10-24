# Kiro adapter

This page maps the Kiro CLI v3 engine (`kiro-cli chat --v3`) onto RimZ's types: launch and resume, how a session binds before its first hook, the global hook file RimZ installs, the provider's local session store, the transcript fold, the account, and cost. The provider-neutral agent model is in [model.md](./model.md), the integration layer in [adapter.md](./adapter.md), and pane binding in [instances.md](./instances.md). The upstream protocol is in [kiro-reference.md](../../externals/agent-adapter/kiro-reference.md).

Kiro reaches RimZ through two sources, and each owns different facts. Native hooks in a global hook file carry session start, turn start, tool use, turn end, and teardown. The provider's local session store carries what no hook reports: the newborn card before the first prompt, pending approvals and questions, cancelled and failed turn outcomes, the context percentage, model, effort, title, and history. Store reads are pulled display truth: neither the adapter nor the snapshot fold appends them to the RimZ event log.

The minimum supported release is Kiro CLI 2.31.0 (`install::MIN_SUPPORTED_CLI`). It is the first release whose engine resolves configuration, skills, sessions, and global hooks from one home ([reference](../../externals/agent-adapter/kiro-reference.md#session-store)), which the adapter's home resolution assumes.

## Files

| File | Owns |
| --- | --- |
| [`agents/adapters/kiro/mod.rs`](../../../crates/rimz/src/agents/adapters/kiro/mod.rs) | The `static AgentSpec` (`KIRO_DESCRIPTOR`), the declared coverage, the hook catalog (`KIRO_HOOKS`), the managed source (`KIRO_MANAGED_SOURCE`), hook decoding (`KiroAdapter::decode_hook`), and the capability impls |
| [`agents/adapters/kiro/hooks.json`](../../../crates/rimz/src/agents/adapters/kiro/hooks.json) | The hook file RimZ installs, embedded as `HOOK_SOURCE` |
| [`agents/adapters/kiro/install.rs`](../../../crates/rimz/src/agents/adapters/kiro/install.rs) | Home and hook-path resolution, the version gate, reclaiming the unmarked file, install, preview, and uninstall |
| [`agents/adapters/kiro/session.rs`](../../../crates/rimz/src/agents/adapters/kiro/session.rs) | Store validation, the discovery cache, exact session lookup, the transcript fold, the context refresh, the resume-id parser, and the `Stop` final reply |

## Launch and resume

The launch spec maps RimZ's launch inputs onto `kiro-cli` argv:

| Input | Argv |
| --- | --- |
| fresh launch | `kiro-cli chat --v3` |
| launch prompt | a bare trailing positional (`PromptStyle::Positional`) |
| profile `model` | `--model <model>` |
| profile `effort` | `--effort <effort>` |
| permission `yolo` | `--trust-all-tools`, before the prompt |
| permission `ask`, `auto`, `plan` | none |
| exact resume | `kiro-cli chat --v3 --resume-id <session_id>` |
| bare `--resume` | `kiro-cli chat --v3 --resume-picker` |
| fork | unsupported |

A launch prompt that starts with `-` is refused before argv is built. The v3 TUI parser reads `--` as an unknown flag that consumes the next token ([reference](../../externals/agent-adapter/kiro-reference.md#launch-and-resume)), so no argv form passes such a prompt. The shared process compiler refuses it for every `PromptStyle::Positional` kind ([`harness/launch.rs`](../../../crates/rimz/src/harness/launch.rs), `AgentProcessCompileErr::PositionalPromptOption`), and the error tells the user to start with a word or send the text with `rimz message` after launch. Resume carries no prompt, so it is unaffected.

Bare `--resume` maps to the native picker because native `--resume` picks the newest conversation for the directory by itself ([reference](../../externals/agent-adapter/kiro-reference.md#launch-and-resume)). Resume strips and re-renders the permission flags through the shared launch machinery.

A live pane reads as Kiro when its process is `kiro-cli` (the launcher) or `kiro-cli-chat` (the v3 engine the launcher execs into). `kiro-cli-term` is excluded: it is the shell-integration process and runs in every integrated shell ([reference](../../externals/agent-adapter/kiro-reference.md#executables-and-engines)). `session::resumed_session_id` reads an exact resume id from either command line, after `kiro-cli` with an optional `chat`, in both the `--resume-id <id>` and `--resume-id=<id>` forms. It accepts only a `sess_<uuid>` id.

## Before the first hook

A Kiro card binds from the local store before any hook fires. The engine writes `session.json` and an empty `messages.jsonl` before the first prompt ([reference](../../externals/agent-adapter/kiro-reference.md#session-store)), and when `SessionStart` fires relative to that is not pinned upstream ([reference](../../externals/agent-adapter/kiro-reference.md#undocumented-behaviour)). Until a session binds, the pane shows the idle row that [instances.md](./instances.md#before-a-session-binds) describes.

Kiro supplies three inputs to the binding order in [instances.md](./instances.md#local-session-observations):

- Every observation sets `fresh_binding_at` to the session's validated `createdAt`.
- A pane whose command line carries `--resume-id` binds that exact session first.
- The observation's projection is `Lifecycle`, so its fold overlays an exact durable row under the rule in that page's projection table.

Through the fresh fallback, a new session binds the unique closest same-directory pane whose incarnation clock (the live process start, or a provisional launch's `registered_at`) is no later than `createdAt`. With neither clock, or with two equally close panes, the session stays unbound until a hook or a resume id arrives. A bind onto a pane that holds a provisional launch row reuses that row, so it keeps the launch-owned fields (`store/snapshot/view/live.rs::merge_bound_local_session`). Observations are not persisted, so a dead pane removes the card while the history stays in Kiro's store.

## Hooks

### Install

`rimz hooks install kiro` writes `<kiro-home>/hooks/rimz.json` whole, from the embedded `hooks.json`, through the shared `ManagedSource` with upgrades enabled. Global hook files fire in every workspace ([reference](../../externals/agent-adapter/kiro-reference.md#hooks)), so one file covers every room. `RIMZ_KIRO_HOOKS`, read from the process environment, overrides the path.

The Kiro home comes from the login env through `install::resolve_home`, and the same home serves configuration, skills (`skills/`), sessions (`sessions/`), and hooks:

| Case | Home |
| --- | --- |
| `KIRO_HOME` set, non-empty, absolute, not the filesystem root, and neither equal to nor an ancestor of `HOME` | `KIRO_HOME` |
| anything else | `$HOME/.kiro` |
| `HOME` unset or empty, and no valid `KIRO_HOME` | none: install fails, and discovery and lookup return nothing |

Both paths are compared after lexical normalization (`utils::path::normalize_path_lexical`), with no filesystem canonicalization. The rule matches the engine's own resolution in 2.31.0 ([reference](../../externals/agent-adapter/kiro-reference.md#session-store)).

The installed file names itself on its first line with a `_rimz_managed` key. The code relies on the v3 hook schema accepting that extra top-level key (the comment on `HOOK_SOURCE`). An unmarked file is RimZ's when its `hooks` array is non-empty and every `action.command` contains `hooks feed --source kiro` (`install::legacy_owned`). Install deletes and rewrites such a file, and uninstall removes it. Any other unmarked file is refused and left untouched.

Install and preview refuse when `kiro-cli --version` reports a release older than 2.31.0 (`install::refuse_old_cli`). An unprobeable binary installs. The same check is the integration's `install_blocker`, so the detected-agents install skips Kiro, `rimz setup` marks it blocked, and `rimz doctor` reports the hook status as blocked with the upgrade fix. Uninstall has no version gate.

Every hook runs `rimz hooks feed --source kiro || true`, because a `Stop` hook that exits 1 continues the turn ([reference](../../externals/agent-adapter/kiro-reference.md#hooks)). `SessionEnd` has a 5-second timeout, the engine's shared teardown budget, and the other four hooks have 10 seconds. Neutral output is empty stdout and exit 0 on every trigger.

### Events

| Native event | `LifecycleSignal` | Notes |
| --- | --- | --- |
| `SessionStart` | `Registered` | The payload carries no start source, so the decoder passes `SessionSource::Startup`. |
| `UserPromptSubmit` | `TurnStarted` | The sanitized `prompt` becomes the task and the prompt metadata. |
| `PostToolUse` | `ToolUsed { mutates, edits }` | `fs_write` is the one mutating and editing tool in the spec. |
| `Stop` | `TurnEnded { errored: false, parked_on_background: false }` | The final message is `session::last_reply`: the last transcript message in the bounded tail, when it is an assistant `Say`. |
| `SessionEnd` | `Ended` | Teardown of a loaded session, never a turn boundary. |

Every hook routes by the payload's `session_id`, the `sess_<uuid>` name of the store directory, and takes its worktree from the payload's `cwd`. `Stop` runs for a turn the graph completes, a refusal included, and is skipped when a model error or a cancel throws out of the turn ([reference](../../externals/agent-adapter/kiro-reference.md#hooks)). Those outcomes come from the store. `/compact` is the spec's compact command. It fires no hook and keeps the session id, so RimZ sees no compaction.

## The local store

The stock layout is `<kiro-home>/sessions/<bucket>/<sess_uuid>/{session.json,messages.jsonl}`. The bucket is the first 16 hex characters of SHA-256 over the session's workspace path (`session::workspace_bucket`). For a one-workspace session this is the engine's own bucket ([reference](../../externals/agent-adapter/kiro-reference.md#session-store)).

### Validation

A session directory is accepted only when every check holds (`session::validate_unscoped_session`, and `validate_session` for the workspace check):

| Check | Rule |
| --- | --- |
| Session id | The directory name is `sess_<uuid>` and equals `session.json`'s `id`. |
| Directory | The session directory is a real directory, not a symlink, and its canonical parent is the canonical bucket. |
| Files | `session.json` and `messages.jsonl` are both regular files, not symlinks. |
| Schema | `schemaVersion` is `1.0.0` and `dataModelVersion` is `1`. |
| Bucket | One absolute `workspacePaths` entry hashes to the bucket name. |
| Workspace | Discovery only: `workspacePaths` contains the requested cwd exactly. |
| `createdAt` | Discovery only: it parses as a timestamp, or the session yields no observation. |

ACP UUID directories, the v2 engine's `sessions/cli/` store, mismatched metadata, unsupported schemas, and symlinked session directories or files stay out.

### Discovery

`session::discover` resolves the home from the login env and looks only in the bucket of each requested workspace. Workspaces are made absolute and lexically normalized, then sorted and deduplicated (`local_session_cache::normalized_workspace_inputs`). The `sessions/` root and the bucket must both be real directories, and the bucket's canonical parent must be the canonical root. Observations come back ordered by `createdAt`, then by session id.

Each bucket admits at most 128 valid sessions (`MAX_SESSIONS_PER_WORKSPACE`), counted in `read_dir` order. When a bucket holds more, which 128 are admitted is unspecified. A session that passes validation but has an unparseable `createdAt` still takes a slot and yields no observation.

The discovery snapshot caches at two levels:

- The catalog (`IncrementalCatalog`) rescans a bucket when the home or the workspace set changes, when the `sessions/` root or a bucket changes, or every 30 seconds (`LOCAL_SESSION_DISCOVERY_BACKSTOP`).
- Between scans, each cataloged session is reparsed only when its directory, `session.json`, or `messages.jsonl` stamp changes (`SessionCandidate::dependencies`). When one becomes invalid or loses its observation, the catalog rescans once, so another valid session can take the free slot.

### Exact lookup

A lookup by session id (`session::transcript_for_session`) does not scan sibling sessions. It walks the 16-hex bucket names under `sessions/` in sorted order, checks each bucket the way discovery does, and returns `messages.jsonl` from the first `<bucket>/<session_id>` that passes the unscoped checks. It does not check the cwd or `createdAt`. The `Stop` final reply and `session_transcript` use it.

`session_transcript` first accepts the transcript path recorded on the row when `session::valid_transcript` passes. That check validates the session against its own parent directory and does not check that the parent sits under the current home's `sessions/` root. So a recorded path keeps resolving after `KIRO_HOME` moves, or through a symlinked bucket.

## The transcript fold

`session::fold` walks the bounded tail of `messages.jsonl` in file order. A line is a record when it parses as JSON with a non-empty `id` and a parseable `timestamp`. Other lines are dropped. Every record, unknown types included, advances `last_activity` and sets `first_event_at` once. Paged assistant reads (`read_assistant_transcript_page`) hold back a torn final line until it is complete.

The starting state comes from `session.json`'s `status` before any record is applied: `in_progress` is running, `failed` is failed, and anything else, absence included, is idle. A newborn session with an empty `messages.jsonl` and no `status` is therefore an idle card.

| Record | Effect |
| --- | --- |
| `user` with `source` absent, `chat`, `steer`, or unknown | The latest prompt, and user text in history when non-empty |
| `user` with `source` `hook` or `api` | Ignored |
| `assistant` with `operationType: "Say"` | Assistant text in history, with no state change |
| `turn_start` with a non-empty `executionId` | Running and reasoning, with every pending wait cleared |
| `pending_interaction`, `interactionType` `tool_approval` or `user_input`, with a non-empty `toolCallId` | Adds a wait keyed by `(toolCallId, consentRound)`, an absent round counting as `0`, and enters waiting with Kiro's question as the detail |
| `interaction_resolved` | Removes only the matching wait. The newest remaining wait becomes current, or the turn returns to running and reasoning. |
| `tool_call` with a non-empty `toolCallId`, `status` `approved`, `executing`, or `completed`, and no pending wait | Running. `fs_write` enters acting, and any other tool keeps the active phase. |
| `tool_result` with a non-empty `toolCallId`, `success: true`, and no pending wait | Running at the active phase |
| `session_metadata` with `key: "contextUsage"` | A finite `usagePercentage`, clamped to `0..=100` and rounded, becomes the context percentage. An invalid value keeps the last one. |
| `session_event` with `category: "session_pause"`, `status: "success"`, and no pending wait | Success |
| `turn_end` with a non-empty `stopReason` | `cancelled` is interrupted and shows idle. `refusal` and `error` fail. Any other reason succeeds. A non-success outcome clears every pending wait, and a success keeps the wait detail. |
| Anything else (`session_start`, `usage_summary`, tool metadata, internal context) | Clocks only |

A pending wait is a pane-only native wait. It is never a routable RimZ ask, and [instances.md](./instances.md#local-session-observations) owns how it overrides a durable ask. `completed` is the `tool_call` status Kiro 2.21.4 wrote ([reference](../../externals/agent-adapter/kiro-reference.md#turn-records)). Kiro's own workflow notices with `source: "steer"` show as user text. Separating them needs a captured marker field.

### Context refresh

`KiroAdapter::local_context_refresh` resolves the transcript through `session_transcript`, then skips the pass when the transcript stat is unchanged (`changed_transcript`). A session that fails validation yields no refresh, so every established value stays and the next tick retries. Because only the transcript stat gates the refresh, an edit to `session.json` alone shows on the next transcript change.

`session::refresh_context` folds the tail and returns this patch:

- The settle is `Interrupted` at a cancelled `turn_end`, `NativeWait` at the current wait's start, or cleared.
- The turn error is `TurnErrorClass::Failed` at a failed `turn_end`, or cleared.
- `modelId`, `effortLevel`, and `title` from `session.json` set the model, effort, and session name, and each absent field clears its value.
- The token fields follow `LocalContextPatch::authoritative_current`, which keeps an established gauge.

## Account and balance

RimZ implements no Kiro account probe: `AccountCapability` is empty, and the declared reason is that the `whoami` schema and the credit ledger are unpublished. No login identity, plan, or rate-limit window reaches the provider dashboard.

## Cost

Kiro meters credits ([reference](../../externals/agent-adapter/kiro-reference.md#usage-credits-and-context)), and RimZ derives nothing from them: no tokens, no context-window size, no dollars, and no realtime or historical spend. Kiro has no spend parser and is left out of fleet spending discovery.

## Known gaps

`rimz coverage --wiring` prints the current wired, partial, and unsupported grid. These gaps have a reason worth recording:

- Errored and cancelled turns fire no hook. The context refresh settles a cancellation as `Interrupted`, which releases `message --wait` and `agents wait` once a sidebar tick or a hook runs the refresh. A failed turn sets a `Failed` turn error, which does not release a running row's waiter. Supervised runs read only durable run records, so both outcomes wait out the run's deadline. A refused turn fires `Stop`, so its hook outcome is a success while the card folds to failed.
- Kiro CLI older than 2.31.0 gets no hooks: install refuses and the detected-agents install skips Kiro. A managed file written before a downgrade still counts as installed, though, and the hook preflight does not check the version.
- Pending approvals and questions have no hook. The waiting card comes from the store, stays out of `rimz asks`, and is answered in Kiro's pane.
- Credits are not dollars. Until Kiro publishes token counts or a dollar figure, no spend parser can be honest.
- Subagents have no discovered session. Parent transcripts carry `sub_agent_start` and `sub_agent_complete`, but child records under `sub-executions/` have no `session.json` and run without hooks. RimZ projects no child cards.
- A session with several workspace paths is invisible. The engine buckets it by a hash of all its paths ([reference](../../externals/agent-adapter/kiro-reference.md#session-store)), and validation needs one path that hashes to the bucket on its own.
- Answer routing, plan approval, compaction events, background parking, remote control, and account probing have no mapped surface.

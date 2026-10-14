# OpenCode adapter

Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md); the pinned upstream protocol is in [opencode-reference.md](../../externals/agent-adapter/opencode-reference.md).

RimZ supports OpenCode 2 only, verified against 2.0.27. OpenCode 1 users stay on RimZ 0.4.3. The adapter embeds [`plugin.ts`](../../../crates/rimz/src/agents/adapters/opencode/plugin.ts) and installs it as `~/.config/opencode/plugin/rimz.ts`. Its default export is `{ id: "rimz", setup }`. Only the registry reaches the concrete adapter; account, database, payload, transcript, and spend readers remain private.

## Shared language servers

No native language-server suppression is verified for OpenCode. Configuring [shared servers](../lsp.md) does not suppress OpenCode's own servers.

## Hooks and lifecycle

`setup(ctx)` does nothing without a nonempty `RIMZ_WORKSPACE_ID` or an executable RimZ (`RIMZ_BIN`, otherwise `PATH`). Repeated setup on the same context reuses its activation. Each activation owns its session, child, model, form, and gauge state, registers session/tool hooks, and consumes `ctx.event.subscribe({signal})`.

The plugin spawns `rimz hooks feed --source opencode`, writes a RimZ-owned snake-case envelope on stdin, and ignores stdout. `RIMZ_AGENT_PID = process.ppid` identifies the pane's TUI, not the private server running the plugin. Every envelope carries its session id, directory, and available title, version, model, and token gauge. Creation records `data.location.directory` and `data.version`; session lookup has no version field.

| Native v2 surface | RimZ envelope or effect |
| --- | --- |
| root `session.created` | `session_created`, registration |
| session `prompt` hook | `chat_message`, turn start with `prompt.text` |
| root `session.execution.succeeded` / `interrupted` | `session_idle`, turn end; plan agent adds `plan_proposed` |
| root `session.execution.failed` | `session_error`, with error type and message |
| child `session.created` | `SubagentStart`, parent id and sanitized title |
| child execution terminal | `SubagentStop`, failed execution marked as error |
| `session.step.started` / `ended` / `failed` | paired by `assistantMessageID`; terminal steps clear the pair and update the gauge when tokens are present, no separate envelope |
| `session.model.selected` / `agent.selected` | update remembered model / agent |
| `session.renamed`, session metadata refresh | `session_updated` when title changes |
| root tool `execute.after` hook | `tool_after`, tool name and error status |
| `session.compaction.started` | `session_compacting` |
| `session.compaction.ended` / `failed` | `session_compacted`, failure marked as error |
| `permission.asked` / `replied` | `permission_ask` / `permission_replied`, request id, action/resources or reply |
| `form.created` | `question_ask`, form title and one question per field; field description supplies the question and field title supplies its header |
| `form.replied` / `cancelled` | `question_replied` / `question_rejected`; answers flattened in remembered field order |
| root `session.deleted` | `session_ended`, reason `deleted` |
| activation cleanup | per-root `session_ended`, reason `dispose` |

Registration is lazy: a wired pane can exist before a session id, and its first prompt creates the session. The sidebar synthesizes an idle row until binding evidence arrives. `/new` hands pane ownership to the latest root; same-process supersession collapses the replaced record. RimZ records no `Fresh` lineage because the wire does not distinguish how a root was created.

Child model resolution re-emits `SubagentStart` once per changed model, enriching the original task and start time. Unknown sessions are classified through `ctx.session.get({sessionID}).parentID`, including after reload and before queued creation events arrive. Child tool events and reminders are skipped so they cannot move the parent's state. Cleanup aborts the subscription, disposes hook registrations, and waits at most 1.5 seconds for the root-end sweep and pending sends. Event handlers recheck disposal after awaits, and a failed handler does not end the subscription. Pane liveness and the reaper remain the crash backstop.

**Native prompts own answers.** Permission and form asks raise waiting cards and retain their structured questions. Replies clear waiting, record native answers, and expire pending asks. `native_ask_ui: true` routes attention to OpenCode's dialog; `rimz answer` is unsupported because no answer transport is wired.

**Plan approval is derived.** Successful or interrupted root execution under the remembered `plan` agent raises `PlanApproval`; the next prompt clears it. Select the agent in OpenCode's TUI with Shift+Tab. Launch-time Plan adds no arguments and does not select that agent. Yolo maps to `--auto`; Ask and Auto keep the provider default.

**Install.** A marked file is reclaimed byte-for-byte on reinstall; an unmarked file is user-owned and install/preview refuse to overwrite it. Setup offers a consented refresh when the marked file differs from the build's embedded copy. Uninstall removes only a marked file.

## Launch and resume

`crates/rimz/src/agents/adapters/opencode/mod.rs::OpencodeAdapter` launches `opencode --standalone [--auto] [--prompt <text>]` and resumes with `opencode --session <id> --standalone`. A private serve child inherits the pane environment and has the TUI as parent. The default shared service is outside this contract: its environment and parent do not identify the pane. Process detection still recognizes `opencode` and `bun`.

Detection searches `PATH`, then `~/.opencode/bin`. Launch, install/setup, and doctor refuse a readable version below 2.0.0, the oldest release RimZ supports, with the fix `opencode upgrade --major`. An unreadable version abstains. Fork and model presets are unsupported; a configured model preset is refused before launch.

Supervised children append `{"action":"subagent","resource":"*","effect":"deny"}` to `OPENCODE_CONFIG_CONTENT.permissions`, preserving the configured object and preceding rules while accepting JSONC comments and trailing commas on input. The emitted value is strict JSON. Absent, invalid, or non-object content becomes an object with that permissions array. Agent-specific permission rules can override this global deny because upstream appends them later. See [subagents](../harness/subagents.md) for the surrounding delegation contract.

Launch reminders ride `RIMZ_LAUNCH_REMINDERS`. The first activation claims the text into a `globalThis` symbol and deletes the environment variable so tool children cannot inherit it. The session `context` hook appends `{type: "text", text}` to the mutable `system` array for root primary requests only. Title, compaction, generation, and known child requests get none.

## Context and transcript

Paired step events provide input, output, reasoning, cache-read, and cache-write tokens. Reasoning is added to output; total is the sum of all five counters. A positive split replaces the gauge, including zero-valued siblings; an all-zero split preserves the last measurement. Later lifecycle envelopes carry the gauge. Context utilization counts input plus cache-read and cache-write, never output.

`ctx.model.list()` supplies model names and a positive `limit.input`, falling back to a positive `limit.context`; otherwise the window is omitted. The per-activation catalog retries after an empty or failed read. Rust retains its Claude-family window fallback when the catalog is unavailable. `ctx.session.get({sessionID})` refreshes title, agent, and model asynchronously at registration, prompt, and execution boundaries without holding terminal envelopes. Timestamped `New session - …` and `Child session - …` titles are never cached. Native rename events also update the title. Version is retained from creation, not fetched from session metadata.

`crates/rimz/src/agents/adapters/opencode/transcript.rs::read_messages` binds the session id and reads `session_message` user `data.text` and assistant `data.content[]` text entries only. Reasoning, tools, synthetic timeline records, and other sessions stay out. Full reads order by `time_created`, then `rowid`. Incremental output and the final-message pick use completed assistant rowids: `time.completed`, `finish`, or `error` marks completion, so an in-place streaming update is not lost behind an early cursor. Imported older-timestamped rows can remain new to this cursor. Cursor identity includes database path and session id.

`rimz agents history` combines these messages with session-scoped spend rows. Spend timestamps have second precision; turn attribution compares boundaries at that precision, while displayed timestamps retain their precision. If two turns open in the same stored second, the later boundary owns it.

## Account and balance

`crates/rimz/src/agents/adapters/opencode/database.rs::credentials_in_file` reads SQLite's `credential` table, ordered by `active DESC, time_created DESC, id DESC`, keeping the first parsed row per `integration_id`. Credential stamps use the maximum `time_updated` in milliseconds of those kept rows, so session writes do not invalidate OAuth usage caches. A missing database or credential table means logged out; database read failures mean unavailable. Types map as `oauth` to metered subscription, `key` to unmetered API key, and `external` to unknown metering. Provider selection prefers the latest assistant row's `model.providerID`, then the first OAuth credential, then provider-name order. The raw provider key becomes `sub_provider`.

The delegated-account loader shares identity and usage selection with Pi, whose loader remains file-backed. OpenAI identity prefers `value.metadata.accountID`, otherwise hashes the refresh token or access-token fallback under OpenCode's domain. Supported OpenAI and Anthropic OAuth credentials delegate to Codex and Claude usage normalization respectively. Windows and extra credits cache under `opencode`; OpenAI receives Codex's lifted 5h completion. API-key, external, and unsupported-provider credentials carry no OAuth quota scope. The plugin does not wire response-header budget enrichment, so bars come from the out-of-band usage probe.

## Cost

`crates/rimz/src/agents/adapters/opencode/database.rs::files` discovers `opencode.db` and valid `opencode-<channel>.db` files under `XDG_DATA_HOME/opencode`, otherwise `~/.local/share/opencode` (`RIMZ_OPENCODE_DATA_DIR` in tests). Per directory it selects the most recently modified logical database, considering the main file and optional WAL. Ties prefer the primary filename, then channel names lexicographically. Credentials and the hook transcript-path fallback share newest-logical-database selection across those directories. Access is read-only. WAL changes invalidate spend and live-cost caches; shared-memory coordination churn does not.

`crates/rimz/src/agents/adapters/opencode/spend.rs::parse` cold-folds assistant and usage-bearing compaction `session_message` rows, joining `session_v2.directory` for origin. Compaction uses its own model when present, otherwise the session's latest assistant model, and the row's creation timestamp. Cost-only compaction rows also count. Compaction summaries stay out of transcripts and active-provider selection. Mutable rows require authoritative replacement of that database's cache set, not cursor resumption. Each assistant row supplies `model.id`, `model.providerID`, tokens, timestamp, cost, and content. Positive cost is authoritative; zero cost is priced from tokens, including reasoning at the output rate and both cache rates. Unknown models retain tokens and zero dollars until pricing refresh. Numeric strings are accepted for token, time, and cost fields; absent token splits fall back to a reported total as output, otherwise a row without positive cost is dropped.

Each spend entry retains its `session_id`. Turn-end hooks reconcile the live card to that session's authoritative dollar sum, not the whole database. Live tool counts come from named `tool_after` events; historical counts come from each assistant row's tool content entries and their `name` fields.

## Known gaps

Run `rimz coverage` for the current matrix. Native answering, background parking, and remote control remain unsupported. Idle attention is partial, reconstructed from turn boundaries, native asks, and the stall window. Launch-time plan selection, model presets, and fork are not supported, although an interactive plan-agent turn still derives a waiting gate. The agent-specific permission override limit is described above. RimZ does not yet honor upstream's `OPENCODE_DB` override; standard-directory discovery remains the storage contract.
